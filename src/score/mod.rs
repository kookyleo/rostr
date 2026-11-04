//! Public benchmark scores per model and axis.
//!
//! Only `rostr score refresh` touches the network, and only through
//! [`HttpsFetcher`] against the fixed URLs each [`ScoreProbe`] declares. Board
//! snapshots are cached and parsed into `rostr.db`; every query is offline and
//! read-only. For each model and axis the first qualified observation from the
//! fixed [`RANKS`] table wins. Sources are never averaged or mixed, and a
//! missing score is Unknown with a null value, never zero.

pub mod cli;
mod epoch;
mod fetch;
mod lmarena;
pub mod mapping;
pub mod overrides;
mod repository;

pub use epoch::EpochProbe;
pub use fetch::{Fetcher, HttpsFetcher};
pub use lmarena::LmArenaProbe;
pub use overrides::ScoreOverrides;
pub use repository::{ScoreRepository, StoredRun};

pub use crate::quota::{ProbeError, ProbeErrorKind, ProbeId, Timestamp, now};
use serde::{Deserialize, Serialize};

/// The public score axes, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    General,
    Coding,
    Reasoning,
    Multimodal,
    Instruction,
}
impl Axis {
    pub const ALL: [Axis; 5] = [
        Axis::General,
        Axis::Coding,
        Axis::Reasoning,
        Axis::Multimodal,
        Axis::Instruction,
    ];
    pub fn id(self) -> &'static str {
        match self {
            Axis::General => "general",
            Axis::Coding => "coding",
            Axis::Reasoning => "reasoning",
            Axis::Multimodal => "multimodal",
            Axis::Instruction => "instruction",
        }
    }
    pub fn parse(s: &str) -> Option<Axis> {
        Axis::ALL.into_iter().find(|a| a.id() == s)
    }
}
impl std::fmt::Display for Axis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

/// One metric a probe can supply.
#[derive(Debug, Clone, Serialize)]
pub struct MetricInfo {
    pub metric: &'static str,
    /// Scale of the native value: `fraction` (0-1), `percent` (0-100),
    /// `elo` (arena rating) or `index` (Epoch Capabilities Index).
    pub unit: &'static str,
    pub description: &'static str,
}

/// Static facts about a source: where it is fetched from and under which terms.
#[derive(Debug, Clone, Serialize)]
pub struct SourceInfo {
    pub id: ProbeId,
    pub name: &'static str,
    pub homepage: &'static str,
    pub license: &'static str,
    pub attribution: &'static str,
    /// The fixed HTTPS URLs (or URL prefixes) refresh may request.
    pub urls: Vec<String>,
    pub metrics: Vec<MetricInfo>,
}

/// The fixed trust-rank table: per axis, `(probe, metric)` in priority order.
/// A higher row wins whenever it has a qualified observation for the model.
pub const RANKS: &[(Axis, &[(&str, &str)])] = &[
    (
        Axis::General,
        &[("epoch-ai", "eci"), ("lmarena", "text-overall")],
    ),
    (
        Axis::Coding,
        &[
            ("epoch-ai", "swe-bench-verified"),
            ("epoch-ai", "aider-polyglot"),
            ("lmarena", "text-coding"),
        ],
    ),
    (
        Axis::Reasoning,
        &[("epoch-ai", "gpqa-diamond"), ("epoch-ai", "hle")],
    ),
    (
        Axis::Multimodal,
        &[("lmarena", "vision-overall"), ("epoch-ai", "video-mme")],
    ),
    (
        Axis::Instruction,
        &[("lmarena", "text-instruction-following")],
    ),
];

pub fn ranks(axis: Axis) -> &'static [(&'static str, &'static str)] {
    RANKS
        .iter()
        .find(|(a, _)| *a == axis)
        .map(|(_, r)| *r)
        .unwrap_or(&[])
}

/// One board row: the board's own model key and its native value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardRow {
    pub metric: String,
    pub source_key: String,
    pub organization: Option<String>,
    pub value: f64,
    /// Observation date (`YYYY-MM-DD`) when the board states one.
    pub as_of: Option<String>,
}

/// A raw downloaded or imported board file, kept byte-for-byte in the cache.
#[derive(Debug, Clone)]
pub struct RawFile {
    /// Cache file name; parsers may use it to identify a page.
    pub name: String,
    /// `https://...` for refresh, `file://...` for import.
    pub origin: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct Parsed {
    pub rows: Vec<BoardRow>,
    pub issues: Vec<String>,
}

/// A parsed board snapshot, ready to record.
#[derive(Debug, Clone, Serialize)]
pub struct ScoreReport {
    pub probe_id: ProbeId,
    pub parser_version: u32,
    pub retrieved_at: Timestamp,
    pub origin: String,
    pub files: Vec<FileDigest>,
    pub rows: Vec<BoardRow>,
    pub issues: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDigest {
    pub name: String,
    pub origin: String,
    pub bytes: usize,
    pub sha256: String,
}

/// A benchmark board adapter, alongside the discover `Adapter` and `QuotaProbe`.
/// `fetch` is the only networked step and goes through the supplied fetcher;
/// `parse` is pure and also serves offline import.
pub trait ScoreProbe {
    fn id(&self) -> ProbeId;
    fn info(&self) -> SourceInfo;
    fn parser_version(&self) -> u32;
    fn fetch(&self, fetcher: &dyn Fetcher) -> Result<Vec<RawFile>, ProbeError>;
    fn parse(&self, files: &[RawFile]) -> Result<Parsed, ProbeError>;
}

pub fn probes() -> Vec<Box<dyn ScoreProbe>> {
    vec![Box::new(EpochProbe), Box::new(LmArenaProbe)]
}

pub fn probe(id: &str) -> Option<Box<dyn ScoreProbe>> {
    probes().into_iter().find(|p| p.id() == id)
}

/// Parses raw files into a report with digests.
pub fn build_report(
    probe: &dyn ScoreProbe,
    files: &[RawFile],
    retrieved_at: Timestamp,
    origin: &str,
) -> Result<ScoreReport, ProbeError> {
    use sha2::{Digest, Sha256};
    let parsed = probe.parse(files)?;
    if parsed.rows.is_empty() {
        return Err(error(
            &probe.id(),
            ProbeErrorKind::InvalidData,
            format!("no score rows parsed; {}", parsed.issues.join("; ")),
        ));
    }
    Ok(ScoreReport {
        probe_id: probe.id(),
        parser_version: probe.parser_version(),
        retrieved_at,
        origin: origin.into(),
        files: files
            .iter()
            .map(|f| FileDigest {
                name: f.name.clone(),
                origin: f.origin.clone(),
                bytes: f.bytes.len(),
                sha256: format!("{:x}", Sha256::digest(&f.bytes)),
            })
            .collect(),
        rows: parsed.rows,
        issues: parsed.issues,
    })
}

pub(crate) fn error(probe: &str, kind: ProbeErrorKind, diagnostic: String) -> ProbeError {
    let retryable = matches!(
        kind,
        ProbeErrorKind::NetworkUnavailable
            | ProbeErrorKind::Timeout
            | ProbeErrorKind::EndpointRateLimited
    );
    ProbeError {
        kind,
        probe_id: probe.into(),
        retryable,
        retry_at: None,
        diagnostic,
    }
}

/// A finite number from board text; `None` for blanks and junk.
pub(crate) fn number(s: &str) -> Option<f64> {
    let v: f64 = s.trim().parse().ok()?;
    v.is_finite().then_some(v)
}

/// The leading `YYYY-MM-DD` of a board timestamp.
pub(crate) fn date(s: &str) -> Option<String> {
    let d = s.trim().get(..10)?;
    chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d")
        .ok()
        .map(|_| d.to_string())
}

/// Score state of one model on one axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreState {
    Known,
    Stale,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Provenance {
    /// Probe ID, or `manual` for a scores.yaml value.
    pub source: String,
    pub metric: String,
    pub unit: String,
    pub as_of: Option<String>,
    pub source_key: String,
    /// `alias:scores.yaml`, `alias:builtin`, `rule:exact`, `rule:variant`,
    /// `rule:dateless` or `manual`.
    pub matched_by: String,
    pub retrieved_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AxisScore {
    pub axis: Axis,
    /// Native value of the winning metric; null when Unknown.
    pub value: Option<f64>,
    pub state: ScoreState,
    pub provenance: Option<Provenance>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelScores {
    pub model: String,
    pub scores: Vec<AxisScore>,
}

#[derive(Debug, Clone)]
pub struct ScoreQuery {
    pub models: Vec<String>,
    pub axes: Vec<Axis>,
    pub max_age_ms: i64,
    pub as_of: Timestamp,
}

pub fn unit_of(probe: &str, metric: &str) -> &'static str {
    self::probe(probe)
        .and_then(|p| p.info().metrics.into_iter().find(|m| m.metric == metric))
        .map_or("unknown", |m| m.unit)
}

/// Resolves every requested model and axis against the latest stored runs.
pub fn resolve(runs: &[StoredRun], overrides: &ScoreOverrides, q: &ScoreQuery) -> Vec<ModelScores> {
    q.models
        .iter()
        .map(|model| ModelScores {
            model: model.clone(),
            scores: q
                .axes
                .iter()
                .map(|&axis| resolve_axis(runs, overrides, model, axis, q))
                .collect(),
        })
        .collect()
}

fn resolve_axis(
    runs: &[StoredRun],
    overrides: &ScoreOverrides,
    model: &str,
    axis: Axis,
    q: &ScoreQuery,
) -> AxisScore {
    if let Some(v) = overrides.value(model, axis) {
        return AxisScore {
            axis,
            value: Some(v.value),
            state: ScoreState::Known,
            provenance: Some(Provenance {
                source: "manual".into(),
                metric: v.metric.clone().unwrap_or_else(|| "scores.yaml".into()),
                unit: v.unit.clone().unwrap_or_else(|| "unknown".into()),
                as_of: v.as_of.clone(),
                source_key: model.into(),
                matched_by: "manual".into(),
                retrieved_at: None,
            }),
            reasons: v.note.iter().cloned().collect(),
        };
    }
    let mut reasons = Vec::new();
    let table = ranks(axis);
    if table.is_empty() {
        reasons.push(format!("no verified source supplies the {axis} axis"));
    }
    for &(probe, metric) in table {
        let Some(run) = runs.iter().find(|r| r.probe_id == probe) else {
            reasons.push(format!(
                "{probe}/{metric}: no snapshot; run `rostr score refresh`"
            ));
            continue;
        };
        let rows: Vec<&BoardRow> = run
            .rows
            .iter()
            .filter(|r| r.metric == metric)
            .filter(|r| !overrides.ignored(model, axis, probe, metric, &r.source_key))
            .collect();
        if rows.is_empty() {
            reasons.push(format!("{probe}/{metric}: no rows in snapshot"));
            continue;
        }
        match mapping::find(model, probe, metric, &rows, overrides) {
            mapping::Match::Found { row, matched_by } => {
                let stale = q.as_of - run.retrieved_at > q.max_age_ms;
                if stale {
                    reasons.push(format!(
                        "{probe} snapshot is older than the maximum age; run `rostr score refresh`"
                    ));
                }
                return AxisScore {
                    axis,
                    value: Some(row.value),
                    state: if stale {
                        ScoreState::Stale
                    } else {
                        ScoreState::Known
                    },
                    provenance: Some(Provenance {
                        source: probe.into(),
                        metric: metric.into(),
                        unit: unit_of(probe, metric).into(),
                        as_of: row.as_of.clone(),
                        source_key: row.source_key.clone(),
                        matched_by,
                        retrieved_at: Some(run.retrieved_at),
                    }),
                    reasons,
                };
            }
            mapping::Match::Ambiguous(keys) => {
                // Do not fall through: a lower-ranked source would hide the ambiguity.
                reasons.push(format!(
                    "{probe}/{metric}: ambiguous variants {}; pin one with an alias in scores.yaml",
                    keys.join(", ")
                ));
                return unknown(axis, reasons);
            }
            mapping::Match::None(why) => reasons.push(format!("{probe}/{metric}: {why}")),
        }
    }
    unknown(axis, reasons)
}

fn unknown(axis: Axis, reasons: Vec<String>) -> AxisScore {
    AxisScore {
        axis,
        value: None,
        state: ScoreState::Unknown,
        provenance: None,
        reasons,
    }
}
