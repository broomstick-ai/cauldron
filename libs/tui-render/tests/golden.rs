//! MECHANISM 3 of the purity gate, and the one that makes the other two
//! load-bearing.
//!
//! A fixed `ViewState`, a fixed `now`, a fixed width and height, asserted
//! against exact expected lines. The dependency set and `clippy.toml` each
//! close a specific door; this closes the general case. With a real clock
//! anywhere inside the crate, no two runs agree — so this test fails the
//! moment the purity obligation is broken **even if the clock arrives by a
//! route the lint list never named**.
//!
//! Which is also why a green here that nobody has forced to fail is not
//! evidence. `../conformance_test.sh` perturbs the model and asserts that this
//! file goes red with a readable diff before it will report success.
//!
//! **Two postconditions, not one.** [`assert_shape`] is the frame's: exactly
//! `height` lines of exactly `width` columns. [`assert_block_height`] is the
//! LAYOUT's: a given block occupies exactly the rows it should at a given
//! width. The first would pass on a frame that laid out every block on one
//! line; the second is what makes the wrapping arithmetic checkable, and it is
//! checked at 100, 80 and 48 columns because the widths that break are never
//! the ones a single case picked.

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use ratatui::style::{Color, Modifier};
use tui_render::geom::text_width;
use tui_render::model::{
    ArchivedRow, ArchivedView, Avail, Facts, ProjectFacts, SheetAct, SheetFacts, SheetRow,
    SheetView,
};
use tui_render::model::{
    Banner, Block, BlockId, Composer, Fetch, Header, Known, Link, ProjectList,
    ProjectRow, QueueItem, QueueView, Sender, ThreadList, ThreadRow, Viewport,
};
use tui_render::{
    activation, frame, frame_with_hits, intent, palette, AgentTree, Intent, Key, LinkState, Mode,
    Notice, Region, Severity, StandIn, SubagentState, Target, TreeId, TreeRow, Via, ViewState,
    GUTTER,
};

const W: u16 = 100;
const H: u16 = 24;

/// The fixed clock. A literal, threaded in as a parameter — there is no
/// `Utc::now()` in this file and there is none in the crate under test.
fn at(h: u32, m: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 14, h, m, s).unwrap()
}

fn now() -> DateTime<Utc> {
    at(19, 30, 0)
}

fn text(vs: &ViewState, w: u16, h: u16) -> Vec<String> {
    text_at(vs, now(), w, h)
}

/// **THE SAME FRAME, DRAWN AT A CLOCK OF MY CHOOSING.** The whole proof that a
/// duration on this face is live rather than remembered is two calls to this
/// with ONE unchanged `ViewState` and two different `now`s — no sleeping, no
/// wall clock, and nothing between the two but time.
fn text_at(vs: &ViewState, at: DateTime<Utc>, w: u16, h: u16) -> Vec<String> {
    frame(vs, at, w, h)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

/// **THE CHARACTERS THE FRAME DREW IN REVERSE VIDEO**, concatenated in screen
/// order.
///
/// The highlight is the only thing on screen that says what a drag has taken,
/// and it is a STYLE rather than a character — so a golden test that compares
/// text alone is blind to it. This is what makes it visible to an assertion.
fn reversed(vs: &ViewState, w: u16, h: u16) -> String {
    use ratatui::style::Modifier;
    frame(vs, now(), w, h)
        .iter()
        .flat_map(|line| {
            line.spans
                .iter()
                .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
                .map(|s| s.content.as_ref())
        })
        .collect()
}

/// Exact-line assertion with a diff that says which line and what changed. On
/// failure it also prints the actual block as a Rust literal, because a golden
/// test whose output you cannot re-adopt in one paste is a golden test people
/// start deleting.
#[track_caller]
fn golden(actual: &[String], expected: &[&str]) {
    let same = actual.len() == expected.len()
        && actual.iter().zip(expected).all(|(a, e)| a.as_str() == *e);
    if same {
        return;
    }
    let mut out = String::new();
    out.push_str(&format!(
        "\ngolden mismatch: {} actual lines, {} expected\n",
        actual.len(),
        expected.len()
    ));
    let n = actual.len().max(expected.len());
    for i in 0..n {
        let a = actual.get(i).map(|s| s.as_str());
        let e = expected.get(i).copied();
        match (a, e) {
            (Some(a), Some(e)) if a == e => out.push_str(&format!("  {i:>3} | {a}\n")),
            (a, e) => {
                out.push_str(&format!("- {:>3} | {}\n", i, e.unwrap_or("<missing>")));
                out.push_str(&format!("+ {:>3} | {}\n", i, a.unwrap_or("<missing>")));
            }
        }
    }
    out.push_str("\nactual, as a literal:\n&[\n");
    for line in actual {
        out.push_str(&format!("    {:?},\n", line));
    }
    out.push_str("]\n");
    panic!("{out}");
}

/// The frame's postcondition: EXACTLY `h` lines of EXACTLY `w` columns.
/// Nothing this crate emits can wrap, because there is no slack for it to wrap
/// into — and `0018` measured what happens if it tries: ratatui 0.28 panics
/// rather than wrapping.
#[track_caller]
fn assert_shape(vs: &ViewState, w: u16, h: u16) {
    let lines = frame(vs, now(), w, h);
    assert_eq!(lines.len(), h as usize, "line count at {w}x{h}");
    for (i, line) in lines.iter().enumerate() {
        let got: usize = line.spans.iter().map(|s| text_width(&s.content)).sum();
        assert_eq!(got, w as usize, "line {i} width at {w}x{h}: {:?}", line);
    }
}

/// The LAYOUT's postcondition: one block, alone in a frame, occupies exactly
/// `rows` rows. The frame is made tall enough that nothing is cropped, and the
/// blank rows the band pads with are subtracted — so this counts the block, not
/// the band.
///
/// This is the assertion `assert_shape` cannot make. A renderer that laid every
/// block out on a single line would satisfy the shape law perfectly.
#[track_caller]
fn assert_block_height(block: Block, w: u16, rows: usize) {
    let mut vs = vs(LinkState::Connected);
    vs.mode = Mode::Thread(Region::Composer);
    vs.blocks = vec![block];
    const TALL: u16 = 40;
    assert_shape(&vs, w, TALL);
    let lines = text(&vs, w, TALL);
    // **THE BODY IS THE CONTENT BAND, AND `bands` IS WHO SAYS WHERE THAT IS.**
    // It used to be "everything above the row containing the word composer" —
    // a TEXT anchor on the band's label, which `T-41` deleted when he said
    // *"You don't need to give me instructions on how to use the textarea"*.
    // The band geometry is the one derivation of how a frame divides, so
    // asking it cannot go stale the way a word on a border can.
    let b = tui_render::bands(&vs, w, TALL);
    let first = b.header + b.banner;
    let body = &lines[first..first + b.content];
    let drawn = body.iter().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(
        drawn,
        rows,
        "block height at {w} columns: expected {rows} rows, got {drawn}\n{:#?}",
        body.iter()
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
    );
}

fn colours(vs: &ViewState, w: u16, h: u16) -> Vec<Color> {
    frame(vs, now(), w, h)
        .iter()
        .flat_map(|l| l.spans.clone())
        .filter_map(|s| s.style.fg)
        .collect()
}

/// **COLUMNS, NOT BYTES.** The transcript is full of one-column multi-byte
/// glyphs — `▸`, `⚙`, `…` — and `str::len` counts three for each of them. A
/// margin assertion written in bytes reads as a broken margin on exactly the
/// rows that carry an affordance, which is the opposite of what it is for.
fn cols_used(line: &str) -> usize {
    line.trim_end().chars().count()
}

// ── fixtures ────────────────────────────────────────────────────────────────

const URL: &str = "ws://127.0.0.1:3651/ws";

fn msg(sender: Sender, id: &str, h: u32, m: u32, text: &str) -> Block {
    Block::Message {
        id: BlockId {
            thread_id: "57db2ec9".into(),
            message_id: id.into(),
        },
        sender,
        ts: Some(at(h, m, 0)),
        text: text.into(),
        note: None,
        meta: Vec::new(),
        key: None,
        expandable: false,
        open: false,
    }
}

fn lane(glyph: char, severity: Severity, text: &str, count: Option<u64>) -> Block {
    Block::Lane {
        ts: Some(at(19, 19, 0)),
        glyph,
        severity,
        text: text.into(),
        count,
        key: None,
        expandable: false,
        open: false,
        affordance: None,
    }
}

/// The same lane row, but with something behind it — which is what makes it
/// draw an expander and claim the two columns the expander sits in.
fn lane_expandable(glyph: char, severity: Severity, text: &str, count: Option<u64>) -> Block {
    match lane(glyph, severity, text, count) {
        Block::Lane {
            ts,
            glyph,
            severity,
            text,
            count,
            key,
            ..
        } => Block::Lane {
            ts,
            glyph,
            severity,
            text,
            count,
            key,
            expandable: true,
            open: false,
            affordance: None,
        },
        other => other,
    }
}

fn blocks() -> Vec<Block> {
    vec![
        Block::Rule {
            text: "compacted (auto) · swallowed #41–#88".into(),
        },
        msg(
            Sender::Assistant,
            "m1",
            19,
            18,
            "The frame is five bands and the event lane is one of them.",
        ),
        lane(
            '⚙',
            Severity::Calm,
            "Read apps/tui/render/src/render.rs",
            None,
        ),
        lane(
            '◇',
            Severity::Calm,
            "task 9f3e215d · building step 3",
            Some(214),
        ),
        msg(Sender::User, "m2", 19, 25, "Mine goes on the right."),
        lane(
            '⚠',
            Severity::Warn,
            "retry #2 at 19:31 · overloaded · staleness is amber, never red",
            None,
        ),
    ]
}

/// **THE PROJECT LIST'S TWO ROWS, AND ONE OF THEM HAS A NAME.**
///
/// Until `0076` NEITHER row had a summary, which `0073` caught and named: the
/// crate's own golden for this screen showed a face made entirely of ids and
/// paths, **so the golden could not have shown the defect it was guarding** —
/// every row was a hex id and a path, and that is how it was reviewed clean
/// for two rounds. A golden that cannot fail on the thing the screen is about
/// is a golden for a different screen.
///
/// So row one carries a summary now, which is the ordinary case — `0073`
/// measured 34 of his 38 visible rows with one — and row two deliberately does
/// NOT, because the cwd fallback is a real path (45 of his 305 threads have an
/// empty summary UPSTREAM, which is filed and is not this crate's to fix) and
/// `a_cwd_is_never_tidied` is about exactly that path.
fn thread_rows() -> Vec<ThreadRow> {
    vec![
        ThreadRow {
            id: "bcccfa79-e3c4-4423-8850-7c46c7d3e5cb".into(),
            summary: "rebuild the index reader".into(),
            project: "harness".into(),
            cwd: "~/code/harness".into(),
            model: "opus".into(),
            state: "idle".into(),
            executing: false,
            queued: Known::Known(0),
            unread: Known::Known(7),
            updated: Some(at(18, 12, 0)),
            pinned: false,
            closed: false,
            known: true,
            activity: None,
            parent: None,
        },
        ThreadRow {
            id: "57db2ec9-e6c0-4492-89d0-19f7070ee0db".into(),
            summary: String::new(),
            project: "miscellaneous".into(),
            cwd: "~/.aui/mythcellaneous".into(),
            model: "opus".into(),
            state: "active".into(),
            executing: true,
            queued: Known::Known(2),
            unread: Known::Known(0),
            updated: Some(at(19, 28, 0)),
            pinned: false,
            closed: false,
            known: true,
            activity: None,
            parent: None,
        },
    ]
}

fn project_rows() -> Vec<ProjectRow> {
    vec![
        ProjectRow {
            name: "broomstick".into(),
            path: "~/minds/broomstick".into(),
            threads: Known::Known(69),
            pinned: 4,
            active: Known::Known(1),
            unread: Known::Known(7),
            last: Some(at(19, 28, 0)),
            unreadable_index: false,
        },
        ProjectRow {
            name: "harness".into(),
            path: "~/minds/harness".into(),
            threads: Known::Known(64),
            pinned: 2,
            active: Known::Known(0),
            unread: Known::Known(0),
            last: Some(at(18, 12, 0)),
            unreadable_index: false,
        },
        // No index at all. It is still a project and it SAYS so, rather than
        // reading as a project with no threads.
        ProjectRow {
            name: "chess".into(),
            path: "~/minds/chess".into(),
            threads: Known::Unknown,
            pinned: 0,
            active: Known::Known(0),
            unread: Known::Unknown,
            last: None,
            unreadable_index: true,
        },
    ]
}

fn vs(link: LinkState) -> ViewState {
    ViewState {
        header: Header {
            thread_page: false,
            trail: Vec::new(),
            // **THE THREAD HEADER NAMES THE THREAD, NOT ITS HASH.** It read
            // `57db2ec9 · active` until `0076`; `store::header` now composes
            // it out of `Store::label_of`.
            title: "answer the tax letter · active".into(),
            cwd: "~/.aui/mythcellaneous".into(),
            link: Link {
                url: URL.into(),
                down_since: match link {
                    // A fixture whose link is DOWN has been down since a known
                    // instant, because every duration the face states about it
                    // is measured from here. `NOW - 41s` is `0040` §9's own
                    // frame ("disconnected for 41s"), so the golden frames are
                    // the doc's frames and not a number picked to be round.
                    LinkState::Disconnected { .. } => Some(now() - TimeDelta::seconds(41)),
                    _ => None,
                },
                state: link,
            },
            account: Known::Known("sam".into()),
            queued: Known::Known(2),
            queue_scope: "this thread".into(),
            unread_elsewhere: Known::Known(5),
            // **`Unknown`, AND THAT IS WHY EVERY GOLDEN FRAME IS UNCHANGED BY
            // THE WARNING COUNT LANDING.**
            //
            // A fixture that said `Known(0)` would also render nothing, so either
            // would keep the frames stable — but `Unknown` is the true statement
            // about this fixture: it describes no daemon warning count at all, and
            // a zero would be claiming a healthy daemon the fixture never asked
            // about. The three states are pinned in
            // `render::tests::a_clean_daemon_puts_no_warning_count_on_the_header`
            // rather than by making every golden carry a number.
            warnings: Known::Unknown,
            warning_errors: Known::Unknown,
            activity: None,
        },
        mode: Mode::Thread(Region::Composer),
        projects: ProjectList::default(),
        list: ThreadList::default(),
        blocks: blocks(),
        viewport: Viewport::at_bottom(),
        // **A REAL COMPOSER, because the band is a real textarea now.** The
        // default is `editable: false`, which is the never-drawn case; a golden
        // built on it would be asserting on a screen he cannot reach.
        composer: Composer {
            editable: true,
            ..Composer::default()
        },
        // Mic mode is OFF in every golden but the mic ones, which build on
        // this and set it — the default is the face he sees.
        mic: tui_render::MicState::Idle,
        playing: None,
        notice: None,
        banner: None,
        stdout_only: false,
        queue: QueueView::default(),
        sheet: None,
        archived: ArchivedView::default(),
        expanded: false,
        selection: None,
        // The default is the HONEST one for his desk: no enhanced keys, so the
        // overlay withholds `shift-enter` rather than advertising it. A golden
        // that assumed the rich case would be asserting on a screen he has
        // never seen.
        caps: tui_render::Caps::default(),
        help_for: Mode::Thread(Region::Composer),
        help_from: 0,
        queue_cursor: 0,
        tree: Default::default(),
        tz_offset_minutes: 0,
        // No modal over this fixture. `Mode::RenameModal` is the only reader.
        rename_under: None,
        modal_label: None,
        // No outage over this fixture; `down_page` goldens set it.
        down: None,
    }
}

fn list_vs() -> ViewState {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::List;
    v.header.title = "harness · 2 threads · 1 active".into();
    // **NO WORKING DIRECTORY ON THE LIST HEADER** (`0040` §2 names it absent,
    // deliberately). `store::header`'s `Mode::List` arm hands back an empty
    // cwd since `0076`.
    v.header.cwd = String::new();
    v.header.account = Known::Unknown;
    v.header.queue_scope = "this project".into();
    v.blocks = Vec::new();
    v.list = ThreadList {
        rows: thread_rows(),
        cursor: 1,
        from_bottom: 0,
        window: Some(2),
        complete: true,
    };
    v
}

fn projects_vs() -> ViewState {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Projects;
    v.header.title = "5 unread · 1 active".into();
    v.header.cwd = String::new();
    v.header.account = Known::Unknown;
    v.header.queued = Known::Known(2);
    v.header.queue_scope = "across his threads".into();
    v.blocks = Vec::new();
    v.projects = ProjectList {
        rows: project_rows(),
        pinned: band_rows(&[("the threads daemon wire — living notes", "harness", "idle")]),
        recent: band_rows(&[
            (
                "Fix the viewport defects and the resize repaint",
                "harness",
                "active",
            ),
            ("Answer the tax letter", "miscellaneous", "idle"),
        ]),
        archived: 2,
        cursor: 1,
        from_bottom: 0,
        loaded: true,
        focus: None,
        // **ALL THREE CAPPED**, which is his real shape: 26 pins, 182 recent,
        // 33 projects against a ten-row default. The golden frame therefore
        // shows the `show more` foot on every band, which is the affordance he
        // asked for and the one the header no longer advertises.
        totals: [4, 9, 7],
        new_project: None,
        aui_down: None,
    };
    v
}

/// **THE SHEET (§5).** `aui` and `daemon` say whether each owner can act, so
/// one fixture draws both the working sheet and the degraded one — which is
/// the whole point: they are the SAME sheet in two states, not two designs.
fn sheet_vs(aui: Option<&str>, daemon: Option<&str>) -> ViewState {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Sheet;
    v.header.title = "Fix the viewport defects and the resize repaint".into();
    v.header.cwd = String::new();
    v.header.account = Known::Unknown;
    v.header.queued = Known::Known(0);
    v.header.queue_scope = "this thread".into();
    v.blocks = Vec::new();
    let a = |stem: &str, hint: Option<&str>| SheetRow {
        act: SheetAct::Model,
        stem: stem.into(),
        hint: hint.map(str::to_string),
        avail: match aui {
            Some(w) => Avail::Unavailable(w.into()),
            None => Avail::Ready,
        },
    };
    let d = |stem: &str, hint: Option<&str>| SheetRow {
        act: SheetAct::Interrupt,
        stem: stem.into(),
        hint: hint.map(str::to_string),
        avail: match daemon {
            Some(w) => Avail::Unavailable(w.into()),
            None => Avail::Ready,
        },
    };
    v.sheet = Some(SheetView {
        title: "Fix the viewport defects and the resize repaint".into(),
        facts: Facts::Thread(SheetFacts {
            project: "harness".into(),
            started: Some(Utc.with_ymd_and_hms(2026, 9, 12, 9, 0, 0).unwrap()),
            turns: 41,
            last_turn: Some(now() - TimeDelta::minutes(2)),
        }),
        rows: vec![
            a("model opus", Some("click to change")),
            d("interrupt the running turn", None),
            d(
                "compact the thread",
                Some("instructions optional, typed below"),
            ),
            a("rename", Some("edit the summary in place")),
            a("pin to the root", None),
            a("move up", None),
            a("move down", None),
            a("mark closed", None),
        ],
        cursor: 0,
        field: None,
        armed: None,
    });
    v
}

/// Rows for the ROOT's cross-project bands. They carry a `project`, which is
/// the column the band draws and the depth-2 list never needs.
fn band_rows(spec: &[(&str, &str, &str)]) -> Vec<ThreadRow> {
    spec.iter()
        .enumerate()
        .map(|(i, (summary, project, state))| ThreadRow {
            id: format!("{i}0000000-e3c4-4423-8850-7c46c7d3e5cb"),
            summary: (*summary).into(),
            project: (*project).into(),
            cwd: String::new(),
            model: "opus".into(),
            state: (*state).into(),
            executing: *state == "active",
            queued: Known::Known(0),
            unread: Known::Known(0),
            // **TOTAL, NOT JUST TOTAL-ENOUGH.** It was `19 - i`, which
            // panicked on the twentieth row — harmless while no fixture had
            // twenty, and a trap the moment the `see all` page needed five
            // thousand. The first twenty stamps are unchanged, so no golden
            // frame moved.
            updated: Some(at(19 - (i as u32 % 20), 28, 0)),
            pinned: true,
            closed: false,
            known: true,
            activity: None,
            parent: None,
        })
        .collect()
}

/// A root with NOTHING pinned and NOTHING recent — the state `0040` §10 draws
/// for the very first launch.
fn bare_root_vs() -> ViewState {
    let mut v = projects_vs();
    v.projects.pinned = Vec::new();
    v.projects.recent = Vec::new();
    v.projects.archived = 0;
    v.projects.cursor = 0;
    v
}

/// **IS THIS DRAWN LINE THE BAND HEADER FOR `name`?**
///
/// A band header is a CURSOR STOP (`DEC-46`), so it wears the two-column
/// gutter — or the cursor MARKER in place of it when he is standing on it.
/// Every test that looks for a header has to see through both spellings, and
/// writing that out per site is how one of them silently stops matching.
///
/// The `· ` is load-bearing: it is what separates the band header from the
/// frame header, which also begins with the word `projects`.
fn band_header_is(line: &str, name: &str) -> bool {
    let body = line
        .strip_prefix("▸ ")
        .or_else(|| line.strip_prefix("  "))
        .unwrap_or(line);
    body.starts_with(&format!("{name} · "))
}

// ── the ROOT'S BANDS — pinned, recent, projects, chrome last ────────────────

/// **THE ORDER IS THE DESIGN'S ARGUMENT** (`DEC-28`): pinned band first, recent
/// band next, then the projects, then the chrome. The operator noticed the
/// build had projects only; this is the test that says it does not.
#[test]
fn the_root_draws_its_bands_in_the_designs_order() {
    let v = projects_vs();
    assert_shape(&v, W, H);
    let lines = text(&v, W, H);
    let row_of = |needle: &str| {
        lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} is nowhere on the root: {lines:#?}"))
    };
    // The BAND HEADERS, not the frame header. A header is a CURSOR STOP since
    // `DEC-46`, so it wears the same two-column gutter every other stop wears
    // and carries its count after the name — `  pinned · 1`. The frame header
    // is row 0 and starts at column 0, so the gutter alone disambiguates it.
    let band = |name: &str| {
        lines
            .iter()
            .skip(1)
            .position(|l| band_header_is(l, name))
            .map(|i| i + 1)
            .unwrap_or_else(|| panic!("no {name:?} band on the root: {lines:#?}"))
    };
    let pinned = band("pinned");
    let recent = band("recent");
    let projects = band("projects");
    let archived = row_of("archived (");
    assert!(
        pinned < recent && recent < projects && projects < archived,
        "band order broke: pinned {pinned}, recent {recent}, projects {projects}, archived {archived}"
    );
    // Each band header sits ABOVE its own rows, which is what makes it a band
    // rather than a word.
    assert!(pinned < row_of("the threads daemon wire"));
    assert!(recent < row_of("Answer the tax letter"));
    assert!(projects < row_of("broomstick"));
    // The cross-project bands name the project each row lives in — the only
    // thing on a root band row that says where it is.
    let tax = &lines[row_of("Answer the tax letter")];
    assert!(
        tax.contains("miscellaneous"),
        "a band row must name its project: {tax:?}"
    );
    // The chrome tally is a real count, not an invented zero.
    assert!(
        lines[archived].contains("archived (2)"),
        "{:?}",
        lines[archived]
    );
}

/// An empty PINNED band gets the doc's own sentence; an empty RECENT band gets
/// NOTHING — no header over nothing. Both are `0040` §10's frame.
#[test]
fn an_empty_pinned_band_says_what_fills_it_and_an_empty_recent_band_is_silent() {
    let v = bare_root_vs();
    assert_shape(&v, W, H);
    let lines = text(&v, W, H);
    assert!(
        lines.iter().any(|l| l.contains("nothing pinned yet")),
        "the empty pinned band must teach the pin: {lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.trim_start().starts_with("recent ·")),
        "an empty recent band drew a header over nothing: {lines:#?}"
    );
    // And the projects still stand, under their own header — which now carries
    // the project COUNT he asked for, in the band header, where he put it.
    // **SKIP ROW 0**: the frame header now reads `projects · 2 with an index
    // · …` and a `trim_start` match would find it first. The band header is a
    // cursor stop, so it wears either the gutter or the cursor marker.
    let projects = lines
        .iter()
        .skip(1)
        .position(|l| band_header_is(l, "projects"))
        .map(|i| i + 1)
        .expect("the projects band");
    assert!(
        lines[projects + 1].contains("broomstick"),
        "{:?}",
        lines[projects + 1]
    );
    // Even at zero the archived row is drawn — a row that comes and goes
    // reshapes the menu under his muscle memory.
    assert!(
        lines.iter().any(|l| l.contains("archived (0)")),
        "{lines:#?}"
    );
}

/// **THE CURSOR SPACE INCLUDES THE BANDS, HEADERS AND ALL** (`DEC-46`).
///
/// `DEC-37` ruled a band header was drawn but NOT a stop. The operator
/// retracted that clause — *"if we are selecting one of the band headers and
/// then click right we should just expand that band"* — and this test was
/// renamed and inverted with it. Everything else `DEC-37` decided stands.
///
/// **THE POINT IS THAT A HEADER IS AN ORDINARY ROW**, not a special case the
/// cursor skips into and out of with different rules in each direction: the
/// walk below is a plain `0..selectable()` and the header rows are in it on the
/// same terms as every other, answering `RootRow(i)` to the hit map exactly as
/// a thread row does.
#[test]
fn the_bands_join_the_cursor_space_and_the_headers_are_stops_too() {
    let v = projects_vs();
    // THREE BAND HEADERS + one pinned + two recent + three projects + THREE
    // `show more` FEET (every band in this fixture is capped, which is his real
    // shape) + the TWO CHROME STOPS `0063` opened: `+ new project` and
    // `archived (N)`. Fourteen.
    assert_eq!(v.projects.selectable(), 14);
    use tui_render::BandId;
    // **THE LADDER, IN FULL.** Written out rather than spot-checked because it
    // is the whole surface grammar in one place: header, rows, foot — three
    // times — and then the chrome.
    use tui_render::RootItem as I;
    assert_eq!(
        (0..v.projects.selectable())
            .map(|i| v.projects.item(i).expect("a stop"))
            .collect::<Vec<_>>(),
        vec![
            I::Band(BandId::Pinned),
            I::Pinned(0),
            I::SeeAll(BandId::Pinned),
            I::Band(BandId::Recent),
            I::Recent(0),
            I::Recent(1),
            I::SeeAll(BandId::Recent),
            I::Band(BandId::Projects),
            I::Project(0),
            I::Project(1),
            I::Project(2),
            I::SeeAll(BandId::Projects),
            I::NewProject,
            I::Archived,
        ]
    );
    assert_eq!(v.projects.item(14), None);

    // **UP AND DOWN PASS THROUGH A HEADER NATURALLY.** Walking the whole cursor
    // space forwards and then backwards must visit the SAME rows in reverse —
    // which is false the moment a header is skipped in one direction only.
    let forwards: Vec<_> = (0..v.projects.selectable())
        .map(|i| v.projects.item(i))
        .collect();
    let backwards: Vec<_> = (0..v.projects.selectable())
        .rev()
        .map(|i| v.projects.item(i))
        .collect();
    let mut reversed = backwards.clone();
    reversed.reverse();
    assert_eq!(forwards, reversed, "the cursor space is not symmetric");

    // **THE COHERENCE THAT ACTUALLY MATTERS**, and the first version of this
    // test did not have it. Asserting `lines()[drawn_row_of(i)].selectable()`
    // reads `selectable()` through `drawn_row_of`, which is derived FROM it —
    // so making a band header a stop left the assertion true and the test
    // green. A forced mutation caught that; this is the replacement.
    //
    // The two coordinate systems must AGREE ON THE FRAME: the row
    // `drawn_row_of` hands the shell's viewport is the same row the renderer
    // drew the cursor marker on, and the same row the hit map answers
    // `RootRow(i)` for. Nothing here is derived from `selectable()`.
    let (_, hits) = frame_with_hits(&v, now(), W, H);
    let lines = text(&v, W, H);
    // `drawn_row_of` counts rows of the SURFACE, which starts under the header
    // and the (here empty) banner — the same coordinate `follow_cursor` scrolls
    // in. The frame adds those bands back.
    let b = tui_render::bands(&v, W, H);
    let top = (b.header + b.banner) as usize;
    for i in 0..v.projects.selectable() {
        let row = top + v.projects.drawn_row_of(i);
        // **PROBED IN THE TAIL, PAST ANY TITLE.** Column 4 used to serve here
        // and no longer can: a band row's TITLE claims its own columns as the
        // door onto §5's sheet, so column 4 names the title rather than the
        // row. The invariant this loop exists for is unchanged — the row
        // `drawn_row_of` hands the viewport is the row the hit map answers for
        // — so it is asserted where the row is still the whole answer.
        assert_eq!(
            hits.hit(W - 5, row as u16),
            Target::RootRow(i),
            "cursor {i} resolves to drawn row {row}, which the frame calls something else: {:?}",
            lines[row]
        );
        // …and whatever is under the pointer on that row, it names THIS index.
        // This is the half that would otherwise have been lost by moving the
        // probe: a title claiming the wrong row's index is exactly the defect
        // a sub-row span can introduce.
        assert_eq!(
            hits.hit(4, row as u16).row_index(),
            Some(i),
            "drawn row {row} has an affordance that names another row's index: {:?}",
            lines[row]
        );
    }
    // **THE TITLE IS THE DOOR, AND IT IS ON BOTH BAND ROWS.** §5: "click the
    // title of a thread, ANYWHERE it appears."
    let band_rows: Vec<usize> = (0..v.projects.selectable())
        .filter(|&i| {
            matches!(
                v.projects.item(i),
                Some(tui_render::RootItem::Pinned(_) | tui_render::RootItem::Recent(_))
            )
        })
        .collect();
    assert_eq!(
        band_rows.len(),
        v.projects.pinned.len() + v.projects.recent.len(),
        "a band row lost its cursor stop"
    );
    for i in band_rows {
        let row = top + v.projects.drawn_row_of(i);
        assert_eq!(
            hits.hit(4, row as u16),
            Target::ThreadTitle(i),
            "band row {i} has no title to click: {:?}",
            lines[row]
        );
        // **A CLICK ON THE NAME SELECTS ITS ROW; the KEY through the same
        // span opens its options.** Round four made the click DRILL, on his
        // *"clicking on a thread opens its options, not the thread"*; he has
        // retracted that reading — *"clicking on a thread shouldn't take me
        // into it. Only right can do it."*
        assert_eq!(
            activation(Target::ThreadTitle(i), Mode::Projects, Via::Click),
            Some(Intent::Select(i)),
            "clicking a thread's name must select its row and nothing else"
        );
        assert_eq!(
            activation(Target::ThreadTitle(i), Mode::Projects, Via::Key),
            Some(Intent::OpenSheet(i)),
            "`o` lost its door onto the sheet"
        );
        // The two marker columns are NOT the title — they are the cursor and
        // the pin mark, and a click there is the plain row behaviour.
        assert_eq!(hits.hit(0, row as u16), Target::RootRow(i));
    }
    // **AND THE BAND HEADERS ARE REACHABLE BY THE MOUSE TOO** (`DEC-46`). The
    // old assertion here was the exact inverse — a header was a free, silent
    // miss — and it is retracted with the clause it tested. A header he can put
    // the cursor on but not click would be the worse half of an affordance, and
    // it resolves to the SAME `RootRow(i)` the keyboard stands on, so the two
    // gestures cannot drift.
    for (name, id) in [
        ("pinned", tui_render::BandId::Pinned),
        ("recent", tui_render::BandId::Recent),
        ("projects", tui_render::BandId::Projects),
    ] {
        let row = lines
            .iter()
            .skip(1)
            .position(|l| band_header_is(l, name))
            .map(|i| i + 1)
            .unwrap_or_else(|| panic!("no {name:?} band"));
        let sel = (0..v.projects.selectable())
            .find(|&i| v.projects.item(i) == Some(tui_render::RootItem::Band(id)))
            .unwrap_or_else(|| panic!("the {name:?} header is not a cursor stop"));
        assert_eq!(
            hits.hit(4, row as u16),
            Target::RootRow(sel),
            "the {name:?} header does not click to the row the cursor stands on"
        );
        assert_eq!(
            top + v.projects.drawn_row_of(sel),
            row,
            "the {name:?} header's cursor row and its drawn row disagree"
        );
    }
    // The cursor's marker is on the row `drawn_row_of` names, and on no other.
    let marked: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with('▸'))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        marked,
        vec![top + v.projects.drawn_row_of(v.projects.cursor)],
        "the drawn cursor and the scroll arithmetic disagree about which row is selected"
    );
}

// ── the chat, at the width he mostly reads it ───────────────────────────────

#[test]
fn the_chat_at_100x24() {
    let v = vs(LinkState::Connected);
    assert_shape(&v, W, H);
    golden(
        &text(&v, W, H),
        &[
        "answer the tax letter · active                                                                      ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "── compacted (auto) · swallowed #41–#88 ────────────────────────────────────────────────────────────",
        "                                                                                                    ",
        "  claude · 19:18 · 12m                                                                              ",
        "  The frame is five bands and the event lane is one of them.                                        ",
        "        ⚙ Read apps/tui/render/src/render.rs                                                        ",
        "        ◇ task 9f3e215d · building step 3 ×214                                                      ",
        "                                                                                                    ",
        "           19:25 · 5m                                                                          m2   ",
        "           Mine goes on the right.                                                                  ",
        "        ⚠ retry #2 at 19:31 · overloaded · staleness is amber, never red                            ",
        "────────────────────────────────────────────────────────────────────────────────────────────────────",
        "   type a message…                                                                                  ",
        "────────────────────────────────────────────────────────────────────────────────────────────────────",
        "  ^/ keys · ^S stdout · ^O queue · ^C quit ┊ ^T mic · ← back at the edge                            ",
    ],
    );
}

/// **THE CHAT SHAPE, as a structural assertion rather than a picture.** His
/// words, restyled: no quote line anywhere, his messages in a block pushed
/// right with the text inside them left-aligned, and the third lane centred
/// between firm margins.
#[test]
fn the_llm_is_on_the_left_his_words_are_on_the_right_and_the_lane_is_neither() {
    let v = vs(LinkState::Connected);
    let lines = text(&v, W, H);

    let assistant = lines
        .iter()
        .find(|l| l.contains("The frame is five bands"))
        .expect("the assistant's box");
    let his = lines
        .iter()
        .find(|l| l.contains("Mine goes on the right"))
        .expect("his box");
    let lane_row = lines
        .iter()
        .find(|l| l.contains("Read apps/tui"))
        .expect("a lane row");

    // **THE QUOTE LINE IS GONE FROM EVERY MESSAGE.** *"You don't need to have
    // the quote line on the left of every message. It's distracting."* — and
    // the whole FRAME is searched, not just the two rows above, because a rail
    // that survived on one shape and not another is exactly the kind of
    // half-applied restyle nobody notices in a fixture.
    for l in &lines {
        assert!(
            !l.contains('▌') && !l.contains('▐'),
            "a quote line survived the restyle: {l:?}"
        );
    }
    assert!(
        assistant.starts_with("  The frame"),
        "the LLM's words start at the gutter, with nothing in front of them: {assistant:?}"
    );
    assert!(
        cols_used(assistant) < W as usize,
        "the LLM's box must not reach the right edge: {assistant:?}"
    );

    // **HIS BLOCK IS OFFSET RIGHT; HIS TEXT IS LEFT-ALIGNED INSIDE IT.** The
    // mid-round correction, as structure: *"User message should be right
    // aligned in the sense that they have a left margin, but the content
    // within them should be left aligned."*
    let lead = his.len() - his.trim_start().len();
    assert!(lead > 8, "his block has no left margin: {his:?}");
    assert!(
        cols_used(his) < W as usize - 2,
        "his text is flush right — that is the alignment this correction retires: {his:?}"
    );

    // **THE THIRD LANE IS CENTRED BETWEEN FIRM MARGINS.** *"info blobs should
    // be centered with a max character size that enforces a margin on each
    // side so it doesn't take up the whole page."* This supersedes the
    // left-aligned two-column indent the lane shipped with, which stood only
    // because the question had been left open.
    let left = tui_render::geom::blob_left(W as usize);
    let width = tui_render::geom::blob_width(W as usize);
    assert_eq!(
        lane_row.len() - lane_row.trim_start().len(),
        left,
        "the lane is not centred: {lane_row:?}"
    );
    assert!(
        cols_used(lane_row) <= left + width,
        "a blob broke its right margin: {lane_row:?}"
    );
    assert!(
        lane_row[left..].starts_with("⚙ "),
        "the lane kept its glyph prefix: {lane_row:?}"
    );
}

/// **THE MARGINS ARE FIRM — EXPANDED AND COLLAPSED, AT EVERY WIDTH.**
///
/// *"Clicking it should expand it but should not break the margins, they're
/// firm. It can expand or collapse vertically."*
///
/// The property has two halves and both are asserted here: the left and right
/// margins of an open blob are IDENTICAL to those of the same blob closed, and
/// opening it adds ROWS. A unit test is where the arithmetic is checked; the
/// pty probe `chat_conformance_test.sh` is where the glass is.
#[test]
fn an_info_blob_grows_down_and_never_sideways() {
    let long = "a tool call whose input is far longer than any of these terminals is wide, \
                and which therefore has to be cut rather than wrapped unless he opens it";
    for w in [120u16, 100, 80, 60, 48, 40] {
        let left = tui_render::geom::blob_left(w as usize);
        let width = tui_render::geom::blob_width(w as usize);
        // **THE MARGIN IS A FUNCTION OF THE WIDTH SINCE HIS 8/8 LANDED**, and
        // asking the constant was what made this read as "no left margin" at
        // 80 columns: the constant is the wide number and 80 is the collapsed
        // one. `blob_margin` is the same single-argument function the painter
        // calls, which is the whole reason the margin can be firm.
        let margin = tui_render::geom::blob_margin(w as usize);
        assert!(left >= margin, "at {w}: no left margin");
        assert!(
            left + width <= w as usize - margin,
            "at {w}: no right margin"
        );

        let mut closed = vs(LinkState::Connected);
        closed.mode = Mode::Thread(Region::Chat);
        closed.blocks = vec![lane_expandable('⚙', Severity::Calm, long, None)];
        let mut open = closed.clone();
        let Block::Lane { open: o, .. } = &mut open.blocks[0] else {
            unreachable!()
        };
        *o = true;

        // EVERY row the blob occupies: from the one carrying its first words,
        // down to the first blank or rule. Filtering on remembered phrases
        // would have quietly missed the LAST row — the one carrying the
        // expander, and therefore the one that decides the left margin.
        let body = |v: &tui_render::ViewState| -> Vec<String> {
            let rows = text(v, w, H);
            let at = rows
                .iter()
                .position(|l| l.contains("a tool call"))
                .expect("the blob is not on the screen");
            rows[at..]
                .iter()
                .take_while(|l| !l.trim().is_empty() && !l.trim_start().starts_with('─'))
                .cloned()
                .collect()
        };
        let c = body(&closed);
        let e = body(&open);
        assert_eq!(c.len(), 1, "at {w}: a closed blob is one row: {c:?}");
        assert!(
            e.len() > c.len(),
            "at {w}: opening the blob showed him nothing more: {e:?}"
        );
        // **THE FIRM MARGIN, STATED AS A BAND NO PAINTED CHARACTER LEAVES.**
        // The expander mark lives in the left margin and is two columns wide,
        // so the floor is `left - 2` and not `left`; the ceiling is the blob's
        // own right edge. Closed and open are held to the SAME two numbers,
        // which is the whole of "they're firm".
        let bounds = |rows: &[String]| -> (usize, usize) {
            (
                rows.iter()
                    .map(|l| l.len() - l.trim_start().len())
                    .min()
                    .unwrap_or(left),
                rows.iter().map(|l| cols_used(l)).max().unwrap_or(0),
            )
        };
        let (lc, rc) = bounds(&c);
        let (le, re) = bounds(&e);
        assert_eq!(
            lc, le,
            "at {w}: opening the blob moved its LEFT margin\nclosed: {c:?}\nopen: {e:?}"
        );
        // Ragged right INSIDE the margin is fine and is what wrapping does;
        // growing PAST where the closed row already stopped is the failure this
        // scope is about.
        assert!(
            re <= rc,
            "at {w}: opening the blob pushed its right edge out\nclosed: {c:?}\nopen: {e:?}"
        );
        for l in c.iter().chain(e.iter()) {
            assert!(
                l.len() - l.trim_start().len() >= left - 2,
                "at {w}: a blob broke its left margin: {l:?}"
            );
            assert!(
                cols_used(l) <= left + width,
                "at {w}: the right margin broke: {l:?}"
            );
        }
    }
}

/// **THE WHITE BOX, AND THE ALIGNMENT INSIDE IT** — at his widths and at a
/// narrow one.
///
/// Three things, and the third is the one the mid-round correction is about:
/// every row of his message starts at the SAME column (the block has a left
/// margin), that column is well right of centre (it is his lane), and a SHORT
/// row does not end where a LONG row ends (the text is ragged right, which is
/// what left-aligned means and what right-aligned text would not do).
#[test]
fn his_block_has_a_left_margin_and_his_text_inside_it_is_left_aligned() {
    for w in [120u16, 100, 80, 60] {
        let mut v = vs(LinkState::Connected);
        v.mode = Mode::Thread(Region::Chat);
        v.blocks = vec![msg(
            Sender::User,
            "m9",
            19,
            25,
            "the quick brown fox jumps over the lazy dog and keeps running until it \
             has to wrap, and then\nshort.",
        )];
        let rows: Vec<String> = text(&v, w, H)
            .into_iter()
            .filter(|l| l.contains("quick brown") || l.contains("short."))
            .collect();
        assert!(rows.len() >= 2, "at {w}: the fixture did not wrap: {rows:?}");
        let cols: Vec<usize> = rows.iter().map(|l| l.len() - l.trim_start().len()).collect();
        assert!(
            cols.windows(2).all(|p| p[0] == p[1]),
            "at {w}: his rows do not share one left edge — the text is not left-aligned: {rows:?}"
        );
        // A left margin at every width, and a GENEROUS one at the widths he
        // actually reads at. At or below 80 columns the margin collapses with
        // everything else — decoration first, metadata second, content never —
        // so the bar there is only that it exists.
        assert!(cols[0] >= 5, "at {w}: his block has no left margin: {rows:?}");
        if w > 80 {
            // **RE-BASELINED: THE MARGIN IS HIS NUMBER AND NO LONGER A
            // FRACTION OF THE SCREEN.** This asked for more than a fifth of
            // the width, which the leftover-margin geometry happened to give
            // (42 columns at 120) and which is the exact complaint being
            // fixed — *"the margins are too big"*. What it asks now is his
            // number: at least ten columns, and the box's own padding on top
            // of it before his words start.
            assert!(
                cols[0] >= tui_render::geom::MSG_MARGIN,
                "at {w}: his block is inside his ten-column margin: {rows:?}"
            );
        }
        // **AND THE BOX STAYS INSIDE THE SCREEN GUTTER.** His measure is not
        // the assistant's at a narrow width — the left margin comes out of the
        // same columns — so a box wrapped at the wrong measure overflows to the
        // right, where it looks like a rendering fault and nothing else.
        for l in &rows {
            assert!(
                cols_used(l) <= w as usize - 2,
                "at {w}: his box overflowed the gutter: {l:?}"
            );
        }
        let ends: Vec<usize> = rows.iter().map(|l| cols_used(l)).collect();
        assert!(
            ends.iter().any(|e| *e != ends[0]),
            "at {w}: every row ends in the same column — that is RIGHT-aligned text, \
             which is the thing his correction retires: {rows:?}"
        );
    }
}

/// **THE MARKDOWN SUBSET, ON THE GLASS** — the three he asked for, and the one
/// he ruled out.
///
/// > *Maybe you can detect markdown and handle it a little bit… auto structure
/// > tables to fill the whole width of the page, bold, italics, numbered,
/// > bulleted lists. Should we handle headers? I don't think so.*
#[test]
fn a_message_renders_the_markdown_subset_and_not_headers() {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Thread(Region::Chat);
    v.blocks = vec![msg(
        Sender::Assistant,
        "m1",
        19,
        18,
        "# Not a header\n**bold** and *italic*\n- first\n- second\n1. one\n2. two",
    )];
    let lines = text(&v, W, H);
    let body: Vec<&String> = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .collect();
    let joined: String = body.iter().map(|l| l.as_str()).collect::<Vec<_>>().join("\n");

    // HEADERS ARE OUT — the hash stays on screen, because the line is prose.
    assert!(joined.contains("# Not a header"), "{joined}");
    // The emphasis MARKERS are gone from the screen…
    assert!(!joined.contains("**bold**"), "{joined}");
    assert!(joined.contains("bold and italic"), "{joined}");
    // …and the emphasis itself is a MODIFIER on the message's own colour, not
    // a second colour. A bold run drawn in a different ink would be a new
    // palette entry nobody decided on.
    let bold: Vec<String> = tui_render::frame(&v, now(), W, H)
        .iter()
        .flat_map(|l| l.spans.clone())
        .filter(|sp| sp.style.add_modifier.contains(ratatui::style::Modifier::BOLD))
        .map(|sp| sp.content.to_string())
        .collect();
    assert!(
        bold.contains(&"bold".to_string()),
        "nothing was bolded: {bold:?}"
    );
    // Lists get a bullet and keep their numbers.
    assert!(joined.contains("• first"), "{joined}");
    assert!(joined.contains("• second"), "{joined}");
    assert!(joined.contains("1. one"), "{joined}");
    assert!(joined.contains("2. two"), "{joined}");
}

/// **A TABLE FILLS THE WIDTH, AND SAYS WHAT IT DOES WHEN IT CANNOT.**
///
/// At his widths the grid is drawn and every one of its rows reaches the same
/// right edge. At a width where a grid of this many columns cannot carry one
/// character per column, the grid is ABANDONED for prose — and every word
/// survives either way, which is the half that matters.
#[test]
fn a_table_fills_the_width_and_degrades_to_prose_rather_than_losing_a_word() {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Thread(Region::Chat);
    v.blocks = vec![msg(
        Sender::Assistant,
        "m1",
        19,
        18,
        "| lane | what it carries | when |\n|---|---|---|\n\
         | chat | speech acts | always |\n| events | tool calls | on demand |",
    )];
    for w in [120u16, 100, 80, 60] {
        let rows: Vec<String> = text(&v, w, H)
            .into_iter()
            .filter(|l| l.contains('│'))
            .collect();
        assert!(!rows.is_empty(), "at {w}: no grid was drawn");
        // **THE COLUMNS LINE UP**, which is the grid's whole reason to exist —
        // measured on the SEPARATORS rather than on the row ends, because the
        // last cell of a row is left-aligned in its column and its trailing
        // padding is invisible.
        let bars = |l: &str| -> Vec<usize> {
            l.chars()
                .enumerate()
                .filter(|(_, c)| *c == '│')
                .map(|(i, _)| i)
                .collect()
        };
        let first = bars(&rows[0]);
        assert_eq!(first.len(), 2, "at {w}: a three-column table has two gaps");
        for r in &rows {
            assert_eq!(bars(r), first, "at {w}: the columns do not line up: {rows:?}");
        }
        // And it FILLS the page width: the rule row under the header is the one
        // row that is padded edge to edge, so it is the one that can say so.
        let rule = rows
            .iter()
            .find(|l| l.contains('─'))
            .expect("the header rule");
        // The assistant's measure is the whole block since the margins became
        // the specification — it has no box to spend two columns on — so the
        // grid's right edge is `GUTTER + box_width` rather than
        // `GUTTER + box_width - 2`.
        assert_eq!(
            cols_used(rule),
            GUTTER + tui_render::geom::box_width(w as usize),
            "at {w}: the grid stops short of the message's measure: {rule:?}"
        );
    }
    // NARROW: three columns need 3*2 + 3 = 9 columns before a grid is even
    // possible, and the assistant's measure at 40 is well under that once the
    // longest cell is considered — so the grid goes and the words stay.
    let narrow: String = text(&v, 40, H).join("\n");
    for word in ["lane", "carries", "speech", "demand"] {
        assert!(narrow.contains(word), "{word} was lost at 40 columns:\n{narrow}");
    }
}

/// **THE BARE MESSAGE ID, AT THE TOP OF HIS BOX, RIGHT ALIGNED.**
///
/// > *I think by default let's include the message id at the top of the user
/// > message, right aligned. Don't say ID or anything, just actually print the
/// > id.*
///
/// No label, no `id:`, no prefix — and it is flush with the box's right edge,
/// not the screen's.
#[test]
fn his_box_prints_the_bare_message_id_at_its_top_right() {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Thread(Region::Chat);
    v.blocks = vec![msg(Sender::User, "4172", 19, 25, "what I said")];
    for w in [120u16, 100, 80, 60] {
        let lines = text(&v, w, H);
        let at = lines
            .iter()
            .position(|l| l.contains("4172"))
            .unwrap_or_else(|| panic!("at {w}: the id is not on the screen"));
        let id_row = &lines[at];
        let words = lines
            .iter()
            .position(|l| l.contains("what I said"))
            .expect("his words");
        assert!(at < words, "at {w}: the id is not at the TOP of the box");
        assert_eq!(at + 1, words, "at {w}: something came between the id and his words");
        // NO LABEL. The row must not say "id" anywhere, in any case.
        assert!(
            !id_row.to_lowercase().contains("id"),
            "at {w}: the id row labelled itself: {id_row:?}"
        );
        // RIGHT-ALIGNED TO THE BOX rather than to the screen: the box's own
        // padding column, then the screen gutter. Three columns, at every
        // width, and it is the SAME three at every width — which is what
        // "right aligned" has to mean once the block itself is inset.
        assert_eq!(
            w as usize - cols_used(id_row),
            3,
            "at {w}: the id is not flush with the box's right edge: {id_row:?}"
        );
        // …and it really is the last thing on the row.
        assert!(
            id_row.trim_end().ends_with("4172"),
            "at {w}: something is drawn to the right of the id: {id_row:?}"
        );
    }
}

/// **THE METADATA IS OFF BY DEFAULT AND THE EXPANDER IS THE TOGGLE.**
///
/// > *There's also some metadata we attach to user inputs… we need a way to
/// > toggle viewing this.*
///
/// His DEFAULT view is the id and nothing else, which is the specific thing a
/// builder would have got wrong by showing the lot.
#[test]
fn a_user_messages_metadata_is_hidden_until_the_block_is_opened() {
    let meta = vec![
        ("intent".to_string(), "immediate".to_string()),
        ("tui.client".to_string(), "harness-tui".to_string()),
    ];
    let build = |open: bool| {
        let mut v = vs(LinkState::Connected);
        v.mode = Mode::Thread(Region::Chat);
        v.blocks = vec![Block::Message {
            id: BlockId {
                thread_id: "57db2ec9".into(),
                message_id: "4172".into(),
            },
            sender: Sender::User,
            ts: Some(at(19, 25, 0)),
            text: "what I said".into(),
            note: None,
            meta: meta.clone(),
            key: None,
            expandable: true,
            open,
        }];
        text(&v, W, H).join("\n")
    };
    let closed = build(false);
    assert!(closed.contains("4172"), "the id is never behind the toggle");
    assert!(
        !closed.contains("harness-tui") && !closed.contains("immediate"),
        "the metadata is showing by default:\n{closed}"
    );
    let open = build(true);
    assert!(open.contains("intent immediate"), "{open}");
    assert!(open.contains("tui.client harness-tui"), "{open}");
}

/// **SCOPE 5's CHECK, EXERCISED RATHER THAN ASSERTED.** The stdout-only
/// toggle landed earlier in the arc and it was his stated immediate priority;
/// a later task restyled the very path it renders through. So: flip it, and
/// look at what is drawn.
///
/// Two things have to hold. Everything but what was SAID is gone — and what is
/// left is the restyled box, not a fallback shape: no quote line, the id at the
/// top right of his, and the white box's left margin still there. A restyle
/// that quietly worked only when the event lane was present would pass every
/// other test in this file.
#[test]
fn the_stdout_only_view_still_draws_the_restyled_boxes() {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Thread(Region::Chat);
    v.stdout_only = true;
    // The projection is the shell's; what the RENDERER is handed under the
    // toggle is the stdout blocks alone, so that is the fixture.
    v.blocks = vec![
        msg(Sender::Assistant, "m1", 19, 18, "what I answered"),
        msg(Sender::User, "4172", 19, 25, "what I said"),
    ];
    let lines = text(&v, W, H);
    let joined = lines.join("\n");
    assert!(joined.contains("what I answered") && joined.contains("what I said"));
    for l in &lines {
        assert!(
            !l.contains('▌') && !l.contains('▐'),
            "the stdout-only view kept a quote line: {l:?}"
        );
    }
    let id = lines.iter().position(|l| l.contains("4172")).expect("the id");
    let words = lines
        .iter()
        .position(|l| l.contains("what I said"))
        .expect("his words");
    assert_eq!(id + 1, words, "his box lost its id row under the toggle");
    let lead = lines[words].len() - lines[words].trim_start().len();
    assert!(lead > 8, "his box lost its left margin under the toggle");
}

/// The count stamp IS the promise that nothing was dropped, so it has to be on
/// screen — and only where something was actually folded.
#[test]
fn a_folded_row_shows_what_it_stands_for() {
    let lines = text(&vs(LinkState::Connected), W, H);
    let folded = lines
        .iter()
        .find(|l| l.contains("building step 3"))
        .unwrap();
    assert!(folded.contains("×214"), "{folded:?}");
    let plain = lines.iter().find(|l| l.contains("Read apps/tui")).unwrap();
    assert!(!plain.contains('×'), "a row of one is not ×1: {plain:?}");
}

// ── block height: the layout law, at three widths ───────────────────────────

#[test]
fn a_closed_info_blob_is_always_exactly_one_row() {
    for w in [100u16, 80, 48] {
        assert_block_height(
            lane(
                '⚙',
                Severity::Calm,
                "a tool call whose input is far longer than any of these terminals is wide, \
                 and which therefore has to be cut rather than wrapped",
                None,
            ),
            w,
            1,
        );
    }
}

#[test]
fn a_rule_is_always_exactly_one_row() {
    for w in [100u16, 80, 48] {
        assert_block_height(
            Block::Rule {
                text: "compacted (auto)".into(),
            },
            w,
            1,
        );
    }
}

/// A message is a run header plus its wrapped rows, and the row count is a
/// function of the block width — `w - 2 - `[`geom::msg_margin`], capped at
/// `BOX_MAX` above 108 columns.
///
/// **THE ARITHMETIC IN THESE COMMENTS WAS RESTATED WHEN THE MARGINS BECAME THE
/// SPECIFICATION.** The row COUNTS did not move, which is the trap: this test
/// stayed green across a change that moved every measure it names, so the
/// comments had gone from explaining the numbers to contradicting them.
#[test]
fn a_message_wraps_to_the_rows_its_box_width_implies() {
    // 90 characters of content, in words that cannot be broken at a lucky
    // place.
    let body = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike \
                november oscar";
    // 100 cols: block 88, the assistant's measure 88 -> header + 2 rows.
    assert_block_height(msg(Sender::Assistant, "m", 19, 18, body), 100, 3);
    // 80 cols: the narrow margin, block 73 -> header + 2 rows (the timestamp
    // drops, the row count does not).
    assert_block_height(msg(Sender::Assistant, "m", 19, 18, body), 80, 3);
    // 48 cols: block 41 -> header + 3 rows.
    assert_block_height(msg(Sender::Assistant, "m", 19, 18, body), 48, 4);
    // …and his side wraps to the same COUNT here, two columns narrower: 39
    // against 41, because his block is the one with a box to pay for.
    assert_block_height(msg(Sender::User, "m", 19, 18, body), 48, 4);
}

#[test]
fn at_eighty_columns_the_timestamps_drop_and_the_content_does_not() {
    let v = vs(LinkState::Connected);
    assert_shape(&v, 80, H);
    let lines = text(&v, 80, H);
    let run_headers: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("claude") || l.trim_end().ends_with("you"))
        .collect();
    assert!(
        !run_headers.is_empty(),
        "run headers vanished at 80 columns"
    );
    for h in run_headers {
        assert!(
            !h.contains("19:18") && !h.contains("19:25"),
            "the timestamp must be the thing that drops, not the content: {h:?}"
        );
    }
    assert!(
        lines.iter().any(|l| l.contains("The frame is five bands")),
        "content was collapsed before metadata"
    );
}

// ── the ROOT: projects ──────────────────────────────────────────────────────

/// **THE ROOT FRAME — one scrolling surface of bands** (`0040` §1, `DEC-28`).
///
/// Pinned first, recent next, the projects, chrome last. The operator's own
/// item was *"Home gains its bands — pinned, recent, misc — per the design
/// doc. The build currently has projects only, and he noticed."*
///
/// Read the two band rows against the depth-2 list: they carry the summary, the
/// PROJECT they live in, a live state in words and an age — and no hash, which
/// is *"don't show me thread or task hash names"* applied where a project row
/// has no id to show either.
#[test]
fn the_root_is_one_surface_of_bands_at_100x24() {
    let v = projects_vs();
    assert_shape(&v, W, H);
    golden(
        &text(&v, W, H),
        &[
        "5 unread · 1 active · 2 queued · connected                                                          ",
        "  pinned · 4                                                                                        ",
        "▸   the threads daemon wire — living notes                                harness       idle      2m",
        "    see all — 3 more                                                                                ",
        "                                                                                                    ",
        "  recent · 9                                                                                        ",
        "    Fix the viewport defects and the resize repaint                       harness     active      2m",
        "    Answer the tax letter                                           miscellaneous       idle  1h 02m",
        "    see all — 7 more                                                                                ",
        "                                                                                                    ",
        "  projects · 7                                                                                      ",
        "    broomstick         ~/minds/broomstick                                   7 unread 69 thr       2m",
        "    harness            ~/minds/harness                                               64 thr   1h 18m",
        "    chess              ~/minds/chess                                        ? unread  ? thr        ?",
        "    see all — 4 more                                                                                ",
        "    + new project — type its name                                                                   ",
        "                                                                                                    ",
        "  archived (2)                                                                                      ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "  ? keys · → open · tab activity · o options · u queue · r rename · end newest · q quit             ",
    ],
    );
}

/// **THE VERY FIRST LAUNCH — the home surface with ONE EXTRA BAND** (`0040`
/// §10).
///
/// The doc puts the hint in the row between the header rule and the body,
/// which is the banner's own slot, and that is why the shell hands it over as
/// a `Banner` rather than as a row inside the root: it IS that band. The
/// consequence worth asserting is the SHAPE — one extra row, the whole surface
/// pushed down by exactly one, and nothing in the root itself rearranged. A
/// teaching line that reflowed the menu it was teaching would be teaching the
/// wrong menu.
#[test]
fn the_very_first_launch_adds_one_band_and_moves_nothing_else() {
    let mut v = projects_vs();
    let ordinary = text(&v, W, H);
    v.banner = Some(Banner {
        // Verbatim `store::FIRST_RUN_HINT`; `store`'s own test pins the text.
        text: "first run · → or click opens · ← goes back · click a title for its sheet · ? all ten keys"
            .into(),
        severity: Severity::Unknown,
    });
    assert_shape(&v, W, H);
    let first = text(&v, W, H);

    assert!(
        first[1].contains("first run · → or click opens"),
        "{:?}",
        first[1]
    );
    assert!(
        first[1].contains("? all ten keys"),
        "the hint is cropped at 100 columns, which is the width he runs: {:?}",
        first[1]
    );
    assert_eq!(
        first[0], ordinary[0],
        "the header is not the band's to move"
    );
    assert_eq!(
        &first[2..5],
        &ordinary[1..4],
        "the surface must be pushed down by exactly one row and not reflowed"
    );
    assert_eq!(
        first.last().unwrap(),
        ordinary.last().unwrap(),
        "the footer stays where his eye expects it"
    );
}

/// The root is read from the TOP, and the PROJECTS inside it are alphabetical.
/// `DIR-3`'s one direction is about a stream of things that ARRIVE; a project
/// list is a stable set he navigates by name, and re-ordering it by recency
/// would move a project out from under his hand every time he used another one.
///
/// The RECENT band is newest-first for the opposite reason and the doc draws it
/// so: it is a fixed-height shortcut read from its top, not a stream.
#[test]
fn the_root_is_anchored_at_its_top_and_a_project_with_no_index_says_so() {
    let v = projects_vs();
    let lines = text(&v, W, H);
    // The SURFACE starts on the row under the header — which is now its first
    // band, not its first project.
    assert_eq!(
        lines
            .iter()
            .skip(1)
            .position(|l| band_header_is(l, "pinned"))
            .map(|i| i + 1)
            .expect("the pinned band"),
        1,
        "the root must start on the row under the header"
    );
    let first = lines
        .iter()
        .position(|l| l.contains("broomstick"))
        .expect("the first project");
    // **AN UNREADABLE INDEX IS `? thr`, NEVER `0 thr`** — and never the words
    // `no index`, which the operator asked to be rid of. The distinction moved
    // from prose into `Known<usize>`; see `index::IndexState` for why deleting
    // it rather than replacing it would have hidden a real data-loss path.
    let chess = lines.iter().find(|l| l.contains("chess")).expect("row");
    assert!(chess.contains("? thr"), "{chess:?}");
    assert!(
        !chess.contains("0 thr"),
        "an unreadable index read as an empty one"
    );
    assert!(
        !chess.contains("no index"),
        "the words he asked to be rid of are still on the row: {chess:?}"
    );
    // **THE PIN TOTAL AND THE ACTIVE COUNT LEFT THE PROJECT ROW**, and it is
    // his own list that removed them: *"I want to see {N} thr, the time since
    // last event … and the num unread."* Three facts, and neither `★N` nor
    // `N act` is among them. Neither lost its only home — every pin is in the
    // `pinned` band directly above, and `★` is still drawn per row on the
    // depth-2 thread list.
    assert!(
        !lines[first].contains('★'),
        "the pin total came back to the project row: {:?}",
        lines[first]
    );
    // THE POSITIVE HALF: the three facts he DID name are on the row, so the
    // absence above means "removed" and not "the row stopped drawing".
    for want in ["69 thr", "7 unread", "2m"] {
        assert!(
            lines[first].contains(want),
            "the project row lost `{want}`: {:?}",
            lines[first]
        );
    }
    // **AND THE PATHS ARE IN A COLUMN**, which is the layout correction he
    // asked for: "I want the directories to be aligned on each other."
    let dirs: Vec<usize> = lines
        .iter()
        .filter(|l| l.contains("~/minds/"))
        .map(|l| l.find("~/minds/").expect("just matched"))
        .collect();
    assert!(dirs.len() >= 3, "too few project rows to prove alignment");
    assert!(
        dirs.windows(2).all(|w| w[0] == w[1]),
        "the project directories are not aligned: {dirs:?}"
    );
    // Newest first in the band, oldest last — the jump-back-in order.
    let a = lines
        .iter()
        .position(|l| l.contains("Fix the viewport"))
        .expect("recent row");
    let b = lines
        .iter()
        .position(|l| l.contains("Answer the tax letter"))
        .expect("recent row");
    assert!(a < b, "the recent band is not newest-first");
}

// ── the thread list ─────────────────────────────────────────────────────────

#[test]
fn the_thread_list_at_100x24() {
    let v = list_vs();
    assert_shape(&v, W, H);
    golden(
        &text(&v, W, H),
        &[
        "harness · 2 threads · 1 active · 2 queued this project · 5 unread elsewhere · connected · acct ?    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "                                                                                                    ",
        "  rebuild the index reader                                                 idle  ▲0   •7      1h 18m",
        "▸ untitled                                                               active  ▲2               2m",
        "  ? keys · → open · ← projects · o options · u queue · r rename · tab activity · end newest · q quit",
    ],
    );
}

/// `DIR-3`, applied to the list: the thread he last touched is the one nearest
/// his hands, and the cursor starts there.
#[test]
fn the_newest_thread_is_at_the_bottom_and_the_cursor_is_on_it() {
    let v = list_vs();
    let lines = text(&v, W, H);
    // **LOCATED BY WHAT THE ROW SAYS, which is now its NAME.** These two
    // handles were `bcccfa79` and `57db2ec9` until `0076` took the hex id off
    // the row; a test that finds a row by a string the row no longer draws is
    // a test that cannot fail for the right reason.
    let old = lines
        .iter()
        .position(|l| l.contains("rebuild the index reader"))
        .unwrap();
    let new = lines
        .iter()
        .position(|l| l.contains("untitled"))
        .unwrap();
    assert!(old < new, "newest at the bottom");
    assert!(
        lines[new].starts_with("▸ "),
        "the cursor marks the selected row: {:?}",
        lines[new]
    );
    assert!(
        !lines[old].starts_with('▸'),
        "only one row is selected: {:?}",
        lines[old]
    );
}

/// The cwd is the only thing on a row he can recognise a thread by, so it is
/// the last thing to go — the metadata on the right is what drops, in order.
#[test]
fn a_narrow_list_drops_metadata_before_it_drops_the_cwd() {
    let v = list_vs();
    let mut last_tail = usize::MAX;
    for w in [100u16, 80, 60, 48, 40] {
        assert_shape(&v, w, H);
        let lines = text(&v, w, H);
        let row = lines
            .iter()
            .find(|l| l.contains("untitled"))
            .unwrap_or_else(|| panic!("the row vanished at {w} columns: {lines:#?}"));
        // (TUI-P1a: the row's label is its NAME, `untitled` here, never its
        // path — and the label is still the last thing to go.)
        assert!(
            row.contains("untitled"),
            "the row's label must survive at {w}: {row:?}"
        );
        assert!(
            row.contains("active"),
            "a thread's state is never dropped at {w}: {row:?}"
        );
        // The right-hand facts shrink monotonically as the row narrows — they
        // are what gives way, and they give way in order.
        let tail = row.trim_end().len() - row.trim_end().rfind("active").unwrap();
        assert!(
            tail <= last_tail,
            "the metadata grew as the row narrowed at {w}: {row:?}"
        );
        last_tail = tail;
    }
    // At the narrowest the AGE has gone and the state has not.
    let narrow = text(&v, 40, H)
        .into_iter()
        .find(|l| l.contains("untitled"))
        .unwrap();
    assert!(
        !narrow.contains("2m"),
        "the age must be the first thing to go: {narrow:?}"
    );
}

/// **WHICH DOORS A CLICK STILL OPENS, AND WHICH ONES ROUND SIX CLOSED.**
///
/// This is the INVERTED test. It used to be
/// `every_other_affordance_still_activates_on_a_click` and it named five doors
/// that survived on his own affordance list — *"buttons and explicit
/// affordances (the queue count, sheet action rows, play buttons, expanders)
/// still activate on click."* That list is retracted: *"Clicking should only
/// move pointers and click buttons (which we don't even have yet - nothing in
/// this app counts as that)."*
///
/// So the rule is now a PROPERTY rather than a list: **a click never changes
/// the mode and never mutates.** It repositions, it reveals in place, and it
/// leaves. Each row below says which side of that line it is on and why, so a
/// future reader can overturn any single call in one edit rather than
/// re-deriving the whole judgment.
///
/// **THREE ROUTES ARE CLOSED HERE AND EACH HAS A VERIFIED KEY DOOR**, checked
/// against `BINDINGS` rather than assumed — that is the condition on closing
/// one at all, because a route deleted with nothing behind it deletes the ROOM
/// and is worse than the gesture he complained about.
///
/// **ONE ROUTE SURVIVES AGAINST THE RULE**: the project sheet, which has no
/// general keyboard door at all. It is reported, not hidden.
///
/// FORCED RED by flipping any one arm's `Via` in `hit.rs`: each assertion names
/// which door moved.
#[test]
fn a_click_never_enters_and_the_doors_it_lost_have_keys() {
    // ── CLOSED: the sheet's act rows. Some open a FIELD (a mode change);
    //    the rest mutate. `→`/`enter` over the sheet's own cursor is the door,
    //    and `↑`/`↓` walk to every row.
    assert_eq!(
        activation(Target::SheetRow(3), Mode::Sheet, Via::Click),
        None,
        "a click still presses a sheet act row"
    );
    assert_eq!(
        activation(Target::SheetRow(3), Mode::Sheet, Via::Key),
        Some(Intent::SheetAct(3)),
        "the sheet's KEY door died with its click door"
    );
    assert_eq!(intent(Key::Right, Mode::Sheet), Intent::Activate);
    assert_eq!(intent(Key::Enter, Mode::Sheet), Intent::Activate);

    // ── CLOSED: the archived room. Un-archiving is a mutation, not a pointer
    //    move. `→`/`enter` is the door.
    assert_eq!(
        activation(Target::ArchivedRow(1), Mode::Archived, Via::Click),
        None,
        "a click still un-archives"
    );
    assert_eq!(
        activation(Target::ArchivedRow(1), Mode::Archived, Via::Key),
        Some(Intent::Unarchive(1)),
        "the archived room's KEY door died with its click door"
    );
    assert_eq!(intent(Key::Right, Mode::Archived), Intent::Activate);

    // ── CLOSED: the queue badge. Raising the queue view is ENTERING a
    //    surface. `u` and `^O` carry it in every body mode. (`^O` and not `^U`:
    //    `^U` is PageUp in the list modes and `Compose(KillToStart)` in the
    //    box, so the letter-for-letter move was taken and `0128` named the
    //    substitute.)
    assert_eq!(
        activation(Target::QueueBadge, Mode::List, Via::Click),
        None,
        "a click still raises the queue overlay"
    );
    for m in tui_render::BODY_MODES {
        assert_eq!(
            intent(Key::Ctrl('o'), *m),
            Intent::QueueOverlay,
            "the queue has no key door in {m:?} and its click door is gone"
        );
    }
    assert_eq!(intent(Key::Char('u'), Mode::List), Intent::QueueOverlay);

    // ── CLOSED: a thread's name. It DRILLED; now it selects its row. The
    //    thread is reached with `→` and its options with `o`.
    assert_eq!(
        activation(Target::ThreadTitle(2), Mode::List, Via::Click),
        Some(Intent::Select(2)),
        "a click on a thread's name is not a plain row click"
    );
    assert_eq!(intent(Key::Char('o'), Mode::List), Intent::Options);
    assert_eq!(intent(Key::Right, Mode::List), Intent::Activate);

    // ── AND NOW CLOSED TOO, WHICH COMPLETES THE LAW: a project's name.
    //
    // This was the one route left open, and the reason was measured rather
    // than preferred — the project sheet had NO keyboard door, so taking the
    // click would have deleted the ROOM. The guard that stood here said: *"a
    // key now reaches the project sheet — narrow the ProjectName arm to
    // Via::Key in the same edit and delete this assertion"*, and that is
    // exactly what happened.
    //
    // **THE DOOR IS `o`, AND IT IS NOT A NEW KEY.** `Intent::Options` already
    // opened a THREAD's sheet; the shell now aims it at `Target::ProjectName`
    // when the row under the cursor is a project. So no `Binding` produces
    // `Intent::OpenProjectSheet` even now — the guard's own test would still
    // pass — and asserting the KEY ARM of this table is what actually proves
    // the room is reachable.
    assert_eq!(
        activation(Target::ProjectName(2), Mode::Projects, Via::Click),
        Some(Intent::Select(2)),
        "a click on a project's name still opens its sheet"
    );
    assert_eq!(
        activation(Target::ProjectName(2), Mode::Projects, Via::Key),
        Some(Intent::OpenProjectSheet(2)),
        "the project sheet's door is gone and nothing replaced it"
    );
    assert_eq!(intent(Key::Char('o'), Mode::Projects), Intent::Options);

    // ── STILL OPEN, and each for a reason that is not "it is a button" ────
    //
    // THE EXPANDER reveals the row it is drawn on, IN PLACE: the mode does not
    // change and the subject does not change. It is also the transcript's only
    // mouse door at all — `Target::Block(_) + Click` is deliberately `None` —
    // so closing it would make the chat unreachable by pointer.
    assert_eq!(
        activation(Target::Expander(5), Mode::Thread(Region::Chat), Via::Click),
        Some(Intent::ExpandBlock(5)),
        "the expander lost its door"
    );
    // THE `↓ N new` CHIP is a SCROLL — "a scroll still scrolls" — plus the
    // follow re-arm that went with `G`'s letter when `G` was retired.
    assert_eq!(
        activation(Target::NewChip, Mode::Thread(Region::Chat), Via::Click),
        Some(Intent::Bottom),
        "the new chip lost its door"
    );
    // MIC MODE'S THREE PER-ROW BUTTONS. These stay on a REACHABILITY
    // argument, not an affordance one: the keyboard spellings reach only the
    // LAST capture and the FIRST failure (`MicPlayLast`, `MicRetryFirst`,
    // `MicDropFirst`), so closing these would strand queue rows 2..n with no
    // way to act on them. A deletion that drops reachability is not one we are
    // allowed to make.
    assert_eq!(
        activation(Target::MicPlay(0), Mode::Mic, Via::Click),
        Some(Intent::MicPlay(0))
    );
    assert_eq!(
        activation(Target::MicRetry(0), Mode::Queue, Via::Click),
        Some(Intent::MicRetry(0))
    );
    assert_eq!(
        activation(Target::MicDrop(0), Mode::Mic, Via::Click),
        Some(Intent::MicDrop(0))
    );
    // AN OVERLAY is DISMISSED by a click inside it. That is leaving, not
    // entering, and it is the mouse's only way out.
    assert_eq!(
        activation(Target::Overlay, Mode::Help, Via::Click),
        Some(Intent::Back)
    );

    // ── AND THE OTHER HALF OF THE LAW: a plain row still selects ──────────
    assert_eq!(
        activation(Target::RootRow(4), Mode::Projects, Via::Click),
        Some(Intent::Select(4))
    );
    assert_eq!(
        activation(Target::ThreadRow(4), Mode::List, Via::Click),
        Some(Intent::Select(4))
    );
    // A band header, a `show more` foot, `+ new project` and `archived (N)`
    // are all claimed as `Target::RootRow(i)`, so a click SELECTS them and
    // `→`/`enter` is what opens them. Recorded rather than checked per row:
    // it is easy to assume otherwise from how those rows read on screen.
    //
    // A transcript row stays silent to the mouse (`CON-4`), and empty space
    // is a free miss.
    assert_eq!(activation(Target::Block(0), Mode::Thread(Region::Chat), Via::Click), None);
    assert_eq!(activation(Target::Empty, Mode::Projects, Via::Click), None);
}

/// **THE PROJECT LIST AT 80 AND AT 60**, which is where the collapse floor
/// he never sees is actually spent.
///
/// `0076` re-derived `LIST_LEFT_FLOOR` when the eight-character id left the
/// row — `2 + 8 + 1 + 12` became `2 + 20` — and there was NO list golden at
/// either of `0040` §11's two named widths to measure that against. A floor
/// changed against no frame is a number changed against an argument.
///
/// What these two frames say, read line by line. At 80 the row carries its
/// name, its state, its badge, its unread dot and its age. At 60 the UNREAD
/// DOT and the QUEUE BADGE have gone and the AGE has not — which is the rank
/// order exactly: the header already carries both of those counts, and a list
/// sorted by recency that stops showing recency is a list ordered by a secret.
/// **And on both frames the NAME is whole.** Before `0076` the leftmost 9 of
/// those columns were an id and a space; they are the thread's name now, and
/// at 60 that is the difference between reading `rebuild the index reader` and
/// reading `bcccfa79 rebuild the …`.
#[test]
fn the_thread_list_collapses_at_eighty_and_sixty() {
    let v = list_vs();
    assert_shape(&v, 80, H);
    assert_shape(&v, 60, H);
    golden(
        &text(&v, 80, H),
        &[
        "harness · 2 threads · 1 active · 2 queued this project · 5 unread elsewhere · a…",
        "                                                                                ",
        "                                                                                ",
        "                                                                                ",
        "                                                                                ",
        "                                                                                ",
        "                                                                                ",
        "                                                                                ",
        "                                                                                ",
        "                                                                                ",
        "                                                                                ",
        "                                                                                ",
        "                                                                                ",
        "                                                                                ",
        "                                                                                ",
        "                                                                                ",
        "                                                                                ",
        "                                                                                ",
        "                                                                                ",
        "                                                                                ",
        "                                                                                ",
        "  rebuild the index reader                             idle  ▲0   •7      1h 18m",
        "▸ untitled                                           active  ▲2               2m",
        "  ? keys · → open · ← projects · o options · u queue · r rename · tab activity  ",
    ],
    );
    golden(
        &text(&v, 60, H),
        &[
        "harness · 2 threads · 1 active · 2 queued this project · 5 …",
        "                                                            ",
        "                                                            ",
        "                                                            ",
        "                                                            ",
        "                                                            ",
        "                                                            ",
        "                                                            ",
        "                                                            ",
        "                                                            ",
        "                                                            ",
        "                                                            ",
        "                                                            ",
        "                                                            ",
        "                                                            ",
        "                                                            ",
        "                                                            ",
        "                                                            ",
        "                                                            ",
        "                                                            ",
        "                                                            ",
        "  rebuild the index reader                     idle   1h 18m",
        "▸ untitled                                   active       2m",
        "  ? keys · → open · ← projects · o options · u queue        ",
    ],
    );
}

/// **A ROW WITH NO NAME IS `untitled`, NEVER ITS PATH** (TUI-P1a, checklist
/// TC-02). This was `a_cwd_is_never_tidied`, which pinned the cwd as the
/// row's fallback label; his verdict on that fallback: *"if a thread wasn't
/// named, then it just shows the project path, which is the worst possible
/// thing it could show."* The fixture row still CARRIES the cwd
/// (`~/.aui/mythcellaneous`), so the absence below is a choice and not a row
/// with nothing to draw — and the named row beside it still draws its name.
///
/// FORCED (report): restore the cwd rung in `select::thread_label` → red.
#[test]
fn an_unnamed_row_is_untitled_and_never_its_path() {
    let v = list_vs();
    assert_eq!(v.list.rows[1].cwd, "~/.aui/mythcellaneous", "witness: the row has a path to draw");
    let lines = text(&v, W, H);
    assert!(
        !lines.iter().any(|l| l.contains("mythcellaneous")),
        "an unnamed row drew its path: {lines:#?}"
    );
    assert!(lines.iter().any(|l| l.contains("▸ untitled")), "{lines:#?}");
    assert!(lines.iter().any(|l| l.contains("rebuild the index reader")), "{lines:#?}");
}

// ── the band heights, which the shell now reads instead of guessing ─────────

/// **`bands` and `frame` are ONE derivation, and this is the test that says
/// so.** The shell pages by `bands(..).content`; if that number ever stopped
/// describing the frame beside it, `PageDown` would move by a different amount
/// than the screen holds and nothing would say a word.
///
/// The three facts it pins are the three the shell's old private copy
/// (`height - 2 - COMPOSER_MIN_ROWS`) got wrong or would soon have got wrong.
#[test]
fn the_band_heights_describe_the_frame_beside_them() {
    let banner = Banner {
        text: "the link is down — retrying in 2s".into(),
        severity: Severity::Critical,
    };

    // 1. THE BANDS ARE THE WHOLE FRAME, in every mode, banner or not.
    for mode in [
        Mode::List,
        Mode::Activity,
        // **EVERY REGION OF A THREAD IS ITS OWN SCREEN TO THE SHAPE LAW.** The
        // composer band draws the client queue's rows now and the footer
        // prints a different right-hand half per region, so a shape check that
        // only ever saw one of them would not have seen either change.
        Mode::Thread(Region::Composer),
        Mode::Thread(Region::Chat),
        Mode::Thread(Region::Queue),
        Mode::Help,
        Mode::Queue,
    ] {
        for with_banner in [false, true] {
            let mut v = vs(LinkState::Connected);
            v.mode = mode;
            v.banner = with_banner.then(|| banner.clone());
            let b = tui_render::bands(&v, W, H);
            assert_eq!(
                b.header + b.banner + b.content + b.composer + b.footer,
                H as usize,
                "{mode:?} (banner {with_banner}) does not add up to the frame: {b:?}"
            );
            assert_shape(&v, W, H);
        }
    }

    // 2. A BANNER COSTS A CONTENT ROW. This is the fact the shell's copy
    //    missed: it never subtracted the banner, so with the link down it
    //    paged by one row more than the transcript had.
    let mut v = vs(LinkState::Connected);
    let without = tui_render::bands(&v, W, H).content;
    v.banner = Some(banner.clone());
    let b = tui_render::bands(&v, W, H);
    assert_eq!(b.banner, 1, "a banner with room under it is one row");
    assert_eq!(
        b.content,
        without - 1,
        "the banner must come out of the content, not out of nothing"
    );
    // …and it is drawn exactly where `bands` puts it, with the composer's
    // border on the first row after the content.
    let lines = text(&v, W, H);
    assert!(
        lines[b.header].contains("the link is down"),
        "the banner is not on the row bands allotted it: {:?}",
        lines[b.header]
    );
    assert!(
        lines[b.header + b.banner + b.content].starts_with("──"),
        "the composer does not start where bands said the content ends: {:?}",
        lines[b.header + b.banner + b.content]
    );

    // 3. AN OVERLAY TAKES THE WHOLE BODY — no composer under it. The shell's
    //    copy keyed off the view UNDER the overlay and subtracted a composer
    //    that is not on the screen.
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Help;
    let b = tui_render::bands(&v, W, H);
    assert_eq!(b.composer, 0, "an overlay has no composer beneath it");
    assert_eq!(b.content, H as usize - 2, "an overlay takes the whole body");

    // 4. Below the minimum there are no bands at all, because the too-small
    //    frame draws none of them — and a pager must not page a frame that is
    //    one sentence.
    let v = vs(LinkState::Connected);
    assert_eq!(tui_render::bands(&v, 10, 4), tui_render::Bands::default());
}

/// **THE CLIENT QUEUE IS DRAWN WHERE HE CAN REACH IT, AND IT IS A REGION OF
/// THE SCREEN AND NOT A COUNT ON A BORDER.**
///
/// *"just make the queue accessible by click or by navigating above or below
/// the textarea."* Before this, what the composer was holding existed on screen
/// only as `2 held here` on the band's border — a number he could notpoint
/// at. Now the rows are in the band, above the text, drawn by the same
/// `client_row` mic mode and the queue view use.
///
/// FORCED: return `0` from `queue_band_rows` and the row assertion goes red;
/// leave it and remove the `+ q` from `composer_height` and `assert_shape`
/// goes red instead, because the band would draw more rows than it claimed.
#[test]
fn the_client_queue_is_drawn_in_the_composer_band_and_can_be_clicked() {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Thread(Region::Composer);
    v.composer.buf = tui_render::compose::Composer::from_text("first queued");
    assert!(v.composer.buf.queue());
    v.composer.buf.insert_paste("second queued");
    assert!(v.composer.buf.queue());
    assert_shape(&v, W, H);
    let lines = text(&v, W, H);
    for want in ["first queued", "second queued"] {
        assert_eq!(
            lines.iter().filter(|l| l.contains(want)).count(),
            1,
            "{want} is not drawn as a row of the band: {lines:#?}"
        );
    }
    // **AND THEY ARE HIT-TESTED**, which is the "by click" half of his ask.
    let (_, hits) = frame_with_hits(&v, now(), W, H);
    let row = lines
        .iter()
        .position(|l| l.contains("second queued"))
        .expect("a queued row");
    assert_eq!(
        hits.hit(GUTTER as u16 + 4, row as u16),
        Target::MicRow(1),
        "a queued row in the band is not clickable"
    );
    // **THE CURSOR IS PAINTED ONLY WHERE THE QUEUE HOLDS THE SELECTION.** A
    // highlight on a band the arrows are not acting on is a lie about where
    // the keyboard is.
    // **THE COMPARISON IS OF THE ROW, NOT OF THE FRAME.** A whole-frame
    // comparison would pass on the footer alone — the legend legitimately
    // changes with the region — and would have said "the highlight works"
    // with the highlight deleted. Asking the cursor's own row is the only
    // form of this assertion that can fail for the right reason.
    let painted_row = |v: &ViewState, want: &str| -> String {
        let lines = frame(v, now(), W, H);
        let i = text(v, W, H)
            .iter()
            .position(|l| l.contains(want))
            .expect("the row is not on screen");
        format!("{:?}", lines[i])
    };
    let unfocused = painted_row(&v, "second queued");
    let mut sel = v.clone();
    sel.mode = Mode::Thread(Region::Queue);
    sel.queue_cursor = 1;
    assert_shape(&sel, W, H);
    assert_ne!(
        painted_row(&sel, "second queued"),
        unfocused,
        "the queue's cursor row is not marked when the queue holds the selection"
    );
    // …and the row the cursor is NOT on is painted exactly as before.
    assert_eq!(
        painted_row(&sel, "first queued"),
        painted_row(&v, "first queued"),
        "a row the cursor is not on was marked too"
    );
    // …and it is the row's STYLE that changed, not its words. (The footer
    // legitimately changes with the region — that is the per-section legend —
    // so the comparison is of the band's own rows.)
    let band = |v: &ViewState| -> Vec<String> {
        text(v, W, H)
            .into_iter()
            .filter(|l| l.contains("queued"))
            .collect()
    };
    assert_eq!(
        band(&sel),
        band(&v),
        "the highlight changed the queued rows' text as well as their style"
    );
}

/// **EACH SECTION SHOWS ITS OWN KEYS, WITH THE INVARIANT ONES IN THE SAME
/// COLUMNS** — on the drawn frame, which is where he sees it.
///
/// *"Keep the invariant ones at the left so its easy to see that they stay the
/// same."* So the assertion is about COLUMNS: the left run must be identical
/// text, character for character, in every region.
///
/// FORCED: make `footer_split` return everything as variable and the shared
/// prefix collapses to nothing; make it return everything as stable and the
/// "differs" assertion goes red.
#[test]
fn the_footer_shows_each_bands_own_keys_over_a_left_half_that_does_not_move() {
    let mut v = vs(LinkState::Connected);
    v.composer.buf = tui_render::compose::Composer::from_text("held");
    assert!(v.composer.buf.queue());
    let footer = |v: &ViewState| -> String {
        let lines = text(v, W, H);
        lines.last().cloned().unwrap_or_default()
    };
    v.mode = Mode::Thread(Region::Composer);
    let box_row = footer(&v);
    v.mode = Mode::Thread(Region::Chat);
    let chat_row = footer(&v);
    v.mode = Mode::Thread(Region::Queue);
    let queue_row = footer(&v);
    assert_ne!(box_row, chat_row, "every band prints the same legend");
    assert_ne!(chat_row, queue_row);
    // The STABLE half is the same COLUMNS: the common prefix reaches the ┊.
    let mark = box_row.find('┊').expect("no stable/variable mark in the box's footer");
    for (name, row) in [("chat", &chat_row), ("queue", &queue_row)] {
        let at = row.find('┊').unwrap_or_else(|| panic!("no mark in {name}'s footer: {row:?}"));
        assert_eq!(
            at, mark,
            "{name}'s invariant keys end in a different column: {row:?}"
        );
        assert_eq!(
            &row[..at],
            &box_row[..mark],
            "{name}'s invariant keys are not the same keys"
        );
    }
    // …and the variable half really is that band's own.
    assert!(chat_row.contains("end"), "the chat's own keys: {chat_row:?}");
    assert!(box_row.contains("mic"), "the box's own keys: {box_row:?}");
}

// ── follow mode and the chip ────────────────────────────────────────────────

#[test]
fn the_new_chip_is_pinned_to_the_bottom_edge_while_follow_is_off() {
    let mut v = vs(LinkState::Connected);
    v.viewport.follow = false;
    v.viewport.from_bottom = 3;
    v.viewport.new_since = 4;
    assert_shape(&v, W, H);
    let lines = text(&v, W, H);
    // The chip sits on the LAST transcript row, not in the footer.
    let chip = lines
        .iter()
        .position(|l| l.contains("↓ 4 new"))
        .expect("no chip");
    // **THE ROW UNDER IT IS THE COMPOSER'S TOP BORDER**, which no longer says
    // the word `composer` — `T-41` took the band's label and its legend off.
    // The band geometry says where the transcript ends, which is the fact this
    // was always reaching for through the label.
    let b = tui_render::bands(&v, W, H);
    assert_eq!(
        chip,
        b.header + b.banner + b.content - 1,
        "the chip must be pinned to the viewport's bottom edge"
    );
    // **THE CHIP NAMES ITS CLICK FIRST.** `G` keeps working and is still on
    // the chip's row as `end`; the chip is now the affordance, which is how a
    // key gives up its letter without giving up its job.
    assert!(
        lines[chip].contains("click here"),
        "the chip must name the gesture that re-arms follow"
    );
}

/// **THE VIEWPORT DOES NOT MOVE WHEN CONTENT ARRIVES.** The render half of the
/// follow-mode promise: same `from_bottom`, more blocks, same rows on screen.
#[test]
fn an_arrival_while_scrolled_up_does_not_move_what_is_on_screen() {
    let mut before = vs(LinkState::Connected);
    // The band has to be FULL first. With content shorter than the viewport
    // everything is on screen and there is nothing follow-mode could protect —
    // which is a real property and the wrong fixture for this law.
    for i in 0..30 {
        before
            .blocks
            .insert(0, lane('·', Severity::Calm, &format!("earlier {i}"), None));
    }
    before.viewport.follow = false;
    before.viewport.from_bottom = 2;
    let shot_before = text(&before, W, H);
    assert!(
        !shot_before[1].trim().is_empty(),
        "the fixture must fill the transcript band, or this law is untestable"
    );

    let mut after = before.clone();
    for i in 0..5 {
        after.blocks.push(lane(
            '·',
            Severity::Calm,
            &format!("something arrived while he was reading, #{i}"),
            None,
        ));
    }
    after.viewport.new_since = 5;
    let shot_after = text(&after, W, H);

    // Everything but the chip row is identical. The chip is the ONE thing an
    // arrival is allowed to change while he is scrolled up.
    let chip = shot_after
        .iter()
        .position(|l| l.contains("↓ 5 new"))
        .expect("the chip must appear");
    for (i, (a, b)) in shot_before.iter().zip(&shot_after).enumerate() {
        if i == chip {
            continue;
        }
        assert_eq!(a, b, "row {i} moved under him:\n{a:?}\n{b:?}");
    }
    // …and the same content is still there, at the same height.
    assert!(shot_after
        .iter()
        .any(|l| l.contains("The frame is five bands")));
    assert!(
        !shot_after
            .iter()
            .any(|l| l.contains("arrived while he was reading")),
        "an arrival must not scroll itself into view"
    );
}

// ── the honesty rules, each as its own assertion ────────────────────────────

/// **THE DISCONNECTED-RENDER RULE** (`DEC-13`). *"Never render stale as
/// current"*, NOT *"render nothing"* — and this deviates from `$ threads`'
/// full-screen down page on purpose: that board is all current-state meters,
/// where nothing on screen survives the daemon; a chat is mostly history, and
/// history remains true.
#[test]
fn a_disconnected_face_keeps_its_transcript_and_greys_only_the_meters() {
    let mut v = vs(LinkState::Disconnected {
        reason: "the daemon closed the socket".into(),
        retry_at: Some(now() + TimeDelta::seconds(4)),
    });
    v.header.queued = Known::Unknown;
    v.header.account = Known::Unknown;
    v.banner = Some(Banner {
        text: "the daemon is not answering — everything below is HISTORY and still true".into(),
        severity: Severity::Critical,
    });
    const WIDE: u16 = 150;
    assert_shape(&v, WIDE, H);
    let lines = text(&v, WIDE, H);

    // TUI-P1a (TC-04): on a thread page the HEADER is the name and status and
    // nothing else, so the outage is the BANNER's to say — its whole row, with
    // the next retry — and no meter on the header can render a zero at all.
    assert!(
        lines[0].trim_end() == "answer the tax letter · active",
        "the thread page's header carries more than its name and status: {:?}",
        lines[0]
    );
    assert!(
        !lines[0].contains("0 queued") && !lines[0].contains("disconnected"),
        "{:?}",
        lines[0]
    );
    assert!(
        lines[1].contains("retry in"),
        "the banner must name the NEXT RETRY, not just the failure: {:?}",
        lines[1]
    );
    assert!(
        lines[1].contains("HISTORY"),
        "the banner sits above the content it qualifies: {:?}",
        lines[1]
    );
    assert!(
        lines.iter().any(|l| l.contains("The frame is five bands")),
        "the transcript is HISTORY and stays true across a disconnect"
    );
}

/// **THE BANNER'S CLAIM ABOUT TIME IS TRUE ON EVERY FRAME, NOT ONLY ON THE ONE
/// THAT PRODUCED IT.**
///
/// The defect this replaces: `LinkState::Disconnected` carried
/// `next_retry_in_ms`, a DURATION measured at the instant of the drop and then
/// printed as a live promise. Nothing anywhere tracked how long the link had
/// actually been down, so a face that had been disconnected for an hour was
/// still saying "retry in 4s" — the stale-`2m ago` defect `DEC-13` exists to
/// prevent, arriving on the one meter the face had not made honest.
///
/// **The proof is that NOTHING in this `ViewState` changes between the two
/// frames.** The same value is drawn twice, four minutes apart on a clock this
/// test owns, and the numbers move. There is no sleep here and no wall clock:
/// a duration that is stored cannot pass this, and a duration that is
/// COMPUTED cannot fail it.
#[test]
fn the_down_duration_and_the_countdown_are_recomputed_on_every_frame() {
    const WIDE: u16 = 160;
    // Down since `now() - 41s`, with the next attempt due at `now() + 4s` —
    // `0040` §9's own frame.
    let mut v = vs(LinkState::Disconnected {
        reason: "the daemon closed the socket".into(),
        retry_at: Some(now() + TimeDelta::seconds(4)),
    });
    // The banner text as `store::banner` builds it — carrying NO number, which
    // is the half of the fix that lives in the shell.
    v.banner = Some(Banner {
        text: "the daemon is not answering — everything below is HISTORY and still true".into(),
        severity: Severity::Critical,
    });
    // TUI-P1a (TC-04): the THREAD page's header is its name and status only,
    // so the header's link/queue/account machinery is exercised on the LIST
    // header, which still draws every part.
    v.mode = Mode::List;

    let first = text_at(&v, now(), WIDE, H);
    assert!(first[0].contains("down 41s"), "{:?}", first[0]);
    assert!(first[0].contains("retry in 4s"), "{:?}", first[0]);
    assert!(
        first[1].contains("down 41s"),
        "the banner leads with the age: {:?}",
        first[1]
    );
    assert!(first[1].contains("retry in 4s"), "{:?}", first[1]);

    // Four minutes later, with the ViewState untouched.
    let later = text_at(&v, now() + TimeDelta::minutes(4), WIDE, H);
    assert!(
        later[0].contains("down 4m"),
        "the age is frozen at the instant of the drop: {:?}",
        later[0]
    );
    assert!(
        later[1].contains("down 4m"),
        "the banner's age is frozen at the projection that built it: {:?}",
        later[1]
    );
    assert!(
        !later[0].contains("down 41s") && !later[1].contains("down 41s"),
        "the face is still showing the second the socket died: {:?}",
        later
    );
    // The deadline PASSED four minutes ago, so the countdown may not still be
    // promising four seconds — and may not have counted into the negative.
    assert!(
        !later[0].contains("retry in 4s") && !later[1].contains("retry in 4s"),
        "a promise the clock has already falsified: {:?}",
        later
    );
    assert!(
        later[0].contains("retrying now"),
        "a countdown past its deadline must say what is happening: {:?}",
        later[0]
    );
    assert!(
        !later[0].contains("retry in -") && !later[1].contains("retry in -"),
        "the countdown ran past zero: {:?}",
        later
    );
}

/// **THE AGE SURVIVES THE RECONNECT LADDER'S `Connecting` HOP.**
///
/// The transport alternates `Disconnected` → `Connecting { attempt: n }` →
/// `Disconnected` for as long as the daemon is gone. The age is carried on
/// `Link` rather than inside the `Disconnected` variant precisely so it spans
/// that alternation, and this is the assertion that it does: a face mid-ladder
/// still knows the outage is four minutes old, which is the fact that tells a
/// blip from a dead daemon.
#[test]
fn a_reconnect_attempt_does_not_restart_the_outage_clock() {
    const WIDE: u16 = 160;
    let mut v = vs(LinkState::Connecting { attempt: 7 });
    // TUI-P1a (TC-04): the THREAD page's header is its name and status only,
    // so the header's link/queue/account machinery is exercised on the LIST
    // header, which still draws every part.
    v.mode = Mode::List;
    v.header.link.down_since = Some(now() - TimeDelta::minutes(4));
    let lines = text_at(&v, now(), WIDE, H);
    assert!(
        lines[0].contains("down 4m"),
        "an attempt in flight forgot how long the daemon has been gone: {:?}",
        lines[0]
    );
}

/// A link that is UP states no duration at all. There is nothing to be down
/// for, and a "down 0s" on a healthy face is an invented zero (`DEC-13`).
#[test]
fn a_healthy_link_says_nothing_about_duration() {
    let lines = text(&vs(LinkState::Connected), 160, H);
    assert!(!lines[0].contains("down "), "{:?}", lines[0]);
}

#[test]
fn unknowns_render_as_question_marks_and_never_as_zeroes() {
    let mut v = vs(LinkState::Connected);
    // TUI-P1a (TC-04): the THREAD page's header is its name and status only,
    // so the header's link/queue/account machinery is exercised on the LIST
    // header, which still draws every part.
    v.mode = Mode::List;
    v.header.queued = Known::Unknown;
    v.header.account = Known::Unknown;
    let lines = text(&v, 150, H);
    assert!(lines[0].contains("? queued"), "{:?}", lines[0]);
    assert!(
        !lines[0].contains("0 queued"),
        "an unknown queue depth rendered as zero"
    );
    assert!(
        !lines[0].contains('▲'),
        "the glyph came back: {:?}",
        lines[0]
    );
    assert!(lines[0].contains("acct ?"), "{:?}", lines[0]);

    // And on a LIST row: an unknown queue is `▲?`, a known zero is `▲0`, and
    // the two are different words on screen.
    let mut l = list_vs();
    l.list.rows[0].queued = Known::Unknown;
    let rows = text(&l, W, H);
    let unknown = rows
        .iter()
        .find(|r| r.contains("rebuild the index reader"))
        .unwrap();
    let known = rows.iter().find(|r| r.contains("untitled")).unwrap();
    assert!(unknown.contains("▲?"), "{unknown:?}");
    assert!(known.contains("▲2"), "{known:?}");
}

/// **THE QUEUE BADGE IS ALWAYS ON SCREEN** (`DIR-6`, first requirement), and
/// its SCOPE travels with it, because a window's sum rendered bare reads as a
/// fleet total.
#[test]
fn the_queue_badge_is_on_every_screen_and_says_what_it_counts() {
    // TUI-P1a (TC-04/TC-30): the THREAD page is no longer here. His newer
    // words — *"The only thing I want to see at the top of the page, is the
    // name of the thread and whether it's active or idle"* — take the count
    // off that one header; the queue overlay (`^O`) still shows it.
    let thread = text(&vs(LinkState::Connected), 150, H);
    assert!(!thread[0].contains("queued"), "the thread page's header still counts the queue: {:?}", thread[0]);
    for (name, v) in [
        ("list", list_vs()),
        ("activity", {
            let mut a = vs(LinkState::Connected);
            a.mode = Mode::Activity;
            a
        }),
        ("help", {
            let mut a = vs(LinkState::Connected);
            a.mode = Mode::Help;
            a
        }),
        ("queue", {
            let mut a = vs(LinkState::Connected);
            a.mode = Mode::Queue;
            a
        }),
    ] {
        let lines = text(&v, 150, H);
        // **RE-KEYED FROM `▲2` TO `2 queued`** (`0072`). The glyph went
        // because the operator could not read it — *"I don't know what ▲1
        // means so let's drop it. users won't get it."* — and the COUNT stayed
        // because that sentence is a legibility complaint about a rendering,
        // not a retraction of `DIR-6`, his own standing directive that the
        // queue count be visible at all times.
        //
        // **THE ANCHOR IS RE-KEYED AND NOT DELETED**, which is the whole point:
        // a `DIR-6` guard that cannot detect a missing queue count is worse
        // than no guard, and this arc has already shipped one sweep test that
        // was green over the exact violation it named.
        assert!(
            lines[0].contains("2 queued"),
            "the queue count is missing from the {name} view: {:?}",
            lines[0]
        );
        assert!(
            !lines[0].contains('▲'),
            "the glyph he could not read came back to the {name} view: {:?}",
            lines[0]
        );
        assert!(
            lines[0].contains("this thread")
                || lines[0].contains("this project")
                || lines[0].contains("his threads"),
            "the badge's scope is missing from the {name} view: {:?}",
            lines[0]
        );
    }
}

/// **"AT ALL TIMES" INCLUDES AT 40 COLUMNS**, and this is the assertion the
/// live run earned.
///
/// The first draft ordered the header by what read nicely wide. Driven against
/// his daemon at 80 columns it cropped to `… · acct ? · ▲…` — the badge had
/// lost its digit to the endpoint URL, and `DIR-6`'s first requirement was
/// quietly a wide-terminal feature. The header is now ordered by what must
/// survive the crop, and this is what holds it there.
#[test]
fn the_queue_badge_survives_the_crop_at_every_width() {
    // (TUI-P1a: the thread page's header carries no queue count — see
    // `the_queue_badge_is_on_every_screen_and_says_what_it_counts`.)
    for base in [list_vs()] {
        for w in [40u16, 48, 60, 80, 100, 150] {
            let lines = text(&base, w, H);
            assert!(
                lines[0].contains("2 queued"),
                "the queue count lost its digit at {w} columns: {:?}",
                lines[0]
            );
        }
    }
    // …and so does the stdout-only announcement, for the same reason: a
    // transcript with rows held back must not look complete. It used to read
    // `filtered(N)`, which is the thing he could not decode — *"pressing s
    // sets a filter(8) and I don't know what that means"* — so the assertion
    // is on the WORDS now and not on a count.
    let mut collapsed = vs(LinkState::Connected);
    collapsed.stdout_only = true;
    for w in [40u16, 60, 80, 100, 150] {
        let head = text(&collapsed, w, H)[0].clone();
        // `stdout`, not `stdout only`: at 40 columns the header legitimately
        // ellipsizes the segment's tail (`· stdout on…`). What must survive
        // every width is the WORD, which is the announcement.
        assert!(
            head.contains("stdout"),
            "a collapsed view looked complete at {w} columns: {head:?}"
        );
        assert!(
            !head.contains("filter"),
            "the header still speaks the filter's vocabulary at {w}: {head:?}"
        );
    }
}

#[test]
fn staleness_is_amber_and_a_steady_connection_is_never_green() {
    let v = vs(LinkState::Connected);
    let cs = colours(&v, W, H);
    assert!(
        cs.contains(&palette::c().amber),
        "the warn lane row lost its amber"
    );
    assert!(
        !cs.contains(&palette::c().green),
        "green appeared on a steady state — it is for momentary confirmations only"
    );
    assert!(
        !cs.contains(&palette::c().red),
        "nothing here has failed, so nothing here may be red"
    );
}

#[test]
fn a_failure_is_red_and_a_notice_may_be_green() {
    let mut v = vs(LinkState::Disconnected {
        reason: "connection refused".into(),
        retry_at: Some(now() + TimeDelta::seconds(4)),
    });
    // TUI-P1a (TC-04): the THREAD page's header is its name and status only,
    // so the header's link/queue/account machinery is exercised on the LIST
    // header, which still draws every part.
    v.mode = Mode::List;
    v.notice = Some(Notice {
        text: "copied".into(),
        severity: Severity::Ok,
        expires_at: None,
    });
    // WIDE: the link line is deliberately late on the header now, so the
    // retry promise is legitimately what ellipsizes away at 100 columns. That
    // is why `store::banner` carries the promise too, and why the disconnected
    // test above asserts it on the banner rather than only here.
    const WIDE: u16 = 160;
    assert_shape(&v, WIDE, H);
    let cs = colours(&v, WIDE, H);
    assert!(cs.contains(&palette::c().red), "a real failure must be red");
    assert!(
        cs.contains(&palette::c().green),
        "a momentary confirmation may be green"
    );
    assert!(
        text(&v, WIDE, H)[0].contains("retry in 4s"),
        "the header must promise the next retry when it has the room"
    );
}

/// A timestamp we were never given renders `?` — never the moment the frame
/// arrived, which is the specific guess that looks right.
#[test]
fn a_message_with_no_timestamp_says_so_rather_than_using_its_arrival_time() {
    let mut v = vs(LinkState::Connected);
    v.blocks = vec![Block::Message {
        id: BlockId::default(),
        sender: Sender::Assistant,
        ts: None,
        text: "no timestamp came with this".into(),
        note: None,
        meta: Vec::new(),
        key: None,
        expandable: false,
        open: false,
    }];
    assert_shape(&v, W, H);
    let lines = text(&v, W, H);
    let header = lines.iter().find(|l| l.contains("claude")).unwrap();
    assert!(header.contains('?'), "{header:?}");
    assert!(
        !header.contains("19:30") && !header.contains("0s"),
        "an arrival time was passed off as an event time: {header:?}"
    );
}

// ── the overlays ────────────────────────────────────────────────────────────

#[test]
fn the_help_overlay_is_the_key_table_and_nothing_else() {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Help;
    // **THE OVERLAY IS CONTEXTUAL NOW**, so this asserts what it promises: the
    // grammar of the view it was raised over, complete, when there is room —
    // and it checks that for EVERY mode by raising it over each in turn, which
    // is a stronger statement than the old one made about a single screen.
    const TALL: u16 = 110;
    for mode in tui_render::DOCUMENTED_MODES {
        v.help_for = *mode;
        assert_shape(&v, W, TALL);
        let body = text(&v, W, TALL).join("\n");
        for (_, act) in tui_render::overlay_rows(*mode, v.caps) {
            assert!(
                body.contains(act),
                "the overlay dropped a binding in {mode:?}: {act:?}"
            );
        }
    }
    v.help_for = Mode::Thread(Region::Composer);
    let body = text(&v, W, TALL).join("\n");
    assert!(
        !body.contains(" M "),
        "the `?` overlay still documents `M`, which no longer does anything"
    );
}

/// A help screen that silently ends mid-list is the same defect as a
/// transcript that silently starts where the buffer does.
#[test]
fn a_help_overlay_too_short_for_its_table_says_so() {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Help;
    assert_shape(&v, W, H);
    let lines = text(&v, W, H);
    // **IT SAYS WHERE IT IS, not merely that there is more.** `0036`'s version
    // said "a taller window shows the rest", which was true and useless: a
    // thread's grammar does not fit any window he owns. Now the overlay
    // scrolls, so the honest line is the position and the keys that move it.
    assert!(
        lines
            .iter()
            .any(|l| l.contains("of") && l.contains("pgdn to move")),
        "a truncated help screen must say where it is and how to move: {lines:#?}"
    );
    // …and the range it names must be a real one.
    assert!(
        lines.iter().any(|l| l.contains("1–")),
        "the overlay opened somewhere other than the top: {lines:#?}"
    );
}

/// **THE FILTER IS GONE, AND THIS IS THE WITNESS THAT SAYS SO WITHOUT LYING.**
///
/// It replaces `the_filter_overlay_renders_every_lane_with_its_key`, which
/// asserted the overlay drew all nine lanes. *"filter is too much. Just drop
/// that feature. We'll just tune the interface."*
///
/// **A DELETION IS PROVEN BY A WITNESS, NOT BY AN ABSENCE** (`0065`). "No
/// filter word is on screen" is satisfied by a face that died on startup, so
/// the frame is asserted positively RIGHT first — the thread screen renders,
/// with its rows and its footer — and the witness is keyed on things NO
/// mutation of this round can move: the message text the fixture put in the
/// transcript, and the `?` hint the footer carries at rank 0 in every mode.
/// Only then does the absence mean anything.
///
/// FORCED: put `Seg::new(format!("filtered({})", 1), …)` back in the header
/// and the absence half goes red; make `frame` return an empty `Vec` and the
/// WITNESS goes red first, which is the whole point of having one.
#[test]
fn no_frame_in_any_mode_says_filter_and_the_face_still_draws() {
    const WIDE: u16 = 160;
    for mode in [
        Mode::Projects,
        Mode::List,
        Mode::Thread(Region::Chat),
        Mode::Thread(Region::Composer),
        Mode::Activity,
        Mode::Help,
        Mode::Queue,
    ] {
        for collapsed in [false, true] {
            let mut v = match mode {
                Mode::Projects => projects_vs(),
                Mode::List => list_vs(),
                _ => vs(LinkState::Connected),
            };
            v.mode = mode;
            v.stdout_only = collapsed;
            assert_shape(&v, WIDE, 40);
            let lines = text(&v, WIDE, 40);
            let body = lines.join("\n");

            // ── THE WITNESS: the frame is POSITIVELY RIGHT ────────────────
            //
            // Keyed on STRUCTURE, not on any word this round could have moved.
            // `? keys` was the first attempt and it was wrong: in a thread the
            // footer prints `alt-/ keys`, so the anchor was absent for a reason
            // that had nothing to do with what is under test — which is the
            // "witness naming the thing under test" trap `0065` names, one step
            // removed. A header row, a footer row and three rows of body cannot
            // be produced by a face that died, and nothing in Scope 1 can
            // blank any of them.
            assert_eq!(lines.len(), 40, "{mode:?} did not draw 40 rows");
            assert!(
                !lines[0].trim().is_empty(),
                "{mode:?} drew no header:\n{body}"
            );
            assert!(
                !lines[39].trim().is_empty(),
                "{mode:?} drew no footer:\n{body}"
            );
            assert!(
                lines.iter().filter(|l| l.trim().len() > 4).count() >= 3,
                "{mode:?} drew almost nothing, so an absence proves nothing:\n{body}"
            );

            // ── AND ONLY NOW THE ABSENCE ──────────────────────────────────
            // **`lane` IS DELIBERATELY NOT IN THIS LIST.** The word is
            // overloaded: the FLEET'S EVENT LANE is a live surface, `tab`
            // legitimately says "switch to the live event lane", and one of
            // these fixtures' own message bodies contains the phrase. The
            // filter's vocabulary was `filter`, `speech` and the overlay's
            // checkboxes.
            for word in ["filter", "speech", "[x]", "[ ]"] {
                assert!(
                    !body.contains(word),
                    "{mode:?} (collapsed={collapsed}) still says `{word}`:\n{body}"
                );
            }
        }
    }

    // **AND THE TOGGLE'S OWN ANNOUNCEMENT IS THE STATE, IN WORDS.** Checked
    // positively so that "no filter word" cannot be satisfied by a header that
    // says nothing at all.
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Thread(Region::Chat);
    v.stdout_only = true;
    assert!(
        text(&v, WIDE, 40)[0].contains("stdout only"),
        "a collapsed transcript does not announce itself: {:?}",
        text(&v, WIDE, 40)[0]
    );
    v.stdout_only = false;
    assert!(
        !text(&v, WIDE, 40)[0].contains("stdout only"),
        "the announcement is furniture — it is lit with the toggle off"
    );
}

/// `DIR-6`'s second requirement. The FAILED half is load-bearing, and the
/// overlay says out loud that it cannot act on anything yet.
#[test]
fn the_queue_overlay_shows_both_halves_and_admits_it_is_read_only() {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Queue;
    v.queue = QueueView {
        thread: "57db2ec9".into(),
        waiting: vec![QueueItem {
            message_id: 1920,
            text: "one that is still waiting".into(),
            intent: "immediate".into(),
            created_at: "2026-09-14T19:20:00+00:00".into(),
        }],
        failed: vec![QueueItem {
            message_id: 1914,
            text: "one whose retry ladder ran out".into(),
            intent: "immediate".into(),
            created_at: "2026-09-07T18:26:49+00:00".into(),
        }],
        state: Fetch::Loaded,
    };
    assert_shape(&v, 120, H);
    let body = text(&v, 120, H).join("\n");
    assert!(body.contains("queue · 57db2ec9"), "{body}");
    assert!(
        body.contains("waiting (1)") && body.contains("#1920"),
        "{body}"
    );
    assert!(
        body.contains("failed (1)") && body.contains("#1914"),
        "the failed half is where an exhausted retry ladder ends up: {body}"
    );
    assert!(
        body.contains("view only"),
        "an overlay that cannot act must say so rather than looking inert: {body}"
    );
}

/// A surface that cannot say WHY it is empty reads as empty.
#[test]
fn a_queue_that_could_not_be_read_says_why_rather_than_looking_empty() {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Queue;
    v.queue = QueueView {
        thread: "57db2ec9".into(),
        waiting: Vec::new(),
        failed: Vec::new(),
        state: Fetch::Failed("no such thread".into()),
    };
    assert_shape(&v, W, H);
    let body = text(&v, W, H).join("\n");
    assert!(
        body.contains("could not read the queue: no such thread"),
        "{body}"
    );
    assert!(
        !body.contains("nothing waiting"),
        "a failure must not be dressed as an empty queue: {body}"
    );
}

// ── the drag selection ──────────────────────────────────────────────────────

/// **A DRAG SELECTION IS PAINTED ON THE CELLS IT COVERS, AND ONLY THOSE.**
///
/// Reverse video rather than a rail marker: the selection is finer than a
/// message, so a per-message mark could not say where it started or ended.
#[test]
fn a_selection_reverses_exactly_the_characters_it_covers() {
    use tui_render::{Selection, TextPos, Unit, UnitKind};
    let mut v = vs(LinkState::Connected);
    let i = v
        .blocks
        .iter()
        .position(|b| b.text().starts_with("The frame is five bands"))
        .expect("the fixture moved");
    let unit = Unit::new(UnitKind::Content, i);

    // **Nothing selected: the ONLY reversed cell on the frame is the
    // composer's caret.** The caret is drawn as a reversed cell deliberately —
    // the terminal's own cursor is hidden (`?25l`), so the face draws its own,
    // and a block is what a cursor looks like. On an empty composer it sits in
    // a cell of its OWN, before the placeholder, which is why this is exactly
    // one space and not the placeholder's first letter.
    assert_shape(&v, W, H);
    assert_eq!(
        reversed(&v, W, H),
        " ",
        "an unselected frame has a highlight that is not the composer's caret"
    );

    // Characters 4..=12 of that block — INCLUSIVE at the far end, as a
    // terminal's own selection is.
    v.selection = Some(Selection::new(
        TextPos::new(unit, 4),
        TextPos::new(unit, 12),
    ));
    assert_shape(&v, W, H);
    let want: String = v.blocks[i].text().chars().skip(4).take(9).collect();
    assert_eq!(
        reversed(&v, W, H),
        // The caret is still on screen and still reversed, and it is BELOW the
        // transcript, so it is the last reversed cell on the frame. Naming it
        // rather than stripping it keeps the assertion exact.
        format!("{want} "),
        "the highlight does not cover the characters the selection names"
    );
    // …and the RAILS are not in it, which is the whole point: the highlight
    // is over the text, not over the row.
    assert!(!reversed(&v, W, H).contains('▌'));
}

/// **THE COPY IS THE LOGICAL TEXT, NOT THE PAINTED SCREEN.** A message wide
/// enough to wrap comes back as ONE line with no gutter, no rail and no hard
/// break — which is the upgrade over what the terminal's own selection would
/// have given him.
#[test]
fn a_copy_takes_the_logical_text_and_not_the_rows_it_was_drawn_on() {
    use tui_render::{selected_text, Selection, TextPos, Unit, UnitKind};
    let mut v = vs(LinkState::Connected);
    let i = v
        .blocks
        .iter()
        .position(|b| b.text().starts_with("The frame is five bands"))
        .expect("the fixture moved");
    let full = v.blocks[i].text().to_string();
    let unit = Unit::new(UnitKind::Content, i);
    let sel = Selection::new(
        TextPos::new(unit, 0),
        TextPos::new(unit, full.chars().count() - 1),
    );
    v.selection = Some(sel);

    // Narrow enough that the block certainly wraps.
    let (_, hits) = frame_with_hits(&v, now(), 50, H);
    let got = selected_text(&v, &hits.texts(), sel);
    assert_eq!(got, full, "the copy is not the block's own text");
    assert!(!got.contains('▌'), "a rail reached the clipboard: {got:?}");
    assert!(
        !got.contains('\n'),
        "a hard wrap reached the clipboard: {got:?}"
    );

    // The rows it was drawn on DO wrap — so the test above is about the
    // derivation and not about a message that happened to be short.
    let rows: Vec<_> = hits
        .texts()
        .into_iter()
        .filter(|t| t.unit == unit)
        .collect();
    assert!(
        rows.len() > 1,
        "the fixture did not wrap; the proof is vacuous"
    );
}

/// **EVERY BAND IS SELECTABLE.** He said ANY part of the interface, and the
/// face owns every cell it draws, so there must be no band whose rows carry no
/// text at all.
#[test]
fn the_header_the_footer_and_the_body_all_carry_copyable_text() {
    use tui_render::UnitKind;
    let mut v = vs(LinkState::Connected);
    v.banner = Some(Banner {
        text: "the link is down — retrying in 2s".into(),
        severity: Severity::Critical,
    });
    let (_, hits) = frame_with_hits(&v, now(), W, H);
    let kinds: Vec<UnitKind> = hits.texts().into_iter().map(|t| t.unit.kind).collect();
    for want in [
        UnitKind::Header,
        UnitKind::Banner,
        UnitKind::Content,
        UnitKind::Composer,
        UnitKind::Footer,
    ] {
        assert!(
            kinds.contains(&want),
            "{want:?} has no selectable text on it — a dead region"
        );
    }

    // A cell on the header row resolves to a position inside the header.
    let p = hits
        .pos_at(3, 0, tui_render::Toward::Down)
        .expect("the header row resolves to nothing");
    assert_eq!(p.unit.kind, UnitKind::Header);

    // …and so does one on the footer row, which is the other end of the frame.
    let p = hits
        .pos_at(3, H - 1, tui_render::Toward::Up)
        .expect("the footer row resolves to nothing");
    assert_eq!(p.unit.kind, UnitKind::Footer);
}

/// A drag that lands on PADDING still starts a selection: it snaps into the
/// content in the direction the selection is growing. Without this, a sweep
/// that begins in the blank space above a short transcript selects nothing and
/// looks broken.
#[test]
fn a_press_on_blank_space_snaps_into_the_content() {
    use tui_render::{Toward, UnitKind};
    let mut v = vs(LinkState::Connected);
    v.blocks.truncate(1); // short content, so the band's top rows are padding
    let (_, hits) = frame_with_hits(&v, now(), W, H);
    let b = tui_render::bands(&v, W, H);
    let padding_row = (b.header + 1) as u16;
    assert!(
        hits.text_row(padding_row).is_none(),
        "the fixture is wrong: row {padding_row} is not padding"
    );
    let p = hits
        .pos_at(0, padding_row, Toward::Down)
        .expect("a press on padding started no selection at all");
    assert_eq!(p.unit.kind, UnitKind::Content);
}

// ── the shape law, everywhere ───────────────────────────────────────────────

/// The widths that break are never the ones a single case picked.
#[test]
fn the_shape_law_holds_everywhere() {
    let mut chat = vs(LinkState::Connected);
    chat.notice = Some(Notice {
        text: "a notice that is quite long and has to share the row".into(),
        severity: Severity::Ok,
        // Standing until something replaces it: these goldens fix `now`,
        // and a notice that expired against a frozen clock would make the
        // case about the clock instead of about the colour.
        expires_at: None,
    });
    chat.banner = Some(Banner {
        text: "a banner, which takes a row from whatever is under it".into(),
        severity: Severity::Warn,
    });
    let mut list = list_vs();
    list.banner = chat.banner.clone();
    let mut queue = vs(LinkState::Connected);
    queue.mode = Mode::Queue;
    queue.queue = QueueView {
        thread: "57db2ec9".into(),
        waiting: vec![QueueItem {
            message_id: 1,
            text: "x".repeat(400),
            intent: "immediate".into(),
            created_at: "2026-09-14T19:20:00+00:00".into(),
        }],
        failed: Vec::new(),
        state: Fetch::Loaded,
    };

    for base in [&chat, &list, &queue] {
        for w in [30u16, 39, 40, 41, 47, 60, 79, 80, 81, 100, 120, 200] {
            for h in [4u16, 7, 8, 9, 12, 24, 40, 62] {
                for mode in [
                    Mode::List,
                    Mode::Thread(Region::Composer),
                    Mode::Thread(Region::Chat),
                    Mode::Thread(Region::Queue),
                    Mode::Activity,
                    Mode::Help,
                    Mode::Queue,
                    // Mic mode is a BODY mode with a band of its own, so it is
                    // in the sweep like every other one: a band whose height
                    // comes out wrong is a frame that is not `height` rows, and
                    // this is the law that says so at every width he can make.
                    Mode::Mic,
                ] {
                    let mut v = base.clone();
                    v.mode = mode;
                    assert_shape(&v, w, h);
                    v.expanded = true;
                    v.stdout_only = true;
                    assert_shape(&v, w, h);
                }
            }
        }
    }

    // …and with nothing to draw at all, which is the state the face starts in.
    let empty = ViewState::default();
    for w in [40u16, 80, 100] {
        for h in [8u16, 24] {
            for mode in [
                Mode::List,
                Mode::Thread(Region::Composer),
                Mode::Thread(Region::Chat),
                Mode::Thread(Region::Queue),
                Mode::Activity,
                Mode::Queue,
            ] {
                let mut v = empty.clone();
                v.mode = mode;
                assert_shape(&v, w, h);
            }
        }
    }
}

#[test]
fn below_the_minimum_it_says_one_true_thing_and_draws_nothing_else() {
    let v = vs(LinkState::Connected);
    assert_shape(&v, 30, 6);
    golden(
        &text(&v, 30, 6),
        &[
            "tui needs 40x8 (have 30x6)    ",
            "                              ",
            "                              ",
            "                              ",
            "                              ",
            "                              ",
        ],
    );
}

/// A wide glyph that straddles the crop boundary must be DROPPED, not
/// half-drawn: half a wide cell shifts every column after it.
#[test]
fn a_double_width_glyph_never_straddles_the_crop() {
    let mut v = vs(LinkState::Connected);
    v.header.title = "日本語".repeat(40);
    v.blocks = vec![
        msg(Sender::Assistant, "m", 19, 18, &"日本語".repeat(40)),
        msg(Sender::User, "m", 19, 18, &"日本語".repeat(40)),
        lane('⚙', Severity::Calm, &"日本語".repeat(40), Some(3)),
    ];
    let mut l = list_vs();
    l.list.rows[0].cwd = "日本語".repeat(40);
    for w in 40u16..=64 {
        assert_shape(&v, w, 24);
        assert_shape(&l, w, 24);
    }
}

// ── the hit map: what is under a coordinate of the frame that was DRAWN ─────

/// **THE HIT MAP DESCRIBES THE FRAME BESIDE IT, ROW FOR ROW.**
///
/// This is `the_band_heights_describe_the_frame_beside_them`'s sibling and it
/// exists for the same reason: a second derivation of the geometry is the
/// defect class this crate has already had to repair once, and a hit map that
/// drifted from the frame would be an invisible version of it — a wrong hit
/// map does not look wrong, it just opens the wrong thing.
#[test]
fn the_hit_map_describes_the_frame_beside_it() {
    let banner = Banner {
        text: "the link is down — retrying in 2s".into(),
        severity: Severity::Critical,
    };
    for mode in [
        Mode::Projects,
        Mode::List,
        Mode::Activity,
        Mode::Thread(Region::Composer),
        Mode::Thread(Region::Chat),
        Mode::Help,
        Mode::Queue,
    ] {
        for with_banner in [false, true] {
            let mut v = match mode {
                Mode::Projects => projects_vs(),
                Mode::List => list_vs(),
                _ => vs(LinkState::Connected),
            };
            v.mode = mode;
            v.banner = with_banner.then(|| banner.clone());
            let (lines, hits) = frame_with_hits(&v, now(), W, H);
            assert_eq!(
                hits.height(),
                lines.len(),
                "{mode:?} (banner {with_banner}): the map and the frame disagree about how many rows there are"
            );
            assert_eq!(hits.width(), W);
            // The bands say where things are; the map must agree with them.
            let b = tui_render::bands(&v, W, H);
            assert_eq!(
                hits.hit(0, 0),
                Target::Header,
                "{mode:?}: row 0 is the header"
            );
            assert_eq!(
                hits.hit(0, H - 1),
                Target::Footer,
                "{mode:?}: the last row is the footer"
            );
            if b.banner == 1 {
                assert_eq!(
                    hits.hit(0, b.header as u16),
                    Target::Banner,
                    "{mode:?}: the banner is not where bands put it"
                );
            }
            for i in 0..b.composer {
                // **THE BAND IS WHERE `bands` PUT IT — and its TEXT rows are
                // now finer targets than its chrome.** A caret lands where the
                // pointer is, so the rows carrying the buffer answer
                // `ComposerRow`; the legend above and the rule below stay
                // `Composer`, which is a free miss. Both are "the composer" for
                // this assertion's purpose, which is that the BAND occupies
                // these rows and nothing else does.
                let t = hits.hit(0, (b.header + b.banner + b.content + i) as u16);
                assert!(
                    matches!(t, Target::Composer | Target::ComposerRow { .. }),
                    "{mode:?}: the composer is not where bands put it (row {i} is {t:?})"
                );
            }
            if mode.is_overlay() {
                assert_eq!(
                    hits.hit(0, (b.header + 1) as u16),
                    Target::Overlay,
                    "{mode:?}: an overlay's body must be dismissible by a click"
                );
            }
        }
    }
    // Below the minimum there is nothing clickable, because there is one
    // sentence on the frame and it is not an affordance.
    let v = vs(LinkState::Connected);
    let (lines, hits) = frame_with_hits(&v, now(), 10, 4);
    assert_eq!(hits.height(), lines.len());
    assert_eq!(hits.hit(0, 0), Target::Empty);
}

/// **THE ROW A CLICK LANDS ON IS THE ROW HE SEES.** The map is asserted
/// against the TEXT of the same frame, not against the model — which is what
/// makes it a test of the layout rather than of the loop that built it.
#[test]
fn clicking_a_row_resolves_to_the_row_that_is_drawn_there() {
    let v = list_vs();
    let (_, hits) = frame_with_hits(&v, now(), W, H);
    let lines = text(&v, W, H);
    for (want, needle) in [(0usize, "rebuild the index reader"), (1, "untitled")] {
        let row = lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle} is not on the frame"));
        // **COLUMN 0 IS THE MARKER, and the marker is the ROW.** It was column
        // 4 until `0076`: the row used to open with two marker columns, an
        // eight-character id and a space, so column 4 was inside the id and
        // belonged to the row. The id is gone, the NAME starts at column 2,
        // and column 4 is now inside the name's own span — which is a
        // different target on purpose. The name is asserted below.
        assert_eq!(
            hits.hit(0, row as u16),
            Target::ThreadRow(want),
            "the map points at a different row than the one drawn at {row}"
        );
        assert_eq!(
            hits.hit(4, row as u16),
            Target::ThreadTitle(want),
            "the name's span is not claimed on the row it is drawn on"
        );
    }
    // The root menu, the same way.
    let p = projects_vs();
    let (_, hits) = frame_with_hits(&p, now(), W, H);
    let lines = text(&p, W, H);
    let row = lines
        .iter()
        // The PROJECT row, which is the one carrying a path — plain "harness"
        // now matches a band row first, and a band row is a different target.
        .position(|l| l.contains("~/minds/harness"))
        .expect("harness is on the frame");
    // Above it: the pinned header + one pinned + the recent header + two recent
    // + the projects header + the first project = seven stops, so the second
    // project is selectable 9. Three of those nine are BAND HEADERS and two are
    // `show more` FEET, all five of which became stops this round.
    //
    // **PROBED IN THE TAIL, PAST THE NAME.** Column 4 used to serve here and no
    // longer can: `0063` made a project's NAME claim its own columns as the
    // door onto §5's project sheet, exactly as a thread's title claims its own.
    // The invariant is unchanged and is asserted where the row is still the
    // whole answer.
    assert_eq!(hits.hit(W - 5, row as u16), Target::RootRow(9));
    // …and the name IS the door, at the columns it was drawn at, naming THIS
    // row's index. That is the half that would have been lost by moving the
    // probe, and a span claiming the wrong row is the defect a sub-row
    // affordance introduces.
    assert_eq!(hits.hit(4, row as u16), Target::ProjectName(9));
    assert_eq!(hits.hit(4, row as u16).row_index(), Some(9));
    // **THE SPAN IS STILL THE DOOR, AND THE GESTURE IS A KEY.** The hit map is
    // unchanged by round six's click reversal — the name is still what is drawn
    // there, and a map that said otherwise would describe a different frame —
    // so the assertion moved to `Via::Key`, which is the gesture that opens it.
    assert_eq!(
        activation(Target::ProjectName(4), Mode::Projects, Via::Key),
        Some(Intent::OpenProjectSheet(4)),
        "the key on a project's name does not open its sheet"
    );
    assert_eq!(
        activation(Target::ProjectName(4), Mode::Projects, Via::Click),
        Some(Intent::Select(4)),
        "a click on a project's name is not a plain row click"
    );
    // The two marker columns are NOT the name — a click there is the plain row
    // behaviour the operator's retraction gave it.
    assert_eq!(hits.hit(0, row as u16), Target::RootRow(9));
    // And the blank floor under a short list is a MISS, not the last row
    // repeated — the commonest way a click-driven list opens the wrong thing.
    assert_eq!(hits.hit(4, H - 2), Target::Empty);
}

/// **CLICK AND `→` ARE ONE OP, ASSERTED ON ONE TARGET.**
///
/// The frame is drawn, the coordinate of a drawn row is hit-tested, and the
/// `Target` that comes back is put through the SAME `activation` the keyboard
/// uses. Two tests that happened to agree would prove nothing; this one shows
/// there is only one thing to agree with.
/// **THE RETRACTION, ON A REAL FRAME, AND IT IS TOTAL NOW.** A click anywhere
/// on a row — the name included — SELECTS it; `→` on the same row OPENS it.
/// Asserted against the coordinates the frame actually drew, because the
/// unit tests in `hit.rs` prove the table and this proves the GEOMETRY — which
/// is where an affordance that is drawn in one place and claimed in another
/// would show up, and it would look perfectly fine on screen.
#[test]
fn a_click_selects_the_row_it_landed_on_and_right_is_what_opens_it() {
    let v = list_vs();
    let (_, hits) = frame_with_hits(&v, now(), W, H);
    let lines = text(&v, W, H);
    let row = lines
        .iter()
        .position(|l| l.contains("untitled"))
        .expect("the row");
    // **OFF THE NAME: still a plain row, and a plain row still selects.** The
    // right-hand metadata columns are the row's own target — `active  ▲2` sits
    // there — so this is the same assertion it always was, aimed at a column
    // the name does not reach.
    let plain = hits.hit(75, row as u16);
    assert_eq!(plain, Target::ThreadRow(1));
    assert_eq!(
        activation(plain, Mode::List, Via::Click),
        Some(Intent::Select(1)),
        "a click off the name must SELECT it — his ruling, narrowed not retired"
    );
    assert_eq!(
        activation(plain, Mode::List, Via::Key),
        Some(Intent::Open),
        "→ is what opens it"
    );
    // **ON THE NAME: THE SAME ANSWER, and that is round six's reversal.** The
    // name is still its own `Target` — `o` aims a key at it — but the click it
    // resolves is the row's. *"clicking on a thread shouldn't take me into it.
    // Only right can do it."*
    // (Column 5: inside `untitled`, which is this row's name since TUI-P1a
    // took the path off it — the old column 10 was inside the longer path.)
    let name = hits.hit(5, row as u16);
    assert_eq!(name, Target::ThreadTitle(1));
    assert_eq!(
        activation(name, Mode::List, Via::Click),
        Some(Intent::Select(1)),
        "a click on a thread's name must SELECT its row, not open the thread"
    );
    // …and the KEY through the same span is the options door, which is what
    // `o` aims at. The two gestures are the two halves of one design.
    assert_eq!(
        activation(name, Mode::List, Via::Key),
        Some(Intent::OpenSheet(1)),
        "`o` lost its door onto the sheet"
    );

    // **THE BODY OF A FOLDED TRANSCRIPT ROW IS SILENT TO THE MOUSE, AND ITS
    // EXPANDER IS NOT.** Two columns apart, on the same drawn row.
    let mut t = vs(LinkState::Connected);
    t.mode = Mode::Thread(Region::Composer);
    t.blocks = vec![lane_expandable(
        '◇',
        Severity::Calm,
        "task 9f3e215d · building step 3",
        Some(214),
    )];
    let (_, hits) = frame_with_hits(&t, now(), W, H);
    let lines = text(&t, W, H);
    let row = lines
        .iter()
        .position(|l| l.contains("×214"))
        .expect("the folded row") as u16;
    // **THE MARK MOVED WITH THE BLOB AND THE BUTTON MOVED WITH THE MARK.** An
    // info blob is centred now, so its expander sits immediately left of the
    // blob rather than at column zero — and the two columns the hit map claims
    // are read from the same arithmetic that painted the glyph, which is the
    // whole reason this assertion computes the column instead of naming one.
    let at = tui_render::geom::blob_left(W as usize) - 2;
    assert!(
        lines[row as usize][..at].chars().all(|c| c == ' ')
            && lines[row as usize][at..].starts_with("▸ "),
        "an expandable row must DRAW its affordance, or it cannot be aimed at: {:?}",
        lines[row as usize]
    );
    assert_eq!(
        hits.hit(at as u16, row),
        Target::Expander(0),
        "the mark's first column is the expander"
    );
    assert_eq!(
        hits.hit(at as u16 + 1, row),
        Target::Expander(0),
        "and so is its second"
    );
    assert_eq!(
        hits.hit(at as u16 - 1, row),
        Target::Block(0),
        "the margin left of the mark is not a button"
    );
    assert_eq!(
        hits.hit(at as u16 + 10, row),
        Target::Block(0),
        "the body beside it is still the block"
    );
    assert_eq!(
        activation(Target::Expander(0), Mode::Thread(Region::Chat), Via::Click),
        Some(Intent::ExpandBlock(0)),
        "the expander is an affordance and still activates on a click"
    );
    assert_eq!(
        activation(Target::Block(0), Mode::Thread(Region::Chat), Via::Click),
        None,
        "the row's BODY must no longer expand on a click"
    );

    // A row with nothing behind it draws no affordance and claims no columns —
    // a button that does nothing is worse than no button.
    let mut plain = vs(LinkState::Connected);
    plain.mode = Mode::Thread(Region::Composer);
    plain.blocks = vec![lane(
        '◇',
        Severity::Calm,
        "a row with nothing behind it",
        None,
    )];
    let (_, hits) = frame_with_hits(&plain, now(), W, H);
    let lines = text(&plain, W, H);
    let row = lines
        .iter()
        .position(|l| l.contains("nothing behind it"))
        .expect("the plain row") as u16;
    assert!(
        !lines[row as usize].starts_with("▸"),
        "{:?}",
        lines[row as usize]
    );
    assert_eq!(hits.hit(0, row), Target::Block(0));

    // **THE QUEUE, AS AN EXPANDABLE CLICK.** `DIR-6` says the badge is visible
    // at all times; this is it being REACHABLE at all times, at the columns it
    // actually survived the crop at.
    for w in [W, 150, 80, 40] {
        let (_, hits) = frame_with_hits(&v, now(), w, H);
        let line = &text(&v, w, H)[0];
        // **RE-KEYED FROM `▲` TO THE WORD** (`0072`): the glyph went and the
        // count stayed, so the MOUSE's only door onto the queue overlay is now
        // the words `N queued`. The part begins at its separating dot, so the
        // probe aims at the digit — inside the claimed span either way.
        let at = line
            .find(" queued")
            .unwrap_or_else(|| panic!("no queue count at {w}: {line:?}"));
        // `find` is a BYTE offset and the header is not ascii — count columns.
        let col = line[..at].chars().count() as u16 - 1;
        assert_eq!(
            hits.hit(col, 0),
            Target::QueueBadge,
            "the badge at {w} columns is not clickable where it is drawn: {line:?}"
        );
        // **AND A CLICK ON IT NO LONGER RAISES THE QUEUE.** Round six: a
        // click never enters a surface. The span is still claimed — the badge
        // is the only thing drawn there and a `Target::Header` under the words
        // `N queued` would be a lie about the frame — and the door it lost is
        // `u` / `^O`, asserted against `BINDINGS` in
        // `a_click_never_enters_and_the_doors_it_lost_have_keys`.
        assert_eq!(activation(Target::QueueBadge, Mode::List, Via::Click), None);
        assert_eq!(
            activation(Target::QueueBadge, Mode::List, Via::Key),
            Some(Intent::QueueOverlay)
        );
        // …and the column beside it is the header, which is a free miss.
        assert_eq!(hits.hit(col.saturating_sub(1), 0), Target::Header);
    }

    // The chip re-arms follow — `G`'s job, kept after `G`'s letter goes.
    let mut c = vs(LinkState::Connected);
    c.viewport.follow = false;
    c.viewport.from_bottom = 3;
    c.viewport.new_since = 4;
    let (_, hits) = frame_with_hits(&c, now(), W, H);
    let lines = text(&c, W, H);
    let row = lines
        .iter()
        .position(|l| l.contains("↓ 4 new"))
        .expect("the chip");
    assert_eq!(hits.hit(4, row as u16), Target::NewChip);
    assert_eq!(
        activation(Target::NewChip, Mode::Thread(Region::Chat), Via::Click),
        Some(Intent::Bottom)
    );
}

/// **A CLICK IN THE COMPOSER PUTS THE CARET WHERE HE POINTED** — *"to that
/// position, or the nearest legal one."* Proven against the drawn frame: the
/// row he clicked is the row the caret lands on, and the column survives the
/// band's gutter.
#[test]
fn a_click_in_the_composer_resolves_to_a_caret_at_that_row_and_column() {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Thread(Region::Composer);
    v.composer.buf = tui_render::compose::Composer::from_text("alpha beta\ngamma delta");
    let (_, hits) = frame_with_hits(&v, now(), W, H);
    let lines = text(&v, W, H);
    let first = lines
        .iter()
        .position(|l| l.contains("alpha beta"))
        .expect("the first composer row") as u16;

    // The row the text is on resolves to laid row 0, and the one under it to 1.
    assert_eq!(
        hits.hit(GUTTER as u16 + 6, first),
        Target::ComposerRow {
            row: 0,
            col: GUTTER as u16 + 6
        }
    );
    assert_eq!(
        hits.hit(GUTTER as u16 + 2, first + 1),
        Target::ComposerRow {
            row: 1,
            col: GUTTER as u16 + 2
        }
    );
    assert_eq!(
        activation(
            Target::ComposerRow {
                row: 1,
                col: GUTTER as u16 + 2
            },
            Mode::Thread(Region::Composer),
            Via::Click
        ),
        Some(Intent::Caret {
            row: 1,
            col: GUTTER as u16 + 2
        })
    );

    // The band's own chrome is NOT a caret position: the legend above the text
    // and the rule below it are a free miss, exactly as the footer is.
    assert_eq!(hits.hit(10, first - 1), Target::Composer);
}

/// **THE `?` OVERLAY IS THE KEY TABLE, so the cuts propagate for free — and
/// this VERIFIES that rather than assuming it.**
#[test]
fn the_key_overlay_is_the_table_and_the_cut_keys_are_gone_from_it() {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Help;
    // Tall enough that nothing is truncated, so this is about the TABLE and
    // not about the fit (which the next test is about).
    let lines = text(&v, 150, 60);
    let body = lines.join("\n");
    for gone in [
        "expand folded rows",  // `x`'s wording, in both modes
        "scroll the key list", // Help's broken scroll rows
    ] {
        assert!(
            !body.contains(gone),
            "a cut binding is still documented: {gone}"
        );
    }
    // `k` and `j` cannot appear as a KEY column entry any more. Checked on the
    // key column only, because the prose legitimately contains those letters.
    for line in &lines {
        let key_col: String = line.chars().skip(5).take(12).collect();
        assert!(
            !key_col.contains(" k ") && !key_col.contains(" j "),
            "a cut key is still in the overlay's key column: {line:?}"
        );
    }
    // What replaced them IS there, in the modes that own it.
    // What replaced them IS there, in the mode that owns it — raised over that
    // mode, because the overlay is contextual.
    v.help_for = Mode::Activity;
    let lane = text(&v, 150, 60).join("\n");
    assert!(lane.contains("expand a folded row (or click the ▸ beside it)"));
    v.help_for = Mode::Projects;
    assert!(text(&v, 150, 60)
        .join("\n")
        .contains("← never exits the root"));
    // FORCED THE OTHER WAY: the overlay renders the table rather than a list of
    // its own, so a binding that IS in the table must be on the screen when the
    // overlay is raised over its mode and there is room for it.
    //
    // **AND IT IS ASSERTED PER REGION, NOT PER MODE.** A thread screen has
    // three grammars on it now; raising the overlay over one region and
    // checking the whole thread's rows would document keys that region does
    // not answer to, which is the exact drift the one-table rule exists to
    // prevent.
    for region in [
        Region::Composer,
        Region::Chat,
        Region::Queue,
        Region::Agents,
    ] {
        v.help_for = Mode::Thread(region);
        let printed = text(&v, 150, 60).join("\n");
        for b in tui_render::BINDINGS {
            if b.mode == Mode::Thread(region) && b.needs.met(v.caps) {
                assert!(
                    printed.contains(b.act),
                    "{region:?}'s overlay is missing one of its own bindings: {}",
                    b.act
                );
            }
        }
        // …and it documents NOTHING from the other regions, which is the half
        // that would be silently wrong if the overlay kept reading `Thread`.
        for b in tui_render::BINDINGS {
            let other = matches!(b.mode, Mode::Thread(r) if r != region);
            if other && b.needs.met(v.caps) && !b.act.is_empty() {
                // A region legitimately shares a sentence with another, and
                // one act's wording can be a SUBSTRING of another's ("up one
                // screen" inside "transcript up one screen"), so both count as
                // this region's own rather than as a leak.
                let shared = tui_render::BINDINGS.iter().any(|o| {
                    o.mode == Mode::Thread(region) && (o.act == b.act || o.act.contains(b.act))
                });
                assert!(
                    shared || !printed.contains(b.act),
                    "{region:?}'s overlay documents another region's key: {}",
                    b.act
                );
            }
        }
    }
    v.help_for = Mode::Thread(Region::Composer);
    let thread = text(&v, 150, 60).join("\n");
    for b in tui_render::BINDINGS {
        if b.mode == Mode::Thread(Region::Composer) && b.needs.met(v.caps) {
            assert!(
                thread.contains(b.act),
                "a live binding is missing from the overlay: {}",
                b.act
            );
        }
    }
}

/// The overlay's FIT, measured rather than hoped for. `0036` found it running
/// at 74 body rows against his 33 — "the one mechanism the old philosophy named
/// as its discoverability guarantee cannot show even half of what it
/// guarantees". This pins how far that came down and refuses a regression.
#[test]
fn the_key_overlay_fits_the_view_he_is_in_and_scrolls_when_it_cannot() {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Help;

    // **THE THREE LIST-SHAPED MODES FIT HIS PANE OUTRIGHT**, which is what
    // making the overlay contextual bought. `0036` measured the all-modes
    // overlay at 74 body rows against the 33 his pane has; raised over a list,
    // the grammar he is standing in is a third of that.
    for mode in [
        Mode::Projects,
        Mode::List,
        Mode::Activity,
        Mode::Queue,
    ] {
        v.help_for = mode;
        let needed = (8u16..90)
            .find(|h| !text(&v, 150, *h).iter().any(|l| l.contains("to move")))
            .unwrap_or(u16::MAX);
        assert!(
            needed <= 33,
            "{mode:?}'s grammar needs {needed} rows and his pane has 33"
        );
    }

    // **A THREAD'S DOES NOT, AND NO FACTORING WOULD MAKE IT.** A textarea has
    // forty bindings; that is what a textarea is. So the overlay scrolls, and
    // the two things this asserts are that it SAYS it is scrolled and that the
    // rest is actually reachable — a note that cannot be acted on would be the
    // same defect in a politer voice.
    v.help_for = Mode::Thread(Region::Composer);
    const SHORT: u16 = 24;
    assert_shape(&v, 150, SHORT);
    let top = text(&v, 150, SHORT);
    assert!(
        top.iter().any(|l| l.contains("to move")),
        "the overlay was truncated and did not say so"
    );
    let first = top.join("\n");
    assert!(
        first.contains("SEND the whole payload"),
        "the overlay opened past the top"
    );

    // SCROLLED: the rows the first screen could not show are now on it, and
    // the position line moved with them.
    //
    // **THE OFFSET IS DERIVED, NOT TYPED.** It was a literal `25`, and round
    // six's ctrl migration took four rows out of the composer's grammar — the
    // unqueue, the two draft-offer keys and the expand — so the literal
    // silently became a CLAMPED offset and the position line read `23–42 of 42`
    // while the assertion waited for `26–`. A test that hard-codes a row count
    // it does not own breaks on every change to the table; this one asks the
    // table.
    let total = tui_render::overlay_rows(Mode::Thread(Region::Composer), v.caps).len();
    let from = total / 3;
    v.help_from = from;
    assert_shape(&v, 150, SHORT);
    let later = text(&v, 150, SHORT).join("\n");
    assert!(
        later.contains(&format!("{}–", from + 1)),
        "the scroll offset did not reach the position line: {later}"
    );
    assert!(
        !later.contains("SEND the whole payload"),
        "scrolling moved nothing"
    );

    // **AND IT CANNOT BE SCROLLED PAST THE END.** An overlay that scrolls into
    // blank space is a face that looks broken in exactly the way a help screen
    // must not.
    v.help_from = 10_000;
    assert_shape(&v, 150, SHORT);
    let end = text(&v, 150, SHORT);
    assert!(
        end.iter().filter(|l| l.trim().is_empty()).count() < SHORT as usize / 2,
        "scrolling past the end emptied the overlay: {end:#?}"
    );
}

// ── MIC MODE ─────────────────────────────────────────────────────────────────

/// A mic-mode face with one of each kind of queue entry, so the band's whole
/// vocabulary is on screen at once.
fn mic_vs() -> ViewState {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Mic;
    // A TYPED entry first, so the band has both kinds side by side and the
    // "identical except for the play button" claim is checkable on screen.
    for ch in "the typed one".chars() {
        v.composer.buf.insert_char(ch);
    }
    assert!(v.composer.buf.queue());
    v.composer.buf.queue_audio("/tmp/cap-ready.wav");
    v.composer
        .buf
        .resolve_audio("/tmp/cap-ready.wav", Ok("the spoken one".into()));
    v.composer.buf.queue_audio("/tmp/cap-waiting.wav");
    v.composer.buf.queue_audio("/tmp/cap-bad.wav");
    v.composer.buf.resolve_audio(
        "/tmp/cap-bad.wav",
        Err("heard nothing — the microphone sent silence".into()),
    );
    v
}

/// **WHAT MIC MODE ACTUALLY LOOKS LIKE**, pinned.
///
/// The three states are all visible and they are visibly different: a ready row
/// carries `▶` and its text, a waiting row says it is transcribing, and a
/// failed row says what went wrong and offers `retry · drop`. A ready spoken
/// row is styled exactly as a typed one — his ruling — so the ONLY thing
/// separating them on screen is that button.
#[test]
fn mic_mode_shows_every_item_and_its_state() {
    let v = mic_vs();
    assert_shape(&v, W, H);
    let lines = text(&v, W, H);
    let joined = lines.join("\n");

    assert!(
        joined.contains(" mic "),
        "the band does not name itself:\n{joined}"
    );
    assert!(
        joined.contains("r record"),
        "the band does not print its keys"
    );
    assert!(
        joined.contains("enter SENDS all"),
        "the send key is not printed"
    );
    assert!(
        joined.contains("4 held here"),
        "the queue depth is not shown"
    );
    assert!(
        joined.contains("1 transcribing"),
        "the in-flight count is not shown"
    );

    assert!(
        joined.contains("▶  the spoken one"),
        "a ready spoken row has no play button"
    );
    // **THE TYPED ROW IS IDENTICAL EXCEPT FOR THE BUTTON**: same left margin,
    // no triangle. That is his ruling, checked on the painted screen.
    assert!(
        joined.contains("   the typed one"),
        "the typed row is not aligned with the spoken one"
    );
    assert!(
        !joined.contains("▶  the typed one"),
        "a typed row grew a play button"
    );
    assert!(
        joined.contains("transcribing…"),
        "a waiting row does not say so"
    );
    assert!(
        joined.contains("heard nothing"),
        "a failed row does not say why"
    );
    assert!(
        joined.contains("retry · drop"),
        "a failed row offers no way out"
    );
    // The word he BANNED is nowhere on this face.
    assert!(
        !joined.to_lowercase().contains("wildcard"),
        "the banned word reached the face"
    );
}

/// **RECORDING LOOKS LIKE RECORDING.** A face with the microphone open that
/// does not say so unmistakably is the one state this mode must never be in.
#[test]
fn a_recording_face_says_so_unmistakably() {
    let mut v = mic_vs();
    v.mic = tui_render::MicState::Recording;
    assert_shape(&v, W, H);
    let joined = text(&v, W, H).join("\n");
    assert!(
        joined.contains("RECORDING"),
        "a recording face does not say it is recording"
    );
    assert!(joined.contains("stops it"), "it does not say how to stop");

    // …and a refusal is a STANDING statement, not a flash.
    let mut r = mic_vs();
    r.mic = tui_render::MicState::Refused(
        "the audio interface is using the microphone — mic mode did not open it".into(),
    );
    assert_shape(&r, W, H);
    assert!(
        text(&r, W, H)
            .join("\n")
            .contains("audio interface is using the microphone"),
        "the refusal is not on the band"
    );
}

/// **THE PLAY BUTTON IS CLICKABLE WHERE IT IS PAINTED**, and a row with nothing
/// to play does not claim those columns — an affordance that is drawn but not
/// claimed is a button the mouse falls through, and one claimed but not drawn
/// is a click that fires from nowhere.
#[test]
fn the_play_button_is_claimed_exactly_where_it_is_drawn() {
    use tui_render::{activation, frame_with_hits, Target, Via};
    let v = mic_vs();
    let (lines, hits) = frame_with_hits(&v, now(), W, H);

    let row = lines
        .iter()
        .position(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .contains("the spoken one")
        })
        .expect("the ready row is on screen") as u16;

    // The triangle's own columns resolve to the button — row 1, because the
    // TYPED entry is row 0 and the index travels with the target.
    assert_eq!(
        hits.hit(2, row),
        Target::MicPlay(1),
        "the play button is not clickable"
    );
    // …and the text beside it does not.
    assert!(
        !matches!(hits.hit(30, row), Target::MicPlay(_)),
        "the button's claim ran over the row's text"
    );
    // A click on it plays; a click on the row body does nothing.
    assert_eq!(
        activation(Target::MicPlay(1), Mode::Mic, Via::Click),
        Some(tui_render::Intent::MicPlay(1))
    );
    assert_eq!(activation(Target::MicRow(1), Mode::Mic, Via::Click), None);
    // **THE TYPED ROW CLAIMS NO BUTTON COLUMNS AT ALL** — the mouse falls
    // through where there is nothing drawn.
    let typed_row = lines
        .iter()
        .position(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .contains("the typed one")
        })
        .expect("the typed row is on screen") as u16;
    assert!(
        !matches!(hits.hit(2, typed_row), Target::MicPlay(_)),
        "a typed row claimed play-button columns it never painted"
    );
}

/// **THE QUEUE VIEW SHOWS EVERY ITEM — the daemon's half AND this face's.**
/// `DIR-6` asks for a way to see the queue's contents; mic mode gives that list
/// a second half, and the two are named apart so neither count reads as the
/// other.
#[test]
fn the_queue_view_shows_the_client_queue_under_the_daemons() {
    let mut v = mic_vs();
    v.mode = Mode::Queue;
    v.queue = QueueView {
        thread: "57db2ec9".into(),
        waiting: Vec::new(),
        failed: Vec::new(),
        state: Fetch::Loaded,
    };
    assert_shape(&v, W, 40);
    let joined = text(&v, W, 40).join("\n");
    assert!(
        joined.contains("held here (4)"),
        "the client queue is not in the view:\n{joined}"
    );
    assert!(
        joined.contains("the spoken one"),
        "a transcribed row is missing"
    );
    assert!(
        joined.contains("transcribing…"),
        "an in-flight row is missing"
    );
    assert!(
        joined.contains("▶"),
        "the play button is missing from the queue view"
    );
    // The two halves are NAMED APART.
    assert!(
        joined.contains("waiting ("),
        "the daemon's half lost its name"
    );
    assert!(
        joined.contains("not sent, and the daemon has not seen them"),
        "the client half does not say whose queue it is"
    );
}

// ────────────────────────────────────────────────────────────────────────────
//  THE SHEET (§5) — details and options, one door
// ────────────────────────────────────────────────────────────────────────────

/// **THE SHEET AS §5 DRAWS IT: facts on top, acts below, `←` closes.**
///
/// Absent DELIBERATELY and asserted as absent below: the account, the working
/// directory, and the id — *"the sheet describes your thread, not the daemon's
/// row."*
#[test]
fn the_sheet_draws_its_facts_then_its_acts() {
    let v = sheet_vs(None, None);
    assert_shape(&v, W, H);
    let lines = text(&v, W, H);
    golden(
        &lines[..11],
        &[
        "Fix the viewport defects and the resize repaint this thread · 5 unread elsewhere · connected · acct…",
        "   harness · started Sep 12 · 41 turns · last turn 2m ago                                           ",
        "                                                                                                    ",
        "  ▸ model opus — click to change                                                                    ",
        "    interrupt the running turn                                                                      ",
        "    compact the thread — instructions optional, typed below                                         ",
        "    rename — edit the summary in place                                                              ",
        "    pin to the root                                                                                 ",
        "    move up                                                                                         ",
        "    move down                                                                                       ",
        "    mark closed                                                                                     ",
    ],
    );
    let whole = lines.join("\n");
    for absent in ["~/minds", "account", &"11111111"[..8]] {
        assert!(
            !whole.contains(absent),
            "the sheet is showing {absent:?} — §5 cuts the account, the cwd and the id"
        );
    }
}

/// **THE DEGRADED SHEET — `0060` §5-degraded, drawn.**
///
/// The four AUI-owned rows keep their POSITIONS and carry the reason in words.
/// The two daemon-owned rows still work, because the daemon is still there —
/// which is the whole argument for binding each act to its own owner.
///
/// FORCED RED by removing an unavailable row instead of muting it (the row
/// count and every line after it shift), and by dropping the reason (the rows
/// read `model opus` with nothing after them, which is a grey row with no
/// words — the thing `0060`'s temporal rule forbids by name).
#[test]
fn an_unreachable_owner_is_a_sentence_on_the_row_and_never_a_missing_row() {
    let down = "needs the audio interface, which is not running";
    let v = sheet_vs(Some(down), None);
    assert_shape(&v, W, H);
    let lines = text(&v, W, H);
    golden(
        &lines[3..11],
        &[
            "  ▸ model opus — needs the audio interface, which is not running                                    ",
            "    interrupt the running turn                                                                      ",
            "    compact the thread — instructions optional, typed below                                         ",
            "    rename — needs the audio interface, which is not running                                        ",
            "    pin to the root — needs the audio interface, which is not running                               ",
            "    move up — needs the audio interface, which is not running                                       ",
            "    move down — needs the audio interface, which is not running                                     ",
            "    mark closed — needs the audio interface, which is not running                                   ",
        ],
    );
    // **THE ROW COUNT IS THE SAME IN BOTH STATES.** A row that comes and goes
    // reshapes the menu under his muscle memory — his own ruling, already
    // applied to `archived (0)`.
    let ready = text(&sheet_vs(None, None), W, H);
    assert_eq!(
        lines.len(),
        ready.len(),
        "the degraded sheet is a different height from the working one"
    );
    for i in 3..11 {
        assert_eq!(
            lines[i].trim_start().starts_with('▸'),
            ready[i].trim_start().starts_with('▸'),
            "the cursor moved because a row went unavailable"
        );
    }
}

/// **THE DOOR-SHUT SENTENCE IS A DIFFERENT, DISTINCT ONE FROM "NOT RUNNING."**
///
/// `apps/tui/src/aui.rs`'s `DownCause` has two members because the TUI can
/// honestly tell exactly two things apart — refused (the process is down) and
/// shut (the configured door cannot be reached at all) — and `0060` fixed
/// both strings verbatim, chosen by what the socket returned. This no longer
/// names Tailscale: since the 9010 client dials loopback by default
/// (`apps/tui/src/aui.rs`'s `endpoint_from`), suggesting he check Tailscale
/// would point him at the wrong half of the system for the common case.
#[test]
fn a_shut_door_is_its_own_sentence_distinct_from_not_running() {
    let v = sheet_vs(Some("the audio interface's door is not open"), None);
    let lines = text(&v, W, H);
    assert!(
        lines[3].contains("the audio interface's door is not open"),
        "the row does not carry the door-shut sentence verbatim: {:?}",
        lines[3]
    );
    assert!(
        !lines[3].contains("not running"),
        "the door-shut row must not read as the refused/not-running sentence: {:?}",
        lines[3]
    );
}

/// **BOTH OWNERS DOWN, AND THE SHEET STILL OPENS.**
///
/// *"A sheet that will not open is a door that lies."* Its facts are file-read
/// and stay true as history; every act row carries its own reason.
#[test]
fn the_sheet_still_opens_with_both_owners_unreachable() {
    let v = sheet_vs(
        Some("needs the audio interface, which is not running"),
        Some("waits for the connection"),
    );
    assert_shape(&v, W, H);
    let lines = text(&v, W, H);
    // The facts survive: they came from the index, not from a socket.
    assert!(
        lines[1].contains("harness · started Sep 12 · 41 turns"),
        "{:?}",
        lines[1]
    );
    assert!(lines[4].contains("interrupt the running turn — waits for the connection"));
    assert!(lines[5].contains("compact the thread — waits for the connection"));
    // Eight rows, still, in the same order.
    let acts: Vec<&str> = lines[3..11].iter().map(|l| l.trim()).collect();
    assert_eq!(acts.len(), 8);
}

/// **AN ACT ROW IS PRESSABLE, AND AN UNAVAILABLE ONE IS PRESSABLE TOO —
/// FROM THE KEYBOARD, WHICH IS THE ONLY GESTURE THAT PRESSES NOW.**
///
/// It returns the SAME intent as any other row — the shell answers it by
/// repeating the row's sentence in the notice line. A different intent here
/// would have made the key table know which process owns a row; no intent at
/// all would have made it a silent no-op, which is the stub the doctrine names.
///
/// **THE GESTURE CHANGED IN ROUND SIX AND THE HIT SPANS DID NOT.** A click no
/// longer presses an act row — some of them open a FIELD, which is a mode
/// change, and the rest mutate; neither is a pointer move. The span is still
/// claimed, because `Target::SheetRow(i)` is what is drawn there and a hit map
/// that said otherwise would be describing a different frame. `→`/`enter` over
/// the sheet's own cursor is the door and `↑`/`↓` walk to every row.
#[test]
fn every_sheet_row_activates_from_the_keyboard_including_the_ones_that_cannot_act() {
    let v = sheet_vs(
        Some("needs the audio interface, which is not running"),
        None,
    );
    let (_, hits) = frame_with_hits(&v, now(), W, H);
    let b = tui_render::bands(&v, W, H);
    // Two rows of facts (the line and the blank) before the acts begin.
    let first = b.header + 2;
    for i in 0..8 {
        let row = (first + i) as u16;
        assert_eq!(
            hits.hit(4, row),
            Target::SheetRow(i),
            "act row {i} is not a click target"
        );
        assert_eq!(
            activation(Target::SheetRow(i), Mode::Sheet, Via::Key),
            Some(Intent::SheetAct(i)),
            "an act row lost its keyboard door"
        );
        assert_eq!(
            activation(Target::SheetRow(i), Mode::Sheet, Via::Click),
            None,
            "a click still presses act row {i}"
        );
    }
    // **AVAILABILITY CHANGES NOTHING ABOUT THE INTENT.** Row 0 cannot act (the
    // AUI is down in this fixture) and row 1 can; both resolve to the same
    // SHAPE, differing only in the index they name. A table that branched here
    // would know which process owns a row, which the pure crate must not.
    assert!(matches!(
        activation(Target::SheetRow(0), Mode::Sheet, Via::Key),
        Some(Intent::SheetAct(0))
    ));
    assert!(matches!(
        activation(Target::SheetRow(1), Mode::Sheet, Via::Key),
        Some(Intent::SheetAct(1))
    ));
    assert!(
        v.sheet.as_ref().expect("a sheet").rows[0]
            .avail
            .reason()
            .is_some()
            && v.sheet.as_ref().expect("a sheet").rows[1].avail.is_ready(),
        "this fixture no longer has one unavailable row and one ready one, \
         so the assertion above proves nothing"
    );
}

/// **`←` CLOSES, AND IT IS THE ONLY MODE WHERE IT MEANS THAT.**
///
/// At the root `←` jumps to the top and never exits; in a thread the composer
/// owns it. The sheet is its own grammar, which is why it is a `Mode`.
#[test]
fn left_closes_the_sheet_and_the_field_keeps_what_is_typed() {
    use tui_render::{intent, Key};
    assert_eq!(intent(Key::Left, Mode::Sheet), Intent::Back);
    assert_eq!(intent(Key::Esc, Mode::Sheet), Intent::Back);
    assert_eq!(intent(Key::Right, Mode::Sheet), Intent::Activate);
    assert_eq!(intent(Key::Enter, Mode::Sheet), Intent::Activate);
    // **IN THE FIELD THE LETTERS ARE TEXT.** `c` types a `c`; it does not
    // compact. That single fact is why the field is a second mode.
    assert_eq!(intent(Key::Char('c'), Mode::SheetField), Intent::Type('c'));
    // **AND IN THE SHEET `c` IS NOW THE SECOND PRESS OF A DE-REGISTRATION**
    // (round five). This line read `Intent::None` until then, and the fact it
    // was guarding is untouched: `c` still does not COMPACT here, and it fires
    // nothing on its own — on a project sheet it is the confirmation the
    // `remove` row is already asking for in words, and on a thread sheet it
    // resolves to no project and is a free miss. The operator's grammar is
    // *"`c` plus confirmation, `Enter` or a second `c`"*, and binding only
    // `enter` would have made the row's own *"press again"* a lie about the
    // key he had just pressed.
    assert_eq!(
        intent(Key::Char('c'), Mode::Sheet),
        Intent::DeregisterProject
    );
    // Enter COMMITS the field; esc leaves it keeping the words.
    assert_eq!(intent(Key::Enter, Mode::SheetField), Intent::SheetCommit);
    assert_eq!(intent(Key::Esc, Mode::SheetField), Intent::Back);
    // …and `←` in the field is a CURSOR MOVE, not a door. A door bound to the
    // key that moves the caret would eat a keystroke he meant as motion.
    assert_eq!(
        intent(Key::Left, Mode::SheetField),
        Intent::Compose(tui_render::Edit::Left)
    );
}

/// The field draws under the acts and names itself in its own border, the way
/// the composer band does — so he never has to remember which row opened it.
#[test]
fn the_typed_field_draws_below_the_acts_and_names_itself() {
    let mut v = sheet_vs(None, None);
    v.mode = Mode::SheetField;
    if let Some(s) = v.sheet.as_mut() {
        s.field = Some(tui_render::SheetField {
            act: SheetAct::Compact,
            label: "instructions",
            buf: tui_render::compose::Composer::from_text("keep the wire notes"),
        });
    }
    assert_shape(&v, W, H);
    let lines = text(&v, W, H);
    // The LAST occurrence: the compact ROW also says "instructions optional",
    // and the field's border is below it — which is exactly the relationship
    // being asserted, so the search has to run from the bottom.
    let border = lines
        .iter()
        .rposition(|l| l.contains("instructions"))
        .expect("the field names itself");
    assert!(border > 10, "the field is above the acts");
    assert!(
        lines[border].contains("enter does it · esc keeps it"),
        "the field does not say what its two keys do: {:?}",
        lines[border]
    );
    assert!(lines[border + 1].contains("keep the wire notes"));
}

// ────────────────────────────────────────────────────────────────────────────
//  THE PROJECT SHEET (§5's second frame), THE ARCHIVED ROOM (§1) AND
//  `+ new project` (§10)
// ────────────────────────────────────────────────────────────────────────────

/// **THE PROJECT SHEET.** `bucket` switches the fixture between an ordinary
/// project and the un-projected one, because they are the SAME sheet asked
/// about two different KINDS of thing — which is exactly what the absence rule
/// below turns on.
fn project_sheet_vs(bucket: bool, aui: Option<&str>) -> ViewState {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Sheet;
    v.header.title = if bucket { "miscellaneous" } else { "harness" }.into();
    v.header.cwd = String::new();
    v.header.account = Known::Unknown;
    v.header.queued = Known::Known(0);
    v.header.queue_scope = "across his threads".into();
    v.blocks = Vec::new();
    let a = |act: SheetAct, stem: &str, hint: Option<&str>| SheetRow {
        act,
        stem: stem.into(),
        hint: hint.map(str::to_string),
        avail: match aui {
            Some(w) => Avail::Unavailable(w.into()),
            None => Avail::Ready,
        },
    };
    let mut rows = vec![a(
        SheetAct::ProjectModel,
        "model ?",
        Some("click to change"),
    )];
    // **THE TWO ROWS THE BUCKET DOES NOT HAVE.** Not built and then hidden —
    // never built, which is the shape `store::project_sheet` produces.
    if !bucket {
        rows.push(a(
            SheetAct::Archive,
            "archive harness",
            Some("leaves the root, threads kept, one click brings it back"),
        ));
        rows.push(a(SheetAct::Remove, "remove harness", Some("asks first")));
    }
    v.sheet = Some(SheetView {
        title: v.header.title.clone(),
        facts: Facts::Project(ProjectFacts {
            name: v.header.title.clone(),
            threads: if bucket { 7 } else { 48 },
            running: Known::Known(if bucket { 0 } else { 1 }),
            oldest: Some(Utc.with_ymd_and_hms(2025, 9, 3, 9, 0, 0).unwrap()),
            bucket,
        }),
        cursor: 0,
        rows,
        field: None,
        armed: None,
    });
    v
}

/// **THE PROJECT SHEET AS §5 DRAWS IT: facts on top, three acts below.**
///
/// The facts line is `0040` §5's own — `48 threads · 1 running · oldest Sep
/// 2025` — and every number on it is computed from what the root already
/// loaded. Nothing here is sourced from 9010, which is `0060`'s second binding
/// condition: the sheet draws identically with the audio interface stopped.
///
/// FORCED RED by dropping the running count from `project_facts_line`, and by
/// printing `oldest` with a day as well as a month.
#[test]
fn the_project_sheet_draws_its_facts_then_its_three_acts() {
    let v = project_sheet_vs(false, None);
    assert_shape(&v, W, H);
    let lines = text(&v, W, H);
    golden(
        &lines[..6],
        &[
        "harness across his threads · 5 unread elsewhere · connected · acct ?                                ",
        "   48 threads · 1 running · oldest Sep 2025                                                         ",
        "                                                                                                    ",
        "  ▸ model ? — click to change                                                                       ",
        "    archive harness — leaves the root, threads kept, one click brings it back                       ",
        "    remove harness — asks first                                                                     ",
    ],
    );
}

/// **`miscellaneous` GETS A TWO-ROW SHEET, AND ITS MISSING ACTS ARE ABSENT —
/// NOT GREYED — WITH THE REASON IN WORDS ON THE FACTS LINE.**
///
/// The distinction the whole round turns on, drawn. Everywhere else on this
/// face an act that cannot be performed KEEPS ITS ROW and wears its reason,
/// because that reason will lift. The bucket's never will: it is the
/// un-projected home, it is not under `~/minds`, there is no symlink to
/// remove. **A greyed `archive` here would promise an outage that can never
/// end** — it would say "not now" about a thing that is "not ever".
///
/// **WITNESS, keyed on something the absence cannot move**: the ORDINARY
/// project's sheet is drawn in the same test at the same size and DOES carry
/// both rows. Without it, a renderer that drew no act rows at all — or one
/// that failed to draw the sheet — would score a triumphant pass on an
/// assertion about two missing words.
///
/// FORCED RED by drawing the two rows greyed for the bucket instead of
/// omitting them, which is the exact failure the design names in both
/// directions.
#[test]
fn the_buckets_sheet_draws_two_rows_and_its_missing_acts_are_absent_not_greyed() {
    // THE WITNESS FIRST, at the identical size and owner state.
    let ordinary = text(&project_sheet_vs(false, None), W, H).join("\n");
    assert!(
        ordinary.contains("archive harness") && ordinary.contains("remove harness"),
        "THE WITNESS FAILED: an ordinary project's sheet does not draw the two rows \
         either, so their absence on the bucket proves nothing:\n{ordinary}"
    );

    let v = project_sheet_vs(true, None);
    assert_shape(&v, W, H);
    let lines = text(&v, W, H);
    golden(
        &lines[..5],
        &[
        "miscellaneous across his threads · 5 unread elsewhere · connected · acct ?                          ",
        "   7 threads · oldest Sep 2025 · the un-projected bucket — it is not archivable and not removable   ",
        "                                                                                                    ",
        "  ▸ model ? — click to change                                                                       ",
        "                                                                                                    ",
    ],
    );
    let whole = lines.join("\n");
    // ABSENT — the WORDS are nowhere on the frame, in any style. An assertion
    // about a colour would pass on a row drawn grey.
    for gone in ["archive", "remove"] {
        assert!(
            !whole.contains(gone),
            "the bucket's sheet drew {gone:?} — a greyed row here promises an outage \
             that can never lift:\n{whole}"
        );
    }
    // …and the facts line SAYS WHY, where he is already looking, rather than
    // leaving him to interpret a silence.
    assert!(
        whole.contains("the un-projected bucket — it is not archivable and not removable"),
        "the bucket's sheet is two rows short and says nothing about it"
    );
}

/// **THE ARCHIVED ROOM (§1's door, opened).** One row per archived project,
/// newest first, each with the age since he archived it.
///
/// It is read off `~/.aui/archived-projects.json` by this face and never off
/// 9010, so the room draws with the audio interface stopped — what needs the
/// AUI is the unarchive, and that is a sentence on the row.
///
/// FORCED RED by sorting oldest-first, and by dropping the age column.
#[test]
fn the_archived_room_draws_a_row_per_project_newest_first() {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Archived;
    v.header.title = "archived · 3".into();
    v.header.cwd = String::new();
    v.header.account = Known::Unknown;
    v.header.queued = Known::Known(0);
    v.header.queue_scope = "across his threads".into();
    v.blocks = Vec::new();
    v.archived = ArchivedView {
        cursor: 0,
        rows: vec![
            ArchivedRow {
                name: "dead".into(),
                since: Some(now() - TimeDelta::days(2)),
                avail: Avail::Ready,
            },
            ArchivedRow {
                name: "older".into(),
                since: Some(now() - TimeDelta::days(40)),
                avail: Avail::Ready,
            },
            // **A ROW WITH NO DATE KEEPS ITS ROW AND WEARS `?`.** `None` is "we
            // do not know", not "a long time ago" — honesty rule 1 — so it
            // sorts LAST rather than first and is never dropped.
            ArchivedRow {
                name: "undated".into(),
                since: None,
                avail: Avail::Ready,
            },
        ],
    };
    assert_shape(&v, W, H);
    let lines = text(&v, W, H);
    golden(
        &lines[1..5],
        &[
            "▸ dead — click to bring it back                                                               2d 00h",
            "  older — click to bring it back                                                             40d 00h",
            "  undated — click to bring it back                                                                 ?",
            "                                                                                                    ",
        ],
    );
}

/// **THE UNREACHABLE OWNER IS A SENTENCE ON THE ARCHIVED ROW, AND THE ROOM
/// STILL DRAWS.**
///
/// The same temporal rule as the sheet's, in the one room whose CONTENT does
/// not depend on the owner at all. A face that showed an empty room because
/// the AUI was down would be sourcing a rendered fact from 9010.
///
/// FORCED RED by emptying the room when the owner is down, and by dropping the
/// reason from the row.
#[test]
fn the_archived_room_draws_with_its_owner_down_and_each_row_says_why() {
    let down = "needs the audio interface, which is not running";
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Archived;
    v.header.title = "archived · 1".into();
    v.header.cwd = String::new();
    v.header.account = Known::Unknown;
    v.header.queued = Known::Known(0);
    v.header.queue_scope = "across his threads".into();
    v.blocks = Vec::new();
    v.archived = ArchivedView {
        cursor: 0,
        rows: vec![ArchivedRow {
            name: "dead".into(),
            since: Some(now() - TimeDelta::days(2)),
            avail: Avail::Unavailable(down.into()),
        }],
    };
    assert_shape(&v, W, H);
    let whole = text(&v, W, H).join("\n");
    assert!(
        whole.contains("dead"),
        "the room went empty because its owner is down — it is read off the \
         filesystem and nothing it renders comes from 9010:\n{whole}"
    );
    assert!(
        whole.contains(down),
        "the muted row does not say why it cannot act:\n{whole}"
    );
}

/// **`+ new project` IS AN INVITATION, A FIELD AND A SENTENCE — AT ONE
/// POSITION, ALWAYS.**
///
/// §10's row, drawn in all three of its states. Resting it invites a name;
/// while he is typing **it IS the field, in place**, with no confirm scope
/// (§1: *"read-back exists for voice, not for typing"*); and with the audio
/// interface unreachable it wears that sentence instead of the invitation,
/// muted, at the SAME position. It never disappears and it never silently does
/// nothing — the same ruling that keeps `archived (0)` drawn.
///
/// FORCED RED by dropping the caret while typing (the field reads as a label
/// that changed rather than as a box), and by removing the row when the owner
/// is down (the row count changes and the assertion on the third state fails).
#[test]
fn the_new_project_row_is_an_invitation_a_field_and_a_sentence() {
    let row_of = |v: &ViewState| -> (usize, String) {
        let lines = text(v, W, H);
        let i = lines
            .iter()
            .position(|l| l.trim_start().starts_with("+ "))
            .unwrap_or_else(|| {
                panic!(
                    "no `+ new project` row is on the frame:\n{}",
                    lines.join("\n")
                )
            });
        (i, lines[i].clone())
    };

    // 1 — RESTING: the invitation.
    let resting = projects_vs();
    let (at_rest, invite) = row_of(&resting);
    assert_eq!(
        invite.trim_end(),
        // FOUR columns, not two: it is the last row of the PROJECTS band and
        // wears the band indent `0072` gave every contained row.
        "    + new project — type its name",
        "the resting row is not §10's invitation"
    );

    // 2 — TYPING: the row IS the field, with the caret after what he typed.
    let mut typing = projects_vs();
    let mut c = tui_render::compose::Composer::new();
    for ch in "new mind".chars() {
        c.insert_char(ch);
    }
    typing.projects.new_project = Some(c);
    let (at_typing, field) = row_of(&typing);
    assert_eq!(
        field.trim_end(),
        "    + new mind█",
        "the row is not the field, in place, with the caret after his name"
    );

    // 3 — OWNER DOWN: the sentence, muted, at the same position.
    let down = "needs the audio interface, which is not running";
    let mut shut = projects_vs();
    shut.projects.aui_down = Some(down.into());
    let (at_down, sentence) = row_of(&shut);
    assert_eq!(
        sentence.trim_end(),
        format!("    + new project — {down}"),
        "the unreachable row does not carry the owner's sentence"
    );

    // **ONE POSITION, THREE STATES.** A row that moved between them would
    // reshape the menu under his muscle memory, which is the ruling this row
    // and `archived (0)` were both drawn by.
    assert_eq!(
        (at_rest, at_typing, at_down),
        (at_rest, at_rest, at_rest),
        "the `+ new project` row moved between its states"
    );
}

/// **THE FACTS LINE NEVER SAYS `0 running`, AND NEVER SAYS `0` FOR A COUNT IT
/// DOES NOT HAVE.**
///
/// Two of this face's standing rules meeting on one line. §1's: *"a column of
/// zeros is noise"* — nothing executing is the ordinary case, so it is silent.
/// Honesty rule 1's: `Unknown` is NOT `0` — before the daemon has answered
/// once, the sheet says `? running` rather than claiming the project is idle.
///
/// **This exists because a forced failure found it unguarded.** Removing the
/// zero-suppression arm from `project_facts_line` left every named test green:
/// the fixtures that cover the rule at all covered it by accident, with no
/// test naming it, which is precisely an inherited green that is not coverage.
///
/// FORCED RED by removing the `Known::Known(0) => {}` arm (the first half
/// fails with `0 running` on the line), and by mapping `Unknown` to `0` (the
/// second fails).
#[test]
fn the_facts_line_never_says_zero_running_and_never_says_zero_for_unknown() {
    let line_of = |running: Known<u32>| -> String {
        let mut v = project_sheet_vs(false, None);
        let Some(sh) = v.sheet.as_mut() else {
            panic!("no sheet")
        };
        let Facts::Project(f) = &mut sh.facts else {
            panic!("not a project's facts")
        };
        f.running = running;
        text(&v, W, H)[1].trim().to_string()
    };

    // WITNESS, keyed on something the suppression cannot move: the thread
    // count is on the line in every state. Without it, an assertion that a
    // string is ABSENT would pass on a facts line that failed to render.
    for r in [Known::Known(0), Known::Known(3), Known::Unknown] {
        assert!(
            line_of(r).contains("48 threads"),
            "THE WITNESS FAILED: the facts line did not render at all for {r:?}"
        );
    }

    assert_eq!(
        line_of(Known::Known(0)),
        "48 threads · oldest Sep 2025",
        "nothing is running, and the line said so anyway — a column of zeros is noise"
    );
    assert_eq!(
        line_of(Known::Known(3)),
        "48 threads · 3 running · oldest Sep 2025",
        "a real running count is not on the line"
    );
    // **THE ONE THAT MATTERS MOST.** `Unknown` reported as `0` would tell him
    // the project is quiet when the truth is that nobody has asked yet.
    assert_eq!(
        line_of(Known::Unknown),
        "48 threads · ? running · oldest Sep 2025",
        "an unknown running count was reported as a fact"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
//  THE COLLAPSE — 80 AND 60 COLUMNS (`0040` §11)
//
//  §11's law is absolute and ORDERED: decoration goes first, metadata second,
//  CONTENT NEVER. Message text wraps; the summary ellipsizes last. Two things
//  never go at any width — the QUEUE COUNT (`DIR-6`, "visible at all times",
//  which §11 spells out as meaning 60 columns too) and the TITLE'S DOOR, which
//  §11 says ellipsizes but survives *"so the sheet never becomes unreachable"*.
//
//  **NOTHING BELOW WAS COVERED BEFORE.** The whole collapse — the header's
//  drop order, the band rows' column widths, the banner, the sheet's rows —
//  was rewritten in `0064` and all 380 inherited tests stayed GREEN through
//  every step of it. That is the shared fragment's rule arriving as a
//  measurement rather than as advice: an inherited green is not coverage, and
//  at 80 and 60 columns this face had none.
// ─────────────────────────────────────────────────────────────────────────────

/// The three widths §11 is about, and the only three this section asserts at.
const COLLAPSE_WIDTHS: [u16; 3] = [100, 80, 60];

/// **THE DISPLAY COLUMN a substring starts at — never its byte offset.**
///
/// `str::rfind` returns BYTES, and this face's rows are full of `—`, `…`, `▲`
/// and `▌`, each three bytes and one column. The first version of the
/// share assertion below compared a byte offset to a column count, so every
/// multi-byte glyph to the left of the boundary inflated the measurement — and
/// it passed a forced failure that had genuinely broken the behaviour, by four
/// bytes' worth of em dash and ellipsis. It is the crate's own standing warning
/// about byte offsets, arriving in a test written to defend it.
fn col_of(row: &str, needle: &str) -> Option<usize> {
    let byte = row.rfind(needle)?;
    Some(text_width(&row[..byte]))
}

/// **THE QUEUE COUNT NEVER GOES — `DIR-6`, AT SIXTY COLUMNS TOO.**
///
/// This is the directive the old header could not keep. MEASURED on a real pty
/// before the fix, the root header at 60 read
/// `projects · 4 under ~/minds · 2 with an index · 5 threads · …` with no `▲`
/// anywhere on the frame: the title is rank 0 and could not be dropped, so it
/// ate the row and the badge went off the end with it — a directive violated in
/// silence, on the width where it matters most.
#[test]
fn the_queue_count_survives_every_width_on_every_surface() {
    for (name, v) in [
        ("root", projects_vs()),
        // (TUI-P1a: not the thread page — its header is its name and status.)
        ("list", list_vs()),
    ] {
        for w in COLLAPSE_WIDTHS {
            assert_shape(&v, w, H);
            let header = &text(&v, w, H)[0];
            // **RE-KEYED FROM THE GLYPH TO THE WORD** (`0072`). `▲N` became
            // `N queued` because the operator could not read the glyph; the
            // COUNT is `DIR-6` and did not move. An anchor still hunting `▲`
            // would have gone red here for the right reason and then been
            // "fixed" by deletion, which is how a directive loses its only
            // machine-checked defence.
            assert!(
                header.contains("queued"),
                "DIR-6: the queue count left the {name} header at {w} columns — \
                 'visible at all times' means 60 too (0040 §11): {header:?}"
            );
            // **THE NUMBER, NOT JUST THE WORD.** The live run that produced
            // §11's own header order lost the digit and kept the `▲`; the same
            // crop can leave `queued` standing with nothing in front of it.
            let n: String = header
                .split(" queued")
                .next()
                .unwrap_or_default()
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit() || *c == '?')
                .collect();
            assert!(
                !n.is_empty(),
                "the count kept its word and lost its number at {w}: {header:?}"
            );
            // **AND THE GLYPH IS REALLY GONE**, at every width and on every
            // surface — otherwise "we dropped it" is true only where the row
            // happened to be roomy.
            assert!(
                !header.contains('▲'),
                "the glyph came back on the {name} header at {w}: {header:?}"
            );
        }
    }
}

/// **THE TITLE STAYS CLICKABLE AT 60 — INCLUDING ITS OWN ELLIPSIS.**
///
/// §11: the title *"ellipsizes but survives — and stays clickable, so the sheet
/// never becomes unreachable"*. The failure this guards is the one that cannot
/// be seen in a frame dump: a door whose hit region shrank while the row it
/// sits on looks exactly right.
///
/// It was real. `crop_with_ellipsis` used to append the `…` as a segment of its
/// own, and `render::span_of` claims the columns of ONE segment — the title's —
/// so the ellipsis belonged to no target. MEASURED on a real pty at 60 columns:
/// clicking columns 2–24 of a band row opened the sheet and column 25, the `…`,
/// did not. The one glyph on the row that says "there is more of this title
/// here" was the one column that would not open it.
///
/// **THE WITNESS IS KEYED ON SOMETHING NO MUTATION CAN MOVE**: the row's own
/// cursor target. A frame that failed to draw the band at all would otherwise
/// score a triumphant pass on an assertion about a title being reachable.
#[test]
fn the_titles_door_survives_at_sixty_and_covers_its_ellipsis() {
    // A summary LONG enough that it must ellipsize at all three widths — the
    // first version of this test used the fixture's own pinned row, which fits
    // whole at 60, so the ellipsis branch below never ran and the forced
    // failure could not bite. A check that passes because its case never
    // arises is the shape of a pass with nothing behind it.
    let mut v = projects_vs();
    let long = "a summary long enough that it cannot survive whole at any of the three                 widths this section is about, so the crop always falls on it";
    v.projects.pinned = band_rows(&[(long, "harness", "idle")]);
    let mut ellipsised = 0;
    for w in COLLAPSE_WIDTHS {
        let (lines, hits) = frame_with_hits(&v, now(), w, H);
        let row = lines
            .iter()
            .position(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .contains("a summary long enough")
            })
            .unwrap_or_else(|| panic!("the pinned band row is not on the frame at {w}"));
        let painted: String = lines[row]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();

        // THE WITNESS: this row is a band row of the root, and the hit map
        // says so at a column no title claim covers — the marker gutter. No
        // mutation of the title's span can move this, so an assertion about
        // the title below means what it says.
        // Selectable 1, not 0: the `pinned` HEADER is the stop above it since
        // `DEC-46`. Derived, never written as a literal, so the witness tracks
        // the surface instead of a number someone has to remember to bump.
        let sel = (0..v.projects.selectable())
            .find(|&i| v.projects.item(i) == Some(tui_render::RootItem::Pinned(0)))
            .expect("the pinned row is not a cursor stop");
        assert_eq!(
            hits.hit(0, row as u16),
            Target::RootRow(sel),
            "witness: the band row lost its own identity at {w}: {painted:?}"
        );

        let door: Vec<u16> = (0..w)
            // **`==`, NOT `matches!`.** `matches!(x, Target::ThreadTitle(sel))`
            // BINDS a fresh `sel` and matches every index — which is how a
            // "derived" witness quietly stopped witnessing anything. The
            // compiler said so (unused variable `sel`); the assertion would
            // not have.
            .filter(|c| hits.hit(*c, row as u16) == Target::ThreadTitle(sel))
            .collect();
        assert!(
            !door.is_empty(),
            "THE SHEET BECAME UNREACHABLE at {w} columns — no column of the \
             title row opens it (0040 §11): {painted:?}"
        );

        // Every column the TITLE painted is a door, and the `…` is one of its
        // columns. The blank past it is not.
        let last = *door.last().expect("non-empty");
        let glyph: Vec<char> = painted.chars().collect();
        assert_ne!(
            glyph.get(last as usize).copied().unwrap_or(' '),
            ' ',
            "the door ran past the title into blank space at {w}: {painted:?}"
        );
        assert!(
            painted.contains('…'),
            "the fixture stopped ellipsizing at {w}, so this test proves \
             nothing about an ellipsized title: {painted:?}"
        );
        ellipsised += 1;
        let ell = text_width(
            &painted
                .chars()
                .take_while(|c| *c != '…')
                .collect::<String>(),
        ) as u16;
        assert!(
            door.contains(&ell),
            "THE ELLIPSIS IS OUTSIDE THE DOOR at {w} columns — the one glyph \
             that says the title continues is the one column that does not \
             open it: {painted:?}"
        );
    }
    assert_eq!(
        ellipsised,
        COLLAPSE_WIDTHS.len(),
        "the ellipsis case did not arise at every width"
    );
}

/// §11's FIRST named casualty, verbatim: *"First to go: the word `connected`
/// (a healthy default is the cheapest fact on screen)"*.
///
/// Before this, `connected` survived 80 intact and reached 60 as `· connec…` —
/// a mid-word fragment of the word the doc says should already have gone,
/// produced by the rank machinery's restore-the-last-one step handing it back
/// cropped. A link in TROUBLE still appears at every width: that is not a
/// steady state, it is news.
#[test]
fn connected_is_the_first_thing_to_go_at_eighty_columns() {
    // **A HEADER WITH ROOM FOR THE LINK AT 80.** The stock fixture's title and
    // cwd already push the link off an 80-column row by RANK alone, so it
    // could not tell a face that drops `connected` deliberately from one that
    // merely ran out of room — and the forced failure passed against it.
    let mut up = vs(LinkState::Connected);
    // TUI-P1a (TC-04): the THREAD page's header is its name and status only,
    // so the header's link/queue/account machinery is exercised on the LIST
    // header, which still draws every part.
    up.mode = Mode::List;
    up.header.title = "thread".into();
    up.header.cwd = String::new();
    up.header.unread_elsewhere = Known::Known(0);
    assert!(
        text(&up, 100, H)[0].contains("connected"),
        "the healthy link belongs on a wide header"
    );
    assert!(
        text_width(&text(&up, 80, H)[0]) == 80 && text(&up, 80, H)[0].matches(" · ").count() >= 2,
        "witness: this header has room to spare at 80, so what it does NOT say \
         there is a choice: {:?}",
        text(&up, 80, H)[0]
    );
    for w in [80u16, 60] {
        let header = &text(&up, w, H)[0];
        assert!(
            !header.contains("connect"),
            "0040 §11: `connected` is the FIRST thing to go at {w} columns, and \
             it must go WHOLE rather than arrive as a fragment: {header:?}"
        );
    }
    // A link in TROUBLE still reaches him at 60 — through the BANNER, which is
    // where the header's own doc says a troubled link belongs ("a link in
    // trouble raises a BANNER over the content, which has a whole row to say
    // what went wrong"). Dropping `connected` from the header costs nothing
    // precisely because the only link state that is NEWS has a better home.
    let mut down = vs(LinkState::Disconnected {
        reason: "the daemon is not answering".into(),
        retry_at: Some(now() + TimeDelta::seconds(4)),
    });
    down.banner = Some(Banner {
        text: "the daemon is not answering — everything below is HISTORY".into(),
        severity: Severity::Warn,
    });
    down.mode = Mode::List;
    let banner = text(&down, 60, H);
    assert!(
        banner.iter().any(|l| l.contains("down 41s")),
        "a link in trouble is news at every width: {banner:#?}"
    );
}

/// **THE OUTAGE LINE'S TWO FACTS, AND WHICH ONE SURVIVES.** §11 could not rule
/// on this — the line did not exist when it was drawn — so `0064` rules it:
/// the AGE stays and the COUNTDOWN goes.
///
/// The age is monotonic and is the fact that tells a blip from a dead daemon;
/// the countdown is a prediction the next frame recomputes anyway.
#[test]
fn the_outage_line_keeps_the_age_and_drops_the_countdown_when_only_one_fits() {
    let mut v = vs(LinkState::Disconnected {
        reason: "the daemon is not answering".into(),
        retry_at: Some(now() + TimeDelta::seconds(4)),
    });
    v.banner = Some(Banner {
        text: "the daemon is not answering (connect failed: IO error: Connection refused) \
               — everything below is HISTORY and still true"
            .into(),
        severity: Severity::Warn,
    });
    let wide = text(&v, 100, H).join("\n");
    assert!(
        wide.contains("down 41s") && wide.contains("retry in 4s"),
        "both readings belong on a wide banner (0040 §9): {wide}"
    );
    let narrow = text(&v, 60, H).join("\n");
    assert!(
        narrow.contains("down 41s"),
        "THE AGE IS THE FACT THAT DIAGNOSES — it never goes: {narrow}"
    );
    assert!(
        !narrow.contains("retry in"),
        "the countdown must give way to the age and the reason at 60: {narrow}"
    );
}

/// **NO LINE EVER EXCEEDS ITS WIDTH, AND NOTHING IS CUT MID-GRAPHEME.**
///
/// `0018` measured that ratatui 0.28 PANICS on an over-reported line rather
/// than wrapping, and `IS-34` measured the emoji hazard that produces one: a
/// ZWJ family folds to two columns as a cluster and eight as characters. So
/// this drives every surface at all three widths with a cluster-hostile string
/// in the content, and checks the postcondition the whole crate is built on.
#[test]
fn nothing_overflows_and_nothing_is_cut_mid_grapheme_at_any_width() {
    // **THE CROP BOUNDARY IS SWEPT ACROSS THE CLUSTER.** One fixed string only
    // proves the boundary did not happen to land inside its emoji — which is
    // how the first version of this test passed a mutation that reintroduced
    // `0018`'s own panic class. Shifting the family by one column at a time
    // guarantees that at some offset it straddles, at every width.
    for pad in 0..10usize {
        let s = format!(
            "{}👨‍👩‍👧‍👦 a family, an accent é, and a wide 漢字 run — one summary",
            "x".repeat(pad)
        );
        let mut v = projects_vs();
        v.projects.pinned = band_rows(&[(&s, "harness", "idle")]);
        v.projects.recent = band_rows(&[(&s, "broomstick", "active")]);
        for w in COLLAPSE_WIDTHS {
            assert_shape(&v, w, H);
            for (i, line) in text(&v, w, H).iter().enumerate() {
                // A row that ends inside a cluster would leave a dangling
                // joiner or a lone modifier as its last character.
                let last = line.trim_end().chars().last().unwrap_or(' ');
                assert!(
                    last != '\u{200d}' && last != '\u{fe0f}',
                    "row {i} at {w} (pad {pad}) ends inside a grapheme cluster: {line:?}"
                );
            }
        }
    }

    // **AND THE CROP ITSELF, AT ITS SOURCE.** No fixture on this face reaches
    // `size_line`'s straddle branch — every row that could is pre-cropped by
    // `crop_with_ellipsis` or pre-wrapped first — so the repair `geom` records
    // in its own prose ("the padding is computed from what was actually TAKEN",
    // the version that "produced a line one column short whenever a
    // double-width cluster straddled the boundary") had NO test anywhere in
    // the crate. Putting it back left all 390 green. `0018` measured what a
    // short or long line costs here: ratatui 0.28 panics rather than wrapping.
    for w in 4..40usize {
        for pad in 0..6usize {
            let text = format!("{}漢字👨‍👩‍👧‍👦漢字 tail", "x".repeat(pad));
            let line = tui_render::geom::size_line(
                vec![ratatui::text::Span::raw(text.clone())],
                w,
                palette::base(),
            );
            let got: usize = line.spans.iter().map(|sp| text_width(&sp.content)).sum();
            assert_eq!(
                got, w,
                "size_line returned {got} columns for {w} (pad {pad}): {text:?}"
            );
        }
    }

    let nasty = "👨‍👩‍👧‍👦 a family, an accent é, and a wide 漢字 run — all in one summary";
    let mut root = projects_vs();
    root.projects.pinned = band_rows(&[(nasty, "harness", "idle")]);
    root.projects.recent = band_rows(&[(nasty, "broomstick", "active")]);
    let mut list = list_vs();
    for r in list.list.rows.iter_mut() {
        r.summary = nasty.into();
    }
    let mut sheet = sheet_vs(
        Some("needs the audio interface, which is not running"),
        None,
    );
    sheet.header.title = nasty.into();

    for (name, v) in [
        ("root", root),
        ("list", list),
        ("sheet", sheet),
        ("thread", vs(LinkState::Connected)),
    ] {
        for w in COLLAPSE_WIDTHS {
            // The shape law itself — exactly `w` columns on every row.
            assert_shape(&v, w, H);
            // And no row ends inside a cluster: every painted row must be a
            // sequence of whole grapheme clusters, which is what the frame
            // being re-measurable at all depends on.
            for (i, line) in text(&v, w, H).iter().enumerate() {
                assert_eq!(
                    text_width(line),
                    w as usize,
                    "row {i} of {name} at {w} is not {w} columns wide: {line:?}"
                );
            }
        }
    }
}

/// **THE ROOT AT SIXTY COLUMNS, DRAWN.** The band rows are the case §11 could
/// not have ruled on — the bands did not exist when it was written — so `0064`
/// rules them: the band ROW is content, its trailing columns are metadata, and
/// the share the content gets GROWS as the pane narrows.
///
/// MEASURED on a real pty before this: at 60 a band row spent 33 columns on
/// `harness  absent  11h 20m` and left 24 for the thread's name — §11 exactly
/// inverted, and invisible unless you look at the frame, because each column is
/// individually reasonable.
#[test]
fn the_root_collapses_at_sixty_and_the_summary_keeps_the_row() {
    let v = projects_vs();
    assert_shape(&v, 60, H);
    golden(
        &text(&v, 60, H),
        &[
            "5 unread · 1 active · 2 queued · connected                  ",
            "  pinned · 4                                                ",
            "▸   the threads daemon wire — living not…    harness      2m",
            "    see all — 3 more                                        ",
            "                                                            ",
            "  recent · 9                                                ",
            "    Fix the viewport defects and the res…    harness      2m",
            "    Answer the tax letter                 miscellan…  1h 02m",
            "    see all — 7 more                                        ",
            "                                                            ",
            "  projects · 7                                              ",
            "    broomstick   ~/minds/broomstick          69 thr       2m",
            "    harness      ~/minds/harness             64 thr   1h 18m",
            "    chess        ~/minds/chess                ? thr        ?",
            "    see all — 4 more                                        ",
            "    + new project — type its name                           ",
            "                                                            ",
            "  archived (2)                                              ",
            "                                                            ",
            "                                                            ",
            "                                                            ",
            "                                                            ",
            "                                                            ",
            "  ? keys · → open · tab activity · o options · u queue      ",
        ],
    );
}

/// §11's own second frame: the rails carry the alignment alone, the title
/// ellipsizes but survives, and the queue count is still there.
#[test]
fn the_thread_collapses_at_eighty_and_sixty() {
    let v = vs(LinkState::Connected);
    assert_shape(&v, 80, H);
    assert_shape(&v, 60, H);
    golden(
        &text(&v, 60, H),
        &[
            "answer the tax letter · active                              ",
            "                                                            ",
            "                                                            ",
            "                                                            ",
            "                                                            ",
            "                                                            ",
            "                                                            ",
            "                                                            ",
            "                                                            ",
            "── compacted (auto) · swallowed #41–#88 ────────────────────",
            "                                                            ",
            "  claude                                                    ",
            "  The frame is five bands and the event lane is one of      ",
            "  them.                                                     ",
            "    ⚙ Read apps/tui/render/src/render.rs                    ",
            "    ◇ task 9f3e215d · building step 3 ×214                  ",
            "                                                            ",
            "                                                       m2   ",
            "      Mine goes on the right.                               ",
            "    ⚠ retry #2 at 19:31 · overloaded · staleness is amb…    ",
            "────────────────────────────────────────────────────────────",
            "   type a message…                                          ",
            "────────────────────────────────────────────────────────────",
            "  ^/ keys · ^S stdout · ^O queue · ^C quit ┊ ^T mic         ",
        ],
    );
}

/// **THE SHEET'S UNREACHABLE SENTENCE WRAPS AND IS NEVER CUT.** The longest
/// string on the face, at the width where it cannot fit.
///
/// MEASURED before this: at 60 the rows read `which is not runnin`, `which i`,
/// `which is not ru` — a different lie on every row, none of them marked. It is
/// ruled CONTENT and wrapped, on this crate's own argument (`wrapped_note`: "a
/// limitation whose reason is cut off at the right margin is worse than no
/// note"), and because the sentence is the AUI's, relayed verbatim (`0063`) —
/// shortening it would be paraphrasing an owner's refusal.
#[test]
fn the_sheets_unreachable_sentence_wraps_at_sixty_and_keeps_every_word() {
    let why = "needs the audio interface, which is not running";
    let v = sheet_vs(Some(why), None);
    for w in COLLAPSE_WIDTHS {
        assert_shape(&v, w, H);
        let body = text(&v, w, H).join(" ");
        // The WITNESS: a row that CAN act is drawn in full at this width, so an
        // assertion about the unreachable sentence cannot pass on a sheet that
        // failed to draw its rows at all.
        assert!(
            body.contains("interrupt the running turn"),
            "witness: the sheet drew no act rows at {w}: {body:?}"
        );
        // **EVERY row that carries the sentence, not merely one of them.** The
        // first version joined the whole body and asked whether the sentence
        // appeared anywhere in it — and `move up — needs the audio interface,
        // which is not running` happens to fit inside 60 columns, so a sheet
        // whose every OTHER unreachable row had been hard-cropped still passed.
        // The assertion was satisfied by a different row than the one it was
        // about.
        let rows = text(&v, w, H);
        for stem in [
            "model opus",
            "rename",
            "pin to the root",
            "move down",
            "mark closed",
        ] {
            let joined: String = rows
                .iter()
                .skip_while(|l| !l.contains(stem))
                .take(3)
                .map(|l| l.trim())
                .collect::<Vec<_>>()
                .join(" ");
            let flat: String = joined.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                flat.contains(why),
                "THE EXPLANATION WAS CUT on the `{stem}` row at {w} columns — a \
                 truncated reason reads as a broken face, not a narrow one \
                 (0040 §11: content never goes): {flat:?}"
            );
        }
    }
}

/// **THE FIRST-RUN TEACHING BAND IS CONTENT.** It is the only place the root's
/// grammar is taught, and the `?` it names is what would teach the rest — so
/// the half that gets cut is the half that would have recovered the other half.
///
/// MEASURED before this: at 80 it read `… · ? a` and at 60 it stopped at
/// `click a title`, both mid-word and unmarked.
#[test]
fn the_first_run_band_wraps_and_keeps_every_key_it_teaches() {
    let mut v = bare_root_vs();
    let hint = "first run · → or click opens · ← goes back · \
                click a title for its sheet · ? all ten keys";
    v.banner = Some(Banner {
        text: hint.into(),
        severity: Severity::Unknown,
    });
    for w in COLLAPSE_WIDTHS {
        assert_shape(&v, w, H);
        let lines = text(&v, w, H);
        // WITNESS: the root drew its own chrome at this width, so the band
        // assertions below are about the band and not about a blank frame.
        assert!(
            lines.iter().any(|l| l.contains("projects")),
            "witness: the root did not draw at {w}: {lines:#?}"
        );
        let flat: String = lines
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        for taught in ["→ or click opens", "← goes back", "? all ten keys"] {
            assert!(
                flat.contains(taught),
                "the first-run band lost `{taught}` at {w} columns — teaching \
                 text is content and content never goes (0040 §11): {flat:?}"
            );
        }
    }
}

/// **CONTENT'S SHARE OF A LIST ROW GROWS AS THE PANE NARROWS.** That is §11's
/// ordering stated as arithmetic, and it is the property `render::content_floor`
/// exists to hold. A fixed floor is what produced the inversion: 23 columns is
/// generous on a 150-column pane and derisory on a 60-column one.
#[test]
fn a_narrow_band_row_gives_the_summary_more_of_the_row_not_less() {
    // The floor content is guaranteed at each width, as a FRACTION of the row.
    // Stated as a floor rather than as monotonic growth because at wide widths
    // the right-hand columns are a fixed size and the share rises on its own —
    // the property is about what the row GUARANTEES the summary, not about the
    // curve.
    let v = projects_vs();
    for (w, floor) in [(150u16, 0.5f64), (100, 0.5), (80, 0.5), (60, 0.66)] {
        assert_shape(&v, w, H);
        let row = text(&v, w, H)
            .into_iter()
            .find(|l| l.contains("threads daemon wire"))
            .unwrap_or_else(|| panic!("the band row vanished at {w}"));
        // Where the right-hand columns begin: the project name is rank 0 there
        // and is never dropped, so it is the boundary at every width.
        let tail_at = col_of(&row, "harness").expect("the project column survives") as f64;
        let share = tail_at / w as f64;
        assert!(
            share >= floor,
            "the summary got {share:.2} of the row at {w} columns and the floor \
             is {floor:.2} — metadata surviving at content's expense is 0040 \
             §11 inverted: {row:?}"
        );
    }
    // And the 60-column case specifically: BEFORE this work the summary had 24
    // of 60 columns (0.40) while `harness  absent  11h 20m` had 33. That number
    // is what this test exists to stop coming back.
    let row = text(&v, 60, H)
        .into_iter()
        .find(|l| l.contains("threads daemon wire"))
        .expect("the band row");
    let tail_at = col_of(&row, "harness").expect("the project column") as f64;
    assert!(
        tail_at >= 40.0,
        "at 60 columns the summary has {tail_at} columns; it had 24 before 0064 \
         and the collapse is supposed to have moved it the other way: {row:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
//  THE SUBAGENT TREE
// ─────────────────────────────────────────────────────────────────────────────

/// A thread with one running subagent, its own running child, one done and one
/// failed — the shape the design's mock draws.
fn tree_vs() -> ViewState {
    let mut v = vs(LinkState::Connected);
    v.tree = AgentTree {
        rows: vec![
            TreeRow {
                id: TreeId::Parent,
                depth: 0,
                last_sibling: false,
                label: "scout the raw list".into(),
                ..TreeRow::default()
            },
            TreeRow {
                id: TreeId::Agent("a1".into()),
                depth: 1,
                last_sibling: false,
                label: "Explore · scout the raw list".into(),
                state: SubagentState::Running,
                started: Some(at(19, 25, 48)),
                tokens: Some(31_000),
                calls: Some(12),
                bash_in_flight: Some(1),
                agents_running: Some(1),
                ..TreeRow::default()
            },
            TreeRow {
                id: TreeId::Agent("a2".into()),
                depth: 2,
                last_sibling: true,
                label: "Explore · sub-scout the daemon".into(),
                state: SubagentState::Running,
                started: Some(at(19, 28, 58)),
                tokens: Some(8_000),
                calls: Some(3),
                ..TreeRow::default()
            },
            TreeRow {
                id: TreeId::Agent("a3".into()),
                depth: 1,
                last_sibling: false,
                label: "core:builder · cutover AUI registry".into(),
                state: SubagentState::Done,
                started: Some(at(19, 17, 57)),
                ended: Some(at(19, 30, 0)),
                tokens: Some(210_000),
                calls: Some(41),
                ..TreeRow::default()
            },
            TreeRow {
                id: TreeId::Agent("a4".into()),
                depth: 1,
                last_sibling: true,
                label: "Explore · re-index ownership".into(),
                state: SubagentState::Failed,
                started: Some(at(19, 29, 20)),
                ended: Some(at(19, 30, 0)),
                tokens: Some(4_000),
                calls: Some(2),
                ..TreeRow::default()
            },
        ],
        cursor: TreeId::Agent("a2".into()),
        subject: TreeId::Parent,
        // The daemon's summary of the parent's DIRECT children (R9.TS1):
        // a1 active, a3 idle, a4 an error. Drawn verbatim (TUI-P1b).
        summary: Some("1 active, 1 idle, 1 error".into()),
    };
    v
}

/// **THE SUBAGENTS ARE A FILE TREE BELOW THE BOX, AND THE PARENT IS ROW 0.**
///
/// > *I just want to see the subagents, and their subagents. It looks like a
/// > file tree. See if you can copy claude code's view. With token size, #
/// > calls, etc… Basically the text area above should be whichever one we've
/// > selected, so the parent needs to be an item in that list.*
///
/// Drawn at HIS pane (97×35) and asserted as painted text, because a flag on a
/// row says what the projection decided and not what he can see.
///
/// FORCED: return `0` from `tree_height` and every one of these goes red;
/// delete the rail from `tree_row_line` and the nesting assertion goes red
/// while the rest stay green, which is what makes the rail's own check real.
#[test]
fn the_subagent_tree_is_a_file_tree_under_the_box_with_the_thread_as_its_first_row() {
    let v = tree_vs();
    assert_shape(&v, 97, 35);
    let lines = text(&v, 97, 35);
    let band: Vec<&String> = lines
        .iter()
        .skip_while(|l| !l.contains("agents · "))
        .collect();
    assert!(!band.is_empty(), "no subagent band was drawn:\n{}", lines.join("\n"));

    // The rule line carries the DAEMON'S children summary, verbatim, and
    // never the retired `N running · M done` (TUI-P1b, TC-09).
    assert!(
        band[0].contains("1 active, 1 idle, 1 error")
            && !band[0].contains("running")
            && !band[0].contains("done"),
        "the band's rule is not the daemon's summary: {:?}",
        band[0]
    );
    // Row 0 is the thread itself, and it says so.
    assert!(
        band[1].contains("scout the raw list") && band[1].contains("this thread"),
        "the parent is not the tree's first row: {:?}",
        band[1]
    );
    // The subject marker is on the parent, whose chat is above.
    assert!(band[1].starts_with('▸'), "the subject is not marked: {:?}", band[1]);
    // NESTING: the child of a subagent is drawn under it, with rails.
    let child = band
        .iter()
        .find(|l| l.contains("sub-scout the daemon"))
        .expect("the nested subagent");
    assert!(
        child.contains("│ └"),
        "a subagent's own subagent is not nested under it: {child:?}"
    );
    // THE FIGURES HE ASKED FOR, on the row that has them.
    // The LABEL CROPS and the figures keep their columns — a row that pushed a
    // number off the edge to keep a long name whole would be the wrong trade.
    let scout = band
        .iter()
        .find(|l| l.contains("Explore · scout the raw l"))
        .expect("the scout's row");
    for want in ["running", "4m", "31k tok", "12 calls", "1 bash in flight", "1 agent"] {
        assert!(scout.contains(want), "the row is missing {want:?}: {scout:?}");
    }
    // A WORD FOR THE STATE, and a dot for live — never a spinner.
    assert!(scout.contains('●'), "a running row has no liveness dot: {scout:?}");
    assert!(
        band.iter().any(|l| l.contains("failed")),
        "a failed subagent does not say so"
    );

    // **AND THE CHAT KEPT MOST OF THE SCREEN.** The band is at most its rule
    // plus six rows whatever the tree holds.
    let b = tui_render::bands(&v, 97, 35);
    assert!(b.tree <= 1 + tui_render::TREE_MAX_ROWS, "the band ran away: {}", b.tree);
    assert!(
        b.content >= 23,
        "the chat was left {} rows at his own pane size",
        b.content
    );
}

/// **A FIGURE THAT CANNOT BE SOURCED PRINTS NOTHING**, never a zero. A number
/// on his screen that is wrong is worse than a column that is absent.
///
/// FORCED: render `bash_in_flight.unwrap_or(0)` and the "no invented zero"
/// assertion goes red.
#[test]
fn an_unsourceable_figure_is_absent_from_the_tree_row_rather_than_zero() {
    let mut v = tree_vs();
    v.tree.rows[1].tokens = None;
    v.tree.rows[1].calls = None;
    v.tree.rows[1].bash_in_flight = None;
    v.tree.rows[1].agents_running = None;
    let lines = text(&v, 97, 35);
    let row = lines
        .iter()
        .find(|l| l.contains("Explore · scout the raw list"))
        .expect("the row");
    for invented in ["0 tok", "0 calls", "0 bash", "0 agents", "tok"] {
        assert!(
            !row.contains(invented),
            "an absent figure was printed as {invented:?}: {row:?}"
        );
    }
    // …and the row is still a row: the state and the label are there.
    assert!(row.contains("running") && row.contains("scout the raw list"));
}

/// **ELAPSED IS SUBTRACTED FROM THE `now` THE FRAME IS HANDED**, so a running
/// row's age moves without anything re-projecting. ONE unchanged `ViewState`,
/// two clocks — the same proof the link banner's age gets.
///
/// FORCED: carry a formatted duration on `TreeRow` instead of its timestamps
/// and the two readings become equal.
#[test]
fn a_running_subagents_age_comes_off_the_clock_the_frame_is_asked_with() {
    let v = tree_vs();
    let early = text_at(&v, now(), 97, 35)
        .into_iter()
        .find(|l| l.contains("sub-scout the daemon"))
        .expect("the row");
    let later = text_at(&v, now() + TimeDelta::minutes(9), 97, 35)
        .into_iter()
        .find(|l| l.contains("sub-scout the daemon"))
        .expect("the row");
    assert_ne!(
        early, later,
        "a running subagent's age is frozen into the projection"
    );
    // …and an ENDED one does not move, because its end is a fact.
    let done_early = text_at(&v, now(), 97, 35)
        .into_iter()
        .find(|l| l.contains("cutover AUI registry"))
        .expect("the row");
    let done_later = text_at(&v, now() + TimeDelta::minutes(9), 97, 35)
        .into_iter()
        .find(|l| l.contains("cutover AUI registry"))
        .expect("the row");
    assert_eq!(done_early, done_later, "a finished subagent's duration moved");
}

/// **NARROW AND SHORT.** The figures drop right to left at 60 columns, and the
/// tree YIELDS BEFORE THE BOX — below its own floor it is not drawn at all,
/// which is the same answer as "this thread has no subagents".
///
/// FORCED: take the composer's minimum out of `tree_height`'s `avail` and the
/// short-frame assertion goes red — the band takes rows the box needs.
#[test]
fn the_tree_drops_figures_when_it_is_narrow_and_yields_to_the_box_when_it_is_short() {
    let v = tree_vs();
    for w in [40u16, 60, 80, 97, 120] {
        assert_shape(&v, w, 35);
    }
    let wide = text(&v, 97, 35)
        .into_iter()
        .find(|l| l.contains("12 calls"))
        .expect("the row");
    assert!(wide.contains("12 calls"));
    let narrow = text(&v, 60, 35)
        .into_iter()
        .find(|l| l.contains("● running"))
        .expect("the row");
    assert!(
        !narrow.contains("calls") && !narrow.contains("bash"),
        "60 columns still tries to print every figure: {narrow:?}"
    );
    assert!(narrow.contains("31k tok"), "60 columns dropped the tokens too: {narrow:?}");
    let tiny = text(&v, 45, 35)
        .into_iter()
        .find(|l| l.contains("● running"))
        .expect("the row");
    assert!(!tiny.contains("tok"), "45 columns still prints tokens: {tiny:?}");

    // SHORT: the box keeps its minimum and the band goes rather than squeeze it.
    for h in 10..=35u16 {
        assert_shape(&v, 97, h);
        let b = tui_render::bands(&v, 97, h);
        assert!(
            b.tree == 0 || b.composer >= tui_render::COMPOSER_MIN_ROWS as usize,
            "at height {h} the tree took the box under its minimum: {b:?}"
        );
        assert!(b.tree == 0 || b.content >= 1, "at height {h} the chat vanished");
    }
}

/// **NO SUBAGENTS, NO BAND** — the parent alone is not a tree, and the thread
/// screen is exactly what it was.
#[test]
fn a_thread_with_no_subagents_draws_no_band_at_all() {
    let v = vs(LinkState::Connected);
    assert_eq!(tui_render::bands(&v, 97, 35).tree, 0);
    assert!(!text(&v, 97, 35).join("\n").contains("agents · "));
    // …and one row (the parent) is still not a tree.
    let mut one = v.clone();
    one.tree.rows = vec![TreeRow::default()];
    assert_eq!(tui_render::bands(&one, 97, 35).tree, 0);
}

/// **THE CURSOR IS HIGHLIGHTED ONLY WHILE THE BAND HOLDS THE SELECTION.** A
/// highlight on a band the arrows are not acting on is a lie about where the
/// keyboard is.
///
/// FORCED: paint the highlight unconditionally and the Composer-mode
/// assertion goes red. The WITNESS is a row the cursor is NOT on, which no
/// mutation of the highlight can move — a whole-frame compare would differ in
/// the FOOTER whatever the highlight did, and would have reported a pass.
#[test]
fn the_tree_cursor_is_painted_only_while_the_tree_has_the_keys() {
    let mut v = tree_vs();
    let cursor_row = |v: &ViewState| -> Vec<Modifier> {
        frame(v, now(), 97, 35)
            .into_iter()
            .find(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .contains("sub-scout the daemon")
            })
            .expect("the cursor's row")
            .spans
            .iter()
            .map(|s| s.style.add_modifier)
            .collect()
    };
    let witness = |v: &ViewState| -> Vec<Modifier> {
        frame(v, now(), 97, 35)
            .into_iter()
            .find(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .contains("cutover AUI registry")
            })
            .expect("the witness row")
            .spans
            .iter()
            .map(|s| s.style.add_modifier)
            .collect()
    };
    v.mode = Mode::Thread(Region::Composer);
    let quiet = cursor_row(&v);
    let quiet_witness = witness(&v);
    v.mode = Mode::Thread(Region::Agents);
    let loud = cursor_row(&v);
    assert_ne!(
        quiet, loud,
        "the cursor's row looks the same whether or not the band has the keys"
    );
    assert_eq!(
        quiet_witness,
        witness(&v),
        "a row the cursor is not on changed — the highlight is not the thing being measured"
    );
}

/// **THE COMPOSER SAYS IT CANNOT SEND RATHER THAN LOOKING LIVE**, and it says
/// where his draft went.
///
/// FORCED: drop the `stand_in` arm from `composer` and the band draws a caret
/// against a subagent that has no inbox — every assertion here goes red.
#[test]
fn the_composer_against_a_direct_subagent_is_two_sentences_and_no_caret() {
    let mut v = tree_vs();
    v.composer.buf.insert_paste("half a message");
    v.composer.editable = false;
    v.composer.stand_in = Some(StandIn {
        what: "Explore · sub-scout the daemon".into(),
        back_to: "scout the raw list".into(),
    });
    assert_shape(&v, 97, 35);
    let painted = text(&v, 97, 35).join("\n");
    assert!(
        painted.contains("runs inside claude code; nothing can be sent to it"),
        "the band does not say why there is no box:\n{painted}"
    );
    assert!(
        painted.contains("your draft is kept"),
        "the band does not say what happened to his draft:\n{painted}"
    );
    // **HIS DRAFT IS NOT ON THE SCREEN AND IS NOT GONE.** It stays in the
    // buffer, untouched, and comes back with the parent.
    assert!(
        !painted.contains("half a message"),
        "the parent's draft is being shown under a subagent"
    );
    assert_eq!(v.composer.buf.text(), "half a message");
    // …and the band's rows are their OWN selection units, not the box's — a
    // drag across both must not address one row twice.
    // …and no click in the band resolves to a caret move.
    let (_, hits) = frame_with_hits(&v, now(), 97, 35);
    for row in 0..35u16 {
        for col in 0..97u16 {
            assert!(
                !matches!(hits.hit(col, row), Target::ComposerRow { .. }),
                "a click at {col},{row} put a caret in a box that is not there"
            );
        }
    }
}

/// **THE TREE'S ROWS ARE THEIR OWN SELECTION UNITS.**
///
/// A drag is a range over the face's LOGICAL document, and a unit is
/// `(kind, index)`. Drawing the band's rows as `UnitKind::Composer` would give
/// tree row 3 the same address as the box's fourth row, and a drag spanning the
/// two would take the wrong text out of one of them — silently, because both
/// addresses resolve.
///
/// FORCED: draw the band with `UnitKind::Composer` and the disjointness
/// assertion goes red.
#[test]
fn a_drag_cannot_address_a_tree_row_and_a_composer_row_with_one_unit() {
    let mut v = tree_vs();
    v.composer.buf.insert_paste("one\ntwo\nthree");
    let (_, hits) = frame_with_hits(&v, now(), 97, 35);
    let mut composer_units = std::collections::HashSet::new();
    let mut agent_units = std::collections::HashSet::new();
    for row in 0..35u16 {
        if let Some(t) = hits.text_row(row) {
            match t.unit.kind {
                tui_render::UnitKind::Composer => {
                    composer_units.insert(t.unit.index);
                }
                tui_render::UnitKind::Agents => {
                    agent_units.insert(t.unit.index);
                }
                _ => {}
            }
        }
    }
    assert!(!agent_units.is_empty(), "the band drew no selectable rows at all");
    assert!(!composer_units.is_empty(), "the box drew no selectable rows at all");
    // The two sets overlap by INDEX — that is the point — but they are
    // different KINDS, so no single unit names a row in both bands.
    assert!(
        agent_units.intersection(&composer_units).count() > 0,
        "the fixture is wrong: the indices do not overlap, so this proves nothing"
    );
}

// ── THE MARGINS — COUNTED IN COLUMNS ON A RENDERED FRAME ────────────────────
//
// > *the margins are too big. The agent margin should be like 10 on the right.
// > The user margin should be like 10 on the left. The tool calls can be like 8
// > on the left and 8 on the right.*
//
// **HIS PANE IS 97 COLUMNS**, which is why every number below is asserted at
// 97 and not at this file's `W`. A test that read a constant back and compared
// it to 10 would be a test of the constant: at 97 columns the constants were
// never the margins — `BOX_MAX`/`BLOB_MAX` bound first and the margin was
// whatever was left over. So these count PAINTED COLUMNS.

/// His pane, measured (`0111`).
const HIS: u16 = 97;

/// **EVERY COLUMN OF ONE RENDERED ROW, WITH THE STYLE THAT PAINTED IT.**
///
/// A margin is not countable from the characters alone: the white box pads its
/// right-hand side with SPACES in the paper style and `cols_used` trims those
/// away, so the box's two edges are a fact about the BACKGROUND. This is the
/// only honest way to read them.
fn painted_cells(line: &ratatui::text::Line<'static>) -> Vec<(char, ratatui::style::Style)> {
    let mut out = Vec::new();
    for s in &line.spans {
        for ch in s.content.chars() {
            let w = text_width(&ch.to_string()).max(1);
            for _ in 0..w {
                out.push((ch, s.style));
            }
        }
    }
    out
}

/// The characters of one rendered row, as a string — for a panic message that
/// has to say WHICH row.
fn row_string(line: &ratatui::text::Line<'static>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// The first and last column painted with `bg`, if any.
fn bg_run(cells: &[(char, ratatui::style::Style)], bg: Color) -> Option<(usize, usize)> {
    let first = cells.iter().position(|(_, s)| s.bg == Some(bg))?;
    let last = cells.iter().rposition(|(_, s)| s.bg == Some(bg))?;
    Some((first, last))
}

/// A body long enough to fill its measure with no wrap luck: one unbroken
/// word, hard-broken by [`geom::wrap`] exactly at the measure, so the row's
/// right-hand edge IS the block's right-hand edge rather than a ragged one.
fn solid() -> String {
    "x".repeat(400)
}

/// `(left margin, right margin)` of the ASSISTANT's block at `w` — its text
/// starts at the gutter and ends at the measure.
fn agent_margins(w: u16) -> (usize, usize) {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Thread(Region::Composer);
    v.blocks = vec![msg(Sender::Assistant, "m1", 19, 18, &solid())];
    let rows = text(&v, w, 40);
    let row = rows
        .iter()
        .find(|l| l.contains("xxxx"))
        .unwrap_or_else(|| panic!("at {w}: the agent's words are not on screen"));
    (row.len() - row.trim_start().len(), w as usize - cols_used(row))
}

/// `(left margin, right margin)` of the USER's box at `w`, read off the PAPER
/// background rather than off the characters.
fn user_margins(w: u16) -> (usize, usize) {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Thread(Region::Composer);
    v.blocks = vec![msg(Sender::User, "4172", 19, 25, &solid())];
    let lines = frame(&v, now(), w, 40);
    let mut seen: Option<(usize, usize)> = None;
    for line in &lines {
        let cells = painted_cells(line);
        if let Some((a, b)) = bg_run(&cells, palette::c().paper) {
            // Every row of the box must agree about its edges — a ragged box
            // is the rendering fault the right pad exists to prevent.
            if let Some(prev) = seen {
                assert_eq!(prev, (a, b), "at {w}: the box's rows disagree on its edges");
            }
            seen = Some((a, b));
        }
    }
    let (a, b) = seen.unwrap_or_else(|| panic!("at {w}: no paper background was painted"));
    (a, w as usize - 1 - b)
}

/// `(left margin, right margin)` of a TOOL CALL blob at `w` — a closed blob
/// whose text overflows, so it is cropped to exactly its own width.
fn blob_margins(w: u16) -> (usize, usize) {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Thread(Region::Composer);
    v.blocks = vec![lane(
        '⚙',
        Severity::Calm,
        "bash · a tool call whose one-line summary is far longer than any of these \
         terminals is wide and therefore fills the blob to its own right edge",
        None,
    )];
    let rows = text(&v, w, 40);
    let row = rows
        .iter()
        .find(|l| l.contains("bash ·"))
        .unwrap_or_else(|| panic!("at {w}: the blob is not on screen"));
    (row.len() - row.trim_start().len(), w as usize - cols_used(row))
}

/// One width's three measurements: `(width, agent, user, blob)`, each pair
/// `(left, right)`. A named type because the tuple is otherwise complex enough
/// that clippy refuses it, and because the panic messages below want all three
/// blocks in one string.
type MarginRow = (u16, (usize, usize), (usize, usize), (usize, usize));

/// Every measurement, as one line — so a failure reports the whole PICTURE and
/// not just the first number that was wrong. **`eprintln!` is a disallowed
/// macro in this crate** (printing to stderr is `0018`'s garbled-screen defect),
/// so a measurement reaches a reader through an assertion message or not at all.
fn margin_summary(rows: &[MarginRow]) -> String {
    rows.iter()
        .map(|(w, a, u, b)| format!("at {w}: agent {a:?}  user {u:?}  blob {b:?}"))
        .collect::<Vec<_>>()
        .join("  |  ")
}

fn measure(w: u16) -> MarginRow {
    (w, agent_margins(w), user_margins(w), blob_margins(w))
}

/// **THE THREE MARGINS, AT HIS WIDTH, EXACTLY.**
///
/// Not "about 10". His word was *"like"*; the task's reading of it is that the
/// numbers are the specification and only a forced rounding may move them, and
/// at 97 columns none is forced: `97 - 2 - 10 = 85` for a message block and
/// `97 - 8 - 8 = 81` for a blob, both whole.
#[test]
fn the_three_margins_are_his_numbers_at_his_width() {
    let row = measure(HIS);
    let (_, agent, user, blob) = row;
    let m = margin_summary(&[row]);
    assert_eq!(agent.1, 10, "the AGENT margin must be 10 on the right — {m}");
    assert_eq!(user.0, 10, "the USER margin must be 10 on the left — {m}");
    assert_eq!(
        blob,
        (8, 8),
        "a TOOL CALL must be 8 on the left and 8 on the right — {m}"
    );
}

/// **THE NARROW COLLAPSE — decoration first, metadata second, content never.**
///
/// At 40 columns a 10-column margin each side of a message would leave 20
/// columns for his words, so the margin halves at or below
/// [`geom::NARROW_WIDTH`]: 5 for a message block, 4 each side of a blob. What
/// is asserted here is the PROPERTY the numbers exist to serve — the content
/// keeps the majority of the screen at every width the face draws at all.
#[test]
fn the_margins_collapse_at_a_narrow_width_and_the_content_keeps_the_screen() {
    // MEASURED FIRST, ASSERTED SECOND, AND EVERY FAILURE CARRIES ALL THREE
    // WIDTHS — a run that dies on the first width reports one number where the
    // question was about three.
    let measured: Vec<MarginRow> = [80u16, 60, 40].into_iter().map(measure).collect();
    let m = margin_summary(&measured);
    for (w, agent, user, blob) in measured {
        assert_eq!(agent.1, 5, "at {w}: the agent's collapsed right margin — {m}");
        assert_eq!(user.0, 5, "at {w}: the user box's collapsed left margin — {m}");
        assert_eq!(blob, (4, 4), "at {w}: a blob's collapsed margins — {m}");
        // CONTENT NEVER COLLAPSES: whatever the margins are, the words keep
        // more than half the screen.
        let content = w as usize - agent.0 - agent.1;
        assert!(
            content * 2 > w as usize,
            "at {w}: the margins took the screen — {content} columns of content — {m}"
        );
    }
}

// ── `see all`: ONE BAND'S OWN PAGE, AND THE BREADCRUMB TRAIL ────────────────

/// One band's own page, with `n` synthetic rows in it.
///
/// It is `projects_vs()` with `focus` set and the mode changed, and nothing
/// else — which is the design's whole claim: the page IS the root's projection
/// with one band in focus, so a fixture that had to build a second row model
/// would have been the first sign the claim was false.
fn seeall_vs(b: tui_render::BandId, n: usize) -> ViewState {
    let mut v = projects_vs();
    v.mode = Mode::SeeAll(b);
    v.projects.focus = Some(b);
    v.projects.cursor = 0;
    v.projects.from_bottom = usize::MAX;
    let rows: Vec<(String, String, String)> = (0..n)
        .map(|i| {
            (
                format!("synthetic thread {i:04} — a summary of a believable length"),
                if i % 2 == 0 { "harness" } else { "miscellaneous" }.to_string(),
                if i % 3 == 0 { "active" } else { "idle" }.to_string(),
            )
        })
        .collect();
    let spec: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
        .collect();
    let built = band_rows(&spec);
    match b {
        tui_render::BandId::Pinned => {
            v.projects.pinned = built;
            v.projects.totals[b.slot()] = n;
        }
        tui_render::BandId::Recent => {
            v.projects.recent = built;
            v.projects.totals[b.slot()] = n;
        }
        tui_render::BandId::Projects => {
            v.projects.rows = project_rows();
            v.projects.totals[b.slot()] = v.projects.rows.len();
        }
    }
    v.header.title = match b {
        tui_render::BandId::Projects => format!("projects · {}", v.projects.rows.len()),
        _ => format!("{} · {n} threads", b.label()),
    };
    v
}

/// **THE PAGE IS THE BAND, THE HEADER NAMES IT, AND THE FOOTER SAYS `menu`.**
///
/// The positive witness `0065` asks for, keyed on three things no mutation of
/// the page's chrome could move: the HEADER LINE naming the band and its depth,
/// a real band ROW with its project column, and the footer's own `← menu` hint
/// rendered FROM the key table. "The root's rows are absent" would have been
/// scored by a face that drew nothing at all.
///
/// And the absences, which are the design: no band-header CAPTION (the header
/// line names it — a caption over the only band on screen is a second name for
/// what he is looking at), no `see all` foot (he is already there), no `+ new
/// project`, no `archived (N)`, no spacer.
#[test]
fn the_see_all_page_is_the_band_and_nothing_the_root_wears() {
    let v = seeall_vs(tui_render::BandId::Recent, 40);
    assert_shape(&v, W, H);
    let lines = text(&v, W, H);

    // WITNESS 1 — the header names the band and how deep it is.
    assert!(
        lines[0].starts_with("recent · 40 threads"),
        "the page's header does not name the band and its depth: {:?}",
        lines[0]
    );
    // WITNESS 2 — a real band row, with the project column a root band row has.
    assert!(
        lines[1].contains("synthetic thread 0000") && lines[1].contains("harness"),
        "the page did not draw the band's first row: {:?}",
        lines[1]
    );
    assert!(lines[1].starts_with('\u{25b8}'), "the cursor is not on row 0: {:?}", lines[1]);
    // WITNESS 3 — the way out, in the footer, from the table.
    let footer = lines.last().expect("a footer");
    assert!(
        footer.contains("\u{2190} menu"),
        "the page does not offer the way back to the main menu: {footer:?}"
    );

    // THE ABSENCES. Over the BODY only: row 0 is the frame header and it
    // legitimately says `recent · 40 threads`.
    let body = lines[1..].join("\n");
    // Skipping row 0, which is the FRAME header and also begins `recent · ` —
    // the exact collision `band_header_is`' own doc comment warns about.
    assert!(
        !lines.iter().skip(1).any(|l| band_header_is(l, "recent")),
        "the page drew a caption over its only band: {lines:#?}"
    );
    for gone in [
        "see all \u{2014}",
        "+ new project",
        "archived (",
        "pinned \u{b7}",
        "projects \u{b7}",
    ] {
        assert!(
            !body.contains(gone),
            "the page wore the root's {gone:?}: {lines:#?}"
        );
    }
    // Every content row is a band row: the body is full of them, top to bottom.
    for (i, l) in lines.iter().enumerate().take(H as usize - 1).skip(1) {
        assert!(
            l.contains("synthetic thread"),
            "row {i} of the page is not a band row: {l:?}"
        );
    }
}

// ── THE GREY, AND THE TWO INLINE-CODE CHIPS ─────────────────────────────────
//
// **PROVEN BY THE RENDERED STYLE, NEVER BY THE CONSTANT.** A test that read
// `palette::PAPER` back and compared it to `#d0d0d0` would pass on a renderer
// that had stopped painting the box at all. Every assertion below reads the
// style off a frame the crate drew, and the palette values are then held to
// PROPERTIES — neutral, not white, and legible by the numbers — rather than to
// a literal that a re-baseline could quietly move.

/// WCAG relative luminance of a palette colour.
fn luminance(c: Color) -> f64 {
    let Color::Rgb(r, g, b) = c else {
        panic!("this palette is RGB and the arithmetic below depends on it: {c:?}")
    };
    let lin = |v: u8| {
        let v = v as f64 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// WCAG contrast ratio between two palette colours.
fn contrast(a: Color, b: Color) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// `r == g == b` — the literal reading of *"maybe a grayscale would be
/// better"*, against a palette that is otherwise warm.
fn is_grayscale(c: Color) -> bool {
    matches!(c, Color::Rgb(r, g, b) if r == g && g == b)
}

/// **THE USER BOX IS A GREY RECTANGLE, AND THE INK ON IT IS THE PALETTE'S.**
///
/// > *The white background for the user input is too bright. Maybe a grayscale
/// > would be better.*
///
/// Read off the frame: every column of every box row carries one background,
/// that background is neutral and is NOT white, his words are drawn in
/// `ON_PAPER` and the metadata row in `ON_PAPER_MUTED`.
#[test]
fn the_user_box_is_painted_in_a_grey_that_is_not_white() {
    let mut v = vs(LinkState::Connected);
    v.mode = Mode::Thread(Region::Chat);
    v.blocks = vec![msg(Sender::User, "4172", 19, 25, "what I said")];
    let lines = frame(&v, now(), HIS, 40);

    let mut box_rows = 0usize;
    let mut saw_ink = false;
    let mut saw_muted = false;
    for line in &lines {
        let cells = painted_cells(line);
        let Some((a, b)) = bg_run(&cells, palette::c().paper) else {
            continue;
        };
        box_rows += 1;
        // A RECTANGLE: no column between the two edges may carry any other
        // background, or the box is a ragged stripe of highlight.
        for (i, (_, st)) in cells.iter().enumerate().take(b + 1).skip(a) {
            assert_eq!(
                st.bg,
                Some(palette::c().paper),
                "column {i} of a box row is not the box: {:?}",
                row_string(line)
            );
        }
        for (ch, st) in &cells[a..=b] {
            if *ch == ' ' {
                continue;
            }
            match st.fg {
                Some(f) if f == palette::c().on_paper => saw_ink = true,
                Some(f) if f == palette::c().on_paper_muted => saw_muted = true,
                other => panic!("an unknown ink inside the box: {other:?}"),
            }
        }
    }
    assert!(box_rows >= 2, "the box did not draw its id row and its words");
    assert!(saw_ink, "his words are not drawn in ON_PAPER");
    assert!(saw_muted, "the id row is not drawn in ON_PAPER_MUTED");

    // **AND THE COLOUR IS DEFENSIBLE BY THE NUMBERS**, which is what "maybe"
    // asked for. Each of these can be forced: white fails the second, a warm
    // grey fails the first, anything darker than `#cfcfcf` fails the fourth.
    assert!(is_grayscale(palette::c().paper), "the box is not a grayscale");
    assert_ne!(
        palette::c().paper,
        Color::Rgb(0xff, 0xff, 0xff),
        "the box is still white — that is the defect"
    );
    let ink = contrast(palette::c().on_paper, palette::c().paper);
    let meta = contrast(palette::c().on_paper_muted, palette::c().paper);
    assert!(ink >= 7.0, "the ink lost AAA on the new grey: {ink:.2}:1");
    assert!(meta >= 4.5, "the metadata lost AA on the new grey: {meta:.2}:1");
    assert!(
        ink > meta * 1.5,
        "the metadata is no longer clearly subordinate to the words: {ink:.2}:1 vs {meta:.2}:1"
    );
    // ON_PAPER_MUTED exists because MUTED is unreadable on a light box. If
    // that ever stops being true the colour has lost its reason to exist, and
    // this is where we would find out.
    let plain_muted = contrast(palette::c().muted, palette::c().paper);
    assert!(
        plain_muted < 4.5,
        "MUTED is readable on the new grey, so ON_PAPER_MUTED is now redundant: {plain_muted:.2}:1"
    );
    // The box must still read as PAPER against the chrome it sits on.
    let edge = contrast(palette::c().paper, palette::c().surface);
    assert!(edge >= 7.0, "the box no longer reads as paper: {edge:.2}:1");
}

/// **INLINE `code` IS A CHIP, IN BOTH LANES, AND PROSE IS NOT.**
///
/// > *`code` inline could be rendered as markdown. Just a different style.
/// > claude-code uses text color but I think a background color change would be
/// > nice too.*
///
/// Both halves of "both": the span's FOREGROUND differs from the prose's and
/// its BACKGROUND differs from the lane's. Forced the other way in the same
/// test — the prose either side of the span must keep the lane's own pair, or
/// the chip has leaked onto the whole row.
#[test]
fn inline_code_is_a_chip_in_both_lanes_and_prose_is_not() {
    for (sender, prose_bg, prose_fg, chip_bg, chip_fg) in [
        (
            Sender::Assistant,
            palette::c().surface,
            palette::c().ink,
            palette::c().code_bg,
            palette::c().code,
        ),
        (
            Sender::User,
            palette::c().paper,
            palette::c().on_paper,
            palette::c().code_on_paper_bg,
            palette::c().code_on_paper,
        ),
    ] {
        let mut v = vs(LinkState::Connected);
        v.mode = Mode::Thread(Region::Chat);
        v.blocks = vec![msg(sender, "m1", 19, 25, "run `cargo test` now")];
        let lines = frame(&v, now(), HIS, 40);
        let row = lines
            .iter()
            .map(painted_cells)
            .find(|c| c.iter().map(|(ch, _)| ch).collect::<String>().contains("cargo test"))
            .unwrap_or_else(|| panic!("{sender:?}: the words are not on screen"));
        let text: String = row.iter().map(|(ch, _)| ch).collect();
        // THE BACKTICKS ARE GONE from the screen.
        assert!(!text.contains('`'), "{sender:?}: a backtick was drawn: {text:?}");
        let at = text.find("cargo test").expect("the span");

        // THE CHIP: every column of the span, both halves of the pair.
        for (i, (_, st)) in row.iter().enumerate().skip(at).take("cargo test".len()) {
            assert_eq!(st.bg, Some(chip_bg), "{sender:?}: column {i} lost the chip");
            assert_eq!(st.fg, Some(chip_fg), "{sender:?}: column {i} lost the ink");
        }
        // AND THE PROSE EITHER SIDE OF IT IS UNTOUCHED — the forced direction.
        for (label, at) in [
            ("left", text.find("run").expect("the prose before")),
            ("right", text.find("now").expect("the prose after")),
        ] {
            for (_, st) in row.iter().skip(at).take(3) {
                assert_eq!(st.bg, Some(prose_bg), "{sender:?}: the chip leaked {label}");
                assert_eq!(st.fg, Some(prose_fg), "{sender:?}: the chip leaked {label}");
            }
        }

        // BOTH, not either: he asked for a text colour AND a background.
        assert_ne!(chip_fg, prose_fg, "{sender:?}: the code ink is the prose ink");
        assert_ne!(chip_bg, prose_bg, "{sender:?}: the code chip is the lane");
        // …and the chip has to be SEEN against the lane it sits in, which is
        // the coupling between this defect and the grey.
        let edge = contrast(chip_bg, prose_bg);
        assert!(
            edge > 1.25,
            "{sender:?}: the chip is invisible against its lane: {edge:.2}:1"
        );
        // The ink on the chip stays readable: AA for body text.
        let legible = contrast(chip_fg, chip_bg);
        assert!(
            legible >= 4.5,
            "{sender:?}: the code ink is not legible on its chip: {legible:.2}:1"
        );
    }
}

/// A row with no backticks in it draws no chip anywhere — the companion
/// direction, at the FRAME level rather than inside `md`.
#[test]
fn a_message_with_no_code_span_draws_no_chip() {
    for sender in [Sender::Assistant, Sender::User] {
        let mut v = vs(LinkState::Connected);
        v.mode = Mode::Thread(Region::Chat);
        v.blocks = vec![msg(sender, "m1", 19, 25, "run cargo test now")];
        for line in frame(&v, now(), HIS, 40) {
            for (_, st) in painted_cells(&line) {
                for bg in [palette::c().code_bg, palette::c().code_on_paper_bg] {
                    assert_ne!(st.bg, Some(bg), "{sender:?}: a chip on a line with no code");
                }
            }
        }
    }
}

/// **THE CEILINGS ARE CEILINGS NOW, AND THIS SAYS WHERE THEY BIND.**
///
/// The defect was not that `BOX_MAX` and `BLOB_MAX` existed; it was that they
/// DECIDED the margin. `BOX_MAX` was 76 and bound at any width over 82, so at
/// his 97-column pane the agent's right margin was 21 and a blob's were 16 and
/// 17 — numbers nobody chose. Both are now derived from
/// [`geom::WIDE_WIDTH`] and the contract is explicit in two halves:
///
/// 1. **From just above the narrow collapse up to `WIDE_WIDTH`, the margin is
///    EXACT** — his number, at every single width in between, not just at 97.
/// 2. **Above `WIDE_WIDTH` the ceiling binds and the margin grows**, which is
///    what stops a 200-column terminal handing him a 188-column line.
///
/// His width is inside band 1, and the last assertion is the one that makes the
/// whole change true rather than true-at-one-number.
#[test]
fn the_ceilings_do_not_bind_below_the_wide_width_and_do_bind_above_it() {
    use tui_render::geom::{
        blob_left, blob_width, box_width, BLOB_MARGIN, MSG_GUTTER, MSG_MARGIN, NARROW_WIDTH,
        WIDE_WIDTH,
    };
    for w in (NARROW_WIDTH as usize + 1)..=WIDE_WIDTH {
        assert_eq!(
            w - MSG_GUTTER - box_width(w),
            MSG_MARGIN,
            "the box ceiling bound at {w} columns, so the margin is not his number"
        );
        assert_eq!(
            w - blob_width(w),
            2 * BLOB_MARGIN,
            "the blob ceiling bound at {w} columns"
        );
        assert_eq!(blob_left(w), BLOB_MARGIN, "a blob is off-centre at {w}");
    }
    // One column past it, both ceilings bite — and a ceiling that never bit
    // would be a constant pretending to be a guard.
    let w = WIDE_WIDTH + 1;
    assert!(
        w - MSG_GUTTER - box_width(w) > MSG_MARGIN,
        "the box ceiling never binds, at any width"
    );
    assert!(
        w - blob_width(w) > 2 * BLOB_MARGIN,
        "the blob ceiling never binds, at any width"
    );
    // **AND HIS PANE IS INSIDE THE EXACT BAND.** This is the assertion the
    // whole defect turns on: 97 must be a width at which the margin, not the
    // ceiling, decides.
    assert!(
        HIS as usize > NARROW_WIDTH as usize && HIS as usize <= WIDE_WIDTH,
        "his 97-column pane fell out of the band where the margins are exact"
    );
}

/// **AN EMPTY PAGE IS A SENTENCE, NEVER A BLANK SCREEN** (`0040` §10).
///
/// The ROOT draws nothing at all for an empty recent band — no header over
/// nothing — and that is right there, with two other bands on screen. It is
/// wrong on a screen that is ONLY that band: a page he asked to be taken to and
/// which shows him nothing is indistinguishable from a face that died.
///
/// Reachable only by a state change under him, which is exactly why it is drawn.
#[test]
fn an_empty_page_says_what_would_fill_the_band() {
    for (b, want) in [
        (tui_render::BandId::Pinned, tui_render::PINNED_EMPTY),
        (tui_render::BandId::Recent, tui_render::RECENT_EMPTY),
    ] {
        let mut v = seeall_vs(b, 0);
        v.projects.pinned = Vec::new();
        v.projects.recent = Vec::new();
        v.projects.totals[b.slot()] = 0;
        assert_shape(&v, W, H);
        let lines = text(&v, W, H);
        assert!(
            lines[1].contains(want),
            "{b:?}'s empty page drew a blank screen: {lines:#?}"
        );
        // …and the way out is still on it.
        assert!(lines.last().expect("a footer").contains("\u{2190} menu"));
    }
}

/// **THE PAGE CROPS BEFORE IT PAINTS, AND BOTH COORDINATE SYSTEMS STAY
/// ABSOLUTE.** `I-12`'s structural half.
///
/// The performance half is `examples/expanded_band_cost.rs`, which is the only
/// thing that can measure wall time. This is the half a test can hold: the
/// crop is only free if the rows it skips are never painted, and it is only
/// CORRECT if the two indexes the skipped rows would have advanced — the drag
/// selection's `Unit` address into `select::content_text`, and the cursor's
/// index over SELECTABLE lines — are still counted from the top of the whole
/// surface. Re-basing either to the window is `0023`'s CLASS 3 in the place
/// where a click lands on a different thread from the one under the pointer.
///
/// Asserted at a scroll position deep inside a 5,000-row band, which is where
/// a window-relative index and an absolute one differ by 4,000.
#[test]
fn the_page_crops_to_the_window_and_keeps_its_indexes_absolute() {
    let mut v = seeall_vs(tui_render::BandId::Recent, 5_000);
    // Park the window in the middle: `from_bottom` is the one scroll coordinate.
    v.projects.from_bottom = 2_000;
    v.projects.cursor = 2_900;
    assert_shape(&v, W, H);
    let (_, hits) = frame_with_hits(&v, now(), W, H);

    // The band is 5,000 rows and 22 of them are on screen; the hit map is the
    // FRAME's, so it can only ever be as tall as the frame.
    let rows: Vec<Target> = (0..H).map(|r| hits.hit(GUTTER as u16 + 1, r)).collect();
    let claimed: Vec<usize> = rows
        .iter()
        .filter_map(|t| match t {
            Target::RootRow(i) => Some(*i),
            _ => None,
        })
        .collect();
    assert!(
        claimed.len() >= 20,
        "the page drew fewer rows than the body has: {claimed:?}"
    );
    // **ABSOLUTE, NOT WINDOW-RELATIVE.** A window-relative `sel` would start
    // this list at 0; the real first row on screen is 2,978 rows down.
    let first = *claimed.first().expect("a row");
    assert!(
        first > 2_000,
        "the page re-based its cursor index to the window (first row claims {first})"
    );
    // Contiguous and ascending — one row per screen row, in order.
    for w in claimed.windows(2) {
        assert_eq!(w[1], w[0] + 1, "the claimed rows are not contiguous: {claimed:?}");
    }
    // And the DRAG unit's address resolves, through `content_text`, to the text
    // that was actually drawn on that row.
    let (lines, _) = frame_with_hits(&v, now(), W, H);
    let drawn = &lines[1];
    let text_of = tui_render::select::content_text(&v, first)
        .expect("the page's first drawn row has no model text");
    let painted: String = drawn.spans.iter().map(|s| s.content.to_string()).collect();
    assert!(
        painted.contains(text_of.split("  ").next().expect("a summary")),
        "the unit address does not name the row that was painted: {painted:?} vs {text_of:?}"
    );
}

/// **A CLICK ON ANY ROW OF THE PAGE MOVES THE CURSOR AND DOES NOTHING ELSE.**
/// `I-14`, and it is this round's reversal:
///
/// > clicking on a thread shouldn't take me into it. Only right can do it.
/// > Clicking should only move pointers and click buttons (which we don't even
/// > have yet - nothing in this app counts as that).
///
/// "nothing in this app counts as that" is the load-bearing clause: the page
/// adds NO button, so every target on it — the row, the thread's TITLE, a
/// project's NAME — answers a click with `Intent::Select` and only `→` opens.
/// The root's band header and `see all` foot are asserted here too, because
/// they are the two rows this feature made into doors and `DEC-78` already
/// says they are `RootRow`s.
///
/// **The table is asked, not the frame** — `activation` is the one resolution,
/// so a new `Via::Click => Open` arm anywhere reddens this whichever surface
/// grew it.
#[test]
fn a_click_on_the_page_only_moves_the_cursor() {
    for b in tui_render::BandId::ALL {
        let m = Mode::SeeAll(b);
        for t in [
            Target::RootRow(7),
            Target::ThreadTitle(7),
            Target::ProjectName(7),
        ] {
            assert_eq!(
                activation(t, m, Via::Click),
                Some(Intent::Select(7)),
                "{t:?} on {m:?} does something other than move the cursor"
            );
        }
        // …and `→` is what opens, so the row is still reachable.
        assert_eq!(activation(Target::RootRow(7), m, Via::Key), Some(Intent::Open));
        assert_eq!(
            activation(Target::ThreadTitle(7), m, Via::Key),
            Some(Intent::OpenSheet(7)),
            "`o` cannot reach a thread's options on {m:?}"
        );
    }
    // **AND THE TWO NEW DOORS AT THE ROOT ARE NOT BUTTONS EITHER.** A band
    // header and a `see all` foot are `RootRow`s, so this is the same arm; it
    // is asserted because they are the rows the feature made into doors.
    let root = projects_vs();
    let lines = root.projects.lines();
    let mut sel = 0usize;
    for l in &lines {
        if !l.selectable() {
            continue;
        }
        if matches!(
            l,
            tui_render::RootLine::Band(_) | tui_render::RootLine::SeeAll(_)
        ) {
            assert_eq!(
                activation(Target::RootRow(sel), Mode::Projects, Via::Click),
                Some(Intent::Select(sel)),
                "{l:?} activates on a click"
            );
            assert_eq!(
                activation(Target::RootRow(sel), Mode::Projects, Via::Key),
                Some(Intent::Open),
                "{l:?} is not a door for `\u{2192}`"
            );
        }
        sel += 1;
    }
    assert!(sel > 0, "the root fixture has no stops at all");
}

/// **THE TRAIL IS DRAWN IN FRONT OF THE TITLE, MUTED, AND IT ELIDES FROM THE
/// LEFT BEFORE IT GOES.**
///
/// *"The threads should be aware of their breadcrumbs."* The words are the
/// shell's (`App::trail`); this is the drawing, and three things about it are
/// the design:
///
///   * the crumbs come FIRST, because `←` walks outward and the eye reads left
///     to right into the thing he is looking at;
///   * they are MUTED and the leaf is not, so the screen he is on is the one
///     his eye lands on;
///   * under width pressure the OUTERMOST crumb goes first (`… ›`), because the
///     nearest door is the one `←` is about to use.
///
/// The LEAF is never elided by the trail: the title keeps its floor, which is
/// what `0040` §11's "it ellipsizes but survives" means.
#[test]
fn the_header_writes_the_trail_in_front_of_the_title_and_elides_from_the_left() {
    let mut v = vs(LinkState::Connected);
    // TUI-P1a (TC-04): no trail on a THREAD page's header any more — it is
    // the name and the status only. The trail machinery is the LIST's.
    v.mode = Mode::List;
    v.header.title = "Answer the tax letter · idle".into();
    v.header.trail = vec!["recent".into()];
    let head = &text(&v, W, H)[0];
    assert!(
        head.starts_with("recent \u{203a} Answer the tax letter"),
        "the trail is not in front of the title: {head:?}"
    );
    // MUTED crumbs, a bright leaf — read off the painted styles, not asserted
    // about the model.
    let line = &frame(&v, now(), W, H)[0];
    let crumb = line
        .spans
        .iter()
        .find(|s| s.content.contains("recent"))
        .expect("a crumb span");
    let leaf = line
        .spans
        .iter()
        .find(|s| s.content.contains("Answer the tax letter"))
        .expect("a leaf span");
    assert_eq!(crumb.style, palette::muted(), "the crumb is not muted");
    assert_eq!(leaf.style, palette::header(), "the leaf is not the header style");

    // TWO CRUMBS, and at 60 columns the outermost one elides rather than the
    // pair vanishing.
    v.header.trail = vec!["recent".into(), "Answer the tax letter".into()];
    v.header.title = "activity · 3 lanes".into();
    let wide = &text(&v, W, H)[0];
    assert!(
        wide.starts_with("recent \u{203a} Answer the tax letter \u{203a} activity"),
        "two crumbs did not both fit at 100: {wide:?}"
    );
    let tight = &text(&v, 60, H)[0];
    assert!(
        tight.contains("activity"),
        "the leaf was lost to the trail at 60: {tight:?}"
    );
    assert!(
        tight.contains("\u{2026} \u{203a}") || !tight.contains('\u{203a}'),
        "the trail neither elided from the left nor dropped whole at 60: {tight:?}"
    );

    // AND NO TRAIL IS NO PART AT ALL — the floor is not written, so a thread
    // opened straight from a band has a header that is exactly its title.
    v.header.trail = Vec::new();
    v.header.title = "Answer the tax letter · idle".into();
    let bare = &text(&v, W, H)[0];
    assert!(
        bare.starts_with("Answer the tax letter"),
        "an empty trail still spent columns: {bare:?}"
    );
    assert!(!bare.contains('\u{203a}'), "an empty trail drew a separator: {bare:?}");
}

/// **THE DOWN PAGE REPLACES THE BODY, AND ONLY WHEN IT IS ASKED FOR** (R7.S1c).
///
/// The same fixture drawn twice: with `down` set, every row between the header
/// and the footer is the page — service, environment, address and its source,
/// and the restart button — and the frame is still exactly `h` rows; with
/// `down` unset, none of those words appear. A renderer that always (or never)
/// drew the page fails one half.
///
/// FORCED RED by drawing `body` in the `Some(d)` arm.
#[test]
fn the_down_page_replaces_the_body_and_only_when_asked() {
    let mut v = vs(LinkState::Disconnected { reason: "refused".into(), retry_at: None });
    let plain = text(&v, 100, 24);
    assert!(!plain.iter().any(|l| l.contains("threads is down")), "{plain:#?}");
    v.down = Some(tui_render::DownPage {
        service: "threads".into(),
        env: "t1".into(),
        addr: "127.0.0.1:47653".into(),
        source: "env.toml /r/data/t1/env.toml".into(),
        comes_back: "auto_restart is on in `t1`: its holder relaunches threads.".into(),
        restart_cmd: "cauldron --env t1 start threads".into(),
        restart_key: 'r',
        restart_note: Some("exit 0: cauldron: started".into()),
    });
    let page = text(&v, 100, 24);
    assert_eq!(page.len(), 24, "the page must fill the frame exactly");
    let all = page.join("\n");
    for want in [
        "threads is down",
        "environment  t1",
        "127.0.0.1:47653  (env.toml /r/data/t1/env.toml)",
        "[r] restart now   cauldron --env t1 start threads",
        "exit 0: cauldron: started",
        "auto_restart is on in `t1`: its holder relaunches threads.",
    ] {
        assert!(all.contains(want), "missing {want:?} in:\n{all}");
    }
    // The header and footer stay: the first and last rows are the same rows
    // the plain frame drew.
    assert_eq!(page[0], plain[0], "the header must stay over the page");
    assert_eq!(page[23], plain[23], "the footer must stay under the page");
}

// ── the light palette (8e4fca10 msg 235, task 0205) ─────────────────────────

fn role(c: &palette::Colors, name: &str) -> Color {
    c.roles().iter().find(|(n, _)| *n == name).unwrap_or_else(|| panic!("no role `{name}`")).1
}

/// Every role is listed by `roles()`: its length is the struct's size.
#[test]
fn light_palette_roles_lists_every_colour_role() {
    assert_eq!(palette::LIGHT.roles().len() * std::mem::size_of::<Color>(), std::mem::size_of::<palette::Colors>());
}

/// **Every ink clears WCAG AA, 4.5:1, on every background it can land on** —
/// a stricter bar than the dark palette was ever held to (its red on the
/// selected button is 1.74:1), because a light theme rebuilt by hand is where
/// a pale ink on a pale chip hides.
#[test]
fn light_palette_every_ink_clears_aa_on_every_background() {
    let l = &palette::LIGHT;
    let mut fails = Vec::new();
    for f in palette::TEXT_ROLES {
        for b in palette::BG_ROLES {
            let r = contrast(role(l, f), role(l, b));
            if r < 4.5 {
                fails.push(format!("{f} on {b}: {r:.2}"));
            }
        }
    }
    for (f, b) in [("on_paper", "paper"), ("on_paper_muted", "paper"), ("code_on_paper", "code_on_paper_bg"), ("code_on_paper", "paper")] {
        let r = contrast(role(l, f), role(l, b));
        if r < 4.5 {
            fails.push(format!("{f} on {b}: {r:.2}"));
        }
    }
    assert!(fails.is_empty(), "{fails:?}");
}

/// Every background stands apart from the surface (no less than the dark
/// palette's faintest, the dim selection's 1.10:1 rounded down), the paper box
/// is still a grayscale, and no two roles share a value — the face tells a
/// selection, a row and a modal apart by their background alone.
#[test]
fn light_palette_backgrounds_are_visible_and_every_role_is_its_own() {
    let l = &palette::LIGHT;
    for b in palette::BG_ROLES.iter().chain(["paper", "modal_inner"].iter()).filter(|b| **b != "surface") {
        let r = contrast(role(l, b), l.surface);
        assert!(r >= 1.10, "{b} against the surface: {r:.2}");
    }
    assert!(is_grayscale(l.paper));
    let roles = l.roles();
    for (i, (a, x)) in roles.iter().enumerate() {
        for (b, y) in &roles[i + 1..] {
            assert!(x != y, "{a} and {b} share {x:?}");
        }
    }
    // Light is light: the surface is pale and its ink dark.
    assert!(luminance(l.surface) > 0.85 && luminance(l.ink) < 0.02);
}

/// The theme is per thread and defaults to dark; setting it switches every
/// style this crate hands out.
#[test]
fn the_theme_switches_every_style() {
    assert_eq!(palette::theme(), palette::Theme::Dark);
    assert_eq!(palette::base().bg, Some(palette::DARK.surface));
    palette::set_theme(palette::Theme::Light);
    assert_eq!(palette::base().bg, Some(palette::LIGHT.surface));
    assert_eq!(palette::muted().fg, Some(palette::LIGHT.muted));
    assert_eq!(palette::paper().bg, Some(palette::LIGHT.paper));
    assert_eq!(palette::code().bg, Some(palette::LIGHT.code_bg));
    assert_eq!(palette::Severity::Critical.style().fg, Some(palette::LIGHT.red));
    let other = std::thread::spawn(palette::theme).join().unwrap();
    assert_eq!(other, palette::Theme::Dark, "another thread draws in its own theme");
    palette::set_theme(palette::Theme::Dark);
}
