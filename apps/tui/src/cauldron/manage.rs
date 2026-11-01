//! **The face's own management acts** — the things he makes in this face and
//! may change his mind about: a cluster made here (rename, move under another
//! cluster, delete), an account (rename, remove), an inference endpoint
//! (add, remove), a repo (register, unregister).
//!
//! Each act is a row — an account's, an endpoint's or a repo's on a Config page
//! ([`crate::cauldron::menu::Entry::Do`]), a cluster's at the foot of its
//! Details (a cluster has no Config, msg 235) — and → opens it. Naming opens a name field
//! (a modal), moving opens a picker, and anything that throws something away
//! asks first in a CONFIRM modal: Enter (or →) does it, Esc (or ←) keeps
//! things as they were. What the seed made is not his to delete from here —
//! a seed cluster's page says so instead of offering the rows.
//!
//! Every act writes through the file's own store, under that file's lock, and
//! every other face on the environment picks the change up on its next tick
//! (`App::sync`).

use std::path::Path;

use crate::cauldron::app::{App, Focus, Modal};
use crate::cauldron::blueprint::{Registered, Registry};
use crate::cauldron::menu::{PickFor, SPage};
use crate::cauldron::store::{self, CreatedClusters};

/// One management act, with what it acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    RenameCluster(String),
    MoveCluster(String),
    DeleteCluster(String),
    RenameAccount { pool: String, id: String },
    RemoveAccount { pool: String, id: String },
    /// V1.6: put account `key` back in the daemon's pool (`on`) or take it
    /// out — the MACHINE-WIDE switch, the `accounts` shape's `disabled`
    /// cell. Asked first, in the pool board's own words.
    InPool { key: String, on: bool },
    /// V1.6: log account `key` in through the threads daemon.
    LogIn { key: String },
    /// `+ add endpoint` (design 0199 §3): the add modal, an endpoint being
    /// added globally or (`here`) to this cauldron alone.
    NewEndpoint { here: bool },
    /// `add endpoint` at the foot of the add modal: the endpoint being added
    /// is added, once it has what its template needs.
    AddEndpoint { here: bool },
    RemoveEndpoint { here: bool, id: String },
    /// `open in Providers` on a global endpoint as this cauldron takes it:
    /// its own modal, on the Providers tab.
    OpenEndpoint(String),
    RegisterRepo,
    UnregisterRepo(String),
    /// The cauldron's switch (msg 186): `true` enables it again (resume),
    /// `false` disables it (pause). Asked first, always.
    Enable(bool),
    /// Storage › Garbage collection › `run now`: what a pass would reclaim,
    /// asked first (0214 A2).
    Gc,
    /// Status › Mesh (msg 403): `restart`, `stop` or `rollback` on a service
    /// of a device, asked first and recorded in the supervision log
    /// (`supervise`) — on the face's data, never on a process.
    Service { device: String, service: String, op: crate::cauldron::supervise::Op },
    /// V1.8: a human action's step submitted — each typed value to its
    /// field's destination — asked first, by field names only.
    HaSubmit { cluster: String, serial: u32, step: usize },
}

/// The cauldron's two states, in the words of its switch (msg 235: "You
/// could just have the setting be called cauldron enabled, and then have
/// two options enabled. Or disabled"): disabled, what is running finishes
/// and nothing new starts.
pub fn enabled_word(on: bool) -> &'static str {
    crate::cauldron::menu::CAULDRON_WORDS[usize::from(!on)]
}

impl App {
    /// Whether this cauldron is enabled (its `cauldron.enabled`).
    pub fn enabled(&self) -> bool {
        self.sget(&crate::cauldron::menu::Bound::new("cauldron.enabled", Default::default())).val.as_bool()
    }

    /// Ask before flipping the cauldron's switch (msg 186): the confirm
    /// modal, disabling an enabled cauldron or enabling a disabled one.
    pub fn ask_enable(&mut self) {
        let on = self.enabled();
        self.start_act(Act::Enable(!on));
    }

    /// The confirm said yes: set the switch.
    fn set_enabled(&mut self, on: bool) -> Result<String, String> {
        let bd = crate::cauldron::menu::Bound::new("cauldron.enabled", Default::default());
        self.sset(&bd, crate::cauldron::settings::Val::Bool(on))?;
        Ok(format!("{} is {}", self.env.name, enabled_word(on)))
    }

    /// The confirm said yes to a service act: record it, and read the soup
    /// again so the mesh shows it.
    fn supervise(&mut self, device: &str, service: &str, op: crate::cauldron::supervise::Op) -> Result<String, String> {
        if !self.persist {
            return Err("nothing is changed by --dump".into());
        }
        let plan = crate::cauldron::supervise::plan(&self.dash.mesh, &self.dash.threads, device, service, op)?;
        let dir = store::soup_dir(&self.env).map_err(|e| format!("{e:#}"))?;
        crate::cauldron::supervise::append(&dir, &crate::cauldron::supervise::record(device, service, op, plan)).map_err(|e| format!("{e:#}"))?;
        self.adopt_soup().map_err(|e| format!("{e:#}"))?;
        Ok(format!("{} {service}: recorded as requested", op.word()))
    }

    /// Whether cluster `key` was made in this face (and so may be renamed,
    /// moved or deleted here).
    pub fn made_here(&self, key: &str) -> bool {
        self.forest.find(key).is_some_and(|g| self.forest.get(g).made_here)
    }

    /// → on an act's row: ask for what it needs.
    pub fn start_act(&mut self, act: Act) {
        // An act on a greyed inference source (every endpoint, a pool other
        // than Claude Code's) is refused, said, and changes nothing (R-V1
        // item 7): no add modal opens, nothing is drafted or written.
        let live = crate::cauldron::settings::inference_live;
        let greyed = match &act {
            Act::NewEndpoint { .. } | Act::AddEndpoint { .. } | Act::RemoveEndpoint { .. } => !live("endpoints"),
            Act::RenameAccount { pool, .. } | Act::RemoveAccount { pool, .. } => !live(pool),
            _ => false,
        };
        if greyed {
            self.notice = Some(crate::cauldron::settings::GREYED.into());
            return;
        }
        match &act {
            Act::NewEndpoint { here } => return self.new_endpoint(*here),
            Act::AddEndpoint { here } => {
                if let Err(why) = self.add_endpoint(*here) {
                    self.notice = Some(format!("not added: {why}"));
                }
                return;
            }
            Act::OpenEndpoint(id) => return self.open_endpoint(id),
            Act::LogIn { key } => return self.start_login(key),
            Act::Gc => return self.ask_gc(),
            // Asked by `App::ha_confirm`, which holds the values.
            Act::HaSubmit { .. } => return,
            Act::Service { device, service, op } => {
                // What cannot be done is said, and nothing is asked.
                if let Err(why) = crate::cauldron::supervise::plan(&self.dash.mesh, &self.dash.threads, device, service, *op) {
                    self.notice = Some(why);
                    return;
                }
            }
            _ => {}
        }
        self.modal = Some(match &act {
            Act::RenameCluster(k) => {
                let name = self.forest.find(k).map_or_else(String::new, |g| self.forest.get(g).name.clone());
                Modal::Name { title: format!("Rename {name}"), name, act, error: None }
            }
            Act::MoveCluster(k) => {
                let Some(g) = self.forest.find(k) else { return };
                let below = self.forest.subtree(g);
                let mut targets: Vec<Option<String>> = vec![None];
                let mut options = vec!["the root".to_string()];
                for (i, s) in self.forest.clusters.iter().enumerate() {
                    // Under the whole cauldron is `the root`, offered first.
                    if !below.contains(&i) && !s.whole {
                        targets.push(Some(s.key.clone()));
                        options.push(self.forest.path(i));
                    }
                }
                let now = self.forest.parent_key(g);
                let selected = targets.iter().position(|t| *t == now).unwrap_or(0);
                Modal::Picker { title: format!("Move {} under", self.forest.get(g).name), for_: PickFor::MoveCluster { key: k.clone(), targets }, options, selected }
            }
            Act::DeleteCluster(k) => {
                let Some(g) = self.forest.find(k) else { return };
                let below = self.forest.subtree(g).len() - 1;
                let name = self.forest.get(g).name.clone();
                let mut body = vec![format!("{name} is deleted from this cauldron.")];
                if below > 0 {
                    body.push(format!("The {below} cluster{} under it go{} with it.", if below == 1 { "" } else { "s" }, if below == 1 { "es" } else { "" }));
                }
                Modal::Confirm { title: format!("Delete {name}?"), body, act, error: None }
            }
            Act::RenameAccount { pool, id } => {
                // Drawn through the one masker (V1-CLEAN2): the title AND the
                // name the field starts with — an address is never typed back
                // onto the screen.
                let name = self.account_name(pool, id);
                Modal::Name { title: format!("Rename {name}"), name, act, error: None }
            }
            Act::RemoveAccount { pool, id } => {
                let label = self.account_name(pool, id);
                let body = vec![format!("{label} leaves {pool}, and every setting it has goes with it.")];
                Modal::Confirm { title: format!("Remove {label}?"), body, act, error: None }
            }
            Act::RemoveEndpoint { here, id } => {
                let name = self.ep_name(*here, id);
                let body = if *here {
                    vec![format!("{name} leaves this cauldron, and every setting it has goes with it.")]
                } else {
                    vec![format!("{name} leaves every cauldron, and every setting it has goes with it."), "The roles here bound to it are unbound.".into()]
                };
                Modal::Confirm { title: format!("Remove {name}?"), body, act, error: None }
            }
            // The pool board's own question (`pool::action`), one way to say
            // it; the account by its masked name.
            Act::InPool { key, on } => {
                // Masked once, by the one masker, told apart from a twin;
                // the act itself keeps the raw key (it is what the daemon is
                // sent), and its question is asked with the drawn name.
                let shown = self.shown_key(key);
                let a = if *on { crate::pool::action::Action::Enable(key.clone()) } else { crate::pool::action::Action::Disable(key.clone()) };
                let body = vec![
                    a.question_as(&shown),
                    "This is the threads daemon's pool, for every cauldron on this machine. Whether one cauldron uses it is its own `use in this cauldron`.".into(),
                ];
                Modal::Confirm { title: format!("{} {shown}?", if *on { "Put back in the pool:" } else { "Take out of the pool:" }), body, act, error: None }
            }
            Act::NewEndpoint { .. } | Act::AddEndpoint { .. } | Act::OpenEndpoint(_) | Act::Gc | Act::HaSubmit { .. } | Act::LogIn { .. } => unreachable!("handled above"),
            Act::RegisterRepo => Modal::Name { title: "Register a repo — its path".into(), name: String::new(), act, error: None },
            Act::UnregisterRepo(n) => {
                let body = vec![format!("{n} leaves the registry. Its files are not touched.")];
                Modal::Confirm { title: format!("Unregister {n}?"), body, act, error: None }
            }
            Act::Service { device, service, op } => {
                use crate::cauldron::supervise::Op;
                let (from, to) = crate::cauldron::supervise::plan(&self.dash.mesh, &self.dash.threads, device, service, *op).unwrap_or_default();
                let what = match op {
                    Op::Restart if crate::cauldron::supervise::stopped(&self.dash.mesh, &self.dash.threads, device, service) => format!("{service} on {device} is started again."),
                    Op::Restart => format!("{service} on {device} is stopped and started again."),
                    Op::Stop => format!("{service} on {device} is stopped, and stays stopped until it is restarted."),
                    Op::Rollback => format!("{service} on {device} goes back from {from} to {to}, the newest version before it proven good."),
                };
                let body = vec![what, "It is recorded in the supervision log as requested by you.".into()];
                Modal::Confirm { title: format!("{} {service}?", capital(op.word())), body, act, error: None }
            }
            Act::Enable(on) => {
                let env = self.env.name.clone();
                if *on {
                    Modal::Confirm { title: format!("Enable the {env} cauldron again?"), body: vec![format!("Are you sure you want to resume {env}?")], act, error: None }
                } else {
                    Modal::Confirm { title: format!("Disable the {env} cauldron?"), body: vec![format!("Are you sure you want to pause {env}?")], act, error: None }
                }
            }
        });
    }

    /// The name modal was confirmed.
    pub(crate) fn finish_name(&mut self) {
        let Some(Modal::Name { act, name, .. }) = self.modal.clone() else { return };
        let r = match &act {
            Act::RenameCluster(k) => self.rename_cluster(k, &name),
            // The field started at the MASKED name: confirmed unchanged, the
            // account keeps its own name (never renamed to `x•••@y•••`).
            Act::RenameAccount { pool, id } if name.trim() == self.account_name(pool, id) => Ok("name unchanged".to_string()),
            Act::RenameAccount { pool, id } => self.settings.rename_account(pool, id, &name).map_err(|e| format!("{e:#}")).map(|()| format!("renamed to {}", crate::pool::model::shown_name(name.trim()))),
            Act::RegisterRepo => self.register_repo(&name),
            _ => Err("not a name".into()),
        };
        self.after(r);
    }

    /// The confirm modal was confirmed.
    pub(crate) fn finish_confirm(&mut self) {
        let Some(Modal::Confirm { act, .. }) = self.modal.clone() else { return };
        let r = match &act {
            Act::DeleteCluster(k) => self.delete_cluster(k),
            Act::RemoveAccount { pool, id } => self.remove_account(pool, id),
            Act::InPool { key, on } => self.send_in_pool(key, *on),
            Act::RemoveEndpoint { here, id } => self.remove_endpoint(*here, id),
            Act::UnregisterRepo(n) => self.unregister_repo(n),
            Act::Enable(on) => self.set_enabled(*on),
            Act::Gc => self.run_gc(),
            Act::Service { device, service, op } => self.supervise(device, service, *op),
            Act::HaSubmit { cluster, serial, step } => self.ha_submit(cluster, *serial, *step),
            _ => Err("nothing to confirm".into()),
        };
        self.after(r);
    }

    /// Close the modal on success (saying what happened), or put the refusal
    /// in it and keep it open.
    fn after(&mut self, r: Result<String, String>) {
        match r {
            Ok(said) => {
                self.modal = None;
                self.notice = Some(said);
            }
            Err(why) => match &mut self.modal {
                Some(Modal::Name { error, .. } | Modal::Confirm { error, .. }) => *error = Some(why),
                _ => self.error = Some(why),
            },
        }
    }

    fn created(&self) -> Result<CreatedClusters, String> {
        if !self.persist {
            return Err("nothing is changed by --dump".into());
        }
        store::soup_dir(&self.env).map(|d| CreatedClusters::new(&d)).map_err(|e| format!("{e:#}"))
    }

    fn rename_cluster(&mut self, key: &str, name: &str) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("a cluster needs a name".into());
        }
        self.created()?.rename(key, name).map_err(|e| format!("{e:#}"))?;
        self.reload_soup().map_err(|e| format!("{e:#}"))?;
        if let Some(g) = self.forest.find(key) {
            self.select(g);
        }
        Ok(format!("renamed to {name}"))
    }

    /// The picker chose where cluster `key` goes: under `target`, or (none)
    /// at the root.
    pub(crate) fn move_cluster(&mut self, key: &str, target: Option<String>) {
        let r = (|| -> Result<String, String> {
            self.created()?.set_parent(key, target.clone(), |k| self.forest.find(k).is_some()).map_err(|e| format!("{e:#}"))?;
            self.reload_soup().map_err(|e| format!("{e:#}"))?;
            if let Some(g) = self.forest.find(key) {
                self.select(g);
            }
            Ok(match target.as_deref().and_then(|t| self.forest.find(t)) {
                Some(p) => format!("moved under {}", self.forest.get(p).name),
                None => "moved to the root".into(),
            })
        })();
        match r {
            Ok(said) => self.notice = Some(said),
            Err(why) => self.error = Some(format!("NOT moved: {why}")),
        }
    }

    /// Delete cluster `key`, and every cluster made under it. A region it
    /// claimed that NO cluster claims once it is gone goes to the UNOWNED
    /// cluster (msg 235: "if we delete a cluster and now there's unowned
    /// regions, we can create an unowned cluster. So that's what we'll do"),
    /// which is made at the root the first time it is needed.
    fn delete_cluster(&mut self, key: &str) -> Result<String, String> {
        let name = self.forest.find(key).map_or_else(|| key.to_string(), |g| self.forest.get(g).name.clone());
        let parent = self.forest.find(key).and_then(|g| self.forest.parent_key(g));
        // What the clusters going claim, and what every other cluster does.
        let going: Vec<usize> = self.forest.find(key).map(|g| self.forest.subtree(g)).unwrap_or_default();
        let theirs: Vec<String> = going.iter().flat_map(|&g| self.forest.get(g).claims.clone()).collect();
        let kept: std::collections::HashSet<String> = (0..self.forest.clusters.len()).filter(|g| !going.contains(g)).flat_map(|g| self.forest.get(g).claims.clone()).collect();
        let mut orphaned: Vec<String> = Vec::new();
        for r in theirs {
            if !kept.contains(&r) && !orphaned.contains(&r) {
                orphaned.push(r);
            }
        }
        let created = self.created()?;
        created.remove(key).map_err(|e| format!("{e:#}"))?;
        let mut said = format!("deleted {name}");
        if !orphaned.is_empty() {
            let keys: Vec<String> = self.forest.clusters.iter().map(|g| g.key.clone()).collect();
            created
                .unowned(&orphaned, |list| store::mint_id(keys.iter().map(String::as_str).chain(list.iter().map(|c| c.id.as_str()))))
                .map_err(|e| format!("deleted, but its {} unclaimed region{} were NOT given to Unowned: {e:#}", orphaned.len(), if orphaned.len() == 1 { "" } else { "s" }))?;
            said.push_str(&format!(" · {} to Unowned", orphaned.join(", ")));
        }
        self.reload_soup().map_err(|e| format!("{e:#}"))?;
        let to = parent.as_deref().and_then(|p| self.forest.find(p)).unwrap_or(0);
        self.select(to);
        self.focus = Focus::Tree;
        Ok(said)
    }

    fn remove_account(&mut self, pool: &str, id: &str) -> Result<String, String> {
        let label = self.account_name(pool, id);
        let before = self.flags();
        self.settings.remove_account(pool, id).map_err(|e| format!("{e:#}"))?;
        self.note_flips(before, &format!("{label} removed"));
        // Its band is gone: the selection stays in range.
        self.snav.open.remove(&SPage::Account(pool.to_string(), id.to_string()));
        let n = self.selectable().len();
        let at = &mut self.rows[crate::cauldron::menu::SETTINGS_ROW];
        *at = (*at).min(n.saturating_sub(1));
        Ok(format!("removed {label} from {pool}"))
    }

    /// `+ add endpoint`: the add modal over an endpoint being added, its
    /// template OpenRouter until he picks another.
    fn new_endpoint(&mut self, here: bool) {
        self.settings.draft_endpoint(here, crate::cauldron::settings::TEMPLATE_IDS[0]);
        self.open_row(SPage::Endpoint { here, id: crate::cauldron::settings::DRAFT_EP.into() });
    }

    /// `add endpoint`: the endpoint being added is added — refused, with
    /// why, until it has what its template needs — and the modal closes
    /// onto its row, the table's last.
    fn add_endpoint(&mut self, here: bool) -> Result<(), String> {
        if let Some(why) = self.draft_missing(here) {
            return Err(why.into());
        }
        let name = self.ep_name(here, crate::cauldron::settings::DRAFT_EP);
        let id = self.settings.add_endpoint(here).map_err(|e| format!("{e:#}"))?;
        self.modal = None;
        self.modal_back = None;
        let row = SPage::Endpoint { here, id };
        if let Some(i) = self.selectable().iter().position(|e| *e == crate::cauldron::menu::Entry::Row(row.clone())) {
            self.rows[crate::cauldron::menu::SETTINGS_ROW] = i;
        }
        self.notice = Some(format!("added {name}"));
        Ok(())
    }

    /// `open in Providers`: global endpoint `id`'s own modal, its row
    /// selected on the Providers tab.
    fn open_endpoint(&mut self, id: &str) {
        self.modal = None;
        self.modal_back = None;
        self.set_cfg_tab(crate::cauldron::menu::CfgTab::Providers);
        self.snav.set_open(SPage::Endpoints, true);
        let row = SPage::Endpoint { here: false, id: id.into() };
        if let Some(i) = self.selectable().iter().position(|e| *e == crate::cauldron::menu::Entry::Row(row.clone())) {
            self.rows[crate::cauldron::menu::SETTINGS_ROW] = i;
        }
        self.open_row(row);
    }

    fn remove_endpoint(&mut self, here: bool, id: &str) -> Result<String, String> {
        let name = self.ep_name(here, id);
        self.settings.remove_endpoint(here, id).map_err(|e| format!("{e:#}"))?;
        let n = self.selectable().len();
        let at = &mut self.rows[crate::cauldron::menu::SETTINGS_ROW];
        *at = (*at).min(n.saturating_sub(1));
        Ok(format!("removed {name}"))
    }

    /// `+ register repo`: he types its PATH; its name is the path's last
    /// part, slugged (the inside of its `[repo:…]` tag).
    fn register_repo(&mut self, path: &str) -> Result<String, String> {
        if !self.persist {
            return Err("nothing is changed by --dump".into());
        }
        let path = path.trim().trim_end_matches('/').to_string();
        if path.is_empty() {
            return Err("type the repo's path".into());
        }
        let name = store::slug(path.rsplit('/').next().unwrap_or(&path));
        if name.is_empty() {
            return Err(format!("`{path}` ends in no name with a letter or a digit"));
        }
        if self.repos.find_repo(&name).is_some() {
            return Err(format!("a repo `{name}` is already registered"));
        }
        let at = chrono::Utc::now().to_rfc3339();
        let n2 = name.clone();
        Registry::global(&store::global_dir(&self.env))
            .update(move |f| {
                if f.registered.iter().any(|r| r.name == n2) {
                    anyhow::bail!("a repo `{n2}` is already registered");
                }
                f.unregistered.retain(|u| *u != n2);
                f.registered.push(Registered { name: n2, path, registered_at: at });
                Ok(())
            })
            .map_err(|e| format!("{e:#}"))?;
        self.reload_soup().map_err(|e| format!("{e:#}"))?;
        Ok(format!("registered {name}"))
    }

    fn unregister_repo(&mut self, name: &str) -> Result<String, String> {
        if !self.persist {
            return Err("nothing is changed by --dump".into());
        }
        let r = self.repos.find_repo(name).ok_or_else(|| format!("no repo `{name}`"))?;
        let n = self.repos.claimants[r].len();
        if n > 0 {
            return Err(format!("{n} cluster{} claim{} it: a claimed repo stays registered until no cluster claims it", if n == 1 { "" } else { "s" }, if n == 1 { "s" } else { "" }));
        }
        // The registry is every cauldron's: a repo another cauldron claims
        // stays registered too.
        let elsewhere: Vec<String> = store::repo_claimants(&self.env, &self.seeds, name, &self.forest).map_err(|e| format!("{e:#}"))?.into_iter().filter(|c| *c != self.env.name).collect();
        if !elsewhere.is_empty() {
            return Err(format!("the {} cauldron{} claim{} it: a claimed repo stays registered until no cluster claims it", elsewhere.join(", "), if elsewhere.len() == 1 { "" } else { "s" }, if elsewhere.len() == 1 { "s" } else { "" }));
        }
        let local = self.repos.repos[r].local;
        let n2 = name.to_string();
        Registry::global(&store::global_dir(&self.env))
            .update(move |f| {
                if local {
                    f.registered.retain(|x| x.name != n2);
                } else if !f.unregistered.contains(&n2) {
                    f.unregistered.push(n2);
                }
                Ok(())
            })
            .map_err(|e| format!("{e:#}"))?;
        self.reload_soup().map_err(|e| format!("{e:#}"))?;
        self.snav.open.remove(&SPage::Repo(name.to_string()));
        let n = self.selectable().len();
        let at = &mut self.rows[crate::cauldron::menu::SETTINGS_ROW];
        *at = (*at).min(n.saturating_sub(1));
        Ok(format!("unregistered {name}"))
    }
}

// ── garbage collection of the face's own files (0214 A2) ──────────────────

impl App {
    fn gc_set(&self, key: &'static str) -> crate::cauldron::settings::Val {
        self.sget(&crate::cauldron::menu::Bound::new(key, Default::default())).val
    }

    /// What a pass would reclaim now, from this face's own two dirs.
    pub fn gc_plan(&self) -> Result<crate::cauldron::gc::Plan, String> {
        let dir = store::soup_dir(&self.env).map_err(|e| format!("{e:#}"))?;
        let cap_mb = self.gc_set("storage.tui.dumps_cap_mb").as_int().unwrap_or(50).max(0) as u64;
        Ok(crate::cauldron::gc::Plan::compute(&dir, &store::global_dir(&self.env), cap_mb * 1024 * 1024))
    }

    /// `run now`: say what a pass would reclaim and ask; a dry run, an empty
    /// pass or a face that may not write only says.
    fn ask_gc(&mut self) {
        let plan = match self.gc_plan() {
            Ok(p) => p,
            Err(e) => return self.error = Some(format!("garbage collection: {e}")),
        };
        if plan.is_empty() {
            let also = plan.notes.iter().map(|n| format!("; {n}")).collect::<String>();
            self.notice = Some(format!("garbage collection: nothing to reclaim — nothing you wrote is ever collected{also}"));
            return;
        }
        let what = plan.lines().join("; ");
        if self.gc_set("storage.gc.dry_run").as_bool() {
            self.notice = Some(format!("dry run, nothing deleted: a pass would reclaim {} ({what})", plan.summary()));
            return;
        }
        if !self.persist {
            self.notice = Some(format!("read only here, nothing deleted: a pass would reclaim {} ({what})", plan.summary()));
            return;
        }
        let mut body = vec![format!("A pass reclaims {} from this face's own files:", plan.summary())];
        body.extend(plan.lines().into_iter().map(|l| format!("· {l}")));
        body.push(String::new());
        body.extend(plan.notes.iter().map(|n| format!("· {n}")));
        body.push("Nothing you wrote is ever touched: the bucket, the old buckets, pastes, dumps, tickets, settings, the clusters made here.".into());
        self.modal = Some(Modal::Confirm { title: "Collect the garbage?".into(), body, act: Act::Gc, error: None });
    }

    /// The confirm said yes: compute the pass again (it is what is true now)
    /// and perform it.
    fn run_gc(&mut self) -> Result<String, String> {
        let plan = self.gc_plan()?;
        match plan.apply() {
            Ok(0) => Ok("garbage collection: nothing to reclaim".into()),
            Ok(n) => Ok(format!("garbage collection reclaimed {} ({})", crate::cauldron::gc::size(n), plan.lines().join("; "))),
            Err((n, why)) => Err(format!("reclaimed {}, but not all of it: {why}", crate::cauldron::gc::size(n))),
        }
    }

    /// The collector on its own: every `storage.gc.interval`, and — with
    /// `on disk pressure` — whenever the disk's free space is under
    /// `free space floor` (looked at once a minute). It DELETES NOTHING
    /// (0217 B1): an automatic pass may take only a regenerable cache, the
    /// face keeps none, and everything else needs his confirm — so it only
    /// says, with `announce GC`, that a pass waits for `run now`. A face
    /// that may not write never looks.
    pub(crate) fn gc_tick(&mut self) {
        if !self.persist {
            return;
        }
        let now = std::time::Instant::now();
        let every = match self.gc_set("storage.gc.interval") {
            crate::cauldron::settings::Val::Str(s) => crate::cauldron::settings::duration_secs(&s),
            _ => None,
        };
        let due = every.is_some_and(|s| now.duration_since(self.gc_at).as_secs() >= s.max(60));
        let mut pressed = false;
        if !due && self.gc_set("storage.gc.on_disk_pressure").as_bool() && now.duration_since(self.gc_looked).as_secs() >= 60 {
            self.gc_looked = now;
            let floor_gb = self.gc_set("machine.disk.free_floor_gb").as_int().unwrap_or(20).max(0) as u64;
            pressed = store::soup_dir(&self.env).ok().and_then(|d| free_bytes(&d)).is_some_and(|free| free < floor_gb * 1024 * 1024 * 1024);
        }
        if !due && !pressed {
            return;
        }
        self.gc_at = now;
        let Ok(plan) = self.gc_plan() else { return };
        if plan.is_empty() {
            return;
        }
        let why = if pressed { "disk pressure" } else { "interval" };
        if self.gc_set("notify.gc.report").as_bool() {
            self.notice = Some(format!("garbage collection ({why}): {} could go — nothing is deleted until you confirm `run now` in Config › Devices › Storage", plan.summary()));
        }
    }
}

/// Free bytes on the disk holding `dir` (`df`, in KB blocks).
fn free_bytes(dir: &Path) -> Option<u64> {
    let out = std::process::Command::new("df").arg("-Pk").arg(dir).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let row = text.lines().nth(1)?;
    row.split_whitespace().nth(3)?.parse::<u64>().ok().map(|kb| kb * 1024)
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}
