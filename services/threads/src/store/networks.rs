//! # The networks tables — rows only, no rules
//!
//! `docs/agent-networks.md` §1–§4 is the design; [`crate::networks`] is where
//! every rule lives. This file is the persistence half and decides nothing: it
//! reads and writes the six tables `schema.sql` declares under "Messaging
//! networks", and nothing else.
//!
//! **Membership is a mutable projection; the ledger is not** (§3.6 of
//! `managed-subagents.md`). These rows change. Every act that changes them is
//! ALSO a ledger event on the acting thread (`crate::networks`), so the history
//! is reconstructible from `events` even though these tables are not
//! append-only.
//!
//! A child module of `store` so it can use the store's one write connection and
//! its read pool without widening either's visibility.

use rusqlite::{params, OptionalExtension};

use super::Store;
use crate::error::Result;

/// One `networks` row.
#[derive(Debug, Clone, PartialEq)]
pub struct NetworkRow {
    pub id: String,
    pub name: String,
    pub created_by: String,
    pub rules_json: String,
    pub created_at: String,
}

/// One `network_members` row. `state` is `member` or `pending` (an offer not
/// yet accepted, §1.3).
#[derive(Debug, Clone, PartialEq)]
pub struct MemberRow {
    pub network_id: String,
    pub thread_id: String,
    pub flags_json: String,
    pub state: String,
    pub joined_by: String,
    pub joined_at: String,
}

/// One held message: mail to a dormant member that is not `wakeable` to the
/// sender. Accepted, durable, and delivered when something else opens the
/// recipient's next turn (`agent-networks.md` §4.2).
#[derive(Debug, Clone, PartialEq)]
pub struct HeldMail {
    pub id: i64,
    pub recipient: String,
    pub network_id: String,
    pub sender: String,
    pub text: String,
    pub metadata_json: String,
}

impl Store {
    // ── networks ─────────────────────────────────────────────────────────

    pub fn upsert_network(&self, row: &NetworkRow) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO networks (id, name, created_by, rules_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET rules_json = excluded.rules_json",
            params![row.id, row.name, row.created_by, row.rules_json, row.created_at],
        )?;
        Ok(())
    }

    pub fn get_network(&self, id: &str) -> Result<Option<NetworkRow>> {
        let conn = self.read()?;
        Ok(conn
            .query_row(
                "SELECT id, name, created_by, rules_json, created_at FROM networks WHERE id = ?1",
                params![id],
                map_network,
            )
            .optional()?)
    }

    pub fn count_networks_created_by(&self, thread: &str) -> Result<usize> {
        let conn = self.read()?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM networks WHERE created_by = ?1 AND id NOT LIKE 'home:%'",
            params![thread],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Every network row (created or a home's override row) in which `thread`
    /// has a `network_members` row of the given state.
    pub fn networks_with_member(&self, thread: &str, state: &str) -> Result<Vec<NetworkRow>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT n.id, n.name, n.created_by, n.rules_json, n.created_at
               FROM networks n JOIN network_members m ON m.network_id = n.id
              WHERE m.thread_id = ?1 AND m.state = ?2
              ORDER BY n.created_at, n.id",
        )?;
        let rows = stmt.query_map(params![thread, state], map_network)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn upsert_member(&self, row: &MemberRow) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO network_members (network_id, thread_id, flags_json, state, joined_by, joined_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(network_id, thread_id) DO UPDATE SET
                 flags_json = excluded.flags_json,
                 state = excluded.state",
            params![row.network_id, row.thread_id, row.flags_json, row.state, row.joined_by, row.joined_at],
        )?;
        Ok(())
    }

    pub fn delete_member(&self, network: &str, thread: &str) -> Result<bool> {
        let conn = self.conn()?;
        let n = conn.execute(
            "DELETE FROM network_members WHERE network_id = ?1 AND thread_id = ?2",
            params![network, thread],
        )?;
        conn.execute(
            "DELETE FROM network_routes WHERE network_id = ?1 AND (holder = ?2 OR target = ?2)",
            params![network, thread],
        )?;
        Ok(n > 0)
    }

    pub fn network_members(&self, network: &str) -> Result<Vec<MemberRow>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT network_id, thread_id, flags_json, state, joined_by, joined_at
               FROM network_members WHERE network_id = ?1 ORDER BY joined_at, thread_id",
        )?;
        let rows = stmt.query_map(params![network], |r| {
            Ok(MemberRow {
                network_id: r.get(0)?,
                thread_id: r.get(1)?,
                flags_json: r.get(2)?,
                state: r.get(3)?,
                joined_by: r.get(4)?,
                joined_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // ── correspondence: delivery grants a route back (§1.5, §5) ─────────

    pub fn add_route(&self, network: &str, holder: &str, target: &str, at: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO network_routes (network_id, holder, target, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![network, holder, target, at],
        )?;
        Ok(())
    }

    pub fn has_route(&self, network: &str, holder: &str, target: &str) -> Result<bool> {
        let conn = self.read()?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM network_routes WHERE network_id = ?1 AND holder = ?2 AND target = ?3",
            params![network, holder, target],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// `(network_id, target)` for every route `holder` holds.
    pub fn routes_of(&self, holder: &str) -> Result<Vec<(String, String)>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT network_id, target FROM network_routes WHERE holder = ?1 ORDER BY created_at, target",
        )?;
        let rows = stmt.query_map(params![holder], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // ── notify policy: one row per (owner, peer, network) (R10.M3) ──────

    /// Write the link's policy — the latest wins (`MESSAGING-ORCHESTRATION.md`
    /// §2.2: "a link has exactly one current policy").
    pub fn set_notify_policy(&self, owner: &str, peer: &str, network: &str, notify: &str, set_by_json: &str, at: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO notify_policy (owner, peer, network_id, notify, set_by_json, set_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(owner, peer, network_id) DO UPDATE SET
                 notify = excluded.notify,
                 set_by_json = excluded.set_by_json,
                 set_at = excluded.set_at",
            params![owner, peer, network, notify, set_by_json, at],
        )?;
        Ok(())
    }

    /// The link's policy word, or `None` when no act ever set one (which the
    /// RULES read as `direct`; this layer decides nothing).
    pub fn notify_policy(&self, owner: &str, peer: &str, network: &str) -> Result<Option<String>> {
        let conn = self.read()?;
        Ok(conn
            .query_row(
                "SELECT notify FROM notify_policy WHERE owner = ?1 AND peer = ?2 AND network_id = ?3",
                params![owner, peer, network],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// `(peer, network_id, notify)` for every link `owner` has set.
    pub fn notify_policies_of(&self, owner: &str) -> Result<Vec<(String, String, String)>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT peer, network_id, notify FROM notify_policy WHERE owner = ?1 ORDER BY peer, network_id",
        )?;
        let rows = stmt.query_map(params![owner], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // ── broadcasts kept for `history: true` (§3.4) ──────────────────────

    pub fn record_broadcast(&self, network: &str, sender: &str, body: &str, at: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO network_broadcasts (network_id, sender, body, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![network, sender, body, at],
        )?;
        Ok(())
    }

    /// `(sender, body)` in send order.
    pub fn network_broadcasts(&self, network: &str) -> Result<Vec<(String, String)>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT sender, body FROM network_broadcasts WHERE network_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![network], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // ── held mail ────────────────────────────────────────────────────────

    pub fn hold_mail(&self, m: &HeldMail, at: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO network_held_mail (recipient, network_id, sender, text, metadata_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![m.recipient, m.network_id, m.sender, m.text, m.metadata_json, at],
        )?;
        Ok(())
    }

    /// Every held message for `recipient`, oldest first. **Reads only** — a
    /// row leaves this table through [`Self::delete_held_mail`], called by
    /// `networks::release_held` AFTER that row's enqueue is durable (R4.S3b,
    /// the R4.S3-land inspector's F3). The earlier `take_held_mail` committed
    /// the DELETE before any enqueue, so a kill or an enqueue error between the
    /// two left the mail in neither table.
    pub fn held_mail(&self, recipient: &str) -> Result<Vec<HeldMail>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT id, recipient, network_id, sender, text, metadata_json
               FROM network_held_mail WHERE recipient = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![recipient], |r| {
            Ok(HeldMail {
                id: r.get(0)?,
                recipient: r.get(1)?,
                network_id: r.get(2)?,
                sender: r.get(3)?,
                text: r.get(4)?,
                metadata_json: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Delete one held message by its row id. Returns whether a row went.
    pub fn delete_held_mail(&self, id: i64) -> Result<bool> {
        let conn = self.conn()?;
        Ok(conn.execute("DELETE FROM network_held_mail WHERE id = ?1", params![id])? > 0)
    }

    pub fn count_held_mail(&self, recipient: &str) -> Result<usize> {
        let conn = self.read()?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM network_held_mail WHERE recipient = ?1",
            params![recipient],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }
}

fn map_network(r: &rusqlite::Row<'_>) -> rusqlite::Result<NetworkRow> {
    Ok(NetworkRow {
        id: r.get(0)?,
        name: r.get(1)?,
        created_by: r.get(2)?,
        rules_json: r.get(3)?,
        created_at: r.get(4)?,
    })
}
