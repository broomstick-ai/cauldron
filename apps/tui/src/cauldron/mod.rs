//! `tui --cauldron`: the Cauldron face, on fake data. It was this crate's whole
//! library before the harness chat face moved in (R7.S1b). Iteration four:
//! cauldron tabs Cluster (was Globs), Energy (was Pools), Mesh and Settings; cluster tabs Questions, DAG,
//! Human Actions and Regions (the explorer into the soup); a collapsible tree;
//! select, then `i` to insert a tag; modals that a click outside closes; one key
//! palette and no legends; a draft persisted and shared live per environment.
//!
//! Iteration six: the cauldron Settings tab is a tree of menus over every
//! setting of design 0057, and each cluster has a sixth tab, Settings, for what
//! it narrows; every boolean is a toggle switch.
//!
//! Iteration seven (msg 82; designs 0069 and 0071): eight cluster tabs —
//! Overview, Questions, Engine, Human Actions, Regions, Repos, Ripples,
//! Settings — each a list of strong lines that open in place, with a trace
//! back to his words on every point; `engine` lays the plan out as rails,
//! `blueprint` holds the repos and their blueprints, and `pages` draws every
//! cluster tab's content (and is the one source of its items).
//!
//! Iteration eight (msgs 96, 112; design 0094): a fifth cauldron tab,
//! Ripples — `ripples` is the policy (every ripple definition, its prompt
//! fragments, its working variant and versions, on disk per environment)
//! and what its ripples did; `rmenu` its pages and keys. A cluster is made
//! with its BOUNDARIES and seeded by the policy's `seed_cluster` ripple
//! through one seam (`seeding`), whose stand-in today is a local matcher.
//!
//! Iteration nine (task 0103): every recorded gap closed on fake data — a
//! face re-reads what another face on its environment changed (`store::Watch`),
//! things made here are managed here (`manage`: rename, move and delete a
//! cluster, rename and remove an account, add a pool, register a repo), the
//! repo registry and unclaimed regions open on Settings, the flip log, the
//! Engine's cross-cluster waits.
//!
//! His msg-165 review (task 0147): four cauldron tabs — Work, Status, System,
//! Config — and six cluster tabs — Overview (boxes), Tasks (questions and
//! human actions, each a box; `tickets` reads a human action's steps in the
//! real ticket format), Soup, Artifacts, Status (the ripples and the engine
//! as bands), Config. `status` is the cauldron's Status tab (devices,
//! services and token sources as bands, running and parked); Config is
//! collapsible bands over the settings (`menu`), each setting explained only
//! in its ⓘ's modal. The input is the DUMP, on Work only. The cauldron has a
//! switch at the top left, and its name is a dropdown of the cauldrons. The
//! Threads tab and its data are gone. Scrolling never changes a row's shape.
//!
//! His msg-186 review, the structure (task 0165): Work is CONTENTS; the
//! dump area is the BUCKET (DUMP is its send), with `⧉ copy`, ⌥A and ⌥M;
//! Overview is bands; Soup and Artifacts are one tab (REGIONS · ARTIFACTS ·
//! REPOS · DIRECTORIES — an artifact is a soup node wrapping a file);
//! Questions has an ASSUMED band; a cluster carries no budget; Status,
//! System and Config have sub-tab strips; the cauldron's switch is Config ›
//! Cauldron's first row, BREWING or BANKED, asked before it flips; Config
//! is Cauldron and Global, the token sources kept once for every cauldron
//! (`settings::is_global`, `dash::GlobalBoard`); aggregate ripples are
//! SPLASHES.
//!
//! His msg-186 mode (task 0167): the word at the top left is a button that
//! switches the face between CAULDRON (the tabs above) and THREADS
//! (`threads`: the harness chat face — the thread list, a transcript, the
//! message box, the subagent tree — rebuilt in this face's visual system on
//! its own data, `<seeds>/threads.json`).
//!
//! His msg-235 review, the structure (task 0190): the top left is the
//! product's name, CAULDRON, and the cauldrons dropdown — no mode button;
//! the cauldron tabs are SURFACE (the soup above, its CLUSTERS in the
//! sidebar — what were surfaces — and the ONE bucket below, two boxes, a
//! divider dragging between them), THREADS (the threads app, a tab now),
//! STATUS (Mesh · Tokens · Soup · Bus — the event bus explored read-only,
//! `bus`) and CONFIG. There is no System tab and nothing about the ripples
//! is configured; a cluster has no Config and no settings; a cluster's tabs
//! are Questions · Actions · Tasks · Nodes (it was Soup, then Contents) · Status · Details (it was Overview). A cluster's id is
//! minted (`X-3`), an item's id is the soup's (`[Q-7]`, no cluster in it);
//! every paste is a chip and there is no dictation.
//!
//! REL-6 (the release plan, R-MVP): the tab is THREADS again and it is REAL
//! — `live` is the face's one connection to the threads daemon (the chat
//! face's seam, mirror and store), and `threads` projects that store:
//! Pinned, Recent and Archived, no Projects band, a send through the seam.
//! `data/threads.json` is no longer read.
//!
//! `model` loads the cluster tree and computes every status; `dash` is the fake
//! data behind the Status tab; `settings` is the settings taxonomy, its
//! inheritance, its explanations and its files; `menu` the Config bands;
//! `toggle` the switch widget; `tags` is the tag grammar; `store` is where
//! the dump and the sends live on disk; `app` holds state and handles input
//! with no terminal attached; `view` draws a frame. `cli` is the only part that
//! owns a terminal, and `render_to_text` is the headless path the `--dump` flag
//! and the tests use.

pub mod about;
pub mod app;
pub mod artifacts;
pub mod artpage;
pub mod blueprint;
pub mod bus;
pub mod engine;
pub mod gc;
pub mod ledger;
pub mod look;
pub mod pages;
pub mod refs;
pub mod corpus;
pub mod dash;
pub mod designs;
pub mod dump;
pub mod manage;
pub mod menu;
pub mod settings;
pub mod toggle;
pub mod store;
pub mod tags;
pub mod cli;
pub mod model;
pub mod ripples;
pub mod seeding;
pub mod status;
pub mod supervise;
pub mod live;
pub mod threads;
pub mod tickets;
pub mod view;

use ratatui::backend::TestBackend;
use ratatui::Terminal;

/// Render one frame of `app` at `width`x`height` into plain text, one string
/// per screen row with trailing spaces trimmed.
pub fn render_to_text(app: &mut app::App, width: u16, height: u16) -> anyhow::Result<Vec<String>> {
    let mut term = Terminal::new(TestBackend::new(width, height))?;
    term.draw(|f| view::draw(f, app))?;
    let buf = term.backend().buffer();
    let mut rows = Vec::with_capacity(height as usize);
    for y in 0..height {
        // A wide glyph's second cell holds an empty symbol, so concatenating
        // symbols reproduces the row exactly.
        let s: String = (0..width).map(|x| buf[(x, y)].symbol()).collect();
        rows.push(s.trim_end().to_string());
    }
    Ok(rows)
}

/// One frame of `app` as text, and which screen rows carry the SELECTION
/// background (`tui_render::palette::c().sel_bg` or `tui_render::palette::c().sel_bg_dim`) on any cell — the
/// selection has no glyph (msg 186), so this is how a test sees it.
pub fn render_with_selection(app: &mut app::App, width: u16, height: u16) -> anyhow::Result<(Vec<String>, Vec<usize>)> {
    let mut term = Terminal::new(TestBackend::new(width, height))?;
    term.draw(|f| view::draw(f, app))?;
    let buf = term.backend().buffer();
    let mut rows = Vec::with_capacity(height as usize);
    let mut sel = Vec::new();
    for y in 0..height {
        let s: String = (0..width).map(|x| buf[(x, y)].symbol()).collect();
        rows.push(s.trim_end().to_string());
        let c = tui_render::palette::c();
        if (0..width).any(|x| buf[(x, y)].bg == c.sel_bg || buf[(x, y)].bg == c.sel_bg_dim) {
            sel.push(y as usize);
        }
    }
    Ok((rows, sel))
}

/// The background of the cell at (`x`, `y`) of one frame of `app`.
pub fn render_bg_at(app: &mut app::App, width: u16, height: u16, x: u16, y: u16) -> anyhow::Result<ratatui::style::Color> {
    let mut term = Terminal::new(TestBackend::new(width, height))?;
    term.draw(|f| view::draw(f, app))?;
    Ok(term.backend().buffer()[(x, y)].bg)
}

/// The foreground of the cell at (`x`, `y`) of one frame of `app`.
pub fn render_fg_at(app: &mut app::App, width: u16, height: u16, x: u16, y: u16) -> anyhow::Result<ratatui::style::Color> {
    let mut term = Terminal::new(TestBackend::new(width, height))?;
    term.draw(|f| view::draw(f, app))?;
    Ok(term.backend().buffer()[(x, y)].fg)
}
