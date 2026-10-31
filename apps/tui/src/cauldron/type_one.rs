//! **The REAL type-one call** behind [`super::TypeOneCall`]: the
//! `type-one` CLI (Cauldron's `ripple/v2` branch, `libs/type-one`; it moved
//! there from substrate-ripple, whose old build wrote 12 values into a
//! catalogue the new one had migrated to 13 — task 0209 D9), run as a
//! child process — `type-one ask --ask-file <f> --adapters <a> --json`.
//!
//! Why the CLI and not a library: that crate links its own tokio, reqwest and
//! rusqlite and lives on another branch (ripple/v2); a cross-branch path
//! dependency is the coupling this face already refused for the threads wire
//! (`Cargo.toml`, DEC-10). The CLI is the adapter's own front door, and it
//! carries everything the call needs that this face must not: the OpenRouter
//! key resolution (env / key FILE — never a value in code or config here), the
//! drift catalogue, calibration and validation of every distribution.
//!
//! ONE batched ask: the state names his boundaries and every candidate; one
//! yes/no question per candidate, its instructions the seeding ripple's own
//! question fragment with `{{slot:boundaries}}` and `{{slot:item}}` filled,
//! its two criteria the definition's. The answer is the full distribution per
//! question; the face keeps `p(true)`.
//!
//! The binary: `CAULDRON_TYPE_ONE_BIN`, else `type-one` on `PATH`. Its own
//! config (`TYPE_ONE_CONFIG`, else `~/.config/substrate/type-one.toml`) is the
//! CLI's business and passes through the environment untouched.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{Answer, Candidate, TypeOneCall};
use crate::cauldron::ripples::RippleDef;

/// Names the binary explicitly.
pub const BIN_ENV: &str = "CAULDRON_TYPE_ONE_BIN";
/// Seconds before the child is killed (default [`DEFAULT_TIMEOUT_S`]).
pub const TIMEOUT_ENV: &str = "CAULDRON_TYPE_ONE_TIMEOUT_S";
pub const DEFAULT_TIMEOUT_S: u64 = 90;

/// The two adapters the CLI has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adapter {
    /// Claude Haiku through the threads daemon.
    Haiku,
    /// Jev through OpenRouter's Decisions endpoint.
    Jev,
}

impl Adapter {
    pub fn name(self) -> &'static str {
        match self {
            Adapter::Haiku => "haiku",
            Adapter::Jev => "jev",
        }
    }
    pub fn from_model(m: &str) -> Option<Adapter> {
        match m.trim() {
            "haiku" => Some(Adapter::Haiku),
            "jev" => Some(Adapter::Jev),
            _ => None,
        }
    }
}

/// The `type-one` binary: `CAULDRON_TYPE_ONE_BIN`, else the first `type-one`
/// on `PATH`. `Err` says which was tried.
pub fn find_bin() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os(BIN_ENV).filter(|v| !v.is_empty()) {
        let p = PathBuf::from(p);
        return if p.is_file() { Ok(p) } else { Err(format!("{BIN_ENV}={} is not a file", p.display())) };
    }
    std::env::var_os("PATH")
        .and_then(|path| std::env::split_paths(&path).map(|d| d.join("type-one")).find(|p| p.is_file()))
        .ok_or_else(|| format!("no `type-one` binary: set {BIN_ENV} or put the ripple/v2 type-one on PATH"))
}

/// One adapter of the `type-one` CLI.
pub struct TypeOneCli {
    pub bin: PathBuf,
    pub adapter: Adapter,
    pub timeout: Duration,
    /// REL-6a7: the fresh directory [`TypeOneCli::local_stand_in`] wrote its
    /// script into. Exists only in this crate's unit-test build; no other
    /// build has a way to set it, so no other build has a path past the gate.
    #[cfg(test)]
    stand_in_dir: Option<PathBuf>,
}

impl TypeOneCli {
    pub fn new(bin: PathBuf, adapter: Adapter) -> Self {
        let s = std::env::var(TIMEOUT_ENV).ok().and_then(|v| v.trim().parse().ok()).filter(|s| *s > 0).unwrap_or(DEFAULT_TIMEOUT_S);
        TypeOneCli {
            bin,
            adapter,
            timeout: Duration::from_secs(s),
            #[cfg(test)]
            stand_in_dir: None,
        }
    }

    /// **THE TEST SEAM** (REL-6a7, unit-test builds only): a CLI whose
    /// program is `script`, written by THIS call as `type-one` in a fresh
    /// temp directory it creates. It takes a script, never a path, so it
    /// cannot be pointed at the real type-one; and the gate is skipped only
    /// while `bin` is still that file ([`Self::is_local_stand_in`]) — repoint
    /// `bin` and the external-API gate is back.
    #[cfg(all(test, unix))]
    pub(crate) fn local_stand_in(script: &str, adapter: Adapter) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("cauldron-type-one-stand-in-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&dir).expect("a fresh stand-in dir");
        let bin = dir.join("type-one");
        std::fs::write(&bin, script).expect("the stand-in script");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod the stand-in");
        let mut cli = TypeOneCli::new(bin, adapter);
        cli.stand_in_dir = Some(dir);
        cli
    }

    /// Whether `bin` is the script [`Self::local_stand_in`] wrote. Always
    /// `false` outside this crate's unit-test build.
    fn is_local_stand_in(&self) -> bool {
        #[cfg(test)]
        if let Some(d) = &self.stand_in_dir {
            return self.bin == d.join("type-one");
        }
        false
    }
}

#[cfg(test)]
impl Drop for TypeOneCli {
    fn drop(&mut self) {
        if let Some(d) = &self.stand_in_dir {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

/// Fill a fragment's `{{slot:name}}`.
fn fill(fragment: &str, boundaries: &str, item: &str) -> String {
    fragment.replace("{{slot:boundaries}}", boundaries).replace("{{slot:item}}", item)
}

fn item_line(c: &Candidate) -> String {
    format!("{} {} — {}", c.kind.word(), c.id, c.text)
}

/// The ask file the CLI reads: ONE ask, one yes/no question per candidate,
/// keyed `<question key>_<index>`.
pub fn ask_json(ripple: &RippleDef, question: &str, boundaries: &str, items: &[Candidate], run_id: &str, policy: &str) -> Result<Value, String> {
    let q = ripple.questions.first().ok_or("the ripple has no question")?;
    if q.ty != "yes_no" {
        return Err(format!("question `{}` is {}, and the seeding asks yes/no", q.key, q.ty));
    }
    let crit = |a: &str| q.criteria.iter().find(|c| c.answer == a).map(|c| c.text.clone()).ok_or(format!("question `{}` has no `{a}` criterion", q.key));
    let (if_true, if_false) = (crit("true")?, crit("false")?);
    let mut state = format!("A NEW CLUSTER. What he said belongs in it, in his own words:\n{boundaries}\n\nTHE SOUP — every candidate; each question asks about exactly one:\n");
    for (i, c) in items.iter().enumerate() {
        state.push_str(&format!("[{i}] {}\n", item_line(c)));
    }
    let questions: Vec<Value> = items
        .iter()
        .enumerate()
        .map(|(i, c)| {
            json!({
                "key": format!("{}_{i}", q.key),
                "template_id": q.template_id,
                "kind": {"type": "yes_no", "if_true": if_true, "if_false": if_false},
                "instructions": fill(question, boundaries, &format!("[{i}] {}", item_line(c))),
            })
        })
        .collect();
    Ok(json!({
        "state": state,
        "questions": questions,
        "tag": {"run_id": run_id, "step_id": ripple.id, "config": policy},
    }))
}

/// `p(true)` per candidate, the cost and the adapter's latency, out of the
/// CLI's `Decision` JSON. Every key must be there: a missing answer is an
/// error, never a default.
pub fn parse_decision(out: &Value, key: &str, n: usize, adapter: Adapter) -> Result<Answer, String> {
    let resolved = out.get("resolved").and_then(Value::as_object).ok_or("the decision has no `resolved` map")?;
    let mut p = Vec::with_capacity(n);
    for i in 0..n {
        let k = format!("{key}_{i}");
        let v = resolved.get(&k).and_then(|d| d.get("true")).and_then(Value::as_f64).ok_or(format!("the decision has no p(true) for `{k}`"))?;
        if !(0.0..=1.0).contains(&v) {
            return Err(format!("p(true) for `{k}` is {v}, outside 0..1"));
        }
        p.push(v);
    }
    let me = out
        .get("per_adapter")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().find(|x| x.get("adapter").and_then(Value::as_str) == Some(adapter.name())))
        .ok_or(format!("the decision carries no `{}` answer", adapter.name()))?;
    Ok(Answer {
        p,
        cost_usd: me.get("cost_usd").and_then(Value::as_f64),
        latency_ms: me.get("latency_ms").and_then(Value::as_u64),
        input_tokens: me.get("input_tokens").and_then(Value::as_u64),
        ask_id: out.get("ask_id").and_then(Value::as_str).map(str::to_string),
    })
}

/// Run `bin args…` to completion within `timeout`; `Ok(stdout)` on exit 0.
/// `local_stand_in` is [`TypeOneCli::is_local_stand_in`]: `false` in every
/// build but this crate's unit tests.
fn run(bin: &Path, args: &[&str], timeout: Duration, local_stand_in: bool) -> Result<String, String> {
    // REL-6a5: the spawn half of `seeding::real_call`'s refusal — every
    // type-one run is an external API call, refused under isolation. REL-6a7:
    // except a unit test's own scripted stand-in, which reaches nothing.
    if !local_stand_in {
        env::egress::admit_external_api("type-one", crate::cauldron::live::isolation(false).as_deref())?;
    }
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {}: {e}", bin.display()))?;
    let mut so = child.stdout.take().expect("piped");
    let mut se = child.stderr.take().expect("piped");
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = se.read_to_string(&mut s);
        s
    });
    let t0 = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if t0.elapsed() >= timeout => {
                // Our own child, and only ours.
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("no answer within {} s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => return Err(format!("waiting on type-one: {e}")),
        }
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    if status.success() {
        Ok(out)
    } else {
        // The CLI's `Error: …` line names the cause (anyhow); keep it short.
        let why = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no stderr").trim();
        Err(format!("type-one exited {}: {}", status.code().map_or("by signal".into(), |c| c.to_string()), why.chars().take(240).collect::<String>()))
    }
}

impl TypeOneCall for TypeOneCli {
    fn adapter(&self) -> String {
        self.adapter.name().to_string()
    }
    fn stand_in(&self) -> bool {
        false
    }
    fn ask(&self, ripple: &RippleDef, question: &str, boundaries: &str, items: &[Candidate], run_id: &str, policy: &str) -> Result<Answer, String> {
        if items.is_empty() {
            return Ok(Answer::default());
        }
        let key = ripple.questions.first().map(|q| q.key.clone()).ok_or("the ripple has no question")?;
        let ask = ask_json(ripple, question, boundaries, items, run_id, policy)?;
        let file = std::env::temp_dir().join(format!("cauldron-type-one-{}.json", uuid::Uuid::new_v4().simple()));
        std::fs::write(&file, serde_json::to_vec(&ask).map_err(|e| e.to_string())?).map_err(|e| format!("writing {}: {e}", file.display()))?;
        let f = file.to_string_lossy().into_owned();
        let r = run(&self.bin, &["ask", "--ask-file", &f, "--adapters", self.adapter.name(), "--json"], self.timeout, self.is_local_stand_in());
        let _ = std::fs::remove_file(&file);
        let out = r?;
        let v: Value = serde_json::from_str(out.trim()).map_err(|e| format!("type-one printed no decision JSON: {e}"))?;
        parse_decision(&v, &key, items.len(), self.adapter)
    }
}
