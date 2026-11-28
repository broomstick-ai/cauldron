//! **Who may act for the owner**: the one decision every owner act asks.
//!
//! Five acts belong to a cauldron's owner:
//!
//! | act | the commands (and tools) |
//! |---|---|
//! | `grant` | `cauldron mcp grant` |
//! | `approve` | `cauldron soup approve` (`--unapprove`), `cauldron ripples approve | reject` (`--unapprove`, `--unreject`), the MCP tools `soup_approve` and `soup_reject` |
//! | `ripples` | `cauldron ripples on` |
//! | `merge` | `cauldron merge` of a source whose records carry decisions (approved or rejected) into the target, with those decisions |
//! | `grammar` | `cauldron soup grammar add`, `grammar rebind --yes`, `grammar remove --yes` |
//!
//! A command run with its input and output on a terminal may always do
//! them. Anything else (an agent's shell, a script, CI, a server, a Claude
//! Code session through the MCP door) may do one only when the owner has
//! chosen to let it. The terminal check stops an agent that follows the
//! rules; it is not a security boundary against a process of the same user
//! that fakes a terminal (`script(1)` gives any command one) or reads the
//! user's files (the service's approval key among them). The boundary is
//! the user account; `by: "tty"` means "run with a terminal attached", not
//! "a person":
//!
//! - **the setting** (`owner_acts`): `<root>/owner-acts.json`, one choice for
//!   the whole root and, optionally, one per cauldron that wins over it
//!   where it is set. A choice is the list of acts that are open: none
//!   (`terminal`, the DEFAULT: no file, or no choice, means exactly the
//!   behaviour of every release before this one), all of them (`open`), or
//!   some of them. A choice is stored as the list of acts it opened, so a
//!   choice recorded as `open` by a release that knew three acts opens
//!   those three, and `merge` stays at a terminal until it is chosen; a
//!   release that does not know an act ignores it in the file. Each choice records when it was set and how
//!   ([`How`]). A cauldron's choice is bound to the cauldron's id, as its
//!   MCP grant is: a cauldron removed and made again under the name, or a
//!   choice copied from another, does not carry it (it is void, and the
//!   root's choice applies). Written by `cauldron owner-acts` and by
//!   `scripts/install --owner-acts`;
//! - **the environment variable** [`VAR`] (`CAULDRON_OWNER_ACTS=open`, or a
//!   comma list of acts) in the environment of the process that acts. It
//!   opens those acts for that process only, whatever the setting says,
//!   and it lets `cauldron owner-acts` persist a choice without a
//!   terminal. Anything that can set its own environment, an agent told to
//!   included, can therefore turn it on: that is the point. The owner
//!   decides whether their agents may act for them, by putting it (or not)
//!   in a shell profile, in their user settings for Claude Code (never a
//!   project's `.claude/settings.json`, which a cloned repository carries),
//!   in CI or on a server.
//!   It is read ONLY by the cauldron command that performs the act: no
//!   service inherits it (`cauldron start` and the supervisor remove it
//!   from every service's environment), so it never opens the MCP door's
//!   `soup_approve` / `soup_reject` to sessions. That is the persisted
//!   setting's alone ([`sessions_view`]).
//!
//! [`decide`] is the one decision: it answers who is acting ([`By::Tty`] or
//! [`By::Agent`], with where the permission came from) or why not. Every
//! act records the answer (`by: "tty" | "agent"`).
//!
//! **Two limits no setting changes.** A ripple never approves or rejects
//! (its own output must not read as reviewed: the spine's flag rule refuses
//! the review flags to every ripple, and the door refuses the ripples
//! service's identity), and no write path sets the review flags or a
//! ripple's provenance directly.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The environment variable that opens owner acts for the process that
/// carries it.
pub const VAR: &str = "CAULDRON_OWNER_ACTS";

/// The setting's file, in the root.
pub const FILE: &str = "owner-acts.json";

/// The setting file's format.
pub const VERSION: u32 = 1;

/// One owner act.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Act {
    /// `cauldron mcp grant`: open a cauldron to the MCP door.
    Grant,
    /// Approving or rejecting what a ripple wrote (and taking either back).
    Approve,
    /// `cauldron ripples on`: automatic processing, model spend included.
    Ripples,
    /// `cauldron merge` carrying the source's decisions (approved or
    /// rejected) into the target.
    Merge,
    /// `cauldron soup grammar add`: a user grammar (an ontology) becomes a
    /// pool of the cauldron, which changes what its soup accepts.
    Grammar,
}

impl Act {
    pub const ALL: [Act; 5] = [Act::Grant, Act::Approve, Act::Ripples, Act::Merge, Act::Grammar];

    pub fn as_str(self) -> &'static str {
        match self {
            Act::Grant => "grant",
            Act::Approve => "approve",
            Act::Ripples => "ripples",
            Act::Merge => "merge",
            Act::Grammar => "grammar",
        }
    }

    pub fn parse(s: &str) -> Option<Act> {
        Act::ALL.into_iter().find(|a| a.as_str() == s)
    }

    /// What it does, in words, for a refusal or a listing.
    pub fn what(self) -> &'static str {
        match self {
            Act::Grant => "opening a cauldron to the MCP door (`cauldron mcp grant`)",
            Act::Approve => "approving and rejecting what a ripple wrote (`cauldron soup approve`, `cauldron ripples approve | reject`, the MCP tools `soup_approve` and `soup_reject`)",
            Act::Ripples => "turning automatic processing on (`cauldron ripples on`), which starts model spend",
            Act::Merge => "merging a cauldron whose records carry decisions (approved or rejected) into another, with those decisions (`cauldron merge`)",
            Act::Grammar => "registering a user grammar as a new pool of a cauldron (`cauldron soup grammar add`)",
        }
    }
}

impl std::fmt::Display for Act {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Who did an act.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    /// A command run with a terminal attached (its input and output were a
    /// terminal). Not proof of a person: a process can fake a terminal.
    Tty,
    /// Something without a terminal, allowed by the owner's choice.
    Agent,
}

impl By {
    pub fn as_str(self) -> &'static str {
        match self {
            By::Tty => "tty",
            By::Agent => "agent",
        }
    }

    pub fn parse(s: &str) -> Option<By> {
        match s {
            "tty" => Some(By::Tty),
            "agent" => Some(By::Agent),
            _ => None,
        }
    }
}

impl std::fmt::Display for By {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a choice was set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum How {
    /// `cauldron owner-acts`, run with a terminal attached.
    Tty,
    /// `cauldron owner-acts`, run without a terminal under [`VAR`].
    Env,
    /// `cauldron install --owner-acts` (which `scripts/install --owner-acts`
    /// runs). Recorded by that code path; no argument sets it.
    Install,
}

impl How {
    pub fn as_str(self) -> &'static str {
        match self {
            How::Tty => "tty",
            How::Env => "env",
            How::Install => "install",
        }
    }
}

/// A list of acts, written the way a person writes it: `open` (all of them),
/// `terminal` (none), or a comma list (`grant,approve`). An unknown word is
/// an error naming it.
pub fn parse_acts(v: &str) -> Result<Vec<Act>, String> {
    let v = v.trim();
    match v {
        "open" | "all" => return Ok(Act::ALL.to_vec()),
        "terminal" | "none" | "" => return Ok(vec![]),
        _ => {}
    }
    let mut out = Vec::new();
    for w in v.split(',').map(str::trim).filter(|w| !w.is_empty()) {
        match Act::parse(w) {
            Some(a) if !out.contains(&a) => out.push(a),
            Some(_) => {}
            None => {
                return Err(format!(
                    "`{w}` is not an owner act: the acts are grant, approve, ripples, merge and grammar (or `open` for all of them, `terminal` for none)"
                ))
            }
        }
    }
    out.sort();
    Ok(out)
}

/// The acts [`VAR`] opens, from its value (`None`: not set, opens nothing).
/// A value that does not parse is an error naming the variable.
pub fn from_var(value: Option<&str>) -> Result<Vec<Act>, String> {
    match value {
        None => Ok(vec![]),
        Some(v) => parse_acts(v).map_err(|e| format!("{VAR}={v}: {e}")),
    }
}

/// [`from_var`] of this process's own environment.
pub fn from_process() -> Result<Vec<Act>, String> {
    from_var(std::env::var(VAR).ok().as_deref())
}

/// The words a list of acts is shown as: `terminal`, `open`, or the list.
pub fn words(acts: &[Act]) -> String {
    if acts.is_empty() {
        "terminal".into()
    } else if Act::ALL.iter().all(|a| acts.contains(a)) {
        "open".into()
    } else {
        acts.iter().map(|a| a.as_str()).collect::<Vec<_>>().join(",")
    }
}

/// One choice: which acts are open, and when and how it was set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    /// The open acts (`grant`, `approve`, `ripples`, `merge`); empty is `terminal`.
    pub open: Vec<String>,
    /// When it was set (UTC, RFC 3339).
    pub set_at: String,
    /// How it was set.
    pub how: How,
}

impl Choice {
    /// Its acts; a word this build does not know is left out (it opens
    /// nothing here).
    pub fn acts(&self) -> Vec<Act> {
        let mut v: Vec<Act> = self.open.iter().filter_map(|w| Act::parse(w)).collect();
        v.sort();
        v.dedup();
        v
    }

    pub fn new(acts: &[Act], how: How) -> Choice {
        let mut open: Vec<String> = acts.iter().map(|a| a.as_str().to_string()).collect();
        open.sort();
        open.dedup();
        Choice { open, set_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true), how }
    }
}

/// One cauldron's choice, which wins over the root's where it is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CauldronChoice {
    pub env: String,
    pub cauldron: String,
    /// The cauldron's id when the choice was made: a cauldron with another
    /// id (made again under the name) does not carry it.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(flatten)]
    pub choice: Choice,
}

/// The whole setting, as the file holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Setting {
    pub version: u32,
    /// The root's choice (`None`: `terminal`, never chosen).
    #[serde(default)]
    pub root: Option<Choice>,
    #[serde(default)]
    pub cauldrons: Vec<CauldronChoice>,
}

impl Default for Setting {
    fn default() -> Self {
        Setting { version: VERSION, root: None, cauldrons: vec![] }
    }
}

/// The setting file of `root`.
pub fn path(root: &Path) -> PathBuf {
    root.join(FILE)
}

/// Read the setting of `root`. No file is the default (`terminal`
/// everywhere): a root from before this setting existed reads so.
pub fn load(root: &Path) -> std::io::Result<Setting> {
    let p = path(root);
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Setting::default()),
        Err(e) => return Err(e),
    };
    let s: Setting = serde_json::from_str(&text).map_err(|e| std::io::Error::other(format!("{} is not an owner-acts setting: {e}", p.display())))?;
    if s.version > VERSION {
        return Err(std::io::Error::other(format!(
            "{} is format {} (this cauldron reads {VERSION}): it was written by a later cauldron",
            p.display(),
            s.version
        )));
    }
    Ok(s)
}

/// Write the setting of `root`: a new file (0600), synced, renamed over the
/// old one.
pub fn store(root: &Path, s: &Setting) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|x| x.as_nanos()).unwrap_or(0);
    let tmp = root.join(format!(".{FILE}.{}-{nanos}", std::process::id()));
    {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(crate::private::FILE_MODE).open(&tmp)?;
        let text = serde_json::to_string_pretty(s).map_err(std::io::Error::other)?;
        f.write_all(text.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    if let Err(e) = std::fs::rename(&tmp, path(root)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::File::open(root)?.sync_all()
}

/// Where an effective choice came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Nothing was chosen: `terminal`.
    Default,
    /// The root's choice.
    Root,
    /// The cauldron's own choice.
    Cauldron,
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Default => "default",
            Source::Root => "root",
            Source::Cauldron => "cauldron",
        }
    }
}

/// The choice that applies to one cauldron, and where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Effective {
    pub open: Vec<Act>,
    pub source: Source,
    pub choice: Option<Choice>,
    /// A choice of this cauldron's that does not apply (and why).
    pub void: Option<String>,
}

impl Setting {
    /// The cauldron's own entry, if any.
    pub fn entry(&self, env: &str, cauldron: &str) -> Option<&CauldronChoice> {
        self.cauldrons.iter().find(|c| c.env == env && c.cauldron == cauldron)
    }

    /// The choice that applies to cauldron `cauldron` (id `id`, as it is
    /// now) of environment `env`: its own, when it is set and bound to this
    /// very cauldron; else the root's; else `terminal`.
    pub fn effective(&self, env: &str, cauldron: &str, id: Option<&str>) -> Effective {
        let mut void = None;
        if let Some(c) = self.entry(env, cauldron) {
            match (&c.id, id) {
                (Some(want), Some(have)) if want != have => {
                    void = Some(format!(
                        "the cauldron's own choice was made for another cauldron of that name (id {want}, not {have}); it does not apply"
                    ))
                }
                (Some(_), None) => void = Some("the cauldron's own choice was made for a cauldron with an id, and this one has none; it does not apply".into()),
                _ => return Effective { open: c.choice.acts(), source: Source::Cauldron, choice: Some(c.choice.clone()), void: None },
            }
        }
        match &self.root {
            Some(r) => Effective { open: r.acts(), source: Source::Root, choice: Some(r.clone()), void },
            None => Effective { open: vec![], source: Source::Default, choice: None, void },
        }
    }

    /// Set the root's choice.
    pub fn set_root(&mut self, c: Choice) {
        self.version = VERSION;
        self.root = Some(c);
    }

    /// Set one cauldron's choice (replacing any it had).
    pub fn set_cauldron(&mut self, env: &str, cauldron: &str, id: Option<String>, c: Choice) {
        self.version = VERSION;
        self.cauldrons.retain(|x| !(x.env == env && x.cauldron == cauldron));
        self.cauldrons.push(CauldronChoice { env: env.into(), cauldron: cauldron.into(), id, choice: c });
        self.cauldrons.sort_by(|a, b| (&a.env, &a.cauldron).cmp(&(&b.env, &b.cauldron)));
    }

    /// Drop one cauldron's choice, so the root's applies. Was there one?
    pub fn unset_cauldron(&mut self, env: &str, cauldron: &str) -> bool {
        let n = self.cauldrons.len();
        self.cauldrons.retain(|x| !(x.env == env && x.cauldron == cauldron));
        n != self.cauldrons.len()
    }
}

/// Where an allowed act's permission came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Allowed {
    /// A command run with a terminal attached: always.
    Terminal,
    /// [`VAR`] in the acting process's environment.
    Var,
    /// The setting (the root's choice or the cauldron's).
    Setting(Source),
}

impl Allowed {
    pub fn by(&self) -> By {
        match self {
            Allowed::Terminal => By::Tty,
            _ => By::Agent,
        }
    }

    /// Where it came from, in words.
    pub fn said(&self) -> String {
        match self {
            Allowed::Terminal => "a command run with a terminal attached".into(),
            Allowed::Var => format!("{VAR} in this process's environment"),
            Allowed::Setting(Source::Cauldron) => "the cauldron's owner_acts setting".into(),
            Allowed::Setting(_) => "the root's owner_acts setting".into(),
        }
    }
}

/// Why an act was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// One sentence on how the owner allows it, to append to the act's own
    /// refusal.
    pub how_to_allow: String,
    /// The variable's value could not be read (said in the refusal too).
    pub bad_var: Option<String>,
}

/// The sentence that says how the owner lets `act` run without a terminal.
pub fn how_to_allow(act: Act, env: &str, cauldron: &str) -> String {
    format!(
        "The owner can let this run without a terminal: `cauldron owner-acts open --only {a} --cauldron {cauldron} --env {env}` \
         run at a terminal (without --cauldron, for every cauldron; `cauldron owner-acts help`), or {VAR}={a} (or `open`) in \
         the environment of the process that runs it.",
        a = act.as_str()
    )
}

/// **THE DECISION**: may `act` run on cauldron `cauldron` (id `id`) of
/// environment `env`, for a process that is (`at_terminal`) or is not at an
/// interactive terminal, whose [`VAR`] is `var`, under `setting`?
///
/// At a terminal: always, [`By::Tty`]. Otherwise [`By::Agent`] when the
/// variable opens the act, or the effective choice does; else refused,
/// with the sentence that says how the owner allows it.
pub fn decide(act: Act, setting: &Setting, env: &str, cauldron: &str, id: Option<&str>, at_terminal: bool, var: Option<&str>) -> Result<Allowed, Refused> {
    if at_terminal {
        return Ok(Allowed::Terminal);
    }
    let mut bad_var = None;
    match from_var(var) {
        Ok(v) if v.contains(&act) => return Ok(Allowed::Var),
        Ok(_) => {}
        Err(e) => bad_var = Some(e),
    }
    let eff = setting.effective(env, cauldron, id);
    if eff.open.contains(&act) {
        return Ok(Allowed::Setting(eff.source));
    }
    Err(Refused { how_to_allow: how_to_allow(act, env, cauldron), bad_var })
}

/// What MCP sessions may do through the door, for one cauldron: `approve`
/// (the tools `soup_approve` and `soup_reject`) open or not, from the
/// PERSISTED setting alone. [`VAR`] never reaches the mcp service, so it is
/// not part of this answer, whatever this process's environment says.
pub fn sessions_view(setting: &Setting, env: &str, cauldron: &str, id: Option<&str>) -> Value {
    let eff = setting.effective(env, cauldron, id);
    let open = eff.open.contains(&Act::Approve);
    json!({
        "approve": if open { "open" } else { "terminal" },
        "from": if open { eff.source.as_str() } else { "default" },
        "note": SESSIONS_NOTE,
    })
}

/// What [`sessions_view`] means, said with it.
pub const SESSIONS_NOTE: &str = "MCP sessions (soup_approve, soup_reject through the door) follow the persisted setting only; \
     CAULDRON_OWNER_ACTS never reaches the mcp service, so it opens nothing for a session";

/// What `acts` means, said with it.
pub const CLI_NOTE: &str = "`acts`: a cauldron command run from this process without a terminal (the persisted setting, plus \
     CAULDRON_OWNER_ACTS in this process's environment, marked `env`); a person at a terminal may always act";

/// The effective setting of one cauldron, as `status` views show it: per
/// act whether it is open and from where for a command run from this
/// process (`acts`, [`VAR`] included, marked `env`), and separately what
/// MCP sessions may do (`sessions`: the persisted setting only).
pub fn view(setting: &Setting, env: &str, cauldron: &str, id: Option<&str>, var: Option<&str>) -> Value {
    let sessions = sessions_view(setting, env, cauldron, id);
    let eff = setting.effective(env, cauldron, id);
    let var_acts = from_var(var);
    let mut acts = BTreeMap::new();
    for a in Act::ALL {
        let from = if eff.open.contains(&a) {
            eff.source.as_str().to_string()
        } else if var_acts.as_ref().is_ok_and(|v| v.contains(&a)) {
            "env".into()
        } else {
            "terminal".into()
        };
        acts.insert(a.as_str().to_string(), json!(from));
    }
    let mut v = json!({
        "setting": words(&eff.open),
        "from": eff.source.as_str(),
        "acts": acts,
        "acts_note": CLI_NOTE,
        "sessions": sessions,
    });
    if let Some(c) = &eff.choice {
        v["set_at"] = json!(c.set_at);
        v["how"] = json!(c.how.as_str());
    }
    if let Some(why) = &eff.void {
        v["void"] = json!(why);
    }
    match var_acts {
        Ok(a) if !a.is_empty() => v["env_var"] = json!({"name": VAR, "opens": words(&a)}),
        Err(e) => v["env_var"] = json!({"name": VAR, "error": e}),
        _ => {}
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acts_parse_as_a_person_writes_them() {
        assert_eq!(parse_acts("open").unwrap(), Act::ALL.to_vec());
        assert_eq!(parse_acts("terminal").unwrap(), vec![]);
        assert_eq!(parse_acts("ripples, grant").unwrap(), vec![Act::Grant, Act::Ripples]);
        assert!(parse_acts("grant,mergeall").unwrap_err().contains("`mergeall` is not an owner act"));
        assert_eq!(parse_acts("merge").unwrap(), vec![Act::Merge]);
        assert_eq!(words(&[Act::Approve]), "approve");
        assert_eq!(words(&Act::ALL), "open");
        assert_eq!(words(&[]), "terminal");
    }

    #[test]
    fn the_default_is_terminal_and_a_terminal_is_always_allowed() {
        let s = Setting::default();
        for a in Act::ALL {
            assert_eq!(decide(a, &s, "dev", "kb", None, true, None), Ok(Allowed::Terminal));
            let r = decide(a, &s, "dev", "kb", None, false, None).unwrap_err();
            assert!(r.how_to_allow.contains("cauldron owner-acts open") && r.how_to_allow.contains(VAR), "{}", r.how_to_allow);
        }
    }

    #[test]
    fn a_choice_made_before_merge_was_an_act_keeps_merge_at_a_terminal() {
        // A root a release with three acts recorded as `open`: the file lists
        // those three, so `merge` stays at a terminal until it is chosen.
        let text = r#"{"version":1,"root":{"open":["approve","grant","ripples"],"set_at":"2026-11-20T00:00:00Z","how":"tty"},"cauldrons":[]}"#;
        let s: Setting = serde_json::from_str(text).unwrap();
        assert_eq!(decide(Act::Approve, &s, "dev", "kb", None, false, None), Ok(Allowed::Setting(Source::Root)));
        assert!(decide(Act::Merge, &s, "dev", "kb", None, false, None).is_err());
        assert_eq!(decide(Act::Merge, &s, "dev", "kb", None, false, Some("merge")), Ok(Allowed::Var));
        assert_eq!(decide(Act::Merge, &s, "dev", "kb", None, true, None), Ok(Allowed::Terminal));
        // A later act this build does not know is ignored, not an error.
        let later = r#"{"version":1,"root":{"open":["merge","fly"],"set_at":"2026-11-20T00:00:00Z","how":"tty"},"cauldrons":[]}"#;
        let s: Setting = serde_json::from_str(later).unwrap();
        assert_eq!(s.root.as_ref().unwrap().acts(), vec![Act::Merge]);
    }

    #[test]
    fn the_variable_opens_only_what_it_lists() {
        let s = Setting::default();
        assert_eq!(decide(Act::Approve, &s, "dev", "kb", None, false, Some("approve")), Ok(Allowed::Var));
        assert!(decide(Act::Grant, &s, "dev", "kb", None, false, Some("approve")).is_err());
        assert_eq!(decide(Act::Grant, &s, "dev", "kb", None, false, Some("open")), Ok(Allowed::Var));
        let bad = decide(Act::Grant, &s, "dev", "kb", None, false, Some("yes")).unwrap_err();
        assert!(bad.bad_var.unwrap().contains("CAULDRON_OWNER_ACTS=yes"));
    }

    #[test]
    fn a_cauldrons_choice_wins_both_ways_and_binds_to_its_id() {
        let mut s = Setting::default();
        s.set_root(Choice::new(&Act::ALL, How::Tty));
        s.set_cauldron("dev", "kb", Some("aa".into()), Choice::new(&[], How::Tty));
        assert!(decide(Act::Approve, &s, "dev", "kb", Some("aa"), false, None).is_err(), "closed for kb");
        assert_eq!(decide(Act::Approve, &s, "dev", "other", None, false, None), Ok(Allowed::Setting(Source::Root)));
        // Made again under the name: the root's choice applies.
        assert_eq!(decide(Act::Approve, &s, "dev", "kb", Some("bb"), false, None), Ok(Allowed::Setting(Source::Root)));
        let mut s = Setting::default();
        s.set_cauldron("dev", "kb", None, Choice::new(&[Act::Approve], How::Env));
        assert_eq!(decide(Act::Approve, &s, "dev", "kb", None, false, None), Ok(Allowed::Setting(Source::Cauldron)));
        assert!(decide(Act::Grant, &s, "dev", "kb", None, false, None).is_err());
        assert!(decide(Act::Approve, &s, "prod", "kb", None, false, None).is_err(), "another environment's cauldron");
        assert!(s.unset_cauldron("dev", "kb"));
        assert!(decide(Act::Approve, &s, "dev", "kb", None, false, None).is_err());
    }

    #[test]
    fn the_views_say_what_a_session_may_do_apart_from_this_process() {
        let mut s = Setting::default();
        let v = view(&s, "dev", "kb", None, Some("approve"));
        assert_eq!(v["acts"]["approve"], "env", "{v}");
        assert_eq!(v["sessions"]["approve"], "terminal", "the variable never opens the door: {v}");
        s.set_root(Choice::new(&[Act::Approve], How::Tty));
        let v = view(&s, "dev", "kb", None, None);
        assert_eq!((v["sessions"]["approve"].clone(), v["sessions"]["from"].clone()), (json!("open"), json!("root")), "{v}");
        s.set_cauldron("dev", "kb", None, Choice::new(&[], How::Tty));
        assert_eq!(view(&s, "dev", "kb", None, Some("open"))["sessions"]["approve"], "terminal");
    }

    #[test]
    fn the_file_round_trips_and_no_file_is_the_default() {
        let d = std::env::temp_dir().join(format!("owner-acts-{}-{}", std::process::id(), line!()));
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(load(&d).unwrap(), Setting::default());
        let mut s = Setting::default();
        s.set_root(Choice::new(&[Act::Grant], How::Install));
        store(&d, &s).unwrap();
        assert_eq!(load(&d).unwrap(), s);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(path(&d)).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&d);
    }
}
