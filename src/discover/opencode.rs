//! OpenCode: `$XDG_CONFIG_HOME/opencode/opencode.json[c]`. Models use `provider/model` form.

use super::{Adapter, AgentReport, Env, Found, read, strip_jsonc};
use crate::model_id;

pub struct OpenCode;

const FILES: &[&str] = &["opencode.json", "opencode.jsonc", "config.json"];

fn split(raw: &str) -> Option<String> {
    let (provider, model) = raw.trim().split_once('/')?;
    (!provider.is_empty() && !model.is_empty()).then(|| model_id::canonical(provider, model))
}

impl Adapter for OpenCode {
    fn name(&self) -> &'static str {
        "OpenCode"
    }

    fn scan(&self, env: &Env) -> AgentReport {
        let mut r = AgentReport::default();
        let dir = env.xdg_config().join("opencode");
        r.installed = env.which("opencode").is_some()
            || env.home.join(".opencode/bin/opencode").exists()
            || dir.is_dir();
        if !r.installed {
            return r;
        }
        r.signed_in = env.home.join(".local/share/opencode/auth.json").is_file();

        for file in FILES {
            let path = dir.join(file);
            let text = match read(&path) {
                Ok(Some(t)) => t,
                Ok(None) => continue,
                Err(e) => {
                    r.warnings.push(e);
                    continue;
                }
            };
            let v: serde_json::Value = match serde_json::from_str(&strip_jsonc(&text)) {
                Ok(v) => v,
                Err(e) => {
                    r.warnings
                        .push(format!("cannot parse {}: {e}", path.display()));
                    continue;
                }
            };
            for (key, via) in [("model", "default model"), ("small_model", "small model")] {
                if let Some(id) = v.get(key).and_then(|m| m.as_str()).and_then(split) {
                    r.models.push(Found::new(id, None, via));
                }
            }
            for section in ["agent", "mode"] {
                let agents = v.get(section).and_then(|a| a.as_object());
                for a in agents.into_iter().flat_map(|m| m.values()) {
                    if let Some(id) = a.get("model").and_then(|m| m.as_str()).and_then(split) {
                        r.models.push(Found::new(id, None, "agent model"));
                    }
                }
            }
            let providers = v.get("provider").and_then(|p| p.as_object());
            for (provider, p) in providers.into_iter().flatten() {
                let models = p.get("models").and_then(|m| m.as_object());
                for (model, m) in models.into_iter().flatten() {
                    let name = m.get("name").and_then(|n| n.as_str()).map(String::from);
                    let id = model_id::canonical(provider, model);
                    r.models.push(Found::new(id, name, "provider model"));
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
    fn scans_jsonc_config() {
        let fx = Fixture::new();
        fx.write(
            ".config/opencode/opencode.jsonc",
            r#"{
              // default
              "model": "anthropic/claude-sonnet-4-5",
              "small_model": "openrouter/google/gemini-2.5-flash",
              "agent": {"plan": {"model": "openai/gpt-5.3-codex"}},
              "provider": {"ollama": {"models": {"llama3": {"name": "Llama 3"}}}},
            }"#,
        );
        let r = OpenCode.scan(&fx.env());
        assert_eq!(
            ids(&r),
            vec![
                "anthropic/claude-sonnet-4-5",
                "google/gemini-2.5-flash",
                "openai/gpt-5.3-codex",
                "ollama/llama3"
            ]
        );
        assert!(r.warnings.is_empty());
    }
}
