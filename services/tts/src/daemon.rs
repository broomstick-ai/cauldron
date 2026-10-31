//! The daemon's life: resolve, bind, open, sweep, serve, register, and stop.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use env::seams::ServiceEnv;

use crate::config::{self, Config};
use crate::manager::TtsManager;
use crate::{seam, server};

/// How long a stop waits for in-flight syntheses. Inside cauldron's 15 s
/// grace before it reports a service that ignored its stop signal.
pub const DRAIN_LIMIT: Duration = Duration::from_secs(10);

/// Open the store under `data_dir` (created if absent), load or seed
/// `tts.toml`, and build the manager.
pub fn build_manager(data_dir: PathBuf, config: Config) -> crate::Result<TtsManager> {
    std::fs::create_dir_all(data_dir.join("audio"))
        .map_err(|e| crate::Error::Config(format!("creating {}: {e}", data_dir.join("audio").display())))?;
    let store = crate::Store::open(data_dir.join("tts.db"))?;
    let file = crate::KeysFile::load_or_seed(&data_dir.join("tts.toml"))?;
    TtsManager::new(store, config, data_dir, file)
}

/// Run until SIGINT or SIGTERM.
///
/// THE ORDER IS THE SAFETY. The websocket port is bound BEFORE the store is
/// opened, so a second daemon pointed at a held port fails before its boot
/// sweep can touch anything — the substrate daemon built its manager first and
/// could not defend itself. The seam is registered AFTER the websocket is
/// listening, so the row never names a daemon nobody can reach.
pub async fn run(se: ServiceEnv) -> Result<()> {
    let started_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs().to_string();
    let store = config::store_dir(&se.data_dir);
    std::fs::create_dir_all(&store).with_context(|| format!("creating {}", store.display()))?;
    seam::attach_sink(&config::warnings_dir(&se.data_dir));
    let cfg = Config::load(&store.join("config.toml"))?;
    let listener = server::bind(cfg.ws_addr(&se.env)?).await?;
    let manager = build_manager(store.clone(), cfg)?;
    match manager.boot_sweep() {
        Ok(report) => tracing::info!(%report, "boot sweep"),
        Err(e) => tracing::error!(error = %e, "boot sweep failed"),
    }
    let reaped = manager.reap_sweep().await;
    if reaped > 0 {
        tracing::warn!(reaped, "reaped orphaned synthesis processes at boot");
    }
    let (ws, server_task) = server::serve(manager.clone(), listener)?;
    let env_name = se.env.clone();
    let seam_addr = seam::start(se, ws, started_at)?;
    tracing::info!(%ws, %seam_addr, env = %env_name, store = %store.display(), "tts listening on ws://{ws}/ws");

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let signal = tokio::select! {
        _ = tokio::signal::ctrl_c() => "SIGINT",
        _ = term.recv() => "SIGTERM",
    };
    let left = manager.drain(DRAIN_LIMIT).await;
    tracing::info!(signal, still_synthesizing = left, "tts stopping");
    server_task.abort();
    Ok(())
}
