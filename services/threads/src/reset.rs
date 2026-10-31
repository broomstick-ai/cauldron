//! Reset-time prose parsing (ported verbatim from CCW `reset.rs`, which the
//! design doc rated "native, better" than AUI's parser — alignment map #21).
//!
//! Claude Code surfaces limit reset times as LOCAL-TIME PROSE, not epochs.
//! Two surfaces produce it:
//!
//! * `/usage` rows — `resets Jul 25 at 1:59am (America/Chicago)`
//! * 429 synthetic messages — `resets 2:40am (America/Chicago)`
//!
//! We parse the prose into a structured [`ResetTime`] and resolve it to a
//! concrete instant relative to a reference "now" so retry scheduling can wait
//! for the actual reset. We never fabricate a time — a string that does not
//! parse yields `None`.

use chrono::{Datelike, NaiveDate, NaiveDateTime};

/// Parse the local-time reset prose out of a 429/limit message body, e.g.
/// `You've hit your session limit · resets 2:40am (America/Chicago)`.
/// (Ported from CCW `transcript::parse_limit_reset` — the only piece of
/// `transcript.rs` the daemon path ever used.)
pub fn parse_limit_reset(text: &str) -> Option<ResetTime> {
    let idx = text.find("resets ")?;
    parse_reset(text[idx + "resets ".len()..].trim())
}

/// A parsed reset descriptor. `raw` is preserved verbatim for display/ETA prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetTime {
    /// The verbatim descriptor, e.g. `Jul 25 at 1:59am (America/Chicago)`.
    pub raw: String,
    /// Month (1-12) if the prose named a date; `None` for time-only prose.
    pub month: Option<u32>,
    /// Day of month if named.
    pub day: Option<u32>,
    /// Hour in 24h form.
    pub hour: u32,
    /// Minute.
    pub minute: u32,
    /// IANA timezone label if present (kept as an opaque string).
    pub tz: Option<String>,
}

fn month_num(tok: &str) -> Option<u32> {
    let m = match tok.to_ascii_lowercase().as_str() {
        "jan" | "january" => 1,
        "feb" | "february" => 2,
        "mar" | "march" => 3,
        "apr" | "april" => 4,
        "may" => 5,
        "jun" | "june" => 6,
        "jul" | "july" => 7,
        "aug" | "august" => 8,
        "sep" | "sept" | "september" => 9,
        "oct" | "october" => 10,
        "nov" | "november" => 11,
        "dec" | "december" => 12,
        _ => return None,
    };
    Some(m)
}

/// Parse a `h:mm am/pm` / `h am/pm` clock token into 24h (hour, minute).
fn parse_clock(tok: &str) -> Option<(u32, u32)> {
    let lower = tok.to_ascii_lowercase();
    let (body, pm) = if let Some(b) = lower.strip_suffix("am") {
        (b, false)
    } else if let Some(b) = lower.strip_suffix("pm") {
        (b, true)
    } else {
        return None;
    };
    let body = body.trim();
    let (h, m) = match body.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (body.parse::<u32>().ok()?, 0),
    };
    if h > 12 || m > 59 {
        return None;
    }
    let hour24 = match (h % 12, pm) {
        (h12, false) => h12,      // 12am -> 0, 1am -> 1
        (h12, true) => h12 + 12,  // 12pm -> 12, 1pm -> 13
    };
    Some((hour24, m))
}

/// Parse a reset descriptor. Accepts (with or without an `at`, a date, or a tz):
/// `Jul 25 at 1:59am (America/Chicago)`, `2:40am (America/Chicago)`, `1pm`.
pub fn parse_reset(raw: &str) -> Option<ResetTime> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    // Peel off a trailing "(tz)".
    let (head, tz) = match (raw.rfind('('), raw.rfind(')')) {
        (Some(o), Some(c)) if c > o => (
            raw[..o].trim().to_string(),
            Some(raw[o + 1..c].trim().to_string()),
        ),
        _ => (raw.to_string(), None),
    };

    // Tokenize; drop the connective "at".
    let toks: Vec<&str> = head
        .split_whitespace()
        .filter(|t| !t.eq_ignore_ascii_case("at"))
        .collect();

    let mut month = None;
    let mut day = None;
    let mut clock = None;

    for t in &toks {
        if clock.is_none() {
            if let Some(c) = parse_clock(t) {
                clock = Some(c);
                continue;
            }
        }
        if month.is_none() {
            if let Some(m) = month_num(t) {
                month = Some(m);
                continue;
            }
        }
        if month.is_some() && day.is_none() {
            if let Ok(d) = t.trim_end_matches(|c: char| !c.is_ascii_digit()).parse::<u32>() {
                if (1..=31).contains(&d) {
                    day = Some(d);
                    continue;
                }
            }
        }
    }

    let (hour, minute) = clock?;
    Some(ResetTime {
        raw: raw.to_string(),
        month,
        day,
        hour,
        minute,
        tz,
    })
}

/// The longest window any pool Claude Code exposes can name: the seven-day
/// weekly, with a day of slack. Mirrors `runtime::MAX_PARSED_RESET`, which
/// refuses anything further out as a second line of defence. Used here for the
/// ONE case where a dated reset legitimately resolves into the next calendar
/// year.
const MAX_PLAUSIBLE_WINDOW: chrono::Duration = chrono::Duration::days(8);

/// How far a TIME-ONLY reset may legitimately roll past midnight.
///
/// Time-only prose (`resets 12:30am`) carries no date, so a clock time behind
/// `now` is ambiguous between "late tonight, wrapping midnight" and "the same
/// time tomorrow". The session pool resets every FIVE HOURS, so the honest
/// reading is bounded: a wrap is minutes-to-hours away, and anything near a
/// full day is not a window this service has.
const MAX_MIDNIGHT_ROLL: chrono::Duration = chrono::Duration::hours(6);

/// **ONE MINUTE OF SLACK PAST A NAMED RESET, BECAUSE THE VENDOR DOES NOT LIFT A
/// QUOTA AT THE INSTANT IT NAMES.**
///
/// > *"It's probably just trying to place on an account before the limit is
/// > really raised. The solution is probably just to extend the limit by 1
/// > minute."* — the operator, 2026-10-06
///
/// # The evidence, measured off the live store
///
/// The two false 24-hour bars of 2026-10-06 were written with `limit_until - now`
/// equal to **23:59:47** and **23:59:40** — 24 hours less thirteen and less
/// twenty seconds. That arithmetic is a signature, not a coincidence: it says the
/// prose named an instant that had elapsed seconds earlier and
/// [`ResetTime::resolve`] rolled it forward a day.
///
/// The three `retries` rows that stranded the operator's threads say the same
/// thing to three decimal places. `compute_retry_at` wrote `resolved + 20s`, so
/// the resolved instant divides straight back out of them, and `created_at` is
/// the moment the refusal was taken:
///
/// ```text
/// thread     retry_at (resolved+20s)              resolved        row written      late by
/// 0f14be24   2026-10-07T08:19:19.999973Z   Oct 7 08:19:00   08:19:05.588141Z    5.588s
/// bcccfa79   2026-10-07T08:19:19.999973Z   Oct 7 08:19:00   08:19:12.711775Z   12.712s
/// 60f0380d   2026-10-07T08:19:19.999975Z   Oct 7 08:19:00   08:19:12.720620Z   12.721s
/// ```
///
/// The prose named **Oct 6** 08:19:00 and resolved to **Oct 7**. All three
/// refusals were taken within thirteen seconds AFTER the instant it named, and
/// from the daemon's own log at `03:19:17Z` — seventeen seconds past the same
/// reset — `/usage` was still reporting `session = 100%`.
///
/// So the input to the misparse was the daemon asking about an instant it had
/// just crossed, and the reason it was asking is that **the upstream quota had
/// not actually lifted yet.** `limit_until` was written as the named instant
/// exactly, so the account re-entered the pool at reset + 0s, three threads went
/// out inside thirteen seconds, the vendor answered each with the stale exhausted
/// reading and the SAME already-passed prose, and the resolver was handed the one
/// input it got wrong — three times.
///
/// # IT WOULD HAVE PREVENTED ALL THREE, ON ITS OWN
///
/// With a minute of slack the correct window closed at `08:20:00Z`, so at
/// `08:19:05.588`, `08:19:12.711` and `08:19:12.720` the account was still
/// barred: `AccountState::available` is false, `unavailable_accounts` carries it,
/// `admit` drops it from the candidate set, and no turn goes out. No 429 is
/// taken, so `resolve` is never handed the crossed instant, so there is no
/// 24-hour bar and no 24-hour floor. **The operator's one-line hypothesis is
/// sufficient by itself, and this is not a reasoned claim — it is his own three
/// rows arithmetic.**
///
/// It is still not a fix for the misparse, and it is not offered as one.
/// `804cffd` fixed the resolution, so a passed reset now answers `None` and the
/// SAME race costs one wasted spawn and a 30-second ladder rung instead of a day.
/// The two are independent lines of defence against one input, and that input
/// also fed the 2026-08-15 five-day bar: the slack is what stops the question
/// being asked, and `804cffd` is what makes the answer honest when something else
/// asks it.
///
/// A minute is also the right SIZE: it is below every meaningful retry rung, so
/// it costs one extra minute of an already-exhausted window and never a turn, and
/// it is the number the operator named.
///
/// **THERE IS NOW ONLY ONE DERIVATION FROM A RESET, AND THIS IS ITS SLACK.** It was
/// applied at two — the bar (`runtime::note_account_limit_for`) and a per-thread
/// retry floor (`runtime::compute_retry_at`), which kept a further `+20s` grace on
/// top so that the bar lifted before the floor came due. The operator removed the
/// per-thread timestamp (*"There should not be a per-thread retry timestamp"*), so
/// the floor and its grace are gone and the bar is the whole of the waiting: a
/// parked prompt is reconsidered by an event or by the pool's cadence, and the gate
/// reads this bar every time. Two derivations can no longer drift because there is
/// one.
pub const RESET_SLACK: chrono::Duration = chrono::Duration::seconds(60);

impl ResetTime {
    /// Resolve to a concrete local instant after `now`, or `None` when the
    /// prose does not describe a future window at all.
    ///
    /// * With a named date: that date this year, when it is still ahead. When it
    ///   has gone by, the window it names has already OPENED — see below.
    /// * Time-only prose: today's occurrence when it is still ahead; otherwise
    ///   tomorrow's, but only when that is a genuine midnight wrap
    ///   ([`MAX_MIDNIGHT_ROLL`]).
    ///
    /// Timezone is treated as a label only — resolution is done in the same
    /// naive frame as `now` (all resets for a given account share a tz).
    ///
    /// # A RESET THAT HAS PASSED IS NOT A FUTURE WINDOW. IT IS NO WINDOW.
    ///
    /// This has now been got wrong twice in the same place, each time by
    /// rolling a passed instant FORWARD to the next thing that looked like it.
    ///
    /// The first was `year += 1`, and it barred a healthy account for five days:
    /// on 2026-08-15 `owner-account` took a 429 reading `Aug 17 at
    /// 5:49pm (America/Chicago)`, the 17th went by, and the parser answered
    /// **2027**-08-17T22:49Z.
    ///
    /// The fix for that rolled forward by a DAY instead, and called itself
    /// "bounded by construction: at most 24 hours out". Twenty-four hours is
    /// not a bound, it is the SAME BUG with a smaller number, and on
    /// 2026-10-06 it cost the operator his working account. The `/usage` poller
    /// read `session = 100%` with `resets Oct 5 at 10:19pm (America/Chicago)`
    /// twenty-three seconds AFTER 10:19pm. At 02:55:03Z that unchanged prose
    /// had resolved to `2026-10-06T03:19:00Z`, correctly; at 03:19:17Z it
    /// resolved to `2026-10-07T03:19:00Z`. Byte-identical prose, 24 hours of
    /// difference, and the only thing that moved was the clock crossing the
    /// instant the prose names. The account was barred until the following
    /// night while `/usage` reported it at 8% of a fresh session.
    ///
    /// So the rule is not "find the nearest plausible instant" — it is that
    /// **a reset instant at or before `now` describes a window that has already
    /// ENDED, and a window that has ended bars nothing.** `None` is the honest
    /// answer, and every caller already handles it correctly and conservatively:
    /// `note_account_limit_for` leaves whatever window is recorded alone rather
    /// than writing a new one, `compute_retry_at` falls back to the
    /// 30/60/120/300s ladder, and `gauge::prose_to_rfc3339` records no
    /// `resets_at` for that reading. None of them invent a bar.
    ///
    /// Rolling forward survives in exactly two shapes, both of which are real
    /// calendar arithmetic rather than a guess about which window was meant:
    /// the next calendar YEAR for a dated reset (`Jan 2` read on `Dec 31`),
    /// accepted only inside [`MAX_PLAUSIBLE_WINDOW`]; and the next DAY for
    /// time-only prose, accepted only inside [`MAX_MIDNIGHT_ROLL`].
    pub fn resolve(&self, now: NaiveDateTime) -> Option<NaiveDateTime> {
        match (self.month, self.day) {
            (Some(mo), Some(d)) => {
                let cand = NaiveDate::from_ymd_opt(now.year(), mo, d)?
                    .and_hms_opt(self.hour, self.minute, 0)?;
                if cand > now {
                    return Some(cand);
                }
                // THE CALENDAR WRAP, AND NOTHING ELSE. `Jan 2 at 1am` read on
                // `Dec 31` names next year and is two days out; the same prose
                // read in March names a date ten months behind and is not a
                // window at all. The bound is what tells those apart, so this
                // cannot become the year-rollover defect again.
                let wrapped = NaiveDate::from_ymd_opt(now.year() + 1, mo, d)?
                    .and_hms_opt(self.hour, self.minute, 0)?;
                if wrapped > now && wrapped - now <= MAX_PLAUSIBLE_WINDOW {
                    return Some(wrapped);
                }
                None
            }
            _ => self.next_clock(now),
        }
    }

    /// Time-only prose: today's occurrence of this clock time when it is still
    /// ahead of `now`, else tomorrow's — but only when tomorrow's is a genuine
    /// MIDNIGHT WRAP and not the 24-hour artefact.
    ///
    /// `resets 12:30am` read at 11:50pm is forty minutes away and must survive.
    /// `resets 2:40am` read at 3:00am is not twenty-three hours and forty
    /// minutes away; the 2:40am window has already ended, and there is no
    /// daily pool for it to have meant. [`MAX_MIDNIGHT_ROLL`] is the line
    /// between those two, drawn at the session cadence.
    fn next_clock(&self, now: NaiveDateTime) -> Option<NaiveDateTime> {
        let today = now.date();
        let cand = today.and_hms_opt(self.hour, self.minute, 0)?;
        if cand > now {
            return Some(cand);
        }
        let tomorrow = today.succ_opt()?.and_hms_opt(self.hour, self.minute, 0)?;
        (tomorrow - now <= MAX_MIDNIGHT_ROLL).then_some(tomorrow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").unwrap()
    }

    /// **THE 24-HOUR BAR, 2026-10-06.** The operator's own row, and the exact
    /// pair of instants out of the daemon's log.
    ///
    /// `/usage` reported `session = 100%` with `resets Oct 5 at 10:19pm
    /// (America/Chicago)`. The SAME string resolved two different ways twenty-two
    /// seconds apart, because the clock crossed the instant it names:
    ///
    /// ```text
    /// 02:55:03Z  until=2026-10-06T03:19:00Z   (correct — still ahead)
    /// 03:19:17Z  until=2026-10-07T03:19:00Z   (a full day of false bar)
    /// ```
    ///
    /// `accounts.limit_until` held the second one, nothing could take it out
    /// (the only early clear is a completed turn, and the bar is what prevents
    /// one), and `/usage` reported the account at 8% of a FRESH session window
    /// throughout.
    ///
    /// This is the test that had to go red: before the fix the second assertion
    /// answered `2026-10-06T22:19:00`.
    #[test]
    fn a_reset_the_clock_has_just_crossed_is_not_a_window_for_another_day() {
        let r = parse_reset("Oct 5 at 10:19pm (America/Chicago)").expect("the operator's prose");

        // 02:55:03Z = 21:55:03 local. Still ahead: the window is real and named.
        assert_eq!(
            r.resolve(at("2026-10-05T21:55:03")),
            Some(at("2026-10-05T22:19:00")),
            "while the named instant is still ahead it is the window, exactly as written"
        );

        // 03:19:23Z = 22:19:23 local — twenty-three seconds LATE, the instant
        // the live row was written at.
        assert_eq!(
            r.resolve(at("2026-10-05T22:19:23")),
            None,
            "twenty-three seconds past the named reset the window has ENDED; it is not a \
             window that lasts until the same time tomorrow"
        );
    }

    /// **THE FIVE-DAY BAR, 2026-08-15** — the same defect's first form, kept
    /// because it is the same rule and must stay fixed. `owner-account`
    /// took a 429 reading `Aug 17 at 5:49pm (America/Chicago)`; resolved after
    /// the 17th had passed, `year += 1` answered 2027 and the account sat barred
    /// at `session_pct=0.0` for five days.
    ///
    /// The 2026 fix answered `2026-08-20T17:49` here — a day instead of a year,
    /// which was the 2026-10-06 bug. The answer is neither.
    #[test]
    fn a_dated_reset_that_has_passed_is_no_window_at_all() {
        let r = parse_reset("Aug 17 at 5:49pm (America/Chicago)").expect("the operator's prose");
        assert_eq!(
            r.resolve(at("2026-08-20T12:00:00")),
            None,
            "a reset three days behind us describes a window that has already ended — not \
             next year, and not this afternoon"
        );
    }

    /// The same prose seen the moment AFTER the named instant — the case that
    /// actually wrote both bad rows. One minute late must cost neither a year
    /// nor a day.
    #[test]
    fn one_minute_past_the_named_instant_is_no_window() {
        let r = parse_reset("Aug 17 at 5:49pm (America/Chicago)").unwrap();
        assert_eq!(r.resolve(at("2026-08-17T17:50:00")), None);
    }

    /// The anti-test: a date still AHEAD of us is untouched. This is the weekly
    /// pool's own case — `resets Aug 22 at 1pm` seen on the 20th is a genuine
    /// two-day window and must resolve to exactly that instant.
    #[test]
    fn a_dated_reset_still_ahead_is_left_alone() {
        let r = parse_reset("Aug 22 at 1pm (America/Chicago)").unwrap();
        assert_eq!(
            r.resolve(at("2026-08-20T12:00:00")).unwrap(),
            at("2026-08-22T13:00:00"),
            "a real weekly window is up to seven days out and must survive intact"
        );
    }

    /// Time-only prose still ahead today is the ordinary session case and is
    /// untouched.
    #[test]
    fn time_only_prose_still_ahead_today_is_left_alone() {
        let r = parse_reset("2:40am (America/Chicago)").unwrap();
        assert_eq!(
            r.resolve(at("2026-08-20T01:00:00")).unwrap(),
            at("2026-08-20T02:40:00"),
            "still ahead today"
        );
    }

    /// **THE MIDNIGHT WRAP — the anti-test for this fix's own reach.** Time-only
    /// prose whose clock time is behind `now` but whose real instant is minutes
    /// away across midnight is a GENUINE window and must survive. If the fix for
    /// the 24-hour bar had simply refused every passed clock time, this is what
    /// it would have broken, and a forty-minute session wait would have become
    /// "no reset known".
    #[test]
    fn a_midnight_wrap_is_a_real_window_and_survives() {
        let r = parse_reset("12:30am (America/Chicago)").unwrap();
        assert_eq!(
            r.resolve(at("2026-08-20T23:50:00")).unwrap(),
            at("2026-08-21T00:30:00"),
            "forty minutes across midnight is the session pool, not a daily one"
        );
    }

    /// The other side of the same line: a time-only reset that has gone by
    /// EARLIER today is not a window twenty-three hours out. There is no daily
    /// pool for it to have meant.
    #[test]
    fn time_only_prose_that_went_by_this_morning_is_no_window() {
        let r = parse_reset("2:40am (America/Chicago)").unwrap();
        assert_eq!(
            r.resolve(at("2026-08-20T03:00:00")),
            None,
            "2:40am seen at 3:00am has ended; it is not 23h40m of bar"
        );
    }

    /// The calendar wrap, which is the one roll-forward that is real
    /// arithmetic rather than a guess: `Jan 2 at 1am` read on `Dec 31` names
    /// next year and is two days out.
    #[test]
    fn a_dated_reset_across_new_year_resolves_into_the_next_year() {
        let r = parse_reset("Jan 2 at 1am (America/Chicago)").unwrap();
        assert_eq!(
            r.resolve(at("2026-12-31T12:00:00")).unwrap(),
            at("2027-01-02T01:00:00"),
            "a weekly window may cross the new year, and that is not a rollover artefact"
        );
    }

    /// …and the bound that stops that carve-out becoming the year-rollover
    /// defect again: the same prose read in March names a date ten months
    /// behind, and next year's is nine months ahead. Neither is a window.
    #[test]
    fn the_new_year_carve_out_cannot_resurrect_the_year_rollover() {
        let r = parse_reset("Jan 2 at 1am (America/Chicago)").unwrap();
        assert_eq!(
            r.resolve(at("2026-03-15T12:00:00")),
            None,
            "nine months out is further than any pool that exists; it must not be a bar"
        );
    }

    /// **THE PROPERTY, OVER THE WHOLE CALENDAR AND THE WHOLE CLOCK.** Nothing
    /// this parser accepts may resolve to an instant that is both in the future
    /// and NOT justified by the prose itself:
    ///
    /// * a resolution is always strictly after `now`, and
    /// * it never exceeds [`MAX_PLAUSIBLE_WINDOW`], and
    /// * when the prose names a date that has already gone by, there is no
    ///   resolution at all unless the next-calendar-year reading is itself
    ///   inside the bound.
    ///
    /// **This assertion could fail, and did.** Run against the pre-fix resolver
    /// the third clause is violated by every passed date in the calendar, each
    /// of which answered a same-clock-time instant up to 24 hours out.
    #[test]
    fn a_passed_date_never_becomes_a_future_window_whatever_the_month() {
        let now = at("2026-08-20T12:00:00");
        let mut passed_seen = 0;
        for month in 1..=12u32 {
            for day in [1u32, 15, 28] {
                let raw = format!("{} {day} at 5:49pm", month_name(month));
                let r = parse_reset(&raw).unwrap_or_else(|| panic!("unparsed: {raw}"));
                let named = NaiveDate::from_ymd_opt(2026, month, day)
                    .unwrap()
                    .and_hms_opt(17, 49, 0)
                    .unwrap();
                match r.resolve(now) {
                    Some(got) => {
                        assert!(got > now, "{raw} resolved into the past: {got}");
                        if named <= now {
                            assert!(
                                got - now <= MAX_PLAUSIBLE_WINDOW,
                                "{raw} has gone by, and the only roll allowed for it resolved \
                                 {got} — further out than any pool that exists"
                            );
                            // The ONLY legal resolution for a passed date.
                            let wrapped = NaiveDate::from_ymd_opt(2027, month, day)
                                .unwrap()
                                .and_hms_opt(17, 49, 0)
                                .unwrap();
                            assert_eq!(
                                got, wrapped,
                                "{raw} has gone by, so the only instant it may name is next \
                                 year's — never a rolled clock time"
                            );
                        }
                    }
                    None => assert!(
                        named <= now,
                        "{raw} is still ahead and must resolve, not be refused"
                    ),
                }
                if named <= now {
                    passed_seen += 1;
                }
            }
        }
        // The loop has to have EXERCISED the passed case, or the clause above
        // is decoration.
        assert!(passed_seen >= 20, "only {passed_seen} passed dates exercised");
    }

    /// **NO PROSE AT ALL MAY PRODUCE A WINDOW NEAR A FULL DAY.** The shape of
    /// the 2026-10-06 bug, asserted over every clock minute the CLI could print
    /// against every position of `now` within a day, with no date in the prose —
    /// the branch that still rolls forward.
    ///
    /// Pre-fix this fails on the first passed clock time it reaches.
    #[test]
    fn time_only_prose_never_resolves_near_a_full_day_out() {
        let mut rolled = 0;
        for h in 0..24u32 {
            for m in [0u32, 19, 40] {
                let suffix = if h < 12 { "am" } else { "pm" };
                let h12 = if h % 12 == 0 { 12 } else { h % 12 };
                let raw = format!("{h12}:{m:02}{suffix}");
                let r = parse_reset(&raw).unwrap_or_else(|| panic!("unparsed: {raw}"));
                for now_h in 0..24u32 {
                    let now = at(&format!("2026-08-20T{now_h:02}:30:00"));
                    if let Some(got) = r.resolve(now) {
                        assert!(got > now, "{raw} at {now} resolved into the past");
                        // The bound applies to the ROLL. A clock time still
                        // ahead today is the prose read literally and is not
                        // this fix's business; a clock time carried onto
                        // TOMORROW is, and that is where the 24 hours came from.
                        if got.date() != now.date() {
                            assert!(
                                got - now <= MAX_MIDNIGHT_ROLL,
                                "{raw} seen at {now} was carried to {got} — {} hours of bar \
                                 from prose that names no date, and the session pool is five",
                                (got - now).num_hours()
                            );
                            rolled += 1;
                        }
                    }
                }
            }
        }
        // The midnight-wrap branch must actually have been taken, or the bound
        // above was never tested against a roll at all.
        assert!(rolled > 0, "no midnight wrap was exercised");
    }

    fn month_name(m: u32) -> &'static str {
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
            [(m - 1) as usize]
    }
}
