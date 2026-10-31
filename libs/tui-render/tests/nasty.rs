//! **LAYER 2 OF FOUR: THE NASTY CORPUS.**
//!
//! Every shape that has ever broken a terminal text model, driven through the
//! composer and through the frame, with the answers RECORDED rather than
//! computed at test time.
//!
//! # Why the widths are literals
//!
//! **Each expectation is a CONTRACT WITH `unicode-width`, and a version bump
//! that changes one must re-green it by hand.** That is the point: the width
//! table is a dependency whose answers move between releases, and when they
//! move the composer's wrap moves with them. A test that asked the table what
//! the table says would pass through any such change in silence; a literal
//! turns it into a conversation. `rustyline` ships a per-terminal guess table
//! whose own unit tests encode the disagreement numerically — the same family
//! emoji at 2, 8 or 16 columns depending on who is asked — which is what this
//! kind of drift looks like when nobody pinned it.
//!
//! # What is here that is not a width
//!
//! * **Escape sequences inside a paste** — the defect-2 corpus entry. The
//!   composer must hold no ESC, no NUL, no BEL, no CR and no tab, whatever was
//!   on the clipboard, because a buffer is what gets written back at a terminal
//!   and an escape byte in one is a CVE class of its own.
//! * **A 10,000-line paste** — one insert, one undo step, and a band that does
//!   not try to be 10,000 rows tall.
//! * **RTL and a Nerd Font PUA glyph**, which are the two the golden frame is
//!   most likely to mis-size.
//!
//! Every case also asserts the FRAME's postcondition — exactly `height` lines
//! of exactly `width` columns — because the thing an over-reported line does in
//! this stack is not look wrong, it is PANIC (`0018`).

use chrono::{TimeZone, Utc};
use tui_render::compose::{cluster_width, str_width, Composer as Buf};
use tui_render::geom::text_width;
use tui_render::model::Composer;
use tui_render::{frame, Mode, Region, ViewState};

/// **THE CORPUS, AND THE WIDTH EACH CLUSTER IS CONTRACTED TO HAVE.**
///
/// `clusters` is how many extended grapheme clusters the string is — the number
/// of times `→` must be pressed to cross it — and `cols` is its measured
/// display width.
const CORPUS: &[(&str, &str, usize, usize)] = &[
    // name, text, clusters, columns
    ("zwj family", "👩‍👩‍👧‍👦", 1, 2),
    ("zwj profession", "👩‍💻", 1, 2),
    ("skin tone", "👍🏽", 1, 2),
    ("regional flag", "🇯🇵", 1, 2),
    (
        "tag-sequence flag",
        "\u{1f3f4}\u{e0067}\u{e0062}\u{e0073}\u{e0063}\u{e0074}\u{e007f}",
        1,
        2,
    ),
    ("keycap", "1️⃣", 1, 2),
    ("vs16 emoji presentation", "❤️", 1, 2),
    ("vs15 text presentation", "❤︎", 1, 1),
    ("precomposed", "é", 1, 1),
    ("decomposed", "e\u{301}", 1, 1),
    ("three combining marks", "e\u{301}\u{302}\u{303}", 1, 1),
    ("conjoining jamo", "\u{1100}\u{1161}\u{11a8}", 1, 2),
    ("cjk", "日本語", 3, 6),
    ("rtl arabic", "شسيب", 4, 4),
    ("zero-width space", "a\u{200b}b", 3, 3),
    ("nerd font pua", "\u{f09b}", 1, 1),
    ("mixed", "a日👩‍👩‍👧‍👦b", 4, 6),
];

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 14, 19, 30, 0).unwrap()
}

/// The frame's postcondition. Over-report is a PANIC in this stack, not a
/// cosmetic slip, so it is asserted on every case here as well as in `golden`.
#[track_caller]
fn assert_shape(vs: &ViewState, w: u16, h: u16) {
    let lines = frame(vs, now(), w, h);
    assert_eq!(lines.len(), h as usize, "line count at {w}x{h}");
    for (i, line) in lines.iter().enumerate() {
        let got: usize = line.spans.iter().map(|s| text_width(&s.content)).sum();
        assert_eq!(got, w as usize, "line {i} width at {w}x{h}: {line:?}");
    }
}

fn thread_with(buf: Buf) -> ViewState {
    ViewState {
        mode: Mode::Thread(Region::Composer),
        composer: Composer {
            buf,
            editable: true,
            ..Composer::default()
        },
        ..ViewState::default()
    }
}

/// **THE CONTRACT WITH THE WIDTH TABLE, IN ONE PLACE.**
#[test]
fn every_corpus_entry_measures_what_it_is_recorded_as() {
    for (name, text, clusters, cols) in CORPUS {
        let c = Buf::from_text(text);
        // Cluster count, via the only thing that can count them: walking the
        // cursor. This is the number that decides how many times `→` has to be
        // pressed, which is what a person actually experiences.
        let mut n = 0usize;
        let mut cur = Buf::from_text(text);
        cur.buffer_start();
        loop {
            let before = cur.cursor();
            cur.right();
            if cur.cursor() == before {
                break;
            }
            n += 1;
        }
        assert_eq!(
            n, *clusters,
            "{name}: {n} cursor stops, recorded as {clusters}"
        );
        assert_eq!(
            str_width(&c.text()),
            *cols,
            "{name}: measured {} columns, recorded as {cols}. This is a CONTRACT \
             with unicode-width — re-measure against the terminal before re-greening.",
            str_width(&c.text())
        );
    }
}

/// **NO CLUSTER IS EVER MEASURED WIDER THAN A CELL PAIR.** The guard that stops
/// a width-table regression from becoming a ratatui over-report panic.
#[test]
fn no_single_cluster_exceeds_two_columns() {
    for (name, text, clusters, _) in CORPUS {
        if *clusters != 1 {
            continue;
        }
        let w = cluster_width(text);
        assert!(
            (1..=2).contains(&w),
            "{name}: one glyph measured {w} cells — a terminal has no third case"
        );
    }
}

/// **THE FRAME SURVIVES EVERY ONE OF THEM, AT EVERY WIDTH THAT MATTERS.**
///
/// 40 is the minimum the face draws at, 41 is one past it, 80 is the collapse
/// threshold, 81 is one past THAT, and 100/150 are his real windows. The widths
/// that break are never the ones a single case picked.
#[test]
fn the_shape_law_holds_with_every_nasty_cluster_in_the_composer() {
    for (name, text, ..) in CORPUS {
        // A line of the cluster repeated, so it has to wrap and has to be
        // broken mid-run at some width.
        for body in [
            text.to_string(),
            text.repeat(40),
            format!("ok {} ok", text.repeat(12)),
        ] {
            let vs = thread_with(Buf::from_text(&body));
            for w in [40u16, 41, 47, 79, 80, 81, 100, 150] {
                for h in [8u16, 12, 24, 40] {
                    assert_shape(&vs, w, h);
                }
            }
        }
        // …and with the caret walked all the way through it, because the caret
        // is a RESTYLE of a drawn row and a restyle that changed the width
        // would be the same panic wearing a different coat.
        let mut c = Buf::from_text(&text.repeat(30));
        c.buffer_start();
        loop {
            let vs = thread_with(c.clone());
            assert_shape(&vs, 47, 12);
            assert_shape(&vs, 100, 24);
            let before = c.cursor();
            c.right();
            if c.cursor() == before {
                break;
            }
        }
        let _ = name;
    }
}

/// **DEFECT 2's CORPUS ENTRY: a paste full of escape sequences, and the buffer
/// holds none of them.**
///
/// The bytes a pty probe produced are all here — the paste terminator itself,
/// NUL, BEL, a CSI introducer, CRLF and a tab. Not one may survive into the
/// buffer, because the buffer is what gets written back at a terminal.
#[test]
fn a_paste_full_of_escape_sequences_leaves_none_of_them_in_the_buffer() {
    let hostile =
        "hello\x1b[201~\x1b[200~ rm -rf\r\nline2\ttabbed\u{0}nul\u{7}bel\u{feff}bom\u{85}nel\r";
    let mut c = Buf::new();
    c.insert_paste(hostile);
    let text = c.text();
    for (what, needle) in [
        ("ESC", "\u{1b}"),
        ("NUL", "\u{0}"),
        ("BEL", "\u{7}"),
        ("CR", "\r"),
        ("TAB", "\t"),
        ("BOM", "\u{feff}"),
        ("C1 NEL", "\u{85}"),
    ] {
        assert!(
            !text.contains(needle),
            "{what} survived the paste: {text:?}"
        );
    }
    // The TEXT is kept — the point is to strip the dangerous bytes, not the
    // words. Losing his clipboard silently would be its own defect.
    for word in [
        "hello", "rm -rf", "line2", "tabbed", "nul", "bel", "bom", "nel",
    ] {
        assert!(
            text.contains(word),
            "{word:?} was lost from the paste: {text:?}"
        );
    }
    // CRLF became a real line break, and the tab became spaces.
    assert!(
        text.lines().count() >= 2,
        "CRLF did not become a line break: {text:?}"
    );
    assert!(text.contains("    "), "the tab did not expand: {text:?}");
    // ONE undo step, whatever was in it.
    assert!(c.undo());
    assert_eq!(c.text(), "");
    // …and the frame draws it without over-reporting a single row.
    c.redo();
    assert_shape(&thread_with(c), 80, 20);
}

/// **A 10,000-LINE PASTE: one insert, one undo, and a band that stays a band.**
#[test]
fn a_ten_thousand_line_paste_is_one_step_and_does_not_grow_the_band() {
    let big: String = (0..10_000)
        .map(|i| format!("line {i} of the paste"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut c = Buf::new();
    c.insert_paste(&big);
    assert_eq!(c.lines().len(), 10_000);

    let vs = thread_with(c.clone());
    // The band is capped at COMPOSER_MAX_ROWS whatever is in it, and the frame
    // is still exactly its height. A composer that tried to be 10,000 rows tall
    // is a face with no transcript.
    let b = tui_render::bands(&vs, 100, 30);
    assert!(
        b.composer <= tui_render::COMPOSER_MAX_ROWS as usize,
        "the band grew to {} rows",
        b.composer
    );
    assert!(b.content >= 1, "the transcript was squeezed out entirely");
    assert_shape(&vs, 100, 30);
    // …and the caret is on the LAST line, so the window shows the end of the
    // paste rather than the start of it.
    assert_eq!(c.cursor().line, 9_999);
    assert!(c.undo(), "nothing to undo after a 10,000-line paste");
    assert_eq!(c.text(), "");
}

/// **A DRAG SELECTION AND A CARET AGREE WITH THE SCREEN over a wide glyph.**
/// A column inside a double-width cluster resolves to that cluster whole; there
/// is no half-cell to land on and no way to reverse half a glyph.
#[test]
fn the_caret_never_lands_inside_a_double_width_cluster() {
    let mut c = Buf::from_text("ab日本語cd👩‍👩‍👧‍👦ef");
    c.buffer_start();
    let mut seen = Vec::new();
    loop {
        for w in [10usize, 20, 40] {
            let (_, col) = c.caret(w);
            // **THE CARET'S COLUMN IS THE SUM OF THE CLUSTER WIDTHS BEFORE
            // IT**, which is what makes it impossible for it to land inside a
            // wide glyph: it is computed by ADDING whole clusters, never by
            // dividing a string. Re-deriving it here from the row's own text
            // has to give the same number, and a per-`char` measurement would
            // not — which is the drift this asserts against.
            let rows = c.layout(w);
            let (r, _) = (c.caret(w).0, ());
            let row = rows[r];
            let text = c.row_text(row);
            let upto = c.cursor().byte.saturating_sub(row.start).min(text.len());
            assert_eq!(
                col,
                str_width(&text[..upto]),
                "the caret column disagrees with the row it is on"
            );
            assert!(col <= w.max(2));
        }
        seen.push(c.cursor().byte);
        let before = c.cursor();
        c.right();
        if c.cursor() == before {
            break;
        }
    }
    // Ten clusters: a, b, 日, 本, 語, c, d, family, e, f — plus the end.
    assert_eq!(seen.len(), 11, "cursor stops: {seen:?}");
}
