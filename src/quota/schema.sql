CREATE TABLE observation_sources (
    id INTEGER PRIMARY KEY, probe_id TEXT NOT NULL, event_id TEXT NOT NULL,
    kind TEXT NOT NULL, schema_version INTEGER NOT NULL,
    UNIQUE(probe_id, event_id)
);
CREATE TABLE probe_runs (
    id INTEGER PRIMARY KEY, source_id INTEGER NOT NULL REFERENCES observation_sources(id),
    probe_id TEXT NOT NULL, started_at INTEGER NOT NULL, collected_at INTEGER NOT NULL,
    status TEXT NOT NULL, report TEXT NOT NULL
);
CREATE TABLE agent_instances (id TEXT PRIMARY KEY, agent_id TEXT NOT NULL);
CREATE TABLE accounts (id TEXT PRIMARY KEY);
CREATE TABLE execution_targets (
    id TEXT PRIMARY KEY, instance_id TEXT NOT NULL REFERENCES agent_instances(id),
    account_id TEXT NOT NULL REFERENCES accounts(id), identity TEXT NOT NULL
);
CREATE TABLE quota_pools (
    id TEXT PRIMARY KEY, account_id TEXT NOT NULL REFERENCES accounts(id),
    pool_key TEXT NOT NULL, metric TEXT NOT NULL, unit TEXT NOT NULL,
    window_key TEXT NOT NULL, UNIQUE(account_id, pool_key, metric, unit, window_key)
);
CREATE TABLE target_pool_expectations (
    target_id TEXT NOT NULL REFERENCES execution_targets(id),
    pool_key TEXT NOT NULL,
    PRIMARY KEY(target_id, pool_key)
);
CREATE TABLE target_pool_bindings (
    target_id TEXT NOT NULL REFERENCES execution_targets(id),
    pool_id TEXT NOT NULL REFERENCES quota_pools(id),
    PRIMARY KEY(target_id, pool_id)
);
CREATE TABLE probe_accounts (
    probe_run_id INTEGER NOT NULL REFERENCES probe_runs(id), account_id TEXT NOT NULL REFERENCES accounts(id),
    PRIMARY KEY(probe_run_id, account_id)
);
CREATE TABLE quota_samples (
    id INTEGER PRIMARY KEY, pool_id TEXT NOT NULL REFERENCES quota_pools(id),
    probe_run_id INTEGER NOT NULL REFERENCES probe_runs(id), observed_at INTEGER NOT NULL,
    valid_until INTEGER NOT NULL, sample TEXT NOT NULL,
    UNIQUE(pool_id, probe_run_id)
);
CREATE TABLE subscription_samples (id INTEGER PRIMARY KEY, probe_run_id INTEGER NOT NULL REFERENCES probe_runs(id), sample TEXT NOT NULL);
CREATE TABLE rate_limit_samples (id INTEGER PRIMARY KEY, probe_run_id INTEGER NOT NULL REFERENCES probe_runs(id), sample TEXT NOT NULL);
CREATE TABLE availability_samples (id INTEGER PRIMARY KEY, probe_run_id INTEGER NOT NULL REFERENCES probe_runs(id), sample TEXT NOT NULL);
-- A later successful snapshot retracts omitted windows for that account/source.
-- Errors/no-observation do not erase successful history.
CREATE VIEW current_quota AS
SELECT s.*, r.probe_id, r.collected_at,
       json_extract(s.sample, '$.remaining') AS remaining,
       json_extract(s.sample, '$.used') AS used,
       json_extract(s.sample, '$.hard_limit') AS hard_limit,
       json_extract(s.sample, '$.state') AS amount_state,
       json_extract(s.sample, '$.unit') AS unit
FROM quota_samples s
JOIN probe_runs r ON r.id = s.probe_run_id
JOIN quota_pools p ON p.id = s.pool_id
WHERE NOT EXISTS (
    SELECT 1 FROM probe_runs newer JOIN probe_accounts a ON a.probe_run_id = newer.id
    WHERE a.account_id = p.account_id AND newer.probe_id = r.probe_id
      AND newer.status IN ('"complete"', '"partial"')
      AND (newer.started_at > r.started_at OR (newer.started_at = r.started_at AND newer.id > r.id))
)
AND NOT EXISTS (
    SELECT 1 FROM quota_samples other JOIN probe_runs rr ON rr.id = other.probe_run_id
    WHERE other.pool_id = s.pool_id AND
      (other.observed_at > s.observed_at OR (other.observed_at = s.observed_at AND other.id > s.id))
);
CREATE VIEW effective_constraints AS
SELECT b.target_id, q.* FROM target_pool_bindings b JOIN current_quota q ON q.pool_id = b.pool_id;

CREATE INDEX quota_samples_pool_time ON quota_samples(pool_id, observed_at);
CREATE INDEX probe_runs_probe_time ON probe_runs(probe_id, started_at);
