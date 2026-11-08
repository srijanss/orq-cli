//! The read-only HTTP face of the remote review inbox.
//!
//! Every request goes through `orq_core::remote_inbox::handle`, which checks the
//! peer, the bearer token and the method and can only read stored reports. This
//! crate adds nothing but the transport: it never reads a request body, never
//! routes by itself and never answers anything the handler did not.

pub mod projects;

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;

use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use orq_core::remote_inbox::{self, ProjectsProvider, RemoteConfig};
use orq_core::storage::StorageRoots;

/// How long a stop waits for requests in flight before cutting connections.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Everything a request needs besides the request itself.
pub struct ServerState {
    pub roots: StorageRoots,
    pub config: RemoteConfig,
    pub project_id: String,
    pub project_name: String,
}

/// The request state plus the provider, when the server was started with one.
struct Shared {
    state: Arc<ServerState>,
    projects: Option<Arc<dyn ProjectsProvider + Send + Sync>>,
    /// Passkey sign-in, when the server was started with a passkey host.
    auth: Option<Arc<orq_core::passkey_routes::PasskeyAuth>>,
}

/// A running server; dropping it stops it.
pub struct Server {
    addr: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    /// Serves on `listener` until the returned value is dropped.
    pub fn start(listener: TcpListener, state: Arc<ServerState>) -> std::io::Result<Server> {
        Server::serve(listener, Shared { state, projects: None, auth: None })
    }

    /// Like `start`, and also answers `/api/projects` from `projects`.
    pub fn start_with_projects(listener: TcpListener, state: Arc<ServerState>, projects: Arc<dyn ProjectsProvider + Send + Sync>) -> std::io::Result<Server> {
        Server::serve(listener, Shared { state, projects: Some(projects), auth: None })
    }

    /// Like `start_with_projects`, and also serves the `/auth/*` passkey routes: a live session cookie
    /// opens `/api`, and once a passkey exists the bearer token no longer does from anywhere but this
    /// machine's own direct loopback.
    pub fn start_with_auth(listener: TcpListener, state: Arc<ServerState>, projects: Arc<dyn ProjectsProvider + Send + Sync>, auth: Arc<orq_core::passkey_routes::PasskeyAuth>) -> std::io::Result<Server> {
        Server::serve(listener, Shared { state, projects: Some(projects), auth: Some(auth) })
    }

    fn serve(listener: TcpListener, state: Shared) -> std::io::Result<Server> {
        let state = Arc::new(state);
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            runtime.block_on(async move {
                let Ok(listener) = tokio::net::TcpListener::from_std(listener) else { return };
                let app = Router::new().fallback(answer).with_state(state);
                let (drained, drain) = tokio::sync::oneshot::channel::<()>();
                let serving = tokio::spawn(async move {
                    let _ = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
                        .with_graceful_shutdown(async move {
                            let _ = drain.await;
                        })
                        .await;
                });
                let _ = stopped.await;
                let _ = drained.send(());
                // Connections that are still open after the grace period are cut.
                let _ = tokio::time::timeout(SHUTDOWN_GRACE, serving).await;
            });
            runtime.shutdown_timeout(SHUTDOWN_GRACE);
        });
        Ok(Server { addr, stop: Some(stop), thread: Some(thread) })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn answer(State(shared): State<Arc<Shared>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, method: Method, uri: Uri, headers: HeaderMap, body: axum::body::Body) -> Response {
    let authorization = headers.get(header::AUTHORIZATION).and_then(|value| value.to_str().ok()).map(str::to_string);
    let cookie = headers.get(header::COOKIE).and_then(|value| value.to_str().ok()).map(str::to_string);
    let path = uri.path().to_string();
    let method = method.as_str().to_string();

    // The page holds nothing private, so it needs no token; it still needs an
    // allowed peer. Anything else about the request goes to the handler below.
    if let Some(asset) = page_asset(peer.ip(), &method, &path) {
        return asset_response(asset, method == "HEAD");
    }

    if let Some(auth) = shared.auth.clone() {
        if path.starts_with("/auth/") {
            let Ok(bytes) = axum::body::to_bytes(body, orq_core::passkey_routes::MAX_AUTH_BODY).await else {
                return json_reply(413, b"{\"error\":\"too large\"}".to_vec());
            };
            let (shared, path, method) = (shared.clone(), path.clone(), method.clone());
            let result = tokio::task::spawn_blocking(move || {
                let request = orq_core::passkey_routes::AuthRequest { method: &method, path: &path, peer: peer.ip(), authorization: authorization.as_deref(), cookie: cookie.as_deref(), body: &bytes };
                auth.handle(&shared.state.roots, &shared.state.config, &request)
            })
            .await;
            let Ok(Some(reply)) = result else { return json_reply(404, b"{\"error\":\"not found\"}".to_vec()) };
            let mut response = json_reply(reply.status, reply.body.to_string().into_bytes());
            if let Some(cookie) = reply.set_cookie.and_then(|value| HeaderValue::from_str(&value).ok()) {
                response.headers_mut().insert(header::SET_COOKIE, cookie);
            }
            return response;
        }
    }

    if method == "GET" || method == "HEAD" {
        let last_event_id = headers.get("last-event-id").and_then(|value| value.to_str().ok()).map(str::to_string);
        if let Some((base, cursor)) = event_stream_target(&path, last_event_id.as_deref()) {
            let credentials = Credentials { forwarded: is_forwarded(&headers), authorization, cookie };
            return follow(shared, peer, credentials, base, cursor, method == "HEAD").await;
        }
    }
    let authorization = effective_authorization(&shared, peer.ip(), is_forwarded(&headers), authorization, cookie).await;

    // Reading storage is blocking work.
    let result = tokio::task::spawn_blocking(move || reply_to(&shared, peer.ip(), &method, &path, authorization.as_deref())).await;
    let Ok(reply) = result else { return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
    json_reply(reply.0, reply.1)
}

/// What a request presented, kept for as long as a stream stays open so each look can be judged afresh.
struct Credentials {
    forwarded: bool,
    authorization: Option<String>,
    cookie: Option<String>,
}

/// The authorization the handler should see. A live session stands in for the token. Otherwise, once
/// a passkey exists, the token is honoured only on a direct loopback connection (`tailscale serve`
/// also connects from loopback, but adds forwarding headers). Whatever cannot be established, such as
/// an unreadable passkey store, refuses the token rather than allowing it.
async fn effective_authorization(shared: &Arc<Shared>, peer: std::net::IpAddr, forwarded: bool, authorization: Option<String>, cookie: Option<String>) -> Option<String> {
    let Some(auth) = shared.auth.as_ref() else { return authorization };
    if auth.session_live(cookie.as_deref()) {
        return Some(format!("Bearer {}", shared.state.config.token));
    }
    // Only the right token could be accepted, so nothing else costs a look at the store.
    if !orq_core::remote_inbox::token_matches(&shared.state.config.token, authorization.as_deref()) || (peer.is_loopback() && !forwarded) {
        return authorization;
    }
    let roots = shared.state.roots.clone();
    let empty = tokio::task::spawn_blocking(move || orq_core::passkey_store::PasskeyStore::open(&roots).list().is_ok_and(|keys| keys.is_empty())).await.unwrap_or(false);
    if empty { authorization } else { None }
}

/// Whether a proxy put its marks on the request: `tailscale serve` adds `Tailscale-*` and forwarding headers.
fn is_forwarded(headers: &HeaderMap) -> bool {
    headers.keys().any(|name| {
        let name = name.as_str();
        name == "x-forwarded-for" || name == "x-forwarded-host" || name == "x-real-ip" || name == "forwarded" || name.starts_with("tailscale-")
    })
}

/// The handler's status and body as a JSON response that is never cached.
fn json_reply(code: u16, body: Vec<u8>) -> Response {
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response = (status, body).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}

/// What the handler answers for one request: its status and body.
fn reply_to(shared: &Shared, peer: std::net::IpAddr, method: &str, path: &str, authorization: Option<&str>) -> (u16, Vec<u8>) {
    let request = remote_inbox::Request { method, path, peer, authorization };
    let state = &shared.state;
    let reply = match &shared.projects {
        Some(projects) => remote_inbox::handle_with(&state.roots, &state.config, &state.project_id, &state.project_name, &request, projects.as_ref()),
        None => remote_inbox::handle(&state.roots, &state.config, &state.project_id, &state.project_name, &request),
    };
    if reply.status == 401 {
        log_refusal(peer, request.method, request.path);
    }
    // Serializing a large report is work too, so it stays off the async workers.
    (reply.status, reply.body_bytes())
}

/// How often a followed run is looked at again, and how long a quiet stream goes without a line.
const FOLLOW_POLL: std::time::Duration = std::time::Duration::from_secs(1);
const FOLLOW_KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(15);

/// `/api/projects/{id}/runs/{run}/events[/after/{event}]`: the timeline path to read, and the event to
/// resume after (the address wins over `Last-Event-ID`). Nothing else is a stream.
fn event_stream_target(path: &str, last_event_id: Option<&str>) -> Option<(String, Option<String>)> {
    let plain = |segment: &str| !segment.is_empty() && segment != "." && segment != "..";
    let segments: Vec<&str> = path.strip_prefix('/')?.split('/').collect();
    let (project, run, cursor) = match segments.as_slice() {
        ["api", "projects", project, "runs", run, "events"] => (project, run, last_event_id),
        ["api", "projects", project, "runs", run, "events", "after", cursor] => (project, run, Some(*cursor)),
        _ => return None,
    };
    // The cursor becomes a path segment again, so only a plain one is accepted.
    let sound = |cursor: &str| plain(cursor) && !cursor.contains(['/', '?', '#', '\\']) && !cursor.chars().any(char::is_control);
    if !plain(project) || !plain(run) || !cursor.map_or(true, sound) {
        return None;
    }
    Some((format!("/api/projects/{project}/runs/{run}"), cursor.map(String::from)))
}

/// Answers a stream request: the handler's own refusal when it refuses, otherwise server-sent events,
/// each carrying the event id so a dropped connection resumes after the last one seen.
async fn follow(shared: Arc<Shared>, peer: SocketAddr, credentials: Credentials, base: String, cursor: Option<String>, head: bool) -> Response {
    struct Follow {
        shared: Arc<Shared>,
        peer: SocketAddr,
        credentials: Credentials,
        base: String,
        cursor: Option<String>,
        queue: std::collections::VecDeque<String>,
        quiet: std::time::Duration,
        hurry: bool,
    }
    // The path of the timeline page after the last event sent.
    fn page_path(state: &Follow) -> String {
        match &state.cursor {
            Some(cursor) => format!("{}/after/{cursor}", state.base),
            None => state.base.clone(),
        }
    }
    // Queues the page's events as stream lines; says whether there were any.
    fn absorb(state: &mut Follow, page: &serde_json::Value) -> bool {
        let events = page["events"].as_array().cloned().unwrap_or_default();
        for event in &events {
            let Some(id) = event["event_id"].as_str().filter(|id| !id.chars().any(char::is_control)) else { continue };
            state.queue.push_back(format!("id: {id}\nevent: event\ndata: {event}\n\n"));
            state.cursor = Some(id.to_string());
        }
        state.hurry = !page["next_cursor"].is_null();
        !events.is_empty()
    }
    // Reads what came after the last event; false when the run can no longer be read.
    async fn fetch(state: &mut Follow) -> bool {
        let path = page_path(state);
        let shared = state.shared.clone();
        let peer = state.peer.ip();
        // Judged on every look, so a logout, an expired session or a newly enrolled passkey ends the stream.
        let authorization = effective_authorization(&shared, peer, state.credentials.forwarded, state.credentials.authorization.clone(), state.credentials.cookie.clone()).await;
        let Ok((200, body)) = tokio::task::spawn_blocking(move || reply_to(&shared, peer, "GET", &path, authorization.as_deref())).await else { return false };
        let Ok(page) = serde_json::from_slice::<serde_json::Value>(&body) else { return false };
        if absorb(state, &page) {
            state.quiet = std::time::Duration::ZERO;
        } else {
            state.quiet += FOLLOW_POLL;
            if state.quiet >= FOLLOW_KEEPALIVE {
                state.queue.push_back(": keepalive\n\n".to_string());
                state.quiet = std::time::Duration::ZERO;
            }
        }
        true
    }

    let mut state = Follow { shared, peer, credentials, base, cursor, queue: Default::default(), quiet: Default::default(), hurry: false };
    // The first look decides the answer: a refused or unknown request is the handler's plain reply.
    let first = {
        let (shared, ip, path) = (state.shared.clone(), peer.ip(), page_path(&state));
        let authorization = effective_authorization(&shared, ip, state.credentials.forwarded, state.credentials.authorization.clone(), state.credentials.cookie.clone()).await;
        tokio::task::spawn_blocking(move || reply_to(&shared, ip, "GET", &path, authorization.as_deref())).await
    };
    let Ok((code, body)) = first else { return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
    if code != 200 {
        return json_reply(code, body);
    }
    state.queue.push_back(": connected\n\n".to_string());
    if let Ok(page) = serde_json::from_slice::<serde_json::Value>(&body) {
        absorb(&mut state, &page);
    }
    let stream = futures_util::stream::unfold(state, |mut state| async move {
        loop {
            if let Some(chunk) = state.queue.pop_front() {
                return Some((Ok::<_, std::convert::Infallible>(chunk), state));
            }
            if !state.hurry {
                tokio::time::sleep(FOLLOW_POLL).await;
            }
            if !fetch(&mut state).await {
                return None;
            }
        }
    });
    // A HEAD gets the stream's headers and nothing to follow.
    let body = if head { axum::body::Body::empty() } else { axum::body::Body::from_stream(stream) };
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}

/// A file compiled into the binary and the exact path it is served at.
pub struct Asset {
    pub path: &'static str,
    pub content_type: &'static str,
    pub body: &'static str,
}

/// The compiled-in file to serve without a token, if there is one: only for
/// GET or HEAD, only from a peer allowed to connect, only on an exact path.
/// Everything else is left to the handler, which refuses it.
pub fn page_asset(peer: std::net::IpAddr, method: &str, path: &str) -> Option<&'static Asset> {
    if !matches!(method, "GET" | "HEAD") || !remote_inbox::peer_allowed(peer) {
        return None;
    }
    let path = if is_application_path(path) { "/" } else { path };
    ASSETS.iter().find(|asset| asset.path == path)
}

/// Places the page itself draws, so a reload or a shared address opens them. Only these exact
/// shapes count, with plain non-empty segments, so the API, dot segments, extra segments and
/// unknown sections keep their answers.
fn is_application_path(path: &str) -> bool {
    let plain = |segment: &str| !segment.is_empty() && segment != "." && segment != "..";
    let segments: Vec<&str> = path.strip_prefix('/').map(|rest| rest.split('/').collect()).unwrap_or_default();
    match segments.as_slice() {
        ["dashboard"] | ["settings"] | ["projects"] => true,
        ["projects", project] => plain(project),
        ["projects", project, section] => plain(project) && ["features", "orchestrations", "reviews"].contains(section),
        ["projects", project, "reviews", run] => plain(project) && plain(run),
        // A run, its next pages of events, and its logs (a file, then the page after a line).
        ["projects", project, "orchestrations", run] | ["projects", project, "orchestrations", run, "logs"] => plain(project) && plain(run),
        ["projects", project, "orchestrations", run, "after", cursor] | ["projects", project, "orchestrations", run, "logs", cursor] => plain(project) && plain(run) && plain(cursor),
        ["projects", project, "orchestrations", run, "logs", file, "after", cursor] => plain(project) && plain(run) && plain(file) && plain(cursor),
        ["reviews", run] => plain(run),
        _ => false,
    }
}

/// The whole set of files the page path can ever serve. No request path is
/// ever joined to a directory, so nothing outside this list can be read.
const ASSETS: [Asset; 3] = [
    Asset { path: "/", content_type: "text/html; charset=utf-8", body: include_str!("../web/index.html") },
    Asset { path: "/app.css", content_type: "text/css; charset=utf-8", body: include_str!("../web/app.css") },
    // Built from web-src by `node crates/orq-remote/web-src/build.mjs`; a test fails when it is stale.
    Asset { path: "/app.react.js", content_type: "text/javascript; charset=utf-8", body: include_str!("../web/app.react.js") },
];

/// Only same-origin scripts, styles and requests; no inline code, framing,
/// form posts or base tag.
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'none'; form-action 'none'; base-uri 'none'";

fn asset_response(asset: &Asset, head_only: bool) -> Response {
    let body = if head_only { String::new() } else { asset.body.to_string() };
    let mut response = (StatusCode::OK, body).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(asset.content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CONTENT_SECURITY_POLICY));
    response
}

/// Most refusal lines written per second; the rest are dropped so unauthenticated
/// traffic cannot turn into unlimited logging I/O.
const LOG_PER_SECOND: u32 = 10;

/// Logs the peer, method and a bounded, escaped path; never the Authorization
/// header. Runs on a blocking thread, so a slow stderr cannot stall the workers.
fn log_refusal(peer: std::net::IpAddr, method: &str, path: &str) {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    // The window start and its count change together under one lock.
    static WINDOW: Mutex<Option<(Instant, u32)>> = Mutex::new(None);
    let allowed = {
        let mut window = WINDOW.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Instant::now();
        match *window {
            Some((start, count)) if now.duration_since(start) < Duration::from_secs(1) => {
                *window = Some((start, count.saturating_add(1)));
                count < LOG_PER_SECOND
            }
            _ => {
                *window = Some((now, 1));
                true
            }
        }
    };
    if !allowed {
        return;
    }
    eprintln!("{}", refusal_line(peer, method, path));
}

/// One log line for a refused request: the peer, the method and a bounded,
/// escaped path, on a single line. It takes no header, so no credential can
/// reach it.
pub fn refusal_line(peer: std::net::IpAddr, method: &str, path: &str) -> String {
    let shown: String = path.chars().take(100).flat_map(char::escape_default).collect();
    let method: String = method.chars().take(16).flat_map(char::escape_default).collect();
    format!("orq-remote: refused {peer} {method} {shown}")
}
