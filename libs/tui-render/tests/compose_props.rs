//! **LAYER 1 OF FOUR: PROPERTY TESTS OVER THE COMPOSER MODEL.**
//!
//! Random unicode crossed with random edit sequences, asserting the laws that
//! no single hand-written case can cover — because the cases that break a text
//! model are never the ones somebody thought to write down.
//!
//! The five laws, and what each one is protecting against:
//!
//! 1. **The cursor is always on an extended-grapheme-cluster boundary.** A
//!    cursor inside a cluster is a panic on the next slice, and it is how
//!    `Left`/`Right` end up inside a ZWJ emoji.
//! 2. **`undo ∘ redo` is identity and `redo ∘ undo` is identity.** An undo
//!    stack that is not an involution loses work, and it loses it silently.
//! 3. **The buffer is always valid UTF-8 and holds no tab, no C0 and no C1.**
//!    The buffer is what gets written back at a terminal; an escape byte in it
//!    is a CVE class, and a tab is the whole tab-stop glitch family.
//! 4. **Rendered width equals computed width.** Every laid-out row is at most
//!    as wide as the band it was laid out for, at EVERY width — which is the
//!    ratatui over-report panic (`0018`) stated as a property.
//! 5. **The rows tile their line.** Contiguous, exhaustive byte ranges, so
//!    every cursor position has exactly one screen cell and the caret is total.
//!
//! **Why a separate file rather than more unit tests.** The unit tests in
//! `compose.rs` are the NAMED cases — the ZWJ family, the ten-thousand-line
//! paste, the goal column through a short line. This file is the unnamed ones,
//! and the two are not substitutes: a property test that fails tells you the
//! law is broken, and a named case tells you which behaviour you lost.

use proptest::prelude::*;
use tui_render::compose::{cluster_width, snap, str_width, Composer, Pos};
use unicode_segmentation::UnicodeSegmentation;

/// The alphabet the generator draws from. Deliberately loaded: ordinary ASCII
/// so the sequences are mostly realistic, plus every shape that has ever broken
/// a terminal text model, plus the control characters that must never survive
/// the door.
const ALPHABET: &[&str] = &[
    "a",
    "b",
    " ",
    " ",
    "z",
    "Q",
    "0",
    "-",
    "/",
    ".",
    "\n",
    // the corpus
    "👩‍👩‍👧‍👦",                       // ZWJ family: one cluster, 25 bytes
    "👩‍💻",                       // ZWJ profession
    "👍🏽",                       // skin-tone modifier
    "🇯🇵",                       // regional-indicator flag
    "1️⃣",                       // keycap
    "❤️",                       // VS16 (emoji presentation)
    "❤︎",                        // VS15 (text presentation)
    "é",                        // precomposed
    "e\u{301}",                 // decomposed
    "e\u{301}\u{302}\u{303}",   // a pile of combining marks
    "\u{1100}\u{1161}\u{11a8}", // conjoining jamo
    "日",
    "本",
    "شسيب",     // RTL
    "\u{200b}", // ZWSP — kept, deliberately
    "\u{f09b}", // Nerd Font PUA
    // …and the things that must be stripped at the door
    "\t",
    "\u{0}",
    "\u{7}",
    "\u{1b}",
    "\r",
    "\u{feff}",
    "\u{85}", // C1 NEL
];

fn text() -> impl Strategy<Value = String> {
    proptest::collection::vec(proptest::sample::select(ALPHABET), 0..24).prop_map(|v| v.concat())
}

/// One thing that can be done to a composer. The `usize`/`bool` payloads are
/// generated too, so a sequence explores widths and directions rather than
/// repeating one.
#[derive(Debug, Clone)]
enum Act {
    Type(char),
    Paste(String),
    Newline,
    Backspace,
    Delete,
    Left,
    Right,
    WordLeft,
    WordRight,
    Up(u8),
    Down(u8),
    RowStart(u8),
    RowEnd(u8),
    BufStart,
    BufEnd,
    KillEnd,
    KillStart,
    KillWord,
    Yank,
    Undo,
    Redo,
    CloseRun,
}

fn act() -> impl Strategy<Value = Act> {
    prop_oneof![
        // Typing is weighted up so a sequence builds something to act on.
        8 => proptest::sample::select(ALPHABET)
            .prop_map(|s| Act::Type(s.chars().next().unwrap_or('x'))),
        3 => text().prop_map(Act::Paste),
        2 => Just(Act::Newline),
        3 => Just(Act::Backspace),
        2 => Just(Act::Delete),
        4 => Just(Act::Left),
        4 => Just(Act::Right),
        1 => Just(Act::WordLeft),
        1 => Just(Act::WordRight),
        2 => (1u8..40).prop_map(Act::Up),
        2 => (1u8..40).prop_map(Act::Down),
        1 => (1u8..40).prop_map(Act::RowStart),
        1 => (1u8..40).prop_map(Act::RowEnd),
        1 => Just(Act::BufStart),
        1 => Just(Act::BufEnd),
        1 => Just(Act::KillEnd),
        1 => Just(Act::KillStart),
        1 => Just(Act::KillWord),
        1 => Just(Act::Yank),
        2 => Just(Act::Undo),
        2 => Just(Act::Redo),
        1 => Just(Act::CloseRun),
    ]
}

fn apply(c: &mut Composer, a: &Act) {
    match a {
        Act::Type(ch) => c.insert_char(*ch),
        Act::Paste(s) => c.insert_paste(s),
        Act::Newline => c.newline(),
        Act::Backspace => c.backspace(),
        Act::Delete => c.delete(),
        Act::Left => c.left(),
        Act::Right => c.right(),
        Act::WordLeft => c.word_left(),
        Act::WordRight => c.word_right(),
        Act::Up(w) => {
            c.up(*w as usize);
        }
        Act::Down(w) => {
            c.down(*w as usize);
        }
        Act::RowStart(w) => c.row_start(*w as usize),
        Act::RowEnd(w) => c.row_end(*w as usize),
        Act::BufStart => c.buffer_start(),
        Act::BufEnd => c.buffer_end(),
        Act::KillEnd => c.kill_to_line_end(),
        Act::KillStart => c.kill_to_line_start(),
        Act::KillWord => c.kill_word_back(),
        Act::Yank => c.yank(),
        Act::Undo => {
            c.undo();
        }
        Act::Redo => {
            c.redo();
        }
        Act::CloseRun => c.close_run(),
    }
}

/// LAW 1 and LAW 3, asserted after every single act rather than at the end —
/// so the report names the act that broke it, not the tail of a sequence.
fn check_invariants(c: &Composer, after: &Act) {
    let Pos { line, byte } = c.cursor();
    assert!(
        line < c.lines().len(),
        "cursor line {line} past the buffer after {after:?}"
    );
    let s = &c.lines()[line];
    assert!(
        byte <= s.len(),
        "cursor byte {byte} past line {line} after {after:?}"
    );
    // LAW 1: on a cluster boundary, always.
    assert_eq!(
        snap(s, byte),
        byte,
        "cursor landed inside a grapheme cluster after {after:?}: line {s:?} byte {byte}"
    );
    // LAW 3: no tab, no C0, no C1, no BOM anywhere in the buffer.
    for l in c.lines() {
        for ch in l.chars() {
            let u = ch as u32;
            assert!(ch != '\t', "a tab reached the buffer after {after:?}");
            assert!(
                u >= 0x20,
                "a C0 control ({u:#x}) reached the buffer after {after:?}"
            );
            assert!(u != 0x7f, "DEL reached the buffer after {after:?}");
            assert!(
                !(0x80..=0x9f).contains(&u),
                "a C1 control ({u:#x}) reached the buffer after {after:?}"
            );
            assert!(u != 0xfeff, "a BOM reached the buffer after {after:?}");
        }
        assert!(
            !l.contains('\n'),
            "a newline survived inside a logical line"
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    /// LAWS 1 and 3, over random edit sequences.
    #[test]
    fn the_cursor_stays_on_a_boundary_and_the_buffer_stays_clean(
        acts in proptest::collection::vec(act(), 1..60)
    ) {
        let mut c = Composer::new();
        for a in &acts {
            apply(&mut c, a);
            check_invariants(&c, a);
        }
    }

    /// **LAW 2: `undo ∘ redo` is identity and `redo ∘ undo` is identity.**
    ///
    /// Asserted from an ARBITRARY point in the history, in both directions,
    /// because the failure everybody actually ships is at one specific depth.
    ///
    /// **What "identity" means here, stated rather than assumed: the TEXT and
    /// the CURSOR.** It deliberately does NOT include the open-edit-run
    /// bookkeeping, because an undo CLOSES the run by design — typing `abc`,
    /// undoing and redoing must not leave a run open that the next keystroke
    /// would then join, which would make the redone text and the new keystroke
    /// one undo step. Comparing the whole struct would pin that bug in place as
    /// though it were the law.
    #[test]
    fn undo_and_redo_are_mutual_inverses_at_every_depth(
        acts in proptest::collection::vec(act(), 1..40)
    ) {
        let mut c = Composer::new();
        for a in &acts {
            apply(&mut c, a);
        }
        let seen = |c: &Composer| (c.text(), c.cursor(), c.pending().to_vec());
        // undo THEN redo returns the exact state.
        for _ in 0..acts.len() {
            let before = seen(&c);
            if !c.undo() {
                break;
            }
            prop_assert!(c.redo(), "undo left nothing to redo");
            prop_assert_eq!(seen(&c), before, "undo∘redo is not identity");
            prop_assert!(c.undo(), "the state moved under a proven-inverse pair");
        }
        // …and from the other end, redo THEN undo.
        for _ in 0..acts.len() {
            let before = seen(&c);
            if !c.redo() {
                break;
            }
            prop_assert!(c.undo(), "redo left nothing to undo");
            prop_assert_eq!(seen(&c), before, "redo∘undo is not identity");
            prop_assert!(c.redo(), "the state moved under a proven-inverse pair");
        }
    }

    /// **LAW 4 and LAW 5, at every width from 1 to 40.**
    ///
    /// The widths that break are never the ones a single case picked, which is
    /// why this sweeps rather than samples — 0018 measured that an over-reported
    /// line is a PANIC in this exact stack, not a cosmetic slip.
    #[test]
    fn every_row_fits_its_band_and_the_rows_tile_their_line(
        acts in proptest::collection::vec(act(), 1..40)
    ) {
        let mut c = Composer::new();
        for a in &acts {
            apply(&mut c, a);
        }
        for w in 1..=40usize {
            let rows = c.layout(w);
            prop_assert!(!rows.is_empty(), "no rows at width {}", w);
            prop_assert_eq!(
                rows.iter().filter(|r| r.start == 0).count(),
                c.lines().len(),
                "one first-row per logical line at width {}", w
            );
            let mut prev: Option<tui_render::compose::VisualRow> = None;
            for r in &rows {
                // LAW 5: contiguous and exhaustive within a line.
                if let Some(p) = prev {
                    if p.line == r.line {
                        prop_assert_eq!(p.end, r.start, "rows do not tile at width {}", w);
                    } else {
                        prop_assert_eq!(
                            p.end, c.lines()[p.line].len(),
                            "a line's last row stops short at width {}", w
                        );
                        prop_assert_eq!(r.start, 0, "a line's first row does not start at 0");
                    }
                }
                prev = Some(*r);
                // LAW 4: no row over-reports its band. The one exception is the
                // medium's, not ours — a two-cell cluster in a one-cell band.
                let got = str_width(c.row_text(*r));
                prop_assert!(
                    got <= w.max(2),
                    "a row measured {} in a band of {}: {:?}", got, w, c.row_text(*r)
                );
                // Every row starts and ends on a cluster boundary, or the
                // renderer slices through a codepoint.
                let line = &c.lines()[r.line];
                prop_assert_eq!(snap(line, r.start), r.start, "row start is not a boundary");
                prop_assert_eq!(snap(line, r.end), r.end, "row end is not a boundary");
            }
            prop_assert_eq!(
                prev.map(|p| p.end),
                Some(c.lines()[c.lines().len() - 1].len()),
                "the last row stops short at width {}", w
            );
            // The caret lands on a row that exists, at a column inside the band.
            let (row, col) = c.caret(w);
            prop_assert!(row < rows.len(), "caret row {} past {} rows", row, rows.len());
            prop_assert!(col <= w.max(2), "caret column {} past band {}", col, w);
        }
    }

    /// **THE SUM OF THE CLUSTER WIDTHS OF A ROW IS THE ROW'S MEASURED WIDTH.**
    ///
    /// The research states it as "rendered width always equals computed width".
    /// It is not tautological: it fails the moment anything measures by `char`
    /// instead of by cluster, which is exactly how a ZWJ emoji over-reports.
    #[test]
    fn a_rows_measured_width_is_the_sum_of_its_clusters(s in text()) {
        let c = Composer::from_text(&s);
        for w in 1..=32usize {
            for r in c.layout(w) {
                let t = c.row_text(r);
                let summed: usize = t.graphemes(true).map(cluster_width).sum();
                prop_assert_eq!(summed, str_width(t), "two ways to measure {:?} disagree", t);
            }
        }
    }

    /// Whatever went in comes back out — modulo the door's normalization, which
    /// is stated rather than assumed: tabs became spaces, CRs became newlines,
    /// controls went.
    #[test]
    fn a_paste_round_trips_through_the_door(s in text()) {
        let mut c = Composer::new();
        c.insert_paste(&s);
        let want = tui_render::compose::sanitize(&s);
        prop_assert_eq!(c.text(), want);
        // …and it is ONE undo step, whatever was in it.
        if !s.is_empty() {
            prop_assert!(c.undo());
            prop_assert_eq!(c.text(), "");
        }
    }

    /// Motion is reversible: `→` then `←` returns to where it started, from
    /// every position in the buffer. This is the property that catches a motion
    /// that skips a zero-width cluster in one direction and not the other.
    #[test]
    fn right_then_left_returns_to_the_same_position(s in text()) {
        let mut c = Composer::from_text(&s);
        c.buffer_start();
        loop {
            let here = c.cursor();
            c.right();
            if c.cursor() == here {
                break;
            }
            let there = c.cursor();
            c.left();
            prop_assert_eq!(c.cursor(), here, "← did not undo → at {:?}", there);
            c.right();
        }
    }
}
