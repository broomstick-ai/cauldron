//! **THE FACE'S LOG FILTER** (V1-ACCT2, V1-ACCT-I N4).
//!
//! `TUI_LOG` is an [`EnvFilter`] spec, `info` when unset. On top of whatever
//! it says, the targets that see raw wire frames are CAPPED at `debug`:
//!
//! - `tungstenite` and `tokio_tungstenite` log every frame received, verbatim,
//!   at `trace` (`tungstenite::protocol: Received message …`) — shape rows,
//!   `members.keys` and refusal text, with whole account addresses in them;
//! - this face's seam and transport (`tui::seam`, `tui::transport`) are the
//!   same wire one layer up.
//!
//! So `TUI_LOG=trace` traces everything EXCEPT a frame's body: their
//! `debug` lines and above still pass, their `trace` lines never do. A body
//! is never needed to debug the face (the flight recorder, `TUI_DIAG`, says
//! what each frame did), and no spelling of `TUI_LOG` lifts the cap.

use tracing::{Level, Metadata};
use tracing_subscriber::filter::{EnvFilter, FilterExt, FilterFn};

/// The targets whose `trace` carries a raw frame.
pub const CAPPED: [&str; 4] = ["tungstenite", "tokio_tungstenite", "tui::seam", "tui::transport"];

/// Whether `meta` passes the cap: anything below `debug` from a [`CAPPED`]
/// target does not.
pub fn under_cap(meta: &Metadata<'_>) -> bool {
    if *meta.level() <= Level::DEBUG {
        return true;
    }
    let t = meta.target();
    !CAPPED.iter().any(|c| t == *c || t.strip_prefix(c).is_some_and(|rest| rest.starts_with("::")))
}

/// The face's filter: `spec` (the value of `TUI_LOG`, `info` when `None` or
/// unparseable), AND the cap.
pub fn face_filter<S>(spec: Option<&str>) -> impl tracing_subscriber::layer::Filter<S> + Send + Sync + 'static
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    let env = spec.and_then(|s| EnvFilter::try_new(s).ok()).unwrap_or_else(|| EnvFilter::new("info"));
    env.and(FilterFn::new(under_cap))
}
