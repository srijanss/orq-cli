//! The file-based run store: one directory per run under the Orq state dir,
//! `runs/<run-id>/run.json`, holding the run's immutable inputs. It lives
//! outside the project tree. SQLite replaces it in a later milestone.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The model profile resolved for one role at run start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleProfile {
    pub harness: String,
    pub model: String,
    pub thinking: Option<String>,
}

/// The inputs a run is created with. Never rewritten afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    pub project_id: String,
    pub feature_id: String,
    pub remote: String,
    pub provider: String,
    pub target_branch: String,
    pub base_sha: String,
    pub branch: String,
    pub worktree: PathBuf,
    pub profiles: BTreeMap<String, RoleProfile>,
}

/// One line of `events.jsonl`: the spec's event envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub schema_version: u32,
    pub event_id: String,
    pub run_id: String,
    pub project_id: String,
    pub feature_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub stage: Option<String>,
    pub attempt: u32,
    pub occurred_at: String,
    pub payload: serde_json::Value,
}

#[derive(Debug)]
pub enum StoreError {
    /// The run id is empty, hidden or contains anything but `[A-Za-z0-9._-]`.
    InvalidRunId(String),
    AlreadyExists(String),
    /// Another run of the same project and feature is still active.
    FeatureBusy { active_run_id: String },
    NotFound(String),
    Io(std::io::Error),
    Corrupt(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::InvalidRunId(id) => write!(f, "invalid run id {id:?}"),
            StoreError::AlreadyExists(id) => write!(f, "run {id} already exists"),
            StoreError::FeatureBusy { active_run_id } => {
                write!(f, "the feature already has an active run: {active_run_id}")
            }
            StoreError::NotFound(id) => write!(f, "run {id} not found"),
            StoreError::Io(err) => write!(f, "run store I/O error: {err}"),
            StoreError::Corrupt(why) => write!(f, "run record is corrupt: {why}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(err: std::io::Error) -> Self {
        StoreError::Io(err)
    }
}

pub struct RunStore {
    root: PathBuf,
}

impl RunStore {
    pub fn new(root: &Path) -> Self {
        RunStore { root: root.to_path_buf() }
    }

    fn run_dir(&self, run_id: &str) -> Result<PathBuf, StoreError> {
        let valid = !run_id.is_empty()
            && !run_id.starts_with('.')
            && run_id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !valid {
            return Err(StoreError::InvalidRunId(run_id.to_string()));
        }
        Ok(self.root.join("runs").join(run_id))
    }

    /// Create a run and take its project+feature lock.
    pub fn start_run(&self, record: &RunRecord) -> Result<(), StoreError> {
        let lock = self.lock_path(&record.project_id, &record.feature_id)?;
        self.run_dir(&record.run_id)?;
        let _guard = self.guard(&record.project_id, &record.feature_id)?;
        let locks = lock.parent().expect("lock path has a parent");
        fs::create_dir_all(locks)?;
        // The lock appears already holding the run id: write a private file,
        // then hard-link it into place, which fails if a lock exists.
        let private = locks.join(format!(".{}.{}.tmp", record.run_id, std::process::id()));
        fs::write(&private, record.run_id.as_bytes())?;
        let linked = fs::hard_link(&private, &lock);
        let _ = fs::remove_file(&private);
        match linked {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                let active_run_id = fs::read_to_string(&lock).unwrap_or_default().trim().to_string();
                return Err(StoreError::FeatureBusy { active_run_id });
            }
            Err(err) => return Err(err.into()),
        }
        if let Err(err) = self.create(record) {
            let _ = fs::remove_file(&lock);
            return Err(err);
        }
        Ok(())
    }

    /// Release the feature lock when the run ends; only its owner may.
    pub fn finish_run(&self, project_id: &str, feature_id: &str, run_id: &str) -> Result<(), StoreError> {
        let lock = self.lock_path(project_id, feature_id)?;
        // Held across the ownership check and the removal, and by every start,
        // so a duplicate release cannot remove a newer owner's lock.
        let _guard = self.guard(project_id, feature_id)?;
        let owner = match fs::read_to_string(&lock) {
            Ok(text) => text.trim().to_string(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::NotFound(run_id.to_string()))
            }
            Err(err) => return Err(err.into()),
        };
        if owner != run_id {
            return Err(StoreError::FeatureBusy { active_run_id: owner });
        }
        fs::remove_file(&lock)?;
        Ok(())
    }

    /// An exclusive advisory lock on a stable per-feature file, released when
    /// the returned handle is dropped. The file itself is never removed.
    fn guard(&self, project_id: &str, feature_id: &str) -> Result<fs::File, StoreError> {
        let path = self.lock_path(project_id, feature_id)?;
        let dir = self.root.join("guards").join(project_id);
        fs::create_dir_all(&dir)?;
        let name = path.file_stem().expect("lock path has a name").to_owned();
        let file = fs::OpenOptions::new().create(true).write(true).truncate(false).open(dir.join(name))?;
        file.lock()?;
        Ok(file)
    }

    fn lock_path(&self, project_id: &str, feature_id: &str) -> Result<PathBuf, StoreError> {
        for part in [project_id, feature_id] {
            let valid = !part.is_empty()
                && !part.starts_with('.')
                && part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
            if !valid {
                return Err(StoreError::InvalidRunId(part.to_string()));
            }
        }
        Ok(self.root.join("locks").join(project_id).join(format!("{feature_id}.lock")))
    }

    /// Write `run.json` for a new run, atomically; refuse to replace one.
    pub fn create(&self, record: &RunRecord) -> Result<(), StoreError> {
        let dir = self.run_dir(&record.run_id)?;
        // Serialize first: a record that cannot be written must not claim the id.
        let json = serde_json::to_vec_pretty(record).map_err(|err| StoreError::Corrupt(err.to_string()))?;
        fs::create_dir_all(dir.parent().expect("run dir has a parent"))?;
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(StoreError::AlreadyExists(record.run_id.clone()))
            }
            Err(err) => return Err(err.into()),
        }
        let temp = dir.join("run.json.tmp");
        let written = (|| -> std::io::Result<()> {
            let mut file = fs::File::create(&temp)?;
            file.write_all(&json)?;
            file.sync_all()?;
            fs::rename(&temp, dir.join("run.json"))
        })();
        if let Err(err) = written {
            let _ = fs::remove_dir_all(&dir);
            return Err(err.into());
        }
        Ok(())
    }

    /// Append one event to the run's `events.jsonl` as a single line. The
    /// run must already exist.
    pub fn append_event(&self, event: &Event) -> Result<(), StoreError> {
        let dir = self.run_dir(&event.run_id)?;
        if !dir.join("run.json").is_file() {
            return Err(StoreError::NotFound(event.run_id.clone()));
        }
        let mut line = serde_json::to_vec(event).map_err(|err| StoreError::Corrupt(err.to_string()))?;
        line.push(b'\n');
        let mut file = fs::OpenOptions::new().create(true).append(true).open(dir.join("events.jsonl"))?;
        // One write call so concurrent appenders cannot interleave a line.
        file.write_all(&line)?;
        file.sync_data()?;
        Ok(())
    }

    /// Every event of the run, in the order written. A line that is not an
    /// event is an error, never silently skipped.
    pub fn events(&self, run_id: &str) -> Result<Vec<Event>, StoreError> {
        let dir = self.run_dir(run_id)?;
        if !dir.join("run.json").is_file() {
            return Err(StoreError::NotFound(run_id.to_string()));
        }
        let text = match fs::read_to_string(dir.join("events.jsonl")) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
        };
        text.lines()
            .enumerate()
            .map(|(index, line)| {
                serde_json::from_str(line).map_err(|err| StoreError::Corrupt(format!("events.jsonl line {}: {err}", index + 1)))
            })
            .collect()
    }

    pub fn load(&self, run_id: &str) -> Result<RunRecord, StoreError> {
        let path = self.run_dir(run_id)?.join("run.json");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::NotFound(run_id.to_string()))
            }
            Err(err) => return Err(err.into()),
        };
        serde_json::from_str(&text).map_err(|err| StoreError::Corrupt(err.to_string()))
    }
}
