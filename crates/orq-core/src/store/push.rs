//! What web push keeps in orq.db: the subscribed devices, the notification kinds switched off, the
//! projects muted, and when each (run, kind) was last notified.

use std::io::Result;

use rusqlite::params;

use super::{db, Store};

/// A device's subscription as the browser gives it, with the label shown for it in Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSubscription {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub label: String,
}

/// A stored subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushSubscription {
    pub id: i64,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub label: String,
    pub created_at: i64,
}

impl Store {
    /// Stores a device's subscription; one from an endpoint already stored refreshes that device's keys and label
    /// and keeps its id and created time. Returns the device's id.
    pub fn add_push_subscription(&self, new: &NewSubscription, now: i64) -> Result<i64> {
        self.conn
            .query_row(
                "INSERT INTO push_subscriptions (endpoint, p256dh, auth, label, created_at) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (endpoint) DO UPDATE SET p256dh = excluded.p256dh, auth = excluded.auth, label = excluded.label
                 RETURNING id",
                params![new.endpoint, new.p256dh, new.auth, new.label, now],
                |row| row.get(0),
            )
            .map_err(db)
    }

    /// Every subscribed device, oldest first.
    pub fn push_subscriptions(&self) -> Result<Vec<PushSubscription>> {
        let mut statement = self.conn.prepare("SELECT id, endpoint, p256dh, auth, label, created_at FROM push_subscriptions ORDER BY id").map_err(db)?;
        let rows = statement
            .query_map([], |row| Ok(PushSubscription { id: row.get(0)?, endpoint: row.get(1)?, p256dh: row.get(2)?, auth: row.get(3)?, label: row.get(4)?, created_at: row.get(5)? }))
            .map_err(db)?;
        rows.collect::<rusqlite::Result<_>>().map_err(db)
    }

    /// Forgets a device; says whether it was there.
    pub fn remove_push_subscription(&self, id: i64) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM push_subscriptions WHERE id = ?1", params![id]).map_err(db)? > 0)
    }

    /// Switches a notification kind on or off for every device.
    pub fn set_push_kind(&self, kind: &str, enabled: bool) -> Result<()> {
        self.conn
            .execute("INSERT INTO push_kinds (kind, enabled) VALUES (?1, ?2) ON CONFLICT (kind) DO UPDATE SET enabled = excluded.enabled", params![kind, enabled])
            .map_err(db)?;
        Ok(())
    }

    /// Each kind that was switched on or off, with which; a kind never chosen has its default.
    pub fn push_kind_choices(&self) -> Result<Vec<(String, bool)>> {
        let mut statement = self.conn.prepare("SELECT kind, enabled FROM push_kinds ORDER BY kind").map_err(db)?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?))).map_err(db)?;
        rows.collect::<rusqlite::Result<_>>().map_err(db)
    }

    /// Mutes or unmutes a project (by its id).
    pub fn set_push_project_muted(&self, project_id: &str, muted: bool) -> Result<()> {
        if muted {
            self.conn.execute("INSERT OR IGNORE INTO push_projects_muted (project_id) VALUES (?1)", params![project_id]).map_err(db)?;
        } else {
            self.conn.execute("DELETE FROM push_projects_muted WHERE project_id = ?1", params![project_id]).map_err(db)?;
        }
        Ok(())
    }

    /// The projects muted.
    pub fn push_muted_projects(&self) -> Result<Vec<String>> {
        self.column("SELECT project_id FROM push_projects_muted ORDER BY project_id")
    }

    /// Takes the right to notify `kind` for `run` at `now` (seconds): true unless one was taken less than `window` ago.
    pub fn claim_push(&self, run_id: &str, kind: &str, now: i64, window: i64) -> Result<bool> {
        // One statement, so two processes claiming at once cannot both win.
        let taken = self
            .conn
            .execute(
                "INSERT INTO push_sent (run_id, kind, sent_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (run_id, kind) DO UPDATE SET sent_at = excluded.sent_at WHERE excluded.sent_at - push_sent.sent_at >= ?4",
                params![run_id, kind, now, window],
            )
            .map_err(db)?;
        Ok(taken > 0)
    }

    fn column(&self, sql: &str) -> Result<Vec<String>> {
        let mut statement = self.conn.prepare(sql).map_err(db)?;
        let rows = statement.query_map([], |row| row.get(0)).map_err(db)?;
        rows.collect::<rusqlite::Result<_>>().map_err(db)
    }
}
