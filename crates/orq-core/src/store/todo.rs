//! The Todo list in orq.db: ideas kept outside any project until somebody moves one into a project as a
//! draft feature. Each item has a name, a description and a version that every edit raises, so an edit
//! made from a stale read is refused instead of overwriting somebody else's.

use std::io::Result;
use std::path::Path;

use rusqlite::{params, OptionalExtension, TransactionBehavior};

use super::{db, project_key, Store};

/// Longest name, in characters.
pub const MAX_NAME_CHARS: usize = 200;
/// Longest description, in characters.
pub const MAX_DESCRIPTION_CHARS: usize = 20_000;
/// Most items the list holds.
pub const MAX_ITEMS: usize = 500;

/// One item on the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItem {
    pub id: i64,
    pub name: String,
    pub description: String,
    /// When it was added and last changed, in seconds since the epoch.
    pub created_at: i64,
    pub updated_at: i64,
    pub version: i64,
}

/// How an add or an edit went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoChange {
    Done(TodoItem),
    /// A value over a limit, or the list full, said in words.
    Invalid(String),
    /// The item changed since the version the edit read.
    Conflict,
    Unknown,
}

/// Why a name or description cannot be kept, if one cannot.
pub fn refusal(name: &str, description: &str) -> Option<String> {
    let length = name.trim().chars().count();
    if length == 0 {
        return Some("the name is empty".to_string());
    }
    if length > MAX_NAME_CHARS {
        return Some(format!("the name is longer than {MAX_NAME_CHARS} characters"));
    }
    if description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Some(format!("the description is longer than {MAX_DESCRIPTION_CHARS} characters"));
    }
    None
}

fn item(row: &rusqlite::Row) -> rusqlite::Result<TodoItem> {
    Ok(TodoItem { id: row.get(0)?, name: row.get(1)?, description: row.get(2)?, created_at: row.get(3)?, updated_at: row.get(4)?, version: row.get(5)? })
}

const COLUMNS: &str = "id, name, description, created_at, updated_at, version";

impl Store {
    /// Every item, newest first.
    pub fn todos(&self) -> Result<Vec<TodoItem>> {
        let mut statement = self.conn.prepare(&format!("SELECT {COLUMNS} FROM todos ORDER BY created_at DESC, id DESC")).map_err(db)?;
        let rows = statement.query_map([], item).map_err(db)?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(db)
    }

    /// The item `id`, if there is one.
    pub fn todo(&self, id: i64) -> Result<Option<TodoItem>> {
        self.conn.query_row(&format!("SELECT {COLUMNS} FROM todos WHERE id = ?1"), params![id], item).optional().map_err(db)
    }

    /// Adds an item at `now`, its name trimmed; refused when a value is over its limit or the list is full.
    pub fn todo_add(&self, name: &str, description: &str, now: i64) -> Result<TodoChange> {
        if let Some(why) = refusal(name, description) {
            return Ok(TodoChange::Invalid(why));
        }
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate).map_err(db)?;
        // Counted in this write, so two adders at once cannot both pass the limit.
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM todos", [], |row| row.get(0)).map_err(db)?;
        if count as usize >= MAX_ITEMS {
            return Ok(TodoChange::Invalid(format!("the Todo list holds at most {MAX_ITEMS} items")));
        }
        tx.execute("INSERT INTO todos (name, description, created_at, updated_at, version) VALUES (?1, ?2, ?3, ?3, 1)", params![name.trim(), description, now])
            .map_err(db)?;
        let id = tx.last_insert_rowid();
        let added = tx.query_row(&format!("SELECT {COLUMNS} FROM todos WHERE id = ?1"), params![id], item).map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(TodoChange::Done(added))
    }

    /// Changes item `id` to `name` and `description` at `now`, if it still has `version`.
    pub fn todo_edit(&self, id: i64, name: &str, description: &str, version: i64, now: i64) -> Result<TodoChange> {
        if let Some(why) = refusal(name, description) {
            return Ok(TodoChange::Invalid(why));
        }
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate).map_err(db)?;
        let current: Option<i64> = tx.query_row("SELECT version FROM todos WHERE id = ?1", params![id], |row| row.get(0)).optional().map_err(db)?;
        match current {
            None => return Ok(TodoChange::Unknown),
            Some(current) if current != version => return Ok(TodoChange::Conflict),
            Some(_) => {}
        }
        tx.execute(
            "UPDATE todos SET name = ?2, description = ?3, updated_at = ?4, version = version + 1 WHERE id = ?1",
            params![id, name.trim(), description, now],
        )
        .map_err(db)?;
        let edited = tx.query_row(&format!("SELECT {COLUMNS} FROM todos WHERE id = ?1"), params![id], item).map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(TodoChange::Done(edited))
    }

    /// Deletes item `id`; `false` when there was none.
    pub fn todo_delete(&self, id: i64) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM todos WHERE id = ?1", params![id]).map_err(db)? == 1)
    }

    /// Asks for draft `feature` of the project whose main checkout is `root` to be queued once it is approved.
    pub fn queue_wish_set(&self, root: &Path, feature: &str, now: i64) -> Result<()> {
        let key = project_key(root)?;
        self.conn
            .execute("INSERT OR REPLACE INTO queue_wishes (project_root, feature, requested_at) VALUES (?1, ?2, ?3)", params![key, feature, now])
            .map(drop)
            .map_err(db)
    }

    /// Whether draft `feature` is to be queued once it is approved.
    pub fn queue_wish(&self, root: &Path, feature: &str) -> Result<bool> {
        let key = project_key(root)?;
        self.conn
            .query_row("SELECT EXISTS (SELECT 1 FROM queue_wishes WHERE project_root = ?1 AND feature = ?2)", params![key, feature], |row| row.get(0))
            .map_err(db)
    }

    /// Drops the wish for `feature`; `false` when there was none.
    pub fn queue_wish_drop(&self, root: &Path, feature: &str) -> Result<bool> {
        let key = project_key(root)?;
        Ok(self.conn.execute("DELETE FROM queue_wishes WHERE project_root = ?1 AND feature = ?2", params![key, feature]).map_err(db)? == 1)
    }
}
