//! # The subscriber ledger — moved to `shapewire`
//!
//! A CHASSIS concern, not a `threads` one: it knows nothing about any
//! particular shape and would behave identically for one not yet written. It
//! moved with the rest of the chassis (`docs/shapes.md` 6.2) and this module is
//! the re-export that keeps `crate::consumers::` paths valid.

pub use shapewire::ledger::*;

// ─────────────────────────────────────────────────────────────────────────────
//  WHERE THE PRODUCER LEDGER LIVES — moved here from substrate's shapewire
// ─────────────────────────────────────────────────────────────────────────────
//
// R3.S2.M (move-and-fit). Substrate's `shapewire::ledger` resolved the cauldron
// root and the environment itself (`cauldron_home`, `cauldron_env`,
// `producer_ledger_path`). Cauldron's `libs/shapewire` deliberately does not: its
// ledger takes a `service_data` directory the caller already resolved
// (`ledger_path`), because `libs/env` owns environment resolution there. threads
// does not link `libs/env` yet (`services/threads.toml` `registers = false`), so
// the three resolvers move HERE, verbatim in behaviour, and the path arithmetic
// goes through the chassis's own `ledger_path` so the two cannot drift on the
// `links/<chassis>.producer.jsonl` tail. When threads links `libs/env` these
// three become `Environment::service_data(chassis)`.

/// The `data/` segment of the cauldron root (mirrors `libs/env`).
pub const DATA_DIR: &str = "data";

/// The default environment when neither `$CAULDRON_ENV` nor `$ENV` says.
pub const DEFAULT_ENV: &str = "prod";

/// **THE CAULDRON HOME.** `$CAULDRON_HOME` if set, else `$HOME/.cauldron`;
/// `None` when neither is set.
pub fn cauldron_home() -> Option<std::path::PathBuf> {
    if let Some(h) = std::env::var_os("CAULDRON_HOME") {
        if !h.is_empty() {
            return Some(std::path::PathBuf::from(h));
        }
    }
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| std::path::PathBuf::from(h).join(".cauldron"))
}

/// **THE ENVIRONMENT NAME** — `$CAULDRON_ENV`, else `$ENV`, else `prod`; `None`
/// when the name is not a usable path component (1-32 of `[a-z0-9_-]`,
/// starting `[a-z0-9]`). Mirrors `libs/env/src/paths.rs`.
pub fn cauldron_env() -> Option<String> {
    let name = match std::env::var("CAULDRON_ENV") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => match std::env::var("ENV") {
            Ok(v) if !v.trim().is_empty() => v,
            _ => DEFAULT_ENV.to_string(),
        },
    };
    let ok = !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .next()
            .map(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            == Some(true)
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if ok {
        Some(name)
    } else {
        None
    }
}

/// **THE PRODUCER'S LEDGER PATH** —
/// `$CAULDRON_HOME/data/<env>/<chassis>/links/<chassis>.producer.jsonl`.
pub fn producer_ledger_path(chassis: &str) -> Option<std::path::PathBuf> {
    let service_data = cauldron_home()?
        .join(DATA_DIR)
        .join(cauldron_env()?)
        .join(chassis);
    Some(shapewire::ledger::ledger_path(&service_data, chassis))
}
