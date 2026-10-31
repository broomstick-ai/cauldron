//! # The proto-ledger — one generic thread record, projected into each runtime's native form
//!
//! `docs/proto-ledger.md` is the design; this module is its smallest honest
//! build. **Beside the existing store, not in it** (`docs/ASSUMPTIONS-LOG.md`
//! A5): nothing here reads or writes `events`, `threads.db`, a native session
//! file under an account, or the running daemon. The pieces, in the order the
//! design builds them:
//!
//! * [`event`] — the event shape, its identity, and the append-only [`Ledger`]
//!   with its JSONL file form. The vocabulary is declared through
//!   `shapewire::declare_cells!` so the chassis's compile-time checks run on it.
//! * [`claude_code`] — the one adapter that exists: wire line → event,
//!   ledger → native session file, native file → what it carries.
//! * [`archive`] — the parser over the captured wire (`turns/<thread>/<cookie>.jsonl`)
//!   and its mismatch census, which is how the design is checked against reality.
//! * [`dirty`] — the per-thread dirty mark, reconcile, and the one enforcement
//!   point: placement.
//! * [`spawn`] — a parent causes a child thread to exist, and waits for it by
//!   ending its turn. Four pure functions; no process is launched.
//! * [`turn`] — **the wiring (R4.S4a).** The three moments of a daemon turn on a
//!   thread whose settings say `projection: projected`: render before the
//!   spawn, observe the wire during it, reconcile after the exit; plus the
//!   placement check `runtime::move_thread` asks. The dirty mark is DERIVED
//!   from the ledger here (the last `projected` with no later `reconciled`),
//!   so it survives a daemon restart without a column.
//! * [`cutover`] — the cutover verb's REFUSAL, and nothing else of the verb:
//!   it refuses unless a raw per-account copy exists at the path it names
//!   (H115: *cut over directly, keep raw per-account copies*). The cutover
//!   itself is R4.S4b.
//!
//! **Ledgers are immutable forever.** [`Ledger`] exposes `append` and reads;
//! there is no mutating accessor to a past event, and the file form is
//! append-only. A test in [`event`] forces that red.

pub mod archive;
pub mod claude_code;
pub mod cutover;
pub mod dirty;
pub mod event;
pub mod spawn;
pub mod turn;

pub use event::{
    CycleFound, Draft, EventId, Kind, Ledger, LedgerEvent, Native, ThreadMeta, KIND_WORDS,
};

/// **CUT A FINAL LINE THAT HAS NO NEWLINE** — an append a kill interrupted.
/// Returns how many bytes were cut (0 for a whole file).
///
/// Moved here from `crate::managed` (R4.S4b, HOLDS R18) so BOTH writers of
/// `ledgers/<thread>.jsonl` repair the file before they read it: the managed
/// runtime (`Managed::load`) and the projected turn (`turn::ProjectedTurn`'s
/// `begin` and its re-read in `persist`). Before, a torn tail left by a killed
/// managed append made the projected re-read fail until `Managed::load`
/// happened to run.
///
/// **Call it only under the `ledgers/` write lock** (`Managed::write_lock`):
/// an append in flight on another writer looks exactly like a torn tail.
pub fn repair_torn_tail(p: &std::path::Path) -> std::io::Result<usize> {
    let bytes = std::fs::read(p)?;
    if bytes.is_empty() || bytes.ends_with(b"\n") {
        return Ok(0);
    }
    let keep = bytes.iter().rposition(|b| *b == b'\n').map(|i| i + 1).unwrap_or(0);
    let cut = bytes.len() - keep;
    tracing::warn!(path = %p.display(), cut, "ledger: cutting a torn final line");
    let f = std::fs::OpenOptions::new().write(true).open(p)?;
    f.set_len(keep as u64)?;
    f.sync_all()?;
    Ok(cut)
}

#[cfg(test)]
mod tests {
    /// Strip `//`-comments and string literals, then return the lines whose
    /// remaining CODE contains `needle` (case-insensitively).
    fn code_lines_containing(src: &str, needle: &str) -> Vec<String> {
        src.lines()
            .map(|l| {
                let l = l.split("//").next().unwrap_or("");
                let mut out = String::new();
                let mut in_str = false;
                for c in l.chars() {
                    if c == '"' {
                        in_str = !in_str;
                        continue;
                    }
                    if !in_str {
                        out.push(c);
                    }
                }
                out
            })
            .filter(|l| l.to_ascii_lowercase().contains(needle))
            .map(|l| l.trim().to_string())
            .collect()
    }

    /// **EVERY `.rs` FILE IN THIS DIRECTORY, READ OFF DISK.**
    ///
    /// This used to be a hand-written list of five `include_str!`s, and that was
    /// a guard with an expiry date: the moment a sixth file joined the module it
    /// would have been scanned by nothing, silently, and the property would have
    /// read green while covering less than it claimed. `spawn.rs` was that sixth
    /// file. So the source set is now discovered rather than listed, and the
    /// count is asserted below — a `read_dir` that returned nothing would fail
    /// loudly instead of vacuously passing every scan built on it.
    fn module_sources() -> Vec<(String, String)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ledger");
        let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("the module's own directory {} must be readable: {e}", dir.display()))
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "rs").unwrap_or(false))
            .map(|p| {
                let name = p.file_name().unwrap().to_string_lossy().to_string();
                (name, std::fs::read_to_string(&p).expect("a source file in this module is readable"))
            })
            .collect();
        out.sort();
        // THE SENTINEL FOR THE DISCOVERY ITSELF: the files this module is known
        // to contain must be among what was found, and every one must be
        // non-empty. A scan over an empty set is the failure mode this guards.
        let names: Vec<&str> = out.iter().map(|(n, _)| n.as_str()).collect();
        for known in ["archive.rs", "claude_code.rs", "cutover.rs", "dirty.rs", "event.rs", "mod.rs", "spawn.rs", "turn.rs"] {
            assert!(names.contains(&known), "read_dir found {names:?}, which is missing {known}");
        }
        // AND THE KNOWN LIST ITSELF MAY NOT FALL BEHIND (R4.S4a inspect
        // finding 3): every module `mod.rs` declares must have been found.
        // A module added to the declarations and to no list is scanned, or
        // this fails naming it.
        let declared: Vec<String> = include_str!("mod.rs")
            .lines()
            .filter_map(|l| l.trim().strip_prefix("pub mod ").or_else(|| l.trim().strip_prefix("mod ")))
            .filter_map(|l| l.strip_suffix(';'))
            .map(|m| m.trim().to_string())
            .collect();
        assert!(declared.len() >= 7, "the declared-module parse sees the module's own declarations: {declared:?}");
        for m in &declared {
            let file = format!("{m}.rs");
            assert!(
                names.contains(&file.as_str()),
                "the ban's scope reaches every module mod.rs declares — `{m}` was not scanned (scanned {names:?})"
            );
        }
        assert!(out.iter().all(|(_, s)| s.len() > 100), "every discovered source has content: {names:?}");
        out
    }

    /// **NOTHING IN THIS MODULE BRANCHES ON "SUBAGENT".** The ruling: a managed
    /// subagent is exactly its parent, except `ThreadMeta.parent_id`. So no
    /// type, function, field or condition in the module's source may be keyed
    /// on the word — it may appear only in comments, doc strings, and string
    /// literals (a loss name, a wire key). Scans every source file in the
    /// directory with comments and string literals stripped, and proves on a
    /// synthetic snippet that the scan sees what it looks for.
    ///
    /// **THE MUTATION.** Add `pub fn is_subagent(m: &ThreadMeta) -> bool { m.parent_id.is_some() }`
    /// to `event.rs` — or to `turn.rs` or `cutover.rs`, which the old
    /// hand-written five-file list did not reach. A second mutation, on the
    /// scope rather than the predicate: drop one file from what
    /// `module_sources` returns (`out.retain(|(n, _)| n != "turn.rs")`, the
    /// old stale-list shape) — the scope check reds with "the ban's scope
    /// reaches every module".
    ///
    /// Forced red:
    /// ```text
    /// no code in the ledger module is keyed on `subagent`
    /// ```
    #[test]
    fn nothing_in_the_ledger_module_is_keyed_on_a_child_agent() {
        let probe = concat!("fn x() {\n    if is_sub", "agent(&m) { return; } // keyed\n    let name = \"sub", "agent_interior\";\n}\n");
        assert_eq!(
            code_lines_containing(probe, "subagent"),
            vec![concat!("if is_sub", "agent(&m) { return; }")],
            "the scan sees a condition keyed on the word and ignores a string literal"
        );
        for (name, src) in module_sources() {
            let hits = code_lines_containing(&src, "subagent");
            assert!(hits.is_empty(), "no code in the ledger module is keyed on `subagent` — {name}: {hits:?}");
        }
    }

    /// **NOTHING IN THIS MODULE LAUNCHES A PROCESS.** A managed child is a
    /// thread with a turn waiting in it, and placement runs it; "spawn" in
    /// [`spawn`] names an append, not an `execv`. This module is the one place
    /// where that could go wrong by accident — a file named `spawn.rs` is
    /// exactly where a future reader would reach for `Command::new` — so the
    /// prohibition is positive rather than implied.
    ///
    /// It is also the guard against the costliest mistake in this area: an
    /// agent that launched a real `claude`, on a real login, by wiring one port
    /// and forgetting the other.
    ///
    /// Scans for the process-starting APIs this crate's dependency set
    /// exposes: `std::process`, any `Command::new`, `tokio::process`, and the
    /// libc entry points — `exec*`, `fork`/`vfork`, `posix_spawn*` and
    /// `system`. **That is a list, not a proof of completeness** (R4.S2
    /// corrected an earlier sentence here, which called the first four "the
    /// only ways … plus the libc pair" while `libc::fork`, `posix_spawn` and
    /// `system` went unscanned): a new crate that wraps process creation under
    /// another name would pass this scan. The sentinel is a synthetic snippet
    /// for each needle, so a scan that had stopped matching would fail before
    /// it could report a clean module.
    ///
    /// **THE MUTATION.** Add `let _ = std::process::Command::new("claude");`
    /// to `spawn.rs`.
    ///
    /// Forced red:
    /// ```text
    /// nothing in the ledger module launches a process — spawn.rs
    /// ```
    #[test]
    fn nothing_in_the_ledger_module_launches_a_process() {
        // Each needle with a snippet it MUST catch. `concat!` keeps the
        // sentinels out of their own scan.
        let probes: [(&str, String); 8] = [
            ("std::process", concat!("let c = std::pro", "cess::Command::new(\"claude\");").to_string()),
            ("command::new", concat!("let c = Comm", "and::new(\"claude\");").to_string()),
            ("tokio::process", concat!("use tokio::pro", "cess::Command;").to_string()),
            ("libc::exec", concat!("unsafe { libc::exec", "v(p, a) };").to_string()),
            ("libc::fork", concat!("let pid = unsafe { libc::fo", "rk() };").to_string()),
            ("libc::vfork", concat!("let pid = unsafe { libc::vfo", "rk() };").to_string()),
            ("libc::posix_spawn", concat!("unsafe { libc::posix_sp", "awnp(&mut pid, f, a, b, c, d) };").to_string()),
            ("libc::system", concat!("unsafe { libc::sys", "tem(cmd) };").to_string()),
        ];
        for (needle, probe) in &probes {
            assert_eq!(
                code_lines_containing(probe, needle).len(),
                1,
                "the scan for `{needle}` sees it in `{probe}`"
            );
        }
        for (name, src) in module_sources() {
            for (needle, _) in &probes {
                let hits = code_lines_containing(&src, needle);
                assert!(
                    hits.is_empty(),
                    "nothing in the ledger module launches a process — {name}: `{needle}` in {hits:?}"
                );
            }
        }
    }

    /// **NOTHING OUTSIDE `ThreadMeta`'S OWN CONSTRUCTORS WRITES `parent_id`.**
    /// This is the other half of "a cycle is unconstructible". The type half is
    /// the compiler's: the field is private and the only way to set it is
    /// [`ThreadMeta::child_of`], which takes the parent **by reference**, so a
    /// parent must already exist and every edge points backwards in
    /// construction order. A compiler-enforced property cannot be tested at
    /// runtime, and a test that asserted it anyway would be one of the probes
    /// that cannot fail.
    ///
    /// What IS testable is that the privacy has not been quietly undone — a
    /// `pub parent_id`, or a second writer inside `event.rs` that takes an id
    /// string. So this scans for a write to the field and expects exactly the
    /// two assignments in `root` and `child_of`, by file and by count.
    ///
    /// **THE MUTATION.** Make the field `pub parent_id: Option<String>` again.
    ///
    /// Forced red:
    /// ```text
    /// `parent_id` is written in exactly two places, both of them ThreadMeta's constructors
    /// ```
    #[test]
    fn only_thread_metas_constructors_write_a_parent_pointer() {
        let probe = concat!("    pub parent", "_id: Option<String>,\n");
        assert_eq!(
            code_lines_containing(probe, "parent_id").len(),
            1,
            "the scan sees a re-exposed field declaration"
        );
        for (name, src) in module_sources() {
            let writes = code_lines_containing(&src, "parent_id:");
            if name == "event.rs" {
                assert_eq!(
                    writes.len(),
                    3,
                    "`parent_id` is written in exactly two places, both of them ThreadMeta's \
                     constructors (plus its one private declaration) — got {writes:?}"
                );
                assert!(
                    writes.iter().all(|w| !w.contains("pub ")),
                    "the field is never public — {writes:?}"
                );
            } else {
                assert!(
                    writes.is_empty(),
                    "only `event.rs` writes a parent pointer — {name}: {writes:?}"
                );
            }
        }
    }
}
