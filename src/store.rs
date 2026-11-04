//! `rostr.db`: automatically collected data. It holds the complete list of
//! known models, where each one was found, and the scan history.
//!
//! The `selected` and `reason` columns are derived from `scope.yaml` on every
//! save. They are a resolved snapshot for queries, not a second copy of user
//! intent: `scope.yaml` always wins, and the next save rewrites them.

use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, params};

use crate::discover::{Candidate, Source};
use crate::model_id;

pub const FILE_NAME: &str = "rostr.db";
const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = "
CREATE TABLE models (
    id          TEXT PRIMARY KEY,
    provider    TEXT NOT NULL,
    name        TEXT NOT NULL,
    first_seen  TEXT NOT NULL,
    -- Time of the last scan that found the model. NULL: never found by a scan.
    last_seen   TEXT,
    -- Derived from scope.yaml at resolved_at.
    selected    INTEGER NOT NULL,
    reason      TEXT NOT NULL,
    resolved_at TEXT NOT NULL
);
CREATE TABLE model_sources (
    model_id  TEXT NOT NULL REFERENCES models(id) ON DELETE CASCADE,
    agent     TEXT NOT NULL,
    via       TEXT NOT NULL,
    last_seen TEXT NOT NULL,
    PRIMARY KEY (model_id, agent, via)
);
CREATE TABLE scans (
    id           INTEGER PRIMARY KEY,
    at           TEXT NOT NULL,
    agents       TEXT NOT NULL,
    models_found INTEGER NOT NULL
);
";

/// A model already recorded in the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Known {
    pub id: String,
    pub name: String,
    pub sources: Vec<Source>,
}

/// The resolved state of one model, to record after a save.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub id: String,
    pub name: String,
    pub selected: bool,
    pub reason: String,
}

pub struct Record<'a> {
    pub agents: Vec<&'a str>,
    pub candidates: &'a [Candidate],
    pub models: Vec<Resolved>,
    pub removed: Vec<String>,
}

pub struct Store {
    pub(crate) conn: Connection,
}

impl Store {
    /// Opens the database, and creates it and its directory when needed.
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
        let conn =
            Connection::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let store = Store { conn };
        store
            .migrate()
            .with_context(|| format!("cannot set up {}", path.display()))?;
        Ok(store)
    }

    /// Opens an existing database read-only. `Ok(None)` when it does not exist.
    pub fn open_read_only(path: &Path) -> Result<Option<Store>> {
        if !path.exists() {
            return Ok(None);
        }
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("cannot open {}", path.display()))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version != 1 && version != SCHEMA_VERSION {
            bail!(
                "{} has schema version {version}; run `rostr discover` to update it",
                path.display()
            );
        }
        Ok(Some(Store { conn }))
    }

    fn migrate(&self) -> Result<()> {
        self.conn.pragma_update(None, "foreign_keys", true)?;
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            bail!("database schema version {version} is newer than this rostr supports");
        }
        let tx = self.conn.unchecked_transaction()?;
        if version == 0 {
            tx.execute_batch(SCHEMA)?;
        }
        if version < 2 {
            tx.execute_batch(include_str!("quota/schema.sql"))?;
        }
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.commit()?;
        Ok(())
    }

    pub fn known(&self) -> Result<Vec<Known>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name FROM models ORDER BY id")?;
        let mut models: Vec<Known> = stmt
            .query_map([], |r| {
                Ok(Known {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    sources: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut stmt = self.conn.prepare(
            "SELECT agent, via FROM model_sources WHERE model_id = ?1 ORDER BY agent, via",
        )?;
        for m in &mut models {
            m.sources = stmt
                .query_map([&m.id], |r| {
                    Ok(Source::new(
                        &r.get::<_, String>(0)?,
                        &r.get::<_, String>(1)?,
                    ))
                })?
                .collect::<rusqlite::Result<_>>()?;
        }
        Ok(models)
    }

    /// Records one confirmed discovery in a single transaction.
    pub fn record(&mut self, rec: &Record) -> Result<()> {
        let tx = self.conn.transaction()?;
        let now: String =
            tx.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now')", [], |r| {
                r.get(0)
            })?;
        for id in &rec.removed {
            tx.execute("DELETE FROM models WHERE id = ?1", [id])?;
        }
        for m in &rec.models {
            tx.execute(
                "INSERT INTO models (id, provider, name, first_seen, selected, reason, resolved_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?4)
                 ON CONFLICT (id) DO UPDATE SET
                     name = excluded.name, selected = excluded.selected,
                     reason = excluded.reason, resolved_at = excluded.resolved_at",
                params![
                    m.id,
                    model_id::provider_of(&m.id),
                    m.name,
                    now,
                    m.selected,
                    m.reason
                ],
            )?;
        }
        for c in rec.candidates {
            tx.execute(
                "UPDATE models SET last_seen = ?2 WHERE id = ?1",
                params![c.id, now],
            )?;
            for s in &c.sources {
                tx.execute(
                    "INSERT INTO model_sources (model_id, agent, via, last_seen)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (model_id, agent, via) DO UPDATE SET last_seen = excluded.last_seen",
                    params![c.id, s.agent, s.via, now],
                )?;
            }
        }
        tx.execute(
            "INSERT INTO scans (at, agents, models_found) VALUES (?1, ?2, ?3)",
            params![now, rec.agents.join(", "), rec.candidates.len() as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    #[cfg(test)]
    fn row(&self, id: &str) -> Option<(bool, String, Option<String>)> {
        self.conn
            .query_row(
                "SELECT selected, reason, last_seen FROM models WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str, agent: &str) -> Candidate {
        Candidate {
            id: id.into(),
            name: model_id::display_name(id),
            sources: vec![Source::new(agent, "model list")],
        }
    }

    fn resolved(id: &str, selected: bool) -> Resolved {
        Resolved {
            id: id.into(),
            name: model_id::display_name(id),
            selected,
            reason: "test".into(),
        }
    }

    #[test]
    fn records_models_sources_and_removals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join(FILE_NAME);
        assert!(Store::open_read_only(&path).unwrap().is_none());

        let mut store = Store::open(&path).unwrap();
        let cands = [cand("a/x", "Codex"), cand("a/y", "Grok CLI")];
        store
            .record(&Record {
                agents: vec!["Codex", "Grok CLI"],
                candidates: &cands,
                models: vec![
                    resolved("a/x", true),
                    resolved("a/y", false),
                    resolved("b/manual", true),
                ],
                removed: vec![],
            })
            .unwrap();
        let known = store.known().unwrap();
        assert_eq!(known.len(), 3);
        assert_eq!(known[0].sources, vec![Source::new("Codex", "model list")]);
        assert!(
            store.row("b/manual").unwrap().2.is_none(),
            "manual model was never scanned"
        );

        // A later scan that misses `a/y` keeps it; a removal deletes it and its sources.
        store
            .record(&Record {
                agents: vec!["Codex"],
                candidates: &cands[..1],
                models: vec![resolved("a/x", false), resolved("b/manual", true)],
                removed: vec!["a/y".into()],
            })
            .unwrap();
        assert!(!store.row("a/x").unwrap().0);
        assert!(store.row("a/y").is_none());
        let n: i64 = store
            .conn
            .query_row("SELECT count(*) FROM model_sources", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);

        drop(store);
        let ro = Store::open_read_only(&path).unwrap().unwrap();
        assert_eq!(ro.known().unwrap().len(), 2);
    }
}
