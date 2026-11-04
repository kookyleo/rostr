//! Local model discovery. Each agent adapter reads only local files and
//! environment variables: no process runs, no network request, no inference.

mod claude;
pub(crate) use claude::resolve as resolve_claude_model;
mod codex;
mod gemini;
mod grok;
mod opencode;
mod pi;

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use crate::model_id;

/// The environment that adapters inspect. Tests build one by hand.
#[derive(Debug, Clone)]
pub struct Env {
    pub home: PathBuf,
    pub vars: HashMap<String, String>,
}

impl Env {
    pub fn from_process() -> Option<Env> {
        let vars: HashMap<String, String> = std::env::vars().collect();
        let home = vars
            .get("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)?;
        Some(Env { home, vars })
    }

    pub fn var(&self, name: &str) -> Option<&str> {
        self.vars
            .get(name)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    /// A directory from an environment variable, else a path under the home directory.
    pub fn dir(&self, var: &str, fallback: &str) -> PathBuf {
        self.var(var)
            .map(PathBuf::from)
            .unwrap_or_else(|| self.home.join(fallback))
    }

    pub fn xdg_config(&self) -> PathBuf {
        self.dir("XDG_CONFIG_HOME", ".config")
    }

    /// Finds an executable on `PATH` without running it.
    pub fn which(&self, name: &str) -> Option<PathBuf> {
        let path = self.var("PATH")?;
        std::env::split_paths(path)
            .map(|d| d.join(name))
            .find(|p| is_executable(p))
    }
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// One model that an agent can use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Canonical `<provider>/<model>` ID.
    pub id: String,
    /// Readable name from the agent's own data, if it has one.
    pub name: Option<String>,
    /// Why the model was found, for example `default model` or `model list`.
    pub via: &'static str,
}

impl Found {
    pub fn new(id: String, name: Option<String>, via: &'static str) -> Found {
        Found { id, name, via }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AgentReport {
    pub name: &'static str,
    pub installed: bool,
    /// A local credential file exists. Rostr checks only that it exists; it never reads it.
    pub signed_in: bool,
    pub models: Vec<Found>,
    /// Problems with files that exist but cannot be parsed.
    pub warnings: Vec<String>,
}

/// An agent adapter knows where one agent lives and how its configuration names models.
pub trait Adapter {
    fn name(&self) -> &'static str;
    fn scan(&self, env: &Env) -> AgentReport;
}

pub fn adapters() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(claude::ClaudeCode),
        Box::new(codex::Codex),
        Box::new(opencode::OpenCode),
        Box::new(grok::Grok),
        Box::new(gemini::Gemini),
        Box::new(pi::Pi),
    ]
}

/// Where a model was found: which agent, and why.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Source {
    pub agent: String,
    pub via: String,
}

impl Source {
    pub fn new(agent: &str, via: &str) -> Source {
        Source {
            agent: agent.into(),
            via: via.into(),
        }
    }
}

/// Distinct agent names, in order of first appearance, separated by commas:
/// `Claude Code, OpenCode`.
pub fn agent_list(sources: &[Source]) -> String {
    group_by_agent(sources)
        .into_iter()
        .map(|(agent, _)| agent)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Each agent with its reasons, separated by commas:
/// `Claude Code (built-in alias, default model), OpenCode (default model)`.
pub fn source_details(sources: &[Source]) -> String {
    group_by_agent(sources)
        .into_iter()
        .map(|(agent, via)| format!("{agent} ({})", via.join(", ")))
        .collect::<Vec<_>>()
        .join(", ")
}

fn group_by_agent(sources: &[Source]) -> Vec<(&str, Vec<&str>)> {
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for s in sources {
        match groups.iter_mut().find(|(a, _)| *a == s.agent) {
            Some((_, via)) if !via.contains(&s.via.as_str()) => via.push(&s.via),
            Some(_) => {}
            None => groups.push((&s.agent, vec![&s.via])),
        }
    }
    groups
}

/// A discovered model, merged across agents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub name: String,
    pub sources: Vec<Source>,
}

#[derive(Debug, Default)]
pub struct Discovery {
    pub agents: Vec<AgentReport>,
    pub candidates: Vec<Candidate>,
}

pub fn discover(env: &Env) -> Discovery {
    discover_with(env, &adapters())
}

pub fn discover_with(env: &Env, adapters: &[Box<dyn Adapter>]) -> Discovery {
    let mut by_id: BTreeMap<String, Candidate> = BTreeMap::new();
    let mut agents = Vec::new();
    for adapter in adapters {
        let mut report = adapter.scan(env);
        report.name = adapter.name();
        for f in &report.models {
            let id = model_id::normalize(&f.id);
            if model_id::validate_id(&id).is_err() {
                report
                    .warnings
                    .push(format!("skipped unrecognized model `{}`", f.id));
                continue;
            }
            let c = by_id.entry(id.clone()).or_insert_with(|| Candidate {
                name: model_id::display_name(&id),
                id,
                sources: Vec::new(),
            });
            if let Some(name) = f.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                c.name = name.to_string();
            }
            let source = Source::new(report.name, f.via);
            if !c.sources.contains(&source) {
                c.sources.push(source);
            }
        }
        agents.push(report);
    }
    Discovery {
        agents,
        candidates: by_id.into_values().collect(),
    }
}

// ---- helpers shared by adapters -------------------------------------------

/// Reads a file. `Ok(None)` when it does not exist.
fn read(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn read_json(path: &Path, report: &mut AgentReport) -> Option<serde_json::Value> {
    read_parsed(path, report, |s| {
        serde_json::from_str(s).map_err(|e| e.to_string())
    })
}

fn read_toml(path: &Path, report: &mut AgentReport) -> Option<toml::Table> {
    read_parsed(path, report, |s| {
        s.parse::<toml::Table>().map_err(|e| e.to_string())
    })
}

fn read_parsed<T>(
    path: &Path,
    report: &mut AgentReport,
    parse: impl FnOnce(&str) -> Result<T, String>,
) -> Option<T> {
    match read(path) {
        Ok(Some(text)) => match parse(&text) {
            Ok(v) => Some(v),
            Err(e) => {
                report
                    .warnings
                    .push(format!("cannot parse {}: {e}", path.display()));
                None
            }
        },
        Ok(None) => None,
        Err(e) => {
            report.warnings.push(e);
            None
        }
    }
}

/// Removes `//` and `/* */` comments and trailing commas from JSONC text.
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_str = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    // Drop trailing commas before `}` or `]`.
    let mut cleaned = String::with_capacity(out.len());
    let bytes: Vec<char> = out.chars().collect();
    let mut in_str = false;
    for (i, &c) in bytes.iter().enumerate() {
        if in_str {
            if c == '"' && bytes[..i].iter().rev().take_while(|&&b| b == '\\').count() % 2 == 0 {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
        } else if c == ',' {
            let next = bytes[i + 1..].iter().find(|c| !c.is_whitespace());
            if matches!(next, Some('}') | Some(']')) {
                continue;
            }
        }
        cleaned.push(c);
    }
    cleaned
}

fn json_str<'a>(v: &'a serde_json::Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = v;
    for key in path {
        cur = cur.get(key)?;
    }
    cur.as_str().map(str::trim).filter(|s| !s.is_empty())
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    pub struct Fixture {
        pub dir: tempfile::TempDir,
    }

    impl Fixture {
        pub fn new() -> Fixture {
            Fixture {
                dir: tempfile::tempdir().unwrap(),
            }
        }

        pub fn write(&self, rel: &str, text: &str) {
            let p = self.dir.path().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }

        pub fn env(&self) -> Env {
            Env {
                home: self.dir.path().to_path_buf(),
                vars: HashMap::new(),
            }
        }
    }

    pub fn ids(report: &AgentReport) -> Vec<&str> {
        report.models.iter().map(|f| f.id.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc() {
        let text = "{\n // c\n \"a\": \"http://x\", /* b */ \"b\": [1,2,],\n}";
        let v: serde_json::Value = serde_json::from_str(&strip_jsonc(text)).unwrap();
        assert_eq!(v["a"], "http://x");
        assert_eq!(v["b"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn source_text() {
        let sources = [
            Source::new("Claude Code", "built-in alias"),
            Source::new("OpenCode", "default model"),
            Source::new("Claude Code", "default model"),
        ];
        assert_eq!(agent_list(&sources), "Claude Code, OpenCode");
        assert_eq!(
            source_details(&sources),
            "Claude Code (built-in alias, default model), OpenCode (default model)"
        );
    }

    #[test]
    fn merges_across_agents() {
        let fx = testutil::Fixture::new();
        fx.write(".claude/settings.json", r#"{"model": "opus"}"#);
        fx.write(".codex/config.toml", "model = \"gpt-5.3-codex\"\n");
        fx.write(
            ".config/opencode/opencode.json",
            r#"{"model": "anthropic/claude-opus-5-5"}"#,
        );
        let d = discover(&fx.env());
        let opus = d
            .candidates
            .iter()
            .find(|c| c.id == "anthropic/claude-opus-5-5")
            .unwrap();
        assert_eq!(opus.sources.len(), 2 + 1, "{:?}", opus.sources);
        assert!(d.candidates.iter().any(|c| c.id == "openai/gpt-5.3-codex"));
        assert!(d.agents.iter().all(|a| a.warnings.is_empty()));
    }
}
