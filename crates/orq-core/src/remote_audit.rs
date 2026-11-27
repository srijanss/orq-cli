//! The audit log of remote actions: one JSON line appended for every attempt, allowed or refused, saying
//! when, who (the signed-in passkey's label), what, and on which project and run. An attempt is built
//! only from these named fields, so no header or token can reach the file, and free text is redacted
//! on the way in. Nothing here rewrites or removes a line; the web reads the log and cannot change it.

use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

use serde_json::{json, Value};

use crate::redact::redact;
use crate::storage::StorageRoots;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Allowed,
    Refused,
}

/// One action somebody tried from the web.
pub struct Attempt<'a> {
    pub identity: &'a str,
    pub action: &'a str,
    pub project: &'a str,
    pub run: Option<&'a str>,
    pub outcome: Outcome,
    pub reason: Option<&'a str>,
}

/// Most bytes one entry may take, so a read can bound how much of the end of the log it looks at.
const MAX_LINE: usize = 16 * 1024;
/// Longest text kept for any one field.
const MAX_FIELD: usize = 256;

/// Longest value looked at at all; anything longer is replaced whole, so the work stays bounded.
const MAX_INPUT: usize = 4096;

/// Redacts the whole text first and only then shortens it, so a secret cut by the limit is already gone.
fn field(text: &str) -> String {
    if text.len() > MAX_INPUT {
        return "[too long to record]".to_string();
    }
    redact(text).chars().take(MAX_FIELD).collect()
}

fn path(roots: &StorageRoots) -> std::path::PathBuf {
    roots.state.join("remote-actions.jsonl")
}

thread_local! {
    /// Who a web request on this thread came from, while it is answered (see [`as_web`]).
    static WEB: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Runs `answer` as the answer to a web request from `identity` (the passkey device, or `token`): every line
/// recorded meanwhile on this thread says `"source": "web"`, and one recorded as plain `web` names `identity`.
pub fn as_web<T>(identity: &str, answer: impl FnOnce() -> T) -> T {
    let before = WEB.with(|web| web.replace(Some(identity.to_string())));
    struct Restore(Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let before = self.0.take();
            WEB.with(|web| *web.borrow_mut() = before);
        }
    }
    let _restore = Restore(before);
    answer()
}

/// Appends one line for `attempt`, made at `now` (seconds since the epoch).
pub fn record(roots: &StorageRoots, attempt: &Attempt, now: u64) -> Result<(), String> {
    let web = WEB.with(|web| web.borrow().clone());
    let identity = match (&web, attempt.identity) {
        (Some(device), "web") => device.as_str(),
        _ => attempt.identity,
    };
    let mut line = json!({
        "time": now,
        "identity": field(identity),
        "action": field(attempt.action),
        "project": field(attempt.project),
        "run": attempt.run.map(field),
        "outcome": match attempt.outcome { Outcome::Allowed => "allowed", Outcome::Refused => "refused" },
        "reason": attempt.reason.map(field),
    });
    if web.is_some() {
        line["source"] = json!("web");
    }
    let mut file = open(roots, true)?;
    // A write that was cut short leaves a line with no end; the next record starts a line of its own.
    let torn = file.metadata().map(|meta| meta.len() > 0).unwrap_or(false) && !ends_with_newline(roots);
    file.write_all(format!("{}{line}\n", if torn { "\n" } else { "" }).as_bytes()).map_err(|e| format!("cannot write the audit log: {}", e.kind()))
}

fn ends_with_newline(roots: &StorageRoots) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = open(roots, false) else { return false };
    let mut last = [0u8; 1];
    file.seek(SeekFrom::End(-1)).and_then(|_| file.read_exact(&mut last)).is_ok() && last[0] == b'\n'
}

/// Opens the log without following a link, and only if it is a regular file this user owns; a file
/// left readable by others is tightened to this user alone.
fn open(roots: &StorageRoots, write: bool) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    // O_NONBLOCK keeps a pipe in the log's place from hanging the open; the descriptor is checked before any I/O.
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    if write {
        options.create(true).append(true).mode(0o600);
    } else {
        options.read(true);
    }
    let file = options.open(path(roots)).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "not found".to_string(),
        kind => format!("cannot open the audit log: {kind}"),
    })?;
    let meta = file.metadata().map_err(|e| format!("cannot open the audit log: {}", e.kind()))?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } {
        return Err("the audit log is not a plain file of this user".to_string());
    }
    if meta.permissions().mode() & 0o077 != 0 {
        file.set_permissions(std::fs::Permissions::from_mode(0o600)).map_err(|e| format!("cannot open the audit log: {}", e.kind()))?;
    }
    Ok(file)
}

/// The last `window` bytes of the file, without a first line that the window cut in two.
fn tail(mut file: std::fs::File, window: usize) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let len = file.metadata()?.len();
    let start = len.saturating_sub(window as u64);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity(len.min(window as u64) as usize);
    file.take(window as u64).read_to_end(&mut bytes)?;
    if start > 0 {
        bytes.drain(..bytes.iter().position(|b| *b == b'\n').map_or(bytes.len(), |i| i + 1));
    }
    Ok(bytes)
}

/// The newest `limit` lines, newest first; no log yet reads as empty.
pub fn read(roots: &StorageRoots, limit: usize) -> Result<Vec<Value>, String> {
    read_window(roots, limit, limit.saturating_mul(MAX_LINE), &Filter::default())
}

/// Which lines a filtered read keeps; `None` keeps any.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Filter {
    /// `web` or `cli`, as [`source_of`] tells them.
    pub source: Option<String>,
    /// The passkey device's label, `token`, or the name a terminal action was recorded under.
    pub identity: Option<String>,
    /// An action (`run.discard`) or its kind, the part before the dot (`run`).
    pub action: Option<String>,
    /// Seconds since the epoch, both inclusive.
    pub since: Option<u64>,
    pub until: Option<u64>,
}

impl Filter {
    fn keeps(&self, line: &Value) -> bool {
        let text = |key: &str| line[key].as_str().unwrap_or("");
        let action = text("action");
        let time = line["time"].as_u64().unwrap_or(0);
        self.source.as_deref().is_none_or(|source| source_of(line) == source)
            && self.identity.as_deref().is_none_or(|identity| text("identity") == identity)
            && self.action.as_deref().is_none_or(|kind| action == kind || action.strip_prefix(kind).is_some_and(|rest| rest.starts_with('.')))
            && self.since.is_none_or(|since| time >= since)
            && self.until.is_none_or(|until| time <= until)
    }
}

/// Where a line came from: `web` for a request to the web server, `cli` for orq's own and the terminal's.
/// A line from before sources were recorded counts as web when it was recorded as plain `web`.
pub fn source_of(line: &Value) -> &'static str {
    match (line["source"].as_str(), line["identity"].as_str()) {
        (Some("web"), _) | (None, Some("web")) => "web",
        _ => "cli",
    }
}

/// How much of the end of the log a filtered read looks through.
const FILTER_WINDOW: usize = 8 * 1024 * 1024;

/// The newest `limit` lines that `filter` keeps, newest first, from the last few megabytes of the log. With no
/// filter it reads only as much as [`read`] does: the newest `limit` lines are in that.
pub fn read_filtered(roots: &StorageRoots, limit: usize, filter: &Filter) -> Result<Vec<Value>, String> {
    if *filter == Filter::default() {
        return read(roots, limit);
    }
    read_window(roots, limit, FILTER_WINDOW, filter)
}

fn read_window(roots: &StorageRoots, limit: usize, window: usize, filter: &Filter) -> Result<Vec<Value>, String> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let tail = match open(roots, false) {
        Ok(file) => tail(file, window).map_err(|e| format!("cannot read the audit log: {}", e.kind()))?,
        Err(why) if why == "not found" => return Ok(Vec::new()),
        Err(why) => return Err(why),
    };
    let text = String::from_utf8_lossy(&tail);
    Ok(text
        .lines()
        .rev()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap_or_else(|_| json!({ "damaged": true })))
        .filter(|line| filter.keeps(line))
        .take(limit)
        .collect())
}
