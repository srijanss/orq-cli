//! Orq's SQLite database (`orq.db` in the data root): an index of the review
//! reports Orq has stored, so a restarted Orq can find them by run, project,
//! feature or head SHA. The report files themselves live under the run's
//! directory; the database holds only their identity and relative path.

use std::io::{Error, ErrorKind, Result};
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::project_settings::{self, Delivery, ProjectAgent, ProjectSettings, SaveOutcome, StoredProjectSettings};
use crate::review_lifecycle::{Gate, ProviderState, ReviewLifecycle};
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
const MIGRATIONS: [&str; 6] = ["
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
", "
    CREATE TABLE review_lifecycles (
        run_id            TEXT    NOT NULL,
        reported_head_sha TEXT    NOT NULL,
        provider          TEXT    NOT NULL,
        review_id         TEXT    NOT NULL,
        url               TEXT    NOT NULL,
        state             TEXT    NOT NULL,
        gate              TEXT    NOT NULL,
        stale             INTEGER NOT NULL,
        observed_head_sha TEXT,
        last_error        TEXT,
        last_checked      INTEGER NOT NULL,
        PRIMARY KEY (run_id, reported_head_sha)
    );
", "
    CREATE TABLE run_pins (
        run_id    TEXT    NOT NULL PRIMARY KEY,
        pinned_at INTEGER NOT NULL
    );
", "
    CREATE TABLE prune_journal (
        rel      TEXT    NOT NULL PRIMARY KEY,
        run_id   TEXT    NOT NULL,
        begun_at INTEGER NOT NULL
    );
", "
    ALTER TABLE review_lifecycles ADD COLUMN observed INTEGER NOT NULL DEFAULT 0;
", "
    CREATE TABLE project_settings (
        project_root        TEXT    NOT NULL PRIMARY KEY,
        implementer_harness TEXT,
        implementer_model   TEXT,
        implementer_effort  TEXT,
        reviewer_harness    TEXT,
        reviewer_model      TEXT,
        reviewer_effort     TEXT,
        delivery            TEXT    NOT NULL DEFAULT 'review',
        revision            INTEGER NOT NULL,
        updated_at          INTEGER NOT NULL
    );
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

    /// Opens an existing database for reading only: nothing is created,
    /// tightened, migrated or switched to WAL. A database that is not at the
    /// schema this Orq understands is refused.
    pub fn open_read_only(path: &Path) -> Result<Store> {
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(path, flags).map_err(db)?;
        conn.busy_timeout(std::time::Duration::from_secs(10)).map_err(db)?;
        let mut store = Store { conn };
        let mut version = store.schema_version();
        if version.is_err() {
            // A WAL database in a directory nobody can write to cannot get its
            // shared-memory file, so a normal reader fails. Fall back to an
            // immutable view, which reads the main file only and writes nothing.
            if let Some(utf8) = path.to_str() {
                let uri = format!("file:{}?immutable=1", utf8.replace('%', "%25").replace('?', "%3f").replace('#', "%23"));
                let uri_flags = flags | rusqlite::OpenFlags::SQLITE_OPEN_URI;
                if let Ok(conn) = Connection::open_with_flags(&uri, uri_flags) {
                    let fallback = Store { conn };
                    if let Ok(v) = fallback.schema_version() {
                        store = fallback;
                        version = Ok(v);
                    }
                }
            }
        }
        let version = version?;
        if version != Self::LATEST_VERSION {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("orq.db has schema version {version}, not the {} this Orq reads; run Orq once to upgrade it", Self::LATEST_VERSION),
            ));
        }
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
        let full_path = data_root.join(&report_path);
        let previous = std::fs::read(&full_path).ok();
        write_atomic(data_root, Path::new(&report_path), &crate::redact::redact_bytes(report))?;
        let record = ReviewRecord { run_id: run_id.into(), project: project.into(), feature: feature.into(), head_sha: head_sha.into(), report_path };
        if let Err(error) = self.record_review(&record) {
            // The index did not take the report: put the file back as it was so
            // the file and the index still describe the same report.
            let rollback = match previous {
                Some(old) => write_atomic(data_root, Path::new(&record.report_path), &old),
                None => std::fs::remove_file(&full_path),
            };
            return Err(match rollback {
                Ok(()) => error,
                Err(undo) => Error::other(format!("{error}; and restoring the previous report failed: {undo}")),
            });
        }
        Ok(record)
    }

    /// Stores (or replaces) what the provider last showed about the review of
    /// `run_id` at its reported head. A recorded error is redacted first.
    pub fn save_lifecycle(&self, run_id: &str, lifecycle: &ReviewLifecycle) -> Result<()> {
        check_run_id(run_id)?;
        check_head_sha(&lifecycle.reported_head_sha)?;
        if let Some(observed) = &lifecycle.observed_head_sha {
            check_head_sha(observed)?;
        }
        let checked = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let error = lifecycle.last_error.as_deref().map(crate::redact::redact);
        self.conn
            .execute(
                "INSERT OR REPLACE INTO review_lifecycles (run_id, reported_head_sha, provider, review_id, url, state, gate, stale, observed_head_sha, last_error, last_checked, observed) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    run_id,
                    lifecycle.reported_head_sha,
                    lifecycle.provider,
                    lifecycle.review_id,
                    lifecycle.url,
                    lifecycle.state.as_str(),
                    lifecycle.gate.as_str(),
                    lifecycle.stale,
                    lifecycle.observed_head_sha,
                    error,
                    checked,
                    lifecycle.observed
                ],
            )
            .map(drop)
            .map_err(db)
    }

    pub fn lifecycles_for_run(&self, run_id: &str) -> Result<Vec<ReviewLifecycle>> {
        let mut statement = self
            .conn
            .prepare("SELECT provider, review_id, url, reported_head_sha, observed_head_sha, state, gate, stale, last_error, observed FROM review_lifecycles WHERE run_id = ?1 ORDER BY last_checked, rowid")
            .map_err(db)?;
        let rows = statement
            .query_map(params![run_id], |row| {
                let (state, gate): (String, String) = (row.get(5)?, row.get(6)?);
                Ok((
                    ReviewLifecycle {
                        provider: row.get(0)?,
                        review_id: row.get(1)?,
                        url: row.get(2)?,
                        reported_head_sha: row.get(3)?,
                        observed_head_sha: row.get(4)?,
                        state: ProviderState::Open,
                        gate: Gate::AwaitingHumanReview,
                        stale: row.get(7)?,
                        last_error: row.get(8)?,
                        observed: row.get(9)?,
                    },
                    state,
                    gate,
                ))
            })
            .map_err(db)?;
        let mut lifecycles = Vec::new();
        for row in rows {
            let (mut lifecycle, state, gate) = row.map_err(db)?;
            let corrupt = |what: &str, value: &str| Error::new(ErrorKind::InvalidData, format!("stored review {what} {value:?} is not recognised"));
            lifecycle.state = ProviderState::parse(&state).ok_or_else(|| corrupt("state", &state))?;
            lifecycle.gate = Gate::parse(&gate).ok_or_else(|| corrupt("gate", &gate))?;
            lifecycles.push(lifecycle);
        }
        Ok(lifecycles)
    }

    /// When the provider was last looked at for the review of `run_id` at `reported_head_sha`, in
    /// seconds since the epoch; `None` when it never was.
    pub fn lifecycle_checked_at(&self, run_id: &str, reported_head_sha: &str) -> Result<Option<i64>> {
        check_run_id(run_id)?;
        self.conn
            .query_row("SELECT last_checked FROM review_lifecycles WHERE run_id = ?1 AND reported_head_sha = ?2", params![run_id, reported_head_sha], |row| row.get(0))
            .optional()
            .map_err(db)
    }

    /// What the provider last showed about the run's review, and when.
    pub fn review_observation(&self, run_id: &str) -> Result<Option<crate::retention::ReviewFacts>> {
        check_run_id(run_id)?;
        let row = self
            .conn
            .query_row(
                "SELECT state, last_checked, last_error FROM review_lifecycles WHERE run_id = ?1 ORDER BY last_checked DESC, rowid DESC LIMIT 1",
                params![run_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, Option<String>>(2)?)),
            )
            .optional()
            .map_err(db)?;
        let Some((state, checked, last_error)) = row else { return Ok(None) };
        let state = ProviderState::parse(&state).ok_or_else(|| Error::new(ErrorKind::InvalidData, format!("stored review state {:?} is not recognised", state)))?;
        Ok(Some(crate::retention::ReviewFacts { state, observed_at: checked.max(0) as u64, last_error }))
    }

    /// Records, before a file is deleted, that it is about to be.
    pub fn begin_removal(&self, run_id: &str, rel: &str) -> Result<()> {
        check_run_id(run_id)?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        self.conn.execute("INSERT OR IGNORE INTO prune_journal (rel, run_id, begun_at) VALUES (?1, ?2, ?3)", params![rel, run_id, now]).map(drop).map_err(db)
    }

    /// Removals begun and not finished, as `(run id, path relative to the data root)`, sorted by path.
    pub fn pending_removals(&self) -> Result<Vec<(String, String)>> {
        let mut statement = self.conn.prepare("SELECT run_id, rel FROM prune_journal ORDER BY rel").map_err(db)?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?))).map_err(db)?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(db)
    }

    /// Completes a removal: forgets the journal entry and any report index row that named the file.
    pub fn finish_removal(&self, rel: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction().map_err(db)?;
        tx.execute("DELETE FROM reviews WHERE report_path = ?1", params![rel]).map_err(db)?;
        tx.execute("DELETE FROM prune_journal WHERE rel = ?1", params![rel]).map_err(db)?;
        tx.commit().map_err(db)
    }

    /// Gives up on a journaled removal that could not be carried out; the file stays and so does its index row.
    pub fn abandon_removal(&self, rel: &str) -> Result<()> {
        self.conn.execute("DELETE FROM prune_journal WHERE rel = ?1", params![rel]).map(drop).map_err(db)
    }

    /// Forgets everything the database holds about a run's reviews.
    pub fn remove_run_metadata(&self, run_id: &str) -> Result<()> {
        check_run_id(run_id)?;
        let tx = self.conn.unchecked_transaction().map_err(db)?;
        tx.execute("DELETE FROM reviews WHERE run_id = ?1", params![run_id]).map_err(db)?;
        tx.execute("DELETE FROM review_lifecycles WHERE run_id = ?1", params![run_id]).map_err(db)?;
        tx.commit().map_err(db)
    }

    /// Whether anything `remove_run_metadata` would forget is still stored for the run.
    pub fn has_run_metadata(&self, run_id: &str) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM reviews WHERE run_id = ?1) OR EXISTS (SELECT 1 FROM review_lifecycles WHERE run_id = ?1)",
                params![run_id],
                |row| row.get(0),
            )
            .map_err(db)
    }

    /// Exempts a run from pruning until it is unpinned. Pinning twice is a no-op.
    pub fn pin(&self, run_id: &str) -> Result<()> {
        check_run_id(run_id)?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        self.conn.execute("INSERT OR IGNORE INTO run_pins (run_id, pinned_at) VALUES (?1, ?2)", params![run_id, now]).map(drop).map_err(db)
    }

    pub fn unpin(&self, run_id: &str) -> Result<()> {
        check_run_id(run_id)?;
        self.conn.execute("DELETE FROM run_pins WHERE run_id = ?1", params![run_id]).map(drop).map_err(db)
    }

    pub fn is_pinned(&self, run_id: &str) -> Result<bool> {
        self.conn.query_row("SELECT EXISTS (SELECT 1 FROM run_pins WHERE run_id = ?1)", params![run_id], |row| row.get(0)).map_err(db)
    }

    /// Every pinned run id, sorted.
    pub fn pinned(&self) -> Result<Vec<String>> {
        let mut statement = self.conn.prepare("SELECT run_id FROM run_pins ORDER BY run_id").map_err(db)?;
        let rows = statement.query_map([], |row| row.get(0)).map_err(db)?;
        rows.collect::<std::result::Result<Vec<String>, _>>().map_err(db)
    }

    /// The settings saved for the project whose main checkout is `root`; a project with no record
    /// has the defaults at revision 0.
    pub fn project_settings(&self, root: &Path) -> Result<StoredProjectSettings> {
        let key = project_key(root)?;
        let row = self
            .conn
            .query_row(
                "SELECT implementer_harness, implementer_model, implementer_effort, reviewer_harness, reviewer_model, reviewer_effort, delivery, revision FROM project_settings WHERE project_root = ?1",
                params![key],
                |row| {
                    let agent = |at: usize| -> rusqlite::Result<Option<ProjectAgent>> {
                        let harness: Option<String> = row.get(at)?;
                        let model: Option<String> = row.get(at + 1)?;
                        let effort: Option<String> = row.get(at + 2)?;
                        Ok(harness.zip(model).map(|(harness, model)| ProjectAgent { harness, model, effort }))
                    };
                    Ok((agent(0)?, agent(3)?, row.get::<_, String>(6)?, row.get::<_, i64>(7)?))
                },
            )
            .optional()
            .map_err(db)?;
        let Some((implementer, reviewer, delivery, revision)) = row else {
            return Ok(StoredProjectSettings { settings: ProjectSettings::default(), revision: 0 });
        };
        let delivery = Delivery::parse(&delivery)
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, format!("orq.db has an unknown delivery {delivery:?} for {key}")))?;
        Ok(StoredProjectSettings { settings: ProjectSettings { implementer, reviewer, delivery }, revision: revision as u64 })
    }

    /// Saves a project's settings if it is still at `expected_revision`; otherwise nothing is
    /// written and the outcome is a conflict.
    pub fn save_project_settings(&mut self, root: &Path, expected_revision: u64, settings: &ProjectSettings) -> Result<SaveOutcome> {
        let key = project_key(root)?;
        project_settings::check(settings)?;
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(db)?;
        let current: u64 = tx
            .query_row("SELECT revision FROM project_settings WHERE project_root = ?1", params![key], |row| row.get::<_, i64>(0))
            .optional()
            .map_err(db)?
            .map_or(0, |revision| revision as u64);
        if current != expected_revision {
            return Ok(SaveOutcome::Conflict);
        }
        let revision = current + 1;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let field = |agent: &Option<ProjectAgent>, pick: fn(&ProjectAgent) -> Option<&str>| agent.as_ref().and_then(pick).map(str::to_string);
        let (implementer, reviewer) = (&settings.implementer, &settings.reviewer);
        tx.execute(
            "INSERT INTO project_settings (project_root, implementer_harness, implementer_model, implementer_effort, reviewer_harness, reviewer_model, reviewer_effort, delivery, revision, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (project_root) DO UPDATE SET
                 implementer_harness = excluded.implementer_harness, implementer_model = excluded.implementer_model, implementer_effort = excluded.implementer_effort,
                 reviewer_harness = excluded.reviewer_harness, reviewer_model = excluded.reviewer_model, reviewer_effort = excluded.reviewer_effort,
                 delivery = excluded.delivery, revision = excluded.revision, updated_at = excluded.updated_at",
            params![
                key,
                field(implementer, |a| Some(&a.harness)),
                field(implementer, |a| Some(&a.model)),
                field(implementer, |a| a.effort.as_deref()),
                field(reviewer, |a| Some(&a.harness)),
                field(reviewer, |a| Some(&a.model)),
                field(reviewer, |a| a.effort.as_deref()),
                settings.delivery.as_str(),
                revision as i64,
                now
            ],
        )
        .map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(SaveOutcome::Saved { revision })
    }

    /// Every stored report, oldest first.
    pub fn all_reviews(&self) -> Result<Vec<ReviewRecord>> {
        self.query("1 = 1", [])
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
pub(crate) fn check_report_path(run_id: &str, report_path: &str) -> Result<()> {
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

/// A project is keyed by its main checkout's absolute UTF-8 path, spelled one way: repeated
/// separators, `.` steps and a trailing separator are dropped. A `..` step is refused, since
/// through a symlink it could name a different checkout than it appears to.
fn project_key(root: &Path) -> Result<String> {
    use std::path::Component;
    let refuse = || Error::new(ErrorKind::InvalidInput, format!("project root {} must be an absolute UTF-8 path without `..`", root.display()));
    if !root.is_absolute() || root.components().any(|part| part == Component::ParentDir) {
        return Err(refuse());
    }
    root.components().collect::<std::path::PathBuf>().into_os_string().into_string().map_err(|_| refuse())
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
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        // NOFOLLOW refuses a symlinked database instead of opening its target.
        options.mode(0o600).custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
        let file = options.open(path)?;
        // A file that already existed keeps its old mode; tighten it
        // (fchmod fails for a file owned by someone else).
        if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    options.open(path).map(drop)
}
