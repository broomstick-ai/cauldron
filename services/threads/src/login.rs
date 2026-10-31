//! **THE LOGIN FLOW, ISSUED THROUGH THREADS.**
//!
//! `claude auth login --claudeai` prints a URL, opens a browser, and then waits
//! for a `code#state` pasted onto its STDIN. That is a two-round-trip
//! conversation with a human in the middle, and it is why logging an account in
//! was the one account operation the interface could not do: it had to suspend
//! itself, hand the operator a bare terminal, and let a bash script own the
//! screen.
//!
//! This module makes it two API calls. [`begin`] starts the child and returns
//! the URL; [`complete`] hands back what the operator pasted. In between, the
//! child is alive, parked on a read, and owned by [`LoginSessions`].
//!
//! # THE PTY QUESTION, settled by measurement
//!
//! `scripts/add-account` deliberately INHERITS stdin and stdout when it runs
//! `claude`, and the comment there says so, so the first question was whether a
//! parent can drive this on plain pipes at all or whether it needs a pty. It
//! was measured against the real binary (v2.1.220) rather than assumed:
//!
//! * On a PIPE the child prints the banner, the URL and the prompt within about
//!   1.2 seconds, in plain UTF-8, with **zero ANSI escapes**. It does not detect
//!   the non-tty, does not suppress the URL, does not refuse and does not
//!   change its behaviour. Writing `code#state\n` to its stdin afterwards makes
//!   it perform the token exchange.
//! * Under a PTY the same output arrives wrapped in an OSC-8 hyperlink, with
//!   the URL repeated a second time in `\x1b[94m…`, plus `\r` line endings and
//!   a cursor-show escape.
//!
//! **So a pty is not required and would be actively worse** — it buys nothing
//! and costs an escape-stripping pass over the exact string we must not get
//! wrong. Pipes it is.
//!
//! Two behaviours the measurement found that this module has to respect, and
//! both would have been easy to get wrong by reasoning alone:
//!
//! * **The code must be written AFTER the prompt appears.** Data pre-fed into
//!   the pipe is consumed at startup and lost; `(printf 'x\n'; sleep 30) | claude
//!   auth login` hangs forever. So [`begin`] reads until it sees the prompt, and
//!   [`complete`] is the only thing that ever writes.
//! * **EOF on stdin is IGNORED.** Closing the pipe does not make the child exit;
//!   it hangs until killed. The parent owns the timeout, always, which is why
//!   every wait in this file is bounded and why [`LoginSessions::sweep`] exists.
//!
//! # It DRIVES `add-account`; it does not reimplement it
//!
//! `add-account` holds the credential guards that make this safe — it refuses
//! when `CLAUDE_CODE_OAUTH_TOKEN` is set (upstream #37512: `claude` then deletes
//! the machine-default keychain item on exit, destroying the live login), it
//! refuses when `CLAUDE_CONFIG_DIR` is already set because it needs the machine
//! login as its baseline, it verifies the new credential is a genuinely
//! DISTINCT account rather than a second copy of one already held, and it
//! carries conformance vectors for all of it. None of that is restated here.
//! This module is a pipe and a state machine around that script.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use crate::error::{Error, Result};

/// How long to wait for the child to print its URL before giving up.
///
/// The measured time to first output is ~1.2s. Sixty seconds is not a guess at
/// that number, it is a bound on the whole startup path `add-account` runs
/// BEFORE reaching `claude`: the gitignore check, the keychain baseline read,
/// the distinct-account preflight and a `security` call or two.
pub const URL_TIMEOUT: Duration = Duration::from_secs(60);

/// How long to wait for the child to finish after the code is pasted.
///
/// The token exchange is one HTTPS round trip, but `add-account` then does its
/// distinct-account verification, which reads the keychain and may run a
/// `/usage` probe.
pub const COMPLETE_TIMEOUT: Duration = Duration::from_secs(180);

/// How long an un-completed session may sit before it is reaped.
///
/// A human is reading a browser page in this window, so it is generous. It is
/// bounded at all because the child IGNORES EOF: a session nobody completes
/// would otherwise be a `claude` process parked on a read for the life of the
/// daemon.
pub const SESSION_TTL: Duration = Duration::from_secs(15 * 60);

/// The marker `claude` prints immediately before it blocks on stdin. Matched as
/// a SUBSTRING and without a trailing newline, because there is not one — the
/// prompt is written and then the process blocks, so a line-oriented reader
/// waits forever for an EOL that only arrives after the answer.
const PASTE_PROMPT: &str = "Paste code here";

/// The line `claude` prints the URL on when the browser did not open, which is
/// always, because this module always passes `--no-browser`.
const URL_LINE: &str = "If the browser didn't open, visit:";

// ── THE CROSSED-LOGIN GUARD, DAEMON SIDE (R12.5b) ───────────────────────────
//
// `accounts/login/begin` spawns `add-account` DIRECTLY, never through
// `claude-accounts login`, so the script guard (R12.5) never ran on the
// daemon's own sign-in: a consent page answered as b@ for a slot meant for a@
// landed b@'s grant in a@'s slot, `add-account` exited 0, and the daemon then
// cleared every flag on a@ unread. This is the same guard, in the daemon:
//
// 1. BEFORE the spawn, a snapshot of the slot `add-account` signs in
//    (`<home>/accounts/<label>`, the home handed to it as
//    `CLAUDE_ACCOUNTS_HOME`): its `.claude.json`, `manifest.toml` and
//    `login-hint` (or that each was absent), the keychain item's secret HASH,
//    and a keychain→keychain copy of a previous credential under a dormant
//    `Claude Code-prelogin.*` name, so a refusal can put it back byte for byte.
// 2. AFTER the child exits, a login LANDED iff the slot's item changed. A
//    landed login is asked of the SERVER (R12.1 `IdentityGuard`, the same guard
//    `recheck_flags` asks): another email ⇒ REFUSED and REVERTED, `wanted X,
//    got Y`; unreadable ⇒ refused and reverted too, never a pass. `add-account`
//    exiting 0 with nothing landed in the checked slot ⇒ refused (R12.5
//    inspect B2: the login landed somewhere nobody checked).
// 3. The revert is the script's: the crossed credential is RETIRED (copied to a
//    dormant `Claude Code-retired.*` name, then deleted — the other account's
//    grant is parked, not destroyed), the previous one is copied back and
//    verified by hash, the slot's files are restored or removed.
//
// A secret never leaves a [`SlotKeychain`] implementation: the guard sees an
// 8-hex hash, and the copy goes keychain→keychain on `security -i`'s stdin.

/// The keychain operations the guard needs, and nothing that hands a secret
/// back. Production is [`SecurityKeychain`]; a test supplies its own.
pub trait SlotKeychain: Send + Sync {
    /// First 8 hex of sha256 of the item's secret, newlines removed (the
    /// script's `kc_secret_sha8`). `None` = no such item.
    fn secret_sha8(&self, service: &str) -> Option<String>;
    /// Create or update `to` with `from`'s secret. `true` only when `to` then
    /// hashes as `from` did.
    fn copy(&self, from: &str, to: &str) -> bool;
    /// Delete the item. `true` when it is gone.
    fn delete(&self, service: &str) -> bool;
}

/// The production [`SlotKeychain`]: `/usr/bin/security`. The secret is read
/// into this process's memory, hashed or hex-encoded onto `security -i`'s
/// STDIN, and dropped — never an argv, a log line or a file. Every call trips
/// the CON-28 test tripwire first.
pub struct SecurityKeychain;

impl SecurityKeychain {
    fn tripwire(service: &str) {
        if crate::accounts::real_keychain_tripwire_armed() {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr(),
                "CON-28 tripwire: the daemon login guard reached the REAL keychain in a test \
                 process (service {service:?}). Install a fake with \
                 `LoginSessions::set_keychain_for_test`. Aborting before any keychain call."
            );
            std::process::abort();
        }
    }

    fn read_secret(service: &str) -> Option<Vec<u8>> {
        Self::tripwire(service);
        let out = std::process::Command::new("/usr/bin/security")
            .args(["find-generic-password", "-a", &crate::accounts::keychain_account(), "-w", "-s", service])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let mut bytes = out.stdout;
        bytes.retain(|b| *b != b'\n');
        Some(bytes)
    }
}

fn sha8_bytes(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(b);
    hash.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

impl SlotKeychain for SecurityKeychain {
    fn secret_sha8(&self, service: &str) -> Option<String> {
        Self::read_secret(service).map(|s| sha8_bytes(&s))
    }

    fn copy(&self, from: &str, to: &str) -> bool {
        use std::io::Write;
        let Some(secret) = Self::read_secret(from) else {
            return false;
        };
        let want = sha8_bytes(&secret);
        Self::tripwire(to);
        let hex: String = secret.iter().map(|b| format!("{b:02x}")).collect();
        drop(secret);
        let Ok(mut child) = std::process::Command::new("/usr/bin/security")
            .arg("-i")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return false;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let line = format!(
                "add-generic-password -U -a \"{}\" -s \"{}\" -X {hex}\n",
                crate::accounts::keychain_account(),
                to
            );
            let _ = stdin.write_all(line.as_bytes());
        }
        let _ = child.wait();
        self.secret_sha8(to).as_deref() == Some(want.as_str())
    }

    fn delete(&self, service: &str) -> bool {
        Self::tripwire(service);
        let _ = std::process::Command::new("/usr/bin/security")
            .args(["delete-generic-password", "-a", &crate::accounts::keychain_account(), "-s", service])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        self.secret_sha8(service).is_none()
    }
}

/// The dormant names, the script's own (`claude-accounts`
/// `PRELOGIN_SERVICE_PREFIX` / `RETIRED_SERVICE_PREFIX`), so one inventory
/// reads both tools' parked items.
pub const PRELOGIN_SERVICE_PREFIX: &str = "Claude Code-prelogin";
pub const RETIRED_SERVICE_PREFIX: &str = "Claude Code-retired";

/// The slot files a login may write, restored or removed by a revert.
const SLOT_FILES: [&str; 3] = [".claude.json", "manifest.toml", "login-hint"];

/// The pre-login snapshot of one slot (step 1 above). File contents are held
/// in memory: `.claude.json` carries no token, and nothing is left on disk.
struct Snapshot {
    home: std::path::PathBuf,
    slot: std::path::PathBuf,
    service: String,
    /// Who this login is FOR, normalised: `--email`, else the slot's
    /// `login-hint`, else the address its `.claude.json` named before. `None`
    /// = any identity the server can READ passes; an unreadable one never does.
    want: Option<String>,
    secret_sha8: Option<String>,
    parked: Option<String>,
    files: Vec<(&'static str, Option<Vec<u8>>)>,
}

/// What the guard decided about a finished (or abandoned) login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardVerdict {
    /// The login landed and the server says it is the wanted account.
    Verified { email: String },
    /// Nothing landed in the slot and `add-account` did not claim success:
    /// its own exit status is the verdict.
    NothingLanded,
    /// Refused (and, where something landed, reverted). `code` mirrors the
    /// script: 3 = another account, 4 = unreadable, 5 = landed elsewhere.
    Refused { code: i32, reason: String },
}

fn want_for(slot: &std::path::Path, email: Option<&str>) -> Result<Option<String>> {
    if let Some(e) = email {
        return crate::accounts::AccountId::parse(e)
            .map(|id| Some(id.as_str().to_string()))
            .map_err(|why| Error::Config(format!("--email: {why}")));
    }
    let slot_s = slot.to_string_lossy();
    if let Some(h) = crate::accounts::recorded_login_address(Some(&slot_s)) {
        if let Ok(id) = crate::accounts::AccountId::parse(&h) {
            return Ok(Some(id.as_str().to_string()));
        }
    }
    // The address the slot held before, as the WANTED identity only: a
    // re-login must not switch accounts silently. Never the answer.
    let prior = std::fs::read_to_string(slot.join(".claude.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v["oauthAccount"]["emailAddress"].as_str().map(str::to_string))
        .and_then(|e| crate::accounts::AccountId::parse(&e).ok());
    Ok(prior.map(|id| id.as_str().to_string()))
}

fn take_snapshot(
    kc: &dyn SlotKeychain,
    home: &std::path::Path,
    account: &str,
    email: Option<&str>,
) -> Result<Snapshot> {
    let slot = home.join("accounts").join(account);
    let service = crate::accounts::keychain_service_for(Some(&slot.to_string_lossy()));
    let want = want_for(&slot, email)?;
    let mut files = Vec::new();
    for f in SLOT_FILES {
        let p = slot.join(f);
        files.push((f, if p.is_file() { Some(std::fs::read(&p)?) } else { None }));
    }
    let secret_sha8 = kc.secret_sha8(&service);
    let parked = match &secret_sha8 {
        None => None,
        Some(_) => {
            let name = format!(
                "{PRELOGIN_SERVICE_PREFIX}.{account}.{}.{}",
                chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            );
            if !kc.copy(&service, &name) {
                return Err(Error::Config(format!(
                    "could not snapshot {account}'s credential before signing in, so a \
                     wrong-account login could not be undone. NOTHING was started; the slot \
                     is exactly as it was."
                )));
            }
            Some(name)
        }
    };
    Ok(Snapshot { home: home.to_path_buf(), slot, service, want, secret_sha8, parked, files })
}

impl Snapshot {
    /// Drop a snapshot whose login passed or never landed: the parked copy is
    /// this guard's own scratch, made moments ago.
    fn discard(self, kc: &dyn SlotKeychain) {
        if let Some(p) = &self.parked {
            kc.delete(p);
        }
    }

    /// REVERT (step 3). `Err` lists every part that could not be undone; the
    /// parked copy is then kept, and named.
    fn revert(self, kc: &dyn SlotKeychain, reason: &str) -> std::result::Result<(), Vec<String>> {
        let mut problems = Vec::new();
        if let Some(crossed) = kc.secret_sha8(&self.service) {
            let dormant = format!(
                "{RETIRED_SERVICE_PREFIX}.{}.{}",
                chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
                crate::accounts::sha8(&self.service)
            );
            if kc.copy(&self.service, &dormant) && kc.delete(&self.service) {
                // The script's paper trail, hashes only.
                let ledger = self.home.join("accounts").join("retired-keychain-items.log");
                let line = format!(
                    "{}\tretired={}\tsecret_sha8={crossed}\tdormant={dormant}\treason=refused login: {reason}\n",
                    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
                    self.service
                );
                use std::io::Write;
                let _ = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&ledger)
                    .and_then(|mut f| f.write_all(line.as_bytes()));
            } else {
                problems.push(format!(
                    "the crossed credential {:?} could NOT be retired; it is still in the slot",
                    self.service
                ));
            }
        }
        if let Some(parked) = &self.parked {
            if kc.copy(parked, &self.service) && kc.secret_sha8(&self.service) == self.secret_sha8 {
                kc.delete(parked);
            } else {
                problems.push(format!(
                    "the previous credential could not be put back; it is parked at {parked:?}"
                ));
            }
        }
        for (name, content) in &self.files {
            let p = self.slot.join(name);
            let r = match content {
                Some(bytes) => std::fs::write(&p, bytes),
                None if p.exists() => std::fs::remove_file(&p),
                None => Ok(()),
            };
            if let Err(e) = r {
                problems.push(format!("{}: {e}", p.display()));
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }

    /// THE VERDICT (step 2), then the revert when it refuses.
    fn judge(self, kc: &dyn SlotKeychain, account: &str, exit_ok: bool) -> GuardVerdict {
        let now = kc.secret_sha8(&self.service);
        let landed = now.is_some() && now != self.secret_sha8;
        if !landed {
            let slot = self.slot.display().to_string();
            self.discard(kc);
            if exit_ok {
                return GuardVerdict::Refused {
                    code: 5,
                    reason: format!(
                        "add-account reported a login for {account}, but no new credential is \
                         in the slot that was checked ({slot}). The login landed where it was \
                         not checked; it is NOT verified. Nothing here changed."
                    ),
                };
            }
            return GuardVerdict::NothingLanded;
        }
        let guard = crate::accounts::trusted_identity_guard();
        let dir = self.slot.to_string_lossy().to_string();
        let before = guard.binding_of(Some(&dir));
        let read = guard.authenticated_as(Some(&dir));
        let after = guard.binding_of(Some(&dir));
        let wanted = self.want.clone().unwrap_or_else(|| "<any readable identity>".into());
        let (code, got) = match read {
            Err(e) => (4, format!("<unreadable: {e}>")),
            Ok(got) if self.want.as_deref().is_some_and(|w| w != got.as_str()) => {
                (3, got.as_str().to_string())
            }
            Ok(_) if after != before => (4, "<unreadable: the credential changed during the read>".into()),
            Ok(got) => {
                self.discard(kc);
                return GuardVerdict::Verified { email: got.as_str().to_string() };
            }
        };
        let what = format!("wanted {wanted}, got {got}");
        let mut reason = format!(
            "REFUSED — the login for {account} did not authenticate as the account it was for: {what}."
        );
        match self.revert(kc, &what) {
            Ok(()) => reason.push_str(
                " Reverted: the slot's files and keychain item are back to what they were before \
                 this login, and the other account's grant is parked (dormant), not used.",
            ),
            Err(problems) => {
                reason.push_str(" The revert was INCOMPLETE: ");
                reason.push_str(&problems.join("; "));
                reason.push('.');
            }
        }
        reason.push_str(&format!(
            " {account} is NOT signed in by this run; sign in again and pick {wanted}."
        ));
        GuardVerdict::Refused { code, reason }
    }
}

/// One login in flight: a live child, parked on a read, and the URL it printed.
struct Session {
    /// The pre-login snapshot (R12.5b). Settled exactly once: by
    /// [`LoginSessions::complete`], or by a cancel / supersede / reap.
    snapshot: Option<Snapshot>,
    account: String,
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    url: String,
    /// Everything read from the child's STDOUT so far. Kept so the completion
    /// report can quote what `add-account` actually said rather than
    /// paraphrasing it.
    transcript: String,
    /// The child's STDERR, drained concurrently by a background task. This is
    /// where every `add-account` refusal goes, so it is part of the report and
    /// not a diagnostic to discard.
    errors: Arc<Mutex<String>>,
    started: std::time::Instant,
}

/// The live logins, keyed by session id.
#[derive(Clone)]
pub struct LoginSessions {
    inner: Arc<Mutex<HashMap<String, Session>>>,
    /// The keychain the crossed-login guard snapshots and reverts through.
    keychain: Arc<std::sync::RwLock<Arc<dyn SlotKeychain>>>,
}

impl Default for LoginSessions {
    fn default() -> Self {
        Self {
            inner: Arc::default(),
            keychain: Arc::new(std::sync::RwLock::new(Arc::new(SecurityKeychain))),
        }
    }
}

/// What [`begin`] hands back.
#[derive(Debug)]
pub struct Begun {
    pub session_id: String,
    pub account: String,
    pub url: String,
}

/// What [`complete`] hands back.
#[derive(Debug)]
pub struct Completed {
    pub account: String,
    /// The script exited 0 AND the guard passed the login (R12.5b).
    pub ok: bool,
    pub exit_code: Option<i32>,
    /// The crossed-login guard's verdict. `ok` is never true unless it is
    /// [`GuardVerdict::Verified`].
    pub guard: GuardVerdict,
    /// Everything the child said, both rounds. The interface shows this on
    /// failure; `add-account`'s refusals are written to be read by a human and
    /// paraphrasing them would lose the remedy they name.
    pub output: String,
}

/// Options a caller may pass through to `add-account`.
#[derive(Default)]
pub struct LoginOptions {
    pub email: Option<String>,
    /// Historically `--force --allow-same-account`: the two flags that unlocked
    /// `add-account`'s snapshot and same-account refusals.
    ///
    /// **BOTH OF THOSE REFUSALS ARE GONE**, and the flags are now accepted and
    /// ignored by the script, so this field no longer changes any outcome. It is
    /// kept because callers still set it and because passing the flags is
    /// harmless — but nothing depends on it, and a caller that leaves it `false`
    /// gets exactly the same behaviour. Re-logging a slot in is always
    /// permitted; there is nothing left to force.
    pub force: bool,
}

impl LoginSessions {
    /// **START A LOGIN.** Spawns `add-account --login <account> --no-browser`
    /// on pipes and reads until the URL and the paste prompt are both in hand.
    ///
    /// `--no-browser` is not a preference, it is the only correct choice here:
    /// the daemon has no display, and the script's browser helper would open a
    /// window on whatever session the daemon happens to be running under. The
    /// URL goes to the interface, which shows it to the operator on the machine
    /// he is actually sitting at.
    pub async fn begin(
        &self,
        script: &std::path::Path,
        threads_home: &std::path::Path,
        account: &str,
        opts: LoginOptions,
    ) -> Result<Begun> {
        if !script.exists() {
            return Err(Error::Config(format!(
                "the account login script is not at {} — the login flow drives that script \
                 rather than reimplementing its credential guards, so it cannot run without it",
                script.display()
            )));
        }
        // ── AN IN-FLIGHT LOGIN IS SUPERSEDED, NOT A REFUSAL ──────────────────
        //
        // This used to return *"a login for X is already in progress — complete
        // it, or cancel it, before starting another"*. That is a refusal on the
        // grounds that prior state exists, and it is a nasty one: an ABANDONED
        // session — a browser tab he closed, a code he never pasted — holds the
        // slot for the full `SESSION_TTL` of fifteen minutes, during which the
        // interface tells him he cannot sign in and offers him no way to clear
        // it. He is not going to go hunting for a cancel button; he is going to
        // press the login key again, which is exactly what he should be able to
        // do.
        //
        // So starting a login SUPERSEDES any login already in flight for that
        // account. The old child is killed first, which is what keeps the
        // original concern honest: the reason for one-at-a-time was that two
        // live children would race on one config dir and one keychain item, and
        // killing the old one before spawning the new one means there are still
        // never two. The newest intent wins.
        {
            let mut live = self.inner.lock().await;
            let superseded: Vec<String> = live
                .iter()
                .filter(|(_, s)| s.account == account)
                .map(|(id, _)| id.clone())
                .collect();
            for id in superseded {
                if let Some(mut s) = live.remove(&id) {
                    tracing::info!(
                        account = %s.account,
                        "superseding a login already in flight — a new sign-in replaces an \
                         abandoned one rather than being refused because of it"
                    );
                    // The child IGNORES EOF, so it is killed rather than
                    // dropped; a dropped handle leaves `claude` parked on a read.
                    let _ = s.child.kill().await;
                    self.settle_abandoned(&mut s).await;
                }
            }
        }

        // ── THE GUARD, part 1: snapshot the slot BEFORE anything can change ──
        let snapshot = {
            let kc = self.keychain();
            let home = threads_home.to_path_buf();
            let (acct, email) = (account.to_string(), opts.email.clone());
            tokio::task::spawn_blocking(move || take_snapshot(kc.as_ref(), &home, &acct, email.as_deref()))
                .await
                .map_err(|e| Error::Config(format!("the login snapshot task failed: {e}")))??
        };

        let mut cmd = tokio::process::Command::new(script);
        cmd.arg("--login").arg(account).arg("--no-browser");
        if let Some(email) = &opts.email {
            cmd.arg("--email").arg(email);
        }
        if opts.force {
            cmd.arg("--force").arg("--allow-same-account");
        }
        cmd.current_dir(threads_home)
            .env("THREADS_HOME", threads_home)
            // The slot `add-account` signs in is `<this home>/accounts/<label>`,
            // which is the slot the guard snapshotted and will check (R12.5
            // inspect B2: a home not handed on is a login nobody checks).
            .env("CLAUDE_ACCOUNTS_HOME", threads_home)
            // `add-account` REFUSES to run with either of these set, and it is
            // right to: with `CLAUDE_CODE_OAUTH_TOKEN` set, `claude` deletes the
            // machine-default keychain item on exit (upstream #37512), and with
            // `CLAUDE_CONFIG_DIR` set the script has no baseline for its
            // is-this-really-a-different-account check. The daemon may well have
            // inherited both; they are removed here so the refusal is not hit
            // for a reason that has nothing to do with the operator.
            .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("CLAUDE_CONFIG_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // **STDERR IS PIPED AND DRAINED, and forgetting the drain was a real
            // bug this file's tests caught.** `add-account` writes every refusal
            // through `say ... >&2` — the `CLAUDE_CODE_OAUTH_TOKEN` guard, the
            // already-exists refusal, the invalid label. The first version piped
            // stderr and then read only stdout, so a refused login reached the
            // operator as "the login script exited without printing anything"
            // instead of the sentence naming the variable he has to unset. The
            // guard was intact and its words were being thrown away.
            .stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                snapshot.discard(self.keychain().as_ref());
                return Err(Error::Config(format!("spawning {}: {e}", script.display())));
            }
        };
        let stdin = child.stdin.take().expect("stdin was piped");
        let mut stdout = child.stdout.take().expect("stdout was piped");
        // Drained CONCURRENTLY rather than after the fact: a child that fills
        // the stderr pipe buffer and blocks writing to it would deadlock
        // against a parent that is only reading stdout, and `add-account` is
        // chatty on stderr.
        let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
        let errors = Arc::new(Mutex::new(String::new()));
        let sink = errors.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                match stderr_pipe.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => sink
                        .lock()
                        .await
                        .push_str(&String::from_utf8_lossy(&buf[..n])),
                }
            }
        });

        // READ UNTIL THE PROMPT. Not `read_line` — the prompt has no trailing
        // newline, so a line reader blocks on it forever waiting for the EOL
        // that only arrives after the answer is given.
        let mut transcript = String::new();
        let deadline = tokio::time::Instant::now() + URL_TIMEOUT;
        let mut buf = [0u8; 4096];
        let outcome = loop {
            if transcript.contains(PASTE_PROMPT) && find_url(&transcript).is_some() {
                break Ok(());
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break Err(format!(
                    "timed out after {}s waiting for the login URL",
                    URL_TIMEOUT.as_secs()
                ));
            }
            match tokio::time::timeout(remaining, stdout.read(&mut buf)).await {
                // EOF: the child exited before printing a URL. That is a
                // REFUSAL from `add-account` — a credential guard, a bad label,
                // an existing slot — and its own words are the answer.
                Ok(Ok(0)) => {
                    // EOF on stdout. Give the stderr drain a moment to land —
                    // the refusal is almost always THERE, not here.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let errs = errors.lock().await.clone();
                    let said = format!("{}\n{}", transcript.trim(), errs.trim());
                    break Err(if said.trim().is_empty() {
                        "the login script exited without printing anything".to_string()
                    } else {
                        said.trim().to_string()
                    });
                }
                Ok(Ok(n)) => transcript.push_str(&String::from_utf8_lossy(&buf[..n])),
                Ok(Err(e)) => break Err(format!("reading from the login script: {e}")),
                Err(_) => {
                    break Err(format!(
                        "timed out after {}s waiting for the login URL",
                        URL_TIMEOUT.as_secs()
                    ))
                }
            }
        };

        if let Err(why) = outcome {
            // The child ignores EOF, so it is KILLED rather than dropped: a
            // dropped handle would leave `claude` parked on a read forever.
            let _ = child.kill().await;
            // No code was ever pasted, so nothing can have landed; the guard
            // still judges rather than assumes, and drops its parked copy.
            let kc = self.keychain();
            let acct = account.to_string();
            let _ = tokio::task::spawn_blocking(move || snapshot.judge(kc.as_ref(), &acct, false)).await;
            return Err(Error::Config(why));
        }

        let url = find_url(&transcript).expect("checked in the loop");
        let session_id = uuid::Uuid::new_v4().to_string();
        let mut live = self.inner.lock().await;
        live.insert(
            session_id.clone(),
            Session {
                snapshot: Some(snapshot),
                account: account.to_string(),
                child,
                stdin,
                stdout,
                url: url.clone(),
                transcript,
                errors,
                started: std::time::Instant::now(),
            },
        );
        Ok(Begun {
            session_id,
            account: account.to_string(),
            url,
        })
    }

    /// **FINISH A LOGIN** by writing what the operator pasted onto the child's
    /// stdin, then waiting for it to exit.
    ///
    /// The pasted value is `code#state`. It is written verbatim with a single
    /// trailing newline and NOT parsed, validated or reformatted here: it is an
    /// opaque token from the authorization server, and a parser in this file
    /// would be a second implementation of something only the server can
    /// actually check.
    pub async fn complete(&self, session_id: &str, code: &str) -> Result<Completed> {
        let mut session = {
            let mut live = self.inner.lock().await;
            live.remove(session_id).ok_or_else(|| {
                Error::Config(format!(
                    "no login session {session_id:?} — it was completed already, cancelled, or \
                     timed out after {} minutes",
                    SESSION_TTL.as_secs() / 60
                ))
            })?
        };

        // A newline-bearing paste would be read as the answer plus a stray
        // empty line, and the empty line lands in whatever prompt comes next.
        let code = code.trim();
        if code.is_empty() {
            let _ = session.child.kill().await;
            self.settle_abandoned(&mut session).await;
            return Err(Error::Config(
                "the pasted code is empty — paste the whole `code#state` value the login page \
                 gave you"
                    .into(),
            ));
        }

        let written = match session.stdin.write_all(format!("{code}\n").as_bytes()).await {
            Ok(()) => session
                .stdin
                .flush()
                .await
                .map_err(|e| Error::Config(format!("flushing the code: {e}"))),
            Err(e) => Err(Error::Config(format!("writing the code to the login script: {e}"))),
        };
        if let Err(e) = written {
            let _ = session.child.kill().await;
            self.settle_abandoned(&mut session).await;
            return Err(e);
        }
        // Now, and only now, is EOF safe: the answer is delivered, so closing
        // the pipe cannot swallow it.
        drop(session.stdin);

        let mut transcript = session.transcript;
        let mut buf = [0u8; 4096];
        let deadline = tokio::time::Instant::now() + COMPLETE_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, session.stdout.read(&mut buf)).await {
                Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                Ok(Ok(n)) => transcript.push_str(&String::from_utf8_lossy(&buf[..n])),
            }
        }

        // The child ignores EOF, so the wait is bounded and the fallback is a
        // kill. A login that hangs must not hold a slot for the life of the
        // daemon.
        let status = match tokio::time::timeout(Duration::from_secs(10), session.child.wait()).await
        {
            Ok(Ok(s)) => Some(s),
            _ => {
                let _ = session.child.kill().await;
                None
            }
        };
        let exit_code = status.and_then(|s| s.code());

        // Both streams, in the report. The refusals are on stderr and the
        // progress prose is on stdout; a reader given only one of them gets
        // either the story without the verdict or the verdict without the
        // remedy.
        let errs = session.errors.lock().await.clone();
        if !errs.trim().is_empty() {
            if !transcript.ends_with('\n') {
                transcript.push('\n');
            }
            transcript.push_str(errs.trim());
        }

        // ── THE GUARD, part 2: did the login land, and as WHOM (R12.5b) ──────
        // Whatever the script returned, a landed login is asked of the server:
        // a non-zero exit AFTER the credential was written does not make a
        // crossed credential safe to leave in place.
        let exit_ok = exit_code == Some(0);
        let guard = match session.snapshot.take() {
            Some(snap) => {
                let kc = self.keychain();
                let acct = session.account.clone();
                tokio::task::spawn_blocking(move || snap.judge(kc.as_ref(), &acct, exit_ok))
                    .await
                    .unwrap_or_else(|e| GuardVerdict::Refused {
                        code: 4,
                        reason: format!("the identity check did not run ({e}); NOT verified"),
                    })
            }
            None => GuardVerdict::Refused {
                code: 4,
                reason: "this login was taken without a snapshot, so it cannot be verified".into(),
            },
        };
        match &guard {
            GuardVerdict::Refused { reason, .. } => {
                tracing::error!(account = %session.account, "{reason}");
                if !transcript.ends_with('\n') {
                    transcript.push('\n');
                }
                transcript.push_str(reason);
            }
            GuardVerdict::Verified { email } => {
                tracing::info!(account = %session.account, %email, "identity verified by the server");
            }
            GuardVerdict::NothingLanded => {}
        }

        Ok(Completed {
            account: session.account,
            // The SCRIPT's exit status is the verdict, not any string in its
            // output — `add-account` exits non-zero for every refusal it makes
            // and for a failed exchange — AND the server must say the slot now
            // authenticates as the account it was signed in for (R12.5b).
            ok: exit_ok && matches!(guard, GuardVerdict::Verified { .. }),
            exit_code,
            guard,
            output: transcript,
        })
    }

    /// The keychain the guard uses (production unless a test replaced it).
    fn keychain(&self) -> Arc<dyn SlotKeychain> {
        self.keychain.read().unwrap().clone()
    }

    /// Replace the guard's keychain for this [`LoginSessions`]. For TESTS: the
    /// production one trips the CON-28 tripwire in a test process.
    #[doc(hidden)]
    pub fn set_keychain_for_test(&self, kc: Arc<dyn SlotKeychain>) {
        *self.keychain.write().unwrap() = kc;
    }

    /// Settle the snapshot of a login that never completed (cancelled,
    /// superseded, reaped, or refused before the exchange). Its child is
    /// already killed. Nothing should have landed — the code was never
    /// exchanged — but the guard judges rather than assumes: a landed,
    /// unverified credential is reverted, and the parked copy is dropped.
    async fn settle_abandoned(&self, s: &mut Session) {
        if let Some(snap) = s.snapshot.take() {
            let kc = self.keychain();
            let acct = s.account.clone();
            let v = tokio::task::spawn_blocking(move || snap.judge(kc.as_ref(), &acct, false)).await;
            if let Ok(GuardVerdict::Refused { reason, .. }) = v {
                tracing::error!(account = %s.account, "abandoned login: {reason}");
            }
        }
    }

    /// Cancel an in-flight login and kill its child.
    pub async fn cancel(&self, session_id: &str) -> bool {
        let taken = self.inner.lock().await.remove(session_id);
        match taken {
            Some(mut s) => {
                let _ = s.child.kill().await;
                self.settle_abandoned(&mut s).await;
                true
            }
            None => false,
        }
    }

    /// Reap sessions nobody completed. Called from the daemon's watcher loop.
    ///
    /// Necessary because the child IGNORES EOF: an abandoned session is a live
    /// `claude` process parked on a read, and without this it would sit there
    /// until the daemon restarted.
    pub async fn sweep(&self) -> usize {
        let reaped: Vec<Session> = {
            let mut live = self.inner.lock().await;
            let stale: Vec<String> = live
                .iter()
                .filter(|(_, s)| s.started.elapsed() > SESSION_TTL)
                .map(|(k, _)| k.clone())
                .collect();
            stale.iter().filter_map(|id| live.remove(id)).collect()
        };
        let n = reaped.len();
        for mut s in reaped {
            tracing::info!(
                account = %s.account,
                "reaping a login session nobody completed — the child ignores EOF, so it \
                 is killed rather than left parked on a read"
            );
            let _ = s.child.kill().await;
            self.settle_abandoned(&mut s).await;
        }
        n
    }

    /// In-flight sessions, for `daemon.status` and for a test.
    pub async fn list(&self) -> Vec<(String, String, String)> {
        self.inner
            .lock()
            .await
            .iter()
            .map(|(id, s)| (id.clone(), s.account.clone(), s.url.clone()))
            .collect()
    }
}

/// What the daemon's own login hands back (R12.5b).
#[derive(Debug)]
pub struct DaemonLogin {
    /// The guarded completion: `ok` only when the script exited 0 AND the
    /// server says the slot authenticates as the account it was for.
    pub done: Completed,
    /// The labels whose login bar the verified re-check lifted.
    pub reopened: Vec<String>,
    /// R12.10's re-check, run as the LAST act of a login that passed the
    /// guard. `None` = the login did not pass, so nothing was re-checked and
    /// nothing was cleared.
    pub recheck: Option<crate::accounts::FlagRecheck>,
}

/// **FINISH THE DAEMON'S OWN LOGIN — one call** (R12.5b; PS4 wraps this in its
/// pending promise). The guarded [`LoginSessions::complete`] (a crossed or
/// unreadable login is refused and reverted), then the pool reload
/// (unconditional: `add-account` may have adopted the slot before failing),
/// then — only for a login that passed — the flag re-check through
/// [`crate::runtime::ThreadManager::note_login_restored_checked`], which clears
/// on a verified identity and keeps on anything else.
pub async fn complete_daemon_login(
    manager: &crate::runtime::ThreadManager,
    session_id: &str,
    code: &str,
) -> Result<DaemonLogin> {
    let done = manager.logins.complete(session_id, code).await?;
    manager.reload_pool();
    let (recheck, reopened) = if done.ok {
        let (r, o) = manager.note_login_restored_checked(&done.account);
        (Some(r), o)
    } else {
        (None, Vec::new())
    };
    Ok(DaemonLogin { done, reopened, recheck })
}

/// **A ROTATION PASS, with its progress on the event stream.**
///
/// `claude-accounts rotate --json` prints ONE structured blob when the whole
/// pass finishes, and writes a line per slot to `accounts/rotation.log` as it
/// goes. The interface wanted both, and got the second one by TAILING THAT
/// FILE: it captured a byte offset before starting, seeked to it, read
/// incrementally every 150 milliseconds, and regex-parsed another program's
/// sentences (`^\S+\s+(ROLLED|skip|MANUAL|ERROR|same|dry|\?)\s+…`) into
/// outcomes. It then classified the results by STRING PREFIX and decided which
/// accounts needed a manual login with `"needs" in status`.
///
/// Two things are wrong with that and only one of them is the parsing. The
/// other is that `rotation.log` is a SHARED, APPEND-ONLY file: a rotation
/// started from the CLI at the same time interleaves into the same tail, and
/// the client attributes those lines to its own run.
///
/// So the pass is driven here instead:
///
/// * The log is pointed at a PRIVATE file through the script's own
///   `CLAUDE_ACCOUNTS_LOG` seam, so this run's lines belong to this run and
///   nothing else can interleave into them.
/// * Each line becomes a [`crate::event::Push::Progress`] frame with a
///   MACHINE-READABLE `phase`. The prose translation happens here, once, beside
///   the script whose format it tracks — rather than in a client that has no
///   way to know the format changed.
/// * The OUTCOME is the `--json` blob, not the log. The log is progress;
///   `--json` is the verdict, and the two are no longer the same channel.
pub async fn rotate(
    script: &std::path::Path,
    threads_home: &std::path::Path,
    run_id: &str,
    dry_run: bool,
    mut on_progress: impl FnMut(&str, String),
) -> Result<(bool, serde_json::Value, String)> {
    if !script.exists() {
        return Err(Error::Config(format!(
            "the accounts script is not at {} — rotation drives that script rather than \
             reimplementing its keychain handling",
            script.display()
        )));
    }
    // The PRIVATE log for this pass. `claude-accounts` reads
    // `CLAUDE_ACCOUNTS_LOG` as a seam and appends there instead of to the
    // shared `accounts/rotation.log`, which is what makes these lines
    // attributable to THIS run and not to a pass the operator started from the
    // CLI a second earlier.
    //
    // Under the data dir rather than `/tmp`, and named for the run: if a pass
    // dies mid-way the file is still there to be read, which is the one
    // situation where somebody actually wants it.
    let dir = threads_home.join("accounts").join(".rotate-runs");
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Config(format!("creating {}: {e}", dir.display())))?;
    let log = dir.join(format!("{run_id}.log"));
    std::fs::write(&log, "")
        .map_err(|e| Error::Config(format!("creating {}: {e}", log.display())))?;

    let mut cmd = tokio::process::Command::new(script);
    cmd.arg("rotate").arg("--json");
    if dry_run {
        cmd.arg("--dry-run");
    }
    cmd.current_dir(threads_home)
        .env("THREADS_HOME", threads_home)
        .env("CLAUDE_ACCOUNTS_LOG", &log)
        .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("CLAUDE_CONFIG_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| Error::Config(format!("spawning {}: {e}", script.display())))?;
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");

    let errors = Arc::new(Mutex::new(String::new()));
    let sink = errors.clone();
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match stderr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => sink
                    .lock()
                    .await
                    .push_str(&String::from_utf8_lossy(&buf[..n])),
            }
        }
    });

    // Read stdout to completion while polling the private log for progress.
    let mut out = String::new();
    let mut buf = [0u8; 8192];
    let mut consumed = 0usize;
    loop {
        // Drain whatever the pass has written since the last look.
        if let Ok(text) = std::fs::read_to_string(&log) {
            if text.len() > consumed {
                for line in text[consumed..].lines() {
                    if let Some((phase, detail)) = classify_rotation_line(line) {
                        on_progress(phase, detail);
                    }
                }
                consumed = text.len();
            }
        }
        match tokio::time::timeout(Duration::from_millis(200), stdout.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => out.push_str(&String::from_utf8_lossy(&buf[..n])),
            Ok(Err(e)) => return Err(Error::Config(format!("reading the rotation output: {e}"))),
            // A tick with nothing on stdout is the normal case: a slot probe
            // takes seconds and says nothing until the whole pass ends. The
            // loop exists to keep reading the LOG during exactly that silence.
            Err(_) => continue,
        }
    }
    if let Ok(text) = std::fs::read_to_string(&log) {
        for line in text[consumed.min(text.len())..].lines() {
            if let Some((phase, detail)) = classify_rotation_line(line) {
                on_progress(phase, detail);
            }
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| Error::Config(format!("waiting for the rotation pass: {e}")))?;
    let errs = errors.lock().await.clone();
    // The progress log has served its purpose the moment the pass ends: the
    // VERDICT is the JSON below, and a stale log left behind would be a second
    // source of truth for anyone who found it. Kept only when the pass failed.
    if status.success() {
        let _ = std::fs::remove_file(&log);
    }

    // THE VERDICT IS THE JSON, and its absence is a failure rather than an
    // empty success. A pass that printed nothing parseable did not rotate
    // anything, and reporting `{}` as a result would read as "nothing needed
    // doing".
    let parsed: serde_json::Value = serde_json::from_str(out.trim()).map_err(|_| {
        Error::Config(format!(
            "the rotation pass produced no parseable JSON result. It said: {}",
            if errs.trim().is_empty() {
                out.trim()
            } else {
                errs.trim()
            }
        ))
    })?;
    Ok((status.success(), parsed, errs))
}

/// One `rotation.log` line → a machine-readable phase and a human detail.
///
/// The translation lives HERE, once, beside the script whose format it tracks —
/// not in a client that cannot know when that format moved. A caller switches
/// on the phase and never reads the prose to learn an outcome.
///
/// Lines are `<rfc3339> <word> <label> — <note>`. An unrecognised line is
/// forwarded as `note` rather than dropped: a line this function does not
/// understand is exactly the line worth showing a human.
fn classify_rotation_line(line: &str) -> Option<(&'static str, String)> {
    let rest = line.split_once(' ').map(|(_, r)| r).unwrap_or(line).trim();
    if rest.is_empty() {
        return None;
    }
    let word = rest.split_whitespace().next().unwrap_or("");
    let phase = match word {
        "rotate:" => {
            if rest.contains("start") {
                "started"
            } else {
                "finished"
            }
        }
        "ROLLED" => "rotated",
        "skip" => "skipped",
        "MANUAL" => "needs_login",
        "ERROR" => "error",
        "same" => "unchanged",
        "dry" => "dry_run",
        "!!" => "alarm",
        _ => "note",
    };
    Some((phase, rest.to_string()))
}

/// The URL out of the child's output.
///
/// Anchored on the literal line `claude` prints when no browser opened, rather
/// than on "the first https:// in the output": the banner and any diagnostic
/// may also contain links, and picking the wrong one would send the operator to
/// a page that authorizes nothing.
fn find_url(s: &str) -> Option<String> {
    let line = s.lines().find(|l| l.contains(URL_LINE))?;
    let idx = line.find("https://")?;
    let url = line[idx..].trim();
    // Only complete when the line has ended — a URL still being written is a
    // truncated URL, and a truncated authorize link fails in a way that looks
    // like the operator's fault.
    if s.split_once(URL_LINE).map(|(_, rest)| rest.contains('\n')) == Some(true) {
        Some(url.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_url_is_taken_from_the_line_that_names_it() {
        let out = "Opening browser to sign in…\n\
                   If the browser didn't open, visit: https://claude.com/cai/oauth/authorize?code=true&state=X\n\
                   Paste code here if prompted > ";
        assert_eq!(
            find_url(out).as_deref(),
            Some("https://claude.com/cai/oauth/authorize?code=true&state=X")
        );
    }

    #[test]
    fn a_half_written_url_is_not_a_url() {
        // The read loop can wake mid-line. A truncated authorize link fails in
        // a way that looks like the operator pasted something wrong.
        let partial = "If the browser didn't open, visit: https://claude.com/cai/oauth/auth";
        assert_eq!(find_url(partial), None);
    }

    #[test]
    fn a_link_somewhere_else_in_the_output_is_not_the_login_url() {
        let out = "see https://docs.example.com/help for details\nPaste code here > ";
        assert_eq!(find_url(out), None, "only the line that names it counts");
    }
}
