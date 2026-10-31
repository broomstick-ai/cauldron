//! `(ViewState, now, width, height) -> Vec<Line>`, and nothing else.
//!
//! **The postcondition, machine-checked on every golden case:** EXACTLY
//! `height` lines of EXACTLY `width` columns. `0018` proved why it is not a
//! style preference — ratatui 0.28 PANICS on an over-reported line rather than
//! wrapping it — so every line in this file leaves through
//! [`geom::size_line`], which cannot return the wrong width.
//!
//! **The one call site rule** (`0023` `defect_classes` CLASS 3): this crate
//! never reads a `Rect`. It is HANDED `width` and `height` as parameters, and
//! the shell has exactly one place that reads them off the frame area. There
//! is no second source for either number to disagree with. The corollary the
//! task states and this file obeys: **nothing here crops or pads using a width
//! captured from a different render pass**, which is structurally impossible
//! because `w` is a parameter and there is no other way to reach one.
//!
//! **The four views, and the root is PROJECTS.** [`Mode::Projects`] is every
//! root the human faces own, [`Mode::List`] is one project's threads,
//! [`Mode::Thread`] is one thread's chat, [`Mode::Activity`] is his threads'
//! event lane. Each has one scroll state and fills the body; none is a pane
//! beside another, because a second pane is the master-detail split `0003`
//! rejected and `DEC-11` rejected again.
//!
//! **The face also returns a [`HitMap`]**, from the same pass — see
//! [`frame_with_hits`]. `frame` is its first half. A click is resolved against
//! that map and never against a geometry the shell re-derives, because a hit
//! map built by a second pass is `0023` `defect_classes` CLASS 3 in a form
//! nobody can see: a wrong one does not look wrong, it opens the wrong thing.

use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset, Utc};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use crate::geom::{
    self, box_width, crop_with_ellipsis, size_line, text_width, Seg, MIN_HEIGHT, MIN_WIDTH,
};
use crate::hit::{HitMap, RowInfo, Target};
use crate::keys::{self, Mode, Region};
use crate::md;
use crate::model::{
    AgentTree, ArchivedView, Banner, Block, Composer, Facts, Fetch, Known, Link, LinkState,
    MicState, ProjectFacts, ProjectList, ProjectRow, QueueItem, QueueView, RootLine, Sender,
    SheetFacts, SubagentClass, SubagentState, ThreadList, ThreadRow, TreeId, TreeRow, ViewState, COMPOSER_MAX_ROWS,
    COMPOSER_MIN_ROWS, FOOTER_ROWS, HEADER_ROWS,
};
use crate::palette::{self, Severity};
use crate::select::{self, RowText, Unit, UnitKind};

/// The left gutter of every transcript row: two columns, always reserved.
/// The left margin every band shares. `pub` since `0052`: the shell has to
/// subtract it from a click column to get a caret column, and a `2` written
/// out in `app.rs` would be a second copy of a layout constant.
pub const GUTTER: usize = 2;

/// **THE GUTTER IS NAMED TWICE AND MAY NEVER BE TWO NUMBERS.**
/// [`geom::MSG_GUTTER`] is the copy the margin arithmetic needs (a lower module
/// cannot reach this one); this fails the BUILD rather than a test if the two
/// ever drift apart, which is the only way a restated constant is safe.
const _: () = assert!(GUTTER == geom::MSG_GUTTER);

/// **HOW FAR A BAND'S CONTAINED ROWS SIT INSIDE ITS HEADER.**
///
/// The operator's ask, and his reason, in one sentence:
///
/// > since the band headers have become selectable and have whitespace on the
/// > left, we need to increase the whitespace on their contained items
///
/// A header used to sit flush at column 0 while its rows wore the two-column
/// cursor gutter, so the indent was free. Making a header a cursor stop
/// (`DEC-46`) gave it the same gutter, and the hierarchy flattened: three
/// captions and thirty rows all starting at column 2.
///
/// **THE CURSOR MARKER STAYS AT COLUMN 0 ON EVERY ROW.** The indent is added
/// AFTER it, never before — a marker that moves column as the cursor walks from
/// a header into its rows is a marker the eye has to re-find on every keypress,
/// and the whole point of a marker is that it does not have to be found. So a
/// header reads `▸ pinned` and its rows read `▸   the thread…`: one marker
/// column, two indent levels.
///
/// **TWO, not four.** Measured at 60 columns, where the band rows already
/// ellipsize: two columns is a visible step and costs the summary two
/// characters; four was legible as a step and cost four, which at 60 is a word.
const ROW_INDENT: usize = 2;

/// The marker-plus-indent prefix a CONTAINED row wears.
fn row_head(selected: bool) -> String {
    let marker = if selected { "▸ " } else { "  " };
    format!("{marker}{}", " ".repeat(ROW_INDENT))
}

/// The column a contained row's own text begins at — `GUTTER + ROW_INDENT`.
/// Read by the hit map's `col0`, so a drag selects from where the text is.
const ROW_COL0: u16 = (GUTTER + ROW_INDENT) as u16;

/// **How one frame's rows divide, and the ONE place that decision is made.**
///
/// It exists because the shell needs ONE of these numbers — [`Bands::content`],
/// the height of a page — and it was deriving it independently:
/// `height - 2 - COMPOSER_MIN_ROWS`, in `app.rs`, from the same constants this
/// file reads. Two derivations of one fact is `0023` `defect_classes` CLASS 3
/// wearing a different coat, and the copy had ALREADY drifted: it did not
/// subtract the banner, so with the link down `PageDown` moved by one row more
/// than the screen holds, and it used `COMPOSER_MIN_ROWS` where this file uses
/// [`composer_height`], which is only the same number while the composer is
/// not editable.
///
/// So [`frame`] and the shell now read the same function, and there is no
/// second answer for either of them to hold. Note what that makes impossible:
/// this is a PURE function of the same three inputs `frame` takes, so a shell
/// that asks it is asking about the frame it is actually looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Bands {
    pub header: usize,
    /// The standing row above the content, when there is one to stand.
    pub banner: usize,
    /// **The scrollable band** — the list, the transcript, or an overlay's
    /// whole body. This is the number a pager moves by.
    pub content: usize,
    pub composer: usize,
    /// **THE SUBAGENT BAND**, between the composer and the footer. `0` when
    /// the thread has no subagents, and `0` when the frame is too short to
    /// hold one without taking the composer under its minimum — the tree
    /// yields first, and when it is not drawn it is not a stop either.
    pub tree: usize,
    pub footer: usize,
}

/// [`Bands`] for this frame. Below the minimum every band is zero, because the
/// too-small frame draws none of them.
pub fn bands(vs: &ViewState, width: u16, height: u16) -> Bands {
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        return Bands::default();
    }
    let body = height as usize - HEADER_ROWS as usize - FOOTER_ROWS as usize;
    let mut b = Bands {
        header: HEADER_ROWS as usize,
        footer: FOOTER_ROWS as usize,
        ..Bands::default()
    };
    match vs.mode {
        // An overlay takes the WHOLE body — no banner, no composer. The shell
        // used to subtract a composer here anyway, because it keyed off the
        // view UNDER the overlay rather than the mode being drawn.
        // The SHEET is an overlay like the other three: it replaces the body,
        // it has no banner and no composer, and its typed field lives INSIDE
        // its own band rather than in the composer's — the composer belongs to
        // the thread underneath and must not be repurposed, or leaving the
        // sheet would hand him a box with somebody else's words in it.
        Mode::Help | Mode::Queue | Mode::Sheet | Mode::SheetField => {
            b.content = body
        }
        // **THE `+ new project` FIELD IS A ROW OF THE ROOT**, not a band: he
        // types into the row where the name will live. So this mode lays out
        // exactly as the root does — the surface does not move under him when
        // the field opens, which is what "typed right in the row" means.
        // **AND `Mode::SeeAll(b)` LAYS OUT EXACTLY AS THE ROOT DOES**, banner
        // included, because it IS the root's projection with one band in focus
        // (`ProjectList::focus`). `Mode::Archived` below takes the whole body
        // instead, and the difference is deliberate: the archived room is a
        // list this face reads off the filesystem itself, while a `see all`
        // page is a list of THREADS — so `DEC-13`'s disconnected posture and
        // the unreadable-registry sentence are both statements about whether
        // what is on it is complete, and both belong over it.
        Mode::Projects
        | Mode::NewProject
        | Mode::SeeAll(_)
        | Mode::List
        | Mode::Activity => {
            b.banner = banner_height(vs.banner.as_ref(), body, width as usize);
            b.content = body - b.banner;
        }
        // **THE LITTLE RENAME MODAL RESERVES A BAND; IT DOES NOT PAINT OVER
        // THE LIST.**
        //
        // *"Just a little modal"* — and the half of that he said out loud is
        // *"don't take me to the settings"*, which is about not LOSING HIS
        // PLACE. A box painted over the content band would occlude rows, and
        // both lists are bottom-anchored, so the rows it would cover are
        // exactly the ones he was looking at. Taking the rows out of the
        // content band instead means the list re-lays into what is left and
        // its own cursor clamp keeps his row on screen — which is `Mode::Mic`'s
        // mechanism (it *"reserves the same tree band, so entering mic does
        // not move the transcript under his eyes"*) and the composer's, one
        // surface over.
        //
        // It borrows the COMPOSER'S band rather than adding a field to
        // [`Bands`]: the box is a typed box at the foot of the screen, which is
        // the thing that band has always meant here.
        Mode::RenameModal => {
            b.banner = banner_height(vs.banner.as_ref(), body, width as usize);
            let rest = body - b.banner;
            b.composer = rename_modal_height(&vs.sheet, rest, width as usize);
            b.content = rest - b.composer;
        }
        // **A BODY MODE WITH NO BANNER.** The archived set is its own screen
        // (§1's frame: header, rows, `<- back`) and it has no link banner of
        // its own — the header carries the link word, as it does everywhere.
        Mode::Archived => b.content = body,
        Mode::Thread(_) => {
            b.banner = banner_height(vs.banner.as_ref(), body, width as usize);
            let rest = body - b.banner;
            // **THE TREE IS MEASURED FIRST AND YIELDS FIRST.** It takes its
            // rows out of the room the composer is then measured against, so
            // the box can never be squeezed under its minimum by a band that
            // is below it; `tree_height` refuses to draw at all rather than do
            // that.
            b.tree = tree_height(&vs.tree, rest);
            b.composer = composer_height(&vs.composer, rest - b.tree, width as usize);
            b.content = rest - b.tree - b.composer;
        }
        // **MIC MODE KEEPS THE TRANSCRIPT AND REPLACES THE COMPOSER.** The band
        // it takes is the composer's band, reused rather than added, so
        // entering mic mode does not move the transcript under his eyes.
        Mode::Mic => {
            b.banner = banner_height(vs.banner.as_ref(), body, width as usize);
            let rest = body - b.banner;
            // **MIC MODE RESERVES THE SAME TREE BAND**, so entering mic does
            // not move the transcript under his eyes. It is drawn read-only
            // and has no tree keys.
            b.tree = tree_height(&vs.tree, rest);
            b.composer = mic_height(vs, rest - b.tree);
            b.content = rest - b.tree - b.composer;
        }
    }
    b
}

/// How many rows the mic band takes: its border, a state row, and one row per
/// queued item up to a ceiling.
///
/// **THE QUEUE SHOWS EVERY ITEM** — his ruling — so the band grows with the
/// queue rather than showing the first few. It stops at
/// [`MIC_MAX_ROWS`] against the space it actually has, and when it does the
/// band says how many are not on screen rather than cropping in silence; the
/// full list is always one `^O` away in the queue view.
fn mic_height(vs: &ViewState, room: usize) -> usize {
    let items = vs.composer.buf.pending().len();
    // border + state row + items (+ an "N more" row when it cannot show them)
    let want = 2 + items + usize::from(items > 0);
    want.clamp(
        MIN_MIC_ROWS,
        MIC_MAX_ROWS.min(room.saturating_sub(1)).max(MIN_MIC_ROWS),
    )
}

/// **HOW MANY ROWS THE LITTLE RENAME MODAL TAKES** — its titled top rule, the
/// name being edited (wrapped if it is long), and its bottom rule.
///
/// It grows with the name rather than cropping it, because a rename box that
/// hides the end of what he typed is a box that makes him retype the whole
/// thing — and stops at [`RENAME_MODAL_MAX_ROWS`] against the room it actually
/// has, so it can never eat the list it is standing over.
fn rename_modal_height(
    sheet: &Option<crate::model::SheetView>,
    room: usize,
    w: usize,
) -> usize {
    let typed = sheet
        .as_ref()
        .and_then(|s| s.field.as_ref())
        .map(|f| f.buf.layout(rename_modal_inner(w)).len())
        .unwrap_or(1)
        .max(1);
    // top rule + the name + bottom rule
    (typed + 2).clamp(
        MIN_RENAME_MODAL_ROWS,
        RENAME_MODAL_MAX_ROWS
            .min(room.saturating_sub(1))
            .max(MIN_RENAME_MODAL_ROWS),
    )
}

/// The width the name is laid out against inside the modal's rules.
fn rename_modal_inner(w: usize) -> usize {
    w.saturating_sub(GUTTER + 2).max(1)
}

/// The modal is never less than its two rules and one row of name.
pub const MIN_RENAME_MODAL_ROWS: usize = 3;
/// …and never eats the list it is standing over.
pub const RENAME_MODAL_MAX_ROWS: usize = 8;

/// The mic band never gets smaller than its border plus one state row.
pub const MIN_MIC_ROWS: usize = 3;
/// …and never eats the transcript whole.
pub const MIC_MAX_ROWS: usize = 12;

/// Render one frame.
pub fn frame(vs: &ViewState, now: DateTime<Utc>, width: u16, height: u16) -> Vec<Line<'static>> {
    frame_with_hits(vs, now, width, height).0
}

/// **The frame AND what is under every coordinate of it, from one pass.**
///
/// [`frame`] is this function's first half and nothing else. The two are not
/// two functions because a hit map built by a second layout pass is a second
/// derivation of the geometry — `0023` `defect_classes` CLASS 3, which this
/// crate has already had to repair once — and it would be a derivation nobody
/// could see drift, since a wrong hit map does not look wrong. Built here, the
/// map cannot describe a layout other than the one it drew.
pub fn frame_with_hits(
    vs: &ViewState,
    now: DateTime<Utc>,
    width: u16,
    height: u16,
) -> (Vec<Line<'static>>, HitMap) {
    let mut hits = HitMap::new(width);
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        let lines = too_small(width, height);
        // Nothing on a too-small frame is clickable and nothing on it is
        // copyable: there is one sentence on it and it is not an affordance.
        for _ in 0..lines.len() {
            hits.push(Target::Empty);
        }
        return (lines, hits);
    }
    let w = width as usize;
    let h = height as usize;
    let b = bands(vs, width, height);

    let mut out: Vec<Line<'static>> = Vec::with_capacity(h);
    let (head, badge) = header_line(vs, now, w);
    hits.push(Target::Header);
    if let Some((from, to)) = badge {
        hits.span_on_last(from, to, Target::QueueBadge);
    }
    if let Some(t) = chrome_text(&head, Unit::new(UnitKind::Header, 0), 0) {
        hits.text_on_last(t);
    }
    out.push(head);

    match &vs.down {
        // **THE DOWN PAGE REPLACES THE BODY, WHATEVER THE MODE** (R7.S1c). The
        // header (which carries the link word) and the footer stay; every row
        // between them is the page. One page for an outage, not a posture per
        // mode.
        Some(d) => out.extend(down_body(d, &vs.header.link, now, w, h, &mut hits)),
        None => out.extend(body(vs, now, w, b, &mut hits)),
    }

    let foot = footer_line(vs, now, w);
    hits.push(Target::Footer);
    if let Some(t) = chrome_text(&foot, Unit::new(UnitKind::Footer, 0), GUTTER as u16) {
        hits.text_on_last(t);
    }
    out.push(foot);

    debug_assert_eq!(out.len(), h);
    debug_assert_eq!(
        hits.height(),
        h,
        "the hit map must describe every drawn row"
    );

    // **THE SELECTION IS PAINTED LAST, OVER THE FRAME THAT WAS JUST DRAWN.**
    //
    // Not by every band learning what a selection is: a highlight is a
    // RESTYLE of cells whose characters, widths and count are already fixed,
    // so the shape law survives it by construction and *every* cell the face
    // draws is selectable without a single band being edited to allow it.
    // That is the mechanism behind "ANY part of the interface" — it is true by
    // construction rather than by having remembered each band in turn.
    if let Some(sel) = vs.selection {
        for (i, line) in out.iter_mut().enumerate() {
            let Some(rt) = hits.text_row(i as u16) else {
                continue;
            };
            if let Some((from, to)) = select::row_span(rt, sel) {
                *line = geom::highlight(std::mem::take(line), from, to.min(w));
            }
        }
    }
    (out, hits)
}

/// **THE "DAEMON IS DOWN" PAGE** — every row between the header and the
/// footer while the link is down (R7.S1c).
///
/// It states four facts and one affordance: which service, in which cauldron
/// environment, at which address (and where that address came from), how long
/// it has been down — spelled by [`link_label`], the one place a link duration
/// is spelled, so the page and the header cannot disagree — and the key that
/// asks cauldron to start it. It has no refresh key and nothing to dismiss: the
/// supervisor holding the environment restarts the daemon, the seam reconnects
/// on its own ladder, and the shell drops `down` on the first frame the link is
/// up again.
fn down_body(
    d: &crate::model::DownPage,
    link: &Link,
    now: DateTime<Utc>,
    w: usize,
    h: usize,
    hits: &mut HitMap,
) -> Vec<Line<'static>> {
    let rows = h.saturating_sub((HEADER_ROWS + FOOTER_ROWS) as usize);
    let pad = " ".repeat(GUTTER);
    let crit = Severity::Critical.style();
    let mut text: Vec<(String, ratatui::style::Style)> = vec![
        (String::new(), palette::base()),
        (format!("{pad}{} is down", d.service), crit),
        (String::new(), palette::base()),
        (format!("{pad}environment  {}", d.env), palette::base()),
        (format!("{pad}address      {}  ({})", d.addr, d.source), palette::base()),
        (format!("{pad}link         {}", link_label(link, now)), palette::amber()),
        (String::new(), palette::base()),
        (format!("{pad}{}", d.comes_back), palette::muted()),
        (String::new(), palette::base()),
        (format!("{pad}[{}] restart now   {}", d.restart_key, d.restart_cmd), palette::base()),
    ];
    if let Some(n) = &d.restart_note {
        text.push((format!("{pad}    {n}"), palette::muted()));
    }
    (0..rows)
        .map(|i| {
            hits.push(Target::Empty);
            match text.get(i) {
                Some((t, st)) => size_line(vec![Span::styled(t.clone(), *st)], w, palette::base()),
                None => size_line(vec![], w, palette::base()),
            }
        })
        .collect()
}

/// The logical text of a CHROME row, read off the row that was drawn.
///
/// Header, banner, footer, composer and overlay rows have no model string
/// behind them — the header in particular assembles itself from ranked
/// segments and may drop or crop several of them. So their logical text is
/// **what survived onto the screen**, taken from the `Line` itself rather than
/// predicted from the segments that were offered. A predicted string would
/// disagree with the row exactly when the row is tight, which is exactly when
/// he is most likely to be copying it to show somebody.
///
/// `col0` skips the band's leading gutter; the trailing pad is trimmed,
/// because pad is not text.
fn chrome_text(line: &Line<'static>, unit: Unit, col0: u16) -> Option<RowText> {
    let whole: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let mut col = 0usize;
    let mut text = String::new();
    for ch in whole.chars() {
        if col >= col0 as usize {
            text.push(ch);
        }
        col += ch.width().unwrap_or(0);
    }
    let text = text.trim_end().to_string();
    if text.is_empty() {
        return None;
    }
    Some(RowText {
        unit,
        start: 0,
        col0,
        text,
    })
}

/// Whichever of the six bodies this mode wants, always exactly as tall as
/// [`bands`] said it would be — and pushing one [`RowInfo`] per row it draws,
/// so what is UNDER a coordinate and what is WRITTEN at it can neither come
/// apart from each other nor from the row.
fn body(
    vs: &ViewState,
    now: DateTime<Utc>,
    w: usize,
    b: Bands,
    hits: &mut HitMap,
) -> Vec<Line<'static>> {
    let banner = |hits: &mut HitMap| {
        let v = banner_rows(vs.banner.as_ref(), &vs.header.link, now, w, b.banner);
        for line in &v {
            hits.push(Target::Banner);
            if let Some(t) = chrome_text(line, Unit::new(UnitKind::Banner, 0), GUTTER as u16) {
                hits.text_on_last(t);
            }
        }
        v
    };
    match vs.mode {
        Mode::Sheet | Mode::SheetField => {
            let (v, infos) = sheet_body(vs, now, b.content, w);
            for (i, line) in v.iter().enumerate() {
                hits.push_row(infos[i].clone());
                if let Some(t) = chrome_text(line, Unit::new(UnitKind::Overlay, i), 0) {
                    hits.text_on_last(t);
                }
            }
            v
        }
        Mode::Help | Mode::Queue => {
            // **THE QUEUE VIEW CARRIES ITS OWN ROW INFOS**, because its client
            // half has pressable buttons on it; the other two overlays are
            // uniformly "click anywhere to dismiss".
            let (v, own) = match vs.mode {
                Mode::Help => (
                    help_overlay(b.content, w, vs.caps, vs.help_for, vs.help_from),
                    None,
                ),
                _ => {
                    let (v, infos) = queue_overlay_with_client(vs, b.content, w);
                    (v, Some(infos))
                }
            };
            for (i, line) in v.iter().enumerate() {
                match own.as_ref().and_then(|o| o.get(i)) {
                    Some(info) => hits.push_row(info.clone()),
                    None => hits.push(Target::Overlay),
                }
                // An overlay row is addressed BY ROW: its rows come from a
                // table, a lane list or a queue and there is no model string
                // behind any single one of them.
                if let Some(t) = chrome_text(line, Unit::new(UnitKind::Overlay, i), 0) {
                    hits.text_on_last(t);
                }
            }
            v
        }
        // **THE PAGE IS THE ROOT'S BODY.** One call, and the whole difference
        // is `ProjectList::focus` — which is why `o`, `r`, `p`, `c`, move-pin,
        // the sheet doors, the drag selection and the click grammar all work on
        // it with no second implementation of anything.
        Mode::Projects | Mode::NewProject | Mode::SeeAll(_) => {
            let mut v = banner(hits);
            let (rows, infos) = project_list(&vs.projects, now, w, b.content);
            hits.push_many(infos);
            v.extend(rows);
            v
        }
        Mode::Archived => {
            let (rows, infos) = archived_body(&vs.archived, now, b.content, w);
            for (i, line) in rows.iter().enumerate() {
                hits.push_row(infos[i].clone());
                if let Some(t) = chrome_text(line, Unit::new(UnitKind::Content, i), GUTTER as u16) {
                    hits.text_on_last(t);
                }
            }
            rows
        }
        Mode::List => {
            let mut v = banner(hits);
            let (rows, infos) = thread_list(&vs.list, now, w, b.content);
            hits.push_many(infos);
            v.extend(rows);
            v
        }
        Mode::Activity => {
            let mut v = banner(hits);
            let (rows, infos) = transcript(vs, now, w, b.content);
            hits.push_many(infos);
            v.extend(rows);
            v
        }
        Mode::Thread(_) => {
            let mut v = banner(hits);
            let (rows, infos) = transcript(vs, now, w, b.content);
            hits.push_many(infos);
            v.extend(rows);
            let (c, cinfos) = composer(vs, w, b.composer);
            // **WHICH LAID-OUT ROW EACH DRAWN ROW IS** — see
            // [`composer_row_map`]. The band scrolls to follow the caret, so
            // drawn row 1 is not laid row 0 in a draft taller than the band;
            // resolving that here means the shell never learns the band's
            // scroll offset, which is the one thing it could get wrong about a
            // click it cannot see.
            let caret_rows = composer_row_map(&vs.composer, w, b.composer);
            for (i, line) in c.iter().enumerate() {
                match caret_rows.get(&i) {
                    Some(&row) => hits.push(Target::ComposerRow { row, col: 0 }),
                    // **THE QUEUE'S ROWS CARRY THEIR OWN TARGETS**, spans and
                    // all, so the `▶` on a spoken row is clickable where it is
                    // painted — the band builds them, this loop does not
                    // re-derive them.
                    None => match cinfos.get(i) {
                        Some(info) if info.target != Target::Composer => hits.push_many(vec![info.clone()]),
                        _ => hits.push(Target::Composer),
                    },
                }
                if let Some(t) = chrome_text(line, Unit::new(UnitKind::Composer, i), GUTTER as u16)
                {
                    hits.text_on_last(t);
                }
            }
            v.extend(c);
            // **THE SUBAGENTS, OUT OF THE CHAT AND INTO A BAND OF THEIR OWN.**
            let (a, ainfos) = agents_band(vs, now, w, b.tree);
            for (i, line) in a.iter().enumerate() {
                match ainfos.get(i) {
                    Some(info) => hits.push_row(info.clone()),
                    None => hits.push(Target::Empty),
                }
                // **ITS OWN UNIT KIND.** Addressing a tree row as
                // `Composer(i)` would name the same unit as the box's `i`th
                // row, and a drag across both would take the wrong text.
                if let Some(t) = chrome_text(line, Unit::new(UnitKind::Agents, i), GUTTER as u16) {
                    hits.text_on_last(t);
                }
            }
            v.extend(a);
            v
        }
        // **MIC MODE.** The transcript above, exactly as in a thread, and the
        // mic band where the composer was.
        Mode::Mic => {
            let mut v = banner(hits);
            let (rows, infos) = transcript(vs, now, w, b.content);
            hits.push_many(infos);
            v.extend(rows);
            let (m, infos) = mic_band(vs, w, b.composer);
            hits.push_many(infos);
            v.extend(m);
            // Drawn, read-only, with no cursor highlight: the band must not
            // claim the keyboard is in it while mic mode has the keys.
            let (a, ainfos) = agents_band(vs, now, w, b.tree);
            hits.push_many(ainfos);
            v.extend(a);
            v
        }
        // **THE LITTLE RENAME MODAL — his surface, still drawn, with a box at
        // the foot of it.**
        //
        // *"Don't take me to the settings. Just a little modal."* So the body
        // is whichever list he was standing on, built by the SAME function that
        // builds it when no modal is up, and the box below it.
        //
        // **EVERY ROW BEHIND THE BOX IS INERT WHILE IT IS UP, and that is not
        // decoration.** The rows are still `ThreadRow`/`ThreadTitle` shaped,
        // and a click on a title resolves to `Intent::OpenAt` — which would
        // open a thread out from under a half-typed rename and throw the
        // keystrokes away. A modal that can be clicked through is not a modal.
        // So the targets are replaced with `Target::Empty`, which
        // `hit::activation` answers with nothing: the box is left with `esc` or
        // committed with `enter`, and there is no third way out that loses
        // work.
        Mode::RenameModal => {
            let mut v = banner(hits);
            // The list he was reading. `rename_under` is the ONE fact the
            // renderer cannot derive, and `Mode::Projects` is the honest
            // fallback for a shell bug rather than a panic inside a render.
            let under = vs.rename_under.unwrap_or(Mode::Projects);
            let (rows, infos) = if under == Mode::List {
                thread_list(&vs.list, now, w, b.content)
            } else {
                project_list(&vs.projects, now, w, b.content)
            };
            hits.push_many(
                infos
                    .into_iter()
                    .map(|_| RowInfo::bare(Target::Empty))
                    .collect::<Vec<_>>(),
            );
            v.extend(rows);
            let box_rows = rename_modal_band(&vs.sheet, vs.modal_label.as_deref(), w, b.composer);
            for line in &box_rows {
                hits.push(Target::Empty);
                if let Some(t) = chrome_text(line, Unit::new(UnitKind::Composer, 0), GUTTER as u16)
                {
                    hits.text_on_last(t);
                }
            }
            v.extend(box_rows);
            v
        }
    }
}

/// **THE LITTLE RENAME MODAL'S BAND** — a titled rule, the name, a closing
/// rule.
///
/// It draws FROM `vs.sheet.field`, which is the sheet's own rename buffer
/// projected exactly as it is for [`Mode::SheetField`]. There is no second copy
/// of what he is typing and therefore no second place for the prefill to be
/// wrong — the property `the_rename_field_is_prefilled_with_the_summary_it_
/// edits` asserts is the same buffer this box shows.
///
/// **THE CARET IS DRAWN, and the sheet's field does not draw one.** That is a
/// deliberate difference and not an inconsistency: this box is the WHOLE
/// surface he is aimed at while it is up, and *"prefilled with the current
/// summary, and the caret placed so he can edit rather than retype"* is the
/// ask. A prefilled box with no visible cursor is a box he cannot tell is
/// prefilled from a box that is showing him a label.
///
/// It is painted by RESTYLING the drawn row, exactly as the composer's caret
/// and a drag selection are, so it cannot change the row's width and the shape
/// law survives it by construction.
fn rename_modal_band(
    sheet: &Option<crate::model::SheetView>,
    label: Option<&str>,
    w: usize,
    rows: usize,
) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::with_capacity(rows);
    if rows == 0 {
        return out;
    }
    // The box NAMES ITSELF AND ITS TWO DOORS in its own rule, exactly as the
    // sheet's field does — he never has to remember what enter will do to a
    // thread's name.
    let label = format!(" {} ", label.unwrap_or("rename — enter renames · esc cancels"));
    let label = label.as_str();
    out.push(line_from(
        vec![
            Seg::new("─".repeat(GUTTER), palette::muted()),
            Seg::new(label, palette::header()),
            Seg::new(
                "─".repeat(w.saturating_sub(GUTTER + text_width(label))),
                palette::muted(),
            ),
        ],
        w,
    ));
    let inner = rename_modal_inner(w);
    let pad = " ".repeat(GUTTER + 1);
    let body_rows = rows.saturating_sub(2);
    match sheet.as_ref().and_then(|s| s.field.as_ref()) {
        // A modal with no field is a shell bug, not a state. An honest sentence
        // beats a panic inside a render, and beats an empty box that looks
        // ready to take a name it has nowhere to put.
        None => {
            out.push(line_from(
                vec![
                    Seg::new(pad.clone(), palette::base()),
                    Seg::new("no name to edit", Severity::Unknown.style()),
                ],
                w,
            ));
        }
        Some(field) => {
            let laid = field.buf.layout(inner);
            let (caret_row, caret_col) = field.buf.caret(inner);
            for i in 0..body_rows {
                let Some(row) = laid.get(i) else {
                    out.push(size_line(vec![], w, palette::base()));
                    continue;
                };
                let line = line_from(
                    vec![
                        Seg::new(pad.clone(), palette::base()),
                        Seg::new(field.buf.row_text(*row).to_string(), palette::ink()),
                    ],
                    w,
                );
                out.push(if i == caret_row {
                    caret(line, GUTTER + 1 + caret_col)
                } else {
                    line
                });
            }
        }
    }
    // The closing rule earns its row for the composer band's reason: without
    // it the name sits directly on the footer's key hints and the two read as
    // one line of chrome.
    out.truncate(rows.saturating_sub(1));
    while out.len() < rows.saturating_sub(1) {
        out.push(size_line(vec![], w, palette::base()));
    }
    out.push(line_from(
        vec![Seg::new("─".repeat(w), palette::muted())],
        w,
    ));
    out.truncate(rows);
    out
}

// ── the too-small frame ──────────────────────────────────────────────────────

/// Below the minimum, the face says ONE true thing and draws nothing else.
///
/// Not a degraded layout: four bands squeezed into six rows would have to lie
/// about at least one of their sizes, and a band that lies about its size is
/// how the over-report panic happens.
fn too_small(width: u16, height: u16) -> Vec<Line<'static>> {
    let w = width as usize;
    let msg = format!("tui needs {MIN_WIDTH}x{MIN_HEIGHT} (have {width}x{height})");
    (0..height as usize)
        .map(|i| {
            if i == 0 {
                size_line(
                    vec![Span::styled(msg.clone(), palette::amber())],
                    w,
                    palette::base(),
                )
            } else {
                size_line(vec![], w, palette::base())
            }
        })
        .collect()
}

// ── band 1: the header ───────────────────────────────────────────────────────

/// **THE HEADER IS ORDERED BY WHAT MUST SURVIVE THE CROP**, not by what reads
/// prettily at 150 columns.
///
/// The row ellipsizes from the right, so position IS priority — and the live
/// run is what settled the order. At 80 columns the first draft cropped to
/// `… · acct ? · ▲…`: the queue badge, which `DIR-6` says must be visible AT
/// ALL TIMES, had lost its digit to the endpoint URL. And in a thread view at
/// 150 the `filtered` note fell off the end, so a transcript with a lane
/// switched off did not look switched off.
///
/// So, in order: **what is happening** (the title), **the queue badge**
/// (`DIR-6`, first requirement), **whether what is below is complete** (the
/// `stdout only` and `expanded` notes — the first was `filtered(N)` until round
/// six deleted the lane filter), **what is happening elsewhere** (unread), and
/// only then the context — cwd, link, account.
///
/// The link's demotion is deliberate and is covered elsewhere: a link in
/// trouble raises a BANNER over the content, which is the surface that has room
/// to say what went wrong and when the next retry is. `connected` is the steady
/// state, and a steady state is the cheapest thing on the row to lose.
///
/// Returns the row AND the column span the queue badge survived at, if it
/// survived — which is what makes `DIR-6`'s "visible at all times" badge
/// REACHABLE at all times too. The span is computed from the segments that
/// actually got drawn, after the rank-drop and the crop, rather than predicted
/// from the ones that were offered: a badge whose columns are guessed is a
/// click target that is wrong exactly when the row is tight, which is when he
/// is most likely to be hunting for it.
fn header_line(
    vs: &ViewState,
    now: DateTime<Utc>,
    w: usize,
) -> (Line<'static>, Option<(u16, u16)>) {
    let hd = &vs.header;
    let dot = || Seg::new(" · ", palette::muted());
    // (rank, segments). Written in DISPLAY order; dropped in RANK order,
    // highest first, until the row fits. A plain crop would have taken them
    // right-to-left, which is how the badge came to lose its digit.
    let mut parts: Vec<(u8, Vec<Seg>)> = Vec::new();

    // **THE BREADCRUMB TRAIL — THE BACK STACK, RENDERED**, in front of the
    // title, muted, outermost crumb first:
    //
    // > The threads should be aware of their breadcrumbs.
    //
    // Each crumb names a screen `←` will land on; the leaf is the title itself
    // and is not repeated. The ROOT IS THE FLOOR AND IS NOT WRITTEN — a thread
    // opened straight from a band therefore has an EMPTY trail and no part at
    // all here, which is honest: its breadcrumb is the menu.
    //
    // **THE SEPARATOR IS `›`, AND IT CARRIES NO INFORMATION A LEGEND WOULD
    // OWE.** `0040`'s "no glyph anywhere carries information" sets the test
    // "could he read it cold, with no legend"; `recent › Answer the tax letter`
    // passes it, because the glyph is punctuation between two words and not a
    // symbol standing for one. `▸` is already the cursor, so this one is not
    // re-used from anywhere.
    //
    // **IT IS ITS OWN PART AT RANK 2** — a STATEMENT ABOUT WHERE HE IS rather
    // than metadata about it — so it drops after the badge's scope label (3),
    // the cwd (5) and `acct` (7), and before `N queued` (1), which `DIR-6`
    // forbids dropping at any width. And before it drops WHOLE it elides from
    // the LEFT: see `elide` below and the loop that calls it.
    // **THE THREAD PAGE'S HEADER IS TWO FACTS: ITS NAME AND ITS STATUS**
    // (TUI-P1a, checklist TC-04). His words, after asking more than once:
    // *"The only thing I want to see at the top of the page, is the name of
    // the thread and whether it's active or idle."* The shell puts both in the
    // TITLE (`store::header`), so on a thread page every other part is
    // withheld here — the trail, the activity phrase, the queue count and its
    // scope, `unread elsewhere`, the warnings, the cwd, the link word and the
    // account. A link in trouble still has the BANNER, which is where an
    // outage has room to say why. The two view chips (`stdout only`,
    // `expanded`) stay: they are absent unless he switched one on, and a
    // transcript with rows held back that does not say so has a hole in it.
    // The ROOT header is a separate ruling and is untouched.
    // `hd.thread_page` is the shell's answer for the surface UNDER an overlay
    // (inspect F-2: with only `vs.mode`, the rename box, queue, help and sheet
    // over a thread brought every banned part back).
    let thread_page = hd.thread_page || matches!(vs.mode, Mode::Thread(_) | Mode::Mic);
    let no_trail: [String; 0] = [];
    let crumbs: &[String] = if thread_page { &no_trail } else { &hd.trail };
    // `… › ` when the outermost crumbs have been elided away, then one
    // `crumb › ` per surviving crumb. Built as ONE segment because the whole
    // part shares one style and the crop never falls inside it — the title's
    // does.
    let trail_segs = |from: usize| -> Vec<Seg> {
        let mut t = String::new();
        if from > 0 {
            t.push_str("… › ");
        }
        for c in &crumbs[from..] {
            t.push_str(c);
            t.push_str(" › ");
        }
        vec![Seg::new(t, palette::muted())]
    };
    // `usize::MAX` means "no trail part on this row", which keeps the slot
    // index arithmetic below honest without a second `Option`.
    let mut trail_slot = usize::MAX;
    if !crumbs.is_empty() {
        trail_slot = parts.len();
        parts.push((2, trail_segs(0)));
    }

    // **THE TITLE IS NOT ALWAYS SLOT 0 ANY MORE**, so its index is recorded
    // rather than assumed. It was assumed in three places and the trail sits in
    // front of it on the row, which is a DISPLAY-order fact: the parts vector
    // is written in display order and concatenated in slot order, so the trail
    // has to be pushed first and the title's floor has to follow it.
    let title_slot = parts.len();
    parts.push((
        0,
        vec![Seg::new(
            if hd.title.is_empty() {
                "no thread open"
            } else {
                &hd.title
            },
            palette::header(),
        )],
    ));
    // **THE QUEUE COUNT, IN WORDS, AND IT IS STILL RANK 1.**
    //
    // It was `▲{N}` until the operator said *"I don't know what ▲1 means so
    // let's drop it. users won't get it."* — which is a man failing to
    // RECOGNISE a rendering, not a man retracting `DIR-6`, his own directive
    // that the queue count be visible AT ALL TIMES and the one round two
    // restored as a defect fix after the collapse work dropped it.
    // "Users won't get it" is a LEGIBILITY complaint, so the defect is the
    // GLYPH and not the information: the count stays, spelled out, at the same
    // never-dropped rank. **This is a decision he may overturn in one beat**,
    // and it is one word and two lines if he does.
    //
    // `? queued` when the daemon has not told us — never `0 queued`, which is
    // honesty rule 1's exact violation, and the zero is dropped entirely rather
    // than spelled, because a word costs more room than a glyph did.
    // **WHAT THE THREAD IS DOING, WHEN ITS STATE WORD WOULD LIE** — `waiting
    // on 2 subagents`, never `idle`, while its children run. Rank 1 beside the
    // queue badge: it is the answer to "is this finished or only started",
    // which is the question the title alone cannot answer.
    if let Some((text, sev)) = hd.activity.as_ref().filter(|_| !thread_page) {
        parts.push((1, vec![dot(), Seg::new(text.clone(), sev.style())]));
    }
    if !thread_page && !matches!(hd.queued, Known::Known(0)) {
        parts.push((
            1,
            vec![
                dot(),
                Seg::new(
                    format!("{} queued", hd.queued.label()),
                    known_style(&hd.queued),
                ),
            ],
        ));
    }
    // The badge's SCOPE — what the number is the queue OF. It sits next to the
    // badge, because a scope label anywhere else reads as qualifying whatever
    // it has landed beside (the live run produced `▲0 · filtered(1) this
    // thread`, where "this thread" appeared to be about the filter). RANK and
    // DISPLAY POSITION are independent here for exactly this reason: it is
    // drawn second and dropped fifth.
    //
    // And it CAN go when the row is tight: the TITLE already frames it —
    // "threads · the 1000" in the fleet view, the thread's own id in a thread
    // view — so a bare `▲N` beside it is never a fleet total in disguise.
    // **THE ROOT HEADER IS THREE THINGS**, by his own list: the unread count,
    // an active count (both in the title), and `connected`. The queue count
    // survives on the `DIR-6` reading above. Everything else this row can carry
    // is dropped HERE rather than in the fit loop, because the fit loop drops
    // by WIDTH and he asked for these to be gone at every width.
    // **A `see all` PAGE IS A ROOT SURFACE FOR THIS CUT.** It is the root's own
    // projection with one band in focus, so the four facts his cut removed from
    // the root header — the badge's scope label, `N unread elsewhere`, the cwd
    // and `acct ?` — are removed from the page's too, and `connected` survives
    // at every width there as it does at the root. *"All I want to see is the #
    // unread …, and connected."*
    let root = matches!(
        vs.mode,
        Mode::Projects | Mode::NewProject | Mode::SeeAll(_)
    );
    if !hd.queue_scope.is_empty() && !root && !thread_page {
        parts.push((
            3,
            vec![Seg::new(format!(" {}", hd.queue_scope), palette::muted())],
        ));
    }
    // **A COLLAPSED VIEW SAYS SO, IN WORDS, AND THE WORDS ARE WHAT IT IS.**
    // This is not metadata: it is a statement about whether what is below is
    // COMPLETE, and a transcript with rows held back that does not look held
    // back is a transcript with a hole in it.
    //
    // **IT USED TO READ `filtered(N)` AND THAT IS THE DEFECT IT IS FIXING.**
    // His words: *"also pressing s sets a filter(8) and I don't know what that
    // means."* A COUNT of hidden lane-groups is only readable by somebody who
    // knows there are nine groups, which is exactly the granularity he threw
    // out. The chip now names the STATE — `stdout only` — which is the whole
    // of what the toggle does and needs no vocabulary to decode.
    if vs.stdout_only {
        parts.push((2, vec![dot(), Seg::new("stdout only", palette::amber())]));
    }
    if vs.expanded {
        parts.push((2, vec![dot(), Seg::new("expanded", palette::muted())]));
    }
    // **`N unread elsewhere` IS GONE FROM THE ROOT** — "I don't need to see …
    // 1656 unread". The number itself is not gone: the root's TITLE carries it
    // as `1656 unread`, without the "elsewhere" framing and without stating
    // that it is counted off the daemon's hundred-thread page, which he ruled
    // must not be said.
    if !root && !thread_page {
        if let other @ (Known::Unknown | Known::Known(1..)) = &hd.unread_elsewhere {
            parts.push((
                4,
                vec![
                    dot(),
                    Seg::new(
                        format!("{} unread elsewhere", other.label()),
                        known_style(other),
                    ),
                ],
            ));
        }
    }
    // ── THE DAEMON'S OWN WARNING COUNT ───────────────────────────────────
    //
    // **RANK 2, AND SHOWN ON THE ROOT TOO.** Both are departures
    // and both are the operator's newer instruction: *"display the number of
    // warnings coming from threads so it annoys me? 27k is incredible."* His
    // earlier root cut (*"All I want to see is the # unread …, and connected"*)
    // removed four facts from this row; this is not a fifth returning, because it
    // is ABSENT whenever there is nothing to say, so a healthy daemon's root
    // header is byte-identical to what that cut left.
    //
    // **`Known(1..)` ONLY — zero and UNKNOWN both draw nothing**, which is where
    // this part differs from `unread elsewhere` above and the difference is
    // deliberate:
    //
    // * ZERO is absent because a number permanently on screen becomes wallpaper,
    //   and wallpaper is exactly how a log file with 29,644 warnings in it
    //   managed to be ignored for five weeks. A `⚠ 0 warnings` he stops seeing
    //   is the same defect with a nicer font.
    // * UNKNOWN is absent, rather than drawn as `?`, because it is the normal
    //   state against every daemon built before this field existed. A `⚠ ?` on
    //   every connection to an un-promoted daemon would be noise this feature is
    //   supposed to be reducing — and unlike the counts the shell DOES paint `?`
    //   for, nothing here is actionable: there is no key he can press to make an
    //   old daemon report.
    //
    // RED once anything was an ERROR, amber otherwise. The colour follows the
    // severity of what was actually counted rather than a magnitude somebody
    // picked; 199 of the live log's records are `ERROR`.
    if let (false, Known::Known(n @ 1..)) = (thread_page, &hd.warnings) {
        let errs = match &hd.warning_errors {
            Known::Known(e) => *e,
            Known::Unknown => 0,
        };
        let style = if errs > 0 {
            palette::red()
        } else {
            palette::amber()
        };
        let mut label = format!(
            "{} warning{}",
            grouped(*n),
            if *n == 1 { "" } else { "s" }
        );
        if errs > 0 {
            label.push_str(&format!(
                " ({} error{})",
                grouped(errs),
                if errs == 1 { "" } else { "s" }
            ));
        }
        parts.push((2, vec![dot(), Seg::new(label, style)]));
    }
    if !hd.cwd.is_empty() && !root && !thread_page {
        parts.push((5, vec![dot(), Seg::new(hd.cwd.clone(), palette::muted())]));
    }
    // The link is LATE on purpose. A link in trouble raises a BANNER over the
    // content, which has a whole row to name the reason and the next retry;
    // `connected` is the steady state and the cheapest thing here to lose.
    //
    // **AT 80 COLUMNS A HEALTHY LINK IS NOT OFFERED AT ALL** — `0040` §11's
    // first named casualty, verbatim: *"First to go: the word `connected` (a
    // healthy default is the cheapest fact on screen)"*. It is dropped rather
    // than left to the rank machinery because the rank machinery's
    // restore-the-last-one step would hand it back CROPPED, and the measured
    // result of that was `· connec…` at 60 — a mid-word fragment of the one
    // word the doc says should already have gone. A link in TROUBLE is still
    // offered at every width: it is not a steady state, it is news.
    // **`connected` SURVIVES AT EVERY WIDTH ON THE ROOT, AND NOWHERE ELSE.**
    //
    // `0040` §11's first named casualty is *"the word `connected` (a healthy
    // default is the cheapest fact on screen)"*, and that stands for the thread
    // and list headers, whose rows are still full. It does NOT stand for the
    // root any more, because the root header is now three facts by his own
    // list and `connected` is one of the three he named:
    //
    // > All I want to see is the # unread …, and connected.
    //
    // Measured at 60: `5 unread · 1 active · 2 queued · connected` is 42 of 60
    // columns, so it is not competing with anything — the §11 rule was about a
    // crowded row, and this row is no longer crowded. A link in TROUBLE is
    // still offered at every width in every mode: it is not a steady state, it
    // is news.
    let drop_healthy_link =
        !root && w <= geom::NARROW_WIDTH as usize && matches!(hd.link.state, LinkState::Connected);
    if !drop_healthy_link && !thread_page {
        parts.push((
            // **A LINK IN TROUBLE IS RANK 1 AND IS THEREFORE NEVER DROPPED.**
            //
            // The comment above has always said a troubled link is offered at
            // every width; it was a hope until now, because rank 6 is droppable
            // and the fit loop drops by rank. MEASURED when `▲?` became
            // `? queued` and this row grew six columns: at 150 a disconnected
            // thread header lost `disconnected (the daemon closed the socket) ·
            // down 41s · retry in 4s` ENTIRELY while `acct ?` — rank 7, the
            // least important thing this row can carry — was restored into the
            // slack. §11's order exactly inverted, and invisible unless you
            // look at the frame.
            //
            // **RANK 2, NOT RANK 1.** Rank 1 is `DIR-6`'s undroppable slot and
            // putting a troubled link in it broke the TITLE FLOOR: an outage
            // label is `disconnected (the daemon closed the socket) · down 41s
            // · retry in 4s`, which alone is wider than a 40-column terminal,
            // so nothing droppable remained and the title was squeezed below
            // `TITLE_FLOOR` — the debug assertion below caught it. Rank 2
            // makes the outage the LAST droppable fact instead of an
            // undroppable one: `acct`, `cwd`, `unread elsewhere` and the
            // badge's scope all go before it, and it still yields at the width
            // where keeping it would cost the title its floor.
            //
            // `connected` keeps rank 6 and stays the cheapest thing here to
            // lose, which is what §11 actually ruled.
            if matches!(hd.link.state, LinkState::Connected) {
                6
            } else {
                2
            },
            vec![
                dot(),
                Seg::new(link_label(&hd.link, now), hd.link.state.severity().style()),
            ],
        ));
    }
    // `acct ?` goes at the root too. It is not on his list of three, and at the
    // root it is `Unknown` by construction — `store::header`'s Projects arm has
    // no thread to read an account off.
    if !root && !thread_page {
        parts.push((
            7,
            vec![
                dot(),
                Seg::new(
                    format!("acct {}", hd.account.label()),
                    known_style(&hd.account),
                ),
            ],
        ));
    }

    let width_of = |p: &[Seg]| p.iter().map(|s| text_width(&s.text)).sum::<usize>();
    let mut slots: Vec<Option<(u8, Vec<Seg>)>> = parts.into_iter().map(Some).collect();

    // **THE TWO THINGS THAT NEVER GO, AT ANY WIDTH** — and the fit is computed
    // around them rather than hoping the crop spares them.
    //
    // `DIR-6`: the queue count is visible AT ALL TIMES, and `0040` §11 spells
    // out that this means 60 columns too. `0040` §11 also rules that the title
    // *"ellipsizes but survives"*.
    //
    // The old fit could honour neither at 60. It dropped whole parts until the
    // row fitted and then let one ellipsis crop fall wherever it landed — so a
    // long title, which rank 0 protects from being dropped, simply ate the row
    // and the badge went off the end with it. MEASURED on a real pty at 60
    // before this change, the root header read
    // `projects · 4 under ~/minds · 2 with an index · 5 threads · …` with NO
    // `▲` anywhere on the frame: a directive violated in silence.
    //
    // So: the title is held to a FLOOR while the others are dropped, and the
    // crop that closes the row falls on the TITLE and never on the badge.
    let title_natural = width_of(&slots[title_slot].as_ref().expect("the title has a slot").1);
    let title_floor = TITLE_FLOOR.min(title_natural).max(1).min(w);
    let others_w = |slots: &[Option<(u8, Vec<Seg>)>]| -> usize {
        slots
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != title_slot)
            .filter_map(|(_, s)| s.as_ref())
            .map(|(_, p)| width_of(p))
            .sum()
    };

    // **THE METADATA GOES FIRST AND ALL OF IT GOES BEFORE THE CONTENT GIVES UP
    // ONE COLUMN.** The loop is written against the title's NATURAL width, not
    // against its floor: it keeps dropping droppable facts for as long as that
    // would let the title stay whole, and the title only starts shortening once
    // there is nothing droppable left. Written against the floor instead, this
    // stopped the moment the title could merely survive — which at 100 columns
    // cropped `projects · 4 und…` while `acct ?` sat on the same row intact,
    // §11's law exactly inverted.
    let mut dropped: Vec<(usize, (u8, Vec<Seg>))> = Vec::new();
    // **THE TRAIL ELIDES FROM THE LEFT, and it does so at its own rank.**
    //
    // The outermost crumb is the FURTHEST door; the nearest one is the door `←`
    // is about to use, so that is the one to keep. When the row is tight, `pinned
    // › the threads daemon wire › ` becomes `… › the threads daemon wire › `
    // and only then, one crumb at a time, does the part go whole.
    //
    // It is done INSIDE the fit loop rather than before it, so the rank order
    // is still what decides: the `acct`, `cwd` and scope facts are dropped
    // first, and the trail only starts shortening when nothing cheaper is left
    // — which is `0040` §11's own order applied to a fact it never had.
    let mut elided = 0usize;
    while others_w(&slots) + title_natural > w {
        let Some(i) = worst(&slots) else { break };
        if i == trail_slot && elided + 1 < crumbs.len() {
            elided += 1;
            slots[i] = Some((2, trail_segs(elided)));
            continue;
        }
        let part = slots[i].take().expect("worst() only returns filled slots");
        dropped.push((i, part));
    }
    // PUT THE LAST ONE BACK — but ONLY if it belongs at the END of the row.
    //
    // Dropping whole parts alone leaves the tail blank whenever the part that
    // did not fit was only slightly too big: at 100 columns that cost the
    // endpoint twenty-five usable characters for nothing. So the most important
    // of the dropped comes back and the ellipsis crop takes what it must, which
    // is the crop doing the one thing it is good at.
    //
    // The guard is the other half of that, and the live run is what asked for
    // it. A restored part with survivors to its RIGHT pushes them off the end —
    // so at 40 columns the badge's scope came back and ate the `filtered(1)`
    // notice that outranks it, which is the exact inversion the ranking exists
    // to prevent. Restore it only where the crop can fall on it and nothing
    // else.
    //
    // **AND IT IS RESTORED INTO SLACK THE TITLE DOES NOT WANT, never into the
    // title's own columns.** The title takes what it NEEDS, not everything
    // left: `connected` surviving at 100 is the doc's own 100-column frame, so
    // handing every spare column to a title that is already whole would have
    // traded a fact for padding.
    //
    // **EXCEPT FOR RANK 2, WHICH TAKES THE TITLE DOWN TO ITS FLOOR.** MEASURED
    // in `0076` — the chip said `filtered(1)` then and says `stdout only` now;
    // the measurement is about the SLACK and survives the rename —
    // when the thread header stopped being an eight-character hash
    // and became the thread's NAME: at 40 columns a filtered transcript's
    // header read `answer the tax letter · acti… · 2 queued` with the
    // `filtered(1)` chip GONE — a view with a lane switched off that did not
    // look switched off, which is the one thing that chip exists to prevent.
    //
    // The mechanism was this slack, and it is subtle: nothing dropped the chip
    // on purpose. The chip was dropped by the fit loop and then NOT RESTORED,
    // because the slack is measured against the title's NATURAL width — so a
    // title that merely got longer silently outbid a fact that outranks it.
    // It is `0072`'s restore defect one level up: that round found this same
    // step handing columns to `acct ?` while a TROUBLED LINK fell off, and
    // fixed it by re-ranking the link to 2. The rank was right; the slack was
    // still reading the title's appetite.
    //
    // So rank 2 — a collapsed (`stdout only`) view, an expanded view, a link
    // in trouble — is a
    // STATEMENT ABOUT WHETHER WHAT IS BELOW IS COMPLETE rather than metadata,
    // and `0040` §11's order says metadata goes before content gives up a
    // column, not that a completeness statement does. It may take the title's
    // columns down to [`TITLE_FLOOR`] and no further. Rank 3 and worse still
    // may not take one column the title wants, which is the old rule intact.
    let rightmost = slots
        .iter()
        .enumerate()
        .filter(|(_, s)| s.is_some())
        .map(|(i, _)| i)
        .next_back()
        .unwrap_or(0);
    if let Some((i, part)) = dropped.pop() {
        if i > rightmost {
            let (rank, segs) = part;
            let title_need = if rank <= 2 {
                title_floor.min(title_natural)
            } else {
                title_natural
            };
            let slack = w.saturating_sub(others_w(&slots)).saturating_sub(title_need);
            if slack > 0 {
                slots[i] = Some((rank, crop_with_ellipsis(segs, slack)));
            }
        }
    }

    // **THE CROP FALLS ON THE TITLE.** Everything else has either survived
    // whole or been dropped whole, so this is the one place a row can lose
    // characters — which is exactly §11's order: the content is what shortens,
    // and it shortens last, after every droppable fact has already gone.
    let title_budget = w.saturating_sub(others_w(&slots));
    debug_assert!(
        title_budget >= title_floor || worst(&slots).is_some(),
        "the title was squeezed below its floor with droppable facts still on the row"
    );
    if let Some(slot) = slots[title_slot].take() {
        slots[title_slot] = Some((slot.0, crop_with_ellipsis(slot.1, title_budget)));
    }

    // The ellipsis crop is still the last word — it is the only thing here that
    // cannot return the wrong width.
    let segs: Vec<Seg> = slots.into_iter().flatten().flat_map(|(_, p)| p).collect();
    let drawn = crop_with_ellipsis(segs, w);
    // **THE BADGE IS THE ONE SEGMENT THAT ENDS IN `queued`.** It was
    // `starts_with('▲')` until the glyph became a word; the claim it feeds is
    // `Target::QueueBadge`, which is the MOUSE's only door onto the queue
    // overlay, so re-keying this was not optional when the rendering changed —
    // a detector still hunting a character nothing draws would have left the
    // badge unclickable and said nothing about it.
    //
    // Its column is the sum of what the crop left to its left — measured, not
    // predicted, so a cropped row claims the columns it actually drew.
    let mut col = 0usize;
    let mut badge = None;
    for s in &drawn {
        let width = text_width(&s.text);
        if badge.is_none() && s.text.trim_end().ends_with("queued") {
            badge = Some((col as u16, (col + width).min(w) as u16));
        }
        col += width;
    }
    (line_from(drawn, w), badge)
}

/// **The columns the header title is never squeezed below**, so that "it
/// ellipsizes but survives" (`0040` §11) is a property and not a hope. Sixteen
/// is a few words plus the `…` — enough to read, and enough to be a door the
/// hand can find.
const TITLE_FLOOR: usize = 16;

/// The index of the LAST filled part with the highest rank — last, so that two
/// parts sharing a rank drop right-to-left, which is the direction the eye
/// expects.
///
/// **RANKS 0 AND 1 ARE NEVER DROPPED.** Rank 0 is the title, which is what the
/// row is about; rank 1 is the QUEUE BADGE, which `DIR-6` says is visible at
/// all times and `0040` §11 says means 60 columns too. Excluding the badge here
/// is what makes that a structural guarantee rather than a consequence of the
/// row happening to fit.
fn worst(slots: &[Option<(u8, Vec<Seg>)>]) -> Option<usize> {
    slots
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.as_ref().map(|(rank, _)| (i, *rank)))
        .filter(|(_, rank)| *rank > 1)
        .max_by_key(|(i, rank)| (*rank, *i))
        .map(|(i, _)| i)
}

/// **`29644` -> `29,644`.** Thousands separators, because the magnitude is the
/// whole message.
///
/// The operator's sentence was *"27k is incredible"* — a statement about a
/// number's SIZE — and `29644` and `2964` are the same shape at a glance. Rust's
/// `{}` does not group and this crate had no formatter that did; `geom::fmt_age`
/// is the only other number-to-prose helper and it is about durations.
///
/// ASCII commas and not a locale: this face has no locale and inventing one for
/// three digits would be a dependency for a comma.
fn grouped(n: u32) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let lead = digits.len() % 3;
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (i.saturating_sub(lead)) % 3 == 0 && (i >= lead) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn known_style<T>(k: &Known<T>) -> ratatui::style::Style {
    match k {
        Known::Unknown => Severity::Unknown.style(),
        Known::Known(_) => palette::ink2(),
    }
}

/// **THE ONE PLACE A DURATION ABOUT THE LINK IS SPELLED**, and it is spelled
/// against the frame's `now` rather than against anything the shell stored.
///
/// `0040` §9 draws both readings — *"disconnected for 41s · next retry in 4s"*
/// — and both are live here: the age comes off [`Link::down_since`] and the
/// countdown off a retry DEADLINE, so neither is a number that was true once.
/// The age LEADS because it is the monotonic one and it is the fact that tells
/// a blip from a dead daemon.
fn link_label(link: &Link, now: DateTime<Utc>) -> String {
    let url = &link.url;
    let down = link
        .down_for(now)
        .map(|d| format!(" · down {}", geom::fmt_age(d)))
        .unwrap_or_default();
    match &link.state {
        LinkState::Connecting { attempt } => format!("connecting #{attempt}{down} · {url}"),
        // **`connected` AND NOT `connected · ws://127.0.0.1:3651/ws`.**
        //
        // He named this fact by its one word — *"All I want to see is the #
        // unread …, and connected"* — and the endpoint is a fact about the
        // FACE'S CONFIGURATION, not about his work: it is identical on every
        // frame of every session, so it is the definition of furniture. It
        // survived this long only because the root header was too full to
        // offer it; cutting the header made room and the room immediately
        // filled with the least useful thing on the row.
        //
        // **IT STILL APPEARS WHEN THE LINK IS IN TROUBLE**, below, because
        // then it is not furniture — it is the address that is not answering,
        // and it is the first thing anyone debugging asks for.
        LinkState::Connected => "connected".to_string(),
        LinkState::Disconnected { reason, retry_at } => {
            format!(
                "disconnected ({reason}){down}{}",
                retry_phrase(*retry_at, now)
            )
        }
    }
}

/// The countdown, as a DEADLINE minus the frame's clock.
///
/// Once the deadline has passed the face says the attempt is happening rather
/// than counting into the negative or freezing on its last positive value —
/// the transport's own `Connecting` event follows within one scheduling hop
/// and replaces it.
fn retry_phrase(retry_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    match retry_at {
        // We are not retrying, or were not told when. Saying nothing is the
        // honest version; inventing a number is not.
        None => String::new(),
        Some(at) if at <= now => " · retrying now".to_string(),
        Some(at) => format!(" · retry in {}", geom::fmt_age(at - now)),
    }
}

// ── the banner ───────────────────────────────────────────────────────────────

/// How many rows the banner takes out of a body of `rows`.
///
/// A banner that would leave nothing under it is a banner about nothing, so
/// below two rows it does not appear at all. Split out from [`banner_rows`] so
/// that [`bands`] can answer the height question without building a `Line`.
fn banner_height(b: Option<&Banner>, rows: usize, w: usize) -> usize {
    match b {
        Some(b) if rows > 1 => banner_wrap(&b.text, w)
            .len()
            .min(BANNER_MAX_ROWS)
            .min(rows - 1),
        _ => 0,
    }
}

/// The banner never takes more than two rows of a twenty-four row screen. Past
/// that it stops being a standing note over the content and becomes the
/// content.
const BANNER_MAX_ROWS: usize = 2;

/// **THE BANNER'S HEIGHT IS A FUNCTION OF ITS STATIC TEXT AND THE WIDTH — AND
/// OF NOTHING ELSE.**
///
/// [`Banner::text`] is built once per projection; the live outage numbers are
/// prefixed per FRAME. So the height is measured on the projection's string
/// alone: a height measured on the composed line would grow when `down 59s`
/// became `down 1h 00m`, and the whole surface under it would step down a row
/// because a clock ticked. The prefix is fitted INTO the rows the static text
/// earned, which is the next function's job.
fn banner_wrap(text: &str, w: usize) -> Vec<geom::WrapRow> {
    geom::wrap(
        &[Seg::new(text, palette::base())],
        w.saturating_sub(GUTTER).max(1),
    )
}

/// The standing row above the content, if [`bands`] allotted one.
///
/// **THE BANNER'S LIVE NUMBERS ARE COMPOSED HERE AND NOT IN THE STORE.**
/// [`Banner::text`] is built once per PROJECTION, and a projection happens on
/// an event; a duration formatted there would be frozen at whatever the clock
/// said when the socket died. The two time-varying facts — how long it has
/// been down, and when the next attempt is due — are therefore left out of
/// that string and prefixed here, where there is a `now` per FRAME. `0040` §9
/// leads its banner with the age, and so does this.
fn banner_rows(
    b: Option<&Banner>,
    link: &Link,
    now: DateTime<Utc>,
    w: usize,
    rows: usize,
) -> Vec<Line<'static>> {
    match b {
        Some(b) if rows > 0 => {
            // **THE OUTAGE LINE'S TWO FACTS, AND WHICH ONE SURVIVES WHEN ONLY
            // ONE FITS.** `0040` §11 could not rule on this — the line did not
            // exist when it was drawn — so it is ruled here: **the AGE stays
            // and the COUNTDOWN goes.**
            //
            // The age is monotonic and is a claim about the PAST; it is the
            // fact that tells a blip from a dead daemon, which is the only
            // question the banner is there to answer. The countdown is a
            // PREDICTION, it is recomputed every frame anyway, and within
            // seconds of mattering it turns into `retrying now` — a fact the
            // next `Connecting` event states more directly than this row can.
            // Losing "when will it try again" costs him a guess; losing "how
            // long has this been broken" costs him the diagnosis.
            //
            // The REASON is content and therefore ellipsizes LAST, after the
            // countdown has already gone.
            let age = link
                .down_for(now)
                .map(|d| format!("down {}", geom::fmt_age(d)));
            let candidates: Vec<String> = match &age {
                Some(a) => vec![
                    format!("{a}{} · {}", retry_phrase(link.retry_at(), now), b.text),
                    format!("{a} · {}", b.text),
                ],
                None => vec![b.text.clone()],
            };
            let chosen = candidates
                .iter()
                .find(|c| banner_wrap(c, w).len() <= rows)
                .unwrap_or_else(|| candidates.last().expect("never empty"));
            let all = banner_wrap(chosen, w);
            let overflowed = all.len() > rows;
            let last = rows.saturating_sub(1);
            all.into_iter()
                .take(rows)
                .enumerate()
                .map(|(n, vr)| {
                    let segs = vec![
                        Seg::new(" ".repeat(GUTTER), palette::base()),
                        Seg::new(vr.text(), b.severity.style()),
                    ];
                    // It STILL did not fit — the reason is longer than the two
                    // rows a standing note may take of a narrow pane. Then, and
                    // only then, the last row ends in an ellipsis, so that a
                    // sentence which had to stop SAYS that it stopped.
                    //
                    // The mark is applied to the TEXT and not to the finished
                    // line, which was this code's first mistake: `line_from`
                    // has already padded the row out to the full width, so an
                    // ellipsis crop over the finished line sees a row that
                    // exactly fits and leaves it alone. The first measurement
                    // after that version read `; every` at 80 columns with no
                    // mark at all — the very defect the branch was added for,
                    // now wearing a fix.
                    if overflowed && n == last {
                        // APPENDED, not cropped. A wrapped row is SHORTER than
                        // the width it was wrapped to, so an ellipsis crop over
                        // it finds a row that already fits and changes nothing
                        // — which is how the first two attempts at this branch
                        // both produced `; every` with no mark on it. The row
                        // that stopped has to be given the mark; there is no
                        // crop that will discover it needs one.
                        let mut segs = segs;
                        if let Some(text) = segs.last_mut() {
                            let trimmed = text.text.trim_end().to_string();
                            text.text = trimmed;
                            text.text.push('…');
                        }
                        line_from(crop_with_ellipsis(segs, w), w)
                    } else {
                        line_from(segs, w)
                    }
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

// ── band 2a: THE ROOT — projects ─────────────────────────────────────────────

/// **THE ROOT IS ONE SCROLLING SURFACE OF BANDS** — pinned, recent, projects,
/// chrome last (`DEC-28`, `0040` §1).
///
/// The AUI's root order survives intact and **the ORDER is the design's
/// argument**: the AUI splits pinned and recent onto their own screens because
/// a linear audio medium can only speak one list at a time, and on glass all
/// three are one glance. What carries the muscle memory is the sequence, so
/// nothing here is rearranged.
///
/// **This function draws; it does not decide.** The sequence comes from
/// [`model::root_lines`], which the hit map, the drag-selection's unit mapping
/// and the shell's scroll arithmetic all read too — four readers of one layout,
/// because a second count of this surface anywhere is how a click lands a row
/// off.
///
/// The projects themselves stay ALPHABETICAL, top to bottom. `DIR-3`'s one
/// direction is about a stream of things that arrive; a project list is a
/// stable set he navigates by name. The RECENT band is newest-first for the
/// opposite reason and the doc draws it so: it is a fixed-height shortcut, not
/// a stream.
fn project_list(
    list: &ProjectList,
    now: DateTime<Utc>,
    w: usize,
    rows: usize,
) -> (Vec<Line<'static>>, Vec<RowInfo>) {
    if rows == 0 {
        return (Vec::new(), Vec::new());
    }
    let lines = list.lines();
    // **THE CROP HAPPENS BEFORE THE PAINT, AND THAT IS THE WHOLE OF `I-12`.**
    //
    // This function used to paint EVERY line of the surface and hand the lot to
    // `window_from_top`, which then threw away all but a screenful. At ten rows
    // a band that cost nothing; `BAND_EXPANDED_MAX = 500` existed precisely to
    // bound it, and its own doc said so — *"the bound is about DRAWING, not
    // about loading"*.
    //
    // *"all results"* retires that constant, so the bound moves to where it
    // always belonged. `RootLine` is a `Copy` enum and `root_lines` over five
    // thousand rows is a `Vec` of five thousand small values; `band_row` and
    // `project_row` — which crop, style and age-format every column — are what
    // costs, and they must run for DRAWN rows only. Frame cost is then
    // O(visible) and a page of five thousand costs what a page of fifty costs.
    //
    // **THE TWO INDEXES STAY ABSOLUTE.** `Unit::new(UnitKind::Content, row)` is
    // the drag selection's address into `select::content_text`, which indexes
    // `list.lines()`; and `sel` is the cursor's coordinate over SELECTABLE
    // lines. Both count from the top of the whole surface, so the window's
    // offset is added to the first and the skipped stops are counted into the
    // second. Re-basing either one to the window would be a second coordinate
    // system for the same row — `0023`'s CLASS 3, in the place where a click
    // lands on the wrong thread.
    let (start, take) = window_span(lines.len(), rows, list.from_bottom);
    let mut laid: Vec<Line<'static>> = Vec::with_capacity(rows);
    let mut infos: Vec<RowInfo> = Vec::with_capacity(rows);
    // Counting `selectable()` over the skipped prefix walks a `Copy` enum and
    // paints nothing, so it is the cheap half of this function even at five
    // thousand rows.
    let mut sel = lines[..start].iter().filter(|l| l.selectable()).count();

    for (row, l) in lines.iter().enumerate().skip(start).take(take) {
        let unit = Unit::new(UnitKind::Content, row);
        let (line, info) = match *l {
            // **A BAND HEADER IS A ROW HE STANDS ON** (`DEC-46`). It wears
            // the same two-column cursor gutter every other stop wears, so
            // `↑`/`↓` walking into it looks like walking onto any other row —
            // a header that highlighted differently would read as the cursor
            // having LEFT the list.
            //
            // The caption keeps `palette::header()`; the note after it is
            // MUTED, because it is metadata about the band and not the band's
            // name. `select::band_header_run` is the copyable text and is
            // derived from the same two pieces, so what a drag copies is what
            // was drawn.
            RootLine::Band(b) => {
                let note = list.band_note(b);
                let text = select::band_header_run(list, b);
                let info = RowInfo::new(
                    Target::RootRow(sel),
                    Some(RowText {
                        unit,
                        start: 0,
                        col0: GUTTER as u16,
                        text,
                    }),
                );
                let line = line_from(
                    vec![
                        Seg::new(if sel == list.cursor { "▸ " } else { "  " }, palette::ink()),
                        Seg::new(b.label(), palette::header()),
                        Seg::new(
                            if note.is_empty() {
                                String::new()
                            } else {
                                format!(" · {note}")
                            },
                            palette::muted(),
                        ),
                    ],
                    w,
                );
                sel += 1;
                (line, info)
            }
            // **THE `see all` FOOT** — the affordance he asked for, at the
            // foot of the band, where he is already looking when he runs out of
            // rows:
            //
            // > Maybe we have the collapsed bands have an item at the bottom
            // > saying 'show more'
            //
            // and, this round, renamed and re-aimed:
            //
            // > let's replace 'show more' with 'see all' and it takes us to a
            // > dedicated page with just that band and all results.
            //
            // It is INDENTED WITH THE ROWS, not with the header, because it is
            // a member of the band and not a second caption for it; and it is
            // muted, because it is a control rather than content.
            //
            // **IT IS NOT A BUTTON.** Its target is `Target::RootRow(sel)`,
            // exactly as every other stop's is, so a CLICK on it selects it and
            // `→` opens the page — the operator's standing ruling, and
            // `DEC-78`'s, with no exception written for it.
            RootLine::SeeAll(b) => {
                let text = list.see_all_words(b);
                let info = RowInfo::new(
                    Target::RootRow(sel),
                    Some(RowText {
                        unit,
                        start: 0,
                        col0: ROW_COL0,
                        text: text.clone(),
                    }),
                );
                let line = line_from(
                    vec![
                        Seg::new(row_head(sel == list.cursor), palette::ink()),
                        Seg::new(text, palette::muted()),
                    ],
                    w,
                );
                sel += 1;
                (line, info)
            }
            RootLine::Hint(msg) => (
                line_from(
                    vec![
                        Seg::new(" ".repeat(GUTTER), palette::base()),
                        Seg::new(msg, Severity::Unknown.style()),
                    ],
                    w,
                ),
                RowInfo::new(
                    Target::Empty,
                    Some(RowText {
                        unit,
                        start: 0,
                        col0: GUTTER as u16,
                        text: msg.to_string(),
                    }),
                ),
            ),
            RootLine::Spacer => (
                size_line(vec![], w, palette::base()),
                RowInfo::bare(Target::Empty),
            ),
            RootLine::Pinned(i) | RootLine::Recent(i) => {
                let r = match *l {
                    RootLine::Pinned(_) => &list.pinned[i],
                    _ => &list.recent[i],
                };
                let (line, title) = band_row(r, sel == list.cursor, now, w);
                let info = RowInfo::new(
                    Target::RootRow(sel),
                    Some(RowText {
                        unit,
                        start: 0,
                        // Cursor marker, then the band's indent, then the run.
                        col0: ROW_COL0,
                        text: select::band_run(r),
                    }),
                )
                // **THE TITLE IS THE DOOR ONTO THE SHEET** (§5: "click the
                // title of a thread, ANYWHERE it appears"). The rest of the row
                // keeps the plain behaviour the operator's retraction gave it:
                // a click moves the cursor and stops.
                .claim(title.0, title.1, Target::ThreadTitle(sel));
                sel += 1;
                (line, info)
            }
            RootLine::Project(i) => {
                let r = &list.rows[i];
                let (line, name) = project_row(r, sel == list.cursor, now, w);
                let info = RowInfo::new(
                    Target::RootRow(sel),
                    Some(RowText {
                        unit,
                        start: 0,
                        col0: ROW_COL0,
                        text: select::project_run(r),
                    }),
                )
                // **THE NAME IS THE DOOR ONTO THE PROJECT'S SHEET** (§2:
                // *"clicking the project's title opens its sheet"*), exactly as
                // a thread's title is the door onto its own. The rest of the
                // row keeps the plain behaviour the operator's retraction gave
                // it: a click moves the cursor and stops, `->` drills in.
                .claim(name.0, name.1, Target::ProjectName(sel));
                sel += 1;
                (line, info)
            }
            // **`+ new project` — A ROW, A FIELD, AND A SENTENCE.**
            //
            // Resting it invites a name; while he is typing it IS the field,
            // in place, with no scope and no confirm (§1: *"takes a typed name
            // right in the row, no confirm scope — read-back exists for voice,
            // not for typing"*); and when the audio interface is unreachable it
            // wears that sentence instead of the invitation, muted, at the same
            // position. It never disappears and it never silently does nothing.
            RootLine::NewProject => {
                let typing = list.new_project.is_some();
                let text = select::new_project_run(list);
                let style = match (typing, list.aui_down.is_some()) {
                    (true, _) => palette::ink(),
                    (_, true) => palette::muted(),
                    _ => palette::ink2(),
                };
                let info = RowInfo::new(
                    Target::RootRow(sel),
                    Some(RowText {
                        unit,
                        start: 0,
                        col0: ROW_COL0,
                        text: text.clone(),
                    }),
                );
                let line = line_from(
                    vec![
                        // INDENTED WITH THE BAND'S ROWS: it is the last row of
                        // the PROJECTS band, not chrome — §1 and §10 both draw
                        // it inside. `archived (N)` below is the opposite case:
                        // it belongs to no band and keeps the bare gutter, so
                        // the indent is doing real work here rather than being
                        // applied to everything that is not a header.
                        Seg::new(row_head(sel == list.cursor), palette::ink()),
                        Seg::new(text, style),
                        // The caret sits after what he typed, so the row reads
                        // as a field rather than as a label that changed.
                        Seg::new(if typing { "█" } else { "" }, palette::ink()),
                    ],
                    w,
                );
                sel += 1;
                (line, info)
            }
            RootLine::Archived => {
                let text = select::archived_run(list.archived);
                let info = RowInfo::new(
                    Target::RootRow(sel),
                    Some(RowText {
                        unit,
                        start: 0,
                        col0: 2,
                        text: text.clone(),
                    }),
                );
                let line = line_from(
                    vec![
                        Seg::new(if sel == list.cursor { "▸ " } else { "  " }, palette::ink()),
                        Seg::new(text, palette::muted()),
                    ],
                    w,
                );
                sel += 1;
                (line, info)
            }
        };
        laid.push(line);
        infos.push(info);
    }
    // The root is read from the TOP, so it is anchored there: short content
    // sits on the CEILING and is padded below, and forty projects in twenty
    // rows show the first twenty, not the last.
    while laid.len() < rows {
        laid.push(size_line(vec![], w, palette::base()));
        infos.push(RowInfo::bare(Target::Empty));
    }
    (laid, infos)
}

/// **WHICH SLICE OF A TOP-ANCHORED SURFACE IS ON SCREEN** — `(start, len)`.
///
/// Split out of [`project_list`]'s old `window_from_top` tail so the window can
/// be chosen BEFORE the rows are painted rather than after; see that function
/// for why the order is the performance property. The arithmetic is unchanged
/// and `from_bottom` is still the one scroll coordinate in the crate, so the
/// shell's viewport arithmetic did not move.
fn window_span(total: usize, rows: usize, from_bottom: usize) -> (usize, usize) {
    if total <= rows {
        return (0, total);
    }
    let from_bottom = from_bottom.min(total - rows);
    (total - rows - from_bottom, rows)
}

/// **ONE ROW OF A CROSS-PROJECT BAND** — pinned or recent.
///
/// It carries what the doc draws and nothing else: the summary he recognises
/// the thread by, the PROJECT it lives in, its live state in a word, and its
/// age. The project column is the whole difference from a depth-2 thread row,
/// where it would be the same value on every line.
///
/// No id. The abbreviated hash is on the depth-2 list because inside a project
/// two threads can share a summary; at the root the band is three rows deep and
/// *"don't show me thread or task hash names"* stands.
///
/// **NO PIN MARK EITHER**, and the doc draws none. In the pinned band it would
/// be the same fact on every row, and a column that never varies is noise by
/// this design's own argument; in the recent band it would be redundant,
/// because the pinned band holds EVERY pin and sits directly above. That buys
/// the summary the second marker column back, which `▸★Fix the viewport…` was
/// visibly spending on nothing.
/// **THE COLUMNS ONE SEGMENT OF A CROPPED HEAD ACTUALLY OCCUPIES.**
///
/// Read off the segments AFTER `crop_with_ellipsis` has had them, never
/// computed from the model — because the claim it feeds
/// ([`RowInfo::claim`](crate::hit::RowInfo::claim)) has to name the columns
/// that were DRAWN. A title cropped to fit a 60-column pane occupies fewer
/// columns than its summary is long, and a span derived from the model would
/// leave the mouse able to "click the title" in the blank space past its
/// ellipsis. That is the same class of mistake `select::thread_label` exists to
/// prevent one layer up: two derivations of what is on a row, and the second
/// one is the one the pointer believes.
fn span_of(segs: &[Seg], idx: usize) -> (u16, u16) {
    let start: usize = segs.iter().take(idx).map(|s| text_width(&s.text)).sum();
    let wide = segs.get(idx).map(|s| text_width(&s.text)).unwrap_or(0);
    (start as u16, (start + wide) as u16)
}

/// One row of the pinned or recent band. Returns the line AND the columns its
/// TITLE occupies, so the caller can claim exactly those as the sheet's door.
fn band_row(
    r: &ThreadRow,
    selected: bool,
    now: DateTime<Utc>,
    w: usize,
) -> (Line<'static>, (u16, u16)) {
    // **THE BAND'S COLLAPSE ORDER — the state word first, then nothing else.**
    //
    // The PROJECT is rank 0 and never goes: a cross-project row that has lost
    // which project it belongs to is a summary with no address, and it is on no
    // header, so the row has to carry it.
    //
    // The STATE WORD goes first because a band is a SHORTCUT, not a status
    // board: it exists to get him back to a thread he was in, and `absent` /
    // `idle` is the third-order fact on a row whose first two are which thread
    // and when. The AGE outranks it — the recent band is SORTED by recency, so
    // dropping the age would leave a list ordered by a fact it no longer shows.
    //
    // The columns also THIN when the pane does. Widths of 14 and 10 are
    // alignment for a wide terminal, and alignment that costs the summary its
    // words is decoration outranking content.
    //
    // Each thinned column keeps an EXPLICIT leading space. Thinning a column to
    // the exact width of its widest value removes the gap the padding used to
    // supply by accident, and the first measurement after this change read
    // `harness11h 36m` — "a column that runs into its neighbour is a number he
    // has to parse rather than read", which is this row's own standing note
    // about the age column, arriving one column to the left.
    let narrow = w <= geom::NARROW_WIDTH as usize;
    let (proj_w, state_w, age_w) = if narrow { (10, 7, 7) } else { (14, 10, 7) };
    let mut tail: Vec<(u8, Seg)> = vec![
        // **CROPPED TO THE COLUMN, NOT MERELY PADDED INTO IT.** `{:>10}` is a
        // MINIMUM width: a project called `miscellaneous` overran it and pushed
        // the age off that row and only that row, so the band's columns stopped
        // lining up and one row silently lost a fact its neighbours kept. A
        // column that different rows disagree about is not a column.
        (
            0,
            Seg::new(
                // `↳ «parent»` for a managed subagent: where it hangs is the
                // fact that places it, not the project it inherited.
                format!(
                    "{:>proj_w$}",
                    fit(
                        &r.parent
                            .as_ref()
                            .map(|p| format!("↳ {p}"))
                            .unwrap_or_else(|| r.project.clone()),
                        proj_w
                    )
                ),
                palette::ink2(),
            ),
        ),
        (
            2,
            match (&r.activity, r.closed) {
                (_, true) => Seg::new(format!(" {:>state_w$}", "closed"), Severity::Unknown.style()),
                // **THE PHRASE OVERRIDES THE WORD**, and it is CROPPED to a
                // fixed column so a long one cannot push the age off this row
                // and only this row.
                (Some((text, sev)), false) => {
                    let aw = state_w + 4;
                    Seg::new(format!(" {:>aw$}", fit(text, aw)), sev.style())
                }
                (None, false) => Seg::new(
                    format!(" {:>state_w$}", &r.state),
                    state_severity(&r.state, r.executing).style(),
                ),
            },
        ),
        (
            1,
            Seg::new(
                format!(
                    " {:>age_w$}",
                    match r.updated {
                        Some(t) => geom::fmt_age(now - t),
                        None => "?".into(),
                    }
                ),
                match r.updated {
                    Some(_) => palette::muted(),
                    None => Severity::Unknown.style(),
                },
            ),
        ),
    ];
    let tail_w = drop_tail_to_floor(&mut tail, w);

    let label_style = if selected {
        palette::header()
    } else {
        palette::ink2()
    };
    let (label, blank) = select::thread_label(r);
    let head = vec![
        Seg::new(row_head(selected), palette::ink()),
        Seg::new(
            label,
            if blank {
                Severity::Unknown.style()
            } else {
                label_style
            },
        ),
    ];
    let left_budget = w.saturating_sub(tail_w + 1);
    let mut segs = crop_with_ellipsis(head, left_budget);
    // The title is segment 1: marker, then the label.
    let title = span_of(&segs, 1);
    let used: usize = segs.iter().map(|s| text_width(&s.text)).sum();
    segs.push(Seg::new(
        " ".repeat(w.saturating_sub(used + tail_w)),
        palette::base(),
    ));
    segs.extend(tail.into_iter().map(|(_, s)| s));
    (line_from(segs, w), title)
}

/// A body with nothing in it: one honest sentence, padded out to the band.
///
/// The sentence is COPYABLE — it is addressed as content row 0, which has no
/// model row behind it, so [`crate::select::selected_text`] falls back to what
/// was painted. "Any part of the interface" includes the part that is telling
/// him why there is nothing here.
fn empty_state(msg: &str, rows: usize, w: usize) -> (Vec<Line<'static>>, Vec<RowInfo>) {
    let line = line_from(
        vec![
            Seg::new(" ".repeat(GUTTER), palette::base()),
            Seg::new(msg, Severity::Unknown.style()),
        ],
        w,
    );
    let mut infos = vec![RowInfo::new(
        Target::Empty,
        Some(RowText {
            unit: Unit::new(UnitKind::Content, 0),
            start: 0,
            col0: GUTTER as u16,
            text: msg.to_string(),
        }),
    )];
    infos.resize_with(rows, || RowInfo::bare(Target::Empty));
    (filled(vec![line], rows, w), infos)
}

/// **THE COLUMN THE PROJECT PATHS LINE UP IN**, measured from the name.
///
/// His ask, quoting his own screen back at himself:
///
/// > airbnb  ~/minds/airbnb …
/// > broomstick  ~/minds/broomstick …
/// > I want the directories to be aligned on each other, so they either need to
/// > be on the left or have a consistent left align towards the left (after the
/// > name of the project).
///
/// **BOTH SPELLINGS WERE OFFERED AND THIS BUILD TAKES THE SECOND** — a padded
/// column AFTER the name — for one reason: the NAME is what his hand scans by.
/// `discover_roots` sorts alphabetically precisely so that *"his hand knows
/// where a project is without reading"*, and putting the paths hard left would
/// move the alphabetical column he navigates by off the leading edge and hide
/// it behind a ragged path. The path is the DISAMBIGUATOR — how he tells two
/// projects of the same name apart — and a disambiguator belongs beside the
/// thing it disambiguates, not in front of it.
///
/// Eighteen columns fits every one of his thirty-three roots but one; a longer
/// name takes its own column back and pushes only ITS OWN path right, which is
/// the failure the `{:<18}` minimum-width spelling gives us for free. It thins
/// at narrow widths for the same reason every other column here does:
/// alignment that costs the path its words is decoration outranking content.
pub(crate) fn name_col(name: &str, w: usize) -> String {
    let n: usize = if w <= geom::NARROW_WIDTH as usize {
        12
    } else {
        18
    };
    // **THE PADDING ONLY** — the name is its own segment, so that the click
    // span the caller reads off segment 1 is the NAME and not the whole
    // aligned gap. A name folded in here widened the project sheet's door
    // across the blank, and `a_list_rows_selectable_run_is_exactly_what_the_
    // row_paints` is what said so.
    //
    // A name that overruns the column still gets ONE separating space, or it
    // runs into the path exactly as `harness11h 36m` once ran into its age;
    // and it pushes only ITS OWN path right, which is the right failure.
    " ".repeat((n + 1).saturating_sub(text_width(name)).max(1))
}

/// One project row, and **the columns its NAME occupies** — the door onto the
/// project's sheet (§5: *"a project's title opens the same shape"*).
///
/// The span is read off the segments AFTER cropping, exactly as `band_row`
/// reads a thread title's: a name cropped to fit a 60-column pane occupies
/// fewer columns than the name is long, and a span derived from the model would
/// leave the mouse able to "click the name" in the blank past its ellipsis.
fn project_row(
    r: &ProjectRow,
    selected: bool,
    now: DateTime<Utc>,
    w: usize,
) -> (Line<'static>, (u16, u16)) {
    // **THE PROJECT ROW'S COLLAPSE ORDER.** The NAME is the content and the
    // PATH is how he tells two projects of the same name apart, so both live in
    // the head. Out here: the thread count is rank 0 — it is what a project row
    // is FOR — and the rest goes by how much it says. The pin count first (it
    // is repeated on the pinned band directly above), then the active count
    // (repeated in the header's queue badge), then the age.
    let mut tail: Vec<(u8, Seg)> = vec![
        // **THE UNREAD COUNT, WHICH HE ASKED FOR BY NAME**: *"I want to see {N}
        // thr, the time since last event, which is what I'm guessing that is
        // now, and the num unread."*
        //
        // **NEVER A BARE ZERO** (honesty rule 1): `Known::Unknown` is `?` and
        // a real zero is BLANK, not `0 unread` — a column of zeros is noise,
        // and a zero we were never told is a lie. That is the same rule the
        // `act` column it replaced was written under.
        (
            2,
            Seg::new(
                match r.unread {
                    Known::Known(0) => "          ".to_string(),
                    ref u => format!("{:>3} unread", u.label()),
                },
                known_style(&r.unread),
            ),
        ),
        // **`act` AND `★` LEFT THIS ROW**, and it is his list that removed
        // them: he named three facts and neither was among them. The pin count
        // was already the pinned band directly above, and the active count is
        // in the header he also cut — so nothing he named has lost its only
        // home. `★` is preserved on the depth-2 thread list, where a pin is
        // marked per row.
        // **`{N} thr` — AND `no index` IS GONE FROM IT.**
        //
        // > Don't tell me no index, just recompute the indexes if they aren't
        // > detected.
        //
        // There is nothing to recompute: a scout measured all twenty of his
        // indexless roots and every one has zero thread logs, zero daemon
        // threads and zero `aui_project` rows. They are directories he has
        // never held a thread in, so `0 thr` is not a euphemism for "I don't
        // know", it is the answer.
        //
        // **THE CORRUPT CASE KEPT ITS DISTINCTION AND LOST ITS SENTENCE.** An
        // index that is present and unparseable renders `? thr` — the same `?`
        // every other `Known` column on this face uses, so there is no second
        // spelling of "unknown" to learn — because `0 thr` there would be a
        // project that looks empty and is not. See `index::IndexState` for why
        // that distinction is worth a type: `aui-nav` would overwrite the file.
        (
            0,
            Seg::new(
                format!("{:>3} thr", r.threads.label()),
                match r.threads {
                    Known::Known(_) => palette::muted(),
                    Known::Unknown => Severity::Unknown.style(),
                },
            ),
        ),
        (
            1,
            Seg::new(
                // NINE columns, not eight: `137d 12h` is eight wide and the live
                // run put it flush against `1 thr` with no gap. A column that runs
                // into its neighbour is a number he has to parse rather than read.
                format!(
                    "{:>9}",
                    match r.last {
                        Some(t) => geom::fmt_age(now - t),
                        None => "?".into(),
                    }
                ),
                match r.last {
                    Some(_) => palette::muted(),
                    None => Severity::Unknown.style(),
                },
            ),
        ),
    ];
    let tail_w = drop_tail_to_floor(&mut tail, w);
    let head = vec![
        Seg::new(row_head(selected), palette::ink()),
        Seg::new(
            r.name.clone(),
            if selected {
                palette::header()
            } else {
                palette::ink2()
            },
        ),
        // **THE PADDING IS ITS OWN SEGMENT, and that is load-bearing.** Folding
        // it into the name made `span_of(&segs, 1)` — the claim that opens the
        // project's sheet — cover the whole aligned gap, so a click in the
        // blank between a short name and its path opened a sheet he never
        // aimed at. Caught by `a_list_rows_selectable_run_is_exactly_what_the_
        // row_paints`, which asserts the claimed span IS the name and is the
        // reason the alignment did not ship with a silently widened door.
        Seg::new(name_col(&r.name, w), palette::base()),
        Seg::new(r.path.clone(), palette::muted()),
    ];
    let left_budget = w.saturating_sub(tail_w + 1);
    let mut segs = crop_with_ellipsis(head, left_budget);
    // The name is segment 1: marker, then the name.
    let name = span_of(&segs, 1);
    let used: usize = segs.iter().map(|s| text_width(&s.text)).sum();
    segs.push(Seg::new(
        " ".repeat(w.saturating_sub(used + tail_w)),
        palette::base(),
    ));
    segs.extend(tail.into_iter().map(|(_, s)| s));
    (line_from(segs, w), name)
}

// ── band 2b: one project's threads ───────────────────────────────────────────

/// One project's threads, newest at the BOTTOM.
///
/// **One direction for the whole face.** `DIR-3` is written about the
/// transcript, and it is obeyed here too rather than reasoned around: the index
/// is sorted ascending by `last_turn_at`, so the thread he last touched sits on
/// the floor of the band where the cursor starts and where his hands already
/// are.
///
/// **Pinned rows are MARKED, not hoisted.** A pin moved to the top would break
/// the one direction and would put the row he pinned somewhere other than where
/// he remembers it; the `★` in the marker column costs no width, because the
/// marker column is two wide either way.
fn thread_list(
    list: &ThreadList,
    now: DateTime<Utc>,
    w: usize,
    rows: usize,
) -> (Vec<Line<'static>>, Vec<RowInfo>) {
    if rows == 0 {
        return (Vec::new(), Vec::new());
    }
    if list.rows.is_empty() {
        return empty_state(
            match list.window {
                // Read, and the answer was nothing. Different from there being
                // no index, and it says which.
                Some(_) => "this project's index holds no threads",
                None => "this project has no .mind/threads.json",
            },
            rows,
            w,
        );
    }

    let laid: Vec<(Line<'static>, (u16, u16))> = list
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| thread_row(r, i == list.cursor, now, w))
        .collect();
    let titles: Vec<(u16, u16)> = laid.iter().map(|(_, t)| *t).collect();
    let laid: Vec<Line<'static>> = laid.into_iter().map(|(l, _)| l).collect();
    let infos: Vec<RowInfo> = list
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            RowInfo::new(
                Target::ThreadRow(i),
                Some(RowText {
                    unit: Unit::new(UnitKind::Content, i),
                    start: 0,
                    // Two columns of cursor+pin marker, then the run.
                    col0: 2,
                    text: select::thread_run(r),
                }),
            )
            // **THE SAME DOOR, ON THE OTHER SURFACE.** §5's "anywhere it
            // appears" is one behaviour, so it is one claim written twice
            // against two layouts rather than two behaviours.
            .claim(titles[i].0, titles[i].1, Target::ThreadTitle(i))
        })
        .collect();
    window(laid, infos, rows, list.from_bottom, w, None)
}

/// Pad a short body out to the band it was given.
fn filled(mut v: Vec<Line<'static>>, rows: usize, w: usize) -> Vec<Line<'static>> {
    while v.len() < rows {
        v.push(size_line(vec![], w, palette::base()));
    }
    v.truncate(rows);
    v
}

/// The fewest columns a list row's LEFT side may be squeezed to before the
/// right-hand facts start dropping instead.
///
/// **`▸ ` + twenty columns of the thread's NAME**, and the derivation changed
/// in `0076` when the hex id left the row.
///
/// It was `2 + 8 + 1 + 12`: two marker columns, an eight-character id, a
/// space, and twelve columns of path — twelve being the width that keeps
/// `~/.aui/myth…` apart from `~/code/harn…`. The id and its space are gone
/// (`DIR-16`), so nine of those columns were reserved for something no longer
/// drawn, and the remaining twelve were sized for a PATH.
///
/// What the row shows now is `select::thread_label` — the name, else
/// `untitled`, and never the cwd (TUI-P1a) — and a name needs more than a path prefix,
/// because a path is distinguished by its first segments and a sentence is
/// not. Twenty columns is `renegotiation letter…`: enough to be a phrase he
/// recognises rather than a stem. It is still two columns LESS than the row
/// reserved before, so nothing on a narrow row gets tighter than it was — the
/// floor buys content now instead of buying a uuid.
const LIST_LEFT_FLOOR: usize = 2 + 20;

/// **THE COLLAPSE ORDER, IN ONE PLACE FOR BOTH LISTS.** Drop the right-hand
/// facts from the right — age, then unread, then the badge — until the left
/// side has [`LIST_LEFT_FLOOR`] back. Returns the surviving tail's width.
///
/// It is a function rather than a loop written twice because the root menu
/// gained a list and copied it, and the copy went straight into `0023`
/// `defect_classes` CLASS 3: the chat suite's forced failure for this rule
/// matched a needle that now appeared twice, mutated the first copy, and
/// reported the mutation as UNNOTICED. A proof that silently points at the
/// wrong code is worse than no proof, and one function is what stops it.
///
/// Rank 0 is never dropped: for a thread that is its state, and a list of
/// threads whose states are invisible is a list of uuids; for a band row it is
/// the PROJECT, and a cross-project row that has lost which project it belongs
/// to is a summary with no address.
///
/// **THE RANK IS CARRIED, NOT INFERRED FROM POSITION.** It used to pop from the
/// end of the vector, which forced the drop order to equal the DISPLAY order
/// and made the two impossible to disagree with each other — so on a band row
/// the AGE, drawn last, was dropped first, even though a band is a recency
/// shortcut and the age is the fact it is sorted by. Display order and collapse
/// order are different questions and now have different answers.
fn drop_tail_to_floor(tail: &mut Vec<(u8, Seg)>, w: usize) -> usize {
    let width_of = |t: &Vec<(u8, Seg)>| t.iter().map(|(_, s)| text_width(&s.text)).sum::<usize>();
    let mut tail_w = width_of(tail);
    while w.saturating_sub(tail_w + 1) < content_floor(w) {
        // The LAST of the highest rank, so equal ranks go right to left.
        let Some(i) = tail
            .iter()
            .enumerate()
            .filter(|(_, (rank, _))| *rank > 0)
            .max_by_key(|(i, (rank, _))| (*rank, *i))
            .map(|(i, _)| i)
        else {
            break;
        };
        let (_, gone) = tail.remove(i);
        tail_w -= text_width(&gone.text);
    }
    tail_w
}

/// **§11's SECOND BREAKPOINT.** `0040` draws the collapse at two widths and
/// names them both; 80 is [`geom::NARROW_WIDTH`] and this is the other one.
pub const COLLAPSE_WIDTH: u16 = 60;

/// **HOW MUCH OF A LIST ROW BELONGS TO ITS CONTENT**, as a function of the room
/// there is — and it GROWS as the pane narrows, which is the whole of §11's
/// law expressed as arithmetic.
///
/// A fixed floor was the defect. [`LIST_LEFT_FLOOR`] is 23 columns, and 23 is a
/// generous share of a 150-column pane and a derisory one of a 60-column pane —
/// so the aligned right-hand columns, sized for a wide terminal, kept every
/// column they had while the summary paid for all of it. MEASURED at 60 before
/// this change: a band row spent 33 columns on `harness  absent  11h 20m` and
/// left 24 for the thread's name. That is §11 exactly inverted — metadata
/// surviving at content's expense — and it is invisible unless you look at the
/// frame, because each column is individually reasonable.
///
/// So the floor is a SHARE: half the row at or below 80 columns, two thirds at
/// or below 60. Never less than [`LIST_LEFT_FLOOR`], which remains the absolute
/// minimum a row needs to say which thread it is.
/// A field cropped to EXACTLY `cap` columns, marked when it had to give
/// something up. Used for the aligned columns of a list row, where a value
/// wider than its column does not widen the column — it breaks it.
fn fit(text: &str, cap: usize) -> String {
    if text_width(text) <= cap {
        return text.to_string();
    }
    crop_with_ellipsis(vec![Seg::new(text, palette::base())], cap)
        .into_iter()
        .map(|s| s.text)
        .collect()
}

fn content_floor(w: usize) -> usize {
    let share = if w <= COLLAPSE_WIDTH as usize {
        w * 2 / 3
    } else if w <= geom::NARROW_WIDTH as usize {
        w / 2
    } else {
        0
    };
    share.max(LIST_LEFT_FLOOR)
}

/// One row of a project's thread list. Returns the line AND the columns its
/// TITLE occupies — see [`band_row`].
fn thread_row(
    r: &ThreadRow,
    selected: bool,
    now: DateTime<Utc>,
    w: usize,
) -> (Line<'static>, (u16, u16)) {
    // THE RIGHT-HAND FACTS, in DROP ORDER — last dropped first. They are the
    // metadata, so they are what goes when the row is narrow; the cwd is the
    // only thing on the row he can recognise a thread by and it is never what
    // collapses first. `0023`'s collapse order, applied to a list: decoration,
    // then metadata, never content.
    // **THE COLLAPSE ORDER, BY RANK AND NOT BY POSITION.** The STATE is rank 0
    // and never goes — a list of threads whose states are invisible is a list
    // of uuids. Then, in the order they leave: the unread dot (the header
    // already carries an unread count), the queue badge (the header carries
    // that one too, and `DIR-6`'s "at all times" is satisfied THERE at every
    // width by construction), and the AGE LAST, because a list sorted by
    // recency that no longer shows recency is a list ordered by a secret.
    //
    // It used to be dropped right-to-left from the vector, which took the age
    // FIRST, and the columns never thinned at all.
    //
    // **EACH COLUMN CARRIES ITS OWN LEADING SEPARATOR**, which is how
    // `band_row` twenty lines up has always written them and how this row did
    // not. It cost nothing while the badge and the unread dot sat between the
    // state and the age — both of those DO lead with spaces — and it showed
    // the moment they were dropped. MEASURED on his real index at 60 columns
    // in `0076`: `scheduling three one-on-ones    idle129d 00h`. The age is
    // right-aligned into eight columns, so an age that FILLS eight has no
    // padding left to act as a gap, and a run of his threads are that old.
    //
    // The golden could not have caught it: its fixture ages are `1h 18m` and
    // `2m`, which pad. That is the same lesson this round learned about the
    // hex id — a fixture that cannot produce the defect is not a guard against
    // it — so `the_thread_list_collapses_at_eighty_and_sixty` is asserted at
    // the two widths where the neighbouring columns are gone.
    let mut tail: Vec<(u8, Seg)> = vec![
        (
            0,
            match (&r.activity, r.closed) {
                (_, true) => Seg::new(format!(" {:>8}", "closed"), Severity::Unknown.style()),
                (Some((text, sev)), false) => Seg::new(format!(" {:>12}", fit(text, 12)), sev.style()),
                (None, false) => Seg::new(
                    format!(" {:>8}", &r.state),
                    state_severity(&r.state, r.executing).style(),
                ),
            },
        ),
        (
            2,
            Seg::new(
                format!("  ▲{:<3}", r.queued.label()),
                known_style(&r.queued),
            ),
        ),
        (
            3,
            Seg::new(
                match r.unread {
                    Known::Known(0) => "      ".to_string(),
                    ref u => format!(" •{:<4}", u.label()),
                },
                known_style(&r.unread),
            ),
        ),
        (
            1,
            Seg::new(
                format!(
                    " {:>8}",
                    match r.updated {
                        Some(t) => geom::fmt_age(now - t),
                        // Never a zero and never blank: an age we were not given is
                        // an age we do not know.
                        None => "?".into(),
                    }
                ),
                match r.updated {
                    Some(_) => palette::muted(),
                    None => Severity::Unknown.style(),
                },
            ),
        ),
    ];
    let tail_w = drop_tail_to_floor(&mut tail, w);

    // TWO COLUMNS EITHER WAY: cursor then pin. A pinned row is marked where it
    // stands rather than hoisted, and the mark costs the row no width at all.
    let marker = match (selected, r.pinned) {
        (true, true) => "▸★",
        (true, false) => "▸ ",
        (false, true) => " ★",
        (false, false) => "  ",
    };
    // **THE SUMMARY IS WHAT HE READS.** Inside a project every thread shares a
    // cwd, so the cwd cannot tell two of them apart; the index's own drifting
    // summary is the line the AUI speaks and it is the line here. The cwd is
    // the fallback for a header that has no summary yet, and it is still
    // rendered VERBATIM when it appears.
    //
    // The CHOICE lives in `select::thread_label` and not here, because the
    // drag-selection has to know what text this row is showing in order to map
    // a column to a character of it. Two copies of this three-way fallback
    // would be two answers to "what is on that row", and the second one is the
    // one that reaches his clipboard.
    //
    // **AND THE HEX ID IS NOT ON THIS ROW.** It used to lead every row, ahead
    // of the label and unconditionally — `0073` counted it on 38 of 38 rows of
    // his real face, beside perfectly good summaries, which is what earned his
    // word *"raw"*. `DIR-16` is his own directive and names it: *"don't show me
    // thread or task hash names. not useful to me."* The root's bands already
    // drew the row this way, twenty lines from here (`band_row`); the list is
    // the surface the `0051` sweep did not reach.
    let (label, blank) = select::thread_label(r);
    let head = vec![
        Seg::new(
            marker,
            if r.pinned {
                palette::amber()
            } else {
                palette::ink()
            },
        ),
        Seg::new(
            label,
            if blank {
                Severity::Unknown.style()
            } else if selected {
                palette::ink()
            } else {
                palette::muted()
            },
        ),
    ];
    let left_budget = w.saturating_sub(tail_w + 1);
    let mut segs = crop_with_ellipsis(head, left_budget);
    // The title is segment 1: the marker, then the label. It was segment 3
    // when an id and a space stood between them.
    let title = span_of(&segs, 1);
    let used: usize = segs.iter().map(|s| text_width(&s.text)).sum();
    segs.push(Seg::new(
        " ".repeat(w.saturating_sub(used + tail_w)),
        palette::base(),
    ));
    segs.extend(tail.into_iter().map(|(_, s)| s));
    (line_from(segs, w), title)
}

/// The daemon's own state vocabulary, mapped once.
///
/// **`executing` is not a colour of its own.** It is the boolean the daemon
/// prefers over `state`, and a thread that is running is a STEADY state — green
/// is for momentary confirmations only, so a busy thread reads calm and bright,
/// never green (`palette`).
fn state_severity(state: &str, executing: bool) -> Severity {
    match state {
        "retrying" | "parked" => Severity::Warn,
        _ if executing => Severity::Calm,
        "active" | "queued" => Severity::Calm,
        "idle" => Severity::Unknown,
        // A state word this build has never heard of is not a failure and is
        // not fine either: the daemon may add one, and saying "unknown" about
        // it is the true thing.
        _ => Severity::Unknown,
    }
}

// ── bands 2 + 3: the transcript, with the event lane interleaved ─────────────

fn transcript(
    vs: &ViewState,
    now: DateTime<Utc>,
    w: usize,
    rows: usize,
) -> (Vec<Line<'static>>, Vec<RowInfo>) {
    if rows == 0 {
        return (Vec::new(), Vec::new());
    }
    if vs.blocks.is_empty() {
        // **THE KEY IS ASKED FOR, NOT SPELLED.** `keys::spelling` reads the
        // same table the `?` overlay and the footer read, so this sentence
        // cannot come to name a key the table has stopped binding — which is
        // exactly what six hand-written `alt-` literals in this crate had
        // already done before the ctrl migration touched them.
        let said = vs.stdout_only.then(|| {
            crate::keys::spelling(
                crate::Intent::StdoutOnly,
                Mode::Thread(Region::Composer),
                vs.caps,
            )
            .map(|k| format!("nothing was said here yet — {k} brings everything back"))
            .unwrap_or_else(|| "nothing was said here yet".to_string())
        });
        return empty_state(
            said.as_deref().unwrap_or("nothing here yet"),
            rows,
            w,
        );
    }

    // **THE FOLLOW-MODE ANCHOR, and it is the whole of the promise.**
    //
    // While he is scrolled up, blocks that arrived AFTER he scrolled are not
    // laid out at all. `from_bottom` is a distance from the bottom of the
    // CONTENT, so with the new blocks included the window would slide down by
    // however many rows they took — which is precisely the yank this rule
    // forbids, and a golden test caught it doing exactly that. Excluding them
    // pins the window to content that is not moving; the `↓ N new` chip is
    // what tells him the bottom grew, and `G` re-arms follow and brings the lot
    // into view in ONE deliberate jump he asked for.
    let held = if vs.viewport.follow {
        &vs.blocks[..]
    } else {
        &vs.blocks[..vs.blocks.len().saturating_sub(vs.viewport.new_since)]
    };
    // **ONLY THE BLOCKS THE VIEWPORT CAN REACH** (THREAD-LOAD-LATENCY F3).
    // The window shows `rows` rows ending `from_bottom` rows above the bottom,
    // so a suffix of blocks whose layout is at least that tall lays out
    // exactly the rows the whole slice would put there. Grown by doubling
    // from the bottom; scrolled all the way up, it is the whole slice, as
    // before.
    let need = vs.viewport.from_bottom.saturating_add(rows).saturating_add(1);
    let mut take = 32usize.min(held.len());
    let mut from_bottom = vs.viewport.from_bottom;
    let (start, (laid, spans_of, texts, marks)) = loop {
        let start = held.len() - take;
        if take > SUFFIX_MAX_BLOCKS && start > 0 {
            // **DEEP IN THE THREAD: LAY OUT ONLY THE BLOCKS ON SCREEN**
            // (THREAD-LOAD-LATENCY F6). Every block's HEIGHT is remembered by
            // content, so where the viewport falls is arithmetic, and only the
            // blocks it touches are materialised.
            let (k0, k1, fb) = visible_blocks(held, vs, now, w, rows, vs.viewport.from_bottom);
            from_bottom = fb;
            break (k0, lay_out_from(&held[..=k1], k0, vs, now, w));
        }
        let l = lay_out_from(held, start, vs, now, w);
        if start == 0 || l.0.len() >= need {
            break (start, l);
        }
        take = (take * 2).min(held.len());
    };

    // **ONE ROW-INFO PER LAID ROW, FROM THE LAYOUT'S OWN SPANS.** `lay_out`
    // already returns `(first_row, row_count)` per block — its doc says
    // "Geometry has one owner and this is it" — so a click resolves to a block
    // through the arithmetic that drew it, not through a second guess at how
    // tall a message is. The same is true one level finer for a drag: the
    // per-row text ranges come out of the WRAP that produced the rows.
    let mut infos: Vec<RowInfo> = texts
        .into_iter()
        .map(|t| RowInfo::new(Target::Empty, t))
        .collect();
    for (j, (first, len)) in spans_of.iter().enumerate() {
        let i = start + j;
        for info in infos.iter_mut().skip(*first).take(*len) {
            info.target = Target::Block(i);
        }
        // **THE EXPANDER CLAIMS THE TWO GUTTER COLUMNS OF THE BLOCK'S LAST
        // ROW** — the same two `lay_out` painted the caret into, read from the
        // same `(first, len)` span that drew them. A second arithmetic for
        // "where is the affordance" is exactly the class `0023` `defect_classes`
        // CLASS 3 names, and here it would mean a button one column away from
        // the glyph that advertises it.
        //
        // The LAST row and not the first: on a message that is the row carrying
        // the `…` he is looking at, and on a one-row lane entry the two are the
        // same row anyway.
        if *len > 0 {
            if let Some(at) = marks.get(j).copied().flatten() {
                if let Some(info) = infos.get_mut(first + len - 1) {
                    info.spans.push((at, at + GUTTER as u16, Target::Expander(i)));
                }
            }
        }
    }

    let chip = (!vs.viewport.follow && vs.viewport.new_since > 0)
        .then(|| format!("  ↓ {} new — click here, or end", vs.viewport.new_since));
    window(laid, infos, rows, from_bottom, w, chip)
}

/// Past this many blocks the bottom-up suffix layout gives way to the
/// height-indexed one ([`visible_blocks`]).
const SUFFIX_MAX_BLOCKS: usize = 256;

/// **WHICH BLOCKS A VIEWPORT `from_bottom` ROWS UP TOUCHES**, from remembered
/// block heights: `(first, last, from_bottom within blocks[first..=last])`.
/// The layout of `blocks[..=last]` from `first`, windowed at that
/// `from_bottom`, is row for row what the whole layout windowed at the
/// original one shows (proved by `the_bounded_transcript_draws_...`).
fn visible_blocks(
    held: &[Block],
    vs: &ViewState,
    now: DateTime<Utc>,
    w: usize,
    rows: usize,
    from_bottom: usize,
) -> (usize, usize, usize) {
    let heights: Vec<usize> = (0..held.len()).map(|i| block_height(held, i, vs, now, w)).collect();
    let total: usize = heights.iter().sum();
    if total <= rows {
        return (0, held.len() - 1, from_bottom);
    }
    let fb = from_bottom.min(total - rows);
    let top = total - rows - fb; // first visible row
    let bottom = top + rows; // one past the last
    let mut at = 0usize;
    let (mut k0, mut k1, mut k0_at) = (0usize, held.len() - 1, 0usize);
    let mut found0 = false;
    for (i, h) in heights.iter().enumerate() {
        if !found0 && at + h > top {
            k0 = i;
            k0_at = at;
            found0 = true;
        }
        if at + h >= bottom {
            k1 = i;
            break;
        }
        at += h;
    }
    let end_at: usize = k0_at + heights[k0..=k1].iter().sum::<usize>();
    (k0, k1, end_at - bottom)
}

/// One block's row count in a whole layout, remembered by everything it
/// depends on: the block's own content, the block before it (which decides
/// whether a run header and a separator are drawn), and the width. The time
/// is not in the key: it changes what a header SAYS, never how many rows it
/// takes.
fn block_height(held: &[Block], i: usize, vs: &ViewState, now: DateTime<Utc>, w: usize) -> usize {
    use std::hash::{Hash, Hasher};
    let mut hsh = std::collections::hash_map::DefaultHasher::new();
    block_key(&held[i], &mut hsh);
    match i.checked_sub(1).map(|p| &held[p]) {
        Some(Block::Message { sender, .. }) => (1u8, *sender == Sender::User).hash(&mut hsh),
        Some(_) => 2u8.hash(&mut hsh),
        None => 3u8.hash(&mut hsh),
    }
    w.hash(&mut hsh);
    vs.tz_offset_minutes.hash(&mut hsh);
    let key = hsh.finish();
    if let Some(h) = HEIGHTS.with(|c| c.borrow().0.get(&key).copied()) {
        return h;
    }
    if let Some(h) = HEIGHTS.with(|c| c.borrow().1.get(&key).copied()) {
        HEIGHTS.with(|c| c.borrow_mut().0.insert(key, h));
        return h;
    }
    let h = lay_out_from(&held[..=i], i, vs, now, w).0.len();
    HEIGHTS.with(|c| {
        let mut c = c.borrow_mut();
        if c.0.len() >= HEIGHTS_GEN {
            let young = std::mem::take(&mut c.0);
            c.1 = young;
        }
        c.0.insert(key, h);
    });
    h
}

fn block_key(b: &Block, hsh: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    match b {
        Block::Message { id, sender, text, note, meta, expandable, open, .. } => {
            0u8.hash(hsh);
            (*sender == Sender::User).hash(hsh);
            text.hash(hsh);
            note.hash(hsh);
            meta.hash(hsh);
            expandable.hash(hsh);
            open.hash(hsh);
            id.message_id.hash(hsh);
        }
        Block::Lane { glyph, severity, text, count, expandable, open, affordance, .. } => {
            1u8.hash(hsh);
            glyph.hash(hsh);
            format!("{severity:?}").hash(hsh);
            text.hash(hsh);
            count.hash(hsh);
            expandable.hash(hsh);
            open.hash(hsh);
            affordance.hash(hsh);
        }
        Block::Rule { text } => {
            2u8.hash(hsh);
            text.hash(hsh);
        }
    }
}

const HEIGHTS_GEN: usize = 16_384;

thread_local! {
    static HEIGHTS: std::cell::RefCell<(
        std::collections::HashMap<u64, usize>,
        std::collections::HashMap<u64, usize>,
    )> = std::cell::RefCell::new(Default::default());
}

/// **WHERE TO PUT THE VIEWPORT SO THAT HIS PREVIOUS (OR NEXT) MESSAGE HEADS THE
/// BAND** — the `from_bottom` for `opt-↑` / `opt-↓` in the chat.
///
/// *"let's make opt-{up,down} jump to the next/previous user message."*
/// Navigation by AUTHORSHIP, so the thing it walks is `Sender::User` boxes and
/// not rows.
///
/// **IT LIVES HERE BECAUSE THE ROW OFFSETS DO.** `lay_out` already returns
/// `(first_row, row_count)` per block — "geometry has one owner and this is
/// it" — so the shell asks this rather than re-deriving how tall a message is,
/// which is `0023`'s CLASS 3 defect by construction. The shell sets
/// `from_bottom` from the answer and the renderer clamps it, exactly as every
/// other scroll does.
///
/// `None` means there is no message of his in that direction. Backwards that
/// means do nothing; FORWARDS it means the caller is past his last message,
/// and the caller's answer to that is [`crate::Intent::Bottom`] — the live
/// state — which is a decision about what the key means and therefore not this
/// function's to make.
pub fn jump_user(
    vs: &ViewState,
    now: DateTime<Utc>,
    w: usize,
    rows: usize,
    forward: bool,
) -> Option<usize> {
    if rows == 0 || vs.blocks.is_empty() {
        return None;
    }
    // The SAME held slice the transcript lays out — while follow is off the
    // arrivals are not laid out at all, and a jump computed over rows that are
    // not on screen would land somewhere he cannot see.
    let held = if vs.viewport.follow {
        &vs.blocks[..]
    } else {
        &vs.blocks[..vs.blocks.len().saturating_sub(vs.viewport.new_since)]
    };
    let (laid, spans_of, _, _) = lay_out(held, vs, now, w);
    let total = laid.len();
    if total <= rows {
        // Everything held is on screen: there is nowhere to jump TO.
        return None;
    }
    let max_from_bottom = total - rows;
    // The row at the top of the band right now, in the same arithmetic
    // `window` uses to put it there.
    let here = total - rows - vs.viewport.from_bottom.min(max_from_bottom);
    let mine = held
        .iter()
        .enumerate()
        .filter(|(_, b)| matches!(b, Block::Message { sender: Sender::User, .. }))
        .filter_map(|(i, _)| spans_of.get(i).map(|(first, _)| *first));
    let target = if forward {
        mine.filter(|first| *first > here).min()
    } else {
        mine.filter(|first| *first < here).max()
    }?;
    let want = max_from_bottom.saturating_sub(target);
    let now = vs.viewport.from_bottom.min(max_from_bottom);
    // **A JUMP THAT CANNOT MOVE IS NOT A JUMP.** The last few of his messages
    // can sit BELOW the top of the band and still be unable to reach it — the
    // transcript is bottom-anchored, so there is nothing under them to scroll
    // up into. Without this, `opt-↓` on those rows would return the viewport
    // it is already at, forever, and never hand the key over to the second act
    // he asked for (*"opt-down to the bottom should go to the live state"*).
    if (forward && want >= now) || (!forward && want <= now) {
        return None;
    }
    Some(want)
}

/// Place a laid-out row sequence into `rows`, newest at the BOTTOM, with an
/// optional chip pinned to the bottom edge — and carry each row's [`Target`]
/// through the same arithmetic, so a click cannot land on a row the window
/// moved.
fn window(
    laid: Vec<Line<'static>>,
    infos: Vec<RowInfo>,
    rows: usize,
    from_bottom: usize,
    w: usize,
    chip: Option<String>,
) -> (Vec<Line<'static>>, Vec<RowInfo>) {
    debug_assert_eq!(laid.len(), infos.len());
    let total = laid.len();
    let (mut view, mut tview): (Vec<Line<'static>>, Vec<RowInfo>) = if total <= rows {
        // Short content sits on the FLOOR of the band, not at its ceiling.
        let pad = rows - total;
        let mut v: Vec<Line<'static>> = (0..pad)
            .map(|_| size_line(vec![], w, palette::base()))
            .collect();
        let mut t: Vec<RowInfo> = (0..pad).map(|_| RowInfo::bare(Target::Empty)).collect();
        v.extend(laid);
        t.extend(infos);
        (v, t)
    } else {
        let max_from_bottom = total - rows;
        let from_bottom = from_bottom.min(max_from_bottom);
        let start = total - rows - from_bottom;
        (
            laid[start..start + rows].to_vec(),
            infos[start..start + rows].to_vec(),
        )
    };

    // The `↓ N new` chip: pinned to the viewport's bottom edge while follow is
    // off. Arrivals never yank the viewport, so this is the only thing that
    // tells him the bottom moved — and it is now the click target that re-arms
    // follow, which is how `G` gives up its letter without giving up its job.
    if let Some(text) = chip {
        if rows > 0 {
            view[rows - 1] = line_from(vec![Seg::new(text.clone(), palette::amber())], w);
            tview[rows - 1] = RowInfo::new(
                Target::NewChip,
                Some(RowText {
                    unit: Unit::new(UnitKind::Chip, 0),
                    start: 0,
                    col0: 0,
                    text,
                }),
            );
        }
    }
    (view, tview)
}

// **`keep_visible` IS GONE.** It scrolled the transcript to whatever block
// Select mode had picked, and Select mode is gone (`keys`, `0041`). The drag
// selection does not need it and must not have it: a drag scrolls because the
// POINTER reached the edge of the band, which the shell decides from the
// gesture, and a renderer that also moved the window would be a second thing
// scrolling during the one gesture where he is watching the rows go past.

/// Blocks -> rows, in ledger order. Every row is already exactly `w` wide.
///
/// Returns three things from ONE pass, and that is the point of the function:
///
/// * the rows;
/// * per block, `(first_row, row_count)` — so a click resolves to a block
///   through the arithmetic that drew it;
/// * per ROW, which characters of which block it paints and at what column —
///   so a DRAG resolves to characters through the same arithmetic. Geometry
///   has one owner and this is it, at both granularities.
///
/// The per-row ranges come straight out of [`geom::wrap`], which is the only
/// thing that knows where it broke the paragraph. Re-deriving them from the
/// painted rows would be a second wrap, and the two would disagree the first
/// time a message contained a run of spaces — silently, because the screen
/// would look identical and only the clipboard would be wrong.
type Laid = (
    Vec<Line<'static>>,
    Vec<(usize, usize)>,
    Vec<Option<RowText>>,
    // The column `gutter_mark` painted each block's expander at, when it
    // painted one. **It is not always column zero any more**: an info blob is
    // centred, so its mark sits immediately left of the blob rather than at the
    // far edge of the screen, and a button the mouse falls through is exactly
    // what a second guess at this arithmetic would produce.
    Vec<Option<u16>>,
);

/// **THE EXPANDER, AS TWO PAINTED COLUMNS** — the same two-column `▸ ` the
/// project list already spends on its cursor, so the glyph vocabulary does not
/// grow.
///
/// It exists because of the operator's retraction: the BODY of a transcript row
/// stopped activating on a click, and *"expanders still activate on click"* is
/// only true of an expander he can SEE and can aim at. `0051` computed
/// `Block::is_expandable` — *"whether opening this row would show him
/// anything"* — and that flag was reaching nothing on screen; this is where it
/// arrives.
///
/// **TWO GLYPHS, AND THE SECOND ONE IS THE AFFORDANCE TELLING THE TRUTH ABOUT
/// ITSELF.** A closed row reads `▸` and an open one reads `▾`.
///
/// `0052` shipped one glyph for both states, not because one was right but
/// because which rows are open is the SHELL's set (`App::opened`, keyed on
/// `(thread_id, seq)`) and it was not on the `ViewState` — the projection
/// consumed it and reported only the resulting `expandable`. Saying so meant a
/// new field on two `Block` variants, outside that task's bound, so it recorded
/// the gap instead of reaching for it. `0056` owns the row model and closed it:
/// see [`Block::Lane::open`].
///
/// **It is not cosmetic, and that is the part worth keeping written down.** A
/// mark that reads the same open and closed means a click that LANDED and a
/// click that MISSED produce identical gutters, so the only way he can tell
/// them apart is to read the body and judge whether it grew. On a row whose
/// expansion adds a line or two that is not a judgement anyone can make at a
/// glance — and it is the same defect `0052` had just fixed one level down,
/// where the affordance was computed and nothing drew it.
const EXPANDER: &str = "▸ ";

/// The open half of [`EXPANDER`]. Two columns, exactly as wide, so opening a
/// row cannot shift anything: [`gutter_mark`] writes into a gutter it has
/// already measured, and a mark of a different width would move every hit-test
/// on the row while still looking fine.
const COLLAPSER: &str = "▾ ";

/// Paint a two-column mark into a row's LEFT GUTTER, without changing its width.
///
/// **It refuses rather than guesses.** Every shape a transcript row takes —
/// assistant box, right-aligned user box, lane row — begins with a run of
/// spaces at least [`GUTTER`] wide, so the mark has a home in all three. A row
/// that does not (a future shape nobody has written yet) is returned UNTOUCHED
/// rather than surgically edited: a mark that shifted the row by a column would
/// put every hit-test on that row one column out, and it would look fine.
fn gutter_mark(line: Line<'static>, mark: &str) -> (Line<'static>, Option<u16>) {
    let mut spans = line.spans;
    let Some(first) = spans.first() else {
        return (Line::from(spans), None);
    };
    let w = text_width(&first.content);
    if w < GUTTER || !first.content.chars().all(|c| c == ' ') {
        return (Line::from(spans), None);
    }
    // **THE MARK IS RIGHT-ADJUSTED IN THE PAD, NOT LEFT.** It was at column
    // zero while every transcript row began with exactly the two-column
    // gutter. An info blob's row begins with a margin [`geom::blob_margin`]
    // columns wide (eight at his width, four narrow) or more, and a mark left
    // at column zero would sit a third of a screen away from the thing it
    // opens.
    let at = w - GUTTER;
    let rest = spans.split_off(1);
    let mut out = Vec::new();
    if at > 0 {
        out.push(Span::styled(" ".repeat(at), palette::base()));
    }
    out.push(Span::styled(mark.to_string(), palette::muted()));
    out.extend(rest);
    (Line::from(out), Some(at as u16))
}

/// **THE ASSISTANT's MEASURE** — the columns its own text occupies, at a given
/// terminal width.
///
/// It is the whole block ([`geom::box_width`]) because the assistant has no box
/// to pay for: its text starts at the [`GUTTER`] and ends
/// [`geom::msg_margin`] columns short of the right edge, which is his number
/// exactly.
///
/// **IT USED TO BE `box_width - 2` AND THE SAME NUMBER SERVED BOTH LANES.**
/// That held only while the margin was a leftover of the ceiling; once both
/// margins are ten and only one lane has a box, the two wrap measures differ by
/// the box's two columns of padding. See [`geom::box_width`] for what is kept
/// instead: the two blocks are the same width.
fn msg_inner(w: usize) -> usize {
    box_width(w)
}

/// **HIS LEFT MARGIN, AND THE MID-ROUND CORRECTION THAT DEFINES IT.**
///
/// > *User message should be right aligned in the sense that they have a left
/// > margin, but the content within them should be left aligned.*
///
/// So "right aligned" is a fact about the BLOCK and never about the text:
/// `(left column, total box width)`, and inside that box every row starts at
/// `left + 1` and runs ragged right. Nothing in here consults the text.
/// **THE MARGIN IS THE SPECIFICATION AND THE BOX IS WHAT IS LEFT.** It was the
/// other way round — a `margin` local of 8 (4 narrow) that lost to
/// `msg_inner`'s ceiling at any width over 82, which is why his 97-column pane
/// measured a 19-column left margin against a constant that said 8. The block
/// width now carries the margin ([`geom::box_width`]) and there is no second
/// number here to disagree with it.
fn user_box(w: usize) -> (usize, usize) {
    // The box IS the block: `box_width` already spent the gutter and the
    // margin, so the only thing left to do is refuse to overrun the gutter at
    // a width below the one the face draws at all.
    let total = box_width(w).min(w.saturating_sub(GUTTER)).max(1);
    (w.saturating_sub(GUTTER + total), total)
}

/// A row of nothing, in the chrome background.
///
/// > *Let's make better use of whitespace in the individual threads.*
///
/// The whitespace is spent HERE and only here: one empty row in front of every
/// speech act, so a transcript reads as a column of separated blocks rather
/// than as a wall. It is not spent on the event lane — those rows are a
/// running commentary and a blank line between each of them would be the
/// opposite of what he asked for.
fn blank_row(w: usize) -> Line<'static> {
    size_line(Vec::new(), w, palette::base())
}

/// **THE TOP ROW OF THE PAPER BOX** — the note on the left, the bare message id
/// flush right.
///
/// > *I think by default let's include the message id at the top of the user
/// > message, right aligned. Don't say ID or anything, just actually print the
/// > id.*
///
/// No label, no `id:`, no prefix. The id.
///
/// **IT REPLACES THE `you · 19:25 · 5m` RUN HEADER RATHER THAN JOINING IT.**
/// Two rows of chrome above every message he types is the opposite of "better
/// use of whitespace", and the paper box already says whose the words are —
/// which is the entire job the word `you` was doing.
fn paper_head(
    id: &str,
    ts: Option<DateTime<Utc>>,
    tz: i32,
    now: DateTime<Utc>,
    narrow: bool,
    note: Option<&str>,
    w: usize,
) -> Line<'static> {
    let (left, total) = user_box(w);
    let inner = total.saturating_sub(2).max(1);
    // The LEFT half is the decoration and the metadata, in the order the
    // collapse rule drops them: the clock goes at 80 columns, the note never
    // does. At 60 columns "is this still coming?" is the question this row is
    // answering.
    let mut lead = String::new();
    if let Some(n) = note {
        lead.push_str(n);
    }
    if !narrow {
        let clock_bit = match ts {
            Some(ts) => format!("{} · {}", clock(ts, tz), geom::fmt_age(now - ts)),
            // A timestamp we were not given is not one we may guess at.
            None => "? · ?".to_string(),
        };
        if !lead.is_empty() {
            lead.push_str(" · ");
        }
        lead.push_str(&clock_bit);
    }
    // An EMPTY id prints nothing at all rather than an empty right margin —
    // the store uses it to say "this box has no message id", which is the case
    // for an agent's `DIRECT INVOCATION` input: the only id it could show is a
    // tool-call hash, and hash names are barred from the face.
    // **CONTENT NEVER COLLAPSES, AND THE ID IS THE CONTENT OF THIS ROW.** When
    // the two halves cannot both fit, the LEAD is cut — it is decoration and
    // metadata — and the id is printed whole. He asked for the id by name.
    let id_w = text_width(id);
    let room = inner.saturating_sub(id_w.min(inner));
    let lead = fit(&lead, room.saturating_sub(if room > 0 { 1 } else { 0 }));
    let gap = inner
        .saturating_sub(text_width(&lead))
        .saturating_sub(id_w.min(inner));
    size_line(
        vec![
            Span::styled(" ".repeat(left), palette::base()),
            Span::styled(" ", palette::paper()),
            Span::styled(lead, palette::paper_muted()),
            Span::styled(" ".repeat(gap), palette::paper()),
            Span::styled(fit(id, inner), palette::paper_muted()),
            Span::styled(" ", palette::paper()),
        ],
        w,
        palette::base(),
    )
}

/// One key/value row of a user message's metadata, inside the paper box.
///
/// Drawn only when the block is OPEN — *"we need a way to toggle viewing
/// this"* — and never when it is closed, which is what makes the default view
/// the one he specified.
fn paper_meta(k: &str, v: &str, w: usize) -> Line<'static> {
    let (left, total) = user_box(w);
    let inner = total.saturating_sub(2).max(1);
    let text = fit(&format!("{k} {v}"), inner);
    let pad = inner.saturating_sub(text_width(&text));
    size_line(
        vec![
            Span::styled(" ".repeat(left), palette::base()),
            Span::styled(" ", palette::paper()),
            Span::styled(text, palette::paper_muted()),
            Span::styled(" ".repeat(pad + 1), palette::paper()),
        ],
        w,
        palette::base(),
    )
}

/// **A MESSAGE'S WRAPPED ROWS, REMEMBERED** (THREAD-LOAD-LATENCY F6).
///
/// `md::lay` is a pure function of the text, the measure and the two styles,
/// and it is most of what a frame costs: scrolled 1,000 rows up a 4,000-block
/// thread every frame re-wrapped every message between the bottom and the
/// viewport (15 ms; 128 ms at the top). Keyed by a hash of exactly those
/// inputs, so a changed message (a streaming reply) is simply a miss and is
/// laid afresh; nothing is ever served for text it was not laid from.
///
/// Bounded as two generations of [`MD_CACHE_GEN`] entries: when the young one
/// fills, the old one is dropped and the young one becomes old, and a hit in
/// the old generation is promoted. Per thread (the renderer runs on one).
fn md_lay_cached(
    text: &str,
    inner: usize,
    base: ratatui::style::Style,
    code: ratatui::style::Style,
) -> std::rc::Rc<Vec<md::MdRow>> {
    use std::hash::{Hash, Hasher};
    let mut hsh = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hsh);
    inner.hash(&mut hsh);
    base.hash(&mut hsh);
    code.hash(&mut hsh);
    let key = (hsh.finish(), text.len());
    MD_CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if let Some(v) = c.0.get(&key) {
            return v.clone();
        }
        if let Some(v) = c.1.remove(&key) {
            c.0.insert(key, v.clone());
            return v;
        }
        let v = std::rc::Rc::new(md::lay(text, inner, base, code));
        if c.0.len() >= MD_CACHE_GEN {
            let young = std::mem::take(&mut c.0);
            c.1 = young;
        }
        c.0.insert(key, v.clone());
        v
    })
}

/// Entries per generation of [`md_lay_cached`]'s cache — two 4,000-block
/// threads' worth of messages, at one width.
const MD_CACHE_GEN: usize = 8_192;

type MdCache = (
    std::collections::HashMap<(u64, usize), std::rc::Rc<Vec<md::MdRow>>>,
    std::collections::HashMap<(u64, usize), std::rc::Rc<Vec<md::MdRow>>>,
);

thread_local! {
    static MD_CACHE: std::cell::RefCell<MdCache> = std::cell::RefCell::new(Default::default());
}

fn lay_out(blocks: &[Block], vs: &ViewState, now: DateTime<Utc>, w: usize) -> Laid {
    lay_out_from(blocks, 0, vs, now, w)
}

/// [`lay_out`] of `blocks[start..]` ONLY, exactly as those blocks are laid in a
/// layout of the whole slice: the one piece of state that crosses a block
/// boundary (`last_sender`, and whether anything was laid above) is derived
/// from `blocks[start - 1]`, and every `Unit` keeps its GLOBAL block index.
/// The returned spans and marks are per block of the SUFFIX (index 0 is block
/// `start`), and its rows are the bottom rows of the full layout.
///
/// THREAD-LOAD-LATENCY F3: the transcript lays out only the blocks the
/// viewport can reach, not all 4,000, on every paint.
fn lay_out_from(blocks: &[Block], start: usize, vs: &ViewState, now: DateTime<Utc>, w: usize) -> Laid {
    let tz = vs.tz_offset_minutes;
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut texts: Vec<Option<RowText>> = Vec::new();
    let mut spans: Vec<(usize, usize)> = Vec::with_capacity(blocks.len().saturating_sub(start));
    let mut marks: Vec<Option<u16>> = Vec::with_capacity(blocks.len().saturating_sub(start));
    let narrow = w <= geom::NARROW_WIDTH as usize;
    let mut last_sender: Option<Sender> = match start.checked_sub(1).and_then(|p| blocks.get(p)) {
        Some(Block::Message { sender, .. }) => Some(*sender),
        _ => None,
    };
    let laid_above = start > 0;

    for (i, b) in blocks.iter().enumerate().skip(start) {
        let first = out.len();
        let unit = Unit::new(UnitKind::Content, i);
        match b {
            Block::Message {
                id,
                sender,
                ts,
                text,
                note,
                meta,
                ..
            } => {
                // Consecutive same-sender ASSISTANT messages group under one
                // header.
                //
                // **A BOX WITH A NOTE ALWAYS GETS ITS OWN HEADER**, even in a
                // run: the note is what the header is FOR in that case, and a
                // growing reply grouped silently under the finished one above
                // it would say "writing" about neither of them or about both.
                //
                // **HIS SIDE NEVER GROUPS**, because the row at the top of his
                // box is the MESSAGE ID and an id is per message. Grouping two
                // of his messages would print one id over two of them, which is
                // worse than printing none.
                let heads = *sender == Sender::User || last_sender != Some(*sender) || note.is_some();
                if heads {
                    if !out.is_empty() || laid_above {
                        out.push(blank_row(w));
                        texts.push(None);
                    }
                    out.push(match sender {
                        Sender::Assistant => {
                            run_header(*ts, tz, now, narrow, note.as_deref(), w)
                        }
                        Sender::User => paper_head(
                            &id.message_id,
                            *ts,
                            tz,
                            now,
                            narrow,
                            note.as_deref(),
                            w,
                        ),
                    });
                    // **THE HEADER IS NOT PART OF THE MESSAGE.** It says who
                    // and when; the message is what was said. A copy that swept
                    // it in would put "claude · 11:59 · 3m" at the top of every
                    // paste, which is exactly the visual-screen copy this whole
                    // path exists to stop being.
                    texts.push(None);
                }
                last_sender = Some(*sender);
                // **THE PROSE STYLE AND THE CODE STYLE ARE ONE DECISION, PER
                // SENDER.** An inline-code chip has to be distinguishable from
                // the background it sits on, and the two lanes do not share
                // one — so the pair is picked here, together, and `md` is
                // handed both rather than choosing either.
                let (base, code) = match sender {
                    Sender::Assistant => (palette::ink(), palette::code()),
                    Sender::User => (palette::paper(), palette::code_on_paper()),
                };
                // **THE MEASURE IS PER SIDE, and it has to be.** His box can be
                // NARROWER than the assistant's at a narrow width, because its
                // left margin comes out of the same columns — so wrapping both
                // at `msg_inner` would have laid his text two columns wider
                // than the box drawn around it and eaten the screen gutter.
                let (col0, inner) = match sender {
                    Sender::Assistant => (GUTTER as u16, msg_inner(w)),
                    Sender::User => {
                        let (left, total) = user_box(w);
                        ((left + 1) as u16, total.saturating_sub(2).max(1))
                    }
                };
                for row in md_lay_cached(text, inner, base, code).iter().cloned() {
                    texts.push(row.text.map(|text| RowText {
                        unit,
                        start: row.start,
                        col0,
                        text,
                    }));
                    out.push(message_row(*sender, row.spans, w));
                }
                // **THE METADATA, AND IT IS OFF BY DEFAULT.** His default view
                // is the id and nothing else; everything the threads system and
                // this app attach to the message is behind the same expander
                // every other foldable row in the transcript uses.
                if b.is_open() {
                    for (k, v) in meta {
                        out.push(paper_meta(k, v, w));
                        texts.push(None);
                    }
                }
            }
            Block::Lane {
                glyph,
                severity,
                text,
                count,
                open,
                affordance,
                ..
            } => {
                last_sender = None;
                blob(
                    unit,
                    *glyph,
                    *severity,
                    text,
                    affordance.as_deref(),
                    *count,
                    *open,
                    w,
                    &mut out,
                    &mut texts,
                );
            }
            Block::Rule { text } => {
                last_sender = None;
                let label = if text.is_empty() {
                    String::new()
                } else {
                    format!(" {text} ")
                };
                let used = GUTTER + text_width(&label);
                let dashes = w.saturating_sub(used);
                texts.push(if text.is_empty() {
                    None
                } else {
                    Some(RowText {
                        unit,
                        start: 0,
                        col0: (GUTTER + 1) as u16,
                        text: text.clone(),
                    })
                });
                out.push(line_from(
                    vec![
                        Seg::new("─".repeat(GUTTER), palette::muted()),
                        Seg::new(label, palette::muted()),
                        Seg::new("─".repeat(dashes), palette::muted()),
                    ],
                    w,
                ));
            }
        }
        // **THE MARK GOES ON THE LAST ROW OF THE BLOCK**, which on a message is
        // the row carrying the `…` he is looking at and on a lane entry is the
        // last row of the blob. `transcript` claims the SAME two columns of the
        // SAME row from the SAME `(first, len)` span and the column this
        // returns, so the glyph and the button are one fact rather than two
        // that have to agree.
        let mut mark_at = None;
        if b.is_expandable() && out.len() > first {
            if let Some(last) = out.pop() {
                // **AND IT SAYS WHICH WAY IT IS POINTING.** `▾` is an open row,
                // `▸` a closed one — both two columns wide, so the claim
                // `transcript` makes on those same two columns is unaffected by
                // which one is painted.
                let mark = match b.is_open() {
                    true => COLLAPSER,
                    false => EXPANDER,
                };
                let (line, at) = gutter_mark(last, mark);
                out.push(line);
                mark_at = at;
            }
        }
        marks.push(mark_at);
        spans.push((first, out.len() - first));
    }
    debug_assert_eq!(out.len(), texts.len());
    (out, spans, texts, marks)
}

/// **THE COLUMNS A ROW'S TEXT MUST KEEP BEFORE A GESTURE IS WORTH RESERVING.**
///
/// A reserved gesture spends its own width out of a blob that is already at its
/// firm margins, and below this floor the spend leaves a row that can say what
/// he may DO but not what he would be doing it TO — `agent … — → opens its
/// chat`, which names nothing. At that point the gesture is DROPPED WHOLE and
/// the row crops like any other, which is the same rule [`tree_columns`]
/// applies to a figure that has lost its own number. It bites only below 48
/// columns; his own pane is 97.
const GESTURE_FLOOR: usize = 12;

/// **THE INFO BLOB — centred, with a FIRM margin each side, growing only DOWN.**
///
/// > *info blobs should be centered with a max character size that enforces a
/// > margin on each side so it doesn't take up the whole page. Clicking it
/// > should expand it but should not break the margins, they're firm. It can
/// > expand or collapse vertically.*
///
/// **"Firm" is the whole specification and it is enforced structurally, not by
/// care.** [`geom::blob_width`] and [`geom::blob_left`] take the terminal width
/// and NOTHING ELSE — not the text, not `open`, not the count stamp — so there
/// is no expression anywhere in this function that could widen a blob on
/// expand. Opening one adds ROWS.
///
/// **THIS SUPERSEDES THE LEFT-ALIGNED THIRD LANE.** The lane was drawn flush
/// left at a two-column indent because centring had been left open (`DEC-11`
/// records the question as an `idk`). He has now answered it: centred, firm
/// margins. The glyph prefix, the muted style and the severity grammar are
/// untouched — only the geometry moved.
#[allow(clippy::too_many_arguments)]
fn blob(
    unit: Unit,
    glyph: char,
    severity: Severity,
    text: &str,
    affordance: Option<&str>,
    count: Option<u64>,
    open: bool,
    w: usize,
    out: &mut Vec<Line<'static>>,
    texts: &mut Vec<Option<RowText>>,
) {
    let width = geom::blob_width(w);
    let left = geom::blob_left(w);
    // Two columns for the glyph and its space; a continuation row is indented
    // under the text rather than under the glyph.
    let body = width.saturating_sub(2).max(1);
    let stamp = count.map(|n| format!(" ×{n}"));
    let pad = |segs: Vec<Seg>| -> Line<'static> {
        let mut all = vec![Seg::new(" ".repeat(left), palette::base())];
        all.extend(segs);
        line_from(crop_with_ellipsis(all, left + width), w)
    };
    if !open {
        // CLOSED: one row, cropped. The crop's `…` stands for characters that
        // are in the logical text anyway, so a drag onto the ellipsis selects
        // the character it stands for — the honest reading of pointing at it.
        let mut segs = vec![Seg::new(format!("{glyph} "), severity.style())];
        // **AN AFFORDANCE IS RESERVED BEFORE THE TEXT IS MEASURED.**
        //
        // A closed blob runs at `blob_width(w)` since the firm margins landed,
        // and on a row that cannot be opened the crop is a DELETION rather
        // than a fold. So a row that declares a trailing gesture keeps it: the
        // gesture's columns come off the budget first and the elastic middle
        // ellipsizes into what is left. If even the gesture will not fit, it is
        // DROPPED WHOLE rather than half-drawn — the same rule `tree_columns`
        // applies to a figure that has lost its own number.
        let stamp_w = stamp.as_deref().map(text_width).unwrap_or(0);
        let reserved = affordance
            .filter(|a| !a.is_empty())
            .map(|a| (a, text_width(a)))
            .filter(|(_, aw)| body >= aw + stamp_w + GESTURE_FLOOR);
        match reserved {
            Some((aff, aff_w)) => {
                let head = text.strip_suffix(aff).unwrap_or(text);
                let room = body - aff_w - stamp_w;
                segs.extend(crop_with_ellipsis(
                    vec![Seg::new(head.to_string(), palette::muted())],
                    room,
                ));
                segs.push(Seg::new(aff.to_string(), palette::muted()));
            }
            None => segs.push(Seg::new(text.to_string(), palette::muted())),
        }
        if let Some(s) = &stamp {
            // The count stamp is the promise that nothing was dropped: it says
            // how many events this one row stands for, and expanding shows
            // every one of them.
            segs.push(Seg::new(s.clone(), palette::amber()));
        }
        texts.push(Some(RowText {
            unit,
            start: 0,
            col0: (left + 2) as u16,
            text: text.to_string(),
        }));
        out.push(pad(segs));
        return;
    }
    // OPEN: the SAME width, more rows.
    let rows = geom::wrap(&[Seg::new(text.to_string(), palette::muted())], body);
    let last = rows.len().saturating_sub(1);
    for (i, row) in rows.into_iter().enumerate() {
        let head = if i == 0 {
            Seg::new(format!("{glyph} "), severity.style())
        } else {
            Seg::new("  ".to_string(), palette::base())
        };
        let painted = row.text();
        let mut segs = vec![head];
        segs.push(Seg::new(painted.clone(), palette::muted()));
        if i == last {
            if let Some(s) = &stamp {
                segs.push(Seg::new(s.clone(), palette::amber()));
            }
        }
        texts.push(Some(RowText {
            unit,
            start: row.start,
            col0: (left + 2) as u16,
            text: painted,
        }));
        out.push(pad(segs));
    }
}

/// The muted line above the first box of an ASSISTANT run. At or below 80
/// columns the timestamp drops: the collapse order is decoration, then
/// METADATA, never content (`0023` `layout`).
///
/// **THE RAIL IS GONE FROM UNDER IT.** *"You don't need to have the quote line
/// on the left of every message. It's distracting."* — so this row is the only
/// thing that says a run has started, which is why it survives at 60 columns
/// with the word `claude` on it and nothing else.
fn run_header(
    ts: Option<DateTime<Utc>>,
    tz: i32,
    now: DateTime<Utc>,
    narrow: bool,
    // The state word, when the box has one: `writing`, or `interrupted
    // mid-reply`. It survives the NARROW path, unlike the clock — at 60
    // columns "is this still coming?" is the question the header is answering.
    note: Option<&str>,
    w: usize,
) -> Line<'static> {
    let mut label = if narrow {
        "claude".to_string()
    } else {
        match ts {
            Some(ts) => format!("claude · {} · {}", clock(ts, tz), geom::fmt_age(now - ts)),
            // A timestamp we were not given is not one we may guess at, and
            // the arrival time is the specific guess that looks right.
            None => "claude · ? · ?".to_string(),
        }
    };
    if let Some(n) = note {
        label.push_str(&format!(" · {n}"));
    }
    line_from(
        vec![
            Seg::new(" ".repeat(GUTTER), palette::base()),
            Seg::new(label, palette::muted()),
        ],
        w,
    )
}

/// One wrapped row of a chat message.
///
/// **THE RAILS ARE GONE AND THE PAPER BOX REPLACED THEM.** His words: *"You
/// don't need to have the quote line on the left of every message. It's
/// distracting. Claude code solves this by making our messages right-aligned
/// with a white background and black text. Let's emulate that."* So the
/// assistant's lane is plain text at the gutter, and his is a filled
/// rectangle — the side is still the speaker, and the scan edge still tells him
/// who is talking before he reads a word.
///
/// **THE BLOCK IS OFFSET RIGHT; THE TEXT INSIDE IT IS LEFT-ALIGNED.** That is
/// his mid-round correction verbatim, and it is why the padding this function
/// adds goes on the RIGHT of the words and the margin goes on the LEFT of the
/// box.
fn message_row(sender: Sender, spans: Vec<Span<'static>>, w: usize) -> Line<'static> {
    match sender {
        Sender::Assistant => {
            let mut all: Vec<Span<'static>> = vec![Span::styled(
                " ".repeat(GUTTER),
                palette::base(),
            )];
            all.extend(spans);
            size_line(all, w, palette::base())
        }
        Sender::User => {
            let (left, total) = user_box(w);
            let inner = total.saturating_sub(2).max(1);
            let body: usize = spans.iter().map(|s| text_width(&s.content)).sum();
            let mut all: Vec<Span<'static>> = vec![
                Span::styled(" ".repeat(left), palette::base()),
                Span::styled(" ".to_string(), palette::paper()),
            ];
            all.extend(spans);
            // The right pad is what makes the box a RECTANGLE. Without it the
            // white background would be ragged down the right-hand side, which
            // is the one thing a reader reads as a rendering fault rather than
            // as a style.
            all.push(Span::styled(
                " ".repeat(inner.saturating_sub(body) + 1),
                palette::paper(),
            ));
            size_line(all, w, palette::base())
        }
    }
}

/// `%H:%M` in the DISPLAY zone the shell handed us. No zone lookup happens
/// here; `chrono::Local` is a banned type in this crate.
fn clock(ts: DateTime<Utc>, tz_offset_minutes: i32) -> String {
    let off = FixedOffset::east_opt(tz_offset_minutes * 60)
        .unwrap_or_else(|| FixedOffset::east_opt(0).expect("UTC is always a valid fixed offset"));
    ts.with_timezone(&off).format("%H:%M").to_string()
}

// ── band 4: the composer ─────────────────────────────────────────────────────

/// The columns of the composer band a character may occupy.
///
/// The band's own gutter on the left and ONE column on the right, which is the
/// caret's parking space: with the text allowed to reach the last column, a
/// caret sitting after the final character of a full row would have nowhere to
/// be drawn and would silently appear on the next row instead.
pub fn composer_inner(w: usize) -> usize {
    w.saturating_sub(GUTTER + 1).max(1)
}

/// **HOW MANY CLIENT-QUEUE ROWS THE COMPOSER'S BAND SHOWS AT ONCE.**
///
/// The queue is a REGION of the thread screen now — *"just make the queue
/// accessible by click or by navigating above or below the textarea"* — so what
/// it is holding is drawn where he can reach it, immediately above the text he
/// is typing.
///
/// It is a WINDOW and not the whole list, for the reason the transcript is:
/// forty queued messages would leave no chat. The window follows the cursor
/// (see [`queue_window`]) and the band's own border already says how many are
/// held, so nothing is hidden without being counted — and `^O` still opens
/// the whole queue.
pub const QUEUE_BAND_MAX: usize = 3;

/// How many queue rows this composer will draw.
fn queue_band_rows(c: &Composer) -> usize {
    if c.offer.is_some() || !c.editable {
        // The offer replaces the band's body while it stands, and a
        // non-editable band has no queue of its own to show.
        return 0;
    }
    c.buf.pending().len().min(QUEUE_BAND_MAX)
}

/// **WHICH QUEUED ROW THE BAND SHOWS FIRST.** The window follows the cursor,
/// exactly as a list's does, so the row the selection is on is always drawn.
fn queue_window(c: &Composer, cursor: usize) -> usize {
    let n = c.buf.pending().len();
    let shown = queue_band_rows(c);
    if n <= shown {
        return 0;
    }
    cursor
        .min(n - 1)
        .saturating_sub(shown.saturating_sub(1))
        .min(n - shown)
}

fn composer_height(c: &Composer, body_rows: usize, w: usize) -> usize {
    // **THE STAND-IN IS TWO ROWS AND IT REPLACES THE BAND.** A greyed box with
    // a caret in it would be a stub pretending to work; two sentences that say
    // what is true and how to get back cost one row less than the box's own
    // minimum. See [`crate::StandIn`].
    if c.stand_in.is_some() {
        return 2.min(body_rows);
    }
    let q = queue_band_rows(c);
    // Always leave at least one transcript row: the transcript is the thing
    // the face is for.
    let ceiling = body_rows.saturating_sub(1).min(COMPOSER_MAX_ROWS as usize + q);
    // **THE BAND GROWS BY WHAT IS LAID OUT, NOT BY COUNTING NEWLINES.** The old
    // body counted `\n` in the text, which is a SECOND wrap derivation and a
    // wrong one: a single logical line that wrapped over three rows reported
    // one, so the band stayed short and the caret went below it. Asking the
    // same `layout` the rows come from is `0023` `defect_classes` CLASS 3
    // closed by construction.
    let text_rows = if c.offer.is_some() {
        // The offer replaces the text while it stands, and it is one row.
        1
    } else {
        c.buf.visual_rows(composer_inner(w))
    };
    let wanted = (text_rows + 2 + q).clamp(
        COMPOSER_MIN_ROWS as usize + q,
        COMPOSER_MAX_ROWS as usize + q,
    );
    wanted.min(ceiling).max(1)
}

// ─────────────────────────────────────────────────────────────────────────────
//  THE SUBAGENT TREE — the band that takes the chat area back
// ─────────────────────────────────────────────────────────────────────────────

/// **HOW MANY SUBAGENT ROWS THE BAND SHOWS AT ONCE**, before the window starts
/// following the cursor.
///
/// Six, plus its rule line, is seven rows at worst on his 97×35 pane — which
/// leaves the chat 23 rows with a three-row composer, against a transcript that
/// was giving *"90% of the chat area"* to the subagents' own rows.
pub const TREE_MAX_ROWS: usize = 6;

/// The least the band can be and still show a child under its parent.
const TREE_MIN_ROWS: usize = 1 + 2;

/// **HOW MANY ROWS THE SUBAGENT BAND TAKES**, given the room the banner left.
///
/// **THE TREE YIELDS BEFORE THE COMPOSER.** It never takes the box under its
/// minimum, and below its own floor it is not drawn at all — and then
/// `Region::Agents` is not a stop either, which is the same answer as "this
/// thread has no subagents" and is reached by the same field.
///
/// `rows.len() < 2` is zero rows: **the parent alone is not a tree.**
pub fn tree_height(t: &AgentTree, room: usize) -> usize {
    if t.rows.len() < 2 {
        return 0;
    }
    let want = 1 + t.rows.len();
    let cap = 1 + TREE_MAX_ROWS;
    let avail = room.saturating_sub(COMPOSER_MIN_ROWS as usize + 1);
    if avail < TREE_MIN_ROWS {
        0
    } else {
        want.min(cap).min(avail)
    }
}

/// Which tree row the band draws first. The window follows the cursor exactly
/// as a list's does, so the row the selection is on is always painted.
fn tree_window(t: &AgentTree, shown: usize) -> usize {
    let n = t.rows.len();
    if n <= shown || shown == 0 {
        return 0;
    }
    t.cursor_at()
        .min(n - 1)
        .saturating_sub(shown.saturating_sub(1))
        .min(n - shown)
}

/// **WHICH FIGURES SURVIVE AT THIS WIDTH**, dropped right to left.
///
/// `(elapsed, tokens, calls, bash, agents)`. A figure that would not fit is
/// DROPPED rather than left to the crop: a column that has lost its own number
/// costs the row its width and says nothing.
fn tree_columns(w: usize) -> (bool, bool, bool, bool, bool) {
    match w {
        0..=59 => (true, false, false, false, false),
        60..=79 => (true, true, false, false, false),
        _ => (true, true, true, true, true),
    }
}

/// `31k tok`. Thousands once there are thousands, because a subagent's tally
/// runs to six digits and the exact units are not what he is reading it for.
///
/// `pub` so the chat TILE spells the same figure the same way. Two formatters
/// for one number is two numbers on one screen that can disagree.
pub fn fmt_tokens(n: u64) -> String {
    if n < 1_000 {
        format!("{n} tok")
    } else if n < 1_000_000 {
        format!("{}k tok", n / 1_000)
    } else {
        format!("{}.{}M tok", n / 1_000_000, (n % 1_000_000) / 100_000)
    }
}

/// The rails a row of depth `d` draws in front of its state word.
///
/// A FILE TREE, which is what he asked for by name: *"It looks like a file
/// tree. See if you can copy claude code's view."* Depth 0 is the thread
/// itself and has no rail.
fn tree_rail(row: &TreeRow) -> String {
    if row.depth == 0 {
        return String::new();
    }
    let mut s = "│ ".repeat(row.depth.saturating_sub(1));
    s.push_str(if row.last_sibling { "└ " } else { "├ " });
    s
}

/// The figures half of one row, already in his order and already dropped to
/// what this width can hold. Empty when the row has nothing to say.
fn tree_figures(row: &TreeRow, now: DateTime<Utc>, w: usize) -> String {
    let (show_elapsed, show_tokens, show_calls, show_bash, show_agents) = tree_columns(w);
    let mut parts: Vec<String> = Vec::new();
    // **ELAPSED IS SUBTRACTED HERE, FROM THE `now` THE FRAME WAS HANDED.** A
    // duration formatted in the projection is frozen at whatever the clock
    // said when the event arrived, which is the defect `store.rs` names.
    if show_elapsed {
        if let Some(started) = row.started {
            let to = row.ended.unwrap_or(now);
            parts.push(geom::fmt_age(to - started));
        }
    }
    if show_tokens {
        if let Some(t) = row.tokens {
            parts.push(fmt_tokens(t));
        }
    }
    if show_calls {
        if let Some(c) = row.calls {
            parts.push(format!("{c} calls"));
        }
    }
    // **`bash` IS CALLS IN FLIGHT AND THE WORD SAYS SO** — his own ruling, and
    // `0080` measured `ps` wrong in BOTH directions on this question. Drawn
    // only when there are any: a `0 bash` on every row is a column of noise.
    if show_bash {
        if let Some(b) = row.bash_in_flight.filter(|b| *b > 0) {
            parts.push(format!("{b} bash in flight"));
        }
    }
    if show_agents {
        if let Some(a) = row.agents_running.filter(|a| *a > 0) {
            parts.push(format!("{a} agent{}", if a == 1 { "" } else { "s" }));
        }
    }
    parts.join(" · ")
}

/// **ONE ROW OF THE TREE.** Returns the line and the target it claims.
fn tree_row_line(
    row: &TreeRow,
    i: usize,
    now: DateTime<Utc>,
    w: usize,
    subject: bool,
) -> (Line<'static>, RowInfo) {
    // `▸` marks the SUBJECT — whose chat is above. The CURSOR is the reverse
    // video the caller paints; the two coincide by default and diverge while
    // he browses, which is the whole point of keeping them apart.
    let mark = if subject { "▸ " } else { "  " };
    let mut segs = vec![Seg::new(mark.to_string(), palette::ink())];
    segs.push(Seg::new(tree_rail(row), palette::muted()));

    if row.depth == 0 {
        // The ROOT row is a THREAD. It has a title and no figures of its own —
        // its figures are the transcript above it. TUI-P1b: its STATUS comes
        // first, in the daemon's words, exactly as every child row's does.
        if let Some(st) = row.status.as_deref() {
            segs.push(Seg::new(format!("{st} "), status_style(st)));
        }
        segs.push(Seg::new(fold_mark(row).to_string(), palette::muted()));
        segs.push(Seg::new(one_line(&row.label), palette::ink()));
        // **WHAT THE THREAD IS DOING, WHEN THAT IS WAITING ON ITS CHILDREN** —
        // the same phrase the header carries, so a parent whose turn ended
        // never reads as a finished thread on its own row 0.
        match row.detail.as_deref() {
            Some(d) => segs.push(Seg::new(format!("  {d}"), Severity::Calm.style())),
            None if row.id == TreeId::Parent => {
                segs.push(Seg::new("  this thread".to_string(), palette::muted()))
            }
            None => {}
        }
        // **`N active, M idle`** — the daemon's summary of its direct
        // children, verbatim (TUI-P1b; replaces `N running · M done`).
        if let Some(cs) = row.children_summary.as_deref() {
            segs.push(Seg::new(format!("  {cs}"), palette::muted()));
        }
        let used: usize = segs.iter().map(|s| text_width(&s.text)).sum();
        if used < w {
            segs.push(Seg::new(" ".repeat(w - used), palette::base()));
        }
        return (
            line_from(crop_with_ellipsis(segs, w), w),
            RowInfo::bare(Target::AgentRow(i)),
        );
    }

    // **A DOT FOR LIVE, AND A WORD FOR THE STATE** — his glyph ruling: dots,
    // never asterisks, and never a spinner (a spinner is a clock this crate
    // does not have).
    // **THE STATUS, THEN THE TITLE** (TUI-P1b): the daemon's display string
    // (`active (+2)`, `active`, `idle`, an error label) when the row carries
    // one — drawn VERBATIM, never computed here — else the state's own word.
    let running = match row.status.as_deref() {
        Some(st) => st.starts_with("active"),
        None => row.state == SubagentState::Running,
    };
    segs.push(Seg::new(
        if running { "● " } else { "  " }.to_string(),
        palette::green(),
    ));
    // Twelve, not eleven: `no end seen` is exactly eleven columns and a
    // width that equals the longest word leaves no space before the label.
    // `ended, unreported` is longer and keeps one space after it.
    let (word, style) = match row.status.as_deref() {
        Some(st) => (st, status_style(st)),
        None => (row.state.word(), row.state.severity().style()),
    };
    let word_w = 12.max(text_width(word) + 1);
    segs.push(Seg::new(format!("{word:<word_w$}"), style));
    segs.push(Seg::new(fold_mark(row).to_string(), palette::muted()));
    // **A MANAGED CHILD IS MARKED `⧉`** — a thread of its own, which `→`
    // opens — and `↯` when its parent is not waiting on it (`wake: never`).
    if row.class == SubagentClass::Managed {
        segs.push(Seg::new(
            if row.no_wake { "⧉↯ " } else { "⧉ " }.to_string(),
            palette::muted(),
        ));
    }

    // **A PARKED CHILD SAYS WHY**, in the daemon's words, where the figures
    // would be: a parked thread has no figures worth more than its reason.
    let mut figures = match row.detail.as_deref() {
        Some(d) if matches!(row.state, SubagentState::Parked | SubagentState::Retrying) => {
            format!("· {}", one_line(d))
        }
        _ => tree_figures(row, now, w),
    };
    // A row with children of its own says how they stand, in the daemon's
    // words, ahead of its figures (TUI-P1b).
    if let Some(cs) = row.children_summary.as_deref() {
        figures = if figures.is_empty() {
            cs.to_string()
        } else {
            format!("{cs} · {figures}")
        };
    }
    let used: usize = segs.iter().map(|s| text_width(&s.text)).sum();
    let fig_w = text_width(&figures);
    // The label takes what the figures leave, and the figures sit at the right
    // edge — so the columns line up down the band and a long label crops
    // rather than pushing a number off the row.
    let room = w.saturating_sub(used + fig_w + 2);
    let mut label = one_line(&row.label);
    if text_width(&label) > room {
        label = crop_to(&label, room);
    }
    let label_w = text_width(&label);
    segs.push(Seg::new(label, palette::ink2()));
    let gap = w.saturating_sub(used + label_w + fig_w);
    if gap > 0 {
        segs.push(Seg::new(" ".repeat(gap), palette::base()));
    }
    segs.push(Seg::new(figures, palette::muted()));
    (
        line_from(crop_with_ellipsis(segs, w), w),
        RowInfo::bare(Target::AgentRow(i)),
    )
}

/// **THE FOLD MARK** (TUI-P1b): `▿ ` expanded, `▹ ` collapsed, nothing for a
/// row with no children to show. `▸` is taken — it marks the SUBJECT.
fn fold_mark(row: &TreeRow) -> &'static str {
    match row.expanded {
        Some(true) => "▿ ",
        Some(false) => "▹ ",
        None => "",
    }
}

/// The colour of one status string in his vocabulary: `active…` calm, `idle`
/// muted, anything else (an error label) critical.
fn status_style(st: &str) -> ratatui::style::Style {
    if st.starts_with("active") {
        Severity::Calm.style()
    } else if st == "idle" {
        palette::muted()
    } else {
        Severity::Critical.style()
    }
}

/// Crop to `room` columns by grapheme cluster, the unit `text_width` measures.
fn crop_to(s: &str, room: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for g in unicode_segmentation::UnicodeSegmentation::graphemes(s, true) {
        let gw = text_width(g);
        if used + gw > room {
            break;
        }
        out.push_str(g);
        used += gw;
    }
    out
}

/// **THE SUBAGENT BAND.** A rule line that tallies the whole tree, then the
/// window of rows the cursor is in.
fn agents_band(
    vs: &ViewState,
    now: DateTime<Utc>,
    w: usize,
    rows: usize,
) -> (Vec<Line<'static>>, Vec<RowInfo>) {
    if rows == 0 {
        return (Vec::new(), Vec::new());
    }
    // **THE CURSOR IS PAINTED ONLY WHERE THE KEYBOARD IS.** Read from the mode
    // rather than taken as a parameter, so the band and the key table cannot
    // disagree about which region holds the selection — and mic mode, which
    // reserves this band read-only, falls out of the same comparison.
    let keys = matches!(vs.mode, Mode::Thread(Region::Agents));
    let t = &vs.tree;
    let shown = rows.saturating_sub(1);
    let first = tree_window(t, shown);
    // ── the rule line ────────────────────────────────────────────────────
    // **`N active, M idle`, IN THE DAEMON'S WORDS** (TUI-P1b) — it replaced
    // `N running · M done`, which spoke a vocabulary he has retired. No
    // summary from the daemon, no count: a number this face derived would be
    // the second copy of the rollup that R9.UI1b's inspection forbade.
    let mut head = vec![Seg::new("─".repeat(GUTTER), palette::muted())];
    match t.summary.as_deref() {
        Some(sum) => {
            head.push(Seg::new("─ agents · ".to_string(), palette::muted()));
            head.push(Seg::new(
                format!("{sum} "),
                if sum.starts_with("0 active") {
                    palette::muted()
                } else {
                    Severity::Calm.style()
                },
            ));
        }
        None => head.push(Seg::new("─ agents ".to_string(), palette::muted())),
    }
    // **NOTHING IS HIDDEN WITHOUT BEING COUNTED.** The window can be smaller
    // than the tree, so the rule says what is off it in each direction.
    let above = first;
    let below = t.rows.len().saturating_sub(first + shown);
    if above > 0 || below > 0 {
        head.push(Seg::new("─ ".to_string(), palette::muted()));
        head.push(Seg::new(format!("↑{above} ↓{below} more "), palette::amber()));
    }
    let used: usize = head.iter().map(|s| text_width(&s.text)).sum();
    if used < w {
        head.push(Seg::new("─".repeat(w - used), palette::muted()));
    }
    let mut out = vec![line_from(crop_with_ellipsis(head, w), w)];
    let mut infos = vec![RowInfo::bare(Target::Empty)];

    // ── the rows ─────────────────────────────────────────────────────────
    let cursor_at = t.cursor_at();
    let subject_at = t.subject_at();
    for k in 0..shown {
        let i = first + k;
        let Some(row) = t.rows.get(i) else {
            out.push(size_line(vec![], w, palette::base()));
            infos.push(RowInfo::bare(Target::Empty));
            continue;
        };
        let (line, info) = tree_row_line(row, i, now, w, subject_at == Some(i));
        // The cursor's row is reverse video ONLY while the band actually holds
        // the selection: a highlight on a band the arrows are not acting on is
        // a lie about where the keyboard is. In mic mode the band is drawn
        // read-only and `keys` is false.
        out.push(if keys && i == cursor_at {
            geom::highlight(line, 0, w)
        } else {
            line
        });
        infos.push(info);
    }
    (out, infos)
}

// ─────────────────────────────────────────────────────────────────────────────
//  MIC MODE
// ─────────────────────────────────────────────────────────────────────────────

/// The `▶` button's cell, including the space after it. Three columns, claimed
/// as [`Target::MicPlay`] on every row that has audio behind it.
const PLAY_CELL: &str = "▶  ";
/// The same width, blank, on a row with nothing to play — so the text of a
/// typed row and the text of a spoken one start in the SAME column. His ruling
/// is that a transcribed row looks exactly like a typed one except for the play
/// button; two different left margins would break that at a glance.
const NO_PLAY_CELL: &str = "   ";

/// **ONE ROW OF THE CLIENT QUEUE — the surface mic mode and the queue view
/// share.**
///
/// It is ONE function called from two places rather than two renderings of the
/// same list, because the two would drift: the day a state is added, a second
/// copy is a row that says `transcribing…` on one screen and nothing on the
/// other.
fn client_row(
    item: &crate::compose::PendingItem,
    i: usize,
    w: usize,
    playing: bool,
    indent: usize,
) -> (Line<'static>, RowInfo) {
    use crate::compose::ItemState;
    let pad = " ".repeat(indent);
    let mut segs = vec![Seg::new(pad.clone(), palette::base())];

    let play_from = indent as u16;
    let has_audio = item.is_playable();
    if has_audio {
        segs.push(Seg::new(
            PLAY_CELL,
            if playing {
                Severity::Ok.style()
            } else {
                palette::ink()
            },
        ));
    } else {
        segs.push(Seg::new(NO_PLAY_CELL, palette::base()));
    }

    let (style, text) = match &item.state {
        // **A TRANSCRIBED ROW IS STYLED EXACTLY AS A TYPED ONE.** Same colour,
        // same column, same everything — the play button is the only
        // difference, which is what he asked for.
        ItemState::Typed | ItemState::Ready => (palette::ink(), one_line(&item.text)),
        ItemState::Transcribing => (Severity::Warn.style(), "transcribing…".to_string()),
        ItemState::Failed(why) => (Severity::Critical.style(), one_line(why)),
    };

    let failed = matches!(item.state, ItemState::Failed(_));
    // The actions sit at the right edge on a failed row, the way the design
    // doc's queue puts `send again · delete` there.
    let actions = if failed { "retry · drop" } else { "" };
    let used = indent + 3;
    let room = w.saturating_sub(used + if failed { actions.len() + 2 } else { GUTTER });
    segs.push(Seg::new(crop_plain(&text, room), style));

    let mut info = RowInfo::new(Target::MicRow(i), None);
    if has_audio {
        // THE BUTTON IS CLAIMED WHERE IT IS PAINTED. Two columns of glyph plus
        // the space, so a click just to the right of the triangle still lands.
        info = info.claim(play_from, play_from + 3, Target::MicPlay(i));
    }

    if failed {
        let laid: usize = segs.iter().map(|s| text_width(&s.text)).sum();
        let at = w.saturating_sub(actions.len() + GUTTER);
        if at > laid {
            segs.push(Seg::new(" ".repeat(at - laid), palette::base()));
            segs.push(Seg::new("retry", palette::ink()));
            segs.push(Seg::new(" · ", palette::muted()));
            segs.push(Seg::new("drop", palette::ink()));
            let a = at as u16;
            info = info.claim(a, a + 5, Target::MicRetry(i));
            info = info.claim(a + 8, a + 12, Target::MicDrop(i));
        }
    }
    (line_from(crop_with_ellipsis(segs, w), w), info)
}

/// Crop to `room` columns without an ellipsis segment — the row's own text is
/// already `one_line`'d and the band's outer crop handles the overflow case.
fn crop_plain(s: &str, room: usize) -> String {
    if text_width(s) <= room {
        return s.to_string();
    }
    let mut out = String::new();
    for g in unicode_segmentation::UnicodeSegmentation::graphemes(s, true) {
        if text_width(&out) + text_width(g) > room.saturating_sub(1) {
            break;
        }
        out.push_str(g);
    }
    out.push('…');
    out
}

/// **THE MIC BAND — what replaces the composer while mic mode is up.**
///
/// A border that says what the keys are, a STATE row that says unmistakably
/// whether the microphone is open, and then the queue: **every item**, his
/// ruling, spoken and typed together, each with the play button if there is a
/// recording behind it.
fn mic_band(vs: &ViewState, w: usize, rows: usize) -> (Vec<Line<'static>>, Vec<RowInfo>) {
    if rows == 0 {
        return (Vec::new(), Vec::new());
    }
    let mut out = Vec::with_capacity(rows);
    let mut infos: Vec<RowInfo> = Vec::new();
    out.push(line_from(crop_with_ellipsis(mic_title(vs, w), w), w));
    infos.push(RowInfo::bare(Target::Composer));

    let body_rows = rows.saturating_sub(2);
    let pad = " ".repeat(GUTTER);

    // ── the state row ────────────────────────────────────────────────────
    let (sev, said) = match &vs.mic {
        MicState::Recording => (
            Severity::Critical,
            "● RECORDING — r or space stops it".to_string(),
        ),
        MicState::Stopping => (Severity::Warn, "stopping…".to_string()),
        MicState::Refused(why) => (Severity::Critical, why.clone()),
        MicState::Idle => {
            let n = vs.composer.buf.pending().len();
            if n == 0 {
                (Severity::Unknown, "press r to record".to_string())
            } else {
                (
                    Severity::Calm,
                    format!("press r to record · enter sends all {n}"),
                )
            }
        }
    };
    out.push(line_from(
        crop_with_ellipsis(
            vec![
                Seg::new(pad.clone(), palette::base()),
                Seg::new(said, sev.style()),
            ],
            w,
        ),
        w,
    ));
    infos.push(RowInfo::bare(Target::Composer));

    // ── the queue: every item ────────────────────────────────────────────
    let items = vs.composer.buf.pending();
    let listed = body_rows.saturating_sub(1);
    let shown = items.len().min(listed);
    for (i, item) in items.iter().take(shown).enumerate() {
        let playing = vs.playing.is_some() && vs.playing.as_deref() == item.audio.as_deref();
        let (line, info) = client_row(item, i, w, playing, GUTTER);
        out.push(line);
        infos.push(info);
    }
    if items.len() > shown {
        // **IT SAYS WHAT IT IS NOT SHOWING.** A list that silently stops at the
        // bottom of a band is a list he cannot trust the length of.
        out.push(indented(
            match crate::keys::spelling(crate::Intent::QueueOverlay, Mode::Mic, vs.caps) {
                Some(k) => format!("{} more · {k} shows every item", items.len() - shown),
                None => format!("{} more", items.len() - shown),
            },
            palette::muted(),
            w,
        ));
        infos.push(RowInfo::bare(Target::Composer));
    }
    while out.len() < rows.saturating_sub(1) {
        out.push(size_line(vec![], w, palette::base()));
        infos.push(RowInfo::bare(Target::Composer));
    }
    // The bottom rule, so the band closes like the composer's does.
    if out.len() < rows {
        out.push(line_from(
            vec![Seg::new("─".repeat(w), palette::muted())],
            w,
        ));
        infos.push(RowInfo::bare(Target::Composer));
    }
    out.truncate(rows);
    infos.truncate(rows);
    (out, infos)
}

/// The mic band's border legend — the keys, where he is looking, exactly as the
/// composer's border carries its two acts.
fn mic_title(vs: &ViewState, w: usize) -> Vec<Seg> {
    let n = vs.composer.buf.pending().len();
    let waiting = vs.composer.buf.transcribing();
    let mut segs = vec![
        Seg::new("─".repeat(GUTTER), palette::muted()),
        Seg::new(" mic ", palette::muted()),
    ];
    if n > 0 {
        segs.push(Seg::new("─ ", palette::muted()));
        segs.push(Seg::new(format!("{n} held here "), Severity::Warn.style()));
    }
    if waiting > 0 {
        segs.push(Seg::new("─ ", palette::muted()));
        segs.push(Seg::new(
            format!("{waiting} transcribing "),
            Severity::Warn.style(),
        ));
    }
    segs.push(Seg::new("─ ", palette::muted()));
    segs.push(Seg::new("r", palette::ink()));
    segs.push(Seg::new(" record · ", palette::muted()));
    segs.push(Seg::new("enter", palette::ink()));
    segs.push(Seg::new(" SENDS all · ", palette::muted()));
    segs.push(Seg::new("esc", palette::ink()));
    segs.push(Seg::new(" leaves ", palette::muted()));
    let used: usize = segs.iter().map(|s| text_width(&s.text)).sum();
    if used < w {
        segs.push(Seg::new("─".repeat(w - used), palette::muted()));
    }
    segs
}

/// **THE BORDER LEGEND — the two acts, printed where he is looking.**
///
/// The operator's grammar has TWO acts and they must be unmistakable: *"in the
/// aui we use left to queue, and wildcard to send the whole payload."* So the
/// band's own border carries which key does which, permanently. That is a key
/// printed on the surface and it costs nothing against the key budget, because
/// the border was already being drawn.
///
/// It also carries the CLIENT queue's depth, and it is named apart from the
/// header's badge on purpose: the header counts what the DAEMON is holding for
/// this thread, and this counts what this composer has set aside and not sent.
/// One word each, so neither can be read as the other.
fn composer_title(c: &Composer, w: usize, caps: crate::keys::Caps) -> Vec<Seg> {
    let n = c.buf.pending().len();
    // **THE BAND NO LONGER NAMES ITSELF AND NO LONGER TEACHES THE BOX.** It
    // read `─ composer ─ enter SENDS all · alt-q queues · alt-enter newline`
    // (its spelling at the time; the queue key is `^Q` since round six),
    // and his answer to that was one sentence: *"You don't need to give me
    // instructions on how to use the textarea."* All four are gone — the word
    // `composer` with them, because a box with a caret in it under a thread's
    // transcript does not need a label either.
    //
    // **THE KEYS ARE NOT UNBOUND**, and none of this touches the table: `enter`
    // still sends, `^Q` still queues, `opt-enter` is still a newline, and
    // the `?` overlay still prints all three, because the overlay renders FROM
    // the table. What was removed is the permanent teaching row, not the
    // grammar.
    let mut segs = vec![Seg::new("─".repeat(GUTTER), palette::muted())];
    if n > 0 {
        segs.push(Seg::new("─ ", palette::muted()));
        segs.push(Seg::new(format!("{n} held here "), Severity::Warn.style()));
    }
    // **THE LEGEND IS DECORATION AND IT GOES FIRST** — `0040` §11's own 60
    // column frame draws a bare `├ composer ┤` where the 80 column one still
    // spells the keys out, and §11's order puts decoration ahead of everything.
    //
    // It is DROPPED rather than left to the crop. Measured at 60 before this,
    // the rule read `─ enter SENDS all · alt-q queues · alt-enter ne…`: a
    // legend that has lost the key it was naming teaches nothing and costs the
    // row forty columns to do it. The keys are all one `alt-/` away and the
    // footer names that binding at every width.
    //
    // `sending…` and the restore/discard offer are NOT decoration and stay:
    // one is the state of his words and the other is an offer he has to answer.
    //
    // **WHAT STAYS IS STATE, NOT INSTRUCTION**: `sending…` is the state of his
    // words on the wire, and the restore/discard offer is a question he has to
    // answer. Neither is a key he already knows.
    if c.sending {
        segs.push(Seg::new("─ ", palette::muted()));
        segs.push(Seg::new("sending… ", Severity::Warn.style()));
    } else if c.offer.is_some() {
        // **THE TWO KEYS COME FROM `keys`, not from this string.** They are the
        // MODAL answer to the offer — live only while the offer is — so they are
        // not rows of `BINDINGS`, and `keys::DRAFT_OFFER_*` is the one place
        // they are written down. This row and `App::on_intent` read the same
        // two constants, so the words on screen and the keys that work cannot
        // drift.
        segs.push(Seg::new("─ ", palette::muted()));
        segs.push(Seg::new(
            crate::keys::DRAFT_OFFER_ACCEPT.label(caps.mods),
            palette::ink(),
        ));
        segs.push(Seg::new(" restore · ", palette::muted()));
        segs.push(Seg::new(
            crate::keys::DRAFT_OFFER_DISCARD.label(caps.mods),
            palette::ink(),
        ));
        segs.push(Seg::new(" discard ", palette::muted()));
    }
    // Fill the rest of the rule, so the band reads as a band rather than as a
    // sentence with blank space after it. Nothing is filled when the legend
    // already over-runs the width — `crop_with_ellipsis` takes it from there.
    let used: usize = segs.iter().map(|s| text_width(&s.text)).sum();
    if used < w {
        segs.push(Seg::new("─".repeat(w - used), palette::muted()));
    }
    segs
}

/// **WHICH LAID-OUT ROW THE BAND SHOWS FIRST, AND HOW MANY IT SHOWS** — the
/// one derivation of the composer's own scroll.
///
/// It was inline in [`composer`] and is now called by it and by
/// [`composer_row_map`], because the two must agree about which row a drawn row
/// IS: a click resolved against a window one row out from the painted one would
/// put the caret on the wrong line, and nothing on screen would look wrong. The
/// window follows the CARET — a long draft must not scroll its own cursor off
/// the bottom — which is why `first` is a function of the caret and not of a
/// stored offset.
fn composer_window(c: &Composer, w: usize, rows: usize) -> (usize, usize) {
    // The queue's rows sit between the border and the text, so they come out
    // of the band's height before the text window is measured — in ONE place,
    // which both the painter and the click resolver read.
    let body_rows = rows.saturating_sub(2 + queue_band_rows(c));
    let (caret_row, _) = c.buf.caret(composer_inner(w));
    (
        caret_row.saturating_sub(body_rows.saturating_sub(1)),
        body_rows,
    )
}

/// **DRAWN ROW → LAID-OUT ROW, for every row of the band a caret may land on.**
///
/// Empty whenever the band is showing something that is not the buffer — a
/// recovered-draft offer, or the non-editable sentence — because a click on
/// either of those is not a caret move and must stay a silent miss. The title
/// rule and the bottom rule are absent for the same reason.
fn composer_row_map(c: &Composer, w: usize, rows: usize) -> BTreeMap<usize, usize> {
    let mut map = BTreeMap::new();
    if rows == 0 || !c.editable || c.offer.is_some() || c.stand_in.is_some() {
        return map;
    }
    let (first, body_rows) = composer_window(c, w, rows);
    let laid = c.buf.visual_rows(composer_inner(w));
    for i in 0..body_rows {
        // A drawn row past the end of the buffer is blank space below the text.
        // It resolves to the LAST laid row, which is where a click below the
        // text means — "the nearest legal position" read vertically.
        let row = (first + i).min(laid.saturating_sub(1));
        map.insert(i + 1 + queue_band_rows(c), row);
    }
    map
}

/// The composer band: a border that names both acts, then the wrapped buffer
/// with the caret drawn as a REVERSED cell.
///
/// **The caret is a reversed cell and not the terminal's own cursor**, and that
/// is deliberate: `term::CLAIMS` hides the terminal cursor (`?25l`) because a
/// caret parked in the corner reads as a hang, and a face that draws its own
/// caret keeps the cursor's position a property of the frame — which is the
/// only form a golden test can see. It also means the caret cannot disagree
/// with the text, because both come out of `layout`.
///
/// Returns the rows AND, for each, the [`RowText`] that makes it selectable —
/// the composer is a selectable surface like every other band (`0041`), and the
/// text a drag takes from it is the LOGICAL line, not the wrapped row.
fn composer(vs: &ViewState, w: usize, rows: usize) -> (Vec<Line<'static>>, Vec<RowInfo>) {
    let c = &vs.composer;
    if rows == 0 {
        return (Vec::new(), Vec::new());
    }
    let mut out = Vec::with_capacity(rows);
    let mut infos: Vec<RowInfo> = Vec::with_capacity(rows);
    // **NOTHING CAN BE SENT TO WHAT THE TRANSCRIPT IS SHOWING.** Two rows
    // instead of the band: what it is, why there is no box, and the key that
    // gets him back — plus the promise that his draft survives, because the
    // first thing a man who typed half a message wants to know is whether it
    // is gone. `Composer.buf` is untouched throughout.
    if let Some(s) = &c.stand_in {
        out.push(line_from(
            crop_with_ellipsis(
                vec![
                    Seg::new("─".repeat(GUTTER), palette::muted()),
                    Seg::new("─ ", palette::muted()),
                    Seg::new(one_line(&s.what), palette::ink()),
                    Seg::new(
                        " — runs inside claude code; nothing can be sent to it ".to_string(),
                        Severity::Warn.style(),
                    ),
                    Seg::new("─".repeat(w), palette::muted()),
                ],
                w,
            ),
            w,
        ));
        infos.push(RowInfo::bare(Target::Composer));
        if rows > 1 {
            out.push(line_from(
                crop_with_ellipsis(
                    vec![
                        Seg::new(" ".repeat(GUTTER), palette::base()),
                        Seg::new("← ".to_string(), palette::ink()),
                        Seg::new(
                            format!("returns to “{}”; your draft is kept", one_line(&s.back_to)),
                            palette::muted(),
                        ),
                    ],
                    w,
                ),
                w,
            ));
            infos.push(RowInfo::bare(Target::Composer));
        }
        for _ in out.len()..rows {
            out.push(size_line(vec![], w, palette::base()));
            infos.push(RowInfo::bare(Target::Composer));
        }
        return (out, infos);
    }
    out.push(line_from(crop_with_ellipsis(composer_title(c, w, vs.caps), w), w));
    infos.push(RowInfo::bare(Target::Composer));

    let inner = composer_inner(w);
    let q = queue_band_rows(c);
    let body_rows = rows.saturating_sub(2 + q);
    let pad = " ".repeat(GUTTER);

    // **THE CLIENT QUEUE, WHERE HE CAN REACH IT.** *"just make the queue
    // accessible by click or by navigating above or below the textarea."* The
    // rows are drawn by the SAME `client_row` mic mode and the queue view use,
    // so a queued message looks the same wherever it is shown and a state
    // added later cannot appear on one screen and not another.
    //
    // The row the QUEUE CURSOR is on is reverse-video — but only while the
    // queue actually HOLDS the selection, because a highlight on a band the
    // arrows are not acting on is a lie about where the keyboard is.
    if q > 0 {
        let first = queue_window(c, vs.queue_cursor);
        let selected = matches!(vs.mode, Mode::Thread(Region::Queue));
        let cursor = vs.queue_cursor.min(c.buf.pending().len().saturating_sub(1));
        for k in 0..q {
            let i = first + k;
            let Some(item) = c.buf.pending().get(i) else {
                break;
            };
            let playing = vs.playing.as_deref().is_some()
                && item.audio.as_deref() == vs.playing.as_deref();
            let (line, info) = client_row(item, i, w, playing, GUTTER);
            out.push(if selected && i == cursor {
                geom::highlight(line, 0, w)
            } else {
                line
            });
            infos.push(info);
        }
    }

    if let Some(offer) = &c.offer {
        // **THE OFFER, NOT THE DRAFT.** It is shown as a quotation of something
        // found on disk, never as text sitting in the box — a stale draft that
        // rendered as current state is the one failure this whole mechanism is
        // designed around.
        let head = format!("a draft from {} was recovered: ", offer.age);
        let quote: String = offer
            .text
            .chars()
            .take(inner.saturating_sub(head.chars().count() + 1))
            .collect();
        out.push(line_from(
            vec![
                Seg::new(pad.clone(), palette::base()),
                Seg::new(head, Severity::Warn.style()),
                Seg::new(one_line(&quote), palette::muted()),
            ],
            w,
        ));
        for _ in 1..body_rows.max(1) {
            out.push(size_line(vec![], w, palette::base()));
        }
    } else if !c.editable {
        // Kept, and it should never be reachable: the band is drawn only in a
        // thread and a thread's composer is editable. If it ever renders, the
        // honest sentence is better than an input that looks ready and is not.
        out.push(line_from(
            vec![
                Seg::new(pad.clone(), palette::base()),
                Seg::new("nothing to compose to from here", Severity::Unknown.style()),
            ],
            w,
        ));
        for _ in 1..body_rows.max(1) {
            out.push(size_line(vec![], w, palette::base()));
        }
    } else {
        let laid = c.buf.layout(inner);
        let (caret_row, caret_col) = c.buf.caret(inner);
        // **AN INACTIVE BOX DRAWS NO CARET.** His words: *"I can see the
        // cursor is still there."* The caret was painted whenever the band was
        // drawn, which is every frame of a thread — so while he was reading
        // the chat the screen went on showing a live cursor in a box that was
        // (from this round) no longer taking his keys. He reads the screen for
        // state, and a caret is the strongest claim this face can make about
        // where the keyboard is.
        //
        // The caret's POSITION is untouched: it is remembered exactly where he
        // left it and is drawn again the moment the box takes the selection
        // back. What is gone is the claim, not the state.
        let live = vs.mode.textarea_is_live();
        // **THE WINDOW FOLLOWS THE CARET.** With more rows than the band holds,
        // the band shows the rows AROUND the caret rather than the first ones —
        // otherwise a long draft scrolls its own cursor off the bottom, which
        // is the most basic textarea glitch there is. `composer_row_map` reads
        // the SAME function, so a click cannot resolve against a different
        // window than the one on screen.
        let (first, _) = composer_window(c, w, rows);
        let empty = c.buf.lines().iter().all(String::is_empty);
        for i in 0..body_rows {
            let Some(row) = laid.get(first + i) else {
                out.push(size_line(vec![], w, palette::base()));
                continue;
            };
            let text = c.buf.row_text(*row);
            let mut segs = vec![Seg::new(pad.clone(), palette::base())];
            if empty && first + i == 0 {
                // **THE CARET GETS A CELL OF ITS OWN HERE**, before the
                // placeholder, rather than reversing the placeholder's first
                // letter — which reads as a typo rather than as a cursor. SHORT
                // prose, too, because the border directly above already carries
                // the grammar: a placeholder that repeats the legend is two rows
                // of the same sentence in a band three rows tall.
                segs.push(Seg::new(" ", palette::base()));
                segs.push(Seg::new("type a message…", Severity::Unknown.style()));
            } else {
                segs.push(Seg::new(text.to_string(), palette::ink()));
            }
            let line = line_from(segs, w);
            // The caret is painted by RESTYLING the drawn row, exactly the way
            // a drag selection is — so it cannot change the row's width and the
            // shape law survives it by construction.
            out.push(if live && first + i == caret_row {
                caret(line, GUTTER + caret_col)
            } else {
                line
            });
        }
    }

    // **THE BOTTOM RULE CLOSES THE BAND, and it earns its row.** Without it the
    // composer's text sits directly on the footer's key hints and the two read
    // as one line of chrome.
    //
    // **THE CARET READOUT IS GONE.** It read `line 10 of 10 · col 50` whenever
    // the draft outgrew the band, and his answer to it was one sentence: *"I
    // don't need to see this in the textarea."* It went with the legend above
    // it and for the same reason — a text box does not need to narrate itself
    // to the person typing in it.
    if rows >= 2 {
        out.push(line_from(
            vec![Seg::new("─".repeat(w), palette::muted())],
            w,
        ));
    }
    out.truncate(rows);
    while out.len() < rows {
        out.push(size_line(vec![], w, palette::base()));
    }
    // Every row past the ones the queue claimed is the composer's own surface:
    // the text rows resolve to a caret through `composer_row_map` in the
    // caller, and the border and the pad rows are plain band.
    while infos.len() < out.len() {
        infos.push(RowInfo::bare(Target::Composer));
    }
    infos.truncate(out.len());
    (out, infos)
}

/// Fold any newline out of a string so it can go on one row. Only reachable for
/// the draft OFFER, which quotes a multi-line draft on a single line.
fn one_line(s: &str) -> String {
    s.replace('\n', " ⏎ ")
}

/// Reverse ONE column of an already-sized line: the caret.
///
/// Deliberately `geom::highlight` with a one-column range rather than a second
/// mechanism. The caret and the drag selection are the same act — say which
/// cell the eye should go to — and one of them being drawn a different way is
/// how the two end up disagreeing about a double-width glyph.
fn caret(line: Line<'static>, col: usize) -> Line<'static> {
    geom::highlight(line, col, col + 1)
}

// ── band 5: the footer ───────────────────────────────────────────────────────

/// **THE FOOTER — the keybind bar, with the info-notice area floating at its
/// right-hand end.**
///
/// His rule for the collision, verbatim: *"If it's longer than the space
/// available, have it sit over the top of the keybind messages."* So the order
/// of operations is **notice first, hints into what is left** — which is the
/// opposite of what this function did before, and the difference is the whole
/// behaviour.
///
/// The old order laid the hints, measured the notice, and drew it only when
/// what was left over was already wide enough. That is a footer in which a long
/// message is not truncated and not overlaid — it is **DROPPED ENTIRELY**,
/// silently, in favour of key hints he already knows. A notice that disappears
/// because it had something long to say is the exact failure the notice area
/// exists to fix.
///
/// **The notice never takes the whole row**: it is capped at `w - GUTTER`, so
/// there is always a left margin and the row never reads as a full-width
/// banner, which is a different band with different rules ([`Banner`]).
fn footer_line(vs: &ViewState, now: DateTime<Utc>, w: usize) -> Line<'static> {
    let mut segs: Vec<Seg> = vec![Seg::new(" ".repeat(GUTTER), palette::base())];
    let mut used = GUTTER;

    // THE NOTICE FIRST. Its width is claimed before a single hint is laid, so
    // a long one takes the columns the hints would have used rather than being
    // dropped for want of room.
    let notice = notice_seg(vs, now, w);
    let notice_w = notice.as_ref().map_or(0, |s| text_width(&s.text));
    // Two columns of air between the last hint and the notice, so they never
    // read as one run of text.
    let hint_budget = w.saturating_sub(notice_w + if notice_w == 0 { 0 } else { 2 });

    // **THE LEGEND IS PER-SECTION, WITH THE INVARIANT KEYS PINNED LEFT.** His
    // words: *"each section, when in that band, should display its own
    // keybindings, not all of them at once. Keep the invariant ones at the
    // left so its easy to see that they stay the same."*
    //
    // The STABILITY is the feature, not the filtering: the left run is the
    // same sequence of hints in every region, so it lands in the same COLUMNS
    // in every region and the only things that move are the ones that are new.
    // The `┊` is where one half ends and the other begins — a different mark
    // from the `·` between hints, so the boundary is visible rather than
    // inferred. `keys::footer_split` decides which hints are which, by asking
    // the table rather than by carrying a list.
    let (stable, variable) = keys::footer_split(vs.mode, vs.caps);
    let n_stable = stable.len();
    for (i, (key, short)) in stable.into_iter().chain(variable).enumerate() {
        let boundary = i == n_stable && n_stable > 0;
        let frag = format!("{key} {short}");
        let cost = text_width(&frag) + if used > GUTTER { 3 } else { 0 };
        if used + cost > hint_budget {
            break;
        }
        if used > GUTTER {
            segs.push(Seg::new(
                if boundary { " ┊ " } else { " · " },
                palette::muted(),
            ));
        }
        segs.push(Seg::new(key, palette::ink()));
        segs.push(Seg::new(format!(" {short}"), palette::muted()));
        used += cost;
    }

    if let Some(notice) = notice {
        // Right-aligned, always: the notice's left edge moves with its length
        // and its right edge does not, so his eye has one place to look.
        segs.push(Seg::new(
            " ".repeat(w.saturating_sub(used + notice_w)),
            palette::base(),
        ));
        segs.push(notice);
    }
    line_from(crop_with_ellipsis(segs, w), w)
}

/// The footer's right-hand end: the info-notice area, or nothing.
///
/// **The `mouse: face / mouse: term` indicator is GONE**, with the `M` key it
/// was the state of. An indicator exists so a toggle's position is not a coin
/// flip; with the toggle deleted it was a permanent label reading `mouse:
/// face`, which is not a fact he can act on and is eleven columns the notice
/// can have instead.
///
/// **Expiry happens HERE, in the render, rather than in the shell's model.**
/// The shell wakes the loop at the deadline so the frame is redrawn; but the
/// decision that an expired notice is not drawn belongs to the one function
/// that has both the notice and the `now`. A model that deleted the notice on a
/// timer would have a window in which the field says one thing and the screen
/// says another.
fn notice_seg(vs: &ViewState, now: DateTime<Utc>, w: usize) -> Option<Seg> {
    let n = vs.notice.as_ref()?;
    if !n.live_at(now) {
        return None;
    }
    // Never the whole row: a full-width message at the bottom is a banner, and
    // a banner means something else (`DEC-13`).
    let cap = w.saturating_sub(GUTTER);
    if cap == 0 || n.text.is_empty() {
        return None;
    }
    let style = n.severity.style();
    // Cropped WITH THE ELLIPSIS, like every other cut this crate makes: a
    // silent crop makes "that is all of it" and "there is more" look identical,
    // and a truncated warning that reads as complete is worse than none.
    let text: String = if text_width(&n.text) > cap {
        crop_with_ellipsis(vec![Seg::new(n.text.clone(), style)], cap)
            .into_iter()
            .map(|s| s.text)
            .collect()
    } else {
        n.text.clone()
    };
    Some(Seg::new(text, style))
}

// ── the overlays ─────────────────────────────────────────────────────────────

/// A titled overlay frame: a rule with a name, the caller's rows, padding.
/// **THE SHEET'S ONE FACTS LINE**, composed at FRAME time because half of it is
/// clock-relative.
///
/// `harness · started Sep 12 · 41 turns · last turn 2m ago`. A fact we were not
/// given is `?` and never a zero and never a blank — honesty rule 1, which
/// applies to a date exactly as it applies to a count.
fn sheet_facts_line(f: &Facts, now: DateTime<Utc>, tz: i32) -> String {
    match f {
        Facts::Thread(t) => thread_facts_line(t, now, tz),
        Facts::Project(p) => project_facts_line(p, tz),
    }
}

/// **THE PROJECT SHEET'S FACTS LINE** — `48 threads · 1 running · oldest Sep
/// 2025` (§5's second frame).
///
/// Three rules, each of them this face's standing ones rather than new:
///
/// - **The running count appears only when it is a nonzero KNOWN.** §1's rule
///   for the root row — *"a column of zeros is noise"* — and honesty rule 1's:
///   `Unknown` is not `0`, so an undecorated project says `? running` rather
///   than claiming nothing is executing.
/// - **`oldest` is a month and a year**, because it answers "how long has this
///   been going" and a day would be a precision the question does not have.
///   `None` is `?`.
/// - **`miscellaneous` SAYS WHAT IT IS, in words, on this line.** Its sheet is
///   missing two rows that every other project's sheet has, and the explanation
///   belongs where he is looking rather than in a silence he has to interpret.
fn project_facts_line(p: &ProjectFacts, tz: i32) -> String {
    let mut parts = vec![format!(
        "{} thread{}",
        p.threads,
        if p.threads == 1 { "" } else { "s" }
    )];
    match p.running {
        Known::Known(0) => {}
        ref a => parts.push(format!("{} running", a.label())),
    }
    parts.push(match p.oldest {
        Some(t) => {
            let off = FixedOffset::east_opt(tz * 60)
                .unwrap_or_else(|| FixedOffset::east_opt(0).expect("UTC is a valid offset"));
            format!("oldest {}", t.with_timezone(&off).format("%b %Y"))
        }
        None => "oldest ?".to_string(),
    });
    if p.bucket {
        parts.push("the un-projected bucket — it is not archivable and not removable".to_string());
    }
    parts.join(" · ")
}

fn thread_facts_line(f: &SheetFacts, now: DateTime<Utc>, tz: i32) -> String {
    let started = match f.started {
        Some(t) => {
            let off = FixedOffset::east_opt(tz * 60)
                .unwrap_or_else(|| FixedOffset::east_opt(0).expect("UTC is a valid offset"));
            t.with_timezone(&off).format("%b %-d").to_string()
        }
        None => "?".to_string(),
    };
    let last = match f.last_turn {
        Some(t) => format!("{} ago", geom::fmt_age(now - t)),
        None => "?".to_string(),
    };
    format!(
        "{} · started {started} · {} turn{} · last turn {last}",
        f.project,
        f.turns,
        if f.turns == 1 { "" } else { "s" },
    )
}

/// **THE SHEET — facts on top, acts below (§5).**
///
/// Returns its own [`RowInfo`]s rather than letting the caller stamp
/// `Target::Overlay` on every row, because a sheet row that dismissed on click
/// would be the opposite of what it is: `hit.rs`'s own header names *"sheet
/// action rows"* among the explicit affordances that still ACTIVATE on click,
/// in the same breath as the ruling that a plain row only moves the cursor.
///
/// **EVERY ROW IS DRAWN IN EVERY STATE.** There is no arm here that removes a
/// row, and there must never be one: a row that comes and goes reshapes the
/// menu under his muscle memory (his own ruling, already applied to
/// `archived (0)`). An act whose owner is unreachable keeps its position, goes
/// muted, and carries the reason in words — see [`SheetRow::words`].
fn sheet_body(
    vs: &ViewState,
    now: DateTime<Utc>,
    rows: usize,
    w: usize,
) -> (Vec<Line<'static>>, Vec<RowInfo>) {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut infos: Vec<RowInfo> = Vec::new();
    let Some(sheet) = vs.sheet.as_ref() else {
        // A sheet mode with no sheet is a shell bug, not a state. It is drawn
        // as an honest empty rather than panicking inside a render.
        out.push(indented("no sheet", palette::muted(), w));
        infos.push(RowInfo::bare(Target::Overlay));
        while out.len() < rows {
            out.push(size_line(vec![], w, palette::base()));
            infos.push(RowInfo::bare(Target::Empty));
        }
        out.truncate(rows);
        infos.truncate(rows);
        return (out, infos);
    };

    // ── THE FACTS LINE ──────────────────────────────────────────────────
    // Absent deliberately (§5): the account, the working directory, the id.
    // *"The sheet describes your thread, not the daemon's row."*
    out.push(indented(
        sheet_facts_line(&sheet.facts, now, vs.tz_offset_minutes),
        palette::ink2(),
        w,
    ));
    infos.push(RowInfo::bare(Target::Overlay));
    out.push(size_line(vec![], w, palette::base()));
    infos.push(RowInfo::bare(Target::Overlay));

    // ── THE ACTS ────────────────────────────────────────────────────────
    for (i, row) in sheet.rows.iter().enumerate() {
        let selected = i == sheet.cursor && vs.mode == Mode::Sheet;
        let (marker, style) = match (selected, row.avail.is_ready()) {
            (true, _) => ("▸ ", palette::header()),
            // **MUTED, NOT RED.** An owner that is not running is a degraded
            // state, not a failure — and staleness is never red (palette rule
            // 1). Red here would make a stopped ui-server read like a crash.
            (false, false) => ("  ", palette::muted()),
            (false, true) => ("  ", palette::ink2()),
        };
        // **THE ROW'S SENTENCE WRAPS; IT DOES NOT TRUNCATE.**
        //
        // This is the one case `0040` §11 could not have ruled on, because the
        // sheet did not exist when it was drawn, and it is the hardest one: the
        // words are CONTENT (the face speaking to him) and also METADATA (a
        // fact about this row), which is the only place §11's ordering is
        // genuinely ambiguous.
        //
        // It is ruled as content, and the argument is the measurement. At 60
        // columns the unreachable-owner rows — the longest strings on the face
        // — hard-cropped to `— needs the audio interface, which is not runnin`,
        // `which i`, `which is not ru`: a different lie on every row, none of
        // them marked. The reader cannot tell whether the sentence said "not
        // running" or "not running because X", and a truncated EXPLANATION
        // reads as a broken face rather than as a narrow one. This crate
        // already holds that argument, in `wrapped_note`'s own words: "a
        // limitation whose reason is cut off at the right margin is worse than
        // no note".
        //
        // A short complete sentence would also satisfy that, but it is not
        // available here — the sentence is the AUI's and is relayed verbatim
        // (0063), so shortening it would be paraphrasing an owner's refusal.
        // Wrapping keeps it whole and keeps it his.
        //
        // **EVERY WRAPPED ROW CLAIMS THE SAME `SheetRow(i)`**, so the act is
        // one door however many rows it took. A continuation row that resolved
        // to nothing would be a dead strip inside a live control.
        let indent = GUTTER + text_width(marker);
        let wrapped = geom::wrap(
            &[Seg::new(row.words(), style)],
            w.saturating_sub(indent).max(1),
        );
        for (n, vr) in wrapped.into_iter().enumerate() {
            out.push(line_from(
                vec![
                    Seg::new(" ".repeat(GUTTER), palette::base()),
                    Seg::new(if n == 0 { marker } else { "  " }, style),
                    Seg::new(vr.text(), style),
                ],
                w,
            ));
            infos.push(RowInfo::bare(Target::SheetRow(i)));
        }
    }

    // ── THE TYPED FIELD, when one is open ───────────────────────────────
    if let Some(field) = sheet.field.as_ref() {
        out.push(size_line(vec![], w, palette::base()));
        infos.push(RowInfo::bare(Target::Overlay));
        // The field names ITSELF in its own border, exactly as the composer
        // band does — so he never has to remember which row opened it.
        let label = format!(" {} — enter does it · esc keeps it ", field.label);
        out.push(line_from(
            vec![
                Seg::new("─".repeat(GUTTER), palette::muted()),
                Seg::new(label.clone(), palette::header()),
                Seg::new(
                    "─".repeat(w.saturating_sub(GUTTER + text_width(&label))),
                    palette::muted(),
                ),
            ],
            w,
        ));
        infos.push(RowInfo::bare(Target::Overlay));
        let inner = w.saturating_sub(GUTTER + 1).max(1);
        for vr in field.buf.layout(inner) {
            out.push(line_from(
                vec![
                    Seg::new(" ".repeat(GUTTER + 1), palette::base()),
                    Seg::new(field.buf.row_text(vr).to_string(), palette::ink()),
                ],
                w,
            ));
            infos.push(RowInfo::bare(Target::Overlay));
        }
    }

    // Pad to the band. The shape law is absolute: exactly `rows` lines.
    while out.len() < rows {
        out.push(size_line(vec![], w, palette::base()));
        infos.push(RowInfo::bare(Target::Empty));
    }
    out.truncate(rows);
    infos.truncate(rows);
    (out, infos)
}

/// **THE ARCHIVED SET, DRAWN** (§1's second frame).
///
/// One row per archived project, `<name> — click to bring it back`, its age on
/// the right. Empty says so in a sentence, because empty is a state that gets
/// sentences on this face and never a blank screen.
///
/// **THE UNREACHABLE OWNER IS A SENTENCE ON THE ROW**, exactly as it is on the
/// sheet: the row stays at its position, goes muted, and says why. Activating
/// it repeats the sentence once and sends nothing. The ROOM still opens, though
/// — it is read off the filesystem, not off 9010 — because nothing this face
/// renders may be sourced from the AUI's socket.
fn archived_body(
    view: &ArchivedView,
    now: DateTime<Utc>,
    rows: usize,
    w: usize,
) -> (Vec<Line<'static>>, Vec<RowInfo>) {
    if rows == 0 {
        return (Vec::new(), Vec::new());
    }
    if view.rows.is_empty() {
        return empty_state(
            "nothing is archived — archive a project from its sheet and it waits here",
            rows,
            w,
        );
    }
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut infos: Vec<RowInfo> = Vec::new();
    for (i, r) in view.rows.iter().enumerate() {
        let selected = i == view.cursor;
        let (marker, style) = match (selected, r.avail.is_ready()) {
            (true, _) => ("▸ ", palette::header()),
            // Muted, not red: an owner that is not running is a degraded state,
            // not a failure (palette rule 1).
            (false, false) => ("  ", palette::muted()),
            (false, true) => ("  ", palette::ink2()),
        };
        let age = format!(
            "{:>8}",
            match r.since {
                Some(t) => geom::fmt_age(now - t),
                None => "?".into(),
            }
        );
        let words = r.words();
        let head = vec![
            Seg::new(marker, palette::ink()),
            Seg::new(words.clone(), style),
        ];
        let left_budget = w.saturating_sub(text_width(&age) + 1);
        let mut segs = crop_with_ellipsis(head, left_budget);
        let used: usize = segs.iter().map(|s| text_width(&s.text)).sum();
        segs.push(Seg::new(
            " ".repeat(w.saturating_sub(used + text_width(&age))),
            palette::base(),
        ));
        segs.push(Seg::new(
            age,
            match r.since {
                Some(_) => palette::muted(),
                None => Severity::Unknown.style(),
            },
        ));
        out.push(line_from(segs, w));
        infos.push(RowInfo::new(
            Target::ArchivedRow(i),
            Some(RowText {
                unit: Unit::new(UnitKind::Content, i),
                start: 0,
                col0: 2,
                text: words,
            }),
        ));
    }
    while out.len() < rows {
        out.push(size_line(vec![], w, palette::base()));
        infos.push(RowInfo::bare(Target::Empty));
    }
    out.truncate(rows);
    infos.truncate(rows);
    (out, infos)
}

fn framed(
    title: &str,
    mut rows_in: Vec<Line<'static>>,
    rows: usize,
    w: usize,
) -> Vec<Line<'static>> {
    let label = format!(" {title} ");
    let mut out: Vec<Line<'static>> = Vec::with_capacity(rows);
    out.push(line_from(
        vec![
            Seg::new("─".repeat(GUTTER), palette::muted()),
            Seg::new(label.clone(), palette::header()),
            Seg::new(
                "─".repeat(w.saturating_sub(GUTTER + text_width(&label))),
                palette::muted(),
            ),
        ],
        w,
    ));
    rows_in.truncate(rows.saturating_sub(1));
    out.extend(rows_in);
    while out.len() < rows {
        out.push(size_line(vec![], w, palette::base()));
    }
    out.truncate(rows);
    out
}

/// **A NOTE, WRAPPED ACROSS AS MANY ROWS AS IT NEEDS.**
///
/// [`indented`] sizes one line, so anything longer than the window is CROPPED —
/// which is fine for a row of a table and wrong for a sentence. The `?`
/// overlay's three standing notes are sentences, and two of them exist to
/// explain a LIMITATION: `shift-enter` is not available here, and an input
/// method's composition is invisible to this face. A limitation whose reason is
/// cut off at the right margin is worse than no note, because it reads as the
/// face being broken rather than as the terminal being unable.
fn wrapped_note(text: &str, style: ratatui::style::Style, w: usize) -> Vec<Line<'static>> {
    let indent = GUTTER + 1;
    let inner = w.saturating_sub(indent + 1).max(1);
    geom::wrap(&[Seg::new(text, style)], inner)
        .into_iter()
        .map(|row| {
            line_from(
                vec![
                    Seg::new(" ".repeat(indent), palette::base()),
                    Seg::new(row.text(), style),
                ],
                w,
            )
        })
        .collect()
}

/// **AN INDENTED ROW, AND IT SAYS SO WHEN IT DID NOT FIT.**
///
/// It used to end in [`line_from`] alone, which crops by dropping whatever
/// straddles the last column and pads the rest — SILENTLY. Every one-line
/// sentence on this face goes through here, so at 60 columns the measured
/// result was a page of statements that simply stopped: the sheet's facts line
/// read `last turn 11h 22m ag`, the queue overlay's read `cancel and re-select
/// ar`, and each stopped at a different place with nothing anywhere saying a
/// cut had happened. A sentence that ends mid-word without a mark does not read
/// as truncated — it reads as a face that is broken.
///
/// The `…` costs one column and buys the only thing that matters here: he can
/// tell what he is not being shown.
fn indented(text: impl Into<String>, style: ratatui::style::Style, w: usize) -> Line<'static> {
    line_from(
        crop_with_ellipsis(
            vec![
                Seg::new(" ".repeat(GUTTER + 1), palette::base()),
                Seg::new(text.into(), style),
            ],
            w,
        ),
        w,
    )
}

/// The `?` overlay has no list of its own. It renders [`keys::BINDINGS`],
/// which is why a key cannot be renamed without the help text following it —
/// and why a MODE cannot gain a grammar he has no way to find.
///
/// **THE SHARED GRAMMAR IS PRINTED ONCE.** It used to be repeated inside every
/// mode's section, and `0036` measured what that cost on his pane: 62 binding
/// rows against 33 rows of room, with the overlay's own amber "…more keys than
/// fit" on the last line — *the one mechanism the old philosophy named as its
/// discoverability guarantee could not show even half of what it guaranteed.*
/// Cutting keys alone does not fix that and a fourth body mode makes it worse,
/// so the repetition goes: one "everywhere" section, then per-mode DELTAS.
/// [`keys::overlay_rows`] derives the rows from the table itself, so a
/// binding that stops being universal moves into its mode's section on its own.
/// **THE `?` OVERLAY: the grammar of the view he is standing in, and it
/// SCROLLS.**
///
/// Two changes the composer forced, both of them arithmetic rather than taste.
///
/// **It is CONTEXTUAL.** A thread's grammar is forty bindings by itself — a
/// textarea has that many keys — so printing all six modes made the body
/// ninety-odd rows against the thirty-three his pane has. `0036` already fixed
/// this overlay once by factoring out the block every mode shares; the composer
/// went straight through that fix. What is left is to print the shared block
/// plus the ONE mode the overlay was raised over, which is also the question he
/// is actually asking.
///
/// **And it SCROLLS.** `0036` deleted Help's `↑`/`↓` because they moved the
/// view UNDERNEATH the overlay and never the overlay — a binding that did the
/// wrong thing quietly, which is worse than no binding. The honest repair is
/// not to leave them deleted, it is to give the overlay something to scroll:
/// `help_from` is that, and the keys are back doing what they always said they
/// did. A thread's grammar does not fit in his window at any factoring, so
/// "make it fit" had run out of room.
fn help_overlay(
    rows: usize,
    w: usize,
    caps: keys::Caps,
    for_mode: Mode,
    from: usize,
) -> Vec<Line<'static>> {
    let mut body: Vec<Line<'static>> = Vec::new();
    let section =
        |title: String, table: Vec<(String, &'static str)>, body: &mut Vec<Line<'static>>| {
            if table.is_empty() {
                return;
            }
            // A BLANK ROW BETWEEN SECTIONS COSTS A BINDING ROW, and this overlay
            // does not have rows to spend: measured at 150x40 after the shared
            // section landed, the body still ran seven rows over and the last two
            // sections were cut. The indented heading is separation enough.
            if !body.is_empty() {
                body.push(size_line(vec![], w, palette::base()));
            }
            body.push(indented(title, palette::header(), w));
            let keycol = table.iter().map(|(k, _)| text_width(k)).max().unwrap_or(0);
            for (k, act) in table {
                let pad = keycol.saturating_sub(text_width(&k));
                body.push(line_from(
                    vec![
                        Seg::new(" ".repeat(GUTTER + 3), palette::base()),
                        Seg::new(k, palette::ink()),
                        Seg::new(" ".repeat(pad + 2), palette::base()),
                        Seg::new(act, palette::ink2()),
                    ],
                    w,
                ));
            }
        };
    // **THE MODE'S OWN ROWS, IN FULL, AND NOTHING ELSE.** There used to be an
    // `everywhere:` section factored out of the six modes, because printing the
    // common block six times cost more rows than his pane has. A contextual
    // overlay prints ONE mode, so there is nothing to factor — and the factoring
    // had become wrong as well as unnecessary: raised over a thread it printed
    // `q quit`, `f filter` and `G newest`, none of which were bindings there
    // (and the last two are not bindings anywhere now — round six retired
    // them).
    section(
        format!("{}:", for_mode.label()),
        keys::overlay_rows(for_mode, caps),
        &mut body,
    );
    // **WHAT THIS TERMINAL CANNOT DELIVER, NAMED.** Omitting an unavailable key
    // is honest; naming it is USEFUL, because he will press `shift-enter`
    // anyway and nothing else on screen would ever tell him why it sent.
    let missing = keys::withheld(for_mode, caps);
    if !missing.is_empty() {
        body.push(size_line(vec![], w, palette::base()));
        body.extend(wrapped_note(
            &format!(
                "not available in this terminal: {} — it cannot report them as \
                 distinct from the unmodified key, so they are not bound. {} \
                 is the newline here.",
                missing.join(", "),
                keys::spelling(
                    crate::Intent::Compose(crate::Edit::Newline),
                    for_mode,
                    caps
                )
                .unwrap_or_else(|| "nothing".to_string())
            ),
            palette::amber(),
            w,
        ));
    }
    body.push(size_line(vec![], w, palette::base()));
    body.extend(wrapped_note(
        "a click activates whatever is under it — never its own action. every other \
         view has its own keys; press ? there.",
        palette::muted(),
        w,
    ));
    if matches!(for_mode, Mode::Thread(_)) {
        // **THE IME LIMITATION, STATED RATHER THAN CLAIMED.** The terminal owns
        // the pre-edit and there is no protocol by which an application learns
        // that a composition is open — every tool in the survey has this gap and
        // the ones that do not say so are the ones that surprise you. What IS in
        // our control is not stealing `enter` out of the middle of a
        // composition, and the honest report of the rest is this line.
        body.extend(wrapped_note(
            &format!(
                "typing CJK through an input method: the terminal owns the composition \
                 and cannot tell this face when one is open. enter may commit mid-word. \
                 {} is safe.",
                keys::spelling(
                    crate::Intent::Compose(crate::Edit::Newline),
                    Mode::Thread(Region::Composer),
                    caps
                )
                .unwrap_or_else(|| "the newline key".to_string())
            ),
            palette::muted(),
            w,
        ));
    }

    // **THE WINDOW, AND THE HONEST LINE UNDER IT.** One row of the frame is the
    // title and one is the scroll note when there is anything to scroll to, so
    // the body gets what is left.
    let total = body.len();
    let note = total + 2 > rows;
    let page = rows.saturating_sub(if note { 2 } else { 1 });
    let from = from.min(total.saturating_sub(page));
    let window: Vec<Line<'static>> = body.into_iter().skip(from).take(page).collect();
    let mut out = framed("keys", window, rows, w);
    if note && rows > 1 {
        // A help screen that silently ends mid-list is the same defect as a
        // transcript that silently starts where the buffer does — so it says
        // where it is and which keys move it.
        let shown = (from + page).min(total);
        out[rows - 1] = indented(
            format!(
                "{}–{shown} of {total} · ↑ ↓ pgup pgdn to move · any other key closes",
                from + 1
            ),
            palette::amber(),
            w,
        );
    }
    out
}

/// `u` — DIR-6's second requirement. **VIEW ONLY in this build**, and it says
/// so on its own last line rather than offering a cancel or a re-select that
/// would be a mutation this step is forbidden to send.
fn queue_overlay(q: &QueueView, rows: usize, w: usize) -> Vec<Line<'static>> {
    let title = if q.thread.is_empty() {
        "queue".to_string()
    } else {
        format!("queue · {}", q.thread)
    };
    framed(&title, queue_overlay_body(q, w), rows, w)
}

/// The daemon half of the queue view, without its frame.
///
/// **Split out of [`queue_overlay`] rather than copied**, so that the version
/// with the client's queue spliced under it
/// ([`queue_overlay_with_client`]) renders the daemon's rows through the
/// same code. Two renderings of one list is how the two come to disagree.
fn queue_overlay_body(q: &QueueView, w: usize) -> Vec<Line<'static>> {
    let mut body: Vec<Line<'static>> = Vec::new();
    match &q.state {
        Fetch::Idle => body.push(indented(
            "no thread in focus — open one, or select one in the list",
            Severity::Unknown.style(),
            w,
        )),
        Fetch::Loading => body.push(indented("asking the daemon…", palette::amber(), w)),
        // A surface that cannot say why it is empty reads as empty, which is
        // the difference between "nothing is queued" and "we never found out".
        Fetch::Failed(why) => body.push(indented(
            format!("could not read the queue: {why}"),
            palette::red(),
            w,
        )),
        Fetch::Loaded => {
            body.push(indented(
                format!("waiting ({})", q.waiting.len()),
                palette::header(),
                w,
            ));
            if q.waiting.is_empty() {
                body.push(indented("nothing waiting", palette::muted(), w));
            }
            for it in &q.waiting {
                body.push(queue_row(it, Severity::Calm, w));
            }
            body.push(size_line(vec![], w, palette::base()));
            // THE FAILED SECTION IS LOAD-BEARING. It is where an exhausted
            // retry ladder ends up — the daemon built `queue_reselect` so that
            // is never a silent drop — and a queue view that omitted it would
            // hide exactly the rows that most need a human.
            body.push(indented(
                format!("failed ({})", q.failed.len()),
                if q.failed.is_empty() {
                    palette::header()
                } else {
                    palette::red()
                },
                w,
            ));
            if q.failed.is_empty() {
                body.push(indented("nothing failed", palette::muted(), w));
            }
            for it in &q.failed {
                body.push(queue_row(it, Severity::Critical, w));
            }
        }
    }
    body.push(size_line(vec![], w, palette::base()));
    body.push(indented(
        "the daemon's queue is view only — cancel and re-select are mutations it does not yet send",
        palette::muted(),
        w,
    ));
    body
}

/// **THE QUEUE VIEW, WITH THE CLIENT'S OWN QUEUE UNDER THE DAEMON'S.**
///
/// `DIR-6` asks for a way to view the queue's contents on demand, and mic mode
/// gives that list a second half: *"the queue shows every item"*. The two
/// halves are drawn one above the other and NAMED APART — `held here` is what
/// this face has set aside and not sent, `waiting`/`failed` is what the daemon
/// is holding — because a count whose scope is a guess reads as the wrong
/// number, which is the rule the header badge already obeys.
///
/// It returns row infos as well as rows, unlike the other two overlays, because
/// **the play button has to be pressable here**. An overlay row is otherwise
/// `Target::Overlay`, which dismisses on click; a `▶` he can see and cannot
/// press would be the worse half of an affordance.
fn queue_overlay_with_client(
    vs: &ViewState,
    rows: usize,
    w: usize,
) -> (Vec<Line<'static>>, Vec<RowInfo>) {
    let lines = queue_overlay(&vs.queue, rows, w);
    let items = vs.composer.buf.pending();
    // Every row of the daemon's half dismisses, as it always did.
    let mut infos: Vec<RowInfo> = lines
        .iter()
        .map(|_| RowInfo::bare(Target::Overlay))
        .collect();
    if items.is_empty() {
        return (lines, infos);
    }
    // Rebuild with the client half spliced in above the closing sentence.
    let mut body: Vec<Line<'static>> = Vec::new();
    let mut binfos: Vec<RowInfo> = Vec::new();
    body.push(indented(
        format!(
            "held here ({}) — not sent, and the daemon has not seen them",
            items.len()
        ),
        palette::header(),
        w,
    ));
    binfos.push(RowInfo::bare(Target::Overlay));
    for (i, item) in items.iter().enumerate() {
        let playing = vs.playing.is_some() && vs.playing.as_deref() == item.audio.as_deref();
        let (line, info) = client_row(item, i, w, playing, GUTTER + 3);
        body.push(line);
        binfos.push(info);
    }
    body.push(size_line(vec![], w, palette::base()));
    binfos.push(RowInfo::bare(Target::Overlay));

    // Splice: the daemon's rows, then ours, then the frame's own last rows.
    // `framed` owns the border, so this rebuilds through it rather than
    // patching painted rows — a second layout of the same band is the class of
    // derivation this crate has already had to repair once.
    let mut inner: Vec<Line<'static>> = Vec::new();
    let mut iinfos: Vec<RowInfo> = Vec::new();
    let daemon = queue_overlay_body(&vs.queue, w);
    let n = daemon.len();
    inner.extend(daemon);
    iinfos.extend((0..n).map(|_| RowInfo::bare(Target::Overlay)));
    inner.extend(body);
    iinfos.extend(binfos);

    let title = if vs.queue.thread.is_empty() {
        "queue".to_string()
    } else {
        format!("queue · {}", vs.queue.thread)
    };
    let out = framed(&title, inner, rows, w);
    // `framed` adds a title row at the top and pads/crops to `rows`. The infos
    // have to describe the SAME rows, so they are rebuilt to the frame's shape:
    // one leading row for the border, then ours, then padding.
    infos = Vec::with_capacity(out.len());
    infos.push(RowInfo::bare(Target::Overlay));
    for k in 0..out.len().saturating_sub(1) {
        infos.push(
            iinfos
                .get(k)
                .cloned()
                .unwrap_or_else(|| RowInfo::bare(Target::Overlay)),
        );
    }
    infos.truncate(out.len());
    (out, infos)
}

fn queue_row(it: &QueueItem, sev: Severity, w: usize) -> Line<'static> {
    line_from(
        crop_with_ellipsis(
            vec![
                Seg::new(" ".repeat(GUTTER + 3), palette::base()),
                Seg::new(format!("#{:<6}", it.message_id), sev.style()),
                Seg::new(format!("{:<10}", it.intent), palette::muted()),
                Seg::new(format!("{:<18}", stamp(&it.created_at)), palette::muted()),
                Seg::new(it.text.replace(['\n', '\r'], " "), palette::ink2()),
            ],
            w,
        ),
        w,
    )
}

/// An RFC3339 instant cut to the minute.
///
/// The daemon's timestamps carry microseconds and an offset —
/// `2026-09-07T18:02:32.193354+00:00`, thirty-two columns — and the live run
/// showed them running straight into the message text they were supposed to
/// label, because the column they were given was twenty-six. Sub-second
/// precision on a queued message is not a fact anyone reads; the minute is.
/// Cut rather than widen: the text is what he is looking for.
///
/// A string this function cannot recognise is passed through UNTOUCHED rather
/// than trimmed to a fixed length, so an upstream format change shows up as an
/// odd-looking stamp instead of a silently mangled one.
fn stamp(s: &str) -> String {
    match (s.len() >= 16, s.as_bytes().get(10)) {
        (true, Some(b'T')) => s[..16].replace('T', " "),
        _ => s.to_string(),
    }
}

// ── the one way a line leaves this file ──────────────────────────────────────

fn line_from(segs: Vec<Seg>, w: usize) -> Line<'static> {
    let spans: Vec<Span<'static>> = segs
        .into_iter()
        .map(|s| Span::styled(s.text, s.style))
        .collect();
    size_line(spans, w, palette::base())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Known, Notice};

    fn row_text(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The characters a row paints starting at DISPLAY COLUMN `col0` — which
    /// is the coordinate a `RowText` speaks, and never a byte offset. Slicing
    /// by bytes goes wrong at the first `▌`.
    fn from_col(line: &Line<'static>, col0: u16, len: usize) -> String {
        let whole = row_text(line);
        let mut col = 0usize;
        whole
            .chars()
            .filter(|ch| {
                let here = col;
                col += ch.width().unwrap_or(0);
                here >= col0 as usize
            })
            .take(len)
            .collect()
    }

    // ─────────────────────────────────────────────────────────────────────
    //  THE DAEMON'S OWN WARNING COUNT
    //
    //  Operator: *"For TUI and the threads tui can you display the number of
    //  warnings coming from threads so it annoys me? 27k is incredible."*
    //
    //  29,644 of one warning reached `~/.aui/threads-daemon.log` and nobody ever
    //  saw one, because a log file is a thing you have to decide to read. These
    //  assert on the PAINTED ROW — the characters he reads — and not on the
    //  `Header` struct, because a struct holding 47 and a row drawing nothing are
    //  not the same product.
    // ─────────────────────────────────────────────────────────────────────

    // ─────────────────────────────────────────────────────────────────────
    //  TUI-P1a (TC-04): THE THREAD PAGE'S HEADER IS ITS NAME AND ITS STATUS
    // ─────────────────────────────────────────────────────────────────────

    /// A header carrying EVERY fact the row can draw, so a cut that let one
    /// back in has something to draw.
    fn loud_header(mode: Mode) -> ViewState {
        let mut vs = ViewState::default();
        vs.mode = mode;
        vs.header.title = "Answer the tax letter · active".into();
        vs.header.trail = vec!["harness".into()];
        vs.header.cwd = "~/code/harness".into();
        vs.header.account = Known::Known("alice-example-com".into());
        vs.header.unread_elsewhere = Known::Known(723);
        vs.header.queued = Known::Known(2);
        vs.header.queue_scope = "this thread".into();
        vs.header.warnings = Known::Known(47);
        vs.header.activity = Some(("waiting on 2 subagents".into(), crate::palette::Severity::Calm));
        vs.header.link = crate::model::Link {
            url: "ws://127.0.0.1:3653".into(),
            state: LinkState::Connected,
            down_since: None,
        };
        vs
    }

    fn header_text(vs: &ViewState, w: usize) -> String {
        let now = DateTime::from_timestamp(1_800_000_000, 0).expect("a fixed instant");
        row_text(&header_line(vs, now, w).0)
    }

    /// **ON A THREAD PAGE THE HEADER IS THE NAME AND THE STATUS, AND NONE OF
    /// THE FOUR THINGS HE NAMED** — `unread elsewhere`, the project path,
    /// `connected`, the account (*"The only thing I want to see at the top of
    /// the page, is the name of the thread and whether it's active or idle"*).
    /// Asserted on the PAINTED ROW, at two widths and in both thread surfaces
    /// (the transcript and mic mode).
    ///
    /// THE POSITIVE CONTROL is the same loud header on a project's LIST page,
    /// which still draws all four — so each `!contains` below is a search that
    /// was shown able to find its target.
    ///
    /// Forcings (report): delete `&& !thread_page` from each of the four gates
    /// in turn (unread elsewhere, cwd, link, account) → each goes red naming
    /// its item.
    #[test]
    fn a_thread_pages_header_is_its_name_and_status_and_nothing_he_removed() {
        let list = header_text(&loud_header(Mode::List), 400);
        for item in ["723 unread elsewhere", "~/code/harness", "connected", "acct alice-example-com"] {
            assert!(list.contains(item), "the control cannot draw {item:?}: {list:?}");
        }
        for mode in [Mode::Thread(crate::keys::Region::Composer), Mode::Mic] {
            for w in [400, 120] {
                let row = header_text(&loud_header(mode), w);
                assert!(
                    row.trim_end().starts_with("Answer the tax letter · active"),
                    "{mode:?} at {w}: the name and status do not lead the header: {row:?}"
                );
                for gone in ["unread elsewhere", "~/code/harness", "connected", "acct", "harness ›",
                             "queued", "warnings", "waiting on"] {
                    assert!(!row.contains(gone), "{mode:?} at {w}: {gone:?} is back on the header: {row:?}");
                }
                assert_eq!(
                    row.trim_end(),
                    "Answer the tax letter · active",
                    "{mode:?} at {w}: the header carries something besides the name and status"
                );
            }
        }
    }

    /// **OVER A THREAD PAGE, EVERY OVERLAY KEEPS THE HEADER TO NAME AND
    /// STATUS** (TUI-P1a, inspect F-2). The rename box, the queue, help and the
    /// sheet each put their own mode in `vs.mode`; the shell says the page under
    /// them is a thread through `Header::thread_page`. N = 4 overlays at 2
    /// widths. Opposed: the same overlays over a LIST page (`thread_page:
    /// false`) still draw the list header's parts, so the cut is the thread
    /// page's and not the overlay's.
    ///
    /// Forcings (report): make `thread_page` ignore `hd.thread_page` (the
    /// pre-fix line) → every overlay red with the banned parts back.
    #[test]
    fn every_overlay_over_a_thread_page_keeps_the_header_to_name_and_status() {
        for mode in [Mode::RenameModal, Mode::Queue, Mode::Help, Mode::Sheet] {
            let mut v = loud_header(mode);
            v.header.thread_page = true;
            for w in [400, 120] {
                let row = header_text(&v, w);
                assert_eq!(
                    row.trim_end(),
                    "Answer the tax letter · active",
                    "{mode:?} over a thread at {w}: the banned parts came back: {row:?}"
                );
            }
            let mut over_list = loud_header(mode);
            over_list.header.thread_page = false;
            let row = header_text(&over_list, 400);
            assert!(
                row.contains("723 unread elsewhere") && row.contains("acct alice-example-com"),
                "{mode:?} over a list: the control drew nothing to cut: {row:?}"
            );
        }
    }

    /// A `ViewState` whose header is otherwise quiet, so the only thing the row
    /// can gain or lose is the warning count.
    fn warn_vs(warnings: Known<u32>, errors: Known<u32>) -> ViewState {
        let mut vs = ViewState::default();
        vs.header.title = "threads".into();
        vs.header.warnings = warnings;
        vs.header.warning_errors = errors;
        vs
    }

    fn warn_row(warnings: Known<u32>, errors: Known<u32>, w: usize) -> String {
        let now = DateTime::from_timestamp(1_800_000_000, 0).expect("a fixed instant");
        let (line, _) = header_line(&warn_vs(warnings, errors), now, w);
        row_text(&line)
    }

    /// **ZERO AND UNKNOWN BOTH DRAW NOTHING, AND A REAL COUNT DRAWS.**
    ///
    /// All three states in one test, because each alone is satisfiable by a stub:
    /// a part that never renders passes the first two and a part that always
    /// renders passes the third.
    ///
    /// * ZERO is absent because *"a number that is always on screen becomes
    ///   wallpaper, which is how the log failed"*.
    /// * UNKNOWN is absent rather than `?` — unlike `unread elsewhere`, which does
    ///   paint `?`. It is the normal state against every daemon built before the
    ///   field existed, and there is no key he could press to make an old daemon
    ///   report, so a `?` here would be noise with no remedy.
    ///
    /// **THE MUTATION.** In `header_line`, widen the pattern from
    /// `Known::Known(n @ 1..)` to `Known::Known(n)`.
    ///
    /// Forced red:
    /// ```text
    /// a daemon with no warnings costs no columns: "threads · 0 warnings"
    /// ```
    #[test]
    fn a_clean_daemon_puts_no_warning_count_on_the_header() {
        let quiet = warn_row(Known::Known(0), Known::Known(0), 120);
        assert!(
            !quiet.contains("warning"),
            "a daemon with no warnings costs no columns: {quiet:?}"
        );

        let unknown = warn_row(Known::Unknown, Known::Unknown, 120);
        assert!(
            !unknown.contains("warning") && !unknown.contains('⚠'),
            "a daemon that has not told us draws nothing, not `?`: {unknown:?}"
        );

        // THE POSITIVE CONTROL. Without it both assertions above pass against a
        // header that can never draw this at all.
        let loud = warn_row(Known::Known(47), Known::Known(0), 120);
        assert!(
            loud.contains("47 warnings"),
            "the same header DOES draw a real count: {loud:?}"
        );
    }

    /// **THE STRING HE WILL READ, INCLUDING THE ERROR HALF.**
    ///
    /// The producer keeps `warn` and `error` in separate cells on purpose, because
    /// one number cannot tell 47 warnings from 47 errors. The header sums them for
    /// the headline — one row has space for one number — and names the errors after
    /// it, so the distinction survives to the screen.
    ///
    /// **THE MUTATION.** In `header_line`, delete the `if errs > 0` block that
    /// appends the parenthesis.
    ///
    /// Forced red:
    /// ```text
    /// the error half is named, not folded away: "threads · 246 warnings"
    /// ```
    #[test]
    fn the_header_names_errors_apart_from_warnings() {
        let both = warn_row(Known::Known(29_843), Known::Known(199), 200);
        assert!(
            both.contains("29,843 warnings (199 errors)"),
            "the error half is named, not folded away, and the total is grouped so \
             its MAGNITUDE reads at a glance: {both:?}"
        );
        let one = warn_row(Known::Known(2), Known::Known(1), 120);
        assert!(
            one.contains("2 warnings (1 error)"),
            "and one error is one error: {one:?}"
        );
        let singular = warn_row(Known::Known(1), Known::Known(0), 120);
        assert!(
            singular.contains("1 warning") && !singular.contains("1 warnings"),
            "one warning is one warning: {singular:?}"
        );
    }

    /// **THOUSANDS SEPARATORS LAND ON THE RIGHT DIGITS.**
    ///
    /// Hand-rolled, so the off-by-one lives here rather than in a library: every
    /// digit-count mod 3 is checked, because a grouper that is right for
    /// `29,644` and wrong for `1,000` is wrong on the number he will see most.
    ///
    /// **THE MUTATION.** In `grouped`, drop the `&& (i >= lead)` guard.
    ///
    /// Forced red:
    /// ```text
    /// assertion `left == right` failed: 100
    ///   left: ",100"
    ///  right: "100"
    /// ```
    #[test]
    fn a_count_is_grouped_in_threes_from_the_right() {
        for (n, want) in [
            (0u32, "0"),
            (7, "7"),
            (47, "47"),
            (100, "100"),
            (999, "999"),
            (1_000, "1,000"),
            (29_644, "29,644"),
            (199_999, "199,999"),
            (1_000_000, "1,000,000"),
            (27_705, "27,705"),
        ] {
            assert_eq!(grouped(n), want, "{n}");
        }
    }

    /// **IT IS NOT DROPPED WHEN THE ROW IS TIGHT.**
    ///
    /// `header_line` drops parts by RANK, highest first, until the row fits. This
    /// part is rank 1 — the same rank as `queued`, the one `DIR-6` says is never
    /// dropped — because a warning count that disappears on a narrow terminal is a
    /// warning count he does not have.
    ///
    /// **64 COLUMNS, AND THE WIDTH IS MEASURED RATHER THAN PICKED.** The first
    /// version of this test used 80 and was GREEN under its own mutation: at 80
    /// the whole row fits, so nothing is dropped at any rank and the test proved
    /// nothing about the rank it was named for.
    ///
    /// The rank is only observable where the fit loop actually has to drop
    /// something. Measured, by rendering the same row at both ranks across
    /// 44–80 columns:
    ///
    /// | width | rank 2 (shipped) | rank 5 (the mutation) |
    /// |---|---|---|
    /// | 44–64 | present, cropped (`(199 e…`) | **gone entirely** |
    /// | 68–80 | present, whole | present, whole |
    ///
    /// So 64 is the widest width that discriminates, and 80 discriminates nothing.
    /// The 80-column assertion is kept BELOW as a separate claim — that the count
    /// is whole and uncropped at the documented narrow floor — which is a
    /// different fact and worth its own line.
    ///
    /// **WHY RANK 2 AND NOT RANK 1, which is what this was first written as.**
    /// Rank 1 is never dropped at all, and `worst()` only offers rank > 1 to the
    /// fit loop — so a rank-1 part 30 columns wide can squeeze the title past
    /// `TITLE_FLOOR` with nothing left to give up, and the `debug_assert` at the
    /// bottom of `fit_header` fired on exactly that:
    ///
    /// ```text
    /// the title was squeezed below its floor with droppable facts still on the row
    /// ```
    ///
    /// `queued` can be rank 1 because `2 queued` is eight columns.
    /// `29,843 warnings (199 errors)` is thirty, and a part that long must stay
    /// droppable or it breaks the machinery that keeps every other row honest.
    /// Rank 2 is the right home on its own terms anyway: this crate's own comment
    /// defines rank 2 as *"a STATEMENT ABOUT WHETHER WHAT IS BELOW IS COMPLETE
    /// rather than metadata"*, which is what a count of the producer's own
    /// complaints is.
    ///
    /// **THE MUTATION.** In `header_line`, change the part's rank from `2` to `5`.
    ///
    /// Forced red:
    /// ```text
    /// the warning count survives an 80-column row: "threads · connecting #0"
    /// ```
    #[test]
    fn the_warning_count_is_not_the_first_thing_dropped() {
        // THE DISCRIMINATING WIDTH. At rank 5 the part is gone from this row
        // entirely; at rank 2 it survives.
        let tight = warn_row(Known::Known(29_644), Known::Known(199), 64);
        assert!(
            tight.contains("warnings"),
            "the warning count outranks what a 64-column row gives up: {tight:?}"
        );

        // Whole and uncropped at the documented narrow floor.
        let floor = warn_row(Known::Known(29_644), Known::Known(199), 80);
        assert!(
            floor.contains("29,644 warnings (199 errors)"),
            "and it is whole, not cropped, at 80: {floor:?}"
        );

        // It still costs nothing when there is nothing to say, so the survival
        // above is not "this part is always drawn".
        let quiet = warn_row(Known::Known(0), Known::Known(0), 64);
        assert!(
            !quiet.contains("warning"),
            "a quiet daemon still costs no columns at 64: {quiet:?}"
        );

        // **AND THE ROW STILL FITS AT AN ABSURD WIDTH.** Rank 2 is what makes this
        // hold: at 40 the part is offered to the fit loop and can be dropped,
        // where a rank-1 part — which `worst()` never offers — tripped
        // `fit_header`'s own `debug_assert` with
        // "the title was squeezed below its floor with droppable facts still on
        // the row".
        let absurd = warn_row(Known::Known(29_644), Known::Known(199), 40);
        assert!(!absurd.is_empty(), "a 40-column row still paints something");
    }

    /// **THE ONE PLACE A LIST ROW'S TEXT IS DECIDED, CHECKED AGAINST THE ROW
    /// ITSELF.**
    ///
    /// `select::thread_run` / `select::project_run` say which characters of a
    /// list row are selectable and where they start; `thread_row` /
    /// `project_row` paint them. If the two ever disagree, every column he
    /// points at resolves to the wrong character — the highlight lands beside
    /// the text and the clipboard gets something he did not point at, with
    /// nothing on screen looking wrong. That is the whole reason this test
    /// exists, and it is why the run is the ABBREVIATED id rather than the
    /// full one the model holds.
    #[test]
    fn a_list_rows_selectable_run_is_exactly_what_the_row_paints() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).expect("a fixed instant");

        let t = ThreadRow {
            id: "57db2ec9-1111-2222-3333-444444444444".into(),
            summary: "the terminal face, drag to copy".into(),
            cwd: "~/code/harness".into(),
            state: "idle".into(),
            queued: Known::Known(0),
            unread: Known::Known(0),
            ..Default::default()
        };
        let (line, _) = thread_row(&t, false, now, 120);
        let run = select::thread_run(&t);
        assert_eq!(
            from_col(&line, 2, run.chars().count()),
            run,
            "the run does not start at column 2 of the row it describes: {:?}",
            row_text(&line)
        );

        // …and the fallback ladder, which is the half most likely to drift.
        let mut t2 = t.clone();
        t2.summary = String::new();
        let (line, _) = thread_row(&t2, false, now, 120);
        let run = select::thread_run(&t2);
        assert_eq!(
            from_col(&line, 2, run.chars().count()),
            run,
            "the untitled fallback drifted"
        );
        // TUI-P1a (TC-02): a row with no name never shows its path.
        assert_eq!(run, "untitled", "an unnamed row fell back to its path");
        assert!(!row_text(&line).contains("~/code/harness"), "{:?}", row_text(&line));
        let mut t3 = t2.clone();
        t3.cwd = String::new();
        let (line, _) = thread_row(&t3, false, now, 120);
        let run = select::thread_run(&t3);
        assert_eq!(
            from_col(&line, 2, run.chars().count()),
            run,
            "the blank fallback drifted"
        );

        let p = ProjectRow {
            name: "harness".into(),
            path: "~/code/harness".into(),
            threads: Known::Known(12),
            active: Known::Known(0),
            ..Default::default()
        };
        let (line, name) = project_row(&p, false, now, 120);
        let run = select::project_run(&p);
        assert_eq!(
            // COLUMN 4, not 2: a band's contained rows are indented past their
            // header since `0072` (`ROW_COL0`), and the run starts where the
            // text does.
            from_col(&line, ROW_COL0, run.chars().count()),
            run,
            "the project run does not start at the band indent: {:?}",
            row_text(&line)
        );
        // **THE NAME SPAN IS THE PROJECT SHEET'S DOOR**, and it must cover the
        // name and nothing beyond it: `~/code/harness` two columns to the right
        // is a different affordance's territory.
        assert_eq!(
            from_col(&line, name.0, (name.1 - name.0) as usize),
            "harness",
            "the claimed name span is not the name"
        );
    }

    /// A transcript row's selectable text starts where the GUTTER ends, not
    /// where the row does. A `col0` that included the gutter would shift every
    /// offset on the row by two characters.
    ///
    /// **THE RAIL IT USED TO ALSO STEP OVER IS GONE** — *"You don't need to
    /// have the quote line on the left of every message. It's distracting."* —
    /// so the assistant's text begins two columns further left than it did, and
    /// this assertion is where that would be caught if the two ever disagreed
    /// again.
    // ── THREAD-LOAD-LATENCY F3: the viewport-bounded layout ──────────────

    fn latency_blocks(n: usize, long_every: usize) -> Vec<Block> {
        let long = "lorem ipsum **dolor** sit amet, `consectetur` adipiscing elit. ".repeat(40);
        (0..n)
            .map(|i| match i % 7 {
                0 => Block::Message {
                    id: Default::default(),
                    sender: Sender::User,
                    ts: None,
                    text: format!("question {i}"),
                    note: None,
                    meta: Vec::new(),
                    key: None,
                    expandable: false,
                    open: false,
                },
                3 => Block::Lane {
                    ts: None,
                    glyph: '•',
                    severity: Severity::Calm,
                    text: format!("tool call {i}"),
                    count: (i % 2 == 0).then_some(3),
                    key: None,
                    expandable: true,
                    open: false,
                    affordance: None,
                },
                5 if i % 35 == 5 => Block::Rule { text: format!("rule {i}") },
                _ => Block::Message {
                    id: Default::default(),
                    sender: Sender::Assistant,
                    ts: None,
                    text: if long_every > 0 && i % long_every == 1 {
                        format!("reply {i}: {long}")
                    } else {
                        format!("reply {i} with *some* markdown")
                    },
                    note: (i % 11 == 2).then(|| "writing".to_string()),
                    meta: Vec::new(),
                    key: None,
                    expandable: i % 13 == 0,
                    open: false,
                },
            })
            .collect()
    }

    /// A frame's cost as the best of three runs: the budget is about the work
    /// a frame does, and a test runner sharing a loaded machine with other
    /// tests preempts single runs at random.
    fn best_of_3(mut f: impl FnMut()) -> u128 {
        (0..3)
            .map(|_| {
                let t0 = std::time::Instant::now();
                f();
                t0.elapsed().as_micros()
            })
            .min()
            .unwrap_or(0)
    }

    fn latency_vs(blocks: Vec<Block>, from_bottom: usize) -> ViewState {
        let mut vs = ViewState {
            mode: Mode::Thread(Region::Composer),
            ..Default::default()
        };
        vs.blocks = blocks;
        vs.viewport.follow = true;
        vs.viewport.from_bottom = from_bottom;
        vs
    }

    /// **A SUFFIX LAYOUT IS THE BOTTOM OF THE WHOLE LAYOUT, ROW FOR ROW** —
    /// every start index, so the one cross-block fact (`last_sender`, the blank
    /// separator) is proved at runs, at user boxes, at lanes and at rules.
    #[test]
    fn a_suffix_layout_is_the_bottom_of_the_whole_layout() {
        let blocks = latency_blocks(120, 9);
        let vs = latency_vs(blocks.clone(), 0);
        let now = DateTime::from_timestamp(1_800_000_000, 0).expect("a fixed instant");
        let (all, spans, texts, marks) = lay_out(&blocks, &vs, now, 100);
        for k in 0..blocks.len() {
            let (part, pspans, ptexts, pmarks) = lay_out_from(&blocks, k, &vs, now, 100);
            let at = spans[k].0;
            let want: Vec<String> = all[at..].iter().map(row_text).collect();
            let got: Vec<String> = part.iter().map(row_text).collect();
            assert_eq!(got, want, "suffix from block {k} differs from the whole layout");
            assert_eq!(ptexts.len(), texts.len() - at);
            for (a, b) in ptexts.iter().zip(&texts[at..]) {
                assert_eq!(a.as_ref().map(|t| (t.unit, t.start)), b.as_ref().map(|t| (t.unit, t.start)));
            }
            assert_eq!(pmarks, marks[k..].to_vec());
            let shifted: Vec<(usize, usize)> = spans[k..].iter().map(|(f, l)| (f - at, *l)).collect();
            assert_eq!(pspans, shifted);
        }
    }

    /// **THE TRANSCRIPT A VIEWPORT SEES IS UNCHANGED** by laying out only what
    /// it can reach — at the bottom, part-way up, and at the very top.
    #[test]
    fn the_bounded_transcript_draws_what_the_whole_one_drew() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).expect("a fixed instant");
        for fb in [0usize, 7, 40, 333, 2_000, usize::MAX] {
            let vs = latency_vs(latency_blocks(400, 9), fb);
            let (got, ginfo) = transcript(&vs, now, 100, 40);
            let (all, spans, texts, marks) = lay_out(&vs.blocks, &vs, now, 100);
            let mut infos: Vec<RowInfo> = texts.into_iter().map(|t| RowInfo::new(Target::Empty, t)).collect();
            for (i, (first, len)) in spans.iter().enumerate() {
                for info in infos.iter_mut().skip(*first).take(*len) {
                    info.target = Target::Block(i);
                }
                if *len > 0 {
                    if let Some(at) = marks.get(i).copied().flatten() {
                        infos[first + len - 1].spans.push((at, at + GUTTER as u16, Target::Expander(i)));
                    }
                }
            }
            let (want, winfo) = window(all, infos, 40, fb, 100, None);
            assert_eq!(
                got.iter().map(row_text).collect::<Vec<_>>(),
                want.iter().map(row_text).collect::<Vec<_>>(),
                "from_bottom={fb}"
            );
            assert_eq!(
                ginfo.iter().map(|r| format!("{:?}", r.target)).collect::<Vec<_>>(),
                winfo.iter().map(|r| format!("{:?}", r.target)).collect::<Vec<_>>(),
                "targets differ at from_bottom={fb}"
            );
        }
    }

    /// **THE FRAME BUDGET** (THREAD-LOAD-LATENCY): a whole frame of a
    /// 4,000-block thread, following the bottom while a ~100 KB reply streams
    /// into its last block, must paint inside 16 ms at p99. Release-mode
    /// number (`build.rs` sets `tui_render_optimized`); an opt-level-0 build
    /// gets 10x. Measured: unbounded layout p50 65 ms / p99 89 ms, bounded
    /// p50 3.5 ms / p99 6.6 ms (release, load ~20).
    #[test]
    fn streaming_a_large_reply_into_a_long_thread_stays_inside_the_frame_budget() {
        // 16 ms optimised; an unoptimised (opt-level 0) build is ~10x slower at
        // everything and gets 10x, which the unbounded layout still failed.
        let budget_us: u128 = if cfg!(tui_render_optimized) { 16_000 } else { 160_000 };
        let now = DateTime::from_timestamp(1_800_000_000, 0).expect("a fixed instant");
        let mut vs = latency_vs(latency_blocks(4_000, 9), 0);
        let chunk = "streamed words of a long reply, with `code` and **bold**. ";
        let mut text = String::new();
        let mut times = Vec::new();
        while text.len() < 100_000 {
            for _ in 0..30 {
                text.push_str(chunk);
            }
            if let Some(Block::Message { text: t, .. }) = vs.blocks.last_mut() {
                *t = text.clone();
            }
            times.push(best_of_3(|| drop(frame(&vs, now, 160, 50))));
        }
        times.sort_unstable();
        let p50 = times[times.len() / 2];
        let p99 = times[(times.len() * 99) / 100];
        eprintln!("FRAME-BUDGET frames={} p50_us={p50} p99_us={p99}", times.len());
        assert!(p99 <= budget_us, "p99 frame {p99} us over the {budget_us} us budget (p50 {p50} us)");
    }

    /// **SCROLLING A LONG THREAD STAYS INSIDE THE FRAME BUDGET AT EVERY DEPTH**
    /// (THREAD-LOAD-LATENCY F6), once the thread has been drawn once. Walks a
    /// 4,000-block thread from the bottom to the top in wheel-sized steps and
    /// holds every frame to 16 ms at p99 (optimised; 10x unoptimised). Before
    /// the wrap cache a frame 1,000 rows up was 15 ms and the top was 128 ms.
    #[test]
    fn scrolling_a_long_thread_to_the_top_stays_inside_the_frame_budget() {
        let budget_us: u128 = if cfg!(tui_render_optimized) { 16_000 } else { 160_000 };
        let now = DateTime::from_timestamp(1_800_000_000, 0).expect("a fixed instant");
        let mut vs = latency_vs(latency_blocks(4_000, 9), 0);
        vs.viewport.follow = false;
        let _ = frame(&vs, now, 160, 50); // the open: one full paint
        vs.viewport.from_bottom = usize::MAX;
        let _ = frame(&vs, now, 160, 50);
        let mut times = Vec::new();
        let mut fb = 0usize;
        while fb < 60_000 {
            vs.viewport.from_bottom = fb;
            times.push(best_of_3(|| drop(frame(&vs, now, 160, 50))));
            fb += 600;
        }
        vs.viewport.from_bottom = usize::MAX;
        let t0 = std::time::Instant::now();
        let _ = frame(&vs, now, 160, 50);
        times.push(t0.elapsed().as_micros());
        times.sort_unstable();
        let p50 = times[times.len() / 2];
        let p99 = times[(times.len() * 99) / 100];
        eprintln!("SCROLL-BUDGET frames={} p50_us={p50} p99_us={p99}", times.len());
        assert!(p99 <= budget_us, "scroll p99 {p99} us over {budget_us} us (p50 {p50})");
    }

    /// The wrap cache never serves rows laid from different inputs.
    #[test]
    fn the_wrap_cache_answers_only_for_the_same_text_measure_and_style() {
        let (b, c) = (palette::ink(), palette::code());
        let rows = |t: &str, w: usize| -> Vec<(Option<String>, usize)> {
            md_lay_cached(t, w, b, c).iter().map(|r| (r.text.clone(), r.start)).collect()
        };
        let fresh = |t: &str, w: usize| -> Vec<(Option<String>, usize)> {
            md::lay(t, w, b, c).into_iter().map(|r| (r.text, r.start)).collect()
        };
        let a = "alpha beta gamma delta epsilon zeta eta theta iota kappa";
        for (t, w) in [(a, 20), (a, 30), ("alpha", 20), (a, 20)] {
            assert_eq!(rows(t, w), fresh(t, w), "text {t:?} at {w}");
        }
        let mut grown = a.to_string();
        grown.push_str(" lambda");
        assert_eq!(rows(&grown, 20), fresh(&grown, 20), "a grown message was served stale");
        let (ub, uc) = (palette::paper(), palette::code_on_paper());
        let spans = |b2, c2| -> String { format!("{:?}", md_lay_cached(a, 20, b2, c2).iter().map(|r| r.spans.clone()).collect::<Vec<_>>()) };
        assert_eq!(spans(ub, uc), format!("{:?}", md::lay(a, 20, ub, uc).into_iter().map(|r| r.spans).collect::<Vec<_>>()));
    }

    /// Scroll-depth frame times, printed for `docs/THREAD-LOAD-LATENCY.md`
    /// (`cargo test -p tui-render --release -- scroll_depth --ignored --nocapture`).
    #[test]
    #[ignore]
    fn scroll_depth_frame_times() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).expect("a fixed instant");
        for fb in [0usize, 200, 1_000, 5_000, 20_000, usize::MAX] {
            let mut vs = latency_vs(latency_blocks(4_000, 9), fb);
            vs.viewport.follow = false;
            let mut t = Vec::new();
            for _ in 0..15 {
                let t0 = std::time::Instant::now();
                let _ = frame(&vs, now, 160, 50);
                t.push(t0.elapsed().as_micros());
            }
            t.sort_unstable();
            eprintln!("SCROLL-DEPTH from_bottom={fb} p50_us={} max_us={}", t[t.len() / 2], t[t.len() - 1]);
        }
    }

    #[test]
    fn a_message_rows_text_starts_after_the_gutter() {
        let mut vs = ViewState {
            mode: Mode::Thread(Region::Composer),
            ..Default::default()
        };
        vs.blocks.push(Block::Message {
            id: Default::default(),
            sender: Sender::Assistant,
            ts: None,
            text: "one two three".into(),
            note: None,
            meta: Vec::new(),
            key: None,
            expandable: false,
            open: false,
        });
        let now = DateTime::from_timestamp(1_800_000_000, 0).expect("a fixed instant");
        let (lines, _, texts, _) = lay_out(&vs.blocks, &vs, now, 60);
        let (i, rt) = texts
            .iter()
            .enumerate()
            .find_map(|(i, t)| t.as_ref().map(|t| (i, t)))
            .expect("no row carried any text");
        assert_eq!(rt.col0 as usize, GUTTER, "the gutter is inside the text");
        assert_eq!(rt.text, "one two three");
        assert_eq!(rt.start, 0);
        assert_eq!(
            from_col(&lines[i], rt.col0, rt.len()),
            rt.text,
            "the row's text is not at the column the map claims: {:?}",
            row_text(&lines[i])
        );
    }

    // ── the info-notice area ────────────────────────────────────────────────

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + secs, 0).expect("a fixed instant")
    }

    fn footer_with(notice: Option<Notice>, w: usize, now: DateTime<Utc>) -> String {
        let vs = ViewState {
            mode: Mode::Thread(Region::Composer),
            notice,
            ..Default::default()
        };
        row_text(&footer_line(&vs, now, w))
    }

    fn note(text: &str, expires: Option<DateTime<Utc>>) -> Notice {
        Notice {
            text: text.into(),
            severity: Severity::Warn,
            expires_at: expires,
        }
    }

    /// **THE NOTICE IS RIGHT-ALIGNED AND THE HINTS GIVE WAY TO IT.**
    ///
    /// His rule, verbatim: *"floating on the right side of the keybind bar. If
    /// it's longer than the space available, have it sit over the top of the
    /// keybind messages."*
    #[test]
    fn a_notice_floats_at_the_right_end_and_the_keybinds_yield_to_it() {
        // **WIDE ENOUGH THAT NOTHING HAS TO GIVE, and that number moved when
        // the speech toggle joined the thread's footer.** At 100 columns this
        // case used to fit with ZERO columns to spare — `alt-/ keys · esc
        // back · alt-u queue · alt-f filter · alt-m mic · ^C quit` plus a
        // 24-column notice is exactly 100 — so the first hint ever added to a
        // thread's grammar was always going to make "both fit" false here.
        // The property under test is the one below it (a notice that FITS must
        // not cost a hint); the width is only the premise, and the premise has
        // to stay true for the assertion to mean anything.
        const W: usize = 132;
        let bare = footer_with(None, W, at(0));
        let with = footer_with(Some(note("paste failed — timed out", None)), W, at(0));

        assert_eq!(
            with.chars().count(),
            W,
            "the footer is not exactly one row wide"
        );
        assert!(
            with.trim_end().ends_with("paste failed — timed out"),
            "the notice is not at the right-hand end: {with:?}"
        );
        // At 100 columns both fit, so nothing has to give — and the hints must
        // be UNTOUCHED, because a notice that dropped a hint it did not need
        // the room for would be spending his footer for nothing.
        assert_eq!(
            hint_count(&with),
            hint_count(&bare),
            "a notice that fits still cost the keybind bar a hint: {with:?}"
        );

        // **NARROW, where the two genuinely compete.** This is the case his
        // rule is about, and the hints are what yield.
        const TIGHT: usize = 64;
        let bare = footer_with(None, TIGHT, at(0));
        let with = footer_with(Some(note("paste failed — timed out", None)), TIGHT, at(0));
        assert_eq!(with.chars().count(), TIGHT);
        assert!(
            with.trim_end().ends_with("paste failed — timed out"),
            "the notice was cut to protect the hints: {with:?}"
        );
        assert!(
            hint_count(&with) < hint_count(&bare),
            "the keybind bar did not yield any columns to the notice: \
             {bare:?} vs {with:?}"
        );
    }

    /// How many ` · `-separated hint fragments survived, which is the thing
    /// the notice competes with for the row.
    fn hint_count(row: &str) -> usize {
        let head = row.trim_end();
        if head.is_empty() {
            return 0;
        }
        head.matches(" · ").count() + 1
    }

    /// **THE FAILURE THIS REPLACES, ASSERTED DIRECTLY.** The old footer laid
    /// the hints FIRST and drew the notice only `if used + tail_w <= w` — so a
    /// message too long for the leftover columns was not truncated and not
    /// overlaid, it was DROPPED, silently, in favour of key hints he already
    /// knows. A long notice must still be on the row.
    #[test]
    fn a_notice_longer_than_the_leftover_room_is_shown_and_never_dropped() {
        const W: usize = 60;
        let long = "the terminal would not take the copy: broken pipe, and tmux \
                    discarded the sequence without a word";
        let row = footer_with(Some(note(long, None)), W, at(0));
        assert_eq!(row.chars().count(), W);
        assert!(
            row.contains("the terminal would not take"),
            "a long notice vanished instead of taking the row: {row:?}"
        );
        // Cut with the ellipsis, never silently: a truncated warning that reads
        // as complete is worse than no warning.
        assert!(row.contains('…'), "the crop was silent: {row:?}");
        // …and never the WHOLE row: the left margin is what keeps it a notice
        // rather than a banner.
        assert!(
            row.starts_with(&" ".repeat(GUTTER)),
            "the notice ate the gutter and reads as a banner: {row:?}"
        );
    }

    /// **THE TIMEOUT, AT AN EXACT INSTANT.** *"Have a timeout on those info
    /// messages."* It is half-open — gone AT the deadline — because the shell
    /// wakes the loop at exactly that instant to redraw, and a closed interval
    /// would need a second wake-up to clear it.
    #[test]
    fn a_notice_is_drawn_until_its_deadline_and_not_at_it() {
        let n = note("sent", Some(at(5)));
        assert!(n.live_at(at(0)));
        assert!(n.live_at(at(4)));
        assert!(!n.live_at(at(5)), "a notice survived its own deadline");
        assert!(!n.live_at(at(600)));

        const W: usize = 100;
        assert!(footer_with(Some(n.clone()), W, at(4)).contains("sent"));
        assert!(
            !footer_with(Some(n.clone()), W, at(5)).contains("sent"),
            "an expired notice was still drawn"
        );
        // …and with it gone the keybind bar gets its columns back, which is the
        // half that says expiry actually re-laid the row rather than blanking
        // some cells.
        assert_eq!(
            footer_with(Some(n), W, at(5)),
            footer_with(None, W, at(5)),
            "an expired notice left a hole where it used to be"
        );
    }

    /// A notice with no deadline stands. It is for a subject that is still
    /// happening — a paste that has not finished arriving — and the shell caps
    /// even those; nothing here may expire one on its own initiative.
    #[test]
    fn a_notice_with_no_deadline_never_expires_on_its_own() {
        let n = note("paste in progress…", None);
        assert!(n.live_at(at(0)) && n.live_at(at(100_000)));
        assert!(footer_with(Some(n), 100, at(100_000)).contains("paste in progress"));
    }

    /// **THE GESTURE A ROW DECLARES IS NOT THE PART THE CROP EATS.**
    ///
    /// The subagent TILE is the row this exists for: one line, NEVER
    /// expandable, and the only thing in his chat that says the tree below can
    /// be drilled. The third lane runs at [`geom::blob_width`] columns since
    /// the firm margins landed, and the tile's text is longer than that at his
    /// own pane size — so before `affordance` existed the crop deleted
    /// `— → opens its chat` and left a row that pointed at nothing. On an
    /// expandable row a crop is a fold he can undo; on this one it is a
    /// deletion with nothing on the screen to say so.
    ///
    /// What is asserted here is the PROPERTY, at every width the face draws
    /// at: the gesture is painted whole, the row is still exactly one row, and
    /// it still obeys the width law. The elastic middle ellipsizes instead,
    /// visibly.
    #[test]
    fn a_lane_rows_declared_gesture_is_never_the_part_the_crop_eats() {
        const AFF: &str = " — → opens its chat";
        let text = format!("agent · Explore · scout the raw list · running · 31k tok{AFF}");
        for w in [200usize, 140, 120, 100, 97, 80, 60, 48] {
            let mut out = Vec::new();
            let mut texts = Vec::new();
            blob(
                Unit::new(UnitKind::Content, 0),
                '◇',
                Severity::Calm,
                &text,
                Some(AFF),
                None,
                false,
                w,
                &mut out,
                &mut texts,
            );
            assert_eq!(out.len(), 1, "a closed blob is one row, at {w}");
            let painted = row_text(&out[0]);
            assert!(
                painted.contains(AFF.trim_start()),
                "the gesture was cropped away at {w}: {painted:?}"
            );
            assert!(
                painted.contains("agent · Ex"),
                "the tile stopped saying what it is at {w}: {painted:?}"
            );
            assert_eq!(text_width(&painted), w, "the width law broke at {w}");
        }
        // BELOW THE FLOOR the gesture is dropped WHOLE rather than half the row
        // spent on a verb with no subject. 40 is `geom::MIN_WIDTH`.
        let mut out = Vec::new();
        let mut texts = Vec::new();
        blob(
            Unit::new(UnitKind::Content, 0),
            '◇',
            Severity::Calm,
            &text,
            Some(AFF),
            None,
            false,
            geom::MIN_WIDTH as usize,
            &mut out,
            &mut texts,
        );
        let painted = row_text(&out[0]);
        assert!(painted.contains("agent · Explore"), "{painted:?}");
        assert!(!painted.contains("opens its chat"), "{painted:?}");
        assert_eq!(text_width(&painted), geom::MIN_WIDTH as usize);
    }

    /// The companion to the test above, and the reason `affordance` is opt-in:
    /// a lane row that declares NO gesture crops exactly as it always did, tail
    /// first. If this ever starts passing by accident the reservation has
    /// leaked onto every row in the third lane.
    #[test]
    fn a_lane_row_that_declares_no_gesture_still_crops_from_its_tail() {
        // **THE PAYLOAD IS SIZED FROM THE BLOB, NOT TYPED TO A LENGTH.** It
        // was a fixed 73-column string, and it stopped overflowing the instant
        // the firm margin went from 6 to 8 and the blob at 97 columns from 64
        // to 81: the assertion below then passed on a row that had not been
        // cropped at all. A fixture that can quietly stop exercising the thing
        // it names is the one failure mode a crop test cannot afford, so the
        // text grows until it does not fit and ends in a sentinel.
        let mut text = "unknown · ".to_string();
        while text_width(&text) <= geom::blob_width(97) {
            text.push_str("a payload far longer than any firm margin will hold, ");
        }
        text.push_str("TAIL");
        let mut out = Vec::new();
        let mut texts = Vec::new();
        blob(
            Unit::new(UnitKind::Content, 0),
            '·',
            Severity::Unknown,
            &text,
            None,
            None,
            false,
            97,
            &mut out,
            &mut texts,
        );
        let painted = row_text(&out[0]);
        assert!(painted.contains('…'), "{painted:?}");
        assert!(!painted.contains("TAIL"), "{painted:?}");
    }

    /// The degenerate widths, which are where a right-aligned thing goes wrong:
    /// a row narrower than the notice, and a notice with nothing in it. The
    /// postcondition every frame owes — exactly `w` columns — must hold at all
    /// of them.
    #[test]
    fn the_notice_keeps_the_footers_width_law_at_every_width() {
        for w in [1usize, 2, GUTTER, GUTTER + 1, 20, 40, 79, 80, 200] {
            for text in [
                "",
                "ok",
                "a notice far longer than this row can hold, twice over",
            ] {
                let row = footer_with(Some(note(text, None)), w, at(0));
                assert_eq!(
                    row.chars().count(),
                    w,
                    "width {w} broke on {text:?}: {row:?}"
                );
            }
        }
    }
}
