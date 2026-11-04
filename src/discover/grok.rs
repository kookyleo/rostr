//! Grok CLI: `~/.grok/config.toml` and the local `models_cache.json`.

use super::{Adapter, AgentReport, Env, Found, json_str, read_json, read_toml};
use crate::model_id;

pub struct Grok;

/// Collects every string value whose key is `model` or ends in `_model`.
fn config_models(table: &toml::Table, out: &mut Vec<String>) {
    for (key, value) in table {
        match value {
            toml::Value::String(s) if key == "model" || key.ends_with("_model") => {
                out.push(s.clone())
            }
            toml::Value::Table(t) => config_models(t, out),
            _ => {}
        }
    }
}

impl Adapter for Grok {
    fn name(&self) -> &'static str {
        "Grok CLI"
    }

    fn scan(&self, env: &Env) -> AgentReport {
        let mut r = AgentReport::default();
        let home = env.dir("GROK_HOME", ".grok");
        r.installed =
            env.which("grok").is_some() || home.join("bin/grok").exists() || home.is_dir();
        if !r.installed {
            return r;
        }
        r.signed_in = home.join("auth.json").is_file();

        if let Some(cfg) = read_toml(&home.join("config.toml"), &mut r) {
            let mut found = Vec::new();
            config_models(&cfg, &mut found);
            for m in found.iter().filter(|m| !m.trim().is_empty()) {
                r.models.push(Found::new(
                    model_id::canonical("xai", m),
                    None,
                    "configured model",
                ));
            }
        }
        if let Some(cache) = read_json(&home.join("models_cache.json"), &mut r) {
            let models = cache.get("models").and_then(|m| m.as_object());
            for (key, m) in models.into_iter().flatten() {
                let info = m.get("info").unwrap_or(m);
                if info.get("hidden").and_then(|h| h.as_bool()) == Some(true) {
                    continue;
                }
                let model = json_str(info, &["model"]).unwrap_or(key);
                let family = json_str(info, &["model_family"]).unwrap_or("xai");
                let name = json_str(info, &["name"]).map(String::from);
                r.models.push(Found::new(
                    model_id::canonical(family, model),
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
    fn scans_config_and_cache() {
        let fx = Fixture::new();
        fx.write(
            ".grok/config.toml",
            "[ui]\nfork_secondary_model = \"grok-4.6\"\nyolo = false\n",
        );
        fx.write(
            ".grok/models_cache.json",
            r#"{"models": {
                "grok-4.7": {"info": {"model": "grok-4.7", "model_family": "xai", "name": "Grok 4.7", "hidden": false}},
                "grok-secret": {"info": {"model": "grok-secret", "hidden": true}}
            }}"#,
        );
        let r = Grok.scan(&fx.env());
        assert_eq!(ids(&r), vec!["xai/grok-4.6", "xai/grok-4.7"]);
        assert_eq!(r.models[1].name.as_deref(), Some("Grok 4.7"));
    }
}
