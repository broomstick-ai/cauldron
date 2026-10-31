//! # **THE GOLDEN TRANSCRIPTS, REPLAYED THROUGH *THIS FACE'S* `Held::fold`.**
//!
//! `docs/consumer-seam.md` §7.3: *"Each app's `tests/it/seam_conformance.rs` replays
//! every transcript through `Held::fold` and asserts the `view::*` answers listed
//! in the transcript's expectation block, byte for byte."* This is that file for
//! the TERMINAL FACE, and it is a near-verbatim copy of the reference's own
//! `tests/conformance.rs` — the imports point at `tui::seam` instead of at the
//! reference crate, the transcripts are read from `libs/shapewire/seam/conformance`
//! so that both faces replay the IDENTICAL files, and `BLESS=1` is refused here
//! because the expectations are the reference's answers and a copy that could
//! rewrite them could make its own drift green.
//!
//! **What this proves that `tests/it/seam_reference.rs` does not.** That test says the
//! two copies are the same TEXT outside ONE fence. This one says they answer the
//! same thing — which is the property a reader actually depends on, and which
//! survives a refactor the diff would flag and a compiler change the diff would
//! not see.
//!
//! **And what it proves about THIS face in particular:** that the one fence this
//! copy spends — `D2`, `FACE = "tui"` and `ENV_PREFIX = "TUI_THREADS"` — moves no
//! answer any transcript locks. It cannot: both consts live in `link.rs`, which
//! has no transport in this test at all, and neither is read by `Held::fold` or
//! by any `view::*` function. The AUI's copy spends a SECOND fence here (`D3` —
//! it drops `narration/progress` because there is no spoken form for a progress
//! line) and this face does not: the terminal activity lane renders `progress`,
//! so `Book::standing` is the reference's own body verbatim and `held.rs` is
//! byte-identical. The two faces therefore replay these transcripts to the same
//! answers through code that is the same text, which is the strongest statement
//! this suite can make.
//!
//! ## Why the transcripts are two kinds, and why both say which they are
//!
//! * `"source": "live producer"` — frames recorded off the running daemon by
//!   `tools/record-transcript.py`. These are the ones that cannot be wrong
//!   about what the producer emits, which is the whole reason §7.3 asks for
//!   producer-generated transcripts: a hand-written frame can only ever
//!   confirm what its author already believed.
//! * `"source": "hand-written"` — the edge cases production will not hand over
//!   on demand: a key vanishing, a re-init, a death export, an `event.kind`
//!   this consumer does not know, and a promise re-offered across a reconnect.
//!   Each is marked, and each names the producer code path it is modelled on so
//!   a reader can check it rather than trust it.
//!
//! ## The expectations are BLESSED, not hand-written, and that is deliberate
//!
//! `BLESS=1 cargo test` rewrites every `expect` line from what the reference
//! actually answers. So the transcript is a REGRESSION LOCK rather than an
//! independent oracle — which is the correct role for it: the independent
//! oracle already exists one layer down (`tests/mirror_oracle.rs` differs the
//! Rust `Mirror` against the Python `ShapeMirror`), and what the seam needs on
//! top of that is a statement that two COPIES answer the same thing, plus a
//! red when the answer changes without anybody meaning it to.
//!
//! The honesty cost of a blessed golden is that a wrong answer blessed is a
//! wrong answer locked. It is paid for in two places:
//! `tests/fold_mutations.rs` proves the lock actually bites, and every bless is
//! a reviewable diff of the `expect` lines rather than an invisible recompute.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tui::seam::held::{Held, Pending};
use tui::seam::{ledger, view};
use serde_json::Value;

// ─────────────────────────────────────────────────────────────────────────────
//  One uniform renderer, so an expectation is one string
// ─────────────────────────────────────────────────────────────────────────────

/// **EVERY ANSWER AS `{:?}`, AND THAT IS NOT LAZINESS.**
///
/// `Fact<T>` is three-valued and the three values are the point (§4.1): a
/// renderer that serialised `Known(x)` as `x` would make `NotHydrated` and
/// `Dead` indistinguishable from each other and from a missing field, which is
/// the exact collapse `docs/shapes.md` 2.8 and the board's experience forbid.
/// `Debug` names the variant, so the locked string says WHICH of the three the
/// answer was, and a copy that turned a `NotHydrated` into a `Known(0)` goes
/// red rather than agreeing.
fn answer(h: &Held, call: &str, args: &[Value]) -> String {
    let s = |n: usize| args[n].as_str().unwrap_or_default().to_string();
    let u = |n: usize| args[n].as_u64().unwrap_or_default();
    match call {
        // the link
        "link" => format!("{:?}", view::link(h)),
        // the fleet
        "running" => format!("{:?}", view::running(h)),
        "running_count" => format!("{:?}", view::running_count(h)),
        "recent" => format!("{:?}", view::recent(h)),
        "accounts" => format!("{:?}", view::accounts(h)),
        "account_row" => format!("{:?}", view::account_row(h, &s(0))),
        "summary_truncated" => format!("{:?}", view::summary_truncated(h)),
        // one thread
        "holding" => format!("{:?}", view::holding(h, &s(0))),
        "status" => format!("{:?}", view::status(h, &s(0))),
        "status_transitions" => format!("{:?}", view::status_transitions(h, &s(0))),
        "liveness" => format!("{:?}", view::liveness(h, &s(0))),
        "positions" => format!("{:?}", view::positions(h, &s(0))),
        "settings" => format!("{:?}", view::settings(h, &s(0))),
        "account" => format!("{:?}", view::account(h, &s(0))),
        "header" => format!("{:?}", view::header(h, &s(0))),
        "title" => format!("{:?}", view::title(h, &s(0))),
        "app_payload" => format!("{:?}", view::app_payload(h, &s(0), &s(1))),
        "body_plan" => format!("{:?}", view::body_plan(h, &s(0))),
        // the log
        "log_rows_len" => format!("{:?}", view::log_rows(h, &s(0)).map(|r| r.len())),
        "log_rows_kinds" => format!(
            "{:?}",
            view::log_rows(h, &s(0)).map(|r| r
                .iter()
                .map(|x| x.get("kind").and_then(Value::as_str).unwrap_or("?").to_string())
                .collect::<Vec<_>>())
        ),
        "log_covers_from" => format!("{:?}", view::log_covers_from(h, &s(0))),
        // R2.S3d's three carriers (the reference runner's renderers, verbatim).
        "context_id" => format!("{:?}", view::context_id(h, &s(0))),
        "message_pages" => format!("{:?}", view::message_pages(h, &s(0))),
        "gauge_series" => format!("{:?}", view::gauge_series(h, &s(0))),
        "ledger_rows_after" => format!(
            "{:?}",
            ledger::rows_after(h, &s(0), u(1))
                .iter()
                .map(|r| (r.seq, r.kind.clone(), r.elided))
                .collect::<Vec<_>>()
        ),
        "ledger_floor" => format!("{:?}", ledger::floor(h, &s(0))),
        "ledger_capacity" => format!("{:?}", ledger::capacity(h)),
        "ledger_open" => format!("{:?}", ledger::open(h)),
        // narration
        "narration_len" => format!("{:?}", view::narration(h, &s(0), None).map(|l| l.len())),
        // commands and eras
        "settled" => format!("{:?}", view::settled(h, u(0))),
        "era" => format!("{:?}", h.era(&s(0))),
        // the books, which no `view::*` exposes and a conformance run must see
        "entry" => format!(
            "{:?}",
            h.book
                .get(&s(0), args.get(1).and_then(Value::as_str))
                .map(|e| e.status().clone())
        ),
        "rejections" => format!("{:?}", h.rejections),
        "keys_of" => format!("{:?}", h.book.keys_of(&s(0))),
        "mirror_keys" => format!(
            "{:?}",
            h.mirror(&s(0)).map(|m| m.keys()).unwrap_or_default()
        ),
        "mirror_dead" => format!("{:?}", h.mirror(&s(0)).map(|m| m.dead())),
        "mirror_refusal" => format!(
            "{:?}",
            h.mirror(&s(0)).and_then(|m| m.refusal()).map(str::to_string)
        ),
        "mirror_exports" => format!("{:?}", h.mirror(&s(0)).map(|m| m.exports())),
        "key_state" => format!(
            "{:?}",
            h.mirror(&s(0)).map(|m| m.key_state(&s(1)))
        ),
        "vanished" => format!(
            "{:?}",
            h.mirror(&s(0)).map(|m| m.vanished()).unwrap_or_default()
        ),
        other => panic!(
            "conformance: no renderer for `{other}`. Add one to `answer` rather \
             than dropping the expectation — a call with no renderer would make \
             that expectation silently unchecked."
        ),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Replay
// ─────────────────────────────────────────────────────────────────────────────

/// One transcript line. A closed set: an unrecognised key is a failure and not
/// a line to skip, because a skipped directive is a transcript that silently
/// stopped testing what it says it tests.
enum Line {
    Header(Value),
    Frame(Value),
    Reset(String),
    Hold { shape: String, key: Option<String> },
    Promise { id: u64, verb: String, subject: Option<String> },
    Expect { call: String, args: Vec<Value>, value: String },
    Blank,
}

fn parse_line(raw: &str, path: &Path, n: usize) -> Line {
    if raw.trim().is_empty() {
        return Line::Blank;
    }
    let v: Value = serde_json::from_str(raw)
        .unwrap_or_else(|e| panic!("{}:{n}: not json: {e}", path.display()));
    let obj = v.as_object().expect("a transcript line is an object");
    if let Some(h) = obj.get("transcript") {
        return Line::Header(h.clone());
    }
    if let Some(f) = obj.get("frame") {
        return Line::Frame(f.clone());
    }
    if let Some(r) = obj.get("reset") {
        return Line::Reset(r.as_str().unwrap_or("the socket died").to_string());
    }
    if let Some(hd) = obj.get("hold") {
        return Line::Hold {
            shape: hd["shape"].as_str().expect("hold.shape").to_string(),
            key: hd.get("key").and_then(Value::as_str).map(str::to_string),
        };
    }
    if let Some(p) = obj.get("promise") {
        return Line::Promise {
            id: p["id"].as_u64().expect("promise.id"),
            verb: p["verb"].as_str().unwrap_or("send").to_string(),
            subject: p.get("subject").and_then(Value::as_str).map(str::to_string),
        };
    }
    if let Some(e) = obj.get("expect") {
        return Line::Expect {
            call: e["call"].as_str().expect("expect.call").to_string(),
            args: e
                .get("args")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            value: e
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        };
    }
    panic!(
        "{}:{n}: a transcript line with no directive in it: {raw}",
        path.display()
    );
}

/// Replay one transcript. Answers the rewritten text when blessing, and the
/// mismatches otherwise.
fn replay(path: &Path, bless: bool) -> (Vec<String>, String, usize, usize) {
    // **BLESSING IS THE REFERENCE'S ACT AND NOT THIS FACE'S.** The expectations
    // are what the REFERENCE answers; a copy that could rewrite them could make
    // its own drift green, which is the one thing this file exists to stop.
    // `BLESS=1` belongs to `lib/shapewire/seam`'s own run.
    assert!(
        !bless,
        "BLESS=1 is not a consumer-side act: these expectations are the \
         reference's answers and only the reference may re-bless them. Run \
         `BLESS=1 cargo test --manifest-path lib/shapewire/seam/Cargo.toml` in the \
         chassis repo, review the diff, and vendor the result."
    );
    let text = std::fs::read_to_string(path).expect("the transcript");
    let mut held = Held::new();
    let mut out = String::new();
    let mut fails = Vec::new();
    let mut frames = 0usize;
    let mut checks = 0usize;

    // **THE LINK IS PUBLISHED UP BEFORE THE FIRST FRAME**, because that is what
    // a real session does and `view::link` is one of the locked answers. The
    // stamp is fixed rather than `Utc::now()`: a golden with a clock in it goes
    // red once a second.
    held.link = tui::seam::held::LinkState::Up {
        boot_id: "transcript".into(),
        since: "2026-10-07T00:00:00Z".parse().expect("a fixed stamp"),
    };

    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        match parse_line(raw, path, n) {
            Line::Blank => out.push('\n'),
            Line::Header(_) | Line::Frame(_) | Line::Reset(_) | Line::Hold { .. }
            | Line::Promise { .. } => {
                // Replayed below; the raw line is carried through untouched so a
                // bless run only ever rewrites `expect` lines.
                match parse_line(raw, path, n) {
                    Line::Frame(f) => {
                        held.fold(&f);
                        frames += 1;
                    }
                    Line::Reset(r) => held.reset(r),
                    Line::Hold { shape, key } => {
                        // `&'static str` is what the book wants and a transcript
                        // has a String; leak, because a test process is short and
                        // the alternative is a lifetime in the public Book API
                        // for the sake of a test.
                        let shape: &'static str = Box::leak(shape.into_boxed_str());
                        held.book.hold(shape, key.as_deref());
                    }
                    Line::Promise { id, verb, subject } => {
                        let verb: &'static str = Box::leak(verb.into_boxed_str());
                        held.promises.open(
                            id,
                            Pending {
                                verb,
                                subject,
                                asked_at: "2026-10-07T00:00:00Z".parse().unwrap(),
                                boot_id: "transcript".into(),
                            },
                        );
                    }
                    _ => {}
                }
                out.push_str(raw);
                out.push('\n');
            }
            Line::Expect { call, args, value } => {
                let got = answer(&held, &call, &args);
                checks += 1;
                // **NO EXEMPTION TABLE, BECAUSE THIS FACE MOVES NO ANSWER.** The
                // AUI's copy has one here — its D3 fence drops
                // `narration/progress`, so `entry("narration","progress")`
                // answers `None` where the transcript locks `Complete`. This
                // face's only fence is D2, two consts in `link.rs` that neither
                // `Held::fold` nor any `view::*` function reads, so every locked
                // answer is compared as written. `the_answer_the_auis_fence_moves_
                // is_not_moved_here` is the positive statement of that, and it is
                // why removing the table is not the same as skipping the check.
                if bless {
                    out.push_str(
                        &serde_json::to_string(&serde_json::json!({
                            "expect": {"call": call, "args": args, "value": got}
                        }))
                        .unwrap(),
                    );
                    out.push('\n');
                } else {
                    if got != value {
                        fails.push(format!(
                            "{}:{n}: {call}({}) \n     want: {value}\n      got: {got}",
                            path.display(),
                            args.iter()
                                .map(|a| a.to_string())
                                .collect::<Vec<_>>()
                                .join(", "),
                        ));
                    }
                    out.push_str(raw);
                    out.push('\n');
                }
            }
        }
    }
    (fails, out, frames, checks)
}

// ─────────────────────────────────────────────────────────────────────────────
//  The answer the OTHER face's fence moves — and this face does not
// ─────────────────────────────────────────────────────────────────────────────

/// **THE ONE LOCKED ANSWER THE AUI EXEMPTS, AND THIS FACE ANSWERS AS WRITTEN.**
///
/// `apps/aui/tests/seam_conformance.rs` carries a `FENCED_ANSWERS` table with
/// exactly one row in it: `fleet_standing.jsonl`'s
/// `entry("narration", "progress")`, which the reference locks to `Complete` and
/// the AUI answers `None`, because its `Book::standing` drops that channel —
/// there is no spoken form for a progress line and the audio law forbids
/// inventing one.
///
/// **This face has no such table, and an absent table is not evidence.** An
/// empty exemption list passes by default and says nothing; a deleted one says
/// nothing even louder. So the claim is made POSITIVELY instead: this test names
/// that exact transcript line, proves the lock is really there (the sentinel —
/// a row that silently stopped existing would otherwise take the whole check
/// with it), and asserts this face's own fold answers it.
///
/// It goes red the day somebody adds a channel filter to this copy's
/// `Book::standing`, which is the mutation `tests/it/seam_reference.rs` cannot see:
/// a filter written INSIDE the D3 fence is a legal text difference and this is
/// the only thing that notices it changed an answer.
const THE_AUIS_ONE_EXEMPTION: (&str, &str, &[&str]) =
    ("fleet_standing.jsonl", "entry", &["narration", "progress"]);

#[test]
fn the_answer_the_auis_fence_moves_is_not_moved_here() {
    let (file, call, args) = THE_AUIS_ONE_EXEMPTION;
    let path = transcripts()
        .into_iter()
        .find(|p| p.file_name().and_then(|f| f.to_str()) == Some(file))
        .unwrap_or_else(|| panic!("`{file}` is not a transcript"));
    let text = std::fs::read_to_string(&path).expect("the transcript");

    // ── THE SENTINEL ────────────────────────────────────────────────────────
    //
    // Find the locked line, and refuse to pass if it is not there. Without this
    // the test would be a loop over nothing the day the transcript is re-blessed
    // without that expectation — green, and measuring absolutely nothing.
    let mut locked: Option<String> = None;
    for raw in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        let Some(e) = v.get("expect") else { continue };
        if e["call"].as_str() != Some(call) {
            continue;
        }
        let a: Vec<&str> = e["args"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if a.as_slice() != args {
            continue;
        }
        locked = Some(e["value"].as_str().unwrap_or_default().to_string());
        break;
    }
    let locked = locked.unwrap_or_else(|| {
        panic!(
            "`{file}` locks no `{call}({args:?})`. That line IS this test — the              AUI's fence is declared against it — so its disappearance is a red              here rather than a quiet pass."
        )
    });
    assert!(
        locked.starts_with("Some("),
        "`{file}`'s `{call}({args:?})` locks `{locked}`, which is already the          AUI's answer — then this face and that one agree, the AUI's exemption          exempts nothing, and the two copies' `Book::standing` need re-reading"
    );

    // ── AND THIS FACE'S OWN FOLD ────────────────────────────────────────────
    let (fails, _, frames, checks) = replay(&path, false);
    assert!(
        checks > 0 && frames > 0,
        "`{file}` replayed {frames} frame(s) and {checks} check(s)"
    );
    assert!(
        fails.is_empty(),
        "this face answers `{call}({args:?})` differently from the reference's          `{locked}`, so its `Book::standing` has a channel filter the AUI's          fence was the only sanctioned place for:\n{}",
        fails.join("\n")
    );
    println!("the AUI exempts {file} {call}({args:?}); this face answers `{locked}`");
}

fn transcripts() -> Vec<PathBuf> {
    // The transcripts are VENDORED beside the crate, not copied into this face:
    // `vendor-chassis.sh` carries them, and the point of §7.3 is that both faces
    // replay the IDENTICAL files. A second copy under `apps/tui/tests/` would be
    // one more thing to keep in step.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("apps/tui is two levels under the repo root")
        .join("libs/shapewire/seam/conformance");
    let mut v: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    v.sort();
    v
}

// ─────────────────────────────────────────────────────────────────────────────
//  The run
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn every_transcript_folds_to_its_locked_answers() {
    let bless = std::env::var("BLESS").is_ok();
    let files = transcripts();
    assert!(
        !files.is_empty(),
        "conformance/ is empty. An empty golden suite is a suite that passes \
         everything, which is worse than not having one."
    );

    let mut total_frames = 0usize;
    let mut total_checks = 0usize;
    let mut report = Vec::new();
    for path in &files {
        let (fails, rewritten, frames, checks) = replay(path, bless);
        total_frames += frames;
        total_checks += checks;
        if bless {
            std::fs::write(path, rewritten).expect("write the blessed transcript");
            println!(
                "blessed {} ({frames} frames, {checks} expectations)",
                path.file_name().unwrap().to_string_lossy()
            );
            continue;
        }
        println!(
            "{}: {frames} frames, {checks} expectations",
            path.file_name().unwrap().to_string_lossy()
        );
        assert!(
            checks > 0,
            "{}: no expectations. A transcript that asserts nothing is a file \
             that folds frames and then throws the answer away.",
            path.display()
        );
        if !fails.is_empty() {
            report.push(format!(
                "── {} ──\n{}",
                path.display(),
                fails.join("\n")
            ));
        }
    }
    if bless {
        println!("BLESSED {} transcript(s); review the diff.", files.len());
        return;
    }
    assert!(
        report.is_empty(),
        "{} transcript(s) disagree with the reference:\n\n{}",
        report.len(),
        report.join("\n\n")
    );
    println!(
        "all {} transcript(s) green: {total_frames} frames, {total_checks} locked answers",
        files.len()
    );
}

/// At least one transcript has to come off the real producer, or the suite is
/// a set of beliefs about the wire rather than a record of it (§7.3).
#[test]
fn at_least_one_transcript_is_the_producers_own() {
    let mut live = 0;
    let mut hand = 0;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for path in transcripts() {
        let first = std::fs::read_to_string(&path)
            .expect("the transcript")
            .lines()
            .next()
            .unwrap_or_default()
            .to_string();
        let v: Value = serde_json::from_str(&first).unwrap_or_else(|e| {
            panic!(
                "{}: the first line must be the transcript header: {e}",
                path.display()
            )
        });
        let src = v["transcript"]["source"].as_str().unwrap_or_else(|| {
            panic!("{}: the header states no `source`", path.display())
        });
        *counts.entry(src.to_string()).or_default() += 1;
        match src {
            "live producer" => live += 1,
            "hand-written" => hand += 1,
            other => panic!(
                "{}: `source` is `{other}`; it must say whether the frames came \
                 off the producer or out of somebody's head",
                path.display()
            ),
        }
    }
    println!("transcript sources: {counts:?}");
    assert!(live > 0, "no transcript was recorded from the live producer");
    assert!(
        hand > 0,
        "no hand-written transcript, so the edge cases production will not \
         produce on demand — a vanishing, a re-init, a death, an unknown \
         event.kind, a re-offered promise — are untested"
    );
}
