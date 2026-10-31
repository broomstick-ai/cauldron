//! The fake data behind the cauldron's Status tab — its devices and their
//! services, the token sources' accounts — and the pools the Config tab
//! configures. Shapes only — every value
//! comes from the environment's seed file, under its `cauldron` key. An
//! account's share, its enabled switch and its note are SETTINGS (`settings`),
//! not board data: the board shows what the settings say.
//!
//! The mesh is the view of the services: each device carries the services it
//! runs, so a service is listed once, under the device it runs on.
//!
//! The account board's data is the operator's `threads` board's (under
//! `substrate-interface-layer/lib/threads/scripts/threads-app`): each
//! account's usage axes, login, badges and pressure. Status draws it as
//! bands (msg 165: "we can deviate from the interface that we inherited"),
//! so the board's own header counts and thread rows are not carried.

use serde::Deserialize;

/// The GLOBAL token board (8e4fca10 msg 186): the token sources are set up
/// once for every cauldron, so their members and what those members have
/// spent are one seed (`global.json`). FAKE, as the rest of the board is, and
/// per environment since REL-3.S2: an environment's own seed carries it, or a
/// test's fixture does; the repo ships none into every environment.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalBoard {
    #[serde(default, rename = "_note")]
    pub note: Option<String>,
    /// Claude Code's accounts and their usage (the threads board's).
    #[serde(default)]
    pub accounts: Vec<Account>,
    #[serde(default)]
    pub next_back: Option<NextBack>,
    /// Every other source's members: Codex's accounts.
    #[serde(default)]
    pub pools: Vec<PoolSeed>,
    /// What each inference endpoint has spent this period and what its
    /// `/models` lists for its key, by endpoint id (design 0199): FAKE —
    /// nothing is called from this face.
    #[serde(default)]
    pub endpoints: std::collections::BTreeMap<String, EndpointSeed>,
    #[serde(default)]
    pub flips: Vec<crate::cauldron::settings::Flip>,
    /// The models each subscription source was DETECTED to offer (msg 235:
    /// "Cloud Code provides the models. We just detect the models that are
    /// provided by the Cloud Code version"), by source id.
    #[serde(default)]
    pub models: std::collections::BTreeMap<String, Vec<String>>,
}

impl GlobalBoard {
    /// The board a cauldron with soup dir `dir` shows: its OWN seed's
    /// (`<dir>/seed/global.json`) if it carries one, else the one in
    /// `seeds/global.json` (a fixture's). REL-3.S2: per environment — a face
    /// on a real environment passes `store::no_seeds`, so an environment
    /// without its own board shows none, never the repo's.
    pub fn load_for(seeds: &std::path::Path, dir: &std::path::Path) -> anyhow::Result<Option<GlobalBoard>> {
        match crate::cauldron::store::own_seed(dir, "global.json") {
            Some(p) => Self::read(&p),
            None => Self::load(seeds),
        }
    }

    /// The board in `seeds/global.json`, if the seed has one.
    pub fn load(seeds: &std::path::Path) -> anyhow::Result<Option<GlobalBoard>> {
        Self::read(&seeds.join("global.json"))
    }

    fn read(p: &std::path::Path) -> anyhow::Result<Option<GlobalBoard>> {
        use anyhow::Context;
        match std::fs::read_to_string(p) {
            Ok(t) => Ok(Some(serde_json::from_str(&t).with_context(|| format!("parse {}", p.display()))?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("read {}", p.display())),
        }
    }

    /// Put the board on `dash`: its token sources are the global ones.
    pub fn apply(self, dash: &mut Dash) {
        dash.threads.accounts = self.accounts;
        dash.threads.next_back = self.next_back;
        dash.pools = self.pools;
        dash.endpoints = self.endpoints;
        dash.flips = self.flips;
        dash.models = self.models;
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dash {
    #[serde(default)]
    pub threads: ThreadsBoard,
    #[serde(default)]
    pub mesh: Vec<Node>,
    /// Pools other than claude_code (whose accounts are the account board's),
    /// each with its accounts' labels. Their settings live in the settings
    /// file, not here.
    #[serde(default)]
    pub pools: Vec<PoolSeed>,
    /// The opportunistic flips the seed records (FAKE); a face appends its
    /// own to the environment's flip log (`settings`).
    #[serde(default)]
    pub flips: Vec<crate::cauldron::settings::Flip>,
    /// The models each subscription source was detected to offer.
    #[serde(default)]
    pub models: std::collections::BTreeMap<String, Vec<String>>,
    /// Each inference endpoint's reading, by id (the global board's).
    #[serde(default)]
    pub endpoints: std::collections::BTreeMap<String, EndpointSeed>,
    /// What the Status tab's census reads beyond the clusters' tasks and the
    /// ripples' runs (FAKE): the background runs no cluster owns.
    #[serde(default)]
    pub status: StatusSeed,
    /// The supervision log this cauldron keeps (`supervise`), oldest first:
    /// never seeded, already replayed over `mesh` and the daemon's board.
    #[serde(skip)]
    pub supervisions: Vec<crate::cauldron::supervise::Record>,
}

/// The rest of the Status tab's census (design 0142 §7.4), FAKE.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusSeed {
    /// Runs of the cauldron's own ripples on no cluster: counted in the
    /// cauldron's strip and on no cluster's.
    #[serde(default)]
    pub background: Vec<BgRun>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BgRun {
    pub ripple: String,
    pub line: String,
    /// `parked` (the gate refused it) or `pending` (waiting its turn).
    pub state: String,
    #[serde(default)]
    pub parked: Option<crate::cauldron::model::Parked>,
}

/// One inference endpoint's reading (FAKE): what its key has spent this
/// budget period, when the period resets, what each cauldron drew of it,
/// and the models its `/models` lists for the key.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointSeed {
    #[serde(default)]
    pub spent: f64,
    #[serde(default)]
    pub resets: String,
    /// Dollars each cauldron drew of it this period, by cauldron name.
    #[serde(default)]
    pub by_cauldron: std::collections::BTreeMap<String, f64>,
    #[serde(default)]
    pub listed: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolSeed {
    pub id: String,
    #[serde(default)]
    pub accounts: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThreadsBoard {
    /// `live` | `offline` | anything else the daemon reports.
    #[serde(default)]
    pub link: String,
    /// How long ago the daemon last answered: `42s`.
    #[serde(default)]
    pub age: Option<String>,
    #[serde(default)]
    pub warnings: u32,
    #[serde(default)]
    pub next_back: Option<NextBack>,
    #[serde(default)]
    pub accounts: Vec<Account>,
    /// When a restart asked from this face started it (the supervision
    /// record's `at`): its age is counted from here. Never in a seed.
    #[serde(skip)]
    pub started: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NextBack {
    pub account: String,
    #[serde(rename = "in")]
    pub in_: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub number: u32,
    pub label: String,
    /// `12d`, `EXPIRED`, `—`.
    pub login: String,
    #[serde(default)]
    pub badges: Vec<String>,
    #[serde(default)]
    pub axes: Vec<Axis>,
    /// `max`, `pro` — the daemon's `subscription_type`.
    #[serde(default)]
    pub subscription: Option<String>,
}

/// The hours in a week: what a weekly window's time left is a share of.
pub const WEEK_HOURS: f64 = 168.0;

impl Account {
    /// Its PRESSURE (8e4fca10 msg 235): the share of its week's tokens left
    /// over the share of its week's time left — "tokens remaining ratio over
    /// time ... you want it to be bigger when there are more tokens
    /// available, and you want it to be bigger when there's less time
    /// available". Above 1 it is not on pace to spend its week; below 1 it
    /// is spending faster than the week. Worked out from this account's own
    /// weekly reading alone — never relative to the other accounts — and
    /// `None` while it has no weekly reading.
    pub fn pressure(&self) -> Option<f64> {
        let w = self.axes.iter().find(|a| a.axis == "weekly")?;
        let tokens_left = (100.0 - f64::from(w.pct.min(100))) / 100.0;
        let time_left = hours_of(&w.resets)?.min(WEEK_HOURS) / WEEK_HOURS;
        (time_left > 0.0).then(|| tokens_left / time_left)
    }
}

/// `3d 4h`, `2h 15m` → hours.
pub fn hours_of(s: &str) -> Option<f64> {
    let mut h = 0.0;
    let mut any = false;
    for part in s.split_whitespace() {
        let (n, unit) = part.split_at(part.find(|c: char| !c.is_ascii_digit())?);
        let n: f64 = n.parse().ok()?;
        h += match unit {
            "d" => n * 24.0,
            "h" => n,
            "m" => n / 60.0,
            _ => return None,
        };
        any = true;
    }
    any.then_some(h)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Axis {
    /// `session` | `weekly` | `fable`.
    pub axis: String,
    pub pct: u32,
    pub resets: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    pub name: String,
    /// `running` | `stopped` | `failed`.
    pub status: String,
    pub pid: Option<u32>,
    pub uptime: String,
    pub addr: String,
    pub version: String,
    #[serde(default)]
    pub note: Option<String>,
    /// Warnings and errors the supervisor has seen from it (FAKE): what its
    /// Status band's header counts.
    #[serde(default)]
    pub warnings: u32,
    #[serde(default)]
    pub errors: u32,
    /// Where its log is (FAKE): the Status tab copies this, never shows the
    /// log.
    #[serde(default)]
    pub log: Option<String>,
    /// The versions the supervisor has seen run clean past its stability
    /// window, newest first (FAKE): what an auto rollback would go back to.
    #[serde(default)]
    pub proven: Vec<Proven>,
    /// When a restart or rollback asked from this face started it (the
    /// supervision record's `at`): its uptime is counted from here. Never in
    /// a seed.
    #[serde(skip)]
    pub started: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proven {
    pub version: String,
    /// When it was proven: `YYYY-MM-DD`.
    pub at: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub node: String,
    pub role: String,
    /// `online` | `offline`.
    pub status: String,
    /// 1, 5 and 15 minute load averages.
    pub load: [f32; 3],
    pub cpu_pct: u32,
    pub mem_pct: u32,
    /// The services running on this device.
    #[serde(default)]
    pub services: Vec<Service>,
    pub last_seen: String,
}
