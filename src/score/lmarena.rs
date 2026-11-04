//! LMArena's official leaderboard dataset (`lmarena-ai/leaderboard-dataset`,
//! CC BY 4.0), read as JSON pages from the Hugging Face dataset viewer API.
//! The precomputed `/rows` pages of each config are fetched in full and the
//! categories are selected locally: `/filter` is computed on demand and too
//! slow and flaky for a refresh.

use super::*;
use serde::Deserialize;

const BASE: &str =
    "https://datasets-server.huggingface.co/rows?dataset=lmarena-ai%2Fleaderboard-dataset&";
const PAGE: usize = 100;
/// Per config; `text` has about 11,000 rows in the `latest` split.
const MAX_PAGES: usize = 400;
const CONFIGS: &[&str] = &["text", "vision"];
const MAX_PAGE_BYTES: u64 = 4 * 1024 * 1024;

/// `(config, category, metric, description)`.
const BOARDS: &[(&str, &str, &str, &str)] = &[
    (
        "text",
        "overall",
        "text-overall",
        "Text Arena overall rating",
    ),
    (
        "text",
        "coding",
        "text-coding",
        "Text Arena coding category rating",
    ),
    (
        "text",
        "instruction_following",
        "text-instruction-following",
        "Text Arena instruction-following category rating",
    ),
    (
        "vision",
        "overall",
        "vision-overall",
        "Vision Arena overall rating",
    ),
];

pub struct LmArenaProbe;

fn url(config: &str, offset: usize) -> String {
    format!("{BASE}config={config}&split=latest&offset={offset}&length={PAGE}")
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    rows: Vec<PageRow>,
    num_rows_total: Option<usize>,
    error: Option<String>,
}
#[derive(Deserialize)]
struct PageRow {
    row: Row,
}
#[derive(Deserialize)]
struct Row {
    model_name: Option<String>,
    organization: Option<String>,
    rating: Option<f64>,
    category: Option<String>,
    leaderboard_publish_date: Option<String>,
}

impl LmArenaProbe {
    fn get_page(&self, fetcher: &dyn Fetcher, url: &str) -> Result<(Vec<u8>, Page), ProbeError> {
        let id = self.id();
        let mut last = None;
        for attempt in 0..4u32 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_secs(1 << attempt));
            }
            let result = fetcher.get(&id, url, MAX_PAGE_BYTES).and_then(|bytes| {
                let page: Page = serde_json::from_slice(&bytes).map_err(|e| {
                    error(&id, ProbeErrorKind::InvalidData, format!("GET {url}: {e}"))
                })?;
                match &page.error {
                    Some(e) => Err(error(
                        &id,
                        ProbeErrorKind::NetworkUnavailable,
                        format!("GET {url}: {e}"),
                    )),
                    None => Ok((bytes, page)),
                }
            });
            match result {
                Ok(v) => return Ok(v),
                Err(e) if e.retryable => last = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(last.expect("at least one attempt"))
    }
}

impl ScoreProbe for LmArenaProbe {
    fn id(&self) -> ProbeId {
        "lmarena".into()
    }
    fn info(&self) -> SourceInfo {
        SourceInfo {
            id: self.id(),
            name: "LMArena leaderboard dataset",
            homepage: "https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset",
            license: "CC-BY-4.0",
            attribution: "LMArena, lmarena-ai/leaderboard-dataset on Hugging Face",
            urls: vec![BASE.into()],
            metrics: BOARDS
                .iter()
                .map(|&(_, _, metric, description)| MetricInfo {
                    metric,
                    unit: "elo",
                    description,
                })
                .collect(),
        }
    }
    fn parser_version(&self) -> u32 {
        1
    }
    fn fetch(&self, fetcher: &dyn Fetcher) -> Result<Vec<RawFile>, ProbeError> {
        let mut files = Vec::new();
        for &config in CONFIGS {
            let mut offset = 0;
            for _ in 0..MAX_PAGES {
                let u = url(config, offset);
                let (bytes, page) = self.get_page(fetcher, &u)?;
                let n = page.rows.len();
                files.push(RawFile {
                    name: format!("{config}-{offset:05}.json"),
                    origin: u,
                    bytes,
                });
                offset += n;
                if n == 0 || page.num_rows_total.is_none_or(|t| offset >= t) {
                    break;
                }
            }
        }
        Ok(files)
    }
    fn parse(&self, files: &[RawFile]) -> Result<Parsed, ProbeError> {
        let id = self.id();
        let mut out = Parsed::default();
        for f in files {
            let name = f.name.rsplit('/').next().unwrap_or(&f.name);
            let Some(config) = CONFIGS.iter().copied().find(|c| {
                name.strip_prefix(&format!("{c}-"))
                    .and_then(|rest| rest.strip_suffix(".json"))
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            }) else {
                out.issues.push(format!("{name}: not a known page name"));
                continue;
            };
            let page: Page = serde_json::from_slice(&f.bytes)
                .map_err(|e| error(&id, ProbeErrorKind::InvalidData, format!("{name}: {e}")))?;
            if let Some(e) = page.error {
                out.issues.push(format!("{name}: {e}"));
                continue;
            }
            for PageRow { row } in page.rows {
                // Categories without an axis metric are expected and skipped.
                let Some(&(_, _, metric, _)) = BOARDS
                    .iter()
                    .find(|(c, cat, _, _)| *c == config && row.category.as_deref() == Some(*cat))
                else {
                    continue;
                };
                let (Some(key), Some(v)) = (
                    row.model_name
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty()),
                    row.rating.filter(|v| v.is_finite()),
                ) else {
                    continue;
                };
                out.rows.push(BoardRow {
                    metric: metric.into(),
                    source_key: key,
                    organization: row.organization.filter(|s| !s.trim().is_empty()),
                    value: v,
                    as_of: row.leaderboard_publish_date.as_deref().and_then(date),
                });
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_under_the_declared_prefix() {
        let u = url("text", 200);
        assert!(u.starts_with(BASE));
        assert!(u.ends_with("offset=200&length=100"));
    }
}
