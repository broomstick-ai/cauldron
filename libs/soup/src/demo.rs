//! **The curated `cauldron-build` environment**: its dataset and its loader.
//!
//! The dataset is cauldron's own development, scrubbed: its areas as clusters
//! (the face, the Cauldron tab, the ledger, the soup, ripples, the threads
//! daemon, the release and its two halves), the operator's directives and
//! constraints paraphrased, the objectives, milestones and tasks the build
//! actually ran on, open and assumed questions, human actions (one of them
//! multi-phase with masked, routed steps), and a ledger of dumps with their
//! phases, ripple trees, one rolled-back tranche and one thread fold. It is
//! authored in the FACE'S OWN SEED FORMATS, because the face reads it through
//! its own-seed path and nothing else (see the crate docs). The ledger follows
//! the V1 contract (an internal report): globs2's `ledger::Seed`, plus
//! `"thread"` on a dump entry that is a thread fold (V1.4.S1), with the kind
//! words `type_one`, `type_two`, `type_three` (V1.9).
//!
//! Files (`demo/cauldron-build/` in this crate, embedded at compile time):
//!
//! ```text
//! clusters.json   the cluster tree, its corpus (regions, nodes) and the cited messages
//! artifacts.json  artifact nodes, each wrapping a file under `{files}/`
//! ledger.json     the dumps and what ran for each
//! files/*         the artifact files themselves, opened with one click
//! ```
//!
//! `{files}` is a placeholder: [`install`] writes the files into the
//! environment's own seed and points every location at that copy, with the
//! file's real sha256 and size recorded as verified.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The one environment the curated data is ever installed into (the
/// teaser's step 6 switches to it).
pub const ENV: &str = "cauldron-build";

/// The placeholder for the installed files directory in `artifacts.json`.
pub const FILES_TOKEN: &str = "{files}";

/// The face's owner and kind directories under an environment's data root
/// (`apps/tui` `cauldron::store::{OWNER, KIND}`): `<data_root>/tui/soup`.
pub const OWNER: &str = "tui";
pub const KIND: &str = "soup";
/// The own-seed directory under the face's soup dir (`store::own_seed_dir`).
pub const SEED: &str = "seed";
/// The curated environment's secrets env file, under the face's soup dir.
pub const SECRETS_ENV: &str = "secrets.env";

/// One embedded file of the dataset.
#[derive(Debug, Clone, Copy)]
pub struct File {
    pub name: &'static str,
    pub text: &'static str,
}

macro_rules! file {
    ($name:literal) => {
        File { name: $name, text: include_str!(concat!("../demo/cauldron-build/", $name)) }
    };
}

/// The seed files, in the face's formats.
pub const CLUSTERS: File = file!("clusters.json");
pub const ARTIFACTS: File = file!("artifacts.json");
pub const LEDGER: File = file!("ledger.json");

/// The artifact files (`files/<name>`).
pub const FILES: &[File] = &[
    file!("files/ledger-v1-sketch.txt"),
    file!("files/runtime-assessment.md"),
    file!("files/teaser-sequence.md"),
    file!("files/install-notes.md"),
    file!("files/incident-log-wedge.md"),
];

/// Every embedded file, seed files first.
pub fn all_files() -> Vec<File> {
    let mut v = vec![CLUSTERS, ARTIFACTS, LEDGER];
    v.extend_from_slice(FILES);
    v
}

/// The dataset, parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct Dataset {
    pub clusters: Value,
    pub artifacts: Value,
    pub ledger: Value,
    /// The artifact files' names (`files/` stripped), as the dataset ships.
    pub files: BTreeSet<String>,
}

impl Dataset {
    /// The embedded dataset.
    /// Every human action in it passes [`crate::actions::validate_action`]:
    /// a curated action that could not be routed fails the load, loudly.
    pub fn embedded() -> Result<Dataset> {
        let ds = Dataset::parse(CLUSTERS.text, ARTIFACTS.text, LEDGER.text, FILES.iter().map(|f| f.name))?;
        ds.validate_actions()?;
        Ok(ds)
    }

    /// Refuse the dataset if any human action holds a field that cannot be
    /// routed (V1.8: such an action never enters the soup).
    pub fn validate_actions(&self) -> Result<()> {
        for c in clusters(self) {
            for h in arr(c, "human_actions") {
                crate::actions::validate_action(s(h, "body"))
                    .with_context(|| format!("cluster {} HA-{}", s(c, "id"), h.get("serial").and_then(Value::as_u64).unwrap_or(0)))?;
            }
        }
        Ok(())
    }

    /// A dataset from its three seed texts and its file names.
    pub fn parse<'a>(clusters: &str, artifacts: &str, ledger: &str, files: impl IntoIterator<Item = &'a str>) -> Result<Dataset> {
        Ok(Dataset {
            clusters: serde_json::from_str(clusters).context("parse clusters.json")?,
            artifacts: serde_json::from_str(artifacts).context("parse artifacts.json")?,
            ledger: serde_json::from_str(ledger).context("parse ledger.json")?,
            files: files.into_iter().map(|n| n.strip_prefix("files/").unwrap_or(n).to_string()).collect(),
        })
    }

    /// The kinds of thing this dataset shows (see [`Kind`]).
    pub fn census(&self) -> BTreeSet<Kind> {
        census(self)
    }
}

/// What the demo must visibly include (the brief's list, product spec §17).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// At least two clusters.
    Clusters,
    /// A cluster whose DESCENDANTS hold open questions and open actions, so
    /// the Aggregated clusters switch has something to roll up.
    AggregateCluster,
    /// An open question.
    Questions,
    /// A question answered by an assumption (the Assumed band).
    AssumedQuestion,
    /// An open human action.
    Actions,
    /// An open action with two or more phases.
    MultiPhaseAction,
    /// An open action with a masked field routed somewhere a secret may go
    /// (`env`, `env:<NAME>`, `keychain`).
    SecretAction,
    Directives,
    Constraints,
    Objectives,
    Milestones,
    /// Tasks, with at least one dependency (the Tasks rails).
    Tasks,
    /// A dump on the ledger, with its words and its dump ripple.
    Dumps,
    /// A dump ripple with its phases (two or more steps, each call resolving).
    LedgerPhases,
    /// A ripple tree three deep holding `type_one`, `type_two` and
    /// `type_three` (the kind words of V1.9; never splash, shell, aggregate).
    RippleTree,
    /// A thread fold as its own ledger row: a dump entry carrying `thread`
    /// (the conversation's display name, V1.4.S1) and the ripples it ran.
    ThreadFold,
    /// A tranche that failed and rolled back.
    RolledBackTranche,
    /// An artifact wrapping a shipped file, embedded in an open question.
    OpenFile,
}

impl Kind {
    pub const ALL: [Kind; 18] = [
        Kind::Clusters,
        Kind::AggregateCluster,
        Kind::Questions,
        Kind::AssumedQuestion,
        Kind::Actions,
        Kind::MultiPhaseAction,
        Kind::SecretAction,
        Kind::Directives,
        Kind::Constraints,
        Kind::Objectives,
        Kind::Milestones,
        Kind::Tasks,
        Kind::Dumps,
        Kind::LedgerPhases,
        Kind::RippleTree,
        Kind::ThreadFold,
        Kind::RolledBackTranche,
        Kind::OpenFile,
    ];
}

// ── the census ──────────────────────────────────────────────────────────────

fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Every cluster, at every depth, pre-order.
fn clusters(ds: &Dataset) -> Vec<&Value> {
    fn walk<'a>(cs: &'a [Value], out: &mut Vec<&'a Value>) {
        for c in cs {
            out.push(c);
            walk(arr(c, "children"), out);
        }
    }
    let mut out = Vec::new();
    walk(arr(&ds.clusters, "clusters"), &mut out);
    out
}

fn descendants(c: &Value) -> Vec<&Value> {
    let mut out = Vec::new();
    for ch in arr(c, "children") {
        out.push(ch);
        out.extend(descendants(ch));
    }
    out
}

fn open_actions(c: &Value) -> impl Iterator<Item = &Value> {
    arr(c, "human_actions").iter().filter(|h| s(h, "status") == "open")
}

/// One phase per section of a human action's body (see [`crate::actions::phases`]).
pub use crate::actions::phases;

/// An open action's field that is secret AND routed somewhere it may go.
fn is_secret_routed(field: &crate::actions::Fence) -> bool {
    crate::actions::is_secret(field) && matches!(crate::actions::route_of(field), crate::actions::Route::Env(_) | crate::actions::Route::Keychain)
}

fn ledger_execs(ds: &Dataset) -> Vec<(&Value, usize)> {
    fn walk<'a>(es: &'a [Value], depth: usize, out: &mut Vec<(&'a Value, usize)>) {
        for e in es {
            out.push((e, depth));
            walk(arr(e, "children"), depth + 1, out);
        }
    }
    let mut out = Vec::new();
    for d in arr(&ds.ledger, "dumps") {
        walk(arr(d, "ripples"), 1, &mut out);
    }
    out
}

fn census(ds: &Dataset) -> BTreeSet<Kind> {
    let mut k = BTreeSet::new();
    let cs = clusters(ds);
    if cs.len() >= 2 {
        k.insert(Kind::Clusters);
    }
    if cs.iter().any(|c| {
        let d = descendants(c);
        d.iter().any(|x| !arr(x, "questions").is_empty()) && d.iter().any(|x| open_actions(x).next().is_some())
    }) {
        k.insert(Kind::AggregateCluster);
    }
    let any = |key: &str| cs.iter().any(|c| !arr(c, key).is_empty());
    if any("questions") {
        k.insert(Kind::Questions);
    }
    if cs.iter().any(|c| arr(c, "closed_questions").iter().any(|q| s(q, "status") == "assumed")) {
        k.insert(Kind::AssumedQuestion);
    }
    let actions: Vec<&Value> = cs.iter().flat_map(|c| open_actions(c)).collect();
    if !actions.is_empty() {
        k.insert(Kind::Actions);
    }
    if actions.iter().any(|h| phases(s(h, "body")).len() >= 2) {
        k.insert(Kind::MultiPhaseAction);
    }
    if actions.iter().any(|h| phases(s(h, "body")).iter().flatten().any(|f| is_secret_routed(f))) {
        k.insert(Kind::SecretAction);
    }
    for (key, kind) in [
        ("directives", Kind::Directives),
        ("constraints", Kind::Constraints),
        ("objectives", Kind::Objectives),
        ("milestones", Kind::Milestones),
    ] {
        if any(key) {
            k.insert(kind);
        }
    }
    if cs.iter().any(|c| arr(c, "tasks").iter().any(|t| !arr(t, "depends_on").is_empty())) {
        k.insert(Kind::Tasks);
    }

    let dumps = arr(&ds.ledger, "dumps");
    if dumps.iter().any(|d| !s(d, "text").is_empty() && arr(d, "ripples").iter().any(|e| s(e, "ripple") == "dump")) {
        k.insert(Kind::Dumps);
    }
    let execs = ledger_execs(ds);
    if execs.iter().any(|(e, _)| {
        let steps = arr(e, "steps");
        let n = arr(e, "children").len();
        s(e, "ripple") == "dump"
            && s(e, "kind") == "type_three"
            && steps.len() >= 2
            && steps.iter().all(|st| arr(st, "calls").iter().all(|c| c.as_u64().is_some_and(|i| (i as usize) < n)))
    }) {
        k.insert(Kind::LedgerPhases);
    }
    for d in dumps {
        let mut sub = Vec::new();
        fn walk<'a>(es: &'a [Value], depth: usize, out: &mut Vec<(&'a Value, usize)>) {
            for e in es {
                out.push((e, depth));
                walk(arr(e, "children"), depth + 1, out);
            }
        }
        walk(arr(d, "ripples"), 1, &mut sub);
        let kinds: BTreeSet<&str> = sub.iter().map(|(e, _)| s(e, "kind")).collect();
        if sub.iter().any(|(_, depth)| *depth >= 3) && ["type_one", "type_two", "type_three"].iter().all(|x| kinds.contains(x)) {
            k.insert(Kind::RippleTree);
        }
        if !s(d, "thread").is_empty() && !arr(d, "ripples").is_empty() {
            k.insert(Kind::ThreadFold);
        }
    }
    if execs.iter().any(|(e, _)| {
        s(e, "ripple") == "tranche" && s(e, "state") == "failed" && arr(e, "steps").iter().any(|st| s(st, "name") == "rollback" && s(st, "state") == "done")
    }) {
        k.insert(Kind::RolledBackTranche);
    }

    let shipped: BTreeSet<&str> = arr(&ds.artifacts, "artifacts")
        .iter()
        .filter(|a| {
            s(a, "location").strip_prefix(FILES_TOKEN).and_then(|r| r.strip_prefix('/')).is_some_and(|f| ds.files.contains(f))
        })
        .map(|a| s(a, "id"))
        .collect();
    if cs.iter().any(|c| arr(c, "questions").iter().any(|q| arr(q, "artifacts").iter().any(|a| a.as_str().is_some_and(|a| shipped.contains(a))))) {
        k.insert(Kind::OpenFile);
    }
    k
}

// ── the loader ──────────────────────────────────────────────────────────────

/// What [`install`] wrote.
#[derive(Debug, Clone)]
pub struct Installed {
    /// `<soup dir>/seed`.
    pub seed_dir: PathBuf,
    /// Every file written, in order (the artifact files, then the seed files,
    /// `clusters.json` last).
    pub wrote: Vec<PathBuf>,
    /// The environment's `secrets.env_file` (V1.8): where the curated secret
    /// steps' `route: env` values go, inside this environment's own data dir.
    pub env_file: PathBuf,
}

/// `<data_root>/tui/soup` of `env`, through the env crate's guard.
pub fn soup_dir(env: &::env::Environment) -> Result<PathBuf> {
    Ok(env.service_data(OWNER)?.join(KIND))
}

/// **The env-creation hook**: seed environment `env` with the curated
/// dataset. Refused for any environment but [`ENV`]: no other environment
/// ever shows seed data (product spec §17).
pub fn seed_env(env: &::env::Environment) -> Result<Installed> {
    if env.name != ENV {
        bail!(
            "REFUSED: the curated dataset is installed only into `{ENV}`, never into `{}`: \
             no other environment shows seed data",
            env.name
        );
    }
    install(&soup_dir(env)?)
}

/// Install the dataset into `soup_dir` (a face's `<data_root>/tui/soup`):
/// the artifact files into `seed/files/`, then `artifacts.json` (locations
/// pointed at those files, each verified with its sha256 and size),
/// `ledger.json`, and `clusters.json` LAST, so a face that sees the clusters
/// sees everything they refer to. Every file is written whole and atomically.
/// Re-running replaces the seed with the dataset (it is idempotent).
///
/// The dataset is validated FIRST ([`Dataset::validate_actions`]): a human
/// action that could not be routed refuses the install before anything,
/// not even the seed directory, is written. This holds outside tests: the
/// CLI's `env create` reaches the data only through here.
pub fn install(soup_dir: &Path) -> Result<Installed> {
    install_from(soup_dir, &Source::EMBEDDED)
}

/// The texts [`install_from`] installs: the embedded dataset
/// ([`Source::EMBEDDED`]), or a variant of it under test.
#[derive(Debug, Clone, Copy)]
pub struct Source<'a> {
    pub clusters: &'a str,
    pub artifacts: &'a str,
    pub ledger: &'a str,
    pub files: &'a [File],
}

impl Source<'static> {
    /// The dataset compiled into this crate.
    pub const EMBEDDED: Source<'static> = Source { clusters: CLUSTERS.text, artifacts: ARTIFACTS.text, ledger: LEDGER.text, files: FILES };
}

/// [`install`], from `src`. Refuses, writing nothing, unless every human
/// action in `src` passes [`crate::actions::validate_action`].
pub fn install_from(soup_dir: &Path, src: &Source<'_>) -> Result<Installed> {
    Dataset::parse(src.clusters, src.artifacts, src.ledger, src.files.iter().map(|f| f.name))?
        .validate_actions()
        .context("REFUSED: the dataset holds a human action that cannot be routed; nothing was installed")?;
    let seed = soup_dir.join(SEED);
    let files_dir = seed.join("files");
    std::fs::create_dir_all(&files_dir).with_context(|| format!("create {}", files_dir.display()))?;
    let files_str = files_dir.to_str().context("the environment's files dir is not UTF-8")?;
    let mut wrote = Vec::new();

    for f in src.files {
        let name = f.name.strip_prefix("files/").unwrap_or(f.name);
        let p = files_dir.join(name);
        write_atomic(&p, f.text)?;
        wrote.push(p);
    }

    // Verified AFTER the files are written: the face calls an artifact whose
    // file is newer than its verification stale.
    let at = now_rfc3339();
    let mut arts: Value = serde_json::from_str(src.artifacts).context("parse artifacts.json")?;
    for d in arts.get_mut("directories").and_then(Value::as_array_mut).into_iter().flatten() {
        if let Some(p) = d.get_mut("path") {
            let replaced = p.as_str().unwrap_or("").replace(FILES_TOKEN, files_str);
            *p = Value::String(replaced);
        }
    }
    for a in arts.get_mut("artifacts").and_then(Value::as_array_mut).into_iter().flatten() {
        let loc = a.get("location").and_then(Value::as_str).unwrap_or("").to_string();
        let Some(rel) = loc.strip_prefix(FILES_TOKEN).and_then(|r| r.strip_prefix('/')) else {
            bail!("artifact {}: location `{loc}` is not under {FILES_TOKEN}/", s(a, "id"));
        };
        let file = src.files
            .iter()
            .find(|f| f.name.strip_prefix("files/") == Some(rel))
            .with_context(|| format!("artifact {}: `{rel}` is not a file the dataset ships", s(a, "id")))?;
        let bytes = file.text.as_bytes();
        let sha = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect::<String>();
        let obj = a.as_object_mut().context("an artifact is an object")?;
        obj.insert("location".into(), Value::String(format!("{files_str}/{rel}")));
        obj.insert("verified".into(), serde_json::json!({ "sha256": sha, "bytes": bytes.len(), "at": at }));
    }
    let p = seed.join("artifacts.json");
    write_atomic(&p, &(serde_json::to_string_pretty(&arts)? + "\n"))?;
    wrote.push(p);

    let p = seed.join("ledger.json");
    write_atomic(&p, src.ledger)?;
    wrote.push(p);

    // V1.8: the curated secret steps route `env`, and an `env` route with no
    // `secrets.env_file` is refused at Submit. The env file lives inside this
    // environment's own data dir, so the teaser's Submit writes a throwaway
    // file and never touches a keychain. A settings file that already has a
    // `[secrets]` table is his and is left alone.
    let env_file = soup_dir.join(SECRETS_ENV);
    let settings = soup_dir.join("settings.toml");
    let have = match std::fs::read_to_string(&settings) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("read {}", settings.display())),
    };
    if !have.as_deref().is_some_and(|t| t.lines().any(|l| l.trim() == "[secrets]")) {
        let path = serde_json::to_string(env_file.to_str().context("the environment's data dir is not UTF-8")?)?;
        let mut text = have.unwrap_or_default();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("\n[secrets]\nenv_file = {path}\n"));
        write_atomic(&settings, &text)?;
        wrote.push(settings);
    }

    let p = seed.join("clusters.json");
    write_atomic(&p, src.clusters)?;
    wrote.push(p);

    Ok(Installed { seed_dir: seed, wrote, env_file })
}

impl Installed {
    /// `<seed>/files`.
    pub fn files_dir(&self) -> PathBuf {
        self.seed_dir.join("files")
    }
}

fn now_rfc3339() -> String {
    // Seconds since the epoch, as a civil UTC instant (no clock crate here).
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Write `text` to `path` whole: a uniquely named temp file beside it, then
/// `rename` over it (atomic on one filesystem).
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let tmp = dir.join(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
    std::fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename {} over {}", tmp.display(), path.display()))?;
    Ok(())
}
