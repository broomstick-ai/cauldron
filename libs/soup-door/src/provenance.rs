//! **Who wrote a record**, as review reads it: a ripple, or a merge that
//! carried a ripple's record from another cauldron.
//!
//! A record is RIPPLE-WRITTEN when the transaction that created it was a
//! ripple's (its initiator is of class `ripple`; a merge lands the records a
//! ripple wrote that nobody decided in such a write of its own, its run
//! `cauldron-merge:<MG-n>`, so that older readers see them as unreviewed
//! too), or when it was created by a MERGE ([`crate::merge`]) and its
//! `meta.merged_from.written_by` says a ripple wrote it where it came from. Both halves are needed for the
//! second form: the merge's own transaction (recognised by the mark only
//! `cauldron merge` writes, [`parse_mark`]) and the record's provenance
//! (which no record write may carry: [`crate::review::refuse_review_flags`]
//! refuses `meta.merged_from` beside the review flags). So a record written
//! by a session never reads as a ripple's, and a ripple's record carried by
//! a merge never reads as anyone else's: it stays in the review queue until
//! the owner decides it, in the cauldron it was carried into.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::Value;

use crate::{Code, Error, Result};

fn store(err: impl std::fmt::Display) -> Error {
    Error::new(Code::Store, err.to_string())
}

/// How every merge's `dispatch` text starts.
pub const MERGE_DISPATCH: &str = "cauldron merge ";

/// **The mark a merge writes** at the END of its soup transaction's
/// `dispatch` text: which merge, from which cauldron (by id), the source's
/// content digest, and how many records.
pub fn mark(merge: &str, source_id: &str, digest: &str, records: usize) -> String {
    format!("[merge {merge} from cauldron {source_id} content sha256:{digest}, {records} records]")
}

/// The run id the ripple part of a merge lands under (`cauldron-merge:<MG-n>`):
/// never a ripple run's (the ripples service mints those), and only a merge
/// writes it.
pub fn ripple_run(merge: &str) -> String {
    format!("{RIPPLE_RUN}{merge}")
}
const RIPPLE_RUN: &str = "cauldron-merge:";

/// **The mark of one PART of a merge** other than its main write: `ripple`
/// (the records a ripple wrote that nobody decided, landed in a transaction
/// of the ripple class, so that every reader since ripples shipped reads
/// them as unreviewed) or `links` (the edges from the main write's records
/// to the ripple part's). Written at the end of the part's `dispatch` (or a
/// ripple initiator's `step_id`).
pub fn part_mark(part: &str, merge: &str, source_id: &str, digest: &str, n: usize, what: &str) -> String {
    format!("[merge {merge} part {part} from cauldron {source_id} content sha256:{digest}, {n} {what}]")
}

/// `(part, merge, source_id, digest, n)` from a text that ends with
/// [`part_mark`]'s.
pub fn parse_part(text: &str) -> Option<(String, String, String, String, u64)> {
    let (_, tail) = text.rsplit_once("[merge ")?;
    let tail = tail.strip_suffix(']')?;
    let (merge, rest) = tail.split_once(" part ")?;
    let (part, rest) = rest.split_once(" from cauldron ")?;
    let (source, rest) = rest.split_once(" content sha256:")?;
    let (digest, rest) = rest.split_once(", ")?;
    let (n, what) = rest.split_once(' ')?;
    let ok = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !ok(merge) || !matches!(part, "ripple" | "links") || !ok(source) || !ok(digest) || !matches!(what, "records" | "edges") {
        return None;
    }
    Some((part.to_string(), merge.to_string(), source.to_string(), digest.to_string(), n.parse().ok()?))
}

/// One landed merge transaction, read off its row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeTx {
    /// Which write of the merge: `main`, `ripple` or `links`.
    pub part: String,
    pub merge: String,
    pub source_id: String,
    pub digest: String,
    pub records: u64,
    /// The spine's sequence number of the row.
    pub seq: i64,
    /// When it landed (the spine's clock, UTC).
    pub at: String,
    /// The committed-commit counter at its commit.
    pub order: i64,
}

/// `(merge, source_id, digest, records)` from a `dispatch` text that is a
/// merge's: it starts [`MERGE_DISPATCH`] and ENDS with [`mark`]'s text.
/// Anything else (a mark copied into the middle of an import's file name,
/// say) is not a merge.
pub fn parse_mark(dispatch: &str) -> Option<(String, String, String, u64)> {
    if !dispatch.starts_with(MERGE_DISPATCH) {
        return None;
    }
    let (_, tail) = dispatch.rsplit_once(" [merge ")?;
    let tail = tail.strip_suffix(" records]")?;
    let (merge, rest) = tail.split_once(" from cauldron ")?;
    let (source, rest) = rest.split_once(" content sha256:")?;
    let (digest, n) = rest.split_once(", ")?;
    let ok = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !ok(merge) || !ok(source) || !ok(digest) || n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((merge.to_string(), source.to_string(), digest.to_string(), n.parse().ok()?))
}

/// **Every landed merge transaction** of the soup in `soup` (read-only), by
/// transaction hash (hex). An `agent` row whose `dispatch` is a merge's.
pub fn merge_txs(soup: &Path) -> Result<HashMap<String, MergeTx>> {
    let conn = spine::offline::read_only_connection(&soup.join(spine::spine_db::SPINE_FILE)).map_err(store)?;
    let mut stmt = conn
        .prepare(
            "SELECT hash, seq, ts, COALESCE(committed_at, 0), json_extract(initiator, '$.class'), \
             COALESCE(json_extract(initiator, '$.dispatch'), ''), COALESCE(json_extract(initiator, '$.run_id'), ''), \
             COALESCE(json_extract(initiator, '$.step_id'), '') FROM transactions WHERE decided = 1 AND \
             ((json_extract(initiator, '$.class') = 'agent' AND json_extract(initiator, '$.dispatch') LIKE 'cauldron merge %') \
              OR (json_extract(initiator, '$.class') = 'ripple' AND json_extract(initiator, '$.run_id') LIKE 'cauldron-merge:%'))",
        )
        .map_err(store)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
            ))
        })
        .map_err(store)?;
    let mut out = HashMap::new();
    for row in rows {
        let (hash, seq, at, order, class, dispatch, run, step) = row.map_err(store)?;
        if class == "agent" {
            if let Some((merge, source_id, digest, records)) = parse_mark(&dispatch) {
                out.insert(hash, MergeTx { part: "main".into(), merge, source_id, digest, records, seq, at, order });
            } else if let Some((part, merge, source_id, digest, n)) = parse_part(&dispatch).filter(|p| p.0 == "links") {
                out.insert(hash, MergeTx { part, merge, source_id, digest, records: n, seq, at, order });
            }
        } else if let Some((part, merge, source_id, digest, n)) = parse_part(&step).filter(|p| p.0 == "ripple" && run == ripple_run(&p.1)) {
            out.insert(hash, MergeTx { part, merge, source_id, digest, records: n, seq, at, order });
        }
    }
    Ok(out)
}

/// The `written_by` a merged record's provenance carries, when it says a
/// ripple wrote it: `(run, at)`.
pub fn merged_ripple(meta: &Value) -> Option<(String, String)> {
    let w = meta.get("merged_from")?.get("written_by")?;
    if w.get("class")?.as_str()? != "ripple" {
        return None;
    }
    Some((w.get("run").and_then(Value::as_str).unwrap_or("").to_string(), w.get("at").and_then(Value::as_str).unwrap_or("").to_string()))
}

/// The transactions that decide whether a record is ripple-written, read
/// once per answer.
#[derive(Debug, Clone, Default)]
pub struct Writers {
    /// Every landed ripple transaction, by hash.
    pub ripples: HashSet<String>,
    /// Every landed merge transaction, by hash.
    pub merges: HashMap<String, MergeTx>,
}

impl Writers {
    /// Read both off the soup of `c`.
    pub fn of(c: &env::Cauldron) -> Result<Writers> {
        let ripples = crate::reads::ripple_commits(c)?;
        let merges = merge_txs(&c.soup())?;
        Ok(Writers { ripples, merges })
    }

    /// Was the record created by `created` (a transaction hash in hex) with
    /// `meta` written by a ripple, here or in the cauldron a merge carried
    /// it from?
    pub fn ripple_written(&self, created: Option<&str>, meta: &Value) -> bool {
        let Some(h) = created else { return false };
        self.ripples.contains(h) || (self.merges.contains_key(h) && merged_ripple(meta).is_some())
    }

    /// The merge that created the record, when one did and the record says
    /// where it came from.
    pub fn merge_of(&self, created: Option<&str>, meta: &Value) -> Option<&MergeTx> {
        let m = self.merges.get(created?)?;
        meta.get("merged_from").filter(|v| v.is_object()).map(|_| m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_merge_s_own_dispatch_carries_the_mark() {
        let m = mark("mg-1", "src-a", "abc", 3);
        assert_eq!(parse_mark(&format!("cauldron merge kb-a {m}")), Some(("mg-1".into(), "src-a".into(), "abc".into(), 3)));
        // An import's dispatch never starts as a merge's, whatever its file name says.
        assert_eq!(parse_mark(&format!("cauldron soup import x {m} [batch sha256:d, 3 records]")), None);
        assert_eq!(parse_mark(&format!("cauldron soup import {m}")), None);
        // The mark must end the text.
        assert_eq!(parse_mark(&format!("cauldron merge kb-a {m} trailing")), None);
        assert_eq!(parse_mark("cauldron merge kb-a [merge a b from cauldron c content sha256:d, 3 records]"), None);
    }

    #[test]
    fn a_part_mark_names_its_part_and_merge() {
        let m = part_mark("ripple", "MG-2", "src-a", "abc", 4, "records");
        assert_eq!(parse_part(&m), Some(("ripple".into(), "MG-2".into(), "src-a".into(), "abc".into(), 4)));
        assert_eq!(parse_part(&format!("cauldron merge a {}", part_mark("links", "MG-2", "s", "d", 1, "edges"))).map(|p| p.0), Some("links".into()));
        assert_eq!(parse_part("[merge MG-2 part other from cauldron s content sha256:d, 1 records]"), None);
        // The main write's mark is not a part's, and a part's is not the main one's.
        assert_eq!(parse_part(&mark("MG-2", "s", "d", 3)), None);
        assert_eq!(parse_mark(&format!("cauldron merge a {m}")), None);
        assert_eq!(ripple_run("MG-2"), "cauldron-merge:MG-2");
    }

    #[test]
    fn a_ripple_is_read_only_from_the_written_by_class() {
        let meta = serde_json::json!({"merged_from": {"written_by": {"class": "ripple", "run": "r1", "at": "t"}}});
        assert_eq!(merged_ripple(&meta), Some(("r1".into(), "t".into())));
        assert_eq!(merged_ripple(&serde_json::json!({"merged_from": {"written_by": {"class": "agent"}}})), None);
        assert_eq!(merged_ripple(&serde_json::json!({})), None);
    }
}
