//! **A THREAD-LOCAL WARN/ERROR CAPTURE** for tests that count what one call
//! site said (WARN-1). It is installed with `tracing::subscriber::set_default`
//! on the test's own thread, so another test in this binary — and the global
//! subscriber `child_finished.rs` installs — can neither add to nor take from
//! what it records.
//!
//! It records every `WARN` and `ERROR` event: target, line, level, rendered
//! message and string-rendered fields. It does NOT go through
//! `threads::warnings::COUNTER`, whose disk half is process-global.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

#[derive(Debug, Clone)]
pub struct Rec {
    pub target: String,
    pub line: u32,
    pub level: Level,
    pub message: String,
    pub fields: BTreeMap<String, String>,
}

#[derive(Default)]
struct V {
    message: String,
    fields: BTreeMap<String, String>,
}

impl Visit for V {
    fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
        let s = format!("{v:?}");
        if f.name() == "message" {
            self.message = s;
        } else {
            self.fields.insert(f.name().to_string(), s);
        }
    }
    fn record_str(&mut self, f: &Field, v: &str) {
        if f.name() == "message" {
            self.message = v.to_string();
        } else {
            self.fields.insert(f.name().to_string(), v.to_string());
        }
    }
}

struct Cap(Arc<Mutex<Vec<Rec>>>);

impl<S: Subscriber> Layer<S> for Cap {
    fn on_event(&self, e: &Event<'_>, _c: Context<'_, S>) {
        let md = e.metadata();
        if *md.level() > Level::WARN {
            return; // INFO and below: not a warning
        }
        let mut v = V::default();
        e.record(&mut v);
        self.0.lock().unwrap().push(Rec {
            target: md.target().to_string(),
            line: md.line().unwrap_or(0),
            level: *md.level(),
            message: v.message,
            fields: v.fields,
        });
    }
}

/// Install the capture on THIS thread until the guard drops.
pub fn install() -> (Arc<Mutex<Vec<Rec>>>, tracing::subscriber::DefaultGuard) {
    let recs = Arc::new(Mutex::new(Vec::new()));
    let sub = tracing_subscriber::registry().with(Cap(Arc::clone(&recs)));
    (recs, tracing::subscriber::set_default(sub))
}
