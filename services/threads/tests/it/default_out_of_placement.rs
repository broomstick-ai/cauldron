//! **CON-27 — the machine `default` account is never placeable.**
//!
//! `default` is the operator's MACHINE login: the one he gets by typing
//! `claude` in a terminal, stored in `~/.claude.json` with no config dir of its
//! own. The `claude-accounts` CLI is how he points that label at whichever of
//! his logins he wants in front of him. Threads owns exactly TWO logins, each
//! with its own config dir, and neither of them is that one.
//!
//! His words (intent item 70, msg #1949): *"I didn't even want the default
//! account to be in scope of what the threads module has access to. It
//! shouldn't even be on the table… And threads, by the way, should never
//! access my default account."* And his diagnosis of why it went wrong, which
//! is the reason this is a constraint and not a bugfix: *"not clearly
//! documented, not clearly isolated."*
//!
//! # Why a type and not a filter
//!
//! `Registry::ensure_default` runs on EVERY boot, so the row is always there. A
//! check at the placement call site is a check the next placement call site
//! will not have. So placement does not take account names at all: it takes
//! [`threads::accounts::Placeable`], whose only constructor refuses the machine
//! default. An account that cannot be made into a `Placeable` cannot be ranked,
//! cannot be failed over to, and cannot be recovered onto — there is no
//! expressible program that does it.
//!
//! # What this file covers
//!
//! 1. THE SOURCE GUARDS — the escape hatches that must not exist, and the
//!    signatures that must keep their type.
//! 2. THE CONSTRUCTOR — both independent refusals.
//! 3. THE THREE PATHS — ordinary placement, retry/failover, boot recovery.
//! 4. VISIBLE BUT NOT PLACEABLE — the machine default stays listable.

#[path = "support/keychain.rs"]
mod keychain;

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::json;
use threads::accounts::Placeable;
use threads::gauge::Axis;
use threads::settings::ThreadSettings;
use threads::{build_manager, Config, ThreadManager};

// ════════════════════════════════════════════════════════════════════════════
// fixtures
// ════════════════════════════════════════════════════════════════════════════

/// # These were the WRONG VARIABLE NAMES, and this file was spawning the REAL
/// `claude` (fixed 2026-08-26)
///
/// It set `THREADS_FAKE_CLAUDE` and `CLAUDE_HOME_FOR_TESTS`. **The crate reads
/// neither.** `spawn.rs::claude_bin` reads `THREADS_CLAUDE_BIN` and
/// `accounts.rs` reads `THREADS_CLAUDE_HOME`; every other test file in this
/// directory sets those two. With no override, `claude_bin` falls through to the
/// literal `"claude"`, so every turn in this file invoked the operator's real
/// CLI — real network, real credentials, real cost — and the assertions here
/// were reading whatever that produced.
///
/// It went unnoticed because a real turn that fails fast still let the
/// placement assertions land inside `eventually`'s ten-second window. It stopped
/// being invisible the moment login expiry got a ladder of its own: the
/// operator's live login is currently expired, so those real spawns now classify
/// as `LoginExpired`, whose first rung is thirty seconds — correctly, because it
/// is waiting on a human — and nothing lands inside ten seconds any more.
/// `failover_never_selects_the_machine_default` then failed by TIMEOUT.
///
/// So the expectation in that test was never wrong and the bar is not too broad.
/// The bug is here. With the fixture actually bound, the file runs offline and
/// that test goes from an 11.9s failure to a 0.18s pass.
fn set_env() {
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        first_exec::warmed_abs(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake-claude")),
    );
    std::env::set_var(
        "THREADS_CLAUDE_HOME",
        std::env::temp_dir().join("threads-tests-claude-home"),
    );
}

/// A config over `dir` registering each name in `extra` WITH its own config
/// dir. The machine `default` is registered on top of this by `ensure_default`,
/// without one — which is exactly the shape this file is about.
fn config_at(dir: &Path, extra: &[&str]) -> Config {
    let mut accounts = BTreeMap::new();
    for name in extra {
        accounts.insert(name.to_string(), dir.join(name).to_string_lossy().to_string());
    }
    Config {
        data_dir: Some(dir.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

fn settings(cwd: &Path) -> ThreadSettings {
    let mut s = ThreadSettings::new(cwd.to_string_lossy().to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    s
}

fn build(cfg: Config) -> ThreadManager {
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    build_manager(cfg).unwrap()
}

/// Record a session-usage reading for `account`, so placement has something to
/// rank on.
fn observe(m: &ThreadManager, account: &str, pct: f64) {
    m.budgets
        .observe(account, Axis::Session, pct, None, "usage_poll")
        .unwrap();
}

async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..400 {
        if f() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    f()
}

// ════════════════════════════════════════════════════════════════════════════
// 1. THE SOURCE GUARDS
// ════════════════════════════════════════════════════════════════════════════

/// **`Placeable` has exactly one way in, and it is fallible.**
///
/// This is the guard that makes every other test in this file durable. A future
/// author who wants "just a name" will reach for one of these, and each one
/// would quietly restore the machine default to the placement domain:
///
/// - `From<String>` / `From<&str>` — "trust me, this name is fine", which is
///   the exact sentence CON-27 exists to make unsayable.
/// - `FromStr` — the same thing wearing a parse.
/// - `Default` — a `Placeable` naming nothing, which every ranking would then
///   have to defend against.
/// - a public tuple field or a `new_unchecked` — the hatch by its usual names.
///
/// Reading the source for absences rather than testing behaviour is deliberate:
/// these are things that must NOT exist, and a type cannot be asked at runtime
/// what impls it does not have.
#[test]
fn placeable_has_no_infallible_constructor() {
    let src = include_str!("../../src/accounts.rs");
    let start = src
        .find("pub struct Placeable(")
        .expect("Placeable was renamed or its field made public — re-point this test");
    // Everything from the declaration to the end of the file: the impls live
    // below it, and a new one added anywhere after is still caught.
    let body = &src[start..];
    let code: String = body
        .lines()
        .filter(|l| !l.trim_start().starts_with("//") && !l.trim_start().starts_with("///"))
        .collect::<Vec<_>>()
        .join("\n");

    for banned in [
        "impl From<String> for Placeable",
        "impl From<&str> for Placeable",
        "impl From<&String> for Placeable",
        "impl FromStr for Placeable",
        "impl std::str::FromStr for Placeable",
        "impl Default for Placeable",
        "fn new_unchecked",
        "fn from_name",
        "fn from_str_unchecked",
        "pub struct Placeable(pub ",
    ] {
        assert!(
            !code.contains(banned),
            "CON-27: `{banned}` is a way to build a Placeable without proving the account \
             is one threads owns. Placement takes Placeable precisely so that no such \
             sentence exists. If a new construction path is genuinely needed, it must be \
             fallible and it must refuse the machine default — then update this guard."
        );
    }

    // The one constructor, and it is `pub(crate)` — so the sentence "this name
    // is placeable" can only be uttered inside this crate, by `pool.rs`.
    assert!(
        code.contains("pub(crate) fn of(name: &str) -> Self"),
        "CON-27: `Placeable::of` must stay the single constructor and must stay \
         `pub(crate)` — public would let any caller mint one from a bare string"
    );
    assert!(
        !code.contains("pub fn of("),
        "CON-27: `Placeable::of` must not be `pub`"
    );
}

/// **THE PLACEMENT MODULE MUST NOT KNOW THE WORD `default`.**
///
/// This is the 2026-08-27 ruling, and it is the one this whole file was
/// restructured around. `Placeable::new` used to open with
/// `if name == DEFAULT_ACCOUNT { return None }` — a literal reserved-name check
/// — and `budgets.rs` carried a matching `POINTER_LABEL`. Both are deleted:
///
/// > *"I do not want default even in the purview of the system. Why does it
/// > even see that there is a default? … It's a completely separate system, not
/// > even visible to placement."*
///
/// And, standing: *"threads must not know the CONCEPT of a default account,
/// though it knows all three real ones."* A module that refuses `default` by
/// name is a module that knows the concept, so the refusal moved upstream and
/// stopped being a refusal — see `pool.rs`.
///
/// A source guard rather than a behavioural one on purpose: the absence of a
/// concept is not observable at runtime, and the failure mode being prevented
/// is a future author "fixing" a leak by adding the check back.
#[test]
fn the_placement_modules_do_not_name_the_machine_default() {
    for (file, src) in [
        ("budgets.rs", include_str!("../../src/budgets.rs")),
        ("pool.rs", include_str!("../../src/pool.rs")),
    ] {
        // PRODUCTION CODE ONLY — everything above `#[cfg(test)]`. The scope is
        // narrowed deliberately and it is the honest boundary: an in-module
        // test may legitimately construct a store row for any string, and a
        // guard that banned the four characters everywhere would be enforcing
        // spelling rather than the rule. What is being guarded is that no
        // SHIPPED code path branches on the name.
        let src = src.split("#[cfg(test)]").next().unwrap();
        // Comments and doc comments are exempt too: this file, `pool.rs` and
        // `budgets.rs` all EXPLAIN the ruling at length, and explaining it is
        // the opposite of encoding it. What is banned is executable code.
        let code: String = src
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !t.starts_with("//") && !t.starts_with("///") && !t.starts_with("//!")
            })
            .collect::<Vec<_>>()
            .join("\n");
        for banned in ["POINTER_LABEL", "DEFAULT_ACCOUNT", "\"default\""] {
            assert!(
                !code.contains(banned),
                "the operator's ruling: threads must not hold the CONCEPT of a default \
                 account. `{banned}` appears in executable code in {file}. The machine \
                 default is kept out by NOT BEING A POOL MEMBER — a `claude_account` entry \
                 must name the directory its login is stored in, and that account names \
                 none. If something is leaking, fix the pool, do not add a name check."
            );
        }
    }

    // …and the constructor specifically, which is where the old check lived.
    let accounts = include_str!("../../src/accounts.rs");
    let start = accounts.find("pub struct Placeable(").expect("Placeable");
    let ctor = &accounts[start..start + 2000];
    assert!(
        !ctor.contains("DEFAULT_ACCOUNT"),
        "CON-27: `Placeable`'s constructor must not carry a reserved-name check"
    );
}

/// **The placement domain keeps its type.**
///
/// The three signatures that carry the constraint. If any of them slips back to
/// `String`, the machine default becomes expressible everywhere downstream and
/// every behavioural test in this file starts passing for the wrong reason.
#[test]
fn the_placement_signatures_take_placeable_not_string() {
    let budgets = include_str!("../../src/budgets.rs");
    for (needle, what) in [
        ("pub accounts: &'a [Placeable]", "AdmissionRequest::accounts"),
        ("pub candidates: &'a [Placeable]", "PlacementRequest::candidates"),
        (
            "pub fn place(&self, req: &PlacementRequest<'_>) -> Result<Option<Placeable>>",
            "Budgets::place",
        ),
        ("Admit { switch_to: Option<Placeable> }", "Admission::Admit"),
    ] {
        assert!(
            budgets.contains(needle),
            "CON-27: {what} must be typed in Placeable, not String — expected to find \
             `{needle}` in budgets.rs"
        );
    }

    let runtime = include_str!("../../src/runtime.rs");
    for (needle, what) in [
        ("pub fn selectable_pool(&self) -> Vec<Placeable>", "selectable_pool"),
        ("pub fn placement_pool(&self) -> Vec<Placeable>", "placement_pool"),
        (
            "fn failover_target(&self, req: &AdmissionRequest<'_>, cause: ParkCause) -> Option<Placeable>",
            "failover_target",
        ),
    ] {
        assert!(
            runtime.contains(needle),
            "CON-27: {what} must be typed in Placeable, not String — expected to find \
             `{needle}` in runtime.rs"
        );
    }
}

/// **The admission gate is handed the PLACEABLE pool, never the visible one.**
///
/// `account_pool()` is the visible list and still contains the machine default
/// on purpose. Handing it to `admit` is the single edit that would undo CON-27
/// while leaving every type in place, so it is banned by name inside
/// `admit_or_park`.
#[test]
fn the_admission_gate_is_not_handed_the_visible_pool() {
    let src = include_str!("../../src/runtime.rs");
    let start = src
        .find("    fn admit_or_park(")
        .expect("admit_or_park was renamed — re-point this test");
    let body = &src[start..];
    let end = body
        .find("\n    fn ")
        .map(|i| i + 1)
        .and_then(|i| body[i..].find("\n    fn ").map(|j| i + j))
        .unwrap_or(body.len());
    let body = &body[..end];
    let code: String = body
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        code.contains("self.placement_pool()"),
        "CON-27: admit_or_park must build its pool from placement_pool()"
    );
    assert!(
        !code.contains("self.account_pool()"),
        "CON-27: admit_or_park must NOT be handed account_pool() — that is the VISIBLE \
         list and it contains the machine default. Use placement_pool()."
    );
}

// ════════════════════════════════════════════════════════════════════════════
// 2. THE CONSTRUCTOR — two independent refusals
// ════════════════════════════════════════════════════════════════════════════

/// **AN ACCOUNT THAT NAMES NO LOGIN DIRECTORY IS NOT A POOL ENTRY — under any
/// name, including one that is not `default`.**
///
/// This is the single refusal that replaced the old pair, and it is phrased
/// without the word `default` on purpose. An account with no login directory
/// spawns with `CLAUDE_CONFIG_DIR` unset, which is `~/.claude` — the operator's
/// own terminal login. The machine default is the account that names none; so
/// is a second machine-default row under any other label; both fall out of the
/// same rule and neither is named by it.
///
/// **THE MUTATION.** Relax `ClaudeAccount::new` to accept an empty
/// `login_dir` — the "obvious simplification" a future author reaches for —
/// and this test goes red, as does
/// `the_machine_default_is_not_in_the_pool_at_all` below.
#[test]
fn an_entry_that_names_no_login_directory_does_not_exist() {
    use threads::pool::{ClaudeAccount, PoolEntry};

    assert!(
        ClaudeAccount::new("").is_none(),
        "an entry with no login directory would spawn on ~/.claude — the operator's own \
         login. It must not be constructible."
    );
    for name in ["default", "looks-legitimate", "owner-account"] {
        assert!(
            PoolEntry::claude_account(name, "").is_none(),
            "{name:?} with no login directory is not a pool entry — and the rule does not \
             consult the name"
        );
    }

    // …and the positive case, so the refusals above are not vacuous. Note the
    // name `default` IS accepted when it names a real directory: the module has
    // no opinion about the word, which is exactly the ruling. It cannot arrive
    // that way from the registry, because the machine-default slot is the one
    // with no directory.
    let ok = PoolEntry::claude_account(
        "owner-account",
        "/tmp/threads-test/owner-account",
    )
    .expect("an entry that names its login directory is a pool member");
    assert_eq!(ok.placeable().name(), "owner-account");
    assert_eq!(
        ok.login_dir(),
        Some("/tmp/threads-test/owner-account"),
        "the entry carries WHERE ITS LOGIN IS STORED as an explicit property"
    );
}

/// **The machine default is not in the pool, and it is not filtered out of
/// it — it was never a member.**
///
/// Derived straight from a registry shaped exactly like the operator's own
/// `accounts.toml`: three real accounts with their own directories, and the
/// `[accounts.default]` row with none.
///
/// **THE MUTATION.** Change `Pool::from_registry` to fall back to a placeholder
/// directory for a row that names none (`unwrap_or("~/.claude")`, the plausible
/// "be helpful" edit) and this goes red on the first assertion.
#[test]
fn the_machine_default_is_not_in_the_pool_at_all() {
    use threads::accounts::Registry;
    use threads::pool::Pool;

    let mut reg = Registry::default();
    reg.add("alice-example-com", Some("/tmp/acct/cc1".into()));
    reg.add("owner-account", Some("/tmp/acct/sam".into()));
    reg.add("owner-gmail", Some("/tmp/acct/gmail".into()));
    // The row the `claude-accounts` CLI maintains, exactly as it appears on his
    // machine: present, and naming no directory.
    reg.add("default", None);

    let pool = Pool::from_registry(&reg);
    assert!(
        !pool.contains("default"),
        "the machine default is not a pool member: {:?}",
        pool.names()
    );
    assert_eq!(
        pool.names(),
        vec![
            "alice-example-com".to_string(),
            "owner-account".to_string(),
            "owner-gmail".to_string()
        ],
        "he knows all three real ones"
    );
    assert_eq!(
        pool.login_dir("default"),
        None,
        "and there is no login directory to spawn it with"
    );
    // The registry still has it. The pool is not the registry with a row
    // removed; it is a different list from a different question.
    assert!(reg.get("default").is_some());
}

// ════════════════════════════════════════════════════════════════════════════
// 3. THE THREE PATHS
// ════════════════════════════════════════════════════════════════════════════

/// **PATH 1 — ordinary placement never returns the machine default, even when
/// it is the only account with headroom.**
///
/// The readings are rigged so that every honest ranking WANTS the machine
/// default: it sits at 1% while the one account threads owns is at 97%. The
/// gauge is not what keeps it out — the type is.
#[tokio::test]
async fn placement_never_returns_the_machine_default_even_with_all_the_headroom() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build(config_at(tmp.path(), &["owned-one"]));

    observe(&m, "default", 1.0);
    observe(&m, "owned-one", 97.0);

    // The pool it ranks over cannot even name the machine default.
    let pool = m.selectable_pool();
    assert!(
        pool.iter().all(|p| p.name() != "default"),
        "CON-27: the machine default is absent from the placement pool: {pool:?}"
    );

    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    assert_eq!(
        m.serving_account(&id).unwrap(),
        "owned-one",
        "CON-27: placement took the 97% account it owns over the 1% machine login"
    );
}

/// **PATH 1b — a PIN on the machine default is refused, and degrades to an
/// ordinary placement rather than to an error.**
///
/// A pin is the one form of "placement" a human writes by hand, so it is the
/// one that has to be refused by hand: it never reaches the ranking, so the
/// type alone cannot catch it. Degrading rather than erroring is deliberate —
/// refusing to create the thread would turn a bad setting into a dead daemon.
#[tokio::test]
async fn a_pin_on_the_machine_default_is_refused_and_placed_automatically() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build(config_at(tmp.path(), &["owned-one"]));

    let mut s = settings(tmp.path());
    s.account = threads::settings::AccountChoice::Named("default".to_string());
    let id = m.create_thread(None, s).unwrap();

    assert_eq!(
        m.serving_account(&id).unwrap(),
        "owned-one",
        "CON-27: a pin naming the machine default is refused and the thread is placed"
    );
}

/// **PATH 2 — failover never selects the machine default.**
///
/// The account the thread is on hits a ceiling and there is genuinely nowhere
/// to go except the machine default, which is wide open. The thread must park
/// rather than move there. Parking is the correct answer: "nowhere to go" is a
/// wait, and the machine default is not a place to go.
#[tokio::test]
async fn failover_never_selects_the_machine_default() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build(config_at(tmp.path(), &["owned-one"]));
    m.start_watchers();

    let mut s = settings(tmp.path());
    s.thresholds.session = Some(50.0);
    let id = m.create_thread(None, s).unwrap();
    assert_eq!(m.serving_account(&id).unwrap(), "owned-one");

    // One turn, so the thread has a conversation worth protecting.
    m.send(&id, "hello".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();
    assert!(eventually(|| !m.store.thread_accounts(&id).unwrap().is_empty()).await);

    // The account it is on trips. The machine default is wide open — and is
    // still not a destination.
    observe(&m, "owned-one", 96.0);
    observe(&m, "default", 1.0);

    m.send(&id, "and again".into(), threads::SendIntent::Immediate, json!({}), false)
        .unwrap();

    assert!(
        eventually(|| m.park_state(&id).unwrap() != serde_json::Value::Null).await,
        "CON-27: with only the machine default free the thread PARKS, it does not move"
    );
    assert_eq!(
        m.serving_account(&id).unwrap(),
        "owned-one",
        "CON-27: failover must not have moved this thread to the machine default"
    );
    assert!(
        !m.store
            .thread_accounts(&id)
            .unwrap()
            .contains(&"default".to_string()),
        "CON-27: no conversation may be ported onto the machine login"
    );
}

/// **PATH 3 — boot recovery never ASSIGNS the machine default.**
///
/// Two halves, and the distinction between them is the whole reason recovery is
/// safe.
///
/// A thread with no account yet is placed on an account threads owns, at boot,
/// exactly like any other placement.
///
/// A thread ALREADY sitting on the machine default — and there are 118 of them
/// on the operator's machine — is LEFT there by recovery. Staying somewhere is
/// not being placed there. Moving its account column at boot would move it
/// WITHOUT carrying its transcript, which is silent reassignment and is exactly
/// how a conversation gets stranded (DEC-56). It is evicted one layer up
/// instead, by the admission gate, which ports the conversation properly on its
/// next turn.
#[tokio::test]
async fn boot_recovery_never_assigns_the_machine_default() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config_at(tmp.path(), &["owned-one"]);
    let m = build(cfg.clone());

    // Half A: a fresh thread, recovered across a restart, lands on an account
    // threads owns — never on the machine default, whatever the readings say.
    observe(&m, "default", 1.0);
    observe(&m, "owned-one", 99.0);
    let fresh = m.create_thread(None, settings(tmp.path())).unwrap();
    assert_eq!(m.serving_account(&fresh).unwrap(), "owned-one");

    // Half B: forge the live situation — a thread stranded on the machine
    // default, exactly as 118 of the operator's threads are.
    let stranded = m.create_thread(None, settings(tmp.path())).unwrap();
    let mut row = m.store.get_thread(&stranded).unwrap().unwrap();
    row.account = Some("default".to_string());
    m.store.upsert_thread(&row).unwrap();

    // THE RESTART: a brand-new manager over the same data dir.
    let m2 = build(cfg);

    assert_eq!(
        m2.serving_account(&fresh).unwrap(),
        "owned-one",
        "CON-27: recovery re-placed nothing onto the machine default"
    );
    assert_eq!(
        m2.serving_account(&stranded).unwrap(),
        "default",
        "CON-27: recovery LEAVES a stranded thread where it is — moving it here would \
         reassign its account without carrying its transcript (DEC-56). The admission \
         gate ports it off on its next turn."
    );
    assert!(
        !m2.is_placeable("default"),
        "CON-27: and it is still not somewhere anything may be PLACED"
    );
}

/// **PATH 2b — the machine default never wakes a parked prompt.**
///
/// A parked prompt is reconsidered whenever anything could change its placement.
/// That question used to be asked by `retry_unblocked_early` over the VISIBLE
/// registry, so the machine default — registered on every boot, perfectly healthy,
/// never barred — always answered "yes, I am free". Every account-picture change
/// therefore woke every parked thread on the strength of an account the gate cannot
/// place a thread on, so the wake could only ever be refused.
///
/// This is the leak that the `Placeable` type did NOT catch on its own, and the
/// reason is worth naming: that site asked a QUESTION about accounts rather than
/// choosing one, so nothing there ever had to be a `Placeable`. A boolean cannot be
/// type-checked into correctness — only the set it is computed over can.
///
/// # Re-grounded 2026-10-06
///
/// `retry_unblocked_early` is GONE, along with the per-thread floor it lifted. The
/// question it asked is now asked once, read-only and pre-lock, by
/// `early_ask_would_move` — which reads `placement_pool()` and so cannot see the
/// machine default at all. CON-27's edge therefore holds here by construction
/// rather than by a second careful reading, and this test asserts it where it is now
/// observable: the pool reconsiders this prompt (it reconsiders everything), the
/// gate finds nowhere to put it, and nothing is spent.
///
/// FORCED RED, run and recorded: pointing `early_ask_would_move`'s `accounts` at
/// `self.registry.names()` instead of `self.placement_pool()` — the exact shape of
/// the original leak — makes the gate believe the machine default is a destination,
/// the ask takes the lock, the ungated thread runs anyway, and the retry row is
/// cleared: `CON-27: nothing may wake this prompt on the strength of the machine
/// default … got None`.
#[tokio::test]
async fn the_machine_default_never_wakes_a_parked_prompt() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    // ONE placeable account, and the machine default beside it. The parked
    // thread sits on the only account threads owns, so the machine default is
    // the ONLY other login that could possibly answer "I am free".
    let m = build(config_at(tmp.path(), &["owned-one"]));

    let id = m.create_thread(None, settings(tmp.path())).unwrap();
    assert_eq!(m.serving_account(&id).unwrap(), "owned-one");

    // Park a prompt on its own account. There is no instant on this row to be
    // early about any more: the pool reconsiders it whatever happens.
    m.store
        .upsert_retry(&threads::store::RetryRow {
            thread_id: id.clone(),
            prompt: "the operator's stranded prompt".into(),
            message_ids: vec![1],
            attempt: 2,
            kind: "usage_limit".into(),
            reason: "rate limit (429)".into(),
            account: Some("owned-one".into()),
            created_at: String::new(),
        })
        .unwrap();

    // THE PREMISES, both of them. The default is healthy and unbarred — so a reading
    // over the VISIBLE registry would call it free — and it is not placeable.
    assert!(
        !m.is_placeable("default"),
        "the premise: the machine default is not somewhere a thread may be PLACED"
    );
    let (unavailable, unhealthy) = m.barred_accounts();
    assert!(
        !unavailable.iter().any(|n| n == "default") && !unhealthy.iter().any(|n| n == "default"),
        "and the other premise: it is not barred either, so only the PLACEABLE set \
         can be what excludes it — got unavailable={unavailable:?} unhealthy={unhealthy:?}"
    );

    // THE POOL IS RECONSIDERED — every row, as always — and the gate finds nowhere.
    assert_eq!(
        m.retries_to_reconsider()
            .iter()
            .filter(|r| r.thread_id == id)
            .count(),
        1,
        "the pool asks about every parked prompt, including this one: a selection \
         that skipped it would make the assertion below vacuous"
    );
    m.retry_sweep(threads::runtime::PassOrigin::Event);
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    let after = m
        .store
        .get_retry(&id)
        .unwrap()
        .expect("CON-27: nothing may wake this prompt on the strength of the machine default");
    assert_eq!(
        after.attempt, 2,
        "CON-27: the machine default is healthy and unbarred, but it is not somewhere \
         this thread could be placed — so no ask may spend an attempt on it. Before \
         the fix every account-picture change woke this thread into a refusal."
    );
    assert_eq!(
        after.prompt, "the operator's stranded prompt",
        "and the prompt is still exactly where it was"
    );
    assert_eq!(
        m.serving_account(&id).unwrap(),
        "owned-one",
        "and the thread was not moved onto the machine default"
    );
    assert!(
        m.store.get_parked_turn(&id).unwrap().is_none(),
        "nor handed over: a question's refusal keeps the prompt in `retries`"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// 4. VISIBLE BUT NOT PLACEABLE
// ════════════════════════════════════════════════════════════════════════════

/// **THE VISIBLE/PLACEABLE SPLIT IS GONE, and its disappearance is the ruling.**
///
/// This test used to assert the OPPOSITE: that `account_pool()` returns three
/// labels including `default`, because *"the operator manages it and has to be
/// able to see what he is choosing"*. He overruled that on 2026-08-27 —
/// *"I do not want default even in the purview of the system. Why does it even
/// see that there is a default?"* — so the daemon's visible list and its
/// placeable list are now the SAME list, and the only thing the wider one ever
/// carried was the machine default.
///
/// He still chooses which login sits in that slot. He does it with
/// `claude-accounts`, through `threads accounts health`, which is the slot view
/// and is deliberately untouched. That is the *separate utility*: *"a carve out
/// specifically to allow me to use cloud code on my machine… a separate utility
/// that just creates a duplicate login for me."*
#[test]
fn the_machine_default_is_not_visible_to_the_daemon_at_all() {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = build(config_at(tmp.path(), &["owned-one", "owned-two"]));

    let visible = m.account_pool();
    assert!(
        !visible.contains(&"default".to_string()),
        "the daemon's account list must not carry the machine default: {visible:?}"
    );
    assert_eq!(visible, vec!["owned-one".to_string(), "owned-two".to_string()]);

    // The visible list and the placeable list are now the same membership.
    let placeable: Vec<String> = m
        .placement_pool()
        .iter()
        .map(|p| p.name().to_string())
        .collect();
    assert_eq!(placeable, visible, "there is no wider 'visible' pool any more");

    // Including the surface the /usage PROBE iterates — the one that used to
    // spawn `claude` against `~/.claude` on a timer.
    assert!(
        !m.poll_targets().contains(&"default".to_string()),
        "the probe must never spawn against the operator's own login: {:?}",
        m.poll_targets()
    );

    // THE ACTIVITY ROLL-UP — the source of `accounts status`, of the
    // `accounts.status` wire payload, and through those of the bar rows the
    // `claude-accounts` TUI draws. This is where the machine default was most
    // visible to him, and where it survived the renderer's own `folds_into`
    // filter on the two branches where the machine login is nobody's alias.
    //
    // MUTATION: inject a `default` row at the end of `activity_by_account`'s
    // seeding loop and this goes red. Without this assertion that mutation
    // survives — it did, on the first mutation pass.
    let rows = threads::accounts::collect_activity(
        &m.store,
        &m.pool(),
        &std::collections::BTreeMap::new(),
        None,
        1000,
    )
    .unwrap();
    let accounts: Vec<&str> = rows.iter().map(|r| r.account.as_str()).collect();
    assert!(
        !accounts.contains(&"default"),
        "the machine default must not be a row in `accounts status` or on the wire, \
         which is where the bar rows come from: {accounts:?}"
    );
    assert_eq!(accounts, vec!["owned-one", "owned-two"]);

    assert!(!m.is_placeable("default"));
    assert!(m.is_placeable("owned-one"));
    assert!(m.is_placeable("owned-two"));
    assert!(
        !m.is_placeable("never-registered"),
        "an account nobody registers is not placeable either"
    );
}

/// **The park reason names the real problem when nothing is placeable.**
///
/// The machine default IS registered and always will be — `ensure_default` runs
/// every boot — so a message saying "no accounts are registered" would send the
/// operator to add an account he can already see in `accounts list`. The text
/// has to distinguish registered from placeable, because after CON-27 those are
/// genuinely different things.
#[test]
fn an_empty_placement_pool_says_placeable_not_registered() {
    let src = include_str!("../../src/budgets.rs");
    assert!(
        src.contains("no placeable account is registered"),
        "CON-27: the empty-pool park must say PLACEABLE, not 'no accounts are registered' — \
         the machine default is registered and is not the answer"
    );

    let runtime = include_str!("../../src/runtime.rs");
    assert!(
        runtime.contains("no placeable account is registered"),
        "CON-27: the create-time failure must say the same thing"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
