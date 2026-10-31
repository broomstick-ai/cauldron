//! How `HOME` and `ENV` compose, asserted on values — including both set, and
//! including the refusals.
//!
//! These tests mutate the process environment, so they run in ONE test
//! function, in order, rather than in parallel.

use env::paths::{Source, RESERVED_SERVICE_NAMES};
use env::Environment;

fn clear() {
    for k in ["CAULDRON_HOME", "CAULDRON_ENV", "ENV"] {
        std::env::remove_var(k);
    }
    std::env::set_var("HOME", "/tmp/cauldron-test-home");
}

#[test]
fn home_and_env_compose() {
    // The REAL home, read before `clear()` repoints HOME — step 7 needs it.
    let real_home = std::env::var("HOME").expect("HOME");
    assert!(real_home.starts_with('/') && real_home != "/tmp/cauldron-test-home", "SENTINEL: {real_home:?}");
    clear();

    // 1. Neither knob: $HOME/.cauldron, environment `prod`. Production is just
    //    the default NAME of an environment, with no special case anywhere.
    let e = Environment::resolve().unwrap();
    assert_eq!(e.root.display().to_string(), "/tmp/cauldron-test-home/.cauldron");
    assert_eq!(e.name, "prod");
    assert_eq!(e.root_source, Source::Home);
    assert_eq!(e.name_source, Source::Default);
    assert_eq!(e.data_root.display().to_string(), "/tmp/cauldron-test-home/.cauldron/data/prod");
    println!("1. neither:        {}", e.banner());

    // 2. ENV alone segments data/ inside the same root.
    std::env::set_var("ENV", "dev");
    let e = Environment::resolve().unwrap();
    assert_eq!(e.root.display().to_string(), "/tmp/cauldron-test-home/.cauldron");
    assert_eq!(e.name, "dev");
    assert_eq!(e.name_source, Source::Env);
    assert_eq!(e.data_root.display().to_string(), "/tmp/cauldron-test-home/.cauldron/data/dev");
    println!("2. ENV=dev:        {}", e.banner());

    // 3. HOME alone relocates the whole root; the environment stays `prod`.
    std::env::remove_var("ENV");
    std::env::set_var("HOME", "/tmp/elsewhere");
    let e = Environment::resolve().unwrap();
    assert_eq!(e.data_root.display().to_string(), "/tmp/elsewhere/.cauldron/data/prod");
    println!("3. HOME=/tmp/elsewhere: {}", e.banner());

    // 4. BOTH SET: they NEST. HOME picks the house, ENV picks the room, and
    //    neither overrides the other.
    std::env::set_var("ENV", "dev");
    let e = Environment::resolve().unwrap();
    assert_eq!(e.root.display().to_string(), "/tmp/elsewhere/.cauldron");
    assert_eq!(e.name, "dev");
    assert_eq!(e.data_root.display().to_string(), "/tmp/elsewhere/.cauldron/data/dev");
    assert_eq!(e.root_source, Source::Home);
    assert_eq!(e.name_source, Source::Env);
    println!("4. BOTH:           {}", e.banner());

    // 5. CAULDRON_HOME wins over HOME, and relocates the cauldron root ONLY.
    std::env::set_var("CAULDRON_HOME", "/tmp/explicit");
    let e = Environment::resolve().unwrap();
    assert_eq!(e.root.display().to_string(), "/tmp/explicit");
    assert_eq!(e.root_source, Source::CauldronHome);
    assert_eq!(e.data_root.display().to_string(), "/tmp/explicit/data/dev");
    println!("5. CAULDRON_HOME:  {}", e.banner());

    // 6. CAULDRON_ENV wins over ENV.
    std::env::set_var("CAULDRON_ENV", "other");
    let e = Environment::resolve().unwrap();
    assert_eq!(e.name, "other");
    assert_eq!(e.name_source, Source::CauldronEnv);
    assert_eq!(e.data_root.display().to_string(), "/tmp/explicit/data/other");
    println!("6. CAULDRON_ENV:   {}", e.banner());

    // 7. RED: a root pointed at production is refused at the first step —
    //    the REAL old store under the REAL home, while HOME is repointed
    //    (REL-4.S4b: the guard derives the home from passwd as well as $HOME,
    //    so a proof that repoints HOME cannot unguard the operator's tree).
    std::env::remove_var("CAULDRON_ENV");
    std::env::set_var(
        "CAULDRON_HOME",
        format!("{real_home}/code/substrate-interface-layer/lib/threads"),
    );
    let err = Environment::resolve()
        .expect_err("a cauldron root inside the live threads store must be refused");
    let text = format!("{err:#}");
    println!("7. RED (root at production):\n    {text}");
    assert!(text.contains("REFUSED"), "{text}");

    // 8. RED: a name that is not one safe path component is refused, not
    //    sanitised. A quietly rewritten name is a quietly wrong data root.
    std::env::set_var("CAULDRON_HOME", "/tmp/explicit");
    for bad in ["../prod", "Dev", "a/b", "with space", "dev/../prod", "a.b"] {
        std::env::set_var("CAULDRON_ENV", bad);
        match Environment::resolve() {
            Ok(e) => panic!("environment name `{bad}` should have been refused, got {}", e.banner()),
            Err(e) => println!("8. RED (name `{bad}`): {e:#}"),
        }
    }

    // 8b. An EMPTY value is treated as unset and falls through to the next
    //     source, rather than being refused. `CAULDRON_ENV=` is how a shell
    //     spells "I did not set this".
    std::env::set_var("CAULDRON_ENV", "");
    std::env::set_var("ENV", "dev");
    let e = Environment::resolve().expect("an empty CAULDRON_ENV falls through to ENV");
    assert_eq!(e.name, "dev");
    assert_eq!(e.name_source, Source::Env);
    println!("8b. empty CAULDRON_ENV falls through to $ENV: {}", e.banner());
    std::env::remove_var("ENV");

    // 8c. RED: `shared` is RESERVED and is refused as an environment name, from
    //     EITHER source. `data/shared/` holds what is deliberately not
    //     per-environment — weights, builds, the build cache — so an environment
    //     resolved onto it would be handed the machine's shared state as its own
    //     data root, and `rm -rf`ing that "environment" would take the weights
    //     and every build with it.
    //
    //     Forced red by deleting the RESERVED_ENV_NAMES check in validate_name:
    //     both arms below then resolve to `<root>/data/shared` and the panic
    //     prints the data root it would have handed out.
    for src in ["CAULDRON_ENV", "ENV"] {
        clear();
        std::env::set_var("CAULDRON_HOME", "/tmp/explicit");
        std::env::set_var(src, "shared");
        match Environment::resolve() {
            Ok(e) => panic!(
                "`shared` must be refused as an environment name (from ${src}), \
                 but it resolved to {}",
                e.data_root.display()
            ),
            Err(e) => {
                let text = format!("{e:#}");
                assert!(text.contains("REFUSED"), "{text}");
                assert!(
                    text.contains("RESERVED"),
                    "the refusal must say the name is reserved, not merely malformed: {text}"
                );
                println!("8c. RED (`shared` from ${src}): {text}");
            }
        }
    }
    clear();

    // 9. GREEN: and it does not refuse every name. This is the other half of 8c
    //    — a guard that refused everything would pass 8c and be worthless, so an
    //    ORDINARY name must still resolve, including names that merely resemble
    //    the reserved one.
    std::env::set_var("CAULDRON_HOME", "/tmp/explicit");
    for good in ["dev", "other", "prod", "stage-2", "e2e_test", "7", "shared-2", "sharedx"] {
        std::env::set_var("CAULDRON_ENV", good);
        let e = Environment::resolve()
            .unwrap_or_else(|err| panic!("name `{good}` should be accepted: {err:#}"));
        assert_eq!(e.name, good);
        println!("9. GREEN (name `{good}`): {}", e.data_root.display());
    }
    clear();
}

/// **THE DECIDED LAYOUT, ASSERTED ON VALUES: `data/<env>/<service>/<kind>/`.**
///
/// This is what production occupies — the live threads store moved to
/// `~/.cauldron/data/prod/threads/store/` on 2026-10-07, with `warnings/` and
/// `links/` beside it. `service_data` used to compute
/// `data/<env>/services/<service>`, an extra level nothing on disk had, so the
/// decided layout and the computed layout were two different layouts.
///
/// Builds its own `Environment` rather than reading the process environment, so
/// it is parallel-safe beside [`home_and_env_compose`].
#[test]
fn service_data_is_the_decided_layout() {
    let e = Environment {
        root: "/tmp/cauldron-layout/.cauldron".into(),
        root_source: Source::Home,
        name: "dev".into(),
        name_source: Source::Env,
        data_root: "/tmp/cauldron-layout/.cauldron/data/dev".into(),
    };

    // 1. data/<env>/<service> — no `services/` level.
    let d = e.service_data("threads").expect("a service data dir must resolve");
    assert_eq!(d.display().to_string(), "/tmp/cauldron-layout/.cauldron/data/dev/threads");
    assert!(!d.to_string_lossy().contains("/services/"), "{}", d.display());
    println!("1. service_data(threads) = {}", d.display());

    // 2. data/<env>/<service>/<kind> — the kinds are the service's, joined onto
    //    the directory it is handed as $CAULDRON_SERVICE_DATA_DIR.
    for kind in ["store", "warnings", "links", "logs"] {
        let k = d.join(kind);
        println!("2. kind `{kind}` = {}", k.display());
        assert_eq!(
            k.display().to_string(),
            format!("/tmp/cauldron-layout/.cauldron/data/dev/threads/{kind}")
        );
    }

    // 3. THE WARNINGS CONTRACT'S DURABLE RECORD lands inside the service's own
    //    directory, as `data/<env>/<service>/<kind>/`. The service joins this
    //    onto $CAULDRON_SERVICE_DATA_DIR and never constructs a path, so this is
    //    the whole of what had to follow the reconciliation.
    let sink = d.join("warnings").join("warnings.jsonl");
    assert_eq!(
        sink.display().to_string(),
        "/tmp/cauldron-layout/.cauldron/data/dev/threads/warnings/warnings.jsonl"
    );
    assert!(sink.starts_with(&e.data_root), "the record must be inside the data root");
    println!("3. warnings record = {}", sink.display());

    // 4. And the shape production actually occupies, spelled out: `prod` is just
    //    the default NAME, so the same function computes it.
    let prod = Environment {
        root: "/Users/testuser/.cauldron".into(),
        root_source: Source::Home,
        name: "prod".into(),
        name_source: Source::Default,
        data_root: "/Users/testuser/.cauldron/data/prod".into(),
    };
    let t = prod.service_data("threads").expect("production's own service dir must resolve");
    assert_eq!(t.display().to_string(), "/Users/testuser/.cauldron/data/prod/threads");
    assert_eq!(
        t.join("store").display().to_string(),
        "/Users/testuser/.cauldron/data/prod/threads/store"
    );
    println!("4. the live store's own path, from the layout: {}", t.join("store").display());

    // 5. RED: a service name that collides with something the environment keeps
    //    directly in its data root is refused. This is the hazard the flattening
    //    created — `services/logs` was harmless, `logs` is the log directory.
    //
    //    The POSITIVE SENTINEL first. The loop below iterates the list, so an
    //    empty list would make it a vacuous green that asserts nothing while
    //    reading as a refusal test. `logs` is named here explicitly so this
    //    stays a test of the one reachable collision rather than a test of
    //    however many entries the list happens to hold.
    assert!(
        !RESERVED_SERVICE_NAMES.is_empty(),
        "RESERVED_SERVICE_NAMES is empty, so the loop below proves nothing"
    );
    assert!(
        RESERVED_SERVICE_NAMES.contains(&"logs"),
        "`logs` is the one name a declaration can legally carry that collides \
         with a data-root sibling; it must be in the list. Got: {RESERVED_SERVICE_NAMES:?}"
    );
    println!("5. sentinel: the list is {RESERVED_SERVICE_NAMES:?} and `logs` is in it");

    for bad in RESERVED_SERVICE_NAMES {
        match e.service_data(bad) {
            Ok(p) => panic!("service name `{bad}` must be refused, but resolved to {}", p.display()),
            Err(err) => {
                let text = format!("{err:#}");
                assert!(text.contains("REFUSED"), "{text}");
                println!("5. RED (service name `{bad}`): {text}");
            }
        }
    }

    // 5b. WHY THE LIST IS ONE ENTRY LONG, asserted rather than asserted-in-a-
    //     comment. `seams.json`, `seams.lock` and `env.toml` are data-root
    //     siblings too, and they used to be in RESERVED_SERVICE_NAMES. They
    //     could never fire: `decl::load` is the only producer of a
    //     `Declaration`, and it refuses a name outside `[A-Za-z0-9_-]` before
    //     any data root is resolved — so a dotted name never reaches
    //     `service_data`. That is the layer that actually protects them, and
    //     this asserts it there instead of carrying three entries that cannot
    //     fire. If this ever stops refusing, the dotted names need to come BACK
    //     into the reserved list and this test says so.
    for dotted in ["seams.json", "seams.lock", "env.toml"] {
        let err = env::decl::load(std::path::Path::new("/tmp/cauldron-layout-no-such-services"), dotted)
            .expect_err("a dotted service name must be refused by the declaration loader");
        let text = format!("{err:#}");
        assert!(
            text.contains("not a usable service name"),
            "`{dotted}` must be refused for its CHARACTER SET, one layer before the \
             reserved-name check; got: {text}"
        );
        println!("5b. refused one layer earlier (`{dotted}`): {text}");
    }
    //     And the converse, so 5b is not passing because the loader refuses
    //     everything: `logs` sails through the character-set filter, which is
    //     exactly why it still needs RESERVED_SERVICE_NAMES.
    let logs_err = format!(
        "{:#}",
        env::decl::load(std::path::Path::new("/tmp/cauldron-layout-no-such-services"), "logs")
            .expect_err("there is no declaration on that path, so this must still be an error")
    );
    assert!(
        !logs_err.contains("not a usable service name"),
        "`logs` must PASS the character-set filter — if it did not, the reserved \
         list would be dead code too; got: {logs_err}"
    );
    println!("5b. GREEN: `logs` passes the character set, so the reserved list is its only guard");

    // 6. GREEN, the other half of 5: it does not refuse every name, including
    //    names that merely resemble a reserved one.
    for good in ["threads", "echo", "tally", "logs-2", "logsx", "env", "seams"] {
        let p = e
            .service_data(good)
            .unwrap_or_else(|err| panic!("service name `{good}` should be accepted: {err:#}"));
        assert_eq!(p, e.data_root.join(good));
        println!("6. GREEN (service name `{good}`): {}", p.display());
    }
}
