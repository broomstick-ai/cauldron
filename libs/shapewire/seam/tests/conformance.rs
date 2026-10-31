//! **THE GOLDEN TRANSCRIPTS, REPLAYED THROUGH THE REAL `Held::fold`.**
//!
//! `docs/consumer-seam.md` §7.3: *"Each app's `tests/seam_conformance.rs`
//! replays every transcript through `Held::fold` and asserts the `view::*`
//! answers listed in the transcript's expectation block, byte for byte."* This
//! is that file, in the chassis repo, against the reference — and
//! `vendor-chassis.sh` carries the same transcripts into the consumer repo so
//! each face runs the identical replay against its own copy.
//!
//! **What this proves and what it does not.** It proves both copies FOLD and
//! READ identically for every scenario in `conformance/`. It does not prove the
//! link's reconnect ladder (that is `link.rs`'s own business, and `link.rs` is
//! byte-identical in both copies, which is what the reference diff is for) and
//! it does not prove the faces.
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

use seam_reference::held::{Held, Pending};
use seam_reference::{ledger, view};
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
        // R2.S3d's three carriers.
        "context_id" => format!("{:?}", view::context_id(h, &s(0))),
        "message_pages" => format!("{:?}", view::message_pages(h, &s(0))),
        "gauge_series" => format!("{:?}", view::gauge_series(h, &s(0))),
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
    held.link = seam_reference::held::LinkState::Up {
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

fn transcripts() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance");
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
