//! `scope.yaml`: the user's model scope. It is the only source of truth for user intent.
//!
//! `patterns` decide which models are selected. `exceptions` record the models
//! whose state differs from what the patterns say, or that the user pinned by hand.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::model_id;

pub const VERSION: u32 = 1;
pub const FILE_NAME: &str = "scope.yaml";

const HEADER: &str = "\
# Rostr model scope. `rostr discover` reads and updates this file; hand edits are safe.
# patterns:   a model is selected when a pattern matches its ID.
# exceptions: explicit choices that win over patterns (`selected: false` excludes a model).
";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub version: u32,
    #[serde(default)]
    pub patterns: Vec<String>,
    #[serde(default)]
    pub exceptions: Vec<Exception>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exception {
    pub id: String,
    pub selected: bool,
}

impl Default for Scope {
    fn default() -> Self {
        Scope {
            version: VERSION,
            patterns: Vec::new(),
            exceptions: Vec::new(),
        }
    }
}

/// Why a model is selected or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    Exception,
    Pattern(String),
    NoMatch,
}

impl Reason {
    pub fn describe(&self) -> String {
        match self {
            Reason::Exception => "exception".into(),
            Reason::Pattern(p) => format!("pattern {p}"),
            Reason::NoMatch => "no pattern matches".into(),
        }
    }
}

impl Scope {
    /// Normalizes IDs and patterns, removes duplicates and sorts exceptions by ID.
    /// For a duplicated exception, `selected: false` wins: an exclusion is never lost.
    pub fn normalized(&self) -> Scope {
        let mut patterns: Vec<String> = Vec::new();
        for p in &self.patterns {
            let p = model_id::normalize(p);
            if !patterns.contains(&p) {
                patterns.push(p);
            }
        }
        let mut exceptions: BTreeMap<String, bool> = BTreeMap::new();
        for e in &self.exceptions {
            let id = model_id::normalize(&e.id);
            exceptions
                .entry(id)
                .and_modify(|s| *s &= e.selected)
                .or_insert(e.selected);
        }
        Scope {
            version: self.version,
            patterns,
            exceptions: exceptions
                .into_iter()
                .map(|(id, selected)| Exception { id, selected })
                .collect(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != VERSION {
            bail!(
                "unsupported scope version {} (this rostr supports version {VERSION})",
                self.version
            );
        }
        for p in &self.patterns {
            model_id::validate_pattern(p)?;
        }
        let mut seen = std::collections::HashSet::new();
        for e in &self.exceptions {
            model_id::validate_id(&e.id)?;
            if !seen.insert(&e.id) {
                bail!("duplicate exception `{}`", e.id);
            }
        }
        Ok(())
    }
}

pub fn resolve_patterns(patterns: &[String], id: &str) -> (bool, Reason) {
    match patterns.iter().find(|p| model_id::wildcard_match(p, id)) {
        Some(p) => (true, Reason::Pattern(p.clone())),
        None => (false, Reason::NoMatch),
    }
}

/// `$ROSTR_CONFIG_DIR`, else `$XDG_CONFIG_HOME/rostr`, else `~/.config/rostr`.
pub fn config_dir(home: &Path, var: impl Fn(&str) -> Option<String>) -> PathBuf {
    xdg_dir(home, var, "ROSTR_CONFIG_DIR", "XDG_CONFIG_HOME", ".config")
}

/// `$ROSTR_DATA_DIR`, else `$XDG_DATA_HOME/rostr`, else `~/.local/share/rostr`.
pub fn data_dir(home: &Path, var: impl Fn(&str) -> Option<String>) -> PathBuf {
    xdg_dir(home, var, "ROSTR_DATA_DIR", "XDG_DATA_HOME", ".local/share")
}

fn xdg_dir(
    home: &Path,
    var: impl Fn(&str) -> Option<String>,
    own: &str,
    xdg: &str,
    fallback: &str,
) -> PathBuf {
    if let Some(dir) = var(own).filter(|s| !s.is_empty()) {
        return PathBuf::from(dir);
    }
    match var(xdg).filter(|s| !s.is_empty()) {
        Some(base) => PathBuf::from(base).join("rostr"),
        None => home.join(fallback).join("rostr"),
    }
}

/// Loads a scope file. `Ok(None)` when the file does not exist.
pub fn load(path: &Path) -> Result<Option<Scope>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let scope: Scope = serde_norway::from_str(&text)
        .with_context(|| format!("cannot parse {}", path.display()))?;
    let scope = scope.normalized();
    scope
        .validate()
        .with_context(|| format!("invalid {}", path.display()))?;
    Ok(Some(scope))
}

pub fn to_yaml(scope: &Scope) -> Result<String> {
    Ok(format!("{HEADER}{}", serde_norway::to_string(scope)?))
}

/// Validates the scope, then writes it through a temporary file and an atomic rename.
pub fn save(path: &Path, scope: &Scope) -> Result<()> {
    let scope = scope.normalized();
    scope.validate()?;
    let yaml = to_yaml(&scope)?;
    // Round-trip check: what we write must load back to the same scope.
    let back: Scope = serde_norway::from_str(&yaml)?;
    if back != scope {
        bail!("internal error: scope does not round-trip through YAML");
    }
    let dir = path
        .parent()
        .context("scope path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".scope.")
        .suffix(".tmp")
        .tempfile_in(dir)
        .with_context(|| format!("cannot create a temporary file in {}", dir.display()))?;
    tmp.write_all(yaml.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)
        .with_context(|| format!("cannot replace {}", path.display()))?;
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exc(id: &str, selected: bool) -> Exception {
        Exception {
            id: id.into(),
            selected,
        }
    }

    #[test]
    fn normalize_dedups_and_keeps_exclusion() {
        let s = Scope {
            version: 1,
            patterns: vec!["Anthropic/*".into(), "anthropic/*".into()],
            exceptions: vec![exc("b/x", true), exc("A/Y", true), exc("b/x", false)],
        }
        .normalized();
        assert_eq!(s.patterns, vec!["anthropic/*"]);
        assert_eq!(s.exceptions, vec![exc("a/y", true), exc("b/x", false)]);
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join(FILE_NAME);
        let s = Scope {
            version: 1,
            patterns: vec!["openai/*codex*".into()],
            exceptions: vec![exc("anthropic/claude-haiku-4-5", false)],
        };
        save(&path, &s).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Rostr"));
        assert_eq!(load(&path).unwrap().unwrap(), s.normalized());
        // No temporary files stay behind.
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn rejects_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        fs::write(&path, "version: 2\n").unwrap();
        assert!(load(&path).is_err());
        fs::write(
            &path,
            "version: 1\nexceptions:\n  - id: nope\n    selected: true\n",
        )
        .unwrap();
        assert!(load(&path).is_err());
        fs::write(&path, "version: 1\npaterns: []\n").unwrap();
        assert!(load(&path).is_err());
        let bad = Scope {
            exceptions: vec![exc("x", true)],
            ..Scope::default()
        };
        assert!(save(&path, &bad).is_err());
        // A failed save leaves the old file untouched.
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "version: 1\npaterns: []\n"
        );
    }

    #[test]
    fn hand_written_yaml_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        fs::write(
            &path,
            "version: 1\npatterns:\n  - \"anthropic/*\"\nexceptions:\n  - id: \"anthropic/claude-haiku-4-5\"\n    selected: false\n",
        )
        .unwrap();
        let s = load(&path).unwrap().unwrap();
        assert_eq!(s.patterns, vec!["anthropic/*"]);
        assert_eq!(s.exceptions, vec![exc("anthropic/claude-haiku-4-5", false)]);
    }
}
