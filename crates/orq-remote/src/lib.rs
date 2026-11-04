//! The read-only HTTP face of the remote review inbox.
//!
//! Every request goes through `orq_core::remote_inbox::handle`, which checks the
//! peer, the bearer token and the method and can only read stored reports. This
//! crate adds nothing but the transport: it never reads a request body, never
//! routes by itself and never answers anything the handler did not.

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;

use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use orq_core::remote_inbox::{self, RemoteConfig};
use orq_core::storage::StorageRoots;

/// Everything a request needs besides the request itself.
pub struct ServerState {
    pub roots: StorageRoots,
    pub config: RemoteConfig,
    pub project_id: String,
    pub project_name: String,
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
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_io().build()?;
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            runtime.block_on(async move {
                let Ok(listener) = tokio::net::TcpListener::from_std(listener) else { return };
                let app = Router::new().fallback(answer).with_state(state);
                let _ = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
                    .with_graceful_shutdown(async move {
                        let _ = stopped.await;
                    })
                    .await;
            });
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

async fn answer(State(state): State<Arc<ServerState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, method: Method, uri: Uri, headers: HeaderMap) -> Response {
    let authorization = headers.get(header::AUTHORIZATION).and_then(|value| value.to_str().ok()).map(str::to_string);
    let path = uri.path().to_string();
    let method = method.as_str().to_string();
    // Reading storage is blocking work.
    let result = tokio::task::spawn_blocking(move || {
        let request = remote_inbox::Request { method: &method, path: &path, peer: peer.ip(), authorization: authorization.as_deref() };
        remote_inbox::handle(&state.roots, &state.config, &state.project_id, &state.project_name, &request)
    })
    .await;
    let Ok(reply) = result else { return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
    let body = reply.body_bytes();
    let status = StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response = (status, body).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}
