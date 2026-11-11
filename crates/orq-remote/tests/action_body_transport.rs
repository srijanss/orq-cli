//! Over the socket an action's JSON body reaches the handler: `{}` acts, any other body is refused
//! before the action runs, and a body over the cap is refused as too large, never cut short.

mod web_deps;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use orq_core::remote_inbox::{ProjectsProvider, RemoteConfig, RunControl, RunControlOutcome, MAX_ACTION_BODY};
use orq_core::storage::StorageRoots;
use orq_remote::{Server, ServerState};

const TOKEN: &str = "action-body-transport-token-0123";

struct Fake {
    asked: Arc<Mutex<Vec<String>>>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!([]))
    }

    fn control_run(&self, _id: &str, run: &str, _control: RunControl) -> Result<RunControlOutcome, String> {
        self.asked.lock().unwrap().push(run.to_string());
        Ok(RunControlOutcome::Done(run.into()))
    }
}

fn post(addr: SocketAddr, path: &str, body: &[u8]) -> (String, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    let head = format!("POST {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len());
    stream.write_all(head.as_bytes()).unwrap();
    let _ = stream.write_all(body);
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    let text = String::from_utf8_lossy(&bytes).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (head.lines().next().unwrap_or("").to_string(), body.to_string())
}

#[test]
fn an_action_body_reaches_the_handler_and_a_large_one_is_refused() {
    let base = web_deps::private_temp_dir("action-body");
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let state = Arc::new(ServerState { roots, config: RemoteConfig { token: TOKEN.to_string() }, project_id: "p".into(), project_name: "p".into() });
    let asked = Arc::new(Mutex::new(Vec::new()));
    let server = Server::start_with_projects(TcpListener::bind("127.0.0.1:0").unwrap(), state, Arc::new(Fake { asked: asked.clone() })).unwrap();
    let pause = "/api/projects/p-1/runs/r-1/pause";

    let (status, _) = post(server.addr(), pause, b"{}");
    assert!(status.contains(" 202 "), "{status}");
    let (status, body) = post(server.addr(), pause, b"{\"run\":\"other\"}");
    assert!(status.contains(" 400 "), "{status}");
    assert!(body.contains("the request body is not one this action takes"), "{body}");

    // Valid JSON with the object only after the cap: refused whole, not read up to the cap and acted on.
    let mut large = vec![b' '; MAX_ACTION_BODY];
    large.extend_from_slice(b"{}");
    let (status, body) = post(server.addr(), pause, &large);
    assert!(status.contains(" 413 "), "{status}");
    assert!(body.contains("the request body is too large"), "{body}");
    assert_eq!(asked.lock().unwrap().len(), 1, "only the empty object reached the provider");
}
