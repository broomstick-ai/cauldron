//! **THE THREAD INDEXES — where the list actually comes from.**
//!
//! # The inversion, and it is the whole of `IS-20`'s fix
//!
//! This face used to ask the daemon *what threads exist*. **The AUI never asks
//! that question and never has.** `0035` established it by reading the live
//! source: `threads/list` appears exactly ONCE in the whole AUI
//! (`apps/aui/src/threads_client.rs:1321`), inside `resync_status`, and its
//! result becomes a table of WHICH SESSION IDS ARE RUNNING — a lookup keyed by
//! id, never a menu. Nothing in the AUI turns a `threads/list` row into a row on
//! screen.
//!
//! What it does instead is what this module does: enumerate the ROOTS the human
//! faces own, read each root's own `.mind/threads.json`, and let that union be
//! the list. **The index drives; the daemon decorates.**
//!
//! # The predicate, and why it is not a filter
//!
//! > A daemon thread belongs on this face's list IFF its id appears as `id`,
//! > `session_id`, or in `legacy_ids` in the index of a root the human faces
//! > own.
//!
//! Measured on the live fleet: **203 of 1,849 survive**, and ZERO of the 1,617
//! threads that self-declare `settings.app_id == "arena"` do. A filter on
//! `threads/list` was considered and is not merely worse, it is **not
//! expressible** — the daemon's SQL is `SELECT … FROM threads ORDER BY
//! updated_at DESC LIMIT ?1` with no `WHERE` clause at all, and its schema has
//! no origin column to filter on. `cwd` is the only candidate and it provably
//! fails: ~300 arena threads carry RELATIVE cwds (`runs/hx-*`) that no
//! project-rooted prefix can exclude, and one project appears under two
//! spellings because the `~/minds` symlink is sometimes resolved and sometimes
//! not.
//!
//! Arena is excluded STRUCTURALLY rather than by pattern: there is no `.mind/`
//! anywhere under `workshops/arena`, and this module reads only project ROOTS.
//!
//! # What this module will not do, and the ONE thing it now does
//!
//! **IT WRITES EXACTLY ONE FILE AND IT IS NOT AN INDEX.**
//! [`apply_registry`] writes `~/.cauldron/projects.json` through
//! `projects::Registry::update` — the crate's own locked, atomic
//! read-modify-write, the SAME call `libs/cauldron/src/bin/cauldron.rs` makes.
//! `0071` §13 sanctions exactly this: *"a third writer, but through the one
//! implementation, and visible to the AUI on its next `Snapshot::get` (a stat)
//! without a restart."* The AUI is built for it —
//! `state::refresh_projects_if_changed` (`apps/aui/src/state.rs:435`) exists
//! because *"this process is not its only writer"*, and is called on root-menu
//! entry (`dispatch/root.rs:47`) and on every v2 projection build
//! (`v2/store.rs:149`). **So `0116`'s "`ui-server` remains the only writer" is
//! superseded by the tree it was written about.**
//!
//! The asymmetry with the paragraph below is the whole reason one is allowed
//! and the other is not: `.mind/threads.json` has an IN-MEMORY MASTER that is
//! rewritten wholesale on every turn, so a write behind its back is undone;
//! the registry has none, and every reader re-stats the file.
//!
//! **It never writes an INDEX.** `.mind/threads.json` is the AUI's private write surface
//! and `ThreadIndex::save` rewrites it WHOLESALE from memory on every turn —
//! the AUI's own comment above its pin handler (`apps/aui/src/v2/mutate.rs:219`)
//! says an on-disk write behind its back "would be silently reverted the next
//! time he spoke". So reading is free and writing is forbidden here; a pin
//! WRITE, when it exists, goes through the AUI's proto-v2 mutation socket on
//! port 9010, which already serves the phone. See [`ThreadIndex`]'s note.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// The AUI's archive set, relative to `$HOME`
/// (`apps/aui/src/v2/archived.rs:57`).
const ARCHIVED: &str = ".aui/archived-projects.json";

/// One header out of a project's index, as this face needs it.
///
/// Every field is optional on the wire. The AUI's own `ThreadHeader` carries
/// twenty-odd more (usage by model, cost, event counts) and marks almost all of
/// them `#[serde(default)]` for the same reason this struct is tolerant: the
/// file is the AUI's and it gains fields without asking anybody.
#[derive(Debug, Clone, Deserialize)]
pub struct IndexThread {
    /// **THE thread id — the same value the daemon keys this thread by.** The
    /// id is minted CLIENT-SIDE (`libs/aui-nav/src/threads.rs:169`) and the
    /// daemon adopts it verbatim, so the decoration join is exact and needs no
    /// translation. That is also why a process calling the daemon directly
    /// creates a daemon row and no header anywhere: "user-generated" is not a
    /// heuristic here, it is which artifact came first.
    pub id: String,
    /// Equal to `id` on everything born after the id migration; kept because
    /// projects that never ran it still carry a different value, and dropping
    /// it would detach those threads from their daemon conversation.
    #[serde(default)]
    pub session_id: String,
    /// Pre-migration ids. PROVENANCE ONLY — nothing is looked up by them here
    /// either, but they count towards membership.
    #[serde(default)]
    pub legacy_ids: Vec<String>,
    #[serde(default)]
    pub summary: String,
    /// **HE NAMED THIS THREAD HIMSELF** — the AUI's manual-summary lock
    /// (`ThreadHeader::set_summary_manual`): no agent-emitted summary may
    /// displace it. Read so a drifting daemon title cannot outrank a name he
    /// gave in the AUI (TUI-P1a, `Store::label_of`).
    #[serde(default)]
    pub summary_locked: bool,
    /// When the lock was engaged — compared against a manual rename made
    /// through the daemon, so the later of two manual names wins.
    #[serde(default)]
    pub summary_set_at: Option<DateTime<Utc>>,
    /// **WHEN THE THREAD STARTED** — `created_at` on the AUI's own
    /// `ThreadHeader` (`libs/aui-nav/src/threads.rs:73`), where it is
    /// non-optional. It is `Option` here for the reason every field on this
    /// struct is tolerant: the file is the AUI's, it gains and loses fields
    /// without asking, and a header written before this field existed must not
    /// cost him the thread. `None` renders as `?` and never as a date the face
    /// made up.
    ///
    /// Read for the SHEET's facts line (§5) and for nothing else — no list is
    /// ordered by it.
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    /// **WHEN HE LAST PUT WORDS INTO THIS THREAD.** `last_user_input_at` on the
    /// AUI's own `ThreadHeader` (`libs/aui-nav/src/threads.rs`), where it is the
    /// `ts` of the newest `user` line in the thread's JSONL log and is written
    /// by exactly two sites — his own turn, and a turn he spoke from another
    /// face that the daemon fold picked up.
    ///
    /// **This, and not `last_turn_at`, is what the recents band is ordered by**
    /// and what the age column renders. Operator directive, verbatim: *"the
    /// recents need to be ordered on last user input, not on when they were
    /// created or any other events, including renames."* `last_turn_at` fails
    /// all three exclusions — it is stamped at thread creation, by a manual
    /// summary set, by an agent-emitted summary on three paths, and at both ends
    /// of a compaction.
    ///
    /// `None` means HE HAS NEVER SPOKEN HERE, and the band reads it that way: it
    /// sorts to the bottom rather than claiming a recency it has not earned. A
    /// header written by an AUI that predates the field also arrives `None`, and
    /// the AUI backfills exactly those off the log when it loads the index — so
    /// by the time this face reads the file, `None` means what it says. This
    /// face does not and must not do that backfill itself: it would be a write
    /// to `.mind/threads.json`, which is forbidden here.
    #[serde(default)]
    pub last_user_input_at: Option<DateTime<Utc>>,
    /// **NOT what any list is ordered by, and not what the age column shows.**
    /// Kept because it is still the DUPLICATE-SURVIVOR key: 46 of his 210 ids
    /// live in two to four project indexes, and the copy with the newest
    /// `last_turn_at` is the one that wins (`store::all_headers`, matching the
    /// AUI's `ledger_fold::resolve_project` exactly). Survivorship wants "which
    /// copy was written to most recently", which is what this field honestly is;
    /// recency-of-speech is [`Self::last_user_input_at`]'s job.
    #[serde(default)]
    pub last_turn_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub turn_count: u64,
    /// Hidden from a cross-project recent feed, KEPT inside its own project and
    /// marked — mirroring `libs/aui-nav/src/threads.rs:77-82`.
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub pin_order: i64,
    #[serde(default)]
    pub queued_count: u64,
}

impl IndexThread {
    /// Every id this header answers to. Membership is a set test over these.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.id.as_str())
            .chain(std::iter::once(self.session_id.as_str()))
            .chain(self.legacy_ids.iter().map(String::as_str))
            .filter(|s| !s.is_empty())
    }
}

/// The on-disk shape, and the LEGACY bare-array shape beside it.
///
/// **An unparseable index degrades to EMPTY, never to an error.** `~/minds/plan/
/// .mind/threads.json` is literally `[]` — valid JSON, wrong shape — and a
/// strict parse would remove that whole project from the face. One bad file
/// must not cost him a project, and a caller that treats `Err` as "skip" cannot
/// tell a missing index from a broken one.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OnDisk {
    Object { threads: Vec<IndexThread> },
    Bare(Vec<IndexThread>),
}

/// One root the human faces own, with its index already read.
#[derive(Debug, Clone)]
pub struct Project {
    /// The `~/minds/<name>` basename — which is ALSO the key the AUI's
    /// mutation socket addresses a pin by, and which is not a field on any
    /// header. A row must keep the project it came from, or a future pin write
    /// has nothing to address.
    pub name: String,
    pub root: PathBuf,
    /// Deduped by id, oldest first by `last_turn_at`.
    pub threads: Vec<IndexThread>,
    /// Which of [`IndexState`]'s four cases reading this root's index took.
    /// **Supersedes the old `no_index: bool`**, which collapsed three of them.
    pub index: IndexState,
}

impl Project {
    pub fn pinned(&self) -> usize {
        self.threads.iter().filter(|t| t.pinned).count()
    }

    pub fn last(&self) -> Option<DateTime<Utc>> {
        self.threads.iter().filter_map(|t| t.last_turn_at).max()
    }
}

/// **WHERE THE LIST OF PROJECTS CAME FROM THIS TIME.**
///
/// Three states, and they are three because they degrade differently. A face
/// that could not tell them apart would have to render "he has no projects"
/// for all three — and for one of them that is a lie he would act on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RegistryState {
    /// `~/.cauldron/projects.json` was read and parsed. The ordinary state,
    /// and the default a test fixture carries: an adopted fixture is not
    /// making a claim about a file it never read.
    #[default]
    Read,
    /// **THE COEXISTENCE WINDOW** (`0071` §10). The registry is ABSENT, so the
    /// list is `projects::Registry::derive_from_minds` — the bootstrap's OWN
    /// derivation, CALLED rather than copied, so the face shows byte-for-byte
    /// the list it showed yesterday and there is still exactly one definition
    /// of that derivation in the tree. An absent file means the AUI has not
    /// booted since the registry landed; it is not an error and it is not a
    /// fallback this face kept.
    NotCreatedYet,
    /// **PRESENT AND UNREADABLE.** Carries the crate's own sentence. The list
    /// is the BUCKET ALONE and the face says why out loud — see
    /// [`crate::store::banner`]. It is emphatically not the same as "he has no
    /// projects", and **this face never repairs the file** — it writes the
    /// registry (see [`apply_registry`]) and refuses to write THIS one, by
    /// exactly the AUI's rule: a file that will not parse is a file a human
    /// must fix, and a writer that ignored that would serialize whatever it
    /// had in hand over it.
    Unreadable(String),
}

/// **THE ROOTS — THE REGISTRY'S OWN VIEW, plus the bucket it appends.**
///
/// # The second derivation this replaced
///
/// This function used to `read_dir(~/minds)` itself. That was the SECOND
/// derivation of a truth `apps/aui` also derived, and the two had already
/// forked once in this arc — the TUI sent `miscellaneous` while the AUI held
/// `_miscellaneous`, and every bucket-thread act from this face was refused.
/// `~/.cauldron/projects.json` is the source of truth for what his projects
/// ARE and `ui-server` is its only writer; this face READS it through
/// `libs/cauldron`, exactly as `apps/aui` does (`0077`).
///
/// **The `~/minds` scan is GONE from this crate.** A derivation still runs
/// when the file is ABSENT, and it is the CRATE'S —
/// `Registry::load_or_derive`, which is the same code the AUI's bootstrap
/// calls to MINT the file. One derivation, two uses, never two definitions.
/// That is the coexistence window `0071` §10 specifies, not a shim.
///
/// The bucket is appended by `Registry::view` — last, always present, never
/// archivable — at `~/.aui/miscellaneous`, which is where it has always been.
/// The order of the rest is the REGISTRY'S order and is not re-sorted here;
/// the crate's derivation sorts by name, so in the coexistence window the
/// order is the one his hand already knows.
pub fn discover_roots(home: &str) -> Vec<(String, PathBuf)> {
    registry_roots(home).0
}

/// [`discover_roots`] with the answer to *"and where did that come from?"*.
/// ONE read of the file, not two.
pub fn registry_roots(home: &str) -> (Vec<(String, PathBuf)>, RegistryState) {
    let home = Path::new(home);
    match projects::Registry::load_or_derive(home) {
        Ok((reg, source)) => {
            let rows = reg
                .view(home)
                .into_iter()
                .map(|r| (r.name, r.path))
                .collect();
            let state = match source {
                projects::Source::File => RegistryState::Read,
                projects::Source::DerivedFromMinds => RegistryState::NotCreatedYet,
            };
            (rows, state)
        }
        // **NOT FAIL-OPEN TO `~/minds`, and the direction is the argument.**
        // An unreadable registry is a file a human must repair. Quietly
        // re-deriving from the relic would make this face disagree with the
        // AUI about what his projects are at exactly the moment nobody is
        // watching — and a second derivation that only runs sometimes is worse
        // than one that always runs, because it forks under the conditions
        // nobody tests. So the list is the BUCKET ALONE — the one root that is
        // not in the file and never was — and the reason is carried out in
        // words rather than inferred from an empty screen.
        Err(e) => {
            let why = e.sentence();
            tracing::error!(
                error = %why,
                "the project registry is present and unreadable — the project list is the \
                 bucket alone until it parses; this face never writes it"
            );
            (
                vec![(projects::BUCKET.to_string(), projects::bucket_root(home))],
                RegistryState::Unreadable(why),
            )
        }
    }
}

/// **THE FOUR STATES A ROOT'S INDEX CAN BE IN, and they are no longer three of
/// them wearing one bit.**
///
/// `load_index` used to return `(threads, no_index: bool)` and collapse
/// *"there is no `.mind/` here"*, *"the file is absent"* and *"the file is
/// present and CORRUPT"* into one `true`. That bit's only surface was the words
/// `no index` on the project row — which the operator asked to be rid of:
///
/// > Don't tell me no index, just recompute the indexes if they aren't
/// > detected.
///
/// **THERE IS NOTHING TO RECOMPUTE, and that is a measurement rather than a
/// judgement.** A scout read a copy of his ledger — 1,849 threads, all 33 roots
/// — and found that ALL TWENTY of his indexless projects have zero thread logs,
/// zero daemon threads and zero `aui_project` rows. They are directories he has
/// never held a thread in, so a recompute computes nothing for twenty of the
/// twenty real cases, and the ask is satisfied by a RENDER change with no write
/// to `.mind/threads.json` — which is the AUI's private surface and the
/// recorded clobber hazard.
///
/// But the CORRUPT case must not be absorbed into "empty" while the banned
/// sentence goes, and this is why: `libs/aui-nav`'s `ThreadIndex::load`
/// degrades an unparseable index to an EMPTY one with a warning, and the next
/// `ThreadIndex::save` writes that empty index back as a whole-file write. A
/// corrupt `threads.json` plus one turn in that project permanently replaces
/// his header list. Zero projects are in that state today; that is luck, not
/// protection, and this face's row is the only surface anywhere that would show
/// it. So the distinction is REPLACED, not deleted — it moves out of prose and
/// into the type, where a test can hold it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
    /// No `.mind/threads.json` at all — nothing has ever written one here.
    /// A TRUE zero.
    Absent,
    /// Read and parsed. The count is real, and it may legitimately be zero:
    /// `~/minds/plan`'s index is literally `[]`.
    Read,
    /// **PRESENT AND UNREADABLE.** The file is there and we could not turn it
    /// into headers. The count is NOT zero, it is UNKNOWN, and saying zero here
    /// is honesty rule 1's exact violation.
    Corrupt,
}

/// Read one root's index. **Never fails** — every path returns a list — but it
/// now says WHICH of [`IndexState`]'s cases it took.
pub fn load_index(root: &Path) -> (Vec<IndexThread>, IndexState) {
    let path = root.join(".mind").join("threads.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (Vec::new(), IndexState::Absent);
    };
    match serde_json::from_str::<OnDisk>(&text) {
        Ok(OnDisk::Object { threads }) | Ok(OnDisk::Bare(threads)) => {
            (dedupe(carry_the_old_clock(threads, &text)), IndexState::Read)
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e,
                "thread index is neither {{threads: [...]}} nor [...] — the COUNT IS UNKNOWN, \
                 not zero; see aui-nav ThreadIndex::save, which would overwrite it");
            (Vec::new(), IndexState::Corrupt)
        }
    }
}

/// **THE MIGRATION WINDOW, AND IT IS NOT OPTIONAL.**
///
/// `last_user_input_at` is written by an AUI this face does not control and
/// cannot restart. Between the commit that adds the field and the moment his
/// `ui-server` is next rebuilt, EVERY header on disk is missing it — measured on
/// his live fleet at the time of writing: 0 of 210 carry it. Read naively, that
/// is 210 threads all claiming "he has never spoken here", which the band reads
/// as "no recency at all" and renders as an undated pile in arbitrary order.
/// **That is a worse face than the wrong order it replaces, and it would land on
/// him without the AUI changing at all.**
///
/// So `None` is split back into the two facts it collapses, exactly as
/// `ThreadFacts`'s `absent` / `?` distinction splits them for the daemon
/// (`store::row_of`): a header whose writer KNOWS the field is believed,
/// `null` — "never spoken" — included; a file whose writer has never heard of
/// the field is read on the old clock, because `last_turn_at` is the best
/// answer that file can give and an arbitrary order is not an answer.
///
/// **The detection is the raw text, and it has to be.** `serde` folds a missing
/// key and an explicit `null` into the same `None` for `Option<T>`, which is the
/// one distinction that matters here, so the question is asked of the BYTES
/// instead: does this file's writer mention the field at all? An index written by
/// the new AUI always does — a `None` header serialises as
/// `"last_user_input_at": null` — so a fleet of genuinely never-spoken threads is
/// NOT mistaken for an old file.
///
/// **It is self-deleting in effect and the condition for deleting it in source
/// is exact:** when no `.mind/threads.json` anywhere lacks the key, this
/// function is dead. It is not a fallback for a defect — it is a compatibility
/// read of a file whose writer has more than one version in the wild, which is
/// the property `IndexThread`'s own header comment already claims ("the file is
/// the AUI's and it gains fields without asking anybody").
fn carry_the_old_clock(mut threads: Vec<IndexThread>, raw: &str) -> Vec<IndexThread> {
    if raw.contains("\"last_user_input_at\"") {
        return threads;
    }
    for t in threads.iter_mut() {
        t.last_user_input_at = t.last_turn_at;
    }
    threads
}

/// **DEDUPE BY ID, KEEPING THE NEWEST HEADER**, then sort ascending by
/// `last_turn_at` so the newest sits at the bottom (`DIR-3`).
///
/// This is not hypothetical tidiness. `.mind/threads.json` is TRACKED IN GIT,
/// so a repo forked or seeded from another carries the other's headers: 46 ids
/// appear in three or four indexes, accounting for 101 of the fleet's 305
/// headers. Without this a third of his list is visibly duplicated.
///
/// The AUI's own lookup is sloppy here — `apps/aui/src/v2/store.rs:78`
/// `find_map`s the FIRST match in arbitrary `HashMap` order. That is a latent
/// bug to avoid, not a rule to copy.
fn dedupe(mut threads: Vec<IndexThread>) -> Vec<IndexThread> {
    threads.sort_by(|a, b| a.last_turn_at.cmp(&b.last_turn_at).then(a.id.cmp(&b.id)));
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<IndexThread> = Vec::new();
    // Walk NEWEST first so the survivor is the newest header, then restore the
    // ascending order the list renders in.
    for t in threads.into_iter().rev() {
        if t.id.is_empty() || seen.contains(&t.id) {
            continue;
        }
        seen.push(t.id.clone());
        out.push(t);
    }
    out.reverse();
    out
}

/// **THE ARCHIVE SET — "I don't want to delete it, but I don't want to see it
/// all the time."**
///
/// `~/.aui/archived-projects.json`, a map of project name to archived-at. It is
/// read here for one reason the AUI's own module states as doctrine: *"the
/// renderings of his world must not disagree about which projects exist"*
/// (`apps/aui/src/v2/archived.rs`). A project he archived on the phone is
/// hidden on the desk too, because archive is a statement about the PROJECT and
/// he is one operator with one attention.
///
/// **FAIL OPEN, and mean it.** Missing, unreadable or corrupt returns EMPTY,
/// which hides NOTHING. Hiding is the dangerous direction: a bad preference
/// file must never make his projects vanish, and the file is the AUI's to
/// repair — this face never writes it, exactly as it never writes an index.
pub fn load_archived(home: &str) -> Vec<Archived> {
    let path = Path::new(home).join(ARCHIVED);
    let Ok(text) = std::fs::read_to_string(&path) else {
        // The ordinary state of a machine where he has never archived
        // anything. Silent on purpose.
        return Vec::new();
    };
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(serde_json::Value::Object(map)) => map
            .into_iter()
            // **`miscellaneous` IS NEVER IN THE ARCHIVE SET, whatever the file
            // says.** It is the un-projected bucket: there is nowhere for its
            // threads to go, so it is not archivable, and a stale line naming
            // it must not put a row in the archived room that would take him
            // somewhere that cannot exist. The same rule `load_projects` has
            // always applied to hiding, applied now to the set itself.
            .filter(|(name, _)| name != "miscellaneous")
            .map(|(name, v)| Archived {
                name,
                // The value is an RFC-3339 instant on every line the AUI
                // writes. A line that is not one loses its DATE and keeps its
                // NAME: `?` for an age is honest, and dropping the row because
                // its timestamp was unreadable would hide a project he can
                // still bring back.
                at: v
                    .as_str()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|t| t.with_timezone(&Utc)),
            })
            .collect(),
        _ => {
            tracing::warn!(path = %path.display(),
                "the archive set is not a JSON object — showing EVERY project");
            Vec::new()
        }
    }
}

/// **ONE PROJECT IN THE ARCHIVE SET** — its name, and when he archived it.
///
/// The date is read for the archived room's age column and for nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archived {
    pub name: String,
    pub at: Option<DateTime<Utc>>,
}

/// Every root, with its index read, **minus the ones his archive set hides**.
/// **This is the whole universe of the face.**
///
/// Returns the survivors and **THE ARCHIVE SET ITSELF**, because the root now
/// draws a DOOR onto that set rather than a tally of what was hidden.
///
/// It used to return the hidden COUNT, and the two numbers can differ: a name
/// in the file whose directory has since gone from `~/minds` hides nothing and
/// would have counted zero, while the archived room still has a row for it —
/// and the AUI's own unarchive answers that row with *"its directory is gone
/// from your minds folder."* **A door's label must describe what is behind the
/// door**, so the count is now the set's size. Fail-open is untouched: a
/// missing or corrupt file is an EMPTY set, which hides nothing and counts
/// zero.
///
/// **And THIRD, [`RegistryState`] — where the project list itself came from.**
/// It rides back with the list rather than being asked for separately, so the
/// answer describes the read that produced THESE rows and cannot be a second,
/// later read of a file that moved in between.
pub fn load_projects(home: &str) -> (Vec<Project>, Vec<Archived>, RegistryState) {
    let archived = load_archived(home);
    let (roots, registry) = registry_roots(home);
    let projects = roots
        .into_iter()
        .filter(|(name, _)| {
            // `miscellaneous` is not a project he can archive — it is the
            // un-projected bucket and there is nowhere for its threads to go.
            // (`load_archived` already drops it from the set; this is the
            // second, independent guard on the same fact.)
            let drop = name != "miscellaneous" && archived.iter().any(|a| &a.name == name);
            !drop
        })
        .map(|(name, root)| {
            let (threads, index) = load_index(&root);
            Project {
                name,
                root,
                threads,
                index,
            }
        })
        .collect();
    (projects, archived, registry)
}

/// Every id every root's index answers to — the membership set the daemon's
/// rows are tested against.
/// **THE ONE WRITE THIS FACE MAKES**, and it is the registry, through the
/// registry's own one implementation.
///
/// See this module's header for why this is allowed where an index write is
/// not, and [`crate::acts::RegistryAct`] for why the act has no mutation
/// behind it. `home` is a parameter for the reason the whole crate takes one:
/// every test runs against a `tempfile` root and his real `~/.cauldron` is
/// never the thing a test opens.
///
/// **Blocking.** `Registry::update` takes an advisory lock and does a
/// temp/fsync/rename; the caller runs it off the UI thread.
///
/// The error is the CRATE'S OWN SENTENCE, relayed and never paraphrased —
/// `projects::Refusal` and `projects::UpdateError` both carry wording written
/// for a human, and this face is not the party that knows why a registration
/// was refused.
pub fn apply_registry(home: &str, w: &crate::acts::RegistryWrite) -> Result<(), String> {
    let home = Path::new(home);
    // ── THE GUARD THAT MATTERS MOST, AND IT IS NOT ABOUT LOCKING ──────────
    //
    // **`projects::Registry::update` ON AN ABSENT FILE STARTS FROM
    // `Registry::empty()` AND WRITES IT** (`libs/cauldron/src/lib.rs:794-797`).
    // That is correct for `cauldron register` on a machine the AUI has never
    // booted — it is how the file legitimately comes into being — and it is
    // CATASTROPHIC here, because an absent registry is exactly the state in
    // which this face is drawing `RegistryState::NotCreatedYet`: thirty-odd
    // projects DERIVED from the `~/minds` relic, none of which are in a file
    // yet. One `p` in that window would mint a registry holding ONE entry,
    // `ui-server`'s `bootstrap_if_absent` would then find the file present and
    // never derive again, and his list would be one project long on both
    // faces. It is `0099`'s degrade-to-empty hazard, one file over.
    //
    // `apps/aui` reached the same conclusion and its wording is kept, because
    // the two faces should refuse the same act the same way: *"this process is
    // the minter, so if the file is absent when a handler wants to write, the
    // boot bootstrap did not happen, and the write would replace his whole
    // list with the one entry it is adding"* (`dispatch/mod.rs:243-250`).
    // Here it is stronger still: this process is NOT the minter.
    //
    // The UNREADABLE case is refused by `update` itself — it returns
    // `UpdateError::Load` before the closure runs — and is checked here too so
    // the refusal is predictable rather than racing the lock.
    match projects::Registry::load(home) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return Err("I can't write the project registry: it isn't there yet.                         The audio interface creates it at startup, so this needs                         ui-server to have run rather than a retry."
                .to_string())
        }
        Err(e) => {
            return Err(format!(
                "I can't write the project registry: {} I won't overwrite it until it reads.",
                e.sentence()
            ))
        }
    }
    match w {
        crate::acts::RegistryWrite::Register { path, name } => {
            // `register` canonicalizes, so a path that is a symlink into
            // `~/minds` lands as the real directory and a second registration
            // of the same tree is refused by PATH rather than by name.
            projects::Registry::update(home, |r| {
                r.register(path, name.as_deref()).map(|_| ())
            })
            .map_err(|e| e.sentence())
        }
        crate::acts::RegistryWrite::MoveBefore { name, before } => {
            projects::Registry::update(home, |r| r.move_before(name, before.as_deref()))
                .map_err(|e| e.sentence())
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
//  PATH COMPLETION — `p`'s autocomplete, with no modal in it
// ────────────────────────────────────────────────────────────────────────────

/// What one `tab` did to what he had typed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Completion {
    /// What the field should now hold. Equal to the input when nothing could
    /// be added — a completion that changes nothing must not look like one
    /// that did.
    pub text: String,
    /// The directory basenames that still match, in the order they will be
    /// shown. Empty means nothing matched, which is a fact to say out loud.
    pub matches: Vec<String>,
    /// True when exactly one directory matched and `text` is now that
    /// directory with its trailing `/` — the case where the next `tab` walks
    /// INTO it rather than sitting on it.
    pub unique: bool,
}

/// **COMPLETE A TYPED PATH TO ITS LONGEST UNAMBIGUOUS PREFIX.**
///
/// The whole of `p`'s autocomplete, and it is a pure-ish function over one
/// `read_dir` rather than a modal: `0119` gives this worker `acts.rs`,
/// `aui.rs`, `index.rs` and the key table, and a picker overlay is
/// `render/src/{model,render,select}.rs` — a sibling's files this round. A
/// completion applied INTO the field needs no new surface at all, so the
/// capability lands without a merge conflict against work nobody has finished.
///
/// - `~` and `~/…` expand against `home`; nothing else is expanded, because
///   `$VAR` expansion would be a second shell nobody asked for.
/// - **DIRECTORIES ONLY.** `Registry::register` refuses a file with its own
///   sentence, so offering one would be offering a refusal.
/// - Dotfiles appear only when the fragment he has typed starts with a dot:
///   `~/c` must not offer `.cauldron`, and `~/.c` must.
/// - A trailing `/` means *"list this directory"*, which is what makes a
///   second `tab` walk down a chain.
pub fn complete_path(home: &str, typed: &str) -> Completion {
    // **`~`, `.` AND `..` NAME A DIRECTORY, SO COMPLETING THEM MEANS LISTING
    // IT.** Without this they fall through to the split below and the
    // FRAGMENT becomes the last component of the expanded path — so `~` would
    // offer the siblings of his home directory and splice their names onto a
    // `~` with no separator. Measured while writing the test for it.
    let typed = match typed.trim() {
        t @ ("~" | "." | "..") => format!("{t}/"),
        t => t.to_string(),
    };
    let typed = typed.as_str();
    let expanded = expand_tilde(home, typed);
    // Split into "the directory to read" and "the fragment to match". A
    // trailing separator means the fragment is empty and the whole directory
    // is the candidate set.
    let (dir, frag) = match expanded.rfind('/') {
        Some(i) => (expanded[..=i].to_string(), expanded[i + 1..].to_string()),
        // No separator at all — there is no directory to read and nothing to
        // complete against. `p` takes an ABSOLUTE-ish path; a bare word is a
        // project NAME and is not this function's business.
        None => {
            return Completion {
                text: typed.to_string(),
                matches: vec![],
                unique: false,
            }
        }
    };
    let read_from = if dir.is_empty() { "/" } else { dir.as_str() };
    let mut names: Vec<String> = match std::fs::read_dir(read_from) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with(&frag))
            .filter(|n| frag.starts_with('.') || !n.starts_with('.'))
            .collect(),
        Err(_) => vec![],
    };
    names.sort();
    if names.is_empty() {
        return Completion {
            text: typed.to_string(),
            matches: vec![],
            unique: false,
        };
    }
    let common = longest_common_prefix(&names);
    let unique = names.len() == 1;
    // Rebuild in the SPELLING HE TYPED: if he wrote `~/`, he gets `~/` back.
    // Re-writing his `~` as `/Users/<him>` would be this face editing his text
    // to say the same thing in its own words.
    let head = &typed[..typed.len() - frag.len()];
    let mut text = format!("{head}{common}");
    if unique && !text.ends_with('/') {
        text.push('/');
    }
    Completion {
        text,
        matches: names,
        unique,
    }
}

/// `~` and `~/…` only. See [`complete_path`].
fn expand_tilde(home: &str, typed: &str) -> String {
    if typed == "~" {
        return home.to_string();
    }
    match typed.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", home.trim_end_matches('/')),
        None => typed.to_string(),
    }
}

fn longest_common_prefix(names: &[String]) -> String {
    let Some(first) = names.first() else {
        return String::new();
    };
    let mut end = first.len();
    for n in &names[1..] {
        let mut i = 0;
        for (a, b) in first.bytes().zip(n.bytes()) {
            if a != b {
                break;
            }
            i += 1;
        }
        end = end.min(i);
    }
    // Never cut a multi-byte character in half.
    while end > 0 && !first.is_char_boundary(end) {
        end -= 1;
    }
    first[..end].to_string()
}

/// **THE PATH `p` ACTUALLY REGISTERS**, with `~` expanded exactly as
/// [`complete_path`] expands it. One expander, two uses: a path he completed
/// and a path he typed whole must resolve to the same directory, or the
/// completion would be showing him one thing and the act would register
/// another.
pub fn resolve_typed_path(home: &str, typed: &str) -> PathBuf {
    PathBuf::from(expand_tilde(home, typed.trim()))
}

pub fn owned_ids(projects: &[Project]) -> std::collections::HashSet<String> {
    projects
        .iter()
        .flat_map(|p| p.threads.iter())
        .flat_map(|t| t.ids())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().expect("has a parent")).expect("mkdir");
        std::fs::write(p, body).expect("write");
    }

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("tui-index-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("mkdir");
        p
    }

    /// **THE DEGRADATION, FORCED.** Three shapes that must all yield a list
    /// rather than an error, because one bad file must never cost him a
    /// project. The bare-array case is not invented: `~/minds/plan`'s index is
    /// literally `[]`.
    #[test]
    fn an_unparseable_index_degrades_to_empty_and_never_to_an_error() {
        let home = tmp("degrade");
        write(
            &home,
            "minds/legacy/.mind/threads.json",
            r#"[{"id":"a","last_turn_at":"2026-01-01T00:00:00Z"}]"#,
        );
        write(&home, "minds/plan/.mind/threads.json", "[]");
        write(
            &home,
            "minds/broken/.mind/threads.json",
            "{ this is not json",
        );
        write(&home, "minds/empty-dir/.keep", "");
        write(
            &home,
            "minds/modern/.mind/threads.json",
            r#"{"threads":[{"id":"b","session_id":"b","last_turn_at":"2026-01-02T00:00:00Z"}]}"#,
        );

        let (ps, _, _) = load_projects(home.to_str().expect("utf8"));
        let by = |n: &str| {
            ps.iter()
                .find(|p| p.name == n)
                .expect("project present")
                .clone()
        };

        // The LEGACY bare-array shape parses. This is the tolerance that keeps
        // `plan` on the list.
        assert_eq!(by("legacy").threads.len(), 1);
        assert_eq!(by("legacy").index, IndexState::Read);
        // `[]` is a valid bare array: empty, PRESENT, and READ. A true zero.
        assert!(by("plan").threads.is_empty());
        assert_eq!(
            by("plan").index,
            IndexState::Read,
            "`[]` is an index, and an empty one"
        );
        // **THE THREE STATES THAT USED TO BE ONE BIT**, now distinguished.
        // Genuinely unparseable: EMPTY, flagged CORRUPT, project SURVIVES.
        assert!(by("broken").threads.is_empty());
        assert_eq!(
            by("broken").index,
            IndexState::Corrupt,
            "a broken index must not be indistinguishable from an empty one — \
             aui-nav would overwrite it"
        );
        // No index at all is ABSENT, which is a true zero and not a corruption.
        assert_eq!(by("empty-dir").index, IndexState::Absent);
        assert_ne!(
            by("empty-dir").index,
            by("broken").index,
            "absent and corrupt collapsed back into one state"
        );
        assert_eq!(by("modern").threads.len(), 1);
        // FORCED FAILURE, the other way: none of the above removed a root.
        assert_eq!(
            ps.len(),
            6,
            "five dirs + miscellaneous; a bad file cost him a project: {:?}",
            ps.iter().map(|p| &p.name).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// **THE MIGRATION WINDOW — an index written by an AUI that has never heard
    /// of `last_user_input_at` is read on the OLD clock, and one that has is
    /// believed absolutely.**
    ///
    /// Measured at the time this landed: 0 of his 210 live headers carried the
    /// key, because his `ui-server` had not been rebuilt and a worker may not
    /// restart it. Without this, every one of those 210 would have read as
    /// "never spoken here" and the recents band would have come back undated and
    /// in arbitrary order — a REGRESSION on the very band the round was fixing,
    /// landing on him with no AUI change at all.
    ///
    /// FORCED RED, three ways, each measured: delete the `carry_the_old_clock`
    /// call and `legacy` comes back `None`; make the shim unconditional and
    /// `modern`'s deliberate `null` is overwritten with its rename stamp, which
    /// is the whole defect coming back through the compatibility path; key the
    /// detection on the parsed `Option` instead of the raw bytes and `modern`
    /// fails the same way, because serde folds a missing key and an explicit
    /// `null` into one value.
    #[test]
    fn an_index_that_predates_last_user_input_is_read_on_the_old_clock() {
        let home = tmp("migration-window");
        // OLD WRITER: the key does not appear in the file at all.
        write(
            &home,
            "minds/legacy/.mind/threads.json",
            r#"{"threads":[
                {"id":"a","summary":"spoken in July, renamed in September",
                 "last_turn_at":"2026-09-28T21:14:47Z","turn_count":5}
            ]}"#,
        );
        // NEW WRITER, EVERY ANSWER `null`: a project he has created threads in
        // and never spoken in. THIS is the fixture that makes the raw-bytes
        // detection load-bearing — read through the parsed `Option` instead,
        // this index is indistinguishable from `legacy` above and every row
        // would silently take its rename stamp.
        write(
            &home,
            "minds/fresh/.mind/threads.json",
            r#"{"threads":[
                {"id":"d","summary":"","last_user_input_at":null,
                 "last_turn_at":"2026-09-28T23:58:00Z","turn_count":0},
                {"id":"e","summary":"","last_user_input_at":null,
                 "last_turn_at":"2026-09-28T23:59:00Z","turn_count":0}
            ]}"#,
        );
        // NEW WRITER: the key is there, and one header's answer is `null` —
        // "he has never spoken here" — which must SURVIVE, not be repaired.
        write(
            &home,
            "minds/modern/.mind/threads.json",
            r#"{"threads":[
                {"id":"b","summary":"he spoke",
                 "last_user_input_at":"2026-07-25T04:43:53Z",
                 "last_turn_at":"2026-09-28T21:14:47Z","turn_count":5},
                {"id":"c","summary":"he never spoke",
                 "last_user_input_at":null,
                 "last_turn_at":"2026-09-28T23:59:00Z","turn_count":0}
            ]}"#,
        );
        let (ps, _, _) = load_projects(home.to_str().expect("utf8"));

        let legacy = ps.iter().find(|p| p.name == "legacy").expect("present");
        let a = &legacy.threads[0];
        assert_eq!(
            a.last_user_input_at, a.last_turn_at,
            "an index whose writer predates the field came back undated — every \
             row would sort to the bottom of the recents band at once"
        );
        assert!(a.last_user_input_at.is_some(), "and it must be SOME, not None");

        let modern = ps.iter().find(|p| p.name == "modern").expect("present");
        let b = modern.threads.iter().find(|t| t.id == "b").expect("b");
        let c = modern.threads.iter().find(|t| t.id == "c").expect("c");
        assert_eq!(
            b.last_user_input_at,
            Some(
                DateTime::parse_from_rfc3339("2026-07-25T04:43:53Z")
                    .expect("fixture")
                    .with_timezone(&Utc)
            ),
            "the new writer's value was not taken verbatim"
        );
        assert_ne!(
            b.last_user_input_at, b.last_turn_at,
            "the witness failed: this fixture no longer distinguishes the two \
             clocks, so the assertion above proves nothing"
        );
        assert_eq!(
            c.last_user_input_at, None,
            "a DELIBERATE `null` from a writer that knows the field was \
             overwritten with its rename stamp — the shim must not reach a \
             modern index at all"
        );

        // And the all-null modern index: BOTH headers keep their `None`.
        let fresh = ps.iter().find(|p| p.name == "fresh").expect("present");
        assert_eq!(fresh.threads.len(), 2);
        for t in &fresh.threads {
            assert_eq!(
                t.last_user_input_at, None,
                "an index written by a MODERN AUI in which he has simply never \
                 spoken was mistaken for a legacy file, and every row took its \
                 creation stamp — which is exclusion (1) coming back through the \
                 compatibility path"
            );
            // The witness: these rows DO carry the old clock, so the assertion
            // above is about the shim and not about an empty fixture.
            assert!(t.last_turn_at.is_some());
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The git-shared-header case, which is a third of his real fleet.
    #[test]
    fn a_thread_in_four_indexes_is_one_row_per_project_and_the_newest_header_wins() {
        let home = tmp("dedupe");
        write(
            &home,
            "minds/forked/.mind/threads.json",
            r#"{"threads":[
                {"id":"dup","summary":"the stale copy","last_turn_at":"2026-01-01T00:00:00Z"},
                {"id":"dup","summary":"the newest copy","last_turn_at":"2026-03-01T00:00:00Z"},
                {"id":"other","summary":"alone","last_turn_at":"2026-02-01T00:00:00Z"}
            ]}"#,
        );
        let (ps, _, _) = load_projects(home.to_str().expect("utf8"));
        let p = ps.iter().find(|p| p.name == "forked").expect("present");
        assert_eq!(p.threads.len(), 2, "the duplicate id was not folded");
        let dup = p.threads.iter().find(|t| t.id == "dup").expect("kept");
        assert_eq!(dup.summary, "the newest copy", "the stale header won");
        // …and the survivors are ASCENDING, so the newest is at the bottom.
        assert_eq!(p.threads[0].id, "other");
        assert_eq!(p.threads[1].id, "dup");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Membership counts `session_id` and `legacy_ids` too — a pre-migration
    /// thread is his, and dropping it would delete his own history.
    #[test]
    fn membership_includes_the_session_and_legacy_ids() {
        let home = tmp("ids");
        write(
            &home,
            "minds/p/.mind/threads.json",
            r#"{"threads":[{"id":"new","session_id":"sess","legacy_ids":["old1","old2"],
                            "last_turn_at":"2026-01-01T00:00:00Z"}]}"#,
        );
        let (ps, _, _) = load_projects(home.to_str().expect("utf8"));
        let ids = owned_ids(&ps);
        for want in ["new", "sess", "old1", "old2"] {
            assert!(ids.contains(want), "{want} is not in the membership set");
        }
        assert!(
            !ids.contains("arena-thread"),
            "the set admits an unknown id"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// **THE ARCHIVE SET HIDES, AND A BAD ONE HIDES NOTHING.**
    ///
    /// Archive is a statement about the PROJECT — "this stopped mattering to
    /// me" — not about the device it was said on, so a project he archived on
    /// the phone is hidden on the desk too. But hiding is the dangerous
    /// direction, and a preference file that cannot be read must never make his
    /// projects vanish.
    ///
    /// FORCED both ways: drop the `retain` and `dead` comes back onto the root;
    /// make the corrupt-file arm return its keys instead of empty and the
    /// second half goes red with every project gone.
    #[test]
    fn the_archive_set_hides_a_project_and_a_broken_one_hides_nothing() {
        let home = tmp("archive");
        write(&home, "minds/live/.keep", "");
        write(&home, "minds/dead/.keep", "");
        write(
            &home,
            ".aui/archived-projects.json",
            r#"{"dead":"2026-09-01T00:00:00Z"}"#,
        );
        let h = home.to_str().expect("utf8");
        let set = load_archived(h);
        assert_eq!(set.len(), 1, "the archive set is not one project");
        assert_eq!(set[0].name, "dead");
        // The archived-at rides along, because the archived room draws an age.
        assert_eq!(
            set[0].at.expect("an archived-at").to_rfc3339(),
            "2026-09-01T00:00:00+00:00"
        );
        let (ps, arch, _) = load_projects(h);
        assert_eq!(arch.len(), 1, "the archived project was not hidden");
        assert!(ps.iter().any(|p| p.name == "live"));
        assert!(
            !ps.iter().any(|p| p.name == "dead"),
            "an archived project is on the root"
        );
        // `miscellaneous` is never archivable — it is the un-projected bucket
        // and there is nowhere for its threads to go.
        assert!(ps.iter().any(|p| p.name == "miscellaneous"));

        // **FAIL OPEN.** A corrupt file hides NOTHING and counts nothing.
        write(&home, ".aui/archived-projects.json", "{ not json at all");
        let (ps, arch, _) = load_projects(h);
        assert!(arch.is_empty(), "a broken archive file hid something");
        assert!(
            ps.iter().any(|p| p.name == "dead"),
            "a bad preference file made his projects vanish"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// **`miscellaneous` IS NOT IN THE ARCHIVE SET, WHATEVER THE FILE SAYS —
    /// and a line whose date is unreadable keeps its NAME.**
    ///
    /// Two rules on one file, and they fail in opposite directions. The bucket
    /// must not appear in the archived room, because unarchiving it would take
    /// him to a project that cannot exist; a project whose timestamp is
    /// garbage must still appear, because dropping it would hide something he
    /// can bring back.
    ///
    /// FORCED both ways: remove the `miscellaneous` filter and the first
    /// assertion goes red; make the date parse drop the row instead of the
    /// date, and the second does.
    #[test]
    fn the_bucket_is_never_archived_and_a_bad_date_keeps_the_row() {
        let home = tmp("archive-bucket");
        write(&home, "minds/live/.keep", "");
        write(
            &home,
            ".aui/archived-projects.json",
            r#"{"miscellaneous":"2026-09-01T00:00:00Z","nodate":"whenever"}"#,
        );
        let h = home.to_str().expect("utf8");
        let set = load_archived(h);
        // WITNESS, keyed on something neither rule can move: the file parsed
        // at all and the row that carries BOTH properties is present. Without
        // it an empty set would score a triumphant pass on the first assert.
        assert_eq!(set.len(), 1, "the archive set is not exactly the one row");
        assert_eq!(
            set[0].name, "nodate",
            "the readable row is not the survivor"
        );
        assert!(set[0].at.is_none(), "an unparseable date became a date");
        assert!(
            !set.iter().any(|a| a.name == "miscellaneous"),
            "the un-projected bucket reached the archived room"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// **THE REGISTRY IS THE SOURCE OF TRUTH AND THE `~/minds` SCAN IS GONE.**
    ///
    /// The decisive shape: a home where the two sources DISAGREE. `~/minds`
    /// holds `relic`, which is exactly what this function used to enumerate,
    /// and `~/.cauldron/projects.json` registers `real`, which is somewhere
    /// else entirely. If a scan survived anywhere in this crate — as the
    /// primary read, as a union, or as a "just in case" fallback — `relic`
    /// would be on this list.
    ///
    /// FORCED: point `registry_roots` back at `derive_from_minds` and the
    /// `relic` assertion goes red; drop the registry read and the `real`
    /// assertion goes red first.
    #[test]
    fn the_registry_is_the_list_and_the_minds_scan_is_gone() {
        let home = tmp("registry");
        write(&home, "minds/relic/.mind/threads.json", r#"{"threads":[]}"#);
        let real = home.join("elsewhere").join("real");
        std::fs::create_dir_all(&real).expect("mkdir");
        // Written through the CRATE'S OWN write path. A hand-rolled document
        // here would be a second definition of the format, which is the whole
        // failure this cutover exists to close.
        let mut reg = projects::Registry::empty();
        reg.register(&real, None).expect("registered");
        reg.save(&home).expect("saved");

        let (roots, state) = registry_roots(home.to_str().expect("utf8"));
        assert_eq!(state, RegistryState::Read, "the registry was not the source");
        let names: Vec<&str> = roots.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec!["real", "miscellaneous"],
            "the face is not reading the registry"
        );
        assert!(
            !names.contains(&"relic"),
            "the ~/minds scan is still in this crate — `relic` is not in the registry"
        );
        // The bucket resolves where it has always resolved, and the crate is
        // what says so rather than a constant in this file.
        assert_eq!(
            roots[1].1,
            projects::bucket_root(&home),
            "the bucket moved out from under 61 daemon threads"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// **AN ABSENT REGISTRY IS THE COEXISTENCE WINDOW, NOT AN ERROR** — and
    /// what it shows is the CRATE'S derivation, which is the same code the
    /// AUI's bootstrap mints the file with. One derivation, two uses.
    ///
    /// FORCED: make the `Ok` arm report `Read` for both sources and this goes
    /// red on the state; make an absent registry return an empty list and it
    /// goes red on `alpha`.
    #[test]
    fn an_absent_registry_shows_yesterdays_list_and_says_it_is_not_created_yet() {
        let home = tmp("registry-absent");
        write(&home, "minds/zebra/.keep", "");
        write(&home, "minds/alpha/.keep", "");
        let (roots, state) = registry_roots(home.to_str().expect("utf8"));
        assert_eq!(state, RegistryState::NotCreatedYet);
        let names: Vec<&str> = roots.iter().map(|(n, _)| n.as_str()).collect();
        // BYTE-FOR-BYTE what this function returned before the cutover:
        // sorted by name, bucket last.
        assert_eq!(names, vec!["alpha", "zebra", "miscellaneous"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// **A REGISTRY THAT WILL NOT PARSE IS THE BUCKET ALONE, AND NEVER
    /// `~/minds`.**
    ///
    /// The direction is the argument. Falling back to the relic here would put
    /// this face and the AUI on two different answers to "what are his
    /// projects" at exactly the moment nobody is watching, and it is the
    /// fallback `0098` forbids by name. The bucket is the one root that is not
    /// in the file and never was, so it survives honestly.
    ///
    /// WITNESS, keyed on something this rule cannot move: `~/minds` really
    /// does hold a project here, so an empty relic directory could not have
    /// produced this result by accident.
    ///
    /// FORCED: make the `Err` arm call `derive_from_minds` and the `would-be`
    /// assertion goes red with `relic` on the list.
    #[test]
    fn an_unreadable_registry_is_the_bucket_alone_and_carries_the_reason() {
        let home = tmp("registry-corrupt");
        write(&home, "minds/relic/.keep", "");
        write(&home, ".cauldron/projects.json", "{ this is not json");
        // The witness: the relic is genuinely there and genuinely scannable.
        assert!(home.join("minds").join("relic").is_dir());

        let (roots, state) = registry_roots(home.to_str().expect("utf8"));
        let names: Vec<&str> = roots.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec!["miscellaneous"],
            "an unreadable registry fell back to the relic"
        );
        match state {
            RegistryState::Unreadable(why) => assert!(
                !why.is_empty(),
                "the refusal reached the face with no sentence on it"
            ),
            other => panic!("a corrupt registry read as {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// **A REGISTERED PROJECT WHOSE DIRECTORY IS GONE KEEPS ITS ROW**
    /// (`0071` §3) — a checkout that moved is a fact to show him, not a reason
    /// to silently shorten his list. Its index reads ABSENT, which is a true
    /// zero and not a corruption.
    #[test]
    fn a_registered_path_that_is_gone_keeps_its_row() {
        let home = tmp("registry-gone");
        let gone = home.join("elsewhere").join("moved");
        std::fs::create_dir_all(&gone).expect("mkdir");
        let mut reg = projects::Registry::empty();
        reg.register(&gone, None).expect("registered");
        reg.save(&home).expect("saved");
        std::fs::remove_dir_all(&gone).expect("rm");

        let (ps, _, state) = load_projects(home.to_str().expect("utf8"));
        assert_eq!(state, RegistryState::Read);
        let p = ps.iter().find(|p| p.name == "moved").expect(
            "a registered project vanished from the list because its directory moved",
        );
        assert_eq!(p.index, IndexState::Absent);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The bucket is a root, it is LAST, and the roots are otherwise sorted.
    #[test]
    fn miscellaneous_is_the_second_kind_of_root_and_sits_last() {
        let home = tmp("roots");
        write(&home, "minds/zebra/.keep", "");
        write(&home, "minds/alpha/.keep", "");
        write(&home, "minds/.hidden/.keep", "");
        write(
            &home,
            ".aui/miscellaneous/.mind/threads.json",
            r#"{"threads":[]}"#,
        );
        let roots = discover_roots(home.to_str().expect("utf8"));
        let names: Vec<&str> = roots.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["alpha", "zebra", "miscellaneous"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    // ────────────────────────────────────────────────────────────────────
    //  THE ONE WRITE — `~/.cauldron/projects.json`
    // ────────────────────────────────────────────────────────────────────

    /// Build a registry file under a scratch `$HOME` with `names`, each
    /// pointing at a real directory under it. Never touches his real home:
    /// `home` is a parameter everywhere in `cauldron` and everywhere here.
    fn registry_of(home: &Path, names: &[&str]) {
        std::fs::create_dir_all(projects::cauldron_dir(home)).expect("mkdir");
        let mut reg = projects::Registry::empty();
        for n in names {
            let dir = home.join("code").join(n);
            std::fs::create_dir_all(&dir).expect("mkdir");
            reg.register(&dir, Some(n)).expect("registered");
        }
        reg.save(home).expect("saved");
    }

    fn names_in(home: &Path) -> Vec<String> {
        projects::Registry::load(home)
            .expect("loads")
            .expect("present")
            .names()
            .map(|s| s.to_string())
            .collect()
    }

    /// **THE WRITE LANDS IN THE FILE, AND THE FACE READS BACK THE NEW ORDER.**
    ///
    /// Two halves, and the second is the one that matters: a write nobody can
    /// see is not a reorder. `registry_roots` is the SAME reader the root
    /// surface uses, so this asserts the screen would change, not just the
    /// bytes.
    ///
    /// FORCED RED by passing `before: None` for every move (the order then
    /// never changes for an up-move and the first assertion fails), and by
    /// making `apply_registry` return `Ok(())` without calling
    /// `Registry::update` (both halves fail).
    #[test]
    fn a_reorder_rewrites_the_registry_and_the_face_reads_the_new_order() {
        let home = tmp("reorder");
        registry_of(&home, &["a", "b", "c"]);
        let h = home.to_str().expect("utf8");

        // WITNESS, keyed on something no write can move: the face draws the
        // registry's order plus the bucket, before anything is written.
        let drawn: Vec<String> = registry_roots(h).0.into_iter().map(|(n, _)| n).collect();
        assert_eq!(
            drawn,
            vec!["a", "b", "c", projects::BUCKET],
            "THE WITNESS FAILED: the face is not reading this scratch registry at all"
        );

        // `c` up — `before` is `b`, so `c` lands where `b` was.
        apply_registry(
            h,
            &crate::acts::RegistryWrite::MoveBefore {
                name: "c".into(),
                before: Some("b".into()),
            },
        )
        .expect("the move was refused");
        assert_eq!(names_in(&home), vec!["a", "c", "b"], "the file did not move");
        let drawn: Vec<String> = registry_roots(h).0.into_iter().map(|(n, _)| n).collect();
        assert_eq!(
            drawn,
            vec!["a", "c", "b", projects::BUCKET],
            "the file moved and the face kept drawing the old order"
        );

        // N >= 2, and the other shape: `None` means LAST.
        apply_registry(
            h,
            &crate::acts::RegistryWrite::MoveBefore {
                name: "a".into(),
                before: None,
            },
        )
        .expect("the move was refused");
        assert_eq!(names_in(&home), vec!["c", "b", "a"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// **REGISTERING A PATH ADDS A ROW, AND A REFUSAL IS THE CRATE'S OWN
    /// SENTENCE.**
    ///
    /// The refusal half is the point: `0060`'s rule is that the party who
    /// knows the rule writes the words, and this face relays them. A
    /// paraphrase here would put this face between him and the only code that
    /// knows why a registration failed.
    ///
    /// FORCED RED by replacing the `map_err(|e| e.sentence())` with a fixed
    /// string (the `contains` on the crate's own wording fails), and by
    /// dropping the `register` call (the row never appears).
    #[test]
    fn registering_a_path_adds_the_row_and_a_refusal_keeps_the_crates_words() {
        let home = tmp("register");
        registry_of(&home, &["a"]);
        let h = home.to_str().expect("utf8");
        let dir = home.join("code").join("newthing");
        std::fs::create_dir_all(&dir).expect("mkdir");

        apply_registry(
            h,
            &crate::acts::RegistryWrite::Register {
                path: dir.clone(),
                name: None,
            },
        )
        .expect("the registration was refused");
        assert_eq!(
            names_in(&home),
            vec!["a", "newthing"],
            "the basename was not used, or the row was not appended"
        );

        // A PATH THAT IS NOT THERE. The crate's sentence, verbatim — compared
        // against the crate's own constructor rather than against a literal,
        // so a reworded refusal in `cauldron` cannot make this test lie.
        let ghost = home.join("code").join("nope");
        let err = apply_registry(
            h,
            &crate::acts::RegistryWrite::Register {
                path: ghost.clone(),
                name: None,
            },
        )
        .expect_err("a path that is not there was registered");
        assert_eq!(
            err,
            projects::nothing_at_path(&ghost).sentence(),
            "the crate's refusal was re-worded on its way to the notice line"
        );
        // …and the refusal left the file alone.
        assert_eq!(names_in(&home), vec!["a", "newthing"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// **TWO WRITERS AT ONCE, AND NEITHER IS LOST** (`core:writing-tools`:
    /// anything that arbitrates is proven at N >= 2 or it is not proven).
    ///
    /// This face is now a writer of a file `ui-server` also writes, so the
    /// thing under test is the arbitration, not the write. `Registry::update`
    /// reloads INSIDE the advisory lock, so a read-modify-write cannot lose
    /// the other writer's row — which is exactly what a `load` + `save` pair
    /// in this crate WOULD have done.
    ///
    /// FORCED RED by replacing `apply_registry`'s `Registry::update` with
    /// `load` + mutate + `save`: with both threads holding the same snapshot,
    /// one row is silently dropped and the count comes back 3 instead of 4.
    #[test]
    fn two_registrations_at_once_both_land() {
        let home = tmp("race");
        registry_of(&home, &["a", "b"]);
        let h = home.to_str().expect("utf8");
        let mut dirs = vec![];
        for n in ["x", "y"] {
            let d = home.join("code").join(n);
            std::fs::create_dir_all(&d).expect("mkdir");
            dirs.push(d);
        }
        let hands: Vec<_> = dirs
            .into_iter()
            .map(|d| {
                let h = h.to_string();
                std::thread::spawn(move || {
                    apply_registry(
                        &h,
                        &crate::acts::RegistryWrite::Register {
                            path: d,
                            name: None,
                        },
                    )
                })
            })
            .collect();
        for t in hands {
            t.join().expect("thread").expect("registration refused");
        }
        let mut got = names_in(&home);
        got.sort();
        assert_eq!(
            got,
            vec!["a", "b", "x", "y"],
            "a concurrent registration was overwritten by the other writer"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// **AN ABSENT REGISTRY IS REFUSED, AND THIS IS THE SHARPEST HAZARD IN
    /// THE ROUND.**
    ///
    /// `projects::Registry::update` on a missing file starts from
    /// `Registry::empty()` and writes it. In the COEXISTENCE WINDOW
    /// (`RegistryState::NotCreatedYet`) this face is drawing a list DERIVED
    /// from the `~/minds` relic and none of it is in a file — so one `p` there
    /// would mint a registry holding the single project he just added,
    /// `ui-server`'s `bootstrap_if_absent` would find the file present and
    /// never derive again, and every other project would be gone from both
    /// faces at once.
    ///
    /// **WITNESS, keyed on something no guard can move**: the SAME write, on
    /// the SAME scratch home, succeeds the moment a registry exists. Without
    /// it, an `apply_registry` that refused everything — or one whose scratch
    /// home was unwritable — would score a triumphant pass.
    ///
    /// FORCED RED by deleting the `Ok(None)` arm: the file appears with one
    /// entry in it, which is the defect, and the `!exists` assertion fails.
    #[test]
    fn a_write_into_an_absent_registry_is_refused_and_mints_no_file() {
        let home = tmp("absent");
        let h = home.to_str().expect("utf8");
        let dir = home.join("code").join("thing");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let w = crate::acts::RegistryWrite::Register {
            path: dir.clone(),
            name: None,
        };

        // The coexistence window: `~/minds` has projects, the file does not
        // exist, and the face is drawing the DERIVED list.
        write(&home, "minds/one/.keep", "");
        write(&home, "minds/two/.keep", "");
        assert_eq!(
            registry_roots(h).1,
            RegistryState::NotCreatedYet,
            "this fixture is not in the coexistence window, so it proves nothing"
        );

        let err = apply_registry(h, &w).expect_err("an absent registry was written");
        assert!(err.contains("isn't there yet"), "{err}");
        assert!(
            !projects::registry_path(&home).exists(),
            "a refused write MINTED the registry — his whole list is now one project"
        );
        // …and the face still draws all of them.
        assert_eq!(registry_roots(h).0.len(), 3, "two projects plus the bucket");

        // THE WITNESS: with a registry present, that identical write lands.
        registry_of(&home, &["alpha"]);
        apply_registry(h, &w).expect("THE WITNESS FAILED: the write is refused unconditionally");
        assert_eq!(names_in(&home), vec!["alpha", "thing"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// **AN UNREADABLE REGISTRY IS NEVER OVERWRITTEN.** The other direction of
    /// the same rule: a file that will not parse is a file a human must
    /// repair, and a writer that ignored that would serialize whatever it had
    /// over it.
    ///
    /// FORCED RED by deleting the `Err(e)` arm — `Registry::update` still
    /// refuses (it returns `UpdateError::Load` before the closure runs), so
    /// what this catches is the SENTENCE changing, which is the half a reader
    /// sees. Both layers are deliberate.
    #[test]
    fn an_unreadable_registry_is_refused_and_left_byte_for_byte() {
        let home = tmp("unreadable");
        let h = home.to_str().expect("utf8");
        write(&home, ".cauldron/projects.json", "{ this is not json");
        let before = std::fs::read(projects::registry_path(&home)).expect("read");
        let dir = home.join("code").join("thing");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let err = apply_registry(
            h,
            &crate::acts::RegistryWrite::Register {
                path: dir,
                name: None,
            },
        )
        .expect_err("an unreadable registry was written");
        assert!(err.contains("won't overwrite it"), "{err}");
        assert_eq!(
            std::fs::read(projects::registry_path(&home)).expect("read"),
            before,
            "the corrupt registry was rewritten"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    // ────────────────────────────────────────────────────────────────────
    //  PATH COMPLETION
    // ────────────────────────────────────────────────────────────────────

    /// **ONE `tab` COMPLETES TO THE LONGEST UNAMBIGUOUS PREFIX**, keeps his
    /// own `~` spelling, offers directories only, and hides dotfiles until he
    /// asks for one.
    ///
    /// FORCED RED five ways, each watched: drop the `is_dir` filter (`notes`
    /// is offered and `~/code/` completes to `no`); drop the dot filter
    /// (`~/c` answers with `.cauldron` in the set); re-emit the EXPANDED path
    /// instead of his spelling (the `~/` assertions fail); return the first
    /// match instead of the common prefix (`~/code/h` completes to `harness/`
    /// with two candidates standing); and drop the trailing `/` on a unique
    /// match (the walk-into-it case stops working).
    #[test]
    fn tab_completes_a_path_to_its_longest_unambiguous_prefix() {
        let home = tmp("complete");
        for d in ["code/harness", "code/harvest", "code/plan", "code/planet", ".cauldron"] {
            std::fs::create_dir_all(home.join(d)).expect("mkdir");
        }
        std::fs::write(home.join("code/notes"), "not a directory").expect("write");
        let h = home.to_str().expect("utf8");

        // AMBIGUOUS, AND THE PREFIX GROWS: `p` -> `plan`, which is as far as
        // two candidates agree — NOT the first of them.
        let c = complete_path(h, "~/code/p");
        assert_eq!(c.text, "~/code/plan", "p -> the common prefix of plan and planet");
        assert_eq!(c.matches, vec!["plan", "planet"]);
        assert!(!c.unique);

        // AMBIGUOUS AND ALREADY AT THE PREFIX: nothing is added, and that must
        // not look like a completion that happened.
        let c = complete_path(h, "~/code/har");
        assert_eq!(c.text, "~/code/har");
        assert_eq!(c.matches, vec!["harness", "harvest"]);

        // UNIQUE: the whole name plus a `/`, so a second `tab` walks into it.
        let c = complete_path(h, "~/code/harn");
        assert_eq!(c.text, "~/code/harness/");
        assert!(c.unique);

        // THE WHOLE DIRECTORY, with the file excluded and his `~` kept.
        let c = complete_path(h, "~/code/");
        assert_eq!(
            c.matches,
            vec!["harness", "harvest", "plan", "planet"],
            "a FILE was offered as a project root"
        );

        // DOTFILES ONLY WHEN HE ASKS.
        assert!(!complete_path(h, "~/").matches.iter().any(|n| n.starts_with('.')));
        assert_eq!(complete_path(h, "~/.c").matches, vec![".cauldron"]);

        // **A BARE `~` LISTS HIS HOME AND DOES NOT SPLICE.** Found by writing
        // this line: without the directory-name case at the top of
        // `complete_path`, `~` expands to `/Users/x`, the FRAGMENT becomes `x`
        // and the answer is the siblings of his home directory, pasted onto a
        // `~` with no separator.
        let c = complete_path(h, "~");
        assert_eq!(c.matches, vec!["code"], "`~` did not list his home");
        assert_eq!(c.text, "~/code/", "`~` spliced a name onto the tilde");

        // AN ABSOLUTE PATH IS COMPLETED TOO, in the spelling he used.
        let c = complete_path(h, &format!("{h}/code/harn"));
        assert_eq!(c.text, format!("{h}/code/harness/"));

        // NOTHING THERE is an empty answer and NOT a silent edit of his text.
        let c = complete_path(h, "~/code/zzz");
        assert!(c.matches.is_empty());
        assert_eq!(c.text, "~/code/zzz", "a failed completion rewrote the field");

        // A BARE WORD IS A NAME and has nothing to complete against.
        assert!(complete_path(h, "harness").matches.is_empty());

        // …and the path the ACT registers is the one the completion showed.
        assert_eq!(
            resolve_typed_path(h, "~/code/plan/"),
            PathBuf::from(format!("{h}/code/plan/")),
            "the completion and the act expand `~` differently"
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
