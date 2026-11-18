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

/// The most features a project's queue holds: as many as one reorder request may name.
pub const QUEUE_LIMIT: usize = 200;

/// What a queued entry does with a run that already exists, instead of starting a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueKind {
    /// Resume the run, with the answer if one was given.
    Resume,
    /// Resume the run to review its unreviewed commits again and fix what that review finds.
    ReviewAgain,
    /// Accept the run, as the web's Accept does.
    Accept,
}

impl QueueKind {
    pub fn as_str(self) -> &'static str {
        match self {
            QueueKind::Resume => "resume",
            QueueKind::ReviewAgain => "review_again",
            QueueKind::Accept => "accept",
        }
    }

    /// The kind named `name`; a feature's start is not one.
    pub fn parse(name: &str) -> Option<QueueKind> {
        [QueueKind::Resume, QueueKind::ReviewAgain, QueueKind::Accept].into_iter().find(|kind| kind.as_str() == name)
    }
}

/// A run action to queue: what to do with which run, and a resume's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueAction {
    pub kind: QueueKind,
    pub run: String,
    pub answer: Option<String>,
}

impl QueueAction {
    /// The entry's name in the queue, which no feature's can be (they hold no `:`).
    pub fn entry_name(&self) -> String {
        format!("{}:{}", self.kind.as_str(), self.run)
    }
}

/// One queued entry: a feature to start, or a run action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedFeature {
    /// The feature to start, or a run action's entry name (`<kind>:<run>`); entries are removed
    /// and reordered by it.
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
    /// The run action, for an entry that is not a feature's start.
    pub action: Option<QueueAction>,
    /// Why the entry is no longer needed, once it was skipped for that.
    pub skipped: Option<String>,
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
            .prepare("SELECT feature, added_at, added_by, implementer, reviewer, run_id, failed, kind, target_run, answer, skipped FROM run_queue WHERE project_root = ?1 ORDER BY position")
            .map_err(db)?;
        let rows = statement
            .query_map(params![key], |row| {
                let implementer: Option<String> = row.get(3)?;
                let reviewer: Option<String> = row.get(4)?;
                let kind: String = row.get(7)?;
                let target: Option<String> = row.get(8)?;
                let answer: Option<String> = row.get(9)?;
                let action = QueueKind::parse(&kind).zip(target).map(|(kind, run)| QueueAction { kind, run, answer });
                Ok(QueuedFeature {
                    feature: row.get(0)?,
                    added_at: row.get(1)?,
                    added_by: row.get(2)?,
                    profiles: implementer.zip(reviewer).map(|(implementer, reviewer)| QueueProfiles { implementer, reviewer }),
                    run: row.get(5)?,
                    failed: row.get(6)?,
                    action,
                    skipped: row.get(10)?,
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
    /// Adds `additions` in order, skipping a feature queued already; refused with `InvalidInput`
    /// when the queue would hold more than [`QUEUE_LIMIT`] features.
    pub fn queue_add(&self, root: &Path, additions: &[QueueAddition], added_by: &str, now: i64) -> Result<Vec<String>> {
        let key = project_key(root)?;
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate).map_err(db)?;
        let mut position: i64 = tx
            .query_row("SELECT COALESCE(MAX(position), 0) FROM run_queue WHERE project_root = ?1", params![key], |row| row.get(0))
            .map_err(db)?;
        // Checked in this write, so two adders at once cannot both pass the limit.
        let queued: Vec<String> = {
            let mut statement = tx.prepare("SELECT feature FROM run_queue WHERE project_root = ?1").map_err(db)?;
            let rows = statement.query_map(params![key], |row| row.get(0)).map_err(db)?;
            rows.collect::<std::result::Result<Vec<_>, _>>().map_err(db)?
        };
        let new: std::collections::BTreeSet<&str> =
            additions.iter().map(|addition| addition.feature.as_str()).filter(|feature| !queued.iter().any(|queued| queued == feature)).collect();
        if queued.len() + new.len() > QUEUE_LIMIT {
            return Err(Error::new(ErrorKind::InvalidInput, format!("the queue holds at most {QUEUE_LIMIT} features; {} are queued", queued.len())));
        }
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

    /// Adds run `action` to the end of the queue and returns its entry name; `None` when the same
    /// action on the same run is queued already. Refused with `InvalidInput` when the queue is full.
    pub fn queue_add_action(&self, root: &Path, action: &QueueAction, added_by: &str, now: i64) -> Result<Option<String>> {
        let key = project_key(root)?;
        let entry = action.entry_name();
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate).map_err(db)?;
        let (count, position, queued): (i64, i64, bool) = tx
            .query_row(
                "SELECT COUNT(*), COALESCE(MAX(position), 0), COALESCE(MAX(feature = ?2), 0) FROM run_queue WHERE project_root = ?1",
                params![key, entry],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(db)?;
        if queued {
            return Ok(None);
        }
        if count as usize >= QUEUE_LIMIT {
            return Err(Error::new(ErrorKind::InvalidInput, format!("the queue holds at most {QUEUE_LIMIT} features; {count} are queued")));
        }
        tx.execute(
            "INSERT INTO run_queue (project_root, feature, position, added_at, added_by, kind, target_run, answer) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![key, entry, position + 1, now, added_by, action.kind.as_str(), action.run, action.answer],
        )
        .map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(Some(entry))
    }

    /// Marks the entry `feature` as no longer needed, for `reason`; it stays in the queue, skipped.
    pub fn queue_skip(&self, root: &Path, feature: &str, reason: &str) -> Result<()> {
        let key = project_key(root)?;
        self.conn
            .execute("UPDATE run_queue SET skipped = ?3 WHERE project_root = ?1 AND feature = ?2", params![key, feature, reason])
            .map(drop)
            .map_err(db)
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
