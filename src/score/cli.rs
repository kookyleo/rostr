use super::*;
use crate::store::Store;
use anyhow::{Context, Result, bail};
use clap::{Arg, ArgAction, ArgMatches, Command};
use std::path::{Path, PathBuf};

const MAX_IMPORT_FILES: usize = 200;
const MAX_IMPORT_BYTES: u64 = 64 * 1024 * 1024;
const DAY_MS: i64 = 86_400_000;

fn source_arg() -> Arg {
    Arg::new("source")
        .long("source")
        .value_parser(["epoch-ai", "lmarena"])
}

pub fn command() -> Command {
    Command::new("score")
        .about("Public benchmark scores per model on five axes; only `refresh` uses the network")
        .subcommand_required(true)
        .subcommand(
            Command::new("refresh")
                .about("Download the fixed benchmark board files over HTTPS, cache them and record a snapshot")
                .arg(source_arg().action(ArgAction::Append))
                .arg(Arg::new("json").long("json").action(ArgAction::SetTrue)),
        )
        .subcommand(
            Command::new("import")
                .about("Record a snapshot from a board file or a cache directory, offline")
                .arg(source_arg().required(true))
                .arg(Arg::new("path").required(true))
                .arg(Arg::new("json").long("json").action(ArgAction::SetTrue)),
        )
        .subcommand(
            Command::new("query")
                .about("Read scores from rostr.db; never refreshes or migrates")
                .arg(
                    Arg::new("model")
                        .long("model")
                        .action(ArgAction::Append)
                        .help("Canonical <provider>/<model> ID (default: selected models)"),
                )
                .arg(
                    Arg::new("all")
                        .long("all")
                        .action(ArgAction::SetTrue)
                        .conflicts_with("model")
                        .help("Every model known to rostr.db, selected or not"),
                )
                .arg(
                    Arg::new("axis")
                        .long("axis")
                        .action(ArgAction::Append)
                        .value_parser(["general", "coding", "reasoning", "multimodal", "instruction"]),
                )
                .arg(
                    Arg::new("max-age")
                        .long("max-age")
                        .default_value("30")
                        .value_parser(clap::value_parser!(u32))
                        .help("Days before a snapshot is Stale"),
                )
                .arg(
                    Arg::new("as-of")
                        .long("as-of")
                        .value_parser(clap::value_parser!(i64))
                        .help("UTC Unix milliseconds (default: now)"),
                )
                .arg(Arg::new("json").long("json").action(ArgAction::SetTrue)),
        )
        .subcommand(
            Command::new("sources")
                .about("List sources, licenses, URLs, the axis rank table and the last snapshots")
                .arg(Arg::new("json").long("json").action(ArgAction::SetTrue)),
        )
}

struct Paths {
    db: PathBuf,
    cache: PathBuf,
    overrides: PathBuf,
}

fn paths() -> Result<Paths> {
    let home = std::env::var("HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .context("HOME is not set")?;
    let home = PathBuf::from(home);
    let var = |k: &str| std::env::var(k).ok();
    let data = crate::scope::data_dir(&home, var);
    let config = crate::scope::config_dir(&home, var);
    Ok(Paths {
        db: data.join(crate::store::FILE_NAME),
        cache: data.join("scores"),
        overrides: config.join(overrides::FILE_NAME),
    })
}

pub fn run(m: &ArgMatches) -> Result<()> {
    let p = paths()?;
    match m.subcommand() {
        Some(("refresh", a)) => refresh(&p, a),
        Some(("import", a)) => import(&p, a),
        Some(("query", a)) => query(&p, a),
        Some(("sources", a)) => sources(&p, a),
        _ => bail!("expected score refresh, import, query or sources"),
    }
}

/// Replaces a probe's cache directory with the files of the new snapshot.
fn write_cache(dir: &Path, probe: &str, files: &[RawFile]) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let tmp = tempfile::Builder::new()
        .prefix(&format!(".{probe}-"))
        .tempdir_in(dir)?;
    for f in files {
        std::fs::write(tmp.path().join(&f.name), &f.bytes)?;
    }
    let target = dir.join(probe);
    if target.exists() {
        std::fs::remove_dir_all(&target)
            .with_context(|| format!("cannot replace {}", target.display()))?;
    }
    std::fs::rename(tmp.keep(), &target)
        .with_context(|| format!("cannot write {}", target.display()))?;
    Ok(())
}

#[derive(Serialize)]
struct RunSummary {
    source: ProbeId,
    status: &'static str,
    rows: usize,
    metrics: Vec<(String, usize)>,
    issues: Vec<String>,
    error: Option<String>,
}

fn summarize(report: &ScoreReport) -> RunSummary {
    let mut metrics: Vec<(String, usize)> = Vec::new();
    for r in &report.rows {
        match metrics.iter_mut().find(|(m, _)| *m == r.metric) {
            Some((_, n)) => *n += 1,
            None => metrics.push((r.metric.clone(), 1)),
        }
    }
    RunSummary {
        source: report.probe_id.clone(),
        status: "recorded",
        rows: report.rows.len(),
        metrics,
        issues: report.issues.clone(),
        error: None,
    }
}

fn print_summaries(summaries: &[RunSummary], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(summaries)?);
        return Ok(());
    }
    for s in summaries {
        match &s.error {
            Some(e) => println!("{}: failed: {e}", s.source),
            None => {
                let m: Vec<String> = s.metrics.iter().map(|(m, n)| format!("{m} {n}")).collect();
                println!("{}: {} rows ({})", s.source, s.rows, m.join(", "));
                for i in &s.issues {
                    println!("  note: {i}");
                }
            }
        }
    }
    Ok(())
}

fn refresh(p: &Paths, a: &ArgMatches) -> Result<()> {
    let wanted: Vec<&String> = a
        .get_many::<String>("source")
        .into_iter()
        .flatten()
        .collect();
    let all = probes();
    let selected: Vec<&Box<dyn ScoreProbe>> = all
        .iter()
        .filter(|probe| wanted.is_empty() || wanted.iter().any(|w| **w == probe.id()))
        .collect();
    let allowed = all.iter().flat_map(|probe| probe.info().urls).collect();
    let fetcher = HttpsFetcher::new(allowed);
    let mut summaries = Vec::new();
    let mut store: Option<Store> = None;
    for probe in selected {
        let outcome = (|| -> Result<ScoreReport> {
            let files = probe.fetch(&fetcher)?;
            let report = build_report(probe.as_ref(), &files, now(), &probe.info().urls[0])?;
            write_cache(&p.cache, &probe.id(), &files)?;
            let s = match &mut store {
                Some(s) => s,
                None => store.insert(Store::open(&p.db)?),
            };
            s.record_scores(&report)?;
            Ok(report)
        })();
        summaries.push(match outcome {
            Ok(report) => summarize(&report),
            Err(e) => RunSummary {
                source: probe.id(),
                status: "failed",
                rows: 0,
                metrics: vec![],
                issues: vec![],
                error: Some(format!("{e:#}")),
            },
        });
    }
    print_summaries(&summaries, a.get_flag("json"))?;
    let failed = summaries.iter().filter(|s| s.error.is_some()).count();
    if failed > 0 {
        bail!("{failed} of {} sources failed to refresh", summaries.len());
    }
    Ok(())
}

fn read_import(path: &Path) -> Result<Vec<RawFile>> {
    let read = |f: &Path| -> Result<RawFile> {
        let len = std::fs::metadata(f)?.len();
        if len > MAX_IMPORT_BYTES {
            bail!("{} is larger than {MAX_IMPORT_BYTES} bytes", f.display());
        }
        let abs = std::fs::canonicalize(f)?;
        Ok(RawFile {
            name: f
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            origin: format!("file://{}", abs.display()),
            bytes: std::fs::read(f).with_context(|| format!("cannot read {}", f.display()))?,
        })
    };
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(path)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect();
        entries.sort();
        if entries.len() > MAX_IMPORT_FILES {
            bail!(
                "{} holds more than {MAX_IMPORT_FILES} files",
                path.display()
            );
        }
        entries.iter().map(|f| read(f)).collect()
    } else {
        Ok(vec![read(path)?])
    }
}

fn import(p: &Paths, a: &ArgMatches) -> Result<()> {
    let id = a.get_one::<String>("source").unwrap();
    let probe = probe(id).context("unknown source")?;
    let path = Path::new(a.get_one::<String>("path").unwrap());
    let files = read_import(path)?;
    let origin = std::fs::canonicalize(path)
        .map(|abs| format!("file://{}", abs.display()))
        .unwrap_or_else(|_| path.display().to_string());
    let report = build_report(probe.as_ref(), &files, now(), &origin)?;
    Store::open(&p.db)?.record_scores(&report)?;
    print_summaries(&[summarize(&report)], a.get_flag("json"))
}

fn fmt_value(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}

fn query(p: &Paths, a: &ArgMatches) -> Result<()> {
    let overrides = ScoreOverrides::load(&p.overrides)?;
    let store = Store::open_read_only(&p.db)?;
    let models: Vec<String> = match a.get_many::<String>("model") {
        Some(v) => {
            let ids: Vec<String> = v.map(|s| crate::model_id::normalize(s)).collect();
            for id in &ids {
                crate::model_id::validate_id(id)?;
            }
            ids
        }
        None => match &store {
            Some(s) if a.get_flag("all") => s.known_models()?,
            Some(s) => s.selected_models()?,
            None => vec![],
        },
    };
    if models.is_empty() {
        bail!(
            "no models to score: pass --model <provider>/<model>, or select models with `rostr discover`"
        );
    }
    let axes: Vec<Axis> = match a.get_many::<String>("axis") {
        Some(v) => v.filter_map(|s| Axis::parse(s)).collect(),
        None => Axis::ALL.to_vec(),
    };
    let runs = match &store {
        Some(s) => s.latest_score_runs()?,
        None => vec![],
    };
    let q = ScoreQuery {
        models,
        axes,
        max_age_ms: i64::from(*a.get_one::<u32>("max-age").unwrap()) * DAY_MS,
        as_of: a.get_one::<i64>("as-of").copied().unwrap_or_else(now),
    };
    let results = resolve(&runs, &overrides, &q);
    if a.get_flag("json") {
        println!("{}", serde_json::to_string_pretty(&results)?);
        return Ok(());
    }
    for m in &results {
        println!("{}", m.model);
        for s in &m.scores {
            let state = serde_json::to_value(s.state)?;
            let state = state.as_str().unwrap_or("");
            match (&s.value, &s.provenance) {
                (Some(v), Some(pv)) => println!(
                    "  {:<12} {:>10}  {:<8} {}/{} [{}] {} {}",
                    s.axis.id(),
                    fmt_value(*v),
                    state,
                    pv.source,
                    pv.metric,
                    pv.unit,
                    pv.source_key,
                    pv.matched_by
                ),
                _ => println!(
                    "  {:<12} {:>10}  {:<8} {}",
                    s.axis.id(),
                    "-",
                    state,
                    s.reasons.last().map(String::as_str).unwrap_or("")
                ),
            }
        }
    }
    if runs.is_empty() {
        println!(
            "No score snapshots in {}; run `rostr score refresh`.",
            p.db.display()
        );
    }
    Ok(())
}

#[derive(Serialize)]
struct SourcesOut {
    sources: Vec<SourceOut>,
    ranks: Vec<RankOut>,
    overrides: String,
}
#[derive(Serialize)]
struct SourceOut {
    #[serde(flatten)]
    info: SourceInfo,
    last_retrieved_at: Option<Timestamp>,
    last_rows: Option<usize>,
}
#[derive(Serialize)]
struct RankOut {
    axis: Axis,
    order: Vec<String>,
}

fn sources(p: &Paths, a: &ArgMatches) -> Result<()> {
    let runs = match Store::open_read_only(&p.db)? {
        Some(s) => s.latest_score_runs()?,
        None => vec![],
    };
    let out = SourcesOut {
        sources: probes()
            .iter()
            .map(|probe| {
                let run = runs.iter().find(|r| r.probe_id == probe.id());
                SourceOut {
                    info: probe.info(),
                    last_retrieved_at: run.map(|r| r.retrieved_at),
                    last_rows: run.map(|r| r.rows.len()),
                }
            })
            .collect(),
        ranks: Axis::ALL
            .iter()
            .map(|&axis| RankOut {
                axis,
                order: ranks(axis)
                    .iter()
                    .map(|(probe, metric)| format!("{probe}/{metric}"))
                    .collect(),
            })
            .collect(),
        overrides: p.overrides.display().to_string(),
    };
    if a.get_flag("json") {
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    for s in &out.sources {
        println!("{} — {} ({})", s.info.id, s.info.name, s.info.license);
        println!("  attribution: {}", s.info.attribution);
        for u in &s.info.urls {
            println!("  url: {u}");
        }
        match (s.last_retrieved_at, s.last_rows) {
            (Some(t), Some(n)) => println!(
                "  last snapshot: {} ({n} rows)",
                chrono::DateTime::from_timestamp_millis(t)
                    .map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string())
                    .unwrap_or_default()
            ),
            _ => println!("  last snapshot: none"),
        }
        for m in &s.info.metrics {
            println!("  - {} [{}]: {}", m.metric, m.unit, m.description);
        }
    }
    println!("\nAxis ranks (first qualified observation wins):");
    for r in &out.ranks {
        println!("  {:<12} {}", r.axis.id(), r.order.join(" > "));
    }
    println!("\nOverrides: {}", out.overrides);
    Ok(())
}
