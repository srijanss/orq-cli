//! `/api/projects/{id}/runs/{run}/events`: new events of a run over server-sent events, behind the
//! same token check, resuming after an event id given in the address or in `Last-Event-ID`.

mod web_deps;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orq_core::remote_inbox::{ProjectsProvider, RemoteConfig};
use orq_core::storage::StorageRoots;
use orq_remote::{Server, ServerState};

const TOKEN: &str = "test-token-value";

/// One run, "r" of project "a", whose events can grow while the server runs.
struct Growing {
    events: Arc<Mutex<Vec<String>>>,
    /// Events per page; zero means everything after the cursor in one page.
    page: usize,
    /// Once set the run can no longer be read.
    gone: Arc<AtomicBool>,
}

impl ProjectsProvider for Growing {
    fn projects(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!([]))
    }

    fn run_timeline(&self, id: &str, run: &str, after: Option<&str>) -> Result<Option<serde_json::Value>, String> {
        if id != "a" || run != "r" || self.gone.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let ids = self.events.lock().unwrap().clone();
        let start = match after {
            None => 0,
            Some(cursor) => match ids.iter().position(|known| known == cursor) {
                Some(at) => at + 1,
                None => return Ok(None),
            },
        };
        let rest = &ids[start..];
        let shown = if self.page == 0 { rest.len() } else { rest.len().min(self.page) };
        let events: Vec<_> = rest[..shown].iter().map(|e| serde_json::json!({"event_id": e, "kind": "stage.started", "stage": null, "attempt": 1, "occurred_at": "2026-11-02T10:00:00Z"})).collect();
        let next = if shown < rest.len() { serde_json::json!(rest[shown - 1]) } else { serde_json::Value::Null };
        Ok(Some(serde_json::json!({"run": {"run_id": "r"}, "events": events, "next_cursor": next})))
    }
}

fn start(ids: &[&str]) -> (Server, Arc<Mutex<Vec<String>>>) {
    let (server, events, _gone) = start_with(ids, 0);
    (server, events)
}

fn start_with(ids: &[&str], page: usize) -> (Server, Arc<Mutex<Vec<String>>>, Arc<AtomicBool>) {
    let base = web_deps::private_temp_dir("event-stream");
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let state = Arc::new(ServerState { roots, config: RemoteConfig { token: TOKEN.to_string() }, project_id: "p".into(), project_name: "p".into() });
    let events = Arc::new(Mutex::new(ids.iter().map(|s| s.to_string()).collect()));
    let gone = Arc::new(AtomicBool::new(false));
    let provider = Growing { events: events.clone(), page, gone: gone.clone() };
    (Server::start_with_projects(TcpListener::bind("127.0.0.1:0").unwrap(), state, Arc::new(provider)).unwrap(), events, gone)
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
    read_for(stream, wanted, 10)
}

fn read_for(stream: &mut TcpStream, wanted: &str, seconds: u64) -> String {
    let (mut raw, started) = (Vec::new(), Instant::now());
    while started.elapsed() < Duration::from_secs(seconds) && !String::from_utf8_lossy(&raw).contains(wanted) {
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

#[test]
fn head_answers_like_get_with_the_stream_headers_and_no_body() {
    let (server, _events) = start(&["e0"]);
    let head = |path: &str, token: Option<&str>| {
        let mut stream = TcpStream::connect(server.addr()).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
        write!(stream, "HEAD {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{auth}\r\n").unwrap();
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        String::from_utf8_lossy(&raw).to_string()
    };
    let ok = head(PATH, Some(TOKEN));
    assert!(ok.starts_with("HTTP/1.1 200") && ok.to_lowercase().contains("content-type: text/event-stream"), "{ok}");
    assert!(ok.ends_with("\r\n\r\n"), "no body: {ok}");
    assert!(head(PATH, None).starts_with("HTTP/1.1 401"));
    assert!(head("/api/projects/a/runs/nope/events", Some(TOKEN)).starts_with("HTTP/1.1 404"));
}

/// Reads until the response body ends (the closing chunk, or the connection closing); false if it is
/// still going after `seconds`.
fn closed_within(stream: &mut TcpStream, seconds: u64) -> bool {
    let (mut raw, started) = (Vec::new(), Instant::now());
    while started.elapsed() < Duration::from_secs(seconds) {
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) => return true,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
            Err(_) => {}
        }
        if raw.ends_with(b"0\r\n\r\n") {
            return true;
        }
    }
    false
}

#[test]
fn a_run_with_more_events_than_a_page_holds_arrives_in_order_and_once() {
    let (server, events, _gone) = start_with(&["e0", "e1", "e2", "e3", "e4"], 2);
    let mut stream = open(server.addr(), PATH, "", Some(TOKEN));
    let got = read_until(&mut stream, "id: e4");
    let at: Vec<_> = ["e0", "e1", "e2", "e3", "e4"].iter().map(|id| got.matches(&format!("id: {id}\n")).count()).collect();
    assert_eq!(at, vec![1; 5], "{got}");
    let order: Vec<_> = ["e0", "e1", "e2", "e3", "e4"].iter().map(|id| got.find(&format!("id: {id}\n")).unwrap()).collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{got}");

    events.lock().unwrap().push("e5".into());
    let later = read_until(&mut stream, "id: e5");
    assert!(later.contains("id: e5") && !later.contains("id: e4"), "{later}");
}

#[test]
fn the_stream_closes_when_the_run_can_no_longer_be_read() {
    let (server, _events, gone) = start_with(&["e0"], 0);
    let mut stream = open(server.addr(), PATH, "", Some(TOKEN));
    read_until(&mut stream, "id: e0");
    gone.store(true, Ordering::SeqCst);
    assert!(closed_within(&mut stream, 10), "the stream stays open after the run is gone");
}

#[test]
fn a_quiet_stream_sends_a_keepalive_line_and_then_the_next_event() {
    let (server, events, _gone) = start_with(&["e0"], 0);
    let mut stream = open(server.addr(), PATH, "", Some(TOKEN));
    read_until(&mut stream, "id: e0");
    let quiet = read_for(&mut stream, ": keepalive", 25);
    assert!(quiet.contains(": keepalive\n\n") && !quiet.contains("id: e0"), "{quiet}");
    events.lock().unwrap().push("e1".into());
    assert!(read_until(&mut stream, "id: e1").contains("id: e1"));
}
