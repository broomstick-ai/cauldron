//! **`cauldron soup grammar check | add | list | show`**: a cauldron's
//! grammars, and registering a user-defined one (an ontology) as a new pool
//! (`soup_door::grammars`). Registering is an owner act (`grammar`, under
//! the owner_acts setting, as `cauldron mcp grant` and `cauldron soup
//! approve` are); the others only read.

use anyhow::{bail, Context, Result};
use serde_json::Value;

pub(crate) const USAGE: &str = "\
cauldron soup grammar — a cauldron's grammars (ontologies), and adding your own

    cauldron soup grammar check <file> [--pool <p>] [--env <e> --cauldron <c>]
        Judge a grammar file, writing nothing: its size, depth and shape, and every rule a
        user grammar meets. With a cauldron named, also against the pools it holds (names,
        id prefixes and kinds already taken; reach targets). JSON out: `ok`, `problems`
        (each with its `rule`, where in the file `at`, and `detail`), and the grammar's
        `schema` and `digest`. Exit 0 when ok, 1 when not.
    cauldron soup grammar add <file> --env <e> --cauldron <c> [--pool <p>]
        Register the grammar as a new pool of the cauldron's soup (the pool is named by
        --pool, else by the file's `pool`, else by its kind). An owner act: it runs with an
        interactive terminal, or where the owner opened `grammar` (`cauldron owner-acts
        open --only grammar`). When the `mcp` service holds the soup, the add is sent to
        it and made through its own handle (`forwarded_to`); no grant is touched. Adding
        the same grammar again changes nothing (`already-added`).
    cauldron soup grammar list [--env <e>] [--cauldron <c>]
        Every pool of the soup: pool, kind, version, `origin` (builtin or user), types, id
        prefixes, node count, and a user grammar's digest.
    cauldron soup grammar show <pool> [--env <e>] [--cauldron <c>]
        One pool's pinned grammar, as a grammar file.
    cauldron soup grammar rebind <pool> --env <e> --cauldron <c> [--yes]
        Take the pool file now in the soup as the pool's own: a copy restored from a backup
        taken before the pool's last write, or a pool an earlier build added. Refused unless
        its pin is the registered grammar, it is sound, nothing in it is another cauldron's,
        and nothing is in flight. Names the writes the copy lacks, the ids they made and the
        edges into them; with --yes records them as lost (those ids are never minted or
        written again). An owner act; the `mcp` service must not hold the soup (stop it,
        rebind, start it: no grant changes).
    cauldron soup grammar remove <pool> --env <e> --cauldron <c> [--yes]
        Remove a user pool that holds no record and that no other pool reaches: its file
        is moved to <soup>/.removed/ (never deleted). Refused for a pool with records (a
        record is never deleted). An owner act; while the `mcp` service holds the soup it
        is sent to it. Without --yes, judges only.

  The file format is `cauldron-grammar/1`: the published grammar.json shape
  (schema/soup/*/grammar.json) plus `\"format\": \"cauldron-grammar/1\"`, with `pool`
  optional. Its JSON Schema is schema/soup/grammar.schema.json; a worked example is
  schema/soup/examples/reading-list.grammar.json. The rules: the kind has a hyphen
  (`reading-list`), so it can never be a built-in kind; every type has an `id_prefix`
  and a `handle` declared {\"required\": true, \"kind\": \"text\", \"min_chars\": 1,
  \"max_chars\": <at most 235>}; type names and prefixes are used once per cauldron;
  an edge may reach the grammar's own types, the workspace's region and content
  types, or another user kind the cauldron already holds. Records are never written
  over: declare a local `supersedes` edge for a type whose records will change. Once
  added, records of its types go to its pool by type through `cauldron soup import`
  and the MCP tool soup_put; soup get | nodes | search (and soup_get, soup_nodes,
  soup_search) find them by id and by type; soup link | unlink | cite change their
  edges and citations. The README's \"Grammars and pools\" section has every key.
";

#[derive(Default)]
struct Args {
    env: Option<String>,
    cauldron: Option<String>,
    pool: Option<String>,
    yes: bool,
    pos: Vec<String>,
}

fn parse(verb: &str, args: &[String]) -> Result<Args> {
    let mut a = Args::default();
    let mut it = args.iter();
    while let Some(s) = it.next() {
        let (flag, inline) = match s.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (s.as_str(), None),
        };
        if !flag.starts_with('-') || flag == "-" {
            a.pos.push(s.clone());
            continue;
        }
        if flag == "--yes" {
            a.yes = true;
            continue;
        }
        if !["--env", "--cauldron", "-c", "--pool"].contains(&flag) {
            bail!("unknown option `{s}` for `cauldron soup grammar {verb}` (`cauldron soup grammar help`)");
        }
        let v = match inline {
            Some(v) => v,
            None => it.next().cloned().with_context(|| format!("`cauldron soup grammar {verb} {flag}` needs a value"))?,
        };
        match flag {
            "--env" => a.env = Some(v),
            "--pool" => a.pool = Some(v),
            _ => a.cauldron = Some(v),
        }
    }
    Ok(a)
}

fn target(verb: &str, a: &Args, writes: bool) -> Result<soup_door::target::Target> {
    let label = format!("`cauldron soup grammar {verb}`");
    let r = soup_door::marker::resolve(&label, None, a.env.as_deref(), a.cauldron.as_deref())?;
    Ok(soup_door::target::locate(&label, &r, writes)?)
}

fn read_file(path: &str) -> Result<Vec<u8>> {
    // Refused by size before it is read whole.
    let meta = std::fs::metadata(path).with_context(|| format!("reading grammar file `{path}`"))?;
    if meta.len() > spine::user_grammar::MAX_FILE_BYTES as u64 {
        return Ok(vec![b' '; spine::user_grammar::MAX_FILE_BYTES + 1]);
    }
    std::fs::read(path).with_context(|| format!("reading grammar file `{path}`"))
}

fn print(v: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn one_file<'a>(verb: &str, a: &'a Args) -> Result<&'a str> {
    match a.pos.as_slice() {
        [f] => Ok(f),
        _ => bail!("`cauldron soup grammar {verb}` takes one grammar file (got {}); `cauldron soup grammar help`", a.pos.len()),
    }
}

/// `cauldron soup grammar <verb> …`.
pub(crate) fn verb_grammar(args: &[String]) -> Result<i32> {
    match args.first().map(String::as_str) {
        Some("help") | Some("-h") | Some("--help") => {
            print!("{USAGE}");
            Ok(0)
        }
        Some("check") => check(&args[1..]),
        Some("add") => add(&args[1..]),
        Some("list") => list(&args[1..]),
        Some("show") => show(&args[1..]),
        Some("rebind") => rebind(&args[1..]),
        Some("remove") => remove(&args[1..]),
        None => {
            eprint!("{USAGE}");
            Ok(2)
        }
        Some(other) => {
            eprintln!("cauldron soup grammar: no verb `{other}` (check | add | list | show | rebind | remove | help)");
            Ok(2)
        }
    }
}

fn check(args: &[String]) -> Result<i32> {
    let a = parse("check", args)?;
    let file = one_file("check", &a)?;
    let bytes = read_file(file)?;
    let t = if a.env.is_some() || a.cauldron.is_some() { Some(target("check", &a, false)?) } else { None };
    let v = soup_door::grammars::check(t.as_ref(), &bytes, a.pool.as_deref())?;
    print(&v)?;
    Ok(if v["ok"] == Value::Bool(true) { 0 } else { 1 })
}

fn add(args: &[String]) -> Result<i32> {
    let a = parse("add", args)?;
    let file = one_file("add", &a)?;
    let (Some(env_name), Some(cauldron)) = (a.env.as_deref(), a.cauldron.as_deref()) else {
        bail!(
            "REFUSED: `cauldron soup grammar add` changes what a cauldron's soup accepts, so it takes the environment and \
             the cauldron only as flags: --env <env> --cauldron <name>. Nothing was written."
        );
    };
    let e = crate::mcp::environment(Some(env_name))?;
    let command = format!(
        "cauldron soup grammar add {file} --env {} --cauldron {cauldron}{}",
        e.name,
        a.pool.as_deref().map(|p| format!(" --pool {p}")).unwrap_or_default()
    );
    // Judged first, so a file that would be refused anyway says why, whoever
    // runs it; the gate is asked before anything is written.
    let bytes = read_file(file)?;
    let t = target("add", &a, true)?;
    let judged = soup_door::grammars::check(Some(&t), &bytes, a.pool.as_deref())?;
    if judged["ok"] != Value::Bool(true) {
        let mut v = judged;
        v["outcome"] = "refused".into();
        v["note"] = "Nothing was written. Each problem names its rule and where in the file.".into();
        print(&v)?;
        return Ok(1);
    }
    let allowed = crate::owner_acts::act(
        env::owner_acts::Act::Grammar,
        &e,
        cauldron,
        &format!(
            "REFUSED: registering a grammar is the owner's act at a terminal: it changes what cauldron `{cauldron}`'s soup \
             accepts. `cauldron soup grammar add` runs only with its input and output on an interactive terminal, unless \
             the owner has let agents register grammars (owner_acts `grammar`). Nothing was written. `cauldron soup \
             grammar check {file} --env {} --cauldron {cauldron}` judges the file without writing. Ask the user to run, in \
             a terminal on this machine:\n    {command}",
            e.name
        ),
    )?;
    let mut tries = 0;
    loop {
        tries += 1;
        match soup_door::grammars::add(&t, &bytes, a.pool.as_deref(), allowed.by().as_str()) {
            Ok(v) => {
                print(&v)?;
                return Ok(if v["outcome"] == "refused" { 1 } else { 0 });
            }
            Err(err) if err.code == soup_door::Code::Locked && tries == 1 => {
                let Some(pid) = soup_door::lock::held_by_service(&t.cauldron) else { return Err(err.into()) };
                match forward(&e, &t, &bytes, a.pool.as_deref(), allowed.by(), &command, pid)? {
                    Some(rc) => return Ok(rc),
                    // The service no longer holds the soup: add directly.
                    None => continue,
                }
            }
            Err(err) => return Err(err.into()),
        }
    }
}

/// What to run when the service holding the soup cannot take the add: stop
/// it, add, start it. Stopping a service touches no grant (they are the
/// owner's file, `mcp/grants.json`), so every grant keeps its scopes and
/// projects exactly; only the sessions' calls wait while it is down.
fn stop_add_start(e: &str, command: &str) -> String {
    format!(
        "Stopping the service changes no grant (each keeps its scopes and projects exactly), so do NOT revoke and grant \
         again, which would reset them:\n    cauldron --env {e} stop mcp\n    {command}\n    cauldron --env {e} start mcp"
    )
}

/// **The add, sent to the `mcp` service** (pid `pid`) that holds the soup,
/// on its `GRAMMAR` seam verb with the key it minted: the service adds the
/// pool through its own writer handle, so its routes have the new types at
/// once and no grant changes. `Some(rc)` once answered and printed; `None`
/// when the service says it does not hold the soup after all (the caller
/// adds directly).
fn forward(
    e: &env::Environment,
    t: &soup_door::target::Target,
    bytes: &[u8],
    pool: Option<&str>,
    by: env::owner_acts::By,
    command: &str,
    pid: u32,
) -> Result<Option<i32>> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        bail!("REFUSED: the grammar file is not UTF-8 text. Nothing was written.");
    };
    let mut body = serde_json::json!({"cauldron": t.cauldron.name, "op": "add", "file": text, "by": by.as_str()});
    if let Some(p) = pool {
        body["pool"] = p.into();
    }
    forward_op(e, t, body, "add", command, pid)
}

/// How long the command line waits for the service's answer to a grammar
/// act: the act waits its turn behind the writes queued before it.
const GRAMMAR_WAIT: std::time::Duration = std::time::Duration::from_secs(600);

/// [`forward`], for any `GRAMMAR` op (`add`, `remove`): `body` less the key.
fn forward_op(e: &env::Environment, t: &soup_door::target::Target, body: Value, what: &str, command: &str, pid: u32) -> Result<Option<i32>> {
    let c = &t.cauldron.name;
    let holder = format!("the `mcp` service (pid {pid})");
    let found = match crate::mcp_door::find(e) {
        Ok(f) if f.pid == pid => f,
        Ok(f) => bail!(
            "REFUSED: {holder} holds cauldron `{c}`'s soup, but the running `mcp` service is pid {}. Nothing was written. See \
             `cauldron mcp status --env {}`.",
            f.pid,
            e.name
        ),
        Err(why) => bail!(
            "REFUSED: {holder} holds cauldron `{c}`'s soup, so the add would be sent to it, but {}. Nothing was written. Try \
             again in a moment. {}",
            why.say(),
            stop_add_start(&e.name, command)
        ),
    };
    let key_path = soup_door::grants::store_dir(e).join("approve.secret");
    let key = std::fs::read_to_string(&key_path)
        .with_context(|| format!("reading the owner's key {} (the `mcp` service mints it when it starts)", key_path.display()))?;
    eprintln!(
        "cauldron soup grammar {what}: {holder} holds cauldron `{c}`'s soup; this is sent to it and made through it (it waits \
         behind the writes queued before it, up to {} minutes)",
        GRAMMAR_WAIT.as_secs() / 60
    );
    let answer = match crate::mcp_door::seam_ask_waiting(&found.seam, &format!("GRAMMAR {} {body}", key.trim()), GRAMMAR_WAIT) {
        Ok(a) => a,
        Err(err) if crate::mcp_door::timed_out(&err) => bail!(
            "IN PROGRESS, NOT KNOWN: {holder} took the `grammar {what}` and has not answered within {} minutes (it is busy with \
             writes queued before it). It may still be made. Check, rather than run it again: `cauldron soup grammar list \
             --env {} --cauldron {c}` (the pool is listed once it is made), and the service's audit (`cauldron mcp status --env \
             {}`).",
            GRAMMAR_WAIT.as_secs() / 60,
            e.name,
            e.name
        ),
        Err(err) => {
            return Err(err).with_context(|| format!("sending `grammar {what}` to {holder}; whether it was made is not known: check `cauldron soup grammar list --env {} --cauldron {c}`", e.name))
        }
    };
    if answer.starts_with("ERR unknown request") {
        bail!(
            "REFUSED: {holder} holds cauldron `{c}`'s soup, and it is older than this command line: it cannot add a grammar \
             while it runs (it answered `{answer}`). Nothing was written. Upgrade it (`scripts/install --replace-running`) \
             and run the add again, or: {}",
            stop_add_start(&e.name, command)
        );
    }
    let mut v: Value = serde_json::from_str(&answer).with_context(|| format!("{holder} answered `{answer}`"))?;
    if v["ok"] == Value::Bool(true) {
        if let Some(m) = v.as_object_mut() {
            m.remove("ok");
            m.insert("forwarded_to".into(), serde_json::json!({"service": "mcp", "pid": pid}));
        }
        print(&v)?;
        return Ok(Some(if v["outcome"] == "refused" { 1 } else { 0 }));
    }
    let code = v["code"].as_str().unwrap_or("");
    let message = v["message"].as_str().unwrap_or("");
    if code == "not_held" || code == "not_granted" {
        return Ok(None);
    }
    bail!("REFUSED ({holder} answered `{code}`): {message}")
}

/// The owner-act gate for `rebind` / `remove`, and the command to name.
fn owner(verb: &str, a: &Args) -> Result<(env::Environment, String, String, env::owner_acts::By)> {
    let (Some(env_name), Some(cauldron)) = (a.env.as_deref(), a.cauldron.as_deref()) else {
        bail!("REFUSED: `cauldron soup grammar {verb}` takes the environment and the cauldron only as flags: --env <env> --cauldron <name>. Nothing was written.");
    };
    let [pool] = a.pos.as_slice() else {
        bail!("`cauldron soup grammar {verb}` takes one pool name (`cauldron soup grammar list`)");
    };
    let e = crate::mcp::environment(Some(env_name))?;
    let command = format!("cauldron soup grammar {verb} {pool} --env {} --cauldron {cauldron} --yes", e.name);
    let by = if a.yes {
        crate::owner_acts::act(
            env::owner_acts::Act::Grammar,
            &e,
            cauldron,
            &format!(
                "REFUSED: `cauldron soup grammar {verb}` is the owner's act at a terminal (owner_acts `grammar`): it changes what \
                 cauldron `{cauldron}`'s soup holds. Nothing was written. Without --yes it only judges. Ask the user to run, in a \
                 terminal on this machine:\n    {command}"
            ),
        )?
        .by()
    } else {
        env::owner_acts::By::Tty
    };
    Ok((e, pool.clone(), command, by))
}

fn rebind(args: &[String]) -> Result<i32> {
    let a = parse("rebind", args)?;
    let (e, pool, command, by) = owner("rebind", &a)?;
    let t = target("rebind", &a, true)?;
    match soup_door::grammars::rebind(&t, &pool, a.yes, if a.yes { by.as_str() } else { "nobody (judged only)" }) {
        Ok(v) => {
            print(&v)?;
            Ok(0)
        }
        Err(err) if err.code == soup_door::Code::Locked && soup_door::lock::held_by_service(&t.cauldron).is_some() => bail!(
            "REFUSED: the `mcp` service holds cauldron `{}`'s soup, and it opened the soup before this file was put back, so the \
             file is taken only with the service stopped. Nothing was written. {}",
            t.cauldron.name,
            stop_add_start(&e.name, &command)
        ),
        Err(err) => Err(err.into()),
    }
}

fn remove(args: &[String]) -> Result<i32> {
    let a = parse("remove", args)?;
    let (e, pool, command, by) = owner("remove", &a)?;
    let t = target("remove", &a, true)?;
    let mut tries = 0;
    loop {
        tries += 1;
        match soup_door::grammars::remove(&t, &pool, a.yes, if a.yes { by.as_str() } else { "nobody (judged only)" }) {
            Ok(v) => {
                print(&v)?;
                return Ok(0);
            }
            Err(err) if err.code == soup_door::Code::Locked && tries == 1 => {
                let Some(pid) = soup_door::lock::held_by_service(&t.cauldron) else { return Err(err.into()) };
                let body = serde_json::json!({"cauldron": t.cauldron.name, "op": "remove", "pool": pool, "apply": a.yes, "by": by.as_str()});
                match forward_op(&e, &t, body, "remove", &command, pid)? {
                    Some(rc) => return Ok(rc),
                    None => continue,
                }
            }
            Err(err) => return Err(err.into()),
        }
    }
}

fn list(args: &[String]) -> Result<i32> {
    let a = parse("list", args)?;
    if let Some(p) = a.pos.first() {
        bail!("`cauldron soup grammar list` takes no arguments (got `{p}`)");
    }
    print(&soup_door::grammars::list(&target("list", &a, false)?)?)?;
    Ok(0)
}

fn show(args: &[String]) -> Result<i32> {
    let a = parse("show", args)?;
    let [pool] = a.pos.as_slice() else {
        bail!("`cauldron soup grammar show` takes one pool name (`cauldron soup grammar list`)");
    };
    let pool = soup_door::reads::checked_pool(pool)?;
    print(&soup_door::grammars::show(&target("show", &a, false)?, &pool)?)?;
    Ok(0)
}
