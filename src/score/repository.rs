use super::*;
use crate::store::Store;
use anyhow::{Result, ensure};
use rusqlite::params;

/// Runs kept per probe; older snapshots are pruned on record.
const KEEP_RUNS: i64 = 3;

#[derive(Debug, Clone)]
pub struct StoredRun {
    pub id: i64,
    pub probe_id: ProbeId,
    pub parser_version: u32,
    pub retrieved_at: Timestamp,
    pub origin: String,
    pub files: Vec<FileDigest>,
    pub issues: Vec<String>,
    pub rows: Vec<BoardRow>,
}

pub trait ScoreRepository {
    fn record_scores(&mut self, report: &ScoreReport) -> Result<i64>;
    /// The latest run of each probe. Empty when the schema predates scores.
    fn latest_score_runs(&self) -> Result<Vec<StoredRun>>;
    /// Selected models from the discovery tables, in ID order.
    fn selected_models(&self) -> Result<Vec<String>>;
    fn known_models(&self) -> Result<Vec<String>>;
}

fn version(store: &Store) -> Result<i64> {
    Ok(store
        .conn
        .pragma_query_value(None, "user_version", |r| r.get(0))?)
}

impl ScoreRepository for Store {
    fn record_scores(&mut self, report: &ScoreReport) -> Result<i64> {
        ensure!(!report.rows.is_empty(), "a score snapshot needs rows");
        ensure!(
            report.rows.iter().all(|r| r.value.is_finite()),
            "score values must be finite"
        );
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO score_runs (probe_id, parser_version, retrieved_at, origin, files, issues)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                report.probe_id,
                report.parser_version,
                report.retrieved_at,
                report.origin,
                serde_json::to_string(&report.files)?,
                serde_json::to_string(&report.issues)?,
            ],
        )?;
        let run = tx.last_insert_rowid();
        {
            let mut stmt = tx.prepare(
                "INSERT INTO score_rows (run_id, ord, metric, source_key, organization, value, as_of)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for (i, r) in report.rows.iter().enumerate() {
                stmt.execute(params![
                    run,
                    i as i64,
                    r.metric,
                    r.source_key,
                    r.organization,
                    r.value,
                    r.as_of
                ])?;
            }
        }
        let stale =
            "SELECT id FROM score_runs WHERE probe_id = ?1 ORDER BY id DESC LIMIT -1 OFFSET ?2";
        tx.execute(
            &format!("DELETE FROM score_rows WHERE run_id IN ({stale})"),
            params![report.probe_id, KEEP_RUNS],
        )?;
        tx.execute(
            &format!("DELETE FROM score_runs WHERE id IN ({stale})"),
            params![report.probe_id, KEEP_RUNS],
        )?;
        tx.commit()?;
        Ok(run)
    }

    fn latest_score_runs(&self) -> Result<Vec<StoredRun>> {
        if version(self)? < 3 {
            return Ok(Vec::new());
        }
        let tx = self.conn.unchecked_transaction()?;
        let mut runs: Vec<StoredRun> = tx
            .prepare(
                "SELECT id, probe_id, parser_version, retrieved_at, origin, files, issues
                 FROM score_runs r
                 WHERE id = (SELECT max(id) FROM score_runs WHERE probe_id = r.probe_id)
                 ORDER BY probe_id",
            )?
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u32>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            })?
            .map(|row| {
                let (id, probe_id, parser_version, retrieved_at, origin, files, issues) = row?;
                Ok(StoredRun {
                    id,
                    probe_id,
                    parser_version,
                    retrieved_at,
                    origin,
                    files: serde_json::from_str(&files)?,
                    issues: serde_json::from_str(&issues)?,
                    rows: Vec::new(),
                })
            })
            .collect::<Result<_>>()?;
        let mut stmt = tx.prepare(
            "SELECT metric, source_key, organization, value, as_of
             FROM score_rows WHERE run_id = ?1 ORDER BY ord",
        )?;
        for run in &mut runs {
            run.rows = stmt
                .query_map([run.id], |r| {
                    Ok(BoardRow {
                        metric: r.get(0)?,
                        source_key: r.get(1)?,
                        organization: r.get(2)?,
                        value: r.get(3)?,
                        as_of: r.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
        }
        Ok(runs)
    }

    fn selected_models(&self) -> Result<Vec<String>> {
        Ok(self
            .conn
            .prepare("SELECT id FROM models WHERE selected = 1 ORDER BY id")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    fn known_models(&self) -> Result<Vec<String>> {
        Ok(self
            .conn
            .prepare("SELECT id FROM models ORDER BY id")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }
}
