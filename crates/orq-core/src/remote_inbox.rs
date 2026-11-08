//! A read-only view of the review inbox for a remote browser.
//!
//! This is the request handler only: it takes an already-parsed request and
//! returns a JSON answer, reading the same stored reports Herdr shows. It has
//! no way to change anything; merging happens at the provider, after
//! following the link in the answer. Every request is read from storage afresh,
//! so a lifecycle change shows on the next request.

use std::net::{IpAddr, Ipv6Addr};

use serde_json::{json, Value};

use crate::review_inbox::{self, InboxDetail};
use crate::storage::{check_run_id, StorageRoots};
use crate::store::Store;

pub struct RemoteConfig {
    /// The shared secret a client sends as `Authorization: Bearer <token>`.
    /// An empty token means nobody is authorised.
    pub token: String,
}

/// Shortest token accepted from the token file.
const MIN_TOKEN_LEN: usize = 32;
/// The rules a token file's metadata must meet: a regular file, owned by the
/// user running the server, and not accessible to anyone else.
pub fn check_token_file_meta(is_file: bool, owner: u32, mode: u32, effective_uid: u32) -> Result<(), String> {
    if !is_file {
        return Err("the remote token file must be a regular file".into());
    }
    if owner != effective_uid {
        return Err("the remote token file must belong to the user running the server".into());
    }
    if mode & 0o077 != 0 {
        return Err("the remote token file must be readable by its owner only (chmod 600)".into());
    }
    Ok(())
}

/// Largest token file read; a bigger one is refused, never cut short.
const MAX_TOKEN_FILE: u64 = 4096;

impl RemoteConfig {
    /// Where the token lives: its own file in Orq's config directory.
    pub fn token_path(roots: &StorageRoots) -> std::path::PathBuf {
        roots.config.join("remote-token")
    }

    /// Like `load`, but a missing token file is created first: 32 random bytes as hex, written
    /// for its owner only, never replacing a file that is already there.
    pub fn load_or_create(roots: &StorageRoots) -> Result<RemoteConfig, String> {
        use std::io::{Read, Write};
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let path = Self::token_path(roots);
        if std::fs::symlink_metadata(&path).is_err() {
            if let Some(dir) = path.parent() {
                std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| format!("cannot create the Orq config directory: {}", e.kind()))?;
            }
            let mut bytes = [0u8; 40];
            std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).map_err(|e| format!("cannot generate a remote token: {}", e.kind()))?;
            let token: String = bytes[..32].iter().map(|b| format!("{b:02x}")).collect();
            // Written whole to a private temporary file, then linked into place without replacing
            // anything: a reader sees the finished token or no file, never a partial one.
            let temp = path.with_file_name(format!(".remote-token.{}.tmp", bytes[32..].iter().map(|b| format!("{b:02x}")).collect::<String>()));
            let written = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&temp)
                .and_then(|mut file| file.write_all(token.as_bytes()).and_then(|()| file.sync_all()));
            let published = written.and_then(|()| std::fs::hard_link(&temp, &path));
            let _ = std::fs::remove_file(&temp);
            match published {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(format!("cannot create the remote token file {}: {}", path.display(), e.kind())),
            }
        }
        Self::load(roots)
    }

    /// Reads the token file. It must be a regular file (not a link) that only
    /// its owner can access, and hold a token of at least 32 characters. An
    /// error never contains the token.
    pub fn load(roots: &StorageRoots) -> Result<RemoteConfig, String> {
        use std::io::Read;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let path = Self::token_path(roots);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)
            .map_err(|e| format!("cannot open the remote token file {}: {}", path.display(), e.kind()))?;
        let meta = file.metadata().map_err(|e| format!("cannot inspect the remote token file: {}", e.kind()))?;
        // SAFETY: geteuid has no preconditions and cannot fail.
        check_token_file_meta(meta.is_file(), meta.uid(), meta.mode(), unsafe { libc::geteuid() })?;
        let mut text = String::new();
        file.take(MAX_TOKEN_FILE + 1).read_to_string(&mut text).map_err(|e| format!("cannot read the remote token file (is it text?): {}", e.kind()))?;
        if text.len() as u64 > MAX_TOKEN_FILE {
            return Err(format!("the remote token file is larger than {MAX_TOKEN_FILE} bytes"));
        }
        let token = text.trim().to_string();
        if token.len() < MIN_TOKEN_LEN || token.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(format!("the remote token must be at least {MIN_TOKEN_LEN} characters with no spaces"));
        }
        Ok(RemoteConfig { token })
    }
}

pub struct Request<'a> {
    pub method: &'a str,
    /// The path without a query string.
    pub path: &'a str,
    /// The address the connection came from.
    pub peer: IpAddr,
    pub authorization: Option<&'a str>,
}

pub struct Response {
    pub status: u16,
    pub body: Value,
}

impl Response {
    /// The bytes to put on the wire: JSON, or nothing at all when the body is
    /// absent (a HEAD answer). `Null` means "no body", never the text `null`.
    pub fn body_bytes(&self) -> Vec<u8> {
        if self.body.is_null() {
            Vec::new()
        } else {
            self.body.to_string().into_bytes()
        }
    }
}

fn reply(status: u16, body: Value) -> Response {
    Response { status, body }
}

fn error(status: u16, message: &str) -> Response {
    reply(status, json!({ "error": message }))
}

/// Loopback and the Tailscale ranges only: 100.64.0.0/10 and fd7a:115c:a1e0::/48.
pub fn peer_allowed(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || (v4.octets()[0] == 100 && (64..=127).contains(&v4.octets()[1])),
        IpAddr::V6(v6) => v6.is_loopback() || tailscale_v6(&v6) || v6.to_ipv4_mapped().is_some_and(|v4| peer_allowed(IpAddr::V4(v4))),
    }
}

fn tailscale_v6(ip: &Ipv6Addr) -> bool {
    ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0]
}

/// A provider link is passed on only when it is a plain `https` URL with a
/// host, no credentials, no whitespace or control characters, and nothing that
/// could break out of an attribute or a header.
pub fn safe_link(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let bad_char = |c: char| c.is_control() || c.is_whitespace() || !c.is_ascii() || matches!(c, '"' | '\'' | '<' | '>' | '`' | '\\');
    if authority.is_empty() || authority.contains('@') || url.chars().any(bad_char) {
        return None;
    }
    Some(url.to_string())
}

pub(crate) fn token_matches(expected: &str, header: Option<&str>) -> bool {
    let Some(given) = header.and_then(|h| h.strip_prefix("Bearer ")) else { return false };
    if expected.is_empty() || given.len() != expected.len() {
        return false;
    }
    given.bytes().zip(expected.bytes()).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
}

/// What the server supplies to answer `/api/projects`. Peer, token and method are checked here
/// before it is asked, so a provider never sees a request that was refused.
pub trait ProjectsProvider {
    /// The projects as JSON, or why they cannot be listed.
    fn projects(&self) -> Result<Value, String>;

    /// The review scope of project `id`, `None` when there is no such project.
    fn review_scope(&self, _id: &str) -> Result<Option<ReviewScope>, String> {
        Ok(None)
    }

    /// The ledger features of project `id` as a JSON array, `None` when there is no such project.
    fn features(&self, _id: &str) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// The orchestration runs of project `id`, newest first, as a JSON array; `None` when there is no such project.
    fn runs(&self, _id: &str) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// One run of project `id` as `{run, events, next_cursor}`: its events after the event `after`
    /// (from the start when `None`), one page at most. `None` when the project, the run (of that
    /// project) or the cursor is unknown.
    fn run_timeline(&self, _id: &str, _run: &str, _after: Option<&str>) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// The log files of a run of project `id` (`file` is `None`), or the redacted lines of one file
    /// after the line count `after`, one page at most. `None` when the project, the run (of that
    /// project), the file or the cursor is unknown.
    fn run_logs(&self, _id: &str, _run: &str, _file: Option<&str>, _after: Option<&str>) -> Result<Option<Value>, String> {
        Ok(None)
    }

    /// The redacted run settings as JSON, `None` when this provider has none.
    fn settings(&self) -> Result<Option<Value>, String> {
        Ok(None)
    }
}

/// The run settings of the config file at `path`, as `{implementer, reviewer, models}`, for the web.
/// Only the harness, model and effort of each agent leave, and each through the redactor; every
/// other key (tokens, environment, credential paths, the remote section) is left behind. A missing
/// file is empty settings; a file that cannot be parsed is an error that does not name the file.
pub fn public_settings(path: &std::path::Path) -> Result<Value, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return Err("the settings could not be read".to_string()),
    };
    let doc: toml_edit::DocumentMut = text.parse().map_err(|_| "the settings could not be read".to_string())?;
    let field = |table: &dyn toml_edit::TableLike, key: &str| table.get(key).and_then(|item| item.as_str()).map(|value| Value::String(crate::redact::redact(value))).unwrap_or(Value::Null);
    let agent = |table: &dyn toml_edit::TableLike| json!({"harness": field(table, "harness"), "model": field(table, "model"), "effort": field(table, "effort")});
    let section = |name: &str| doc.get(name).and_then(|item| item.as_table_like()).map(agent).unwrap_or(Value::Null);
    let models: Vec<Value> = doc.get("models").and_then(|item| item.as_table_like()).map(|table| table.iter().filter_map(|(name, item)| item.as_table_like().map(|t| json!({"name": crate::redact::redact(name), "harness": field(t, "harness"), "model": field(t, "model"), "effort": field(t, "effort")}))).collect()).unwrap_or_default();
    Ok(json!({"implementer": section("implementer"), "reviewer": section("reviewer"), "models": models}))
}

/// How the review store knows a project: the id its reports are saved under and its display name.
pub struct ReviewScope {
    pub review_key: String,
    pub name: String,
}

pub fn handle(roots: &StorageRoots, config: &RemoteConfig, project_id: &str, project_name: &str, request: &Request) -> Response {
    answer(roots, config, project_id, project_name, request, None)
}

/// Like [`handle`], and `/api/projects` is answered by `provider`.
pub fn handle_with(roots: &StorageRoots, config: &RemoteConfig, project_id: &str, project_name: &str, request: &Request, provider: &dyn ProjectsProvider) -> Response {
    answer(roots, config, project_id, project_name, request, Some(provider))
}

fn answer(roots: &StorageRoots, config: &RemoteConfig, project_id: &str, project_name: &str, request: &Request, provider: Option<&dyn ProjectsProvider>) -> Response {
    let mut response = route(roots, config, project_id, project_name, request, provider);
    if request.method == "HEAD" {
        // Same status as GET, never a body.
        response.body = Value::Null;
    }
    response
}

fn route(roots: &StorageRoots, config: &RemoteConfig, project_id: &str, project_name: &str, request: &Request, provider: Option<&dyn ProjectsProvider>) -> Response {
    // A stranger's address and a bad token are refused identically, so a
    // caller learns nothing about which check failed.
    if !peer_allowed(request.peer) || !token_matches(&config.token, request.authorization) {
        return error(401, "a valid bearer token is required");
    }
    if !matches!(request.method, "GET" | "HEAD") {
        return error(405, "this view is read-only");
    }
    let path = request.path;
    if path == "/api/projects" {
        return match provider {
            Some(provider) => match provider.projects() {
                Ok(projects) => reply(200, json!({ "projects": projects })),
                Err(why) => error(500, &why),
            },
            None => error(404, "not found"),
        };
    }
    if path == "/api/settings" {
        return match provider.map(|provider| provider.settings()) {
            Some(Ok(Some(settings))) => reply(200, json!({ "settings": settings })),
            Some(Err(why)) => error(500, &why),
            _ => error(404, "not found"),
        };
    }
    if let Some(rest) = path.strip_prefix("/api/projects/") {
        if let Some(id) = rest.strip_suffix("/features").filter(|id| !id.is_empty() && !id.contains('/')) {
            return match provider.map(|provider| provider.features(id)) {
                Some(Ok(Some(features))) => reply(200, json!({ "features": features })),
                // The reason can name a path on this machine, so the caller is only told it failed.
                Some(Err(_)) => error(500, "the feature ledger could not be read"),
                _ => error(404, "not found"),
            };
        }
        if let Some(id) = rest.strip_suffix("/runs").filter(|id| !id.is_empty() && !id.contains('/')) {
            return match provider.map(|provider| provider.runs(id)) {
                Some(Ok(Some(runs))) => reply(200, json!({ "runs": runs })),
                Some(Err(_)) => error(500, "the runs could not be read"),
                _ => error(404, "not found"),
            };
        }
        let parts: Vec<&str> = rest.split('/').collect();
        let logs = match parts.as_slice() {
            [id, "runs", run, "logs"] => Some((*id, *run, None, None)),
            [id, "runs", run, "logs", file] => Some((*id, *run, Some(*file), None)),
            [id, "runs", run, "logs", file, "after", cursor] => Some((*id, *run, Some(*file), Some(*cursor))),
            _ => None,
        };
        if let Some((id, run, file, after)) = logs.filter(|(id, run, file, after)| !id.is_empty() && !run.is_empty() && file.map_or(true, |file| !file.is_empty()) && after.map_or(true, |cursor| !cursor.is_empty())) {
            return match provider.map(|provider| provider.run_logs(id, run, file, after)) {
                Some(Ok(Some(logs))) => reply(200, logs),
                Some(Err(_)) => error(500, "the logs could not be read"),
                _ => error(404, "not found"),
            };
        }
        let timeline = match parts.as_slice() {
            [id, "runs", run] => Some((*id, *run, None)),
            [id, "runs", run, "after", cursor] => Some((*id, *run, Some(*cursor))),
            _ => None,
        };
        if let Some((id, run, after)) = timeline.filter(|(id, run, after)| !id.is_empty() && !run.is_empty() && after.map_or(true, |cursor| !cursor.is_empty())) {
            return match provider.map(|provider| provider.run_timeline(id, run, after)) {
                Some(Ok(Some(timeline))) => reply(200, timeline),
                Some(Err(_)) => error(500, "the run could not be read"),
                _ => error(404, "not found"),
            };
        }
        return match (provider, project_path(rest)) {
            (Some(provider), Some((id, run))) => match provider.review_scope(id) {
                Ok(Some(scope)) => match run {
                    None => list(roots, &scope.review_key, &scope.name, true),
                    Some(run_id) => one(roots, run_id, Some(&scope.review_key)),
                },
                Ok(None) => error(404, "not found"),
                Err(why) => error(500, &why),
            },
            _ => error(404, "not found"),
        };
    }
    if path == "/api/reviews" {
        return list(roots, project_id, project_name, false);
    }
    match path.strip_prefix("/api/reviews/") {
        Some(run_id) if !run_id.contains('/') => one(roots, run_id, None),
        _ => error(404, "not found"),
    }
}

/// `{id}/reviews` gives (id, None); `{id}/reviews/{run}` gives (id, Some(run)).
fn project_path(rest: &str) -> Option<(&str, Option<&str>)> {
    let (id, tail) = rest.split_once('/')?;
    if id.is_empty() {
        return None;
    }
    match tail.strip_prefix("reviews")? {
        "" => Some((id, None)),
        run => run.strip_prefix('/').filter(|run| !run.contains('/')).map(|run| (id, Some(run))),
    }
}

/// The newest stored lifecycle of `run_id` for the commit its report is about.
struct Facts {
    project: String,
    head: String,
    observed: Option<String>,
    link: Option<String>,
    stale: bool,
    /// `current`, `stale` or `unverified`, decided here and nowhere else.
    freshness: &'static str,
    /// The head the provider showed: the reviewed commit when current, the other one when stale.
    provider_head: Option<String>,
    checked_at: Option<String>,
}

/// Seconds since the epoch as plain UTC text, `2026-11-09T10:00:00Z`.
fn utc_text(seconds: i64) -> String {
    let (days, rem) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn facts(roots: &StorageRoots, run_id: &str) -> Option<Facts> {
    read_facts(roots, run_id).ok().flatten()
}

/// One read of the newest report record fixes the project, the commit and the lifecycle facts together.
/// A missing index or run is `None`; an index that cannot be read is an error, never "not found".
fn read_facts(roots: &StorageRoots, run_id: &str) -> Result<Option<Facts>, String> {
    // Only a genuinely absent index is "none"; one that cannot be looked at is an error.
    if !roots.database().try_exists().map_err(|err| format!("cannot look for the review index: {err}"))? {
        return Ok(None);
    }
    let store = Store::open_read_only(&roots.database()).map_err(|err| format!("cannot open the review index: {err}"))?;
    let records = store.reviews_for_run(run_id).map_err(|err| format!("cannot read the review index: {err}"))?;
    let Some(record) = records.into_iter().last() else { return Ok(None) };
    let (project, head) = (record.project, record.head_sha);
    // A lifecycle that cannot be read is unknown, never "fresh": it counts as stale and offers no link.
    let unknown = |project, head| Facts { project, head, observed: None, link: None, stale: true, freshness: "unverified", provider_head: None, checked_at: None };
    let Ok(all) = store.lifecycles_for_run(run_id) else { return Ok(Some(unknown(project, head))) };
    let Some(lifecycle) = all.into_iter().rev().find(|l| l.reported_head_sha == head) else {
        // Never looked at the provider: not current, but not known to be out of date either.
        return Ok(Some(Facts { stale: false, ..unknown(project, head) }));
    };
    // A look whose time cannot be read is not evidence either.
    let checked = store.lifecycle_checked_at(run_id, &head);
    let checked_at = checked.as_ref().ok().copied().flatten().map(utc_text);
    // The newest look decides: a mismatch stays stale whatever came after; a failed look is unverified.
    let freshness = if lifecycle.stale { "stale" } else if lifecycle.last_error.is_some() || !lifecycle.observed || checked_at.is_none() { "unverified" } else { "current" };
    let link = safe_link(&lifecycle.url).filter(|_| freshness == "current");
    let provider_head = match freshness {
        "current" => Some(head.clone()),
        "stale" => lifecycle.observed_head_sha.clone(),
        _ => None,
    };
    Ok(Some(Facts { observed: lifecycle.observed_head_sha, link, stale: lifecycle.stale, freshness, provider_head, checked_at, project, head }))
}

fn list(roots: &StorageRoots, project_id: &str, project_name: &str, only_this_project: bool) -> Response {
    let rows = if only_this_project { review_inbox::inbox_of_project(roots, project_id, project_name) } else { review_inbox::inbox(roots, project_id, project_name) };
    let rows = match rows {
        Ok(rows) => rows,
        Err(why) => return error(500, &why),
    };
    let reviews: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            let f = facts(roots, &row.run_id);
            let freshness = f.as_ref().map_or("unverified", |f| f.freshness);
            let link = f.and_then(|f| f.link);
            let link_allowed = link.is_some();
            json!({
                "generated_at": row.generated_at,
                "run_id": row.run_id, "feature": row.feature, "project": row.project, "current": row.current,
                "status": row.status, "risk": row.risk, "stale": row.stale, "link": link,
                "freshness": freshness, "link_allowed": link_allowed,
            })
        })
        .collect();
    reply(200, json!({ "reviews": reviews }))
}

fn one(roots: &StorageRoots, run_id: &str, only_project: Option<&str>) -> Response {
    if check_run_id(run_id).is_err() {
        return error(400, "invalid run id");
    }
    // The project and the commit come from the same record, so the check and the report cannot disagree.
    let f = match read_facts(roots, run_id) {
        Ok(Some(f)) if only_project.is_none_or(|owner| f.project == owner) => f,
        Ok(_) => return error(404, "no review report for this run"),
        Err(why) if only_project.is_some() => return error(500, &why),
        Err(_) => return error(404, "no review report for this run"),
    };
    let InboxDetail { summary, risks, files, tests, rounds, generated_at } = match review_inbox::detail_at(roots, run_id, &f.head) {
        Ok(detail) => detail,
        Err(_) => return error(404, "no review report for this run"),
    };
    reply(
        200,
        json!({
            "generated_at": generated_at,
            "run_id": run_id, "reported_head_sha": f.head, "observed_head_sha": f.observed, "stale": f.stale, "link": f.link,
            "freshness": f.freshness, "link_allowed": f.link.is_some(), "reviewed_head_sha": f.head, "provider_head_sha": f.provider_head, "checked_at": f.checked_at,
            "summary": summary, "risks": risks, "files": files, "tests": tests, "rounds": rounds,
        }),
    )
}
