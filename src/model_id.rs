//! Canonical model IDs (`<provider>/<model>`), wildcard rules and display names.

use anyhow::{Result, bail};

/// Normalizes a raw `<provider>/<model>` string: trims it and lowercases it.
pub fn normalize(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

fn valid_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-' | ':' | '@' | '+')
}

fn check_provider(provider: &str, input: &str) -> Result<()> {
    if provider.is_empty() || !provider.chars().all(valid_char) {
        bail!("invalid provider in `{input}`: use lowercase letters, digits, `.`, `_`, `-`");
    }
    Ok(())
}

/// Validates a canonical model ID. The model part may contain `/`
/// (for example `openrouter/meta-llama/llama-4`), but no wildcards.
pub fn validate_id(id: &str) -> Result<()> {
    let Some((provider, model)) = id.split_once('/') else {
        bail!("invalid model ID `{id}`: expected `<provider>/<model>`");
    };
    check_provider(provider, id)?;
    if model.is_empty()
        || model.starts_with('/')
        || model.ends_with('/')
        || model.contains("//")
        || !model.chars().all(|c| valid_char(c) || c == '/')
    {
        bail!("invalid model ID `{id}`: model part has unsupported characters");
    }
    Ok(())
}

/// Validates an include rule: a canonical ID in which `*` and `?` act as wildcards.
pub fn validate_pattern(rule: &str) -> Result<()> {
    let Some((provider, model)) = rule.split_once('/') else {
        bail!(
            "invalid pattern `{rule}`: expected `<provider>/<pattern>`, for example `anthropic/*`"
        );
    };
    check_provider(provider, rule)?;
    if model.is_empty()
        || !model
            .chars()
            .all(|c| valid_char(c) || matches!(c, '/' | '*' | '?'))
    {
        bail!("invalid pattern `{rule}`: pattern has unsupported characters");
    }
    Ok(())
}

pub fn is_pattern(s: &str) -> bool {
    s.contains('*') || s.contains('?')
}

/// Matches `text` against a glob `pattern` (`*` = any run of characters, `?` = one character).
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// Infers the model vendor from a bare model name. Returns `None` when unknown.
pub fn infer_vendor(model: &str) -> Option<&'static str> {
    let m = model.to_ascii_lowercase();
    let starts = |prefixes: &[&str]| prefixes.iter().any(|p| m.starts_with(p));
    if starts(&["claude"]) {
        Some("anthropic")
    } else if starts(&["gpt-", "gpt", "o1", "o3", "o4", "codex", "chatgpt"]) {
        Some("openai")
    } else if starts(&["grok"]) {
        Some("xai")
    } else if starts(&["gemini", "gemma"]) {
        Some("google")
    } else if starts(&["deepseek"]) {
        Some("deepseek")
    } else if starts(&["qwen"]) {
        Some("alibaba")
    } else if starts(&["mistral", "codestral", "devstral"]) {
        Some("mistral")
    } else if starts(&["kimi", "moonshot"]) {
        Some("moonshotai")
    } else if starts(&["glm"]) {
        Some("zhipuai")
    } else {
        None
    }
}

/// Builds a canonical ID from a route provider and a model name. Aggregator
/// names such as `anthropic/claude-x` keep their own vendor prefix; otherwise
/// the vendor comes from the model name, and then from the route provider.
pub fn canonical(route_provider: &str, model: &str) -> String {
    let model = normalize(model);
    if let Some((vendor, rest)) = model.split_once('/')
        && !vendor.is_empty()
        && !rest.is_empty()
    {
        return format!("{vendor}/{rest}");
    }
    let vendor = infer_vendor(&model).unwrap_or(route_provider);
    format!("{}/{model}", normalize(vendor))
}

pub fn provider_of(id: &str) -> &str {
    id.split_once('/').map_or(id, |(p, _)| p)
}

pub fn provider_display(provider: &str) -> String {
    match provider {
        "anthropic" => "Anthropic".into(),
        "openai" => "OpenAI".into(),
        "xai" => "xAI".into(),
        "google" => "Google".into(),
        "deepseek" => "DeepSeek".into(),
        "mistral" => "Mistral".into(),
        "alibaba" => "Alibaba".into(),
        "moonshotai" => "Moonshot AI".into(),
        "zhipuai" => "Zhipu AI".into(),
        other => title_word(other),
    }
}

fn title_word(w: &str) -> String {
    let mut c = w.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// Derives a readable name from a canonical ID when no source supplies one.
/// `anthropic/claude-opus-4-5` becomes `Claude Opus 4.5`.
pub fn display_name(id: &str) -> String {
    let model = id.split_once('/').map_or(id, |(_, m)| m);
    let model = model.rsplit('/').next().unwrap_or(model);
    let mut words: Vec<String> = Vec::new();
    for part in model.split('-') {
        let numeric = !part.is_empty() && part.chars().all(|c| c.is_ascii_digit());
        // Join short version digits: `4-5` -> `4.5`. Keep dates (8 digits) apart.
        if numeric
            && part.len() <= 2
            && let Some(last) = words.last_mut()
            && last.chars().all(|c| c.is_ascii_digit() || c == '.')
            && last.len() <= 4
        {
            last.push('.');
            last.push_str(part);
            continue;
        }
        let word = match part {
            "gpt" => "GPT".to_string(),
            p if p.len() <= 2 && !numeric => p.to_ascii_uppercase(),
            p => title_word(p),
        };
        words.push(word);
    }
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(wildcard_match("anthropic/*", "anthropic/claude-opus-4-5"));
        assert!(wildcard_match("openai/*codex*", "openai/gpt-5.3-codex"));
        assert!(!wildcard_match("openai/*codex*", "openai/gpt-5"));
        assert!(wildcard_match("xai/grok-?", "xai/grok-4"));
        assert!(!wildcard_match("xai/grok-*", "anthropic/grok-4"));
        assert!(wildcard_match("a/b", "a/b"));
    }

    #[test]
    fn ids_and_rules() {
        assert!(validate_id("anthropic/claude-opus-4-5").is_ok());
        assert!(validate_id("openrouter/meta/llama-4").is_ok());
        assert!(validate_id("claude-opus").is_err());
        assert!(validate_id("anthropic/*").is_err());
        assert!(validate_id("Anthropic/x").is_err());
        assert!(validate_pattern("openai/*codex*").is_ok());
        assert!(validate_pattern("*").is_err());
    }

    #[test]
    fn canonical_ids() {
        assert_eq!(
            canonical("opencode", "Claude-Sonnet-4"),
            "anthropic/claude-sonnet-4"
        );
        assert_eq!(canonical("openrouter", "meta/llama-4"), "meta/llama-4");
        assert_eq!(canonical("ollama", "llama3"), "ollama/llama3");
    }

    #[test]
    fn names() {
        assert_eq!(display_name("anthropic/claude-opus-4-5"), "Claude Opus 4.5");
        assert_eq!(display_name("openai/gpt-5.3-codex"), "GPT 5.3 Codex");
        assert_eq!(
            display_name("anthropic/claude-sonnet-4-20250514"),
            "Claude Sonnet 4 20250514"
        );
        assert_eq!(provider_display("xai"), "xAI");
    }
}
