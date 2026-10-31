//! tts is a cauldron SERVICE now: it registers its own seam, answers the
//! supervisor's two words, serves the warnings contract by call site, and
//! says where its websocket is. Each of these was a gap in the hosted row
//! the substrate daemon got (`registered = false`, no boot token, no
//! warnings, `live:unverified` at best).

mod common;

use std::io::{BufRead, BufReader, Write};

use env::seams::{SeamState, ServiceEnv, Table};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

/// A service environment under a temp data root, as `cauldron start` builds.
fn service_env(root: &std::path::Path, token: &str) -> ServiceEnv {
    let data_root = root.join("data/t");
    std::fs::create_dir_all(data_root.join("tts")).unwrap();
    let data_root = data_root.canonicalize().unwrap();
    ServiceEnv {
        service: "tts".into(),
        env: "t".into(),
        data_dir: data_root.join("tts"),
        seams: data_root.join("seams.json"),
        data_root,
        boot_token: token.into(),
        version: "test-label".into(),
        commit: "0123456789abcdef0123456789abcdef01234567".into(),
        dirty: false,
        bind: "127.0.0.1:0".into(),
    }
}

fn say(addr: &str, line: &str) -> String {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    writeln!(s, "{line}").unwrap();
    let mut out = String::new();
    BufReader::new(s).read_line(&mut out).unwrap();
    out.trim().to_string()
}

#[tokio::test]
async fn the_daemon_registers_its_own_seam_and_answers_the_supervisor() {
    let _ = tracing_subscriber::registry()
        .with(tts::seam::WarningsLayer.with_filter(tracing_subscriber::filter::LevelFilter::WARN))
        .try_init();
    let (_m, ws, _t) = common::daemon(common::TWO_KEYS, None).await;
    let root = tempfile::tempdir().unwrap();
    let se = service_env(root.path(), "boot-token-1");
    let (env_name, data_root, seams) = (se.env.clone(), se.data_root.clone(), se.seams.clone());
    let addr = tts::seam::start(se, ws, "1700000000".into()).unwrap();

    // THE ROW: written by the service itself, carrying this start's token.
    let table = Table::load(&env_name, &data_root, &seams).unwrap();
    let row = table.seams.get("tts").expect("tts wrote its row");
    assert!(row.registered, "a registered row, not a hosted one");
    assert_eq!((row.addr.as_str(), row.boot_token.as_str()), (addr.to_string().as_str(), "boot-token-1"));
    assert_eq!(row.pid_source, env::seams::PidSource::SELF);

    // THE STRONG VERDICT: the HELLO handshake matches the token.
    let verdict = env::seams::probe(row);
    assert_eq!(verdict.state, SeamState::Live, "{}", verdict.detail);

    // THE CONTRACT: asked as the supervisor asks, it answers for itself.
    tracing::warn!("a warning from this call site");
    let c = env::warnings::check(row, &verdict, Some("warnings"));
    assert!(!c.is_violation(), "{:?}", c.missing());
    let export = c.export().expect("served");
    assert_eq!((export.producer.as_str(), export.counts_since.as_str()), ("tts", "1700000000"));
    assert!(export.warn >= 1, "the warn! above was not counted");
    assert!(export.kinds.iter().any(|k| k.target.ends_with("seam.rs") && k.level == "warn"), "{:?}", export.kinds);
    assert!(say(&addr.to_string(), "WARNINGS gossip").starts_with("ERR "), "another shape was answered");

    // AND WHERE THE WEBSOCKET IS, for an environment whose port the kernel chose.
    assert_eq!(say(&addr.to_string(), "WS"), format!("ws://{ws}/ws"));
    assert_eq!(say(&addr.to_string(), "PING"), "PONG");
    assert!(say(&addr.to_string(), "DATA").starts_with("ERR unknown request"));
}

/// The durable half: a warning lands in `<service dir>/warnings/warnings.jsonl`.
#[test]
fn a_warning_is_written_to_the_record_beside_the_count() {
    let dir = tempfile::tempdir().unwrap();
    tts::seam::attach_sink(dir.path());
    tts::seam::record("error", "src/somewhere.rs", 12, "a durable error");
    let text = std::fs::read_to_string(dir.path().join("warnings.jsonl")).unwrap();
    let rec: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    assert_eq!((rec["level"].as_str(), rec["line"].as_u64(), rec["message"].as_str()), (Some("error"), Some(12), Some("a durable error")));
}
