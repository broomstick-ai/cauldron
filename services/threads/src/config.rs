//! Daemon configuration — `~/.threads/config.toml`.
//!
//! `threads` is a service, not a CLI: the `threads` binary with no args starts
//! the daemon, reading this file. Everything is optional with boring defaults
//! so a fresh machine runs with no config at all.
//!
//! ```toml
//! # ~/.threads/config.toml
//! chassis_port = 3653            # THE port: the chassis door (WS + /health)
//! data_dir = "~/.threads"        # SQLite + accounts registry live here
//! claude_bin = "claude"          # THREADS_CLAUDE_BIN env wins
//! secret = "..."                 # optional shared-secret auth (Q12)
//!
//! [accounts]
//! default = ""                   # machine default (CLAUDE_CONFIG_DIR unset)
//! work = "/Users/me/.threads/accounts/work"
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// **THE DAEMON'S PORT — the chassis door, and since R2.S4 the only one.**
///
/// `threads` SUPERSEDED CCW and inherited its reserved port **3651** for the
/// legacy WS door; the API then moved into `shapewire` and stood up here, on a
/// second port, while the legacy door kept serving ("since the API is being
/// moved in you'll need to run a second daemon until the migration happens and
/// that's ok"). The migration happened: every consumer reads shapes on this
/// port, so R2.S4 (2026-10-10) retired the `:3651` listener and this port is
/// the one the daemon binds — first, as the singleton (see
/// [`crate::server::bind`]).
///
/// **3653, and the numbers it is not.** 3649 is the mesh daemon, 3650 is a
/// deliberate gap, 3651 was the legacy door (now bound by nothing), and **3652
/// is `tts`** — which `core/tools/promote` also health-probes, so `+1` would
/// have put this server on another live service's port.
///
/// `0` is refused by `run_daemon`: a daemon with no port serves nothing.
/// The `port` key a pre-R2.S4 `config.toml` may still carry is ignored (serde
/// skips unknown keys); it named the retired door.
pub const DEFAULT_CHASSIS_PORT: u16 = 3653;

/// Parsed daemon config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// **THE DAEMON'S PORT** — the chassis door, the only one it binds.
    /// Default [`DEFAULT_CHASSIS_PORT`]; `0` is refused by `run_daemon`. Env
    /// `THREADS_CHASSIS_PORT` takes precedence (see
    /// [`Config::effective_chassis_port`]).
    #[serde(default = "default_chassis_port")]
    pub chassis_port: u16,
    /// Bind host. Default `127.0.0.1` (local-only).
    #[serde(default = "default_host")]
    pub host: String,
    /// State/data dir. `None` → `~/.threads` (honors `THREADS_HOME`).
    #[serde(default)]
    pub data_dir: Option<String>,
    /// claude binary path override. Env `THREADS_CLAUDE_BIN` takes precedence.
    #[serde(default)]
    pub claude_bin: Option<String>,
    /// Optional shared secret (Q12: "support a secret, don't require it").
    /// When set, every WS connection must authenticate with an `auth` frame
    /// before any other frame is honored. When unset the socket is open —
    /// AUI manages its own secret manifest; hardening comes with the OS.
    /// Env `THREADS_SECRET` takes precedence over the file value.
    #[serde(default)]
    pub secret: Option<String>,
    /// Optional inline account seeds (name → CLAUDE_CONFIG_DIR; `""` = machine
    /// default). Merged into the `accounts.toml` registry on daemon open.
    #[serde(default)]
    pub accounts: BTreeMap<String, String>,
    /// **THE IDLE CADENCE: how often (seconds) to poll `claude -p "/usage"`
    /// across the WHOLE POOL, whether or not anything is running on it**
    /// (DEC-15 layer 2). `0` disables this leg entirely and leaves only the busy
    /// leg, the free `rate_limit_event` push and the 429 backstop — unchanged
    /// meaning, and the only meaning `0` has ever had here.
    ///
    /// The poll is a real `claude` invocation — a slash command CCW's recon
    /// measured at `num_turns: 0` (zero cost) — so the daemon only ever calls
    /// it when a budget key, a per-thread threshold, or calibration actually
    /// wants a reading. A machine with none of those never shells out.
    /// Floor: 60s.
    ///
    /// # 3600, AND IT WAS 900 UNTIL 2026-09-20
    ///
    /// The operator:
    ///
    /// > *"the daemon should only be reading balances on accounts that are
    /// > active on the accelerated schedule. If there isn't any placement you
    /// > can move it to 1x/hour so we don't get rate limited."*
    ///
    /// Both halves of that sentence are about this field, and the first half was
    /// already true: the ACCELERATED schedule is
    /// [`Self::probe_interval_busy_secs`] and it polls only logins with a turn
    /// or a subagent in flight. This leg is the other one — the whole pool,
    /// including every account nothing is happening on — and 900 seconds was
    /// four sweeps an hour over every login to re-read numbers that had not
    /// moved.
    ///
    /// **What the cost actually is, because it is easy to argue the wrong one.**
    /// `/usage` spends no tokens and no quota. The cost is one `claude` process
    /// spawn per pool member per sweep and the API round trip inside it, and it
    /// is the round trips he is naming: four per account per hour, forever, on a
    /// machine where nothing is running, is the profile that attracts a rate
    /// limit for no information at all. At 3600 it is one.
    ///
    /// **What 3600 costs, stated rather than waved at.** This leg is the only
    /// thing keeping an IDLE account's reading from going stale forever, so the
    /// staleness floor for an account nothing is running on moves from 15
    /// minutes to an hour. That is acceptable precisely because it is the idle
    /// case: an idle account's reading does not move, and the instant work lands
    /// on it the busy leg picks it up at 120 s. The detection bound that
    /// actually matters — "how long can the daemon keep placing onto an
    /// exhausted account" — is the BUSY leg's and is untouched at two minutes,
    /// because an account with nothing running on it cannot be spending.
    ///
    /// **It is a DEFAULT, not a policy.** A deployment that wants the old
    /// cadence sets `probe_interval_secs = 900` and gets it.
    #[serde(default = "default_probe_interval")]
    pub probe_interval_secs: u64,
    /// **THE BUSY CADENCE: how often (seconds) to re-read `/usage` WHILE WORK IS
    /// ACTUALLY RUNNING — and only on the accounts that have the work.** `0`
    /// disables the busy scan (and the placement reading with it) and restores
    /// the single flat [`Self::probe_interval_secs`] sweep.
    ///
    /// The operator asked for exactly this shape:
    ///
    /// > *"If we can add a periodic scan to threads at the higher rate anthropic
    /// > allows, while threads are running only, then that would be useful in
    /// > many ways and can start the emit."*
    ///
    /// **Why a second interval instead of just lowering the first one.** The
    /// 900-second sweep polls EVERY login in the pool, whether or not anything
    /// is happening on it. Lowering it to two minutes would multiply the cost by
    /// 7.5 across the whole pool to get a fresher number for the one or two
    /// accounts anyone is watching. This scan polls only accounts with a turn or
    /// a subagent in flight — normally one, occasionally two or three — so the
    /// fresher number costs less in absolute terms than the slow whole-pool
    /// sweep it sits beside.
    ///
    /// **WHAT THE COST ACTUALLY IS, because it is easy to argue the wrong one.**
    /// `/usage` is a slash command measured at `num_turns: 0`
    /// ([`crate::gauge`]): it consumes no tokens and spends no quota, and it
    /// does not move the meter it reads. So the cost is NOT rate limit and is
    /// not the operator's subscription. It is ONE `claude` PROCESS SPAWN per
    /// busy login per tick — process startup, a config-dir read and an API round
    /// trip. That is the whole budget being spent here, and it is why the
    /// default is measured in minutes rather than seconds.
    ///
    /// **WHY 120 SECONDS.** Four bounds, and 120 is the loosest number that
    /// satisfies all of them:
    ///
    /// * **The detection floor it sets.** `gauge_bar_pct` turns a measured 100%
    ///   into a placement bar, so the poll interval IS how long the daemon can
    ///   keep placing threads onto an exhausted account. On 2026-08-14 that gap
    ///   was thirteen minutes and five threads (see [`Self::gauge_bar_pct`]).
    ///   Two minutes bounds the same failure at two minutes — while work is
    ///   running, which is the only time the failure can happen at all.
    /// * **The settle delay it must clear.** A reading takes seconds to catch up
    ///   with spend — that is what the settle ladder MEASURES rather than
    ///   assumes, and its own rungs run out at 120s. Polling faster than the
    ///   meter settles buys noise, not information.
    /// * **The process cost it accepts.** One spawn per busy login per two
    ///   minutes is at most ~0.5/min in the ordinary case and ~1.5/min with
    ///   three accounts lit up — comfortably below the edge-poll pair this
    ///   daemon already spends on a single dispatch.
    /// * **What it does NOT collide with.** The closing settle ladder only walks
    ///   an account that is IDLE for us, and this scan only polls accounts that
    ///   are NOT. The two are disjoint by construction, so raising this rate
    ///   cannot corrupt a calibration bracket.
    ///
    /// Floor: 30s, applied where the timer is built. Below that the process cost
    /// stops being negligible and the settle argument above stops holding, so a
    /// smaller number in the file is clamped rather than honoured.
    #[serde(default = "default_probe_interval_busy")]
    pub probe_interval_busy_secs: u64,
    /// Poll the gauge for CALIBRATION, not only for enforcement.
    ///
    /// The sweep used to poll only when a budget key or a per-thread threshold
    /// existed. The audio interface sets neither, so in the real deployment
    /// **no `/usage` poll was ever recorded** — and with no polls there are no
    /// brackets, no calibration samples, and `tokens_per_percent` stays absent
    /// forever no matter how anything else is arranged. Percentage budgets are
    /// unsafe until calibration exists, and calibration cannot start without a
    /// reading, so this defaults ON: while any (account, axis) is uncalibrated
    /// the sweep polls. The poll is a `num_turns: 0` slash command — measured at
    /// zero cost, zero turns, and it does not move the meter it reads.
    #[serde(default = "default_true")]
    pub calibrate: bool,
    /// EDGE POLLING: poll at the boundaries of our own activity rather than only
    /// on a timer (account-selection-design §3.2).
    ///
    /// A 900-second sweep almost never lands in the specific idle gap a
    /// calibration bracket needs — the `before` poll has to sit between the
    /// previous window's end and this window's start — so `Unbracketed` was the
    /// dominant rejection. Polling when an account goes idle, and again before
    /// dispatching onto an account that has been idle, puts a poll in that gap
    /// BY CONSTRUCTION. The timer stays as a floor.
    #[serde(default = "default_true")]
    pub edge_poll: bool,
    /// The ladder of offsets, in seconds after an account goes idle for us, at
    /// which the closing edge READS the meter.
    ///
    /// These reads are unrecorded: they are the settle MEASUREMENT
    /// (`gauge::settle_profile`), and the closing observation pair is taken only
    /// once two consecutive rungs agree. No settle constant is hard-coded
    /// anywhere, because none has ever been measured and guessing it low reads a
    /// small delta, which estimates a large limit, which is the direction where a
    /// budget fails to trip. If the ladder runs out while the meter is still
    /// moving, nothing is recorded and the window simply stays unbracketed.
    ///
    /// **At least two rungs are required** — one reading can never be shown to
    /// have settled, so a one-rung ladder records no closing poll at all.
    /// Fractional seconds are allowed.
    #[serde(default = "default_settle_ladder")]
    pub settle_ladder_secs: Vec<f64>,
    /// How a budget kill tears an execution down: `process_group` (default —
    /// signals claude AND everything it spawned) or `child_only` (the
    /// orphaning comparison arm). `THREADS_TEARDOWN` takes precedence. This
    /// is a SEAM for M-9's strategy trial, not a settled answer.
    #[serde(default)]
    pub teardown_strategy: Option<String>,
    /// **THE `turn_end` GUARD. Default OFF — this preserves today's behaviour.**
    ///
    /// [`crate::event::SendIntent::TurnEnd`] promises delivery only once "the
    /// parent result arrived AND every async subagent has gone quiet". The
    /// second half has never once been true: a casing bug meant
    /// `Liveness.outstanding` was never populated, so `busy()` counted no
    /// subagents and `TurnEnd` and `Immediate` were the same intent. Fixing the
    /// parser makes `outstanding` real, which would ACTIVATE `TurnEnd` semantics
    /// for the first time on live threads and change when queued messages are
    /// delivered.
    ///
    /// That timing change does not ship silently. With this `false`,
    /// `maybe_dispatch` uses the same idle test it uses today and delivery
    /// timing is unchanged, while everything OBSERVATIONAL —
    /// `Push::Liveness.running_subagents`, thread status, the ledger's
    /// `agent_launched` / `agent_completed` / `task_*` rows — becomes truthful.
    /// The daemon sees; nothing acts.
    ///
    /// With this `true`, a `turn_end` message additionally waits until the
    /// thread is owed no MANAGED child — a fold over its durable ledger
    /// (`crate::managed`, R4.S2), which survives a restart. Native in-process
    /// agents need no wait of their own: they cannot outlive the turn's
    /// process. (Before R4.S2 this read `Liveness::busy()`, kept true by an
    /// mtime sweep polled after the turn; both are deleted.) It cannot
    /// deadlock on a child that ends: the child's report removes it from the
    /// fold. A child that never ends keeps the message waiting, by design.
    #[serde(default)]
    pub turn_end_waits_for_subagents: bool,
    /// **THE GAUGE MAY BAR AN ACCOUNT.** Observed percent, at or above which a
    /// `/usage` reading is written to `accounts.limit_until` exactly as a 429
    /// would be. `0` (or anything > 100) disables the behaviour entirely and
    /// restores the old "the poller only ever observes" posture.
    ///
    /// **Why this exists.** On 2026-08-14 at 19:26:13 the daemon's own poller
    /// read `weekly_all = 100.0` on `owner-account` and did nothing
    /// with it, because the gauge could observe but not bar. It then placed
    /// five more threads onto that exhausted account, six subagents died with
    /// "You've hit your weekly limit", and failover fired only at 19:39:37 when
    /// the MAIN turn finally took its own 429 — thirteen minutes after the
    /// daemon knew, and thirty-three minutes into operator-facing silence.
    ///
    /// A measured 100% is not a fabricated estimate, so CON-5 (never gate on a
    /// gauge you have not calibrated) does not apply — thresholds read the raw
    /// observed percent and bite from the first observation by design
    /// (`budgets`' module docs). And a wrong bar self-heals in two independent
    /// ways: [`crate::runtime::ThreadManager`]'s `note_account_served` clears
    /// `limit_until` on ANY completed turn, and
    /// [`crate::accounts::AccountState::limit_is_stale`] retires a window whose
    /// login identity no longer matches.
    #[serde(default = "default_gauge_bar_pct")]
    pub gauge_bar_pct: f64,
}

/// 3600 — the IDLE cadence, one whole-pool sweep an hour. Justified in full on
/// [`Config::probe_interval_secs`]; in one line, the accelerated schedule is the
/// busy leg and covers the accounts that are actually spending, so sweeping
/// every idle login four times an hour bought rate-limit exposure and no
/// information. It was 900 until 2026-09-20.
fn default_probe_interval() -> u64 {
    3600
}

/// 120 — the busy cadence. Justified in full on
/// [`Config::probe_interval_busy_secs`]; in one line, it is the loosest interval
/// that still bounds "how long can we keep placing onto an exhausted account"
/// at two minutes, and it costs one `claude` spawn per BUSY login rather than
/// one per pool member.
fn default_probe_interval_busy() -> u64 {
    DEFAULT_PROBE_INTERVAL_BUSY_SECS
}

/// The busy cadence's default, as a name — so the one shape that sizes a window
/// off it (`crate::shapes::gauge_history::PER_AXIS`) reads the number rather
/// than restating it.
pub const DEFAULT_PROBE_INTERVAL_BUSY_SECS: u64 = 120;

/// The floor the busy cadence is clamped to. See
/// [`Config::probe_interval_busy_secs`] — below 30s the process cost stops being
/// negligible and the reading stops having settled.
pub const PROBE_BUSY_FLOOR_SECS: u64 = 30;

/// 100.0 — bar only on an EXHAUSTED reading. The margin below it is a
/// per-thread `Thresholds.weekly`, not this (see
/// [`crate::budgets::DEFAULT_WEEKLY_THRESHOLD_PCT`]): a threshold parks a
/// thread and lets it fail over, where this writes a hard limit window.
fn default_gauge_bar_pct() -> f64 {
    100.0
}

fn default_true() -> bool {
    true
}

/// `+5s`, `+30s`, `+120s` — the ladder the design named for the first
/// measurement. Three rungs an order of magnitude apart bracket any plausible
/// settle delay while costing three free polls. Fractional seconds are allowed
/// so a test can walk a whole ladder in well under a second.
fn default_settle_ladder() -> Vec<f64> {
    vec![5.0, 30.0, 120.0]
}

fn default_chassis_port() -> u16 {
    DEFAULT_CHASSIS_PORT
}
fn default_host() -> String {
    "127.0.0.1".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // **`0` — DISABLED BY DEFAULT IN `Default`, AND ONLY THERE.** The
            // serde default is `DEFAULT_CHASSIS_PORT`, so a real `threads.toml`
            // gets the server; `Config::default()` is what every TEST
            // builds from, and 60-odd test binaries each binding 3653 would race
            // each other for a port none of them asked for. `run_daemon` is the
            // one caller that wants it, and it comes through the file or the env.
            chassis_port: 0,
            host: default_host(),
            data_dir: None,
            claude_bin: None,
            secret: None,
            accounts: BTreeMap::new(),
            probe_interval_secs: default_probe_interval(),
            probe_interval_busy_secs: default_probe_interval_busy(),
            calibrate: default_true(),
            edge_poll: default_true(),
            settle_ladder_secs: default_settle_ladder(),
            teardown_strategy: None,
            // OFF: the repaired liveness accounting is observed, not acted on.
            turn_end_waits_for_subagents: false,
            gauge_bar_pct: default_gauge_bar_pct(),
        }
    }
}

impl Config {
    /// Parse from TOML text.
    pub fn from_toml(s: &str) -> Result<Self> {
        toml::from_str(s).map_err(|e| Error::Config(format!("parsing config.toml: {e}")))
    }

    /// Load from a path; **a missing file is an EMPTY file, not `Default`.**
    ///
    /// It returned `Self::default()` until 2026-09-26, and the difference is not
    /// cosmetic: `Default` deliberately sets `chassis_port: 0` so that sixty-odd
    /// test binaries do not race for one port, while the SERDE default is
    /// `DEFAULT_CHASSIS_PORT`. This machine has no `config.toml` at all, so the
    /// daemon took `Default`, logged `chassis server disabled (chassis_port = 0)`
    /// and never stood the second server up — the whole API move was promoted and
    /// the port it moved to was dark. The comment on `Default` had assumed
    /// "`run_daemon` … comes through the file or the env", and there was neither.
    ///
    /// Routing a missing file through `from_toml("")` means "no config" and "an
    /// empty config" answer the same, which is the only defensible reading, and
    /// `Default` keeps its test-shaped value for the tests that build from it.
    pub fn load(path: &std::path::Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::from_toml(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::from_toml(""),
            Err(e) => Err(Error::Config(format!("reading {}: {e}", path.display()))),
        }
    }

    /// The resolved data dir (config override, else the state home).
    pub fn resolved_data_dir(&self) -> Result<PathBuf> {
        match &self.data_dir {
            Some(d) if !d.is_empty() => Ok(PathBuf::from(shellexpand_home(d))),
            _ => state_home(),
        }
    }

    /// The effective shared secret: `THREADS_SECRET` env, else the file value.
    /// The chassis server's port, with `THREADS_CHASSIS_PORT` taking precedence
    /// over the file value. `0` means "do not stand the second server up" —
    /// which is what every test that does not need it gets, so no test binary
    /// races another for 3653.
    ///
    /// **A VALUE THAT IS NOT A PORT IS REFUSED, NOT IGNORED.**
    ///
    /// **THE DEFECT THIS FIXES (2026-09-26).** This was
    /// `s.parse().unwrap_or(self.chassis_port)`, so `THREADS_CHASSIS_PORT=garbage`
    /// — a typo, a shell expansion that produced nothing usable, a `$PORT` that
    /// was never exported — silently took the file value, and the daemon came up
    /// on a port nobody had asked for with not one line said about it. That is the
    /// same class as `chassis_port: 0` taking `Default` and logging a sentence
    /// nobody read while *"the whole API move was promoted and the port it moved
    /// to was dark"*: a misconfiguration that disables or misdirects a feature and
    /// reports nothing. The operator set the variable; the only honest answers are
    /// to use it or to refuse.
    ///
    /// So it returns a [`Result`] and `run_daemon` fails the boot on it, which is
    /// the same treatment the very next line of that function already gives an
    /// unparseable bind address. An EMPTY or unset variable still means *"I did not
    /// set this"* and still takes the file value — absent and wrong are two
    /// different states and only one of them is an error.
    pub fn effective_chassis_port(&self) -> Result<u16> {
        match std::env::var("THREADS_CHASSIS_PORT").ok().as_deref() {
            Some(s) if !s.is_empty() => s.parse::<u16>().map_err(|e| {
                Error::Config(format!(
                    "THREADS_CHASSIS_PORT={s:?} is not a port number (0-65535): {e}. \
                     Refusing to fall back to the file value ({}) — a chassis server \
                     standing up somewhere the operator did not ask for, quietly, is \
                     the failure this refusal exists to prevent. Unset the variable to \
                     use the file value. (0 is refused at boot since R2.S4: the chassis door is the only door.)",
                    self.chassis_port
                ))
            }),
            _ => Ok(self.chassis_port),
        }
    }

    pub fn effective_secret(&self) -> Option<String> {
        std::env::var("THREADS_SECRET")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| self.secret.clone().filter(|s| !s.is_empty()))
    }

    /// Apply `claude_bin` to the process env so [`crate::spawn::claude_bin`]
    /// picks it up, unless `THREADS_CLAUDE_BIN` is already set.
    pub fn apply_claude_bin(&self) {
        if std::env::var("THREADS_CLAUDE_BIN").is_err() {
            if let Some(b) = &self.claude_bin {
                if !b.is_empty() {
                    std::env::set_var("THREADS_CLAUDE_BIN", b);
                }
            }
        }
    }
}

/// The state home: `$THREADS_HOME`, else `~/.threads`.
pub fn state_home() -> Result<PathBuf> {
    if let Ok(h) = std::env::var("THREADS_HOME") {
        if !h.is_empty() {
            return Ok(PathBuf::from(h));
        }
    }
    let home = std::env::var("HOME").map_err(|_| Error::Config("HOME is not set".into()))?;
    Ok(PathBuf::from(home).join(".threads"))
}

/// Resolve the config path: explicit argv path → `$THREADS_CONFIG` → state
/// home `config.toml`.
pub fn resolve_config_path(explicit: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(PathBuf::from(p));
    }
    if let Ok(p) = std::env::var("THREADS_CONFIG") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    Ok(state_home()?.join("config.toml"))
}

/// Expand a leading `~/` to `$HOME` (boring, no external crate).
fn shellexpand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    p.to_string()
}

#[cfg(test)]
mod missing_file_tests {
    use super::*;

    /// **A MISSING CONFIG MUST STAND THE SECOND SERVER UP.**
    ///
    /// The API move shipped behind a second port and this is what decides whether
    /// that port exists on a machine with no `config.toml` — which is this one.
    /// It was dark after a successful promote, and nothing failed: the daemon
    /// logged that it was disabled and served the legacy port perfectly.
    #[test]
    fn a_missing_config_is_an_empty_config_and_not_default() {
        let missing = std::path::Path::new("/nonexistent/threads/config.toml");
        let loaded = Config::load(missing).expect("a missing file is not an error");

        assert_eq!(
            loaded.chassis_port,
            DEFAULT_CHASSIS_PORT,
            "a machine with no config.toml must still get the chassis server; \
             `Config::default()`'s 0 is for tests that build from it directly"
        );
        assert_eq!(
            loaded.chassis_port,
            Config::from_toml("").unwrap().chassis_port,
            "no config and an empty config must answer the same"
        );
        assert_eq!(
            Config::default().chassis_port, 0,
            "and Default keeps its test-shaped value, so binaries do not race"
        );
    }

    /// **A PRE-R2.S4 `config.toml` STILL LOADS, AND ITS `port` MOVES NOTHING.**
    ///
    /// `port = 3651` named the legacy door, which no longer exists. A config
    /// that still carries it must not fail the boot (the daemon would refuse to
    /// come up over a key that now means nothing) and must not move the port
    /// the daemon actually binds. The second assertion is the positive half:
    /// the same text with `chassis_port` set is read, so the first is not
    /// passing because the parser ignores the whole file.
    ///
    /// **Forced red** (R2.S4) by `#[serde(deny_unknown_fields)]` on `Config`:
    /// `a legacy \`port\` key must still parse: … unknown field \`port\``.
    #[test]
    fn the_retired_port_key_is_ignored_and_the_chassis_port_is_read() {
        let legacy = Config::from_toml("port = 3651\n").expect("a legacy `port` key must still parse");
        assert_eq!(legacy.chassis_port, DEFAULT_CHASSIS_PORT, "`port` must not move the bound port");
        let both = Config::from_toml("port = 3651\nchassis_port = 4999\n").unwrap();
        assert_eq!(both.chassis_port, 4999, "the file IS read — `chassis_port` lands");
    }
}
