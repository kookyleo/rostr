//! Bounded, read-only native cache observers. No credentials, processes or transport.
use super::*;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

const MAX_FILES: usize = 128;
const MAX_TAIL: u64 = 4 * 1024 * 1024;
const MAX_LINE: u64 = 256 * 1024;

pub struct CodexNativeProbe;
pub struct GrokBuildNativeProbe;
pub fn native_probes() -> Vec<Box<dyn QuotaProbe>> {
    vec![Box::new(CodexNativeProbe), Box::new(GrokBuildNativeProbe)]
}
fn digest(data: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(data.as_ref()))
}
fn stamp(v: &Value) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(v.as_str()?)
        .ok()
        .map(|t| t.timestamp_millis())
        .filter(|t| *t >= 0)
}
fn label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 120
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:/".contains(&b))
}
fn empty(id: &str, installed: bool, request: &ProbeRequest) -> ProbeReport {
    ProbeReport {
        probe_id: id.into(),
        version: 1,
        started_at: 0,
        collected_at: request.as_of,
        source: ObservationSource {
            kind: "native_local_cache".into(),
            schema_version: 1,
            event_id: String::new(),
        },
        completeness: if installed {
            Completeness::NoObservation
        } else {
            Completeness::NotInstalled
        },
        bindings: vec![],
        buckets: vec![],
        subscriptions: vec![],
        rate_limits: vec![],
        availability: vec![],
        issues: vec![],
    }
}
fn finish(mut r: ProbeReport, unsupported: bool) -> Result<ProbeReport, ProbeError> {
    if !r.buckets.is_empty() {
        r.completeness = Completeness::Partial;
    } else if unsupported {
        r.completeness = Completeness::Unsupported;
    }
    // Hash sanitized evidence, omitting collection time for idempotent successful refresh.
    let collected = r.collected_at;
    if !r.buckets.is_empty() {
        r.collected_at = r.started_at;
    }
    r.source.event_id = digest(serde_json::to_vec(&r).expect("report serialization"));
    r.collected_at = collected;
    super::snapshot::validate(&mut r).map_err(|_| ProbeError {
        kind: ProbeErrorKind::InvalidData,
        probe_id: r.probe_id.clone(),
        retryable: false,
        retry_at: None,
        diagnostic: "unsupported native cache evidence".into(),
    })?;
    Ok(r)
}
fn capabilities() -> ProbeCapabilities {
    ProbeCapabilities {
        local_snapshot: true,
        subscription: false,
        quota: true,
        rate_limit: true,
        schema_versions: vec![1],
    }
}
// Skip discovered symlink files/directories; an explicit root may be a symlink.
fn files(root: &Path, depth: usize, out: &mut Vec<PathBuf>, budget: &mut usize) {
    if depth == 0
        || *budget == 0
        || out.len() >= MAX_FILES
        || !std::fs::symlink_metadata(root).is_ok_and(|m| m.file_type().is_dir())
    {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut entries: Vec<_> = entries
        .take((*budget).min(512))
        .filter_map(Result::ok)
        .collect();
    *budget = budget.saturating_sub(entries.len());
    entries.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
    for entry in entries {
        if out.len() >= MAX_FILES {
            break;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            files(&entry.path(), depth - 1, out, budget);
        } else if kind.is_file() && entry.path().extension().is_some_and(|e| e == "jsonl") {
            out.push(entry.path());
        }
    }
}
fn regular(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}
fn read_lines(file: &mut File, offset: u64, max: u64) -> std::io::Result<Vec<Value>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::new(file.take(max));
    let mut result = Vec::new();
    let mut buf = Vec::new();
    if offset > 0 {
        let skipped = reader
            .by_ref()
            .take(MAX_LINE + 1)
            .read_until(b'\n', &mut buf)?;
        if skipped as u64 > MAX_LINE {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
    }
    loop {
        buf.clear();
        let read = reader
            .by_ref()
            .take(MAX_LINE + 1)
            .read_until(b'\n', &mut buf)?;
        if read == 0 {
            break;
        }
        if read as u64 > MAX_LINE {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        if buf.last() != Some(&b'\n') {
            break;
        }
        if let Ok(value) = serde_json::from_slice(&buf) {
            result.push(value);
        }
    }
    Ok(result)
}
fn lines(path: &Path) -> std::io::Result<Vec<Value>> {
    if !regular(path) {
        return Ok(vec![]);
    }
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let offset = len.saturating_sub(MAX_TAIL);
    let mut events = if offset > MAX_LINE {
        // Session metadata is at the start; recent windows are normally at the tail.
        read_lines(&mut file, 0, MAX_LINE)?
    } else {
        vec![]
    };
    events.extend(read_lines(
        &mut file,
        if offset <= MAX_LINE { 0 } else { offset },
        MAX_TAIL + MAX_LINE,
    )?);
    Ok(events)
}

fn target(
    agent: AgentId,
    root: &Path,
    scope: &str,
    provider: &str,
    model: &str,
) -> ExecutionTarget {
    let instance = digest(root.as_os_str().as_encoded_bytes());
    ExecutionTarget {
        agent,
        instance: instance.clone(),
        model: crate::model_id::canonical(provider, model),
        route: "unresolved-local-session".into(),
        account: format!("unresolved-{}", digest(format!("{instance}:{scope}"))),
        raw_provider: provider.into(),
        raw_model: model.into(),
        profile: None,
        billing_mode: None,
    }
}
struct PercentEvidence<'a> {
    used: Option<&'a Value>,
    duration: Option<i64>,
    reset: Option<i64>,
    at: i64,
}
fn percent(
    account: &str,
    key: String,
    window: &str,
    evidence: PercentEvidence<'_>,
    req: &ProbeRequest,
) -> Option<QuotaBucket> {
    let PercentEvidence {
        used,
        duration,
        reset,
        at,
    } = evidence;
    let used = used?.as_number()?.to_string();
    let amount = if let Some(n) = used.strip_prefix('-') {
        -decimal(n).ok()?
    } else {
        decimal(&used).ok()?
    };
    let remaining = (0..=100_000_000).contains(&amount).then(|| {
        format!(
            "{}.{:06}",
            (100_000_000 - amount) / 1_000_000,
            (100_000_000 - amount) % 1_000_000
        )
    });
    let mut flags = vec!["identity_unresolved".into()];
    if remaining.is_none() {
        flags.push("percentage_out_of_range".into());
    }
    Some(QuotaBucket {
        pool_key: key,
        account: account.into(),
        metric: "subscription_usage".into(),
        unit: Unit::Percent,
        window_key: window.into(),
        window_kind: WindowKind::Unknown,
        duration_ms: duration,
        state: if remaining.is_some() {
            AmountState::Known
        } else {
            AmountState::Unknown
        },
        used: Some(used),
        remaining,
        hard_limit: None,
        enforcement: Enforcement::Unknown,
        observed_at: at,
        valid_until: Some(
            at.saturating_add(req.max_age_ms.max(0))
                .min(reset.unwrap_or(i64::MAX)),
        ),
        reset_at: reset,
        confidence: Confidence::Unknown,
        quality_flags: flags,
    })
}

struct CachedLimit {
    observed: i64,
    seen: i64,
    value: Value,
    provider: Option<String>,
    model: Option<String>,
}

impl QuotaProbe for CodexNativeProbe {
    fn id(&self) -> ProbeId {
        "codex-native".into()
    }
    fn capabilities(&self) -> ProbeCapabilities {
        capabilities()
    }
    fn bindings(&self, env: &Env) -> Result<BindingReport, ProbeError> {
        let r = self.observe(
            &ProbeContext { env },
            &ProbeRequest {
                access: AccessPolicy::LocalOnly,
                as_of: now(),
                max_age_ms: 60_000,
            },
        )?;
        Ok(BindingReport {
            bindings: r.bindings,
            ..Default::default()
        })
    }
    fn observe(
        &self,
        ctx: &ProbeContext<'_>,
        req: &ProbeRequest,
    ) -> Result<ProbeReport, ProbeError> {
        let root = ctx.env.dir("CODEX_HOME", ".codex");
        let mut r = empty(
            &self.id(),
            root.is_dir() || ctx.env.which("codex").is_some(),
            req,
        );
        let mut paths = vec![];
        files(&root.join("sessions"), 5, &mut paths, &mut 4096);
        let mut unsupported = false;
        for path in paths {
            let Ok(events) = lines(&path) else {
                unsupported = true;
                continue;
            };
            let scope = digest(path.as_os_str().as_encoded_bytes());
            let mut provider = None;
            let mut model = None;
            let mut latest: BTreeMap<String, CachedLimit> = BTreeMap::new();
            for e in events {
                let p = &e["payload"];
                match e["type"].as_str() {
                    Some("session_meta") => {
                        provider = p["model_provider"].as_str().map(str::to_owned);
                    }
                    Some("turn_context") => {
                        model = p["model"].as_str().filter(|s| label(s)).map(str::to_owned);
                    }
                    Some("event_msg")
                        if p["type"] == "token_count" && !p["rate_limits"].is_null() =>
                    {
                        if let Some(at) = stamp(&e["timestamp"]).filter(|at| *at <= req.as_of) {
                            let limits = &p["rate_limits"];
                            let id = match limits.get("limit_id") {
                                None | Some(Value::Null) => "codex",
                                Some(Value::String(id)) if label(id) => id,
                                _ => {
                                    unsupported = true;
                                    continue;
                                }
                            };
                            let old = latest.get(id);
                            if old.is_none_or(|old| old.seen <= at) {
                                // Codex may re-emit a cached limit alongside newer token usage.
                                let observed = old
                                    .filter(|old| {
                                        old.value["primary"] == limits["primary"]
                                            && old.value["secondary"] == limits["secondary"]
                                    })
                                    .map_or(at, |old| old.observed);
                                latest.insert(
                                    id.into(),
                                    CachedLimit {
                                        observed,
                                        seen: at,
                                        value: limits.clone(),
                                        provider: provider.clone(),
                                        model: model.clone(),
                                    },
                                );
                            }
                        } else {
                            unsupported = true;
                        }
                    }
                    _ => {}
                }
            }
            for (limit_id, cached) in latest {
                let CachedLimit {
                    observed: at,
                    value: limits,
                    provider,
                    model,
                    ..
                } = cached;
                // Provider and model evidence must come from this rollout, never current auth/config.
                let (Some(provider), Some(model)) = (provider, model) else {
                    unsupported = true;
                    continue;
                };
                if provider != "openai" {
                    unsupported = true;
                    continue;
                }
                let t = target(AgentId::Codex, &root, &scope, &provider, &model);
                let mut buckets = vec![];
                for name in ["primary", "secondary"] {
                    let w = &limits[name];
                    if w.is_null() {
                        continue;
                    }
                    let duration = w["window_minutes"]
                        .as_i64()
                        .filter(|n| *n > 0)
                        .and_then(|n| n.checked_mul(60_000));
                    let reset = w["resets_at"]
                        .as_i64()
                        .filter(|n| *n >= 0)
                        .and_then(|n| n.checked_mul(1000));
                    if duration.is_none() || reset.is_none() {
                        unsupported = true;
                        continue;
                    }
                    if let Some(b) = percent(
                        &t.account,
                        format!("codex-{limit_id}-{name}"),
                        name,
                        PercentEvidence {
                            used: w.get("used_percent"),
                            duration,
                            reset,
                            at,
                        },
                        req,
                    ) {
                        buckets.push(b);
                    } else {
                        unsupported = true;
                    }
                }
                if buckets.is_empty() {
                    unsupported = true;
                    continue;
                }
                r.started_at = r.started_at.max(at);
                r.bindings.push(AccountBinding {
                    target: t,
                    pool_keys: buckets.iter().map(|b| b.pool_key.clone()).collect(),
                });
                r.buckets.extend(buckets);
            }
        }
        finish(r, unsupported)
    }
}

impl QuotaProbe for GrokBuildNativeProbe {
    fn id(&self) -> ProbeId {
        "grok-build-native".into()
    }
    fn capabilities(&self) -> ProbeCapabilities {
        capabilities()
    }
    fn bindings(&self, env: &Env) -> Result<BindingReport, ProbeError> {
        let r = self.observe(
            &ProbeContext { env },
            &ProbeRequest {
                access: AccessPolicy::LocalOnly,
                as_of: now(),
                max_age_ms: 60_000,
            },
        )?;
        Ok(BindingReport {
            bindings: r.bindings,
            ..Default::default()
        })
    }
    fn observe(
        &self,
        ctx: &ProbeContext<'_>,
        req: &ProbeRequest,
    ) -> Result<ProbeReport, ProbeError> {
        let root = ctx.env.dir("GROK_HOME", ".grok");
        let mut r = empty(
            &self.id(),
            root.is_dir() || ctx.env.which("grok").is_some(),
            req,
        );
        let logs = root.join("logs");
        if std::fs::symlink_metadata(&logs).is_ok_and(|m| m.file_type().is_symlink()) {
            return finish(r, true);
        }
        let log = logs.join("unified.jsonl");
        let events = match lines(&log) {
            Ok(v) => v,
            Err(_) => return finish(r, true),
        };
        let mut latest: Option<(i64, Value)> = None;
        let mut unsupported = false;
        for e in events {
            if e["src"] != "shell"
                || e["lvl"] != "info"
                || e["msg"] != "billing: fetched credits config"
            {
                continue;
            }
            if let Some(at) = stamp(&e["ts"]).filter(|at| *at <= req.as_of) {
                if latest.as_ref().is_none_or(|(old, _)| *old <= at) {
                    latest = Some((at, e["ctx"]["config"].clone()));
                }
            } else {
                unsupported = true;
            }
        }
        let Some((at, config)) = latest else {
            return finish(r, unsupported);
        };
        let period = &config["currentPeriod"];
        let (window, duration) = match period["type"].as_str() {
            Some("USAGE_PERIOD_TYPE_WEEKLY") => ("weekly", Some(7 * 24 * 60 * 60 * 1000)),
            Some("USAGE_PERIOD_TYPE_MONTHLY") => ("monthly", None),
            _ => return finish(r, true),
        };
        let (Some(start), Some(end)) = (stamp(&period["start"]), stamp(&period["end"])) else {
            return finish(r, true);
        };
        if start >= end || at < start {
            return finish(r, true);
        }
        // The log omits account identity. Isolate each event so login switches cannot inherit pools.
        let scope = format!("billing:{at}");
        let mut models = BTreeMap::new();
        let cfg_path = root.join("config.toml");
        if regular(&cfg_path) {
            let mut text = String::new();
            if File::open(&cfg_path)
                .and_then(|f| f.take(MAX_LINE + 1).read_to_string(&mut text))
                .is_ok()
                && text.len() as u64 <= MAX_LINE
            {
                if let Ok(cfg) = text.parse::<toml::Table>() {
                    config_models(&cfg, &mut models);
                }
            }
        }
        // An explicit unknown model keeps a native observation visible without claiming execution routing.
        if models.is_empty() {
            models.insert("unknown".into(), ());
        }
        for model in models.keys() {
            let t = target(AgentId::GrokBuild, &root, &scope, "xai", model);
            r.bindings.push(AccountBinding {
                target: t,
                pool_keys: vec![format!("grok-build-{window}")],
            });
        }
        let account = &r.bindings[0].target.account;
        let Some(bucket) = percent(
            account,
            format!("grok-build-{window}"),
            window,
            PercentEvidence {
                used: config.get("creditUsagePercent"),
                duration,
                reset: Some(end),
                at,
            },
            req,
        ) else {
            r.bindings.clear();
            return finish(r, true);
        };
        r.buckets.push(bucket);
        r.started_at = at;
        finish(r, unsupported)
    }
}
fn config_models(table: &toml::Table, out: &mut BTreeMap<String, ()>) {
    for (key, value) in table {
        match value {
            toml::Value::String(s) if (key == "model" || key.ends_with("_model")) && label(s) => {
                out.insert(s.clone(), ());
            }
            toml::Value::Table(t) => config_models(t, out),
            _ => {}
        }
    }
}
