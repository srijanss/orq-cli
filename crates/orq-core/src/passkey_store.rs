//! The registered passkeys: one private file in Orq's state directory. It holds only public facts
//! (a credential id, a public key, a counter, a label, a time), never a private key or a secret.
//!
//! The store is empty by default, and anything doubtful refuses everyone: a missing file means no
//! passkeys, but a file that cannot be read, is not a plain private file of this user, is too big,
//! or is not exactly the expected shape is an error, and `any_registered` is then false.

use crate::storage::StorageRoots;
use serde_json::{json, Value};
use rustix::fs::{AtFlags, Mode, OFlags};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::PathBuf;

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
    let mut ids = HashSet::new();
    let mut labels = HashSet::new();
    for key in keys {
        if !ids.insert(key.credential_id.as_str()) {
            return Err("two passkeys share a credential id".into());
        }
        if !labels.insert(key.label.as_str()) {
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

    /// Opens the state directory itself, never following a link, and only if it is this user's and
    /// cannot be written by anyone else. Every later open, rename and removal is relative to this
    /// descriptor, so the directory cannot be swapped under a running change.
    fn directory(&self, create: bool) -> Result<Option<OwnedFd>, String> {
        if create {
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.dir).map_err(|e| format!("cannot create the Orq state directory: {}", e.kind()))?;
        }
        self.check_ancestors()?;
        let fd = match rustix::fs::open(&self.dir, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty()) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(e) => return Err(format!("cannot open the Orq state directory {} (it must be a real directory, not a link): {e}", self.dir.display())),
        };
        let meta = std::fs::File::from(fd.try_clone().map_err(|e| format!("cannot inspect the state directory: {}", e.kind()))?).metadata().map_err(|e| format!("cannot inspect the state directory: {}", e.kind()))?;
        // SAFETY: geteuid has no preconditions and cannot fail.
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err("the Orq state directory must belong to the user running the server".into());
        }
        if meta.mode() & 0o022 != 0 {
            return Err("the Orq state directory must not be writable by anyone else".into());
        }
        Ok(Some(fd))
    }

    /// Nobody but this user (or root) may be able to rename or replace a directory on the way to the
    /// state directory: each ancestor must belong to this user or root and must not be writable by
    /// everyone, unless it is sticky (like /tmp). Ancestors that do not exist yet are skipped.
    fn check_ancestors(&self) -> Result<(), String> {
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        for ancestor in self.dir.ancestors().skip(1).filter(|a| !a.as_os_str().is_empty()) {
            let meta = match std::fs::metadata(ancestor) {
                Ok(meta) => meta,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("cannot inspect {}: {}", ancestor.display(), e.kind())),
            };
            if meta.uid() != me && meta.uid() != 0 {
                return Err(format!("{} must belong to the user running the server or to root", ancestor.display()));
            }
            if meta.mode() & 0o002 != 0 && meta.mode() & 0o1000 == 0 {
                return Err(format!("{} must not be writable by everyone", ancestor.display()));
            }
        }
        Ok(())
    }

    /// Every registered passkey. No file is none; an unreadable or doubtful file is an error.
    pub fn list(&self) -> Result<Vec<Passkey>, String> {
        match self.directory(false)? {
            None => Ok(Vec::new()),
            Some(dir) => self.read(&dir),
        }
    }

    fn read(&self, dir: &OwnedFd) -> Result<Vec<Passkey>, String> {
        let file = match rustix::fs::openat(dir, FILE_NAME, OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC, Mode::empty()) {
            Ok(fd) => std::fs::File::from(fd),
            Err(rustix::io::Errno::NOENT) => return Ok(Vec::new()),
            Err(e) => return Err(format!("cannot open the passkey file {}: {e}", self.file().display())),
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

    /// Registers a passkey. A duplicate credential id or label, a bad value, a store that would
    /// outgrow the size it can be read back at, or a file that cannot be trusted is refused and
    /// nothing changes.
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
        let dir = self.directory(true)?.ok_or("the Orq state directory is missing")?;
        let lock = rustix::fs::openat(&dir, LOCK_NAME, OFlags::WRONLY | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::from_raw_mode(0o600)).map_err(|e| format!("cannot open the passkey lock: {e}"))?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).map_err(|e| format!("cannot lock the passkey file: {e}"))?;
        let mut keys = self.read(&dir)?;
        edit(&mut keys)?;
        self.write(&dir, &keys)
    }

    /// Written whole to a private temporary file, then renamed into place, then the directory is
    /// synced: a reader sees the old file or the new one, never a partial one, and a finished
    /// change (a removal above all) survives a crash. Anything over the size a read accepts is
    /// refused before a file is made.
    fn write(&self, dir: &OwnedFd, keys: &[Passkey]) -> Result<(), String> {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let text = render(keys);
        if text.len() as u64 > MAX_FILE {
            return Err(format!("the passkey file would be larger than {MAX_FILE} bytes"));
        }
        let temp = format!(".passkeys.{}.{}.tmp", std::process::id(), COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let _ = rustix::fs::unlinkat(dir, temp.as_str(), AtFlags::empty());
        let published = rustix::fs::openat(dir, temp.as_str(), OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::from_raw_mode(0o600))
            .map_err(|e| e.to_string())
            .and_then(|fd| {
                let mut file = std::fs::File::from(fd);
                fault(Stage::Write)?;
                file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
                fault(Stage::FileSync)?;
                file.sync_all().map_err(|e| e.to_string())
            })
            .and_then(|()| rustix::fs::renameat(dir, temp.as_str(), dir, FILE_NAME).map_err(|e| e.to_string()));
        if let Err(e) = published {
            let _ = rustix::fs::unlinkat(dir, temp.as_str(), AtFlags::empty());
            return Err(format!("cannot write the passkey file {}: {e}", self.file().display()));
        }
        // The new file is now the store. If only the directory sync fails, the change is applied but
        // might not survive a crash, and the message says so rather than claiming nothing changed.
        fault(Stage::DirSync).and_then(|()| rustix::fs::fsync(dir).map_err(|e| e.to_string())).map_err(|e| format!("{APPLIED_NOT_DURABLE}: {e}"))
    }
}

/// Starts the error of a change that was applied but could not be confirmed durable.
pub const APPLIED_NOT_DURABLE: &str = "the change was applied but could not be confirmed durable (cannot sync the Orq state directory)";

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Write,
    FileSync,
    DirSync,
}

#[cfg(test)]
thread_local!(static FAULT: std::cell::Cell<Option<Stage>> = const { std::cell::Cell::new(None) });

#[cfg(test)]
fn fault(at: Stage) -> Result<(), String> {
    if FAULT.with(|f| f.get()) == Some(at) { Err("injected failure".into()) } else { Ok(()) }
}

#[cfg(not(test))]
fn fault(_: Stage) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A rename that cannot happen (a directory is in the way) leaves the store as it was and no
    /// temporary file behind.
    #[test]
    fn a_failed_publish_reports_an_error_and_leaves_no_temporary_file() {
        let base = std::env::temp_dir().join(format!("orq-passkeys-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let store = PasskeyStore { dir: base.join("state") };
        let dir = store.directory(true).unwrap().unwrap();
        std::fs::create_dir(store.file()).unwrap();
        let key = Passkey { credential_id: "a".into(), public_key: "b".into(), sign_count: 0, label: "c".into(), created_at: 1 };
        assert!(store.write(&dir, &[key]).is_err());
        let names: Vec<_> = std::fs::read_dir(&store.dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(names, [FILE_NAME], "{names:?}");
    }

    fn store(tag: &str) -> PasskeyStore {
        let base = std::env::temp_dir().join(format!("orq-passkeys-unit-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        PasskeyStore { dir: base.join("state") }
    }

    fn key(id: &str, label: &str) -> Passkey {
        Passkey { credential_id: id.into(), public_key: "b".into(), sign_count: 0, label: label.into(), created_at: 1 }
    }

    fn entries(store: &PasskeyStore) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(&store.dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        names
    }

    /// A failure while writing or syncing the new file leaves the earlier store as it was, no
    /// temporary file, and the lock free for the next change.
    #[test]
    fn a_failed_write_or_file_sync_keeps_the_existing_store_and_releases_the_lock() {
        for stage in [Stage::Write, Stage::FileSync] {
            let store = store("fail");
            store.add(key("a", "one")).unwrap();
            let before = std::fs::read_to_string(store.file()).unwrap();
            FAULT.with(|f| f.set(Some(stage)));
            let failed = store.add(key("b", "two"));
            FAULT.with(|f| f.set(None));
            assert!(failed.is_err());
            assert_eq!(std::fs::read_to_string(store.file()).unwrap(), before);
            assert_eq!(entries(&store), [".passkeys.lock", "passkeys.json"]);
            store.add(key("b", "two")).expect("the lock is free");
            assert_eq!(store.list().unwrap().len(), 2);
        }
    }

    /// When only the directory sync fails the new file is already the store, and the error says the
    /// change was applied rather than that nothing changed.
    #[test]
    fn a_directory_sync_failure_is_reported_as_applied_not_as_unchanged() {
        let store = store("dirsync");
        store.add(key("a", "one")).unwrap();
        FAULT.with(|f| f.set(Some(Stage::DirSync)));
        let result = store.add(key("b", "two"));
        FAULT.with(|f| f.set(None));
        assert!(result.unwrap_err().starts_with(APPLIED_NOT_DURABLE));
        assert_eq!(store.list().unwrap().len(), 2, "the change is in the store");
        assert_eq!(entries(&store), [".passkeys.lock", "passkeys.json"]);
    }

    /// An ancestor that anyone can write to (and is not sticky) could be used to swap the state
    /// directory away, so it is refused; a sticky one like /tmp is fine.
    #[test]
    fn an_ancestor_writable_by_everyone_is_refused() {
        let store = store("ancestor");
        let parent = store.dir.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(store.list().is_err() && store.add(key("a", "one")).is_err() && !store.any_registered());
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o1777)).unwrap();
        store.add(key("a", "one")).expect("a sticky ancestor is trusted");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
    }
}
