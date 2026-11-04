use rostr::{quota::*, store::Store};
use std::{fs, path::Path, process::Command};
const OBS: i64 = 1793754000000;
fn fixture() -> ProbeReport {
    import_file(
        Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/quota/claude-statusline.json"
        )),
        OBS + 1000,
    )
    .unwrap()
}
fn query() -> TargetQuery {
    TargetQuery {
        agent: None,
        min_confidence: ConfidencePolicy::Any,
        unknown: UnknownPolicy::Include,
        billing: BillingPolicy::Any,
        models: vec![],
        requirements: vec![QuotaRequirement {
            metric: "subscription_usage".into(),
            unit: Unit::Percent,
            min_remaining: "20".into(),
        }],
        max_age_ms: 120_000,
        as_of: OBS + 1000,
    }
}
fn database() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::open(&d.path().join("rostr.db")).unwrap();
    (d, s)
}
#[test]
fn multiwindow_percent_unknown_and_expiry() {
    let (_d, mut s) = database();
    let r = fixture();
    assert_eq!(r.buckets.len(), 2);
    assert_eq!(r.buckets[0].remaining.as_deref(), Some("35.000000"));
    s.record_probe(&r).unwrap();
    assert_eq!(
        s.query_targets(&query()).unwrap()[0].eligibility,
        Eligibility::Blocked
    );
    let mut q = query();
    q.as_of = OBS + 61_000;
    assert_eq!(
        s.query_targets(&q).unwrap()[0].eligibility,
        Eligibility::Unknown
    );
    let (_d, mut s) = database();
    let mut r = fixture();
    r.buckets.pop();
    r.bindings[0].pool_keys.pop();
    s.record_probe(&r).unwrap();
    assert_eq!(
        s.query_targets(&query()).unwrap()[0].eligibility,
        Eligibility::Eligible
    );
    q = query();
    q.requirements[0].unit = Unit::TotalTokens;
    assert_eq!(
        s.query_targets(&q).unwrap()[0].eligibility,
        Eligibility::Unknown
    );
    q = query();
    q.as_of = 1793761200001;
    assert_eq!(
        s.query_targets(&q).unwrap()[0].eligibility,
        Eligibility::Unknown
    );
}
#[test]
fn shared_pool_identity_dedup_and_atomic_failure() {
    let (d, mut s) = database();
    let mut r = fixture();
    let mut shared = r.bindings[0].clone();
    shared.target.agent = AgentId::OpenCode;
    shared.target.route = "reseller-subscription".into();
    r.bindings.push(shared);
    s.record_probe(&r).unwrap();
    s.record_probe(&r).unwrap();
    let conn = rusqlite::Connection::open(d.path().join("rostr.db")).unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM quota_samples", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(s.query_targets(&query()).unwrap().len(), 2);
    let mut invalid = r.clone();
    invalid.source.event_id = "bad".into();
    invalid.buckets[1].remaining = Some("NaN".into());
    assert!(s.record_probe(&invalid).is_err());
    assert_eq!(
        conn.query_row("SELECT count(*) FROM probe_runs", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let mut switch = r.clone();
    switch.source.event_id = "switch".into();
    switch.buckets.clear();
    switch.bindings.truncate(1);
    switch.bindings[0].target.account = "fixture-account-b".into();
    switch.bindings[0].pool_keys.clear();
    s.record_probe(&switch).unwrap();
    let all = s.query_targets(&query()).unwrap();
    assert_eq!(
        all.iter()
            .find(|t| t.target.account == "fixture-account-b")
            .unwrap()
            .eligibility,
        Eligibility::Unknown
    );
}
#[test]
fn missing_windows_retract_without_resurrecting_older_imports() {
    let (_d, mut s) = database();
    let mut r = fixture();
    s.record_probe(&r).unwrap();
    r.source.event_id = "second".into();
    r.started_at += 1000;
    r.collected_at += 1000;
    r.buckets.pop();
    s.record_probe(&r).unwrap();
    let out = s.query_targets(&query()).unwrap();
    assert_eq!(out[0].pools.len(), 1);
    assert_eq!(out[0].eligibility, Eligibility::Unknown);
    let mut older = fixture();
    older.source.event_id = "older".into();
    older.started_at -= 1000;
    for b in &mut older.buckets {
        b.observed_at -= 1000;
    }
    s.record_probe(&older).unwrap();
    assert_eq!(s.query_targets(&query()).unwrap()[0].pools.len(), 1);
}
#[test]
fn unknown_unlimited_and_exact_decimals_are_distinct() {
    let (_d, mut s) = database();
    let mut r = fixture();
    r.buckets.truncate(1);
    r.bindings[0].pool_keys.truncate(1);
    r.buckets[0].state = AmountState::Unknown;
    r.buckets[0].remaining = None;
    s.record_probe(&r).unwrap();
    assert_eq!(
        s.query_targets(&query()).unwrap()[0].eligibility,
        Eligibility::Unknown
    );
    r.source.event_id = "unlimited".into();
    r.started_at += 1;
    r.buckets[0].state = AmountState::Unlimited;
    s.record_probe(&r).unwrap();
    assert_eq!(
        s.query_targets(&query()).unwrap()[0].eligibility,
        Eligibility::Eligible
    );
    r.source.event_id = "decimal".into();
    r.started_at += 1;
    r.buckets[0].state = AmountState::Known;
    r.buckets[0].remaining = Some("9.999999".into());
    s.record_probe(&r).unwrap();
    let mut q = query();
    q.requirements[0].min_remaining = "10".into();
    assert_eq!(
        s.query_targets(&q).unwrap()[0].eligibility,
        Eligibility::Blocked
    );
}
#[test]
fn migration_preserves_discovery_and_readonly_does_not_migrate() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("rostr.db");
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute_batch("CREATE TABLE models(id TEXT PRIMARY KEY, name TEXT); INSERT INTO models VALUES ('a/b','B'); CREATE TABLE model_sources(model_id TEXT,agent TEXT,via TEXT); CREATE TABLE scans(id INTEGER PRIMARY KEY); INSERT INTO scans VALUES(7); PRAGMA user_version=1;").unwrap();
    let ro = Store::open_read_only(&path).unwrap().unwrap();
    assert_eq!(ro.known().unwrap().len(), 1);
    assert!(
        ro.query_targets(&query())
            .unwrap_err()
            .to_string()
            .contains("schema v2")
    );
    drop(ro);
    assert_eq!(
        c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(Store::open(&path).unwrap());
    assert_eq!(
        c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        rostr::store::SCHEMA_VERSION
    );
    assert_eq!(
        c.query_row("SELECT id FROM scans", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        7
    );
    let ro = Store::open_read_only(&path).unwrap().unwrap();
    assert_eq!(ro.known().unwrap().len(), 1);
}
fn cli(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rostr"))
        .arg("quota")
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "")
        .output()
        .unwrap()
}
#[test]
fn cli_is_local_and_query_does_not_write_scope_or_database() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let out = cli(h, &["query", "--json"]);
    assert!(out.status.success());
    assert!(!h.join(".local").exists());
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/quota/claude-statusline.json"
    );
    let out = cli(h, &["import", path]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let db = h.join(".local/share/rostr/rostr.db");
    let before = fs::read(&db).unwrap();
    let out = cli(
        h,
        &[
            "query",
            "--metric",
            "subscription_usage",
            "--unit",
            "percent",
            "--min-remaining",
            "20",
            "--as-of",
            "1793754001000",
            "--json",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("blocked"));
    assert_eq!(before, fs::read(db).unwrap());
    assert!(!h.join(".config").exists());
    assert!(!cli(h, &["refresh", "--network"]).status.success());
}
#[test]
fn local_probe_has_no_ambient_agent_or_secret_dependency() {
    let dir = tempfile::tempdir().unwrap();
    let env = rostr::discover::Env {
        home: dir.path().to_path_buf(),
        vars: Default::default(),
    };
    let p: Box<dyn QuotaProbe> = Box::new(LocalSnapshotProbe {
        path: Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/quota/claude-statusline.json"
        ))
        .into(),
    });
    assert!(p.capabilities().local_snapshot);
    assert_eq!(p.bindings(&env).unwrap().bindings.len(), 1);
    assert_eq!(
        p.observe(
            &ProbeContext { env: &env },
            &ProbeRequest {
                access: AccessPolicy::LocalOnly,
                as_of: OBS + 1000,
                max_age_ms: 60_000
            }
        )
        .unwrap()
        .buckets
        .len(),
        2
    );
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn confidence_billing_and_unknown_policy_are_explicit() {
    let (_d, mut s) = database();
    let mut r = fixture();
    r.buckets.truncate(1);
    r.bindings[0].pool_keys.truncate(1);
    s.record_probe(&r).unwrap();
    let mut q = query();
    q.min_confidence = ConfidencePolicy::AuthoritativeOnly;
    assert_eq!(
        s.query_targets(&q).unwrap()[0].eligibility,
        Eligibility::Unknown
    );
    q.unknown = UnknownPolicy::Exclude;
    assert!(s.query_targets(&q).unwrap().is_empty());
    q = query();
    q.billing = BillingPolicy::SubscriptionOnly;
    assert_eq!(
        s.query_targets(&q).unwrap()[0].eligibility,
        Eligibility::Eligible
    );
}
#[test]
fn generic_yaml_import_and_cooldown_do_not_become_quota_zero() {
    let d = tempfile::tempdir().unwrap();
    let mut r = fixture();
    r.buckets.truncate(1);
    r.bindings[0].pool_keys.truncate(1);
    r.rate_limits.push(RateLimitObservation {
        account: r.bindings[0].target.account.clone(),
        pool_key: None,
        blocked_until: OBS + 30_000,
        observed_at: OBS,
    });
    let yaml = format!(
        "version: 1\nreport:\n{}",
        serde_norway::to_string(&r)
            .unwrap()
            .lines()
            .map(|l| format!("  {l}\n"))
            .collect::<String>()
    );
    fs::write(d.path().join("snapshot.yaml"), yaml).unwrap();
    let imported = import_file(&d.path().join("snapshot.yaml"), OBS + 1000).unwrap();
    assert_eq!(imported.buckets[0].confidence, Confidence::UserSupplied);
    let mut s = Store::open(&d.path().join("rostr.db")).unwrap();
    s.record_probe(&imported).unwrap();
    let result = s.query_targets(&query()).unwrap();
    assert_eq!(result[0].eligibility, Eligibility::Blocked);
    assert_eq!(result[0].pools[0].remaining.as_deref(), Some("35.000000"));
    let mut q = query();
    q.as_of = OBS + 31_000;
    assert_eq!(
        s.query_targets(&q).unwrap()[0].eligibility,
        Eligibility::Eligible
    );
}

#[test]
fn unobserved_applicable_pool_is_not_an_absent_constraint() {
    let (_d, mut s) = database();
    let mut r = fixture();
    r.buckets.pop();
    s.record_probe(&r).unwrap();
    assert_eq!(
        s.query_targets(&query()).unwrap()[0].eligibility,
        Eligibility::Unknown
    );
}

#[test]
fn anomaly_is_preserved_and_failed_refresh_retains_prior_observation() {
    let d = tempfile::tempdir().unwrap();
    let json = include_str!("fixtures/quota/claude-statusline.json").replace("65,", "-5,");
    let path = d.path().join("anomaly.json");
    fs::write(&path, json).unwrap();
    let anomaly = import_file(&path, OBS + 1000).unwrap();
    assert_eq!(anomaly.buckets[0].used.as_deref(), Some("-5"));
    assert_eq!(anomaly.buckets[0].state, AmountState::Unknown);
    assert!(anomaly.buckets[0].remaining.is_none());
    let (_d, mut s) = database();
    let mut r = fixture();
    r.buckets.truncate(1);
    r.bindings[0].pool_keys.truncate(1);
    s.record_probe(&r).unwrap();
    r.source.event_id = "no-observation".into();
    r.started_at += 1;
    r.buckets.clear();
    r.completeness = Completeness::NoObservation;
    s.record_probe(&r).unwrap();
    let result = s.query_targets(&query()).unwrap();
    assert_eq!(result[0].pools[0].remaining.as_deref(), Some("35.000000"));
    assert!(
        result[0]
            .reasons
            .iter()
            .any(|r| r.contains("prior evidence retained"))
    );
}

#[test]
fn percent_anomalies_and_event_conflicts_cannot_prove_eligibility() {
    let (_d, mut s) = database();
    let mut r = fixture();
    r.buckets.truncate(1);
    r.bindings[0].pool_keys.truncate(1);
    r.buckets[0].remaining = Some("101".into());
    s.record_probe(&r).unwrap();
    assert_eq!(
        s.query_targets(&query()).unwrap()[0].eligibility,
        Eligibility::Unknown
    );
    r.buckets[0].remaining = Some("99".into());
    assert!(s.record_probe(&r).is_err());
    let out = s.query_targets(&query()).unwrap();
    assert_eq!(out[0].pools[0].remaining.as_deref(), Some("101"));
    assert_eq!(out[0].provenance[0].source.event_id, "claude-fixture-1");
}
