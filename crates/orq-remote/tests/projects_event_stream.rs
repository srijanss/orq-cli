//! `/api/projects/{id}/runs/{run}/events`: new events of a run over server-sent events, behind the
//! same token check, resuming after an event id given in the address or in `Last-Event-ID`.

mod web_deps;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orq_core::remote_inbox::{ProjectsProvider, RemoteConfig};
use orq_core::storage::StorageRoots;
use orq_remote::{Server, ServerState};

const TOKEN: &str = "test-token-value";

/// One run, "r" of project "a", whose events can grow while the server runs.
struct Growing(Arc<Mutex<Vec<String>>>);

impl ProjectsProvider for Growing {
    fn projects(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!([]))
    }

    fn run_timeline(&self, id: &str, run: &str, after: Option<&str>) -> Result<Option<serde_json::Value>, String> {
        if id != "a" || run != "r" {
            return Ok(None);
        }
        let ids = self.0.lock().unwrap().clone();
        let start = match after {
            None => 0,
            Some(cursor) => match ids.iter().position(|known| known == cursor) {
                Some(at) => at + 1,
                None => return Ok(None),
            },
        };
        let events: Vec<_> = ids[start..].iter().map(|e| serde_json::json!({"event_id": e, "kind": "stage.started", "stage": null, "attempt": 1, "occurred_at": "2026-11-02T10:00:00Z"})).collect();
        Ok(Some(serde_json::json!({"run": {"run_id": "r"}, "events": events, "next_cursor": null})))
    }
}

fn start(ids: &[&str]) -> (Server, Arc<Mutex<Vec<String>>>) {
    let base = web_deps::private_temp_dir("event-stream");
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let state = Arc::new(ServerState { roots, config: RemoteConfig { token: TOKEN.to_string() }, project_id: "p".into(), project_name: "p".into() });
    let events = Arc::new(Mutex::new(ids.iter().map(|s| s.to_string()).collect()));
    (Server::start_with_projects(TcpListener::bind("127.0.0.1:0").unwrap(), state, Arc::new(Growing(events.clone()))).unwrap(), events)
}

fn open(addr: SocketAddr, path: &str, extra: &str, token: Option<&str>) -> TcpStream {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    write!(stream, "GET {path} HTTP/1.1\r\nHost: x\r\n{auth}{extra}\r\n").unwrap();
    stream
}

/// Reads until `wanted` has appeared or ten seconds have passed; returns everything read.
fn read_until(stream: &mut TcpStream, wanted: &str) -> String {
    let (mut raw, started) = (Vec::new(), Instant::now());
    while started.elapsed() < Duration::from_secs(10) && !String::from_utf8_lossy(&raw).contains(wanted) {
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
            Err(_) => {}
        }
    }
    String::from_utf8_lossy(&raw).to_string()
}

const PATH: &str = "/api/projects/a/runs/r/events";

#[test]
fn the_stream_sends_the_events_so_far_and_then_each_new_one() {
    let (server, events) = start(&["e0", "e1"]);
    let mut stream = open(server.addr(), PATH, "", Some(TOKEN));
    let first = read_until(&mut stream, "id: e1");
    let head = first.split("\r\n\r\n").next().unwrap().to_lowercase();
    assert!(first.starts_with("HTTP/1.1 200"), "{first}");
    assert!(head.contains("content-type: text/event-stream") && head.contains("cache-control: no-store"), "{head}");
    assert!(first.contains("id: e0\nevent: event\ndata: {") && first.contains("\"kind\":\"stage.started\""), "{first}");
    assert!(first.find("id: e0").unwrap() < first.find("id: e1").unwrap(), "{first}");

    events.lock().unwrap().push("e2".into());
    let later = read_until(&mut stream, "id: e2");
    assert!(later.contains("id: e2") && !later.contains("id: e1"), "an event is sent once: {later}");
}

#[test]
fn a_dropped_connection_resumes_after_the_cursor_from_the_address_or_last_event_id() {
    let (server, _events) = start(&["e0", "e1", "e2"]);
    let mut by_address = open(server.addr(), &format!("{PATH}/after/e0"), "", Some(TOKEN));
    let got = read_until(&mut by_address, "id: e2");
    assert!(got.contains("id: e1") && got.contains("id: e2") && !got.contains("id: e0"), "{got}");

    let mut by_header = open(server.addr(), PATH, "Last-Event-ID: e1\r\n", Some(TOKEN));
    let got = read_until(&mut by_header, "id: e2");
    assert!(got.contains("id: e2") && !got.contains("id: e1") && !got.contains("id: e0"), "{got}");

    // An address cursor wins over the header, and a cursor that is not an event of the run is refused.
    let mut both = open(server.addr(), &format!("{PATH}/after/e1"), "Last-Event-ID: e0\r\n", Some(TOKEN));
    let got = read_until(&mut both, "id: e2");
    assert!(!got.contains("id: e1"), "{got}");
    let mut unknown = open(server.addr(), &format!("{PATH}/after/nope"), "", Some(TOKEN));
    assert!(read_until(&mut unknown, "\r\n\r\n").starts_with("HTTP/1.1 404"));
}

#[test]
fn the_stream_needs_the_token_and_only_reads() {
    let (server, _events) = start(&["e0"]);
    assert!(read_until(&mut open(server.addr(), PATH, "", None), "\r\n\r\n").starts_with("HTTP/1.1 401"));
    assert!(read_until(&mut open(server.addr(), PATH, "", Some("wrong")), "\r\n\r\n").starts_with("HTTP/1.1 401"));
    assert!(read_until(&mut open(server.addr(), "/api/projects/a/runs/nope/events", "", Some(TOKEN)), "\r\n\r\n").starts_with("HTTP/1.1 404"));
    let mut post = TcpStream::connect(server.addr()).unwrap();
    post.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    write!(post, "POST {PATH} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: 0\r\n\r\n").unwrap();
    assert!(read_until(&mut post, "\r\n\r\n").starts_with("HTTP/1.1 405"));
}
