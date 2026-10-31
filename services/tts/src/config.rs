//! Where this daemon's state lives, and what it reads to find out.
//!
//! # The data directory is cauldron's to hand out
//!
//! The substrate daemon resolved its own: `config.data_dir`, else `$TTS_HOME`,
//! else `~/.tts` — three places to be wrong, one of which was a git worktree
//! for six weeks. This one is handed its directory by the supervisor through
//! `ServiceEnv` (`$CAULDRON_SERVICE_DATA_DIR`, already guarded into the
//! environment's data root) and resolves nothing:
//!
//! ```text
//!   <service data dir>/store/    tts.db, audio/, tts.toml, config.toml
//!   <service data dir>/warnings/ warnings.jsonl — the warnings contract's record
//! ```
//!
//! `store/` is exactly where production's store already sits
//! (`data/prod/tts/store/`), so the rewritten daemon opens the live store in
//! place with no move. There is no `TTS_HOME` and no `TTS_CONFIG`.
//!
//! # The websocket address is per ENVIRONMENT
//!
//! The faces dial `127.0.0.1:3652` (their `AUI_TTS_PORT` overrides it); that
//! is the wire and it does not move. But a port is a per-environment fact: a
//! test environment that also bound 3652 would collide with the operator's
//! voice. So the address comes from `store/config.toml`, which is data and
//! therefore per environment, and when no port is stated the daemon binds an
//! EPHEMERAL one and publishes it on its cauldron seam (the `WS` verb). The
//! substrate daemon's default of 3652 is gone, and one environment is refused
//! the new default by name — see [`Config::ws_addr`].

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

/// The faces' port. Reserved in substrate's `config.rs` beside 3649 (mesh) and
/// 3651 (threads); stated here so the `prod` refusal can name it.
pub const FACES_PORT: u16 = 3652;

/// The one environment whose daemon a face dials without reading a seam.
const FACES_ENV: &str = "prod";

/// `store/` under the service data dir: the store, the audio, both TOML files.
pub fn store_dir(service_data_dir: &Path) -> PathBuf {
    service_data_dir.join("store")
}

/// `warnings/` under the service data dir, the kind the warnings contract uses.
pub fn warnings_dir(service_data_dir: &Path) -> PathBuf {
    service_data_dir.join("warnings")
}

/// `store/config.toml`. Every key is optional and a missing file is defaults.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub host: Option<String>,
    /// Absent means ephemeral — and in `prod`, refused. See [`Config::ws_addr`].
    #[serde(default)]
    pub port: Option<u16>,
    /// Shared secret for the websocket. `$TTS_SECRET` wins over it.
    #[serde(default)]
    pub secret: Option<String>,
    /// Read only to be REFUSED: a `config.toml` that still names a data dir is
    /// a file written for the substrate daemon, and silently ignoring the line
    /// would leave a reader believing the store is somewhere it is not.
    #[serde(default)]
    data_dir: Option<String>,
}

impl Config {
    pub fn from_toml(s: &str) -> Result<Self> {
        let c: Config =
            toml::from_str(s).map_err(|e| Error::Config(format!("parsing config.toml: {e}")))?;
        if let Some(d) = &c.data_dir {
            return Err(Error::Config(format!(
                "config.toml names `data_dir = {d:?}`. The data directory is handed to this \
                 daemon by cauldron (`$CAULDRON_SERVICE_DATA_DIR`) and is not configurable; \
                 delete the line. The store is `<service data dir>/store/`."
            )));
        }
        Ok(c)
    }

    /// Load `path`; a missing file is the default config.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::from_toml(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::Config(format!("reading {}: {e}", path.display()))),
        }
    }

    pub fn host(&self) -> &str {
        self.host.as_deref().filter(|h| !h.is_empty()).unwrap_or("127.0.0.1")
    }

    pub fn effective_secret(&self) -> Option<String> {
        std::env::var("TTS_SECRET")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| self.secret.clone().filter(|s| !s.is_empty()))
    }

    /// The websocket address for environment `env`.
    ///
    /// A stated port is used as stated. An unstated one is EPHEMERAL, except in
    /// `prod`, where it is refused: the faces dial a fixed port there and read
    /// no seam, so a `prod` daemon on a kernel-chosen port would be a healthy
    /// process the operator could not hear. That is the one place this crate
    /// treats an environment name as special, and it is special in the
    /// direction that fails loudly. `prod`'s `store/config.toml` needs
    /// `port = 3652` before the rewritten daemon is pinned there.
    pub fn ws_addr(&self, env: &str) -> Result<SocketAddr> {
        let port = match self.port {
            Some(p) => p,
            None if env == FACES_ENV => {
                return Err(Error::Config(format!(
                    "REFUSED: environment `{env}` states no websocket port. The faces dial \
                     127.0.0.1:{FACES_PORT} there and read no seam, so an ephemeral port would \
                     be a daemon nobody can hear. Add `port = {FACES_PORT}` to \
                     `store/config.toml` in this environment's tts data directory."
                )))
            }
            None => 0,
        };
        let s = format!("{}:{port}", self.host());
        s.parse().map_err(|e| Error::Config(format!("websocket address {s}: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unstated_port_is_ephemeral_outside_prod_and_refused_in_it() {
        let c = Config::default();
        assert_eq!(c.ws_addr("r3s3").unwrap().port(), 0);
        let e = c.ws_addr("prod").unwrap_err().to_string();
        assert!(e.contains("port = 3652"), "the refusal names the fix: {e}");
        let stated = Config::from_toml("port = 3652").unwrap();
        assert_eq!(stated.ws_addr("prod").unwrap().to_string(), "127.0.0.1:3652");
    }

    #[test]
    fn a_config_naming_a_data_dir_is_refused_not_ignored() {
        let e = Config::from_toml("data_dir = \"~/.tts\"\nport = 1").unwrap_err();
        assert!(e.to_string().contains("not configurable"), "{e}");
        assert!(Config::from_toml("port = 1\nsecret = \"s\"").is_ok());
    }

    #[test]
    fn the_store_and_the_warnings_record_are_kinds_under_the_service_dir() {
        let d = Path::new("/x/data/r3s3/tts");
        assert_eq!(store_dir(d), Path::new("/x/data/r3s3/tts/store"));
        assert_eq!(warnings_dir(d), Path::new("/x/data/r3s3/tts/warnings"));
    }
}
