//! `scores.yaml`: manual aliases, ignore rules and values. Hand-edited only;
//! rostr never writes it.

use super::{Axis, probe};
use crate::model_id;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const FILE_NAME: &str = "scores.yaml";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScoreOverrides {
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default)]
    pub aliases: Vec<Alias>,
    #[serde(default)]
    pub ignore: Vec<Ignore>,
    #[serde(default)]
    pub values: Vec<ManualValue>,
}
fn one() -> u32 {
    1
}

/// Pins the board key a source uses for a model. With an alias in place for a
/// source (and metric, when given), the rules are not consulted for it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Alias {
    pub model: String,
    pub source: String,
    #[serde(default)]
    pub metric: Option<String>,
    pub key: String,
}

/// Drops board rows before matching. Every given field must match; `model`
/// may be a wildcard pattern such as `openai/*`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ignore {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub metric: Option<String>,
    #[serde(default)]
    pub axis: Option<Axis>,
    #[serde(default)]
    pub key: Option<String>,
}

/// A manual value. It outranks every source for its model and axis.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualValue {
    pub model: String,
    pub axis: Axis,
    pub value: f64,
    #[serde(default)]
    pub metric: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub as_of: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

fn check_source(source: &str, metric: Option<&str>) -> Result<()> {
    let Some(p) = probe(source) else {
        bail!("unknown source `{source}`");
    };
    if let Some(m) = metric
        && !p.info().metrics.iter().any(|x| x.metric == m)
    {
        bail!("source `{source}` has no metric `{m}`");
    }
    Ok(())
}

impl ScoreOverrides {
    /// Loads the file; a missing file means no overrides.
    pub fn load(path: &Path) -> Result<ScoreOverrides> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ScoreOverrides::default_v1());
            }
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        let o: ScoreOverrides = serde_norway::from_str(&text)
            .with_context(|| format!("cannot parse {}", path.display()))?;
        o.validate()
            .with_context(|| format!("invalid {}", path.display()))?;
        Ok(o)
    }

    pub fn default_v1() -> ScoreOverrides {
        ScoreOverrides {
            version: 1,
            ..Default::default()
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            bail!("unsupported version {}; expected 1", self.version);
        }
        for a in &self.aliases {
            model_id::validate_id(&a.model)?;
            check_source(&a.source, a.metric.as_deref())?;
            if a.key.trim().is_empty() {
                bail!("alias for `{}` has an empty key", a.model);
            }
        }
        for i in &self.ignore {
            if i.model.is_none()
                && i.source.is_none()
                && i.metric.is_none()
                && i.axis.is_none()
                && i.key.is_none()
            {
                bail!("an ignore rule needs at least one field");
            }
            if let Some(m) = &i.model {
                model_id::validate_pattern(m)?;
            }
            if let Some(s) = &i.source {
                check_source(s, i.metric.as_deref())?;
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for v in &self.values {
            model_id::validate_id(&v.model)?;
            if !v.value.is_finite() {
                bail!("value for `{}` {} is not a finite number", v.model, v.axis);
            }
            if !seen.insert((v.model.as_str(), v.axis)) {
                bail!("duplicate value for `{}` {}", v.model, v.axis);
            }
        }
        Ok(())
    }

    pub fn aliases_for(&self, model: &str, source: &str, metric: &str) -> Vec<&str> {
        let pinned = |want_metric: bool| -> Vec<&str> {
            self.aliases
                .iter()
                .filter(|a| a.model == model && a.source == source)
                .filter(|a| match &a.metric {
                    Some(m) => want_metric && m == metric,
                    None => !want_metric,
                })
                .map(|a| a.key.as_str())
                .collect()
        };
        let specific = pinned(true);
        if specific.is_empty() {
            pinned(false)
        } else {
            specific
        }
    }

    pub fn ignored(&self, model: &str, axis: Axis, source: &str, metric: &str, key: &str) -> bool {
        self.ignore.iter().any(|i| {
            i.model
                .as_deref()
                .is_none_or(|m| model_id::wildcard_match(m, model))
                && i.source.as_deref().is_none_or(|s| s == source)
                && i.metric.as_deref().is_none_or(|m| m == metric)
                && i.axis.is_none_or(|a| a == axis)
                && i.key.as_deref().is_none_or(|k| k.eq_ignore_ascii_case(key))
        })
    }

    pub fn value(&self, model: &str, axis: Axis) -> Option<&ManualValue> {
        self.values
            .iter()
            .find(|v| v.model == model && v.axis == axis)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_validates() {
        let o: ScoreOverrides = serde_norway::from_str(
            "version: 1\naliases:\n  - {model: anthropic/claude-x, source: epoch-ai, key: claude-x_32K}\n\
             ignore:\n  - {model: 'openai/*', source: lmarena}\n\
             values:\n  - {model: a/b, axis: coding, value: 0.5}\n",
        )
        .unwrap();
        o.validate().unwrap();
        assert_eq!(
            o.aliases_for("anthropic/claude-x", "epoch-ai", "eci"),
            vec!["claude-x_32K"]
        );
        assert!(o.ignored(
            "openai/gpt-5",
            Axis::General,
            "lmarena",
            "text-overall",
            "GPT-5"
        ));
        assert!(!o.ignored("openai/gpt-5", Axis::General, "epoch-ai", "eci", "GPT-5"));
        assert_eq!(o.value("a/b", Axis::Coding).unwrap().value, 0.5);

        for bad in [
            "version: 2\n",
            "version: 1\nignore:\n  - {}\n",
            "version: 1\naliases:\n  - {model: a/b, source: nope, key: x}\n",
            "version: 1\naliases:\n  - {model: a/b, source: lmarena, metric: eci, key: x}\n",
            "version: 1\nvalues:\n  - {model: a/b, axis: coding, value: 1}\n  - {model: a/b, axis: coding, value: 2}\n",
            "version: 1\nvalues:\n  - {model: a/b, axis: speed, value: 1}\n",
        ] {
            let parsed: Result<ScoreOverrides, _> = serde_norway::from_str(bad);
            assert!(parsed.map_or(true, |o| o.validate().is_err()), "{bad}");
        }
    }
}
