//! **Which cauldron the CAULDRON face opens** (V1.2 Q-F1, V1.2c) — the one
//! rule, shared by the router (`cli/src/main.rs`, which must launch that
//! cauldron's own `tui-<env>` build) and the face (`apps/tui` `store.rs`
//! `default_env`, which opens it). Two copies drifted apart would have the
//! router launch one cauldron's build while the face opens another.

/// The cauldron that opens: a `named` one wins; else `last` (the one last
/// opened) while it is still in `there`; else `prod`, if it is there; else
/// the first of `there`. `None` only when nothing is named and `there` is
/// empty — the first-run screen. Pure: the callers read the record and list
/// the cauldrons; nothing is created or written by asking.
pub fn opens(named: Option<String>, last: Option<String>, there: &[String]) -> Option<String> {
    named
        .or_else(|| last.filter(|l| there.contains(l)))
        .or_else(|| there.iter().find(|n| *n == "prod").cloned())
        .or_else(|| there.first().cloned())
}

/// The flags of `tui --cauldron` (`apps/tui/src/cauldron/cli.rs` `parse`)
/// that TAKE A VALUE, `--env` apart. [`named_in`] skips each one's value so a
/// value (`--tab status`) is never read as a bare cauldron name. The face's
/// unit tests (`cauldron::cli::tests`) pin this list to its parser, flag by
/// flag, so the two cannot drift.
pub const VALUE_FLAGS: &[&str] = &[
    "--seeds", "--data", "--tab", "--cluster", "--cluster-tab", "--status-tab", "--config-tab",
    "--modal", "--new-name", "--search", "--region", "--node", "--item", "--draft", "--note",
    "--quote", "--paste", "--thread", "--agent", "--input", "--settings", "--parent",
    "--boundaries", "--bus", "--keys", "--scroll", "--size",
    // MERGE-V1i: REL-6's `--settle <ms>` (a dump's wait for the daemon) takes
    // a value too, so the router never reads its millis as a cauldron name.
    "--settle",
];

/// **The cauldron the face's own arguments name** (V1.2d), read the way the
/// face reads them: `--env <name>` (the last one wins), or the first bare
/// positional name while no `--env` came before it (`tui prod`). `Ok(None)`:
/// nothing named. `Err`: arguments the face itself refuses — `--env` or a
/// value flag with no value, or a second bare name — so no name can be
/// trusted from them. The router (`cli/src/main.rs`) asks this so it launches
/// the build of the cauldron the face will actually open.
pub fn named_in(args: &[String]) -> Result<Option<String>, String> {
    let mut named: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let a = a.as_str();
        if a == "--env" {
            named = Some(it.next().ok_or("--env needs a value")?.clone());
        } else if VALUE_FLAGS.contains(&a) {
            it.next().ok_or_else(|| format!("{a} needs a value"))?;
        } else if !a.starts_with('-') {
            if named.is_some() {
                return Err(format!("unknown argument `{a}`"));
            }
            named = Some(a.to_string());
        }
    }
    Ok(named)
}

#[cfg(test)]
mod tests {
    use super::{named_in, opens};

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }
    fn there(v: &[&str]) -> Vec<String> {
        v.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn a_named_cauldron_wins_over_the_record_and_prod() {
        assert_eq!(opens(s("beta"), s("alpha"), &there(&["alpha", "beta", "prod"])), s("beta"));
        // Named wins even when it is not (yet) there: the caller decides what that means.
        assert_eq!(opens(s("gamma"), s("alpha"), &there(&["alpha", "prod"])), s("gamma"));
    }

    #[test]
    fn unnamed_the_last_opened_wins_while_it_exists() {
        assert_eq!(opens(None, s("alpha"), &there(&["alpha", "beta", "prod"])), s("alpha"));
    }

    #[test]
    fn a_record_of_a_gone_cauldron_is_passed_over_for_prod() {
        assert_eq!(opens(None, s("gone"), &there(&["alpha", "prod"])), s("prod"));
        assert_eq!(opens(None, None, &there(&["alpha", "prod"])), s("prod"));
    }

    #[test]
    fn with_no_prod_the_first_there_opens() {
        assert_eq!(opens(None, s("gone"), &there(&["alpha", "beta"])), s("alpha"));
        assert_eq!(opens(None, None, &there(&["beta", "alpha"])), s("beta"));
    }

    #[test]
    fn with_nothing_there_and_nothing_named_it_is_the_first_run_screen() {
        assert_eq!(opens(None, s("alpha"), &[]), None);
        assert_eq!(opens(None, None, &[]), None);
    }

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn named_in_reads_a_bare_name_and_env_as_the_face_does() {
        assert_eq!(named_in(&v(&[])), Ok(None));
        assert_eq!(named_in(&v(&["--dump", "--tag"])), Ok(None));
        assert_eq!(named_in(&v(&["prod"])), Ok(s("prod")));
        assert_eq!(named_in(&v(&["--dump", "alpha", "--tab", "status"])), Ok(s("alpha")));
        assert_eq!(named_in(&v(&["--env", "beta"])), Ok(s("beta")));
        // A later --env overrides a bare name, as the face's loop does.
        assert_eq!(named_in(&v(&["alpha", "--env", "beta"])), Ok(s("beta")));
    }

    #[test]
    fn named_in_never_takes_a_flags_value_for_a_name() {
        assert_eq!(named_in(&v(&["--tab", "status"])), Ok(None));
        assert_eq!(named_in(&v(&["--draft", "--env"])), Ok(None));
        assert_eq!(named_in(&v(&["--size", "80x24", "prod"])), Ok(s("prod")));
    }

    #[test]
    fn named_in_refuses_what_the_face_refuses() {
        assert!(named_in(&v(&["--env"])).is_err());
        assert!(named_in(&v(&["--tab"])).is_err());
        assert!(named_in(&v(&["alpha", "beta"])).is_err());
        assert!(named_in(&v(&["--env", "alpha", "beta"])).is_err());
    }
}
