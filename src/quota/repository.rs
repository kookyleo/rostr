use super::*;
use crate::store::Store;
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};

pub trait QuotaRepository {
    fn record_probe(&mut self, report: &ProbeReport) -> Result<()>;
    fn query_targets(&self, query: &TargetQuery) -> Result<Vec<TargetQuota>>;
}
fn pool_id(b: &QuotaBucket) -> Result<String> {
    Ok(serde_json::to_string(&(
        &b.account,
        &b.pool_key,
        &b.metric,
        &b.unit,
        &b.window_key,
    ))?)
}
impl QuotaRepository for Store {
    fn record_probe(&mut self, report: &ProbeReport) -> Result<()> {
        let mut report = report.clone();
        super::snapshot::validate(&mut report)?;
        let tx = self.conn.transaction()?;
        let old: Option<String> = tx.query_row("SELECT r.report FROM probe_runs r JOIN observation_sources s ON s.id=r.source_id WHERE s.probe_id=?1 AND s.event_id=?2", params![report.probe_id, report.source.event_id], |r| r.get(0)).optional()?;
        if let Some(old) = old {
            let mut old: ProbeReport = serde_json::from_str(&old)?;
            old.collected_at = report.collected_at;
            ensure!(
                serde_json::to_string(&old)? == serde_json::to_string(&report)?,
                "event ID already exists with different observations"
            );
            return Ok(());
        }
        tx.execute("INSERT INTO observation_sources(probe_id,event_id,kind,schema_version) VALUES (?1,?2,?3,?4)", params![report.probe_id,report.source.event_id,report.source.kind,report.source.schema_version])?;
        let source = tx.last_insert_rowid();
        tx.execute("INSERT INTO probe_runs(source_id,probe_id,started_at,collected_at,status,report) VALUES (?1,?2,?3,?4,?5,?6)", params![source, report.probe_id, report.started_at, report.collected_at, serde_json::to_string(&report.completeness)?, serde_json::to_string(&report)?])?;
        let run = tx.last_insert_rowid();
        for binding in &report.bindings {
            let t = &binding.target;
            let instance = serde_json::to_string(&(&t.agent, &t.instance))?;
            tx.execute(
                "INSERT OR IGNORE INTO agent_instances VALUES (?1,?2)",
                params![instance, serde_json::to_string(&t.agent)?],
            )?;
            tx.execute("INSERT OR IGNORE INTO accounts VALUES (?1)", [&t.account])?;
            tx.execute(
                "INSERT OR IGNORE INTO execution_targets VALUES (?1,?2,?3,?1)",
                params![t.id(), instance, t.account],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO probe_accounts VALUES (?1,?2)",
                params![run, t.account],
            )?;
            for key in &binding.pool_keys {
                tx.execute(
                    "INSERT OR IGNORE INTO target_pool_expectations VALUES (?1,?2)",
                    params![t.id(), key],
                )?;
            }
        }
        for b in &report.buckets {
            let id = pool_id(b)?;
            tx.execute(
                "INSERT OR IGNORE INTO quota_pools VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    id,
                    b.account,
                    b.pool_key,
                    b.metric,
                    serde_json::to_string(&b.unit)?,
                    b.window_key
                ],
            )?;
            tx.execute("INSERT INTO quota_samples(pool_id,probe_run_id,observed_at,valid_until,sample) VALUES (?1,?2,?3,?4,?5)", params![id,run,b.observed_at,b.valid_until,serde_json::to_string(b)?])?;
            for binding in &report.bindings {
                if binding.target.account == b.account && binding.pool_keys.contains(&b.pool_key) {
                    tx.execute(
                        "INSERT OR IGNORE INTO target_pool_bindings VALUES (?1,?2)",
                        params![binding.target.id(), id],
                    )?;
                }
            }
        }
        // Bind targets to existing shared pools even when this report has no new sample.
        for b in &report.bindings {
            for key in &b.pool_keys {
                tx.execute("INSERT OR IGNORE INTO target_pool_bindings SELECT ?1,id FROM quota_pools WHERE account_id=?2 AND pool_key=?3", params![b.target.id(),b.target.account,key])?;
            }
        }
        for (table, samples) in [
            (
                "subscription_samples",
                report
                    .subscriptions
                    .iter()
                    .map(serde_json::to_string)
                    .collect::<serde_json::Result<Vec<_>>>()?,
            ),
            (
                "rate_limit_samples",
                report
                    .rate_limits
                    .iter()
                    .map(serde_json::to_string)
                    .collect::<serde_json::Result<Vec<_>>>()?,
            ),
            (
                "availability_samples",
                report
                    .availability
                    .iter()
                    .map(serde_json::to_string)
                    .collect::<serde_json::Result<Vec<_>>>()?,
            ),
        ] {
            for sample in samples {
                tx.execute(
                    &format!("INSERT INTO {table}(probe_run_id,sample) VALUES (?1,?2)"),
                    params![run, sample],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    fn query_targets(&self, q: &TargetQuery) -> Result<Vec<TargetQuota>> {
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        ensure!(
            version >= 2,
            "quota requires schema v2 or newer; run `rostr quota refresh --local` or `rostr quota import <snapshot>` to migrate; query is read-only"
        );
        ensure!(q.max_age_ms >= 0, "max age must be nonnegative");
        for r in &q.requirements {
            decimal(&r.min_remaining)?;
        }
        let tx = self.conn.unchecked_transaction()?;
        let targets: Vec<String> = tx
            .prepare("SELECT identity FROM execution_targets ORDER BY id")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut out = Vec::new();
        for identity in targets {
            let target: ExecutionTarget = serde_json::from_str(&identity)?;
            if q.agent.as_ref().is_some_and(|a| *a != target.agent)
                || (!q.models.is_empty() && !q.models.contains(&target.model))
            {
                continue;
            }
            let samples: Vec<(String,String)> = tx.prepare("SELECT e.sample,r.report FROM effective_constraints e JOIN probe_runs r ON r.id=e.probe_run_id WHERE e.target_id=?1 AND e.observed_at<=?2 ORDER BY e.pool_id")?.query_map(params![identity,q.as_of], |r|Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
            let mut pools = Vec::new();
            let mut provenance = Vec::new();
            for (sample, report) in samples {
                let bucket: QuotaBucket = serde_json::from_str(&sample)?;
                let report: ProbeReport = serde_json::from_str(&report)?;
                provenance.push(PoolProvenance {
                    pool_key: bucket.pool_key.clone(),
                    probe_id: report.probe_id,
                    source: report.source,
                    collected_at: report.collected_at,
                });
                pools.push(bucket);
            }
            let expected_keys: Vec<String> = tx
                .prepare("SELECT pool_key FROM target_pool_expectations WHERE target_id=?1")?
                .query_map([&identity], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let mut unknown =
                q.requirements.is_empty() || pools.is_empty() || pools.len() < expected_keys.len();
            if q.billing == BillingPolicy::SubscriptionOnly
                && target.billing_mode.as_deref() != Some("subscription")
            {
                unknown = true;
            }
            let mut blocked = false;
            let mut reasons = Vec::new();
            if unknown {
                reasons.push("no declared requirement or current quota evidence".into());
            }
            for b in &pools {
                let fresh = b.observed_at <= q.as_of
                    && q.as_of.saturating_sub(b.observed_at) <= q.max_age_ms
                    && b.valid_until.is_some_and(|v| v > q.as_of)
                    && b.reset_at.is_none_or(|v| v > q.as_of);
                if !fresh {
                    unknown = true;
                    reasons.push(format!("{}: stale observation", b.pool_key));
                    continue;
                }
                if q.min_confidence == ConfidencePolicy::AuthoritativeOnly
                    && b.confidence != Confidence::Authoritative
                {
                    unknown = true;
                    reasons.push(format!("{}: source confidence below policy", b.pool_key));
                }
                if b.enforcement == Enforcement::Advisory {
                    continue;
                }
                if b.enforcement == Enforcement::Unknown || !b.quality_flags.is_empty() {
                    unknown = true;
                }
                // Every applicable hard window participates, even if its metric is not requested.
                if b.enforcement == Enforcement::Hard
                    && b.state == AmountState::Known
                    && b.remaining
                        .as_deref()
                        .map(decimal)
                        .transpose()?
                        .is_some_and(|v| v == 0)
                {
                    blocked = true;
                    reasons.push(format!("{}: exhausted", b.pool_key));
                }
                if b.state == AmountState::Unknown
                    || b.state == AmountState::NotApplicable
                    || (b.state == AmountState::Known && b.remaining.is_none())
                {
                    unknown = true;
                }
            }
            for req in &q.requirements {
                let matched: Vec<_> = pools
                    .iter()
                    .filter(|b| {
                        b.metric == req.metric
                            && b.unit == req.unit
                            && b.enforcement != Enforcement::Advisory
                    })
                    .collect();
                if matched.is_empty() {
                    unknown = true;
                    reasons.push(format!("{}: no evidence in requested unit", req.metric));
                }
                for b in matched {
                    let fresh = b.valid_until.is_some_and(|v| v > q.as_of)
                        && q.as_of.saturating_sub(b.observed_at) <= q.max_age_ms;
                    if !fresh || b.enforcement != Enforcement::Hard {
                        continue;
                    }
                    if let Some(v) = &b.remaining {
                        if decimal(v)? < decimal(&req.min_remaining)? {
                            blocked = true;
                            reasons.push(format!("{}: below required remaining", b.pool_key));
                        }
                    } else if b.state != AmountState::Unlimited {
                        unknown = true;
                    }
                }
            }
            // Cooldown and availability are independent of quota, and newer evidence wins.
            let reports: Vec<String> = tx.prepare("SELECT report FROM probe_runs WHERE started_at<=?1 ORDER BY started_at DESC,id DESC")?.query_map([q.as_of],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
            let mut seen_limits = std::collections::BTreeSet::new();
            let mut seen_availability = false;
            let mut seen_probes = std::collections::BTreeSet::new();
            for json in reports {
                let r: ProbeReport = serde_json::from_str(&json)?;
                if r.bindings
                    .iter()
                    .any(|b| b.target.account == target.account)
                    && seen_probes.insert(r.probe_id.clone())
                    && !matches!(
                        r.completeness,
                        Completeness::Complete | Completeness::Partial
                    )
                {
                    reasons.push(format!(
                        "{}: latest probe produced no usable observation; prior evidence retained",
                        r.probe_id
                    ));
                }
                for limit in r.rate_limits.iter().filter(|l| l.account == target.account) {
                    if seen_limits.insert((r.probe_id.clone(), limit.pool_key.clone()))
                        && limit.blocked_until > q.as_of
                        && limit.observed_at <= q.as_of
                        && limit
                            .pool_key
                            .as_ref()
                            .is_none_or(|k| expected_keys.contains(k))
                    {
                        blocked = true;
                        reasons.push("active rate-limit cooldown".into());
                    }
                }
                for a in r.availability.iter().filter(|a| a.target_id == identity) {
                    if !seen_availability {
                        seen_availability = true;
                        if a.valid_until > q.as_of {
                            match a.blocked {
                                Some(true) => {
                                    blocked = true;
                                    reasons.push("availability blocked".into());
                                }
                                Some(false) => {}
                                None => unknown = true,
                            }
                        } else {
                            unknown = true;
                        }
                    }
                }
            }
            if unknown && reasons.is_empty() {
                reasons.push("constraint evidence is incomplete".into());
            }
            if unknown && !blocked && q.unknown == UnknownPolicy::Exclude {
                continue;
            }
            out.push(TargetQuota {
                target,
                pools,
                provenance,
                eligibility: if blocked {
                    Eligibility::Blocked
                } else if unknown {
                    Eligibility::Unknown
                } else {
                    Eligibility::Eligible
                },
                reasons,
                as_of: q.as_of,
            });
        }
        tx.commit()?;
        Ok(out)
    }
}
