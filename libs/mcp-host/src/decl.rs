//! # The MCP tool declaration — `mcp/<name>.toml`
//!
//! One file per tool SERVER (a server exposes one or more tools), discovered by
//! filename exactly as `services/<name>.toml` is, and searched in the same two
//! places: the cauldron root's `mcp/` (the operator's overrides) and then the
//! `mcp/` directory of the repository this binary was built from — the
//! version-controlled one. The first directory that declares a name wins.
//!
//! ```toml
//! name        = "fetch"                 # must equal the file stem
//! description = "An external fetch tool"
//! residency   = "process"               # or "in-process"
//!
//! [process]                             # residency = "process" only
//! command = "bin/fetch-mcp"             # relative: resolved against THIS file's directory
//! args    = ["--stdio"]
//! [process.env]
//! FETCH_LIMIT = "10"
//!
//! [[tools]]                             # what tools/list shows WITHOUT waking it
//! name         = "fetch_url"
//! description  = "Fetch one URL"
//! input_schema = { type = "object", properties = { url = { type = "string" } }, required = ["url"] }
//! ```
//!
//! An `in-process` server names its `provider` (the service whose code serves
//! it) and carries no `[process]`. Its `[[tools]]` may be omitted: the provider
//! is the authority on its own surface.
//!
//! **Why `[[tools]]` is declared and not discovered.** Discovering a process
//! server's tools means waking it, and `tools/list` is asked at the start of
//! every turn. With at most one process awake, discovery would wake and kill
//! every declared server on every turn. Declaring the surface is also what
//! makes it version-controlled: the surface a thread is given is the one in
//! the repository, not whatever the binary says today.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

/// Where a declared server's code runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Residency {
    /// Ours: a library call inside a running service (`provider`). Never a
    /// process the supervisor wakes.
    InProcess,
    /// External: a stdio MCP server binary the supervisor wakes on demand.
    Process,
}

/// One tool, as `tools/list` will show it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// JSON Schema for the arguments. Absent means "an object".
    #[serde(default)]
    pub input_schema: Option<Value>,
}

impl ToolSpec {
    pub fn schema(&self) -> Value {
        self.input_schema.clone().unwrap_or_else(|| serde_json::json!({"type": "object"}))
    }
}

/// How to start an external server.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessSpec {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// One declared tool server.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDecl {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub residency: Residency,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub process: Option<ProcessSpec>,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    /// The file this was read from. Never in the TOML.
    #[serde(skip)]
    pub source: PathBuf,
}

impl ToolDecl {
    /// Parse one declaration. `source` is the file it came from: its stem must
    /// equal `name`, and a relative `command` is resolved against its parent.
    pub fn parse(text: &str, source: &Path) -> Result<Self> {
        let mut d: ToolDecl =
            toml::from_str(text).with_context(|| format!("parsing MCP tool declaration {}", source.display()))?;
        d.source = source.to_path_buf();
        d.validate()?;
        if let Some(p) = d.process.as_mut() {
            let cmd = Path::new(&p.command);
            if cmd.is_relative() && p.command.contains('/') {
                if let Some(dir) = source.parent() {
                    p.command = dir.join(cmd).display().to_string();
                }
            }
        }
        Ok(d)
    }

    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text, path)
    }

    fn validate(&self) -> Result<()> {
        let src = self.source.display();
        if self.name.trim().is_empty() {
            bail!("{src}: `name` is empty");
        }
        if let Some(stem) = self.source.file_stem().and_then(|s| s.to_str()) {
            if !stem.is_empty() && stem != self.name {
                bail!(
                    "{src}: declares `name = \"{}\"` but the file is `{stem}.toml`; a declaration is \
                     discovered by its filename, so the two must agree",
                    self.name
                );
            }
        }
        let mut seen = BTreeSet::new();
        for t in &self.tools {
            if t.name.trim().is_empty() {
                bail!("{src}: a [[tools]] entry has an empty `name`");
            }
            if !seen.insert(t.name.as_str()) {
                bail!("{src}: tool `{}` is declared twice", t.name);
            }
        }
        match self.residency {
            Residency::InProcess => {
                if self.provider.as_deref().map(str::trim).unwrap_or("").is_empty() {
                    bail!(
                        "{src}: `residency = \"in-process\"` needs `provider` — the service whose code \
                         serves these tools. An in-process tool with no provider is served by nobody"
                    );
                }
                if self.process.is_some() {
                    bail!(
                        "{src}: `residency = \"in-process\"` with a [process] section; an in-process \
                         tool is never a process the supervisor wakes. Pick one"
                    );
                }
            }
            Residency::Process => {
                let Some(p) = &self.process else {
                    bail!("{src}: `residency = \"process\"` needs a [process] section naming `command`");
                };
                if p.command.trim().is_empty() {
                    bail!("{src}: [process] `command` is empty");
                }
                if self.tools.is_empty() {
                    bail!(
                        "{src}: a process server must declare its [[tools]]: tools/list is answered \
                         from the declaration, without waking the process"
                    );
                }
            }
        }
        Ok(())
    }
}

/// `<root>/mcp`, then this repository's `mcp/` — the same two places
/// `env::decl::search_paths` looks for services, in the same order.
///
/// DEPTH-DEPENDENT, exactly as that function is: `nth(2)` is the workspace
/// root because this crate lives two directories down (`libs/mcp-host`).
pub fn search_paths(root: &Path) -> Vec<PathBuf> {
    vec![root.join("mcp"), shipped_dir()]
}

/// The version-controlled `mcp/` directory of the repository this binary was
/// built from.
pub fn shipped_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap_or(Path::new("/"))
        .join("mcp")
}

/// Every declaration in `dirs`, first directory winning per name. A missing
/// directory declares nothing; a malformed file is an error naming it.
pub fn load(dirs: &[PathBuf]) -> Result<Vec<ToolDecl>> {
    let mut out: BTreeMap<String, ToolDecl> = BTreeMap::new();
    for dir in dirs {
        let rd = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("listing {}", dir.display())),
        };
        let mut files: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("toml"))
            .collect();
        files.sort();
        for f in files {
            let d = ToolDecl::read(&f)?;
            out.entry(d.name.clone()).or_insert(d);
        }
    }
    let decls: Vec<ToolDecl> = out.into_values().collect();
    check_unique_tools(&decls)?;
    Ok(decls)
}

/// A tool name routes to exactly one server, or the routing is ambiguous.
pub fn check_unique_tools(decls: &[ToolDecl]) -> Result<()> {
    let mut owner: BTreeMap<&str, &str> = BTreeMap::new();
    for d in decls {
        for t in &d.tools {
            if let Some(prev) = owner.insert(t.name.as_str(), d.name.as_str()) {
                bail!(
                    "tool `{}` is declared by both `{prev}` and `{}`; a call must route to exactly one \
                     server",
                    t.name,
                    d.name
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(name: &str) -> PathBuf {
        PathBuf::from(format!("/repo/mcp/{name}.toml"))
    }

    #[test]
    fn a_process_declaration_parses_and_resolves_its_command_beside_itself() {
        let d = ToolDecl::parse(
            r#"
name = "fetch"
residency = "process"
[process]
command = "bin/fetch"
[[tools]]
name = "fetch_url"
input_schema = { type = "object", required = ["url"] }
"#,
            &at("fetch"),
        )
        .unwrap();
        assert_eq!(d.residency, Residency::Process);
        assert_eq!(d.process.unwrap().command, "/repo/mcp/bin/fetch");
        assert_eq!(d.tools[0].schema()["required"][0], "url");
    }

    #[test]
    fn a_bare_command_is_left_for_path_lookup() {
        let d = ToolDecl::parse(
            "name = \"x\"\nresidency = \"process\"\n[process]\ncommand = \"npx\"\n[[tools]]\nname = \"t\"\n",
            &at("x"),
        )
        .unwrap();
        assert_eq!(d.process.unwrap().command, "npx");
    }

    /// Each refusal paired with the nearest accepted form, so a validator that
    /// refuses everything fails the accepted half.
    #[test]
    fn the_validator_refuses_each_incoherent_form_and_accepts_its_coherent_twin() {
        let ok_in = "name = \"agents\"\nresidency = \"in-process\"\nprovider = \"threads\"\n";
        assert!(ToolDecl::parse(ok_in, &at("agents")).is_ok());
        // stem mismatch
        assert!(ToolDecl::parse(ok_in, &at("other")).is_err());
        // in-process with no provider
        assert!(ToolDecl::parse("name = \"a\"\nresidency = \"in-process\"\n", &at("a")).is_err());
        // in-process with a process
        assert!(ToolDecl::parse(
            "name = \"a\"\nresidency = \"in-process\"\nprovider = \"p\"\n[process]\ncommand = \"x\"\n",
            &at("a")
        )
        .is_err());
        // process with no tools
        assert!(ToolDecl::parse("name = \"a\"\nresidency = \"process\"\n[process]\ncommand = \"x\"\n", &at("a")).is_err());
        // unknown key
        assert!(ToolDecl::parse(&format!("{ok_in}bogus = 1\n"), &at("agents")).is_err());
        // duplicate tool
        assert!(ToolDecl::parse(
            "name = \"a\"\nresidency = \"process\"\n[process]\ncommand = \"x\"\n[[tools]]\nname = \"t\"\n[[tools]]\nname = \"t\"\n",
            &at("a")
        )
        .is_err());
    }

    #[test]
    fn the_first_directory_wins_and_a_tool_name_routes_to_one_server() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let body = |cmd: &str, tool: &str| {
            format!("name = \"s\"\nresidency = \"process\"\n[process]\ncommand = \"{cmd}\"\n[[tools]]\nname = \"{tool}\"\n")
        };
        std::fs::write(a.path().join("s.toml"), body("first", "t1")).unwrap();
        std::fs::write(b.path().join("s.toml"), body("second", "t1")).unwrap();
        let got = load(&[a.path().to_path_buf(), b.path().to_path_buf(), a.path().join("absent")]).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].process.as_ref().unwrap().command, "first");

        // Two servers claiming one tool name: refused.
        std::fs::write(
            b.path().join("u.toml"),
            "name = \"u\"\nresidency = \"process\"\n[process]\ncommand = \"c\"\n[[tools]]\nname = \"t1\"\n",
        )
        .unwrap();
        let err = load(&[a.path().to_path_buf(), b.path().to_path_buf()]).unwrap_err();
        assert!(format!("{err:#}").contains("declared by both"), "{err:#}");
    }

    /// The repository's own `mcp/` loads clean — the shipped declarations are
    /// held to the validator, not merely present.
    #[test]
    fn the_shipped_declarations_load() {
        let decls = load(&[shipped_dir()]).unwrap();
        let agents = decls.iter().find(|d| d.name == "agents").expect("mcp/agents.toml is shipped");
        assert_eq!(agents.residency, Residency::InProcess);
        assert_eq!(agents.provider.as_deref(), Some("threads"));
    }
}
