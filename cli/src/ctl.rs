//! **`cauldron ctl` — THE COORDINATOR'S FACE** (RV1.S12a; EPIC-2 M0, §2.1–2.2).
//!
//! A second face on the SAME calls the TUI makes, so the coordinator meets the
//! system exactly as the operator does. No verb here owns a write path:
//!
//! - `ctl dump` runs the face itself, headless — `tui --cauldron --dump
//!   --send` with the bucket flags — so the dump is built by the composer's
//!   own code and sent by the function Enter calls (`apps/tui/src/cauldron/
//!   cli.rs` `send`). One code path, so a `ctl` dump and a TUI dump are the
//!   same line of `dumps.jsonl` by construction, and `apps/tui/tests/it/
//!   ctl_send.rs` proves the flag and the key agree.
//! - `ctl frame` is `tui --cauldron --dump …`: the frame the operator would see.
//! - `ctl node` is ONE `cauldron/read` on the environment's soup door, at the
//!   address the environment's seam table declares for `soup` — never a
//!   hard-coded port — dialled through `env::egress::connect_tcp`, the one
//!   choke point, so an isolated process dials only what isolation admits.
//!
//! Output is JSON on stdout (a frame is its text), errors go to stderr, and
//! every refusal exits non-zero. There is no write verb but `dump`, and never
//! a direct soup write (EPIC-2 §2.2).

use std::io::{IsTerminal, Read};
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use env::Environment;
use serde_json::{json, Value};

const USAGE: &str = "\
cauldron ctl — the coordinator's face: the TUI's own calls, headless

    cauldron ctl dump --env <name> [--cluster <id>] [--quote <tag>]... [--note <text>]...
                      [--paste <text>]...
        Send ONE dump into <name>, its text on stdin, exactly as the TUI's send key does:
        the face builds the bucket (stdin as --draft, then the flags in order) and sends it
        through the key's own function. Prints the dump record appended to the
        environment's dumps.jsonl. Refused on cauldron-build (read-only) and on an empty
        bucket. It starts from an empty bucket, never the draft the face has open.
    cauldron ctl frame [--env <name>] [tui --dump flags...]
        One frame of the face, headless (`tui --cauldron --dump ...`), as text:
        e.g. `ctl frame --env dev --status-tab ledger`.
    cauldron ctl node <id> --env <name> --pool <pool> --type <type> [--field <f>]...
        One `cauldron/read` of node <id> on <name>'s soup door, at the address its seam
        table declares for `soup`: `MATCH (n: <pool>/<type> { id: $id }) RETURN n.id,
        REV(n)[, n.<f>]...`. Prints {id, pool, type, node}. The reader secret is read
        from $SOUP_SECRET and never printed. An id the soup does not hold exits 1 with
        code `not_found`; a refusal from the soup exits 1 with its code verbatim.
        --edges is not available yet: a soup edge read names its kind and far end's
        type, which needs the pool's grammar (owed once the soup is in this tree).
    cauldron ctl help

  Every verb prints JSON on stdout (a frame prints its text), errors on stderr, and exits
  non-zero on any refusal. `dump` is the only verb that writes, and it writes only the
  dump. `cauldron --env <name> ctl ...` names the environment too.
";

/// `cauldron ctl <verb> …`.
pub fn verb_ctl(args: &[String]) -> Result<i32> {
    match args.first().map(String::as_str) {
        Some("help") | Some("-h") | Some("--help") => {
            print!("{USAGE}");
            Ok(0)
        }
        Some("dump") => dump(&args[1..]),
        Some("frame") => frame(&args[1..]),
        Some("node") => node(&args[1..]),
        None => {
            eprint!("{USAGE}");
            Ok(2)
        }
        Some(other) => {
            eprintln!("cauldron ctl: no verb `{other}` (dump | frame | node | help)");
            Ok(2)
        }
    }
}

/// The environment a verb acts on: its own `--env`, else one the process
/// NAMED (`cauldron --env <name> ctl …`, `$CAULDRON_ENV`). Never the default:
/// a coordinator that forgot `--env` must not land in prod.
fn named_env(flag: Option<String>, verb: &str) -> Result<String> {
    if let Some(n) = flag {
        return Ok(n);
    }
    match Environment::resolve() {
        Ok(e) if e.name_source != env::paths::Source::Default => Ok(e.name),
        _ => bail!("`ctl {verb}` needs --env <name>: it never defaults to prod"),
    }
}

/// `--flag value` from `it`, or a refusal naming the flag.
fn value(it: &mut std::slice::Iter<'_, String>, flag: &str) -> Result<String> {
    it.next().cloned().with_context(|| format!("{flag} needs a value"))
}

/// **`ctl dump`** — the face's own send, headless.
fn dump(args: &[String]) -> Result<i32> {
    let mut env = None;
    let mut cluster = None;
    let mut ops: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--env" => env = Some(value(&mut it, "--env")?),
            "--cluster" => cluster = Some(value(&mut it, "--cluster")?),
            f @ ("--quote" | "--note" | "--paste") => {
                let v = value(&mut it, f)?;
                ops.push(f.to_string());
                ops.push(v);
            }
            other => bail!("ctl dump: unknown argument `{other}` (--env, --cluster, --quote, --note, --paste; the text is on stdin)"),
        }
    }
    let env = named_env(env, "dump")?;
    // The text is stdin. A terminal is not a pipe: nothing is waited for.
    let mut text = String::new();
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        stdin.lock().read_to_string(&mut text).context("ctl dump: reading the dump's text on stdin")?;
    }
    let text = text.trim_end_matches(['\n', '\r']);
    let mut argv: Vec<String> = ["--cauldron", "--env", &env, "--dump", "--send"].iter().map(|s| s.to_string()).collect();
    if !text.is_empty() {
        argv.push("--draft".into());
        argv.push(text.to_string());
    }
    if let Some(c) = cluster {
        argv.push("--cluster".into());
        argv.push(c);
    }
    argv.extend(ops);
    super::app("tui", &argv, Some(&env))
}

/// **`ctl frame`** — `tui --cauldron --dump …`, passed through.
fn frame(args: &[String]) -> Result<i32> {
    if let Some(w) = args.iter().find(|a| matches!(a.as_str(), "--send" | "--input" | "--keys")) {
        bail!("ctl frame only looks: `{w}` is not taken (a dump is `ctl dump`)");
    }
    let mut flag = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--env" {
            flag = it.next().cloned();
        }
    }
    let opens = flag.or_else(|| Environment::resolve().ok().filter(|e| e.name_source != env::paths::Source::Default).map(|e| e.name));
    let mut argv: Vec<String> = vec!["--cauldron".into(), "--dump".into()];
    argv.extend(args.iter().filter(|a| *a != "--dump").cloned());
    super::app("tui", &argv, opens.as_deref())
}

/// The soup door's seam name in an environment's seam table.
pub const SOUP_SEAM: &str = "soup";

/// The soup's reader secret, read from the environment (never printed).
pub const SECRET_ENV: &str = "SOUP_SECRET";

/// How long `ctl node` waits for the soup at each step.
const NODE_TIMEOUT: Duration = Duration::from_secs(10);

/// A pool name (`[a-z][a-z0-9-]*`) or a type/field name (`[a-z_][a-z0-9_]*`),
/// the soup grammar's charset rules — so nothing typed reaches the statement
/// as anything but a name.
fn name_ok(s: &str, pool: bool) -> bool {
    let mut cs = s.chars();
    let first = cs.next();
    match first {
        Some(c) if c.is_ascii_lowercase() || (!pool && c == '_') => {}
        _ => return false,
    }
    cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || if pool { c == '-' } else { c == '_' })
}

/// **The minimal read-by-id**: the node's id and revision, plus each field
/// named.
pub fn read_statement(pool: &str, ty: &str, fields: &[String]) -> String {
    let mut s = format!("MATCH (n: {pool}/{ty} {{ id: $id }}) RETURN n.id, REV(n)");
    for f in fields {
        s.push_str(&format!(", n.{f}"));
    }
    s
}

/// One refusal, as JSON on stderr: `{"code", "message"}`.
fn refuse(code: &str, message: &str) -> i32 {
    eprintln!("{}", json!({ "code": code, "message": message }));
    1
}

/// **`ctl node`** — one `cauldron/read` on the environment's soup door.
fn node(args: &[String]) -> Result<i32> {
    let (mut id, mut env, mut pool, mut ty, mut fields, mut edges) = (None, None, None, None, Vec::new(), false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--env" => env = Some(value(&mut it, "--env")?),
            "--pool" => pool = Some(value(&mut it, "--pool")?),
            "--type" => ty = Some(value(&mut it, "--type")?),
            "--field" => fields.push(value(&mut it, "--field")?),
            "--edges" => edges = true,
            s if !s.starts_with('-') && id.is_none() => id = Some(s.to_string()),
            other => bail!("ctl node: unknown argument `{other}`"),
        }
    }
    let id = id.context("ctl node <id>: which node")?;
    if edges {
        // STUB (RV1.S12a, floats on RV1.S1): an edge read names its kind and its
        // far end's pool/type, which only the pool's grammar can supply.
        eprintln!("cauldron ctl: node --edges is not available yet: an edge read needs the pool's grammar (owed after the soup lands in this tree)");
        return Ok(2);
    }
    let pool = pool.context("ctl node: --pool <pool> (the soup's read is pool-qualified)")?;
    let ty = ty.context("ctl node: --type <type> (the soup's read names the node's type)")?;
    if !name_ok(&pool, true) {
        bail!("ctl node: `{pool}` is not a pool name");
    }
    for n in std::iter::once(&ty).chain(&fields) {
        if !name_ok(n, false) {
            bail!("ctl node: `{n}` is not a type or field name");
        }
    }
    let base = Environment::resolve()?;
    let env = base.for_name(&named_env(env, "node")?)?;
    // THE ADDRESS IS THE SEAM TABLE'S: the row the soup registered there.
    let table = env::seams::Table::load(&env.name, &env.data_root, &env.seams())?;
    let row = table.seams.get(SOUP_SEAM).with_context(|| {
        format!("environment `{}` has no `{SOUP_SEAM}` seam in {}: the soup is not running there", env.name, env.seams().display())
    })?;
    let addr: SocketAddr = row.addr.parse().with_context(|| format!("the `{SOUP_SEAM}` seam's address `{}` is not host:port", row.addr))?;
    let secret = std::env::var(SECRET_ENV).ok().filter(|s| !s.trim().is_empty()).with_context(|| format!("ctl node reads as the soup's reader: set {SECRET_ENV}"))?;

    // THE ONE DIAL, through the choke point: isolated, it is admitted only to
    // a port this process was given or its scratch root records.
    let stream = env::egress::connect_tcp("soup door (ctl node)", &addr, NODE_TIMEOUT).with_context(|| format!("ctl node: dialling the soup at {addr}"))?;
    stream.set_read_timeout(Some(NODE_TIMEOUT))?;
    stream.set_write_timeout(Some(NODE_TIMEOUT))?;
    let (mut ws, _) = tungstenite::client(format!("ws://{addr}/ws"), stream).map_err(|e| anyhow::anyhow!("ctl node: the websocket handshake with {addr}: {e}"))?;

    let next = |ws: &mut tungstenite::WebSocket<std::net::TcpStream>| -> Result<Value> {
        loop {
            match ws.read().context("ctl node: reading from the soup")? {
                tungstenite::Message::Text(t) => return serde_json::from_str(&t).with_context(|| format!("ctl node: a frame that is not JSON: {t}")),
                tungstenite::Message::Close(_) => bail!("ctl node: the soup closed the connection"),
                _ => continue,
            }
        }
    };
    let welcome = next(&mut ws)?;
    if welcome["type"] != "welcome" {
        bail!("ctl node: the soup did not say welcome: {welcome}");
    }
    let consumer = json!({ "name": "cauldron-ctl", "shape_version": "v6", "build": super::SELF_COMMIT });
    let send = |ws: &mut tungstenite::WebSocket<std::net::TcpStream>, v: Value| -> Result<()> {
        ws.send(tungstenite::Message::Text(v.to_string())).context("ctl node: writing to the soup")
    };
    send(&mut ws, json!({ "type": "auth", "secret": secret, "consumer": consumer }))?;
    let ack = next(&mut ws)?;
    if ack["type"] != "auth_ack" || ack["ok"] != true {
        let e = &ack["error"];
        return Ok(refuse(e["code"].as_str().unwrap_or("unauthorized"), e["message"].as_str().unwrap_or("the soup refused the reader credential")));
    }
    const ID: &str = "ctl-node-1";
    let statement = read_statement(&pool, &ty, &fields);
    send(&mut ws, json!({
        "type": "request", "id": ID, "method": "cauldron/read",
        "params": { "statement": statement, "bindings": { "id": id }, "format_version": 1 },
    }))?;
    let outcome = loop {
        let v = next(&mut ws)?;
        if v["type"] == "response" && v["correlate"] == ID {
            break v["outcome"].clone();
        }
    };
    let _ = ws.close(None);
    let outcome: shapewire::envelope::Outcome = serde_json::from_value(outcome.clone()).with_context(|| format!("ctl node: an outcome this build cannot read: {outcome}"))?;
    match outcome {
        shapewire::envelope::Outcome::Success { payload } => {
            let rows = payload["rows"].as_array().cloned().unwrap_or_default();
            match rows.as_slice() {
                [] => Ok(refuse(shapewire::envelope::codes::NOT_FOUND, &format!("no node `{id}` in {pool}/{ty} in environment `{}`", env.name))),
                [one] => {
                    println!("{}", json!({ "id": id, "pool": pool, "type": ty, "node": one }));
                    Ok(0)
                }
                many => Ok(refuse("ambiguous", &format!("{} rows for `{id}` in {pool}/{ty}", many.len()))),
            }
        }
        // RV1.M5: `detail` (shapewire, ripple/v2) is additive; the refusal line
        // stays {code, message} as C3 pins it.
        shapewire::envelope::Outcome::Error { error, code, .. } => Ok(refuse(&code, &error)),
        shapewire::envelope::Outcome::Promise { promise } => Ok(refuse("unexpected", &format!("a read answered with a promise ({promise})"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_soup_charset_and_the_statement_is_the_minimal_read() {
        assert!(name_ok("ws", true) && name_ok("my-pool2", true) && !name_ok("My", true) && !name_ok("a b", true) && !name_ok("_x", true));
        assert!(name_ok("task", false) && name_ok("_meta", false) && !name_ok("a-b", false) && !name_ok("x})", false) && !name_ok("", false));
        assert_eq!(read_statement("ws", "task", &[]), "MATCH (n: ws/task { id: $id }) RETURN n.id, REV(n)");
        assert_eq!(read_statement("ws", "task", &["title".into()]), "MATCH (n: ws/task { id: $id }) RETURN n.id, REV(n), n.title");
    }
}
