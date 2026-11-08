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
        Server::serve(listener, Shared { state, projects: None })
    }

    /// Like `start`, and also answers `/api/projects` from `projects`.
    pub fn start_with_projects(listener: TcpListener, state: Arc<ServerState>, projects: Arc<dyn ProjectsProvider + Send + Sync>) -> std::io::Result<Server> {
        Server::serve(listener, Shared { state, projects: Some(projects) })
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

async fn answer(State(shared): State<Arc<Shared>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, method: Method, uri: Uri, headers: HeaderMap) -> Response {
    let authorization = headers.get(header::AUTHORIZATION).and_then(|value| value.to_str().ok()).map(str::to_string);
    let path = uri.path().to_string();
    let method = method.as_str().to_string();

    // The page holds nothing private, so it needs no token; it still needs an
    // allowed peer. Anything else about the request goes to the handler below.
    if let Some(asset) = page_asset(peer.ip(), &method, &path) {
        return asset_response(asset, method == "HEAD");
    }

    // Reading storage is blocking work.
    let result = tokio::task::spawn_blocking(move || {
        let request = remote_inbox::Request { method: &method, path: &path, peer: peer.ip(), authorization: authorization.as_deref() };
        let state = &shared.state;
        let reply = match &shared.projects {
            Some(projects) => remote_inbox::handle_with(&state.roots, &state.config, &state.project_id, &state.project_name, &request, projects.as_ref()),
            None => remote_inbox::handle(&state.roots, &state.config, &state.project_id, &state.project_name, &request),
        };
        if reply.status == 401 {
            log_refusal(peer.ip(), request.method, request.path);
        }
        // Serializing a large report is work too, so it stays off the async workers.
        let body = reply.body_bytes();
        (reply.status, body)
    })
    .await;
    let Ok(reply) = result else { return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
    let (code, body) = reply;

    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response = (status, body).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
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
