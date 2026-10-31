//! The cache-key model: `store/tts.toml`.
//!
//! > "have each cache have its own like key. So you can have one key, like a
//! > cache key for all of the like menu items. And then you can have a second
//! > cache key, which has a least recently used cache on it with maximum cache
//! > size." — the operator
//!
//! A cache key is a named row of policy. Nothing in Rust enumerates keys:
//! `mode` and `queueing` are the only enums and neither grows, so a new key is
//! one TOML stanza (or one `tts.upsert_key`) and no code.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};

/// `keep_warm` — never evicted ("keep everything warm"); `lru` — bounded in
/// bytes ("or you can set a cache limit"); `lru_count` — the same LRU bounded
/// in ENTRIES ("we set a max number that we are going to track… since we set
/// a count of one, it just evicts the old one").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    KeepWarm,
    Lru,
    LruCount,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::KeepWarm => "keep_warm",
            Mode::Lru => "lru",
            Mode::LruCount => "lru_count",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Mode::KeepWarm, Mode::Lru, Mode::LruCount].into_iter().find(|m| m.as_str() == s)
    }

    /// A `keep_warm` key is never scanned for victims, by its own budget or by
    /// global pressure. Both bounded modes are.
    pub fn is_evictable(&self) -> bool {
        !matches!(self, Mode::KeepWarm)
    }
}

/// "It should either be last in first out or first in first out. And for our
/// service, we're going to use first in first out." Both are implemented and
/// tested; flipping a key is one word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Queueing {
    Fifo,
    Lifo,
}

impl Queueing {
    pub fn as_str(&self) -> &'static str {
        match self {
            Queueing::Fifo => "fifo",
            Queueing::Lifo => "lifo",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Queueing::Fifo, Queueing::Lifo].into_iter().find(|q| q.as_str() == s)
    }
}

/// `[global]`: service-wide policy and the last line of defence against the
/// 33 GB incident recurring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalConfig {
    /// Hard disk backstop across all keys.
    #[serde(default = "d_global_max_bytes", deserialize_with = "de_size")]
    pub max_bytes: u64,
    /// The synthesis gate. "Let's do four."
    #[serde(default = "d_max_concurrent")]
    pub max_concurrent: usize,
    /// Per-synthesis hard timeout.
    #[serde(default = "d_synth_timeout_ms")]
    pub synth_timeout_ms: u64,
    /// A runaway-backend guard: an output past this is refused as `too_large`
    /// rather than dropped silently.
    #[serde(default = "d_max_entry_bytes", deserialize_with = "de_size")]
    pub max_entry_bytes: u64,
    /// Oversize batches are rejected, never truncated.
    #[serde(default = "d_max_batch_items")]
    pub max_batch_items: usize,
    /// `true`: after `breaker_threshold` consecutive failures a module is
    /// degraded and requests fail fast with `backend_unavailable`. `false`:
    /// every request queues behind a wedged backend.
    #[serde(default = "d_true")]
    pub breaker_enabled: bool,
    #[serde(default = "d_breaker_threshold")]
    pub breaker_threshold: u32,
    /// While degraded, one probe synthesis is let through this often.
    #[serde(default = "d_breaker_probe_secs")]
    pub breaker_probe_secs: u64,
    /// Reap `say` processes writing into this service's audio dir that it does
    /// not own. Off in every test.
    #[serde(default = "d_true")]
    pub reap_orphans: bool,
    #[serde(default = "d_reap_interval_secs")]
    pub reap_interval_secs: u64,
    /// `*.part.aiff` older than this are stale and unlinked at boot.
    #[serde(default = "d_stale_part_secs")]
    pub stale_part_secs: u64,
    /// The boot sweep refuses to delete more than this percentage of the audio
    /// directory in one pass ("It might've even deleted all of my WAV files").
    #[serde(default = "d_sweep_pct")]
    pub boot_sweep_max_delete_pct: u32,
}

fn d_global_max_bytes() -> u64 {
    1536 * 1024 * 1024
}
fn d_max_concurrent() -> usize {
    4
}
fn d_synth_timeout_ms() -> u64 {
    90_000
}
fn d_max_entry_bytes() -> u64 {
    32 * 1024 * 1024
}
fn d_max_batch_items() -> usize {
    2000
}
fn d_true() -> bool {
    true
}
fn d_breaker_threshold() -> u32 {
    5
}
fn d_breaker_probe_secs() -> u64 {
    30
}
fn d_reap_interval_secs() -> u64 {
    300
}
fn d_stale_part_secs() -> u64 {
    600
}
fn d_sweep_pct() -> u32 {
    20
}
fn d_priority() -> i64 {
    crate::priority::DEFAULT
}
/// Two minutes: the cache does not evict something it made four seconds ago.
fn d_min_age() -> i64 {
    120
}

impl Default for GlobalConfig {
    fn default() -> Self {
        toml::from_str("").expect("every [global] field has a default")
    }
}

/// One `[keys.<name>]` stanza. `name` comes from the stanza's key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyConfig {
    #[serde(skip)]
    pub name: String,
    pub mode: Mode,
    /// Required iff `lru`. Enforced here and by a CHECK in the schema.
    #[serde(default, deserialize_with = "de_size_opt")]
    pub max_bytes: Option<u64>,
    /// Required iff `lru_count`, at least 1. `max_entries = 1` evicts the old
    /// value on every write by construction — the pinned-count label's design.
    #[serde(default)]
    pub max_entries: Option<u64>,
    /// `keep_warm` only: warn and push `cache_pressure` past here.
    #[serde(default, deserialize_with = "de_size_opt")]
    pub warn_bytes: Option<u64>,
    /// `keep_warm` only: REFUSE new work past here. Refusing to add is honest;
    /// evicting from a key declared permanent is not.
    #[serde(default, deserialize_with = "de_size_opt")]
    pub refuse_bytes: Option<u64>,
    pub queueing: Queueing,
    pub module: String,
    /// Owned by the module; feeds its fingerprint, the second half of the hash.
    #[serde(default)]
    pub module_config: BTreeMap<String, toml::Value>,
    /// The cache's DEFAULT ITEM PRIORITY — what an item gets when its caller
    /// names none. See [`crate::priority`].
    #[serde(default = "d_priority")]
    pub priority: i64,
    #[serde(default = "d_min_age")]
    pub min_age_secs: i64,
    /// A key removed from the file is disabled, never purged.
    #[serde(default = "d_true")]
    pub enabled: bool,
}

impl KeyConfig {
    /// `module_config` as JSON: what the store persists and the module reads.
    pub fn module_config_json(&self) -> Value {
        Value::Object(self.module_config.iter().map(|(k, v)| (k.clone(), toml_to_json(v))).collect())
    }

    /// A key arriving over the wire (`tts.upsert_key`): the same struct,
    /// defaults and validation as a stanza on disk — one kind of key arriving
    /// by a second door.
    pub fn from_json(name: &str, v: &Value) -> Result<Self> {
        let mut cfg: KeyConfig = serde_json::from_value(v.clone())
            .map_err(|e| Error::bad_params(format!("key {name}: {e}")))?;
        cfg.name = name.to_string();
        cfg.validate()?;
        Ok(cfg)
    }

    /// The structural rules of one key, shared by the file and the wire.
    pub fn validate(&self) -> Result<()> {
        let name = &self.name;
        let bad = |why: String| Err(Error::bad_params(format!("key {name}: {why}")));
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(Error::bad_params(format!(
                "cache key name {name:?} must be a slug ([A-Za-z0-9_-]+)"
            )));
        }
        if (self.mode == Mode::Lru) != self.max_bytes.is_some() {
            return bad(format!(
                "max_bytes is required by mode = \"lru\" and forbidden otherwise (mode = {:?}; a \
                 keep-warm key is never evicted from — bound it with refuse_bytes)",
                self.mode.as_str()
            ));
        }
        if (self.mode == Mode::LruCount) != self.max_entries.is_some() {
            return bad(format!(
                "max_entries is required by mode = \"lru_count\" and forbidden otherwise (mode = {:?})",
                self.mode.as_str()
            ));
        }
        if self.max_entries == Some(0) {
            return bad("max_entries must be >= 1 (a key that can hold nothing evicts what it just \
                        synthesized)"
                .into());
        }
        if self.mode.is_evictable() && (self.warn_bytes.is_some() || self.refuse_bytes.is_some()) {
            return bad("warn_bytes/refuse_bytes are keep_warm-only".into());
        }
        if let (Some(w), Some(r)) = (self.warn_bytes, self.refuse_bytes) {
            if w > r {
                return bad(format!("warn_bytes ({w}) must be <= refuse_bytes ({r})"));
            }
        }
        if self.module.is_empty() {
            return bad("module is required".into());
        }
        Ok(())
    }

    /// The key in the shape `tts.upsert_key` accepts, so a client can read one
    /// back and send it forward unchanged.
    pub fn to_json(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("mode".into(), self.mode.as_str().into());
        for (k, v) in [
            ("max_bytes", self.max_bytes),
            ("max_entries", self.max_entries),
            ("warn_bytes", self.warn_bytes),
            ("refuse_bytes", self.refuse_bytes),
        ] {
            if let Some(v) = v {
                m.insert(k.into(), v.into());
            }
        }
        m.insert("queueing".into(), self.queueing.as_str().into());
        m.insert("module".into(), self.module.clone().into());
        m.insert("priority".into(), self.priority.into());
        m.insert("min_age_secs".into(), self.min_age_secs.into());
        m.insert("enabled".into(), self.enabled.into());
        m.insert("module_config".into(), self.module_config_json());
        Value::Object(m)
    }
}

fn toml_to_json(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::from(*i),
        toml::Value::Float(f) => Value::from(*f),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(a) => Value::Array(a.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => Value::Object(t.iter().map(|(k, v)| (k.clone(), toml_to_json(v))).collect()),
    }
}

/// The whole `tts.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeysFile {
    #[serde(default)]
    pub global: GlobalConfig,
    #[serde(default)]
    pub keys: BTreeMap<String, KeyConfig>,
}

impl KeysFile {
    pub fn from_toml(s: &str) -> Result<Self> {
        let mut f: KeysFile =
            toml::from_str(s).map_err(|e| Error::Config(format!("parsing tts.toml: {e}")))?;
        for (name, k) in f.keys.iter_mut() {
            k.name = name.clone();
        }
        f.validate()?;
        Ok(f)
    }

    /// Load `path`, writing [`DEFAULT_TTS_TOML`] there first if it is absent,
    /// so a fresh environment boots with its launch keys and no hand edit.
    pub fn load_or_seed(path: &std::path::Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::from_toml(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::write(path, DEFAULT_TTS_TOML)
                    .map_err(|e| Error::Config(format!("seeding {}: {e}", path.display())))?;
                Self::from_toml(DEFAULT_TTS_TOML)
            }
            Err(e) => Err(Error::Config(format!("reading {}: {e}", path.display()))),
        }
    }

    /// Structural validation. Whether each key's module exists and accepts
    /// its config is the manager's check, against the module registry.
    pub fn validate(&self) -> Result<()> {
        if self.global.max_concurrent == 0 {
            return Err(Error::Config("global.max_concurrent must be >= 1".into()));
        }
        self.keys.values().try_for_each(KeyConfig::validate)
    }
}

/// Insert or replace `[keys.<name>]` in the text of `tts.toml`, keeping every
/// comment the operator wrote. `toml_edit` rather than a serde round trip
/// because the file is still his: it carries his numbers with his words
/// beside them. A brand-new stanza is marked as written by a client.
pub fn upsert_key_text(doc_text: &str, cfg: &KeyConfig) -> Result<String> {
    use toml_edit::{value, DocumentMut, InlineTable, Item, Table};
    let mut doc: DocumentMut =
        doc_text.parse().map_err(|e| Error::Config(format!("parsing tts.toml for edit: {e}")))?;
    if !doc.contains_key("keys") {
        doc["keys"] = Item::Table(Table::new());
    }
    let keys = doc["keys"]
        .as_table_mut()
        .ok_or_else(|| Error::Config("tts.toml: `keys` is not a table".into()))?;
    keys.set_implicit(true);

    let mut t = Table::new();
    t.insert("mode", value(cfg.mode.as_str()));
    for (k, v) in [
        ("max_bytes", cfg.max_bytes),
        ("max_entries", cfg.max_entries),
        ("warn_bytes", cfg.warn_bytes),
        ("refuse_bytes", cfg.refuse_bytes),
    ] {
        if let Some(v) = v {
            t.insert(k, value(v as i64));
        }
    }
    t.insert("queueing", value(cfg.queueing.as_str()));
    t.insert("module", value(cfg.module.as_str()));
    t.insert("priority", value(cfg.priority));
    t.insert("min_age_secs", value(cfg.min_age_secs));
    t.insert("enabled", value(cfg.enabled));
    let mut mc = Table::new();
    for (k, v) in &cfg.module_config {
        mc.insert(k, Item::Value(toml_to_edit(v)));
    }
    t.insert("module_config", Item::Table(mc));

    // An existing stanza keeps whatever the operator wrote above it.
    let prefix = match keys.get(&cfg.name).and_then(Item::as_table) {
        Some(prev) => prev.decor().prefix().and_then(|s| s.as_str()).unwrap_or("\n").to_string(),
        None => "\n# created by tts.upsert_key — a client declared this key at runtime\n".to_string(),
    };
    t.decor_mut().set_prefix(prefix);
    keys.insert(&cfg.name, Item::Table(t));

    fn toml_to_edit(v: &toml::Value) -> toml_edit::Value {
        match v {
            toml::Value::String(s) => s.clone().into(),
            toml::Value::Integer(i) => (*i).into(),
            toml::Value::Float(f) => (*f).into(),
            toml::Value::Boolean(b) => (*b).into(),
            toml::Value::Datetime(d) => d.to_string().into(),
            toml::Value::Array(a) => toml_edit::Value::Array(a.iter().map(toml_to_edit).collect()),
            toml::Value::Table(t) => {
                let mut it = InlineTable::new();
                for (k, x) in t {
                    it.insert(k, toml_to_edit(x));
                }
                toml_edit::Value::InlineTable(it)
            }
        }
    }
    Ok(doc.to_string())
}

/// What a fresh environment's `tts.toml` starts as: the two launch keys and
/// one key per volatile label.
pub const DEFAULT_TTS_TOML: &str = r#"# The tts service's cache keys. A cache key is a NAMED CONFIGURATION: a new one
# is a stanza here and a `tts.reload` (or a client's `tts.upsert_key`), never code.

[global]
# The disk backstop across ALL keys, not a per-key budget.
max_bytes        = "1536MiB"
max_concurrent   = 4
synth_timeout_ms = 90000

# Interface constants: menu items, navigation fragments, model labels.
[keys.interface]
mode          = "keep_warm"
queueing      = "fifo"
module        = "system"
priority      = 1             # SYSTEM: chrome outranks a message body
min_age_secs  = 120
warn_bytes    = "128MiB"      # warn and push cache_pressure past here
refuse_bytes  = "256MiB"      # REFUSE new work past here; never evict
[keys.interface.module_config]
voice = "Samantha"

# User content: messages, thread names, project names.
[keys.content]
mode          = "lru"
max_bytes     = "1GiB"
queueing      = "fifo"
module        = "system"
priority      = 0             # DEFAULT: the floor of the scale
min_age_secs  = 120
[keys.content.module_config]
voice = "Samantha"

# One key per volatile label. "since we set a count of one, it just evicts the
# old one. By the time we ever even make it back to the menu, it should
# already be warm." min_age_secs = 0 on both: on a count-bounded key a
# two-minute floor would stop the very eviction that is the design.

# "Pinned. N threads" — one entry, the current count.
[keys.pinned-count]
mode          = "lru_count"
max_entries   = 1
queueing      = "fifo"
module        = "system"
priority      = 1
min_age_secs  = 0
[keys.pinned-count.module_config]
voice = "Samantha"

# "restart aui. N active" — "10 is fine. 100 is too much."
[keys.active-count]
mode          = "lru_count"
max_entries   = 10
queueing      = "fifo"
module        = "system"
priority      = 1
min_age_secs  = 0
[keys.active-count.module_config]
voice = "Samantha"
"#;

/// `"1GiB"`, `"128MiB"`, `"512kb"`, `1073741824`. Binary units throughout, so
/// a budget stated in gigabytes is read in gigabytes.
pub fn parse_size(s: &str) -> std::result::Result<u64, String> {
    let t = s.trim();
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return Err(format!("size {s:?} does not start with a number"));
    }
    let n: u64 = t[..digits].parse().map_err(|_| format!("size {s:?} overflows"))?;
    let mult: u64 = match t[digits..].trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        other => return Err(format!("unknown size unit {other:?} in {s:?}")),
    };
    n.checked_mul(mult).ok_or_else(|| format!("size {s:?} overflows"))
}

fn size_of<E: serde::de::Error>(v: toml::Value) -> std::result::Result<u64, E> {
    match v {
        toml::Value::Integer(i) if i >= 0 => Ok(i as u64),
        toml::Value::String(s) => parse_size(&s).map_err(E::custom),
        other => Err(E::custom(format!("expected a size, got {other}"))),
    }
}

fn de_size<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<u64, D::Error> {
    size_of(toml::Value::deserialize(d)?)
}

fn de_size_opt<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<u64>, D::Error> {
    Option::<toml::Value>::deserialize(d)?.map(size_of).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_key(body: &str) -> Result<KeysFile> {
        KeysFile::from_toml(&format!("[keys.k]\nqueueing = \"fifo\"\nmodule = \"system\"\n{body}"))
    }

    #[test]
    fn sizes_are_binary_and_garbage_is_refused() {
        assert_eq!(parse_size("1GiB").unwrap(), 1_073_741_824);
        assert_eq!(parse_size("1536MiB").unwrap(), 1_610_612_736);
        assert_eq!(parse_size("128").unwrap(), 128);
        assert!(parse_size("MiB").is_err());
        assert!(parse_size("12 furlongs").is_err());
        assert!(parse_size("99999999999999GiB").is_err());
    }

    /// The seeded file, read against the published scale rather than bare
    /// numbers, so the seed and the ladder cannot drift apart.
    #[test]
    fn the_seed_carries_the_launch_keys_and_one_key_per_volatile_label() {
        let f = KeysFile::from_toml(DEFAULT_TTS_TOML).unwrap();
        assert_eq!(f.global.max_concurrent, 4);
        let (i, c) = (&f.keys["interface"], &f.keys["content"]);
        assert_eq!((i.mode, i.refuse_bytes), (Mode::KeepWarm, Some(256 << 20)));
        assert_eq!((c.mode, c.max_bytes, c.queueing), (Mode::Lru, Some(1 << 30), Queueing::Fifo));
        assert_eq!((i.priority, c.priority), (crate::priority::SYSTEM, crate::priority::DEFAULT));
        assert!(crate::priority::look_ahead(crate::priority::LOOK_AHEAD_DEPTH) > i.priority);
        assert_eq!(c.module_config_json()["voice"], "Samantha");
        let (p, a) = (&f.keys["pinned-count"], &f.keys["active-count"]);
        assert_eq!((p.mode, p.max_entries, p.min_age_secs), (Mode::LruCount, Some(1), 0));
        assert_eq!((a.mode, a.max_entries, a.min_age_secs), (Mode::LruCount, Some(10), 0));
        assert_eq!(p.priority, crate::priority::SYSTEM);
    }

    /// Each bound belongs to exactly one mode, refused at LOAD.
    #[test]
    fn every_bound_is_required_by_its_mode_and_refused_by_the_others() {
        for bad in [
            "mode = \"lru\"",
            "mode = \"keep_warm\"\nmax_bytes = \"1GiB\"",
            "mode = \"lru_count\"",
            "mode = \"lru_count\"\nmax_entries = 1\nmax_bytes = \"1GiB\"",
            "mode = \"lru\"\nmax_bytes = \"1GiB\"\nmax_entries = 10",
            "mode = \"lru_count\"\nmax_entries = 0",
            "mode = \"lru\"\nmax_bytes = 10\nwarn_bytes = 5",
            "mode = \"keep_warm\"\nwarn_bytes = 9\nrefuse_bytes = 5",
        ] {
            assert!(one_key(bad).is_err(), "accepted: {bad}");
        }
        let ok = one_key("mode = \"lru_count\"\nmax_entries = 1").unwrap();
        assert_eq!((ok.keys["k"].max_entries, ok.keys["k"].max_bytes), (Some(1), None));
    }

    #[test]
    fn only_keep_warm_is_immune_from_eviction_and_the_names_round_trip() {
        assert!(!Mode::KeepWarm.is_evictable() && Mode::Lru.is_evictable() && Mode::LruCount.is_evictable());
        for m in [Mode::KeepWarm, Mode::Lru, Mode::LruCount] {
            assert_eq!(Mode::parse(m.as_str()), Some(m));
        }
        assert_eq!(Queueing::parse("lifo"), Some(Queueing::Lifo));
        assert_eq!(Mode::parse("lru-count"), None);
    }

    #[test]
    fn a_wire_key_reads_back_in_the_shape_it_was_sent() {
        let sent = serde_json::json!({"mode": "lru_count", "max_entries": 3, "queueing": "lifo",
            "module": "system", "priority": 1, "module_config": {"voice": "Samantha"}});
        let k = KeyConfig::from_json("labels", &sent).unwrap();
        let back = KeyConfig::from_json("labels", &k.to_json()).unwrap();
        assert_eq!(back.to_json(), k.to_json());
        assert!(KeyConfig::from_json("bad name", &sent).is_err());
    }

    /// The edit keeps the operator's comments and marks a client's new stanza.
    #[test]
    fn upserting_a_key_keeps_every_comment_in_the_file() {
        let k = KeyConfig::from_json(
            "fresh",
            &serde_json::json!({"mode": "keep_warm", "queueing": "fifo", "module": "system",
                "module_config": {"voice": "Samantha"}}),
        )
        .unwrap();
        let out = upsert_key_text(DEFAULT_TTS_TOML, &k).unwrap();
        assert!(out.contains("10 is fine. 100 is too much."));
        assert!(out.contains("created by tts.upsert_key"));
        let f = KeysFile::from_toml(&out).unwrap();
        assert_eq!(f.keys.len(), 5);
        assert_eq!(f.keys["fresh"].mode, Mode::KeepWarm);
    }
}
