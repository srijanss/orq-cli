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
    /// state directory. Every directory the path passes through, including each one a link leads
    /// into along the way, must belong to this user or root and must not be writable by group or
    /// others, unless it is sticky (like /tmp).
    fn check_ancestors(&self) -> Result<(), String> {
        let here = if self.dir.is_absolute() { PathBuf::new() } else { std::env::current_dir().map_err(|e| format!("cannot find the current directory: {}", e.kind()))? };
        self.check_ancestors_from(&here)
    }

    /// `check_ancestors` with a relative state path taken to start in `here`.
    fn check_ancestors_from(&self, here: &std::path::Path) -> Result<(), String> {
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        let absolute = here.join(&self.dir);
        for directory in directories_passed(absolute.parent().unwrap_or(std::path::Path::new("/")))? {
            let meta = std::fs::metadata(&directory).map_err(|e| format!("cannot inspect {}: {}", directory.display(), e.kind()))?;
            if meta.uid() != me && meta.uid() != 0 {
                return Err(format!("{} must belong to the user running the server or to root", directory.display()));
            }
            if meta.mode() & 0o022 != 0 && meta.mode() & 0o1000 == 0 {
                return Err(format!("{} must not be writable by group or others", directory.display()));
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

    /// Registers a passkey only if none is registered, in the same locked step that stores it: of
    /// several token-authorized enrolments that race, exactly one lands. Anything else is refused.
    pub fn add_first(&self, key: Passkey) -> Result<(), String> {
        validate(&key)?;
        self.change(|keys| {
            if !keys.is_empty() {
                return Err("a passkey is already registered".into());
            }
            keys.push(key);
            Ok(())
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
                // An injected write failure happens after half the bytes are written.
                let injected = fault(Stage::Write);
                let end = if injected.is_err() { text.len() / 2 } else { text.len() };
                file.write_all(&text.as_bytes()[..end]).map_err(|e| e.to_string())?;
                injected?;
                fault(Stage::FileSync)?;
                file.sync_all().map_err(|e| e.to_string())
            })
            .and_then(|()| fault(Stage::Rename).and_then(|()| rustix::fs::renameat(dir, temp.as_str(), dir, FILE_NAME).map_err(|e| e.to_string())));
        if let Err(e) = published {
            let _ = rustix::fs::unlinkat(dir, temp.as_str(), AtFlags::empty());
            return Err(format!("cannot write the passkey file {}: {e}", self.file().display()));
        }
        // The new file is now the store. If only the directory sync fails, the change is applied but
        // might not survive a crash, and the message says so rather than claiming nothing changed.
        fault(Stage::DirSync).and_then(|()| rustix::fs::fsync(dir).map_err(|e| e.to_string())).map_err(|e| format!("{APPLIED_NOT_DURABLE}: {e}"))
    }
}

/// Every real directory visited while resolving `path` (absolute) component by component: the
/// directory holding each link and each directory a link leads through are included, not only the
/// start and the end. It stops at the first component that does not exist yet.
fn directories_passed(path: &std::path::Path) -> Result<Vec<PathBuf>, String> {
    use std::ffi::OsString;
    use std::path::Component;
    const MAX_LINKS: u32 = 40;
    let words = |p: &std::path::Path| -> Vec<OsString> { p.components().rev().filter_map(|c| match c { Component::Normal(n) => Some(n.to_os_string()), Component::ParentDir => Some("..".into()), _ => None }).collect() };
    let mut pending = words(path);
    let mut current = PathBuf::from("/");
    let mut seen = vec![current.clone()];
    let mut links = 0;
    while let Some(word) = pending.pop() {
        if word == ".." {
            current.pop();
            continue;
        }
        let next = current.join(&word);
        let meta = match std::fs::symlink_metadata(&next) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(format!("cannot inspect {}: {}", next.display(), e.kind())),
        };
        if meta.file_type().is_symlink() {
            links += 1;
            if links > MAX_LINKS {
                return Err(format!("too many links on the way to {}", path.display()));
            }
            let target = std::fs::read_link(&next).map_err(|e| format!("cannot read the link {}: {}", next.display(), e.kind()))?;
            if target.is_absolute() {
                current = PathBuf::from("/");
            }
            pending.extend(words(&target));
        } else if meta.is_dir() {
            current = next;
            if !seen.contains(&current) {
                seen.push(current.clone());
            }
        } else {
            return Err(format!("{} is not a directory", next.display()));
        }
    }
    Ok(seen)
}

/// Starts the error of a change that was applied but could not be confirmed durable.
pub const APPLIED_NOT_DURABLE: &str = "the change was applied but could not be confirmed durable (cannot sync the Orq state directory)";

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Write,
    FileSync,
    Rename,
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

    /// A failure while writing (half the bytes written), syncing or renaming the new file over a valid store leaves the earlier store as it was, no
    /// temporary file, and the lock free for the next change.
    #[test]
    fn a_failed_write_or_file_sync_keeps_the_existing_store_and_releases_the_lock() {
        for stage in [Stage::Write, Stage::FileSync, Stage::Rename] {
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
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o770)).unwrap();
        assert!(store.list().is_err() && store.add(key("a", "one")).is_err(), "group-writable");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o1777)).unwrap();
        store.add(key("a", "one")).expect("a sticky ancestor is trusted");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
    }

    /// A link on the way is followed and the directories it leads through are checked too.
    #[test]
    fn a_link_ancestor_is_resolved_and_its_real_parents_are_checked() {
        let base = std::env::temp_dir().join(format!("orq-passkeys-unit-{}-link", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(base.join("safe")).unwrap();
        let shared = base.join("shared");
        std::fs::DirBuilder::new().mode(0o777).create(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::fs::DirBuilder::new().mode(0o700).create(shared.join("owned")).unwrap();
        std::os::unix::fs::symlink(shared.join("owned"), base.join("safe/link")).unwrap();
        let store = PasskeyStore { dir: base.join("safe/link/state") };
        assert!(store.add(key("a", "one")).is_err() && store.list().is_err() && !store.any_registered());
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o700)).unwrap();
        store.add(key("a", "one")).expect("a link into a private tree is fine");
    }

    /// The directory holding the link is checked as well as the directories the link leads through.
    #[test]
    fn the_directory_holding_a_link_is_checked_too() {
        let base = std::env::temp_dir().join(format!("orq-passkeys-unit-{}-holder", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(base.join("private/owned")).unwrap();
        let holder = base.join("holder");
        std::fs::DirBuilder::new().mode(0o700).create(&holder).unwrap();
        std::os::unix::fs::symlink(base.join("private/owned"), holder.join("link")).unwrap();
        std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o777)).unwrap();
        let store = PasskeyStore { dir: holder.join("link/state") };
        assert!(store.add(key("a", "one")).is_err() && !store.any_registered());
        std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o700)).unwrap();
        store.add(key("a", "one")).expect("private holder and target");
    }

    /// A relative state path is checked from the directory it is relative to, up.
    #[test]
    fn a_relative_state_path_is_checked_from_its_starting_directory() {
        let base = std::env::temp_dir().join(format!("orq-passkeys-unit-{}-relative", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&base).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o777)).unwrap();
        let store = PasskeyStore { dir: "state".into() };
        assert!(store.check_ancestors_from(&base).is_err(), "a world-writable starting directory is refused");
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        store.check_ancestors_from(&base).expect("a private starting directory is fine");
    }

    /// A chain of links is followed all the way: a writable directory that only an intermediate link
    /// passes through is refused.
    #[test]
    fn a_chain_of_links_is_checked_at_every_step() {
        let base = std::env::temp_dir().join(format!("orq-passkeys-unit-{}-chain", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let private = |p: std::path::PathBuf| std::fs::DirBuilder::new().recursive(true).mode(0o700).create(p).unwrap();
        private(base.join("trusted"));
        private(base.join("private/owned"));
        let shared = base.join("shared");
        private(shared.clone());
        std::os::unix::fs::symlink(base.join("private/owned"), shared.join("next")).unwrap();
        std::os::unix::fs::symlink(shared.join("next"), base.join("trusted/first")).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        let store = PasskeyStore { dir: base.join("trusted/first/state") };
        assert!(store.add(key("a", "one")).is_err() && !store.any_registered());
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o700)).unwrap();
        store.add(key("a", "one")).expect("every directory on the chain is private");
    }

    /// Dot-dot steps and a loop of links are handled rather than followed forever.
    #[test]
    fn dot_dot_steps_resolve_and_a_loop_of_links_is_refused() {
        let base = std::env::temp_dir().join(format!("orq-passkeys-unit-{}-loop", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(base.join("a/b")).unwrap();
        let dirs = directories_passed(&base.join("a/b/../b")).unwrap();
        assert!(dirs.contains(&base.join("a/b")) && dirs.contains(&base.join("a")));
        std::os::unix::fs::symlink(base.join("l2"), base.join("l1")).unwrap();
        std::os::unix::fs::symlink(base.join("l1"), base.join("l2")).unwrap();
        assert!(directories_passed(&base.join("l1/x")).is_err());
    }
}
