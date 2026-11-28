//! **The owner acts' one gate, on the command line**: `cauldron mcp grant`,
//! `cauldron soup approve`, `cauldron ripples approve | reject` and
//! `cauldron ripples on` each call [`act`] and nothing else to learn whether
//! they may run and who is acting (`env::owner_acts`); `cauldron merge` asks
//! the same decision for the act `merge` through `soup_door::merge::merger`,
//! because only its plan knows whether it carries a decision.

use anyhow::{bail, Result};
use env::owner_acts::{self, Act, Allowed};
use env::Environment;

/// Is this command's input and output an interactive terminal?
pub(crate) fn at_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// **May `act` run on `cauldron` of `e`, from this process?** At a terminal
/// always (`tty`); without one when the owner allowed it (`agent`: the
/// environment variable, or the setting). Refused otherwise, with
/// `refusal` (the act's own words, naming the command to run at a terminal)
/// and the sentence that says how the owner allows it.
pub(crate) fn act(act: Act, e: &Environment, cauldron: &str, refusal: &str) -> Result<Allowed> {
    let at = at_terminal();
    if at {
        return Ok(Allowed::Terminal);
    }
    let setting = owner_acts::load(&e.root).map_err(|err| anyhow::anyhow!("{refusal}\nThe owner_acts setting could not be read: {err}"))?;
    let id = e.find_cauldron(cauldron).ok().flatten().and_then(|c| c.id());
    let var = std::env::var(owner_acts::VAR).ok();
    match owner_acts::decide(act, &setting, &e.name, cauldron, id.as_deref(), at, var.as_deref()) {
        // Approving and rejecting are carried out by the mcp service, which
        // takes an agent's decision only where the PERSISTED setting opens
        // `approve` (it refuses `ADECIDE` otherwise): the variable alone
        // lets this command ask, and the service would refuse. Said here,
        // before anything is sent.
        Ok(Allowed::Var) if act == Act::Approve && !setting.effective(&e.name, cauldron, id.as_deref()).open.contains(&Act::Approve) => bail!(
            "{refusal}\n{var_name} opens `approve` for this command, but approving and rejecting are carried out by the `mcp` \
             service, which takes an agent's decision only where the owner's persisted owner_acts setting opens `approve`, and \
             for cauldron `{cauldron}` it does not. Nothing was sent. Record the choice once (the variable lets this run \
             without a terminal): `{var_name}=approve cauldron owner-acts open --only approve --cauldron {cauldron} --env {env}` \
             (without --cauldron, for every cauldron).",
            var_name = owner_acts::VAR,
            env = e.name
        ),
        Ok(a) => Ok(a),
        Err(r) => {
            let bad = r.bad_var.map(|b| format!("\n{b}; it opens nothing.")).unwrap_or_default();
            bail!("{refusal}{bad}\n{}", r.how_to_allow)
        }
    }
}

// ------------------------------------------------------- cauldron owner-acts

const USAGE: &str = "\
cauldron owner-acts — who may act for the owner without a terminal

    cauldron owner-acts open [--only grant,approve,ripples,merge,grammar] [--cauldron <name>] [--env <env>]
    cauldron owner-acts terminal [--only grant,approve,ripples,merge,grammar] [--cauldron <name>] [--env <env>]
        Choose which owner acts may run without an interactive terminal: `grant`
        (`cauldron mcp grant`), `approve` (`cauldron soup approve`, `cauldron ripples
        approve | reject`, and the MCP tools `soup_approve` and `soup_reject`), `ripples`
        (`cauldron ripples on`), `merge` (`cauldron merge` carrying the source's
        approvals and rejections; undoing the owner's merge, and `cauldron soup recover
        --abandon-undo | --abandon-merge | --settle-merge` on the owner's merge or undo) and
        `grammar` (`cauldron soup grammar add`, which registers a user grammar as a new pool,
        and `grammar rebind | remove --yes`). `open` opens them (all five, or those --only names,
        added to what is open already; a choice recorded before an act existed lists the acts
        it opened, so a newer act stays at a terminal until it is opened); `terminal` closes them again (all, or those
        --only names). Without --cauldron the choice is the root's, for every cauldron of
        every environment of this root; with --cauldron it is that cauldron's own and wins
        over the root's (both ways), bound to the cauldron's id (a cauldron made again
        under the name does not carry it). A person at a terminal may always do every act.
        Opening needs a terminal, or CAULDRON_OWNER_ACTS in this command's environment
        opening each act it opens; closing runs from anywhere. Recorded with when and how
        it was set, by the command itself: `tty` (run with a terminal attached), `env` (run
        without one, under the variable) or `install` (`cauldron install --owner-acts`, which
        scripts/install runs). JSON out.
    cauldron owner-acts unset --cauldron <name> [--env <env>]
        Drop that cauldron's own choice, so the root's applies to it again.
    cauldron owner-acts show [--env <env>]
        The root's choice, the variable as this process sees it, and per cauldron of the
        environment the effective setting per act and where it came from. JSON; writes
        nothing.
    cauldron owner-acts help

  The default is `terminal`: with nothing chosen, every act refuses without a terminal,
  as in every release before this setting. CAULDRON_OWNER_ACTS=open (or a comma list of
  acts) in a process's environment opens those acts for that process, whatever the
  setting says. Anything that can set its own environment, an agent told to included,
  can turn this on: that is the point, and the owner decides whether their agents may
  act for them. Only the cauldron command that carries it reads it: no service inherits
  it, and the mcp service, which carries out every approval and rejection (the commands'
  and the MCP tools soup_approve and soup_reject), takes an agent's decision only where
  the persisted setting opens `approve` (`show` says what sessions may do apart from this
  shell). The terminal check stops an agent that follows the rules; it is not a security
  boundary against a process of this user that fakes a terminal (script(1) does) or
  reads this user's files: `tty` means run with a terminal attached. What `open` gives up: an agent can approve what a ripple wrote, open a
  cauldron to every Claude Code session, and start automatic model spend. A ripple never
  approves or rejects, whatever the setting. Every act records who did it (`by: tty` or
  `by: agent`). The setting lives in <root>/owner-acts.json (owner-only).
";

#[derive(Default)]
struct Args {
    only: Option<String>,
    cauldron: Option<String>,
    env: Option<String>,
    pos: Vec<String>,
}

fn parse(verb: &str, args: &[String], allow: &[&str]) -> Result<Args> {
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
            bail!("unknown option `{s}` for `cauldron owner-acts {verb}` (`cauldron owner-acts help`)");
        }
        let v = match inline {
            Some(v) => v,
            None => match it.next() {
                Some(v) => v.clone(),
                None => bail!("`cauldron owner-acts {verb} {flag}` needs a value"),
            },
        };
        match flag {
            "--only" => a.only = Some(v),
            "--cauldron" => a.cauldron = Some(v),
            _ => a.env = Some(v),
        }
    }
    if let Some(p) = a.pos.first() {
        bail!("`cauldron owner-acts {verb}` takes no arguments (got `{p}`); `cauldron owner-acts help`");
    }
    Ok(a)
}

/// `cauldron owner-acts <verb> …`.
pub(crate) fn verb_owner_acts(args: &[String]) -> Result<i32> {
    match args.first().map(String::as_str) {
        Some("help") | Some("-h") | Some("--help") => {
            print!("{USAGE}");
            Ok(0)
        }
        Some(v @ ("open" | "terminal")) => set(v == "open", &args[1..]),
        Some("unset") => unset(&args[1..]),
        Some("show") => show(&args[1..]),
        None => {
            eprint!("{USAGE}");
            Ok(2)
        }
        Some(other) => {
            eprintln!("cauldron owner-acts: no verb `{other}` (open | terminal | unset | show | help)");
            Ok(2)
        }
    }
}

/// What `open` gives up, said with every answer that opens something.
const GIVES_UP: &str = "Open acts run without a terminal: an agent, a script or CI can then do them for the owner. `approve` \
                        lets an agent approve or reject what a ripple wrote (approved records then read as reviewed); \
                        `grant` lets it open a cauldron to every Claude Code session of this user; `ripples` lets it start \
                        automatic model spend and writes; `merge` lets it carry the source's approvals and rejections into another \
                        cauldron with a merge; `grammar` lets it register, rebind or remove a user grammar, which changes what a \
                        cauldron's soup accepts. Each act records `by: \"agent\"`. A ripple never approves or rejects.";

/// The environment for `--cauldron`, and that cauldron (it must exist).
fn the_cauldron(env_flag: Option<&str>, name: &str) -> Result<(Environment, env::Cauldron)> {
    let e = crate::mcp::environment(env_flag)?;
    let Some(c) = e.find_cauldron(name)? else {
        bail!("REFUSED: there is no cauldron `{name}` in environment `{}` (`cauldron list --env {}`). Nothing was changed.", e.name, e.name);
    };
    Ok((e, c))
}

fn set(open: bool, args: &[String]) -> Result<i32> {
    let verb = if open { "open" } else { "terminal" };
    if args.iter().any(|x| x == "--how" || x.starts_with("--how=")) {
        bail!(
            "`--how` is not an option of `cauldron owner-acts {verb}`: how a choice was set is recorded by the command that set \
             it (`tty` or `env` here; `install` only by `cauldron install --owner-acts`, which scripts/install runs). Nothing was \
             changed."
        );
    }
    set_as(open, args, None)
}

/// [`set`], with `how` recorded as given when the caller is a code path
/// that knows it (`cauldron install --owner-acts` passes
/// [`owner_acts::How::Install`]); else `tty` or `env` by what this process
/// has.
pub(crate) fn set_as(open: bool, args: &[String], how: Option<owner_acts::How>) -> Result<i32> {
    let verb = if open { "open" } else { "terminal" };
    let a = parse(verb, args, &["--only", "--cauldron", "--env"])?;
    let named = match &a.only {
        Some(o) => {
            let v = owner_acts::parse_acts(o).map_err(|e| anyhow::anyhow!("`--only`: {e}"))?;
            if v.is_empty() {
                bail!("`--only` names one or more of grant, approve, ripples, merge, grammar");
            }
            v
        }
        None => Act::ALL.to_vec(),
    };
    if a.env.is_some() && a.cauldron.is_none() {
        bail!("`--env` goes with `--cauldron`: without it the choice is the root's, for every environment of this root");
    }
    let root_env = Environment::resolve()?;
    let mut setting = owner_acts::load(&root_env.root)?;
    // The scope's list now, and after.
    let (scope_env, target) = match &a.cauldron {
        Some(name) => {
            let (e, c) = the_cauldron(a.env.as_deref(), name)?;
            (Some(e), Some(c))
        }
        None => (None, None),
    };
    let before: Vec<Act> = match (&scope_env, &target) {
        (Some(e), Some(c)) => setting.effective(&e.name, &c.name, c.id().as_deref()).open,
        _ => setting.root.as_ref().map(|r| r.acts()).unwrap_or_default(),
    };
    // A cauldron with no choice of its own starts from the root's: the acts
    // it inherited become its own, and stop following the root's. Said in
    // the answer, by name.
    let inherited: Vec<Act> = match (&scope_env, &target) {
        (Some(e), Some(c)) if setting.effective(&e.name, &c.name, c.id().as_deref()).source != owner_acts::Source::Cauldron => before.clone(),
        _ => vec![],
    };
    let mut after = before.clone();
    if open {
        after.extend(named.iter().copied());
    } else {
        after.retain(|x| !named.contains(x));
    }
    after.sort();
    after.dedup();
    let widened: Vec<Act> = after.iter().copied().filter(|x| !before.contains(x)).collect();
    // Opening needs a terminal, or the variable opening each act it opens.
    let at = at_terminal();
    let var = std::env::var(owner_acts::VAR).ok();
    let how = match how {
        Some(h) => h,
        None if at => owner_acts::How::Tty,
        None => owner_acts::How::Env,
    };
    if !widened.is_empty() && !at {
        let var_acts = owner_acts::from_var(var.as_deref());
        let missing: Vec<Act> = match &var_acts {
            Ok(v) => widened.iter().copied().filter(|x| !v.contains(x)).collect(),
            Err(_) => widened.clone(),
        };
        if !missing.is_empty() {
            let list = |v: &[Act]| v.iter().map(|x| x.as_str()).collect::<Vec<_>>().join(",");
            let mut cmd = format!("cauldron owner-acts open --only {}", list(&widened));
            if let (Some(e), Some(c)) = (&scope_env, &target) {
                cmd.push_str(&format!(" --cauldron {} --env {}", c.name, e.name));
            }
            let bad = match var_acts {
                Err(e) => format!(" ({e})"),
                Ok(_) => String::new(),
            };
            bail!(
                "REFUSED: letting agents act for the owner is the owner's choice. `cauldron owner-acts open` runs without \
                 an interactive terminal only when {} in its environment opens each act it opens{bad}, and it would open \
                 {} here. Nothing was changed. Ask the user to run, in a terminal on this machine:\n    {cmd}\nor to set {}={} \
                 in the environment of the process that runs it (a shell profile, your user settings for Claude Code but never a project's .claude/settings.json, CI, a server).",
                owner_acts::VAR,
                list(&missing),
                owner_acts::VAR,
                list(&widened),
            );
        }
    }
    let choice = owner_acts::Choice::new(&after, how);
    let (scope, store_env, store_cauldron) = match (&scope_env, &target) {
        (Some(e), Some(c)) => {
            // The cauldron's id, given now if it has none yet: the choice
            // is bound to it.
            let id = soup_door::grants::adopt(e, c)?.or_else(|| c.id());
            setting.set_cauldron(&e.name, &c.name, id, choice.clone());
            ("cauldron", Some(e.name.clone()), Some(c.name.clone()))
        }
        _ => {
            setting.set_root(choice.clone());
            ("root", None, None)
        }
    };
    owner_acts::store(&root_env.root, &setting)?;
    let mut doc = serde_json::json!({
        "outcome": "set",
        "scope": scope,
        "setting": owner_acts::words(&after),
        "open": after.iter().map(|x| x.as_str()).collect::<Vec<_>>(),
        "was": owner_acts::words(&before),
        "set_at": choice.set_at,
        "how": choice.how.as_str(),
        "store": owner_acts::path(&root_env.root).display().to_string(),
    });
    if let (Some(e), Some(c)) = (store_env, store_cauldron) {
        let kept: Vec<Act> = inherited.iter().copied().filter(|x| after.contains(x) && !named.contains(x)).collect();
        if !kept.is_empty() {
            doc["copied_from_root"] = owner_acts::words(&kept).into();
            doc["copied_note"] = format!(
                "`{c}` had no choice of its own, so it started from the root's; besides what --only named, it now holds {} as \
                 its own choice, and a later change of the root's choice no longer reaches it (`cauldron owner-acts unset \
                 --cauldron {c} --env {e}` hands it back to the root's)",
                owner_acts::words(&kept)
            )
            .into();
        }
        doc["env"] = e.into();
        doc["cauldron"] = c.into();
    }
    if !after.is_empty() {
        doc["note"] = GIVES_UP.into();
    }
    println!("{}", serde_json::to_string_pretty(&doc)?);
    Ok(0)
}

fn unset(args: &[String]) -> Result<i32> {
    let a = parse("unset", args, &["--cauldron", "--env"])?;
    let Some(name) = a.cauldron.as_deref() else {
        bail!("`cauldron owner-acts unset` takes --cauldron <name> (the root's choice is changed with `open` or `terminal`)");
    };
    let root_env = Environment::resolve()?;
    let e = crate::mcp::environment(a.env.as_deref())?;
    let mut setting = owner_acts::load(&root_env.root)?;
    let had = setting.unset_cauldron(&e.name, name);
    if had {
        owner_acts::store(&root_env.root, &setting)?;
    }
    let id = e.find_cauldron(name).ok().flatten().and_then(|c| c.id());
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "outcome": if had { "unset" } else { "not-set" },
            "env": e.name,
            "cauldron": name,
            "effective": owner_acts::view(&setting, &e.name, name, id.as_deref(), None),
        }))?
    );
    Ok(0)
}

/// The effective setting of every cauldron of `e`, for `show` and the
/// status views.
pub(crate) fn per_cauldron(e: &Environment) -> Vec<serde_json::Value> {
    let setting = owner_acts::load(&e.root).unwrap_or_default();
    let var = std::env::var(owner_acts::VAR).ok();
    e.cauldrons()
        .unwrap_or_default()
        .iter()
        .map(|c| {
            let mut v = owner_acts::view(&setting, &e.name, &c.name, c.id().as_deref(), var.as_deref());
            v["cauldron"] = c.name.clone().into();
            v
        })
        .collect()
}

/// One cauldron's effective setting, for a status view (`None` when the
/// setting cannot be read).
pub(crate) fn of_cauldron(e: &Environment, c: &env::Cauldron) -> serde_json::Value {
    match owner_acts::load(&e.root) {
        Ok(s) => owner_acts::view(&s, &e.name, &c.name, c.id().as_deref(), std::env::var(owner_acts::VAR).ok().as_deref()),
        Err(err) => serde_json::json!({"error": err.to_string()}),
    }
}

/// The root's choice, for a status view.
pub(crate) fn summary(e: &Environment) -> serde_json::Value {
    let var = std::env::var(owner_acts::VAR).ok();
    let mut v = match owner_acts::load(&e.root) {
        Ok(s) => match &s.root {
            Some(r) => serde_json::json!({"root": owner_acts::words(&r.acts()), "set_at": r.set_at, "how": r.how.as_str(),
                                          "cauldron_choices": s.cauldrons.len()}),
            None => serde_json::json!({"root": "terminal", "from": "default", "cauldron_choices": s.cauldrons.len()}),
        },
        Err(err) => serde_json::json!({"error": err.to_string()}),
    };
    match owner_acts::from_var(var.as_deref()) {
        Ok(a) if !a.is_empty() => {
            v["env_var"] = serde_json::json!({"name": owner_acts::VAR, "opens": owner_acts::words(&a),
                                              "for": "a cauldron command run from this process only; never an MCP session"})
        }
        Err(err) => v["env_var"] = serde_json::json!({"name": owner_acts::VAR, "error": err}),
        _ => {}
    }
    // What MCP sessions may do: the persisted root choice (a cauldron's own
    // choice may differ; each cauldron's `sessions` says).
    if let Ok(s) = owner_acts::load(&e.root) {
        let open = s.root.as_ref().is_some_and(|r| r.acts().contains(&Act::Approve));
        v["sessions"] = serde_json::json!({"approve": if open { "open" } else { "terminal" }, "note": owner_acts::SESSIONS_NOTE});
    }
    v["show"] = "cauldron owner-acts show".into();
    v
}

fn show(args: &[String]) -> Result<i32> {
    let a = parse("show", args, &["--env"])?;
    let root_env = Environment::resolve()?;
    let setting = owner_acts::load(&root_env.root)?;
    let e = match a.env.as_deref() {
        Some(n) => Some(crate::mcp::environment(Some(n))?),
        None => crate::mcp::environment(None).ok(),
    };
    let mut doc = serde_json::json!({
        "store": owner_acts::path(&root_env.root).display().to_string(),
        "summary": summary(&root_env),
        "cauldron_choices": setting.cauldrons,
    });
    if let Some(r) = &setting.root {
        doc["root_choice"] = serde_json::to_value(r)?;
    }
    if let Some(e) = &e {
        doc["env"] = e.name.clone().into();
        doc["cauldrons"] = per_cauldron(e).into();
    }
    doc["note"] = "`acts` says per act where it is open from for a cauldron command run from this process without a terminal \
                   (`cauldron`, `root` or `env`: the variable, for this process only) or `terminal` (refused without one). \
                   `sessions` says what MCP sessions may do through the door (soup_approve, soup_reject): the persisted \
                   setting alone, since CAULDRON_OWNER_ACTS never reaches the mcp service. A person at a terminal may always \
                   do every act."
        .into();
    println!("{}", serde_json::to_string_pretty(&doc)?);
    Ok(0)
}
