//! **The ripple POLICY and the EVENT BUS**: every ripple definition the
//! cauldron runs, the prompt fragments they roll up, the events on the bus
//! and the triggers each one woke, and the recorded runs — the executions —
//! that show what each ripple did (design 0094; 8e4fca10 msg 235). FAKE
//! DATA: the seed is `<seeds>/ripples.json`; nothing here calls a model or
//! reaches the threads daemon.
//!
//! Nothing here is CONFIGURED from the face (msg 235: "Does anybody need the
//! ability to go in and change ripples? Does a user actually need that here?
//! Maybe not. Probably not ... there's no system level config stuff. That's
//! my call"): the policy is read, and Status › Bus explores it read-only —
//! events by type, the triggers, the ripples (the handlers), the
//! executions (`bus`).
//!
//! **Where it is kept** — per environment, beside the settings files:
//!
//! ```text
//! <data>/<env>/tui/soup/ripples/curator/policy.toml            a LIVE policy written before msg 235, read as live
//! <data>/<env>/tui/soup/ripples/curator/prompts/<id>/<N>.md    its fragments, the harness rollup layout
//! <data>/<env>/tui/soup/ripples/runs/<run-id>.json             runs recorded here
//! ```
//!
//! Until a live directory exists the seed's policy IS the live policy.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::cauldron::store::write_text_atomic;

// ── the definition ──────────────────────────────────────────────────────────

/// `[policy]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub live_since: String,
}

/// A policy as its `policy.toml` holds it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PolicyFile {
    pub policy: Meta,
    #[serde(default)]
    pub ripple: Vec<RippleDef>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    TypeOne,
    TypeTwo,
    /// A ripple made of ripples — an aggregate ripple, which the screen
    /// calls a SPLASH (8e4fca10 msg 186: "the aggregate ripples, which are
    /// now going to be called a splash"). Its stored word stays `shell` (the
    /// policy files say it); `aggregate` and `splash` read as the same thing,
    /// and so does `type_three` (V1.9's kind word, the curated
    /// `cauldron-build` ledger's).
    #[serde(alias = "aggregate", alias = "splash", alias = "type_three")]
    Shell,
}

impl Kind {
    /// What the screen calls it — his words, never the build's.
    pub fn label(self) -> &'static str {
        match self {
            Kind::TypeOne => "type one",
            Kind::TypeTwo => "type two",
            Kind::Shell => "splash",
        }
    }
}

fn yes() -> bool {
    true
}
fn is_false(b: &bool) -> bool {
    !*b
}

/// One ripple's definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RippleDef {
    pub id: String,
    pub kind: Kind,
    /// One line, his sentence form: what it does.
    #[serde(default)]
    pub job: String,
    /// A role of Roles → models; the role resolves model, fallback and pool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// The catalog group of a top-level ripple: what wakes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The shell the catalog lists it under (display only; what CALLS it is
    /// read off the graphs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, rename = "trigger", skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<Trigger>,
    // ── a shell ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<Step>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub body: Vec<Step>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<Step>,
    // ── a type one ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub walk: Option<Walk>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<QuestionDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub map: Vec<MapRule>,
    /// What happens when no rule fires ("otherwise"); none means nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<ActRef>,
    // ── a type two ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<Turn>,
    // ── type one and type two ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<PromptDef>,
    #[serde(default, rename = "action_set", skip_serializing_if = "Vec::is_empty")]
    pub action_sets: Vec<ActionSet>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flag_scope: Vec<String>,
    #[serde(default)]
    pub reach: Reach,
    #[serde(default)]
    pub model: ModelCfg,
    #[serde(default)]
    pub limits: Limits,
}

/// What wakes a ripple from the bus: MATCH + CONDITION, and the BINDING that
/// fills its slots from the event (0093's five-part reaction; 0094 Q2 puts it
/// in the policy).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Trigger {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// The event class.
    #[serde(rename = "match")]
    pub on: String,
    /// The fields it watches, so a prose edit does not refire it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    /// A ladle boolean; empty is always.
    #[serde(default, rename = "where", skip_serializing_if = "String::is_empty")]
    pub when: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bind: Vec<Binding>,
    /// none | all | any | k_of_n | fifo
    #[serde(default = "none_word")]
    pub gather: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gather_k: Option<u32>,
    /// queue | stop_restart | coalesce | run_both
    #[serde(default = "queue_word")]
    pub on_newer: String,
    /// 0 is unlimited.
    #[serde(default)]
    pub max_at_once: u32,
    /// cluster | cauldron
    #[serde(default = "per_word")]
    pub per: String,
    /// None is forever.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_invocations: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub wake_on_delivery: bool,
    /// For a `schedule` match: `every 10m`, `at 02:10`, `opportunistic`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub schedule: String,
    /// skip | fire_on_wake
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub misfire: String,
}

fn none_word() -> String {
    "none".into()
}
fn queue_word() -> String {
    "queue".into()
}
fn per_word() -> String {
    "cluster".into()
}

/// `slot ← source path`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub slot: String,
    /// event | soup | literal | default | step
    pub source: String,
    #[serde(default)]
    pub path: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
}

/// One step of a shell's graph. Exactly one of `call`, `native`, `foreach`,
/// `branch`, `loop`, `wait` is set; [`Step::word`] says which.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Step {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreach: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The loop's `until` condition.
    #[serde(default, rename = "loop", skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    /// A wait's timeout (`30m`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub input: BTreeMap<String, String>,
    #[serde(default, rename = "as", skip_serializing_if = "Option::is_none")]
    pub out: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub parallel: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallel: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub body: Vec<Step>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cases: BTreeMap<String, Vec<Step>>,
    #[serde(default, rename = "else", skip_serializing_if = "Vec::is_empty")]
    pub otherwise: Vec<Step>,
}

impl Step {
    /// `call native foreach branch loop wait`.
    pub fn word(&self) -> &'static str {
        if self.call.is_some() {
            "call"
        } else if self.native.is_some() {
            "native"
        } else if self.foreach.is_some() {
            "foreach"
        } else if self.branch.is_some() {
            "branch"
        } else if self.until.is_some() {
            "loop"
        } else {
            "wait"
        }
    }
    /// What it names: the ripple, the function, the list, the value.
    pub fn target(&self) -> String {
        self.call
            .clone()
            .or_else(|| self.native.clone())
            .or_else(|| self.foreach.clone())
            .or_else(|| self.branch.clone())
            .or_else(|| self.until.clone())
            .or_else(|| self.wait.clone())
            .unwrap_or_default()
    }
    /// The steps it holds.
    pub fn children(&self) -> Vec<&Step> {
        let mut out: Vec<&Step> = self.body.iter().collect();
        for steps in self.cases.values() {
            out.extend(steps.iter());
        }
        out.extend(self.otherwise.iter());
        out
    }
    /// A new step of kind `word` naming `target`.
    pub fn new(word: &str, target: &str) -> Step {
        let mut s = Step::default();
        let t = Some(target.to_string());
        match word {
            "call" => s.call = t,
            "native" => s.native = t,
            "foreach" => {
                s.foreach = t;
                s.parallel = true;
                s.max_parallel = Some(4);
            }
            "branch" => s.branch = t,
            "loop" => {
                s.until = t;
                s.max = Some(3);
            }
            _ => s.wait = Some(if target.is_empty() { "30m".into() } else { target.to_string() }),
        }
        s
    }
}

/// A type one's walk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Walk {
    /// Which list; empty walks the input once.
    #[serde(default)]
    pub over: String,
    /// sequential | batched
    pub mode: String,
    #[serde(default)]
    pub expand: String,
    /// fixpoint | once
    pub until: String,
    pub max_passes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct QuestionDef {
    pub key: String,
    /// yes_no | choice | score
    #[serde(rename = "type")]
    pub ty: String,
    /// The calibration key.
    pub template_id: String,
    /// The fragment holding its instructions.
    #[serde(default)]
    pub instructions: String,
    /// One per answer, in order.
    #[serde(default)]
    pub criteria: Vec<Criterion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Criterion {
    pub answer: String,
    pub text: String,
}

/// `key = answer  p ≥ min_p  →  set.name(args)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MapRule {
    pub key: String,
    pub answer: String,
    pub min_p: f64,
    pub set: String,
    pub name: String,
    /// When set, the rule fires only for an item of this kind (`region`,
    /// `repo`, `node`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub item_kind: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ActRef {
    pub set: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Turn {
    pub max_turns: u32,
    /// The terminal set that ends it.
    pub ends_on: String,
    pub repairs: u32,
}

/// A prompt: its own fragment (by id and version) and how the rollup fills
/// it. The references and the slots it USES are read off its text; what is
/// stored is how each is pinned, inserted and bound.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PromptDef {
    pub fragment: String,
    pub version: u32,
    /// Per referenced fragment: `latest`, a version number, or `<ancestor>.latest`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pins: BTreeMap<String, String>,
    /// Per referenced fragment: `reference` (default) or `raw`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub insert: BTreeMap<String, String>,
    /// Referenced fragments that change per turn (rolled up last).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volatile: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<Binding>,
    /// The context renderer and its budget in characters.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub renderer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_chars: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ActionSet {
    pub name: String,
    #[serde(default)]
    pub terminal: bool,
    #[serde(default)]
    pub exclusive: bool,
    /// A ladle boolean that admits the set; empty is always.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub guard: String,
    #[serde(default, rename = "actions")]
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub name: String,
    /// read | emit | call | write
    pub effect: String,
    #[serde(default)]
    pub params: Vec<Param>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sets_flags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_calls: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub blocking: bool,
    /// Its prose fragment.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fragment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Param {
    pub name: String,
    /// bool | int | enum | node_id | msg_ref | text | list, with a bound
    /// after a colon: `text:2000`, `enum:27`, `enum:a|b|c`, `int:0..9`.
    pub ty: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub optional: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Reach {
    /// this | related | all
    #[serde(default = "related_word")]
    pub clusters: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<String>,
}

fn related_word() -> String {
    "related".into()
}

impl Default for Reach {
    fn default() -> Self {
        Reach { clusters: related_word(), repos: Vec::new() }
    }
}

/// Model overrides. Absent inherits from the role (Roles → models) or, for a
/// type one, from Settings › Ripples.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelCfg {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_key: Option<String>,
}

/// Limit overrides; absent inherits from Settings › Ripples, and a ripple may
/// only narrow.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked_max_age: Option<String>,
}

/// The built runtime's call-depth cap (`MAX_DEPTH`).
pub const MAX_DEPTH: u32 = 16;

/// The event classes a trigger may match (provisional, 0094 `unknowns`).
pub const EVENT_CLASSES: &[&str] = &[
    "user_input",
    "dump.received",
    "dump.fragmented",
    "dump.processed",
    "dump.group_failed",
    "transaction_committed",
    "ripple.call",
    "ripple.return",
    "report_landed",
    "cluster_created",
    "cluster_opened",
    "push",
    "schedule",
    "external_arrival",
    "nag",
];

// ── the observations ────────────────────────────────────────────────────────

/// What one ripple has been doing (fake).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Stats {
    #[serde(default)]
    pub today: u32,
    #[serde(default)]
    pub week: u32,
    #[serde(default)]
    pub median_s: f64,
    #[serde(default)]
    pub cost_each: Option<f64>,
    #[serde(default)]
    pub running: u32,
    #[serde(default)]
    pub parked: u32,
    #[serde(default)]
    pub last: String,
    /// When the last run failed: how long ago.
    #[serde(default)]
    pub failed_last: Option<String>,
    /// A type one's calibration drift this week.
    #[serde(default)]
    pub drift: Option<String>,
    /// Its recent calls, newest first (for a ripple another one calls).
    #[serde(default)]
    pub recent: Vec<Recent>,
    /// Per action: chosen this week.
    #[serde(default)]
    pub used_week: BTreeMap<String, u32>,
    /// Per trigger index: recent matches, newest first.
    #[serde(default)]
    pub fires: BTreeMap<String, Vec<Fire>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Recent {
    pub run: String,
    pub at: String,
    pub cluster: String,
    pub cite: String,
    pub line: String,
    pub tail: String,
    /// done | running | failed
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Fire {
    /// When the event it matched was on the bus: `14:31:04`, today unless
    /// `day` says otherwise.
    pub at: String,
    #[serde(default = "today_word")]
    pub day: String,
    pub cite: String,
    pub cluster: String,
    /// The run it started — the EXECUTION — when one is recorded; none when
    /// `where` refused it, or when its run is not recorded here.
    #[serde(default)]
    pub run: Option<String>,
    #[serde(default)]
    pub elapsed: String,
    /// running | done | failed | refused
    pub state: String,
}

/// One event on the bus, as the seed records it (8e4fca10 msg 235: "I want to
/// be able to, like, explore in the status and see, like, events ... I can
/// see the event, details, like the payload and the time"). Which triggers
/// it woke is read off the triggers' FIRES ([`Stats::fires`]): a fire of a
/// trigger matching this event's class at its time (and citing its message)
/// is a trigger it fired.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BusEvent {
    pub id: String,
    /// Its class: what a trigger matches (`dump.received`).
    pub class: String,
    /// `14:31:04`, today unless `day` says otherwise.
    pub at: String,
    #[serde(default = "today_word")]
    pub day: String,
    /// The message it carries, when it carries one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cite: String,
    /// The cluster it happened on, when one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cluster: String,
    #[serde(default)]
    pub payload: BTreeMap<String, String>,
}

/// An event as Status › Bus shows it: the bus's record, and every trigger
/// it fired with what that firing did.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub id: String,
    pub class: String,
    pub at: String,
    pub day: String,
    pub cite: String,
    pub cluster: String,
    pub payload: BTreeMap<String, String>,
    pub fired: Vec<Fired>,
}

/// One trigger an event fired: the ripple that handles it, which of its
/// triggers, and the firing — its run (the execution), its state.
#[derive(Debug, Clone, PartialEq)]
pub struct Fired {
    pub ripple: String,
    pub trigger: usize,
    pub fire: Fire,
}

impl Event {
    /// Today's events newest first, then yesterday's, then older days'.
    fn order(&self) -> (u8, std::cmp::Reverse<String>, std::cmp::Reverse<String>) {
        let d = match self.day.as_str() {
            "today" => 0,
            "yesterday" => 1,
            _ => 2,
        };
        (d, std::cmp::Reverse(self.day.clone()), std::cmp::Reverse(self.at.clone()))
    }
}

/// A recorded run: the step tree, every step a record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub id: String,
    pub ripple: String,
    /// The KEY of the cluster it ran on (`cauldron-ripples`), never its name:
    /// a name can be renamed, and a run must still find its cluster.
    pub cluster: String,
    #[serde(default)]
    pub cite: String,
    /// `14:02:11` or `today 14:02`; the day group comes from `day`.
    pub started: String,
    /// today | yesterday | a date. The seed's runs are written relative to
    /// the day they are read on; a run recorded here carries [`Run::at`] and
    /// its date, and its day word is read from `at`.
    #[serde(default = "today_word")]
    pub day: String,
    /// When it started, as an RFC 3339 instant: every run recorded here
    /// carries one. The seed's runs have none (they are "today", whatever
    /// today is) and are placed by `day` and `started`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(default)]
    pub elapsed: String,
    #[serde(default)]
    pub cost: Option<f64>,
    /// running | done | failed | parked
    pub state: String,
    pub line: String,
    #[serde(default)]
    pub account: String,
    /// The policy it ran under; none means the seed's own policy.
    #[serde(default)]
    pub policy_hash: Option<String>,
    #[serde(default)]
    pub steps: u32,
    #[serde(default)]
    pub tree: Vec<StepRec>,
}

fn today_word() -> String {
    "today".into()
}

impl Run {
    /// When it started, on this machine's clock: [`Run::at`] when it was
    /// recorded here; for a seed run, its `day` word and the clock time in
    /// `started`, read against today.
    pub fn instant(&self) -> Option<chrono::DateTime<chrono::Local>> {
        use chrono::{Local, NaiveDate, NaiveTime, TimeZone};
        if let Some(at) = &self.at {
            return chrono::DateTime::parse_from_rfc3339(at).ok().map(|t| t.with_timezone(&Local));
        }
        let today = Local::now().date_naive();
        let date = match self.day.as_str() {
            "today" => today,
            "yesterday" => today.pred_opt()?,
            d => NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()?,
        };
        let clock = self.started.split_whitespace().last()?;
        let time = NaiveTime::parse_from_str(clock, "%H:%M:%S").or_else(|_| NaiveTime::parse_from_str(clock, "%H:%M")).ok()?;
        Local.from_local_datetime(&date.and_time(time)).earliest()
    }

    /// The day it is grouped under on the runs page: `today`, `yesterday`, or
    /// its date. A run recorded here is read from [`Run::at`], so a run made
    /// yesterday is never shown as today's.
    pub fn day_word(&self) -> String {
        let Some(t) = self.at.as_ref().and(self.instant()) else { return self.day.clone() };
        let (d, today) = (t.date_naive(), chrono::Local::now().date_naive());
        if d == today {
            "today".into()
        } else if Some(d) == today.pred_opt() {
            "yesterday".into()
        } else {
            d.format("%Y-%m-%d").to_string()
        }
    }

    /// Every soup node any of its steps wrote, foreach iterations included.
    pub fn written_nodes(&self) -> Vec<&str> {
        fn walk<'a>(steps: &'a [StepRec], out: &mut Vec<&'a str>) {
            for s in steps {
                out.extend(s.effects.iter().map(|e| e.node.as_str()));
                for it in &s.items {
                    walk(&it.steps, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.tree, &mut out);
        out
    }
}

/// One step's record (0094 §1.7, `StepRecord`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StepRec {
    pub step: String,
    /// native | type_one | type_two | shell
    pub kind: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub line: String,
    /// Per adapter, the probability its answer carried.
    #[serde(default)]
    pub p: BTreeMap<String, f64>,
    #[serde(default)]
    pub turns: Option<u32>,
    #[serde(default)]
    pub ms: u64,
    #[serde(default)]
    pub cost: Option<f64>,
    #[serde(default)]
    pub effects: Vec<Effect>,
    #[serde(default)]
    pub input: String,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub prompt_hash: String,
    /// `×n` and `∥k` for a foreach.
    #[serde(default)]
    pub foreach: Option<u32>,
    #[serde(default)]
    pub parallel: Option<u32>,
    #[serde(default)]
    pub items: Vec<IterRec>,
    /// done | failed | running
    #[serde(default = "done_word")]
    pub state: String,
}

fn done_word() -> String {
    "done".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct IterRec {
    pub text: String,
    #[serde(default)]
    pub ms: u64,
    #[serde(default)]
    pub steps: Vec<StepRec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Effect {
    pub node: String,
    #[serde(default)]
    pub rev_before: Option<u64>,
    #[serde(default)]
    pub rev_after: Option<u64>,
}

/// A variant the dream or a policy engineer made: edits over the live policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SeedVariant {
    pub id: String,
    /// dream | engineer
    pub by: String,
    pub when: String,
    #[serde(default)]
    pub backtest: Option<f64>,
    pub edits: Vec<Edit>,
}

/// One edit of a variant: a ripple's field (a JSON pointer into its
/// definition) set to a value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    pub ripple: String,
    pub ptr: String,
    pub to: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EventClass {
    pub class: String,
    #[serde(default)]
    pub today: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Backtest {
    pub score: f64,
    pub runs: u32,
    pub from: String,
    pub to: String,
    pub split: String,
    pub promote_when: String,
}

/// The seed file, `<seeds>/ripples.json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeedFile {
    #[serde(default, rename = "_note")]
    pub note: String,
    pub policy: PolicyFile,
    /// Fragment id → version → text.
    pub fragments: BTreeMap<String, BTreeMap<String, String>>,
    pub backtest: Backtest,
    #[serde(default)]
    pub variants: Vec<SeedVariant>,
    #[serde(default)]
    pub events: Vec<EventClass>,
    /// The event bus's record: every event, with its payload (msg 235).
    #[serde(default)]
    pub bus: Vec<BusEvent>,
    #[serde(default)]
    pub stats: BTreeMap<String, Stats>,
    #[serde(default)]
    pub runs: Vec<Run>,
    /// The week's totals the runs footer reads.
    #[serde(default)]
    pub week: WeekTotals,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WeekTotals {
    pub today: u32,
    pub week: u32,
    pub cost_today: f64,
    pub cost_week: f64,
    pub failed_today: u32,
    pub yesterday: u32,
}

// ── a policy in memory ─────────────────────────────────────────────────────

pub type Fragments = BTreeMap<String, BTreeMap<u32, String>>;

/// A whole policy: its file and its fragments.
#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    pub file: PolicyFile,
    pub fragments: Fragments,
}

impl Policy {
    pub fn get(&self, id: &str) -> Option<&RippleDef> {
        self.file.ripple.iter().find(|r| r.id == id)
    }

    pub fn index(&self, id: &str) -> Option<usize> {
        self.file.ripple.iter().position(|r| r.id == id)
    }

    /// `sha256` over canonical JSON of the policy with its fragments inlined
    /// (as `Policy::hash` does in the runtime), as hex.
    pub fn hash_hex(&self) -> String {
        let canon = serde_json::json!({"policy": self.file, "prompt_files": self.fragments});
        let mut h = Sha256::new();
        h.update(serde_json::to_vec(&canon).expect("a policy serializes"));
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The short hash the screens show.
    pub fn hash8(&self) -> String {
        self.hash_hex()[..8].to_string()
    }

    /// The text of fragment `id` at `version`.
    pub fn fragment(&self, id: &str, version: u32) -> Option<&str> {
        self.fragments.get(id).and_then(|v| v.get(&version)).map(String::as_str)
    }

    /// The newest version of fragment `id`.
    pub fn latest(&self, id: &str) -> Option<u32> {
        self.fragments.get(id).and_then(|v| v.keys().max().copied())
    }

    /// The version a pin resolves to.
    pub fn resolve_pin(&self, id: &str, pin: &str) -> Option<u32> {
        match pin.parse::<u32>() {
            Ok(n) => Some(n),
            Err(_) => self.latest(id),
        }
    }

    /// Every ripple whose graph calls `id`, with the branch case that routes
    /// there (read off the graphs; never stored).
    pub fn called_by(&self, id: &str) -> Vec<(String, Option<String>)> {
        fn walk(steps: &[&Step], id: &str, case: Option<&str>, out: &mut Vec<Option<String>>) {
            for s in steps {
                if s.call.as_deref() == Some(id) {
                    out.push(case.map(str::to_string));
                }
                walk(&s.body.iter().collect::<Vec<_>>(), id, case, out);
                for (c, steps) in &s.cases {
                    walk(&steps.iter().collect::<Vec<_>>(), id, Some(c), out);
                }
                walk(&s.otherwise.iter().collect::<Vec<_>>(), id, Some("else"), out);
            }
        }
        let mut out = Vec::new();
        for r in &self.file.ripple {
            let mut hits = Vec::new();
            walk(&r.steps(), id, None, &mut hits);
            if let Some(first) = hits.into_iter().next() {
                out.push((r.id.clone(), first));
            }
        }
        out
    }

    /// The ripples a shell's graph calls, each once, in graph order.
    pub fn calls(&self, id: &str) -> Vec<String> {
        fn walk(steps: &[&Step], out: &mut Vec<String>) {
            for s in steps {
                if let Some(c) = &s.call {
                    if !out.contains(c) {
                        out.push(c.clone());
                    }
                }
                walk(&s.children(), out);
            }
        }
        let mut out = Vec::new();
        if let Some(r) = self.get(id) {
            walk(&r.steps(), &mut out);
        }
        out
    }

    /// The catalog members of `id`: the ripples listed under it.
    pub fn members(&self, id: &str) -> Vec<&RippleDef> {
        self.file.ripple.iter().filter(|r| r.shell.as_deref() == Some(id)).collect()
    }

    /// The refusal that keeps this policy from being saved, if any.
    pub fn check(&self) -> std::result::Result<(), String> {
        let mut ids = BTreeSet::new();
        for r in &self.file.ripple {
            if !ids.insert(r.id.as_str()) {
                return Err(format!("two ripples are called `{}`", r.id));
            }
        }
        for r in &self.file.ripple {
            check_ripple(self, r)?;
        }
        Ok(())
    }
}

impl RippleDef {
    /// Entry, body and exit, in order.
    pub fn steps(&self) -> Vec<&Step> {
        let mut out: Vec<&Step> = self.entry.iter().collect();
        out.extend(self.body.iter());
        out.extend(self.exit.iter());
        out
    }

    /// The set a turn may end on.
    pub fn terminal_sets(&self) -> Vec<&ActionSet> {
        self.action_sets.iter().filter(|s| s.terminal).collect()
    }
}

/// The one refusal a ripple's definition can carry, said in one line.
pub fn check_ripple(policy: &Policy, r: &RippleDef) -> std::result::Result<(), String> {
    if r.terminal_sets().len() > 1 {
        return Err("one terminal set per ripple".into());
    }
    if r.kind == Kind::TypeOne {
        for s in &r.action_sets {
            for a in &s.actions {
                if !a.sets_flags.is_empty() {
                    return Err("a type one sets no flags".into());
                }
                if a.params.iter().any(|p| p.ty.starts_with("text")) {
                    return Err("a type one takes no free text".into());
                }
            }
        }
        for m in &r.map {
            if !r.questions.iter().any(|q| q.key == m.key) {
                return Err(format!("no question `{}`", m.key));
            }
            if !(0.0..=1.0).contains(&m.min_p) {
                return Err("p is between 0 and 1".into());
            }
        }
    }
    for m in &r.map {
        if !r.action_sets.iter().any(|s| s.name == m.set && s.actions.iter().any(|a| a.name == m.name)) {
            return Err(format!("no action {}.{}", m.set, m.name));
        }
    }
    if let Some(t) = &r.turn {
        if t.max_turns == 0 {
            return Err("at least one turn".into());
        }
        if !r.action_sets.iter().any(|s| s.name == t.ends_on && s.terminal) {
            return Err(format!("`{}` is not a terminal set", t.ends_on));
        }
    }
    if let Some(w) = &r.walk {
        if w.max_passes == 0 {
            return Err("at least one pass".into());
        }
    }
    for t in &r.triggers {
        for b in &t.bind {
            if b.required && b.path.trim().is_empty() && b.source != "default" {
                return Err(format!("`{}` is required: give it a source", b.slot));
            }
        }
    }
    if let Some(p) = &r.prompt {
        for b in &p.slots {
            if b.required && b.path.trim().is_empty() && b.source != "default" {
                return Err(format!("`{}` is required: give it a source", b.slot));
            }
        }
        if policy.fragment(&p.fragment, p.version).is_none() {
            return Err(format!("no fragment {} v{}", p.fragment, p.version));
        }
    }
    for s in r.steps() {
        check_step(policy, s)?;
    }
    if r.kind == Kind::Shell && r.entry.is_none() {
        return Err("a shell has an entry".into());
    }
    Ok(())
}

fn check_step(policy: &Policy, s: &Step) -> std::result::Result<(), String> {
    if let Some(c) = &s.call {
        if policy.get(c).is_none() {
            return Err(format!("no ripple `{c}` to call"));
        }
    }
    if s.branch.is_some() && s.otherwise.is_empty() && !s.cases.is_empty() && s.cases.len() < 2 {
        return Err("a branch with one case needs an else".into());
    }
    if s.until.is_some() && s.max.unwrap_or(0) == 0 {
        return Err("a loop has a max".into());
    }
    for c in s.children() {
        check_step(policy, c)?;
    }
    Ok(())
}

// ── the rollup ─────────────────────────────────────────────────────────────

/// `{{prompt:id}}`, `{{prompt:id:3}}` and `{{slot:name}}` markers in a text,
/// in order: (is_slot, name, byte range).
pub fn markers(text: &str) -> Vec<(bool, String, std::ops::Range<usize>)> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = text[at..].find("{{") {
        let start = at + i;
        let Some(j) = text[start..].find("}}") else { break };
        let end = start + j + 2;
        let inner = &text[start + 2..end - 2];
        if let Some(rest) = inner.strip_prefix("prompt:") {
            let id = rest.split([':', '(']).next().unwrap_or("").to_string();
            out.push((false, id, start..end));
        } else if let Some(rest) = inner.strip_prefix("slot:") {
            let name = rest.split(['?', '}']).next().unwrap_or("").to_string();
            out.push((true, name, start..end));
        }
        at = end;
    }
    out
}

/// The action space the runtime appends to a prompt, generated from the
/// ripple's action sets (so its size is never stored).
pub fn action_space(r: &RippleDef) -> String {
    let mut s = String::from("ACTIONS\n");
    for set in &r.action_sets {
        let mode = match (set.terminal, set.exclusive) {
            (true, true) => "terminal, exclusive",
            (true, false) => "terminal",
            (false, true) => "exclusive",
            (false, false) => "pooled",
        };
        s.push_str(&format!("{} ({mode})\n", set.name));
        for a in &set.actions {
            let ps: Vec<String> = a.params.iter().map(|p| format!("{}{}: {}", p.name, if p.optional { "?" } else { "" }, p.ty)).collect();
            s.push_str(&format!("  - {}({}) [{}]\n", a.name, ps.join(", "), a.effect));
        }
    }
    s
}

/// The turn schema the runtime appends, generated from the action sets.
pub fn turn_schema(r: &RippleDef) -> String {
    let acts: Vec<Value> = r
        .action_sets
        .iter()
        .flat_map(|s| {
            s.actions.iter().map(move |a| {
                let props: serde_json::Map<String, Value> = a
                    .params
                    .iter()
                    .map(|p| (p.name.clone(), serde_json::json!({"type": p.ty, "optional": p.optional})))
                    .collect();
                serde_json::json!({"set": s.name, "action": a.name, "params": props})
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({"type": "object", "one_of": acts})).unwrap_or_default()
}

// ── where it is kept ───────────────────────────────────────────────────────

/// The policy an environment with no seed policy of its own starts with
/// (REL-3.S2): the catalog's definitions and fragments, nothing run.
pub const DEFAULT_POLICY: &str = include_str!("default_ripples.json");

/// The seed file.
pub fn seed_path(seeds: &Path) -> PathBuf {
    seeds.join("ripples.json")
}

/// The policy store for one environment: the live policy, and what the
/// ripples did — the bus's events, the triggers' fires, the runs.
pub struct Store {
    pub dir: PathBuf,
    writable: bool,
    pub seed: SeedFile,
    /// The seed's own policy, as it was (the runs it recorded ran under it).
    pub seed_policy: Policy,
    pub live: Policy,
    /// Runs recorded in this environment (the seeding ripple's), newest last.
    pub local_runs: Vec<Run>,
}

fn read_policy_dir(dir: &Path) -> Result<Option<Policy>> {
    let file = dir.join("policy.toml");
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", file.display())),
    };
    let pf: PolicyFile = toml::from_str(&text).with_context(|| format!("parse {}", file.display()))?;
    let mut fragments: Fragments = BTreeMap::new();
    let prompts = dir.join("prompts");
    fn walk(base: &Path, d: &Path, out: &mut Fragments) -> Result<()> {
        let rd = match std::fs::read_dir(d) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).with_context(|| format!("read {}", d.display())),
        };
        for ent in rd {
            let p = ent?.path();
            if p.is_dir() {
                walk(base, &p, out)?;
            } else if let (Some(stem), Some("md")) = (p.file_stem().and_then(|s| s.to_str()), p.extension().and_then(|s| s.to_str())) {
                let Ok(n) = stem.parse::<u32>() else { continue };
                let id = p.parent().and_then(|x| x.strip_prefix(base).ok()).map(|x| x.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                let text = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
                out.entry(id).or_default().insert(n, text);
            }
        }
        Ok(())
    }
    walk(&prompts, &prompts, &mut fragments)?;
    Ok(Some(Policy { file: pf, fragments }))
}

impl Store {
    /// Open the policy for the environment whose face state lives in `soup`.
    /// With `writable` false nothing is ever written (the `--dump` path).
    pub fn open(seeds: &Path, soup: &Path, writable: bool) -> Result<Store> {
        // A cauldron that carries its OWN seed may carry its own policy too;
        // else a fixture's; else (REL-3.S2: a real environment, which reads
        // no repo seed) the DEFAULT policy compiled into the face — the
        // catalog with nothing run.
        let path = crate::cauldron::store::own_seed(soup, "ripples.json").unwrap_or_else(|| seed_path(seeds));
        let text = if path.is_file() {
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?
        } else {
            DEFAULT_POLICY.to_string()
        };
        // One written in the old words is refused as it stands (task 0209
        // D14): its writer writes the new words; nothing corrects it here.
        let seed: SeedFile = serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        let mut fragments: Fragments = BTreeMap::new();
        for (id, versions) in &seed.fragments {
            for (n, t) in versions {
                let n: u32 = n.parse().with_context(|| format!("{}: fragment {id} version `{n}` is not a number", path.display()))?;
                fragments.entry(id.clone()).or_default().insert(n, t.clone());
            }
        }
        let seed_policy = Policy { file: seed.policy.clone(), fragments };
        seed_policy.check().map_err(|e| anyhow::anyhow!("{}: the seed policy is refused: {e}", path.display()))?;
        let dir = soup.join("ripples");
        let name = seed_policy.file.policy.name.clone();
        let live = read_policy_dir(&dir.join(&name))?.unwrap_or_else(|| seed_policy.clone());
        let mut local_runs = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir.join("runs")) {
            let mut paths: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
            paths.sort();
            for p in paths {
                let t = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
                local_runs.push(serde_json::from_str::<Run>(&t).with_context(|| format!("parse {}", p.display()))?);
            }
        }
        // Newest last, by when each started (a run's file name is a random id).
        local_runs.sort_by_key(|r| r.instant());
        Ok(Store { dir, writable, seed, seed_policy, live, local_runs })
    }

    pub fn name(&self) -> &str {
        &self.seed_policy.file.policy.name
    }

    /// Record a run made here.
    pub fn record_run(&mut self, run: Run) -> Result<()> {
        if self.writable {
            let text = serde_json::to_string_pretty(&run)?;
            write_text_atomic(&self.dir.join("runs").join(format!("{}.json", run.id)), &text)?;
        }
        self.local_runs.push(run);
        Ok(())
    }

    /// Every run, newest first: those recorded here, then the seed's.
    pub fn runs(&self) -> Vec<&Run> {
        let mut out: Vec<&Run> = self.local_runs.iter().rev().collect();
        out.extend(self.seed.runs.iter());
        out
    }

    /// Every run with a STABLE index: the seed's, then those recorded here,
    /// oldest first. A run recorded later only ever adds an index, so an index
    /// held by a page keeps meaning the same run.
    pub fn indexed_runs(&self) -> impl Iterator<Item = (usize, &Run)> {
        self.seed.runs.iter().chain(self.local_runs.iter()).enumerate()
    }

    /// The run at index `ix` of [`Store::indexed_runs`].
    pub fn run_at(&self, ix: usize) -> Option<&Run> {
        self.seed.runs.iter().chain(self.local_runs.iter()).nth(ix)
    }

    pub fn run(&self, id: &str) -> Option<&Run> {
        self.local_runs.iter().chain(self.seed.runs.iter()).find(|r| r.id == id)
    }

    /// The hash a run ran under.
    pub fn run_hash(&self, run: &Run) -> String {
        run.policy_hash.clone().unwrap_or_else(|| self.seed_policy.hash8())
    }

    pub fn stats(&self, id: &str) -> Stats {
        self.seed.stats.get(id).cloned().unwrap_or_default()
    }

    /// EVERY event on the bus, newest first (msg 235), each with the triggers
    /// it fired. The bus's record is the seed's `bus`; a trigger's fire that
    /// matches no recorded event (its class at its time, its message) is an
    /// event of its own, recorded by its fire alone; and a run recorded here
    /// (the seeding's) is an event of the class its ripple's first trigger
    /// wakes on, which fired that trigger.
    pub fn events(&self) -> Vec<Event> {
        let mut out: Vec<Event> = self
            .seed
            .bus
            .iter()
            .map(|b| Event { id: b.id.clone(), class: b.class.clone(), at: b.at.clone(), day: b.day.clone(), cite: b.cite.clone(), cluster: b.cluster.clone(), payload: b.payload.clone(), fired: Vec::new() })
            .collect();
        for r in &self.live.file.ripple {
            let st = self.stats(&r.id);
            for (ti, t) in r.triggers.iter().enumerate() {
                for f in st.fires.get(&ti.to_string()).into_iter().flatten() {
                    let fired = Fired { ripple: r.id.clone(), trigger: ti, fire: f.clone() };
                    match out.iter_mut().find(|e| e.class == t.on && e.at == f.at && e.day == f.day && (f.cite.is_empty() || e.cite == f.cite)) {
                        Some(e) => e.fired.push(fired),
                        None => {
                            let mut payload = BTreeMap::new();
                            if !f.cluster.is_empty() {
                                payload.insert("cluster".to_string(), f.cluster.clone());
                            }
                            out.push(Event { id: format!("EV-{}-{}", r.id, f.at.replace(':', "")), class: t.on.clone(), at: f.at.clone(), day: f.day.clone(), cite: f.cite.clone(), cluster: f.cluster.clone(), payload, fired: vec![fired] });
                        }
                    }
                }
            }
        }
        for run in &self.local_runs {
            let Some(t) = self.live.get(&run.ripple).and_then(|r| r.triggers.first()) else { continue };
            let at = run.instant().map_or_else(|| run.started.clone(), |i| i.format("%H:%M:%S").to_string());
            let fire = Fire { at: at.clone(), day: run.day_word(), cite: run.cite.clone(), cluster: run.cluster.clone(), run: Some(run.id.clone()), elapsed: run.elapsed.clone(), state: run.state.clone() };
            let payload = BTreeMap::from([("cluster".to_string(), run.cluster.clone())]);
            out.push(Event { id: format!("EV-{}", run.id), class: t.on.clone(), at, day: fire.day.clone(), cite: run.cite.clone(), cluster: run.cluster.clone(), payload, fired: vec![Fired { ripple: run.ripple.clone(), trigger: 0, fire }] });
        }
        out.sort_by_key(Event::order);
        out
    }

    /// The event classes the bus knows, in the seed's order, then any its
    /// events carry that the seed does not list.
    pub fn classes(&self) -> Vec<String> {
        let mut out: Vec<String> = self.seed.events.iter().map(|e| e.class.clone()).collect();
        for e in self.events() {
            if !out.contains(&e.class) {
                out.push(e.class);
            }
        }
        out
    }
}

/// The bytes of a text, as the screens show them: `2,039 B`, `6.1 KB`.
pub fn bytes(n: usize) -> String {
    if n >= 10_000 {
        format!("{:.1} KB", n as f64 / 1000.0)
    } else if n >= 1000 {
        format!("{},{:03} B", n / 1000, n % 1000)
    } else {
        format!("{n} B")
    }
}

/// Today's date, for `live since`.
pub fn today() -> String {
    chrono::Local::now().format("%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_are_read_off_the_text() {
        let m = markers("a {{prompt:curator/frame}} b {{slot:unit}} c {{slot:cite?\"none\"}} {{prompt:x:3}}");
        let names: Vec<(bool, &str)> = m.iter().map(|(s, n, _)| (*s, n.as_str())).collect();
        assert_eq!(names, vec![(false, "curator/frame"), (true, "unit"), (true, "cite"), (false, "x")]);
    }

    #[test]
    fn bytes_read_as_the_design_writes_them() {
        assert_eq!(bytes(412), "412 B");
        assert_eq!(bytes(2039), "2,039 B");
        assert_eq!(bytes(61204), "61.2 KB");
    }
}
