//! # `PlStore` — protoledger's `Store`, where cauldron keeps it
//!
//! `<data dir>/protoledger/` (in prod `~/.cauldron/data/<env>/threads/store/
//! protoledger/`): `ledgers/` holds one ledger per thread, the truth, and
//! `index.db` is protoledger's derived index (D-1, D-3). cauldron keeps no
//! index of its own (rev 2 §0.5: `ledger_index` and the reverse-link map are
//! dropped; `Store::linked_from` answers).
//!
//! **`store_root_in`: a data root counts only when the data dir is inside it**
//! (kept from the first PL-1 draft, `5fae96e4` `paths.rs`). A process that
//! declares a data root (`CAULDRON_DATA_ROOT`) may open a store only under it;
//! a test or a mis-pointed daemon whose data dir is a temp dir while its
//! environment still names the prod root is REFUSED, so it can never write
//! beside prod.

use std::path::{Path, PathBuf};

use protoledger::store::Store;
use protoledger::{Agent, Appended, Event, Link};
use serde_json::{json, Map, Value};

use super::write::{self, AppFact, NetworkJoin, PeerMessage, Prompt, SettingsSnapshot, Spawn};
use super::CT;
use crate::error::{Error, Result};

/// The store's directory, under the data dir.
pub const STORE_DIR: &str = "protoledger";
/// The variable cauldron exports to every service: `<home>/data/<env>`.
pub const DATA_ROOT_VAR: &str = "CAULDRON_DATA_ROOT";
/// The link relation of a managed agent to the thread that spawned it.
pub const SPAWNED_BY: &str = "spawned-by";

fn pl<E: std::fmt::Display>(e: E) -> Error {
    Error::Store(format!("protoledger: {e:#}"))
}

/// **THE STORE ROOT** for `data_dir`, given the data root the process declares
/// (if any). Pure: the environment read is [`store_root`]'s.
pub fn store_root_in(data_dir: &Path, data_root: Option<&Path>) -> Result<PathBuf> {
    match data_root {
        Some(root) if !root.as_os_str().is_empty() && !data_dir.starts_with(root) => Err(Error::Config(format!(
            "protoledger: data dir {} is outside the declared data root {} ({DATA_ROOT_VAR}); refusing to open a store there",
            data_dir.display(),
            root.display()
        ))),
        _ => Ok(data_dir.join(STORE_DIR)),
    }
}

/// [`store_root_in`] over this process's `$CAULDRON_DATA_ROOT`.
pub fn store_root(data_dir: &Path) -> Result<PathBuf> {
    let root = std::env::var_os(DATA_ROOT_VAR).map(PathBuf::from);
    store_root_in(data_dir, root.as_deref())
}

/// cauldron's handle on its protoledger store, and the only writer of the
/// facts in [`super::write`].
pub struct PlStore {
    inner: Store,
}

impl PlStore {
    /// Open (creating if needed) the store for `data_dir`, honouring the
    /// declared data root.
    pub fn open_in(data_dir: &Path) -> Result<PlStore> {
        PlStore::open_at(&store_root(data_dir)?)
    }

    /// Open (creating if needed) the store at exactly `root`.
    pub fn open_at(root: &Path) -> Result<PlStore> {
        Ok(PlStore { inner: Store::open(root).map_err(pl)? })
    }

    pub fn root(&self) -> &Path {
        self.inner.root()
    }

    /// The underlying store, for reads this adapter does not wrap.
    pub fn store(&self) -> &Store {
        &self.inner
    }

    /// The underlying store, mutably (sync, projection and import need it).
    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.inner
    }

    pub fn has_thread(&self, thread_id: &str) -> Result<bool> {
        Ok(self.inner.agent(thread_id).map_err(pl)?.is_some())
    }

    /// A ROOT thread's ledger (header only). `meta` goes under CT.
    pub fn create_thread(&mut self, thread_id: &str, created_at: &str, meta: Map<String, Value>) -> Result<()> {
        let mut agent = Agent {
            id: thread_id.to_string(),
            created_at: Some(created_at.to_string()),
            source: None,
            parent: None,
            links: Vec::new(),
            meta: Map::new(),
        };
        if !meta.is_empty() {
            agent.meta.insert(CT.to_string(), Value::Object(meta));
        }
        self.inner.create_agent(agent).map_err(pl)
    }

    /// **SPAWN**: the child's root ledger with a `spawned-by` link to its
    /// parent (and the parent's spawning event), CT meta `{parent,
    /// spawn_request_id, budget_key}`, its first FULL settings snapshot, then
    /// ONE `network_joined` event per network, each its own append. Returns
    /// every append in order (settings first).
    pub fn spawn(&mut self, s: &Spawn) -> Result<Vec<Appended>> {
        let mut meta = Map::new();
        meta.insert("parent".into(), Value::String(s.parent.clone()));
        if let Some(r) = &s.spawn_request_id {
            meta.insert("spawn_request_id".into(), Value::String(r.clone()));
        }
        if let Some(b) = &s.budget_key {
            meta.insert("budget_key".into(), Value::String(b.clone()));
        }
        let agent = Agent {
            id: s.thread_id.clone(),
            created_at: Some(s.created_at.clone()),
            source: None,
            parent: None,
            links: vec![Link {
                rel: SPAWNED_BY.to_string(),
                agent: Some(s.parent.clone()),
                event: s.parent_event.clone(),
                extra: Map::new(),
            }],
            meta: Map::from_iter([(CT.to_string(), Value::Object(meta))]),
        };
        self.inner.create_agent(agent).map_err(pl)?;
        let mut out = vec![self.write_settings(&s.thread_id, &s.created_at, &s.settings)?];
        for n in &s.networks {
            out.push(self.write_network_join(
                &s.thread_id,
                &s.created_at,
                &NetworkJoin { network: n.clone(), at_spawn: true },
            )?);
        }
        Ok(out)
    }

    /// Append `make(id)` to `thread_id`, its id `ct:<word>:<seq>` — unique in
    /// the ledger because `seq` is.
    fn append_with(&mut self, thread_id: &str, make: impl FnOnce(&str) -> Event) -> Result<Appended> {
        let next = self.inner.held(thread_id).map_err(pl)?.last_seq() + 1;
        let probe = make("");
        let id = format!("ct:{}:{next}", write::id_word(&probe));
        let mut e = probe;
        e.id = id;
        self.inner.append(thread_id, e).map_err(pl)
    }

    /// The composed prompt, as `text`.
    pub fn write_prompt(&mut self, thread_id: &str, ts: &str, p: &Prompt) -> Result<Appended> {
        self.append_with(thread_id, |id| write::prompt_event(id, ts, p))
    }

    /// One FULL settings snapshot.
    pub fn write_settings(&mut self, thread_id: &str, ts: &str, s: &SettingsSnapshot) -> Result<Appended> {
        self.append_with(thread_id, |id| write::settings_event(id, ts, s))
    }

    /// A network delivery, in this thread's ledger.
    pub fn write_peer_message(&mut self, thread_id: &str, ts: &str, m: &PeerMessage) -> Result<Appended> {
        self.append_with(thread_id, |id| write::peer_message_event(id, ts, m))
    }

    /// A network join (its own event, at spawn or later).
    pub fn write_network_join(&mut self, thread_id: &str, ts: &str, j: &NetworkJoin) -> Result<Appended> {
        self.append_with(thread_id, |id| write::network_join_event(id, ts, j))
    }

    /// A summary (`app`) or a title (`title`).
    pub fn write_app(&mut self, thread_id: &str, ts: &str, f: &AppFact) -> Result<Appended> {
        self.append_with(thread_id, |id| write::app_fact_event(id, ts, f))
    }

    /// Every event of `thread_id`, read back through the store's index and
    /// ledger (`Store::events` + `Store::event_at`), in seq order.
    pub fn read_events(&self, thread_id: &str) -> Result<Vec<Event>> {
        let mut out = Vec::new();
        let mut from = 1;
        loop {
            let rows = self.inner.events(thread_id, from, 512).map_err(pl)?;
            if rows.is_empty() {
                break;
            }
            for r in &rows {
                let e = self
                    .inner
                    .event_at(thread_id, r.seq)
                    .map_err(pl)?
                    .ok_or_else(|| Error::Store(format!("protoledger: {thread_id} seq {} indexed but unreadable", r.seq)))?;
                out.push(e);
            }
            from = rows.last().map(|r| r.seq + 1).unwrap_or(from);
        }
        Ok(out)
    }

    /// The threads that name `thread_id` in a `spawned-by` link.
    pub fn spawned_by(&self, thread_id: &str) -> Result<Vec<String>> {
        Ok(self.inner.linked_from(thread_id, SPAWNED_BY).map_err(pl)?.into_iter().map(|a| a.id).collect())
    }

    /// `thread_id`'s header links, as the index holds them.
    pub fn links(&self, thread_id: &str) -> Result<Vec<Link>> {
        Ok(self.inner.agent(thread_id).map_err(pl)?.map(|a| a.links).unwrap_or_default())
    }

    /// `thread_id`'s CT header meta, read from the ledger header.
    pub fn header_meta(&mut self, thread_id: &str) -> Result<Value> {
        let l = self.inner.held(thread_id).map_err(pl)?;
        Ok(l.agent().meta.get(CT).cloned().unwrap_or(json!(null)))
    }
}
