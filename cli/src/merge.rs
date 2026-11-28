//! **`cauldron merge <source> --into <target> --env <env> [--dry-run]`**:
//! copy one cauldron's soup and dumps into another, all or nothing
//! (`soup_door::merge`). The plan is always printed: with `--dry-run` it is
//! all that happens (judged on a snapshot of the target; nothing is held or
//! written); without, the same plan is printed with what landed.
//!
//! The target's writer: this command when nothing holds the target, or the
//! environment's `mcp` service when it holds it, to which the merge is sent
//! as one `MERGE` seam line (as `cauldron soup import` sends its batch). The
//! source must be held by no writer, and is never written.
//!
//! The owner act: a merge that carries a record's approval or rejection
//! (with that decision) is the owner act `merge` (`env::owner_acts`, asked
//! by `soup_door::merge::merger`, the one gate): with this command's input
//! and output on an interactive terminal (`by: tty`), or without one only
//! where the owner opened `merge` (`cauldron owner-acts open --only merge`,
//! or `CAULDRON_OWNER_ACTS=merge` for this command; `by: agent`). A merge
//! sent to the `mcp` service goes as `MERGE` from a terminal and as
//! `AMERGE` without one, which this command sends only where the owner's
//! persisted setting opens `merge` (the service checks that again, and
//! never reads the variable); a service older than `AMERGE` refuses it, so
//! an agent's merge is never recorded as a terminal's.

use anyhow::{bail, Result};
use serde_json::{json, Value};
use soup_door::merge::{self, Opts};

const USAGE: &str = "\
cauldron merge — copy one cauldron's soup and dumps into another

    cauldron merge <source> --into <target> --env <env> [--dry-run] [--allow-large]
    cauldron merge --undo <MG-n> --into <target> --env <env>
    cauldron merge --status <MG-n> --into <target> --env <env>

        Copies every record of <source> (both pools: regions, nodes, clusters and their claims), every
        edge between them, and every dump with its bytes, into <target>, as one all-or-nothing act.
        Prints the plan first, always, as JSON: counts by type, the id collisions and the id map (every
        id is re-minted in the target; cites, edges, [[links]] and the ids boundaries and notes name are
        rewritten through it), the regions and their boundaries, regions and clusters whose handle or
        boundary reads the same as one the target has (both are kept and flagged, never folded), edges
        across the two pools, the dumps (blobs the target holds already are shared), each record's
        review state, what is not carried and why, and what blocks the merge.

        Review state travels with each record: a ripple's record nobody decided stays unreviewed in the
        target, and lands in a write of the ripple class, so an older `cauldron` lists it as unreviewed
        too. An approval or rejection is carried only when the source's own write history vouches
        for it: the last write of that flag was a decision there (yours at a terminal, `by: tty`; or,
        where you opened approving to agents, your command without a terminal or a session's
        soup_approve / soup_reject, `by: agent`), or a merge that carried one. It arrives with who
        decided, how and when, and which merge carried it and who ran that merge, marked
        `carried_by_merge`; any other flag is not carried and the record arrives without it, saying
        why. The plan counts the decisions that will and will not be carried. Carrying decisions is
        the owner act `merge`: at an interactive terminal, or without one only where you opened it
        (`cauldron owner-acts open --only merge`, or CAULDRON_OWNER_ACTS=merge for the command; a
        merge sent to the `mcp` service without a terminal needs the persisted setting). The merge
        records who ran it (`by: tty | agent`).

        Not carried: the source's MCP grant and its automatic ripples setting (the target keeps its
        own; a merge never opens anything), and the source itself, which is left untouched (remove it
        with `cauldron remove` if you mean to).

        Nothing is carried twice: a record or dump whose origin (the cauldron it was first written in
        and its id there) the target already holds, merged before by this source or by another route,
        is listed under `already_here` and not carried again, and what points at it points at the
        target's copy. Merging a source again carries only what it gained since; a merge with nothing
        new is refused.

        Before anything lands, the merge takes a snapshot of the whole target (its soup, its dumps
        with every blob, its merges, its region lock table), verified file by file (sizes and
        SHA-256 in a manifest), beside the cauldron's directory as cauldrons/<target>.<MG-n>-before-
        <time>/, private to you. On APFS each file is a clone, so it costs almost nothing. It saves
        the merge's record and id map in merges/<MG-n>/. A merge cut short is settled by `cauldron
        soup recover --env <env> --cauldron <target>` (abandoned if it was cut before it held the
        target, else finished, or the target put back from the snapshot; `--abandon-merge` lifts
        its hold and keeps what landed); until then nothing else of this release writes to the
        target, and reads say `merge_pending`. While a merge lands, another writer is refused at once
        with `retry_after_secs` (`locked` directly, `busy` through the `mcp` service); reads go on.

        --dry-run    the plan only, judged on a private copy of the target; nothing is held or
                     written
        --allow-large
                     merge a source of more than 20,000 records (both pools together), or one
                     estimated to take longer than that many ordinary records would (edges and dump
                     bytes count), which is refused by default before anything is read in full,
                     held or written: a merge is one soup write whose time and memory grow faster
                     than the source (measured on one machine, a release build: 6.7 s and 620 MB at
                     11,000 records, 14 s and 1.2 GB at 22,000, 55 s and 3.1 GB at 55,000; 5,000
                     records with 30 edges each, 9.4 s and 1.1 GB), and the target refuses every
                     other writer for most of it. The refusal prints the estimate; with this flag
                     it is printed and the merge goes on. A source whose write would pass the
                     soup's per-statement fuel (about 3 a record and 5 an edge, against 1,000,000)
                     cannot land as one statement and is refused whatever this flag says
        --undo <MG-n>
                     put the target back exactly as it was before that merge, from its snapshot,
                     only if the target holds nothing but what it held when the merge began and
                     the merge's own writes (compared by content, the merge's records, dumps,
                     edges and blobs left out); otherwise refused, changing nothing, listing what
                     else changed by count and id. It never removes a write the merge did not
                     make; removing only the merge's records is not offered (the soup never
                     removes a record). Resumable: cut short, run it again (or `cauldron soup
                     recover`, or any write of this release, a merge included) to finish it, as
                     long as its snapshot is there and whole; if it is not, every one of those
                     refuses naming `cauldron soup recover --abandon-undo`, which keeps the merge:
                     the cauldron ends exactly as the merge left it, or it refuses and says why. What the undo
                     replaces is moved beside the cauldron (<target>.<MG-n>-undone-<time>/), not
                     deleted. Undoing the owner's merge (run at a terminal, or carrying
                     decisions) is the owner act `merge`, as the merge was. Refused while the
                     `mcp` service holds the target.

        --status <MG-n>
                     where that merge stands, read only: `planned` (cut short before its journal;
                     nothing landed), `landing` (cut short after it: `cauldron soup recover`
                     finishes it), `abandoned`, `restored`, `merged` (with the undo command),
                     `undoing` (an undo cut short, and how far it got), `undone`, `torn` or
                     `partial`, read from the stores

    Exit 0 when merged (or, with --dry-run, when the merge would land, or with --undo, when undone),
    1 when refused or blocked.
";

#[derive(Default)]
struct Args {
    env: Option<String>,
    into: Option<String>,
    dry_run: bool,
    allow_large: bool,
    undo: Option<String>,
    status: Option<String>,
    pos: Vec<String>,
}

fn parse(args: &[String]) -> Result<Args> {
    let mut a = Args::default();
    let mut it = args.iter();
    while let Some(s) = it.next() {
        let (flag, inline) = match s.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (s.as_str(), None),
        };
        let take = |it: &mut std::slice::Iter<'_, String>| -> Result<String> {
            match &inline {
                Some(v) => Ok(v.clone()),
                None => it.next().cloned().ok_or_else(|| anyhow::anyhow!("`cauldron merge {flag}` needs a value")),
            }
        };
        match flag {
            "--env" => a.env = Some(take(&mut it)?),
            "--into" => a.into = Some(take(&mut it)?),
            "--dry-run" => a.dry_run = true,
            "--allow-large" => a.allow_large = true,
            "--undo" => a.undo = Some(take(&mut it)?),
            "--status" => a.status = Some(take(&mut it)?),
            f if f.starts_with('-') => bail!("unknown option `{s}` for `cauldron merge` (`cauldron merge help`)"),
            _ => a.pos.push(s.clone()),
        }
    }
    Ok(a)
}

fn print(v: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// A refusal as JSON on stdout (with the plan, when it has one) and its
/// sentence on stderr; exit 1.
fn refused(e: &soup_door::Error) -> Result<i32> {
    let mut doc = match &e.detail {
        Some(d) if d.get("merge").is_some() => d.clone(),
        Some(d) => json!({"detail": d}),
        None => json!({}),
    };
    doc["outcome"] = json!("refused");
    doc["refusal"] = json!(e.code.as_str());
    doc["why"] = json!(e.message);
    print(&doc)?;
    eprintln!("cauldron merge: {}", e.message);
    Ok(1)
}

pub(crate) fn verb_merge(args: &[String]) -> Result<i32> {
    if matches!(args.first().map(String::as_str), Some("help" | "-h" | "--help")) {
        print!("{USAGE}");
        return Ok(0);
    }
    let a = parse(args)?;
    if let Some(merge_id) = a.status.as_deref() {
        let (true, Some(target), Some(env_name), false, None) = (a.pos.is_empty(), a.into.as_deref(), a.env.as_deref(), a.dry_run, a.undo.as_deref()) else {
            eprint!("{USAGE}");
            bail!("`cauldron merge --status <merge>` takes `--into <target>` and `--env <env>`, and nothing else");
        };
        let e = crate::mcp::environment(Some(env_name))?;
        let dst = soup_door::target::named(&e.name, target)?;
        return match merge::status(&dst.cauldron, merge_id) {
            Ok(doc) => {
                print(&doc)?;
                Ok(0)
            }
            Err(err) => refused(&err),
        };
    }
    if let Some(merge_id) = a.undo.as_deref() {
        let (true, Some(target), Some(env_name), false) = (a.pos.is_empty(), a.into.as_deref(), a.env.as_deref(), a.dry_run) else {
            eprint!("{USAGE}");
            bail!("`cauldron merge --undo <merge>` takes `--into <target>` and `--env <env>`, and no source and no --dry-run");
        };
        let e = crate::mcp::environment(Some(env_name))?;
        return undo(&e, merge_id, target);
    }
    let ([source], Some(target), Some(env_name)) = (a.pos.as_slice(), a.into.as_deref(), a.env.as_deref()) else {
        eprint!("{USAGE}");
        bail!("`cauldron merge` takes ONE source cauldron, `--into <target>` and `--env <env>`");
    };
    let e = crate::mcp::environment(Some(env_name))?;
    let src = soup_door::target::named(&e.name, source)?;
    let dst = soup_door::target::named(&e.name, target)?;
    let command = format!("cauldron merge {source} --into {target} --env {}{}", e.name, if a.dry_run { " --dry-run" } else { "" });
    // The size first: a source over the ceiling is refused before anything
    // is read in full, held or written (a dry run included: it reads the
    // whole source too).
    let size = match merge::refuse_large(&src, &dst, a.allow_large, &command) {
        Ok(size) => size,
        Err(err) => return refused(&err),
    };
    if let Some(said) = size["allow_large"].as_str() {
        eprintln!("cauldron merge: {said}");
    }
    let command = format!("cauldron merge {source} --into {target} --env {}", e.name);
    if a.dry_run {
        let mut doc = merge::dry_run(&src, &dst)?;
        doc["size"] = size;
        print(&doc)?;
        return Ok(if doc["outcome"] == "validated" { 0 } else { 1 });
    }
    let at_terminal = crate::review::at_terminal();
    let o = Opts { dry_run: false, at_terminal, var: std::env::var(env::owner_acts::VAR).ok(), command: command.clone() };
    merge::fault::check()?;
    soup_door::lock::refuse_live_soup(&e)?;
    let lock = match soup_door::lock::take_writer_lock(&dst.cauldron, merge::VERB) {
        Ok(l) => l,
        Err(err) if err.code == soup_door::Code::Locked => match soup_door::lock::held_by_service(&dst.cauldron) {
            Some(pid) => return forward(&e, source, target, &command, pid, at_terminal, a.allow_large),
            None => return refused(&err),
        },
        Err(err) => return refused(&err),
    };
    match direct(&src, &dst, &o) {
        Ok(mut doc) => {
            drop(lock);
            doc["size"] = size;
            print(&doc)?;
            eprintln!(
                "cauldron merge: merged `{source}` into `{target}` as {}: {} record(s), {} dump(s). Its record: {}",
                doc["merge"].as_str().unwrap_or("?"),
                doc["landed"]["records"],
                doc["landed"]["dumps"],
                doc["record"].as_str().unwrap_or("?")
            );
            Ok(0)
        }
        Err(err) => refused(&err),
    }
}

/// The merge, with this command as the target's writer (its soup's lock is
/// held by the caller).
fn direct(src: &soup_door::Target, dst: &soup_door::Target, o: &Opts) -> soup_door::Result<Value> {
    let c = &dst.cauldron;
    // An undo cut short is finished first (as by every writer of this
    // release), so it is not reported as a soup not at rest.
    merge::refuse_pending(c, "start a merge")?;
    let unrest = spine::offline::rest_check(&c.soup()).map_err(|err| soup_door::Error::new(soup_door::Code::Store, format!("reading the target's spine: {err}")))?;
    if !unrest.is_empty() {
        return Err(soup_door::Error::new(
            soup_door::Code::NotAtRest,
            format!(
                "REFUSED: the target, cauldron `{}`'s soup, is not at rest ({}); run `cauldron soup recover --env {} --cauldron {}` \
                 first. Nothing was written.",
                c.name,
                unrest.join("; "),
                dst.env.name,
                c.name
            ),
        ));
    }
    merge::refuse_pending(c, "start a merge")?;
    let _held = merge::hold_source(src)?;
    let p = merge::prepare(src, dst)?;
    // Refused before anything is opened; the plan says why.
    if let Some(err) = merge::refusal_of(&p, o) {
        return Err(err);
    }
    eprintln!(
        "cauldron merge: plan {}: {} record(s), {} dump(s); landing into `{}`",
        p.doc["merge"].as_str().unwrap_or("?"),
        p.doc["counts"]["ws"].as_object().map_or(0, |m| m.values().filter_map(Value::as_u64).sum::<u64>())
            + p.doc["counts"]["cauldron"].as_object().map_or(0, |m| m.values().filter_map(Value::as_u64).sum::<u64>()),
        p.doc["dumps"].as_array().map_or(0, Vec::len),
        c.name
    );
    // The dumps store's writer lock too, then the snapshot of the whole
    // target, before anything of it is opened (an open may recover or
    // sweep, and the snapshot is the target as it was).
    let dumps_lock = soup_door::dumps::lock_for_merge(c, &format!("cli {} merge", std::process::id()))?;
    let before = merge::snapshot(&p, dst)?;
    let mut handle = spine::Cauldron::open(&c.soup(), spine::config::SpineConfig::default())
        .map_err(|err| soup_door::Error::new(soup_door::Code::Store, format!("opening cauldron `{}`'s soup: {err}", c.name)))?;
    handle.set_region_guard(soup_door::region_lock::Guard::new(c));
    let mut store = if p.needs_dumps { Some(soup_door::dumps::Store::open_held(c, dumps_lock)?) } else { drop(dumps_lock); None };
    merge::apply(p, &before, dst, &mut handle, store.as_mut(), o)
}

/// **`cauldron merge --undo <merge> --into <target>`**: the target's writer
/// locks (refused while the `mcp` service holds it), then
/// [`merge::undo`].
fn undo(e: &env::Environment, merge_id: &str, target: &str) -> Result<i32> {
    let dst = soup_door::target::named(&e.name, target)?;
    let c = &dst.cauldron;
    let by = if crate::review::at_terminal() { "tty" } else { "agent" };
    let run = || -> soup_door::Result<Value> {
        soup_door::lock::refuse_live_soup(e)?;
        let _lock = soup_door::lock::take_writer_lock(c, merge::VERB).map_err(|err| match soup_door::lock::held_by_service(c) {
            Some(pid) => soup_door::Error::new(
                soup_door::Code::Locked,
                format!(
                    "REFUSED: the `{}` service (pid {pid}) holds cauldron `{target}`, and an undo puts its files back, which \
                     nothing may do while a writer has it open. Nothing was changed. Close it to the service first (`cauldron \
                     mcp revoke {target} --env {}`, granting it again afterwards) or stop the service (`cauldron --env {} stop \
                     mcp`), then undo.",
                    soup_door::lock::MCP_SERVICE,
                    e.name,
                    e.name
                ),
            ),
            None => err,
        })?;
        let _dumps = soup_door::dumps::lock_for_merge(c, &format!("cli {} merge-undo", std::process::id()))?;
        merge::undo(&dst, merge_id, by, std::env::var(env::owner_acts::VAR).ok().as_deref())
    };
    match run() {
        Ok(doc) => {
            print(&doc)?;
            eprintln!("cauldron merge: {merge_id} undone; cauldron `{target}` is as it was before it");
            Ok(0)
        }
        Err(err) => refused(&err),
    }
}

/// **The merge, sent to the `mcp` service** that holds the target (its pid
/// is in the writer lock) as one line with the approval key: `MERGE` from a
/// terminal, `AMERGE` without one, sent only where the owner's PERSISTED
/// owner_acts setting opens `merge` for the target (the service takes an
/// agent's merge on that alone, and checks it again).
fn forward(e: &env::Environment, source: &str, target: &str, command: &str, pid: u32, at_terminal: bool, allow_large: bool) -> Result<i32> {
    let svc = crate::mcp_door::SERVICE;
    let verb = if at_terminal {
        "MERGE"
    } else {
        let setting = env::owner_acts::load(&e.root).unwrap_or_default();
        let id = e.find_cauldron(target).ok().flatten().and_then(|c| c.id());
        if env::owner_acts::decide(env::owner_acts::Act::Merge, &setting, &e.name, target, id.as_deref(), false, None).is_err() {
            let var_opens = env::owner_acts::from_process().is_ok_and(|v| v.contains(&env::owner_acts::Act::Merge));
            bail!(
                "REFUSED: the `{svc}` service (pid {pid}) holds cauldron `{target}`, so this merge would be sent to it, and a merge \
                 sent to it without a terminal is taken only where the owner's persisted owner_acts setting opens `merge` for \
                 `{target}`{}. Nothing was written. Ask the user to run, in a terminal on this machine:\n    {command}\n\
                 or record the choice once (`cauldron owner-acts open --only merge --cauldron {target} --env {env}`, at a \
                 terminal or with {var}=merge for that command), or close the target to the service first (`cauldron mcp \
                 revoke {target} --env {env}`, granting it again afterwards) and merge directly.",
                if var_opens { format!(" ({} opens it for this command only, and never reaches the service)", env::owner_acts::VAR) } else { String::new() },
                env = e.name,
                var = env::owner_acts::VAR
            );
        }
        "AMERGE"
    };
    eprintln!("cauldron merge: the `{svc}` service (pid {pid}) holds cauldron `{target}`; the merge is sent to it and lands through it");
    let mut body = json!({"source": source, "target": target, "command": command});
    if allow_large {
        // Sent only when given, so a service that does not know it is never
        // sent a key it would refuse for a merge that does not need it.
        body["allow_large"] = json!(true);
    }
    let v = match crate::review::send_owner(e, verb, "the verb this cauldron merges into a cauldron the service holds with", body, std::time::Duration::from_secs(600)) {
        Ok(v) => v,
        Err(err) => bail!(
            "{err:#}. If the merge was sent, WHETHER IT LANDED IS NOT KNOWN from here: run the same command again (a merge that \
             landed is refused as already merged), and if `cauldron soup recover --env {} --cauldron {target}` reports a merge \
             it finished, it was cut short and is now whole.",
            e.name
        ),
    };
    let mut doc = v.clone();
    doc["forwarded_to"] = json!({"service": svc, "pid": pid});
    if v["ok"] == json!(true) {
        if let Some(m) = doc.as_object_mut() {
            m.remove("ok");
        }
        print(&doc)?;
        eprintln!("cauldron merge: merged `{source}` into `{target}` as {} through the `{svc}` service", v["merge"].as_str().unwrap_or("?"));
        Ok(0)
    } else {
        let mut out = match &v["detail"] {
            d if d.get("merge").is_some() => d.clone(),
            Value::Null => json!({}),
            d => json!({"detail": d}),
        };
        out["outcome"] = json!("refused");
        out["refusal"] = v["code"].clone();
        out["why"] = v["message"].clone();
        out["forwarded_to"] = json!({"service": svc, "pid": pid});
        print(&out)?;
        eprintln!("cauldron merge: refused by the `{svc}` service ({}): {}", v["code"].as_str().unwrap_or("?"), v["message"].as_str().unwrap_or(""));
        Ok(1)
    }
}

/// **Every merge or undo into a cauldron of `e` that was cut short**, for
/// `cauldron status`: `{cauldron, merge, state: landing | undoing, finish}`.
pub(crate) fn cut_short_all(e: &env::Environment) -> Vec<Value> {
    let mut out = Vec::new();
    for c in e.cauldrons().unwrap_or_default() {
        if let Ok(Some(u)) = merge::undo_pending(&c) {
            out.push(json!({
                "cauldron": c.name,
                "merge": u["merge"],
                "state": "undoing",
                "step": u["state"],
                "finish": format!("cauldron merge --undo {} --into {} --env {} (or cauldron soup recover --env {} --cauldron {})", u["merge"].as_str().unwrap_or("?"), c.name, e.name, e.name, c.name),
            }));
        }
        if let Ok(Some((id, _))) = merge::pending(&c) {
            out.push(json!({
                "cauldron": c.name,
                "merge": id,
                "state": "landing",
                "finish": format!("cauldron soup recover --env {} --cauldron {}", e.name, c.name),
            }));
        }
    }
    out
}
