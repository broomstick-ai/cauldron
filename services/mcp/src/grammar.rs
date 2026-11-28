//! **The owner's grammar add, while this service holds the soup**: the
//! `GRAMMAR` seam verb, which only `cauldron soup grammar add` sends.
//!
//! A soup this service has opened for writing is held by its writer thread
//! (the writer lock), and that thread's handle knows the pools it opened
//! with. So `cauldron soup grammar add` does not add the pool beside the
//! service: it sends the file here, and the writer thread adds the pool
//! through its own handle (`soup_door::grammars::add_on`). The new pool's
//! types route at once, from `soup_put` and from a forwarded import alike,
//! and nothing about the cauldron's grant is touched: no revoke, no grant,
//! no session cut.
//!
//! Body: `GRAMMAR <key> {cauldron, file, pool?, by}`, where `key` is the
//! approval key this service minted into its directory (`approve.secret`)
//! at its last start, the same one `DECIDE` takes; `file` is the grammar
//! file's text; `by` is `tty` (the command line found a terminal) or
//! `agent` (it did not, and asked the owner_acts gate). An `agent` add is
//! taken only where the owner's PERSISTED owner_acts setting opens
//! `grammar` for the cauldron, whatever the command line's environment
//! said; a `tty` claim is recorded as what it says. Answered with one JSON
//! line: the add's own answer plus `ok`, or `{ok: false, code, message}`.
//! When the writer is not open (nothing here holds the soup), the answer
//! is `not_held` and the command line adds directly. Every call after the
//! key is audited (`tool: soup_grammar_add`).

use serde_json::{json, Value};
use shapewire::envelope::Refusal;

/// The seam verb.
pub const VERB: &str = "GRAMMAR";

/// The refusal code when this service does not hold the cauldron's soup.
pub const NOT_HELD: &str = "not_held";

/// **Answer one `GRAMMAR` line.**
pub fn answer(door: &crate::door::Door, key: &crate::secret::Secret, rest: &str) -> String {
    let fail = |r: &Refusal| {
        let mut v = json!({"ok": false, "code": r.code, "message": r.message});
        if let Some(d) = &r.detail {
            v["detail"] = d.clone();
        }
        v.to_string()
    };
    let (given, body) = rest.split_once(' ').unwrap_or((rest, ""));
    if given.is_empty() || !key.is(given) {
        tracing::warn!("a GRAMMAR line without the approval key was refused");
        return fail(&Refusal::new(
            "unauthorized",
            "registering a grammar is the owner's act: this verb answers only `cauldron soup grammar add` (at a terminal, \
             or without one where the owner's owner_acts setting opens `grammar`), which carries the key this service \
             minted into its directory (approve.secret) at its last start. No MCP tool registers a grammar. Nothing was \
             written.",
        ));
    }
    let args: serde_json::Map<String, Value> = match serde_json::from_str::<Value>(body.trim()) {
        Ok(Value::Object(m)) => m,
        _ => return fail(&Refusal::new("usage", "GRAMMAR takes one JSON object after the key. Nothing was written.")),
    };
    let cauldron = args.get("cauldron").and_then(Value::as_str).unwrap_or("").to_string();
    let by_word = args.get("by").and_then(Value::as_str).unwrap_or("").to_string();
    let out = (|| -> Result<Value, Refusal> {
        if let Some(k) = args.keys().find(|k| !["cauldron", "op", "file", "pool", "apply", "by"].contains(&k.as_str())) {
            return Err(Refusal::new("usage", format!("GRAMMAR takes `cauldron`, `op`, `file`, `pool`, `apply` and `by`, not `{k}`. Nothing was written.")));
        }
        let op = args.get("op").and_then(Value::as_str).unwrap_or("add").to_string();
        if op != "add" && op != "remove" {
            return Err(Refusal::new("usage", "`op` is `add` or `remove`. Nothing was written."));
        }
        let apply = args.get("apply").and_then(Value::as_bool).unwrap_or(false);
        if cauldron.trim().is_empty() {
            return Err(Refusal::new("usage", "`cauldron` must be a non-empty string. Nothing was written."));
        }
        let file = args.get("file").and_then(Value::as_str).map(str::to_string);
        if op == "add" && file.is_none() {
            return Err(Refusal::new("usage", "`file` is the grammar file's text. Nothing was written."));
        }
        let pool = match args.get("pool") {
            None | Some(Value::Null) => None,
            Some(Value::String(p)) => Some(p.clone()),
            Some(_) => return Err(Refusal::new("usage", "`pool` is a string. Nothing was written.")),
        };
        let by = match by_word.as_str() {
            "tty" => env::owner_acts::By::Tty,
            "agent" => env::owner_acts::By::Agent,
            _ => return Err(Refusal::new("usage", "`by` is `tty` or `agent`. Nothing was written.")),
        };
        let t = door.owner_target(&cauldron)?;
        if by == env::owner_acts::By::Agent {
            let setting = env::owner_acts::load(&door.env.root)
                .map_err(|e| Refusal::new("store", format!("reading the owner_acts setting: {e}. Nothing was written.")))?;
            let id = t.cauldron.id();
            if env::owner_acts::decide(env::owner_acts::Act::Grammar, &setting, &door.env.name, &t.cauldron.name, id.as_deref(), false, None).is_err() {
                return Err(Refusal::new(
                    crate::door::OWNER_ACT_CLOSED,
                    format!(
                        "an agent's grammar {op} is taken only where the owner's persisted owner_acts setting opens `grammar`, \
                         and for cauldron `{c}` it keeps it at a terminal. Nothing was written. CAULDRON_OWNER_ACTS does not \
                         open it here: record the choice with `cauldron owner-acts open --only grammar --cauldron {c} --env {e}` \
                         (at a terminal), or run the add at a terminal.",
                        c = t.cauldron.name,
                        e = door.env.name
                    ),
                ));
            }
        }
        let by_s = by.as_str().to_string();
        let ran = door.writers.with_mut_if_open(
            &t,
            Box::new(move |h, t| match (op.as_str(), file, pool) {
                ("remove", _, Some(p)) => soup_door::grammars::remove_on(h, t, &p, apply, &by_s).map_err(crate::refusal),
                ("remove", _, None) => Err(Refusal::new("usage", "`remove` names the `pool`. Nothing was removed.")),
                (_, f, p) => soup_door::grammars::add_on(h, t, f.unwrap_or_default().as_bytes(), p.as_deref(), &by_s).map_err(crate::refusal),
            }),
        );
        match ran {
            Some(r) => r,
            None => Err(Refusal::new(
                NOT_HELD,
                format!("this service does not hold cauldron `{cauldron}`'s soup (it has not opened it for writing), so the add is the command line's to make directly. Nothing was written here."),
            )),
        }
    })();
    let mut line = crate::audit::Line {
        client: Some("cli".into()),
        cauldron: (!cauldron.is_empty()).then_some(cauldron),
        tool: format!("soup_grammar_{}", args.get("op").and_then(Value::as_str).unwrap_or("add")),
        by: (!by_word.is_empty()).then_some(by_word),
        ..crate::audit::Line::default()
    };
    match &out {
        Ok(v) => {
            line.outcome = v.get("outcome").and_then(Value::as_str).unwrap_or("ok").to_string();
            line.counts = json!({"pool": v["pool"].get("pool").cloned().unwrap_or_else(|| v["pool"].clone()), "schema": v["pool"]["schema"].clone(),
                "by_is": "the command line's claim (it holds the owner's key); the service checks `agent` against the owner's recorded choice, and cannot check `tty`"});
        }
        Err(r) => line.outcome = r.code.to_string(),
    }
    door.audit(line);
    match out {
        Ok(mut v) => {
            v["ok"] = json!(true);
            v.to_string()
        }
        Err(r) => fail(&r),
    }
}
