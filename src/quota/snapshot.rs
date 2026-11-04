use super::*;
use anyhow::{Result, ensure};
use serde::Deserialize;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

const MAX_BYTES: u64 = 1024 * 1024;

/// An explicit user export, never an agent credential/cache search.
pub struct LocalSnapshotProbe {
    pub path: PathBuf,
}
impl QuotaProbe for LocalSnapshotProbe {
    fn id(&self) -> ProbeId {
        "local-snapshot".into()
    }
    fn capabilities(&self) -> ProbeCapabilities {
        ProbeCapabilities {
            local_snapshot: true,
            subscription: true,
            quota: true,
            rate_limit: true,
            schema_versions: vec![1],
        }
    }
    fn bindings(&self, _env: &Env) -> Result<BindingReport, ProbeError> {
        let report = import_file(&self.path, now()).map_err(error)?;
        Ok(BindingReport {
            bindings: report.bindings,
            ..Default::default()
        })
    }
    fn observe(
        &self,
        ctx: &ProbeContext<'_>,
        request: &ProbeRequest,
    ) -> Result<ProbeReport, ProbeError> {
        let _ = (ctx.env, request.access);
        let mut report = import_file(&self.path, request.as_of).map_err(error)?;
        for b in &mut report.buckets {
            b.valid_until = Some(
                b.valid_until
                    .unwrap_or(i64::MAX)
                    .min(b.observed_at.saturating_add(request.max_age_ms.max(0))),
            );
        }
        Ok(report)
    }
}
fn error(error: anyhow::Error) -> ProbeError {
    ProbeError {
        kind: match error
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind)
        {
            Some(std::io::ErrorKind::PermissionDenied) => ProbeErrorKind::PermissionDenied,
            Some(_) => ProbeErrorKind::Io,
            None => ProbeErrorKind::InvalidData,
        },
        probe_id: "local-snapshot".into(),
        retryable: false,
        retry_at: None,
        diagnostic: "invalid or unreadable local snapshot; check schema and permissions".into(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    #[serde(default)]
    report: Option<ProbeReport>,
    #[serde(default)]
    claude_statusline: Option<ClaudeExport>,
}
/// Rostr envelope supplies identity/time absent from native statusline stdin.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaudeExport {
    event_id: String,
    observed_at: Timestamp,
    target: ExecutionTarget,
    statusline: Statusline,
}
#[derive(Deserialize)]
struct Statusline {
    #[serde(default)]
    rate_limits: Windows,
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Windows {
    five_hour: Option<Window>,
    seven_day: Option<Window>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Window {
    used_percentage: serde_json::Number,
    resets_at: Option<i64>,
}

pub fn import_file(path: &Path, collected_at: Timestamp) -> Result<ProbeReport> {
    let mut text = String::new();
    File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_string(&mut text)?;
    ensure!(text.len() as u64 <= MAX_BYTES, "snapshot exceeds 1 MiB");
    // Never include parser errors containing untrusted input in diagnostics.
    let envelope: Envelope =
        serde_norway::from_str(&text).map_err(|_| anyhow::anyhow!("invalid snapshot schema"))?;
    ensure!(envelope.version == 1, "unsupported snapshot version");
    let mut report = match (envelope.report, envelope.claude_statusline) {
        (Some(report), None) => report,
        (None, Some(export)) => claude(export, collected_at)?,
        _ => anyhow::bail!("provide exactly one report or claude_statusline"),
    };
    report.collected_at = collected_at;
    // Exports are supplied by the user, not authenticated provider responses.
    for b in &mut report.buckets {
        b.confidence = Confidence::UserSupplied;
    }
    validate(&mut report)?;
    Ok(report)
}
fn claude(export: ClaudeExport, collected_at: Timestamp) -> Result<ProbeReport> {
    ensure!(
        export.target.agent == AgentId::ClaudeCode,
        "Claude export requires claude-code target"
    );
    let mut buckets = Vec::new();
    for (key, duration, window) in [
        (
            "five_hour",
            5 * 60 * 60 * 1000,
            export.statusline.rate_limits.five_hour,
        ),
        (
            "seven_day",
            7 * 24 * 60 * 60 * 1000,
            export.statusline.rate_limits.seven_day,
        ),
    ] {
        if let Some(w) = window {
            let used = w.used_percentage.to_string();
            let amount = signed_percent(&used)?;
            let remaining = ((0..=100_000_000).contains(&amount)).then(|| {
                format!(
                    "{}.{:06}",
                    (100_000_000 - amount) / 1_000_000,
                    (100_000_000 - amount) % 1_000_000
                )
            });
            buckets.push(QuotaBucket {
                pool_key: format!("claude-subscription-{key}"),
                account: export.target.account.clone(),
                metric: "subscription_usage".into(),
                unit: Unit::Percent,
                window_key: key.into(),
                window_kind: WindowKind::Rolling,
                duration_ms: Some(duration),
                state: if remaining.is_some() {
                    AmountState::Known
                } else {
                    AmountState::Unknown
                },
                used: Some(used),
                remaining,
                hard_limit: None,
                enforcement: Enforcement::Hard,
                observed_at: export.observed_at,
                valid_until: None,
                reset_at: w
                    .resets_at
                    .map(|v| {
                        v.checked_mul(1000)
                            .ok_or_else(|| anyhow::anyhow!("reset out of range"))
                    })
                    .transpose()?,
                confidence: Confidence::UserSupplied,
                quality_flags: if !(0..=100_000_000).contains(&amount) {
                    vec!["percentage_out_of_range".into()]
                } else {
                    vec![]
                },
            });
        }
    }
    Ok(ProbeReport {
        probe_id: "claude-statusline".into(),
        version: 1,
        started_at: export.observed_at,
        collected_at,
        source: ObservationSource {
            kind: "user_export".into(),
            schema_version: 1,
            event_id: export.event_id,
        },
        completeness: Completeness::Complete,
        bindings: vec![AccountBinding {
            target: export.target,
            pool_keys: buckets.iter().map(|b| b.pool_key.clone()).collect(),
        }],
        buckets,
        subscriptions: vec![],
        rate_limits: vec![],
        availability: vec![],
        issues: vec![],
    })
}
fn signed_percent(s: &str) -> Result<i128> {
    match s.strip_prefix('-') {
        Some(v) => Ok(-decimal(v)?),
        None => decimal(s),
    }
}
fn label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 160
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.:/".contains(&c))
}
pub(crate) fn validate(report: &mut ProbeReport) -> Result<()> {
    ensure!(
        report.version == 1 && report.source.schema_version == 1,
        "unsupported report version"
    );
    ensure!(
        label(&report.probe_id) && label(&report.source.event_id) && label(&report.source.kind),
        "invalid source identity"
    );
    ensure!(
        report.started_at >= 0 && report.started_at <= report.collected_at,
        "invalid observation time"
    );
    ensure!(
        report.issues.is_empty(),
        "free-form imported diagnostics are not accepted"
    );
    for binding in &report.bindings {
        let t = &binding.target;
        ensure!(
            [
                &t.instance,
                &t.model,
                &t.route,
                &t.account,
                &t.raw_provider,
                &t.raw_model
            ]
            .iter()
            .all(|s| label(s)),
            "use opaque non-secret identity labels"
        );
        ensure!(
            t.profile
                .iter()
                .chain(t.billing_mode.iter())
                .all(|s| label(s)),
            "invalid profile/billing mode"
        );
        ensure!(
            crate::model_id::validate_id(&t.model).is_ok(),
            "invalid canonical model ID"
        );
        ensure!(
            binding.pool_keys.iter().all(|s| label(s)),
            "invalid pool key"
        );
    }
    ensure!(
        matches!(
            report.completeness,
            Completeness::Complete | Completeness::Partial
        ) || (report.buckets.is_empty()
            && report.subscriptions.is_empty()
            && report.rate_limits.is_empty()
            && report.availability.is_empty()),
        "unsuccessful reports cannot publish samples"
    );
    let mut pools = std::collections::BTreeSet::new();
    for b in &mut report.buckets {
        ensure!(
            label(&b.pool_key) && label(&b.account) && label(&b.metric) && label(&b.window_key),
            "invalid pool identity"
        );
        ensure!(
            pools.insert((&b.account, &b.pool_key)),
            "duplicate pool in snapshot"
        );
        ensure!(
            report
                .bindings
                .iter()
                .any(|v| v.target.account == b.account && v.pool_keys.contains(&b.pool_key)),
            "pool requires explicit account binding"
        );
        for v in [&b.remaining, &b.hard_limit].into_iter().flatten() {
            let value = decimal(v)?;
            if b.unit == Unit::Percent
                && value > 100_000_000
                && !b
                    .quality_flags
                    .iter()
                    .any(|v| v == "percentage_out_of_range")
            {
                b.quality_flags.push("percentage_out_of_range".into());
            }
        }
        if let Some(used) = &b.used {
            if b.unit == Unit::Percent {
                let amount = signed_percent(used)?;
                if !(0..=100_000_000).contains(&amount)
                    && !b
                        .quality_flags
                        .iter()
                        .any(|v| v == "percentage_out_of_range")
                {
                    b.quality_flags.push("percentage_out_of_range".into());
                }
            } else {
                decimal(used)?;
            }
        }
        ensure!(
            (b.state == AmountState::Known || b.remaining.is_none())
                && (b.state != AmountState::Unlimited || b.hard_limit.is_none()),
            "unknown/unlimited cannot carry a remaining balance"
        );
        ensure!(
            b.observed_at >= 0 && b.observed_at <= report.started_at,
            "invalid observation time"
        );
        let expiry = b
            .valid_until
            .unwrap_or_else(|| b.observed_at.saturating_add(60_000))
            .min(b.reset_at.unwrap_or(i64::MAX));
        b.valid_until = Some(expiry);
        ensure!(
            b.quality_flags.iter().all(|s| label(s)),
            "invalid quality flag"
        );
        match &b.unit {
            Unit::Credits { namespace }
            | Unit::Opaque { name: namespace }
            | Unit::Money {
                currency: namespace,
            } => ensure!(label(namespace), "invalid unit namespace"),
            _ => {}
        }
    }
    for s in &report.subscriptions {
        ensure!(
            report
                .bindings
                .iter()
                .any(|b| b.target.account == s.account),
            "unbound subscription"
        );
        ensure!(
            s.plan
                .iter()
                .chain(s.status.iter())
                .chain(s.billing_mode.iter())
                .all(|v| label(v)),
            "invalid subscription label"
        );
        ensure!(
            s.observed_at >= 0 && s.observed_at <= report.started_at,
            "invalid subscription time"
        );
    }
    for r in &report.rate_limits {
        ensure!(
            report.bindings.iter().any(|b| b.target.account == r.account
                && r.pool_key.as_ref().is_none_or(|p| b.pool_keys.contains(p))),
            "unbound rate limit"
        );
        ensure!(
            r.observed_at >= 0 && r.observed_at <= report.started_at,
            "invalid rate limit time"
        );
    }
    for a in &report.availability {
        ensure!(
            report.bindings.iter().any(|b| b.target.id() == a.target_id),
            "unbound availability"
        );
        ensure!(
            a.observed_at >= 0 && a.observed_at <= report.started_at,
            "invalid availability time"
        );
    }
    Ok(())
}
