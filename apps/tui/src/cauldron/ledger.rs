//! **Status › Ledger: the whole history of execution** (8e4fca10 msg 457).
//!
//! > *"let's make it the first tab inside of status. So then you can
//! > actually like see what ran, you know, which dump, which ripples, how
//! > long, you know, the whole nested structure ... It'll have the entire
//! > history of execution ... it should be sorted by most recent at the top
//! > ... it'd also be cool if we could kind of see if they're type 1, type
//! > 2, or type 3 ... it's actually a type surrounded by a circle ... if I'm
//! > looking at a type 1 ... You're just going to have an input and an
//! > output ... looking at a type 2 is going to look just like an agent.
//! > Looking at a type 3 is going to look kind of like a step function ...
//! > I think that the best way to visualize it might be to display it like
//! > the way that you're displaying the tasks in the surface ... Actually, I
//! > think ledger is just a cooler word."*
//!
//! Every DUMP is a band, newest first; inside it the ripples it ran, nested
//! to any depth — a type 3 holds the ripples it called, and they
//! theirs — drawn as the Tasks tab draws its graph: a rail margin with each
//! row's state glyph in its lane, a rail running down from a ripple to the
//! last of what it ran. Each row says its TYPE as a circled glyph (① ② ③),
//! its state, when it started and how long it took. Enter or a click
//! explores one in the bus modal ([`crate::cauldron::bus::BusPage::Exec`]):
//! a type 1 its input and its output; a type 2 its transcript, as an agent's
//! reads; a type 3 its steps, in order, as a step function, each ripple a
//! step called a link to its own page.
//!
//! DEMO DATA ONLY (msg 403 Q9, V1.4.S2): the executions are the seed's —
//! the environment's own `seed/ledger.json` (REL-3.S2: only `cauldron-build`
//! has one, the soup crate's curated dataset), or — only when a test names
//! a fixture dir (`--seeds`) — a fixture's `soup/<env>.ledger.json`. Every
//! other environment, prod included, has none (V1.4.S2b: a real face names
//! no fixture, so no file anywhere can give it one). Executions are keyed by the dump they ran for. A dump in the bucket's log the seed has no
//! executions for — every dump made in this face, which has no engine — is
//! on the ledger too, with nothing run yet.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::cauldron::ripples::Kind;
use crate::cauldron::store::Send;

/// One ripple's execution, and the ripples it ran.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Exec {
    /// The ripple that ran (`dump`, `fragment`, `author`).
    pub ripple: String,
    /// Type 1, 2 or 3 (`type_one`, `type_two`, `type_three`; `shell` still reads as a type 3).
    pub kind: Kind,
    /// running | done | failed | parked | pending
    pub state: String,
    /// When it started, in ms after its dump was made.
    #[serde(default)]
    pub after_ms: u64,
    /// How long it ran (so far, while running).
    #[serde(default)]
    pub ms: u64,
    /// One line: what it did.
    #[serde(default)]
    pub line: String,
    #[serde(default)]
    pub model: String,
    // ── a type 1 ──
    #[serde(default)]
    pub input: String,
    #[serde(default)]
    pub output: String,
    // ── a type 2 ──
    #[serde(default)]
    pub transcript: Vec<Turn>,
    // ── a type 3 ──
    #[serde(default)]
    pub steps: Vec<Step>,
    /// The ripples it ran, in the order they started.
    #[serde(default)]
    pub children: Vec<Exec>,
}

/// One turn of a type 2's transcript.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Turn {
    /// system | user | assistant | tool
    pub role: String,
    /// A tool turn's call (`soup.read`); empty otherwise.
    #[serde(default)]
    pub tool: String,
    pub text: String,
}

/// One step of a type 3's step function.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub name: String,
    /// native | call | foreach | branch | wait
    pub word: String,
    pub state: String,
    #[serde(default)]
    pub ms: u64,
    #[serde(default)]
    pub line: String,
    /// The ripples this step ran: indexes into the execution's `children`.
    #[serde(default)]
    pub calls: Vec<usize>,
}

/// A dump as the seed records what ran for it.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SeedDump {
    pub seq: u64,
    /// `-2h`, `-1d`, or an RFC 3339 instant.
    pub at: String,
    /// The dump's words, for a dump the bucket's log no longer holds.
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub ripples: Vec<Exec>,
    /// A THREAD FOLD (V1.4, R-V1 item 4: "thread folds appear as rows"):
    /// the thread's display name; empty for an ordinary dump. A fold is a
    /// per-turn type 3 (product spec §10), so it carries ripples like a dump.
    #[serde(default)]
    pub thread: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seed {
    #[serde(default, rename = "_note")]
    pub note: String,
    #[serde(default)]
    pub dumps: Vec<SeedDump>,
}

impl Seed {
    /// Environment `env`'s ledger seed; none is an empty ledger. Its OWN
    /// seed first (`<dir>/seed/ledger.json`, REL-3.S2: what
    /// `soup::demo::install` writes into `cauldron-build`, the demo's one
    /// real place), else — ONLY when a test named a fixture dir
    /// (`fixtures` is `Some`, see [`fixtures`]) — the fixture named for
    /// `env` (`<fixtures>/soup/<env>.ledger.json`). A face on a real
    /// environment passes `None`: it reads its own seed or nothing, whatever
    /// any directory holds (V1.4.S2b, G13). No environment falls back to
    /// anything else: prod's old `<seeds>/ledger.json` is gone (V1.4.S2,
    /// G10), so prod shows only its bucket log's dumps.
    ///
    /// THE one loader of the ledger, called once per `App::open` (and so
    /// once per `switch_env`); [`loads_on_this_thread`] counts it (G12).
    pub fn load(fixtures: Option<&Path>, env: &str, dir: &Path) -> Result<Seed> {
        LOADS.with(|n| n.set(n.get() + 1));
        let fixture = fixtures.map(|f| f.join("soup").join(format!("{env}.ledger.json")));
        let p = if let Some(mine) = crate::cauldron::store::own_seed(dir, "ledger.json") {
            mine
        } else if let Some(fx) = fixture.filter(|p| p.is_file()) {
            fx
        } else {
            return Ok(Seed::default());
        };
        let text = match std::fs::read_to_string(&p) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Seed::default()),
            Err(e) => return Err(e).with_context(|| format!("read {}", p.display())),
        };
        let mut s: Seed = serde_json::from_str(&text).with_context(|| format!("parse {}", p.display()))?;
        for d in &mut s.dumps {
            d.at = absolute(&d.at).with_context(|| format!("{}: dump {}: `at` {}", p.display(), d.seq, d.at))?;
        }
        Ok(s)
    }
}

/// The fixture dir the ledger of a face opened on `seeds` may fall back to:
/// `seeds` itself when a test NAMED it (`tui --seeds <dir>`, or a test's
/// `App::open(env, seed_dir(), ..)`), and NONE when the face is on a real
/// environment — the CLI hands a real face `store::no_seeds(env)`, so that
/// is the one value that means "no fixture was named" (V1.4.S2b: a real
/// environment, prod included, gets no fixture fallback at all; only
/// `cauldron-build`'s own seed and an explicitly named fixture are read).
pub fn fixtures<'a>(seeds: &'a Path, env: &::env::Environment) -> Option<&'a Path> {
    (seeds != crate::cauldron::store::no_seeds(env).as_path()).then_some(seeds)
}

thread_local! {
    /// How many times [`Seed::load`] ran on this thread (G12: one loader,
    /// once per open; a read from the render path would move it).
    static LOADS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times [`Seed::load`] has run on the calling thread.
pub fn loads_on_this_thread() -> usize {
    LOADS.with(|n| n.get())
}

/// `-2h` (that long before now) as an RFC 3339 instant; an instant as it is.
fn absolute(at: &str) -> Result<String> {
    let Some(rel) = at.strip_prefix('-') else { return Ok(at.to_string()) };
    let (n, unit) = rel.split_at(rel.len().saturating_sub(1));
    let n: i64 = n.parse().context("a number, then m, h or d")?;
    let secs = match unit {
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => 1,
    } * n;
    Ok((chrono::Utc::now() - chrono::Duration::seconds(secs)).to_rfc3339())
}

/// A dump on the ledger: its number, when, its words, what ran.
#[derive(Debug, Clone, PartialEq)]
pub struct Dump {
    pub seq: u64,
    pub at: Option<chrono::DateTime<chrono::Local>>,
    pub text: String,
    pub ripples: Vec<Exec>,
    /// A thread fold's thread, by its display name; empty for a dump.
    pub thread: String,
}

/// The Ledger's order, one switch (release question Q-L1): `true` lists the
/// newest dump at the top (his 10-30 words, IF#457), `false` at the bottom
/// as a Claude Code thread reads.
pub const NEWEST_FIRST: bool = true;

impl Dump {
    /// Its one state: running while anything runs; failed when something
    /// failed; pending while nothing has run; else done.
    pub fn state(&self) -> &'static str {
        let all = self.all();
        if all.is_empty() {
            "pending"
        } else if all.iter().any(|e| e.state == "running") {
            "running"
        } else if all.iter().any(|e| e.state == "failed") {
            "failed"
        } else if all.iter().any(|e| e.state == "parked") {
            "parked"
        } else {
            "done"
        }
    }
    /// Every execution under it, at every depth.
    pub fn all(&self) -> Vec<&Exec> {
        fn walk<'a>(es: &'a [Exec], out: &mut Vec<&'a Exec>) {
            for e in es {
                out.push(e);
                walk(&e.children, out);
            }
        }
        let mut out = Vec::new();
        walk(&self.ripples, &mut out);
        out
    }
    /// From the dump to the end of the last thing that ran for it.
    pub fn span_ms(&self) -> u64 {
        self.ripples.iter().map(|e| e.after_ms + e.ms).max().unwrap_or(0)
    }
    /// Its first line, for its band.
    pub fn headline(&self) -> String {
        self.text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('>')).or_else(|| self.text.lines().next()).unwrap_or("").to_string()
    }
    /// Its words as its event block shows them: the headline and at most
    /// one more non-empty line (quoted `>` lines are references, not words).
    pub fn words(&self) -> Vec<String> {
        let head = self.headline();
        let mut out = vec![head.clone()];
        let mut seen = false;
        for l in self.text.lines().map(str::trim) {
            if !seen {
                seen = l == head;
                continue;
            }
            if !l.is_empty() && !l.starts_with('>') {
                out.push(l.to_string());
                break;
            }
        }
        out.retain(|l| !l.is_empty());
        out
    }
    /// The phases of a top-level ripple that is a stepped type 3: each step,
    /// with the indexes into its `children` the step called (out-of-range
    /// ones dropped), then the children no step called, in order. `None` for
    /// any other ripple, which draws as a plain tree.
    pub fn phases(e: &Exec) -> Option<(Vec<(&Step, Vec<usize>)>, Vec<usize>)> {
        if e.kind != Kind::TypeThree || e.steps.is_empty() {
            return None;
        }
        let steps: Vec<(&Step, Vec<usize>)> = e.steps.iter().map(|st| (st, st.calls.iter().copied().filter(|&i| i < e.children.len()).collect())).collect();
        let rest = (0..e.children.len()).filter(|i| !steps.iter().any(|(_, c)| c.contains(i))).collect();
        Some((steps, rest))
    }
    /// The execution at `path`: `[i]` the dump's i-th ripple, `[i, j]` its
    /// j-th child, and so on down.
    pub fn exec_at(&self, path: &[usize]) -> Option<&Exec> {
        let (first, rest) = path.split_first()?;
        let mut e = self.ripples.get(*first)?;
        for i in rest {
            e = e.children.get(*i)?;
        }
        Some(e)
    }
}

/// The ledger: every dump the log or the seed knows, newest first. A dump
/// both know takes its words and time from the log, its executions from the
/// seed — SETTLED (re-audit 10 D5): the seed's clock is not the log's, so a
/// dump the log dated never shows the seed's `running` and its frozen `so
/// far`; what the seed left running shows finished.
pub fn dumps(seed: &Seed, sends: &[Send]) -> Vec<Dump> {
    #[cfg(test)]
    DUMPS_CALLS.with(|c| c.set(c.get() + 1));
    let local = |s: &str| chrono::DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&chrono::Local));
    let mut out: Vec<Dump> = sends
        .iter()
        .map(|s| {
            let sd = seed.dumps.iter().find(|d| d.seq == s.seq);
            Dump { seq: s.seq, at: local(&s.at), text: s.text.clone(), ripples: sd.map(|d| settled(&d.ripples)).unwrap_or_default(), thread: sd.map(|d| d.thread.clone()).unwrap_or_default() }
        })
        .collect();
    for d in &seed.dumps {
        if !out.iter().any(|o| o.seq == d.seq) {
            out.push(Dump { seq: d.seq, at: local(&d.at), text: d.text.clone(), ripples: d.ripples.clone(), thread: d.thread.clone() });
        }
    }
    out.sort_by(|a, b| b.at.cmp(&a.at).then(b.seq.cmp(&a.seq)));
    if !NEWEST_FIRST {
        out.reverse();
    }
    out
}

/// The newest dump of `ds` (as [`dumps`] orders them), whichever end
/// [`NEWEST_FIRST`] puts it: the one the Ledger opens with.
pub fn newest(ds: &[Dump]) -> Option<u64> {
    if NEWEST_FIRST { ds.first() } else { ds.last() }.map(|d| d.seq)
}

#[cfg(test)]
thread_local! {
    /// How many times [`dumps`] ran on this thread (test-only; G9).
    pub(crate) static DUMPS_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Executions as a past dump shows them: every one the seed left running,
/// and every running step of a type 3, finished.
fn settled(es: &[Exec]) -> Vec<Exec> {
    es.iter()
        .map(|e| {
            let mut e = e.clone();
            if e.state == "running" {
                e.state = "done".into();
            }
            for st in &mut e.steps {
                if st.state == "running" {
                    st.state = "done".into();
                }
            }
            e.children = settled(&e.children);
            e
        })
        .collect()
}

/// A type as he asked for it: the number in a circle.
pub fn type_glyph(k: Kind) -> &'static str {
    match k {
        Kind::TypeOne => "①",
        Kind::TypeTwo => "②",
        Kind::TypeThree => "③",
    }
}

/// A duration as a row shows it: `320ms`, `4.1s`, `41s`, `2m 10s`, `1h 5m`.
pub fn took(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 10_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if ms < 60_000 {
        format!("{}s", ms / 1000)
    } else if ms < 3_600_000 {
        format!("{}m {}s", ms / 60_000, (ms / 1000) % 60)
    } else {
        format!("{}h {}m", ms / 3_600_000, (ms / 60_000) % 60)
    }
}

/// When something started, as a row shows it: `14:31:04` today,
/// `yesterday 14:31`, else its date and time.
pub fn when(t: Option<chrono::DateTime<chrono::Local>>) -> String {
    let Some(t) = t else { return "—".into() };
    let today = chrono::Local::now().date_naive();
    let d = t.date_naive();
    if d == today {
        t.format("%H:%M:%S").to_string()
    } else if Some(d) == today.pred_opt() {
        format!("yesterday {}", t.format("%H:%M"))
    } else {
        t.format("%b %-d %H:%M").to_string()
    }
}

/// [`when`] in fewer columns, for a band header with little room: `14:31`
/// today, else the day alone, `Oct 29`.
pub fn when_short(t: Option<chrono::DateTime<chrono::Local>>) -> String {
    let Some(t) = t else { return "—".into() };
    if t.date_naive() == chrono::Local::now().date_naive() {
        t.format("%H:%M").to_string()
    } else {
        t.format("%b %-d").to_string()
    }
}

/// When execution `e` of a dump made at `at` started.
pub fn started(at: Option<chrono::DateTime<chrono::Local>>, e: &Exec) -> Option<chrono::DateTime<chrono::Local>> {
    at.map(|t| t + chrono::Duration::milliseconds(e.after_ms as i64))
}

/// A path as a key: `0.2.1`.
pub fn path_key(path: &[usize]) -> String {
    path.iter().map(usize::to_string).collect::<Vec<_>>().join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec(ripple: &str, state: &str, children: Vec<Exec>) -> Exec {
        serde_json::from_value(serde_json::json!({"ripple": ripple, "kind": "type_three", "state": state, "ms": 41000})).map(|mut e: Exec| {
            e.steps = vec![Step { name: "go".into(), word: "native".into(), state: state.into(), ms: 0, line: String::new(), calls: vec![] }];
            e.children = children;
            e
        }).unwrap()
    }

    fn send(seq: u64, at: &str) -> Send {
        serde_json::from_value(serde_json::json!({"seq": seq, "at": at, "env": "prod", "from_cluster": "", "text": format!("dump {seq}"), "tags": [], "targets": []})).unwrap()
    }

    fn seed(dumps: Vec<(u64, Vec<Exec>)>) -> Seed {
        Seed { note: String::new(), dumps: dumps.into_iter().map(|(seq, ripples)| SeedDump { seq, at: "2026-10-29T23:43:00Z".into(), text: format!("seed {seq}"), ripples, thread: String::new() }).collect() }
    }

    fn states(d: &Dump) -> Vec<String> {
        let mut v: Vec<String> = d.all().iter().map(|e| e.state.clone()).collect();
        v.extend(d.all().iter().flat_map(|e| e.steps.iter().map(|s| s.state.clone())));
        v
    }

    /// Re-audit 10 D5: a dump the LOG dates never shows the seed's
    /// `running` — at any depth, nor on a type 3's steps — so neither its
    /// band nor its explore page can say `running` or `so far`.
    #[test]
    fn d5_a_dump_the_log_dates_shows_nothing_running() {
        let s = seed(vec![(11, vec![exec("dump", "running", vec![exec("apply", "running", vec![exec("author", "running", vec![])]), exec("fragment", "done", vec![])])])]);
        let ds = dumps(&s, &[send(11, "2026-10-29T23:43:00Z")]);
        assert_eq!(ds.len(), 1);
        let d = &ds[0];
        assert_eq!(d.text, "dump 11", "the log's words");
        assert_eq!(d.all().len(), 4, "every execution the seed has is still there");
        assert!(!states(d).iter().any(|s| s == "running"), "{:?}", states(d));
        assert_eq!(d.state(), "done");
        assert_eq!(d.exec_at(&[0, 0, 0]).unwrap().ms, 41000, "its duration is kept");
    }

    /// The opposition: only what was RUNNING is settled — a failed, parked
    /// or pending execution keeps its state — and a dump only the SEED
    /// knows keeps the seed's claim, running included.
    #[test]
    fn d5_settling_touches_running_alone_and_only_log_dated_dumps() {
        let s = seed(vec![
            (11, vec![exec("a", "failed", vec![]), exec("b", "parked", vec![]), exec("c", "pending", vec![])]),
            (7, vec![exec("dump", "running", vec![])]),
        ]);
        let ds = dumps(&s, &[send(11, "2026-10-29T23:43:00Z")]);
        let d11 = ds.iter().find(|d| d.seq == 11).unwrap();
        assert_eq!(d11.ripples.iter().map(|e| e.state.as_str()).collect::<Vec<_>>(), ["failed", "parked", "pending"]);
        assert_eq!(d11.state(), "failed");
        let d7 = ds.iter().find(|d| d.seq == 7).expect("a dump the seed has and the log lacks is on the ledger");
        assert_eq!(d7.text, "seed 7");
        assert_eq!(d7.state(), "running");
    }

    /// A dump the log has and the seed lacks: on the ledger, nothing run.
    #[test]
    fn d5_a_dump_the_seed_lacks_has_nothing_run() {
        let ds = dumps(&seed(vec![(11, vec![exec("dump", "running", vec![])])]), &[send(12, "2026-10-30T06:00:00Z"), send(11, "2026-10-29T23:43:00Z")]);
        assert_eq!(ds.iter().map(|d| d.seq).collect::<Vec<_>>(), [12, 11], "newest first");
        assert!(ds[0].ripples.is_empty());
        assert_eq!(ds[0].state(), "pending");
        assert_eq!(ds[1].state(), "done");
        assert!(dumps(&Seed::default(), &[]).is_empty());
    }

    /// V1.4: a seed dump's `thread` marks a FOLD and reaches the ledger's
    /// dump, from the seed alone or merged with the log; without it, a dump.
    #[test]
    fn v14_a_fold_carries_its_thread_to_the_ledger() {
        let mut s = seed(vec![(11, vec![]), (7, vec![])]);
        s.dumps[0].thread = "release notes".into();
        let ds = dumps(&s, &[send(11, "2026-10-29T23:43:00Z")]);
        assert_eq!(ds.iter().find(|d| d.seq == 11).unwrap().thread, "release notes", "merged with the log");
        assert_eq!(ds.iter().find(|d| d.seq == 7).unwrap().thread, "", "a dump is no fold");
        let only: Seed = serde_json::from_value(serde_json::json!({"dumps": [{"seq": 3, "at": "2026-10-29T23:43:00Z", "thread": "t"}]})).unwrap();
        assert_eq!(dumps(&only, &[])[0].thread, "t", "from the seed alone");
        assert!(serde_json::from_value::<Seed>(serde_json::json!({"dumps": [{"seq": 3, "at": "x", "threadd": "t"}]})).is_err(), "an unknown field is still refused");
    }

    /// V1.4: a block's words are the headline and at most ONE more
    /// non-empty, non-quote line.
    #[test]
    fn v14_words_are_the_headline_and_one_more_line() {
        let d = |t: &str| Dump { seq: 1, at: None, text: t.into(), ripples: vec![], thread: String::new() };
        assert_eq!(d("> [T-5]\nThe echo.\n\n> ref\nSecond.\nThird.").words(), ["The echo.", "Second."]);
        assert_eq!(d("Only.").words(), ["Only."]);
        assert!(d("").words().is_empty());
    }

    /// V1.4: a stepped type 3's phases are its steps with the children each
    /// called, then the children none called; anything else has none.
    #[test]
    fn v14_phases_are_steps_their_calls_and_the_rest() {
        let mut e = exec("dump", "done", vec![exec("a", "done", vec![]), exec("b", "done", vec![]), exec("c", "done", vec![]), exec("d", "done", vec![])]);
        e.steps = ["discover", "stage", "land"].iter().zip([vec![0], vec![1, 2, 9], vec![]]).map(|(n, calls)| Step { name: n.to_string(), word: "call".into(), state: "done".into(), ms: 0, line: String::new(), calls }).collect();
        let (steps, rest) = Dump::phases(&e).expect("a stepped type 3");
        assert_eq!(steps.iter().map(|(s, c)| (s.name.as_str(), c.clone())).collect::<Vec<_>>(), [("discover", vec![0]), ("stage", vec![1, 2]), ("land", vec![])], "out-of-range calls dropped");
        assert_eq!(rest, [3]);
        let mut two = e.clone();
        two.kind = Kind::TypeTwo;
        assert!(Dump::phases(&two).is_none(), "a type 2 has no phases");
        e.steps.clear();
        assert!(Dump::phases(&e).is_none(), "a type 3 with no steps has none");
    }

    /// V1.4 G9: one Status view of 200 dumps reads the ledger ONCE — the
    /// newest is computed once, never per block (it was `is_open` calling
    /// `dumps()` per band: quadratic, on every key).
    #[test]
    fn g9_one_status_view_of_200_dumps_reads_the_ledger_once() {
        use env::paths::Source;
        use env::Environment;
        let root = std::env::temp_dir().join(format!("v14-g9-{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("CAULDRON_TYPE_ONE", "off");
        let e = Environment { root: root.clone(), root_source: Source::CauldronHome, name: "prod".into(), name_source: Source::Default, data_root: root.join("data").join("prod") };
        let mut a = crate::cauldron::app::App::open(e, crate::cauldron::store::seed_dir(), true).expect("opens");
        a.sends.sends.clear();
        a.ledger = Seed { note: String::new(), dumps: (1..=200).map(|seq| SeedDump { seq, at: format!("2026-10-{:02}T{:02}:00:00Z", 1 + seq / 24, seq % 24), text: format!("dump {seq}"), ripples: vec![exec("dump", "done", vec![])], thread: String::new() }).collect() };
        a.set_ctab(crate::cauldron::app::CTab::Status);
        a.set_sttab(crate::cauldron::status::StatusTab::Ledger);
        DUMPS_CALLS.with(|c| c.set(0));
        let v = crate::cauldron::status::view(&a, 120);
        let calls = DUMPS_CALLS.with(|c| c.get());
        assert_eq!(v.items.iter().filter(|i| i.key.starts_with("ld:")).count(), 200, "all 200 drawn");
        assert_eq!(calls, 1, "the ledger read once per view, not once per dump");
    }
}
