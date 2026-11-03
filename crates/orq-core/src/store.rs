//! Orq's SQLite database (`orq.db` in the data root): an index of the review
//! reports Orq has stored, so a restarted Orq can find them by run, project,
//! feature or head SHA. The report files themselves live under the run's
//! directory; the database holds only their identity and relative path.

use std::io::{Error, ErrorKind, Result};
use std::path::Path;

use rusqlite::{params, Connection, TransactionBehavior};

use crate::storage::{check_head_sha, check_run_id, write_atomic};

/// One stored review report, pinned to the head SHA it reviewed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRecord {
    pub run_id: String,
    pub project: String,
    pub feature: String,
    pub head_sha: String,
    /// Path of the report relative to the data root.
    pub report_path: String,
}

/// Each entry upgrades the schema by one version; entries are never edited
/// once released, only appended.
const MIGRATIONS: [&str; 1] = ["
    CREATE TABLE reviews (
        run_id      TEXT    NOT NULL,
        project     TEXT    NOT NULL,
        feature     TEXT    NOT NULL,
        head_sha    TEXT    NOT NULL,
        report_path TEXT    NOT NULL,
        recorded_at INTEGER NOT NULL,
        PRIMARY KEY (run_id, head_sha)
    );
    CREATE INDEX reviews_by_feature ON reviews (project, feature);
    CREATE INDEX reviews_by_head_sha ON reviews (head_sha);
"];

pub struct Store {
    conn: Connection,
}

impl Store {
    pub const LATEST_VERSION: u32 = MIGRATIONS.len() as u32;

    /// Opens (creating owner-only if needed) and migrates the database. A
    /// database written by a newer Orq is refused rather than downgraded.
    pub fn open(path: &Path) -> Result<Store> {
        create_private_file(path)?;
        let conn = Connection::open(path).map_err(db)?;
        conn.busy_timeout(std::time::Duration::from_secs(10)).map_err(db)?;
        conn.pragma_update(None, "journal_mode", "WAL").map_err(db)?;
        let mut store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn schema_version(&self) -> Result<u32> {
        self.conn.pragma_query_value(None, "user_version", |row| row.get(0)).map_err(db)
    }

    fn migrate(&mut self) -> Result<()> {
        // An up-to-date database needs no write lock, so opening it never waits
        // behind a writer.
        if self.schema_version()? == Self::LATEST_VERSION {
            return Ok(());
        }
        // The version is re-read inside the write lock so two processes
        // opening at once cannot both apply the same migration.
        let tx =self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(db)?;
        let current: u32 = tx.pragma_query_value(None, "user_version", |row| row.get(0)).map_err(db)?;
        if current > Self::LATEST_VERSION {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("orq.db has schema version {current}, newer than this Orq understands ({})", Self::LATEST_VERSION),
            ));
        }
        for (index, migration) in MIGRATIONS.iter().enumerate().skip(current as usize) {
            tx.execute_batch(migration).map_err(db)?;
            tx.pragma_update(None, "user_version", index as u32 + 1).map_err(db)?;
        }
        tx.commit().map_err(db)
    }

    /// Stores (or replaces) the report for `run_id` at `head_sha`. Identifiers
    /// that could name a path outside the run's directory are refused.
    pub fn record_review(&self, record: &ReviewRecord) -> Result<()> {
        check_run_id(&record.run_id)?;
        check_head_sha(&record.head_sha)?;
        check_report_path(&record.run_id, &record.report_path)?;
        let recorded_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        self.conn
            .execute(
                "INSERT OR REPLACE INTO reviews (run_id, project, feature, head_sha, report_path, recorded_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![record.run_id, record.project, record.feature, record.head_sha, record.report_path, recorded_at],
            )
            .map(drop)
            .map_err(db)
    }

    /// Writes `report` to `runs/<run_id>/review/<head_sha>/review.json` under
    /// `data_root` atomically, then indexes it. The file goes first, so the
    /// index never points at a report that was not written.
    pub fn save_review(&self, data_root: &Path, run_id: &str, project: &str, feature: &str, head_sha: &str, report: &[u8]) -> Result<ReviewRecord> {
        check_run_id(run_id)?;
        check_head_sha(head_sha)?;
        let report_path = format!("runs/{run_id}/review/{head_sha}/review.json");
        write_atomic(data_root, Path::new(&report_path), report)?;
        let record = ReviewRecord { run_id: run_id.into(), project: project.into(), feature: feature.into(), head_sha: head_sha.into(), report_path };
        self.record_review(&record)?;
        Ok(record)
    }

    pub fn reviews_for_run(&self, run_id: &str) -> Result<Vec<ReviewRecord>> {
        self.query("run_id = ?1", params![run_id])
    }

    pub fn reviews_for_project(&self, project: &str) -> Result<Vec<ReviewRecord>> {
        self.query("project = ?1", params![project])
    }

    pub fn reviews_for_feature(&self, project: &str, feature: &str) -> Result<Vec<ReviewRecord>> {
        self.query("project = ?1 AND feature = ?2", params![project, feature])
    }

    pub fn reviews_for_head_sha(&self, head_sha: &str) -> Result<Vec<ReviewRecord>> {
        self.query("head_sha = ?1", params![head_sha])
    }

    fn query(&self, condition: &str, values: impl rusqlite::Params) -> Result<Vec<ReviewRecord>> {
        let sql = format!(
            "SELECT run_id, project, feature, head_sha, report_path FROM reviews WHERE {condition} ORDER BY recorded_at, rowid"
        );
        let mut statement = self.conn.prepare(&sql).map_err(db)?;
        let rows = statement
            .query_map(values, |row| {
                Ok(ReviewRecord { run_id: row.get(0)?, project: row.get(1)?, feature: row.get(2)?, head_sha: row.get(3)?, report_path: row.get(4)? })
            })
            .map_err(db)?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(db)
    }
}

/// A report path is relative to the data root and must sit under its own
/// run's directory, so joining it to the root can never leave the root.
fn check_report_path(run_id: &str, report_path: &str) -> Result<()> {
    use std::path::Component;
    let mut parts = Path::new(report_path).components();
    let inside = matches!(parts.next(), Some(Component::Normal(first)) if first == "runs")
        && matches!(parts.next(), Some(Component::Normal(second)) if second == run_id)
        && parts.clone().next().is_some()
        && parts.all(|part| matches!(part, Component::Normal(_)));
    if inside {
        Ok(())
    } else {
        Err(Error::new(ErrorKind::InvalidInput, format!("report path {report_path:?} is not inside runs/{run_id}/")))
    }
}

fn db(error: rusqlite::Error) -> Error {
    Error::other(error)
}


/// Creates the database file owner-only before SQLite can create it with the
/// process umask; the WAL and shared-memory files inherit its mode.
fn create_private_file(path: &Path) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map(drop)
}
