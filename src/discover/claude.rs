//! Claude Code: `~/.claude/settings.json`, `~/.claude.json` and `ANTHROPIC_*` variables.

use super::{Adapter, AgentReport, Env, Found, json_str, read_json};

pub struct ClaudeCode;

/// Model aliases that Claude Code accepts. Update this table when Claude Code
/// changes what an alias points to.
const ALIASES: &[(&str, &str)] = &[
    ("opus", "claude-opus-5-5"),
    ("sonnet", "claude-sonnet-5-5"),
    ("haiku", "claude-haiku-5-5"),
    ("fable", "claude-fable-5-1"),
];

const MODEL_VARS: &[&str] = &[
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "CLAUDE_CODE_SUBAGENT_MODEL",
];

/// Turns a Claude Code model setting into canonical IDs.
/// Handles aliases, `[1m]` context suffixes, Bedrock and Vertex forms.
pub(crate) fn resolve(raw: &str) -> Vec<String> {
    let mut s = raw.trim().to_ascii_lowercase();
    if let Some(i) = s.find('[') {
        s.truncate(i);
    }
    match s.as_str() {
        "" | "default" => return Vec::new(),
        "opusplan" => {
            return resolve("opus")
                .into_iter()
                .chain(resolve("sonnet"))
                .collect();
        }
        _ => {}
    }
    if let Some((_, target)) = ALIASES.iter().find(|(a, _)| *a == s) {
        return vec![format!("anthropic/{target}")];
    }
    // Bedrock ARN or ID: `...inference-profile/us.anthropic.claude-x-v1:0`.
    if let Some(i) = s.rfind('/') {
        s = s[i + 1..].to_string();
    }
    if let Some(i) = s.find("anthropic.") {
        s = s[i + "anthropic.".len()..].to_string();
    }
    if let Some(i) = s.rfind("-v")
        && s[i + 2..].chars().all(|c| c.is_ascii_digit() || c == ':')
        && s.len() > i + 2
    {
        s.truncate(i);
    }
    // Vertex form: `claude-x@20250805`.
    if let Some(i) = s.find('@') {
        s.truncate(i);
    }
    if s.starts_with("claude") {
        vec![format!("anthropic/{s}")]
    } else {
        Vec::new()
    }
}

impl Adapter for ClaudeCode {
    fn name(&self) -> &'static str {
        "Claude Code"
    }

    fn scan(&self, env: &Env) -> AgentReport {
        let mut r = AgentReport::default();
        let dir = env.dir("CLAUDE_CONFIG_DIR", ".claude");
        let state = env.home.join(".claude.json");
        r.installed = env.which("claude").is_some()
            || env.home.join(".local/bin/claude").exists()
            || dir.is_dir()
            || state.is_file();
        if !r.installed {
            return r;
        }
        r.signed_in = dir.join(".credentials.json").is_file();

        let push = |r: &mut AgentReport, raw: &str, via: &'static str| {
            for id in resolve(raw) {
                r.models.push(Found::new(id, None, via));
            }
        };
        for (alias, _) in ALIASES {
            push(&mut r, alias, "built-in alias");
        }
        for file in ["settings.json", "settings.local.json"] {
            let Some(v) = read_json(&dir.join(file), &mut r) else {
                continue;
            };
            if let Some(m) = json_str(&v, &["model"]) {
                push(&mut r, m, "default model");
            }
            for var in MODEL_VARS {
                if let Some(m) = json_str(&v, &["env", var]) {
                    push(&mut r, m, "settings env");
                }
            }
        }
        for var in MODEL_VARS {
            if let Some(m) = env.var(var) {
                push(&mut r, m, "environment");
            }
        }
        if let Some(v) = read_json(&state, &mut r) {
            r.signed_in |= v.get("oauthAccount").is_some_and(|a| !a.is_null());
            let options = v
                .get("additionalModelOptionsCache")
                .and_then(|o| o.as_array());
            for opt in options.into_iter().flatten() {
                if let Some(m) = json_str(opt, &["value"]) {
                    push(&mut r, m, "account model list");
                }
            }
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{Fixture, ids};
    use super::*;

    #[test]
    fn resolves_forms() {
        assert_eq!(resolve("opus"), vec!["anthropic/claude-opus-5-5"]);
        assert_eq!(
            resolve("claude-fable-5-1[1m]"),
            vec!["anthropic/claude-fable-5-1"]
        );
        assert_eq!(
            resolve("us.anthropic.claude-opus-4-1-20250805-v1:0"),
            vec!["anthropic/claude-opus-4-1-20250805"]
        );
        assert_eq!(
            resolve("claude-opus-4-1@20250805"),
            vec!["anthropic/claude-opus-4-1"]
        );
        assert_eq!(resolve("opusplan").len(), 2);
        assert!(resolve("default").is_empty());
        assert!(resolve("gpt-5").is_empty());
    }

    #[test]
    fn scans_settings_and_state() {
        let fx = Fixture::new();
        fx.write(
            ".claude/settings.json",
            r#"{"model": "claude-sonnet-4-5", "env": {"ANTHROPIC_SMALL_FAST_MODEL": "claude-haiku-4-5"}}"#,
        );
        fx.write(
            ".claude.json",
            r#"{"additionalModelOptionsCache": [{"value": "claude-x-9[1m]", "label": "X"}]}"#,
        );
        let mut env = fx.env();
        env.vars
            .insert("ANTHROPIC_MODEL".into(), "claude-opus-4-1".into());
        let r = ClaudeCode.scan(&env);
        let ids = ids(&r);
        for want in [
            "anthropic/claude-opus-5-5",
            "anthropic/claude-sonnet-4-5",
            "anthropic/claude-haiku-4-5",
            "anthropic/claude-opus-4-1",
            "anthropic/claude-x-9",
        ] {
            assert!(ids.contains(&want), "missing {want} in {ids:?}");
        }
    }

    #[test]
    fn absent_agent_reports_nothing() {
        let r = ClaudeCode.scan(&Fixture::new().env());
        assert!(!r.installed && r.models.is_empty());
    }
}
