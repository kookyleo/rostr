//! Codex CLI: `$CODEX_HOME/config.toml` and the local `models_cache.json`.

use super::{Adapter, AgentReport, Env, Found, json_str, read_json, read_toml};
use crate::model_id;

pub struct Codex;

/// Codex `model_provider` values that mean the OpenAI API itself.
fn route(provider: Option<&str>) -> &str {
    match provider.map(str::trim) {
        None | Some("") | Some("openai") | Some("azure") => "openai",
        Some(other) => other,
    }
}

fn push_model(
    r: &mut AgentReport,
    table: &toml::Table,
    inherited: Option<&str>,
    via: &'static str,
) {
    let provider = table
        .get("model_provider")
        .and_then(|v| v.as_str())
        .or(inherited);
    for key in ["model", "review_model"] {
        if let Some(m) = table
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|m| !m.trim().is_empty())
        {
            let id = model_id::canonical(route(provider), m);
            r.models.push(Found::new(id, None, via));
        }
    }
}

impl Adapter for Codex {
    fn name(&self) -> &'static str {
        "Codex"
    }

    fn scan(&self, env: &Env) -> AgentReport {
        let mut r = AgentReport::default();
        let home = env.dir("CODEX_HOME", ".codex");
        r.installed = env.which("codex").is_some() || home.is_dir();
        if !r.installed {
            return r;
        }
        r.signed_in = home.join("auth.json").is_file();

        if let Some(cfg) = read_toml(&home.join("config.toml"), &mut r) {
            push_model(&mut r, &cfg, None, "default model");
            let top_provider = cfg.get("model_provider").and_then(|v| v.as_str());
            if let Some(profiles) = cfg.get("profiles").and_then(|v| v.as_table()) {
                for profile in profiles.values().filter_map(|p| p.as_table()) {
                    push_model(&mut r, profile, top_provider, "profile model");
                }
            }
        }
        if let Some(cache) = read_json(&home.join("models_cache.json"), &mut r) {
            let models = cache.get("models").and_then(|m| m.as_array());
            for m in models.into_iter().flatten() {
                if json_str(m, &["visibility"]) == Some("hide") {
                    continue;
                }
                let Some(slug) = json_str(m, &["slug"]) else {
                    continue;
                };
                let name = json_str(m, &["display_name"]).map(String::from);
                r.models.push(Found::new(
                    model_id::canonical("openai", slug),
                    name,
                    "model list",
                ));
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
    fn scans_config_profiles_and_cache() {
        let fx = Fixture::new();
        fx.write(
            ".codex/config.toml",
            "model = \"gpt-5.3-codex\"\n[profiles.local]\nmodel = \"qwen3-coder\"\nmodel_provider = \"ollama\"\n[profiles.oss]\nmodel = \"llama3\"\nmodel_provider = \"ollama\"\n",
        );
        fx.write(
            ".codex/models_cache.json",
            r#"{"models": [
                {"slug": "gpt-5.2-codex", "display_name": "GPT-5.2 Codex", "visibility": "list"},
                {"slug": "internal-x", "display_name": "Hidden", "visibility": "hide"}
            ]}"#,
        );
        let r = Codex.scan(&fx.env());
        assert_eq!(
            ids(&r),
            vec![
                "openai/gpt-5.3-codex",
                "alibaba/qwen3-coder",
                "ollama/llama3",
                "openai/gpt-5.2-codex"
            ]
        );
        assert_eq!(r.models[3].name.as_deref(), Some("GPT-5.2 Codex"));
    }

    #[test]
    fn broken_config_warns() {
        let fx = Fixture::new();
        fx.write(".codex/config.toml", "model = \n");
        let r = Codex.scan(&fx.env());
        assert!(r.installed);
        assert_eq!(r.warnings.len(), 1);
    }
}
