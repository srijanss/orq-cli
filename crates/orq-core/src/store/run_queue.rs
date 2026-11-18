//! A project's run queue in orq.db: the features waiting to be started one by one by the server, in
//! the order they were added until someone reorders them, each with the run it started and whether
//! that run failed, and whether the queue is paused and why. Keyed by the project's main checkout,
//! as its settings are.

use std::io::{Error, ErrorKind, Result};
use std::path::Path;

use rusqlite::{params, OptionalExtension, TransactionBehavior};

use super::{db, project_key, Store};

/// The implementer and reviewer profiles an entry starts with, as the launch picker names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueProfiles {
    pub implementer: String,
    pub reviewer: String,
}

/// A feature to add to a queue, with the profiles to start it with (the project's own when none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueAddition {
    pub feature: String,
    pub profiles: Option<QueueProfiles>,
}

/// One queued feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedFeature {
    pub feature: String,
    /// When it was added, in seconds since the epoch.
    pub added_at: i64,
    /// Where it was added from: `web` or `cli`.
    pub added_by: String,
    pub profiles: Option<QueueProfiles>,
    /// The run the queue started for it, once it did.
    pub run: Option<String>,
    /// Whether that run failed or was blocked.
    pub failed: bool,
}

/// A project's queue: its entries in order, and the reason it is paused, if it is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunQueue {
    pub entries: Vec<QueuedFeature>,
    pub paused: Option<String>,
}

impl Store {
    /// The queue of the project whose main checkout is `root`; empty and running when it has none.
    pub fn queue(&self, root: &Path) -> Result<RunQueue> {
        let key = project_key(root)?;
        let mut statement = self
            .conn
            .prepare("SELECT feature, added_at, added_by, implementer, reviewer, run_id, failed FROM run_queue WHERE project_root = ?1 ORDER BY position")
            .map_err(db)?;
        let rows = statement
            .query_map(params![key], |row| {
                let implementer: Option<String> = row.get(3)?;
                let reviewer: Option<String> = row.get(4)?;
                Ok(QueuedFeature {
                    feature: row.get(0)?,
                    added_at: row.get(1)?,
                    added_by: row.get(2)?,
                    profiles: implementer.zip(reviewer).map(|(implementer, reviewer)| QueueProfiles { implementer, reviewer }),
                    run: row.get(5)?,
                    failed: row.get(6)?,
                })
            })
            .map_err(db)?;
        let entries = rows.collect::<std::result::Result<Vec<_>, _>>().map_err(db)?;
        let paused = self
            .conn
            .query_row("SELECT reason FROM run_queue_paused WHERE project_root = ?1", params![key], |row| row.get(0))
            .optional()
            .map_err(db)?;
        Ok(RunQueue { entries, paused })
    }

    /// The projects with a queue, entries or a pause, by their main checkout's path.
    pub fn queued_projects(&self) -> Result<Vec<String>> {
        let mut statement = self
            .conn
            .prepare("SELECT project_root FROM run_queue UNION SELECT project_root FROM run_queue_paused ORDER BY project_root")
            .map_err(db)?;
        let rows = statement.query_map([], |row| row.get(0)).map_err(db)?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(db)
    }

    /// Adds each feature not queued yet to the end of the queue, in the order given, and returns the
    /// ones it added.
    pub fn queue_add(&self, root: &Path, additions: &[QueueAddition], added_by: &str, now: i64) -> Result<Vec<String>> {
        let key = project_key(root)?;
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate).map_err(db)?;
        let mut position: i64 = tx
            .query_row("SELECT COALESCE(MAX(position), 0) FROM run_queue WHERE project_root = ?1", params![key], |row| row.get(0))
            .map_err(db)?;
        let mut added = Vec::new();
        for addition in additions {
            let (implementer, reviewer) = match &addition.profiles {
                Some(p) => (Some(p.implementer.as_str()), Some(p.reviewer.as_str())),
                None => (None, None),
            };
            position += 1;
            let inserted = tx
                .execute(
                    "INSERT OR IGNORE INTO run_queue (project_root, feature, position, added_at, added_by, implementer, reviewer) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![key, addition.feature, position, now, added_by, implementer, reviewer],
                )
                .map_err(db)?;
            if inserted == 1 {
                added.push(addition.feature.clone());
            }
        }
        tx.commit().map_err(db)?;
        Ok(added)
    }

    /// Removes `feature` from the queue; `false` when it was not queued.
    pub fn queue_remove(&self, root: &Path, feature: &str) -> Result<bool> {
        let key = project_key(root)?;
        let removed = self.conn.execute("DELETE FROM run_queue WHERE project_root = ?1 AND feature = ?2", params![key, feature]).map_err(db)?;
        Ok(removed == 1)
    }

    /// Puts the queue in `order`, which must name every entry exactly once.
    pub fn queue_reorder(&self, root: &Path, order: &[String]) -> Result<()> {
        let key = project_key(root)?;
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate).map_err(db)?;
        let mut current: Vec<String> = {
            let mut statement = tx.prepare("SELECT feature FROM run_queue WHERE project_root = ?1").map_err(db)?;
            let rows = statement.query_map(params![key], |row| row.get(0)).map_err(db)?;
            rows.collect::<std::result::Result<Vec<_>, _>>().map_err(db)?
        };
        let mut given = order.to_vec();
        current.sort();
        given.sort();
        if current != given {
            return Err(Error::new(ErrorKind::InvalidInput, "the new order must name every queued feature exactly once"));
        }
        for (index, feature) in order.iter().enumerate() {
            tx.execute("UPDATE run_queue SET position = ?3 WHERE project_root = ?1 AND feature = ?2", params![key, feature, index as i64 + 1])
                .map_err(db)?;
        }
        tx.commit().map_err(db)
    }

    /// Records the run the queue started for `feature`, clearing an earlier failure.
    pub fn queue_started(&self, root: &Path, feature: &str, run: &str) -> Result<()> {
        let key = project_key(root)?;
        self.conn
            .execute("UPDATE run_queue SET run_id = ?3, failed = 0 WHERE project_root = ?1 AND feature = ?2", params![key, feature, run])
            .map(drop)
            .map_err(db)
    }

    /// Marks `feature`'s run as failed; the entry stays in the queue.
    pub fn queue_failed(&self, root: &Path, feature: &str) -> Result<()> {
        let key = project_key(root)?;
        self.conn.execute("UPDATE run_queue SET failed = 1 WHERE project_root = ?1 AND feature = ?2", params![key, feature]).map(drop).map_err(db)
    }

    /// Pauses the queue with `reason`, replacing an earlier one.
    pub fn queue_pause(&self, root: &Path, reason: &str) -> Result<()> {
        let key = project_key(root)?;
        self.conn
            .execute("INSERT OR REPLACE INTO run_queue_paused (project_root, reason) VALUES (?1, ?2)", params![key, reason])
            .map(drop)
            .map_err(db)
    }

    pub fn queue_resume(&self, root: &Path) -> Result<()> {
        let key = project_key(root)?;
        self.conn.execute("DELETE FROM run_queue_paused WHERE project_root = ?1", params![key]).map(drop).map_err(db)
    }
}
