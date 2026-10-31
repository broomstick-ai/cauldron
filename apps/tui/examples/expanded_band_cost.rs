//! **WHAT DOES A `see all` PAGE COST PER FRAME?** — `I-12`, and it is the
//! invariant that decides whether "all means all" is honest.
//!
//! # What it used to measure, and why the claim moved
//!
//! It was written for `0072`'s in-place band expansion, and the claim it tested
//! was that expansion is a larger `truncate` over data `load_projects` already
//! holds, so there is nothing to await. That claim is still true and still
//! measured below (`project=`). What CHANGED is the bound: expansion put more
//! rows on the root and was capped at `BAND_EXPANDED_MAX = 500` to keep the
//! frame affordable, and the operator then asked for
//!
//! > a dedicated page with just that band and all results
//!
//! — `all`, with no number in it. So the constant is retired and the bound moves
//! to where its own doc comment always said it belonged: `render::project_list`
//! used to paint EVERY line of the surface and only then crop to the viewport,
//! so a five-thousand-row page would have cost five thousand `band_row` paints
//! per keystroke. It now chooses the window FIRST and paints a screenful.
//!
//! **THE PROPERTY**: per-frame wall time on a 5,000-row page is within 3× of a
//! 50-row page at the same width and height. Not "fast" — within a constant of
//! the small case, which is what O(visible) means and what a scroll step can
//! afford. A scroll step is ONE frame, so `frame=` below is the whole budget a
//! keypress has.
//!
//! **THE BAND IS SYNTHETIC, ON PURPOSE.** His real `~/minds` holds ~200 recent
//! threads (`0035`, `0109`), which is a tenth of the size this has to hold up
//! at; a probe that could only measure what exists today would go quiet exactly
//! when his fleet grew into the case it was written for. The real tree is read
//! too, at the bottom, so the synthetic numbers can be placed against it.
//!
//! **RUN IT IN `--release`.** A debug build's `frame` is dominated by
//! unoptimised string work and says nothing about the shipped face:
//!
//!     cargo run --release -p tui --example expanded_band_cost

use std::time::Instant;
use tui_render::BandId;

/// One synthetic root of `n` dated, distinct threads — the shape `all_headers`
/// walks and `project_list` sorts, with nothing special about it except its
/// size.
fn synth(name: &str, n: usize) -> tui::index::Project {
    tui::index::Project {
        name: name.to_string(),
        root: std::path::PathBuf::from(format!("/synthetic/{name}")),
        threads: (0..n)
            .map(|i| tui::index::IndexThread {
                id: format!("{name}-{i:05}"),
                session_id: format!("{name}-{i:05}"),
                legacy_ids: Vec::new(),
                summary_locked: false,
                summary_set_at: None,
                created_at: None,
                // A real-looking summary, because `band_row` crops and styles it
                // and a one-character label would measure the wrong thing.
                summary: format!(
                    "synthetic thread {i:05} — a summary about as long as the ones he actually has"
                ),
                last_turn_at: Some(
                    chrono::DateTime::parse_from_rfc3339(&format!(
                        "2026-09-01T{:02}:{:02}:{:02}Z",
                        i / 3600 % 24,
                        i / 60 % 60,
                        i % 60
                    ))
                    .expect("fixture stamp")
                    .with_timezone(&chrono::Utc),
                ),
                // **THE SAME STAMP AS `last_turn_at`, and that is deliberate
                // for a PERF fixture.** `0131` split the recents sort key out
                // of `last_turn_at` into its own field; this example measures
                // DRAWING COST and never the order, so the honest fixture value
                // is "he spoke when the turn happened" rather than `None`,
                // which would sort every synthetic row to the bottom and make
                // the band this benchmark fills an empty one.
                last_user_input_at: Some(
                    chrono::DateTime::parse_from_rfc3339(&format!(
                        "2026-09-01T{:02}:{:02}:{:02}Z",
                        i / 3600 % 24,
                        i / 60 % 60,
                        i % 60
                    ))
                    .expect("fixture stamp")
                    .with_timezone(&chrono::Utc),
                ),
                turn_count: 1,
                closed: false,
                pinned: false,
                pin_order: 0,
                queued_count: 0,
            })
            .collect(),
        index: tui::index::IndexState::Read,
    }
}

fn measure(label: &str, rows: usize) -> std::time::Duration {
    let mut store = tui::store::Store::new(String::new(), "/synthetic".to_string());
    store.adopt_projects(vec![synth("synthetic", rows)]);
    let now = chrono::Utc::now();

    let mut vs = tui_render::ViewState {
        // **THE PAGE, NOT THE ROOT.** `Mode::SeeAll(b)` is what makes
        // `render::project_list` the page's body and `focus` what makes the band
        // uncapped; measuring `Mode::Projects` here would measure a ten-row band
        // whatever the fixture held.
        mode: tui_render::Mode::SeeAll(BandId::Recent),
        ..Default::default()
    };

    // The PROJECTION, per frame: this is what a keypress re-runs.
    let t = Instant::now();
    for _ in 0..20 {
        vs.projects = tui::store::project_list(&store, 0, 0, Some(BandId::Recent));
    }
    let project = t.elapsed() / 20;

    // **AND THE SAME FIXTURE PROJECTED AS THE ROOT**, capped at ten. The
    // difference between these two numbers is the only part of the projection
    // the PAGE is responsible for; everything they share is the cost of a fleet
    // this size and was there before a page existed, because `all_headers`
    // walks every header of every project whatever the cap then throws away.
    let t = Instant::now();
    for _ in 0..20 {
        let _ = tui::store::project_list(&store, 0, 0, None);
    }
    let root_project = t.elapsed() / 20;

    let drawn = vs.projects.drawn_len();
    let stops = vs.projects.selectable();

    // **THE MINIMUM OF MANY, NOT THE MEAN, and that is the correct estimator
    // here rather than a flattering one.** This round runs six workers in
    // parallel worktrees, each with a cargo build in flight, so the mean over a
    // loop is the mean of "this frame plus whatever else the machine was doing".
    // Measured: two consecutive runs of the identical 50-row case returned
    // 370µs and 526µs — a 42% spread with no code change between them. A frame
    // cannot be made FASTER by scheduler noise, only slower, so the minimum is
    // the estimate of the work itself and the mean is an estimate of the
    // machine's load. The median is printed beside it so a run where the two
    // diverge wildly is visible rather than quietly averaged.
    let mut worst = std::time::Duration::ZERO;
    for w in [100u16, 60] {
        let mut runs: Vec<std::time::Duration> = Vec::with_capacity(200);
        for _ in 0..200 {
            let t = Instant::now();
            let _ = tui_render::frame_with_hits(&vs, now, w, 40);
            runs.push(t.elapsed());
        }
        runs.sort_unstable();
        let frame = runs[0];
        let median = runs[runs.len() / 2];
        worst = worst.max(frame);
        println!(
            "{label:<26} w={w:<4} rows={drawn:<6} stops={stops:<6} \
             project={project:>9.2?} (root {root_project:>9.2?})  \
             frame min={frame:>9.2?} med={median:>9.2?}"
        );
    }
    worst
}

fn main() {
    println!("── THE PAGE, PER FRAME ─────────────────────────────────────────");
    let small = measure("50-row page", 50);
    // **500 IS THE OLD CEILING**, the `BAND_EXPANDED_MAX` this round retired, so
    // it is the apples-to-apples number: whatever the page costs HERE is what
    // the shipped face already paid for an expanded band.
    let _ = measure("500-row page (old cap)", 500);
    let big = measure("5,000-row page", 5_000);
    println!();
    let ratio = big.as_secs_f64() / small.as_secs_f64().max(f64::MIN_POSITIVE);
    println!("  50 rows    {small:.2?}");
    println!("  5,000 rows {big:.2?}");
    println!("  ratio      {ratio:.2}×   (I-12 wants <= 3.00×)");
    println!(
        "  verdict    {}",
        if ratio <= 3.0 { "PASS" } else { "FAIL" }
    );

    // **AND THE REAL TREE, FOR SCALE.** Read-only, and timed, because "there is
    // nothing to await" would be false at startup even if it is true per frame.
    println!();
    println!("── HIS ACTUAL ~/minds, FOR SCALE ───────────────────────────────");
    let home = std::env::var("HOME").expect("HOME");
    let (projects, _archived, _registry) = tui::index::load_projects(&home);
    let roots = projects.len();
    let mut store = tui::store::Store::new(String::new(), home);
    let t = Instant::now();
    store.adopt_projects(projects);
    let load = t.elapsed();
    let page = tui::store::project_list(&store, 0, 0, Some(BandId::Recent));
    println!(
        "  roots {roots}   recent band on its page {}   load_projects {load:.2?}",
        page.recent.len()
    );

    if ratio > 3.0 {
        std::process::exit(1);
    }
}
