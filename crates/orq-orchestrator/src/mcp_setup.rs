//! Setting up a run's MCP servers in its worktree, the same way for every project: from the main
//! checkout's lock when it has one, so the worktree runs exactly the versions the project locks.
//! Every mcpctl call runs in the worktree, never in the main checkout, and its output goes to the
//! run's setup log. A problem is a note, never a failed run.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MCPCTL_LOCK: &str = ".mcpctl.lock";
/// mcpctl's record of the files `init` scaffolded.
const MCPCTL_SCAFFOLD: &str = ".mcpctl.scaffold.json";
/// What mcpctl says when `setup` refuses because the project's pins exclude the newest source.
const PINS_REFUSED: &str = "does not satisfy constraint";
/// The longest piece of mcpctl's output a note quotes.
const MAX_LINE: usize = 300;

/// A call that did not succeed: what happened, and the last line it printed.
struct Failed {
    what: String,
    last_line: Option<String>,
}

impl Failed {
    fn note(&self) -> String {
        match &self.last_line {
            Some(line) => format!("{}: {line}", self.what),
            None => self.what.clone(),
        }
    }
}

/// How long each mcpctl call may take before it is stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpTimeouts {
    /// `sync` and `setup`, which may install.
    pub install: Duration,
    /// `init`, which only copies files.
    pub init: Duration,
}

impl Default for McpTimeouts {
    fn default() -> Self {
        Self { install: Duration::from_secs(60), init: Duration::from_secs(30) }
    }
}

/// What to set up, and where.
pub struct McpSetupRequest<'a> {
    pub main_checkout: &'a Path,
    pub worktree: &'a Path,
    pub servers: &'a [String],
    /// Added to each call's environment.
    pub env: &'a [(String, String)],
    /// The program, `None` when it is not installed.
    pub mcpctl: Option<&'a Path>,
    /// Where the output of each call is appended.
    pub log: &'a Path,
    pub timeouts: McpTimeouts,
}

/// One server's version in the worktree, and where it came from: `locked`, `newest`, `within pins`
/// or `machine active`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerVersion {
    pub name: String,
    pub version: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpSetupReport {
    pub versions: Vec<McpServerVersion>,
    pub notes: Vec<String>,
}

/// Sets up `servers` in the worktree; see the module documentation.
pub fn run_mcp_setup(request: &McpSetupRequest<'_>) -> McpSetupReport {
    let mut report = McpSetupReport::default();
    let mut seen = std::collections::HashSet::new();
    let names: Vec<&str> = request.servers.iter().map(String::as_str).filter(|name| seen.insert(*name)).collect();
    let machine_active = || {
        let active = names.iter().map(|name| McpServerVersion { name: name.to_string(), version: None, source: "machine active".to_string() });
        active.collect()
    };
    let Some(program) = request.mcpctl else {
        report.notes.push("mcpctl is not on PATH; the configured MCP servers were not set up".to_string());
        report.versions = machine_active();
        return report;
    };
    // Setup rewrites the project's files: in the main checkout it would change the user's own.
    let same = |a: &Path, b: &Path| matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b);
    if same(request.main_checkout, request.worktree) {
        report.notes.push("the worktree is the main checkout; MCP servers are not set up there".to_string());
        report.versions = machine_active();
        return report;
    }
    carry_lock(request, &mut report.notes);
    let (install, init) = (request.timeouts.install, request.timeouts.init);
    let locked = request.worktree.join(MCPCTL_LOCK).exists();
    let done = if locked {
        calls(request, program, &[(&["sync", "--locked"], install), (&["init"], init)]).map(|()| "locked")
    } else {
        let setup: Vec<&str> = std::iter::once("setup").chain(names.iter().flat_map(|name| ["--mcp", *name])).collect();
        calls(request, program, &[(&setup, install)]).map(|()| "newest")
    };
    let done = match done {
        // Only `setup` falls back: a locked install that fails must never be replaced by an unlocked one.
        Err(refused) if !locked && refused.last_line.as_deref().is_some_and(|line| line.contains(PINS_REFUSED)) => {
            calls(request, program, &[(&["sync"], install), (&["init"], init)]).map(|()| {
                let versions = locked_versions(request.worktree, &names, "within pins");
                let used: Vec<String> = versions
                    .iter()
                    .map(|v| format!("{} {}", v.name, v.version.as_deref().unwrap_or("(version unknown)")))
                    .collect();
                report.notes.push(format!(
                    "mcpctl setup was refused ({}); the versions within the project's pins were used instead: {}",
                    refused.last_line.as_deref().unwrap_or_default(),
                    used.join(", ")
                ));
                report.versions = versions;
            })
        }
        Err(failed) => Err(failed),
        Ok(source) => {
            report.versions = locked_versions(request.worktree, &names, source);
            Ok(())
        }
    };
    if let Err(failed) = done {
        report.notes.push(failed.note());
        report.versions = machine_active();
    }
    // Even a failed call may have left files behind.
    if let Err(why) = exclude_created(request.worktree) {
        report.notes.push(format!("the files MCP setup created could not all be kept out of git status: {why}"));
    }
    report
}

/// The scaffold record's listed files, as plain relative paths; anything else is skipped.
pub(crate) fn scaffolded_files(worktree: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(worktree.join(MCPCTL_SCAFFOLD)) else {
        return Vec::new();
    };
    let Ok(record) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let files = record.get("files").and_then(|files| files.as_object());
    files
        .into_iter()
        .flat_map(|files| files.keys())
        .filter(|name| crate::local_files::plain_relative(name))
        .cloned()
        .collect()
}

/// Adds the lock, the scaffold record and every scaffolded file that exists, is not tracked and is
/// not already ignored to the repository's local exclude, in one write.
fn exclude_created(worktree: &Path) -> Result<(), String> {
    let mut names: Vec<String> = vec![MCPCTL_LOCK.to_string(), MCPCTL_SCAFFOLD.to_string()];
    names.extend(scaffolded_files(worktree));
    names.retain(|name| std::fs::symlink_metadata(worktree.join(name)).is_ok());
    if names.is_empty() {
        return Ok(());
    }
    let tracked = git_listed(worktree, &["ls-files", "-z", "--"], &names)?;
    names.retain(|name| !tracked.contains(name));
    let unignored = |names: &[String]| -> Result<Vec<String>, String> {
        let ignored = git_listed(worktree, &["check-ignore", "-z", "--stdin"], names)?;
        Ok(names.iter().filter(|name| !ignored.contains(*name)).cloned().collect())
    };
    let missing = unignored(&names)?;
    if missing.is_empty() {
        return Ok(());
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["rev-parse", "--path-format=absolute", "--git-path", "info/exclude"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .ok_or("the local exclude was not found")?;
    let path = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    for name in &missing {
        text.push_str(&format!("/{}\n", crate::local_files::escape_pattern(name)));
    }
    path.parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, text))
        .map_err(|err| format!("the local exclude could not be written ({:?})", err.kind()))?;
    match unignored(&missing)?.as_slice() {
        [] => Ok(()),
        left => Err(format!("git still does not ignore {}", left.join(", "))),
    }
}

/// The NUL-separated paths git prints for `args` and `names`: after the arguments, or NUL-separated on
/// its input when `args` asks for `--stdin`. Exit code 1 means "none".
fn git_listed(worktree: &Path, args: &[&str], names: &[String]) -> Result<Vec<String>, String> {
    let ran = |err: std::io::Error| format!("git could not be run ({:?})", err.kind());
    let mut command = Command::new("git");
    command.arg("-C").arg(worktree).args(args);
    let out = if args.contains(&"--stdin") {
        let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(ran)?;
        let input: String = names.iter().map(|name| format!("{name}\0")).collect();
        let written = child.stdin.take().map(|mut stdin| stdin.write_all(input.as_bytes()));
        let out = child.wait_with_output().map_err(ran)?;
        written.transpose().map_err(ran)?;
        out
    } else {
        command.args(names).output().map_err(ran)?
    };
    if !matches!(out.status.code(), Some(0 | 1)) {
        return Err(format!("git {} failed ({})", args[0], out.status));
    }
    let printed = String::from_utf8_lossy(&out.stdout);
    Ok(printed.split('\0').filter(|name| !name.is_empty()).map(str::to_string).collect())
}

/// Runs each call in turn, stopping at the first that fails.
fn calls(request: &McpSetupRequest<'_>, program: &Path, steps: &[(&[&str], Duration)]) -> Result<(), Failed> {
    steps.iter().try_for_each(|(args, limit)| call(request, program, args, *limit))
}

/// Copies the main checkout's lock into a worktree that has none, kept out of git status.
fn carry_lock(request: &McpSetupRequest<'_>, notes: &mut Vec<String>) {
    let (source, target) = (request.main_checkout.join(MCPCTL_LOCK), request.worktree.join(MCPCTL_LOCK));
    if std::fs::symlink_metadata(&target).is_ok() || !std::fs::symlink_metadata(&source).is_ok_and(|meta| meta.is_file()) {
        return;
    }
    if let Err(why) = crate::local_files::exclude_locally(request.worktree, MCPCTL_LOCK) {
        notes.push(format!("{MCPCTL_LOCK} is not carried into the worktree: {why}"));
        return;
    }
    if let Err(err) = std::fs::copy(&source, &target) {
        notes.push(format!("{MCPCTL_LOCK} could not be carried into the worktree ({:?})", err.kind()));
    }
}

/// Runs `mcpctl <args>` in the worktree with its output appended to the log, stopped after `limit`.
fn call(request: &McpSetupRequest<'_>, program: &Path, args: &[&str], limit: Duration) -> Result<(), Failed> {
    let what = format!("mcpctl {}", args.join(" "));
    let failed = |what: String| Failed { what, last_line: None };
    let unopened = |err: std::io::Error| failed(format!("{what} was not run: its log could not be opened ({:?})", err.kind()));
    let mut log = OpenOptions::new().create(true).append(true).open(request.log).map_err(unopened)?;
    log.write_all(format!("$ {what}\n").as_bytes()).map_err(unopened)?;
    let before = std::fs::metadata(request.log).map_or(0, |meta| meta.len());
    let err_log = log.try_clone().map_err(unopened)?;
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(request.worktree)
        .envs(request.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err_log)
        // Its own group, so a stopped call takes whatever it started with it.
        .process_group(0);
    let child = crate::child_groups::spawn(&mut command)
        .map_err(|err| failed(format!("{what} could not be started ({:?})", err.kind())))?;
    let started = Instant::now();
    let why = loop {
        match child.has_exited() {
            Ok(true) => match child.reap() {
                Ok(status) if status.success() => return Ok(()),
                Ok(status) => break format!("{what} failed ({status})"),
                Err(err) => break format!("{what} could not be waited for ({:?})", err.kind()),
            },
            Ok(false) if started.elapsed() >= limit => {
                child.end_group();
                let _ = child.reap();
                break format!("{what} did not finish within {}s and was stopped", limit.as_secs());
            }
            Ok(false) => std::thread::sleep(Duration::from_millis(20)),
            Err(err) => break format!("{what} could not be waited for ({:?})", err.kind()),
        }
    };
    Err(Failed { what: why, last_line: last_line(request.log, before) })
}

/// The last non-empty line written to `log` after `from`, cut to [`MAX_LINE`] characters.
fn last_line(log: &Path, from: u64) -> Option<String> {
    let bytes = std::fs::read(log).ok()?;
    let written = bytes.get(usize::try_from(from).ok()?..)?;
    let text = String::from_utf8_lossy(written);
    let line = text.lines().map(str::trim).filter(|line| !line.is_empty()).last()?;
    Some(line.chars().take(MAX_LINE).collect())
}

/// Each of `servers`' version in the worktree's lock, marked `source`.
fn locked_versions(worktree: &Path, servers: &[&str], source: &str) -> Vec<McpServerVersion> {
    let doc = std::fs::read_to_string(worktree.join(MCPCTL_LOCK))
        .ok()
        .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok());
    let tables = doc.as_ref().and_then(|doc| doc.get("mcp")).and_then(|item| item.as_array_of_tables());
    servers
        .iter()
        .map(|name| {
            let version = tables
                .into_iter()
                .flat_map(|tables| tables.iter())
                .find(|table| table.get("name").and_then(|v| v.as_str()) == Some(*name))
                .and_then(|table| table.get("version")?.as_str().map(str::to_string));
            McpServerVersion { name: name.to_string(), version, source: source.to_string() }
        })
        .collect()
}
