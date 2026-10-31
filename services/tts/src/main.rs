//! The `tts` binary: a cauldron service. Started by `cauldron start tts`
//! (or the daemon), which hands it every `CAULDRON_*` variable; run by hand
//! with none of them, it refuses and exits 2 — there is no default data root
//! for it to run against by mistake.
//!
//! `tts --build-stamp` is the one thing it answers with no environment: which
//! source produced these bytes (`build.rs`). It reads and writes nothing.
//!
//! What the substrate binary also had and this one does not: `--self-check`
//! (the self-update preflight; cauldron never runs it), and the `import` and
//! `status` subcommands (both are websocket methods, `tts.import` and
//! `tts.status`, and a second door onto the live store is a second thing to
//! keep safe).

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// `commit=<sha> provenance=<pin|git|unknown> dirty=<1|0|unknown>
/// dirt_scope=<a,b,...>` — the line cauldron's `Stamp` parser reads, in the
/// same order `cauldron --build-stamp` prints its own. `dirty` is passed
/// through only as `0` or `1`; anything else is `unknown`, never clean.
fn build_stamp() -> String {
    let dirty = match env!("TTS_BUILD_DIRTY") {
        d @ ("0" | "1") => d,
        _ => "unknown",
    };
    format!(
        "commit={} provenance={} dirty={dirty} dirt_scope={}",
        env!("TTS_BUILD_COMMIT"),
        env!("TTS_BUILD_PROVENANCE"),
        env!("TTS_BUILD_DIRT_SCOPE")
    )
}

fn main() {
    // Before the environment is read: a freshly built image has none.
    if std::env::args().nth(1).as_deref() == Some("--build-stamp") {
        println!("{}", build_stamp());
        return;
    }
    let se = match env::seams::ServiceEnv::from_env() {
        Ok(se) => se,
        Err(e) => {
            eprintln!("tts: {e:#}");
            std::process::exit(2);
        }
    };
    tracing_subscriber::registry()
        .with(
            // No colour codes: cauldron writes this to a log file.
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))),
        )
        .with(tts::seam::WarningsLayer.with_filter(tracing_subscriber::filter::LevelFilter::WARN))
        .init();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    if let Err(e) = rt.block_on(tts::daemon::run(se)) {
        tracing::error!("tts daemon: {e:#}");
        eprintln!("tts daemon: {e:#}");
        std::process::exit(1);
    }
}
