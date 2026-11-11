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

fn start() -> (Server, Arc<Mutex<Vec<String>>>) {
    let base = web_deps::private_temp_dir("action-body");
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let state = Arc::new(ServerState { roots, config: RemoteConfig { token: TOKEN.to_string() }, project_id: "p".into(), project_name: "p".into() });
    let asked = Arc::new(Mutex::new(Vec::new()));
    let server = Server::start_with_projects(TcpListener::bind("127.0.0.1:0").unwrap(), state, Arc::new(Fake { asked: asked.clone() })).unwrap();
    (server, asked)
}

/// Sends the head with `extra` headers and `sent`, keeps the connection open, and reads the status line.
fn send_partly(addr: SocketAddr, method: &str, extra: &str, sent: &[u8]) -> String {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(20))).unwrap();
    let head = format!("{method} /api/projects/p-1/runs/r-1/pause HTTP/1.1\r\nHost: x\r\nConnection: close\r\nAuthorization: Bearer {TOKEN}\r\n{extra}\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(sent).unwrap();
    let mut bytes = [0u8; 64];
    let n = stream.read(&mut bytes).unwrap_or(0);
    String::from_utf8_lossy(&bytes[..n]).lines().next().unwrap_or("").to_string()
}

#[test]
fn an_action_body_reaches_the_handler_and_a_large_one_is_refused() {
    let (server, asked) = start();
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

#[test]
fn a_body_that_never_finishes_arriving_is_refused_after_a_short_deadline() {
    let (server, asked) = start();
    let begun = std::time::Instant::now();
    let status = send_partly(server.addr(), "POST", "Content-Type: application/json\r\nContent-Length: 100\r\n", b"{");
    assert!(status.contains(" 400 "), "{status}");
    assert!(begun.elapsed() < std::time::Duration::from_secs(10), "answered after {:?}", begun.elapsed());
    assert!(asked.lock().unwrap().is_empty());
}

#[test]
fn a_broken_body_is_refused_as_a_bad_request_not_as_too_large() {
    let (server, asked) = start();
    let status = send_partly(server.addr(), "POST", "Content-Type: application/json\r\nTransfer-Encoding: chunked\r\n", b"zz\r\n{}\r\n0\r\n\r\n");
    assert!(status.contains(" 400 "), "{status}");
    assert!(asked.lock().unwrap().is_empty());
}

#[test]
fn only_a_post_has_its_body_read_so_other_methods_are_answered_without_waiting_for_one() {
    let (server, asked) = start();
    let begun = std::time::Instant::now();
    let status = send_partly(server.addr(), "DELETE", "Content-Type: application/json\r\nContent-Length: 100\r\n", b"{");
    assert!(status.starts_with("HTTP/1.1 4"), "{status}");
    assert!(begun.elapsed() < std::time::Duration::from_secs(2), "answered after {:?}", begun.elapsed());
    assert!(asked.lock().unwrap().is_empty());
}

#[test]
fn a_body_exactly_at_the_cap_is_read_whole_and_one_byte_over_or_chunked_over_is_too_large() {
    let (server, asked) = start();
    let pause = "/api/projects/p-1/runs/r-1/pause";

    let mut at_cap = vec![b' '; MAX_ACTION_BODY - 2];
    at_cap.extend_from_slice(b"{}");
    let (status, _) = post(server.addr(), pause, &at_cap);
    assert!(status.contains(" 202 "), "{status}");

    let mut over = vec![b' '; MAX_ACTION_BODY - 1];
    over.extend_from_slice(b"{}");
    let (status, _) = post(server.addr(), pause, &over);
    assert!(status.contains(" 413 "), "{status}");

    // Chunked carries no length up front: the cap still holds as the chunks arrive.
    let chunk = vec![b' '; MAX_ACTION_BODY / 2];
    let mut chunked = Vec::new();
    for _ in 0..3 {
        chunked.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        chunked.extend_from_slice(&chunk);
        chunked.extend_from_slice(b"\r\n");
    }
    chunked.extend_from_slice(b"2\r\n{}\r\n0\r\n\r\n");
    let status = send_partly(server.addr(), "POST", "Content-Type: application/json\r\nTransfer-Encoding: chunked\r\n", &chunked);
    assert!(status.contains(" 413 "), "{status}");

    assert_eq!(asked.lock().unwrap().len(), 1, "only the body at the cap reached the provider");
}
