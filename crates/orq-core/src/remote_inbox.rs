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

fn token_matches(expected: &str, header: Option<&str>) -> bool {
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
    if let Some(rest) = path.strip_prefix("/api/projects/") {
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
    let Ok(all) = store.lifecycles_for_run(run_id) else {
        return Ok(Some(Facts { project, head, observed: None, link: None, stale: true }));
    };
    let lifecycle = all.into_iter().rev().find(|l| l.reported_head_sha == head);
    Ok(Some(Facts {
        observed: lifecycle.as_ref().and_then(|l| l.observed_head_sha.clone()),
        link: lifecycle.as_ref().and_then(|l| safe_link(&l.url)),
        stale: lifecycle.as_ref().is_some_and(|l| l.stale),
        project,
        head,
    }))
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
            let link = facts(roots, &row.run_id).and_then(|f| f.link);
            json!({
                "run_id": row.run_id, "feature": row.feature, "project": row.project, "current": row.current,
                "status": row.status, "risk": row.risk, "stale": row.stale, "link": link,
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
    let InboxDetail { summary, risks, files, tests, rounds } = match review_inbox::detail_at(roots, run_id, &f.head) {
        Ok(detail) => detail,
        Err(_) => return error(404, "no review report for this run"),
    };
    reply(
        200,
        json!({
            "run_id": run_id, "reported_head_sha": f.head, "observed_head_sha": f.observed, "stale": f.stale, "link": f.link,
            "summary": summary, "risks": risks, "files": files, "tests": tests, "rounds": rounds,
        }),
    )
}
