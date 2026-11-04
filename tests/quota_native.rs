use rostr::{discover::Env, quota::*, store::Store};
use std::{fs, path::Path, process::Command};

const OBS: i64 = 1793754000000;
const CODEX: &str = include_str!("fixtures/quota/native/codex-rollout.jsonl");
const GROK: &str = include_str!("fixtures/quota/native/grok-unified.jsonl");

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}
fn env(home: &Path) -> Env {
    Env {
        home: home.into(),
        vars: Default::default(),
    }
}
fn observe(probe: &dyn QuotaProbe, env: &Env) -> ProbeReport {
    probe
        .observe(
            &ProbeContext { env },
            &ProbeRequest {
                access: AccessPolicy::LocalOnly,
                as_of: OBS,
                max_age_ms: 60_000,
            },
        )
        .unwrap()
}
fn query() -> TargetQuery {
    TargetQuery {
        agent: None,
        models: vec![],
        requirements: vec![QuotaRequirement {
            metric: "subscription_usage".into(),
            unit: Unit::Percent,
            min_remaining: "20".into(),
        }],
        min_confidence: ConfidencePolicy::Any,
        unknown: UnknownPolicy::Include,
        billing: BillingPolicy::Any,
        max_age_ms: 60_000,
        as_of: OBS,
    }
}
fn assert_unresolved(report: &ProbeReport) {
    assert!(matches!(report.completeness, Completeness::Partial));
    for b in &report.buckets {
        assert!(b.quality_flags.iter().any(|f| f == "identity_unresolved"));
        assert_eq!(b.confidence, Confidence::Unknown);
        assert_eq!(b.enforcement, Enforcement::Unknown);
        assert!(b.hard_limit.is_none());
    }
    assert!(
        report
            .bindings
            .iter()
            .all(|b| b.target.account.starts_with("unresolved-"))
    );
}
#[test]
fn codex_preserves_native_windows_and_original_time() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), ".codex/sessions/2026/11/04/rollout.jsonl", CODEX);
    let r = observe(&CodexNativeProbe, &env(d.path()));
    assert_unresolved(&r);
    assert_eq!(r.buckets.len(), 2);
    assert_eq!(r.buckets[0].remaining.as_deref(), Some("75.000000"));
    assert_eq!(r.buckets[1].remaining.as_deref(), Some("27.500000"));
    assert_eq!(r.buckets[0].duration_ms, Some(300 * 60_000));
    assert_eq!(r.buckets[1].duration_ms, Some(10080 * 60_000));
    assert_eq!(r.buckets[0].reset_at, Some(1793761200000));
    assert_eq!(r.buckets[0].observed_at, OBS);
    assert_eq!(r.buckets[0].valid_until, Some(OBS + 60_000));
    assert_eq!(r.bindings[0].target.model, "openai/gpt-5.3-codex");
    let mut store = Store::open(&d.path().join("quota.db")).unwrap();
    store.record_probe(&r).unwrap();
    assert_eq!(
        store.query_targets(&query()).unwrap()[0].eligibility,
        Eligibility::Unknown
    );
    let mut q = query();
    q.requirements[0].unit = Unit::TotalTokens;
    assert_eq!(
        store.query_targets(&q).unwrap()[0].eligibility,
        Eligibility::Unknown
    );
}
#[test]
fn grok_models_share_one_event_pool_without_claiming_account_identity() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), ".grok/logs/unified.jsonl", GROK);
    write(
        d.path(),
        ".grok/config.toml",
        "model = \"grok-4.7\"\n[ui]\nfork_secondary_model = \"grok-4.6\"\n",
    );
    let r = observe(&GrokBuildNativeProbe, &env(d.path()));
    assert_unresolved(&r);
    assert_eq!(r.bindings.len(), 2);
    assert_eq!(r.buckets.len(), 1);
    assert_eq!(r.buckets[0].window_key, "weekly");
    assert_eq!(r.buckets[0].remaining.as_deref(), Some("60.000000"));
    assert_eq!(r.bindings[0].target.account, r.bindings[1].target.account);
    assert_eq!(r.bindings[0].pool_keys, r.bindings[1].pool_keys);
    let mut store = Store::open(&d.path().join("quota.db")).unwrap();
    store.record_probe(&r).unwrap();
    store.record_probe(&r).unwrap();
    let c = rusqlite::Connection::open(d.path().join("quota.db")).unwrap();
    let count: i64 = c
        .query_row("SELECT count(*) FROM quota_samples", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
    let targets = store.query_targets(&query()).unwrap();
    assert_eq!(targets.len(), 2);
    assert!(
        targets
            .iter()
            .all(|t| t.eligibility == Eligibility::Unknown)
    );
    assert_eq!(targets[0].pools[0].pool_key, targets[1].pools[0].pool_key);
    let encoded = serde_json::to_string(&r).unwrap();
    assert!(!encoded.contains("NEVER_PERSIST_PROMPT"));
    assert!(!encoded.contains("prepaidBalance"));
}
#[test]
fn session_totals_and_absent_agents_never_become_balances() {
    let d = tempfile::tempdir().unwrap();
    for p in native_probes() {
        assert!(matches!(
            observe(p.as_ref(), &env(d.path())).completeness,
            Completeness::NotInstalled
        ));
    }
    write(
        d.path(),
        ".codex/sessions/token.jsonl",
        include_str!("fixtures/quota/native/codex-token-only.jsonl"),
    );
    write(
        d.path(),
        ".grok/sessions/session/signals.json",
        include_str!("fixtures/quota/native/grok-signals.json"),
    );
    write(d.path(), ".grok/config.toml", "model = \"grok-4.7\"\n");
    for p in native_probes() {
        let r = observe(p.as_ref(), &env(d.path()));
        assert!(matches!(r.completeness, Completeness::NoObservation));
        assert!(r.buckets.is_empty());
        assert!(r.bindings.is_empty());
    }
}
#[test]
fn incompatible_schemas_and_percent_anomalies_stay_unknown() {
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        ".codex/sessions/rollout.jsonl",
        &CODEX.replace("window_minutes", "window_seconds"),
    );
    write(
        d.path(),
        ".grok/logs/unified.jsonl",
        &GROK.replace("USAGE_PERIOD_TYPE_WEEKLY", "USAGE_PERIOD_TYPE_DAILY"),
    );
    for p in native_probes() {
        let r = observe(p.as_ref(), &env(d.path()));
        assert!(matches!(r.completeness, Completeness::Unsupported));
        assert!(r.buckets.is_empty());
    }
    write(
        d.path(),
        ".codex/sessions/rollout.jsonl",
        &CODEX.replace("\"used_percent\":25", "\"used_percent\":125"),
    );
    write(
        d.path(),
        ".grok/logs/unified.jsonl",
        &GROK.replace("\"creditUsagePercent\":40", "\"creditUsagePercent\":-5"),
    );
    for p in native_probes() {
        let r = observe(p.as_ref(), &env(d.path()));
        assert_unresolved(&r);
        let b = &r.buckets[0];
        assert_eq!(b.state, AmountState::Unknown);
        assert!(b.remaining.is_none());
        assert_ne!(b.state, AmountState::Unlimited);
        assert!(
            b.quality_flags
                .iter()
                .any(|f| f == "percentage_out_of_range")
        );
    }
}
#[test]
fn configured_homes_and_credentials_do_not_change_native_evidence() {
    let d = tempfile::tempdir().unwrap();
    let codex = d.path().join("codex-custom");
    let grok = d.path().join("grok-custom");
    write(&codex, "sessions/rollout.jsonl", CODEX);
    write(&grok, "logs/unified.jsonl", GROK);
    let mut e = env(d.path());
    e.vars
        .insert("CODEX_HOME".into(), codex.to_string_lossy().into_owned());
    e.vars
        .insert("GROK_HOME".into(), grok.to_string_lossy().into_owned());
    let before: Vec<_> = native_probes()
        .iter()
        .map(|p| observe(p.as_ref(), &e))
        .collect();
    fs::write(codex.join("auth.json"), b"\xffSECRET_TOKEN_NOT_JSON").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(codex.join("auth.json"), grok.join("auth.json")).unwrap();
    #[cfg(not(unix))]
    fs::write(grok.join("auth.json"), b"\xffSECRET_TOKEN_NOT_JSON").unwrap();
    let after: Vec<_> = native_probes()
        .iter()
        .map(|p| observe(p.as_ref(), &e))
        .collect();
    assert_eq!(
        serde_json::to_string(&before).unwrap(),
        serde_json::to_string(&after).unwrap()
    );
    assert!(
        !serde_json::to_string(&after)
            .unwrap()
            .contains("SECRET_TOKEN")
    );
}
fn cli(home: &Path, bin: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rostr"))
        .arg("quota")
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("PATH", bin)
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .output()
        .unwrap()
}
#[test]
fn local_refresh_runs_file_readers_is_idempotent_and_never_starts_agents() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), ".codex/sessions/rollout.jsonl", CODEX);
    write(d.path(), ".grok/logs/unified.jsonl", GROK);
    let bin = d.path().join("bin");
    let marker = d.path().join("agent-started");
    for name in ["claude", "codex", "grok", "curl", "wget"] {
        write(
            &bin,
            name,
            &format!(
                "#!/bin/sh\nprintf started > '{}'\nexit 99\n",
                marker.display()
            ),
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    for _ in 0..2 {
        let out = cli(d.path(), &bin, &["refresh", "--local", "--json"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let reports: Vec<ProbeReport> = serde_json::from_slice(&out.stdout).unwrap();
        // Codex, Grok Build and the Claude status line snapshot (absent here).
        assert_eq!(reports.len(), 3);
        assert_eq!(reports.iter().map(|r| r.buckets.len()).sum::<usize>(), 3);
    }
    assert!(!marker.exists());
    let c = rusqlite::Connection::open(d.path().join(".local/share/rostr/rostr.db")).unwrap();
    let count: i64 = c
        .query_row("SELECT count(*) FROM quota_samples", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 3);
    let out = cli(
        d.path(),
        &bin,
        &[
            "query",
            "--agent",
            "codex",
            "--metric",
            "subscription_usage",
            "--unit",
            "percent",
            "--as-of",
            "1793754000000",
            "--json",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let targets: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(targets.as_array().unwrap().len(), 1);
    assert_eq!(targets[0]["eligibility"], "unknown");
    assert_eq!(targets[0]["pools"].as_array().unwrap().len(), 2);
    assert!(
        !cli(d.path(), &bin, &["refresh", "--network"])
            .status
            .success()
    );
    assert!(!cli(d.path(), &bin, &["refresh"]).status.success());
    assert!(!marker.exists());
}

#[test]
fn incomplete_tail_does_not_override_the_last_complete_observation() {
    let d = tempfile::tempdir().unwrap();
    let mut codex = CODEX.to_string();
    codex.push_str("{\"timestamp\":\"2026-11-04T01:00:01Z\",\"type\":\"event_msg\",\"payload\":");
    write(d.path(), ".codex/sessions/rollout.jsonl", &codex);
    let r = observe(&CodexNativeProbe, &env(d.path()));
    assert_unresolved(&r);
    assert_eq!(r.buckets.len(), 2);
    assert_eq!(r.buckets[0].observed_at, OBS);
    let mut grok = GROK.to_string();
    grok.push_str("{\"ts\":\"2026-11-04T01:00:01Z\",\"src\":\"shell\",\"ctx\":");
    write(d.path(), ".grok/logs/unified.jsonl", &grok);
    let r = observe(&GrokBuildNativeProbe, &env(d.path()));
    assert_unresolved(&r);
    assert_eq!(r.buckets[0].remaining.as_deref(), Some("60.000000"));
}

#[test]
fn unbound_accounts_are_isolated_between_rollouts_and_billing_events() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), ".codex/sessions/a.jsonl", CODEX);
    write(d.path(), ".codex/sessions/b.jsonl", CODEX);
    let r = observe(&CodexNativeProbe, &env(d.path()));
    assert_eq!(r.bindings.len(), 2);
    assert_ne!(r.bindings[0].target.account, r.bindings[1].target.account);
    write(d.path(), ".grok/logs/unified.jsonl", GROK);
    let a = observe(&GrokBuildNativeProbe, &env(d.path()));
    write(
        d.path(),
        ".grok/logs/unified.jsonl",
        &GROK.replace("2026-11-04T01:00:00Z", "2026-11-04T00:59:30Z"),
    );
    let b = observe(&GrokBuildNativeProbe, &env(d.path()));
    assert_ne!(a.bindings[0].target.account, b.bindings[0].target.account);
}

#[test]
fn codex_keeps_latest_windows_for_each_limit_id() {
    let d = tempfile::tempdir().unwrap();
    let first = CODEX.replace("2026-11-04T01:00:00Z", "2026-11-04T00:59:00Z");
    let mut second: serde_json::Value =
        serde_json::from_str(CODEX.lines().last().unwrap()).unwrap();
    second["timestamp"] = "2026-11-04T00:59:30Z".into();
    second["payload"]["rate_limits"]["limit_id"] = "codex-extra".into();
    second["payload"]["rate_limits"]["primary"]["used_percent"] = 10.into();
    second["payload"]["rate_limits"]["secondary"]["used_percent"] = 20.into();
    let latest = CODEX.replace("\"used_percent\":25", "\"used_percent\":33");
    let text = format!("{first}{second}\n{}\n", latest.lines().last().unwrap());
    write(d.path(), ".codex/sessions/rollout.jsonl", &text);
    let r = observe(&CodexNativeProbe, &env(d.path()));
    assert_unresolved(&r);
    assert_eq!(r.buckets.len(), 4);
    let buckets: std::collections::BTreeMap<_, _> =
        r.buckets.iter().map(|b| (b.pool_key.as_str(), b)).collect();
    assert_eq!(buckets["codex-codex-primary"].used.as_deref(), Some("33"));
    assert_eq!(buckets["codex-codex-primary"].observed_at, OBS);
    assert_eq!(
        buckets["codex-codex-extra-primary"].used.as_deref(),
        Some("10")
    );
    assert_eq!(
        buckets["codex-codex-extra-primary"].observed_at,
        OBS - 30_000
    );
    assert!(buckets.contains_key("codex-codex-secondary"));
    assert!(buckets.contains_key("codex-codex-extra-secondary"));
    assert!(r.buckets.iter().all(|b| b.account == r.buckets[0].account));
}

#[test]
fn repeated_cached_codex_limits_do_not_renew_observation_or_ttl() {
    let d = tempfile::tempdir().unwrap();
    let path = ".codex/sessions/rollout.jsonl";
    let original = CODEX.replace("2026-11-04T01:00:00Z", "2026-11-04T00:58:00Z");
    write(d.path(), path, &original);
    let before = observe(&CodexNativeProbe, &env(d.path()));
    let mut repeated: serde_json::Value =
        serde_json::from_str(CODEX.lines().last().unwrap()).unwrap();
    repeated["payload"]["info"]["total_token_usage"]["total_tokens"] = 99999.into();
    write(d.path(), path, &format!("{original}{repeated}\n"));
    let after = observe(&CodexNativeProbe, &env(d.path()));
    assert_unresolved(&after);
    assert_eq!(after.source.event_id, before.source.event_id);
    for b in &after.buckets {
        assert_eq!(b.observed_at, OBS - 120_000);
        assert_eq!(b.valid_until, Some(OBS - 60_000));
    }
    let mut store = Store::open(&d.path().join("quota.db")).unwrap();
    store.record_probe(&before).unwrap();
    store.record_probe(&after).unwrap();
    let targets = store.query_targets(&query()).unwrap();
    assert_eq!(targets[0].eligibility, Eligibility::Unknown);
    assert!(
        targets[0]
            .reasons
            .iter()
            .any(|r| r.contains("stale observation"))
    );
}

#[test]
fn grok_full_credit_usage_cannot_prove_a_hard_block() {
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        ".grok/logs/unified.jsonl",
        &GROK.replace("\"creditUsagePercent\":40", "\"creditUsagePercent\":100"),
    );
    let r = observe(&GrokBuildNativeProbe, &env(d.path()));
    assert_unresolved(&r);
    assert_eq!(r.buckets[0].state, AmountState::Known);
    assert_eq!(r.buckets[0].remaining.as_deref(), Some("0.000000"));
    assert_eq!(r.buckets[0].enforcement, Enforcement::Unknown);
    let mut store = Store::open(&d.path().join("quota.db")).unwrap();
    store.record_probe(&r).unwrap();
    assert_eq!(
        store.query_targets(&query()).unwrap()[0].eligibility,
        Eligibility::Unknown
    );
}

#[test]
fn large_codex_rollout_joins_prefix_metadata_with_recent_windows() {
    let d = tempfile::tempdir().unwrap();
    let fixture: Vec<_> = CODEX.lines().collect();
    let mut rollout = String::from(fixture[0]);
    rollout.push('\n');
    // Valid ignored lines keep the file large without a giant JSON value or many events.
    let padding = format!("{}{{}}\n", " ".repeat(4096));
    for _ in 0..1280 {
        rollout.push_str(&padding);
    }
    rollout.push_str(fixture[1]);
    rollout.push('\n');
    rollout.push_str(fixture[2]);
    rollout.push('\n');
    assert!(rollout.len() > 5 * 1024 * 1024);
    write(d.path(), ".codex/sessions/rollout.jsonl", &rollout);
    let r = observe(&CodexNativeProbe, &env(d.path()));
    assert_unresolved(&r);
    assert_eq!(r.buckets.len(), 2);
    assert_eq!(r.bindings[0].target.raw_provider, "openai");
    assert_eq!(r.bindings[0].target.model, "openai/gpt-5.3-codex");
    assert_eq!(r.buckets[0].observed_at, OBS);
    assert_eq!(r.buckets[0].remaining.as_deref(), Some("75.000000"));
}
