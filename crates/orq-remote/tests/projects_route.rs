//! `/api/projects` over HTTP: answered from the provider the server was started with, only after the
//! token check, and absent when the server has no provider.

mod web_deps;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;

use orq_core::remote_inbox::{ProjectsProvider, RemoteConfig};
use orq_core::storage::StorageRoots;
use orq_remote::{Server, ServerState};

const TOKEN: &str = "test-token-value";

struct Fixed;
impl ProjectsProvider for Fixed {
    fn projects(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!([{"id": "a", "name": "a", "available": true}]))
    }
}

fn state() -> Arc<ServerState> {
    let base = web_deps::private_temp_dir("projects-route");
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    Arc::new(ServerState { roots, config: RemoteConfig { token: TOKEN.to_string() }, project_id: "p".into(), project_name: "p".into() })
}

fn get(addr: SocketAddr, token: Option<&str>) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    write!(stream, "GET /api/projects HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{auth}\r\n").unwrap();
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    let raw = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    (head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0), body.to_string())
}

#[test]
fn a_server_with_a_provider_answers_projects_after_the_token_check() {
    let server = Server::start_with_projects(TcpListener::bind("127.0.0.1:0").unwrap(), state(), Arc::new(Fixed)).unwrap();
    let (status, body) = get(server.addr(), Some(TOKEN));
    assert_eq!(status, 200, "{body}");
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["projects"][0]["id"], "a");
    assert_eq!(get(server.addr(), None).0, 401);
}

#[test]
fn a_server_without_a_provider_has_no_projects_route() {
    let server = Server::start(TcpListener::bind("127.0.0.1:0").unwrap(), state()).unwrap();
    assert_eq!(get(server.addr(), Some(TOKEN)).0, 404);
}
