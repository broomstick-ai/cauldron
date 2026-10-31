//! # The cutover verb — its refusal, and nothing else of it yet
//!
//! The operator's ruling, `cc-session:<session>#H115` (OR-164): *skip the
//! staged shadow-render migration; cut over directly, keep raw per-account
//! copies for recovery.* The cutover of a live thread — building its ledger
//! from `events` + `turns/<id>/` + its native copies and flipping it to
//! `projection: projected` — is R4.S4b, a production act that stages the raw
//! copies first and needs the operator at the keyboard.
//!
//! What THIS step builds is the verb's precondition, so the act cannot be
//! written without it: [`cutover`] refuses unless a raw per-account copy of
//! the thread's native file exists at the path [`raw_copy_path`] names, and
//! the refusal names that path. With the copy present it refuses again,
//! naming the step that builds the rest. Both refusals are typed and reach the
//! caller; `the_cutover_refuses_without_a_raw_copy` forces the first one red.

use std::path::{Path, PathBuf};

/// Where the raw per-account copy of a thread's native file must sit before
/// the thread may be cut over: `<root>/raw-copies/<account>/<thread_id>.jsonl`.
/// `root` is the daemon's data directory.
pub fn raw_copy_path(root: &Path, account: &str, thread_id: &str) -> PathBuf {
    root.join("raw-copies").join(account).join(format!("{thread_id}.jsonl"))
}

/// Why the cutover did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CutoverRefused {
    /// No raw per-account copy at the path the verb requires. The copy is the
    /// recovery the operator asked for, and the verb does not run without it.
    NoRawCopy {
        thread_id: String,
        account: String,
        path: PathBuf,
    },
    /// The copy is there and the rest of the verb is not built.
    NotBuilt { step: &'static str, raw_copy: PathBuf },
    /// The store has no thread by that id, or it has no account to name the
    /// raw copy by (R4.S4b: [`cutover_thread`]).
    UnknownThread { thread_id: String },
    /// The store could not be read.
    StoreUnreadable { why: String },
}

impl std::fmt::Display for CutoverRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CutoverRefused::NoRawCopy { thread_id, account, path } => write!(
                f,
                "thread {thread_id} is not cut over: no raw copy of its native file for account \
                 {account} at {} — stage the copy first (H115: keep raw per-account copies for recovery)",
                path.display()
            ),
            CutoverRefused::NotBuilt { step, raw_copy } => write!(
                f,
                "the raw copy is staged at {}; the cutover itself is {step} and is not built",
                raw_copy.display()
            ),
            CutoverRefused::UnknownThread { thread_id } => {
                write!(f, "no thread {thread_id} with an account in this store: nothing to cut over")
            }
            CutoverRefused::StoreUnreadable { why } => write!(f, "the thread store could not be read: {why}"),
        }
    }
}

/// **CUT ONE THREAD OVER** — refused, by construction, until R4.S4b builds the
/// act. The precondition is real and is the part that must never be skipped:
/// a raw per-account copy at [`raw_copy_path`], non-empty.
pub fn cutover(root: &Path, account: &str, thread_id: &str) -> Result<std::convert::Infallible, CutoverRefused> {
    let path = raw_copy_path(root, account, thread_id);
    let staged = std::fs::metadata(&path).map(|m| m.is_file() && m.len() > 0).unwrap_or(false);
    if !staged {
        return Err(CutoverRefused::NoRawCopy {
            thread_id: thread_id.to_string(),
            account: account.to_string(),
            path,
        });
    }
    Err(CutoverRefused::NotBuilt {
        step: "R4.S4b",
        raw_copy: path,
    })
}

/// **THE VERB'S CALLER** (R4.S4b, HOLDS R17): cut over the thread the STORE
/// knows by `thread_id`, on the account its row names. The account is read
/// from the store rather than taken from the caller because the raw copy is
/// per ACCOUNT, and a copy staged under the wrong name is not the copy of this
/// thread. `threads cutover <thread_id>` (the CLI, `main.rs`) is the one
/// caller; the operator runs it at the keyboard, as H115 has it.
pub fn cutover_thread(
    store: &crate::store::Store,
    root: &Path,
    thread_id: &str,
) -> Result<std::convert::Infallible, CutoverRefused> {
    let row = store
        .get_thread(thread_id)
        .map_err(|e| CutoverRefused::StoreUnreadable { why: e.to_string() })?;
    let Some(account) = row.and_then(|r| r.account).filter(|a| !a.is_empty()) else {
        return Err(CutoverRefused::UnknownThread {
            thread_id: thread_id.to_string(),
        });
    };
    cutover(root, &account, thread_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **THE CALLER READS THE ACCOUNT OFF THE STORE.** A thread on `cc2` is
    /// refused naming `raw-copies/cc2/…`; with the copy staged under `cc2`
    /// the verb is reached (`NotBuilt`); a copy staged under ANOTHER account
    /// does not count; an unknown thread is refused as unknown.
    ///
    /// **THE MUTATION.** In `cutover_thread`, pass a constant account
    /// (e.g. `"default"`) instead of the row's.
    ///
    /// Forced red:
    /// ```text
    /// the caller names the raw copy by the thread's own account
    /// ```
    #[test]
    fn the_caller_cuts_over_on_the_account_the_store_names() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(tmp.path().join("threads.db")).unwrap();
        let now = crate::store::now_rfc3339();
        store
            .upsert_thread(&crate::store::ThreadRow {
                id: "t-9".into(),
                account: Some("cc2".into()),
                cwd: Some("/w".into()),
                model: Some("fable".into()),
                settings_json: "{}".into(),
                last_seq: 0,
                next_message_id: 1,
                last_compaction_message_id: 0,
                needs_restate: false,
                created_at: now.clone(),
                updated_at: now,
            })
            .unwrap();
        let want = raw_copy_path(tmp.path(), "cc2", "t-9");
        match cutover_thread(&store, tmp.path(), "t-9") {
            Err(CutoverRefused::NoRawCopy { account, path, .. }) => {
                assert_eq!(account, "cc2", "the caller names the raw copy by the thread's own account");
                assert_eq!(path, want);
            }
            other => panic!("the caller names the raw copy by the thread's own account: {other:?}"),
        }
        // A copy under another account is not this thread's copy.
        let elsewhere = raw_copy_path(tmp.path(), "default", "t-9");
        std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
        std::fs::write(&elsewhere, "{\"type\":\"user\"}\n").unwrap();
        assert!(
            matches!(cutover_thread(&store, tmp.path(), "t-9"), Err(CutoverRefused::NoRawCopy { .. })),
            "a copy staged under another account does not count"
        );
        std::fs::create_dir_all(want.parent().unwrap()).unwrap();
        std::fs::write(&want, "{\"type\":\"user\"}\n").unwrap();
        assert!(
            matches!(cutover_thread(&store, tmp.path(), "t-9"), Err(CutoverRefused::NotBuilt { .. })),
            "with its own copy staged the verb itself is reached"
        );
        assert_eq!(
            cutover_thread(&store, tmp.path(), "t-none").unwrap_err(),
            CutoverRefused::UnknownThread { thread_id: "t-none".into() }
        );
    }

    /// **NO RAW COPY, NO CUTOVER — and the refusal names the path.** With the
    /// copy staged the verb still refuses, naming the step that builds it; an
    /// EMPTY copy is not a copy.
    ///
    /// **THE MUTATION.** In `cutover`, set `let staged = true;`.
    ///
    /// Forced red:
    /// ```text
    /// the cutover refuses without a raw per-account copy, naming the path
    /// ```
    #[test]
    fn the_cutover_refuses_without_a_raw_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let want = tmp.path().join("raw-copies").join("cc1").join("t-1.jsonl");
        match cutover(tmp.path(), "cc1", "t-1") {
            Err(CutoverRefused::NoRawCopy { thread_id, account, path }) => {
                assert_eq!((thread_id.as_str(), account.as_str()), ("t-1", "cc1"));
                assert_eq!(path, want, "the refusal names the exact path the copy must sit at");
            }
            other => panic!("the cutover refuses without a raw per-account copy, naming the path: {other:?}"),
        }
        assert!(cutover(tmp.path(), "cc1", "t-1").unwrap_err().to_string().contains(&want.display().to_string()));
        std::fs::create_dir_all(want.parent().unwrap()).unwrap();
        std::fs::write(&want, "").unwrap();
        assert!(
            matches!(cutover(tmp.path(), "cc1", "t-1"), Err(CutoverRefused::NoRawCopy { .. })),
            "an empty file is not a raw copy"
        );
        std::fs::write(&want, "{\"type\":\"user\"}\n").unwrap();
        match cutover(tmp.path(), "cc1", "t-1") {
            Err(CutoverRefused::NotBuilt { step, raw_copy }) => {
                assert_eq!(step, "R4.S4b");
                assert_eq!(raw_copy, want);
            }
            other => panic!("with the copy staged, the verb names the unbuilt step: {other:?}"),
        }
    }
}
