//! The registered passkeys: one private file in Orq's state directory. It holds only public facts
//! (a credential id, a public key, a counter, a label, a time), never a private key or a secret.
//!
//! The store is empty by default, and anything doubtful refuses everyone: a missing file means no
//! passkeys, but a file that cannot be read, is not a plain private file of this user, is too big,
//! or is not exactly the expected shape is an error, and `any_registered` is then false.

use crate::storage::StorageRoots;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "passkeys.json";
const LOCK_NAME: &str = ".passkeys.lock";
const VERSION: u64 = 1;
const MAX_FILE: u64 = 1 << 20;
const MAX_ID: usize = 1024;
const MAX_KEY: usize = 4096;
const MAX_LABEL: usize = 64;
const FIELDS: [&str; 5] = ["credential_id", "public_key", "sign_count", "label", "created_at"];

/// One registered passkey. The id and key are base64url text without padding; `created_at` is
/// seconds since the Unix epoch, given by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Passkey {
    pub credential_id: String,
    pub public_key: String,
    pub sign_count: u32,
    pub label: String,
    pub created_at: i64,
}

pub struct PasskeyStore {
    dir: PathBuf,
}

fn base64url(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn validate(key: &Passkey) -> Result<(), String> {
    if !base64url(&key.credential_id, MAX_ID) {
        return Err("a passkey's credential id must be 1 to 1024 base64url characters".into());
    }
    if !base64url(&key.public_key, MAX_KEY) {
        return Err("a passkey's public key must be 1 to 4096 base64url characters".into());
    }
    let length = key.label.chars().count();
    if length == 0 || length > MAX_LABEL || key.label.chars().any(char::is_control) {
        return Err("a passkey's label must be 1 to 64 characters with no control characters".into());
    }
    Ok(())
}

fn check_unique(keys: &[Passkey]) -> Result<(), String> {
    for (at, key) in keys.iter().enumerate() {
        if keys[..at].iter().any(|other| other.credential_id == key.credential_id) {
            return Err("two passkeys share a credential id".into());
        }
        if keys[..at].iter().any(|other| other.label == key.label) {
            return Err("two passkeys share a label".into());
        }
    }
    Ok(())
}

fn parse(text: &str) -> Result<Vec<Passkey>, String> {
    let doc: Value = serde_json::from_str(text).map_err(|_| "the passkey file is not valid JSON".to_string())?;
    let root = doc.as_object().filter(|root| root.len() == 2).ok_or("the passkey file is not the expected object")?;
    if root.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err("the passkey file has an unknown version".into());
    }
    let list = root.get("passkeys").and_then(Value::as_array).ok_or("the passkey file has no list of passkeys")?;
    let mut keys = Vec::new();
    for entry in list {
        let entry = entry.as_object().filter(|entry| entry.len() == FIELDS.len() && FIELDS.iter().all(|f| entry.contains_key(*f))).ok_or("a passkey entry does not have exactly the expected fields")?;
        let text = |name: &str| entry[name].as_str().map(str::to_string).ok_or_else(|| format!("a passkey's {name} is not text"));
        let key = Passkey {
            credential_id: text("credential_id")?,
            public_key: text("public_key")?,
            sign_count: entry["sign_count"].as_u64().and_then(|n| u32::try_from(n).ok()).ok_or("a passkey's sign_count is not a counter")?,
            label: text("label")?,
            created_at: entry["created_at"].as_i64().ok_or("a passkey's created_at is not a time")?,
        };
        validate(&key)?;
        keys.push(key);
    }
    check_unique(&keys)?;
    Ok(keys)
}

fn render(keys: &[Passkey]) -> String {
    let list: Vec<Value> = keys.iter().map(|k| json!({"credential_id": k.credential_id, "public_key": k.public_key, "sign_count": k.sign_count, "label": k.label, "created_at": k.created_at})).collect();
    serde_json::to_string_pretty(&json!({"version": VERSION, "passkeys": list})).expect("plain values serialise")
}

impl PasskeyStore {
    pub fn path(roots: &StorageRoots) -> PathBuf {
        roots.state.join(FILE_NAME)
    }

    pub fn open(roots: &StorageRoots) -> PasskeyStore {
        PasskeyStore { dir: roots.state.clone() }
    }

    fn file(&self) -> PathBuf {
        self.dir.join(FILE_NAME)
    }

    /// Every registered passkey. No file is none; an unreadable or doubtful file is an error.
    pub fn list(&self) -> Result<Vec<Passkey>, String> {
        let path = self.file();
        let file = match std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("cannot open the passkey file {}: {}", path.display(), e.kind())),
        };
        let meta = file.metadata().map_err(|e| format!("cannot inspect the passkey file: {}", e.kind()))?;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        if !meta.is_file() {
            return Err("the passkey file must be a regular file".into());
        }
        if meta.uid() != me {
            return Err("the passkey file must belong to the user running the server".into());
        }
        if meta.mode() & 0o077 != 0 {
            return Err("the passkey file must be readable by its owner only (chmod 600)".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_FILE + 1).read_to_end(&mut bytes).map_err(|e| format!("cannot read the passkey file: {}", e.kind()))?;
        if bytes.len() as u64 > MAX_FILE {
            return Err(format!("the passkey file is larger than {MAX_FILE} bytes"));
        }
        parse(std::str::from_utf8(&bytes).map_err(|_| "the passkey file is not text".to_string())?)
    }

    /// Whether any passkey is registered. False when there are none and when the file cannot be
    /// trusted, so a doubtful store never lets anyone in.
    pub fn any_registered(&self) -> bool {
        self.list().map(|keys| !keys.is_empty()).unwrap_or(false)
    }

    pub fn find(&self, credential_id: &str) -> Result<Option<Passkey>, String> {
        Ok(self.list()?.into_iter().find(|key| key.credential_id == credential_id))
    }

    /// Registers a passkey. A duplicate credential id or label, a bad value, or a file that cannot
    /// be trusted is refused and nothing changes.
    pub fn add(&self, key: Passkey) -> Result<(), String> {
        validate(&key)?;
        self.change(|keys| {
            keys.push(key);
            check_unique(keys)
        })
    }

    /// Removes the passkey with this label.
    pub fn remove(&self, label: &str) -> Result<(), String> {
        self.change(|keys| {
            let at = keys.iter().position(|key| key.label == label).ok_or("no passkey has that label")?;
            keys.remove(at);
            Ok(())
        })
    }

    /// Reads, changes and writes back under an exclusive lock, so simultaneous changes are all kept.
    fn change(&self, edit: impl FnOnce(&mut Vec<Passkey>) -> Result<(), String>) -> Result<(), String> {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.dir).map_err(|e| format!("cannot create the Orq state directory: {}", e.kind()))?;
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(self.dir.join(LOCK_NAME))
            .map_err(|e| format!("cannot open the passkey lock: {}", e.kind()))?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).map_err(|e| format!("cannot lock the passkey file: {e}"))?;
        let mut keys = self.list()?;
        edit(&mut keys)?;
        self.write(&keys)
    }

    /// Written whole to a private temporary file, then renamed into place: a reader sees the old
    /// file or the new one, never a partial one.
    fn write(&self, keys: &[Passkey]) -> Result<(), String> {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let temp = self.dir.join(format!(".passkeys.{}.{}.tmp", std::process::id(), COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        let _ = std::fs::remove_file(&temp);
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temp)
            .and_then(|mut file| file.write_all(render(keys).as_bytes()).and_then(|()| file.sync_all()))
            .and_then(|()| std::fs::rename(&temp, self.file()));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&temp);
            return Err(format!("cannot write the passkey file {}: {}", Path::new(&self.file()).display(), e.kind()));
        }
        Ok(())
    }
}
