-- Benchmark score snapshots (schema v3). One run is one parsed board snapshot
-- from one probe; rows keep the board's own model keys and native values.
-- Model mapping is resolved at query time from scores.yaml, so editing aliases
-- never needs a refresh.
CREATE TABLE score_runs (
    id INTEGER PRIMARY KEY,
    probe_id TEXT NOT NULL,
    parser_version INTEGER NOT NULL,
    retrieved_at INTEGER NOT NULL,
    origin TEXT NOT NULL,
    files TEXT NOT NULL,
    issues TEXT NOT NULL
);
CREATE TABLE score_rows (
    run_id INTEGER NOT NULL REFERENCES score_runs(id) ON DELETE CASCADE,
    ord INTEGER NOT NULL,
    metric TEXT NOT NULL,
    source_key TEXT NOT NULL,
    organization TEXT,
    value REAL NOT NULL,
    as_of TEXT,
    PRIMARY KEY (run_id, ord)
);
CREATE INDEX score_runs_probe ON score_runs(probe_id, id);
CREATE INDEX score_rows_metric ON score_rows(run_id, metric);
