//! Pi: `<agent-dir>/settings.json` and `<agent-dir>/models.json`. The agent
//! directory is `$PI_CODING_AGENT_DIR`, else `~/.pi/agent`.

use super::{Adapter, AgentReport, Env, Found, json_str, read_json};
use crate::model_id;

pub struct Pi;

/// Thinking levels that Pi accepts as a `:<level>` suffix on a model pattern.
const THINKING_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// An exact `provider/id` model pattern, without its thinking suffix. Globs
/// and fuzzy names need Pi's model catalog, so they are skipped.
fn exact_pattern(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let raw = match raw.rsplit_once(':') {
        Some((head, level)) if THINKING_LEVELS.contains(&level) => head,
        _ => raw,
    };
    let (provider, model) = raw.split_once('/')?;
    if provider.is_empty() || model.is_empty() || raw.contains(['*', '?', '[']) {
        return None;
    }
    Some(model_id::canonical(provider, model))
}

impl Adapter for Pi {
    fn name(&self) -> &'static str {
        "Pi"
    }

    fn scan(&self, env: &Env) -> AgentReport {
        let mut r = AgentReport::default();
        let dir = env.dir("PI_CODING_AGENT_DIR", ".pi/agent");
        r.installed = env.which("pi").is_some() || dir.is_dir();
        if !r.installed {
            return r;
        }
        r.signed_in = dir.join("auth.json").is_file();

        if let Some(v) = read_json(&dir.join("settings.json"), &mut r) {
            if let Some(model) = json_str(&v, &["defaultModel"]) {
                // A `vendor/model` ID carries its own vendor; `canonical` ignores the route.
                let provider = json_str(&v, &["defaultProvider"])
                    .or_else(|| model.contains('/').then_some(""))
                    .or_else(|| model_id::infer_vendor(model));
                match provider {
                    Some(p) => r.models.push(Found::new(
                        model_id::canonical(p, model),
                        None,
                        "default model",
                    )),
                    None => r.warnings.push(format!(
                        "default model `{model}` has no defaultProvider; skipped"
                    )),
                }
            }
            let enabled = v.get("enabledModels").and_then(|m| m.as_array());
            for p in enabled.into_iter().flatten().filter_map(|p| p.as_str()) {
                if let Some(id) = exact_pattern(p) {
                    r.models.push(Found::new(id, None, "enabled model"));
                }
            }
            let levels = v.get("modelThinkingLevels").and_then(|m| m.as_object());
            for key in levels.into_iter().flat_map(|m| m.keys()) {
                if let Some(id) = exact_pattern(key) {
                    r.models.push(Found::new(id, None, "thinking level"));
                }
            }
        }
        if let Some(v) = read_json(&dir.join("models.json"), &mut r) {
            let providers = v.get("providers").and_then(|p| p.as_object());
            for (provider, p) in providers.into_iter().flatten() {
                let models = p.get("models").and_then(|m| m.as_array());
                for m in models.into_iter().flatten() {
                    let Some(model) = json_str(m, &["id"]) else {
                        continue;
                    };
                    let name = json_str(m, &["name"]).map(String::from);
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
    fn scans_settings_and_models() {
        let fx = Fixture::new();
        fx.write(
            ".pi/agent/settings.json",
            r#"{
              "defaultProvider": "anthropic",
              "defaultModel": "claude-opus-5-5",
              "enabledModels": ["openai/gpt-5.3-codex:high", "google/*", "sonnet"],
              "modelThinkingLevels": {"xai/grok-4.7": "low"}
            }"#,
        );
        fx.write(
            ".pi/agent/models.json",
            r#"{"providers": {"ollama": {
                "baseUrl": "http://localhost:11434/v1",
                "models": [{"id": "qwen2.5-coder:7b", "name": "Qwen Coder"}, {"id": "llama3"}]
            }}}"#,
        );
        let r = Pi.scan(&fx.env());
        assert_eq!(
            ids(&r),
            vec![
                "anthropic/claude-opus-5-5",
                "openai/gpt-5.3-codex",
                "xai/grok-4.7",
                "alibaba/qwen2.5-coder:7b",
                "ollama/llama3"
            ]
        );
        assert_eq!(r.models[3].name.as_deref(), Some("Qwen Coder"));
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn agent_dir_from_env() {
        let fx = Fixture::new();
        fx.write(
            "custom/settings.json",
            r#"{"defaultProvider": "autodl", "defaultModel": "Qwen3.8-Flash-Next"}"#,
        );
        let mut env = fx.env();
        env.vars.insert(
            "PI_CODING_AGENT_DIR".into(),
            fx.dir.path().join("custom").display().to_string(),
        );
        let r = Pi.scan(&env);
        assert!(r.installed);
        assert_eq!(ids(&r), vec!["alibaba/qwen3.8-flash-next"]);
    }

    #[test]
    fn default_model_without_provider() {
        let fx = Fixture::new();
        fx.write(".pi/agent/settings.json", r#"{"defaultModel": "my-model"}"#);
        let r = Pi.scan(&fx.env());
        assert!(r.models.is_empty());
        assert_eq!(r.warnings.len(), 1);
        assert!(!Pi.scan(&Fixture::new().env()).installed);
    }
}
