use rostr::score::*;
use rostr::store::Store;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/score");

/// Builds a `benchmark_data.zip` from the plain-text fixture CSVs.
fn epoch_zip() -> Vec<u8> {
    let root = Path::new(FIXTURES).join("epoch");
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut files = vec![];
    for e in walk(&root) {
        files.push(e);
    }
    files.sort();
    for f in files {
        let name = f
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        zip.start_file(name, opts).unwrap();
        zip.write_all(&std::fs::read(&f).unwrap()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
}
impl Env {
    fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        Env { _dir: dir, root }
    }
    fn db(&self) -> PathBuf {
        self.root.join("data/rostr.db")
    }
    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_rostr"))
            .args(args)
            .env_clear()
            .env("HOME", &self.root)
            .env("ROSTR_DATA_DIR", self.root.join("data"))
            .env("ROSTR_CONFIG_DIR", self.root.join("config"))
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
    fn import_fixtures(&self) {
        let zip = self.root.join("benchmark_data.zip");
        std::fs::write(&zip, epoch_zip()).unwrap();
        self.ok(&[
            "score",
            "import",
            "--source",
            "epoch-ai",
            zip.to_str().unwrap(),
        ]);
        let lm = Path::new(FIXTURES).join("lmarena");
        self.ok(&[
            "score",
            "import",
            "--source",
            "lmarena",
            lm.to_str().unwrap(),
        ]);
    }
    fn query(&self, extra: &[&str]) -> Value {
        let mut args = vec!["score", "query", "--json"];
        args.extend_from_slice(extra);
        serde_json::from_str(&self.ok(&args)).unwrap()
    }
    fn overrides(&self, yaml: &str) {
        std::fs::create_dir_all(self.root.join("config")).unwrap();
        std::fs::write(self.root.join("config/scores.yaml"), yaml).unwrap();
    }
}

fn axis<'a>(v: &'a Value, model: &str, axis: &str) -> &'a Value {
    v.as_array()
        .unwrap()
        .iter()
        .find(|m| m["model"] == model)
        .unwrap()["scores"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["axis"] == axis)
        .unwrap()
}

fn reasons(s: &Value) -> String {
    s["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect::<Vec<_>>()
        .join("; ")
}

const MODELS: &[&str] = &[
    "--model",
    "anthropic/claude-opus-4-5",
    "--model",
    "openai/gpt-5",
    "--model",
    "xai/grok-4",
    "--model",
    "example/unknown-model",
];

#[test]
fn ranks_mapping_and_unknowns() {
    let env = Env::new();
    env.import_fixtures();
    let v = env.query(MODELS);

    // Highest-ranked source wins; provenance names the board row.
    let g = axis(&v, "anthropic/claude-opus-4-5", "general");
    assert_eq!(g["value"], 150.1);
    assert_eq!(g["state"], "known");
    assert_eq!(g["provenance"]["source"], "epoch-ai");
    assert_eq!(g["provenance"]["metric"], "eci");
    assert_eq!(g["provenance"]["unit"], "index");
    assert_eq!(g["provenance"]["source_key"], "Claude Opus 4.5");
    assert_eq!(g["provenance"]["matched_by"], "rule:exact");

    let c = axis(&v, "anthropic/claude-opus-4-5", "coding");
    assert_eq!(c["value"], 0.79);
    assert_eq!(c["provenance"]["source_key"], "claude-opus-4-5-20251101");
    assert_eq!(c["provenance"]["matched_by"], "rule:dateless");
    assert_eq!(c["provenance"]["as_of"], "2025-12-01");

    // A plain row wins over a tagged variant of the same snapshot.
    let r = axis(&v, "anthropic/claude-opus-4-5", "reasoning");
    assert_eq!(r["value"], 0.83);

    // No vision row and no Video-MME table: Unknown with null, never 0.
    let m = axis(&v, "anthropic/claude-opus-4-5", "multimodal");
    assert_eq!(m["state"], "unknown");
    assert!(m["value"].is_null());
    assert!(m["provenance"].is_null());
    assert!(
        reasons(m).contains("epoch-ai/video-mme: no rows"),
        "{}",
        reasons(m)
    );

    let i = axis(&v, "anthropic/claude-opus-4-5", "instruction");
    assert_eq!(i["value"], 1470.0);
    assert_eq!(i["provenance"]["source"], "lmarena");
    assert_eq!(i["provenance"]["unit"], "elo");

    // Two SWE-bench variants: ambiguous, and no fall-through to Aider.
    let c = axis(&v, "openai/gpt-5", "coding");
    assert_eq!(c["state"], "unknown");
    assert!(c["value"].is_null());
    assert!(reasons(c).contains("ambiguous variants"), "{}", reasons(c));
    // One variant only: matched by variant.
    let r = axis(&v, "openai/gpt-5", "reasoning");
    assert_eq!(r["value"], 0.86);
    assert_eq!(r["provenance"]["matched_by"], "rule:variant");
    assert_eq!(axis(&v, "openai/gpt-5", "multimodal")["value"], 1300.0);

    // Missing from the first-ranked board: the next one supplies the axis.
    let g = axis(&v, "xai/grok-4", "general");
    assert_eq!(g["provenance"]["source"], "lmarena");
    assert_eq!(g["value"], 1440.0);
    let c = axis(&v, "xai/grok-4", "coding");
    assert_eq!(c["provenance"]["metric"], "aider-polyglot");
    assert_eq!(c["value"], 70.0);

    for a in [
        "general",
        "coding",
        "reasoning",
        "multimodal",
        "instruction",
    ] {
        let s = axis(&v, "example/unknown-model", a);
        assert_eq!(s["state"], "unknown");
        assert!(s["value"].is_null());
    }

    // Text output and axis filter.
    let text = env.ok(&[
        "score",
        "query",
        "--model",
        "openai/gpt-5",
        "--axis",
        "reasoning",
    ]);
    assert!(text.contains("reasoning"), "{text}");
    assert!(text.contains("epoch-ai/gpqa-diamond"), "{text}");
    assert!(!text.contains("coding"), "{text}");
}

#[test]
fn overrides_alias_ignore_and_values() {
    let env = Env::new();
    env.import_fixtures();
    env.overrides(
        "version: 1\n\
         aliases:\n  - {model: openai/gpt-5, source: epoch-ai, metric: swe-bench-verified, key: gpt-5-2025-08-07_high}\n\
         ignore:\n  - {model: anthropic/claude-opus-4-5, source: epoch-ai, metric: eci}\n\
         values:\n  - {model: example/unknown-model, axis: coding, value: 0.5, metric: team-eval, note: internal}\n",
    );
    let v = env.query(MODELS);
    let c = axis(&v, "openai/gpt-5", "coding");
    assert_eq!(c["value"], 0.71);
    assert_eq!(c["provenance"]["matched_by"], "alias:scores.yaml");
    let g = axis(&v, "anthropic/claude-opus-4-5", "general");
    assert_eq!(g["provenance"]["source"], "lmarena");
    assert_eq!(g["value"], 1480.5);
    let m = axis(&v, "example/unknown-model", "coding");
    assert_eq!(m["value"], 0.5);
    assert_eq!(m["provenance"]["source"], "manual");
    assert_eq!(m["provenance"]["metric"], "team-eval");

    env.overrides("version: 1\nvalues:\n  - {model: a/b, axis: speed, value: 1}\n");
    let out = env.run(&["score", "query", "--model", "a/b"]);
    assert!(!out.status.success());
}

#[test]
fn stale_snapshots_and_readonly_query() {
    let env = Env::new();
    // No database: Unknown everywhere, and query creates nothing.
    let v = env.query(&["--model", "openai/gpt-5"]);
    assert_eq!(axis(&v, "openai/gpt-5", "general")["state"], "unknown");
    assert!(reasons(axis(&v, "openai/gpt-5", "general")).contains("rostr score refresh"));
    assert!(!env.db().exists());
    // No models selected and none given.
    assert!(!env.run(&["score", "query"]).status.success());

    env.import_fixtures();
    let far = (now() + 10 * 86_400_000).to_string();
    let v = env.query(&["--model", "openai/gpt-5", "--max-age", "1", "--as-of", &far]);
    let g = axis(&v, "openai/gpt-5", "general");
    assert_eq!(g["state"], "stale");
    assert_eq!(g["value"], 140.5);
}

#[test]
fn default_models_come_from_discover_selection() {
    let env = Env::new();
    env.import_fixtures();
    let c = rusqlite::Connection::open(env.db()).unwrap();
    c.execute_batch(
        "INSERT INTO models (id, provider, name, first_seen, selected, reason, resolved_at) VALUES
         ('openai/gpt-6-astra','openai','GPT 6 Astra','t',1,'pattern','t'),
         ('xai/grok-4','xai','Grok 4','t',0,'no match','t');",
    )
    .unwrap();
    let v = env.query(&[]);
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(axis(&v, "openai/gpt-6-astra", "general")["value"], 169.0);
    assert_eq!(env.query(&["--all"]).as_array().unwrap().len(), 2);
}

#[test]
fn sources_lists_ranks_and_snapshots() {
    let env = Env::new();
    let out = env.ok(&["score", "sources"]);
    assert!(out.contains("epoch-ai"), "{out}");
    assert!(out.contains("CC-BY-4.0"), "{out}");
    assert!(
        out.contains("instruction  lmarena/text-instruction-following"),
        "{out}"
    );
    env.import_fixtures();
    let v: Value = serde_json::from_str(&env.ok(&["score", "sources", "--json"])).unwrap();
    let epoch = &v["sources"][0];
    assert_eq!(epoch["id"], "epoch-ai");
    assert_eq!(epoch["last_rows"], 11);
    assert_eq!(v["ranks"][1]["axis"], "coding");
}

#[test]
fn schema_v3_keeps_quota_working() {
    let env = Env::new();
    env.import_fixtures();
    let quota = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/quota/claude-statusline.json"
    );
    env.ok(&["quota", "import", quota]);
    let c = rusqlite::Connection::open(env.db()).unwrap();
    let version: i64 = c
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 3);
    let out = env.ok(&["quota", "query", "--json", "--as-of", "1793754001000"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
}

#[test]
fn v2_database_migrates_on_write_only() {
    let env = Env::new();
    std::fs::create_dir_all(env.root.join("data")).unwrap();
    let c = rusqlite::Connection::open(env.db()).unwrap();
    c.execute_batch(include_str!("../src/quota/schema.sql"))
        .unwrap();
    c.execute_batch(
        "CREATE TABLE models (id TEXT PRIMARY KEY, provider TEXT, name TEXT, first_seen TEXT, last_seen TEXT, selected INTEGER, reason TEXT, resolved_at TEXT);
         PRAGMA user_version = 2;",
    )
    .unwrap();
    // Read-only query on v2: Unknown, schema untouched.
    let v = env.query(&["--model", "openai/gpt-5"]);
    assert_eq!(axis(&v, "openai/gpt-5", "general")["state"], "unknown");
    let version = |c: &rusqlite::Connection| -> i64 {
        c.pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap()
    };
    assert_eq!(version(&c), 2);
    env.import_fixtures();
    assert_eq!(version(&c), 3);
}

/// Serves fixture bytes by URL and records requests.
struct FakeFetcher {
    pages: HashMap<String, Vec<u8>>,
    seen: std::cell::RefCell<Vec<String>>,
}
impl Fetcher for FakeFetcher {
    fn get(&self, probe: &str, url: &str, max_bytes: u64) -> Result<Vec<u8>, ProbeError> {
        self.seen.borrow_mut().push(url.into());
        let body = self
            .pages
            .get(url)
            .cloned()
            .unwrap_or_else(|| br#"{"rows":[],"num_rows_total":0}"#.to_vec());
        if body.len() as u64 > max_bytes {
            return Err(ProbeError {
                kind: ProbeErrorKind::InvalidData,
                probe_id: probe.into(),
                retryable: false,
                retry_at: None,
                diagnostic: "too large".into(),
            });
        }
        Ok(body)
    }
}

fn lm_url(config: &str, offset: usize) -> String {
    format!(
        "https://datasets-server.huggingface.co/rows?dataset=lmarena-ai%2Fleaderboard-dataset&config={config}&split=latest&offset={offset}&length=100"
    )
}

fn lm_page(category: &str, total: usize, names: &[&str]) -> Vec<u8> {
    let rows: Vec<Value> = names
        .iter()
        .map(|n| {
            serde_json::json!({"row": {"model_name": n, "organization": "openai", "rating": 1200.0,
                "category": category, "leaderboard_publish_date": "2026-10-28"}})
        })
        .collect();
    serde_json::to_vec(&serde_json::json!({"rows": rows, "num_rows_total": total})).unwrap()
}

#[test]
fn refresh_fetches_declared_urls_and_pages() {
    let names: Vec<String> = (0..100).map(|i| format!("m-{i}")).collect();
    let first: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut pages = HashMap::new();
    pages.insert(lm_url("text", 0), lm_page("overall", 101, &first));
    pages.insert(
        lm_url("text", 100),
        lm_page("math", 101, &["skipped-category"]),
    );
    pages.insert(
        "https://epoch.ai/data/benchmark_data.zip".into(),
        epoch_zip(),
    );
    let fetcher = FakeFetcher {
        pages,
        seen: Default::default(),
    };

    let lm = LmArenaProbe;
    let files = lm.fetch(&fetcher).unwrap();
    // Two pages of the text config, then one (empty) vision page.
    assert_eq!(files.len(), 3);
    assert_eq!(files[1].name, "text-00100.json");
    let report = build_report(&lm, &files, 1, "test").unwrap();
    // Categories without an axis metric are skipped.
    assert_eq!(report.rows.len(), 100);
    let prefix = &lm.info().urls[0];
    assert!(
        fetcher
            .seen
            .borrow()
            .iter()
            .all(|u| u.starts_with(prefix.as_str()))
    );

    let epoch = EpochProbe;
    let files = epoch.fetch(&fetcher).unwrap();
    let report = build_report(&epoch, &files, 1, "test").unwrap();
    assert_eq!(report.rows.len(), 11);
    assert_eq!(report.files[0].sha256.len(), 64);
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.contains("hle_external.csv: missing"))
    );
    assert!(report.issues.iter().any(|i| i.contains("skipped 1 rows")));

    // Snapshots are recorded and pruned per probe.
    let d = tempfile::tempdir().unwrap();
    let mut store = Store::open(&d.path().join("rostr.db")).unwrap();
    for _ in 0..5 {
        store.record_scores(&report).unwrap();
    }
    let runs = store.latest_score_runs().unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].rows.len(), 11);
    let n: i64 = rusqlite::Connection::open(d.path().join("rostr.db"))
        .unwrap()
        .query_row("SELECT count(*) FROM score_runs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 3);
}

#[test]
fn malformed_input_is_rejected() {
    let epoch = EpochProbe;
    let junk = RawFile {
        name: "benchmark_data.zip".into(),
        origin: "test".into(),
        bytes: b"not a zip".to_vec(),
    };
    assert!(build_report(&epoch, &[junk], 1, "test").is_err());
    let lm = LmArenaProbe;
    let page = RawFile {
        name: "text-00000.json".into(),
        origin: "test".into(),
        bytes: br#"{"rows":[],"num_rows_total":0}"#.to_vec(),
    };
    // Zero rows is an error, so an empty snapshot never replaces a good one.
    assert!(build_report(&lm, &[page], 1, "test").is_err());
}
