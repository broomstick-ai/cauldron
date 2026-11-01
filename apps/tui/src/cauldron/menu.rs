//! **The settings menus**: the cauldron's Config tab and every cluster's
//! Config tab, as a tree of collapsible BANDS over [`settings::DEFS`]
//! (8e4fca10 msg 165: "do all of the settings as band headers instead of
//! showing a breadcrumb").
//!
//! A band's content is an [`SPage`]; [`App::page_entries`] lists what it
//! holds — section heads, read-only facts, nested bands, setting rows, list
//! items, `+ add` rows. [`App::entries`] flattens the tree as it is open:
//! every band header, and under each OPEN one its content, a level deeper.
//! The view draws exactly these entries and the keys act on exactly these
//! entries, so what he sees and what a key does cannot disagree.
//!
//! The grammar is the cluster tree's (`docs/navigation.md`): ↑↓ move, → (or
//! Enter) expands a band, ← collapses it or climbs to the band above; Space
//! flips a switch (a band's too: an account's), `x` or Backspace clears an
//! override, `+` / `-` nudge a number, `h` (or a click on its ⓘ) explains a
//! setting.
//!
//! Claude Code's accounts are REAL (V1.6): the threads daemon's `accounts`
//! shape, off the face's one connection (`cauldron::accounts`), logged in and
//! switched in or out of the pool through the daemon. The other pools are
//! still the seed's `cauldron.pools` (greyed, R-V1 item 7), and accounts
//! added to them here are kept beside the draft.

use crate::cauldron::app::{App, CTab, Focus, Modal, Row};
use crate::cauldron::manage::Act;
use crate::cauldron::tags::Tag;
use crate::cauldron::settings::{self, def, endpoint_prefix, show, Ctx, Prov, Resolved, Role, Ty, Val, DRAFT_EP, ROLES, ROLE_GROUPS};
use crate::cauldron::toggle::Toggle;
use crate::cauldron::store;

/// One setting, with its placeholders filled.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Bound {
    pub key: &'static str,
    pub ctx: Ctx,
}

impl Bound {
    pub fn new(key: &'static str, ctx: Ctx) -> Bound {
        let d = def(key);
        Bound { key: d.key, ctx }
    }
    pub fn def(&self) -> &'static settings::Def {
        def(self.key)
    }
    /// The concrete key, as the file holds it.
    pub fn concrete(&self) -> String {
        settings::subst(self.key, &self.ctx)
    }
}

fn b(key: &'static str) -> Bound {
    Bound::new(key, Ctx::default())
}

/// A band of a Config tab: what it holds is its page.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SPage {
    /// A Config tab's top level (8e4fca10 msg 235: "Let's actually just split
    /// up Global into, like, different chunks of things").
    Tab(CfgTab),
    /// DIRECTORIES (msg 186: File System "is config information ... call it
    /// just directories"): the cauldron's root, the weights path, and every
    /// root the cauldron's files live under.
    Paths,
    /// Which global accounts THIS cauldron uses, a table per source.
    Use,
    /// One global account as THIS cauldron takes it: its modal.
    UseAccount(String, String),
    Pool(String),
    /// INFERENCE ENDPOINTS on Providers (8e4fca10 msg 235: "we can call it
    /// like inference endpoints"; design 0199 §1): the global registry, a
    /// table of endpoints, `+ add endpoint`, the defaults an endpoint is born
    /// with.
    Endpoints,
    /// One endpoint, registered in this cauldron (`here`) or globally: its
    /// modal (§2) — the add modal (§3) while `id` is [`DRAFT_EP`].
    Endpoint { here: bool, id: String },
    /// INFERENCE ENDPOINTS on this cauldron's tab (§5): every endpoint it
    /// can see, whether each is used here, its metered cap.
    UseEndpoints,
    /// A GLOBAL endpoint as this cauldron takes it: its modal (§6).
    UseEndpoint(String),
    /// One global account: its modal.
    Account(String, String),
    Local,
    Roles,
    /// One role: its modal.
    Role(String),
    Machine,
    Storage,
    Gc,
    /// What an agent may touch and who the operator is.
    Identity,
    Attention,
    Mesh,
    Supervisor,
    Daemon,
    Devices,
    /// The repo registry (design 0071 S7): every repo, `+ register repo`,
    /// and the TRACE ROOTS.
    Repos,
    /// One registered repo's page: where it is, and `unregister`.
    Repo(String),
    /// A region of the soup: its boundary and its nodes.
    Region(String),
    /// A soup node: its body.
    Node(String),
    /// A list setting's own menu: `+ add`, then one row per item.
    List(Bound),
    /// One titled section of page `.0` (what was a `── TITLE ──` divider):
    /// a band like any other, open until he closes it (one band grammar,
    /// audit 0177 V3) — or, headed by [`Entry::Fixed`], never closed.
    Section(Box<SPage>, String),
}

impl SPage {
    /// Whether the band is open before he touches it: a section is, every
    /// other band is closed.
    pub fn open_by_default(&self) -> bool {
        matches!(self, SPage::Section(..))
    }

    /// What it is called.
    pub fn title(&self) -> String {
        match self {
            SPage::Tab(t) => t.label().into(),
            SPage::Paths => "Directories".into(),
            SPage::Use => "Accounts".into(),
            SPage::Pool(p) => pool_label(p),
            SPage::Endpoints | SPage::UseEndpoints => "Inference endpoints".into(),
            SPage::Endpoint { id, .. } | SPage::UseEndpoint(id) => id.clone(),
            SPage::Account(_, a) | SPage::UseAccount(_, a) => a.clone(),
            SPage::Local => "Local inference".into(),
            SPage::Roles => "Roles → models".into(),
            SPage::Role(r) => settings::role_name(r),
            SPage::Machine => "This machine".into(),
            SPage::Storage => "Storage & retention".into(),
            SPage::Gc => "Garbage collection".into(),
            SPage::Identity => "Identity".into(),
            SPage::Attention => "Attention & voice".into(),
            SPage::Mesh => "Mesh & services".into(),
            SPage::Supervisor => "Supervisor".into(),
            SPage::Daemon => "Threads daemon".into(),
            SPage::Devices => "Devices".into(),
            SPage::Repos => "Repos".into(),
            SPage::Repo(n) | SPage::Region(n) | SPage::Node(n) => n.clone(),
            SPage::List(b) => b.def().label.into(),
            SPage::Section(_, t) => t.clone(),
        }
    }

    /// The settings key prefix the page governs — what `i` quotes on its
    /// row: `pools.claude_code`, `pools.claude_code.accounts.ali`, `roles`.
    pub fn key_prefix(&self) -> String {
        let slug = |s: &str| s.replace(' ', "-");
        match self {
            SPage::Tab(_) => "settings".into(),
            SPage::Paths => "paths".into(),
            SPage::Use => "cauldron.accounts".into(),
            SPage::UseAccount(p, a) => format!("cauldron.accounts.{p}.{a}"),
            SPage::Pool(p) => format!("pools.{p}"),
            SPage::Endpoints => "endpoints".into(),
            SPage::Endpoint { here, id } => format!("{}.{id}", endpoint_prefix(*here)),
            SPage::UseEndpoints => "cauldron.use".into(),
            SPage::UseEndpoint(id) => format!("cauldron.use.{id}"),
            SPage::Account(p, a) => format!("pools.{p}.accounts.{a}"),
            SPage::Local => "local".into(),
            SPage::Roles => "roles".into(),
            SPage::Role(r) => format!("roles.{r}"),
            SPage::Machine => "machine".into(),
            SPage::Storage => "storage".into(),
            SPage::Gc => "storage.gc".into(),
            SPage::Identity => "identity".into(),
            SPage::Attention => "attention".into(),
            SPage::Mesh => "mesh".into(),
            SPage::Supervisor => "supervisor".into(),
            SPage::Daemon => "threads".into(),
            SPage::Devices => "mesh.devices".into(),
            SPage::Repos | SPage::Repo(_) => "repos".into(),
            SPage::Region(_) | SPage::Node(_) => "settings".into(),
            SPage::List(b) => slug(&b.concrete()),
            SPage::Section(p, _) => p.key_prefix(),
        }
    }

    /// The word a `--settings` path names it by.
    pub fn slug(&self) -> String {
        match self {
            SPage::Pool(p) => p.clone(),
            SPage::Account(_, a) | SPage::UseAccount(_, a) => a.clone(),
            SPage::Role(r) => r.clone(),
            SPage::List(b) => b.key.rsplit('.').next().unwrap_or(b.key).into(),
            SPage::Tab(t) => t.slug().into(),
            SPage::Paths => "directories".into(),
            SPage::Use => "accounts".into(),
            SPage::Endpoints | SPage::UseEndpoints => "endpoints".into(),
            SPage::Endpoint { id, .. } | SPage::UseEndpoint(id) => id.clone(),
            SPage::Local => "local".into(),
            SPage::Roles => "roles".into(),
            SPage::Machine => "machine".into(),
            SPage::Storage => "storage".into(),
            SPage::Gc => "gc".into(),
            SPage::Identity => "identity".into(),
            SPage::Attention => "attention".into(),
            SPage::Mesh => "mesh".into(),
            SPage::Supervisor => "supervisor".into(),
            SPage::Daemon => "threads".into(),
            SPage::Devices => "devices".into(),
            SPage::Repos => "repos".into(),
            SPage::Repo(n) | SPage::Region(n) | SPage::Node(n) => n.clone(),
            SPage::Section(_, t) => t.to_lowercase().replace(' ', "-"),
        }
    }

    /// Whether the page is a configurable ROW's: one row of a table, opened
    /// in a modal (msg 235: "account specific settings would be rows that
    /// you expand into a modal with the settings ... that's what we should
    /// do for every configurable row in anything that I call a table").
    pub fn is_row(&self) -> bool {
        matches!(self, SPage::Account(..) | SPage::UseAccount(..) | SPage::Role(_) | SPage::Endpoint { .. } | SPage::UseEndpoint(_))
    }
}

/// The page a line sits on: its band, or the page of the section it is in.
pub fn unsection(mut p: Option<SPage>) -> Option<SPage> {
    while let Some(SPage::Section(page, _)) = p {
        p = Some(*page);
    }
    p.filter(|x| !matches!(x, SPage::Tab(_)))
}

/// One line of a settings page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A section head: its title and its metadata. Shown as a band
    /// ([`SPage::Section`]) holding every entry up to the next head.
    Head(String, String),
    /// An ALWAYS-EXPANDED band's head (msg 235: "instead of it being a
    /// chevron, it could just be a dot ... this is a band, but like you don't
    /// have to collapse it or expand it, it's just always open"): its title
    /// and its metadata; it holds every entry up to the next head, and
    /// nothing selects it.
    Fixed(String, String),
    /// A read-only fact: label, value.
    Fact(String, String),
    /// A blank line.
    Gap,
    /// A band: what it holds is `page`.
    Sub { page: SPage, label: String, summary: String },
    /// One ROW of a table: a configurable thing whose settings open in a
    /// modal ([`SPage::is_row`]). Consecutive rows are one table.
    Row(SPage),
    /// A setting.
    Set(Bound),
    /// Item `index` of a list.
    Item { list: Bound, index: usize, text: String },
    /// `+ add …`: a button.
    Add(Add),
    /// An action: a button.
    Run(Bound),
    /// A management act (rename, delete, remove, register): a button.
    Do { act: crate::cauldron::manage::Act, label: String, summary: String },
    /// A paragraph, wrapped (a region's boundary, a node's body).
    Para(String),
    /// A thing on the page that `i` quotes and → does not open (a root
    /// directory, a soup node's own line).
    Ref { tag: crate::cauldron::tags::Tag, label: String, summary: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Add {
    Account(String),
    Item(Bound),
}

impl Entry {
    pub fn selectable(&self) -> bool {
        !matches!(self, Entry::Head(..) | Entry::Fixed(..) | Entry::Fact(..) | Entry::Gap | Entry::Para(_))
    }

    /// Whether the entry is a BUTTON (msg 235: "make it look like a button
    /// and not like a whole row that you can click on"): selected, the
    /// button lights, not the row; one click presses it.
    pub fn is_button(&self) -> bool {
        matches!(self, Entry::Add(_) | Entry::Run(_) | Entry::Do { .. })
    }

    /// The band this entry heads, if it heads one: a submenu, or a list
    /// setting (its items and `+ add` are its content).
    pub fn band(&self) -> Option<SPage> {
        match self {
            Entry::Sub { page, .. } => Some(page.clone()),
            Entry::Set(bd) if bd.def().ty == Ty::List => Some(SPage::List(bd.clone())),
            _ => None,
        }
    }
}

/// One cell of a table row: a switch (that works on the row, msg 235:
/// "you can't actually change it. It's just an illusion"), or a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cell {
    Toggle(Bound),
    Text(String),
}

/// One line of a Config tab as it is open: the entry, how deep it sits
/// (a band's content is one deeper than its header), and — on a band header
/// — whether the band is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flat {
    pub depth: usize,
    pub entry: Entry,
    pub open: Option<bool>,
    /// The band this line sits in (none at the top).
    pub parent: Option<SPage>,
}

/// Where one Config tab is: its root, and which of its bands are open.
/// Nothing else: there is no page stack and no breadcrumb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nav {
    pub root: SPage,
    pub open: std::collections::HashSet<SPage>,
}

impl Nav {
    pub fn cauldron() -> Nav {
        Nav { root: SPage::Tab(CfgTab::Cauldron), open: Default::default() }
    }
    /// `open` holds the bands turned from their default.
    pub fn is_open(&self, page: &SPage) -> bool {
        page.open_by_default() != self.open.contains(page)
    }
    pub fn set_open(&mut self, page: SPage, open: bool) {
        if self.is_open(&page) != open && !self.open.remove(&page) {
            self.open.insert(page);
        }
    }
}

/// The Config tabs (8e4fca10 msg 235: "one for the TUI, one for the
/// machine, or we'll call it devices ... providers. And then finally at the
/// end, we'll have one for this cauldron ... it should just be like this
/// cauldron"). THIS cauldron's tab, named for it (`prod cauldron`,
/// [`App::cfg_label`]), is the FIRST (b964d571 msg 23: "move that dynamic
/// one to be the first tab"); every other is GLOBAL: what it sets, every
/// cauldron on the machine shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CfgTab {
    Cauldron,
    Tui,
    Devices,
    Providers,
    /// V1.1: the accounts Providers held (each source's table and its
    /// modal), moved to a tab of their own on the global bar.
    Accounts,
    Permissions,
}

impl CfgTab {
    /// Every settings page. V1.1: each is a root tab
    /// ([`crate::cauldron::app::CTab::page`]); none is a sub-tab any more.
    pub const ALL: [CfgTab; 6] = [CfgTab::Cauldron, CfgTab::Tui, CfgTab::Devices, CfgTab::Providers, CfgTab::Accounts, CfgTab::Permissions];
    /// Its name; the first one's is the cauldron's ([`App::cfg_label`]).
    pub fn label(self) -> &'static str {
        match self {
            CfgTab::Tui => "TUI",
            CfgTab::Devices => "Devices",
            CfgTab::Providers => "Providers",
            CfgTab::Accounts => "Accounts",
            CfgTab::Permissions => "Permissions",
            CfgTab::Cauldron => "cauldron",
        }
    }
    /// The word `--config-tab` and a `--settings` path name it by.
    pub fn slug(self) -> &'static str {
        match self {
            CfgTab::Tui => "tui",
            CfgTab::Devices => "devices",
            CfgTab::Providers => "providers",
            CfgTab::Accounts => "accounts",
            CfgTab::Permissions => "permissions",
            CfgTab::Cauldron => "cauldron",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.slug().eq_ignore_ascii_case(s))
    }
    /// The page the tab's bands hang from.
    pub fn root(self) -> SPage {
        SPage::Tab(self)
    }
}

/// A setting's value being typed into, on its row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub bound: Bound,
    pub text: String,
    pub error: Option<String>,
}

/// What a picker's choice goes to: a setting, or where a cluster made here
/// moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickFor {
    Setting { bound: Bound },
    /// Where a cluster made here moves: each option's cluster key (none: the
    /// root), in the picker's order.
    MoveCluster { key: String, targets: Vec<Option<String>> },
    /// A model put back on a cauldron's narrowed list: only one of the
    /// key's own (design 0199 §6: the list "may only remove").
    AddItem { list: Bound },
}

/// The row the cauldron Config tab's selection is kept in (`App::rows`).
pub const SETTINGS_ROW: usize = 1;

/// One account of a pool, as the menus list it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcctView {
    pub id: String,
    pub label: String,
    /// Its card on the seed's account board, if it has one.
    pub seed: Option<usize>,
    /// V1.6: its key in the threads daemon's `accounts` shape — what a verb
    /// names it by — when it is one of the daemon's. `label` is then the key
    /// masked (or the label he renamed it to), never an address in full.
    pub key: Option<String>,
}

/// The pool the threads daemon manages today.
pub const CLAUDE_CODE: &str = "claude_code";

/// The token sources every cauldron has a band for (msg 165: "We should
/// have a Codex one"), in the order Tokens lists them: the subscriptions.
/// What spends an API key is an INFERENCE ENDPOINT, not a source (msg 235:
/// "instead of calling this like open router at all ... inference
/// endpoints"; design 0199), and the sources once planned beside them —
/// the Anthropic API, Gemini — are endpoint templates.
pub const TOKEN_SOURCES: [&str; 2] = [CLAUDE_CODE, "codex"];

/// The sources whose accounts a cauldron switches on or off for itself
/// (msg 186: "toggle on and off whether you can use specific accounts in
/// this cauldron").
pub const USE_SOURCES: [&str; 2] = TOKEN_SOURCES;

/// A token source by its name: `claude_code` → `Claude Code`.
/// `n` and its noun, singular or plural: `1 root`, `3 roots`.
pub fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

pub fn pool_label(id: &str) -> String {
    match id {
        CLAUDE_CODE => "Claude Code".into(),
        "codex" => "Codex".into(),
        other => other.to_string(),
    }
}

/// An account's id: its label, slugged — and, for a key that is an ADDRESS,
/// the slug of its MASKED form ([`crate::pool::model::shown_name`]) plus a
/// short stable digest of the key, so two addresses with the same initials
/// still get two ids (V1-CLEAN2, V1-CLEAN1-I B1). The digest is KEYED by this
/// install's secret ([`crate::pool::model::install_key`], V1-ACCT2): an id
/// cannot be checked against a guessed address. The id is drawn — page
/// titles, `i`'s settings key, `--settings` paths, a picker's options — so it
/// must never carry the address: the slug of `qmzrtb@vxnhwk.example` was
/// `qmzrtb-vxnhwk-example`, the whole address one Enter from a masked card.
pub fn account_id(label: &str) -> String {
    let shown = crate::pool::model::shown_name(label);
    if shown == label {
        return store::slug(label);
    }
    format!("{}-{}", store::slug(&shown), crate::pool::model::digest_hex(label))
}

/// **THE IDS ACCOUNT `label` WAS STORED UNDER BEFORE [`account_id`]** (V1-ACCT,
/// V1-CLEAN2-I B1, V1-ACCT2). The id is a persisted settings key —
/// `pools.{p}.accounts.{id}.*`, `cauldron.accounts.{p}.{id}.*`,
/// `accounts.json`'s `{p}/{id}` — and an address-keyed account has had three
/// before this one. What is found under one of them is moved to
/// [`account_id`] once ([`crate::cauldron::settings::Settings::adopt_account_ids`]).
///
/// **IN PRECEDENCE ORDER** (V1-ACCT-I N1): when one account has a value under
/// more than one, the FIRST here wins and the others are dropped with a
/// notice — and a value already under [`account_id`] beats all of them (he
/// set it since):
///
/// 1. the UNSALTED digest id (`c-e-13f65b`, V1-CLEAN2 to V1-ACCT): the newest
///    form a face wrote, so its value is the latest write;
/// 2. the raw key (a hand-written file): it names exactly one account;
/// 3. the plain slug of the key (`carol-example-com`, before V1-CLEAN2): the
///    oldest form, and one that two addresses can share
///    (`c.l@` / `c-l@`), so it is last.
///
/// Empty when the id never changed (a key that is not an address).
pub fn legacy_account_ids(label: &str) -> Vec<String> {
    let now = account_id(label);
    let shown = crate::pool::model::shown_name(label);
    let unsalted = if shown == label { None } else { Some(format!("{}-{}", store::slug(&shown), crate::pool::model::unsalted_digest_hex(label))) };
    let mut out = Vec::new();
    for old in unsalted.into_iter().chain([label.to_string(), store::slug(label)]) {
        if !old.is_empty() && old != now && !out.contains(&old) {
            out.push(old);
        }
    }
    out
}

/// What a subscription source sets, flat on its band (msg 235: "just
/// un-nest them from the defaults band"): what each of its accounts
/// inherits, then how it places work.
pub const POOL_SETTINGS: [&str; 15] = [
    "pools.{pool}.budget.session.threshold_pct",
    "pools.{pool}.budget.weekly.threshold_pct",
    "pools.{pool}.budget.fable.threshold_pct",
    "pools.{pool}.budget.session.cap_pct",
    "pools.{pool}.budget.weekly.cap_pct",
    "pools.{pool}.budget.fable.cap_pct",
    "pools.{pool}.budget.bar_pct",
    "pools.{pool}.budget.threshold_action",
    "pools.{pool}.budget.reserve_pct",
    "pools.{pool}.max_concurrent",
    "pools.{pool}.accounts_enabled",
    "pools.{pool}.selection",
    "pools.{pool}.failover",
    "pools.{pool}.opportunistic.enabled",
    "pools.{pool}.budget.cash.threshold_usd",
];

/// The cauldron's own switch says what it does (msg 235: "You could just
/// have the setting be called cauldron enabled, and then have two options
/// enabled. Or disabled").
pub const CAULDRON_WORDS: [&str; 2] = ["enabled", "disabled"];

/// A cauldron's model list of a global endpoint: the key's whole list, or
/// narrowed (design 0199 §6: "INHERIT  narrow").
pub const NARROW_WORDS: [&str; 2] = ["inherit", "narrow"];

/// The widest an endpoint's url or models cell grows in a table.
pub const EP_URL_W: usize = 26;

/// `s` cut to `w` characters, its last one `…` when it is cut.
pub fn cut(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        return s.to_string();
    }
    format!("{}…", s.chars().take(w.saturating_sub(1)).collect::<String>())
}

/// Setting `field` of endpoint `id`, registered `here` or globally.
pub fn ep_bound(here: bool, id: &str, field: &str) -> Bound {
    Bound::new(settings::def(&format!("{}.{{ep}}.{field}", endpoint_prefix(here))).key, Ctx::ep(id))
}

/// Whether setting `bd` is a TRI-STATE switch (msg 235: "if there's an
/// inherent option on any setting in any config menu ... we're going to
/// have inherit, on, and off"): a switch that inherits.
pub fn is_tri(bd: &Bound) -> bool {
    bd.def().ty == Ty::Toggle && bd.def().inherits.is_some()
}

impl App {
    // ── the world the menus are over ───────────────────────────────────────

    /// Every token source's id: the three every cauldron has, then the
    /// seed's other pools and the pools added here.
    pub fn pool_ids(&self) -> Vec<String> {
        let mut out: Vec<String> = TOKEN_SOURCES.iter().map(|s| s.to_string()).collect();
        out.extend(self.dash.pools.iter().map(|p| p.id.clone()).filter(|p| !TOKEN_SOURCES.contains(&p.as_str())).collect::<Vec<_>>());
        out.extend(self.settings.accounts_file.pools.iter().map(|p| p.id.clone()).filter(|p| !out.contains(p)).collect::<Vec<_>>());
        out
    }

    /// Whether account `id` of `pool` is on (its enabled switch).
    pub fn account_on(&self, pool: &str, id: &str) -> bool {
        self.sget(&Bound::new("pools.{pool}.accounts.{account}.enabled", crate::cauldron::settings::Ctx::account(pool, id))).val.as_bool()
    }

    /// Whether THIS cauldron may place work on account `id` of `pool` (its
    /// `cauldron.accounts.…allowed` switch, msg 186), apart from whether the
    /// account itself is on (a global switch).
    pub fn account_allowed(&self, pool: &str, id: &str) -> bool {
        self.sget(&Bound::new("cauldron.accounts.{pool}.{account}.allowed", Ctx::account(pool, id))).val.as_bool()
    }

    /// Every root the cauldron's files live under, and what each is: its
    /// home, each registered repo, each directory a cluster claims.
    pub fn file_roots(&self) -> Vec<(String, String)> {
        let mut roots: Vec<(String, String)> = vec![(self.env.root.display().to_string(), "cauldron home".into())];
        for r in &self.repos.repos {
            roots.push((r.path.clone(), "repo".into()));
        }
        for d in &self.arts.directories {
            if !roots.iter().any(|(p, _)| *p == d.path) {
                roots.push((d.path.clone(), "directory".into()));
            }
        }
        roots
    }

    /// Show settings page `t`: the root tab that shows it (V1.1), its first
    /// row selected when it was not already shown.
    pub fn set_cfg_tab(&mut self, t: CfgTab) {
        self.set_ctab(CTab::of_page(t));
    }

    /// What Config tab `t` is called: the first is THIS cauldron, by its
    /// name (msg 235: "You could have it say like design cauldron or prod
    /// cauldron").
    pub fn cfg_label(&self, t: CfgTab) -> String {
        match t {
            CfgTab::Cauldron => format!("{} cauldron", self.env.name),
            t => t.label().to_string(),
        }
    }

    /// The accounts of `pool`: its seed accounts, then those added here.
    pub fn accounts(&self, pool: &str) -> Vec<AcctView> {
        // V1.6: Claude Code's are the daemon's, by its keys — never the
        // seed's `global.json`. A label he gave one here is kept (a local
        // name for an email-keyed account); nothing is added or removed
        // here, since the daemon holds the roster.
        if pool == CLAUDE_CODE {
            let f = &self.settings.accounts_file;
            let keys = self.roster_keys();
            return keys
                .clone()
                .into_iter()
                .map(|k| {
                    let id = account_id(&k);
                    // The drawn name, masked ONCE here: his rename (itself
                    // through the masker, in case he typed an address), else
                    // the key told apart from its twins (V1-ACCT).
                    let label = match f.renamed.get(&format!("{pool}/{id}")) {
                        Some(r) => crate::pool::model::shown_name(r),
                        None => crate::pool::model::shown_among(&k, keys.iter().map(String::as_str)),
                    };
                    AcctView { id, label, seed: None, key: Some(k) }
                })
                .collect();
        }
        let mut out: Vec<AcctView> = {
            self.dash
                .pools
                .iter()
                .filter(|p| p.id == pool)
                .flat_map(|p| p.accounts.iter())
                .map(|l| AcctView { id: account_id(l), label: l.clone(), seed: None, key: None })
                .collect()
        };
        // Seed accounts removed here are gone; renamed ones wear their new name.
        let f = &self.settings.accounts_file;
        out.retain(|a| !f.removed.contains(&format!("{pool}/{}", a.id)));
        for a in &mut out {
            if let Some(l) = f.renamed.get(&format!("{pool}/{}", a.id)) {
                a.label = l.clone();
            }
        }
        for a in self.settings.added.iter().filter(|a| a.pool == pool) {
            out.push(AcctView { id: a.id.clone(), label: a.label.clone(), seed: None, key: None });
        }
        out
    }

    /// **WHAT ACCOUNT `id` OF `pool` IS CALLED ON SCREEN — NEVER A RAW
    /// ADDRESS** (V1-CLEAN2, V1-CLEAN1-I B1). Its label when this face lists
    /// it, else the daemon's key of the live card (or loading card) whose id
    /// it is — the Status › Tokens card Enter opens — and every one of them
    /// through [`crate::pool::model::shown_name`], the one masker. Only an id
    /// nothing names is drawn as it is, and an id never carries an address
    /// ([`account_id`]).
    pub fn account_name(&self, pool: &str, id: &str) -> String {
        use crate::pool::model::{shown_among, shown_name};
        if let Some(x) = self.accounts(pool).into_iter().find(|x| x.id == id) {
            // A daemon-keyed row's label is already the drawn name (masked
            // once in `accounts`); masking it again would strip its twin
            // mark. Any other label is masked here, once.
            return if x.key.is_some() { x.label } else { shown_name(&x.label) };
        }
        if pool == CLAUDE_CODE {
            if let Some(pl) = self.live.pool() {
                let keys: Vec<&str> = pl.board.cards.iter().map(|c| c.label.as_str()).chain(pl.loading.iter().map(String::as_str)).collect();
                if let Some(k) = keys.iter().find(|k| account_id(k) == id) {
                    return shown_among(k, keys.iter().copied());
                }
            }
        }
        id.to_string()
    }

    /// The account board's card of `acct`, when it has one.
    pub fn card(&self, pool: &str, acct: &AcctView) -> Option<&crate::cauldron::dash::Account> {
        acct.seed.filter(|_| pool == CLAUDE_CODE).map(|i| &self.dash.threads.accounts[i])
    }

    /// Whether account `acct` of `pool` is flagged OPPORTUNISTIC now (msg
    /// 235): it is on, it may be (its `opportunistic` setting, inherited
    /// from its source and the global default), and it is not on pace to
    /// spend its week — its pressure is above 1. `None` while it has no
    /// weekly reading.
    pub fn opportunistic(&self, pool: &str, acct: &AcctView) -> Option<bool> {
        // The daemon's pressure for one of its accounts (V1.6); the seed
        // card's for the rest.
        let p = match &acct.key {
            Some(k) => self.live_pressure(k)?,
            None => self.card(pool, acct)?.pressure()?,
        };
        let ctx = Ctx::account(pool, &acct.id);
        let may = self.val("pools.{pool}.accounts.{account}.opportunistic.enabled", ctx.clone()).as_bool() && self.val("pools.{pool}.accounts.{account}.enabled", ctx).as_bool();
        Some(may && p > 1.0)
    }

    /// Every account's opportunistic flag now: `(pool, account) → flag`.
    pub fn flags(&self) -> Vec<((String, String), Option<bool>)> {
        let mut out = Vec::new();
        for p in self.pool_ids() {
            for a in self.accounts(&p) {
                let f = self.opportunistic(&p, &a);
                out.push(((p.clone(), a.id.clone()), f));
            }
        }
        out
    }

    /// Record in the flip log every account whose flag turned on or off
    /// since `before`, saying what did it.
    pub fn note_flips(&mut self, before: Vec<((String, String), Option<bool>)>, why: &str) {
        let at = chrono::Utc::now().to_rfc3339();
        let flips: Vec<settings::Flip> = self
            .flags()
            .into_iter()
            .filter_map(|(k, now)| {
                let was = before.iter().find(|(b, _)| *b == k).map(|(_, f)| *f).unwrap_or(None);
                let on = now?;
                (was != Some(on)).then(|| settings::Flip { at: at.clone(), pool: k.0, account: k.1, on, why: why.to_string() })
            })
            .collect();
        if let Err(e) = self.settings.record_flips(flips) {
            self.error = Some(format!("the flip log NOT written: {e:#}"));
        }
    }

    /// An account's last flip: the newest of the seed's and those recorded here.
    pub fn last_flip(&self, pool: &str, account: &str) -> Option<settings::Flip> {
        self.dash
            .flips
            .iter()
            .chain(self.settings.flips.iter())
            .filter(|f| f.pool == pool && f.account == account)
            .max_by(|a, b| a.at.cmp(&b.at))
            .cloned()
    }

    /// The models subscription source `pool` was detected to offer.
    pub fn detected_models(&self, pool: &str) -> Vec<String> {
        self.dash.models.get(pool).cloned().unwrap_or_default()
    }

    /// Whether source `pool` offers Fable (a Claude model): its Fable
    /// settings mean nothing on a source that does not (task 0209 D6).
    pub fn offers_fable(&self, pool: &str) -> bool {
        self.detected_models(pool).iter().any(|m| m.to_ascii_lowercase().contains("fable"))
    }

    /// `keys` without the Fable ones when `pool` does not offer Fable.
    fn for_pool<'k>(&self, pool: &str, keys: &[&'k str]) -> Vec<&'k str> {
        let fable = self.offers_fable(pool);
        keys.iter().copied().filter(|k| fable || !k.contains(".fable.")).collect()
    }

    /// The models source `pool` offers a role: its detected models that
    /// are switched on.
    pub fn pool_models(&self, pool: &str) -> Vec<String> {
        self.detected_models(pool).into_iter().filter(|m| self.val("pools.{pool}.model.{model}", Ctx::model(pool, m)).as_bool()).collect()
    }

    // ── inference endpoints (design 0199) ────────────────────────────────

    /// Every endpoint THIS cauldron can see, as `(here, id)`: each global
    /// one, then each registered here.
    pub fn seen_endpoints(&self) -> Vec<(bool, String)> {
        let mut out: Vec<(bool, String)> = self.settings.endpoint_ids(false).into_iter().map(|id| (false, id)).collect();
        out.extend(self.settings.endpoint_ids(true).into_iter().map(|id| (true, id)));
        out
    }

    /// Endpoint `id`'s value of `field` (`url`, `name`, …), registered
    /// `here` or globally.
    pub fn ep_val(&self, here: bool, id: &str, field: &str) -> Val {
        self.sget(&ep_bound(here, id, field)).val
    }

    /// What endpoint `id` is called.
    pub fn ep_name(&self, here: bool, id: &str) -> String {
        match self.ep_val(here, id, "name") {
            Val::Str(s) if !s.is_empty() => s,
            _ => id.to_string(),
        }
    }

    /// Whether this cauldron places work on endpoint `id`: a global one's
    /// `use here` (inheriting its own switch), its own one's switch.
    pub fn ep_used_here(&self, here: bool, id: &str) -> bool {
        if here {
            self.ep_val(true, id, "enabled").as_bool()
        } else {
            self.val("cauldron.use.{ep}.enabled", Ctx::ep(id)).as_bool()
        }
    }

    /// The models endpoint `id`'s key may serve: its explicit list, or —
    /// `any` — what the endpoint lists for it (the fake board's listing:
    /// nothing is called from this face).
    pub fn ep_models(&self, here: bool, id: &str) -> Vec<String> {
        match self.ep_val(here, id, "models") {
            Val::Str(m) if m == "explicit" => self.ep_val(here, id, "model_list").as_list().to_vec(),
            _ => self.dash.endpoints.get(id).map(|e| e.listed.clone()).unwrap_or_default(),
        }
    }

    /// The models this cauldron may use of endpoint `id`: the key's, or a
    /// global one's narrowed here.
    pub fn ep_models_here(&self, here: bool, id: &str) -> Vec<String> {
        if !here && self.val("cauldron.use.{ep}.narrow", Ctx::ep(id)).as_bool() {
            let key = self.ep_models(false, id);
            return self.val("cauldron.use.{ep}.model_list", Ctx::ep(id)).as_list().iter().filter(|m| key.contains(m)).cloned().collect();
        }
        self.ep_models(here, id)
    }

    /// The url endpoint `id` is called at from this cauldron, as a table
    /// shows it: no scheme, cut with `…` to its column (design 0199 §1).
    pub fn ep_host(&self, here: bool, id: &str) -> String {
        let url = if here { self.ep_val(true, id, "url") } else { self.val("cauldron.use.{ep}.url", Ctx::ep(id)) };
        match url {
            Val::Str(u) => cut(u.split_once("://").map_or(u.as_str(), |(_, rest)| rest).trim_end_matches('/'), EP_URL_W),
            _ => String::new(),
        }
    }

    /// The period endpoint `id`'s budget runs over, as a dollar figure's
    /// tail says it: ` / month`, or nothing for `none`.
    pub fn ep_per(&self, here: bool, id: &str) -> &'static str {
        match self.ep_val(here, id, "budget_period") {
            Val::Str(p) if p == "daily" => " / day",
            Val::Str(p) if p == "weekly" => " / week",
            Val::Str(p) if p == "monthly" => " / month",
            _ => "",
        }
    }

    /// A dollar figure of endpoint `id` with its period: `$100 / month`, or
    /// `off`.
    pub fn ep_dollars(&self, here: bool, id: &str, v: &Val) -> String {
        match v {
            Val::Int(n) => format!("${n}{}", self.ep_per(here, id)),
            _ => "off".into(),
        }
    }

    /// The roles endpoint `id` serves here (design 0199 D8).
    pub fn ep_used_by(&self, id: &str) -> Vec<&'static str> {
        ROLES.iter().filter(|r| self.val("roles.{role}.endpoint", Ctx::role(r.id)) == Val::Str(id.to_string())).map(|r| r.id).collect()
    }

    /// Why the endpoint being added (`here` or global) cannot be added yet,
    /// or `None` when it can: a name, a url, and — on a template that needs
    /// one — a key.
    pub fn draft_missing(&self, here: bool) -> Option<&'static str> {
        let id = DRAFT_EP;
        let empty = |v: Val| !matches!(v, Val::Str(s) if !s.trim().is_empty());
        if empty(self.ep_val(here, id, "name")) {
            return Some("needs a name");
        }
        if empty(self.ep_val(here, id, "url")) {
            return Some("needs a url");
        }
        let t = self.settings.endpoint_template(here, id);
        if t.key_required && empty(self.ep_val(here, id, "key")) {
            return Some("needs a key");
        }
        None
    }

    /// Whether setting `bd` is drawn as a STRIP — the segmented control,
    /// every option on screen (design 0199: the template strip is "his own
    /// toggle widget with six options"): an endpoint's few-option choices.
    pub fn strip_words(&self, bd: &Bound) -> Option<&'static [&'static str]> {
        let k = bd.key;
        if k == "ui.theme" {
            return Some(settings::THEMES);
        }
        if !(k.starts_with("endpoints.") || k.starts_with("cauldron.endpoints.")) {
            return None;
        }
        let field = k.rsplit('.').next().unwrap_or(k);
        Some(match field {
            "template" => settings::TEMPLATE_WORDS,
            "server" => settings::SERVER_WORDS,
            "protocol" => settings::PROTOCOL_WORDS,
            "auth" => settings::AUTH_WORDS,
            "models" => settings::MODEL_MODES,
            "budget_period" => settings::PERIODS,
            "threshold_action" => settings::THRESHOLD_WORDS,
            _ => return None,
        })
    }

    // ── values ─────────────────────────────────────────────────────────────

    /// The palette this face draws in: Config › TUI › theme.
    pub fn theme(&self) -> tui_render::palette::Theme {
        match self.sget(&Bound::new("ui.theme", Ctx::default())).val {
            Val::Str(w) => tui_render::palette::Theme::from_word(&w).unwrap_or_default(),
            _ => tui_render::palette::Theme::default(),
        }
    }

    /// The effective value of a setting, and where it came from.
    pub fn sget(&self, b: &Bound) -> Resolved {
        if b.key == "ui.sidebar_ratio" {
            // Kept in ONE place: the layout the divider drag writes.
            return match self.layout.sidebar_permille {
                Some(p) => Resolved { val: Val::Int(i64::from((p + 5) / 10)), prov: Prov::Here },
                None => Resolved { val: Val::Str("auto".into()), prov: Prov::Default },
            };
        }
        self.settings.get(b.def(), &b.ctx)
    }

    /// What a setting would be without a value of its own here.
    pub fn sinherited(&self, b: &Bound) -> Resolved {
        if b.key == "ui.sidebar_ratio" {
            return Resolved { val: Val::Str("auto".into()), prov: Prov::Default };
        }
        self.settings.inherited(b.def(), &b.ctx)
    }

    /// Whether `b` holds a value of its own here.
    pub fn sis_set(&self, b: &Bound) -> bool {
        if b.key == "ui.sidebar_ratio" {
            return self.layout.sidebar_permille.is_some();
        }
        self.settings.is_set(b.def(), &b.ctx)
    }

    /// Set a value here. Refused, with the reason, when it breaks a bound: a
    /// cap another setting puts on it. A value that is what the setting
    /// would be anyway is NOT kept as a value of its own: the setting goes
    /// back to its default (8e4fca10 msg 235: "even if you change it from
    /// off to on and then back to off, it still says that it's overriding
    /// the default of off") — except a tri-state switch, whose `on` is a
    /// choice apart from `inherit`.
    pub fn sset(&mut self, b: &Bound, v: Val) -> Result<(), String> {
        let d = b.def();
        // A greyed source's setting is never written here, and nothing points
        // work at one (R-V1 item 7): what is on disk of it stays as it is.
        if settings::greyed_key(b.key, &b.ctx) || settings::greyed_value(b.key, &v) {
            return Err(settings::GREYED.into());
        }
        if let (Some(cap), Val::Int(n)) = (d.cap, &v) {
            let cb = Bound::new(cap, Ctx::default());
            if let Some(m) = self.sget(&cb).val.as_int() {
                if *n > m {
                    return Err(format!("at most {} — {}", show(cb.def(), &Val::Int(m)), cb.def().label));
                }
            }
        }
        if b.key == "ui.sidebar_ratio" {
            let Val::Int(pct) = v else { return Err("a percent".into()) };
            self.layout.sidebar_permille = Some((pct * 10) as u16);
            self.save_layout();
            return Ok(());
        }
        // A global endpoint switched off is off in every cauldron (design
        // 0199: "turn ON an endpoint the registry has disabled" is refused).
        if b.key == "cauldron.use.{ep}.enabled" && v == Val::Bool(true) && !self.ep_val(false, &b.ctx.ep, "enabled").as_bool() {
            return Err("disabled in Providers".into());
        }
        // A value he CHOOSES is kept as he chose it, even when it equals the
        // default (0250, the code half of 0225 F1): it never drops out of his
        // file, and it reads as the default (no ↺, msg 235 V4). Only ↺, `x`
        // or `inherit` clears. A setting that INHERITS is the one exception:
        // the value it inherits, typed in, is inheriting — kept, it would stop
        // following its parent while saying it inherits.
        if d.inherits.is_some() && !is_tri(b) && d.ty != Ty::LiveLogin && !b.key.ends_with(".template") && v == self.sinherited(b).val {
            return self.sclear(b);
        }
        self.settings.set(d, &b.ctx, v).map_err(|e| format!("NOT saved: {e:#}"))
    }

    /// Clear a value here: it inherits again.
    pub fn sclear(&mut self, b: &Bound) -> Result<(), String> {
        if settings::greyed_key(b.key, &b.ctx) {
            return Err(settings::GREYED.into());
        }
        if b.key == "ui.sidebar_ratio" {
            self.layout.sidebar_permille = None;
            self.save_layout();
            return Ok(());
        }
        self.settings.clear(b.def(), &b.ctx).map_err(|e| format!("NOT saved: {e:#}"))
    }

    fn val(&self, key: &'static str, ctx: Ctx) -> Val {
        self.sget(&Bound::new(key, ctx)).val
    }

    fn shown(&self, key: &'static str, ctx: Ctx) -> String {
        let bd = Bound::new(key, ctx);
        show(bd.def(), &self.sget(&bd).val)
    }

    /// The switch setting `bd` is drawn with: `inherit · on · off` when it
    /// can inherit, the cauldron's own `enabled · disabled`, else `on · off`.
    pub fn toggle_of(&self, bd: &Bound) -> Toggle {
        if let (Some(words), Ty::Choice(opts)) = (self.strip_words(bd), bd.def().ty) {
            let cur = self.sget(bd).val;
            let selected = opts.iter().position(|o| cur == Val::Str(o.to_string())).unwrap_or(0);
            return Toggle { options: words, selected };
        }
        if bd.key == "cauldron.use.{ep}.narrow" {
            return Toggle::pair(&NARROW_WORDS, !self.sget(bd).val.as_bool());
        }
        if is_tri(bd) {
            let own = self.sis_set(bd).then(|| self.sget(bd).val.as_bool());
            return Toggle::tri(own);
        }
        let on = self.sget(bd).val.as_bool();
        if bd.key == "cauldron.enabled" {
            return Toggle::pair(&CAULDRON_WORDS, on);
        }
        Toggle::new(on)
    }

    /// Whether setting `bd` holds something other than its default — what
    /// its revert glyph ↺ is shown for (msg 235: "you could have a little
    /// revert ASCII character appear to the right of it. And then if you
    /// click on that revert button, then it will revert to the default").
    pub fn reverts(&self, bd: &Bound) -> bool {
        let d = bd.def();
        // A key is replaced by pasting another, never reverted away.
        if matches!(d.ty, Ty::LiveLogin | Ty::Action | Ty::Secret) || bd.key.ends_with(".template") || !self.sis_set(bd) {
            return false;
        }
        is_tri(bd) || self.sget(bd).val != self.sinherited(bd).val
    }

    /// What setting `bd`'s default IS, said on its row (msg 235: "we should
    /// actually just say what the default is"): `default off` — or, for a
    /// setting that inherits, what inheriting gives now: `inherits 80 %`.
    pub fn default_note(&self, bd: &Bound) -> String {
        let d = bd.def();
        // An endpoint's key says whether it is there, never what it is.
        if d.ty == Ty::Secret {
            let here = bd.key.starts_with("cauldron.");
            return if self.sis_set(bd) {
                "set".into()
            } else if bd.key.ends_with(".key") && self.settings.endpoint_template(here, &bd.ctx.ep).key_required {
                "required".into()
            } else {
                String::new()
            };
        }
        // A url says the url it falls back to, without its scheme (task
        // 0209 D12): the value, never a word standing in for it.
        if bd.key.ends_with("endpoints.{ep}.url") || bd.key == "cauldron.use.{ep}.url" {
            let inh = self.sinherited(bd);
            let url = match &inh.val { Val::Str(u) => u.trim_start_matches("https://").trim_start_matches("http://").to_string(), _ => String::new() };
            let word = if bd.key.starts_with("cauldron.use.") { "inherits" } else { "default" };
            return if url.is_empty() { String::new() } else { format!("{word} {url}") };
        }
        // The cauldron's switch and the live login say their defaults too.
        if bd.key == "cauldron.enabled" {
            return "default enabled".into();
        }
        // The live login is a RADIO (one account of the machine at a time):
        // on, its row says so instead of a default it could be reverted to —
        // it goes off only by another account (or none, under Permissions ›
        // Identity) taking it (0214 A13).
        if d.ty == Ty::LiveLogin {
            return if self.sget(bd).val.as_bool() { "one account at a time".into() } else { "default off".into() };
        }
        // A list with a default says it (0217 B12: the settle ladder).
        if d.ty == Ty::List && !d.default.is_empty() && !d.default.starts_with('@') && !bd.key.starts_with("cauldron.use.") {
            return format!("default {}", d.default);
        }
        if matches!(d.ty, Ty::Action | Ty::List) || bd.key.ends_with(".template") || bd.key == "cauldron.use.{ep}.narrow" {
            return String::new();
        }
        let inh = self.sinherited(bd);
        // A strip says its default in its own words: `default park new`.
        let v = match (self.strip_words(bd), d.ty, &inh.val) {
            (Some(words), Ty::Choice(opts), Val::Str(x)) => opts.iter().position(|o| o == x).map_or_else(|| x.clone(), |i| words[i].to_string()),
            _ => show(d, &inh.val),
        };
        // A reference to an account with none by default says so (0211 R9).
        // So does an endpoint reference, and a choice with none (0217 B12:
        // `endpoint`, `engine` showed nothing).
        let none_by_default = matches!(d.ty, Ty::AccountRef | Ty::EndpointRef) || matches!(d.ty, Ty::Choice(_)) && d.default.is_empty();
        if none_by_default && (matches!(&inh.val, Val::Unset | Val::Off) || matches!(&inh.val, Val::Str(x) if x.is_empty() || x == "none")) {
            return "default none".into();
        }
        if v.is_empty() || inh.val == Val::Unset {
            return String::new();
        }
        // A value this environment defaults in code (ENV_DEFAULTS) is a
        // DEFAULT, never inherited from the row it would otherwise inherit
        // (task 0209 D3: Claude Code's weekly 80 % is not Providers' 95 %).
        let env_default = self.settings.env_defaults.contains_key(&settings::subst(d.key, &bd.ctx));
        let word = if d.inherits.is_some() && !env_default { "inherits" } else { "default" };
        format!("{word} {v}")
    }

    /// Option `opt` of setting `bd`'s switch picked (a click on it): a
    /// tri-state's `inherit` clears it; any other option sets it.
    pub fn set_option(&mut self, bd: &Bound, opt: usize) {
        let t = self.toggle_of(bd);
        if opt >= t.options.len() || opt == t.selected {
            return;
        }
        if bd.key == "cauldron.enabled" {
            // The cauldron's switch asks first (msg 186), however it is flipped.
            return self.ask_enable();
        }
        if let (Some(_), Ty::Choice(opts)) = (self.strip_words(bd), bd.def().ty) {
            self.apply(bd, Ok(Val::Str(opts[opt].to_string())));
            return;
        }
        // Narrowing starts from the key's whole list, and only takes away;
        // back to inherit forgets the narrowed list.
        if bd.key == "cauldron.use.{ep}.narrow" {
            let list = Bound::new("cauldron.use.{ep}.model_list", bd.ctx.clone());
            if opt == 1 {
                let all = self.ep_models(false, &bd.ctx.ep);
                if self.apply(bd, Ok(Val::Bool(true))) {
                    self.apply(&list, Ok(Val::List(all)));
                }
            } else if self.sclear(bd).is_ok() {
                let _ = self.sclear(&list);
            }
            return;
        }
        let v = match (is_tri(bd), opt) {
            (true, 0) => None,
            (true, 1) | (false, 0) => Some(Val::Bool(true)),
            _ => Some(Val::Bool(false)),
        };
        match v {
            Some(v) => {
                self.apply(bd, Ok(v));
            }
            None => self.clear_entry(Entry::Set(bd.clone())),
        }
    }

    /// The name a setting's row shows: a model's switch is the model's
    /// name; every other setting its label.
    pub fn row_name(&self, bd: &Bound) -> String {
        match bd.key {
            "pools.{pool}.model.{model}" => bd.ctx.model.clone(),
            _ => bd.def().label.to_string(),
        }
    }

    /// A setting's value as its row shows it: a secret masked (or, never
    /// set, the template's stand-in — Ollama's `ollama` — or nothing), an
    /// endpoint by its name, a cauldron's metered cap with its key's period.
    pub fn value_text(&self, bd: &Bound) -> String {
        let d = bd.def();
        let r = self.sget(bd);
        match (d.ty, &r.val) {
            (Ty::Secret, Val::Str(s)) if r.prov == Prov::Default => s.clone(),
            (Ty::Secret, Val::Unset) => String::new(),
            (Ty::EndpointRef, Val::Str(id)) => match self.seen_endpoints().into_iter().find(|(_, x)| x == id) {
                Some((here, id)) => self.ep_name(here, &id),
                None => format!("{id} (gone)"),
            },
            (Ty::EndpointRef, _) => "none".into(),
            (_, v) if bd.key == "cauldron.use.{ep}.metered_cap_usd" => self.ep_dollars(false, &bd.ctx.ep, v),
            (_, v) => show(d, v),
        }
    }

    // ── pages ──────────────────────────────────────────────────────────────

    pub fn nav(&self) -> &Nav {
        &self.snav
    }

    fn nav_mut(&mut self) -> &mut Nav {
        &mut self.snav
    }

    /// Every line of a Config tab as it is open: each band's header, and
    /// under each open band what it holds, a level deeper.
    pub fn entries(&self) -> Vec<Flat> {
        let root = self.cfg_tab.root();
        let mut out = Vec::new();
        self.flatten(&root, 0, None, &mut out);
        out
    }

    /// Every line of a ROW's modal: what its page holds, at the top level.
    pub fn modal_entries(&self, page: &SPage) -> Vec<Flat> {
        let mut out = Vec::new();
        self.flatten(page, 0, None, &mut out);
        out
    }

    fn flatten(&self, page: &SPage, depth: usize, parent: Option<SPage>, out: &mut Vec<Flat>) {
        let entries = self.page_entries(page);
        // The section the entries are in, after a head: its band, whether it
        // is open, and whether it is always open (its lines then climb to
        // the band around it: its head is not a row).
        let mut section: Option<(SPage, bool, bool)> = None;
        let is_head = |x: &Entry| matches!(x, Entry::Head(..) | Entry::Fixed(..));
        for (i, e) in entries.iter().cloned().enumerate() {
            // A head is a band (one band grammar, audit 0177 V3): it holds
            // every entry up to the next head, a level deeper.
            if let Entry::Head(title, meta) | Entry::Fixed(title, meta) = &e {
                let fixed = matches!(e, Entry::Fixed(..));
                let band = SPage::Section(Box::new(page.clone()), title.clone());
                let open = fixed || self.nav().is_open(&band);
                let head = if fixed { e.clone() } else { Entry::Sub { page: band.clone(), label: title.clone(), summary: meta.clone() } };
                out.push(Flat { depth, entry: head, open: (!fixed).then_some(open), parent: parent.clone() });
                section = Some((band, open, fixed));
                continue;
            }
            let (depth, parent) = match &section {
                None => (depth, parent.clone()),
                Some((band, open, fixed)) => {
                    // The gap before the next head stands between the bands.
                    let tail = entries[i..].iter().take_while(|x| **x == Entry::Gap).count();
                    let at_end = entries.get(i + tail).is_none_or(is_head);
                    if e == Entry::Gap && at_end {
                        if tail == 1 && entries.get(i + 1).is_some() {
                            out.push(Flat { depth, entry: e, open: None, parent: parent.clone() });
                        }
                        continue;
                    }
                    if !open {
                        continue;
                    }
                    (depth + 1, if *fixed { parent.clone() } else { Some(band.clone()) })
                }
            };
            match e.band() {
                Some(band) => {
                    let open = self.nav().is_open(&band);
                    out.push(Flat { depth, entry: e, open: Some(open), parent: parent.clone() });
                    // A band never holds itself, however its pages refer to
                    // each other.
                    if open && band != *page && depth < 8 {
                        self.flatten(&band, depth + 1, Some(band.clone()), out);
                    }
                }
                None => out.push(Flat { depth, entry: e, open: None, parent: parent.clone() }),
            }
        }
    }

    /// The selectable lines of a Config tab as it is open, in order.
    pub fn selectable_flat(&self) -> Vec<Flat> {
        self.entries().into_iter().filter(|f| f.entry.selectable()).collect()
    }

    /// The selectable entries of a Config tab as it is open, in order.
    pub fn selectable(&self) -> Vec<Entry> {
        self.selectable_flat().into_iter().map(|f| f.entry).collect()
    }

    /// The selected row of the Config tab.
    pub fn settings_sel(&self) -> usize {
        self.rows[SETTINGS_ROW]
    }

    fn set_settings_sel(&mut self, i: usize) {
        self.rows[SETTINGS_ROW] = i;
    }

    fn sub(&self, page: SPage, label: &str, summary: String) -> Entry {
        Entry::Sub { page, label: label.into(), summary }
    }

    fn sets(out: &mut Vec<Entry>, keys: &[&'static str], ctx: &Ctx) {
        out.extend(keys.iter().map(|k| Entry::Set(Bound::new(k, ctx.clone()))));
    }

    fn pool_summary(&self, pool: &str) -> String {
        let n = self.accounts(pool).len();
        let models = self.detected_models(pool).len();
        if models == 0 {
            return count(n, "account", "accounts");
        }
        format!("{} · {}", count(n, "account", "accounts"), count(models, "model", "models"))
    }

    /// The column names of the table a ROW of `page` is in.
    pub fn row_head(&self, page: &SPage) -> Vec<&'static str> {
        match page {
            // V1.6: the daemon's machine-wide switch, then the global default
            // every cauldron inherits — two switches, named apart.
            SPage::Account(p, _) if p == CLAUDE_CODE => vec!["account", "in the pool", "on in new cauldrons", "weekly threshold", "opportunistic"],
            SPage::Account(..) => vec!["account", "on in new cauldrons", "weekly threshold", "opportunistic"],
            // An allowance: an account allowed here is in use only while it
            // is itself enabled (0214 A7).
            SPage::UseAccount(..) => vec!["account", "allowed here", "weekly threshold", "opportunistic"],
            // Design 0199 §1 and §5: the registry, and what this cauldron
            // takes of everything it can see.
            SPage::Endpoint { here: false, .. } => vec!["endpoint", "url", "models", "budget", "enabled"],
            SPage::Endpoint { here: true, .. } | SPage::UseEndpoint(_) => vec!["endpoint", "scope", "url", "metered cap", "use here"],
            // A metered role's source is its endpoint (design 0199 D8).
            SPage::Role(_) => vec!["role", "model", "fallback", "source"],
            _ => Vec::new(),
        }
    }

    /// What a ROW of `page` is called: its first column.
    pub fn row_label(&self, page: &SPage) -> String {
        match page {
            SPage::Account(p, a) | SPage::UseAccount(p, a) => self.account_name(p, a),
            SPage::Endpoint { here, id } => self.ep_name(*here, id),
            SPage::UseEndpoint(id) => self.ep_name(false, id),
            other => other.title(),
        }
    }

    /// The cells of a ROW of `page`, after its name (msg 235: "For the
    /// accounts, the simplest thing is just to have on/off and weekly
    /// threshold and opportunistic mode").
    pub fn row_cells(&self, page: &SPage) -> Vec<Cell> {
        let text = |key: &'static str, ctx: Ctx| Cell::Text(self.shown(key, ctx));
        match page {
            SPage::Endpoint { here: false, id } => {
                let models = self.ep_models_label(false, id);
                vec![Cell::Text(self.ep_host(false, id)), Cell::Text(models), Cell::Text(self.ep_dollars(false, id, &self.ep_val(false, id, "budget_usd"))), Cell::Toggle(ep_bound(false, id, "enabled"))]
            }
            // One registered here: its budget is its own wallet and its
            // allowance both; its switch is plain (nothing above it).
            SPage::Endpoint { here: true, id } => vec![
                Cell::Text("here".into()),
                Cell::Text(self.ep_host(true, id)),
                Cell::Text(self.ep_dollars(true, id, &self.ep_val(true, id, "budget_usd"))),
                Cell::Toggle(ep_bound(true, id, "enabled")),
            ],
            SPage::UseEndpoint(id) => {
                let ctx = Ctx::ep(id);
                vec![
                    Cell::Text("global".into()),
                    Cell::Text(self.ep_host(false, id)),
                    Cell::Text(self.ep_dollars(false, id, &self.val("cauldron.use.{ep}.metered_cap_usd", ctx.clone()))),
                    Cell::Toggle(Bound::new("cauldron.use.{ep}.enabled", ctx)),
                ]
            }
            SPage::Account(p, a) => {
                let ctx = Ctx::account(p, a);
                let in_pool = (p == CLAUDE_CODE).then(|| {
                    let key = self.accounts(p).into_iter().find(|x| x.id == *a).and_then(|x| x.key).unwrap_or_default();
                    Cell::Text(self.in_pool_words(&key))
                });
                in_pool.into_iter().chain([
                    Cell::Toggle(Bound::new("pools.{pool}.accounts.{account}.enabled", ctx.clone())),
                    text("pools.{pool}.accounts.{account}.budget.weekly.threshold_pct", ctx.clone()),
                    Cell::Toggle(Bound::new("pools.{pool}.accounts.{account}.opportunistic.enabled", ctx)),
                ])
                .collect()
            }
            SPage::UseAccount(p, a) => {
                let ctx = Ctx::account(p, a);
                vec![
                    Cell::Toggle(Bound::new("cauldron.accounts.{pool}.{account}.allowed", ctx.clone())),
                    text("cauldron.accounts.{pool}.{account}.weekly_threshold_pct", ctx.clone()),
                    Cell::Toggle(Bound::new("cauldron.accounts.{pool}.{account}.opportunistic", ctx)),
                ]
            }
            SPage::Role(r) => {
                let ctx = Ctx::role(r);
                let ep = Bound::new("roles.{role}.endpoint", ctx.clone());
                let source = if self.sget(&ep).val == Val::Unset { self.shown("roles.{role}.pool", ctx.clone()) } else { self.value_text(&ep) };
                vec![text("roles.{role}.model", ctx.clone()), text("roles.{role}.fallback", ctx), Cell::Text(source)]
            }
            _ => Vec::new(),
        }
    }

    /// An endpoint's models as its table row says them: `any`, its one
    /// model, or `<first> +n`.
    pub fn ep_models_label(&self, here: bool, id: &str) -> String {
        if !matches!(self.ep_val(here, id, "models"), Val::Str(m) if m == "explicit") {
            return "any".into();
        }
        let list = self.ep_val(here, id, "model_list");
        match list.as_list() {
            [] => "none".into(),
            [one] => cut(one, EP_URL_W + 4),
            [first, rest @ ..] => {
                let more = format!(" +{}", rest.len());
                format!("{}{more}", cut(first, EP_URL_W + 4 - more.len()))
            }
        }
    }

    /// The INFERENCE ENDPOINTS band's metadata: counts, never dollars
    /// (design 0199 D6).
    fn endpoints_summary(&self, cauldron: bool) -> String {
        if cauldron {
            let seen = self.seen_endpoints();
            let used = seen.iter().filter(|(h, id)| self.ep_used_here(*h, id)).count();
            let own = seen.iter().filter(|(h, _)| *h).count();
            let mut meta = format!("{} · {used} used here", count(seen.len(), "endpoint", "endpoints"));
            if own > 0 {
                meta.push_str(&format!(" · {own} added here"));
            }
            return meta;
        }
        let ids = self.settings.endpoint_ids(false);
        let on = ids.iter().filter(|id| self.ep_val(false, id, "enabled").as_bool()).count();
        format!("{} · {on} on", count(ids.len(), "endpoint", "endpoints"))
    }

    /// An endpoint's modal (design 0199 §2), or the add modal while it is
    /// [`DRAFT_EP`] (§3, §4): every row it has, its template's own rows
    /// only where they apply.
    fn endpoint_entries(&self, here: bool, id: &str, out: &mut Vec<Entry>) {
        let draft = id == DRAFT_EP;
        let set = |out: &mut Vec<Entry>, f: &str| out.push(Entry::Set(ep_bound(here, id, f)));
        let template = match self.ep_val(here, id, "template") {
            Val::Str(t) => t,
            _ => "custom".into(),
        };
        let local = template == "local";
        if draft {
            set(out, "template");
            if local {
                set(out, "server");
            }
        }
        set(out, "name");
        if !draft {
            out.push(Entry::Fact("template".into(), settings::template_word(&template).into()));
            if local {
                let server = ep_bound(here, id, "server");
                let at = settings::SERVERS.iter().position(|x| self.sget(&server).val == Val::Str(x.to_string())).unwrap_or(0);
                out.push(Entry::Fact("server".into(), settings::SERVER_WORDS[at].into()));
            }
        }
        for f in ["protocol", "url", "key", "auth"] {
            set(out, f);
        }
        // Headers (none of the local servers takes any): each a line, x
        // takes one off, `+ add header` puts one on.
        if !local {
            let list = ep_bound(here, id, "headers");
            let items = self.sget(&list).val.as_list().to_vec();
            out.push(Entry::Fact("headers".into(), if items.is_empty() { "none".into() } else { count(items.len(), "header", "headers") }));
            for (index, text) in items.into_iter().enumerate() {
                out.push(Entry::Item { list: list.clone(), index, text });
            }
            out.push(Entry::Add(Add::Item(list)));
        }
        set(out, "models");
        if matches!(self.ep_val(here, id, "models"), Val::Str(m) if m == "explicit") {
            let list = ep_bound(here, id, "model_list");
            for (index, text) in self.sget(&list).val.as_list().iter().enumerate() {
                out.push(Entry::Item { list: list.clone(), index, text: text.clone() });
            }
            out.push(Entry::Add(Add::Item(list)));
        }
        for f in ["enabled", "budget_usd", "budget_period"] {
            set(out, f);
        }
        let managed = template == "openrouter" && self.sis_set(&ep_bound(here, id, "management_key"));
        out.push(Entry::Fact("enforced by".into(), if managed { "openrouter · cauldron meter".into() } else { "cauldron meter".into() }));
        for f in ["threshold_pct", "threshold_action", "max_concurrent"] {
            set(out, f);
        }
        if template == "openrouter" {
            set(out, "management_key");
        }
        if local || template == "custom" {
            set(out, "price_in");
            set(out, "price_out");
        }
        if !draft {
            let by = self.ep_used_by(id);
            out.push(Entry::Fact("used by".into(), if by.is_empty() { "none".into() } else { by.iter().map(|r| settings::role_name(r)).collect::<Vec<_>>().join(", ") }));
        }
        out.push(Entry::Gap);
        if draft {
            out.push(Entry::Do { act: Act::AddEndpoint { here }, label: "add endpoint".into(), summary: self.draft_missing(here).unwrap_or_default().into() });
        } else {
            out.push(Entry::Do { act: Act::RemoveEndpoint { here, id: id.into() }, label: "remove endpoint".into(), summary: String::new() });
        }
    }

    /// A GLOBAL endpoint as this cauldron takes it (design 0199 §6): what
    /// this cauldron sets of it, then the endpoint's own facts, read here
    /// and set on Providers.
    fn use_endpoint_entries(&self, id: &str, out: &mut Vec<Entry>) {
        let ctx = Ctx::ep(id);
        Self::sets(out, &["cauldron.use.{ep}.enabled", "cauldron.use.{ep}.url", "cauldron.use.{ep}.narrow"], &ctx);
        if self.val("cauldron.use.{ep}.narrow", ctx.clone()).as_bool() {
            let list = Bound::new("cauldron.use.{ep}.model_list", ctx.clone());
            for (index, text) in self.sget(&list).val.as_list().iter().enumerate() {
                out.push(Entry::Item { list: list.clone(), index, text: text.clone() });
            }
            out.push(Entry::Add(Add::Item(list)));
        }
        Self::sets(out, &["cauldron.use.{ep}.metered_cap_usd", "cauldron.use.{ep}.threshold_pct", "cauldron.use.{ep}.max_concurrent"], &ctx);
        out.push(Entry::Gap);
        let key = ep_bound(false, id, "key");
        out.push(Entry::Fact("key".into(), if self.sis_set(&key) { self.value_text(&key) } else { "none".into() }));
        let budget = self.ep_dollars(false, id, &self.ep_val(false, id, "budget_usd"));
        let managed = matches!(self.ep_val(false, id, "template"), Val::Str(t) if t == "openrouter") && self.sis_set(&ep_bound(false, id, "management_key"));
        out.push(Entry::Fact("budget".into(), if managed && budget != "off" { format!("{budget} · openrouter") } else { budget }));
        let by = self.ep_used_by(id);
        out.push(Entry::Fact("used by".into(), if by.is_empty() { "none".into() } else { by.iter().map(|r| settings::role_name(r)).collect::<Vec<_>>().join(", ") }));
        out.push(Entry::Gap);
        out.push(Entry::Do { act: Act::OpenEndpoint(id.into()), label: "open in Providers".into(), summary: String::new() });
    }

    /// What every token source inherits, the opportunistic default last:
    /// flat at the head of Providers. Fable's threshold and cap are Claude
    /// Code's alone, so they sit on its band and not here (0214 A10).
    const BUDGET_DEFAULTS: &'static [&'static str] = &[
        "budgets.default.session.threshold_pct",
        "budgets.default.weekly.threshold_pct",
        "budgets.default.session.cap_pct",
        "budgets.default.weekly.cap_pct",
        "budgets.default.cash.threshold_usd",
        "budgets.default.cash.cap_usd_month",
        "budgets.default.bar_pct",
        "budgets.default.threshold_action",
        "budgets.default.reserve_pct",
        "opportunistic.enabled",
    ];

    /// Every entry of `page`.
    pub fn page_entries(&self, page: &SPage) -> Vec<Entry> {
        let mut out = Vec::new();
        let none = Ctx::default();
        match page {
            // TUI (msg 235: "one for the TUI"): how this face looks and
            // behaves, then when the cauldron interrupts and speaks.
            SPage::Tab(CfgTab::Tui) => {
                Self::sets(&mut out, &["ui.theme", "ui.default_tab", "ui.aggregate_default", "ui.preserve.config", "ui.preserve.status"], &none);
                out.push(self.sub(SPage::Attention, "Attention & voice", format!("{} · voice {}", self.shown("attention.autodecide", none.clone()), self.shown("notify.tts.enabled", none.clone()))));
            }
            // DEVICES (msg 235: "one for the machine, or we'll call it
            // devices"): the machine, what it keeps, the mesh.
            SPage::Tab(CfgTab::Devices) => {
                out.push(self.sub(SPage::Machine, "This machine", format!("RAM {} · disk {}", self.shown("machine.ram_max_gb", none.clone()), self.shown("machine.disk.cap_gb", none.clone()))));
                out.push(self.sub(SPage::Storage, "Storage & retention", format!("threads {} · GC every {}", self.shown("storage.threads.retention_age", none.clone()), self.shown("storage.gc.interval", none.clone()))));
                // What it holds, not a reading (0220 E8: how many services
                // run is the Mesh's, on Status).
                out.push(self.sub(SPage::Mesh, "Mesh & services", "supervisor · threads daemon · devices".to_string()));
                // The repo registry is every cauldron's (msg 235): a
                // cauldron's clusters claim from it.
                out.push(self.sub(SPage::Repos, "Repos", count(self.repos.repos.len(), "repo", "repos")));
            }
            // PROVIDERS (msg 235: "One for, like, accounts or providers.
            // We'll call it providers"): the defaults every source inherits,
            // FLAT at its head (msg 235: "I don't even really like the idea
            // of having like the defaults all be collapsed"; task 0209 D4 —
            // no Defaults band), then each token source a band, then the
            // inference endpoints (design 0199 §1).
            SPage::Tab(CfgTab::Providers) => {
                Self::sets(&mut out, Self::BUDGET_DEFAULTS, &none);
                out.push(self.sub(SPage::Endpoints, "Inference endpoints", self.endpoints_summary(false)));
            }
            // ACCOUNTS (V1.1, release plan R-V1: the global bar's
            // "accounts"): each token source's band — its accounts' table and
            // their modals — moved here from Providers as it was, not
            // rebuilt (V1.6 makes it real).
            SPage::Tab(CfgTab::Accounts) => {
                for p in self.pool_ids() {
                    out.push(self.sub(SPage::Pool(p.clone()), &pool_label(&p), self.pool_summary(&p)));
                }
            }
            // PERMISSIONS: what an agent may touch, and who the operator is.
            SPage::Tab(CfgTab::Permissions) => {
                Self::sets(
                    &mut out,
                    &["permissions.mode", "permissions.allowed_tools", "permissions.disallowed_tools", "permissions.add_dirs", "permissions.plugin_roots", "permissions.plugin_dirs", "permissions.repos"],
                    &none,
                );
                out.push(self.sub(SPage::Identity, "Identity", format!("operator {}", self.shown("identity.operator", none.clone()))));
            }
            // THIS CAULDRON (msg 235: "there's settings for this cauldron,
            // and that's what this was supposed to be"): its switch first,
            // whether it takes opportunistic accounts, what it takes of the
            // global sources, then what is its own.
            SPage::Tab(CfgTab::Cauldron) => {
                out.push(Entry::Set(b("cauldron.enabled")));
                out.push(Entry::Set(b("cauldron.opportunistic")));
                // The sidebar's width is this cauldron's own (its layout.json),
                // so it is set here and not on the global TUI tab (0214 A8).
                out.push(Entry::Set(b("ui.sidebar_ratio")));
                let used: Vec<(String, AcctView)> = USE_SOURCES.iter().flat_map(|p| self.accounts(p).into_iter().map(move |a| (p.to_string(), a))).collect();
                let in_use = used.iter().filter(|(p, a)| self.account_allowed(p, &a.id) && self.account_on(p, &a.id)).count();
                out.push(self.sub(SPage::Use, "Accounts", format!("{in_use} of {} in use here", count(used.len(), "account", "accounts"))));
                out.push(self.sub(SPage::UseEndpoints, "Inference endpoints", self.endpoints_summary(true)));
                out.push(self.sub(SPage::Paths, "Directories", count(self.file_roots().len(), "root", "roots")));
                let local = if self.val("local.enabled", none.clone()).as_bool() { format!("on · {}", self.shown("local.engine", none.clone())) } else { "off".into() };
                out.push(self.sub(SPage::Local, "Local inference", local));
                out.push(self.sub(SPage::Roles, "Roles → models", format!("{} · coordinator {}", count(ROLES.len(), "role", "roles"), self.shown("roles.{role}.model", Ctx::role("coordinator")))));
            }
            SPage::Paths => {
                // Every root the cauldron's files live under: `i` quotes its
                // path; a cluster's Nodes tab opens them.
                let roots = self.file_roots();
                out.push(Entry::Head("Roots".into(), count(roots.len(), "root", "roots")));
                for (path, what) in roots {
                    out.push(Entry::Ref { tag: Tag::thing("path", &tilde(&path)), label: tilde(&path), summary: what });
                }
                // V1.8: where a human action's `route: env` value is written.
                out.push(Entry::Set(b("secrets.env_file")));
            }
            // Which global accounts this cauldron uses: a table per source
            // (msg 235: "I want you to have a collapsible header for Claude
            // and then have a table inside of it").
            SPage::Use => {
                for p in USE_SOURCES {
                    let accts = self.accounts(p);
                    if accts.is_empty() {
                        continue;
                    }
                    let allowed = accts.iter().filter(|a| self.account_allowed(p, &a.id)).count();
                    let in_use = accts.iter().filter(|a| self.account_allowed(p, &a.id) && self.account_on(p, &a.id)).count();
                    out.push(Entry::Head(pool_label(p), format!("{} · {allowed} allowed here · {in_use} in use", count(accts.len(), "account", "accounts"))));
                    for a in accts {
                        out.push(Entry::Row(SPage::UseAccount(p.to_string(), a.id)));
                    }
                }
            }
            SPage::UseAccount(p, a) => {
                Self::sets(&mut out, &["cauldron.accounts.{pool}.{account}.allowed", "cauldron.accounts.{pool}.{account}.weekly_threshold_pct", "cauldron.accounts.{pool}.{account}.opportunistic"], &Ctx::account(p, a));
            }
            // The global registry (design 0199 §1): the table, `+ add
            // endpoint`, then — flattened — what a new endpoint inherits.
            SPage::Endpoints => {
                for id in self.settings.endpoint_ids(false) {
                    out.push(Entry::Row(SPage::Endpoint { here: false, id }));
                }
                out.push(Entry::Do { act: Act::NewEndpoint { here: false }, label: "+ add endpoint".into(), summary: String::new() });
                out.push(Entry::Gap);
                Self::sets(&mut out, &["endpoints.defaults.budget_period", "endpoints.defaults.threshold_pct", "endpoints.defaults.threshold_action", "endpoints.defaults.max_concurrent"], &none);
            }
            // What this cauldron can see (§5): every global endpoint, then
            // its own; `+ add endpoint` here registers one only it sees.
            SPage::UseEndpoints => {
                for (here, id) in self.seen_endpoints() {
                    out.push(Entry::Row(if here { SPage::Endpoint { here, id } } else { SPage::UseEndpoint(id) }));
                }
                out.push(Entry::Do { act: Act::NewEndpoint { here: true }, label: "+ add endpoint".into(), summary: String::new() });
            }
            SPage::Endpoint { here, id } => self.endpoint_entries(*here, id, &mut out),
            SPage::UseEndpoint(id) => self.use_endpoint_entries(id, &mut out),
            // A subscription source (msg 235: "first just show me a table of
            // all of my accounts, and then just show me a flattened list of
            // all of the settings ... And then the exception being models"):
            // its accounts' table, its settings, its detected models.
            SPage::Pool(p) => {
                let ctx = Ctx::pool(p);
                // V1.6: until the daemon's pool is in, say why there is no
                // table — never an empty one that reads as no accounts.
                if p == CLAUDE_CODE {
                    if let crate::cauldron::accounts::Roster::Waiting(why) = self.roster() {
                        out.push(Entry::Fact("accounts".into(), why));
                    }
                }
                for a in self.accounts(p) {
                    out.push(Entry::Row(SPage::Account(p.clone(), a.id)));
                }
                out.push(Entry::Add(Add::Account(p.clone())));
                Self::sets(&mut out, &self.for_pool(p, &POOL_SETTINGS), &ctx);
                let models = self.detected_models(p);
                if !models.is_empty() {
                    let on = models.iter().filter(|m| self.val("pools.{pool}.model.{model}", Ctx::model(p, m)).as_bool()).count();
                    out.push(Entry::Fixed("Models".into(), format!("{on} of {} on", models.len())));
                    for m in models {
                        out.push(Entry::Set(Bound::new("pools.{pool}.model.{model}", Ctx::model(p, &m))));
                    }
                }
            }
            // One account, in its modal: every setting it has, then rename
            // and remove.
            SPage::Account(p, a) => {
                let ctx = Ctx::account(p, a);
                let key = (p == CLAUDE_CODE).then(|| self.accounts(p).into_iter().find(|x| x.id == *a).and_then(|x| x.key)).flatten();
                Self::sets(
                    &mut out,
                    &self.for_pool(p, &[
                        "pools.{pool}.accounts.{account}.enabled",
                        "pools.{pool}.accounts.{account}.budget.session.threshold_pct",
                        "pools.{pool}.accounts.{account}.budget.weekly.threshold_pct",
                        "pools.{pool}.accounts.{account}.budget.fable.threshold_pct",
                        "pools.{pool}.accounts.{account}.budget.session.cap_pct",
                        "pools.{pool}.accounts.{account}.budget.weekly.cap_pct",
                        "pools.{pool}.accounts.{account}.budget.fable.cap_pct",
                        "pools.{pool}.accounts.{account}.budget.cash.threshold_usd",
                        "pools.{pool}.accounts.{account}.budget.bar_pct",
                        "pools.{pool}.accounts.{account}.share",
                        "pools.{pool}.accounts.{account}.max_concurrent",
                        "pools.{pool}.accounts.{account}.opportunistic.enabled",
                        "pools.{pool}.accounts.{account}.live_login",
                        "pools.{pool}.accounts.{account}.note",
                    ]),
                    &ctx,
                );
                out.push(Entry::Gap);
                // V1.6: one of the daemon's — its pool switch and its login,
                // each the daemon's, under its settings here.
                if let Some(key) = &key {
                    out.push(Entry::Fact("in the pool".into(), self.in_pool_words(key)));
                    out.push(Entry::Fact("login".into(), self.login_words(key)));
                    if let Some(on) = self.in_pool(key) {
                        let label = if on { "take out of the pool" } else { "put back in the pool" };
                        out.push(Entry::Do { act: Act::InPool { key: key.clone(), on: !on }, label: label.into(), summary: String::new() });
                    }
                    out.push(Entry::Do { act: Act::LogIn { key: key.clone() }, label: "log in".into(), summary: String::new() });
                    out.push(Entry::Gap);
                }
                out.push(Entry::Do { act: Act::RenameAccount { pool: p.clone(), id: a.clone() }, label: "rename".into(), summary: String::new() });
                // The daemon's roster is not removed from here (its delete is
                // out of V1.6): only a seed pool's account is.
                if key.is_none() {
                    out.push(Entry::Do { act: Act::RemoveAccount { pool: p.clone(), id: a.clone() }, label: "remove".into(), summary: String::new() });
                }
            }
            SPage::Local => Self::sets(
                &mut out,
                &["local.enabled", "local.engine", "local.models", "local.max_concurrent", "local.ram_max_gb", "local.disk_max_gb", "local.gpu"],
                &none,
            ),
            SPage::Roles => {
                for g in ROLE_GROUPS {
                    let roles: Vec<&Role> = ROLES.iter().filter(|r| r.group == g).collect();
                    let here = roles.iter().filter(|r| ["roles.{role}.model", "roles.{role}.fallback", "roles.{role}.pool"].iter().any(|k| self.sis_set(&Bound::new(k, Ctx::role(r.id))))).count();
                    let mut meta = count(roles.len(), "role", "roles");
                    if here > 0 {
                        meta.push_str(&format!(" · {here} changed"));
                    }
                    out.push(Entry::Head(crate::cauldron::look::sentence(g), meta));
                    for r in roles {
                        out.push(Entry::Row(SPage::Role(r.id.into())));
                    }
                }
            }
            SPage::Role(r) => Self::sets(&mut out, &["roles.{role}.model", "roles.{role}.fallback", "roles.{role}.pool", "roles.{role}.endpoint"], &Ctx::role(r)),
            SPage::Machine => {
                Self::sets(
                    &mut out,
                    &[
                        "machine.ram_max_gb",
                        "machine.ram_reserve_gb",
                        "machine.cpu_max_pct",
                        "machine.max_child_processes",
                        "machine.gpu.enabled",
                        "machine.gpu.memory_max_gb",
                        "machine.disk.cap_gb",
                        "machine.disk.free_floor_gb",
                        "machine.on_battery",
                    ],
                    &none,
                );
            }
            SPage::Storage => {
                // Store-wide: a sibling of the stores' heads, under none of
                // them (0211 R8).
                out.push(self.sub(SPage::Gc, "Garbage collection", format!("every {}", self.shown("storage.gc.interval", none.clone()))));
                out.push(Entry::Head("Threads".into(), format!("retention {} · cap {}", self.shown("storage.threads.retention_age", none.clone()), self.shown("storage.threads.cap_gb", none.clone()))));
                Self::sets(
                    &mut out,
                    &[
                        "storage.threads.retention_age",
                        "storage.threads.cap_gb",
                        "storage.threads.eviction_order",
                        "storage.threads.protect_pinned",
                        "storage.threads.protect_active_clusters",
                        "storage.threads.archive_before_delete",
                    ],
                    &none,
                );
                out.push(Entry::Head("Logs".into(), format!("retention {} · cap {}", self.shown("storage.logs.retention_age", none.clone()), self.shown("storage.logs.cap_mb", none.clone()))));
                Self::sets(&mut out, &["storage.logs.retention_age", "storage.logs.cap_mb", "storage.supervisions.cap_mb"], &none);
                out.push(Entry::Fact("warnings cap".into(), format!("{} MB", shapewire::ledger::LEDGER_MAX_BYTES / (1024 * 1024))));
                const OTHER: [&str; 5] = ["storage.usage_observations.retention_age", "storage.account_backups.keep", "storage.tts.cap_gb", "storage.tui.dumps_cap_mb", "storage.pins.good_history"];
                out.push(Entry::Head("Other stores".into(), count(OTHER.len(), "setting", "settings")));
                Self::sets(&mut out, &OTHER, &none);
                // The root of every cauldron and the weights path: the
                // machine's, not one cauldron's (task 0209 D8).
                out.push(Entry::Head("Directories".into(), format!("root {}", tilde(&self.shown("paths.root", none.clone())))));
                Self::sets(&mut out, &["paths.root", "paths.weights"], &none);
            }
            SPage::Gc => {
                Self::sets(&mut out, &["storage.gc.interval", "storage.gc.dry_run", "storage.gc.on_disk_pressure"], &none);
                // What a pass would take is in `run now`'s confirm, not a
                // live reading here (0217 B7: no status data in Config, C6).
                out.push(Entry::Run(b("storage.gc.run_now")));
            }
            SPage::Identity => Self::sets(&mut out, &["identity.socket_secret_required", "identity.live_login", "identity.operator"], &none),
            SPage::Attention => {
                Self::sets(&mut out, &["attention.autodecide", "attention.interrupt_floor"], &none);
                out.push(Entry::Head("Voice".into(), format!("speech {}", self.shown("notify.tts.enabled", none.clone()))));
                Self::sets(&mut out, &["notify.tts.enabled", "notify.tts.voice", "notify.tts.rate"], &none);
                const ALARMS: [&str; 4] = ["notify.limits.threshold_crossed", "notify.limits.login_expiry_lead", "notify.gc.report", "notify.quiet_hours"];
                out.push(Entry::Head("Alarms".into(), count(ALARMS.len(), "setting", "settings")));
                Self::sets(&mut out, &ALARMS, &none);
            }
            SPage::Mesh => {
                out.push(self.sub(SPage::Supervisor, "Supervisor", format!("auto restart {}", self.shown("supervisor.auto_restart", none.clone()))));
                out.push(self.sub(SPage::Daemon, "Threads daemon", format!("{}:{}", self.shown("threads.host", none.clone()), self.shown("threads.chassis_port", none.clone()))));
                out.push(self.sub(SPage::Devices, "Devices", self.shown("mesh.devices", none.clone())));
            }
            SPage::Supervisor => Self::sets(
                &mut out,
                &[
                    "supervisor.auto_restart",
                    "supervisor.auto_rollback",
                    "supervisor.max_start_failures",
                    "supervisor.poll_secs",
                    "supervisor.down_after",
                    "supervisor.throttle_secs",
                    "supervisor.backoff_base_secs",
                    "supervisor.backoff_max_secs",
                    "supervisor.stable_after_secs",
                ],
                &none,
            ),
            SPage::Daemon => {
                Self::sets(
                    &mut out,
                    &[
                        "threads.probe_interval_secs",
                        "threads.probe_interval_busy_secs",
                        "threads.calibrate",
                        "threads.edge_poll",
                        "threads.settle_ladder_secs",
                        "threads.chassis_port",
                        "threads.host",
                        "threads.claude_bin",
                    ],
                    &none,
                );
            }
            SPage::Devices => Self::sets(&mut out, &["mesh.devices", "mesh.dispatch_remote", "mesh.accept_remote"], &none),
            SPage::Repos => {
                for repo in &self.repos.repos {
                    out.push(Entry::Sub { page: SPage::Repo(repo.name.clone()), label: repo.name.clone(), summary: tilde(&repo.path) });
                }
                out.push(Entry::Do { act: Act::RegisterRepo, label: "+ register repo".into(), summary: String::new() });
                out.push(Entry::Gap);
                let roots = Bound::new("repos.trace_roots", none.clone());
                out.push(Entry::Head("Trace roots".into(), count(self.sget(&roots).val.as_list().len(), "root", "roots")));
                out.push(Entry::Add(Add::Item(roots.clone())));
                for (index, text) in self.sget(&roots).val.as_list().iter().enumerate() {
                    out.push(Entry::Item { list: roots.clone(), index, text: text.clone() });
                }
            }
            SPage::Repo(name) => self.repo_entries(name, &mut out),
            SPage::Region(id) => self.region_entries(id, &mut out),
            SPage::Node(id) => self.node_entries(id, &mut out),
            SPage::List(bd) => {
                out.push(Entry::Add(Add::Item(bd.clone())));
                let v = self.sget(bd).val;
                for (index, text) in v.as_list().iter().enumerate() {
                    out.push(Entry::Item { list: bd.clone(), index, text: text.clone() });
                }
            }
            // A section: the entries of its page after its head, up to the next.
            SPage::Section(p, t) => {
                let all = self.page_entries(p);
                let from = all.iter().position(|e| matches!(e, Entry::Head(h, _) | Entry::Fixed(h, _) if h == t)).map_or(all.len(), |i| i + 1);
                out.extend(all[from..].iter().take_while(|e| !matches!(e, Entry::Head(..) | Entry::Fixed(..))).cloned());
                while out.last() == Some(&Entry::Gap) {
                    out.pop();
                }
            }
        }
        out
    }

    /// A registered repo's page (design 0071 S7): where it is, and
    /// `unregister`. What it holds and who claims it are the soup's and
    /// Status's to say, never Config's (msg 235: "that would be stuff that's
    /// in status ... It does not belong in config").
    fn repo_entries(&self, name: &str, out: &mut Vec<Entry>) {
        let Some(r) = self.repos.find_repo(name) else {
            out.push(Entry::Fact("repo".into(), format!("`{name}` is no longer registered")));
            return;
        };
        let repo = &self.repos.repos[r];
        out.push(Entry::Ref { tag: Tag::thing("repo", &repo.name), label: repo.name.clone(), summary: tilde(&repo.path) });
        let claimed = !self.repos.claimants[r].is_empty();
        // A claimed repo shows no button: one would promise an act it
        // refuses (0217 B8). It says why, read-only.
        if claimed {
            out.push(Entry::Fact("unregister".into(), "not while a cluster claims it — let it go there first".into()));
        } else {
            out.push(Entry::Do { act: Act::UnregisterRepo(repo.name.clone()), label: "unregister".into(), summary: String::new() });
        }
    }

    /// A region's page on Settings (an unclaimed region is reached from
    /// nowhere else): its boundary, then its nodes by kind, each opening to
    /// its body — the Soup tab's drill, in the menus' grammar.
    fn region_entries(&self, id: &str, out: &mut Vec<Entry>) {
        let c = &self.corpus;
        let Some(r) = c.find_region(id) else {
            out.push(Entry::Fact("region".into(), format!("`{id}` is not in the soup")));
            return;
        };
        let reg = &c.regions[r];
        out.push(Entry::Ref { tag: Tag::thing("region", &reg.id), label: reg.id.clone(), summary: reg.handle.clone() });
        out.push(Entry::Para(reg.boundary.clone()));
        let up: Vec<&str> = reg.narrows.iter().map(|&x| c.regions[x].id.as_str()).collect();
        if !up.is_empty() {
            out.push(Entry::Fact("narrows".into(), up.join(" ")));
        }
        let n = c.claimants[r].len();
        out.push(Entry::Fact("claimed by".into(), if n == 0 { "no cluster".into() } else { c.claimants[r].iter().map(|&g| self.forest.path(g)).collect::<Vec<_>>().join(" · ") }));
        let mut kinds: Vec<&str> = Vec::new();
        for &m in &c.members[r] {
            if !kinds.contains(&c.nodes[m].kind.as_str()) {
                kinds.push(&c.nodes[m].kind);
            }
        }
        for k in kinds {
            let ms: Vec<usize> = c.members[r].iter().copied().filter(|&m| c.nodes[m].kind == k).collect();
            out.push(Entry::Head(crate::cauldron::look::sentence(k), ms.len().to_string()));
            for m in ms {
                let node = &c.nodes[m];
                out.push(Entry::Sub { page: SPage::Node(node.id.clone()), label: node.id.clone(), summary: format!("{}  {}", node.state, node.handle) });
            }
        }
    }

    /// A soup node's page on Settings: its line, where it sits, its body.
    fn node_entries(&self, id: &str, out: &mut Vec<Entry>) {
        let c = &self.corpus;
        let Some(n) = c.find_node(id) else {
            out.push(Entry::Fact("node".into(), format!("`{id}` is not in the soup")));
            return;
        };
        let node = &c.nodes[n];
        out.push(Entry::Ref { tag: Tag::thing("node", &node.id), label: node.id.clone(), summary: format!("{} · {}", node.kind, node.state) });
        out.push(Entry::Para(node.handle.clone()));
        let regions: Vec<String> = node.in_region.iter().map(|&r| c.regions[r].id.clone()).collect();
        let mut parts: Vec<String> = node.cite.iter().cloned().collect();
        parts.push(regions.join(" "));
        out.push(Entry::Fact("↑".into(), parts.join(" · ")));
        out.push(Entry::Gap);
        out.push(Entry::Para(node.body.clone()));
    }

    // ── acting on a row ────────────────────────────────────────────────────

    /// The selected entry of a settings tab.
    pub fn settings_entry(&self) -> Option<Entry> {
        self.selectable().into_iter().nth(self.settings_sel())
    }

    /// Open band `page` of a Config tab (its header stays where it is).
    fn expand(&mut self, page: SPage) {
        self.nav_mut().set_open(page, true);
    }

    /// Select the header of band `page` on a Config tab, if it is shown.
    fn select_band(&mut self, page: &SPage) -> bool {
        let at = self.selectable().iter().position(|e| e.band().as_ref() == Some(page));
        if let Some(i) = at {
            self.set_settings_sel(i);
        }
        at.is_some()
    }

    /// Open account `id` of `pool`: Accounts › its source open, its row
    /// selected and its modal open.
    pub fn open_account_settings(&mut self, pool: &str, id: &str) {
        self.set_cfg_tab(CfgTab::Accounts);
        self.field = None;
        self.expand(SPage::Pool(pool.into()));
        let page = SPage::Account(pool.into(), id.into());
        if let Some(i) = self.selectable().iter().position(|e| *e == Entry::Row(page.clone())) {
            self.set_settings_sel(i);
        }
        if self.focus != Focus::Input {
            self.focus = Focus::Content;
        }
        self.open_row(page);
    }

    /// Open endpoint `id` on Config (Status's `config ▸`): a global one's
    /// row on Providers, one of this cauldron's own on its tab — selected,
    /// its modal open.
    pub fn open_endpoint_settings(&mut self, here: bool, id: &str) {
        self.set_cfg_tab(if here { CfgTab::Cauldron } else { CfgTab::Providers });
        self.field = None;
        self.expand(if here { SPage::UseEndpoints } else { SPage::Endpoints });
        let page = SPage::Endpoint { here, id: id.into() };
        if let Some(i) = self.selectable().iter().position(|e| *e == Entry::Row(page.clone())) {
            self.set_settings_sel(i);
        }
        if self.focus != Focus::Input {
            self.focus = Focus::Content;
        }
        self.open_row(page);
    }

    /// Open the MODAL of a table's row (msg 235: "when you click on the row,
    /// you go to modal with all the settings"), its first setting selected.
    pub fn open_row(&mut self, page: SPage) {
        self.field = None;
        self.modal = Some(Modal::Settings { page, sel: 0 });
    }

    /// What a row's modal is titled: the row, and whose it is.
    pub fn row_title(&self, page: &SPage) -> String {
        let name = self.row_label(page);
        match page {
            SPage::Account(p, _) => format!("{} · {name}", pool_label(p)),
            SPage::UseAccount(..) => format!("{name} · {}", self.cfg_label(CfgTab::Cauldron)),
            SPage::Role(_) => format!("Role · {name}"),
            // Whose it is: global, or this cauldron's (design 0199 §2, §6).
            SPage::Endpoint { here, id } if id == DRAFT_EP => format!("New endpoint · {}", self.ep_scope(*here)),
            SPage::Endpoint { here, .. } => format!("Endpoint · {name} · {}", self.ep_scope(*here)),
            SPage::UseEndpoint(_) => format!("Endpoint · {name} · {} · global", self.cfg_label(CfgTab::Cauldron)),
            _ => name,
        }
    }

    /// Where an endpoint is registered, in words: `global` or `prod
    /// cauldron`.
    pub fn ep_scope(&self, here: bool) -> String {
        if here {
            self.cfg_label(CfgTab::Cauldron)
        } else {
            "global".into()
        }
    }

    /// Whether row page `page` still stands (an account removed from its
    /// modal does not).
    pub fn row_stands(&self, page: &SPage) -> bool {
        match page {
            SPage::Account(p, a) | SPage::UseAccount(p, a) => self.accounts(p).iter().any(|x| x.id == *a),
            // The endpoint being added stands while it is being added.
            SPage::Endpoint { here, id } if id == DRAFT_EP => self.sis_set(&ep_bound(*here, id, "template")),
            SPage::Endpoint { here, id } => self.settings.endpoint_ids(*here).contains(id),
            SPage::UseEndpoint(id) => self.settings.endpoint_ids(false).contains(id),
            _ => true,
        }
    }

    /// The selectable entries of row modal `page`, in order.
    pub fn modal_selectable(&self, page: &SPage) -> Vec<Entry> {
        self.modal_entries(page).into_iter().map(|f| f.entry).filter(Entry::selectable).collect()
    }

    /// Option `opt` of switch `cell` on entry `e` picked: a setting's own
    /// switch is cell 0; a table row's switches are counted along it.
    pub(crate) fn pick_cell(&mut self, e: Entry, cell: usize, opt: usize) {
        let bd = match e {
            Entry::Set(bd) if cell == 0 => Some(bd),
            Entry::Row(page) => self
                .row_cells(&page)
                .into_iter()
                .filter_map(|c| match c {
                    Cell::Toggle(bd) => Some(bd),
                    Cell::Text(_) => None,
                })
                .nth(cell),
            _ => None,
        };
        if let Some(bd) = bd {
            self.refusal = None;
            self.set_option(&bd, opt);
        }
    }

    /// The row modal's selected entry.
    pub fn modal_entry(&self) -> Option<Entry> {
        let Some(Modal::Settings { page, sel }) = &self.modal else { return None };
        self.modal_selectable(page).into_iter().nth(*sel)
    }

    /// Keys while a row's modal is open: ↑ ↓ move, Space / Enter / → act on
    /// the selected setting as they would on Config, `x` reverts it, `+ -`
    /// nudge it, `h` explains it, Esc or ← closes the modal. A value being
    /// typed takes the keys first.
    pub(crate) fn row_modal_key(&mut self, key: ratatui::crossterm::event::KeyEvent) {
        use ratatui::crossterm::event::{KeyCode as K, KeyModifiers};
        if self.field.is_some() {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            return self.field_key(key, ctrl);
        }
        self.refusal = None;
        if !matches!(key.code, K::Char('x') | K::Backspace | K::Delete) {
            self.pending_remove = None;
        }
        let Some(Modal::Settings { page, sel }) = self.modal.clone() else { return };
        let n = self.modal_selectable(&page).len();
        let set_sel = |app: &mut App, i: usize| {
            if let Some(Modal::Settings { sel, .. }) = &mut app.modal {
                *sel = i;
            }
        };
        match key.code {
            K::Esc | K::Left | K::Char('q') => self.modal = None,
            K::Up => set_sel(self, sel.saturating_sub(1)),
            K::Down => set_sel(self, (sel + 1).min(n.saturating_sub(1))),
            K::Home => set_sel(self, 0),
            K::End => set_sel(self, n.saturating_sub(1)),
            K::Enter | K::Right => {
                if let Some(e) = self.modal_entry() {
                    self.open_entry(e);
                }
            }
            K::Char(' ') => {
                if let Some(e) = self.modal_entry() {
                    self.space_entry(e);
                }
            }
            K::Char('h') => {
                if let Some(e) = self.modal_entry() {
                    self.help_entry(&e);
                }
            }
            K::Char('x') | K::Backspace | K::Delete => {
                if let Some(e) = self.modal_entry() {
                    self.clear_entry(e);
                }
            }
            K::Char('+' | '=') => {
                if let Some(e) = self.modal_entry() {
                    self.nudge_entry(e, true);
                }
            }
            K::Char('-' | '_') => {
                if let Some(e) = self.modal_entry() {
                    self.nudge_entry(e, false);
                }
            }
            _ => {}
        }
    }

    /// ← on a Config tab: an open band closes; a line inside a band climbs to
    /// that band's header. False on a closed band at the top (← leaves the
    /// tab's rows there: to the sidebar, on a cluster).
    pub fn settings_back(&mut self) -> bool {
        self.field = None;
        let Some(f) = self.selectable_flat().into_iter().nth(self.settings_sel()) else { return false };
        if let (Some(true), Some(band)) = (f.open, f.entry.band()) {
            self.nav_mut().set_open(band, false);
            return true;
        }
        match f.parent {
            Some(p) => self.select_band(&p),
            None => false,
        }
    }

    /// Whether Config entry `e` belongs to an inference source that is not
    /// live ([`settings::inference_live`], R-V1 item 7): drawn greyed, still
    /// selectable and read, and every act on it refused with a notice. A
    /// greyed pool's band and the endpoints' bands open, and an endpoint's
    /// row opens its modal, for reading.
    pub fn entry_greyed(&self, e: &Entry) -> bool {
        let live = settings::inference_live;
        match e {
            Entry::Sub { page, .. } | Entry::Row(page) => match page {
                SPage::Pool(p) | SPage::Account(p, _) | SPage::UseAccount(p, _) => !live(p),
                SPage::Endpoints | SPage::UseEndpoints | SPage::Endpoint { .. } | SPage::UseEndpoint(_) => !live("endpoints"),
                SPage::Local => !live("local"),
                _ => false,
            },
            Entry::Set(bd) | Entry::Run(bd) => settings::greyed_key(bd.key, &bd.ctx),
            Entry::Item { list, .. } | Entry::Add(Add::Item(list)) => settings::greyed_key(list.key, &list.ctx),
            Entry::Add(Add::Account(p)) => !live(p),
            Entry::Do { act, .. } => match act {
                Act::NewEndpoint { .. } | Act::AddEndpoint { .. } | Act::RemoveEndpoint { .. } => !live("endpoints"),
                Act::RenameAccount { pool, .. } | Act::RemoveAccount { pool, .. } => !live(pool),
                _ => false,
            },
            _ => false,
        }
    }

    /// Refuse an act on greyed entry `e`, saying so: true when refused.
    fn refuse_greyed(&mut self, e: &Entry) -> bool {
        // Opening a band or a row's modal is reading, never refused.
        if matches!(e, Entry::Sub { .. } | Entry::Row(_)) || !self.entry_greyed(e) {
            return false;
        }
        self.notice = Some(settings::GREYED.into());
        true
    }

    /// → / Enter: open a band (on an open one, step to its first line),
    /// flip a switch, pick from a list, or start typing into a value. False
    /// when the row does nothing.
    pub fn settings_open(&mut self) -> bool {
        let Some(e) = self.settings_entry() else { return false };
        self.open_entry(e)
    }

    /// Whether `e` is a VALUE row — a picker, a number, a text, a secret —
    /// which opens its picker or editor at ONE click (0217 B4, D18: a value
    /// is a control, not a row to select first).
    pub fn is_value_row(&self, e: &Entry) -> bool {
        match e {
            Entry::Set(bd) if self.strip_words(bd).is_none() => !matches!(bd.def().ty, Ty::Toggle | Ty::LiveLogin | Ty::List | Ty::Action),
            _ => false,
        }
    }

    /// Whether `e` is a SWITCH — its options are the control, so a click on
    /// its label only selects it, and never cycles it unseen (0217 B17).
    pub fn is_switch(&self, e: &Entry) -> bool {
        match e {
            Entry::Set(bd) => self.strip_words(bd).is_some() || matches!(bd.def().ty, Ty::Toggle | Ty::LiveLogin),
            _ => false,
        }
    }

    /// → / Enter on settings entry `e` (wherever it is shown).
    pub fn open_entry(&mut self, e: Entry) -> bool {
        if let Some(band) = e.band() {
            if self.nav().is_open(&band) {
                // Already open: on to the first line it holds.
                let sel = self.settings_sel();
                let n = self.selectable().len();
                if sel + 1 < n {
                    self.set_settings_sel(sel + 1);
                }
            } else {
                self.expand(band);
            }
            return true;
        }
        if self.refuse_greyed(&e) {
            return true;
        }
        match e {
            Entry::Sub { .. } => {}
            Entry::Row(page) => self.open_row(page),
            // A strip steps to its next option, as a switch does.
            Entry::Set(bd) if self.strip_words(&bd).is_some() => self.flip(&bd),
            Entry::Set(bd) => match bd.def().ty {
                Ty::Toggle | Ty::LiveLogin => self.flip(&bd),
                Ty::List => {}
                Ty::Choice(_) | Ty::Model { .. } | Ty::PoolRef { .. } | Ty::AccountRef | Ty::EndpointRef => self.open_picker(bd),
                // A secret is pasted afresh: what it holds is never put back
                // in front of him, even masked.
                Ty::Secret => self.field = Some(Field { bound: bd, text: String::new(), error: None }),
                Ty::Action => self.run(&bd),
                _ => {
                    let v = self.sget(&bd);
                    let text = match (&v.prov, &v.val) {
                        (_, Val::Int(n)) => n.to_string(),
                        (_, Val::Off) => "off".into(),
                        (_, Val::Str(s)) => s.clone(),
                        _ => String::new(),
                    };
                    self.field = Some(Field { bound: bd, text, error: None });
                }
            },
            Entry::Add(Add::Account(pool)) => self.modal = Some(Modal::AddAccount { pool, name: String::new(), error: None }),
            // A cauldron's narrowed list takes back only the key's own models.
            Entry::Add(Add::Item(list)) if list.key == "cauldron.use.{ep}.model_list" => {
                let have = self.sget(&list).val.as_list().to_vec();
                let options: Vec<String> = self.ep_models(false, &list.ctx.ep).into_iter().filter(|m| !have.contains(m)).collect();
                if options.is_empty() {
                    self.notice = Some("every model of the key is on the list".into());
                } else {
                    self.modal = Some(Modal::Picker { for_: PickFor::AddItem { list }, title: "Put back a model".into(), options, selected: 0 });
                }
            }
            Entry::Add(Add::Item(list)) => self.modal = Some(Modal::AddItem { list, name: String::new(), error: None }),
            Entry::Run(bd) => self.run(&bd),
            Entry::Do { act, .. } => self.start_act(act),
            Entry::Item { .. } | Entry::Head(..) | Entry::Fixed(..) | Entry::Fact(..) | Entry::Gap | Entry::Para(_) | Entry::Ref { .. } => return false,
        }
        true
    }

    /// `h` (or a click on its ⓘ): what the selected setting means, in a
    /// modal — the only place the explanation is.
    pub fn settings_help(&mut self) {
        if let Some(e) = self.settings_entry() {
            self.help_entry(&e);
        }
    }

    /// The help modal for any Config row (its ⓘ, V7).
    pub fn help_entry(&mut self, e: &Entry) {
        if let Some((title, text)) = self.entry_help(e) {
            self.modal = Some(Modal::Help { title, text });
        }
    }

    /// The help modal for setting `bd`.
    pub fn open_help(&mut self, bd: &Bound) {
        let d = bd.def();
        self.modal = Some(Modal::Help { title: d.label.to_string(), text: settings::help(d) });
    }

    /// Space: step the switch on the selected row (a switch setting, or a
    /// table row's first switch).
    pub fn settings_space(&mut self) {
        if let Some(e) = self.settings_entry() {
            self.space_entry(e);
        }
    }

    /// Space on settings entry `e`.
    pub fn space_entry(&mut self, e: Entry) {
        // A greyed row's switch is greyed with it.
        if self.entry_greyed(&e) {
            self.notice = Some(settings::GREYED.into());
            return;
        }
        match e {
            // A choice drawn as a switch (`dark · light`, an endpoint's
            // template) steps like any other switch.
            Entry::Set(bd) if matches!(bd.def().ty, Ty::Toggle | Ty::LiveLogin) || self.strip_words(&bd).is_some() => self.flip(&bd),
            Entry::Row(page) => {
                if let Some(Cell::Toggle(bd)) = self.row_cells(&page).into_iter().find(|c| matches!(c, Cell::Toggle(_))) {
                    self.flip(&bd);
                }
            }
            _ => {}
        }
    }

    /// Step a switch to its next option: `on` ⇄ `off`, or `inherit` → `on`
    /// → `off` → `inherit`.
    pub(crate) fn flip(&mut self, bd: &Bound) {
        if bd.def().ty == Ty::LiveLogin {
            let on = self.sget(bd).val.as_bool();
            self.apply(bd, Ok(Val::Bool(!on)));
            return;
        }
        let t = self.toggle_of(bd);
        let mut next = (t.selected + 1) % t.options.len();
        // A global endpoint switched off cannot be `on` here: Space steps
        // past it, inherit → off → inherit.
        if bd.key == "cauldron.use.{ep}.enabled" && next == 1 && !self.ep_val(false, &bd.ctx.ep, "enabled").as_bool() {
            next = 2;
        }
        self.set_option(bd, next);
    }

    /// `x` / Backspace: clear the selected row's override, or remove the
    /// selected item from its list.
    pub fn settings_clear(&mut self) {
        if let Some(e) = self.settings_entry() {
            self.clear_entry(e);
        }
    }

    /// `x` on settings entry `e`.
    pub fn clear_entry(&mut self, e: Entry) {
        if self.entry_greyed(&e) {
            self.notice = Some(settings::GREYED.into());
            return;
        }
        match Some(e) {
            // `x` on an endpoint's row removes it, asked first; a global one
            // in a cauldron's table is removed on Providers (design 0199 §7).
            Some(Entry::Row(SPage::Endpoint { here, id })) => self.start_act(Act::RemoveEndpoint { here, id }),
            Some(Entry::Row(SPage::UseEndpoint(_))) => self.notice = Some("a global endpoint: remove it in Providers".into()),
            // A key is never thrown away by `x`: pasting another replaces it;
            // and an endpoint's template is what it is.
            Some(Entry::Set(bd)) if bd.def().ty == Ty::Secret || bd.key.ends_with(".template") => {}
            Some(Entry::Set(bd)) => {
                let before = self.flags();
                match self.sclear(&bd) {
                    Ok(()) => self.note_flips(before, &format!("{} inherits again", bd.def().label)),
                    Err(e) => self.refusal = Some((bd, e)),
                }
            }
            // An entry he typed goes only at a second `x` on it (0220 E4):
            // the first asks, on its row.
            Some(Entry::Item { list, index, .. }) if self.pending_remove.as_ref() != Some(&(list.concrete(), index)) => {
                self.pending_remove = Some((list.concrete(), index));
            }
            Some(Entry::Item { list, index, .. }) => {
                self.pending_remove = None;
                let mut items = self.sget(&list).val.as_list().to_vec();
                items.remove(index);
                self.apply(&list, Ok(Val::List(items)));
                let n = self.selectable().len();
                let sel = self.settings_sel().min(n.saturating_sub(1));
                self.set_settings_sel(sel);
            }
            _ => {}
        }
    }

    /// `+` / `-` on a number, percent or duration row.
    pub fn settings_nudge(&mut self, up: bool) {
        if let Some(e) = self.settings_entry() {
            self.nudge_entry(e, up);
        }
    }

    /// `+` / `-` on settings entry `e`.
    pub fn nudge_entry(&mut self, e: Entry, up: bool) {
        if self.refuse_greyed(&e) {
            return;
        }
        let Entry::Set(bd) = e else { return };
        let cur = self.sget(&bd).val;
        if let Some(v) = settings::nudge(bd.def().ty, &cur, up) {
            self.apply(&bd, Ok(v));
        }
    }

    /// Set a value, or put the reason it was refused on its row.
    fn apply(&mut self, bd: &Bound, v: Result<Val, String>) -> bool {
        let before = self.flags();
        let shown_before = show(bd.def(), &self.sget(bd).val);
        match v.and_then(|v| self.sset(bd, v)) {
            Ok(()) => {
                self.refusal = None;
                let now = show(bd.def(), &self.sget(bd).val);
                self.note_flips(before, &format!("{} {shown_before} → {now}", bd.def().label));
                true
            }
            Err(e) => {
                self.refusal = Some((bd.clone(), e));
                false
            }
        }
    }

    /// `run now` (the one action row): a garbage-collection pass, asked
    /// first (0214 A2).
    fn run(&mut self, _bd: &Bound) {
        self.start_act(Act::Gc);
    }

    /// The choices a picker offers for a setting.
    pub fn choices(&self, bd: &Bound) -> Vec<String> {
        let mut out: Vec<String> = match bd.def().ty {
            Ty::Choice(opts) => opts.iter().map(|s| s.to_string()).collect(),
            Ty::Model { none } => {
                let mut m: Vec<String> = Vec::new();
                if none {
                    m.push("none".into());
                }
                // A role's models are its pool's (0217 B12: a Claude Code
                // role was offered gpt-5-codex, o3 and qwen3). `auto`, or
                // a setting no role holds, offers every source's.
                let pool = if bd.key.starts_with("roles.") { match self.val("roles.{role}.pool", bd.ctx.clone()) { Val::Str(p) => Some(p), _ => None } } else { None };
                let only = |src: &str| pool.as_deref().is_none_or(|p| p == "auto" || p == src);
                for p in self.pool_ids().into_iter().filter(|p| only(p)) {
                    for x in self.pool_models(&p) {
                        if !m.contains(&x) {
                            m.push(x);
                        }
                    }
                }
                for (here, id) in self.seen_endpoints().into_iter().filter(|(h, id)| self.ep_used_here(*h, id)).filter(|_| pool.as_deref().is_none_or(|p| p == "auto")) {
                    for x in self.ep_models_here(here, &id) {
                        if !m.contains(&x) {
                            m.push(x);
                        }
                    }
                }
                if self.val("local.enabled", Ctx::default()).as_bool() && only("local") {
                    for x in self.val("local.models", Ctx::default()).as_list() {
                        if !m.contains(x) {
                            m.push(x.clone());
                        }
                    }
                }
                m
            }
            Ty::PoolRef { auto } => {
                let mut m: Vec<String> = if auto { vec!["auto".into()] } else { Vec::new() };
                m.extend(self.pool_ids());
                m.push("local".into());
                m
            }
            Ty::AccountRef => self.pool_ids().iter().flat_map(|p| self.accounts(p).into_iter().map(move |a| format!("{p}/{}", a.id))).collect(),
            // `none`, then every endpoint used here, by name (the picker maps
            // its choice back to the id, [`App::endpoint_refs`]).
            Ty::EndpointRef => return std::iter::once("none".to_string()).chain(self.endpoint_refs().into_iter().map(|(_, label)| label)).collect(),
            _ => Vec::new(),
        };
        // What it holds now is always a choice, even if no pool offers it.
        if let Val::Str(cur) = self.sget(bd).val {
            if !out.contains(&cur) {
                out.push(cur);
            }
        }
        out
    }

    /// The endpoints a role may be bound to here, as (id, what the picker
    /// says): every one used here, by its name and url.
    pub fn endpoint_refs(&self) -> Vec<(String, String)> {
        self.seen_endpoints().into_iter().filter(|(h, id)| self.ep_used_here(*h, id)).map(|(h, id)| (id.clone(), format!("{} · {}", self.ep_name(h, &id), self.ep_host(h, &id)))).collect()
    }

    fn open_picker(&mut self, bd: Bound) {
        let options = self.choices(&bd);
        let cur = match bd.def().ty {
            Ty::EndpointRef => {
                let id = self.sget(&bd).val;
                self.endpoint_refs().into_iter().find(|(x, _)| Val::Str(x.clone()) == id).map_or_else(|| "none".to_string(), |(_, l)| l)
            }
            // The option as the picker holds it: the value itself, never
            // the words a row says it in.
            _ => match self.sget(&bd).val {
                Val::Str(s) => s,
                v => show(bd.def(), &v),
            },
        };
        let selected = options.iter().position(|o| *o == cur).unwrap_or(0);
        let title = bd.def().label.to_string();
        self.modal = Some(Modal::Picker { for_: PickFor::Setting { bound: bd }, title, options, selected });
    }

    /// The picker chose option `i`.
    pub(crate) fn pick(&mut self, i: usize) {
        let Some(Modal::Picker { for_, options, .. }) = self.modal.clone() else { return };
        let Some(choice) = options.get(i).cloned() else { return };
        self.modal = None;
        match for_ {
            PickFor::Setting { bound } if bound.def().ty == Ty::EndpointRef => match i {
                0 => self.clear_entry(Entry::Set(bound)),
                _ => {
                    if let Some((id, _)) = self.endpoint_refs().into_iter().nth(i - 1) {
                        self.apply(&bound, Ok(Val::Str(id)));
                    }
                }
            },
            PickFor::Setting { bound } => {
                let v = settings::parse_input(bound.def().ty, &choice);
                self.apply(&bound, v);
            }
            PickFor::AddItem { list } => {
                let mut items = self.sget(&list).val.as_list().to_vec();
                items.push(choice);
                self.apply(&list, Ok(Val::List(items)));
            }
            PickFor::MoveCluster { key, targets } => {
                if let Some(target) = targets.get(i).cloned() {
                    self.move_cluster(&key, target);
                }
            }
        }
    }

    /// Commit the field: Enter keeps it open on a refusal; leaving it (Esc,
    /// ←, ↑, ↓) closes it either way, the refusal staying on the row.
    pub(crate) fn commit_field(&mut self, leaving: bool) {
        let Some(f) = self.field.clone() else { return };
        // A secret opens empty (one click opens it, 0217 B4): left empty, it
        // closes as it was, with no refusal said about a key nobody typed.
        if leaving && f.bound.def().ty == Ty::Secret && f.text.trim().is_empty() {
            self.field = None;
            return;
        }
        let v = settings::parse_input(f.bound.def().ty, &f.text);
        // Opened and left as it reads, with nothing of its own here: no
        // choice was made, and nothing is written (0218). A value typed in is
        // a choice, kept even when it equals the default (0225 F1).
        if matches!(&v, Ok(v) if !self.sis_set(&f.bound) && *v == self.sget(&f.bound).val) {
            self.field = None;
            return;
        }
        let ok = self.apply(&f.bound, v);
        if ok || leaving {
            self.field = None;
        } else if let (Some(field), Some((_, e))) = (&mut self.field, &self.refusal) {
            field.error = Some(e.clone());
        }
    }

    /// The name modal for an account or a list item was confirmed.
    pub(crate) fn add_named(&mut self) {
        match self.modal.clone() {
            Some(Modal::AddAccount { pool, .. }) if !settings::inference_live(&pool) => {
                self.modal = None;
                self.notice = Some(settings::GREYED.into());
            }
            // V1.6: an account is added to Claude Code by logging it in,
            // through the daemon — it is listed when the daemon lists it.
            Some(Modal::AddAccount { pool, name, .. }) if pool == CLAUDE_CODE => self.start_login(&name),
            Some(Modal::AddAccount { pool, name, .. }) => {
                let taken: Vec<String> = self.accounts(&pool).into_iter().map(|a| a.id).collect();
                match self.settings.add_account(&pool, &name, |id| taken.iter().any(|t| t == id)) {
                    Ok(id) => {
                        self.modal = None;
                        let at = self.selectable().iter().position(|e| matches!(e, Entry::Row(SPage::Account(_, a)) if *a == id));
                        if let Some(i) = at {
                            self.set_settings_sel(i);
                        }
                        self.notice = Some(format!("added {} to {pool}", crate::pool::model::shown_name(name.trim())));
                    }
                    Err(e) => {
                        if let Some(Modal::AddAccount { error, .. }) = &mut self.modal {
                            *error = Some(format!("{e:#}"));
                        }
                    }
                }
            }
            Some(Modal::AddItem { list, name, .. }) => {
                let item = name.trim().to_string();
                let mut items = self.sget(&list).val.as_list().to_vec();
                let r = if item.is_empty() {
                    Err("type something to add".to_string())
                } else if let Err(why) = item_shape(list.key, &item) {
                    Err(why)
                } else if items.contains(&item) {
                    Err(format!("`{item}` is already there"))
                } else {
                    items.push(item);
                    self.sset(&list, Val::List(items))
                };
                match r {
                    Ok(()) => {
                        self.modal = None;
                        self.refusal = None;
                    }
                    Err(e) => {
                        if let Some(Modal::AddItem { error, .. }) = &mut self.modal {
                            *error = Some(e);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// The rows ⌥↑ / ⌥↓ land on: every top-level band, every section's band,
    /// and the first row under each section head.
    pub fn settings_heads(&self) -> Vec<usize> {
        let mut heads = Vec::new();
        let mut i = 0;
        let mut fresh = true;
        for f in self.entries() {
            if f.entry.selectable() {
                let section = matches!(&f.entry, Entry::Sub { page: SPage::Section(..), .. });
                if fresh || section || (f.depth == 0 && f.open.is_some()) {
                    heads.push(i);
                }
                fresh = false;
                i += 1;
            } else if matches!(f.entry, Entry::Head(..)) {
                fresh = true;
            }
        }
        heads
    }

    /// Open the bands named by a path of slugs, as → would on each one, and
    /// select the last: `providers/claude_code`. A last slug naming a ROW of
    /// a table opens its modal: `claude_code/acct2`.
    pub fn open_settings_path(&mut self, path: &str) -> anyhow::Result<()> {
        let mut under: Option<SPage> = None;
        let mut segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        // A leading tab's slug picks the tab; else the tab whose top level
        // has the first band. V1.1: a band that moved tabs (a source's, from
        // Providers to Accounts) is still found under its old tab's slug.
        let top_band = |a: &mut App, t: CfgTab, slug: &str| {
            a.set_cfg_tab(t);
            a.selectable_flat().iter().any(|f| unsection(f.parent.clone()).is_none() && f.entry.band().is_some_and(|b| b.slug() == slug))
        };
        if let Some(t) = segs.first().and_then(|s| CfgTab::parse(s)) {
            segs.remove(0);
            if let Some(&next) = segs.first() {
                if !top_band(self, t, next) && !CfgTab::ALL.into_iter().any(|o| top_band(self, o, next)) {
                    self.set_cfg_tab(t);
                }
            } else {
                self.set_cfg_tab(t);
            }
        } else if let Some(first) = segs.first() {
            for t in CfgTab::ALL {
                if top_band(self, t, first) {
                    break;
                }
            }
        }
        let last = segs.len().saturating_sub(1);
        for (i, seg) in segs.into_iter().enumerate() {
            // A row of a table, last: selected, its modal open.
            if i == last {
                let row = self.selectable_flat().into_iter().position(|f| unsection(f.parent.clone()) == under && matches!(&f.entry, Entry::Row(p) if p.slug() == seg));
                if let Some(at) = row {
                    self.set_settings_sel(at);
                    if let Some(Entry::Row(p)) = self.settings_entry() {
                        self.open_row(p);
                    }
                    return Ok(());
                }
            }
            // A band inside a section of `under` is in `under` (a path names
            // pages, never sections).
            let band = self.selectable_flat().into_iter().find(|f| unsection(f.parent.clone()) == under && f.entry.band().is_some_and(|b| b.slug() == seg)).and_then(|f| f.entry.band());
            let Some(band) = band else {
                anyhow::bail!("no band `{seg}` {}", under.map_or("at the top".to_string(), |u| format!("in {}", u.title())))
            };
            self.expand(band.clone());
            self.select_band(&band);
            under = Some(band);
        }
        Ok(())
    }

    /// Select the row whose setting is `key` on the page on screen.
    pub fn select_setting(&mut self, key: &str) -> bool {
        let at = self.selectable().iter().position(|e| match e {
            Entry::Set(bd) => bd.key == key || bd.concrete() == key,
            Entry::Sub { label, .. } => label == key,
            _ => false,
        });
        if let Some(i) = at {
            self.set_settings_sel(i);
        }
        at.is_some()
    }

    /// Every settings key a key event may act on reaches here: the region
    /// handler for a settings tab. True when the key was taken.
    pub(crate) fn settings_key(&mut self, code: ratatui::crossterm::event::KeyCode) -> bool {
        use ratatui::crossterm::event::KeyCode as K;
        self.refusal = None;
        if !matches!(code, K::Char('x') | K::Backspace | K::Delete) {
            self.pending_remove = None;
        }
        match code {
            K::Right | K::Enter => {
                self.settings_open();
            }
            K::Left => {
                self.settings_back();
            }
            K::Char(' ') => self.settings_space(),
            K::Char('h') => self.settings_help(),
            K::Char('x') | K::Backspace | K::Delete => self.settings_clear(),
            K::Char('+' | '=') => self.settings_nudge(true),
            K::Char('-' | '_') => self.settings_nudge(false),
            _ => return false,
        }
        true
    }

    /// The tag `i` quotes for selectable Config row `i` on screen:
    /// `[setting:<key>]` for a setting, a list or one of its items, an
    /// action; `[setting:<prefix>]` for a submenu (the page it opens). `+ add`
    /// rows have none.
    pub fn setting_tag(&self, i: usize) -> Option<crate::cauldron::tags::Tag> {
        let key = match self.selectable().into_iter().nth(i)? {
            Entry::Set(bd) | Entry::Run(bd) | Entry::Item { list: bd, .. } => bd.concrete().replace(' ', "-"),
            Entry::Sub { page: SPage::Repo(n), .. } => return Some(Tag::thing("repo", &n)),
            Entry::Sub { page: SPage::Region(n), .. } => return Some(Tag::thing("region", &n)),
            Entry::Sub { page: SPage::Node(n), .. } => return Some(Tag::thing("node", &n)),
            // An endpoint is quoted as itself (design 0199 §7).
            Entry::Row(SPage::Endpoint { id, .. } | SPage::UseEndpoint(id)) => return Some(Tag::thing("endpoint", &id)),
            Entry::Sub { page, .. } | Entry::Row(page) => page.key_prefix(),
            Entry::Ref { tag, .. } => return Some(tag),
            Entry::Do { .. } | Entry::Add(_) | Entry::Head(..) | Entry::Fixed(..) | Entry::Fact(..) | Entry::Gap | Entry::Para(_) => return None,
        };
        Some(crate::cauldron::tags::Tag::thing("setting", &key))
    }

    /// The rows of the cauldron's Config tab.
    pub(crate) fn settings_rows(&self) -> Vec<Row> {
        (0..self.selectable().len()).map(Row::Setting).collect()
    }
}

/// What a list's `+ add` button says: `+ add model`, `+ add header`, or
/// `+ add`.
pub fn add_words(list: &Bound) -> String {
    match list.key.rsplit('.').next() {
        Some("model_list") => "+ add model".into(),
        Some("headers") => "+ add header".into(),
        _ => "+ add".into(),
    }
}

/// Whether `item` has the shape list `key` takes: a model slug is one word
/// (`vendor/model` or a bare name — checked for shape only, never against
/// the provider: nothing is called from this face); a header is `Name:
/// value`.
pub fn item_shape(key: &str, item: &str) -> Result<(), String> {
    if key.ends_with(".model_list") && (item.contains(char::is_whitespace) || item.starts_with('/') || item.ends_with('/') || item.matches('/').count() > 1) {
        return Err("a model's exact name: vendor/model, or a bare name".into());
    }
    if key.ends_with(".headers") {
        let ok = item.split_once(':').is_some_and(|(n, v)| !n.trim().is_empty() && !n.contains(char::is_whitespace) && !v.trim().is_empty());
        if !ok {
            return Err("Name: value".into());
        }
    }
    Ok(())
}

/// The `ui.preserve.*` setting of cauldron tab `tab` (`config`, `status`):
/// whether its expansion is kept when he leaves it.
pub fn preserve_key(tab: &str) -> &'static str {
    match tab {
        "config" => "ui.preserve.config",
        _ => "ui.preserve.status",
    }
}

/// A path under the home directory, written from `~`.
fn tilde(path: &str) -> String {
    crate::cauldron::artpage::tilde(path)
}
