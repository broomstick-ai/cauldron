//! **The cauldron's settings** — the taxonomy of design report 0057, as DATA,
//! and the one rule that resolves a value. A cluster carries no settings of
//! its own (8e4fca10 msg 235: "I actually think we can drop config from the
//! cluster right now. There's just global and cauldron specific config"), and
//! nothing about the ripples is configured here (msg 235: "there's no system
//! level config stuff. That's my call").
//!
//! [`DEFS`] is the whole taxonomy: one [`Def`] per setting, keyed by its
//! dotted path in the environment's settings file. A key may carry
//! placeholders — `{pool}`, `{account}`, `{role}`, `{service}` — filled from a
//! [`Ctx`]. The menus (`menu.rs`) are built over this table, and the table is
//! the only place a setting's type, default and inheritance are written.
//!
//! **One rule** (0057 `inheritance_model`). Effective value = the nearest
//! EXPLICIT value walking up the chain — cauldron default → pool → account,
//! written as each [`Def::inherits`] naming the def it inherits from; absent
//! means inherit. A value that must legitimately be "none" (an ungated
//! threshold) is the explicit word `off`, never an absence, so inherit and
//! off stay distinguishable on disk.
//!
//! **Where it is kept** — per environment, beside the draft, except the
//! GLOBAL values ([`is_global`], 8e4fca10 msg 186), which every cauldron
//! shares:
//!
//! ```text
//! <data>/<env>/tui/soup/settings.toml                 the cauldron's explicit values
//! <data>/shared/tui/settings.toml                     the global values: the token sources and their defaults
//! <data>/shared/tui/accounts.json                     accounts and pools added; accounts renamed or removed
//! <data>/shared/tui/flips.jsonl                       the opportunistic FLIP LOG: each account's flag turning on or off
//! ```
//!
//! Only explicit values are written. Until the cauldron's file exists, the
//! environment's SEED is read in its place (`<seeds>/settings/<env>.toml` in the
//! repo). An environment may also carry DEFAULTS of its own ([`ENV_DEFAULTS`]):
//! prod's claude_code pool defaults to a weekly threshold of 80 (8e4fca10 msg
//! 64), in code, so it holds whatever file exists, every account added to the
//! pool inherits it, and an account (or the pool) overrides it as any value.
//! Nothing here is pushed to the threads daemon yet.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// The level a def lives at — what a provenance tag names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Cauldron,
    Pool,
    Account,
    /// One inference endpoint (8e4fca10 msg 235; design 0199).
    Endpoint,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Cauldron => "cauldron",
            Level::Pool => "pool",
            Level::Account => "account",
            Level::Endpoint => "endpoint",
        }
    }
}

/// A setting's type: what it holds, how it is edited, how it is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    /// The toggle switch. Space, Enter or → flips it.
    Toggle,
    /// A whole percent in `min..=max`; `off` typeable when `off`.
    Percent { off: bool, min: i64, max: i64 },
    /// A whole number of `unit` (`$` is shown before it), nudged by `step`.
    /// 0 is always zero. With `unlimited`, -1 is `unlimited` (8e4fca10 msg
    /// 165: "zero ... should actually mean zero. Negative one means
    /// unlimited"), typed as `-1` or the word, and the floor a nudge stops at.
    Number { unit: &'static str, step: i64, off: bool, unlimited: bool },
    /// `30s 15m 6h 90d`; `off` typeable when `off`.
    Duration { off: bool },
    /// One of a fixed set, chosen in a picker.
    Choice(&'static [&'static str]),
    /// A model, chosen in a picker from the models of the enabled pools;
    /// `none` adds the word `none` (a fallback that is not there).
    Model { none: bool },
    /// A pool, chosen in a picker; `auto` adds the word `auto`.
    PoolRef { auto: bool },
    /// An account (`<pool>/<account>`), chosen in a picker.
    AccountRef,
    /// An inference endpoint this cauldron can see, chosen in a picker:
    /// held as its minted id (`ep-1`), shown by its name.
    EndpointRef,
    /// A list of words, edited in its band: `+ add` first, `x` removes.
    List,
    Text,
    /// A base URL: `http://` or `https://` and a host (an endpoint's, design
    /// 0199).
    Url,
    /// A value never shown: a key or a password, masked on its row and in
    /// its field (8e4fca10 msg 165; an endpoint's key, design 0199):
    /// `sk-or-v1-••••••••••••••••c81e`, its prefix, dots, its last four.
    Secret,
    /// Dollars per million tokens, cents allowed (`0.15`): what Cauldron's
    /// meter charges for a local or custom endpoint's model (design 0199).
    Price,
    Path,
    /// A daily window `HH:MM-HH:MM`, or `off`.
    Hours,
    /// A row that does something rather than holding a value.
    Action,
    /// The account's "this is the machine's live login" switch: a VIEW of
    /// `identity.live_login`, which is the one place the choice is kept.
    LiveLogin,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Def {
    /// The dotted key, with placeholders.
    pub key: &'static str,
    pub label: &'static str,
    pub ty: Ty,
    /// The default, as typed. `@role.model` and friends are filled from the
    /// role table or the mesh; a default a type cannot parse is SHOWN as
    /// written (`daemon default`) and never stored.
    pub default: &'static str,
    pub level: Level,
    /// The def this one inherits from when it holds no value of its own.
    pub inherits: Option<&'static str>,
    /// A trailing value on the row: [`MACHINE_RAM_NOTE`] is the only one, and
    /// is filled from the machine (`ram_note`).
    pub note: &'static str,
    /// A number may not exceed this other setting's effective value.
    pub cap: Option<&'static str>,
    /// Bookkeeping kept in a settings file and set by no row (the last
    /// endpoint id minted): no band lists it.
    pub hidden: bool,
}

const fn d(key: &'static str, label: &'static str, ty: Ty, default: &'static str, level: Level) -> Def {
    Def { key, label, ty, default, level, inherits: None, note: "", cap: None, hidden: false }
}

impl Def {
    const fn inh(self, parent: &'static str) -> Def {
        Def { inherits: Some(parent), ..self }
    }
    const fn note(self, note: &'static str) -> Def {
        Def { note, ..self }
    }
    const fn cap(self, key: &'static str) -> Def {
        Def { cap: Some(key), ..self }
    }
    const fn hide(self) -> Def {
        Def { hidden: true, ..self }
    }
    /// Whether a value of this def is kept in a settings file at all.
    pub fn stored(&self) -> bool {
        !matches!(self.ty, Ty::Action | Ty::LiveLogin) && self.key != "ui.sidebar_ratio"
    }
}

use Level::{Account as A, Cauldron as C, Endpoint as E, Pool as P};

/// The note on `machine.ram_max_gb`: the value as a share of THIS machine's
/// RAM, read from the machine (8e4fca10 msg 64, F44).
pub const MACHINE_RAM_NOTE: &str = "@machine_ram";

/// This machine's physical RAM in GB, read once; None where it cannot be read.
pub fn machine_ram_gb() -> Option<u64> {
    static RAM: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *RAM.get_or_init(|| {
        let bytes: u64 = if cfg!(target_os = "macos") {
            let out = std::process::Command::new("/usr/sbin/sysctl").args(["-n", "hw.memsize"]).output().ok()?;
            String::from_utf8_lossy(&out.stdout).trim().parse().ok()?
        } else {
            let t = std::fs::read_to_string("/proc/meminfo").ok()?;
            let kb: u64 = t.lines().find(|l| l.starts_with("MemTotal:"))?.split_whitespace().nth(1)?.parse().ok()?;
            kb * 1024
        };
        (bytes > 0).then(|| (bytes + (1 << 29)) >> 30)
    })
}

/// A def's note as the row shows it: `75% of 64 GB` for the machine's RAM cap.
pub fn ram_note(v: &Val) -> String {
    match (machine_ram_gb(), v) {
        (Some(total), Val::Int(n)) => format!("{}% of {total} GB", n * 100 / total.max(1) as i64),
        (Some(total), _) => format!("of {total} GB"),
        (None, _) => String::new(),
    }
}

/// Defaults an ENVIRONMENT carries over the taxonomy's own, by concrete key:
/// a real default (not seed data), so it holds once the settings file exists,
/// is what clearing an override returns to, and is inherited down the chain.
///
/// `*` is every environment: a default of a GLOBAL key ([`is_global`]) holds
/// wherever the global tab is read from.
pub const ENV_DEFAULTS: &[(&str, &str, &str)] = &[
    // 8e4fca10 msg 64: his claude max accounts get an 80% weekly threshold,
    // inherited by every account added, overridable per account. Keyed to the
    // claude_code pool, which is where those accounts are (F41). The accounts
    // are GLOBAL now (msg 186), so the default is every cauldron's.
    ("*", "pools.claude_code.budget.weekly.threshold_pct", "80"),
];

/// Whether a key (a template or a concrete key) is GLOBAL (8e4fca10 msg 186:
/// "things like what Claude Code accounts are available globally ... we want
/// the cauldrons to inherit things like that"): the token sources — every
/// `pools.` key, accounts and API keys included — and the defaults they
/// inherit (`budgets.default.`, `opportunistic.`). A global value is kept
/// ONCE, under `data/shared/tui/`, and every cauldron reads the same one;
/// what a cauldron takes of them is its own (`cauldron.accounts.…`,
/// `cauldron.use.…`).
///
/// 8e4fca10 msg 235 made the rest of the machine's settings global too —
/// "this machine, and storage and retention, permissions, identity,
/// attention and voice, mission services, repos, TUI, all that stuff belongs
/// in global" — so what stays a cauldron's own is only what is about THIS
/// cauldron: its switch and what it takes of the sources (`cauldron.`), its
/// directories, its local inference, its roles. The repo REGISTRY is global
/// as well (`blueprint::Registry::global`; a cauldron's clusters still claim
/// from it), and with it `repos.`.
pub const GLOBAL_PREFIXES: &[&str] = &[
    "pools.",
    // The inference endpoints registered for every cauldron (design 0199):
    // a cauldron's own are `cauldron.endpoints.`, and what it takes of the
    // global ones `cauldron.use.`.
    "endpoints.",
    "budgets.default.",
    "opportunistic.",
    "machine.",
    // The root holding every cauldron and the one weights path are the
    // machine's (task 0209 D8): Devices › Storage.
    "paths.",
    "storage.",
    "permissions.",
    "identity.",
    "attention.",
    "notify.",
    "supervisor.",
    "services.",
    "threads.",
    "mesh.",
    "repos.",
    "ui.",
];

pub fn is_global(key: &str) -> bool {
    GLOBAL_PREFIXES.iter().any(|p| key.starts_with(p))
}

/// Keys that WERE settings and are not any more: the per-pool switch (only
/// an account is switched, 8e4fca10 msg 165), a cluster's pause (the
/// cauldron's switch is the only pause), dreaming (parked, msg 165), the
/// local weights dir (the one weights path is `paths.weights`), every
/// cluster-level budget and resource limit (msg 186), and the dump's rows
/// (the bucket's now). A file that
/// still holds one is READ — the value is dropped, named in
/// [`Store::retired`], and written back as he wrote it (never deleted, task
/// 0209 D1) — because
/// the setting it set no longer exists; refusing the whole file for it would
/// lose every setting that does.
pub const RETIRED: &[&str] = &[
    // 0220 E8: a service's version and address are live readings (Status ›
    // Mesh), never settings: nothing read an override.
    "services.{service}.version",
    "services.{service}.addr",
    // 0217 B10: the microphone's tones were dictation's, and msg 235 took
    // dictation out of the face ("I actually don't want to support
    // transcription directly into the Cauldron"); nothing read them.
    "notify.tones.capture",
    "notify.tones.stored",
    "pools.{pool}.enabled",
    "surface.paused",
    "ripples.dream.enabled",
    "ripples.dream.schedule",
    "surface.ripples.dream.enabled",
    "local.weights_dir",
    // 8e4fca10 msg 186: budgets apply to the cauldron only — "we should not
    // be able to configure resources to specific surfaces" — so every
    // cluster-level budget, token and resource limit is gone at the source.
    "budgets.surface_default.session.cap_pct",
    "budgets.surface_default.weekly.cap_pct",
    "budgets.surface_default.fable.cap_pct",
    "budgets.surface_default.cash.cap_usd_week",
    "budgets.surface_default.priority",
    "budgets.surface_default.at_cap",
    "surface.budget.session.cap_pct",
    "surface.budget.weekly.cap_pct",
    "surface.budget.fable.cap_pct",
    "surface.budget.cash.cap_usd_week",
    "surface.budget.priority",
    "surface.budget.at_cap",
    "surface.pools",
    "surface.ripples.max_concurrent",
    "surface.ripples.max_steps",
    "surface.ripples.opportunistic_eligible",
    // msg 186: the dump area is the BUCKET.
    "ui.dump_rows",
    // 8e4fca10 msg 235: a cluster has no settings of its own — "I'm not
    // sure that we actually need a config for the cluster anyway" — and
    // nothing about the ripples is configured — "there's no system level
    // config stuff. That's my call."
    "ripples.max_concurrent",
    "ripples.max_concurrent_per_surface",
    "ripples.max_steps",
    "ripples.step_timeout",
    "ripples.retry_policy",
    "ripples.teardown_strategy",
    "ripples.turn_end_waits_for_subagents",
    "ripples.type_one.side_by_side",
    "ripples.type_one.trusted",
    "ripples.type_one.log_drift",
    "ripples.opportunistic_only",
    "ripples.parked.auto_resume",
    "ripples.parked.max_age",
    "ripples.policy",
    "ripples.dump.max_concurrent",
    "ripples.dump.max_groups",
    "ui.preserve.system",
    // msg 235: every paste is a chip, whatever its length; the bucket is
    // three to six rows high, or as high as its divider is dragged.
    "ui.paste_chip_over",
    "ui.bucket_rows",
    // msg 235: what was a surface is a CLUSTER.
    "storage.threads.protect_active_surfaces",
    // msg 235: the opportunistic submenu is gone — "the whole opportunistic
    // mode submenu basically goes away and you just have a setting of
    // whether to enable opportunistic mode on each individual account".
    "opportunistic.margin_pct",
    "opportunistic.session_cap_pct",
    "opportunistic.lift_thresholds",
    "opportunistic.weekly_threshold_pct",
    "opportunistic.fable_threshold_pct",
    "opportunistic.recompute_every",
    "opportunistic.prefer_in_placement",
    "pools.{pool}.opportunistic.margin_pct",
    "pools.{pool}.opportunistic.session_cap_pct",
    "pools.{pool}.opportunistic.lift_thresholds",
    "pools.{pool}.opportunistic.weekly_threshold_pct",
    "pools.{pool}.opportunistic.fable_threshold_pct",
    "pools.{pool}.opportunistic.prefer_in_placement",
    // msg 235: OpenRouter is not a token source — "instead of calling this
    // like open router at all ... we can call it like inference endpoints"
    // (design 0199): its keys, budgets, admin key and model list are an
    // ENDPOINT's now, and every `pools.openrouter.` key goes with it
    // ([`RETIRED_PREFIXES`]).
    "cauldron.openrouter.key",
    "cauldron.openrouter.max_budget_usd",
    "cauldron.openrouter.metered_cap_usd",
    "pools.{pool}.admin_key",
    "pools.{pool}.models",
    // A source's kind was said only for a source added here, and `+ add
    // token source` is gone with it: what is not a subscription is an
    // endpoint, by its template.
    "pools.{pool}.provider",
    // A source's monthly cash cap was OpenRouter's key budget: an
    // endpoint's `budget` now. A subscription is not capped in dollars.
    "pools.{pool}.budget.cash.cap_usd_month",
];

/// Whole subtrees RETIRED: every key under one is read, dropped and named,
/// whatever setting it would otherwise be — `pools.openrouter.selection`
/// is a pool's setting in form, but the pool is gone (design 0199).
pub const RETIRED_PREFIXES: &[&str] = &["pools.openrouter."];

/// Keys RENAMED at the source: a file holding the old key is read as the new
/// one, and written under the new one at its next save — the value is kept,
/// not dropped, because what it set still exists under another name.
pub const RENAMED: &[(&str, &str)] = &[
    // msg 235: an account's switch inherits its source's, so the source's
    // switch is what its accounts are, not what a new one starts as.
    ("pools.{pool}.new_accounts_enabled", "pools.{pool}.accounts_enabled"),
    // 0220 E11: the log is `dumps.jsonl`; `sends` was its old name.
    ("storage.tui.sends_cap_mb", "storage.tui.dumps_cap_mb"),
];

/// `key` under its new name, when it is one of [`RENAMED`].
pub fn renamed(key: &str) -> Option<String> {
    RENAMED.iter().find_map(|(old, new)| {
        let mut ctx = Ctx::default();
        match_simple(old, key, &mut ctx).then(|| subst(new, &ctx))
    })
}

/// Values RENAMED at the source: a file holding the old value is read as the
/// new one, for the reason [`RENAMED`] keeps a renamed key's value — what it
/// chose still exists under another name (8e4fca10 msg 457: Surface is the
/// Cauldron tab, Threads the Agents tab).
pub const RENAMED_VALUES: &[(&str, &str, &str)] = &[
    ("ui.default_tab", "surface", "cauldron"),
    ("ui.default_tab", "threads", "agents"),
];

/// Values a setting can no longer hold (a tab that is gone):
/// a file holding one is READ, the value dropped and named, as a retired
/// key is, because refusing the whole file for it would lose every setting
/// that still stands.
pub const RETIRED_VALUES: &[(&str, &str)] = &[
    // msg 235: the cauldron tabs are Surface, Threads, Status and Config.
    ("ui.default_tab", "contents"),
    ("ui.default_tab", "system"),
];

/// Whether concrete key `key` is one of [`RETIRED`].
pub fn is_retired(key: &str) -> bool {
    RETIRED.iter().any(|t| match_simple(t, key, &mut Ctx::default())) || RETIRED_PREFIXES.iter().any(|p| key.starts_with(p))
}

/// Environment `env`'s defaults, as values.
pub fn env_defaults(env: &str) -> BTreeMap<String, Val> {
    ENV_DEFAULTS
        .iter()
        .filter(|(e, _, _)| *e == env || *e == "*")
        .map(|(_, k, v)| {
            let (d, _) = lookup(k).unwrap_or_else(|| panic!("ENV_DEFAULTS: `{k}` is not a setting"));
            (k.to_string(), parse_input(d.ty, v).unwrap_or_else(|e| panic!("ENV_DEFAULTS: `{k}`: {e}")))
        })
        .collect()
}

const PCT: Ty = Ty::Percent { off: false, min: 0, max: 100 };
const PCT_OFF: Ty = Ty::Percent { off: true, min: 0, max: 100 };
const BAR: Ty = Ty::Percent { off: false, min: 0, max: 1000 };
const SHARE: Ty = Ty::Percent { off: false, min: 1, max: 100 };
const USD_OFF: Ty = Ty::Number { unit: "$", step: 10, off: true, unlimited: false };
const COUNT: Ty = Ty::Number { unit: "", step: 1, off: false, unlimited: false };
const THREADS: Ty = Ty::Number { unit: "", step: 1, off: false, unlimited: true };
const GB: Ty = Ty::Number { unit: "GB", step: 1, off: false, unlimited: false };
const MB: Ty = Ty::Number { unit: "MB", step: 10, off: false, unlimited: false };
const DUR: Ty = Ty::Duration { off: false };

pub const SELECTIONS: &[&str] = &["min_risk", "round_robin", "pinned"];
pub const ENGINES: &[&str] = &["ollama", "mlx", "llama_cpp"];
pub const THRESHOLD_ACTIONS: &[&str] = &["park_new", "park_all"];
pub const BATTERY: &[&str] = &["normal", "no_opportunistic", "pause"];
pub const EVICTION: &[&str] = &["lru", "oldest_created", "largest"];
pub const PERMISSION_MODES: &[&str] = &["bypass", "default", "plan", "accept_edits"];
pub const AUTODECIDE: &[&str] = &["ask_everything", "confirm", "three_ways", "three_ways_floor"];
pub const INTERRUPT: &[&str] = &["never", "violations", "blocking_questions", "all"];
pub const TABS: &[&str] = &["cauldron", "agents", "status", "config"];
/// The face's palettes (`tui_render::palette::Theme`), in their switch's order.
pub const THEMES: &[&str] = &["dark", "light"];

// ── inference endpoints (8e4fca10 msg 235; design 0199) ─────────────────────
//
// "this whole open router section should just be like basically like a list
// of like API keys ... we can call it like inference endpoints ... when
// you're creating a new one we can have a modal where it's like you're
// selecting like open router ... you can select like a local one that
// you're running your own inference server ... they would all have the same
// structure ... So you just have to have pre-made templates for all of
// those accounts."
//
// An endpoint is ONE KEY AT ONE BASE URL. Its template only PREFILLS: a row
// he has not typed into takes its value from the template (the template is
// its default), so choosing another template in the add modal moves every
// such row and keeps every row he typed. Every value lives under
// `endpoints.{ep}.` (global, every cauldron sees it) or
// `cauldron.endpoints.{ep}.` (one cauldron's own); what a cauldron takes of
// a global one is `cauldron.use.{ep}.`. Ids are minted `ep-<n>` from ONE
// series (`endpoints.last_id`, global), so an id names one endpoint wherever
// it is registered.

/// An endpoint's template: what it prefills.
pub struct Template {
    pub id: &'static str,
    /// The name a new endpoint of it starts with.
    pub name: &'static str,
    pub url: &'static str,
    pub auth: &'static str,
    pub protocol: &'static str,
    /// The key it starts with: Ollama's client needs a value its server
    /// ignores. Empty: none.
    pub key: &'static str,
    /// Whether an endpoint of it cannot be added without a key.
    pub key_required: bool,
    pub headers: &'static str,
    /// `any` (what its `/models` lists for the key) or `explicit`.
    pub models: &'static str,
    /// The model slugs an explicit list starts with.
    pub model_list: &'static str,
}

/// The templates, in the add modal's order. `local` is drawn from its
/// server's ([`LOCAL_SERVERS`]).
pub const TEMPLATES: &[Template] = &[
    Template { id: "openrouter", name: "openrouter", url: "https://openrouter.ai/api/v1", auth: "bearer", protocol: "openai_chat", key: "", key_required: true, headers: "X-OpenRouter-Title: Cauldron", models: "any", model_list: "" },
    // Native Messages, not the OpenAI layer (design 0199 Q2-B: Anthropic's
    // own page calls that layer not for production, and it drops prompt
    // caching); Messages takes its key as `x-api-key`.
    Template { id: "anthropic", name: "anthropic", url: "https://api.anthropic.com/v1", auth: "x_api_key", protocol: "anthropic_messages", key: "", key_required: true, headers: "", models: "explicit", model_list: "claude-opus-5-5, claude-sonnet-4-6" },
    Template { id: "openai", name: "openai", url: "https://api.openai.com/v1", auth: "bearer", protocol: "openai_chat", key: "", key_required: true, headers: "", models: "any", model_list: "" },
    Template { id: "gemini", name: "gemini", url: "https://generativelanguage.googleapis.com/v1beta/openai/", auth: "bearer", protocol: "openai_chat", key: "", key_required: true, headers: "", models: "any", model_list: "" },
    Template { id: "custom", name: "custom", url: "", auth: "bearer", protocol: "openai_chat", key: "", key_required: false, headers: "", models: "any", model_list: "" },
];

/// The local servers the `local` template prefills from (docs read
/// 2026-10-25, design 0199 §4; llama.cpp's port is its unknown U3).
pub const LOCAL_SERVERS: &[Template] = &[
    Template { id: "ollama", name: "ollama", url: "http://localhost:11434/v1", auth: "bearer", protocol: "openai_chat", key: "ollama", key_required: false, headers: "", models: "any", model_list: "" },
    Template { id: "llama_cpp", name: "llama-cpp", url: "http://127.0.0.1:8080/v1", auth: "bearer", protocol: "openai_chat", key: "", key_required: false, headers: "", models: "any", model_list: "" },
    Template { id: "vllm", name: "vllm", url: "http://localhost:8000/v1", auth: "bearer", protocol: "openai_chat", key: "", key_required: false, headers: "", models: "any", model_list: "" },
];

/// Every template's id, in the strip's order, and the words the strip says.
pub const TEMPLATE_IDS: &[&str] = &["openrouter", "anthropic", "openai", "gemini", "local", "custom"];
pub const TEMPLATE_WORDS: &[&str] = &["OpenRouter", "Anthropic API", "OpenAI", "Gemini", "Local server", "Custom"];
pub const SERVERS: &[&str] = &["ollama", "llama_cpp", "vllm"];
pub const SERVER_WORDS: &[&str] = &["Ollama", "llama.cpp", "vLLM"];
pub const PROTOCOLS: &[&str] = &["openai_chat", "anthropic_messages"];
pub const PROTOCOL_WORDS: &[&str] = &["openai chat", "anthropic messages"];
pub const AUTHS: &[&str] = &["bearer", "x_api_key", "none"];
pub const AUTH_WORDS: &[&str] = &["bearer", "x-api-key", "none"];
pub const MODEL_MODES: &[&str] = &["any", "explicit"];
/// A budget's period — OpenRouter's `limit_reset`, so a budget is its
/// key's limit verbatim (design 0199 D3).
pub const PERIODS: &[&str] = &["daily", "weekly", "monthly", "none"];
pub const THRESHOLD_WORDS: &[&str] = &["park new", "park all"];

/// The template an endpoint of template `template` (and, for `local`,
/// server `server`) prefills from.
pub fn template(template: &str, server: &str) -> &'static Template {
    if template == "local" {
        return LOCAL_SERVERS.iter().find(|t| t.id == server).unwrap_or(&LOCAL_SERVERS[0]);
    }
    TEMPLATES.iter().find(|t| t.id == template).unwrap_or(&TEMPLATES[TEMPLATES.len() - 1])
}

/// What the add modal's strip says for template `id`.
pub fn template_word(id: &str) -> &'static str {
    TEMPLATE_IDS.iter().position(|t| *t == id).map_or("Custom", |i| TEMPLATE_WORDS[i])
}

/// A secret as every row shows it (design 0199): its prefix — what it
/// starts with up to its last `-` in the first twelve characters, or its
/// first four — sixteen dots, its last four. A short one is dots only.
pub fn mask(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() < 16 {
        return "•".repeat(8);
    }
    let head: String = chars[..12].iter().collect();
    let prefix: String = match head.rfind('-') {
        Some(i) => head[..=i].to_string(),
        None => chars[..4].iter().collect(),
    };
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{prefix}{}{tail}", "•".repeat(16))
}

/// Whether `id` is a minted endpoint id: `ep-<n>`.
pub fn is_endpoint_id(id: &str) -> bool {
    id.strip_prefix("ep-").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// A role: who in the cauldron spends capacity, and its defaults (0057 §4).
pub struct Role {
    pub id: &'static str,
    pub group: &'static str,
    pub what: &'static str,
    pub model: &'static str,
    pub fallback: &'static str,
    pub pool: &'static str,
}

const fn role(id: &'static str, group: &'static str, what: &'static str, model: &'static str, fallback: &'static str, pool: &'static str) -> Role {
    Role { id, group, what, model, fallback, pool }
}

pub const LINEAGE: &str = "Lineage";
pub const RIPPLE: &str = "Ripples";
pub const VOICE: &str = "Voice & search";
pub const ROLE_GROUPS: [&str; 3] = [LINEAGE, RIPPLE, VOICE];

pub const ROLES: &[Role] = &[
    role("default", LINEAGE, "any thread with no role", "sonnet", "none", "auto"),
    role("coordinator", LINEAGE, "the thread he talks to", "fable", "opus", "claude_code"),
    role("curator", LINEAGE, "workspace curator", "sonnet", "none", "claude_code"),
    role("surface_compactor", LINEAGE, "surface compactor", "opus", "none", "claude_code"),
    role("region_judgment_worker", LINEAGE, "region judgment", "opus", "fable", "claude_code"),
    role("builder", LINEAGE, "writes code against a design", "opus", "none", "claude_code"),
    role("designer", LINEAGE, "holds the design pen", "opus", "none", "claude_code"),
    role("initialization_agent", LINEAGE, "adopts a repo", "opus", "none", "claude_code"),
    role("scout", LINEAGE, "read-only surveys", "sonnet", "haiku", "claude_code"),
    role("plugin_manager", LINEAGE, "maintains the plugins", "opus", "none", "claude_code"),
    role("fence_block_curator", LINEAGE, "learned-experience fences", "opus", "none", "claude_code"),
    role("migration", LINEAGE, "workspace migrations", "opus", "sonnet", "claude_code"),
    role("type_one.primary", RIPPLE, "typed yes/no answers (trusted)", "haiku", "none", "claude_code"),
    role("type_one.secondary", RIPPLE, "typed answers, side by side", "jev", "none", "auto"),
    role("intake.splitter", RIPPLE, "a dump into atoms", "sonnet", "haiku", "auto"),
    role("intake.resolver", RIPPLE, "references against what he heard", "haiku", "none", "auto"),
    role("intake.router", RIPPLE, "a walk down the project tree", "haiku", "none", "auto"),
    role("writer", RIPPLE, "applies an atom as one transaction", "sonnet", "opus", "auto"),
    role("consequence", RIPPLE, "invalidation, violation, unblock", "sonnet", "none", "auto"),
    role("curation.edge_drawer", RIPPLE, "draws edges", "sonnet", "none", "auto"),
    role("curation.valuer", RIPPLE, "ask, confirm, or decide and record", "opus", "sonnet", "auto"),
    role("voice", RIPPLE, "talks back from the living state", "sonnet", "haiku", "auto"),
    role("ladle", VOICE, "semantic search embeddings", "local embedding", "none", "local"),
    role("stt", VOICE, "speech to text", "whisper", "none", "local"),
    role("tts", VOICE, "text to speech voice", "tts daemon voice", "none", "local"),
];

pub fn role_of(id: &str) -> Option<&'static Role> {
    ROLES.iter().find(|r| r.id == id)
}

/// A role in words, as a row names it (0214 A9: `surface_compactor`,
/// `type_one.primary` were shown as identifiers): `surface compactor`,
/// `type one · primary`; the voice roles by what they do.
pub fn role_name(id: &str) -> String {
    match id {
        "stt" => "speech to text".into(),
        "tts" => "text to speech".into(),
        _ => id.split('.').map(|p| p.replace('_', " ")).collect::<Vec<_>>().join(" · "),
    }
}

/// Every setting. Order within a section is the order its page lists it.
pub static DEFS: &[Def] = &[
    // ── the cauldron itself (8e4fca10 msg 165) ──
    // The one pause (msg 186): the FIRST row of THIS cauldron's Config tab,
    // enabled or disabled (msg 235), asked before it flips. A cluster has
    // none.
    d("cauldron.enabled", "cauldron enabled", Ty::Toggle, "on", C),
    // Whether this cauldron takes advantage of the accounts flagged
    // opportunistic (8e4fca10 msg 235: "Whether in a cauldron will take
    // advantage of that is a cauldron setting"); each account's own use of
    // it inherits this.
    d("cauldron.opportunistic", "opportunistic accounts", Ty::Toggle, "on", C),
    // What this cauldron takes of the GLOBAL token sources (msg 186): which
    // accounts it may use, and the OpenRouter key it spends, with its two
    // dollar walls — the key's max budget and Cauldron's own metered cap.
    // `allowed here` is a TRI-STATE (b964d571 msg 23: "the prod cauldron
    // should have inherit for allowed here"): inherit takes the account's
    // own switch, on the Providers tab, as `use here` takes an endpoint's.
    d("cauldron.accounts.{pool}.{account}.allowed", "use in this cauldron", Ty::Toggle, "on", A).inh("pools.{pool}.accounts.{account}.enabled"),
    // This cauldron's own weekly threshold on a global account, inheriting
    // the account's (msg 235: the cauldron's accounts table — "on/off and
    // weekly threshold and opportunistic mode").
    d("cauldron.accounts.{pool}.{account}.weekly_threshold_pct", "weekly threshold", PCT_OFF, "95", A).inh("pools.{pool}.accounts.{account}.budget.weekly.threshold_pct"),
    d("cauldron.accounts.{pool}.{account}.opportunistic", "opportunistic", Ty::Toggle, "on", A).inh("cauldron.opportunistic"),
    // ── inference endpoints (8e4fca10 msg 235; design 0199 §8) ──
    // The flattened defaults every endpoint is born inheriting, listed under
    // the global table; then the bookkeeping of the one id series.
    d("endpoints.defaults.budget_period", "budget period", Ty::Choice(PERIODS), "monthly", C),
    d("endpoints.defaults.threshold_pct", "threshold", PCT, "80", C),
    d("endpoints.defaults.threshold_action", "at threshold", Ty::Choice(THRESHOLD_ACTIONS), "park_new", C),
    d("endpoints.defaults.max_concurrent", "max concurrent", THREADS, "-1", C),
    d("endpoints.last_id", "last endpoint id", COUNT, "0", C).hide(),
    // A seed endpoint he removed: the seed is only ever READ (task 0209
    // D1), so its removal is the one thing written about it.
    d("endpoints.{ep}.removed", "removed", Ty::Toggle, "off", E).hide(),
    d("cauldron.endpoints.{ep}.removed", "removed", Ty::Toggle, "off", E).hide(),
    // The fields of a seed endpoint he set back to what they inherit (↺):
    // the seed is only READ, so that is recorded here — never as a value
    // written over the seed's (task 0212 adversarial pass).
    d("endpoints.{ep}.inherit", "inherit", Ty::List, "", E).hide(),
    d("cauldron.endpoints.{ep}.inherit", "inherit", Ty::List, "", E).hide(),
    d("cauldron.use.{ep}.inherit", "inherit", Ty::List, "", E).hide(),
    // One endpoint registered for every cauldron …
    d("endpoints.{ep}.name", "name", Ty::Text, "@template.name", E),
    d("endpoints.{ep}.template", "template", Ty::Choice(TEMPLATE_IDS), "custom", E),
    d("endpoints.{ep}.server", "server", Ty::Choice(SERVERS), "ollama", E),
    d("endpoints.{ep}.protocol", "protocol", Ty::Choice(PROTOCOLS), "@template.protocol", E),
    d("endpoints.{ep}.url", "url", Ty::Url, "@template.url", E),
    d("endpoints.{ep}.key", "key", Ty::Secret, "@template.key", E),
    d("endpoints.{ep}.auth", "auth", Ty::Choice(AUTHS), "@template.auth", E),
    d("endpoints.{ep}.headers", "headers", Ty::List, "@template.headers", E),
    d("endpoints.{ep}.models", "models", Ty::Choice(MODEL_MODES), "@template.models", E),
    d("endpoints.{ep}.model_list", "models", Ty::List, "@template.model_list", E),
    d("endpoints.{ep}.enabled", "enabled", Ty::Toggle, "on", E),
    d("endpoints.{ep}.budget_usd", "budget", USD_OFF, "off", E),
    d("endpoints.{ep}.budget_period", "budget period", Ty::Choice(PERIODS), "monthly", E).inh("endpoints.defaults.budget_period"),
    d("endpoints.{ep}.threshold_pct", "threshold", PCT, "80", E).inh("endpoints.defaults.threshold_pct"),
    d("endpoints.{ep}.threshold_action", "at threshold", Ty::Choice(THRESHOLD_ACTIONS), "park_new", E).inh("endpoints.defaults.threshold_action"),
    d("endpoints.{ep}.max_concurrent", "max concurrent", THREADS, "-1", E).inh("endpoints.defaults.max_concurrent"),
    d("endpoints.{ep}.management_key", "management key", Ty::Secret, "", E),
    d("endpoints.{ep}.price_in", "price in / 1M", Ty::Price, "0", E),
    d("endpoints.{ep}.price_out", "price out / 1M", Ty::Price, "0", E),
    // … one registered in this cauldron alone (the same shape) …
    d("cauldron.endpoints.{ep}.name", "name", Ty::Text, "@template.name", E),
    d("cauldron.endpoints.{ep}.template", "template", Ty::Choice(TEMPLATE_IDS), "custom", E),
    d("cauldron.endpoints.{ep}.server", "server", Ty::Choice(SERVERS), "ollama", E),
    d("cauldron.endpoints.{ep}.protocol", "protocol", Ty::Choice(PROTOCOLS), "@template.protocol", E),
    d("cauldron.endpoints.{ep}.url", "url", Ty::Url, "@template.url", E),
    d("cauldron.endpoints.{ep}.key", "key", Ty::Secret, "@template.key", E),
    d("cauldron.endpoints.{ep}.auth", "auth", Ty::Choice(AUTHS), "@template.auth", E),
    d("cauldron.endpoints.{ep}.headers", "headers", Ty::List, "@template.headers", E),
    d("cauldron.endpoints.{ep}.models", "models", Ty::Choice(MODEL_MODES), "@template.models", E),
    d("cauldron.endpoints.{ep}.model_list", "models", Ty::List, "@template.model_list", E),
    d("cauldron.endpoints.{ep}.enabled", "enabled", Ty::Toggle, "on", E),
    d("cauldron.endpoints.{ep}.budget_usd", "budget", USD_OFF, "off", E),
    d("cauldron.endpoints.{ep}.budget_period", "budget period", Ty::Choice(PERIODS), "monthly", E).inh("endpoints.defaults.budget_period"),
    d("cauldron.endpoints.{ep}.threshold_pct", "threshold", PCT, "80", E).inh("endpoints.defaults.threshold_pct"),
    d("cauldron.endpoints.{ep}.threshold_action", "at threshold", Ty::Choice(THRESHOLD_ACTIONS), "park_new", E).inh("endpoints.defaults.threshold_action"),
    d("cauldron.endpoints.{ep}.max_concurrent", "max concurrent", THREADS, "-1", E).inh("endpoints.defaults.max_concurrent"),
    d("cauldron.endpoints.{ep}.management_key", "management key", Ty::Secret, "", E),
    d("cauldron.endpoints.{ep}.price_in", "price in / 1M", Ty::Price, "0", E),
    d("cauldron.endpoints.{ep}.price_out", "price out / 1M", Ty::Price, "0", E),
    // … and what this cauldron takes of a global one: whether it is used
    // here (inheriting the endpoint's switch), its url, a NARROWER model
    // list, the allowance this cauldron may draw from the key (its metered
    // cap, design 0199 D3: the key's budget is the wallet), the threshold
    // and how many at once.
    d("cauldron.use.{ep}.enabled", "use here", Ty::Toggle, "on", E).inh("endpoints.{ep}.enabled"),
    d("cauldron.use.{ep}.url", "url", Ty::Url, "", E).inh("endpoints.{ep}.url"),
    d("cauldron.use.{ep}.narrow", "models", Ty::Toggle, "off", E),
    d("cauldron.use.{ep}.model_list", "models", Ty::List, "", E),
    d("cauldron.use.{ep}.metered_cap_usd", "metered cap", USD_OFF, "off", E),
    d("cauldron.use.{ep}.threshold_pct", "threshold", PCT, "80", E).inh("endpoints.{ep}.threshold_pct"),
    d("cauldron.use.{ep}.max_concurrent", "max concurrent", THREADS, "-1", E).inh("endpoints.{ep}.max_concurrent"),
    d("paths.root", "cauldron root", Ty::Path, "@env.root", C),
    d("paths.weights", "weights path", Ty::Path, "$root/data/shared/weights", C),
    // V1.8: the env file a human action's `route: env` value is written to,
    // per cauldron. No default (R-V1 item 8): unset, such a Submit is refused.
    d("secrets.env_file", "secrets env file", Ty::Path, "", C),
    // ── §2.1 pools: the token sources ──
    d("pools.{pool}.selection", "selection", Ty::Choice(SELECTIONS), "min_risk", P),
    d("pools.{pool}.failover", "failover", Ty::Toggle, "on", P),
    d("pools.{pool}.max_concurrent", "max concurrent", THREADS, "-1", P),
    // Whether its accounts are on, for every account that inherits it
    // (msg 235: "Default means, you know, if you change the global default,
    // you know, it's going to change here too").
    d("pools.{pool}.accounts_enabled", "accounts enabled", Ty::Toggle, "on", P),
    // A source's models are DETECTED, never typed (msg 235: "we never add
    // models to cloud code ... We just detect the models"): each one a
    // switch.
    d("pools.{pool}.model.{model}", "model", Ty::Toggle, "on", P),
    // ── §2.2 accounts ──
    d("pools.{pool}.accounts.{account}.enabled", "account enabled", Ty::Toggle, "on", A).inh("pools.{pool}.accounts_enabled"),
    d("pools.{pool}.accounts.{account}.share", "share", SHARE, "100", A),
    d("pools.{pool}.accounts.{account}.max_concurrent", "max concurrent", THREADS, "-1", A).inh("pools.{pool}.max_concurrent"),
    d("pools.{pool}.accounts.{account}.live_login", "live login", Ty::LiveLogin, "off", A),
    d("pools.{pool}.accounts.{account}.note", "note", Ty::Text, "", A),
    // ── §2.3 local inference ──
    d("local.enabled", "local inference", Ty::Toggle, "off", C),
    d("local.engine", "engine", Ty::Choice(ENGINES), "", C),
    d("local.models", "models", Ty::List, "", C),
    d("local.max_concurrent", "max concurrent", COUNT, "1", C),
    d("local.ram_max_gb", "RAM cap", GB, "8", C).cap("machine.ram_max_gb"),
    d("local.gpu", "GPU", Ty::Toggle, "on", C),
    d("local.disk_max_gb", "disk cap", GB, "20", C).cap("machine.disk.cap_gb"),
    // ── §3.1 cauldron defaults ──
    d("budgets.default.session.threshold_pct", "session threshold", PCT_OFF, "off", C),
    d("budgets.default.weekly.threshold_pct", "weekly threshold", PCT_OFF, "95", C),
    // Fable is Claude Code's: its band sets these (0214 A10); the cauldron-wide
    // default stays the root they inherit from, and is not shown.
    d("budgets.default.fable.threshold_pct", "fable threshold", PCT_OFF, "off", C).hide(),
    d("budgets.default.session.cap_pct", "session cap", PCT_OFF, "off", C),
    d("budgets.default.weekly.cap_pct", "weekly cap", PCT_OFF, "off", C),
    d("budgets.default.fable.cap_pct", "fable cap", PCT_OFF, "off", C).hide(),
    d("budgets.default.cash.threshold_usd", "cash threshold", USD_OFF, "off", C),
    d("budgets.default.cash.cap_usd_month", "cash cap / month", USD_OFF, "off", C),
    d("budgets.default.bar_pct", "hard bar", BAR, "100", C),
    d("budgets.default.threshold_action", "threshold action", Ty::Choice(THRESHOLD_ACTIONS), "park_new", C),
    d("budgets.default.reserve_pct", "reserve for you", PCT, "0", C),
    // ── §3.2 pool budget ──
    d("pools.{pool}.budget.session.threshold_pct", "session threshold", PCT_OFF, "off", P).inh("budgets.default.session.threshold_pct"),
    d("pools.{pool}.budget.weekly.threshold_pct", "weekly threshold", PCT_OFF, "95", P).inh("budgets.default.weekly.threshold_pct"),
    d("pools.{pool}.budget.fable.threshold_pct", "fable threshold", PCT_OFF, "off", P).inh("budgets.default.fable.threshold_pct"),
    d("pools.{pool}.budget.session.cap_pct", "session cap", PCT_OFF, "off", P).inh("budgets.default.session.cap_pct"),
    d("pools.{pool}.budget.weekly.cap_pct", "weekly cap", PCT_OFF, "off", P).inh("budgets.default.weekly.cap_pct"),
    d("pools.{pool}.budget.fable.cap_pct", "fable cap", PCT_OFF, "off", P).inh("budgets.default.fable.cap_pct"),
    d("pools.{pool}.budget.cash.threshold_usd", "cash threshold", USD_OFF, "off", P).inh("budgets.default.cash.threshold_usd"),
    d("pools.{pool}.budget.bar_pct", "hard bar", BAR, "100", P).inh("budgets.default.bar_pct"),
    d("pools.{pool}.budget.threshold_action", "threshold action", Ty::Choice(THRESHOLD_ACTIONS), "park_new", P).inh("budgets.default.threshold_action"),
    d("pools.{pool}.budget.reserve_pct", "reserve for you", PCT, "0", P).inh("budgets.default.reserve_pct"),
    // ── §3.3 account budget ──
    d("pools.{pool}.accounts.{account}.budget.session.threshold_pct", "session threshold", PCT_OFF, "off", A).inh("pools.{pool}.budget.session.threshold_pct"),
    d("pools.{pool}.accounts.{account}.budget.weekly.threshold_pct", "weekly threshold", PCT_OFF, "95", A).inh("pools.{pool}.budget.weekly.threshold_pct"),
    d("pools.{pool}.accounts.{account}.budget.fable.threshold_pct", "fable threshold", PCT_OFF, "off", A).inh("pools.{pool}.budget.fable.threshold_pct"),
    d("pools.{pool}.accounts.{account}.budget.session.cap_pct", "session cap", PCT_OFF, "off", A).inh("pools.{pool}.budget.session.cap_pct"),
    d("pools.{pool}.accounts.{account}.budget.weekly.cap_pct", "weekly cap", PCT_OFF, "off", A).inh("pools.{pool}.budget.weekly.cap_pct"),
    d("pools.{pool}.accounts.{account}.budget.fable.cap_pct", "fable cap", PCT_OFF, "off", A).inh("pools.{pool}.budget.fable.cap_pct"),
    d("pools.{pool}.accounts.{account}.budget.cash.threshold_usd", "cash threshold", USD_OFF, "off", A).inh("pools.{pool}.budget.cash.threshold_usd"),
    d("pools.{pool}.accounts.{account}.budget.bar_pct", "hard bar", BAR, "100", A).inh("pools.{pool}.budget.bar_pct"),
    // ── §3.4 opportunistic mode (8e4fca10 msg 235) ──
    // An account is OPPORTUNISTIC when it is not on pace to spend its week
    // (its pressure is above 1); then parked work may spend it freely. What
    // is set is only whether an account may be: a global default (on), a
    // source's, each account's.
    d("opportunistic.enabled", "opportunistic", Ty::Toggle, "on", C),
    d("pools.{pool}.opportunistic.enabled", "opportunistic", Ty::Toggle, "on", P).inh("opportunistic.enabled"),
    d("pools.{pool}.accounts.{account}.opportunistic.enabled", "opportunistic", Ty::Toggle, "on", A).inh("pools.{pool}.opportunistic.enabled"),
    // ── §4 roles → models ──
    d("roles.{role}.model", "model", Ty::Model { none: false }, "@role.model", C),
    d("roles.{role}.fallback", "fallback", Ty::Model { none: true }, "@role.fallback", C),
    // `source`, as the Roles table's column names it (0217 B12).
    d("roles.{role}.pool", "source", Ty::PoolRef { auto: true }, "@role.pool", C),
    // The endpoint a METERED role spends (design 0199 D8): set, the role's
    // work goes to it and its pool is not used.
    d("roles.{role}.endpoint", "endpoint", Ty::EndpointRef, "", C),
    // The dump ripple (design 0138 §6): how many groups apply at once, and
    // the most groups one dump is cut into.
    // ── §6 this machine ──
    d("machine.ram_max_gb", "RAM max", GB, "@machine.ram_75", C).note(MACHINE_RAM_NOTE),
    d("machine.ram_reserve_gb", "RAM reserve", GB, "4", C),
    d("machine.cpu_max_pct", "CPU max", PCT, "80", C),
    d("machine.max_child_processes", "child processes", COUNT, "12", C),
    d("machine.gpu.enabled", "GPU", Ty::Toggle, "on", C),
    d("machine.gpu.memory_max_gb", "GPU memory max", GB, "@machine.ram_75", C).inh("machine.ram_max_gb"),
    d("machine.disk.cap_gb", "disk cap", GB, "50", C),
    d("machine.disk.free_floor_gb", "free space floor", GB, "20", C),
    d("machine.on_battery", "on battery", Ty::Choice(BATTERY), "no_opportunistic", C),
    // ── §7 storage & retention / GC ──
    d("storage.threads.retention_age", "threads retention", DUR, "90d", C),
    d("storage.threads.cap_gb", "threads cap", GB, "10", C),
    d("storage.threads.eviction_order", "eviction order", Ty::Choice(EVICTION), "lru", C),
    d("storage.threads.protect_pinned", "protect pinned", Ty::Toggle, "on", C),
    d("storage.threads.protect_active_clusters", "protect active clusters", Ty::Toggle, "on", C),
    d("storage.threads.archive_before_delete", "archive before delete", Ty::Toggle, "off", C),
    d("storage.logs.retention_age", "logs retention", DUR, "30d", C),
    d("storage.logs.cap_mb", "logs cap", MB, "500", C),
    d("storage.supervisions.cap_mb", "supervisions cap", MB, "50", C),
    d("storage.usage_observations.retention_age", "usage samples retention", DUR, "180d", C),
    d("storage.account_backups.keep", "login backups kept", COUNT, "per prune rule", C),
    d("storage.tts.cap_gb", "tts cap", GB, "2", C),
    d("storage.tui.dumps_cap_mb", "dumps cap", MB, "50", C),
    d("storage.pins.good_history", "good versions kept", COUNT, "10", C),
    d("storage.gc.interval", "interval", DUR, "6h", C),
    d("storage.gc.dry_run", "dry run", Ty::Toggle, "off", C),
    d("storage.gc.on_disk_pressure", "on disk pressure", Ty::Toggle, "on", C),
    d("storage.gc.run_now", "run now", Ty::Action, "", C),
    // ── §8 permissions & identity ──
    d("permissions.mode", "permission mode", Ty::Choice(PERMISSION_MODES), "bypass", C),
    d("permissions.allowed_tools", "allowed tools", Ty::List, "", C),
    d("permissions.disallowed_tools", "disallowed tools", Ty::List, "", C),
    d("permissions.add_dirs", "extra dirs", Ty::List, "", C),
    d("permissions.plugin_roots", "plugin roots", Ty::List, "<project>/.mind/plugins, <repo>/core-plugins", C),
    d("permissions.plugin_dirs", "plugin dirs", Ty::List, "", C),
    d("permissions.repos", "repos", Ty::List, "", C),
    d("identity.socket_secret_required", "socket secret required", Ty::Toggle, "off", C),
    d("identity.live_login", "live login", Ty::AccountRef, "", C),
    d("identity.operator", "operator", Ty::Text, "cc1@example.com", C),
    // ── §9 attention, notifications & voice ──
    d("attention.autodecide", "autodecide", Ty::Choice(AUTODECIDE), "three_ways_floor", C),
    d("attention.interrupt_floor", "interrupt floor", Ty::Choice(INTERRUPT), "blocking_questions", C),
    d("notify.tts.enabled", "voice", Ty::Toggle, "on", C),
    d("notify.tts.voice", "voice id", Ty::Text, "daemon default", C),
    d("notify.tts.rate", "speaking rate", Ty::Number { unit: "wpm", step: 10, off: false, unlimited: false }, "daemon default", C),
    d("notify.limits.threshold_crossed", "announce limits", Ty::Toggle, "on", C),
    d("notify.limits.login_expiry_lead", "login expiry warning", DUR, "3d", C),
    d("notify.gc.report", "announce GC", Ty::Toggle, "on", C),
    d("notify.quiet_hours", "quiet hours", Ty::Hours, "off", C),
    // ── §10 mesh & services ──
    d("supervisor.auto_restart", "auto restart", Ty::Toggle, "off", C),
    d("supervisor.auto_rollback", "auto rollback", Ty::Toggle, "on", C),
    d("supervisor.max_start_failures", "start failures", COUNT, "3", C),
    d("supervisor.poll_secs", "poll", DUR, "4s", C),
    d("supervisor.down_after", "down after (probes)", COUNT, "3", C),
    d("supervisor.throttle_secs", "throttle", DUR, "10s", C),
    d("supervisor.backoff_base_secs", "backoff base", DUR, "2s", C),
    d("supervisor.backoff_max_secs", "backoff max", DUR, "300s", C),
    d("supervisor.stable_after_secs", "stable after", DUR, "60s", C),
    d("threads.probe_interval_secs", "probe interval", DUR, "3600s", C),
    d("threads.probe_interval_busy_secs", "busy probe interval", DUR, "120s", C),
    d("threads.calibrate", "calibrate", Ty::Toggle, "on", C),
    d("threads.edge_poll", "edge poll", Ty::Toggle, "on", C),
    d("threads.settle_ladder_secs", "settle ladder", Ty::List, "5s, 30s, 120s", C),
    d("threads.chassis_port", "chassis port", COUNT, "3653", C),
    d("threads.host", "host", Ty::Text, "127.0.0.1", C),
    d("threads.claude_bin", "claude binary", Ty::Path, "auto", C),
    d("mesh.devices", "devices", Ty::List, "this machine", C),
    d("mesh.dispatch_remote", "dispatch to other devices", Ty::Toggle, "off", C),
    d("mesh.accept_remote", "accept from other cauldrons", Ty::Toggle, "off", C),
    // ── 0071 S7: the repo registry ──
    // The roots the blueprint back-edge trace scans (core:blueprint
    // OQ-TRACE-ROOTS, open): kept here, read by nothing yet.
    d("repos.trace_roots", "trace roots", Ty::List, "the soup", C),
    // ── §11 interface ──
    // 8e4fca10 msg 235: "the display stuff, like light mode versus dark mode".
    d("ui.theme", "theme", Ty::Choice(THEMES), "dark", C),
    d("ui.sidebar_ratio", "sidebar", Ty::Percent { off: false, min: 5, max: 95 }, "auto", C),
    d("ui.default_tab", "opens on", Ty::Choice(TABS), "cauldron", C),
    // The default of the sidebar's `Aggregated clusters` (8e4fca10 msg 403).
    d("ui.aggregate_default", "Aggregated clusters", Ty::Toggle, "off", C),
    // Off: leaving the tab and coming back opens it with every band
    // collapsed (8e4fca10 msg 165).
    d("ui.preserve.config", "preserve Config expansion", Ty::Toggle, "off", C),
    d("ui.preserve.status", "preserve Status expansion", Ty::Toggle, "off", C),
];

/// The def whose key template is exactly `key`.
pub fn def(key: &str) -> &'static Def {
    DEFS.iter().find(|d| d.key == key).unwrap_or_else(|| panic!("no setting `{key}` in DEFS"))
}

// ── values ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Val {
    Bool(bool),
    Int(i64),
    Str(String),
    List(Vec<String>),
    /// Explicitly ungated.
    Off,
    /// No value and no default (an engine not chosen yet).
    Unset,
}

impl Val {
    pub fn as_bool(&self) -> bool {
        matches!(self, Val::Bool(true))
    }
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Val::Int(n) => Some(*n),
            _ => None,
        }
    }
    pub fn as_list(&self) -> &[String] {
        match self {
            Val::List(l) => l,
            _ => &[],
        }
    }
}

/// Which keys fill a template's placeholders.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Ctx {
    pub pool: String,
    pub account: String,
    pub role: String,
    pub service: String,
    /// A detected model of a source (`pools.{pool}.model.{model}`).
    pub model: String,
    /// An inference endpoint's minted id (`endpoints.{ep}.…`).
    pub ep: String,
}

impl Ctx {
    pub fn pool(p: &str) -> Ctx {
        Ctx { pool: p.into(), ..Ctx::default() }
    }
    pub fn account(p: &str, a: &str) -> Ctx {
        Ctx { pool: p.into(), account: a.into(), ..Ctx::default() }
    }
    pub fn role(r: &str) -> Ctx {
        Ctx { role: r.into(), ..Ctx::default() }
    }
    pub fn service(s: &str) -> Ctx {
        Ctx { service: s.into(), ..Ctx::default() }
    }
    pub fn model(p: &str, m: &str) -> Ctx {
        Ctx { pool: p.into(), model: m.into(), ..Ctx::default() }
    }
    pub fn ep(id: &str) -> Ctx {
        Ctx { ep: id.into(), ..Ctx::default() }
    }
}

/// `template` with its placeholders filled from `ctx`.
pub fn subst(template: &str, ctx: &Ctx) -> String {
    template
        .replace("{pool}", &ctx.pool)
        .replace("{account}", &ctx.account)
        .replace("{role}", &ctx.role)
        .replace("{service}", &ctx.service)
        .replace("{model}", &ctx.model)
        .replace("{ep}", &ctx.ep)
}

/// The def and placeholders a concrete stored `key` is.
pub fn lookup(key: &str) -> Option<(&'static Def, Ctx)> {
    for def in DEFS.iter().filter(|d| d.stored()) {
        if def.key.contains("{role}") {
            for r in ROLES {
                let mut ctx = Ctx::role(r.id);
                if match_simple(&def.key.replace("{role}", r.id), key, &mut ctx) {
                    return Some((def, ctx));
                }
            }
        } else {
            let mut ctx = Ctx::default();
            if match_simple(def.key, key, &mut ctx) {
                return Some((def, ctx));
            }
        }
    }
    None
}

fn match_simple(template: &str, key: &str, ctx: &mut Ctx) -> bool {
    let ts: Vec<&str> = template.split('.').collect();
    let ks: Vec<&str> = key.split('.').collect();
    if ts.len() != ks.len() {
        return false;
    }
    for (t, k) in ts.iter().zip(&ks) {
        let slot = match *t {
            "{pool}" => &mut ctx.pool,
            "{account}" => &mut ctx.account,
            "{service}" => &mut ctx.service,
            "{model}" => &mut ctx.model,
            // An endpoint's slot takes only a minted id, so `endpoints.defaults`
            // is never an endpoint.
            "{ep}" if !is_endpoint_id(k) => return false,
            "{ep}" => &mut ctx.ep,
            t if t == *k => continue,
            _ => return false,
        };
        if k.is_empty() {
            return false;
        }
        *slot = k.to_string();
    }
    true
}

/// The text of a default once its `@` references are filled: a role's
/// defaults from [`ROLES`], a service's from the mesh.
fn default_text(def: &Def, ctx: &Ctx, services: &BTreeMap<String, (String, String)>, root: &str) -> String {
    let r = role_of(&ctx.role);
    let s = services.get(&ctx.service);
    match def.default {
        "@role.model" => r.map_or("", |r| r.model).to_string(),
        "@role.fallback" => r.map_or("", |r| r.fallback).to_string(),
        "@role.pool" => r.map_or("", |r| r.pool).to_string(),
        "@service.version" => s.map_or(String::new(), |s| s.0.clone()),
        "@service.addr" => s.map_or(String::new(), |s| s.1.clone()),
        "@env.root" => root.to_string(),
        // 75% of THIS machine's RAM (F44); 48 where it cannot be read.
        "@machine.ram_75" => machine_ram_gb().map_or_else(|| "48".to_string(), |gb| (gb * 3 / 4).to_string()),
        other => other.to_string(),
    }
}

/// A default as a value. A default the type cannot parse (`daemon default`,
/// `per prune rule`) is kept as words, shown as written.
fn default_val(ty: Ty, text: &str) -> Val {
    if text.is_empty() {
        return match ty {
            Ty::List => Val::List(Vec::new()),
            _ => Val::Unset,
        };
    }
    match ty {
        Ty::List => Val::List(text.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()),
        _ => parse_input(ty, text).unwrap_or_else(|_| Val::Str(text.to_string())),
    }
}

/// Parse what he typed into a value of `ty`, or say why not in one line.
pub fn parse_input(ty: Ty, text: &str) -> std::result::Result<Val, String> {
    let t = text.trim();
    let off_ok = matches!(ty, Ty::Percent { off: true, .. } | Ty::Number { off: true, .. } | Ty::Duration { off: true } | Ty::Hours);
    if t.eq_ignore_ascii_case("off") && off_ok {
        return Ok(Val::Off);
    }
    match ty {
        Ty::Toggle | Ty::LiveLogin => match t {
            "on" | "true" => Ok(Val::Bool(true)),
            "off" | "false" => Ok(Val::Bool(false)),
            _ => Err("on or off".into()),
        },
        Ty::Percent { off, min, max } => {
            let why = || if off { format!("a percent {min}–{max}, or off") } else { format!("a percent {min}–{max}") };
            let n: i64 = t.trim_end_matches('%').trim().parse().map_err(|_| why())?;
            if n < min || n > max {
                return Err(why());
            }
            Ok(Val::Int(n))
        }
        Ty::Number { unlimited: true, .. } if t.eq_ignore_ascii_case("unlimited") => Ok(Val::Int(-1)),
        Ty::Number { unit, off, unlimited, .. } => {
            let digits = t.trim_start_matches('$').trim_end_matches(unit).trim();
            let n: i64 = digits.parse().map_err(|_| match (off, unlimited) {
                (true, _) => "a whole number, or off".to_string(),
                (_, true) => "a whole number, or -1 for unlimited".to_string(),
                _ => "a whole number".to_string(),
            })?;
            match (unlimited, n) {
                (true, n) if n < -1 => Err("not below -1 (unlimited)".into()),
                (false, n) if n < 0 => Err("not below 0".into()),
                _ => Ok(Val::Int(n)),
            }
        }
        Ty::Duration { off } => {
            if duration_secs(t).is_some() {
                Ok(Val::Str(t.to_string()))
            } else if off {
                Err("like 30s 15m 6h 90d, or off".into())
            } else {
                Err("like 30s 15m 6h 90d".into())
            }
        }
        Ty::Choice(opts) => opts.iter().find(|o| **o == t).map(|o| Val::Str(o.to_string())).ok_or_else(|| format!("one of {}", opts.join(", "))),
        Ty::Hours => {
            let ok = t.split_once('-').is_some_and(|(a, b)| hhmm(a) && hhmm(b));
            if ok { Ok(Val::Str(t.to_string())) } else { Err("HH:MM-HH:MM, or off".into()) }
        }
        Ty::Path if t.is_empty() => Err("a path".into()),
        Ty::Model { .. } | Ty::PoolRef { .. } | Ty::AccountRef | Ty::Path if t.is_empty() => Err("a name".into()),
        Ty::EndpointRef if !is_endpoint_id(t) => Err("an endpoint's id, like ep-1".into()),
        Ty::Url => match t.split_once("://") {
            Some(("http" | "https", rest)) if !rest.is_empty() && !rest.starts_with('/') && !t.contains(char::is_whitespace) => Ok(Val::Str(t.to_string())),
            _ => Err("a url: http:// or https:// and a host".into()),
        },
        Ty::Model { .. } | Ty::PoolRef { .. } | Ty::AccountRef | Ty::EndpointRef | Ty::Text | Ty::Path => Ok(Val::Str(t.to_string())),
        Ty::Price => {
            let n: f64 = t.trim_start_matches('$').trim().parse().map_err(|_| "dollars per million tokens, like 0.15".to_string())?;
            if !n.is_finite() || n < 0.0 {
                return Err("not below 0".into());
            }
            Ok(Val::Str(trim_price(n)))
        }
        Ty::Secret if t.is_empty() => Err("paste the key".into()),
        Ty::Secret => Ok(Val::Str(t.to_string())),
        Ty::List => Err("a list is edited in its own menu".into()),
        Ty::Action => Err("an action holds no value".into()),
    }
}

/// A price as it is kept: no trailing zeros (`0.15`, `3`).
fn trim_price(n: f64) -> String {
    let s = format!("{n:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn hhmm(s: &str) -> bool {
    let Some((h, m)) = s.split_once(':') else { return false };
    matches!((h.parse::<u32>(), m.parse::<u32>()), (Ok(h), Ok(m)) if h < 24 && m < 60 && m.to_string().len() <= 2)
}

/// `90d` → seconds.
pub fn duration_secs(s: &str) -> Option<u64> {
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: u64 = n.parse().ok()?;
    let mul = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => return None,
    };
    Some(n * mul)
}

/// `+` / `-` on a value: percent by 5, a number by its step, a duration by
/// one of its unit. `None` when the value cannot be nudged.
pub fn nudge(ty: Ty, v: &Val, up: bool) -> Option<Val> {
    let dir = if up { 1 } else { -1 };
    match (ty, v) {
        (Ty::Percent { min, max, .. }, Val::Int(n)) => Some(Val::Int((n + 5 * dir).clamp(min, max))),
        (Ty::Percent { off: true, min, .. }, Val::Off) if up => Some(Val::Int(min.max(5))),
        // A percent that says a word (the sidebar's `auto`, a third) steps
        // from the share the word stands for: + and − always move it (0212).
        (Ty::Percent { min, max, .. }, Val::Str(_)) => Some(Val::Int((33 + 5 * dir).clamp(min, max))),
        // An unlimited count steps -1 ⇄ 0 by one, whatever its step.
        (Ty::Number { unlimited: true, .. }, Val::Int(-1)) if up => Some(Val::Int(0)),
        (Ty::Number { unlimited: true, .. }, Val::Int(0)) if !up => Some(Val::Int(-1)),
        (Ty::Number { unlimited: true, .. }, Val::Int(-1)) => None,
        (Ty::Number { step, .. }, Val::Int(n)) => Some(Val::Int((n + step * dir).max(0))),
        (Ty::Number { off: true, step, .. }, Val::Off) if up => Some(Val::Int(step)),
        (Ty::Number { step, .. }, Val::Str(_)) if up => Some(Val::Int(step)),
        (Ty::Duration { .. }, Val::Str(s)) => {
            let at = s.find(|c: char| !c.is_ascii_digit())?;
            let n: i64 = s[..at].parse().ok()?;
            Some(Val::Str(format!("{}{}", (n + dir).max(1), &s[at..])))
        }
        _ => None,
    }
}

/// A value as a row shows it (a toggle is drawn by the switch instead).
pub fn show(def: &Def, v: &Val) -> String {
    match v {
        Val::Bool(b) => if *b { "on" } else { "off" }.to_string(),
        Val::Int(n) => match def.ty {
            Ty::Percent { .. } => format!("{n} %"),
            Ty::Number { unlimited: true, .. } if *n == -1 => "unlimited".to_string(),
            Ty::Number { unit: "$", .. } => format!("${n}"),
            Ty::Number { unit: "", .. } => n.to_string(),
            Ty::Number { unit, .. } => format!("{n} {unit}"),
            _ => n.to_string(),
        },
        // A secret is never drawn: its prefix and its last four.
        Val::Str(s) if def.ty == Ty::Secret => mask(s),
        Val::Str(s) if def.ty == Ty::Price => format!("${s}"),
        // A choice is said in words, never as its identifier (task 0209
        // D11): `park new`, `min risk`, `oldest created`.
        Val::Str(s) if matches!(def.ty, Ty::Choice(_)) => match s.as_str() {
            "lru" => "least recently used".to_string(),
            "llama_cpp" => "llama.cpp".to_string(),
            _ => s.replace('_', " "),
        },
        // A token source by its name, as Providers says it (0211 R9): `Claude
        // Code`, never `claude_code`; an account as `account · source`.
        Val::Str(s) if matches!(def.ty, Ty::PoolRef { .. }) => crate::cauldron::menu::pool_label(s),
        Val::Str(s) if def.ty == Ty::AccountRef => match s.split_once('/') {
            Some((pool, acct)) => format!("{acct} · {}", crate::cauldron::menu::pool_label(pool)),
            None if s.is_empty() => "none".to_string(),
            None => s.clone(),
        },
        Val::Str(s) => s.clone(),
        Val::List(l) if l.is_empty() => "none".to_string(),
        Val::List(l) => l.join(", "),
        Val::Off => "off".to_string(),
        Val::Unset if def.ty == Ty::AccountRef => "none".to_string(),
        Val::Unset => "unset".to_string(),
    }
}

// ── one settings file ───────────────────────────────────────────────────────

fn to_toml(v: &Val) -> Option<toml::Value> {
    Some(match v {
        Val::Bool(b) => toml::Value::Boolean(*b),
        Val::Int(n) => toml::Value::Integer(*n),
        Val::Str(s) => toml::Value::String(s.clone()),
        Val::List(l) => toml::Value::Array(l.iter().cloned().map(toml::Value::String).collect()),
        Val::Off => toml::Value::String("off".into()),
        Val::Unset => return None,
    })
}

fn from_toml(def: &Def, v: &toml::Value) -> std::result::Result<Val, String> {
    match (def.ty, v) {
        (Ty::Toggle, toml::Value::Boolean(b)) => Ok(Val::Bool(*b)),
        (Ty::Percent { .. } | Ty::Number { .. } | Ty::Price, toml::Value::Integer(n)) => parse_input(def.ty, &n.to_string()),
        (Ty::Price, toml::Value::Float(n)) => parse_input(def.ty, &n.to_string()),
        (Ty::List, toml::Value::Array(a)) => a
            .iter()
            .map(|x| x.as_str().map(str::to_string).ok_or_else(|| "a list of words".to_string()))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map(Val::List),
        (Ty::Toggle | Ty::List, _) => Err(format!("expected {}", if def.ty == Ty::Toggle { "true or false" } else { "a list" })),
        (_, toml::Value::String(s)) => parse_input(def.ty, s),
        _ => Err("wrong kind of value".into()),
    }
}

/// The explicit values of ONE file: the cauldron's, or the global one.
#[derive(Debug, Clone, Default)]
pub struct Store {
    pub vals: BTreeMap<String, Val>,
    /// Retired keys the file still held ([`RETIRED`]): read, never used.
    pub retired: Vec<String>,
    /// Those retired entries as the file held them: written back at every
    /// save, so retiring a key never deletes what he wrote (task 0209 D1).
    pub kept: Vec<(String, toml::Value)>,
    /// Keys this build does not know, or values it cannot read, as the file
    /// held them (0220 E3): kept, written back untouched, never used, and
    /// said once — never a reason to refuse the face (a key from a newer
    /// build, or a typo, must not lock him out).
    pub unknown: Vec<(String, toml::Value)>,
    /// Why each of [`Store::unknown`] is not used, in his words.
    pub warnings: Vec<String>,
    /// The file's text as read: a save edits THIS, so his comments and order
    /// survive (0220 E3).
    pub source: Option<String>,
}

impl Store {
    /// Parse a settings file's text. A key that is not a setting of this kind
    /// of file, or a value not of its type, is KEPT and named in
    /// [`Store::warnings`], never used (0220 E3: refusing the whole face over
    /// one line locked him out of it). Only text that is not TOML at all is
    /// refused.
    pub fn parse(text: &str, what: &str) -> Result<Store> {
        let table: toml::Table = text.parse().with_context(|| format!("parse {what}"))?;
        let mut flat = Vec::new();
        flatten("", &table, &mut flat);
        let mut vals = BTreeMap::new();
        let mut retired = Vec::new();
        let mut kept = Vec::new();
        let mut unknown = Vec::new();
        let mut warnings = Vec::new();
        for (key, v) in flat {
            let key = renamed(&key).unwrap_or(key);
            let v = match RENAMED_VALUES.iter().find(|(k, old, _)| *k == key && v.as_str() == Some(old)) {
                Some((_, _, new)) => toml::Value::String(new.to_string()),
                None => v,
            };
            if (lookup(&key).is_none() || RETIRED_PREFIXES.iter().any(|p| key.starts_with(p))) && is_retired(&key) {
                retired.push(key.clone());
                kept.push((key, v));
                continue;
            }
            if RETIRED_VALUES.iter().any(|(k, x)| *k == key && v.as_str() == Some(x)) {
                retired.push(format!("{key} = {}", v.as_str().unwrap_or_default()));
                kept.push((key, v));
                continue;
            }
            let Some((def, _)) = lookup(&key) else {
                warnings.push(format!("{what}: `{key}` is not a setting — kept in the file, not used"));
                unknown.push((key, v));
                continue;
            };
            match from_toml(def, &v) {
                Ok(val) => {
                    vals.insert(key, val);
                }
                Err(e) => {
                    warnings.push(format!("{what}: `{key}`: {e} — kept in the file, not used"));
                    unknown.push((key, v));
                }
            }
        }
        Ok(Store { vals, retired, kept, unknown, warnings, source: Some(text.to_string()) })
    }

    pub fn load(path: &Path) -> Result<Option<Store>> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(Some(Store::parse(&s, &path.display().to_string())?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    /// The file's text: the text it was read from ([`Store::source`]) with
    /// only the values changed — set, added or removed — so every comment he
    /// wrote, his order and every key this build does not know survive the
    /// save (0220 E3). An endpoint being added ([`DRAFT_EP`]) is never
    /// written: until it is added it lives only in the face that is adding it.
    pub fn to_text(&self) -> Result<String> {
        self.to_text_over(self.source.as_deref())
    }

    /// Take `text`, just written to this store's file, as what it holds: the
    /// file as it now is (another face's values in it included) is the base
    /// of the next save's changes. An endpoint being added stays.
    fn take_written(&mut self, text: &str) {
        let Ok(mut now) = Store::parse(text, "the file as written") else { return };
        now.vals.extend(self.vals.iter().filter(|(k, _)| k.split('.').any(|seg| seg == DRAFT_EP)).map(|(k, v)| (k.clone(), v.clone())));
        *self = now;
    }

    /// Every value this store would write, by key: its live values, then
    /// what it keeps as he wrote it (retired entries, keys this build does not
    /// know) where no live value replaced them.
    fn want(&self) -> BTreeMap<String, toml::Value> {
        let mut want: BTreeMap<String, toml::Value> = BTreeMap::new();
        for (k, v) in self.vals.iter().filter(|(k, _)| !k.split('.').any(|seg| seg == DRAFT_EP)) {
            if let Some(tv) = to_toml(v) {
                want.insert(k.clone(), tv);
            }
        }
        for (k, v) in self.kept.iter().chain(&self.unknown) {
            want.entry(k.clone()).or_insert_with(|| v.clone());
        }
        want
    }

    /// [`Store::to_text`] over `base`, the file as it is on disk now. Only
    /// what THIS store changed since it was read ([`Store::source`]) is
    /// applied, so a value another face wrote since — or a comment he added
    /// by hand — is kept, never reverted by a whole-file write (0223
    /// adversarial pass).
    pub fn to_text_over(&self, base: Option<&str>) -> Result<String> {
        let want = self.want();
        let was = match (base, self.source.as_deref()) {
            (Some(_), Some(src)) => Store::parse(src, "the file as read").map(|s| s.want()).unwrap_or_default(),
            _ => BTreeMap::new(),
        };
        let changed: BTreeMap<&String, &toml::Value> = want.iter().filter(|(k, v)| was.get(*k) != Some(*v)).collect();
        let removed: std::collections::BTreeSet<&String> = was.keys().filter(|k| !want.contains_key(*k)).collect();
        let mut doc: toml_edit::DocumentMut = base.unwrap_or("").parse().context("the settings file is not TOML")?;
        // Keys are matched as the face names them: a renamed key's old
        // spelling in the file is the same setting, written under its new
        // name.
        let mut have = Vec::new();
        edit_keys(doc.as_table(), &mut Vec::new(), &mut have);
        let mut set: Vec<(&String, &toml::Value)> = changed.into_iter().collect();
        for path in have {
            let dotted = path.join(".");
            let named = renamed(&dotted).unwrap_or_else(|| dotted.clone());
            if removed.contains(&named) || named != dotted {
                edit_remove(doc.as_table_mut(), &path);
            }
            if named != dotted {
                if let Some((k, v)) = want.get_key_value(&named) {
                    set.push((k, v));
                }
            }
        }
        for (key, tv) in set {
            let segs: Vec<&str> = key.split('.').collect();
            let v: toml_edit::Value = format!("v = {tv}").parse::<toml_edit::DocumentMut>().ok().and_then(|d| d.get("v").and_then(|i| i.as_value()).cloned()).with_context(|| format!("`{key}` cannot be written"))?;
            edit_set(doc.as_table_mut(), &segs, v).with_context(|| format!("`{key}` sits under a value"))?;
        }
        Ok(doc.to_string())
    }
}

/// Every value's key path in a `toml_edit` table (tables recursed, inline
/// ones and dotted keys too).
fn edit_keys(t: &dyn toml_edit::TableLike, prefix: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
    for (k, item) in t.iter() {
        prefix.push(k.to_string());
        match item.as_table_like() {
            Some(sub) => edit_keys(sub, prefix, out),
            None if !item.is_none() => out.push(prefix.clone()),
            None => {}
        }
        prefix.pop();
    }
}

/// Remove the value at `path`, and every table the removal leaves empty —
/// unless he wrote a comment on it, which stays with its header.
fn edit_remove(t: &mut dyn toml_edit::TableLike, path: &[String]) {
    match path {
        [last] => {
            t.remove(last);
        }
        [first, rest @ ..] => {
            let emptied = match t.get_mut(first) {
                Some(item) => {
                    if let Some(sub) = item.as_table_like_mut() {
                        edit_remove(sub, rest);
                    }
                    let commented = |d: &toml_edit::Decor| d.prefix().and_then(|p| p.as_str()).is_some_and(|p| p.contains('#'));
                    match item {
                        toml_edit::Item::Table(sub) => sub.is_empty() && !commented(sub.decor()),
                        toml_edit::Item::Value(toml_edit::Value::InlineTable(sub)) => sub.is_empty() && !commented(sub.decor()),
                        _ => false,
                    }
                }
                None => false,
            };
            if emptied {
                t.remove(first);
            }
        }
        [] => {}
    }
}

/// The same value, however it is spelled or commented.
fn same_value(a: &toml_edit::Value, b: &toml_edit::Value) -> bool {
    let read = |v: &toml_edit::Value| {
        let mut v = v.clone();
        v.decor_mut().clear();
        format!("v = {v}").parse::<toml::Table>().ok().and_then(|mut t| t.remove("v"))
    };
    read(a).is_some() && read(a) == read(b)
}

/// Set the value at `segs`, keeping the old value's comments (`# …` after it)
/// and leaving it as written when it already holds that value.
fn edit_set(t: &mut dyn toml_edit::TableLike, segs: &[&str], v: toml_edit::Value) -> Result<()> {
    let [first, rest @ ..] = segs else { return Ok(()) };
    if rest.is_empty() {
        match t.get_mut(first) {
            Some(toml_edit::Item::Value(old)) => {
                if !same_value(old, &v) {
                    let decor = old.decor().clone();
                    *old = v;
                    *old.decor_mut() = decor;
                }
            }
            Some(other) => *other = toml_edit::Item::Value(v),
            None => {
                t.insert(first, toml_edit::Item::Value(v));
            }
        }
        return Ok(());
    }
    if t.get(first).is_none_or(|i| i.is_none()) {
        let mut sub = toml_edit::Table::new();
        sub.set_implicit(true);
        t.insert(first, toml_edit::Item::Table(sub));
    }
    let sub = t.get_mut(first).and_then(|i| i.as_table_like_mut()).context("not a table")?;
    edit_set(sub, rest, v)
}

/// A file's text, or `None` when there is no file.
fn read_if_any(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

fn flatten(prefix: &str, t: &toml::Table, out: &mut Vec<(String, toml::Value)>) {
    for (k, v) in t {
        let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        match v {
            toml::Value::Table(sub) => flatten(&key, sub, out),
            other => out.push((key, other.clone())),
        }
    }
}

/// The id of the endpoint being added in the add modal: its values are kept
/// like any endpoint's while he fills it in, never saved, and moved to a
/// minted id when he adds it.
pub const DRAFT_EP: &str = "ep-0";

/// The keys an endpoint registered `here` (in this cauldron) or globally
/// lives under: `cauldron.endpoints` or `endpoints`.
pub fn endpoint_prefix(here: bool) -> &'static str {
    if here {
        "cauldron.endpoints"
    } else {
        "endpoints"
    }
}

// ── accounts added here ─────────────────────────────────────────────────────

/// An account added to a pool in this face (a FAKE stand-in for
/// `claude-accounts add`). It is written with no settings of its own: it
/// inherits every value from its pool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AddedAccount {
    pub pool: String,
    pub id: String,
    pub label: String,
    pub added_at: String,
}

/// A pool added in this face (`Settings › Pools › + add pool`): an id and
/// when. FAKE, as an added account is: it has no accounts until he adds them
/// and inherits every value from the cauldron's defaults.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AddedPool {
    pub id: String,
    pub added_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AccountsFile {
    #[serde(default)]
    pub accounts: Vec<AddedAccount>,
    /// Pools added here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pools: Vec<AddedPool>,
    /// A seed account's label as renamed here, by `pool/id` (an account
    /// added here is renamed in place). The id never changes: its settings
    /// are keyed by it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub renamed: BTreeMap<String, String>,
    /// Seed accounts removed here, as `pool/id`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
}

/// One flip of an account's opportunistic flag (0057 `opportunistic_mode`):
/// when, which account, which way, and what changed to cause it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Flip {
    /// RFC 3339.
    pub at: String,
    pub pool: String,
    pub account: String,
    pub on: bool,
    pub why: String,
}

// ── the settings of one environment ─────────────────────────────────────────

/// Where a value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prov {
    /// Set at this level.
    Here,
    /// Taken from above: the level that holds it.
    Inherited(String),
    /// Nothing set anywhere: the default.
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub val: Val,
    pub prov: Prov,
}


pub struct Settings {
    /// `<data>/<env>/tui/soup`.
    pub dir: PathBuf,
    /// `<data>/shared/tui`: the GLOBAL values ([`is_global`]), the accounts
    /// and pools added, and the flip log — one copy for every cauldron.
    pub global_dir: PathBuf,
    pub cauldron: Store,
    /// The global values ([`is_global`]).
    pub global: Store,
    /// Global keys this cauldron's own file still held: adopted into the
    /// global file when it did not exist yet, else dropped from this file
    /// at its next save. Said once, not silently.
    pub moved: Vec<String>,
    pub added: Vec<AddedAccount>,
    /// The rest of `accounts.json`: pools added, accounts renamed and removed.
    pub accounts_file: AccountsFile,
    /// The flips recorded here, oldest first.
    pub flips: Vec<Flip>,
    /// Each service's version and address on the mesh: its settings' defaults.
    pub services: BTreeMap<String, (String, String)>,
    /// Every pool's id.
    pub pool_ids: Vec<String>,
    /// This environment's own defaults ([`ENV_DEFAULTS`]), by concrete key.
    pub env_defaults: BTreeMap<String, Val>,
    /// The cauldron's root as the environment resolves it: `paths.root`'s
    /// default.
    pub root: String,
    /// The seed's FAKE inference endpoints under a global file of his own,
    /// and the seed's own endpoints and uses of them under a cauldron file
    /// of his own: READ beneath his values, never written into his files
    /// (task 0209 D1). Where the seed is itself the store (no file yet) its
    /// endpoints and role bindings are split off into this layer too.
    pub seed_global: Store,
    pub seed_cauldron: Store,
    /// Global keys this cauldron's file held, adopted into the global values
    /// in memory only: written into the global file before this cauldron's
    /// file is next written without them, so opening writes nothing.
    pending_global: std::cell::Cell<bool>,
    /// Which keys those are.
    adopted: Vec<String>,
    /// This cauldron's file on disk still holds global keys: they leave it
    /// at the first write of either file, so a global value he clears is
    /// never re-adopted from the stale copy (0211 R1).
    held_on_disk: std::cell::Cell<bool>,
    writable: bool,
}

impl Settings {
    pub fn path(&self) -> PathBuf {
        self.dir.join("settings.toml")
    }

    /// The global values' file.
    pub fn global_path(&self) -> PathBuf {
        self.global_dir.join("settings.toml")
    }

    fn accounts_path(&self) -> PathBuf {
        self.global_dir.join("accounts.json")
    }

    /// Where an environment kept its accounts before they were global: read
    /// until the global file exists, never written.
    fn legacy_accounts_path(&self) -> PathBuf {
        self.dir.join("accounts.json")
    }

    /// The environment's settings: its file, or until that exists its seed;
    /// the GLOBAL file under `global_dir` (or until it
    /// exists `global_seed`, with whatever global keys this cauldron's file
    /// held adopted into it); the accounts added.
    #[allow(clippy::too_many_arguments)]
    pub fn open(dir: &Path, global_dir: &Path, env: &str, seed: Option<&Path>, global_seed: Option<&Path>, writable: bool, services: BTreeMap<String, (String, String)>) -> Result<Settings> {
        let mut s = Settings {
            dir: dir.to_path_buf(),
            global_dir: global_dir.to_path_buf(),
            cauldron: Store::default(),
            global: Store::default(),
            moved: Vec::new(),
            added: Vec::new(),
            accounts_file: AccountsFile::default(),
            flips: Vec::new(),
            services,
            pool_ids: Vec::new(),
            env_defaults: env_defaults(env),
            root: String::new(),
            seed_global: Store::default(),
            seed_cauldron: Store::default(),
            pending_global: std::cell::Cell::new(false),
            adopted: Vec::new(),
            held_on_disk: std::cell::Cell::new(false),
            writable,
        };
        let own_file = Store::load(&s.path())?;
        let from_file = own_file.is_some();
        s.cauldron = match own_file {
            Some(st) => st,
            // A seed is read, never copied: his file's first write holds
            // only his values, not the seed's text.
            None => match seed {
                Some(p) if p.is_file() => Store { source: None, ..Store::load(p)?.unwrap_or_default() },
                _ => Store::default(),
            },
        };
        // The global keys are kept once, for every cauldron (msg 186, msg
        // 235). A cauldron file written before a key was global still holds
        // it: a value the global file does not hold yet is ADOPTED into it
        // (the first cauldron to open carries its value over); every held
        // key is then dropped from the cauldron's file, said once.
        // Until the global file exists, the cauldron's own values win over
        // the global seed's; once it does, they fill only what it lacks.
        let held: Vec<String> = s.cauldron.vals.keys().filter(|k| is_global(k)).cloned().collect();
        let global_file = Store::load(&s.global_path())?;
        let fresh = global_file.is_none();
        s.global = match global_file {
            Some(g) => g,
            None => match global_seed {
                Some(p) if p.is_file() => Store { source: None, ..Store::load(p)?.unwrap_or_default() },
                _ => Store::default(),
            },
        };
        // Opening a cauldron writes NOTHING into his files (task 0209 D1):
        // the held keys are adopted in memory and move on disk only when a
        // file is next written for a change he made.
        for k in &held {
            if let (true, Some(v)) = (fresh || !s.global.vals.contains_key(k), s.cauldron.vals.get(k)) {
                s.global.vals.insert(k.clone(), v.clone());
                s.pending_global.set(from_file);
                s.adopted.push(k.clone());
            }
        }
        for k in &held {
            s.cauldron.vals.remove(k);
        }
        s.held_on_disk.set(from_file && !held.is_empty());
        // Said only for a file of its own: a seed is read, never written.
        s.moved = if from_file { held } else { Vec::new() };
        // The inference endpoints arrived after his files did (design 0199):
        // the seed's FAKE endpoints are READ beneath a file of his own, as
        // every other fake value is, and never copied into it. A role is
        // never bound to one in his file: the seed's role bindings are read
        // only where the seed is itself the store.
        if fresh {
            // The seed is the store until his file exists: its endpoints move
            // to the layer beneath, so the file's first write holds none.
            let (eps, rest) = std::mem::take(&mut s.global.vals).into_iter().partition(|(k, _)| k.starts_with("endpoints."));
            (s.seed_global.vals, s.global.vals) = (eps, rest);
        } else if let Some(seed) = global_seed.filter(|p| p.is_file()).map(Store::load).transpose()?.flatten() {
            s.seed_global.vals = seed.vals.into_iter().filter(|(k, _)| k.starts_with("endpoints.")).collect();
        }
        let ours = |k: &str| k.starts_with("cauldron.endpoints.") || k.starts_with("cauldron.use.");
        let bound = |k: &str| k.starts_with("roles.") && k.ends_with(".endpoint");
        if from_file {
            if let Some(seed) = seed.filter(|p| p.is_file()).map(Store::load).transpose()?.flatten() {
                s.seed_cauldron.vals = seed.vals.into_iter().filter(|(k, _)| ours(k)).collect();
            }
        } else {
            let (eps, rest) = std::mem::take(&mut s.cauldron.vals).into_iter().partition(|(k, _)| ours(k) || bound(k));
            (s.seed_cauldron.vals, s.cauldron.vals) = (eps, rest);
        }
        s.accounts_file = s.read_accounts()?;
        s.added = s.accounts_file.accounts.clone();
        // The flip log is global; one an environment kept before is still read.
        for flips in [dir.join("flips.jsonl"), s.flips_path()] {
            match std::fs::read_to_string(&flips) {
                Ok(t) => {
                    for l in t.lines().filter(|l| !l.trim().is_empty()) {
                        s.flips.push(serde_json::from_str(l).with_context(|| format!("parse a line of {}", flips.display()))?);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("read {}", flips.display())),
            }
        }
        Ok(s)
    }

    /// Every line of a settings file here this build kept but does not use
    /// (a key it does not know, a value it cannot read; 0220 E3).
    pub fn warnings(&self) -> Vec<String> {
        self.cauldron.warnings.iter().chain(&self.global.warnings).cloned().collect()
    }

    /// Every retired key a settings file here still held, as `file: key`.
    pub fn retired(&self) -> Vec<String> {
        let mut out: Vec<String> = self.cauldron.retired.clone();
        out.extend(self.global.retired.iter().map(|k| format!("global/{k}")));
        out
    }

    fn flips_path(&self) -> PathBuf {
        self.global_dir.join("flips.jsonl")
    }

    /// The accounts file: the global one, or until it exists the one this
    /// environment kept before accounts were global (adopted at the first
    /// change).
    fn read_accounts(&self) -> Result<AccountsFile> {
        for path in [self.accounts_path(), self.legacy_accounts_path()] {
            match std::fs::read_to_string(&path) {
                Ok(t) => return serde_json::from_str::<AccountsFile>(&t).with_context(|| format!("parse {}", path.display())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
            }
        }
        Ok(AccountsFile::default())
    }

    /// Change `accounts.json` under its lock, from the file as it is inside
    /// the lock (another face may have written it), and adopt the result.
    fn update_accounts(&mut self, f: impl FnOnce(&mut AccountsFile) -> Result<()>) -> Result<()> {
        if !self.writable {
            bail!("nothing is changed by --dump");
        }
        let path = self.accounts_path();
        let file = env::lock::with_lock(&path, || {
            let mut file = self.read_accounts()?;
            f(&mut file)?;
            crate::cauldron::store::write_text_atomic(&path, &serde_json::to_string_pretty(&file)?)?;
            Ok(file)
        })?;
        self.added = file.accounts.clone();
        self.accounts_file = file;
        Ok(())
    }

    /// Append flips to the flip log (one JSON line each, under its lock).
    pub fn record_flips(&mut self, flips: Vec<Flip>) -> Result<()> {
        if flips.is_empty() {
            return Ok(());
        }
        if self.writable {
            use std::io::Write;
            let path = self.flips_path();
            std::fs::create_dir_all(&self.global_dir).with_context(|| format!("create {}", self.global_dir.display()))?;
            env::lock::with_lock(&path, || {
                let mut text = String::new();
                for f in &flips {
                    text.push_str(&serde_json::to_string(f)?);
                    text.push('\n');
                }
                let mut fh = std::fs::OpenOptions::new().create(true).append(true).open(&path).with_context(|| format!("open {}", path.display()))?;
                fh.write_all(text.as_bytes())?;
                fh.sync_all()?;
                Ok(())
            })?;
        }
        self.flips.extend(flips);
        Ok(())
    }

    /// Rename account `id` of `pool` to `label`. Its id (and so every
    /// setting it has) stays.
    pub fn rename_account(&mut self, pool: &str, id: &str, label: &str) -> Result<()> {
        let label = label.trim().to_string();
        if label.is_empty() {
            bail!("an account needs a name");
        }
        let key = format!("{pool}/{id}");
        self.update_accounts(|f| {
            match f.accounts.iter_mut().find(|a| a.pool == pool && a.id == id) {
                Some(a) => a.label = label,
                None => {
                    f.renamed.insert(key, label);
                }
            }
            Ok(())
        })
    }

    /// Remove account `id` from `pool`: an account added here is deleted, a
    /// seed account is recorded as removed. Every setting it had goes with it
    /// (a later account of the same name starts clean), and so does the live
    /// login if it was this account.
    pub fn remove_account(&mut self, pool: &str, id: &str) -> Result<()> {
        let key = format!("{pool}/{id}");
        self.update_accounts(|f| {
            let before = f.accounts.len();
            f.accounts.retain(|a| !(a.pool == pool && a.id == id));
            if f.accounts.len() == before && !f.removed.contains(&key) {
                f.removed.push(key.clone());
            }
            f.renamed.remove(&key);
            Ok(())
        })?;
        let prefix = format!("pools.{pool}.accounts.{id}.");
        let had = self.global.vals.len();
        self.global.vals.retain(|k, _| !k.starts_with(&prefix));
        if self.global.vals.len() != had {
            self.save_global()?;
        }
        // What this cauldron took of it goes too.
        let mine = format!("cauldron.accounts.{pool}.{id}.");
        let had = self.cauldron.vals.len();
        self.cauldron.vals.retain(|k, _| !k.starts_with(&mine));
        if self.cauldron.vals.len() != had {
            self.save()?;
        }
        if self.live_login().as_deref() == Some(key.as_str()) {
            self.clear(self::def("identity.live_login"), &Ctx::default())?;
        }
        Ok(())
    }

    fn store(&self, key: &str) -> &Store {
        if is_global(key) {
            &self.global
        } else {
            &self.cauldron
        }
    }

    /// The seed layer read beneath `key`'s file ([`Settings::seed_global`]).
    fn seed_store(&self, key: &str) -> &Store {
        if is_global(key) {
            &self.seed_global
        } else {
            &self.seed_cauldron
        }
    }

    /// `key`'s explicit value: his file's, else the seed's beneath it — but
    /// never one of a seed endpoint he removed.
    fn val(&self, key: &str) -> Option<&Val> {
        if let Some(v) = self.store(key).vals.get(key) {
            return Some(v);
        }
        let v = self.seed_store(key).vals.get(key)?;
        // A seed value he set back to inheriting reads as nothing.
        if let Some((m, field)) = seed_mask(key) {
            if self.store(&m).vals.get(&m).is_some_and(|l| l.as_list().iter().any(|f| f == field)) {
                return None;
            }
        }
        match seed_endpoint_of(key) {
            Some((prefix, ep)) if {
                let removed = format!("{prefix}.{ep}.removed");
                self.store(&removed).vals.get(&removed).is_some_and(Val::as_bool)
            } =>
            {
                None
            }
            _ => Some(v),
        }
    }

    /// The value of `def` at the cauldron, walking its `inherits` chain:
    /// the value and the def it was found at, and whether it was explicit.
    fn chain(&self, def: &'static Def, ctx: &Ctx) -> (Val, &'static Def, bool) {
        let key = subst(def.key, ctx);
        if let Some(v) = self.val(&key) {
            return (v.clone(), def, true);
        }
        if let Some(v) = self.env_defaults.get(&key) {
            return (v.clone(), def, false);
        }
        match def.inherits {
            Some(p) => self.chain(self::def(p), ctx),
            None => (self.default_of(def, ctx), def, false),
        }
    }

    pub fn default_of(&self, def: &Def, ctx: &Ctx) -> Val {
        // Every token source: there is no per-pool switch (msg 165).
        if def.default == "@pools" {
            return Val::List(self.pool_ids.clone());
        }
        // An endpoint's template is its default (design 0199: the template
        // only prefills).
        if let Some(field) = def.default.strip_prefix("@template.") {
            let t = self.endpoint_template(def.key.starts_with("cauldron."), &ctx.ep);
            let text = match field {
                "name" => t.name,
                "url" => t.url,
                "auth" => t.auth,
                "protocol" => t.protocol,
                "key" => t.key,
                "headers" => t.headers,
                "models" => t.models,
                "model_list" => t.model_list,
                other => panic!("no template field `{other}`"),
            };
            return match def.ty {
                // A header holds a comma of its own: one per template.
                Ty::List if field == "headers" => Val::List(if text.is_empty() { Vec::new() } else { vec![text.to_string()] }),
                _ => default_val(def.ty, text),
            };
        }
        default_val(def.ty, &default_text(def, ctx, &self.services, &self.root))
    }

    /// The template endpoint `ep` prefills from: its own (`here`, this
    /// cauldron's) or a global one's.
    pub fn endpoint_template(&self, here: bool, ep: &str) -> &'static Template {
        let p = endpoint_prefix(here);
        let get = |f: &str| match self.val(&format!("{p}.{ep}.{f}")) {
            Some(Val::Str(s)) => s.clone(),
            _ => String::new(),
        };
        template(&get("template"), &get("server"))
    }

    /// The provenance of a value found at `at` by a def at `own`: inherited
    /// from a pool, an account or a sibling setting — or, from the
    /// cauldron's own level, just the DEFAULT (8e4fca10 msg 165: "You should
    /// just say default. It's just the default").
    fn from_label(own: &Def, at: &Def) -> Prov {
        if at.level == own.level {
            Prov::Inherited(at.label.to_string())
        } else if at.level == Level::Cauldron {
            Prov::Default
        } else {
            Prov::Inherited(at.level.label().to_string())
        }
    }

    /// The effective value of `def` (with `ctx`).
    pub fn get(&self, def: &'static Def, ctx: &Ctx) -> Resolved {
        if let Ty::LiveLogin = def.ty {
            let on = self.live_login().is_some_and(|l| l == format!("{}/{}", ctx.pool, ctx.account));
            return Resolved { val: Val::Bool(on), prov: Prov::Default };
        }
        let (val, at, explicit) = self.chain(def, ctx);
        let prov = if std::ptr::eq(at, def) {
            if explicit { Prov::Here } else { Prov::Default }
        } else {
            Self::from_label(def, at)
        };
        Resolved { val, prov }
    }

    /// What `def` would be WITHOUT a value of its own here: the value an
    /// override is shown against, and what clearing it returns to.
    pub fn inherited(&self, def: &'static Def, ctx: &Ctx) -> Resolved {
        if let Some(v) = self.env_defaults.get(&subst(def.key, ctx)) {
            return Resolved { val: v.clone(), prov: Prov::Default };
        }
        match def.inherits {
            Some(p) => {
                let r = self.get(self::def(p), ctx);
                let prov = match r.prov {
                    Prov::Here | Prov::Default => Self::from_label(def, self::def(p)),
                    p => p,
                };
                Resolved { val: r.val, prov }
            }
            None => Resolved { val: self.default_of(def, ctx), prov: Prov::Default },
        }
    }

    pub fn live_login(&self) -> Option<String> {
        match self.store("identity.live_login").vals.get("identity.live_login") {
            Some(Val::Str(s)) => Some(s.clone()),
            _ => None,
        }
    }

    /// Set `def`'s value here.
    pub fn set(&mut self, def: &'static Def, ctx: &Ctx, v: Val) -> Result<()> {
        if let Ty::LiveLogin = def.ty {
            let me = format!("{}/{}", ctx.pool, ctx.account);
            return if v.as_bool() {
                self.set(self::def("identity.live_login"), &Ctx::default(), Val::Str(me))
            } else if self.live_login().as_deref() == Some(me.as_str()) {
                self.clear(self::def("identity.live_login"), &Ctx::default())
            } else {
                Ok(())
            };
        }
        if !def.stored() {
            bail!("`{}` is not kept in a settings file", def.key);
        }
        let key = subst(def.key, ctx);
        if is_global(&key) {
            self.global.vals.insert(key, v);
            return self.save_global();
        }
        self.cauldron.vals.insert(key, v);
        self.save()
    }

    /// Delete `def`'s value here, so it inherits again.
    pub fn clear(&mut self, def: &'static Def, ctx: &Ctx) -> Result<()> {
        if let Ty::LiveLogin = def.ty {
            return self.set(def, ctx, Val::Bool(false));
        }
        let key = subst(def.key, ctx);
        let global = is_global(&key);
        let store = if global { &mut self.global } else { &mut self.cauldron };
        let mut changed = store.vals.remove(&key).is_some();
        // A value the seed holds beneath his file is never deleted from the
        // seed: what it inherits is written over it instead — only where it
        // differs; one that changes nothing already reads as inheriting.
        if self.seed_store(&key).vals.contains_key(&key) && self.is_set(def, ctx) {
            let store = if global { &mut self.global } else { &mut self.cauldron };
            match seed_mask(&key) {
                // Recorded as set back, so it truly inherits — a tri-state
                // reads `inherit` again — and nothing of the seed is copied.
                Some((m, field)) => {
                    let mut l = store.vals.get(&m).map(|v| v.as_list().to_vec()).unwrap_or_default();
                    if !l.iter().any(|f| f == field) {
                        l.push(field.to_string());
                    }
                    store.vals.insert(m, Val::List(l));
                }
                None => {
                    let v = self.inherited(def, ctx).val;
                    let store = if global { &mut self.global } else { &mut self.cauldron };
                    store.vals.insert(key, v);
                }
            }
            changed = true;
        }
        if changed {
            if global {
                self.save_global()?;
            } else {
                self.save()?;
            }
        }
        Ok(())
    }

    /// Whether `def` has a value of its own here: one in his file, or one
    /// the seed beneath it holds that differs from what it would inherit (a
    /// seed value that changes nothing reads as inheriting, so its ↺ and a
    /// tri-state's `inherit` mean what they say; 0212 adversarial pass).
    pub fn is_set(&self, def: &'static Def, ctx: &Ctx) -> bool {
        let key = subst(def.key, ctx);
        if self.store(&key).vals.contains_key(&key) {
            return true;
        }
        self.val(&key).is_some_and(|v| *v != self.inherited(def, ctx).val)
    }

    /// Write the cauldron's file whole, atomically. On the `--dump` path
    /// nothing is written and the change lives only in this face.
    fn save(&mut self) -> Result<()> {
        if !self.writable {
            return Ok(());
        }
        // The global keys this file held go into the global file first, so
        // writing this one without them loses nothing — merged into the file
        // as it is on disk (another face may have written it since), each
        // only where it holds none.
        if self.pending_global.get() {
            let path = self.global_path();
            let text = env::lock::with_lock(&path, || {
                let text = match Store::load(&path)? {
                    Some(mut disk) => {
                        for k in &self.adopted {
                            if let (false, Some(v)) = (disk.vals.contains_key(k), self.global.vals.get(k)) {
                                disk.vals.insert(k.clone(), v.clone());
                            }
                        }
                        disk.to_text()?
                    }
                    None => self.global.to_text()?,
                };
                crate::cauldron::store::write_text_atomic(&path, &text)?;
                Ok(text)
            })?;
            self.global.take_written(&text);
            self.pending_global.set(false);
        }
        let own = self.path();
        let text = env::lock::with_lock(&own, || {
            let disk = read_if_any(&own)?;
            let text = self.cauldron.to_text_over(disk.as_deref().or(self.cauldron.source.as_deref()))?;
            crate::cauldron::store::write_text_atomic(&own, &text)?;
            Ok(text)
        })?;
        self.cauldron.take_written(&text);
        self.held_on_disk.set(false);
        Ok(())
    }

    /// Write the global file whole, atomically, under its lock (every
    /// cauldron's face may write it).
    fn save_global(&mut self) -> Result<()> {
        if !self.writable {
            return Ok(());
        }
        let path = self.global_path();
        let text = env::lock::with_lock(&path, || {
            let disk = read_if_any(&path)?;
            let text = self.global.to_text_over(disk.as_deref().or(self.global.source.as_deref()))?;
            crate::cauldron::store::write_text_atomic(&path, &text)?;
            Ok(text)
        })?;
        self.global.take_written(&text);
        self.pending_global.set(false);
        // The global file now holds every adopted key, so this cauldron's
        // stale copies leave its file: from the file as it is on disk, only
        // the global keys dropped, everything else he wrote kept verbatim.
        if self.held_on_disk.get() {
            let own = self.path();
            env::lock::with_lock(&own, || match Store::load(&own)? {
                Some(mut disk) => {
                    disk.vals.retain(|k, _| !is_global(k));
                    crate::cauldron::store::write_text_atomic(&own, &disk.to_text()?)
                }
                None => Ok(()),
            })?;
            self.held_on_disk.set(false);
        }
        Ok(())
    }

    /// Add account `label` to `pool`: recorded with no settings of its own,
    /// so it inherits every value from the pool. Refused when its id is
    /// taken. Returns the new account's id.
    pub fn add_account(&mut self, pool: &str, label: &str, taken: impl Fn(&str) -> bool) -> Result<String> {
        let id = crate::cauldron::store::slug(label);
        if id.is_empty() {
            bail!("an account needs a name with a letter or a digit in it");
        }
        if !self.writable {
            bail!("nothing is created by --dump");
        }
        let acct = AddedAccount { pool: pool.into(), id: id.clone(), label: label.trim().into(), added_at: chrono::Utc::now().to_rfc3339() };
        let id2 = id.clone();
        self.update_accounts(move |file| {
            if taken(&id2) || file.accounts.iter().any(|a| a.pool == pool && a.id == id2) {
                bail!("`{id2}` is already in {}", crate::cauldron::menu::pool_label(pool));
            }
            // A seed account of that name removed earlier stays removed; this
            // one is new, with no settings of its own.
            file.accounts.push(acct);
            Ok(())
        })?;
        Ok(id)
    }

    // ── inference endpoints ────────────────────────────────────────────────

    /// The endpoints registered globally (`here` false) or in this cauldron:
    /// every id holding a template, in the order they were minted. The one
    /// being added is not among them.
    pub fn endpoint_ids(&self, here: bool) -> Vec<String> {
        let (store, seed) = if here { (&self.cauldron, &self.seed_cauldron) } else { (&self.global, &self.seed_global) };
        let p = format!("{}.", endpoint_prefix(here));
        let mut ids: Vec<String> = store
            .vals
            .keys()
            .chain(seed.vals.keys())
            .filter_map(|k| k.strip_prefix(&p)?.strip_suffix(".template").map(str::to_string))
            .filter(|id| is_endpoint_id(id) && id != DRAFT_EP)
            .filter(|id| self.val(&format!("{p}{id}.template")).is_some())
            .collect();
        ids.sort();
        ids.dedup();
        ids.sort_by_key(|id| id[3..].parse::<u64>().unwrap_or(0));
        ids
    }

    /// Begin adding an endpoint (`here`: to this cauldron alone) of
    /// `template`: [`DRAFT_EP`], held in this face only.
    pub fn draft_endpoint(&mut self, here: bool, template: &str) {
        self.discard_draft();
        let key = format!("{}.{DRAFT_EP}.template", endpoint_prefix(here));
        let store = if here { &mut self.cauldron } else { &mut self.global };
        store.vals.insert(key, Val::Str(template.to_string()));
    }

    /// Forget the endpoint being added, wherever it was.
    pub fn discard_draft(&mut self) {
        let mid = format!(".{DRAFT_EP}.");
        self.global.vals.retain(|k, _| !k.contains(&mid));
        self.cauldron.vals.retain(|k, _| !k.contains(&mid));
    }

    /// Add the endpoint being added: it takes the next id of the one series
    /// and is written. Returns its id.
    pub fn add_endpoint(&mut self, here: bool) -> Result<String> {
        if !self.writable {
            bail!("nothing is added by --dump");
        }
        let p = endpoint_prefix(here);
        let from = format!("{p}.{DRAFT_EP}.");
        // Past every id the seed's fake endpoints hold too: read beneath his
        // file, an id of theirs is taken.
        let seed_last = self.seed_global.vals.get("endpoints.last_id").and_then(Val::as_int).unwrap_or(0);
        let next = match self.global.vals.get("endpoints.last_id") {
            Some(Val::Int(n)) => n.max(&seed_last) + 1,
            _ => seed_last + 1,
        };
        // The id is taken under the global file's lock, against the file as
        // it is there: another face may have minted one since this one read.
        let path = self.global_path();
        let n = env::lock::with_lock(&path, || {
            let on_disk = Store::load(&path)?.and_then(|g| g.vals.get("endpoints.last_id").and_then(Val::as_int)).unwrap_or(0);
            let n = next.max(on_disk + 1);
            self.global.vals.insert("endpoints.last_id".into(), Val::Int(n));
            crate::cauldron::store::write_text_atomic(&path, &self.global.to_text()?)?;
            Ok(n)
        })?;
        let id = format!("ep-{n}");
        let store = if here { &mut self.cauldron } else { &mut self.global };
        let moved: Vec<(String, Val)> = store.vals.iter().filter(|(k, _)| k.starts_with(&from)).map(|(k, v)| (format!("{p}.{id}.{}", &k[from.len()..]), v.clone())).collect();
        store.vals.retain(|k, _| !k.starts_with(&from));
        store.vals.extend(moved);
        if here {
            self.save()?;
        } else {
            self.save_global()?;
        }
        Ok(id)
    }

    /// Remove endpoint `id`, registered `here` or globally: every value it
    /// has goes, and so does what this cauldron took of it and every role
    /// here bound to it.
    pub fn remove_endpoint(&mut self, here: bool, id: &str) -> Result<()> {
        let mine = format!("{}.{id}.", endpoint_prefix(here));
        let uses = format!("cauldron.use.{id}.");
        // One the seed holds is READ, never deleted: its removal is recorded.
        let seeded = if here { &self.seed_cauldron } else { &self.seed_global }.vals.contains_key(&format!("{mine}template"));
        let store = if here { &mut self.cauldron } else { &mut self.global };
        store.vals.retain(|k, _| !k.starts_with(&mine));
        if seeded {
            store.vals.insert(format!("{mine}removed"), Val::Bool(true));
        }
        if !here {
            self.save_global()?;
            self.cauldron.vals.retain(|k, _| !k.starts_with(&uses));
        }
        self.cauldron.vals.retain(|k, v| !(k.starts_with("roles.") && k.ends_with(".endpoint") && *v == Val::Str(id.to_string())));
        self.save()
    }
}

// ── what each setting means (the tooltip's text) ────────────────────────────

/// What a setting means, by the END of its key: one explanation serves a
/// threshold wherever it is set (the cauldron's default, a pool's, an
/// account's, a cluster's). Longest suffix wins. This is the ONLY place the
/// explanations live: the page shows none (msg 165), its ⓘ opens this text.
const HELP: &[(&str, &str)] = &[
    ("cauldron.enabled", "Whether this cauldron is enabled. Disabled, nothing new starts anywhere in it: no ripple, no task, no seeding. What is running finishes. A cluster cannot be disabled on its own. Changing it asks first."),
    ("cauldron.opportunistic", "Whether this cauldron takes advantage of the accounts flagged opportunistic: parked work here may spend them freely. Each account's own answer inherits this one."),
    (".allowed", "Whether this cauldron may place work on this account. The account itself is set up once, on the Providers tab, for every cauldron; this switch only says whether THIS cauldron uses it. Inherit takes the account's own switch there."),
    ("cauldron.accounts.{pool}.{account}.weekly_threshold_pct", "When this account's weekly window reaches this share, this cauldron stops placing new work on it. off: never stop for the week."),
    ("cauldron.accounts.{pool}.{account}.opportunistic", "Whether this cauldron takes advantage of this account while it is flagged opportunistic."),
    ("endpoints.defaults.budget_period", "The period a new endpoint's budget runs over until it is given its own: daily, weekly, monthly, or none (a total that never resets)."),
    ("endpoints.defaults.threshold_pct", "The share of its budget at which a new endpoint stops taking new work, before the wall itself, until it is given its own."),
    ("endpoints.defaults.threshold_action", "What a new endpoint does at its threshold until it is given its own: park new work only, or park everything that has not started."),
    ("endpoints.defaults.max_concurrent", "The most pieces of work a new endpoint runs at once until it is given its own. -1 means no limit."),
    ("{ep}.removed", "A seed endpoint removed here. The seed is only ever read, so its removal is the one thing written about it."),
    ("{ep}.inherit", "The fields of a seed endpoint set back to what they inherit. The seed is only ever read, so that is recorded here, never a value written over it."),
    ("endpoints.last_id", "The last endpoint id minted. Every endpoint, global or a cauldron's own, takes the next one, so an id names one endpoint wherever it is used."),
    ("endpoints.{ep}.name", "This endpoint's name: the label on its row everywhere. Renaming it re-keys nothing; its id stays."),
    ("endpoints.{ep}.template", "What this endpoint was made from. The template only filled in the rows not typed into: its url, how its key is sent, its protocol, its models. Chosen once, when it is added."),
    ("endpoints.{ep}.server", "Which local server this endpoint calls: its url and whether it needs a key come from it."),
    ("endpoints.{ep}.protocol", "The wire this endpoint speaks: the OpenAI chat-completions protocol every template shares, or Anthropic's own Messages, which keeps prompt caching."),
    ("endpoints.{ep}.url", "The base URL the client is pointed at: chat completions and the model list hang off it. The url, not a company, is what an endpoint is."),
    ("endpoints.{ep}.key", "The API key this endpoint spends. Never shown: only how it begins and its last four characters."),
    ("endpoints.{ep}.auth", "How the key is sent: as a bearer token, as an x-api-key header, or not at all."),
    ("endpoints.{ep}.headers", "Extra headers sent on every call, as Name: value."),
    ("endpoints.{ep}.models", "Which models this key may serve: any the endpoint lists for it, or only the ones named here."),
    ("endpoints.{ep}.model_list", "The models this key may serve, each by the exact name the endpoint takes (vendor/model on OpenRouter, a bare name elsewhere)."),
    ("endpoints.{ep}.enabled", "Whether any cauldron may place work on this endpoint. Off, nothing new is placed on it anywhere."),
    ("endpoints.{ep}.budget_usd", "This key's budget, the wallet: the most it may spend in its period, for every cauldron together. off: no budget."),
    ("endpoints.{ep}.budget_period", "The period this key's budget runs over: daily, weekly, monthly, or none (a total that never resets)."),
    ("endpoints.{ep}.threshold_pct", "The share of its budget at which this endpoint stops taking new work, before the wall itself."),
    ("endpoints.{ep}.threshold_action", "What reaching the threshold does: park new work only, or park everything that has not started."),
    ("endpoints.{ep}.management_key", "OpenRouter's management key, optional. With it the key's budget is also its limit at OpenRouter; without it Cauldron's own meter is the only wall. Never shown."),
    ("endpoints.{ep}.price_in", "What Cauldron's meter charges per million tokens sent to this endpoint's models. $0: free."),
    ("endpoints.{ep}.price_out", "What Cauldron's meter charges per million tokens this endpoint's models send back. $0: free."),
    ("cauldron.use.{ep}.enabled", "Whether this cauldron places work on this endpoint. Inherit takes the endpoint's own switch on the Providers tab; an endpoint switched off there cannot be switched on here."),
    ("cauldron.use.{ep}.url", "The url this cauldron calls this endpoint at. Inherit takes the endpoint's own."),
    ("cauldron.use.{ep}.narrow", "Which of the key's models this cauldron may use: all of them, or fewer — a narrowed list can only take models away, never add one the key does not serve."),
    ("cauldron.use.{ep}.model_list", "The models this cauldron may use of this key's: x takes one off, + add model puts one of the key's back."),
    ("cauldron.use.{ep}.metered_cap_usd", "The most this cauldron may draw from this key in the key's period, metered by Cauldron: its allowance from the key's wallet. Past it, this cauldron's work on the key parks. off: no cap."),
    ("cauldron.use.{ep}.threshold_pct", "The share of this cauldron's metered cap at which its new work on this endpoint stops."),
    ("roles.{role}.endpoint", "The inference endpoint this role's work goes to, metered. Set, its pool is not used. none: the role spends a subscription."),
    ("paths.root", "Where every cauldron keeps everything: each one's data, soup and settings. Every other path is under it unless you say otherwise."),
    ("secrets.env_file", "The env file a human action's value goes to when its field says `route: env`: an absolute path, written NAME=value, mode 0600, and only when git ignores it (or it is in no git repo). Unset, such a value is refused — there is no default."),
    ("paths.weights", "Where model weights are stored. Point it somewhere shared (~/.weights) and other services on the machine find the same weights."),
    (".selection", "How the next piece of work picks an account in this source: the one at least risk of a limit, round robin, or one pinned account."),
    (".failover", "When an account hits a limit mid-work, move the work to another account of the same source instead of parking it."),
    ("max_concurrent", "The most pieces of work running at once here. 0 means none at all; -1 means no limit."),
    ("cauldron.use.{ep}.max_concurrent", "The most pieces of this cauldron's work running at once on this endpoint. 0 means none at all; -1 means no limit."),
    (".accounts_enabled", "Whether this source's accounts are on: every account set to inherit takes this."),
    (".models", "The models this source offers. A role can only be given a model some source offers."),
    (".model.{model}", "Whether this detected model may be used. Off, no role is given it from this source."),
    (".accounts.{account}.enabled", "Whether this cauldron uses this account at all. Off, nothing new is placed on it; what runs on it finishes."),
    (".share", "This account's share of the work its source places, against the other accounts' shares."),
    (".live_login", "This account is the machine's own login: the one a plain claude on this machine uses. Exactly one account can be."),
    (".note", "Your note on this account, shown wherever the account is."),
    ("local.enabled", "Run models on this machine. Off, no local model is offered to any role."),
    ("local.engine", "What runs the local models."),
    ("local.ram_max_gb", "The most memory local models may take. Never more than the machine's own RAM cap."),
    ("local.gpu", "Let local models use the GPU."),
    ("local.disk_max_gb", "The most disk local model weights may take. Never more than the machine's disk cap."),
    (".session.threshold_pct", "When an account's session window reaches this share, new work stops being placed on it until the window resets. off: never stop for the session window."),
    (".weekly.threshold_pct", "When an account's weekly window reaches this share, new work stops being placed on it until the week resets. off: never stop for the week."),
    (".fable.threshold_pct", "When an account's Fable window reaches this share, no new Fable work is placed on it. off: never stop for Fable."),
    (".session.cap_pct", "The most of a session window this may spend. Past it, its work parks. off: no cap."),
    (".weekly.cap_pct", "The most of the weekly window this may spend. Past it, its work parks. off: no cap."),
    (".fable.cap_pct", "The most of the Fable window this may spend. Past it, its Fable work parks. off: no cap."),
    (".cash.threshold_usd", "When spending reaches this many dollars, new work stops being placed here. off: no threshold."),
    (".cash.cap_usd_month", "The most dollars this may spend in a month. off: no cap."),
    (".cash.cap_usd_week", "The most dollars this may spend in a week. off: no cap."),
    (".bar_pct", "The hard bar: past this share of any window nothing runs here at all, not even work already placed."),
    (".threshold_action", "What crossing a threshold does: park new work only, or park everything that has not started."),
    (".reserve_pct", "A share of every window kept back for you: the cauldron's own work never spends it."),
    (".opportunistic.enabled", "Whether this account may be flagged opportunistic. An account is flagged while it is not on pace to spend its week — its pressure, tokens left over time left, is above 1 — and then parked work may spend it freely."),
    ("opportunistic.enabled", "The default every source and account inherits: whether an account may be flagged opportunistic. An account is flagged while it is not on pace to spend its week — its pressure, tokens left over time left, is above 1 — and then parked work may spend it freely."),
    (".model", "The model this role runs on."),
    (".fallback", "The model this role falls back to when its model is not available. none: it waits."),
    ("roles.{role}.pool", "The token source this role's work is placed in. auto: the first source offering its model."),
    (".parked.max_age", "How long work may stay parked before it is dropped and said so."),
    ("machine.ram_max_gb", "The most memory the cauldron's work may take on this machine."),
    ("machine.ram_reserve_gb", "Memory always left free for the rest of the machine."),
    ("machine.cpu_max_pct", "The most CPU the cauldron's work may take."),
    ("machine.max_child_processes", "The most processes the cauldron may run at once."),
    ("machine.gpu.enabled", "Let the cauldron's work use the GPU."),
    ("machine.gpu.memory_max_gb", "The most GPU memory the cauldron's work may take."),
    ("machine.disk.cap_gb", "The most disk the cauldron may fill."),
    ("machine.disk.free_floor_gb", "Free disk always left on the machine. Below it, collection runs and new work waits."),
    ("machine.on_battery", "What the cauldron does on battery: carry on, stop opportunistic work, or pause."),
    (".retention_age", "How long it is kept before collection may remove it."),
    ("storage.threads.cap_gb", "The most disk agent threads may take."),
    ("storage.threads.eviction_order", "Which threads go first when the cap is reached."),
    ("storage.threads.protect_pinned", "Pinned threads are never collected."),
    ("storage.threads.protect_active_clusters", "Threads of clusters with open work are never collected."),
    ("storage.threads.archive_before_delete", "Archive a thread before it is deleted."),
    ("storage.logs.cap_mb", "The most disk logs may take."),
    ("storage.supervisions.cap_mb", "The most disk the supervisor's records may take."),
    ("storage.account_backups.keep", "How many backups of each account's login are kept."),
    ("storage.tts.cap_gb", "The most disk spoken audio may take."),
    ("storage.tui.dumps_cap_mb", "Past this, collection says the dumps are large. Dumps are what you sent and are never deleted."),
    ("storage.pins.good_history", "How many proven-good versions of each service are remembered for rollback."),
    ("storage.gc.interval", "How often collection looks for what could go. It deletes nothing by itself: you confirm with run now."),
    ("storage.gc.dry_run", "Collection only reports what it would remove."),
    ("storage.gc.on_disk_pressure", "Look for what could go as soon as free disk drops below the floor. It deletes nothing by itself."),
    ("storage.gc.run_now", "List what could go and ask before deleting it. Nothing you wrote is ever collected."),
    (".permissions.mode", "How much an agent may do without asking: everything, ask as usual, plan only, or accept edits."),
    ("permissions.mode", "How much an agent may do without asking: everything, ask as usual, plan only, or accept edits."),
    ("allowed_tools", "Tools an agent may always use."),
    ("disallowed_tools", "Tools an agent may never use."),
    ("add_dirs", "Directories an agent may read and write beyond its own."),
    ("permissions.plugin_roots", "Where plugins are looked for."),
    ("permissions.plugin_dirs", "Plugins loaded on top of those found."),
    ("permissions.repos", "Repositories agents may work in."),
    ("identity.socket_secret_required", "Faces and agents must present the socket secret to talk to the daemon."),
    ("identity.live_login", "The account the machine's own claude is logged in as."),
    ("identity.operator", "Who you are, as the cauldron records it."),
    (".autodecide", "What the cauldron decides without you: nothing, only with your confirmation, between three ways, or between three ways above a floor."),
    (".interrupt_floor", "What is important enough to interrupt you: nothing, violations, blocking questions, or everything."),
    ("notify.tts.enabled", "The cauldron speaks to you."),
    ("notify.tts.voice", "The voice it speaks in."),
    ("notify.tts.rate", "How fast it speaks, in words a minute."),
    ("notify.limits.threshold_crossed", "Say so when an account crosses a threshold."),
    ("notify.limits.login_expiry_lead", "How far ahead an expiring login is announced."),
    ("notify.gc.report", "Say when collection found something waiting for your confirm."),
    ("notify.quiet_hours", "A daily window in which nothing is spoken."),
    ("supervisor.auto_restart", "Restart a service that stops on its own."),
    ("supervisor.auto_rollback", "Go back to the last proven-good version of a service that keeps failing."),
    ("supervisor.max_start_failures", "How many failed starts before a service is left stopped."),
    ("supervisor.poll_secs", "How often each service is checked."),
    ("supervisor.down_after", "How many failed checks in a row mark a service down."),
    ("supervisor.throttle_secs", "The least time between two restarts of one service."),
    ("supervisor.backoff_base_secs", "The first wait before restarting a failing service; it doubles each time."),
    ("supervisor.backoff_max_secs", "The longest wait before restarting a failing service."),
    ("supervisor.stable_after_secs", "How long a service must run clean before its version counts as proven good."),
    (".version", "The version this service runs."),
    (".addr", "Where this service listens."),
    ("threads.probe_interval_secs", "How often the threads daemon reads each account's usage while idle."),
    ("threads.probe_interval_busy_secs", "How often it reads usage while work is running."),
    ("threads.calibrate", "Learn each account's real limits from what it spends."),
    ("threads.edge_poll", "Read usage again right at the edge of a window."),
    ("threads.settle_ladder_secs", "The waits before a finished turn's usage is read."),
    ("threads.chassis_port", "The port the threads daemon listens on."),
    ("threads.host", "The address the threads daemon listens on."),
    ("threads.claude_bin", "The claude binary the threads daemon runs. auto: the one on the path."),
    ("mesh.devices", "The machines in this cauldron's mesh."),
    ("mesh.dispatch_remote", "Send work to other devices of the mesh."),
    ("mesh.accept_remote", "Take work from other cauldrons."),
    ("repos.trace_roots", "Where the blueprint back-edge trace looks for code that names a blueprint entry."),
    ("ui.theme", "The face's colours: dark, or light for a light terminal. Switches at once."),
    ("ui.sidebar_ratio", "How wide the clusters sidebar is in THIS cauldron, as a share of the window. auto: a third. Dragging its grabber sets it."),
    ("ui.default_tab", "The cauldron tab this face opens on."),
    ("ui.aggregate_default", "Open with Aggregated clusters on: every cluster with everything below it included."),
    ("ui.preserve.config", "Keep which Config bands are open when you leave the tab. Off: you come back to it collapsed."),
    ("ui.preserve.status", "Keep which Status bands are open when you leave the tab. Off: you come back to it collapsed."),
];

/// The explanation of `def`, and where its value comes from when it has none
/// of its own.
pub fn help(def: &Def) -> String {
    let base = HELP
        .iter()
        .filter(|(k, _)| def.key == *k || def.key.ends_with(k))
        .max_by_key(|(k, _)| k.len())
        .map_or("", |(_, t)| *t);
    let from = match (def.level, def.inherits) {
        (_, Some(p)) if p.starts_with("cauldron.") => " Inherit takes this cauldron's.",
        (_, Some(p)) if p.starts_with("endpoints.{ep}.") => " Inherit takes the endpoint's own, on the Providers tab.",
        (_, Some(p)) if p.starts_with("endpoints.defaults.") => " Inherit takes the default under the endpoints table.",
        (_, Some(p)) if def.key.starts_with("cauldron.") && p.starts_with("pools.") => " Inherit takes the account's own, on the Providers tab.",
        (Level::Account, Some(_)) => " Inherit takes its token source's.",
        (Level::Pool, Some(_)) => " Inherit takes the global default.",
        _ => "",
    };
    format!("{base}{from}")
}

/// The endpoint a key of one belongs to, as (`endpoints` or
/// `cauldron.endpoints`, id); a use of a global one belongs to it.
/// The list a seed endpoint's field is set back to inheriting under, and the
/// field: `cauldron.use.ep-2.enabled` → (`cauldron.use.ep-2.inherit`,
/// `enabled`).
fn seed_mask(key: &str) -> Option<(String, &str)> {
    for p in ["cauldron.endpoints.", "cauldron.use.", "endpoints."] {
        if let Some(rest) = key.strip_prefix(p) {
            let (ep, field) = rest.split_once('.')?;
            return (is_endpoint_id(ep) && field != "inherit").then(|| (format!("{p}{ep}.inherit"), field));
        }
    }
    None
}

fn seed_endpoint_of(key: &str) -> Option<(&'static str, &str)> {
    let (prefix, rest) = if let Some(r) = key.strip_prefix("cauldron.endpoints.") {
        ("cauldron.endpoints", r)
    } else if let Some(r) = key.strip_prefix("cauldron.use.") {
        ("endpoints", r)
    } else {
        ("endpoints", key.strip_prefix("endpoints.")?)
    };
    let ep = rest.split('.').next()?;
    is_endpoint_id(ep).then_some((prefix, ep))
}

/// The repo's seed settings for environment `name`, if it has one.
pub fn seed_path(seeds: &Path, name: &str) -> Option<PathBuf> {
    let p = seeds.join("settings").join(format!("{name}.toml"));
    p.is_file().then_some(p)
}

/// The repo's seed of the GLOBAL settings (`<seeds>/settings/global.toml`), read
/// until the global file exists.
pub fn global_seed_path(seeds: &Path) -> Option<PathBuf> {
    seed_path(seeds, "global")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_resolves_to_its_own_def_and_every_inherits_names_a_def() {
        for d in DEFS {
            if let Some(p) = d.inherits {
                let parent = def(p);
                assert_eq!(parent.ty == Ty::List, d.ty == Ty::List, "{} inherits a different kind", d.key);
            }
            if let Some(c) = d.cap {
                def(c);
            }
            if !d.stored() {
                continue;
            }
            let ctx = Ctx { pool: "p".into(), account: "a".into(), role: "type_one.primary".into(), service: "threads".into(), model: "opus".into(), ep: "ep-7".into() };
            let key = subst(d.key, &ctx);
            let (found, _) = lookup(&key).unwrap_or_else(|| panic!("{key} does not resolve"));
            assert_eq!(found.key, d.key);
        }
    }

    #[test]
    fn a_file_round_trips_and_a_wrong_value_is_refused() {
        let text = "[pools.claude_code.budget.weekly]\nthreshold_pct = 80\n[roles.\"type_one\".primary]\nmodel = \"sonnet\"\n";
        let s = Store::parse(text, "t").unwrap();
        assert_eq!(s.vals.get("pools.claude_code.budget.weekly.threshold_pct"), Some(&Val::Int(80)));
        assert_eq!(s.vals.get("roles.type_one.primary.model"), Some(&Val::Str("sonnet".into())));
        let again = Store::parse(&s.to_text().unwrap(), "t").unwrap();
        assert_eq!(again.vals, s.vals);
        // Not a setting, or not its type: kept and named, never used, never
        // a refusal (0220 E3).
        for bad in ["[budgets.default.weekly]\nthreshold_pct = 140\n", "nonsense = 1\n", "[surface.attention]\nquiet = true\n", "[pools.claude_code.budget.weekly]\nthreshold_pct = 140\n"] {
            let s = Store::parse(bad, "t").unwrap();
            assert!(s.vals.is_empty(), "{bad}");
            assert_eq!(s.warnings.len(), 1, "{bad}");
            assert_eq!(s.to_text().unwrap(), bad, "written back as he wrote it");
        }
        assert!(Store::parse("not = toml = at all", "t").is_err(), "only text that is not TOML is refused");
    }

    #[test]
    fn e3_a_save_keeps_his_comments_his_order_and_keys_it_does_not_know() {
        let text = "# my settings, by hand\n\n# the weekly line\n[pools.claude_code.budget.weekly]\nthreshold_pct = 80 # was 95\n\n[pools.claude_code.accounts.acct2]\nshare = 30   # half of acct1\n\n[zzz_unknown]\nmine = \"kept\" # from a newer build\n";
        let mut s = Store::parse(text, "t").unwrap();
        assert_eq!(s.warnings.len(), 1);
        s.vals.insert("pools.claude_code.accounts.acct2.share".into(), Val::Int(40));
        s.vals.insert("ui.theme".into(), Val::Str("light".into()));
        let out = s.to_text().unwrap();
        for line in ["# my settings, by hand", "# the weekly line", "threshold_pct = 80 # was 95", "# half of acct1", "mine = \"kept\" # from a newer build"] {
            assert!(out.contains(line), "lost `{line}` from:\n{out}");
        }
        assert!(out.contains("share = 40"), "{out}");
        assert!(out.find("[pools.claude_code.budget.weekly]").unwrap() < out.find("[zzz_unknown]").unwrap(), "his order");
        let again = Store::parse(&out, "t").unwrap();
        assert_eq!(again.vals.get("ui.theme"), Some(&Val::Str("light".into())));
        // Nothing changed: the file comes back byte for byte.
        assert_eq!(Store::parse(text, "t").unwrap().to_text().unwrap(), text);
        // A value removed (↺) leaves its line and nothing else.
        let mut s = Store::parse(text, "t").unwrap();
        s.vals.remove("pools.claude_code.accounts.acct2.share");
        let out = s.to_text().unwrap();
        assert!(!out.contains("share") && out.contains("# was 95") && out.contains("# my settings, by hand"), "{out}");
    }

    #[test]
    fn a_retired_key_is_read_dropped_and_named_and_an_unknown_one_kept_and_named() {
        // His prod file still held the per-pool switch (msg 165 retired it).
        let s = Store::parse("[pools.claude_code]\nenabled = true\n[pools.claude_code.accounts.acct2]\nshare = 30\n", "t").unwrap();
        assert_eq!(s.retired, vec!["pools.claude_code.enabled".to_string()]);
        assert_eq!(s.vals.get("pools.claude_code.accounts.acct2.share"), Some(&Val::Int(30)), "every setting that still exists is kept");
        assert!(s.to_text().unwrap().contains("[pools.claude_code]\nenabled = true"), "never deleted: written back as he wrote it (task 0209 D1)");
        let s = Store::parse("[ripples]\nmax_steps = 50\n[ui]\ndefault_tab = \"contents\"\npaste_chip_over = 240\n", "t").unwrap();
        assert_eq!(s.retired, vec!["ripples.max_steps".to_string(), "ui.default_tab = contents".to_string(), "ui.paste_chip_over".to_string()], "no ripple is configured, Contents is gone, every paste is a chip (msg 235)");
        assert!(s.vals.is_empty());
        let typo = Store::parse("[pools.claude_code]\nenabld = true\n", "t").unwrap();
        assert!(typo.retired.is_empty() && typo.warnings[0].contains("enabld"), "a typo is not retired: named, kept, not used");
    }

    #[test]
    fn zero_is_zero_and_minus_one_is_unlimited() {
        // 8e4fca10 msg 165: "zero ... should actually mean zero. Negative one
        // means unlimited."
        let d = def("pools.{pool}.max_concurrent");
        assert_eq!(parse_input(d.ty, "0"), Ok(Val::Int(0)));
        assert_eq!(parse_input(d.ty, "-1"), Ok(Val::Int(-1)));
        assert_eq!(parse_input(d.ty, "unlimited"), Ok(Val::Int(-1)));
        assert!(parse_input(d.ty, "-2").is_err());
        assert_eq!(show(d, &Val::Int(0)), "0");
        assert_eq!(show(d, &Val::Int(-1)), "unlimited");
        assert_eq!(default_val(d.ty, d.default), Val::Int(-1), "the default is still unlimited");
        assert_eq!(nudge(d.ty, &Val::Int(0), false), Some(Val::Int(-1)));
        assert_eq!(nudge(d.ty, &Val::Int(-1), false), None);
        assert_eq!(nudge(d.ty, &Val::Int(-1), true), Some(Val::Int(0)));
        assert_eq!(nudge(d.ty, &Val::Int(0), true), Some(Val::Int(1)));
        // A count that has no unlimited stays at or above zero.
        assert!(parse_input(def("machine.max_child_processes").ty, "-1").is_err());
    }

    #[test]
    fn every_setting_explains_itself_and_a_secret_is_never_shown() {
        for d in DEFS {
            let h = help(d);
            assert!(h.len() > 20, "`{}` has no explanation: {h:?}", d.key);
        }
        let k = def("endpoints.{ep}.key");
        assert_eq!(show(k, &Val::Str("sk-or-v1-0123456789abcdefc81e".into())), "sk-or-v1-••••••••••••••••c81e", "its prefix, dots, its last four");
        assert_eq!(show(k, &Val::Str("AIzaSyD-0123456789abcdef1234".into())), "AIzaSyD-••••••••••••••••1234");
        assert_eq!(show(k, &Val::Str("AIza0123456789abcdef1234".into())), "AIza••••••••••••••••1234", "no dash: its first four");
        assert_eq!(show(k, &Val::Str("short-key".into())), "••••••••", "a short one: dots only");
        assert!(help(def("pools.{pool}.accounts.{account}.budget.weekly.threshold_pct")).contains("token source"), "an account says what it inherits");
    }

    #[test]
    fn off_is_a_value_and_absent_inherits() {
        assert_eq!(parse_input(PCT_OFF, "off"), Ok(Val::Off));
        assert!(parse_input(PCT, "off").is_err());
        assert_eq!(parse_input(PCT, "80%"), Ok(Val::Int(80)));
        assert_eq!(parse_input(GB, "12 GB"), Ok(Val::Int(12)));
        assert_eq!(parse_input(USD_OFF, "$40"), Ok(Val::Int(40)));
        assert!(parse_input(DUR, "15 minutes").is_err());
        assert_eq!(nudge(PCT_OFF, &Val::Int(80), true), Some(Val::Int(85)));
        assert_eq!(nudge(DUR, &Val::Str("15m".into()), false), Some(Val::Str("14m".into())));
    }
}
