//! **THE DOOR'S REQUEST ROUND TRIP, TIMED** (RV1.S1e).
//!
//! RV1.S1b-I finding 3 reported that a `threads/{id}/send` round trip was
//! ~15x slower behind ripple/v2's shapewire (median ~105 ms) than behind
//! rel/v1's (~7 ms). This bench isolates the CHASSIS half of that round trip:
//! a producer whose handler does no work, served by the real door
//! (`Chassis::serve_on_with_shutdown`, the path `threads::chassis::serve`
//! takes), driven over a real websocket with the secret handshake, and N
//! request -> response round trips timed one by one.
//!
//! It is a module of the `auth_gate` test binary (the crate's one
//! `--features server` test binary), not a test binary of its own. It uses
//! ONLY the API rel/v1's shapewire already had (`Producer::handle`,
//! `Chassis::new`, `server::bind`, `serve_on_with_shutdown`) so the SAME source
//! measures rel/v1's door (A) and this branch's (B): see the RV1.S1e report
//! for the swap recipe.
//!
//! `#[ignore]`d — it is a measurement, not a gate. Run it with
//!
//! ```text
//! CAULDRON_ISOLATED=1 cargo test -p shapewire --features server --test auth_gate \
//!     -- --ignored --nocapture door_request_round_trip
//! ```
//!
//! Knobs (environment): `SHAPEWIRE_LAT_N` samples (default 300, floor 200);
//! `SHAPEWIRE_LAT_MAX_MEDIAN_MS` the median ceiling the run must stay under
//! (default 20 ms; rel/v1's door measured well under 1 ms on this machine).
//! It prints one `RV1S1E-LAT` line with n / min / median / p90 / p99 / max.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use shapewire::envelope::{Identity, Refusal};
use shapewire::hub::{Hub, Kind};
use shapewire::server::{Chassis, NoControl, NoStream, Producer};
use shapewire::ConnId;
use tokio_tungstenite::tungstenite::Message;

type BoxFut<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

const SECRET: &str = "s3cret-for-the-latency-bench";

#[derive(Clone)]
struct Ev;
impl Kind for Ev {
    fn kind(&self) -> &'static str {
        "ev"
    }
}

/// A producer that answers every request at once and implements no identity
/// hook — the population threads is in.
struct Echo {
    id: Identity,
    hub: Hub<Ev>,
}

impl Producer for Echo {
    type Input = Ev;
    type Stream = NoStream;
    type Control = NoControl;
    fn identity(&self) -> &Identity {
        &self.id
    }
    fn hub(&self) -> &Hub<Ev> {
        &self.hub
    }
    fn secret(&self) -> Option<String> {
        Some(SECRET.into())
    }
    fn names(&self) -> &'static [&'static str] {
        &["bench/send"]
    }
    fn handle<'a>(&'a self, _: &'a str, params: Value) -> BoxFut<'a, Result<Value, Refusal>> {
        Box::pin(async move { Ok(json!({ "accepted": true, "echo": params["text"] })) })
    }
    fn connect(&self, _: ConnId) -> (NoStream, NoControl) {
        (NoStream, NoControl)
    }
}

fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn pct(sorted: &[Duration], p: f64) -> Duration {
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

#[tokio::test]
#[ignore = "measurement (RV1.S1e): run with --ignored --nocapture"]
async fn door_request_round_trip_latency() {
    let n = env_num("SHAPEWIRE_LAT_N", 300).max(200) as usize;
    let ceiling_ms = env_num("SHAPEWIRE_LAT_MAX_MEDIAN_MS", 20) as f64;

    let chassis = Chassis::new(Arc::new(Echo {
        id: Identity {
            daemon: "bench".into(),
            version: "0.0.0-test".into(),
            proto: 1,
            boot_id: "boot-bench".into(),
        },
        hub: Hub::new(),
    }));
    let listener = shapewire::server::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let (addr, task) = chassis
        .serve_on_with_shutdown(listener, std::future::pending())
        .unwrap();

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();

    async fn read(ws: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin)) -> Value {
        loop {
            match tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
                Err(_) => panic!("no frame within 10s"),
                Ok(Some(Ok(Message::Text(t)))) => return serde_json::from_str(&t).unwrap(),
                Ok(Some(Ok(_))) => continue,
                other => panic!("socket ended: {other:?}"),
            }
        }
    }

    let welcome = read(&mut ws).await;
    assert_eq!(welcome["type"], "welcome", "{welcome}");
    ws.send(Message::Text(json!({"type":"auth","secret":SECRET}).to_string()))
        .await
        .unwrap();
    let ack = read(&mut ws).await;
    assert_eq!(ack["ok"], true, "{ack}");

    // Warm-up: the first few requests pay allocation and routing-table cost.
    let mut samples = Vec::with_capacity(n);
    for i in 0..(n + 20) {
        let id = format!("r{i}");
        let frame = json!({
            "type": "request",
            "id": id,
            "method": "bench/send",
            "params": {"text": format!("message {i}")},
        })
        .to_string();
        let t0 = Instant::now();
        ws.send(Message::Text(frame)).await.unwrap();
        let r = read(&mut ws).await;
        let dt = t0.elapsed();
        assert_eq!(r["correlate"], id, "{r}");
        assert_eq!(r["outcome"]["type"], "success", "{r}");
        if i >= 20 {
            samples.push(dt);
        }
    }
    task.abort();

    samples.sort();
    let median = pct(&samples, 0.5);
    println!(
        "RV1S1E-LAT n={} min={:.3}ms median={:.3}ms p90={:.3}ms p99={:.3}ms max={:.3}ms",
        samples.len(),
        ms(samples[0]),
        ms(median),
        ms(pct(&samples, 0.9)),
        ms(pct(&samples, 0.99)),
        ms(*samples.last().unwrap()),
    );
    assert!(
        ms(median) < ceiling_ms,
        "the door's request round trip median {:.3} ms is over the {ceiling_ms} ms ceiling",
        ms(median)
    );
}
