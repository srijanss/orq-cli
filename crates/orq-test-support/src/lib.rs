mod fake_agent;

pub use fake_agent::{FakeAgent, FakeAgentBuilder, Run};

mod fake_provider;

pub use fake_provider::{FakeProvider, FakeProviderBuilder, ProviderCall};

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Create a new, empty directory under the temp dir that only this user can
/// enter. `create_dir` fails on an existing path (including a planted symlink),
/// so a pre-created directory is never adopted: the next name is tried instead.
pub(crate) fn private_dir(prefix: &str) -> PathBuf {
    use std::os::unix::fs::DirBuilderExt;

    loop {
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("{prefix}-{}-{unique}", std::process::id()));
        match fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => return dir,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => panic!("create private temp dir {dir:?}: {err}"),
        }
    }
}

/// A private, empty directory under the temp dir, created exclusively (see `private_dir`) and
/// removed with everything in it when dropped, including when the test panics.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(prefix: &str) -> Self {
        TempDir { path: private_dir(prefix) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// The process temp dir, and the open directory whose lock says its process is still running.
static PROCESS_TEMP_DIR: std::sync::OnceLock<(PathBuf, fs::File)> = std::sync::OnceLock::new();

extern "C" {
    fn atexit(callback: extern "C" fn()) -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
    fn getuid() -> u32;
    fn flock(fd: i32, operation: i32) -> i32;
}

extern "C" fn remove_process_temp_dir() {
    if let Some((dir, _)) = PROCESS_TEMP_DIR.get() {
        let _ = fs::remove_dir_all(dir);
    }
}

/// Takes the exclusive lock on `dir` without waiting; `None` while another process holds it. The
/// lock lasts while the returned file is open, and the system drops it when its process dies,
/// however it dies, in whatever PID namespace.
fn try_lock(dir: &Path) -> Option<fs::File> {
    use std::os::fd::AsRawFd;
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    let file = fs::File::open(dir).ok()?;
    // SAFETY: locks a descriptor this function owns.
    (unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0).then_some(file)
}

/// A new process temp dir, locked before it gets the `orq-tests-<pid>-<n>` name a sweep looks for,
/// so no sweep ever finds a live one unlocked.
fn locked_process_root() -> (PathBuf, fs::File) {
    let staged = private_dir("orq-tests-new");
    let lock = try_lock(&staged).unwrap_or_else(|| panic!("lock new private temp dir {staged:?}"));
    for _ in 0..1000 {
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("orq-tests-{}-{unique}", std::process::id()));
        if fs::symlink_metadata(&dir).is_err() && fs::rename(&staged, &dir).is_ok() {
            return (dir, lock);
        }
    }
    panic!("cannot find an unused name for {staged:?}");
}

/// Whether no process `pid` exists. Signal 0 only checks: a process of another user answers
/// "not permitted", which still means it exists.
fn process_gone(pid: i32) -> bool {
    const ESRCH: i32 = 3;
    // SAFETY: signal 0 sends nothing.
    unsafe { kill(pid, 0) != 0 && std::io::Error::last_os_error().raw_os_error() == Some(ESRCH) }
}

/// Removes this user's `orq-tests-<pid>-<n>` roots whose process no longer exists: a test process
/// that was killed (SIGKILL, the OOM killer) never ran its exit cleanup. A PID not seen here may
/// belong to a process in another PID namespace sharing this temp dir, so a root is removed only
/// when its lock is free too, and the lock is held while it is removed.
fn sweep_roots_of_gone_processes(temp: &Path) {
    use std::os::unix::fs::MetadataExt;
    let Ok(entries) = fs::read_dir(temp) else { return };
    // SAFETY: getuid cannot fail.
    let me = unsafe { getuid() };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some((pid, unique)) = name.to_str().and_then(|name| name.strip_prefix("orq-tests-")).and_then(|rest| rest.split_once('-')) else { continue };
        let (Ok(pid), Ok(_)) = (pid.parse::<i32>(), unique.parse::<u64>()) else { continue };
        // A symlink or another user's entry is never followed or removed.
        let Ok(metadata) = fs::symlink_metadata(entry.path()) else { continue };
        if pid > 0 && metadata.is_dir() && metadata.uid() == me && process_gone(pid) {
            if let Some(_lock) = try_lock(&entry.path()) {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// A private directory under the temp dir for everything this test process makes there, removed
/// when the process exits (tests that pass, fail or panic alike). Tests use it where they would
/// use `std::env::temp_dir()`, and no longer leave their scratch directories behind. The first use
/// also removes the roots of earlier test processes that were killed before they could clean up.
pub fn process_temp_dir() -> PathBuf {
    PROCESS_TEMP_DIR
        .get_or_init(|| {
            sweep_roots_of_gone_processes(&std::env::temp_dir());
            let root = locked_process_root();
            // SAFETY: registers a plain function that only removes a directory.
            unsafe { atexit(remove_process_temp_dir) };
            root
        })
        .0
        .clone()
}

/// A throwaway Git repository in a unique temp directory, removed on drop.
pub struct GitFixture {
    base: PathBuf,
    root: PathBuf,
    nested: PathBuf,
}

impl GitFixture {
    pub fn new() -> Self {
        let base = private_dir("orq-fixture");
        let root = base.join("repo");
        let nested = root.join("src").join("nested");
        fs::create_dir_all(&nested).expect("create fixture dirs");
        fs::write(root.join("README.md"), "fixture\n").expect("write README");

        run_git(&root, &["init", "--quiet"]);
        run_git(&root, &["add", "README.md"]);
        run_git(
            &root,
            &[
                "-c",
                "user.name=Orq Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "initial",
            ],
        );

        Self { base, root, nested }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn nested_dir(&self) -> &Path {
        &self.nested
    }

    /// Adds a linked worktree on a new branch `name`, as a sibling of the repo.
    pub fn add_linked_worktree(&self, name: &str) -> PathBuf {
        let path = self.base.join(format!("worktree-{name}"));
        let path_arg = path.to_str().expect("utf-8 fixture path");
        run_git(&self.root, &["worktree", "add", "--quiet", "-b", name, path_arg]);
        path
    }

    /// Creates a bare repository and registers it as the `origin` remote.
    pub fn add_bare_remote(&self) -> PathBuf {
        let path = self.base.join("remote.git");
        let path_arg = path.to_str().expect("utf-8 fixture path");
        run_git(&self.base, &["init", "--quiet", "--bare", path_arg]);
        run_git(&self.root, &["remote", "add", "origin", path_arg]);
        path
    }
}

impl Default for GitFixture {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for GitFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        // Isolate from the developer's git config (signing, hooks, templates).
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// One entry to write into a fixture `.tdd-features.json`.
pub struct LedgerEntry {
    feature_name: String,
    status: String,
    description: Option<String>,
    depends_on: Vec<String>,
}

impl LedgerEntry {
    pub fn new(feature_name: &str, status: &str) -> Self {
        Self {
            feature_name: feature_name.to_string(),
            status: status.to_string(),
            description: None,
            depends_on: Vec::new(),
        }
    }

    pub fn description(mut self, description: &str) -> Self {
        self.description = Some(description.to_string());
        self
    }

    pub fn depends_on(mut self, names: &[&str]) -> Self {
        self.depends_on = names.iter().map(|name| name.to_string()).collect();
        self
    }

    fn to_json(&self) -> serde_json::Value {
        let mut entry = serde_json::json!({
            "featureName": self.feature_name,
            "dependsOn": self.depends_on,
            "status": self.status,
        });
        if let Some(description) = &self.description {
            entry["description"] = description.clone().into();
        }
        entry
    }
}

/// Writes `entries` as `<dir>/.tdd-features.json` (2-space JSON array, trailing
/// newline) and returns the path. `dir` is a fixture repository or worktree.
pub fn write_ledger(dir: &Path, entries: &[LedgerEntry]) -> PathBuf {
    let array: Vec<serde_json::Value> = entries.iter().map(LedgerEntry::to_json).collect();
    let mut text = serde_json::to_string_pretty(&array).expect("serialize ledger");
    text.push('\n');
    write_ledger_bytes(dir, text.as_bytes())
}

/// Writes `bytes` verbatim as `<dir>/.tdd-features.json` (for malformed or
/// non-UTF-8 cases) and returns the path.
pub fn write_ledger_bytes(dir: &Path, bytes: &[u8]) -> PathBuf {
    let path = dir.join(".tdd-features.json");
    fs::write(&path, bytes).expect("write fixture ledger");
    path
}
