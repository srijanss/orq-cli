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

pub mod run_queue;

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
/// once released, only appended. Version 3's `run_pins` belonged to run pins, which were removed:
/// version 7 drops it in code (see [`PINS_REMOVED_VERSION`]).
const MIGRATIONS: [&str; 9] = ["
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
", "
", "
    ALTER TABLE project_settings ADD COLUMN max_active_runs INTEGER;
    ALTER TABLE project_settings ADD COLUMN min_free_disk_gb INTEGER;
", "
    CREATE TABLE run_queue (
        project_root TEXT    NOT NULL,
        feature      TEXT    NOT NULL,
        position     INTEGER NOT NULL,
        added_at     INTEGER NOT NULL,
        added_by     TEXT    NOT NULL,
        implementer  TEXT,
        reviewer     TEXT,
        run_id       TEXT,
        failed       INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (project_root, feature)
    );
    CREATE TABLE run_queue_paused (
        project_root TEXT NOT NULL PRIMARY KEY,
        reason       TEXT NOT NULL
    );
    ALTER TABLE project_settings ADD COLUMN queue_hours TEXT;
    ALTER TABLE project_settings ADD COLUMN queue_stop_on_failure INTEGER NOT NULL DEFAULT 0;
"];

/// Whether each migration, by version, changes or drops something an older Orq reads or writes. A migration that
/// only adds (a table, a column with a default, an index) leaves older Orq binaries using the database; one that
/// breaks them raises the database's `compat_from`, so they are refused instead of misreading it.
const BREAKING: [bool; MIGRATIONS.len()] = [true, false, false, false, false, false, true, false, false];

/// Whether migration `version` leaves an older Orq unable to use the database. An unknown version is taken as breaking.
pub fn is_breaking(version: u32) -> bool {
    version.checked_sub(1).and_then(|index| BREAKING.get(index as usize)).copied().unwrap_or(true)
}

/// The oldest schema an Orq must understand to use a database at `version`: that of its newest breaking migration.
fn compat_after(version: u32) -> u32 {
    (1..=version).rev().find(|v| is_breaking(*v)).unwrap_or(0)
}

/// The one-row marker of the version it was written for and that version's `compat_from`. It sits outside the
/// numbered migrations, so writing it never moves the schema past what an older Orq knows.
const COMPAT_MARKER: &str = "
    CREATE TABLE IF NOT EXISTS schema_compat (
        only        INTEGER PRIMARY KEY CHECK (only = 1),
        version     INTEGER NOT NULL,
        compat_from INTEGER NOT NULL
    );
";

/// The schema version that removed run pins: an empty `run_pins` is dropped, and one with pins in
/// it is kept until [`Store::take_removed_pins`] has counted them.
const PINS_REMOVED_VERSION: u32 = 7;

/// The schema version whose migration added `project_settings`.
const PROJECT_SETTINGS_VERSION: u32 = 6;

/// The schema version whose migration added a project's run limit and disk floor.
const RUN_LIMITS_VERSION: u32 = 8;

/// The schema version whose migration added the run queue and a project's queue settings.
const RUN_QUEUE_VERSION: u32 = 9;

pub struct Store {
    conn: Connection,
}

/// The marker's version and `compat_from`; `None` when no marking Orq has migrated the database.
fn read_marker(conn: &Connection) -> Result<Option<(u32, u32)>> {
    let exists: bool = conn
        .query_row("SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_compat')", [], |row| row.get(0))
        .map_err(db)?;
    if !exists {
        return Ok(None);
    }
    conn.query_row("SELECT version, compat_from FROM schema_compat WHERE only = 1", [], |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(db)
}

impl Store {
    pub const LATEST_VERSION: u32 = MIGRATIONS.len() as u32;

    /// Opens (creating owner-only if needed) and migrates the database. A
    /// database written by a newer Orq is refused rather than downgraded.
    pub fn open(path: &Path) -> Result<Store> {
        Self::open_understanding(path, Self::LATEST_VERSION)
    }

    /// [`Store::open`] as an Orq that understands schema versions up to `latest` (at most this Orq's own), so tests
    /// can stand in for an older Orq.
    #[doc(hidden)]
    pub fn open_understanding(path: &Path, latest: u32) -> Result<Store> {
        let latest = latest.min(Self::LATEST_VERSION);
        create_private_file(path)?;
        let conn = Connection::open(path).map_err(db)?;
        conn.busy_timeout(std::time::Duration::from_secs(10)).map_err(db)?;
        conn.pragma_update(None, "journal_mode", "WAL").map_err(db)?;
        let mut store = Store { conn };
        store.migrate(latest)?;
        Ok(store)
    }

    /// Opens an existing database for reading only: nothing is created,
    /// tightened, migrated or switched to WAL. A database that is not at the
    /// schema this Orq understands is refused.
    pub fn open_read_only(path: &Path) -> Result<Store> {
        let (store, version) = Self::open_read_only_at_any_version(path)?;
        if version > Self::LATEST_VERSION {
            store.check_newer(version, Self::LATEST_VERSION)?;
        } else if version != Self::LATEST_VERSION {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("orq.db has schema version {version}, not the {} this Orq reads; run Orq once to upgrade it", Self::LATEST_VERSION),
            ));
        }
        Ok(store)
    }

    /// The settings saved for a project, read without writing anything: the database is not
    /// created, migrated or switched to WAL. A database from before project settings existed has
    /// none; one newer than this Orq understands is refused.
    pub fn read_project_settings(path: &Path, root: &Path) -> Result<StoredProjectSettings> {
        let (store, version) = Self::open_read_only_at_any_version(path)?;
        if version > Self::LATEST_VERSION {
            store.check_newer(version, Self::LATEST_VERSION)?;
        }
        if version < PROJECT_SETTINGS_VERSION {
            project_key(root)?;
            return Ok(StoredProjectSettings { settings: ProjectSettings::default(), revision: 0 });
        }
        store.project_settings_at(root, version)
    }

    /// The schema `Store::open` would upgrade the database at `path` from, and the version an Orq must understand
    /// afterwards, when the upgrade is breaking: when it would raise the database's `compat_from`, leaving an Orq that
    /// uses it now unable to. `None` without a database, or when the pending migrations only add.
    pub fn breaking_upgrade(path: &Path) -> Result<Option<(u32, u32)>> {
        if !path.exists() {
            return Ok(None);
        }
        let (_, version) = Self::open_read_only_at_any_version(path)?;
        let needed = compat_after(Self::LATEST_VERSION);
        Ok((version < Self::LATEST_VERSION && needed > compat_after(version)).then_some((version, needed)))
    }

    fn open_read_only_at_any_version(path: &Path) -> Result<(Store, u32)> {
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(path, flags).map_err(db)?;
        conn.busy_timeout(std::time::Duration::from_secs(10)).map_err(db)?;
        let mut store = Store { conn };
        let mut version = store.schema_version();
        if version.is_err() && !wal_may_hold_commits(path) {
            // A WAL database in a directory nobody can write to cannot get its
            // shared-memory file, so a normal reader fails. Fall back to an
            // immutable view, which reads the main file only and writes nothing.
            // With a non-empty -wal file that view could miss committed changes,
            // so the read error stands instead.
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
        Ok((store, version?))
    }

    pub fn schema_version(&self) -> Result<u32> {
        self.conn.pragma_query_value(None, "user_version", |row| row.get(0)).map_err(db)
    }

    /// The oldest schema version an Orq must understand to use this database, as its marker says; `None` for a
    /// database no marking Orq has migrated.
    pub fn compat_from(&self) -> Result<Option<u32>> {
        Ok(read_marker(&self.conn)?.map(|(_, compat)| compat))
    }

    /// Lets an Orq that understands up to `latest` use a database at the newer `version` only when the marker,
    /// written for that same version, says nothing since `latest` broke what it reads or writes.
    fn check_newer(&self, version: u32, latest: u32) -> Result<()> {
        match read_marker(&self.conn)? {
            Some((marked, compat)) if marked == version && compat <= latest => Ok(()),
            Some((marked, compat)) if marked == version => Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "orq.db has schema version {version}, which needs an Orq that understands version {compat} or newer; \
                     this Orq understands {latest}: restart runs started before the upgrade"
                ),
            )),
            _ => Err(Error::new(ErrorKind::InvalidData, format!("orq.db has schema version {version}, newer than this Orq understands ({latest})"))),
        }
    }

    fn migrate(&mut self, latest: u32) -> Result<()> {
        // An up-to-date or compatible newer database needs no write lock, so opening it never waits behind a writer.
        let version = self.schema_version()?;
        if version == latest {
            return Ok(());
        }
        if version > latest {
            return self.check_newer(version, latest);
        }
        // The version is re-read inside the write lock so two processes
        // opening at once cannot both apply the same migration.
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(db)?;
        let current: u32 = tx.pragma_query_value(None, "user_version", |row| row.get(0)).map_err(db)?;
        if current >= latest {
            drop(tx);
            return if current == latest { Ok(()) } else { self.check_newer(current, latest) };
        }
        for (index, migration) in MIGRATIONS.iter().enumerate().take(latest as usize).skip(current as usize) {
            tx.execute_batch(migration).map_err(db)?;
            if index as u32 + 1 == PINS_REMOVED_VERSION && !has_pins(&tx)? {
                drop_pins_table(&tx)?;
            }
            tx.pragma_update(None, "user_version", index as u32 + 1).map_err(db)?;
        }
        tx.execute_batch(COMPAT_MARKER).map_err(db)?;
        tx.execute("INSERT OR REPLACE INTO schema_compat (only, version, compat_from) VALUES (1, ?1, ?2)", params![latest, compat_after(latest)])
            .map_err(db)?;
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

    /// Stores `next` as the lifecycle of `run_id` at its reported head only while the stored one is still
    /// `expected` (`None`: nothing stored for that head yet) and that head is still the run's latest
    /// report, checked and written under one write lock. Returns whether it was stored; `false` means
    /// someone changed the lifecycle or published a newer report since it was read.
    pub fn replace_lifecycle(&self, run_id: &str, expected: Option<&ReviewLifecycle>, next: &ReviewLifecycle) -> Result<bool> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate).map_err(db)?;
        if self.reviews_for_run(run_id)?.last().is_some_and(|latest| latest.head_sha != next.reported_head_sha) {
            return Ok(false);
        }
        let stored = self.lifecycles_for_run(run_id)?.into_iter().rev().find(|l| l.reported_head_sha == next.reported_head_sha);
        if stored.as_ref() != expected {
            return Ok(false);
        }
        self.save_lifecycle(run_id, next)?;
        tx.commit().map_err(db)?;
        Ok(true)
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

    /// The runs whose newest provider review (not one kept only here) is open and that were not merged or
    /// set aside here, each with when the
    /// provider was last looked at, in seconds since the epoch; `None` when that time cannot be read.
    /// With `run_id`, only that run.
    pub fn open_provider_reviews(&self, run_id: Option<&str>) -> Result<Vec<(String, Option<i64>)>> {
        if let Some(run_id) = run_id {
            check_run_id(run_id)?;
        }
        let mut statement = self
            .conn
            .prepare(
                "SELECT l.run_id, CASE WHEN typeof(l.last_checked) = 'integer' THEN l.last_checked END FROM review_lifecycles l \
                 WHERE l.provider != ?1 AND l.state = 'open' AND (?2 IS NULL OR l.run_id = ?2) \
                 AND l.rowid = (SELECT MAX(n.rowid) FROM review_lifecycles n WHERE n.run_id = l.run_id AND n.provider != ?1) \
                 AND NOT EXISTS (SELECT 1 FROM review_lifecycles m WHERE m.run_id = l.run_id AND m.provider = ?1 AND m.state IN ('merged', 'closed') \
                 AND m.reported_head_sha = (SELECT r.head_sha FROM reviews r WHERE r.run_id = l.run_id ORDER BY r.recorded_at DESC, r.rowid DESC LIMIT 1)) \
                 ORDER BY l.run_id",
            )
            .map_err(db)?;
        let rows = statement.query_map(params![crate::review_lifecycle::LOCAL_PROVIDER, run_id], |row| Ok((row.get(0)?, row.get(1)?))).map_err(db)?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(db)
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

    /// How many runs were pinned before pins were removed, counted once: the `run_pins` table the
    /// upgrade kept for them is dropped, so a later call finds none.
    /// One immediate transaction, so two callers at once serialize and only the first counts them.
    pub fn take_removed_pins(&self) -> Result<u64> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate).map_err(db)?;
        if !pins_table_exists(&tx)? {
            return Ok(0);
        }
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM run_pins", [], |row| row.get(0)).map_err(db)?;
        drop_pins_table(&tx)?;
        tx.commit().map_err(db)?;
        Ok(count.max(0) as u64)
    }

    /// The settings saved for the project whose main checkout is `root`; a project with no record
    /// has the defaults at revision 0.
    pub fn project_settings(&self, root: &Path) -> Result<StoredProjectSettings> {
        self.project_settings_at(root, Self::LATEST_VERSION)
    }

    /// [`Store::project_settings`] from a database at schema `version`: columns added after it read
    /// as unset.
    fn project_settings_at(&self, root: &Path, version: u32) -> Result<StoredProjectSettings> {
        let key = project_key(root)?;
        let limits = if version >= RUN_LIMITS_VERSION { "max_active_runs, min_free_disk_gb" } else { "NULL, NULL" };
        let queue = if version >= RUN_QUEUE_VERSION { "queue_hours, queue_stop_on_failure" } else { "NULL, 0" };
        let row = self
            .conn
            .query_row(
                &format!("SELECT implementer_harness, implementer_model, implementer_effort, reviewer_harness, reviewer_model, reviewer_effort, delivery, revision, {limits}, {queue} FROM project_settings WHERE project_root = ?1"),
                params![key],
                |row| {
                    let agent = |at: usize| -> rusqlite::Result<Option<ProjectAgent>> {
                        let harness: Option<String> = row.get(at)?;
                        let model: Option<String> = row.get(at + 1)?;
                        let effort: Option<String> = row.get(at + 2)?;
                        Ok(harness.zip(model).map(|(harness, model)| ProjectAgent { harness, model, effort }))
                    };
                    Ok((agent(0)?, agent(3)?, row.get::<_, String>(6)?, row.get::<_, i64>(7)?, row.get::<_, Option<i64>>(8)?, row.get::<_, Option<i64>>(9)?, row.get::<_, Option<String>>(10)?, row.get::<_, bool>(11)?))
                },
            )
            .optional()
            .map_err(db)?;
        let Some((implementer, reviewer, delivery, revision, max_active_runs, min_free_disk_gb, queue_hours, queue_stop_on_failure)) = row else {
            return Ok(StoredProjectSettings { settings: ProjectSettings::default(), revision: 0 });
        };
        let delivery = Delivery::parse(&delivery)
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, format!("orq.db has an unknown delivery {delivery:?} for {key}")))?;
        let out_of_range = |field: &str, value: i64| Error::new(ErrorKind::InvalidData, format!("orq.db has {field} {value} for {key}, out of range"));
        let max_active_runs = max_active_runs.map(|v| u8::try_from(v).map_err(|_| out_of_range("max_active_runs", v))).transpose()?;
        let min_free_disk_gb = min_free_disk_gb.map(|v| u32::try_from(v).map_err(|_| out_of_range("min_free_disk_gb", v))).transpose()?;
        Ok(StoredProjectSettings { settings: ProjectSettings { implementer, reviewer, delivery, max_active_runs, min_free_disk_gb, queue_hours, queue_stop_on_failure }, revision: revision as u64 })
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
            "INSERT INTO project_settings (project_root, implementer_harness, implementer_model, implementer_effort, reviewer_harness, reviewer_model, reviewer_effort, delivery, revision, updated_at, max_active_runs, min_free_disk_gb, queue_hours, queue_stop_on_failure)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT (project_root) DO UPDATE SET
                 implementer_harness = excluded.implementer_harness, implementer_model = excluded.implementer_model, implementer_effort = excluded.implementer_effort,
                 reviewer_harness = excluded.reviewer_harness, reviewer_model = excluded.reviewer_model, reviewer_effort = excluded.reviewer_effort,
                 delivery = excluded.delivery, revision = excluded.revision, updated_at = excluded.updated_at,
                 max_active_runs = excluded.max_active_runs, min_free_disk_gb = excluded.min_free_disk_gb,
                 queue_hours = excluded.queue_hours, queue_stop_on_failure = excluded.queue_stop_on_failure",
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
                now,
                settings.max_active_runs.map(i64::from),
                settings.min_free_disk_gb.map(i64::from),
                settings.queue_hours,
                settings.queue_stop_on_failure
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

/// Whether the database's -wal file may hold commits the main file lacks: it is non-empty, or
/// its size cannot be read.
fn wal_may_hold_commits(path: &Path) -> bool {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    match std::fs::metadata(&wal) {
        Ok(meta) => meta.len() > 0,
        Err(err) => err.kind() != std::io::ErrorKind::NotFound,
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

fn pins_table_exists(conn: &Connection) -> Result<bool> {
    conn.query_row("SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'run_pins')", [], |row| row.get(0)).map_err(db)
}

/// Whether a `run_pins` table is there with at least one pin in it.
fn has_pins(conn: &Connection) -> Result<bool> {
    Ok(pins_table_exists(conn)? && conn.query_row("SELECT EXISTS (SELECT 1 FROM run_pins)", [], |row| row.get(0)).map_err(db)?)
}

fn drop_pins_table(conn: &Connection) -> Result<()> {
    conn.execute_batch("DROP TABLE IF EXISTS run_pins").map_err(db)
}
