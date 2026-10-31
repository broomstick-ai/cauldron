//! The test harness.
//!
//! FIXTURE ONLY: `TTS_SAY_BIN` points at `tests/fixtures/fake-say` for every
//! test, so nothing here runs a real `say`. Every data directory is a
//! `tempfile::TempDir`, and every daemon a test starts binds `127.0.0.1:0`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

pub fn fake_say() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-say")
}

/// Process-global and identical in every test, so parallel tests never race
/// on it. Per-test control is files, never environment.
pub fn set_env() {
    std::env::set_var("TTS_SAY_BIN", fake_say());
}

// ── fixture control: files under <data_dir>/.fakesay/ ─────────────────────

fn ctl(data_dir: &Path) -> PathBuf {
    let p = data_dir.join(".fakesay");
    std::fs::create_dir_all(p.join("live")).unwrap();
    p
}

fn flag(data_dir: &Path, name: &str, on: bool) {
    let p = ctl(data_dir).join(name);
    if on {
        std::fs::write(p, "1").unwrap();
    } else {
        let _ = std::fs::remove_file(p);
    }
}

pub fn say_takes_ms(data_dir: &Path, ms: u64) {
    std::fs::write(ctl(data_dir).join("ms"), ms.to_string()).unwrap();
}

pub fn say_writes_bytes(data_dir: &Path, n: u64) {
    std::fs::write(ctl(data_dir).join("bytes"), n.to_string()).unwrap();
}

pub fn say_fails(data_dir: &Path, on: bool) {
    flag(data_dir, "fail", on);
}

/// Every synthesis is KILLED by a signal mid-flight, the way a reaper or a
/// supervisor kills a real `say`. Not the same thing as [`say_fails`].
pub fn say_dies_on_signal(data_dir: &Path, on: bool) {
    flag(data_dir, "signal", on);
}

pub fn say_writes_nothing(data_dir: &Path) {
    flag(data_dir, "empty", true);
}

/// The texts the fixture was asked to speak, one per invocation.
pub fn say_calls(data_dir: &Path) -> Vec<String> {
    std::fs::read_to_string(data_dir.join(".fakesay/calls"))
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// The most invocations ever running at once.
pub fn peak_concurrency(data_dir: &Path) -> u64 {
    std::fs::read_to_string(data_dir.join(".fakesay/peaks"))
        .map(|s| s.lines().filter_map(|l| l.trim().parse().ok()).max().unwrap_or(0))
        .unwrap_or(0)
}

/// Poll until `f` holds or `ms` pass.
pub async fn eventually(ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    f()
}

// ── managers and daemons on temp data dirs ────────────────────────────────

use tts::{KeysFile, Store, TtsManager};

/// Two keys on the test scale: `interface` keep-warm at 100, `content` LRU
/// at 50, a 5 s synthesis timeout and the reaper off.
pub const TWO_KEYS: &str = r#"
[global]
max_bytes        = "1536MiB"
max_concurrent   = 4
synth_timeout_ms = 5000
reap_orphans     = false

[keys.interface]
mode          = "keep_warm"
queueing      = "fifo"
module        = "system"
priority      = 100
min_age_secs  = 0
warn_bytes    = "128MiB"
refuse_bytes  = "256MiB"
[keys.interface.module_config]
voice = "Samantha"

[keys.content]
mode          = "lru"
max_bytes     = "1GiB"
queueing      = "fifo"
module        = "system"
priority      = 50
min_age_secs  = 0
[keys.content.module_config]
voice = "Samantha"
"#;

/// A started manager on a fresh temp data dir with `toml` as its tts.toml.
pub fn manager_with(toml: &str) -> (TtsManager, tempfile::TempDir) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("tts.toml"), toml).unwrap();
    std::fs::create_dir_all(tmp.path().join("audio")).unwrap();
    let store = Store::open(tmp.path().join("tts.db")).unwrap();
    let m = TtsManager::new(store, tts::Config::default(), tmp.path().to_path_buf(), KeysFile::from_toml(toml).unwrap())
        .unwrap();
    m.start();
    (m, tmp)
}

pub fn manager() -> (TtsManager, tempfile::TempDir) {
    manager_with(TWO_KEYS)
}

/// Built the way the daemon builds one: tts.toml seeded when absent.
pub fn built_manager() -> (TtsManager, tempfile::TempDir) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    let m = tts::build_manager(tmp.path().to_path_buf(), tts::Config::default()).unwrap();
    (m, tmp)
}

/// A real daemon on `127.0.0.1:0`.
pub async fn daemon(toml: &str, secret: Option<&str>) -> (TtsManager, std::net::SocketAddr, tempfile::TempDir) {
    set_env();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("tts.toml"), toml).unwrap();
    let mut cfg = tts::Config::default();
    cfg.secret = secret.map(str::to_string);
    let m = tts::build_manager(tmp.path().to_path_buf(), cfg).unwrap();
    let listener = tts::server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let (addr, _task) = tts::server::serve(m.clone(), listener).unwrap();
    (m, addr, tmp)
}

pub fn items(texts: &[&str]) -> serde_json::Value {
    serde_json::Value::Array(texts.iter().map(|t| serde_json::json!({"text": t})).collect())
}

pub fn hash_of(text: &str) -> String {
    tts::hash::hash_for(text, "Samantha")
}

/// How many times the backend was asked to speak exactly `text`.
pub fn calls_of(dir: &Path, text: &str) -> usize {
    say_calls(dir).iter().filter(|c| *c == text).count()
}

/// Warm `text` under `key` through `tts.request`; its hash.
pub async fn warm(m: &TtsManager, key: &str, text: &str) -> String {
    warm_within(m, key, text, 10_000).await
}

/// [`warm`], waiting up to `wait_ms` for the render.
pub async fn warm_within(m: &TtsManager, key: &str, text: &str, wait_ms: u64) -> String {
    let r = m.request(key, None, Some(text.into()), wait_ms, "path").await.unwrap_or_else(|e| panic!("warming {text:?}: {e}"));
    r["hash"].as_str().unwrap().to_string()
}

pub fn state_of(m: &TtsManager, hash: &str) -> Option<String> {
    m.store.get_render(hash).unwrap().map(|r| r.state)
}

// ── a websocket client ────────────────────────────────────────────────────

use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

pub struct Client {
    wr: SplitSink<Ws, Message>,
    rd: SplitStream<Ws>,
    pub pending: std::collections::VecDeque<Value>,
    next_id: u64,
}

impl Client {
    /// Connect, returning the client and its welcome frame.
    pub async fn connect(addr: std::net::SocketAddr) -> (Self, Value) {
        let (ws, _) = connect_async(format!("ws://{addr}/ws")).await.unwrap();
        let (wr, rd) = ws.split();
        let mut c = Self { wr, rd, pending: Default::default(), next_id: 0 };
        let welcome = c.next_frame().await;
        (c, welcome)
    }

    pub async fn send_raw(&mut self, v: Value) {
        self.wr.send(Message::Text(v.to_string())).await.unwrap();
    }

    pub async fn next_frame(&mut self) -> Value {
        loop {
            let msg = tokio::time::timeout(std::time::Duration::from_secs(10), self.rd.next())
                .await
                .expect("timed out waiting for a frame")
                .expect("socket closed")
                .expect("socket error");
            match msg {
                Message::Text(t) => return serde_json::from_str(&t).unwrap(),
                Message::Ping(_) | Message::Pong(_) => continue,
                other => panic!("unexpected frame {other:?}"),
            }
        }
    }

    /// The outcome of one request; other frames are kept in `pending`.
    pub async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = format!("r{}", self.next_id);
        self.send_raw(json!({"type": "request", "id": id, "method": method, "params": params})).await;
        loop {
            let f = self.next_frame().await;
            if f["type"] == "response" && f["correlate"] == json!(id) {
                return f["outcome"].clone();
            }
            self.pending.push_back(f);
        }
    }

    pub async fn ok(&mut self, method: &str, params: Value) -> Value {
        let o = self.request(method, params).await;
        assert_eq!(o["type"], "success", "{method} failed: {o}");
        o["payload"].clone()
    }

    pub async fn err(&mut self, method: &str, params: Value) -> Value {
        let o = self.request(method, params).await;
        assert_eq!(o["type"], "error", "{method} unexpectedly succeeded: {o}");
        o
    }

    /// Buffered pushes, plus whatever arrives within `ms`.
    pub async fn collect_pushes(&mut self, ms: u64) -> Vec<Value> {
        let mut out: Vec<Value> = self.pending.drain(..).collect();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(50), self.rd.next()).await {
                Ok(Some(Ok(Message::Text(t)))) => out.push(serde_json::from_str(&t).unwrap()),
                Ok(Some(Ok(_))) | Err(_) => {}
                Ok(_) => break,
            }
        }
        out
    }
}
