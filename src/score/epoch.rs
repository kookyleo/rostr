//! Epoch AI "Capabilities & benchmarking" data (CC BY 4.0): one zip of CSVs.

use super::*;
use std::io::{Cursor, Read};

pub const URL: &str = "https://epoch.ai/data/benchmark_data.zip";
const MAX_ZIP: u64 = 64 * 1024 * 1024;
const MAX_ENTRY: u64 = 32 * 1024 * 1024;

pub struct EpochProbe;

struct Table {
    metric: &'static str,
    file: &'static str,
    model: &'static str,
    value: &'static str,
    organization: &'static str,
    date: Option<&'static str>,
    unit: &'static str,
    description: &'static str,
}

const TABLES: &[Table] = &[
    Table {
        metric: "eci",
        file: "epoch_capabilities_index/eci_scores.csv",
        model: "Model",
        value: "eci",
        organization: "Organization",
        date: None,
        unit: "index",
        description: "Epoch Capabilities Index",
    },
    Table {
        metric: "swe-bench-verified",
        file: "swe_bench_verified.csv",
        model: "Model version",
        value: "Best score (across scorers)",
        organization: "Organization",
        date: Some("Started at"),
        unit: "fraction",
        description: "SWE-bench Verified, run by Epoch AI",
    },
    Table {
        metric: "aider-polyglot",
        file: "aider_polyglot_external.csv",
        model: "Model version",
        value: "Percent correct",
        organization: "Organization",
        date: Some("Date of evaluation"),
        unit: "percent",
        description: "Aider Polyglot, external results collected by Epoch AI",
    },
    Table {
        metric: "gpqa-diamond",
        file: "gpqa_diamond.csv",
        model: "Model version",
        value: "Best score (across scorers)",
        organization: "Organization",
        date: Some("Started at"),
        unit: "fraction",
        description: "GPQA Diamond, run by Epoch AI",
    },
    Table {
        metric: "hle",
        file: "hle_external.csv",
        model: "Model version",
        value: "Accuracy",
        organization: "Organization",
        date: None,
        unit: "fraction",
        description: "Humanity's Last Exam, external results collected by Epoch AI",
    },
    Table {
        metric: "video-mme",
        file: "video_mme_external.csv",
        model: "Model version",
        value: "Overall (no subtitles)",
        organization: "Organization",
        date: None,
        unit: "fraction",
        description: "Video-MME overall without subtitles, external results collected by Epoch AI",
    },
];

impl ScoreProbe for EpochProbe {
    fn id(&self) -> ProbeId {
        "epoch-ai".into()
    }
    fn info(&self) -> SourceInfo {
        SourceInfo {
            id: self.id(),
            name: "Epoch AI Capabilities & benchmarking",
            homepage: "https://epoch.ai/benchmarks",
            license: "CC-BY-4.0",
            attribution: "Epoch AI, 'Capabilities & benchmarking'. Retrieved from https://epoch.ai/benchmarks",
            urls: vec![URL.into()],
            metrics: TABLES
                .iter()
                .map(|t| MetricInfo {
                    metric: t.metric,
                    unit: t.unit,
                    description: t.description,
                })
                .collect(),
        }
    }
    fn parser_version(&self) -> u32 {
        1
    }
    fn fetch(&self, fetcher: &dyn Fetcher) -> Result<Vec<RawFile>, ProbeError> {
        Ok(vec![RawFile {
            name: "benchmark_data.zip".into(),
            origin: URL.into(),
            bytes: fetcher.get(&self.id(), URL, MAX_ZIP)?,
        }])
    }
    fn parse(&self, files: &[RawFile]) -> Result<Parsed, ProbeError> {
        let id = self.id();
        let bad = |m: String| error(&id, ProbeErrorKind::InvalidData, m);
        let [file] = files else {
            return Err(bad(format!("expected one zip file, got {}", files.len())));
        };
        let mut zip = zip::ZipArchive::new(Cursor::new(&file.bytes))
            .map_err(|e| bad(format!("{}: not a zip archive: {e}", file.name)))?;
        let mut out = Parsed::default();
        for t in TABLES {
            let mut text = Vec::new();
            match zip.by_name(t.file) {
                Ok(entry) => {
                    if entry.size() > MAX_ENTRY {
                        out.issues.push(format!("{}: entry too large", t.file));
                        continue;
                    }
                    if let Err(e) = entry.take(MAX_ENTRY).read_to_end(&mut text) {
                        out.issues.push(format!("{}: {e}", t.file));
                        continue;
                    }
                }
                Err(_) => {
                    out.issues.push(format!("{}: missing from archive", t.file));
                    continue;
                }
            }
            match parse_csv(t, &text) {
                Ok((rows, skipped)) => {
                    if skipped > 0 {
                        out.issues.push(format!(
                            "{}: skipped {skipped} rows without a value",
                            t.file
                        ));
                    }
                    out.rows.extend(rows);
                }
                Err(e) => out.issues.push(format!("{}: {e}", t.file)),
            }
        }
        Ok(out)
    }
}

fn parse_csv(t: &Table, text: &[u8]) -> Result<(Vec<BoardRow>, usize), String> {
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(text);
    let headers = reader.headers().map_err(|e| e.to_string())?.clone();
    let col = |name: &str| {
        headers
            .iter()
            .position(|h| h.trim() == name)
            .ok_or_else(|| format!("column `{name}` not found"))
    };
    let model = col(t.model)?;
    let value = col(t.value)?;
    let org = col(t.organization).ok();
    let date = t.date.map(col).transpose()?;
    let (mut rows, mut skipped) = (Vec::new(), 0);
    for record in reader.records() {
        let record = record.map_err(|e| e.to_string())?;
        let key = record.get(model).unwrap_or("").trim();
        let Some(v) = record.get(value).and_then(number) else {
            skipped += 1;
            continue;
        };
        if key.is_empty() {
            skipped += 1;
            continue;
        }
        rows.push(BoardRow {
            metric: t.metric.into(),
            source_key: key.into(),
            organization: org
                .and_then(|i| record.get(i))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from),
            value: v,
            as_of: date.and_then(|i| record.get(i)).and_then(super::date),
        });
    }
    Ok((rows, skipped))
}
