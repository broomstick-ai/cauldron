//! **A merge into a cauldron this service holds**: the `MERGE` seam verb,
//! which `cauldron merge` sends when the target's writer lock is this
//! service's (as `cauldron soup import` sends its batch here as one
//! `soup_put`). No MCP tool merges.
//!
//! `MERGE <key> {source, target, command}` and `AMERGE <key> {...}`: the key
//! is the approval key this service mints into `approve.secret` at every
//! start, which the command line reads only at an interactive terminal
//! (`MERGE`, recorded `by: "tty"`) or where the owner's owner_acts setting
//! opens `merge` (`AMERGE`, recorded `by: "agent"`). An `AMERGE` is taken
//! only where the PERSISTED setting opens `merge` for the target; this
//! service never reads `CAULDRON_OWNER_ACTS`. A verb of its own, so that a
//! service older than it refuses an agent's merge instead of recording it
//! as a terminal's. No session and no ripple can send either: no MCP tool
//! merges. (A claim of a terminal, `MERGE`, cannot be checked from here; it
//! is recorded as what it says. The boundary is the user account.)
//!
//! The merge runs on the target's writer thread, with its handle
//! (`soup_door::merge`): the source is held for the length of it, the plan
//! is made there, and it lands under the target's writer, its dumps through
//! the dumps store this service holds for the target (or, when it holds
//! none, one it opens for the merge alone). A merge that is cut short after
//! it began landing closes the target to this service, so that `cauldron
//! soup recover` can finish it.

use std::sync::Arc;

use serde_json::{json, Value};
use shapewire::envelope::Refusal;
use soup_door::merge;

use crate::refusal;

/// The seam verb of a merge run at a terminal.
pub const VERB: &str = "MERGE";
/// The seam verb of a merge run without one, where the owner opened it.
pub const AVERB: &str = "AMERGE";

/// **Answer one `MERGE` line** (a terminal's merge).
pub fn answer(door: &Arc<crate::door::Door>, key: &crate::secret::Secret, rest: &str) -> String {
    answer_as(door, key, rest, env::owner_acts::By::Tty)
}

/// **Answer one `AMERGE` line** (an agent's merge, where the owner's
/// persisted setting opens `merge`).
pub fn answer_agent(door: &Arc<crate::door::Door>, key: &crate::secret::Secret, rest: &str) -> String {
    answer_as(door, key, rest, env::owner_acts::By::Agent)
}

fn answer_as(door: &Arc<crate::door::Door>, key: &crate::secret::Secret, rest: &str, by: env::owner_acts::By) -> String {
    let verb = if by == env::owner_acts::By::Tty { VERB } else { AVERB };
    let fail = |r: &Refusal| {
        let mut v = json!({"ok": false, "code": r.code, "message": r.message});
        if let Some(d) = &r.detail {
            v["detail"] = d.clone();
        }
        v.to_string()
    };
    let (given, body) = rest.split_once(' ').unwrap_or((rest, ""));
    if given.is_empty() || !key.is(given) {
        tracing::warn!("a merge line without the approval key was refused");
        return fail(&Refusal::new(
            "unauthorized",
            "a merge sent to this service is the owner's: this verb answers only `cauldron merge` run at a terminal (MERGE) or \
             without one where the owner opened `merge` (AMERGE), which carries the key this service minted into its directory \
             (approve.secret) at its last start. Nothing was written.",
        ));
    }
    let args: serde_json::Map<String, Value> = match serde_json::from_str::<Value>(body.trim()) {
        Ok(Value::Object(m)) => m,
        _ => return fail(&Refusal::new("usage", format!("{verb} takes one JSON object after the key. Nothing was written."))),
    };
    for k in args.keys() {
        if !["source", "target", "command", "allow_large"].contains(&k.as_str()) {
            return fail(&Refusal::new("usage", format!("{verb} does not take `{k}`. Nothing was written.")));
        }
    }
    let text = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let (source, target, command) = (text("source"), text("target"), text("command"));
    let out = (|| -> Result<Value, Refusal> {
        if source.trim().is_empty() || target.trim().is_empty() {
            return Err(Refusal::new("usage", "`source` and `target` are cauldron names. Nothing was written."));
        }
        let t = door.owner_target(&target)?;
        // What this service can check of the caller's claim: an agent's
        // merge is taken only where the PERSISTED setting opens `merge`.
        if by == env::owner_acts::By::Agent {
            let setting = env::owner_acts::load(&door.env.root)
                .map_err(|e| Refusal::new("store", format!("reading the owner_acts setting: {e}. Nothing was written.")))?;
            let id = t.cauldron.id();
            if env::owner_acts::decide(env::owner_acts::Act::Merge, &setting, &door.env.name, &t.cauldron.name, id.as_deref(), false, None).is_err() {
                return Err(Refusal::new(
                    crate::door::OWNER_ACT_CLOSED,
                    format!(
                        "an agent's merge (AMERGE) is taken only where the owner's persisted owner_acts setting opens `merge`, and \
                         for cauldron `{c}` it keeps it at a terminal. Nothing was written. CAULDRON_OWNER_ACTS does not open it \
                         here: record the choice with `cauldron owner-acts open --only merge --cauldron {c} --env {e}`.",
                        c = t.cauldron.name,
                        e = door.env.name
                    ),
                ));
            }
        }
        let src = soup_door::target::named(&door.env.name, &source).map_err(refusal)?;
        // Over the ceiling: refused before the writer thread is asked for
        // the target (nothing held, nothing read in full).
        let allow_large = args.get("allow_large").and_then(Value::as_bool).unwrap_or(false);
        let size = merge::refuse_large(&src, &t, allow_large, &command).map_err(refusal)?;
        // From here until it is done, a session's write to the target is
        // refused at once (`busy`, with a retry_after) instead of waiting
        // in the writer's queue behind the merge.
        let _mark = door.begin_merge(&t.cauldron.name, size["estimate"]["seconds"].as_u64().unwrap_or(30));
        let thread = if by == env::owner_acts::By::Tty { crate::approve::BY.to_string() } else { format!("{}:agent", crate::approve::BY) };
        let who = crate::writer::Who { client: "cli".into(), thread, session: None };
        let d = door.clone();
        door.writers.with_mut(
            &t,
            who,
            Box::new(move |soup, t| {
                let _held = merge::hold_source(&src).map_err(refusal)?;
                let p = merge::prepare(&src, t).map_err(refusal)?;
                // The service never reads CAULDRON_OWNER_ACTS: an agent's
                // merge carries decisions only where the persisted setting
                // opens `merge` (`merge::merger`, the one gate).
                let o = merge::Opts { dry_run: false, at_terminal: by == env::owner_acts::By::Tty, var: None, command };
                if let Some(e) = merge::refusal_of(&p, &o) {
                    return Err(refusal(e));
                }
                let name = t.cauldron.name.clone();
                // The store this service holds for the target, or one opened
                // for the merge alone; either way its writer lock is held
                // before the snapshot of the whole target is taken, and
                // nothing of the target writes while it is copied (this is
                // its writer thread).
                let mut p = Some(p);
                let held = d.dumps.with_held_mut(&name, |st| -> Result<Value, Refusal> {
                    let p = p.take().expect("once");
                    let before = merge::snapshot(&p, t).map_err(refusal)?;
                    merge::apply(p, &before, t, soup, Some(st), &o).map_err(refusal)
                });
                match held {
                    Some(r) => r,
                    None => {
                        let p = p.take().expect("once");
                        let lock = soup_door::dumps::lock_for_merge(&t.cauldron, &format!("{} {}", soup_door::lock::MCP_SERVICE, std::process::id()))
                            .map_err(refusal)?;
                        let before = merge::snapshot(&p, t).map_err(refusal)?;
                        if !p.needs_dumps {
                            drop(lock);
                            return merge::apply(p, &before, t, soup, None, &o).map_err(refusal);
                        }
                        let mut st = soup_door::dumps::Store::open_held(&t.cauldron, lock).map_err(refusal)?;
                        merge::apply(p, &before, t, soup, Some(&mut st), &o).map_err(refusal)
                    }
                }
            }),
        )
    })();
    // Cut short after it began landing: let the cauldron go, so `cauldron
    // soup recover` can finish it.
    if out.is_err() {
        if let Ok(Some(c)) = door.env.find_cauldron(&target) {
            if merge::pending(&c).ok().flatten().is_some() {
                door.writers.close(&target, "a merge was cut short; `cauldron soup recover` finishes it");
                door.dumps.close(&target, "a merge was cut short; `cauldron soup recover` finishes it");
            }
        }
    }
    let mut line = crate::audit::Line {
        client: Some("cli".into()),
        cauldron: (!target.is_empty()).then(|| target.clone()),
        tool: "merge".into(),
        by: Some(by.as_str().into()),
        ..crate::audit::Line::default()
    };
    match &out {
        Ok(v) => {
            line.outcome = v["outcome"].as_str().unwrap_or("ok").to_string();
            line.tx = v["landed"]["soup_tx"].as_str().map(str::to_string);
            line.counts = json!({"source": source, "merge": v["merge"], "records": v["landed"]["records"], "dumps": v["landed"]["dumps"]});
        }
        Err(r) => {
            line.outcome = r.code.to_string();
        }
    }
    door.audit(line);
    match out {
        Ok(v) => {
            let mut v = v;
            v["ok"] = json!(true);
            v.to_string()
        }
        Err(r) => fail(&r),
    }
}
