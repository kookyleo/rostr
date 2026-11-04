//! Gemini CLI: `~/.gemini/settings.json` and `GEMINI_MODEL`.

use super::{Adapter, AgentReport, Env, Found, json_str, read_json};
use crate::model_id;

pub struct Gemini;

impl Adapter for Gemini {
    fn name(&self) -> &'static str {
        "Gemini CLI"
    }

    fn scan(&self, env: &Env) -> AgentReport {
        let mut r = AgentReport::default();
        let dir = env.home.join(".gemini");
        r.installed = env.which("gemini").is_some() || dir.is_dir();
        if !r.installed {
            return r;
        }
        r.signed_in = dir.join("oauth_creds.json").is_file();

        let push = |r: &mut AgentReport, m: &str, via| {
            r.models
                .push(Found::new(model_id::canonical("google", m), None, via));
        };
        if let Some(v) = read_json(&dir.join("settings.json"), &mut r) {
            // Newer versions nest it as `model.name`; older ones use a string.
            if let Some(m) = json_str(&v, &["model", "name"]).or_else(|| json_str(&v, &["model"])) {
                push(&mut r, m, "default model");
            }
        }
        if let Some(m) = env.var("GEMINI_MODEL") {
            push(&mut r, m, "environment");
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{Fixture, ids};
    use super::*;

    #[test]
    fn scans_settings() {
        let fx = Fixture::new();
        fx.write(
            ".gemini/settings.json",
            r#"{"model": {"name": "gemini-2.5-pro"}}"#,
        );
        assert_eq!(ids(&Gemini.scan(&fx.env())), vec!["google/gemini-2.5-pro"]);
    }
}
