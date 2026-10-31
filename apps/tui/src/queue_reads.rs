//! # **THE QUEUE PANEL, AND IT ASKS THE PRODUCER NOTHING.**
//!
//! > *"I only want to use the shapes. **ONLY**."*
//!
//! **This module is no longer an exception to that.** It was written as one —
//! two reads that no shape answered, parked in a file a reader would trip over
//! rather than buried in a 1,500-line event loop — and the header that stood
//! here described that state long after it had ended.
//!
//! ## What is actually true now, measured
//!
//! `threads/{id}/queue` and `threads/{id}/queue_failed` answer *which messages
//! are waiting* and *which ones failed*: the queue's CONTENTS. **A declared
//! cell carries that list.** The `threads` shape carries BOTH halves of the
//! queue story:
//!
//! | cell on `threads` | what it carries | declared at |
//! |---|---|---|
//! | `queued` | the COUNT, which [`crate::mirror::status_row`] projects onto the row | `seam::inventory::threads::QUEUED` |
//! | `queue` | the LIST, `list<struct:QueueEntry>` | `seam::inventory::threads::QUEUE` |
//!
//! Both are in `libs/shapewire/inventory.json`. There is no separate
//! `queue_failed` cell and there does not need to be: the one `queue` cell
//! holds waiting and failed rows together, each carrying its own `state`, and
//! [`crate::seam::view::queue_failed`] is a filter over
//! [`crate::seam::view::queue`] rather than a second read.
//!
//! **THE HEADER THAT STOOD HERE CLAIMED THE OPPOSITE** — *"the producer's
//! seven shapes carry a queue COUNT and nothing else … and
//! `libs/shapewire/inventory.json` lists no cell on any shape holding either
//! list"* — and that was false, not stale: the cell it says does not exist is
//! declared, is in the vendored inventory, and is what this module's two calls
//! have been reading. R2.S2b replaced the claim with the measurement.
//!
//! ## So what are the two calls still doing here
//!
//! **Carrying a reply envelope, and nothing else.**
//! [`crate::mirror::Handle::enveloped_queue_read`] holds the thread's `threads`
//! row, reads the one `queue` cell off the mirror, and wraps the rows in the
//! legacy key this module already destructures — `queue` becomes
//! `{"waiting": [...]}` and `queue_failed` becomes `{"failed": [...]}`.
//! **No request leaves the face for either leaf.** The producer does not
//! dispatch either method any more, so the leaf names select a reply shape and
//! address nothing.
//!
//! The envelope is kept rather than inlined because `arr()` below and the
//! `Ev::Queue` payload are the panel's call shape, and changing a reply key is
//! not what landing a cell was for. Collapsing it is a later, separate pass.
//!
//! ## And the pin at two stays, with a narrower claim
//!
//! [`crate::mirror::THE_ENVELOPED_QUEUE_READS`] is the list, and
//! `the_only_enveloped_queue_reads_are_these_two` fails if it grows. That guard
//! is still worth having and it now guards something smaller and true: a third
//! entry would be a third legacy reply key, not a third read escaping the
//! shapes. Both calls stay HERE rather than in `seam/commands.rs`, where
//! `methods::TEMPLATES` is commands-only and `seamdiff --inventory` routes it,
//! and not behind a generic `call(&method, params)` door — which is still how a
//! real read would arrive unnoticed.

use serde_json::Value;
use tokio::sync::mpsc;

use crate::event::Ev;
use crate::mirror::Handle;

/// **BOTH HALVES OF THE QUEUE, IN ONE ROUND, OFF ONE CELL.** Neither call
/// reaches the producer — see the module header. The queue MUTATIONS
/// (`queue_cancel`, `queue_reselect`) really are methods, are in the producer's
/// table, and are called nowhere in this crate.
pub fn spawn_queue(h: Handle, tx: mpsc::Sender<Ev>, thread_id: String) {
    tokio::spawn(async move {
        let result = async {
            // NO `thread_id` IN THE PARAMS. It is the SUBJECT of both paths and
            // the daemon REFUSES a call that spells it twice — the same refusal
            // whether the two agree or disagree, because a precedence rule
            // would be a place for them to disagree.
            let waiting = h.enveloped_queue_read("queue", &thread_id).await?;
            let failed = h.enveloped_queue_read("queue_failed", &thread_id).await?;
            Ok::<_, String>((arr(&waiting, "waiting"), arr(&failed, "failed")))
        }
        .await;
        let _ = tx.send(Ev::Queue { thread_id, result }).await;
    });
}

fn arr(v: &Value, key: &str) -> Vec<Value> {
    v.get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    /// **THE CARVE-OUT IS TWO, AND A THIRD IS A RED.**
    ///
    /// The positive half first, because it is the half that cannot go quiet: a
    /// renamed or deleted leaf makes the list differ and this fails loudly. The
    /// old `reads.rs` guard was seven NEGATIVE claims over a spelling, and when
    /// the daemon re-pathed itself every needle stopped existing anywhere in the
    /// crate, so every assertion became trivially true and the test went on
    /// passing. That is the asymmetry this file is written against.
    #[test]
    fn the_only_enveloped_queue_reads_are_these_two() {
        assert_eq!(
            crate::mirror::THE_ENVELOPED_QUEUE_READS,
            ["queue", "queue_failed"],
            "the queue-read envelope grew. A third legacy reply key needs a \
             reason written down, not a line here"
        );
    }

    /// **AND THIS MODULE IS THE ONLY PLACE EITHER OF THEM IS NAMED.**
    ///
    /// The source scan, with its own positive sentinel: an extractor that
    /// silently matched nothing would make every assertion below vacuous at
    /// once, so it must first FIND the two leaves in this file before it is
    /// believed about their absence elsewhere.
    #[test]
    fn no_other_module_names_a_daemon_method() {
        let here = include_str!("queue_reads.rs");
        // ── THE SENTINEL ────────────────────────────────────────────────────
        for leaf in crate::mirror::THE_ENVELOPED_QUEUE_READS {
            assert!(
                here.contains(&format!("\"{leaf}\"")),
                "`{leaf}` is not named in this file, so the scan below is \
                 looking for something that is not here and cannot fail"
            );
        }
        // ── AND THE CLAIM ───────────────────────────────────────────────────
        //
        // A method PATH is what is banned, in both grammars: `"threads/…"` is
        // the current spelling and `"threads.…"` is the pre-2026-09-28 one. The
        // second selector is the one a migration's own guard forgets, and it is
        // blind to exactly the regression it was written for — a line REVERTED.
        for (file, whole) in [
            ("app.rs", include_str!("app.rs")),
            ("acts.rs", include_str!("acts.rs")),
            ("sends.rs", include_str!("sends.rs")),
            ("store.rs", include_str!("store.rs")),
            ("queue_reads.rs", here),
        ] {
            // **CODE ONLY, and the strip is load-bearing rather than tidy.**
            // Three of these files NAME a banned method in prose so the reason
            // for the ban can be written beside it — `acts.rs` explains at
            // length why `threads/{id}/update` must not be sent — and a scan
            // that read those sentences would force every one of them to be
            // deleted, which is the comment-versus-code defect this crate has
            // already had to repair once. A TRAILING comment on a code line is
            // deliberately still in scope, so the strip cannot be used as a
            // hiding place.
            // **AND THE TEST MODULES ARE CUT OFF FIRST, WHICH IS THE
            // SELF-WITNESSING CASE NAMED.** `acts.rs` and `sends.rs` carry the
            // SAME two needles as string literals in their own guards, so a
            // scan that read them would fail on the existence of the checks
            // rather than on anything shipped. `seamdiff`'s engine has the
            // identical problem and the identical answer, written down as
            // `SELF_EXCLUDED`: a source-scanning guard that reads its own
            // source is not measuring what it claims to.
            let whole = whole.split_once("#[cfg(test)]").map_or(whole, |(c, _)| c);
            let src: String = whole
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            let src = src.as_str();
            // ── THE SECOND SENTINEL, AND IT IS THE ONE THAT WAS MISSING ─────
            //
            // **The sentinel above reads `here`; this claim scans `src`. Those
            // are two different strings, and the gap between them is a way for
            // the scan to go vacuous that the first sentinel cannot see.**
            // `split_once("#[cfg(test)]")` cuts on the FIRST occurrence of that
            // LITERAL — a doc comment that merely MENTIONS it in prose cuts
            // there instead of at the real test module, and every line of real
            // code after it is silently dropped from `src` while `here` still
            // contains everything, so the first sentinel still passes.
            //
            // **Measured, R2.S2b:** a banned method literal added as real code
            // fires this test; the identical line with one prose mention of
            // `#[cfg(test)]` above it passes GREEN. Latent today — in all five
            // files the first match is the genuine attribute on its own line —
            // and closed here rather than left as a note, because the whole
            // point of the sentinel above was that this scan must not be able
            // to look at nothing.
            //
            // Tied to `queue_reads.rs` because its own two leaf literals are
            // the one thing this file is GUARANTEED to contain as code above
            // its test module.
            if file == "queue_reads.rs" {
                for leaf in crate::mirror::THE_ENVELOPED_QUEUE_READS {
                    assert!(
                        src.contains(&format!("\"{leaf}\"")),
                        "the STRIPPED source of `queue_reads.rs` no longer \
                         contains `{leaf}`, so the scan below is reading a \
                         truncated file and cannot fail. The usual cause is a \
                         `#[cfg(test)]` written in prose above the real test \
                         module, which moves where `split_once` cuts"
                    );
                }
            }
            for needle in ["\"threads/", "\"threads."] {
                for (i, _) in src.match_indices(needle) {
                    let rest = &src[i + 1..];
                    let lit = &rest[..rest.find('"').unwrap_or(0)];
                    // `threads.json` is a FILE and `src/index.rs` reads it;
                    // named here so a reader learns the selector is
                    // deliberately shape-based rather than wondering.
                    assert!(
                        lit == "threads.json",
                        "`{file}` contains the method literal `{lit}`. Every \
                         method this face speaks is a `seam::commands` verb or \
                         one of the two reads `queue_reads` owns"
                    );
                }
            }
        }
    }
}
