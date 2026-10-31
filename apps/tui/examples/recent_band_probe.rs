//! **DOES THE RECENT BAND ACTUALLY FILL, AGAINST HIS REAL INDEXES?**
//!
//! `0072`'s task file handed down a diagnosis the manager had verified at the
//! line — *"the entire cause is `RECENT_ROWS: usize = 3`"* — and the shared
//! fragment's rule is that a handed-down fix is a HYPOTHESIS until the worker's
//! own measurement agrees. This is that measurement, and it is a probe rather
//! than a test because its input is his actual `~/minds`, which no fixture can
//! stand in for: the thing under suspicion was a SELECTION MECHANISM, and a
//! synthetic index cannot show one that only bites at real scale.
//!
//! **IT IS READ-ONLY, ABSOLUTELY.** `index::load_projects` opens
//! `.mind/threads.json` for reading and nothing else. It never touches the
//! daemon, never touches the AUI, and never touches `threads.db`.
//!
//! It prints four things the diagnosis has to survive:
//!   1. how many rows the collapsed band actually holds,
//!   2. whether they are DISTINCT threads,
//!   3. whether they are in recency order, newest first,
//!   4. where the rows with no `last_turn_at` landed.
//!
//! Run: `cargo run -p tui --example recent_band_probe`

fn main() {
    let home = std::env::var("HOME").expect("HOME");
    let (projects, _archived, _registry) = tui::index::load_projects(&home);
    let roots = projects.len();
    let raw: usize = projects.iter().map(|p| p.threads.len()).sum();

    let mut store = tui::store::Store::new(String::new(), home);
    store.adopt_projects(projects);

    for (label, focus) in [
        ("root (capped at ten)", None),
        ("the recent page (all of them)", Some(tui_render::BandId::Recent)),
    ] {
        let list = tui::store::project_list(&store, 0, 0, focus);
        let ids: Vec<&str> = list.recent.iter().map(|r| r.id.as_str()).collect();
        let mut uniq = ids.clone();
        uniq.sort_unstable();
        uniq.dedup();

        let ordered = list
            .recent
            .windows(2)
            .all(|w| std::cmp::Reverse(w[0].updated) <= std::cmp::Reverse(w[1].updated));
        let undated: Vec<usize> = list
            .recent
            .iter()
            .enumerate()
            .filter(|(_, r)| r.updated.is_none())
            .map(|(i, _)| i)
            .collect();

        println!("── {label} ─────────────────────────────────");
        println!("  roots            {roots}");
        println!("  raw headers      {raw}");
        println!("  totals           {:?}", list.totals);
        println!("  band rows        {}", list.recent.len());
        println!("  distinct ids     {}", uniq.len());
        println!("  newest-first     {ordered}");
        println!("  undated at       {undated:?} (of {})", list.recent.len());
        println!("  pinned rows      {}", list.pinned.len());
        println!("  project rows     {}", list.rows.len());
        println!(
            "  band_note        {:?}",
            list.band_note(tui_render::BandId::Recent)
        );
        println!(
            "  projects note    {:?}",
            list.band_note(tui_render::BandId::Projects)
        );
        for (i, r) in list.recent.iter().take(12).enumerate() {
            println!(
                "    {i:>3}  {:<28}  {:<16}  {}",
                r.summary.chars().take(28).collect::<String>(),
                r.project,
                r.updated
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_else(|| "—".into())
            );
        }
    }
}
