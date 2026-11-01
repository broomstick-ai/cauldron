//! **TCP_NODELAY AND A BURST OF PUSHES — MEASURED, NOT CHANGED** (RV1.S1g).
//!
//! RV1.S1e-I asked whether Nagle could bite where request/response cannot: a
//! BURST of small server -> client frames written back to back on one socket
//! (event fan-out, field deltas). shapewire's `axum::serve` leaves
//! `tcp_nodelay` unset, and shapewire's serve is not changed for a
//! measurement, so this measures it WITHOUT touching shapewire:
//! the option is set on the LISTENER, which every socket the kernel accepts
//! from it inherits (asserted below, both ways, before anything is timed),
//! and the listener is handed to the real `Chassis::serve_on_with_shutdown`.
//!
//! One producer, one connection: a `bench/burst` request makes the producer's
//! own per-connection stream (`ProducerStream`, the chassis's extension arm)
//! yield 50 frames, each forwarded by the chassis as its own websocket write.
//! Timed: request sent -> the 50th push received. Arm A = listener without
//! `TCP_NODELAY` (shapewire today), arm B = with it; rounds A B B A A B.
//! Informational: nothing is asserted about the numbers.
//!
//! ```text
//! CAULDRON_ISOLATED=1 cargo test -p threads --test it -- --ignored --nocapture \
//!     push_burst_of_50_with_and_without_tcp_nodelay
//! ```

use std::os::fd::AsRawFd;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use shapewire::envelope::{Identity, Refusal};
use shapewire::hub::{Hub, Kind};
use shapewire::server::{Chassis, ExtraFrame, NoControl, Producer, ProducerStream};
use shapewire::ConnId;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

type BoxFut<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

const BURST: usize = 50;

#[derive(Clone)]
struct Ev;
impl Kind for Ev {
    fn kind(&self) -> &'static str {
        "ev"
    }
}

struct Burster {
    id: Identity,
    hub: Hub<Ev>,
    tx: mpsc::UnboundedSender<String>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<String>>>,
}

struct BurstStream(mpsc::UnboundedReceiver<String>);
impl ProducerStream for BurstStream {
    fn next(&mut self, _deliverable: bool) -> BoxFut<'_, ExtraFrame> {
        Box::pin(async move {
            match self.0.recv().await {
                Some(s) => ExtraFrame::Send(s),
                None => ExtraFrame::Closed,
            }
        })
    }
}

impl Producer for Burster {
    type Input = Ev;
    type Stream = BurstStream;
    type Control = NoControl;
    fn identity(&self) -> &Identity {
        &self.id
    }
    fn hub(&self) -> &Hub<Ev> {
        &self.hub
    }
    fn names(&self) -> &'static [&'static str] {
        &["bench/burst"]
    }
    fn handle<'a>(&'a self, _: &'a str, params: Value) -> BoxFut<'a, Result<Value, Refusal>> {
        Box::pin(async move {
            let b = params["b"].as_u64().unwrap_or(0);
            for i in 0..BURST {
                // ~120 bytes, the size of a small event or field delta.
                let f = json!({"type": "bench_push", "b": b, "i": i, "pad": "x".repeat(64)});
                self.tx.send(f.to_string()).unwrap();
            }
            Ok(json!({"queued": BURST}))
        })
    }
    fn connect(&self, _: ConnId) -> (BurstStream, NoControl) {
        let rx = self.rx.lock().unwrap().take().expect("one connection per bench server");
        (BurstStream(rx), NoControl)
    }
}

fn set_nodelay(fd: i32, on: bool) {
    let v: libc::c_int = on as libc::c_int;
    // SAFETY: a valid fd and a c_int option value of the right size.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_NODELAY,
            &v as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    assert_eq!(rc, 0, "setsockopt(TCP_NODELAY) failed: {}", std::io::Error::last_os_error());
}

async fn listener(nodelay: bool) -> tokio::net::TcpListener {
    let l = shapewire::server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    set_nodelay(l.as_raw_fd(), nodelay);
    l
}

/// Does a socket accepted from this listener carry the option? The same
/// kernel `accept` axum's serve loop performs.
async fn accepted_nodelay(nodelay: bool) -> bool {
    let l = listener(nodelay).await;
    let addr = l.local_addr().unwrap();
    let (_client, (server, _)) =
        tokio::join!(async { tokio::net::TcpStream::connect(addr).await.unwrap() }, async {
            l.accept().await.unwrap()
        });
    server.nodelay().unwrap()
}

async fn read(
    ws: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
) -> Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
            Err(_) => panic!("no frame within 10s"),
            Ok(Some(Ok(Message::Text(t)))) => return serde_json::from_str(&t).unwrap(),
            Ok(Some(Ok(_))) => continue,
            other => panic!("socket ended: {other:?}"),
        }
    }
}

/// One arm: a fresh server, one connection, `warm + n` bursts; returns the
/// sorted burst times of the last `n`.
async fn arm(nodelay: bool, warm: usize, n: usize) -> Vec<Duration> {
    let (tx, rx) = mpsc::unbounded_channel();
    let chassis = Chassis::new(Arc::new(Burster {
        id: Identity {
            daemon: "burst-bench".into(),
            version: "0.0.0-test".into(),
            proto: 1,
            boot_id: "boot-burst".into(),
        },
        hub: Hub::new(),
        tx,
        rx: Mutex::new(Some(rx)),
    }));
    let (addr, task) = chassis
        .serve_on_with_shutdown(listener(nodelay).await, std::future::pending())
        .unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws")).await.unwrap();
    let welcome = read(&mut ws).await;
    assert_eq!(welcome["type"], "welcome", "{welcome}");

    let mut out = Vec::with_capacity(n);
    for b in 0..(warm + n) {
        let id = format!("r{b}");
        let req = json!({"type": "request", "id": id, "method": "bench/burst", "params": {"b": b}});
        let t0 = Instant::now();
        ws.send(Message::Text(req.to_string())).await.unwrap();
        let (mut pushes, mut answered) = (0usize, false);
        let mut done_at = None;
        while pushes < BURST || !answered {
            let f = read(&mut ws).await;
            if f["type"] == "response" {
                assert_eq!(f["correlate"], id.as_str(), "{f}");
                assert_eq!(f["outcome"]["type"], "success", "{f}");
                answered = true;
            } else {
                assert_eq!(f["type"], "bench_push", "{f}");
                assert_eq!(f["b"], b as u64, "a push from another burst: {f}");
                assert_eq!(f["i"], pushes as u64, "pushes out of order: {f}");
                pushes += 1;
                if pushes == BURST {
                    done_at = Some(t0.elapsed());
                }
            }
        }
        if b >= warm {
            out.push(done_at.unwrap());
        }
    }
    task.abort();
    out.sort();
    out
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

#[tokio::test]
#[ignore = "measurement (RV1.S1g): run with --ignored --nocapture"]
async fn push_burst_of_50_with_and_without_tcp_nodelay() {
    // Sentinel first: the two arms must really differ on the server socket.
    assert!(!accepted_nodelay(false).await, "sentinel: an accepted socket is NODELAY with the listener unset");
    assert!(accepted_nodelay(true).await, "sentinel: an accepted socket does not inherit the listener's NODELAY");

    let n = std::env::var("THREADS_BURST_N").ok().and_then(|v| v.parse().ok()).unwrap_or(200usize);
    for (round, nodelay) in [false, true, true, false, false, true].into_iter().enumerate() {
        let mut l = [0f64; 3];
        // SAFETY: room for 3.
        unsafe { libc::getloadavg(l.as_mut_ptr(), 3) };
        let s = arm(nodelay, 20, n).await;
        let p = |q: f64| ms(s[((s.len() as f64 - 1.0) * q).round() as usize]);
        println!(
            "RV1S1G-BURST round={} arm={} nodelay={nodelay} load={:.2} bursts={} burst50 min={:.3}ms median={:.3}ms p90={:.3}ms p99={:.3}ms max={:.3}ms",
            round + 1,
            if nodelay { "B" } else { "A" },
            l[0],
            s.len(),
            ms(s[0]),
            p(0.5),
            p(0.9),
            p(0.99),
            ms(*s.last().unwrap()),
        );
    }
}
