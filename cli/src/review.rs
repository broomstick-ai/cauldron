//! **`cauldron ripples review`**: the records a ripple wrote that nobody
//! has reviewed yet, grouped by the dump and the run that produced them
//! (`soup_door::review`). Read-only, through connections that cannot
//! write, so it works anywhere and whatever holds the cauldron.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use soup_door::review::{self, QueueOpts};

/// The flags the review verbs take, parsed.
#[derive(Default)]
pub(crate) struct Args {
    pub env: Option<String>,
    pub cauldron: Option<String>,
    pub dump: Option<String>,
    pub run: Option<String>,
    pub node_type: Option<String>,
    pub since: Option<String>,
    pub after: Option<String>,
    pub max_chars: Option<usize>,
    pub reason: Option<String>,
    pub text: bool,
    pub interactive: bool,
    pub undo: bool,
    pub pos: Vec<String>,
}

const VALUED: &[&str] = &["--env", "--cauldron", "--dump", "--run", "--type", "--since", "--after", "--max-chars", "--reason"];

pub(crate) fn parse(verb: &str, args: &[String], allow: &[&str]) -> Result<Args> {
    let mut a = Args::default();
    let mut it = args.iter();
    while let Some(s) = it.next() {
        let (flag, inline) = match s.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (s.as_str(), None),
        };
        if !flag.starts_with('-') {
            a.pos.push(s.clone());
            continue;
        }
        if !allow.contains(&flag) {
            bail!("unknown option `{s}` for `cauldron ripples {verb}` (`cauldron ripples help`)");
        }
        if !VALUED.contains(&flag) {
            if inline.is_some() {
                bail!("`{flag}` takes no value");
            }
            match flag {
                "--text" => a.text = true,
                "--interactive" => a.interactive = true,
                _ => a.undo = true,
            }
            continue;
        }
        let v = match inline {
            Some(v) => v,
            None => it.next().cloned().with_context(|| format!("`cauldron ripples {verb} {flag}` needs a value"))?,
        };
        let count = |v: &str| -> Result<usize> { v.parse::<usize>().with_context(|| format!("`{flag}` takes a whole number, not `{v}`")) };
        match flag {
            "--env" => a.env = Some(v),
            "--cauldron" => a.cauldron = Some(v),
            "--dump" => a.dump = Some(v),
            "--run" => a.run = Some(v),
            "--type" => a.node_type = Some(v),
            "--since" => a.since = Some(v),
            "--after" => a.after = Some(v),
            "--max-chars" => a.max_chars = Some(count(&v)?),
            _ => a.reason = Some(v),
        }
    }
    Ok(a)
}

/// The cauldron the verb is about, named by both flags.
pub(crate) fn target(verb: &str, a: &Args) -> Result<soup_door::Target> {
    let (Some(env), Some(cauldron)) = (a.env.as_deref(), a.cauldron.as_deref()) else {
        bail!("`cauldron ripples {verb}` takes the environment and the cauldron as flags: --env <env> --cauldron <name>");
    };
    let label = format!("`cauldron ripples {verb}`");
    let r = soup_door::marker::resolve(&label, None, Some(env), Some(cauldron))?;
    Ok(soup_door::target::locate(&label, &r, false)?)
}

pub(crate) fn opts(a: &Args) -> QueueOpts {
    QueueOpts {
        dump: a.dump.clone(),
        run: a.run.clone(),
        node_type: a.node_type.clone(),
        since: a.since.clone(),
        after: a.after.clone(),
        max_chars: a.max_chars,
    }
}

fn print(v: &Value) -> Result<()> {
    println!("{}", review::json_for_terminal(v));
    Ok(())
}

/// **`cauldron ripples review`**.
pub(crate) fn review(args: &[String]) -> Result<i32> {
    let a = parse(
        "review",
        args,
        &["--env", "--cauldron", "--dump", "--run", "--type", "--since", "--after", "--max-chars", "--text", "--interactive"],
    )?;
    if let Some(p) = a.pos.first() {
        bail!("`cauldron ripples review` takes no arguments (got `{p}`); filter with --dump, --run, --type or --since");
    }
    if a.interactive {
        if a.text || a.after.is_some() || a.max_chars.is_some() {
            bail!("`--interactive` walks the whole queue: it takes the filters (--dump, --run, --type, --since), not --text, --after or --max-chars");
        }
        return crate::review_pass::interactive(&a);
    }
    let max = review::check_max(a.max_chars)?;
    let t = target("review", &a)?;
    if a.text {
        print!("{}", review::text_within(&t, &opts(&a), max)?);
    } else {
        print(&review::queue_within(&t, &opts(&a), max)?)?;
    }
    Ok(0)
}

/// Is this command's input and output an interactive terminal?
pub(crate) fn at_terminal() -> bool {
    crate::owner_acts::at_terminal()
}

/// `x.y.z` (anything after a `-` or `+` is ignored) as numbers.
pub(crate) fn version_parts(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    let out = (it.next()??, it.next()??, it.next()??);
    it.next().is_none().then_some(out)
}

/// The refusal for a running `mcp` service that cannot take this command
/// line's decision: `why` says what was found, and the version-skew message
/// (or, between two builds of one version, the same two commands) says how
/// to move the service to this build.
fn old_service(e: &env::Environment, service: Option<&str>, why: &str, sent: bool) -> anyhow::Error {
    let how = match service.and_then(|sv| crate::mcp_door::skew_note(sv, &e.name)) {
        Some(note) => format!("Version skew: {note}."),
        None => {
            let [build, pin] = crate::mcp_door::upgrade_commands(&e.name);
            format!(
                "Run this version's service: `scripts/install --replace-running` from your checkout, or `{build}` and then \
                 `{pin}` (the supervisor moves the running service to the new build)."
            )
        }
    };
    let done = if sent { "It wrote nothing, and nothing was decided" } else { "Nothing was sent and nothing was written" };
    anyhow::anyhow!(
        "REFUSED: {why}. {done}: an older `mcp` service would not read this decision as this cauldron means it, so the \
         `mcp` service must be upgraded with the command line. {how}"
    )
}

/// **Send one decision to the running `mcp` service**, after the caller
/// asked the owner acts' gate (`crate::owner_acts::act`): its `DECIDE` seam
/// verb for a person at a terminal (`by: tty`), `ADECIDE` for an agent the
/// owner allowed (`by: agent`), with the approval key it minted. `body` is
/// what either takes, less the cauldron, which is added. The service's JSON
/// answer. A service older than `ADECIDE` answers `ERR unknown request` and
/// writes nothing, so an agent's decision is never recorded as a person's.
///
/// Refused, with nothing sent, when the service is older than this command
/// line or its version cannot be read; and when the service does not know
/// `DECIDE` (a service from before it: it answers `ERR unknown request` and
/// writes nothing), with the upgrade commands.
pub(crate) fn send(e: &env::Environment, cauldron: &str, mut body: Value, by: env::owner_acts::By) -> Result<Value> {
    let verb = match by {
        env::owner_acts::By::Tty => DECIDE,
        env::owner_acts::By::Agent => ADECIDE,
    };
    body["cauldron"] = Value::String(cauldron.to_string());
    send_owner(e, verb, "the verb this cauldron decides records with", body, std::time::Duration::from_secs(5))
}

/// **One owner verb to the running `mcp` service** (`DECIDE`, `ADECIDE`,
/// `MERGE`, `AMERGE`), after the caller asked the owner acts' gate:
/// refused, with nothing sent, when the service is older than this command
/// line or does not say its version, and when it does not know `verb`.
/// `what` says what the verb is for, in the refusal. The answer is waited
/// for up to `wait`.
pub(crate) fn send_owner(e: &env::Environment, verb: &str, what: &str, body: Value, wait: std::time::Duration) -> Result<Value> {
    let found = match crate::mcp_door::find(e) {
        Ok(f) => f,
        Err(why) => bail!(
            "REFUSED: the `mcp` service writes the decision, and {}. Nothing was written. Start it: `{}`.",
            why.say(),
            crate::mcp_door::start_command(&e.name)
        ),
    };
    let here = crate::mcp_door::CLI_VERSION;
    let service = crate::mcp_door::service_version(e, found.door);
    match service.as_deref().map(|s| (s, version_parts(s))) {
        None => return Err(old_service(e, None, "the running `mcp` service did not say which version it is", false)),
        Some((s, None)) => return Err(old_service(e, Some(s), &format!("the running `mcp` service says it is version `{s}`, which is not a version this cauldron can compare"), false)),
        Some((s, Some(v))) if Some(v) < version_parts(here) => {
            return Err(old_service(e, Some(s), &format!("the running `mcp` service is cauldron {s}, older than this cauldron ({here})"), false))
        }
        Some(_) => {}
    }
    let key_path = soup_door::grants::store_dir(e).join("approve.secret");
    let key = std::fs::read_to_string(&key_path)
        .with_context(|| format!("reading the approval key {} (the `mcp` service mints it when it starts)", key_path.display()))?;
    let answer = crate::mcp_door::seam_ask_within(&found.seam, &format!("{verb} {} {body}", key.trim()), wait)?;
    if answer.starts_with("ERR unknown request") {
        return Err(old_service(e, service.as_deref(), &format!("the running `mcp` service does not know `{verb}`, {what} (it answered `{answer}`)"), true));
    }
    serde_json::from_str(&answer).with_context(|| format!("the mcp service answered `{answer}`"))
}

/// The `mcp` seam verb a decision made at a terminal goes on.
pub(crate) const DECIDE: &str = "DECIDE";

/// The `mcp` seam verb a decision made without a terminal goes on, where
/// the owner opened `approve` (`env::owner_acts`).
pub(crate) const ADECIDE: &str = "ADECIDE";

/// **`cauldron ripples approve | reject`**: the owner's decision on a
/// ripple's records, by dump, by run or by id, at a terminal.
pub(crate) fn decide(verb: &str, args: &[String]) -> Result<i32> {
    let reject = verb == "reject";
    let undo_flag = if reject { "--unreject" } else { "--unapprove" };
    let mut allow = vec!["--env", "--cauldron", "--dump", "--run", undo_flag];
    if reject {
        allow.push("--reason");
    }
    let a = parse(verb, args, &allow)?;
    let ways = usize::from(a.dump.is_some()) + usize::from(a.run.is_some()) + usize::from(!a.pos.is_empty());
    if ways != 1 {
        bail!(
            "`cauldron ripples {verb}` takes the records one way: --dump <DM-n>, --run <run>, or record ids \
             (`cauldron ripples review --env <env> --cauldron <name>` lists them)"
        );
    }
    let (Some(env_name), Some(cauldron)) = (a.env.as_deref(), a.cauldron.as_deref()) else {
        bail!(
            "REFUSED: `cauldron ripples {verb}` takes the environment and the cauldron only as flags: --env <env> --cauldron \
             <name>. Nothing was written."
        );
    };
    if let Some(d) = &a.dump {
        review::checked_dump(d)?;
    }
    if let Some(r) = &a.run {
        review::checked_run(r)?;
    }
    if a.reason.is_some() && a.undo {
        bail!("`--reason` goes with a rejection, not with --unreject");
    }
    if let Some(r) = &a.reason {
        let one = review::one_line(r);
        if one.is_empty() || one.chars().count() > 500 {
            bail!("`--reason` is one line of 1 to 500 characters");
        }
    }
    let e = crate::mcp::environment(Some(env_name))?;
    let action = match (reject, a.undo) {
        (false, false) => "approve",
        (false, true) => "unapprove",
        (true, false) => "reject",
        (true, true) => "unreject",
    };
    let sel = match (&a.dump, &a.run) {
        (Some(d), _) => format!("--dump {d}"),
        (_, Some(r)) => format!("--run {r}"),
        _ => a.pos.join(" "),
    };
    let mut command = format!("cauldron ripples {verb} {sel} --env {} --cauldron {cauldron}", e.name);
    if let Some(r) = &a.reason {
        command.push_str(&format!(" --reason {}", shell_quote(r)));
    }
    if a.undo {
        command.push(' ');
        command.push_str(undo_flag);
    }
    // Checked before anything is read or sent, so the command the refusal
    // names is one that would work.
    let allowed = crate::owner_acts::act(
        env::owner_acts::Act::Approve,
        &e,
        cauldron,
        &format!(
            "REFUSED: {} a ripple's records is the owner's act at a terminal. `cauldron ripples {verb}` runs only with its \
             input and output on an interactive terminal, unless the owner has let agents approve (owner_acts), so an \
             agent, a ripple or a script cannot {action} records by itself. Nothing was written. Ask the user to run, in a \
             terminal on this machine:\n    {command}",
            if reject { "rejecting" } else { "approving" }
        ),
    )?;
    let mut body = serde_json::json!({"action": action, "from": format!("cauldron ripples {verb}")});
    if let Some(d) = &a.dump {
        body["dump"] = d.clone().into();
    } else if let Some(r) = &a.run {
        body["run"] = r.clone().into();
    } else {
        body["ids"] = serde_json::json!(a.pos);
    }
    if let Some(r) = &a.reason {
        body["reason"] = r.clone().into();
    }
    let v = send(&e, cauldron, body, allowed.by())?;
    print(&v)?;
    if v["ok"] == serde_json::json!(true) {
        let done = v[match action {
            "approve" => "approved",
            "unapprove" => "unapproved",
            "reject" => "rejected",
            _ => "unrejected",
        }]
        .as_array()
        .map_or(0, Vec::len);
        let skipped = v["skipped"].as_array().map_or(0, Vec::len);
        eprintln!(
            "cauldron ripples: {action}: {done} record(s) written, {skipped} skipped (each with why in `skipped`); by: {} ({}).",
            allowed.by(),
            allowed.said()
        );
        Ok(0)
    } else {
        eprintln!("cauldron ripples: refused ({}): {}", v["code"].as_str().unwrap_or("?"), v["message"].as_str().unwrap_or(""));
        Ok(1)
    }
}

/// `s` as one shell word.
fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b':' | b',')) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The review queue's size for one cauldron, for a status view: how many
/// records wait and the command that lists them; `None` when its soup
/// cannot be read.
pub(crate) fn waiting(e: &env::Environment, c: &env::Cauldron) -> Option<Value> {
    if !c.soup().join(spine::spine_db::SPINE_FILE).exists() {
        return None;
    }
    let t = soup_door::Target { env: e.clone(), cauldron: c.clone(), resolved_from: None };
    let n = review::waiting(&t).ok()?;
    let mut v = serde_json::json!({"cauldron": c.name, "unreviewed": n});
    if n > 0 {
        v["review"] = Value::String(format!("cauldron ripples review --env {} --cauldron {} --text", e.name, c.name));
    }
    Some(v)
}

/// [`waiting`] for every cauldron of `e` (or only `only`).
pub(crate) fn waiting_all(e: &env::Environment, only: Option<&str>) -> Vec<Value> {
    e.cauldrons().unwrap_or_default().iter().filter(|c| only.is_none_or(|o| c.name == o)).filter_map(|c| waiting(e, c)).collect()
}

#[cfg(test)]
mod tests {
    use super::version_parts;

    #[test]
    fn versions_compare_by_number_and_a_suffix_is_ignored() {
        assert_eq!(version_parts("0.1.12"), Some((0, 1, 12)));
        assert_eq!(version_parts("0.1.13-dev+abc"), Some((0, 1, 13)));
        assert!(version_parts("0.1.9") < version_parts("0.1.12"), "numbers, not text");
        for bad in ["", "test", "0.1", "0.1.x", "1.2.3.4"] {
            assert_eq!(version_parts(bad), None, "{bad}");
        }
    }
}
