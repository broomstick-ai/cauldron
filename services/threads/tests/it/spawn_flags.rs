//! Spawn recipe tests (ported from CCW's `spawn_flags.rs`): the canonical
//! streaming argv, session-id vs resume, per-turn plugin dirs, env recipe.

use threads::settings::{ThreadSettings, ToolConfig};
use threads::spawn::{SpawnConfig, SpawnLogin};

fn base_settings() -> ThreadSettings {
    let mut s = ThreadSettings::new("/work/project", "sonnet");
    s.add_dirs = vec!["/minds".into()];
    s.append_system_prompt = Some("role prompt".into());
    s
}

#[test]
fn first_turn_uses_session_id_then_resume() {
    let cfg = SpawnConfig::from_settings("uuid-1", &base_settings(), vec![]);
    let first = cfg.build_args(true);
    let later = cfg.build_args(false);

    // The invariant prefix (AUI/CCW-identical streaming recipe).
    for want in ["-p", "--output-format", "stream-json", "--verbose"] {
        assert!(first.contains(&want.to_string()), "missing {want}");
    }
    // Default posture is dangerously-skip-permissions (AUI's default).
    assert!(first.contains(&"--dangerously-skip-permissions".to_string()));

    let sid = first.iter().position(|a| a == "--session-id").unwrap();
    assert_eq!(first[sid + 1], "uuid-1");
    assert!(!first.contains(&"--resume".to_string()));

    let res = later.iter().position(|a| a == "--resume").unwrap();
    assert_eq!(later[res + 1], "uuid-1");
    assert!(!later.contains(&"--session-id".to_string()));
}

#[test]
fn plugin_dirs_are_per_turn_arguments() {
    // Q9 hot reload: the resolved dir list is handed in per spawn, not frozen.
    let cfg = SpawnConfig::from_settings(
        "uuid-2",
        &base_settings(),
        vec!["/plugins/core".into(), "/proj/.mind/plugins/x".into()],
    );
    let args = cfg.build_args(true);
    let idxs: Vec<usize> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| *a == "--plugin-dir")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(idxs.len(), 2);
    assert_eq!(args[idxs[0] + 1], "/plugins/core");
    assert_eq!(args[idxs[1] + 1], "/proj/.mind/plugins/x");
}

#[test]
fn permission_mode_and_tool_fences() {
    let mut s = base_settings();
    s.permission_mode = Some("plan".into());
    s.tools = ToolConfig {
        clean_slate: true,
        tools: Some("ignored-when-clean-slate".into()),
        agents_json: Some("{\"a\":{}}".into()),
        allowed_tools: vec!["Bash(git *)".into()],
        disallowed_tools: vec!["WebFetch".into()],
    };
    let args = SpawnConfig::from_settings("uuid-3", &s, vec![]).build_args(true);

    let pm = args.iter().position(|a| a == "--permission-mode").unwrap();
    assert_eq!(args[pm + 1], "plan");
    assert!(!args.contains(&"--dangerously-skip-permissions".to_string()));

    // Clean slate: `--tools ""` and the explicit set is ignored.
    let t = args.iter().position(|a| a == "--tools").unwrap();
    assert_eq!(args[t + 1], "");
    let at = args.iter().position(|a| a == "--allowedTools").unwrap();
    assert_eq!(args[at + 1], "Bash(git *)");
    let dt = args.iter().position(|a| a == "--disallowedTools").unwrap();
    assert_eq!(args[dt + 1], "WebFetch");
}

#[test]
fn env_recipe_strips_key_and_keeps_subagents_alive() {
    let cfg = SpawnConfig::from_settings("uuid-4", &base_settings(), vec![]);
    let env = cfg.env(&SpawnLogin::pool_account("/accounts/work").unwrap());
    let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
    assert_eq!(get("ANTHROPIC_API_KEY"), Some(None)); // removed
    assert_eq!(get("CLAUDE_CODE_DISABLE_AUTO_MEMORY"), Some(Some("1".into())));
    assert_eq!(
        get("CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS"),
        Some(Some("0".into()))
    );
    assert_eq!(
        get("CLAUDE_CONFIG_DIR"),
        Some(Some("/accounts/work".into()))
    );
    // The machine default account clears the var — and it now has to be ASKED
    // FOR BY NAME. The old signature took `Option<&str>`, so this arm was also
    // what every FAILURE to resolve a login directory produced; the two
    // meanings are different values of a type now (see `SpawnLogin`, and
    // `docs/spawn-config-dir-guard.md`).
    let env = cfg.env(&SpawnLogin::machine_default());
    let cd = env.iter().find(|(k, _)| k == "CLAUDE_CONFIG_DIR").unwrap();
    assert_eq!(cd.1, None);
}

#[test]
fn model_is_verbatim_and_changeable() {
    // Q10: the model comes from live settings at each spawn; aliases float.
    let mut s = base_settings();
    s.model = "fable".into();
    let args = SpawnConfig::from_settings("uuid-5", &s, vec![]).build_args(false);
    let m = args.iter().position(|a| a == "--model").unwrap();
    assert_eq!(args[m + 1], "fable");
}
