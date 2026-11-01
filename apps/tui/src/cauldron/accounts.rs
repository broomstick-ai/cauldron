//! **V1.6 — Config › Accounts, real and global, toggled per cauldron.**
//!
//! The V1 release plan, item 6: *"Accounts are real and global, logged in
//! once, and toggled per cauldron. The global default is on. The default is
//! configurable, for example off for work accounts."*
//!
//! - **The roster is the daemon's.** Claude Code's accounts are the `accounts`
//!   shape's rows, off the face's ONE connection ([`crate::cauldron::live::Live::pool`],
//!   the same mirror Status › Tokens reads) — never the seed's `global.json`.
//! - **Two switches, named apart.** *in the pool* is the daemon's `disabled`
//!   cell, machine-wide, read off the shape and flipped with
//!   `accounts/{account}/enable|disable` through the seam ([`Act::InPool`]).
//!   *on in new cauldrons* is the global setting
//!   `pools.{pool}.accounts.{account}.enabled` (default on), which every
//!   cauldron's `cauldron.accounts.{pool}.{account}.allowed` inherits.
//! - **Logged in once.** `+ add account` and an account's `log in` run the
//!   daemon's piped login: `accounts/login/begin` → the URL in the modal → the
//!   pasted code to `accounts/login/{session_id}/complete`, `cancel` on Esc.
//!   The new account arrives on the shape (rule 2.4: the ack is never the row).
//! - **No address is ever drawn whole.** An account keyed by an email is
//!   shown through the ONE masker, `pool::model::shown_name` (twins told
//!   apart by `shown_among`); anything the daemon says back is masked the
//!   same way (`pool::model::masked_text`); the pasted code is drawn as dots,
//!   is never printed by `{:?}` ([`LoginCode`]), and never leaves this module
//!   but in the `complete` request's params.
//!
//! [`Act::InPool`]: crate::cauldron::manage::Act::InPool

use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;

use crate::acts::AccountVerb;
use crate::cauldron::app::{App, Modal};
use crate::cauldron::live::AccountReply;

use crate::pool::model::{masked_text, shown_among};

/// **THE LOGIN CODE, WHICH NO `{:?}` PRINTS** (V1-ACCT, V1.6-I residual).
/// The code he pastes rides `Modal::Login`, and `Modal` derives `Debug` — a
/// test diagnostic or a log of `app.modal` would print a live code. This
/// holds it with a `Debug` that says only how long it is; it derefs to the
/// `String` it holds, so typing, pasting and the one `complete` request read
/// it as before.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct LoginCode(String);

impl std::fmt::Debug for LoginCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<redacted login code, {} chars>", self.0.chars().count())
    }
}

impl std::ops::Deref for LoginCode {
    type Target = String;
    fn deref(&self) -> &String {
        &self.0
    }
}

impl std::ops::DerefMut for LoginCode {
    fn deref_mut(&mut self) -> &mut String {
        &mut self.0
    }
}

/// Where a login in the modal stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginStage {
    /// `begin` is asked; nothing back yet.
    Starting,
    /// The daemon's login is waiting for the code: open `url`, paste it.
    AwaitCode { session_id: String, url: String },
    /// The code went to the daemon.
    Exchanging { session_id: String },
    /// Finished, either way: what the daemon said, masked.
    Done { ok: bool, said: String },
}

/// The login modal's own words for its stage — the view draws these; none
/// carries the code.
pub fn stage_lines(stage: &LoginStage) -> Vec<(String, bool)> {
    match stage {
        LoginStage::Starting => vec![("asking the threads daemon to start the login…".into(), false)],
        LoginStage::AwaitCode { url, .. } => vec![
            ("1. open this address and sign in:".into(), false),
            (url.clone(), false),
            ("2. paste the code it gives you, then Enter:".into(), false),
        ],
        LoginStage::Exchanging { .. } => vec![("handing the code to the threads daemon…".into(), false)],
        LoginStage::Done { ok: true, said } => vec![(said.clone(), false)],
        LoginStage::Done { ok: false, said } => vec![(said.clone(), true)],
    }
}

/// One login's tag, so a reply that arrives after he left it is known for
/// what it is.
static LOGIN_TAG: AtomicU64 = AtomicU64::new(1);

/// Replies of the in-pool switch, tagged apart from any login's.
const IN_POOL_TAG: u64 = 0;

/// What Claude Code's roster is now, for a page that lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Roster {
    /// The daemon's accounts are in (their keys, in its order).
    Known(Vec<String>),
    /// Not yet: why, in words (`connecting…`, `loading…`, `threads is down`,
    /// `not connected`).
    Waiting(String),
}

impl App {
    /// **CLAUDE CODE'S ACCOUNTS, AS THE DAEMON HOLDS THEM** — every registered
    /// row and every member named without one yet (drawn loading, never left
    /// out). Never the seed's.
    pub fn roster(&self) -> Roster {
        let Some(pool) = self.live.pool() else {
            return Roster::Waiting(format!("not connected: {}", self.live.why_not.clone().unwrap_or_else(|| "not connected to a threads daemon".into())));
        };
        if !pool.known {
            return Roster::Waiting(match &self.live.store.link {
                tui_render::LinkState::Disconnected { reason, .. } => format!("threads is down: {reason}"),
                tui_render::LinkState::Connected => "loading the account pool…".into(),
                _ => "connecting to the threads daemon…".into(),
            });
        }
        let mut keys: Vec<String> = pool.board.cards.iter().map(|c| c.label.clone()).collect();
        keys.extend(pool.loading.iter().filter(|k| !keys.contains(k)).cloned().collect::<Vec<_>>());
        Roster::Known(keys)
    }

    /// The keys of [`App::roster`], none while it is waiting.
    pub fn roster_keys(&self) -> Vec<String> {
        match self.roster() {
            Roster::Known(k) => k,
            Roster::Waiting(_) => Vec::new(),
        }
    }

    /// **CARRY EVERY KNOWN ACCOUNT'S SETTINGS TO ITS ID NOW** (V1-ACCT):
    /// the daemon's roster for Claude Code, and every other pool's seed and
    /// added accounts, through [`crate::cauldron::settings::Settings::adopt_account_ids`].
    /// Called when the face opens and whenever the connection brought
    /// something; it writes only when an old id is found, so once moved it
    /// is a read. A failure is said, masked, never a crash.
    pub fn adopt_account_ids(&mut self) -> usize {
        use crate::cauldron::menu::CLAUDE_CODE;
        let mut by_pool: Vec<(String, Vec<String>)> = Vec::new();
        let roster = self.roster_keys();
        if !roster.is_empty() {
            by_pool.push((CLAUDE_CODE.to_string(), roster));
        }
        for p in &self.dash.pools {
            let mut labels: Vec<String> = p.accounts.clone();
            labels.extend(self.settings.added.iter().filter(|a| a.pool == p.id).map(|a| a.label.clone()));
            by_pool.push((p.id.clone(), labels));
        }
        for a in &self.settings.added {
            if !by_pool.iter().any(|(p, _)| *p == a.pool) {
                by_pool.push((a.pool.clone(), vec![a.label.clone()]));
            } else if let Some((_, l)) = by_pool.iter_mut().find(|(p, _)| *p == a.pool) {
                if !l.contains(&a.label) {
                    l.push(a.label.clone());
                }
            }
        }
        let mut moved = 0;
        for (pool, labels) in by_pool {
            match self.settings.adopt_account_ids(&pool, &labels) {
                Ok(n) => moved += n,
                Err(e) => self.error = Some(masked_text(&format!("carrying account settings to their new ids: {e:#}"))),
            }
        }
        // What it decided that he should know (an old id two accounts
        // shared, a value dropped for a newer one): said, never silent
        // (V1-ACCT-I N1/N2). Masked names only, and once more through the
        // masker in case.
        let notes = std::mem::take(&mut self.settings.adoption_notes);
        if !notes.is_empty() {
            self.notice = Some(masked_text(&notes.join("; ")));
        }
        moved
    }

    /// What daemon account `key` is drawn as: the one masker, told apart
    /// from a twin on the roster that masks the same (V1-ACCT).
    pub fn shown_key(&self, key: &str) -> String {
        let keys = self.roster_keys();
        shown_among(key, keys.iter().map(String::as_str))
    }

    /// Whether account `key` is IN THE POOL, by the daemon's `disabled` cell:
    /// `None` while its row has not arrived.
    pub fn in_pool(&self, key: &str) -> Option<bool> {
        let pool = self.live.pool()?;
        pool.board.cards.iter().find(|c| c.label == key).map(|c| !c.disabled())
    }

    /// What the *in the pool* cell says for `key`: the producer's mark when
    /// the `disabled` cell carries one (consumer §7), else `in` / `out`, else
    /// `loading`.
    pub fn in_pool_words(&self, key: &str) -> String {
        if let Some(mk) = self.live.pool().and_then(|p| p.marks.get(key).and_then(|m| m.get("disabled")).cloned()) {
            return masked_text(&mk.words());
        }
        match self.in_pool(key) {
            Some(true) => "in".into(),
            Some(false) => "out".into(),
            None => "loading".into(),
        }
    }

    /// The account's login, in the pool board's words (`login ok`,
    /// `EXPIRED`, …), or the producer's mark on its `credential` cell.
    pub fn login_words(&self, key: &str) -> String {
        let Some(pool) = self.live.pool() else { return "—".into() };
        if let Some(mk) = pool.marks.get(key).and_then(|m| m.get("credential")) {
            return masked_text(&mk.words());
        }
        pool.board.cards.iter().find(|c| c.label == key).map_or_else(|| "loading".into(), |c| c.slot.badge().0)
    }

    /// The daemon's pressure for `key` (∞ as infinity), when it has one.
    pub fn live_pressure(&self, key: &str) -> Option<f64> {
        let pool = self.live.pool()?;
        let c = pool.board.cards.iter().find(|c| c.label == key)?;
        if c.infinite {
            return Some(f64::INFINITY);
        }
        c.pressure
    }

    /// The confirm said yes: say `enable` or `disable` for `key` to the
    /// daemon. The row moves when the shape does — never here.
    pub(crate) fn send_in_pool(&mut self, key: &str, on: bool) -> Result<String, String> {
        self.live.account(AccountVerb::InPool { account: key.to_string(), on }, IN_POOL_TAG)?;
        Ok(format!("asked the threads daemon to {} {} — the table follows when it does", if on { "put" } else { "take" }, self.shown_key(key)) + if on { " back in the pool" } else { " out of the pool" })
    }

    /// **START A LOGIN** for `account` — a new slot's name (from `+ add
    /// account`) or an existing account's key (its `log in`): `begin` goes to
    /// the daemon and the modal waits for its URL.
    pub fn start_login(&mut self, account: &str) {
        let account = account.trim().to_string();
        if account.is_empty() {
            if let Some(Modal::AddAccount { error, .. }) = &mut self.modal {
                *error = Some("type the account's name".into());
            }
            return;
        }
        let tag = LOGIN_TAG.fetch_add(1, Ordering::SeqCst);
        let title = format!("Log in {}", self.shown_key(&account));
        match self.live.account(AccountVerb::LoginBegin { account }, tag) {
            Ok(()) => self.modal = Some(Modal::Login { title, stage: LoginStage::Starting, code: LoginCode::default(), tag }),
            Err(why) => self.modal = Some(Modal::Login { title, stage: LoginStage::Done { ok: false, said: format!("not started: {}", masked_text(&why)) }, code: LoginCode::default(), tag }),
        }
    }

    /// Keys while the login modal is open: the code is typed or pasted into
    /// its field, Enter hands it over, Esc leaves (and cancels a login the
    /// daemon is holding).
    pub(crate) fn login_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(Modal::Login { stage, code, tag, .. }) = &mut self.modal else { return };
        match (key.code, stage.clone()) {
            (KeyCode::Esc | KeyCode::Left, LoginStage::AwaitCode { session_id, .. } | LoginStage::Exchanging { session_id }) => {
                code.clear();
                let tag = *tag;
                self.modal = None;
                let _ = self.live.account(AccountVerb::LoginCancel { session_id }, tag);
                self.notice = Some("login cancelled".into());
            }
            (KeyCode::Esc | KeyCode::Left, LoginStage::Starting) => {
                // `begin` is still out: its session is cancelled when its
                // reply finds the modal gone (`account_tick`).
                self.modal = None;
                self.notice = Some("login cancelled".into());
            }
            (KeyCode::Esc | KeyCode::Left | KeyCode::Enter | KeyCode::Right, LoginStage::Done { .. }) => self.modal = None,
            (KeyCode::Enter | KeyCode::Right, LoginStage::AwaitCode { session_id, .. }) => {
                let c = std::mem::take(&mut **code).trim().to_string();
                if c.is_empty() {
                    return;
                }
                let tag = *tag;
                *stage = LoginStage::Exchanging { session_id: session_id.clone() };
                if let Err(why) = self.live.account(AccountVerb::LoginComplete { session_id, code: c }, tag) {
                    if let Some(Modal::Login { stage, .. }) = &mut self.modal {
                        *stage = LoginStage::Done { ok: false, said: format!("not signed in: {}", masked_text(&why)) };
                    }
                }
            }
            (KeyCode::Backspace, LoginStage::AwaitCode { .. }) => {
                code.pop();
            }
            (KeyCode::Char('u'), LoginStage::AwaitCode { .. }) if ctrl => code.clear(),
            (KeyCode::Char(ch), LoginStage::AwaitCode { .. }) if !ctrl => code.push(ch),
            _ => {}
        }
    }

    /// A paste into the login modal: the code field takes it, trimmed.
    pub(crate) fn login_paste(&mut self, s: &str) {
        if let Some(Modal::Login { stage: LoginStage::AwaitCode { .. }, code, .. }) = &mut self.modal {
            code.push_str(s.trim());
        }
    }

    /// **WHAT THE DAEMON ANSWERED** an account verb, folded into the modal
    /// (or a notice). Called on every tick; returns how many replies moved
    /// something.
    pub fn account_tick(&mut self) -> usize {
        let replies = self.live.take_account_replies();
        let n = replies.len();
        for r in replies {
            self.account_reply(r);
        }
        n
    }

    fn account_reply(&mut self, r: AccountReply) {
        if r.tag == IN_POOL_TAG {
            if let Err(why) = r.result {
                self.error = Some(format!("the in-pool switch was refused: {}", masked_text(&why)));
            }
            return;
        }
        let current = matches!(&self.modal, Some(Modal::Login { tag, .. }) if *tag == r.tag);
        if !current {
            // He left this login: a session the daemon opened for it after
            // he did is cancelled, so nothing is left holding a login open.
            if r.verb == "begin" {
                if let Some(session_id) = r.result.as_ref().ok().and_then(|p| p.get("session_id")).and_then(Value::as_str) {
                    let _ = self.live.account(AccountVerb::LoginCancel { session_id: session_id.to_string() }, r.tag);
                }
            }
            return;
        }
        let Some(Modal::Login { stage, .. }) = &mut self.modal else { return };
        *stage = match (r.verb, r.result) {
            ("begin", Ok(p)) => match (p.get("session_id").and_then(Value::as_str), p.get("url").and_then(Value::as_str)) {
                (Some(s), Some(u)) => LoginStage::AwaitCode { session_id: s.to_string(), url: u.to_string() },
                _ => LoginStage::Done { ok: false, said: "the threads daemon started no login (no session or address came back)".into() },
            },
            ("begin", Err(why)) => LoginStage::Done { ok: false, said: format!("the threads daemon refused the login: {}", masked_text(&why)) },
            ("complete", Ok(p)) if p.get("ok").and_then(Value::as_bool) == Some(true) => {
                LoginStage::Done { ok: true, said: "signed in. The account is listed here when the threads daemon lists it.".into() }
            }
            ("complete", Ok(p)) => {
                let why = p.get("refused").and_then(Value::as_str).map(masked_text).unwrap_or_else(|| "the login script refused, or the exchange failed".into());
                LoginStage::Done { ok: false, said: format!("not signed in: {why}") }
            }
            ("complete", Err(why)) => LoginStage::Done { ok: false, said: format!("not signed in: {}", masked_text(&why)) },
            (_, _) => return,
        };
    }
}
