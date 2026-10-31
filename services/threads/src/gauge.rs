//! The LAYERED usage gauge (DEC-15) — how `threads` learns "percent of limit"
//! per account, and how it converts that percent into a live TOKEN limit.
//!
//! Four layers, in order of preference, exactly as the gauge scout settled
//! Q-20 (`gauge-scout-report`, workspace `aui-tweaks`):
//!
//! 1. **`rate_limit_event`** — a free PUSH line real claude emits inline on
//!    every turn (`{status, resetsAt, rateLimitType, utilization,
//!    isUsingOverage}`). `utilization` is a **0-1 fraction** (the `/usage`
//!    prose is 0-100 — the normalization every consumer must handle). It was
//!    previously swallowed as `ThreadEvent::Unknown`; it is now a first-class
//!    ledger event and a gauge observation.
//! 2. **`/usage` poll** — `claude -p "/usage" --output-format json
//!    --no-session-persistence`, ported from CCW (`ccw::claude::usage_snapshot`
//!    + `ccw::usage`). The ONLY gauge that covers all three axes in one call,
//!    and CCW's recon measured it at `num_turns: 0` (zero cost). This is the
//!    authority ([`ClaudeUsageProbe`]).
//! 3. **Parsed 429s** — the backstop. [`classify_limit_pool`] fixes the
//!    permanent `LimitHit.pool: None` by classifying the refusal prose.
//! 4. **`tokens_per_percent` calibration** — CCW's math, ported: pair a token
//!    delta from `usage_ledger` with a percent delta from two observations of
//!    the same (account, axis) window, and the ratio IS the live limit
//!    estimate (DEC-12: limits are not knowable constants).
//!
//! **CON-5 / CCW's INTENT #202 rule is retained verbatim: never fabricate a
//! percent before one real calibration sample exists.** An axis with no
//! calibration reports `calibrating` and does not gate anything.
//!
//! ## The fourth axis: CASH
//!
//! Round-7 item 5 added a cash / extra-usage axis denominated in DOLLARS
//! rather than percent. It needs no calibration at all — `costUSD` arrives on
//! every `result_turn`, so the meter is measured, not estimated, and it is
//! POOLED across accounts ($20 means $20 in total). What it still lacks is an
//! OBSERVABLE gauge: no claude build seen so far reports a cash row, so the
//! threshold half of the axis is inert. [`Axis::from_pool_label`] and
//! [`parse_usage_prose`] already accept one (a `$` row under a label like
//! `extra usage`); nothing here fabricates a reading until one arrives.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::accounts::UsageProbe;
use crate::reset::{parse_reset, ResetTime};
use crate::store::ObservationRow;

/// The rolling session window length. Used to place the window START behind an
/// OBSERVED end (`resetsAt`), and — since the null-reset fix — to run an
/// ASSUMED window forward from the observation when the gauge reports no end at
/// all ([`ResetBasis::Assumed`]). The assumed instant is derived per read and
/// never stored, so nothing fabricates a `resets_at`. This axis can only ever
/// assume: there is no second five-hour pool to borrow a reset from, and it is
/// also the axis missing the most resets, because a five-hour pool spends more
/// of its life freshly rolled than a weekly one. `five_hour` is Claude Code's
/// own name for this pool.
pub const SESSION_WINDOW_SECS: i64 = 5 * 3600;
/// The weekly window length (Claude Code's `seven_day` pool).
pub const WEEK_WINDOW_SECS: i64 = 7 * 24 * 3600;
/// The CASH (extra-usage) window length. Claude bills extra usage per billing
/// period; no live gauge reports its reset instant yet, so this length is only
/// ever used as the ROLLING fallback window (see the report's cash-gauge
/// section for what a live gauge still needs). It is never used to claim a
/// reset instant we have not observed.
pub const CASH_WINDOW_SECS: i64 = 30 * 24 * 3600;

/// Where an effective reset instant CAME FROM — the honesty tag that lets an
/// assumed window be used for ranking without ever being mistaken for a
/// measurement.
///
/// The upstream gauge reports NO reset time for a pool the account has never
/// consumed against: the window does not exist until the first token is spent
/// on it, so there is nothing to report. Verified against the live store —
/// every one of the 6,843 null-`resets_at` observations sits at exactly 0.0%,
/// with no exceptions. That absence used to make the account UNRANKABLE, which
/// inverted placement exactly: the emptiest account, the one with the most
/// room, was dropped from the front *because* it was empty.
///
/// The operator's ruling is to assume rather than abstain — *"if an interval
/// has an unknown value, of when it starts and when it ends, then it starts
/// right now … you can use that in your pressure calculation when an account is
/// not giving you that field."*
///
/// This tag is derived at READ time and is never persisted.
/// `usage_observations.resets_at` keeps storing SQL NULL, because that column's
/// honesty is load-bearing and `reset.rs` states the policy outright ("we never
/// fabricate a time"). A later real observation therefore always wins, simply
/// by existing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetBasis {
    /// The gauge reported this instant for this very (account, axis).
    Observed,
    /// BORROWED from the same account's weekly all-models axis. The fable axis
    /// has no reset time of its own here, but the two weekly axes are the same
    /// week: across 12,352 matched pairs in the live store they always share a
    /// reset instant, and the only differences are exactly 60 seconds — an
    /// upstream rendering artifact of "12:59pm" versus "1pm", already inside
    /// [`WINDOW_MATCH_TOLERANCE_SECS`]. A borrowed instant is REAL, so it is
    /// always preferred over an assumed one.
    Borrowed,
    /// ASSUMED: no reset time anywhere, so the window is taken to have opened
    /// at the moment of observation and to run the axis's full length. The
    /// session axis can only ever land here — there is no second six-hour axis
    /// to borrow from — which is also the axis with the most missing resets,
    /// since a five-hour pool spends more of its life freshly rolled than a
    /// weekly one does.
    Assumed,
}

/// A reset instant together with [`ResetBasis`] — what we will rank on, and
/// how much it is worth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveReset {
    pub at: DateTime<Utc>,
    pub basis: ResetBasis,
}

impl ResetBasis {
    /// The wire tag. A reader that is shown a reset instant is owed how much
    /// that instant is WORTH — an assumed window is a guess and a reader who
    /// cannot tell will present it with the same confidence as a reported one.
    pub fn as_str(&self) -> &'static str {
        match self {
            ResetBasis::Observed => "observed",
            ResetBasis::Borrowed => "borrowed",
            ResetBasis::Assumed => "assumed",
        }
    }
}

impl EffectiveReset {
    /// True when nothing upstream reported this instant for any axis.
    pub fn is_assumed(&self) -> bool {
        matches!(self.basis, ResetBasis::Assumed)
    }
}

/// What a limit is measured in. The three percentage axes divide by a limit
/// that must be estimated (DEC-12); the cash axis is measured DIRECTLY in
/// dollars, which is why it never reports `calibrating`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Percent,
    Dollars,
}

impl Unit {
    pub fn as_str(&self) -> &'static str {
        match self {
            Unit::Percent => "percent",
            Unit::Dollars => "dollars",
        }
    }
}

/// The limit axes the operator's decisions name. Round-7 item 4/5 widened this
/// from "budgets meter two of three" to **budgets AND thresholds on every
/// axis**: the three percentage axes (session, weekly all-model, weekly fable)
/// plus a CASH axis denominated in DOLLARS and pooled across accounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    /// The rolling session pool (`/usage`: `session`; stream: `five_hour`).
    Session,
    /// The weekly all-models pool (`/usage`: `week (all models)`; stream:
    /// `seven_day`).
    WeeklyAll,
    /// The weekly model-scoped pool the operator calls "fable" (`/usage`:
    /// `week (Fable)`).
    WeeklyFable,
    /// The cash / extra-usage limit, in DOLLARS (round-7 item 5). Every usage
    /// limit gets thresholds and budgets, and this one is not a percentage:
    /// "$20 is $20 across all the accounts", so its meter pools dollars.
    Cash,
}

impl Axis {
    pub const ALL: [Axis; 4] = [
        Axis::Session,
        Axis::WeeklyAll,
        Axis::WeeklyFable,
        Axis::Cash,
    ];
    /// The three PERCENTAGE axes — the ones whose limit has to be estimated.
    pub const PERCENT_AXES: [Axis; 3] = [Axis::Session, Axis::WeeklyAll, Axis::WeeklyFable];
    /// Every axis a BUDGET meters. Round-7 item 4/5: all of them.
    pub const BUDGET_AXES: [Axis; 4] = Axis::ALL;

    pub fn as_str(&self) -> &'static str {
        match self {
            Axis::Session => "session",
            Axis::WeeklyAll => "weekly_all",
            Axis::WeeklyFable => "weekly_fable",
            Axis::Cash => "cash",
        }
    }

    pub fn parse(s: &str) -> Option<Axis> {
        match s {
            "session" => Some(Axis::Session),
            "weekly_all" | "weekly" => Some(Axis::WeeklyAll),
            "weekly_fable" | "fable" => Some(Axis::WeeklyFable),
            "cash" | "extra_usage" | "overage" => Some(Axis::Cash),
            _ => None,
        }
    }

    /// What this axis is denominated in.
    pub fn unit(&self) -> Unit {
        match self {
            Axis::Cash => Unit::Dollars,
            _ => Unit::Percent,
        }
    }

    /// The window LENGTH used to place a window start behind an observed
    /// reset instant.
    pub fn window_secs(&self) -> i64 {
        match self {
            Axis::Session => SESSION_WINDOW_SECS,
            Axis::WeeklyAll | Axis::WeeklyFable => WEEK_WINDOW_SECS,
            Axis::Cash => CASH_WINDOW_SECS,
        }
    }

    /// The reset instant implied by ASSUMING a window opened at `started_at`
    /// and runs this axis's full length. See [`ResetBasis::Assumed`] for when
    /// that assumption is legal and why.
    pub fn assumed_reset(&self, started_at: DateTime<Utc>) -> DateTime<Utc> {
        started_at + chrono::Duration::seconds(self.window_secs())
    }

    /// True when this axis counts only model-scoped ("fable") consumption —
    /// the token side of its calibration must be filtered the same way, and a
    /// charge from another model must not reach its meter.
    pub fn is_model_scoped(&self) -> bool {
        matches!(self, Axis::WeeklyFable)
    }

    /// Does a charge from `model` belong on this axis? Session, weekly-all and
    /// cash take everything; the fable axis takes fable-model usage only
    /// (round-7 item 4 — a fable BUDGET that counted Sonnet tokens would be
    /// measuring the wrong pool).
    pub fn accepts_model(&self, model: &str) -> bool {
        !self.is_model_scoped() || is_fable_model(model)
    }

    /// Map an as-reported `/usage` pool label to an axis. Labels are kept
    /// as-reported by CCW because "accounts expose different model-scoped
    /// pools"; unmappable labels return `None` and are recorded as unmapped
    /// rather than guessed at.
    pub fn from_pool_label(label: &str) -> Option<Axis> {
        let l = label.trim().to_ascii_lowercase();
        if l == "session" || l.starts_with("session") {
            return Some(Axis::Session);
        }
        // The cash / extra-usage row. No live claude build has been OBSERVED
        // emitting one; the mapping exists so a gauge that appears is picked
        // up without a code change, and the axis stays unobserved until then.
        if l.contains("extra usage") || l.contains("overage") || l.contains("cash") {
            return Some(Axis::Cash);
        }
        if l.starts_with("week") {
            if l.contains("all") {
                return Some(Axis::WeeklyAll);
            }
            if l.contains("fable") {
                return Some(Axis::WeeklyFable);
            }
            return None; // a model-scoped weekly pool we have no axis for
        }
        None
    }

    /// Map a `rate_limit_event.rateLimitType` to an axis. Only `seven_day`
    /// has ever been OBSERVED (burn-in 2026-07-27); the other arms are named
    /// from Claude Code's own vocabulary and stay inert until one shows up.
    pub fn from_rate_limit_type(ty: &str) -> Option<Axis> {
        let t = ty.trim().to_ascii_lowercase();
        match t.as_str() {
            "seven_day" | "weekly" | "week" => Some(Axis::WeeklyAll),
            "five_hour" | "session" | "hourly" => Some(Axis::Session),
            _ => {
                if t.contains("fable") || t.contains("opus") {
                    Some(Axis::WeeklyFable)
                } else {
                    None
                }
            }
        }
    }
}

/// Is this model id part of the "fable" weekly pool? Model ids are passed
/// through verbatim so tiers keep floating with Claude Code updates, so the
/// test is on the NAME, exactly as the calibration's `model_filter` already
/// matched (`LOWER(model) LIKE '%fable%'`) — the two must agree or a fable
/// budget would charge on one definition and calibrate on another.
pub fn is_fable_model(model: &str) -> bool {
    model.to_ascii_lowercase().contains("fable")
}

/// Classify which pool a 429 / usage-limit refusal was about, from its prose.
/// This is the fix for `LimitHit.pool`'s permanent `None` (DEC-15 layer 3):
/// the information was already sitting unused in `text`.
pub fn classify_limit_pool(text: &str) -> Option<Axis> {
    let t = text.to_ascii_lowercase();
    // Most specific first: a model-scoped weekly limit names the model.
    if (t.contains("fable") || t.contains("opus")) && (t.contains("week") || t.contains("limit")) {
        return Some(Axis::WeeklyFable);
    }
    if t.contains("weekly") || t.contains("week") || t.contains("7-day") || t.contains("seven day")
    {
        return Some(Axis::WeeklyAll);
    }
    if t.contains("session limit") || t.contains("5-hour") || t.contains("five hour") {
        return Some(Axis::Session);
    }
    None
}

// ── `/usage` parsing (ported from `ccw::usage`) ──────────────────────────────

/// One parsed `/usage` pool row.
#[derive(Debug, Clone, PartialEq)]
pub struct PoolObservation {
    /// As-reported label with the leading `Current ` stripped, e.g. `session`,
    /// `week (all models)`, `week (Fable)`.
    pub pool: String,
    /// Percent of this pool consumed, **0-100**.
    pub pct: f64,
    /// The parsed reset descriptor, if the row carried one.
    pub reset: Option<ResetTime>,
}

impl PoolObservation {
    pub fn axis(&self) -> Option<Axis> {
        Axis::from_pool_label(&self.pool)
    }
}

/// A full parsed `/usage` snapshot for one account.
#[derive(Debug, Clone, Default)]
pub struct UsageSnapshot {
    pub pools: Vec<PoolObservation>,
    /// The `/usage` run's own session id (the probe uses
    /// `--no-session-persistence`, so this is informational only).
    pub session_id: Option<String>,
}

#[derive(Deserialize)]
struct UsageEnvelope {
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

/// Parse the full JSON output of `claude -p "/usage" --output-format json`.
pub fn parse_usage_json(raw: &str) -> std::result::Result<UsageSnapshot, String> {
    let json_start = raw.find('{').ok_or("no JSON object in /usage output")?;
    let env: UsageEnvelope = serde_json::from_str(raw[json_start..].trim())
        .map_err(|e| format!("parsing /usage JSON: {e}"))?;
    let prose = env.result.unwrap_or_default();
    Ok(UsageSnapshot {
        pools: parse_usage_prose(&prose),
        session_id: env.session_id,
    })
}

/// Parse the prose body (the `result` string) into pool rows. Tolerant of
/// indentation and surrounding narrative lines.
///
/// ```text
/// Current session: 99% used · resets Jul 25 at 1:59am (America/Chicago)
/// Current week (all models): 73% used · resets Jul 25 at 12:59pm (America/Chicago)
/// Current week (Fable): 50% used · resets Jul 25 at 1pm (America/Chicago)
/// ```
pub fn parse_usage_prose(prose: &str) -> Vec<PoolObservation> {
    let mut out = Vec::new();
    for line in prose.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("Current ") else {
            continue;
        };
        let Some((label, tail)) = rest.split_once(':') else {
            continue;
        };
        let tail = tail.trim();
        // Percentage rows (every pool claude is OBSERVED to report) carry a
        // `%`. A cash/extra-usage row would carry a `$` instead — the value is
        // then DOLLARS, not percent, and the axis's unit says so. No live
        // build has been seen emitting one; this is the seam, not a claim.
        let value = match tail.find('%') {
            Some(i) => trailing_number(&tail[..i]),
            None => tail.find('$').and_then(|i| leading_number(&tail[i + 1..])),
        };
        let Some(pct) = value else {
            continue;
        };
        let reset = tail
            .find("resets ")
            .map(|i| tail[i + "resets ".len()..].trim())
            .and_then(parse_reset);
        out.push(PoolObservation {
            pool: label.trim().to_string(),
            pct,
            reset,
        });
    }
    out
}

/// The number immediately BEFORE the end of `s` (`"12"` in `"…used: 12"`).
fn trailing_number(s: &str) -> Option<f64> {
    let digits: String = s
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    digits.parse().ok()
}

/// The number at the START of `s` (`"12.34"` in `"12.34 of $20"`).
fn leading_number(s: &str) -> Option<f64> {
    let digits: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits.parse().ok()
}

// ── the poll probe (DEC-15 layer 2) ──────────────────────────────────────────

/// The real [`UsageProbe`]: shells CCW's zero-cost `/usage` invocation under
/// the account's `CLAUDE_CONFIG_DIR`.
///
/// This replaces `NoProbe` as the daemon default when the config enables
/// probing. It is a real `claude` invocation (a slash command, `num_turns: 0`
/// in CCW's recon), so the daemon only ever calls it when a budget or a
/// threshold actually needs an axis — see `Config::probe_interval`.
pub struct ClaudeUsageProbe {
    pub timeout: Duration,
}

impl Default for ClaudeUsageProbe {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(60),
        }
    }
}

impl UsageProbe for ClaudeUsageProbe {
    fn probe(&self, config_dir: Option<&str>) -> Option<Vec<(String, f64, Option<String>)>> {
        let mut cmd = std::process::Command::new(crate::spawn::claude_bin());
        cmd.args([
            "-p",
            "/usage",
            "--output-format",
            "json",
            "--no-session-persistence",
        ]);
        // Account isolation, identical to a turn spawn.
        cmd.env_remove("ANTHROPIC_API_KEY");
        match config_dir {
            Some(d) => {
                cmd.env("CLAUDE_CONFIG_DIR", d);
            }
            None => {
                cmd.env_remove("CLAUDE_CONFIG_DIR");
            }
        }
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::null());
        let out = output_with_timeout(cmd, self.timeout)?;
        let text = String::from_utf8_lossy(&out);
        let snap = parse_usage_json(&text).ok()?;
        if snap.pools.is_empty() {
            return None;
        }
        Some(
            snap.pools
                .into_iter()
                .map(|p| (p.pool, p.pct, p.reset.map(|r| r.raw)))
                .collect(),
        )
    }
}

/// Run a command, killing it (and returning `None`) if it outlives `timeout`.
fn output_with_timeout(mut cmd: std::process::Command, timeout: Duration) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => return None,
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut buf = Vec::new();
    if let Some(mut s) = stdout.take() {
        let _ = s.read_to_end(&mut buf);
    }
    Some(buf)
}

// ── continuous limit estimation (DEC-12, CCW's calibration math) ─────────────

/// A single tokens-per-percent sample from an observation pair. `None` when
/// the percent did not increase or no tokens were consumed — no signal, and
/// therefore no estimate (CON-5).
pub fn tokens_per_percent(tokens_delta: u64, pct_before: f64, pct_after: f64) -> Option<f64> {
    let dp = pct_after - pct_before;
    if dp <= 0.0 || tokens_delta == 0 {
        return None;
    }
    Some(tokens_delta as f64 / dp)
}

/// Merge a new sample into a running mean. Returns `(mean, sample_count)`.
pub fn merge_calibration(existing: Option<(f64, u64)>, new_sample: f64) -> (f64, u64) {
    match existing {
        Some((mean, n)) if n > 0 => {
            let nf = n as f64;
            ((mean * nf + new_sample) / (nf + 1.0), n + 1)
        }
        _ => (new_sample, 1),
    }
}

/// What 100% of an axis translates to in TOKENS, given its calibration.
///
/// `None` used to mean "uncalibrated ⇒ the axis reports `calibrating` and never
/// gates". That refusal is SUPERSEDED by the prior below: [`Denominator`]
/// always has a number, so callers on the enforcement path get a `Some` from
/// first boot. This helper survives for the raw-estimate readouts
/// (`estimation_state`, `gauge_state`) that still want to say "no measurement".
pub fn limit_tokens(tokens_per_percent: Option<f64>) -> Option<f64> {
    tokens_per_percent.map(|tpp| tpp * 100.0)
}

// ── the conservative PRIOR and the confidence blend (msgs #876/#1047/#1055) ──
//
// The operator cancelled the dedicated ~1% calibration run outright:
//
//   #1047 — "I thought you didn't need to run a calibration because we're going
//            to do the math before we ever test anything."
//   #1055 — "Either that or you can set a prior and that'll work too… we just
//            need to have it be the most simple startup possible."
//
// So NOTHING is ever spent on calibration. A percentage budget has to be usable
// on the very first turn, which means the denominator needs a number before any
// sample exists, and that number has to be wrong in a SAFE direction.
//
// ## Which direction is safe
//
// A percentage meter divides OUR OWN tokens by the estimate:
// `consumed_pct = tokens / tokens_per_percent`. A LOW estimate therefore reads
// a HIGH percent and the budget refuses EARLY. Under-estimating the pool is the
// conservative error; over-estimating it is the one that lets spend run away.
// This is the same rule [`model_rates`] already applies to an unrecognised
// model ("price it as the most expensive tier rather than as free").
//
// ## Where the number comes from — provenance
//
// Not from a published Anthropic table (there is none in token units; the
// published limits are approximate hours and messages, and they move). It comes
// from THIS deployment's own measurement, and it is a LOWER BOUND by
// construction.
//
// The live stress test of 2026-07-29 (harness workspace `aui-tweaks`,
// `live-stress-test-report.md` §4) recorded, on the `default` account:
//
//   * the session gauge moving 4% → 13% — a nine-point rise — over eleven
//     minutes, and
//   * 181,780 tokens on this daemon's OWN ledger across the same stretch.
//
// 181,780 ÷ 9 = 20,197.8 tokens per percent. That quotient can only UNDERSTATE
// the truth, and the reason it understates is exactly the report's headline
// finding: the operator's live AUI was burning the same account throughout, so
// other consumers moved the percent while contributing nothing to our token
// count. Divide our tokens by somebody else's percent and the answer comes out
// too small. Every such contamination pushes the same way, so the quotient is a
// floor on the real tokens-per-percent, which is precisely the conservative side.
//
// Rounded DOWN to [`PRIOR_TOKENS_PER_PERCENT`] = 20,000, because rounding down
// lowers the estimate, raises the reported percent and refuses earlier still.
//
// ## Why ONE constant covers all three percentage axes
//
// The window lengths nest: a five-hour session sits inside a seven-day week
// ([`SESSION_WINDOW_SECS`] < [`WEEK_WINDOW_SECS`]), so 100% of a weekly pool
// cannot be smaller than 100% of a session pool without a single session being
// able to exhaust a whole week. Hence `tpp_weekly >= tpp_session`, and the
// session floor is a valid — merely more cautious — floor for both weekly axes
// too. That inequality is the whole of the "plan limit shape" this needs; no
// published per-plan table is required, and there is nothing for the operator
// to tune.
//
// ## The ladder
//
// The constant is the FLOOR of a three-rung ladder, best evidence first:
//
//   1. this (account, axis)'s own accepted samples — the durable
//      `limit_estimates` row, which survives restarts and is what the blend
//      below shrinks towards;
//   2. failing that, the tightest measurement any OTHER account has produced on
//      the SAME axis (accounts usually share a plan, and "tightest" keeps the
//      conservative direction) — so a second account starts from the first
//      one's hard-won calibration rather than from the raw constant;
//   3. failing that, this constant.
//
// `budgets::Budgets::axis_prior` implements rungs 2 and 3; rung 1 is the blend.

/// The conservative tokens-per-percent floor. See the module note above for its
/// derivation — it is a measured LOWER BOUND, not a guess, and it errs by
/// refusing early.
pub const PRIOR_TOKENS_PER_PERCENT: f64 = 20_000.0;

/// The conservative dollars-per-token floor, used by the dual-currency adapter
/// until the account's own ledger can supply a realised rate.
///
/// `50.0 / 1e6` is [`model_rates`]'s unrecognised-model OUTPUT rate — the most
/// expensive token of the most expensive tier. Same rule as above, applied to
/// the other denomination: a HIGH dollars-per-token reads a HIGH dollar figure
/// out of a token count, so a dollar limit refuses early.
pub const PRIOR_USD_PER_TOKEN: f64 = 50.0 / 1_000_000.0;

/// How many accepted samples it takes to displace the prior completely.
///
/// The prior enters the blend as [`PRIOR_SAMPLES`] pseudo-samples and each real
/// sample buys out one of them, so the estimate is pure prior at zero samples,
/// pure measurement at four, and a strict interpolation in between. One
/// constant, no knobs, and it closes IS-50 — the audit's finding that the FIRST
/// accepted overlap-window sample used to arm the meter by itself. At one
/// sample it now carries a quarter of the weight.
pub const PRIOR_SAMPLES: f64 = 4.0;

/// The pseudo-count the prior still holds after `samples` real measurements.
pub fn prior_weight(samples: u64) -> f64 {
    (PRIOR_SAMPLES - samples as f64).max(0.0)
}

/// Shrink `prior` towards the measured mean as accepted samples accumulate.
///
/// The blend is a pseudo-count weighted mean taken on the RECIPROCAL — on
/// percent-per-token rather than tokens-per-percent — for two reasons. It is
/// the quantity the meter actually multiplies by (`consumed_pct = tokens ×
/// percent_per_token`), so shrinking it is the natural move; and a weighted
/// mean of reciprocals is a harmonic mean, which never exceeds the arithmetic
/// one, so the blend always lands on the more conservative side of the two.
///
/// ```text
///                  w·(1/prior) + n·(1/mean)                    w = max(4-n, 0)
///   1/estimate  =  ───────────────────────── ,
///                          w + n
/// ```
///
/// `n = 0` ⇒ exactly the prior. `n >= PRIOR_SAMPLES` ⇒ exactly the measured
/// mean, with no residual bias for a well-measured account to carry forever.
pub fn blend_rate(prior: f64, measured: Option<(f64, u64)>) -> f64 {
    let usable = |v: f64| v.is_finite() && v > 0.0;
    let prior = if usable(prior) {
        prior
    } else {
        PRIOR_TOKENS_PER_PERCENT
    };
    let Some((mean, n)) = measured.filter(|(m, n)| *n > 0 && usable(*m)) else {
        return prior;
    };
    let w = prior_weight(n);
    if w <= 0.0 {
        return mean;
    }
    let nf = n as f64;
    let inv = (w / prior + nf / mean) / (w + nf);
    if usable(inv) {
        1.0 / inv
    } else {
        prior
    }
}

// ── the dual-currency adapter (msg #876) ─────────────────────────────────────
//
//   #876 — "build adapters for BOTH dollars and tokens with long-term memory of
//           measurements so the code self-optimizes … don't wait for them,
//           build out the logic to handle the calibrating data points."
//
// One conversion surface, both directions, dollars ↔ tokens ↔ percent, carrying
// its own confidence — so a limit written in ANY denomination can be compared
// against a gauge reading in any other, always through the same estimator.
// TOKENS are the hub: percent and dollars each convert through them.

/// A denomination a limit or a reading can be written in. Distinct from
/// [`Unit`] (which is a property of an AXIS and has only the two the operator's
/// decisions name); this is a property of a VALUE, and tokens are the hub every
/// conversion passes through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Denom {
    Percent,
    Tokens,
    Dollars,
}

impl Denom {
    pub fn as_str(&self) -> &'static str {
        match self {
            Denom::Percent => "percent",
            Denom::Tokens => "tokens",
            Denom::Dollars => "dollars",
        }
    }

    /// The denomination an axis is natively measured in.
    pub fn of(unit: Unit) -> Denom {
        match unit {
            Unit::Percent => Denom::Percent,
            Unit::Dollars => Denom::Dollars,
        }
    }
}

/// The estimator for one (account, axis): what a percent costs in tokens, what
/// a token costs in dollars, and how much measurement is behind both.
///
/// Built by `budgets::Budgets::denominator`. Always has numbers — the prior
/// fills in whatever has not been measured yet — which is what makes percentage
/// budgets gate from first boot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Denominator {
    /// The BLENDED tokens-per-percent: prior shrunk towards this account's own
    /// accepted samples.
    pub tokens_per_percent: f64,
    /// Dollars per token, realised from the account's ledger where it has one
    /// and [`PRIOR_USD_PER_TOKEN`] until then.
    pub usd_per_token: f64,
    /// Accepted samples behind `tokens_per_percent`. Zero = pure prior.
    pub samples: u64,
}

impl Denominator {
    /// The pure-prior adapter, for an account with no measurement at all.
    pub fn prior() -> Self {
        Denominator {
            tokens_per_percent: PRIOR_TOKENS_PER_PERCENT,
            usd_per_token: PRIOR_USD_PER_TOKEN,
            samples: 0,
        }
    }

    /// Gating on the prior alone — no accepted sample yet. This is what the
    /// `calibrating` meter status means now: informative, never a refusal to
    /// gate.
    pub fn prior_only(&self) -> bool {
        self.samples == 0
    }

    /// How far the estimate has walked from the prior towards measurement,
    /// `0.0` (pure prior) to `1.0` (fully measured). The "same confidence"
    /// every denomination is converted with.
    pub fn confidence(&self) -> f64 {
        (self.samples as f64 / PRIOR_SAMPLES).clamp(0.0, 1.0)
    }

    /// What 100% of this axis costs in tokens.
    pub fn limit_tokens(&self) -> f64 {
        self.tokens_per_percent * 100.0
    }

    /// `value`, given in `from`, expressed in TOKENS — the hub every
    /// conversion passes through.
    fn tokens_of(&self, value: f64, from: Denom) -> f64 {
        match from {
            Denom::Tokens => value,
            Denom::Percent => value * self.tokens_per_percent,
            Denom::Dollars => value / self.usd_per_token,
        }
    }

    /// A token count expressed in `to` — the hub on the way back out.
    fn denominate(&self, tokens: f64, to: Denom) -> f64 {
        match to {
            Denom::Tokens => tokens,
            Denom::Percent => tokens / self.tokens_per_percent,
            Denom::Dollars => tokens * self.usd_per_token,
        }
    }

    /// Convert `value` from one denomination to another through this
    /// estimator. `None` when the estimator holds a degenerate rate (a zero or
    /// non-finite factor) — never a fabricated number.
    pub fn convert(&self, value: f64, from: Denom, to: Denom) -> Option<f64> {
        if !(self.tokens_per_percent.is_finite()
            && self.tokens_per_percent > 0.0
            && self.usd_per_token.is_finite()
            && self.usd_per_token > 0.0
            && value.is_finite())
        {
            return None;
        }
        if from == to {
            return Some(value);
        }
        let out = self.denominate(self.tokens_of(value, from), to);
        out.is_finite().then_some(out)
    }
}

// ── the overlap-window calibration method (operator msg #620) ────────────────
//
// The naive approach — pair every two CONSECUTIVE polls and attribute the
// whole intervening percent delta to our own tokens — is simply wrong on an
// account this daemon does not exclusively own: the operator's live AUI burns
// the same account, and the first real samples (2026-07-29) were off by
// roughly 2-50x for exactly that reason (see the live-stress-test report §4).
//
// The operator's fix (msg #620, verbatim): "If you can track when threads are
// overlapping ... then you can find the beginning of the first one in the
// overlap and the end of the last one in the overlap and look at the usage of
// the sum of all of them and compare that against your budget." Read as a
// method:
//
//   1. Track this daemon's own (start, end) execution intervals per account.
//   2. Merge overlapping/touching intervals into maximal OVERLAP WINDOWS.
//   3. Bracket a window with the `/usage` poll immediately before its start
//      and the one immediately after its end — both taken from the IDLE
//      stretches around it, so no other work of ours falls inside the bracket.
//      The percent delta between those two polls is attributable to the
//      window's summed tokens ONLY if nothing else moved the gauge — which is
//      exactly what step 4 checks.
//   4. Reject the window (never fabricate a sample) when: no poll brackets it
//      inside those idle stretches; an execution of ours is still in flight,
//      so the token side is incomplete; the bracket spans a rollover, or
//      cannot be shown to lie inside ONE window of the axis (msg #690); the
//      delta is at or below the gauge's granularity; or the percentage is seen
//      to move in the idle time immediately surrounding the window while
//      nothing of ours was running — the self-diagnosing signal that the
//      account is shared with a third party.

/// A maximal span of wall-clock time covered by one or more of this daemon's
/// OWN executions on one account, with no gap — step 2 of the method.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlapWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// Why a candidate window was refused as a calibration sample — step 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowRejection {
    /// No `/usage` poll was observed in the quiet stretch at/before the
    /// window's start, or none in the quiet stretch at/after its end — the
    /// window cannot be bracketed (may resolve later, as more polls land).
    ///
    /// "Quiet stretch" is what makes this stricter than it reads: a poll on
    /// the far side of ANOTHER of our windows may not bracket this one,
    /// because the delta it measures would include that other window's
    /// consumption while the token side counts only this window's.
    Unbracketed,
    /// An execution of ours was still IN FLIGHT across this window's bracket,
    /// so its tokens are not on the ledger yet: the token side of the pair is
    /// knowably incomplete. Temporary — it resolves when the turn lands.
    Unsettled,
    /// The bracket cannot be shown to lie inside ONE limit window (operator
    /// msg #690): either the polls report no reset instant at all — so a
    /// rollover between them would be invisible — or the span reaches outside
    /// the window their reset instant places. A window that straddles a
    /// session boundary measures two pools as one and must never calibrate.
    SessionBoundary,
    /// The bracketing polls straddle a window rollover (a reset, not a rise).
    Rollover,
    /// The percent did not rise, or rose by less than the gauge can
    /// represent — a sample needs the delta to clear the axis's own
    /// granularity (the `/usage` poll is whole percentage points).
    BelowGranularity,
    /// The percentage moved in the idle time immediately bracketing this
    /// window while NO execution of ours was running — proof the account is
    /// shared with a third party. This is the method's self-diagnosis.
    IdleMovement,
}

/// Parse an RFC3339 string to a UTC instant, or `None` for anything else
/// (never guessed — an unparseable timestamp simply cannot bracket a window).
pub fn parse_instant(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Merge (start, end) execution intervals into maximal OVERLAP WINDOWS — step
/// 2 of the method. Touching counts as overlapping: an end exactly equal to
/// (or past) the next start is still one continuous span, because nothing
/// idle happened between them for this account.
pub fn merge_windows(mut intervals: Vec<(DateTime<Utc>, DateTime<Utc>)>) -> Vec<OverlapWindow> {
    intervals.sort_by_key(|(s, _)| *s);
    let mut out: Vec<OverlapWindow> = Vec::new();
    for (s, e) in intervals {
        match out.last_mut() {
            Some(w) if s <= w.end => {
                if e > w.end {
                    w.end = e;
                }
            }
            _ => out.push(OverlapWindow { start: s, end: e }),
        }
    }
    out
}

/// How far apart two reported reset instants may be and still name the SAME
/// window — see `identical_prose_resolves_to_a_byte_identical_instant` and the
/// live 2026-07-29 jitter this guards against (sub-second drift on identical
/// prose, and claude's own rendering crossing minute boundaries). A genuine
/// rollover moves the reset by the window length (5 hours session, 7 days
/// weekly), four orders of magnitude past this tolerance.
const WINDOW_MATCH_TOLERANCE_SECS: i64 = 120;

/// Do two reported reset instants name the same window? Both absent = the
/// same (unknown) window; exactly one absent = not comparable, so refuse
/// rather than pair across a boundary we cannot see. Unparseable values fall
/// back to string equality.
pub fn same_window(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => match (
            DateTime::parse_from_rfc3339(x),
            DateTime::parse_from_rfc3339(y),
        ) {
            (Ok(x), Ok(y)) => (x.timestamp() - y.timestamp()).abs() <= WINDOW_MATCH_TOLERANCE_SECS,
            _ => x == y,
        },
        _ => false,
    }
}

/// The observation `source` a calibration bracket may be built from: the
/// zero-cost `/usage` poll, and nothing else.
///
/// The other two sources cannot honestly bracket a window, and both of them
/// COULD before this was enforced (each one is a wrong sample this rules out):
///
/// * `rate_limit_event` — a free push line that arrives from INSIDE one of our
///   own executions. By construction the account was not idle at that instant,
///   so such a reading can never witness a quiet bracket; worse, the push from
///   a LATER, still-running turn would be picked as the `after` poll of an
///   earlier window, folding that later turn's burn into the earlier window's
///   delta while the token side counted only the earlier window.
/// * `limit_hit` — a 429 CLIFF pinned at 100%, not a measured reading.
///   `observe()` already refuses to CALIBRATE from one, but the row stayed in
///   the bracket material, so the next poll could still pair a real reading
///   against that fabricated 100% and record a wildly wrong sample.
pub const POLL_SOURCE: &str = "usage_poll";

/// The `/usage` gauge's own resolution: whole percentage points. Used as the
/// tolerance when comparing a fractional in-turn reading against a poll.
pub const POLL_GRANULARITY_PCT: f64 = 1.0;

/// May this observation bracket a window (or witness an idle stretch quiet)?
/// See [`POLL_SOURCE`] for why only a `/usage` poll qualifies.
pub fn can_bracket(o: &ObservationRow) -> bool {
    o.source == POLL_SOURCE
}

/// Everything [`evaluate_window`] needs besides the candidate window itself.
pub struct WindowEvidence<'a> {
    /// The axis being calibrated — its window LENGTH is what places the limit
    /// window's start behind the reset instant the polls report (msg #690).
    pub axis: Axis,
    /// Every window this account's executions form in the span under
    /// consideration, this one included: the neighbours are what define the
    /// idle stretches around it.
    pub all_windows: &'a [OverlapWindow],
    /// Observations for this (account, axis), in any order. Only
    /// [`POLL_SOURCE`] rows are used.
    pub observations: &'a [ObservationRow],
    /// Tokens already charged inside `[window.start, window.end]` (the caller
    /// sums them from the ledger, filtered to the axis's model scope where
    /// applicable).
    pub tokens: u64,
    /// The earliest start of an execution of ours that is STILL RUNNING on
    /// this account — its spend is not on the ledger yet, so nothing at or
    /// after it can be trusted as a bracket. `None` when the account is idle.
    pub unsettled_since: Option<DateTime<Utc>>,
}

/// Does the whole measured span sit inside ONE limit window? The operator's
/// msg #690 constraint: "whatever session you're trying to estimate, all of
/// the items in that window were in that session ... from the beginning of the
/// session to the end of the session".
///
/// The reset instant a poll reports IS that window's end, so the window is
/// `[resets_at - axis.window_secs(), resets_at]`, and the span the percent
/// delta was measured over must lie inside it. Two consequences worth being
/// explicit about:
///
/// * an UNKNOWN reset instant fails: with no boundary in view, a rollover in
///   the middle of the span is invisible, and pairing across one attributes a
///   fresh window's rise to tokens spent in the previous one;
/// * the bound is taken from the STRICTER of the two polls (latest reported
///   start, earliest reported end), so a jittered pair narrows the accepted
///   span rather than widening it.
///
/// The start bound carries [`WINDOW_MATCH_TOLERANCE_SECS`] of slack because a
/// reset is reported only to the MINUTE: an execution that itself opened the
/// session starts within a minute of the placed boundary, and rounding must
/// not turn that into a rejection.
pub fn within_one_limit_window(
    axis: Axis,
    span_start: DateTime<Utc>,
    span_end: DateTime<Utc>,
    resets_before: Option<&str>,
    resets_after: Option<&str>,
) -> bool {
    let (Some(rb), Some(ra)) = (
        resets_before.and_then(parse_instant),
        resets_after.and_then(parse_instant),
    ) else {
        return false;
    };
    let end_bound = rb.min(ra);
    let start_bound = rb.max(ra)
        - chrono::Duration::seconds(axis.window_secs() + WINDOW_MATCH_TOLERANCE_SECS);
    span_start >= start_bound && span_end <= end_bound
}

/// Evaluate ONE candidate overlap window against the polls around it and every
/// known window for this account/axis — the pure heart of step 3/4.
///
/// The bracket is deliberately TIGHT: the `before` poll must lie in the idle
/// stretch immediately preceding the window (after any earlier window of ours
/// ended) and the `after` poll in the idle stretch immediately following it
/// (before any later window of ours began, in-flight ones included). A looser
/// bracket measures a percent delta that other work of ours also moved, while
/// the token side counts only this window — which records a sample that is
/// simply wrong rather than merely coarse.
pub fn evaluate_window(
    window: OverlapWindow,
    ev: WindowEvidence<'_>,
) -> Result<f64, WindowRejection> {
    // Only `/usage` polls may bracket or witness quiet — see `POLL_SOURCE`.
    // Sorted by instant so the caller's ordering is never load-bearing.
    let mut polls: Vec<(DateTime<Utc>, &ObservationRow)> = ev
        .observations
        .iter()
        .filter(|o| can_bracket(o))
        .filter_map(|o| parse_instant(&o.observed_at).map(|t| (t, o)))
        .collect();
    polls.sort_by_key(|(t, _)| *t);

    // The idle stretches immediately around this window: from the end of the
    // PREVIOUS window (or the beginning of time) up to this window's start,
    // and from this window's end up to the start of the NEXT window (or the
    // end of time) — exactly the boundaries rule 3 asks to be "quiet".
    let prev_end = ev
        .all_windows
        .iter()
        .filter(|w| w.end <= window.start)
        .map(|w| w.end)
        .max();
    let mut next_start = ev
        .all_windows
        .iter()
        .filter(|w| w.start >= window.end)
        .map(|w| w.start)
        .min();

    // An execution still IN FLIGHT has written no ledger row, so it forms no
    // window yet — but it is spending. If it began before this window closed,
    // the token side of this pair is incomplete and nothing can be salvaged
    // until it lands; if it began after, it bounds the trailing idle stretch
    // exactly as a completed window would.
    if let Some(u) = ev.unsettled_since {
        if u < window.end {
            return Err(WindowRejection::Unsettled);
        }
        next_start = Some(next_start.map_or(u, |n| n.min(u)));
    }

    // Bracket: the LAST poll in the leading idle stretch, and the FIRST poll
    // in the trailing one.
    let mut before: Option<&ObservationRow> = None;
    let mut after: Option<&ObservationRow> = None;
    for (t, o) in &polls {
        if *t <= window.start && prev_end.is_none_or(|pe| *t >= pe) {
            before = Some(o);
        }
        if after.is_none() && *t >= window.end && next_start.is_none_or(|ns| *t <= ns) {
            after = Some(o);
        }
    }
    let (Some(before), Some(after)) = (before, after) else {
        return Err(WindowRejection::Unbracketed);
    };
    if before.id == after.id {
        // A zero-length bracket can only happen for a degenerate window;
        // there is nothing to attribute either way.
        return Err(WindowRejection::Unbracketed);
    }

    // A rollover invalidates the pair outright: the percent RESET, it did not
    // move in response to consumption.
    if !same_window(before.resets_at.as_deref(), after.resets_at.as_deref()) {
        return Err(WindowRejection::Rollover);
    }

    // msg #690: everything measured has to have happened inside ONE window of
    // the axis being estimated — no straddling a session boundary.
    let (Some(bt), Some(at)) = (
        parse_instant(&before.observed_at),
        parse_instant(&after.observed_at),
    ) else {
        return Err(WindowRejection::Unbracketed);
    };
    if !within_one_limit_window(
        ev.axis,
        bt,
        at,
        before.resets_at.as_deref(),
        after.resets_at.as_deref(),
    ) {
        return Err(WindowRejection::SessionBoundary);
    }

    // A reading INSIDE the bracket that sits meaningfully HIGHER than the
    // closing one means the meter fell somewhere in between: the pool reset
    // inside the bracket, however much the two bracket polls' reset instants
    // agree. A 429 cliff at 100% is the loud case — it cannot bracket, but it
    // is still evidence, and pairing across it would attribute a fresh pool's
    // rise to tokens spent in the previous one. The tolerance is one whole
    // percentage point because a poll reports whole points while an in-turn
    // push reports a fraction, so a push may legitimately read slightly higher
    // than the poll that follows it.
    for o in ev.observations {
        let Some(t) = parse_instant(&o.observed_at) else {
            continue;
        };
        if t > bt && t < at && o.pct > after.pct + POLL_GRANULARITY_PCT {
            return Err(WindowRejection::Rollover);
        }
    }

    // The self-diagnosis: did the percentage move in either idle stretch while
    // nothing of ours was running?
    for pair in polls.windows(2) {
        let ((ta, a), (tb, b)) = (pair[0], pair[1]);
        let in_before_gap = tb <= window.start && prev_end.is_none_or(|pe| ta >= pe);
        let in_after_gap = ta >= window.end && next_start.is_none_or(|ns| tb <= ns);
        if !(in_before_gap || in_after_gap) {
            continue;
        }
        // A rollover inside the idle gap is a window boundary, not evidence
        // of third-party consumption — the guard above already refuses to
        // pair across it, so skip it here too.
        if !same_window(a.resets_at.as_deref(), b.resets_at.as_deref()) {
            continue;
        }
        if (a.pct - b.pct).abs() > f64::EPSILON {
            return Err(WindowRejection::IdleMovement);
        }
    }

    tokens_per_percent(ev.tokens, before.pct, after.pct)
        .ok_or(WindowRejection::BelowGranularity)
}

/// Convert an epoch-seconds `resetsAt` (the `rate_limit_event` shape) to
/// RFC3339 UTC — the form every other reset in this crate uses.
pub fn epoch_to_rfc3339(epoch: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(epoch, 0).map(|t| t.to_rfc3339())
}

/// Resolve reset PROSE (the `/usage` and 429 shape) to RFC3339 UTC.
///
/// **This function must be deterministic for identical prose** — the whole
/// calibration path depends on it. `Budgets::calibrate_overlap_window` uses
/// `resets_at` equality as its window-rollover guard, so a value that drifts
/// between two polls of the SAME window makes every consecutive pair look like
/// a rollover, and NO sample is ever recorded. That is the root cause of the
/// decorative-budgets dragon: observed live on 2026-07-29, the session axis
/// resolved to `…T23:50:00.000262+00:00`, then `.000116`, then `.000039` from
/// byte-identical prose.
///
/// Two bugs produced that drift, and both are fixed here:
///
/// 1. `Local::now()` and `Utc::now()` were called separately, microseconds
///    apart, so `Utc::now() + (at - Local::now())` carried the difference
///    between the two calls. ONE instant is now sampled and converted.
/// 2. Even anchored, the result inherits `now`'s sub-second component. The
///    resolved instant is therefore TRUNCATED TO THE SECOND — a reset is
///    reported to the minute upstream, so sub-second precision is noise that
///    only ever breaks equality.
pub fn prose_to_rfc3339(prose: &str) -> Option<String> {
    use chrono::Timelike;
    let r = parse_reset(prose)?;
    // ONE instant, used for both the local anchor and the UTC base.
    let now = crate::clock::now_local();
    let now_local = now.naive_local();
    let at = r.resolve(now_local)?;
    let delta = at - now_local;
    let resolved = now.with_timezone(&chrono::Utc) + delta;
    // Truncate to the second: identical prose ⇒ byte-identical output.
    Some(resolved.with_nanosecond(0)?.to_rfc3339())
}

// ── per-model pricing (the mid-turn CASH estimate) ───────────────────────────

/// Cache reads bill at this multiple of the model's input rate.
const CACHE_READ_MULTIPLIER: f64 = 0.1;
/// Cache WRITES bill at this multiple of the input rate.
///
/// 2.0 is the one-hour-TTL rate, and it is what Claude Code actually uses —
/// derived from a live haiku-4.5 turn on 2026-07-29 rather than assumed. That
/// turn reported 9 input, 43 output, 18,664 cache-read and 3,687 cache-write
/// tokens against an authoritative `costUSD` of $0.0094644; solving for the
/// write rate gives exactly 2.0×. The 5-minute-TTL rate (1.25×) would have
/// under-estimated that turn by about 30%.
const CACHE_WRITE_MULTIPLIER: f64 = 2.0;

/// Per-million-token rates for one model family: `(input, output)` in dollars.
///
/// **These rates price an ESTIMATE, never the record.** `MessageUsage` carries
/// token counts but no cost, so without this table the cash meter cannot move
/// until `result_turn` — which is exactly why a cash budget could never trip
/// MID-turn (observed 2026-07-29: the kill path was unreachable). The
/// authoritative `costUSD` on `result_turn` reconciles the estimate away at
/// turn end, so a drifting rate here can only mis-TIME a trip, never mis-state
/// the ledger. Published rates change; that reconciliation is what keeps this
/// table from ever becoming a correctness dependency.
fn model_rates(model: &str) -> (f64, f64) {
    let m = model.to_ascii_lowercase();
    // Most specific first — "opus" must not swallow a hypothetical
    // "fable-opus" and `is_fable_model` owns the fable definition.
    if is_fable_model(&m) || m.contains("mythos") {
        return (10.0, 50.0);
    }
    if m.contains("opus") {
        return (5.0, 25.0);
    }
    if m.contains("haiku") {
        return (1.0, 5.0);
    }
    if m.contains("sonnet") {
        return (3.0, 15.0);
    }
    // An unrecognized model: price it as the most expensive tier rather than
    // as free. Under-charging a budget is the failure that lets spend run
    // away; over-charging only trips early, and reconciliation corrects it.
    (10.0, 50.0)
}

/// Estimate the dollar cost of one assistant message from the token counts
/// `MessageUsage` carries. See [`model_rates`] for why this is an estimate and
/// what reconciles it.
pub fn estimate_cost_usd(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
) -> f64 {
    let (input, output) = model_rates(model);
    let per_token = |rate: f64, n: u64| rate * n as f64 / 1_000_000.0;
    per_token(input, input_tokens)
        + per_token(output, output_tokens)
        + per_token(input * CACHE_READ_MULTIPLIER, cache_read_tokens)
        + per_token(input * CACHE_WRITE_MULTIPLIER, cache_creation_tokens)
}

// ── the SETTLE profile (edge polling, account-selection-design §3.2) ─────────
//
// How long after an execution lands does the `/usage` meter finish moving? The
// verification pass named this as the residual it would fix next and refused to
// guess at it, and the guess has a DANGEROUS direction: a closing poll taken too
// early reads a low delta, a low delta divides into a high tokens-per-percent,
// and a high tokens-per-percent means the budget fails to trip.
//
// So nothing here hard-codes a settle delay. Edge polling walks a LADDER of
// offsets after an account goes idle, READING the meter at each rung without
// recording anything, until two consecutive rungs agree. The profile below reads
// those rungs back and names the offset where the reading stopped moving. Only
// then is the closing observation PAIR recorded, from a reading that is settled
// by construction.
//
// The unrecorded phase is what makes the unmeasured constant harmless. A rung is
// a measurement, not evidence: calibration is evaluated as each observation
// lands, so recording an early rung would write the under-estimated delta before
// the later rungs could contradict it. And if the ladder runs out while the meter
// is still moving, nothing is recorded at all — the window stays unbracketed,
// which costs one free poll, where a wrong sample costs a budget that never
// trips.
//
// `evaluate_window`'s `IdleMovement` rule backs this up rather than carrying it:
// if third-party spend moves the meter between the two recorded closing polls,
// the sample is refused there too.

/// One rung of a settle ladder: milliseconds after the account went idle, and
/// the whole-percent reading the poll at that offset reported.
///
/// Milliseconds rather than seconds only so a test can walk a ladder in a
/// fraction of a second; the production ladder is in whole seconds and
/// [`SettleProfile::settled_after_secs`] is what a report quotes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SettleReading {
    pub offset_ms: u64,
    pub pct: f64,
}

/// What a settle ladder showed.
#[derive(Debug, Clone, PartialEq)]
pub struct SettleProfile {
    /// The earliest ladder offset from which the reading never changed again —
    /// the measured settle delay, in milliseconds. `None` when the ladder cannot
    /// say: fewer than two rungs, or the reading was still moving at the last
    /// one.
    pub settled_after_ms: Option<u64>,
    /// True when the last two rungs disagree: the meter had not finished moving
    /// when the ladder ran out, so the ladder is too short.
    pub still_moving: bool,
    /// Total movement across the ladder, in percentage points. Movement here is
    /// either settle lag (ours) or third-party spend (not ours) — the ladder
    /// cannot tell them apart on its own, which is why it waits for the reading
    /// to stop moving rather than trying to correct it.
    pub moved_pct: f64,
    /// The rungs actually observed, sorted by offset.
    pub rungs: Vec<SettleReading>,
}

impl SettleProfile {
    /// Did the ladder produce a usable answer?
    pub fn determinate(&self) -> bool {
        self.settled_after_ms.is_some()
    }

    /// The measured settle delay in SECONDS — the form a report quotes.
    pub fn settled_after_secs(&self) -> Option<f64> {
        self.settled_after_ms.map(|ms| ms as f64 / 1000.0)
    }

    /// A one-line log/report form. Hash-free, value-free, just the numbers.
    pub fn describe(&self) -> String {
        let rungs: Vec<String> = self
            .rungs
            .iter()
            .map(|r| format!("+{:.1}s={}%", r.offset_ms as f64 / 1000.0, r.pct))
            .collect();
        match self.settled_after_secs() {
            Some(s) => format!(
                "settled by +{s:.1}s after {:.0} point(s) of movement [{}]",
                self.moved_pct,
                rungs.join(" ")
            ),
            None if self.still_moving => format!(
                "STILL MOVING at the last rung ({:.0} point(s) so far) — ladder too short [{}]",
                self.moved_pct,
                rungs.join(" ")
            ),
            None => format!("indeterminate, {} rung(s) [{}]", self.rungs.len(), rungs.join(" ")),
        }
    }
}

/// Read a settle ladder: where did the meter stop moving?
///
/// The rungs may arrive in any order. Equality is exact because a `/usage` poll
/// reports whole percentage points ([`POLL_GRANULARITY_PCT`]) — there is no
/// float slop to absorb, and treating a 1-point move as "settled" would defeat
/// the whole measurement.
pub fn settle_profile(readings: &[SettleReading]) -> SettleProfile {
    let mut rungs: Vec<SettleReading> = readings.to_vec();
    rungs.sort_by_key(|r| r.offset_ms);
    let moved_pct: f64 = rungs
        .windows(2)
        .map(|w| (w[1].pct - w[0].pct).abs())
        .sum();
    if rungs.len() < 2 {
        return SettleProfile {
            settled_after_ms: None,
            still_moving: false,
            moved_pct,
            rungs,
        };
    }
    let last = rungs[rungs.len() - 1];
    let prev = rungs[rungs.len() - 2];
    if (last.pct - prev.pct).abs() > f64::EPSILON {
        return SettleProfile {
            settled_after_ms: None,
            still_moving: true,
            moved_pct,
            rungs,
        };
    }
    // The earliest rung whose reading equals every later reading.
    let settled = rungs
        .iter()
        .position(|r| (r.pct - last.pct).abs() <= f64::EPSILON)
        .map(|i| rungs[i].offset_ms);
    SettleProfile {
        settled_after_ms: settled,
        still_moving: false,
        moved_pct,
        rungs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_prose_parses_all_three_axes() {
        let prose = "Current session: 99% used · resets Jul 25 at 1:59am (America/Chicago)\n\
                     Current week (all models): 73% used · resets Jul 25 at 12:59pm (America/Chicago)\n\
                     Current week (Fable): 50% used · resets Jul 25 at 1pm (America/Chicago)\n";
        let pools = parse_usage_prose(prose);
        assert_eq!(pools.len(), 3);
        assert_eq!(pools[0].axis(), Some(Axis::Session));
        assert_eq!(pools[0].pct, 99.0);
        assert_eq!(pools[1].axis(), Some(Axis::WeeklyAll));
        assert_eq!(pools[2].axis(), Some(Axis::WeeklyFable));
        assert!(pools[2].reset.is_some());
    }

    #[test]
    fn usage_json_envelope_parses() {
        let raw = r#"{"type":"result","subtype":"success","is_error":false,"num_turns":0,
            "result":"Current session: 12% used · resets 2:40am (America/Chicago)",
            "session_id":"abc"}"#;
        let snap = parse_usage_json(raw).unwrap();
        assert_eq!(snap.pools.len(), 1);
        assert_eq!(snap.pools[0].pct, 12.0);
        assert_eq!(snap.session_id.as_deref(), Some("abc"));
    }

    #[test]
    fn rate_limit_type_mapping_matches_observed_burn_in() {
        // The ONLY value observed in the 2026-07-27 burn-in.
        assert_eq!(Axis::from_rate_limit_type("seven_day"), Some(Axis::WeeklyAll));
        assert_eq!(Axis::from_rate_limit_type("five_hour"), Some(Axis::Session));
        // Unknown types stay unmapped rather than being guessed into an axis.
        assert_eq!(Axis::from_rate_limit_type("mystery_pool"), None);
    }

    #[test]
    fn limit_pool_classification_fixes_pool_none() {
        assert_eq!(
            classify_limit_pool("You've hit your session limit · resets 2:40am"),
            Some(Axis::Session)
        );
        assert_eq!(
            classify_limit_pool("Weekly usage limit reached"),
            Some(Axis::WeeklyAll)
        );
        assert_eq!(
            classify_limit_pool("You've reached your Fable weekly limit"),
            Some(Axis::WeeklyFable)
        );
        assert_eq!(classify_limit_pool("something went wrong"), None);
    }

    #[test]
    fn calibration_never_guesses_without_a_sample() {
        // No percent movement, or no tokens ⇒ no sample (CCW INTENT #202).
        assert_eq!(tokens_per_percent(1000, 50.0, 50.0), None);
        assert_eq!(tokens_per_percent(0, 50.0, 60.0), None);
        // A real pair yields a real ratio.
        let s = tokens_per_percent(1_000_000, 50.0, 60.0).unwrap();
        assert!((s - 100_000.0).abs() < 1e-6);
        // …which becomes a 100%-of-axis token limit.
        assert_eq!(limit_tokens(None), None);
        assert!((limit_tokens(Some(s)).unwrap() - 10_000_000.0).abs() < 1e-6);
    }

    #[test]
    fn calibration_merges_as_a_running_mean() {
        let (m, n) = merge_calibration(None, 100.0);
        assert_eq!((m, n), (100.0, 1));
        let (m, n) = merge_calibration(Some((m, n)), 200.0);
        assert_eq!((m, n), (150.0, 2));
        let (m, n) = merge_calibration(Some((m, n)), 300.0);
        assert_eq!((m, n), (200.0, 3));
    }

    // ── the overlap-window method (operator msg #620) ────────────────────

    fn t(s: &str) -> DateTime<Utc> {
        parse_instant(s).unwrap()
    }

    fn obs(id: i64, pct: f64, resets_at: Option<&str>, observed_at: &str) -> ObservationRow {
        ObservationRow {
            id,
            account: "a".into(),
            axis: "session".into(),
            pct,
            resets_at: resets_at.map(str::to_string),
            source: POLL_SOURCE.into(),
            observed_at: observed_at.into(),
        }
    }

    /// The same row from a source that may NOT bracket (see `POLL_SOURCE`).
    fn obs_from(
        id: i64,
        pct: f64,
        resets_at: Option<&str>,
        observed_at: &str,
        source: &str,
    ) -> ObservationRow {
        ObservationRow {
            source: source.into(),
            ..obs(id, pct, resets_at, observed_at)
        }
    }

    /// Every fixture below runs on the SESSION axis, whose window is 5 hours;
    /// a reset instant a few hours out is what a real poll reports, and the
    /// msg-#690 containment rule needs one that is consistent with the axis.
    const RESET: &str = "2030-01-01T04:00:00Z";

    fn ev<'a>(
        all_windows: &'a [OverlapWindow],
        observations: &'a [ObservationRow],
        tokens: u64,
    ) -> WindowEvidence<'a> {
        WindowEvidence {
            axis: Axis::Session,
            all_windows,
            observations,
            tokens,
            unsettled_since: None,
        }
    }

    #[test]
    fn merge_windows_joins_touching_and_overlapping_but_not_disjoint_spans() {
        let ws = merge_windows(vec![
            (t("2030-01-01T00:00:00Z"), t("2030-01-01T00:01:00Z")),
            // Overlaps the first.
            (t("2030-01-01T00:00:30Z"), t("2030-01-01T00:02:00Z")),
            // Touches the merged span exactly at its end.
            (t("2030-01-01T00:02:00Z"), t("2030-01-01T00:03:00Z")),
            // Disjoint — a real gap follows.
            (t("2030-01-01T01:00:00Z"), t("2030-01-01T01:00:10Z")),
        ]);
        assert_eq!(ws.len(), 2);
        assert_eq!(ws[0].start, t("2030-01-01T00:00:00Z"));
        assert_eq!(ws[0].end, t("2030-01-01T00:03:00Z"));
        assert_eq!(ws[1].start, t("2030-01-01T01:00:00Z"));
        assert_eq!(ws[1].end, t("2030-01-01T01:00:10Z"));
    }

    #[test]
    fn overlap_window_produces_a_sample_when_bracketed_and_quiet() {
        let w = OverlapWindow {
            start: t("2030-01-01T00:00:30Z"),
            end: t("2030-01-01T00:00:45Z"),
        };
        let observations = vec![
            obs(1, 4.0, Some(RESET), "2030-01-01T00:00:00Z"),
            obs(2, 9.0, Some(RESET), "2030-01-01T00:01:00Z"),
        ];
        // 500_000 tokens across a 5-point rise = 100_000 tokens per percent.
        let sample = evaluate_window(w, ev(&[w], &observations, 500_000)).unwrap();
        assert!((sample - 100_000.0).abs() < 1.0);
    }

    #[test]
    fn overlap_window_rejects_a_rollover_straddling_bracket() {
        let w = OverlapWindow {
            start: t("2030-01-01T00:00:30Z"),
            end: t("2030-01-01T00:00:45Z"),
        };
        let observations = vec![
            obs(1, 90.0, Some("2030-01-01T00:00:00Z"), "2030-01-01T00:00:00Z"),
            // Reset instant jumped 5 hours — a real rollover, not jitter.
            obs(2, 5.0, Some("2030-01-01T05:00:00Z"), "2030-01-01T00:01:00Z"),
        ];
        assert_eq!(
            evaluate_window(w, ev(&[w], &observations, 500_000)),
            Err(WindowRejection::Rollover)
        );
    }

    #[test]
    fn overlap_window_rejects_a_delta_below_granularity() {
        let w = OverlapWindow {
            start: t("2030-01-01T00:00:30Z"),
            end: t("2030-01-01T00:00:45Z"),
        };
        let observations = vec![
            obs(1, 4.0, Some(RESET), "2030-01-01T00:00:00Z"),
            // A whole-percent gauge that did not move — cent-scale spend that
            // never clears the 1% granularity floor.
            obs(2, 4.0, Some(RESET), "2030-01-01T00:01:00Z"),
        ];
        assert_eq!(
            evaluate_window(w, ev(&[w], &observations, 500)),
            Err(WindowRejection::BelowGranularity)
        );
    }

    #[test]
    fn overlap_window_is_unbracketed_without_a_poll_on_both_sides() {
        let w = OverlapWindow {
            start: t("2030-01-01T00:00:30Z"),
            end: t("2030-01-01T00:00:45Z"),
        };
        // Only a poll BEFORE the window exists — nothing brackets its end yet.
        let observations = vec![obs(1, 4.0, Some(RESET), "2030-01-01T00:00:00Z")];
        assert_eq!(
            evaluate_window(w, ev(&[w], &observations, 500_000)),
            Err(WindowRejection::Unbracketed)
        );
    }

    /// THE self-diagnosis rule (step 4): a window bracketed by two clean polls
    /// still must not calibrate if the percentage is seen moving in the idle
    /// gap right next to it while nothing of ours was running — that movement
    /// is proof a third party (e.g. the operator's own AUI) shares the
    /// account, exactly the 2026-07-29 defect this method exists to catch.
    #[test]
    fn overlap_window_rejects_idle_movement_as_proof_of_sharing() {
        let w = OverlapWindow {
            start: t("2030-01-01T00:02:00Z"),
            end: t("2030-01-01T00:02:15Z"),
        };
        let observations = vec![
            // An earlier idle poll…
            obs(1, 4.0, Some(RESET), "2030-01-01T00:00:00Z"),
            // …and a later one, taken before this window even starts, that
            // already shows the gauge moved with nothing of ours running.
            obs(2, 6.0, Some(RESET), "2030-01-01T00:01:00Z"),
            // The window's own bracket looks perfectly clean in isolation.
            obs(3, 6.0, Some(RESET), "2030-01-01T00:01:55Z"),
            obs(4, 11.0, Some(RESET), "2030-01-01T00:02:20Z"),
        ];
        assert_eq!(
            evaluate_window(w, ev(&[w], &observations, 500_000)),
            Err(WindowRejection::IdleMovement)
        );
    }

    /// Two separate windows on the SAME account: the gap between them must be
    /// quiet for either to calibrate, but a poll pair fully INSIDE a window is
    /// exempt from the idle check — that movement is exactly what the running
    /// execution is expected to cause.
    #[test]
    fn overlap_windows_do_not_flag_movement_that_happens_during_execution() {
        let w1 = OverlapWindow {
            start: t("2030-01-01T00:00:00Z"),
            end: t("2030-01-01T00:00:10Z"),
        };
        let w2 = OverlapWindow {
            start: t("2030-01-01T00:05:00Z"),
            end: t("2030-01-01T00:05:10Z"),
        };
        let observations = vec![
            obs(1, 4.0, Some(RESET), "2030-01-01T00:00:00Z"),
            obs(2, 6.0, Some(RESET), "2030-01-01T00:00:10Z"),
            // Quiet between the windows: no movement.
            obs(3, 6.0, Some(RESET), "2030-01-01T00:04:00Z"),
            obs(4, 8.0, Some(RESET), "2030-01-01T00:05:10Z"),
        ];
        let sample = evaluate_window(w2, ev(&[w1, w2], &observations, 200_000)).unwrap();
        assert!((sample - 100_000.0).abs() < 1.0);
    }

    // ── the wrong samples the bracket used to let through ─────────────────
    //
    // Every test in this block is a case where the method recorded a sample
    // that was WRONG, not merely coarse. A wrong sample is worse than none: it
    // gates confidently at the wrong number, where an uncalibrated axis gates
    // nothing at all (CON-5).

    /// A bracket may not reach across ANOTHER of our own windows. Two
    /// executions minutes apart with only one poll before the first and one
    /// after the second: the newest window's token sum is its own, but the
    /// delta those two polls measure is BOTH executions' consumption. Pairing
    /// them attributed one window's tokens to two windows' percent.
    #[test]
    fn a_bracket_reaching_over_an_earlier_window_of_ours_is_refused() {
        let w1 = OverlapWindow {
            start: t("2030-01-01T00:01:00Z"),
            end: t("2030-01-01T00:01:30Z"),
        };
        let w2 = OverlapWindow {
            start: t("2030-01-01T00:03:00Z"),
            end: t("2030-01-01T00:03:30Z"),
        };
        // The only polls: one before w1, one after w2. Nothing witnesses the
        // gap between them, and the 5-point rise covers BOTH executions.
        let observations = vec![
            obs(1, 4.0, Some(RESET), "2030-01-01T00:00:00Z"),
            obs(2, 9.0, Some(RESET), "2030-01-01T00:04:00Z"),
        ];
        // w2's own tokens alone (100_000) against the pair's 5 points would
        // record 20_000/percent — five times too small, because 400_000 more
        // tokens of ours moved the same 5 points from inside w1.
        assert_eq!(
            evaluate_window(w2, ev(&[w1, w2], &observations, 100_000)),
            Err(WindowRejection::Unbracketed)
        );
        // The leading window is refused for the mirror-image reason: the only
        // poll after it sits on the far side of w2.
        assert_eq!(
            evaluate_window(w1, ev(&[w1, w2], &observations, 400_000)),
            Err(WindowRejection::Unbracketed)
        );
    }

    /// An execution still IN FLIGHT has spent tokens the ledger does not have
    /// yet, so a poll taken during it cannot close a bracket — not for the
    /// window it is running past, and not for itself.
    #[test]
    fn a_poll_taken_while_another_execution_is_in_flight_cannot_bracket() {
        let w = OverlapWindow {
            start: t("2030-01-01T00:01:00Z"),
            end: t("2030-01-01T00:01:30Z"),
        };
        let observations = vec![
            obs(1, 4.0, Some(RESET), "2030-01-01T00:00:00Z"),
            // The background sweep polls at 00:03 — while a second execution
            // of ours, started at 00:02, is still running. Its burn is inside
            // this reading but not on the ledger.
            obs(2, 9.0, Some(RESET), "2030-01-01T00:03:00Z"),
        ];
        let windows = [w];
        let mut e = ev(&windows, &observations, 100_000);
        e.unsettled_since = Some(t("2030-01-01T00:02:00Z"));
        assert_eq!(
            evaluate_window(w, e),
            Err(WindowRejection::Unbracketed),
            "a poll taken after an unrecorded execution began may not close the bracket"
        );

        // And when the in-flight turn began BEFORE this window closed, the
        // token side of the window itself is incomplete: nothing to salvage.
        let mut e = ev(&windows, &observations, 100_000);
        e.unsettled_since = Some(t("2030-01-01T00:01:20Z"));
        assert_eq!(evaluate_window(w, e), Err(WindowRejection::Unsettled));
    }

    /// A 429 is a CLIFF pinned at 100%, not a measurement. `observe()` already
    /// refuses to calibrate FROM one, but the row stayed in the bracket
    /// material — so the next poll could pair a real reading against that
    /// fabricated 100% and record a sample off by any factor at all.
    #[test]
    fn a_429_cliff_may_not_close_a_bracket() {
        let w = OverlapWindow {
            start: t("2030-01-01T00:01:00Z"),
            end: t("2030-01-01T00:01:30Z"),
        };
        let observations = vec![
            obs(1, 40.0, Some(RESET), "2030-01-01T00:00:00Z"),
            // The refusal, recorded as an observation at 100%.
            obs_from(2, 100.0, Some(RESET), "2030-01-01T00:02:00Z", "limit_hit"),
        ];
        assert_eq!(
            evaluate_window(w, ev(&[w], &observations, 500_000)),
            Err(WindowRejection::Unbracketed),
            "pairing 40% against a 429's 100% would record 8_333 tokens/percent"
        );

        // A real poll after the cliff does NOT rescue the pair either: the
        // cliff is proof the pool passed 100% and came back inside the bracket
        // — a rollover, whatever reset instant the two polls report.
        let mut with_poll = observations.clone();
        with_poll.push(obs(3, 45.0, Some(RESET), "2030-01-01T00:02:30Z"));
        assert_eq!(
            evaluate_window(w, ev(&[w], &with_poll, 500_000)),
            Err(WindowRejection::Rollover)
        );

        // A cliff OUTSIDE the bracket blocks nothing: this window's own pair
        // is intact, and the 429 belongs to an earlier stretch of history.
        let earlier_cliff = vec![
            obs_from(1, 100.0, Some(RESET), "2029-12-31T23:59:00Z", "limit_hit"),
            obs(2, 4.0, Some(RESET), "2030-01-01T00:00:00Z"),
            obs(3, 9.0, Some(RESET), "2030-01-01T00:02:00Z"),
        ];
        let sample = evaluate_window(w, ev(&[w], &earlier_cliff, 500_000)).unwrap();
        assert!((sample - 100_000.0).abs() < 1.0);
    }

    /// The free `rate_limit_event` push arrives from INSIDE one of our own
    /// executions, so it can never witness an idle bracket: the account was
    /// provably busy at that instant. Using one as the `after` poll folded a
    /// later turn's burn into an earlier window's delta.
    #[test]
    fn an_in_turn_push_reading_may_not_close_a_bracket() {
        let w = OverlapWindow {
            start: t("2030-01-01T00:01:00Z"),
            end: t("2030-01-01T00:01:30Z"),
        };
        let observations = vec![
            obs(1, 4.0, Some(RESET), "2030-01-01T00:00:00Z"),
            obs_from(
                2,
                9.0,
                Some(RESET),
                "2030-01-01T00:02:00Z",
                "rate_limit_event",
            ),
        ];
        assert_eq!(
            evaluate_window(w, ev(&[w], &observations, 100_000)),
            Err(WindowRejection::Unbracketed)
        );
    }

    /// THE msg-#690 constraint: every execution in the window must fall inside
    /// ONE session window, from that session's beginning to its end.
    #[test]
    fn a_window_that_cannot_be_placed_inside_one_session_never_calibrates() {
        let w = OverlapWindow {
            start: t("2030-01-01T00:00:30Z"),
            end: t("2030-01-01T00:00:45Z"),
        };
        // Polls that report NO reset instant place no session boundary at all,
        // so a rollover between them is invisible: the rise from 4% to 9%
        // could be 5 points of one session or a fresh session's first 9.
        let blind = vec![
            obs(1, 4.0, None, "2030-01-01T00:00:00Z"),
            obs(2, 9.0, None, "2030-01-01T00:01:00Z"),
        ];
        assert_eq!(
            evaluate_window(w, ev(&[w], &blind, 500_000)),
            Err(WindowRejection::SessionBoundary)
        );

        // A window that reaches back BEFORE the session the polls report (here
        // the reset places the session start at 00:00 on Jan 2) is refused for
        // the same reason: part of its spend was charged to the previous pool.
        let straddle = vec![
            obs(1, 4.0, Some("2030-01-02T05:00:00Z"), "2030-01-01T00:00:00Z"),
            obs(2, 9.0, Some("2030-01-02T05:00:00Z"), "2030-01-01T00:01:00Z"),
        ];
        assert_eq!(
            evaluate_window(w, ev(&[w], &straddle, 500_000)),
            Err(WindowRejection::SessionBoundary)
        );

        // The predicate itself, on both axes it has to serve: the same
        // reasoning holds for the weekly pools, only the length differs.
        assert!(within_one_limit_window(
            Axis::Session,
            t("2030-01-01T00:00:00Z"),
            t("2030-01-01T01:00:00Z"),
            Some(RESET),
            Some(RESET),
        ));
        assert!(!within_one_limit_window(
            Axis::Session,
            // Six hours before a 5-hour window's end is outside it.
            t("2029-12-31T22:00:00Z"),
            t("2030-01-01T01:00:00Z"),
            Some(RESET),
            Some(RESET),
        ));
        assert!(within_one_limit_window(
            Axis::WeeklyAll,
            t("2029-12-31T22:00:00Z"),
            t("2030-01-01T01:00:00Z"),
            Some(RESET),
            Some(RESET),
        ));
        assert!(!within_one_limit_window(
            Axis::WeeklyAll,
            t("2029-12-20T22:00:00Z"),
            t("2030-01-01T01:00:00Z"),
            Some(RESET),
            Some(RESET),
        ));
        // A span that runs PAST the reported reset straddles the boundary from
        // the other side.
        assert!(!within_one_limit_window(
            Axis::Session,
            t("2030-01-01T03:00:00Z"),
            t("2030-01-01T04:30:00Z"),
            Some(RESET),
            Some(RESET),
        ));
        // Minute-granularity reset prose must not reject an execution that
        // OPENED the session (the placed start can be up to a minute late).
        assert!(within_one_limit_window(
            Axis::Session,
            t("2029-12-31T22:59:15Z"),
            t("2030-01-01T01:00:00Z"),
            Some(RESET),
            Some(RESET),
        ));
    }

    #[test]
    fn the_cash_axis_is_denominated_in_dollars() {
        // ROUND-7 item 5. The axis exists end to end; what it still lacks is a
        // live gauge — no observed claude build reports a cash row.
        assert_eq!(Axis::Cash.unit(), Unit::Dollars);
        assert_eq!(Axis::Session.unit(), Unit::Percent);
        assert_eq!(Axis::parse("cash"), Some(Axis::Cash));
        assert_eq!(Axis::from_pool_label("extra usage"), Some(Axis::Cash));
        assert_eq!(Axis::from_pool_label("overage"), Some(Axis::Cash));
        // Budgets meter every axis now, not two.
        assert_eq!(Axis::BUDGET_AXES.len(), 4);
        assert_eq!(Axis::PERCENT_AXES.len(), 3);
    }

    #[test]
    fn a_dollar_denominated_usage_row_parses_as_dollars() {
        // The SEAM for a live cash gauge: a `$` row yields the dollar amount
        // where a `%` row yields the percentage. Speculative shape — see the
        // round-7 report for what a real gauge still has to supply.
        let prose = "Current session: 12% used · resets Jul 25 at 1:59am (America/Chicago)\n\
                     Current extra usage: $4.50 of $20.00 · resets Aug 1\n";
        let pools = parse_usage_prose(prose);
        assert_eq!(pools.len(), 2);
        assert_eq!(pools[0].axis(), Some(Axis::Session));
        assert_eq!(pools[0].pct, 12.0);
        assert_eq!(pools[1].axis(), Some(Axis::Cash));
        assert_eq!(pools[1].pct, 4.50);
    }

    #[test]
    fn only_fable_usage_reaches_the_fable_axis() {
        // ROUND-7 item 4: the weekly-fable BUDGET must count fable usage only,
        // and its definition must match the calibration filter exactly.
        assert!(is_fable_model("claude-fable-5"));
        assert!(!is_fable_model("claude-sonnet-5"));
        assert!(Axis::WeeklyFable.accepts_model("claude-fable-5"));
        assert!(!Axis::WeeklyFable.accepts_model("claude-sonnet-5"));
        for axis in [Axis::Session, Axis::WeeklyAll, Axis::Cash] {
            assert!(axis.accepts_model("claude-sonnet-5"));
            assert!(axis.accepts_model("claude-fable-5"));
        }
    }

    /// THE decorative-budgets dragon, in one assertion: two polls of identical
    /// prose must resolve to a byte-identical instant. When they did not, the
    /// rollover guard in `calibrate_overlap_window` rejected every observation pair
    /// and the percentage meters could never leave `calibrating`.
    ///
    /// # The prose is built from `now`, and that is load-bearing
    ///
    /// This test used to name three fixed instants — `Jul 30 at 1:59am`,
    /// `6:50pm`, `Aug 1 at 1pm` — and it was green all year only because
    /// `ResetTime::resolve` would invent a future instant for a date that had
    /// gone by, rolling it forward to the next matching clock time. That
    /// invention is the 2026-10-06 false-bar defect and it is now gone: a
    /// passed reset resolves to `None`, so fixed past dates make
    /// `expect("prose must resolve")` panic, as they did here on 2026-10-05.
    ///
    /// The property this test is actually about is DETERMINISM — identical prose
    /// in, byte-identical instant out — and that property has nothing to do with
    /// which calendar day the suite runs on. So the prose is derived from the
    /// current local time and is deliberately AHEAD of it, which is also the
    /// only state real `/usage` prose is ever in when the poller reads it.
    #[test]
    fn identical_prose_resolves_to_a_byte_identical_instant() {
        use chrono::Timelike;
        let now = crate::clock::now_local();
        let clock = |t: chrono::DateTime<chrono::Local>| {
            let h24 = t.hour();
            let (h12, half) = match h24 {
                0 => (12, "am"),
                1..=11 => (h24, "am"),
                12 => (12, "pm"),
                _ => (h24 - 12, "pm"),
            };
            format!("{h12}:{:02}{half}", t.minute())
        };
        let dated = |days: i64, t: chrono::DateTime<chrono::Local>| {
            let d = t + chrono::Duration::days(days);
            format!("{} at {} (America/Chicago)", d.format("%b %-d"), clock(d))
        };
        // Two dated windows still ahead (the weekly pool's shape) and one
        // time-only window two hours out (the session pool's), which is what
        // `/usage` hands this function in practice.
        let two_hours = now + chrono::Duration::hours(2);
        for prose in [
            dated(1, now),
            dated(3, now),
            format!("{} (America/Chicago)", clock(two_hours)),
        ] {
            let prose = prose.as_str();
            let first = prose_to_rfc3339(prose).expect("prose must resolve");
            // Deliberately let real time advance between the two calls — the
            // old implementation's drift came from exactly this gap.
            std::thread::sleep(std::time::Duration::from_millis(3));
            let second = prose_to_rfc3339(prose).expect("prose must resolve");
            assert_eq!(first, second, "resolving {prose:?} twice must not drift");
            // And it is truncated to the second, so no sub-second noise can
            // ever re-enter the comparison.
            assert!(
                !first.contains('.'),
                "resolved instant must carry no sub-second component: {first}"
            );
        }
    }

    /// The mid-turn cash estimate. Without it `MessageUsage` moves no dollar
    /// meter and a cash budget cannot trip until the turn is already over —
    /// which made the kill path unreachable (observed 2026-07-29).
    #[test]
    fn message_tokens_price_into_a_mid_turn_cash_estimate() {
        // Haiku at $1/$5 per Mtok: 1M input = $1.00, 1M output = $5.00,
        // 1M cache reads = $0.10, 1M cache writes = $1.25.
        let c = estimate_cost_usd("claude-haiku-4-5", 1_000_000, 0, 0, 0);
        assert!((c - 1.00).abs() < 1e-9, "{c}");
        let c = estimate_cost_usd("claude-haiku-4-5", 0, 1_000_000, 0, 0);
        assert!((c - 5.00).abs() < 1e-9, "{c}");
        let c = estimate_cost_usd("claude-haiku-4-5", 0, 0, 1_000_000, 0);
        assert!((c - 0.10).abs() < 1e-9, "{c}");
        let c = estimate_cost_usd("claude-haiku-4-5", 0, 0, 0, 1_000_000);
        assert!((c - 2.00).abs() < 1e-9, "{c}");
        // Tiers are distinguished, and a fable-model charge is the dearest.
        let tokens = (1_000_000, 0, 0, 0);
        let haiku = estimate_cost_usd("claude-haiku-4-5", tokens.0, 0, 0, 0);
        let sonnet = estimate_cost_usd("claude-sonnet-5", tokens.0, 0, 0, 0);
        let opus = estimate_cost_usd("claude-opus-5", tokens.0, 0, 0, 0);
        let fable = estimate_cost_usd("claude-fable-5", tokens.0, 0, 0, 0);
        assert!(haiku < sonnet && sonnet < opus && opus < fable);
        // An unknown model prices at the TOP tier — under-charging is the
        // dangerous direction, and reconciliation corrects an over-charge.
        assert_eq!(
            estimate_cost_usd("some-future-model", 1_000_000, 0, 0, 0),
            fable
        );
        // Zero usage costs zero (a no-op charge must stay a no-op).
        assert_eq!(estimate_cost_usd("claude-haiku-4-5", 0, 0, 0, 0), 0.0);
    }

    /// The live turn this table was calibrated against — real haiku-4.5 spend
    /// captured 2026-07-29, with the `costUSD` claude itself reported.
    ///
    /// It reproduces to the cent, which is what makes the mid-turn cash meter
    /// trustworthy enough to KILL on. It is still only an estimate: it prices
    /// the counts the stream happens to carry, and `result_turn` remains the
    /// authority (see `estimate_cost_usd`).
    #[test]
    fn the_estimate_reproduces_a_real_turn_but_is_still_not_the_record() {
        let est = estimate_cost_usd("claude-haiku-4-5-20251001", 9, 43, 18_664, 3_687);
        let authoritative = 0.0094644_f64;
        assert!(
            (est - authoritative).abs() < 1e-9,
            "estimate {est} vs authoritative {authoritative}"
        );
        // A turn whose output the stream under-reports (the usual case: the
        // per-message line lags the aggregate) still estimates LOW, and the
        // epilogue's signed reconciliation closes the gap either way.
        let mid_turn = estimate_cost_usd("claude-haiku-4-5-20251001", 9, 4, 18_664, 3_687);
        assert!(mid_turn < authoritative);
    }

    #[test]
    fn utilization_is_a_fraction_not_a_percent() {
        // The burn-in's verbatim value: 0.73 means 73%.
        let observed = 0.73_f64;
        assert!((observed * 100.0 - 73.0).abs() < 1e-9);
    }

    // ── the settle profile (edge polling) ────────────────────────────────

    fn rung(offset_secs: u64, pct: f64) -> SettleReading {
        SettleReading {
            offset_ms: offset_secs * 1000,
            pct,
        }
    }

    #[test]
    fn a_settle_ladder_measures_where_the_meter_stopped_moving() {
        // The shape a lagging meter has: it rises, then holds. The measured
        // settle delay is the FIRST offset carrying the final reading — +30s
        // here, not +120s, because +120s only confirms it.
        let p = settle_profile(&[rung(5, 41.0), rung(30, 43.0), rung(120, 43.0)]);
        assert_eq!(p.settled_after_secs(), Some(30.0));
        assert!(!p.still_moving);
        assert!(p.determinate());
        assert!((p.moved_pct - 2.0).abs() < 1e-9);

        // Already settled at the first rung: the meter was current when the turn
        // landed, and the answer is +5s, not "no movement, so unknown".
        let p = settle_profile(&[rung(5, 41.0), rung(30, 41.0), rung(120, 41.0)]);
        assert_eq!(p.settled_after_secs(), Some(5.0));
        assert!((p.moved_pct - 0.0).abs() < 1e-9);

        // Rungs may arrive in any order.
        let p = settle_profile(&[rung(120, 43.0), rung(5, 41.0), rung(30, 43.0)]);
        assert_eq!(p.settled_after_secs(), Some(30.0));
        assert_eq!(
            p.rungs.iter().map(|r| r.offset_ms).collect::<Vec<_>>(),
            vec![5_000, 30_000, 120_000]
        );
    }

    #[test]
    fn a_ladder_that_is_still_moving_refuses_to_name_a_settle_delay() {
        // Still climbing at the last rung: the ladder is too short, and the
        // honest answer is "I do not know" — NOT the last rung. Guessing low
        // here reads a small delta, which estimates a LARGE limit, which is the
        // direction where a budget fails to trip.
        let p = settle_profile(&[rung(5, 41.0), rung(30, 43.0), rung(120, 45.0)]);
        assert_eq!(p.settled_after_secs(), None);
        assert!(p.still_moving);
        assert!(!p.determinate());
        assert!(p.describe().contains("STILL MOVING"));

        // One rung says nothing at all: a pair is the minimum that can.
        let p = settle_profile(&[rung(5, 41.0)]);
        assert_eq!(p.settled_after_secs(), None);
        assert!(!p.still_moving);
        let p = settle_profile(&[]);
        assert_eq!(p.settled_after_secs(), None);
        assert!(p.rungs.is_empty());
    }

    /// The backstop behind the settle ladder: if two RECORDED polls in one idle
    /// gap disagree, the window is refused outright.
    ///
    /// The ladder's own defence is to record nothing until the reading stops
    /// moving (`ThreadManager::walk_settle_ladder`). This rule is what catches
    /// the case the ladder cannot see — the meter settles, and then a third party
    /// spends between the two closing polls.
    #[test]
    fn a_still_settling_ladder_rejects_its_own_window() {
        let w = OverlapWindow {
            start: t("2030-01-01T00:01:00Z"),
            end: t("2030-01-01T00:02:00Z"),
        };
        let reset = "2030-01-01T04:00:00Z";
        let obs = |id: i64, at: &str, pct: f64| ObservationRow {
            id,
            account: "a".into(),
            axis: "session".into(),
            pct,
            resets_at: Some(reset.to_string()),
            source: POLL_SOURCE.into(),
            observed_at: at.to_string(),
        };
        // The opening pair (quiet, agreeing) and a closing ladder whose first two
        // rungs disagree — the meter was still catching up.
        let moving = vec![
            obs(1, "2030-01-01T00:00:30Z", 40.0),
            obs(2, "2030-01-01T00:00:50Z", 40.0),
            obs(3, "2030-01-01T00:02:05Z", 41.0),
            obs(4, "2030-01-01T00:02:30Z", 43.0),
        ];
        assert_eq!(
            evaluate_window(
                w,
                WindowEvidence {
                    axis: Axis::Session,
                    all_windows: &[w],
                    observations: &moving,
                    tokens: 1_000_000,
                    unsettled_since: None,
                }
            ),
            Err(WindowRejection::IdleMovement),
            "a ladder that is still moving must refuse the sample, not shrink the delta"
        );

        // The same window with a SETTLED closing ladder is a sample — and the
        // delta is the full, settled one (40 -> 43), not the early 40 -> 41 that
        // would have estimated a limit three times too large.
        let settled = vec![
            obs(1, "2030-01-01T00:00:30Z", 40.0),
            obs(2, "2030-01-01T00:00:50Z", 40.0),
            obs(3, "2030-01-01T00:02:05Z", 43.0),
            obs(4, "2030-01-01T00:02:30Z", 43.0),
        ];
        let tpp = evaluate_window(
            w,
            WindowEvidence {
                axis: Axis::Session,
                all_windows: &[w],
                observations: &settled,
                tokens: 1_000_000,
                unsettled_since: None,
            },
        )
        .expect("a quiet, settled bracket is a sample");
        assert!(
            (tpp - 1_000_000.0 / 3.0).abs() < 1.0,
            "3 points of settled movement, not 1: {tpp}"
        );
    }
}
