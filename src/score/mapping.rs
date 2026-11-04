//! Maps canonical `<provider>/<model>` IDs to board keys.
//!
//! Order: `scores.yaml` aliases, then builtin aliases, then deterministic
//! rules. The rules only normalize spelling (case, separators, `4.5` vs
//! `4-5`), split off an explicit variant tag (`_high`, `(thinking-32k)`) and
//! an explicit snapshot date. There is no similarity scoring: two distinct
//! board keys that fit the same model are reported as ambiguous.

use super::{BoardRow, ScoreOverrides};
use crate::model_id;

/// `(model, source, metric or "" for every metric, board key)`.
/// Only for board names that the rules cannot reach.
pub const BUILTIN_ALIASES: &[(&str, &str, &str, &str)] = &[];

#[derive(Debug, PartialEq)]
pub enum Match<'a> {
    Found {
        row: &'a BoardRow,
        matched_by: String,
    },
    Ambiguous(Vec<String>),
    None(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    /// Normalized name including any snapshot date.
    pub base: String,
    /// Normalized variant tag, such as `high` or `thinking-32k`.
    pub variant: Option<String>,
    /// `base` without a trailing snapshot date.
    pub dateless: String,
    pub dated: bool,
}

fn canon(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        let digit_dot = c == '.'
            && i > 0
            && chars[i - 1].is_ascii_digit()
            && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit());
        let sep = c.is_whitespace() || c == '_' || c == '-' || digit_dot;
        if sep {
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out.trim_end_matches('-').to_string()
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() == 8 && b.iter().all(u8::is_ascii_digit) && s.starts_with("20"))
        || (b.len() == 10
            && s.starts_with("20")
            && b[4] == b'-'
            && b[7] == b'-'
            && b.iter()
                .enumerate()
                .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit()))
}

/// Splits a trailing snapshot date (`-20251101`, `-2025-11-13`) off a canonical name.
fn strip_date(base: &str) -> Option<&str> {
    if let Some((head, tail)) = base.rsplit_once('-')
        && is_date(tail)
        && !head.is_empty()
    {
        return Some(head);
    }
    if base.len() > 11 {
        let (head, tail) = base.split_at(base.len() - 11);
        if let Some(d) = tail.strip_prefix('-')
            && is_date(d)
            && !head.is_empty()
        {
            return Some(head);
        }
    }
    None
}

pub fn key(raw: &str) -> Key {
    let s = raw.trim();
    let s = s.rsplit('/').next().unwrap_or(s).trim();
    let mut name = s.to_string();
    let mut variant: Option<String> = None;
    if name.ends_with(')')
        && let Some(open) = name.rfind('(')
    {
        let inner = name[open + 1..name.len() - 1].trim().to_string();
        name = name[..open].trim().to_string();
        if is_date(&canon(&inner)) {
            name = format!("{name}-{inner}");
        } else if !inner.is_empty() {
            variant = Some(inner);
        }
    }
    if variant.is_none()
        && let Some((head, tail)) = name.rsplit_once('_')
        && !head.is_empty()
        && !tail.is_empty()
    {
        variant = Some(tail.to_string());
        name = head.to_string();
    }
    let mut base = canon(&name);
    if variant.is_none()
        && let Some(i) = base.find("-thinking")
    {
        let rest = &base[i + "-thinking".len()..];
        let budget = rest.strip_prefix('-').unwrap_or(rest);
        if rest.is_empty()
            || (rest.starts_with('-')
                && budget.ends_with('k')
                && budget[..budget.len() - 1]
                    .bytes()
                    .all(|c| c.is_ascii_digit()))
        {
            variant = Some(base[i + 1..].to_string());
            base.truncate(i);
        }
    }
    let (dateless, dated) = match strip_date(&base) {
        Some(head) => (head.to_string(), true),
        None => (base.clone(), false),
    };
    Key {
        base,
        variant: variant.map(|v| canon(&v)).filter(|v| !v.is_empty()),
        dateless,
        dated,
    }
}

/// The provider a board row belongs to, from its organization or its name.
pub fn row_provider(row: &BoardRow) -> Option<String> {
    let org = row
        .organization
        .as_deref()
        .and_then(|o| o.split(',').next())
        .map(|o| o.trim().to_ascii_lowercase());
    let by_org = org.as_deref().and_then(|o| {
        Some(match o {
            "openai" => "openai",
            "anthropic" => "anthropic",
            "google" | "google deepmind" | "deepmind" => "google",
            "xai" | "x.ai" => "xai",
            "deepseek" => "deepseek",
            "alibaba" | "qwen" => "alibaba",
            "mistral" | "mistral ai" | "mistralai" => "mistral",
            "moonshot" | "moonshot ai" | "moonshotai" => "moonshotai",
            "z.ai (zhipu ai)" | "zhipu" | "zhipu ai" | "zhipuai" | "z.ai" | "zai" => "zhipuai",
            _ => return None,
        })
    });
    by_org
        .or_else(|| model_id::infer_vendor(&key(&row.source_key).base))
        .map(String::from)
}

fn newest<'a>(rows: &[&'a BoardRow]) -> &'a BoardRow {
    // Rows keep board order; a later `as_of` wins, ties keep the first row.
    let mut best = rows[0];
    for r in &rows[1..] {
        if r.as_of > best.as_of {
            best = r;
        }
    }
    best
}

fn distinct_keys(rows: &[&BoardRow]) -> Vec<String> {
    let mut keys: Vec<String> = rows.iter().map(|r| r.source_key.clone()).collect();
    keys.sort();
    keys.dedup();
    keys
}

/// One candidate set: exactly one distinct board key, else ambiguous.
fn pick<'a>(rows: &[&'a BoardRow], matched_by: &str) -> Match<'a> {
    let keys = distinct_keys(rows);
    if keys.len() > 1 {
        return Match::Ambiguous(keys);
    }
    Match::Found {
        row: newest(rows),
        matched_by: matched_by.into(),
    }
}

fn pinned<'a>(rows: &[&'a BoardRow], keys: &[&str], matched_by: &str) -> Match<'a> {
    let hits: Vec<&BoardRow> = rows
        .iter()
        .copied()
        .filter(|r| keys.iter().any(|k| k.eq_ignore_ascii_case(&r.source_key)))
        .collect();
    if hits.is_empty() {
        return Match::None(format!("pinned key {} not on this board", keys.join(", ")));
    }
    pick(&hits, matched_by)
}

/// Rules for rows already known to be the same model name (after normalization).
fn by_variant<'a>(rows: &[&'a BoardRow], target: &Key, plain_label: &str) -> Option<Match<'a>> {
    if rows.is_empty() {
        return None;
    }
    if target.variant.is_some() {
        return Some(pick(rows, plain_label));
    }
    let plain: Vec<&BoardRow> = rows
        .iter()
        .copied()
        .filter(|r| key(&r.source_key).variant.is_none())
        .collect();
    if !plain.is_empty() {
        return Some(pick(&plain, plain_label));
    }
    Some(pick(rows, "rule:variant"))
}

pub fn find<'a>(
    model: &str,
    source: &str,
    metric: &str,
    rows: &[&'a BoardRow],
    overrides: &ScoreOverrides,
) -> Match<'a> {
    let user = overrides.aliases_for(model, source, metric);
    if !user.is_empty() {
        return pinned(rows, &user, "alias:scores.yaml");
    }
    let builtin: Vec<&str> = BUILTIN_ALIASES
        .iter()
        .filter(|(m, s, x, _)| *m == model && *s == source && (x.is_empty() || *x == metric))
        .map(|(_, _, _, k)| *k)
        .collect();
    if !builtin.is_empty() {
        return pinned(rows, &builtin, "alias:builtin");
    }

    let Some((provider, name)) = model.split_once('/') else {
        return Match::None("not a canonical model ID".into());
    };
    let target = key(name);
    let compatible: Vec<(&'a BoardRow, Key)> = rows
        .iter()
        .copied()
        .filter(|r| row_provider(r).is_none_or(|p| p == provider))
        .map(|r| (r, key(&r.source_key)))
        .filter(|(_, k)| target.variant.is_none() || k.variant == target.variant)
        .collect();

    let exact: Vec<&BoardRow> = compatible
        .iter()
        .filter(|(_, k)| k.base == target.base)
        .map(|(r, _)| *r)
        .collect();
    if let Some(m) = by_variant(&exact, &target, "rule:exact") {
        return m;
    }
    // A dated name never matches a different date; an undated name matches
    // a single dated snapshot and vice versa.
    let dateless: Vec<&BoardRow> = compatible
        .iter()
        .filter(|(_, k)| k.dateless == target.dateless && !(k.dated && target.dated))
        .map(|(r, _)| *r)
        .collect();
    if let Some(m) = by_variant(&dateless, &target, "rule:dateless") {
        return m;
    }
    Match::None("no board entry for this model".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, org: Option<&str>) -> BoardRow {
        BoardRow {
            metric: "m".into(),
            source_key: key.into(),
            organization: org.map(String::from),
            value: 1.0,
            as_of: None,
        }
    }

    #[test]
    fn keys() {
        let k = key("claude-opus-4-5-20251101_32K");
        assert_eq!(k.base, "claude-opus-4-5-20251101");
        assert_eq!(k.variant.as_deref(), Some("32k"));
        assert_eq!(k.dateless, "claude-opus-4-5");
        assert_eq!(key("Claude Opus 4.5").base, "claude-opus-4-5");
        let k = key("Claude Opus 5.5 (High)");
        assert_eq!(
            (k.base.as_str(), k.variant.as_deref()),
            ("claude-opus-5-5", Some("high"))
        );
        let k = key("gpt-5.1-2025-11-13_high");
        assert_eq!(k.dateless, "gpt-5-1");
        assert!(k.dated);
        let k = key("claude-opus-4-5-20251101-thinking-32k");
        assert_eq!(k.variant.as_deref(), Some("thinking-32k"));
        assert_eq!(k.dateless, "claude-opus-4-5");
        let k = key("GPT-4o (2024-08-06)");
        assert_eq!((k.base.as_str(), k.variant), ("gpt-4o-2024-08-06", None));
        assert_eq!(key("Qwen3.8 Max (0902)").variant.as_deref(), Some("0902"));
        assert_eq!(key("openai/gpt-5").base, "gpt-5");
    }

    #[test]
    fn rules() {
        let o = ScoreOverrides::default_v1();
        let rows = [
            row("claude-opus-4-5-20251101", Some("Anthropic")),
            row("gpt-5.1-2025-11-13_high", Some("OpenAI")),
            row("gpt-5-2025-08-07_high", Some("OpenAI")),
            row("gpt-5-2025-08-07_medium", Some("OpenAI")),
            row("GPT-6 Astra", Some("OpenAI")),
            row("claude-sonnet-4-6", Some("Anthropic")),
            row("claude-sonnet-4-6_max", Some("Anthropic")),
        ];
        let refs: Vec<&BoardRow> = rows.iter().collect();
        let found = |model: &str| match find(model, "s", "m", &refs, &o) {
            Match::Found { row, matched_by } => Some((row.source_key.clone(), matched_by)),
            _ => None,
        };
        assert_eq!(
            found("anthropic/claude-opus-4-5"),
            Some(("claude-opus-4-5-20251101".into(), "rule:dateless".into()))
        );
        assert_eq!(
            found("anthropic/claude-opus-4-5-20251101"),
            Some(("claude-opus-4-5-20251101".into(), "rule:exact".into()))
        );
        // A different snapshot date never matches.
        assert_eq!(found("anthropic/claude-opus-4-5-20250101"), None);
        assert_eq!(
            found("openai/gpt-5.1"),
            Some(("gpt-5.1-2025-11-13_high".into(), "rule:variant".into()))
        );
        assert_eq!(
            found("openai/gpt-6-astra"),
            Some(("GPT-6 Astra".into(), "rule:exact".into()))
        );
        // The plain row wins over a tagged variant of the same name.
        assert_eq!(
            found("anthropic/claude-sonnet-4-6"),
            Some(("claude-sonnet-4-6".into(), "rule:exact".into()))
        );
        // Provider mismatch: no cross-vendor match.
        assert_eq!(found("xai/gpt-6-astra"), None);
        assert!(matches!(
            find("openai/gpt-5", "s", "m", &refs, &o),
            Match::Ambiguous(k) if k.len() == 2
        ));
        // No fuzzy matching on prefixes.
        assert_eq!(found("openai/gpt-6"), None);
        assert_eq!(found("anthropic/claude-opus-4"), None);
    }

    #[test]
    fn aliases_pin_a_variant() {
        let o: ScoreOverrides = serde_norway::from_str(
            "version: 1\naliases:\n  - {model: openai/gpt-5, source: s, key: gpt-5-2025-08-07_high}\n",
        )
        .unwrap();
        let rows = [
            row("gpt-5-2025-08-07_high", Some("OpenAI")),
            row("gpt-5-2025-08-07_medium", Some("OpenAI")),
        ];
        let refs: Vec<&BoardRow> = rows.iter().collect();
        assert!(matches!(
            find("openai/gpt-5", "s", "m", &refs, &o),
            Match::Found { row, matched_by } if row.source_key == "gpt-5-2025-08-07_high" && matched_by == "alias:scores.yaml"
        ));
    }
}
