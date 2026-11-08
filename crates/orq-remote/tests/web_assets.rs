use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;

use orq_core::remote_inbox::RemoteConfig;
use orq_core::storage::StorageRoots;
use orq_remote::{Server, ServerState};

const TOKEN: &str = "web-assets-test-token-0123456789ab";

fn start(tag: &str) -> Server {
    let base = std::env::temp_dir().join(format!("orq-remote-web-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let state = Arc::new(ServerState { roots, config: RemoteConfig { token: TOKEN.to_string() }, project_id: "p-0a1b2c3d".into(), project_name: "orq-cli".into() });
    Server::start(TcpListener::bind("127.0.0.1:0").unwrap(), state).unwrap()
}

struct Reply {
    status: u16,
    head: String,
    body: String,
}

fn request(addr: SocketAddr, method: &str, path: &str, token: Option<&str>) -> Reply {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{auth}Content-Length: 0\r\n\r\n").unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("the response was cut short");
    let raw = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = raw.split_once("\r\n\r\n").expect("no header/body separator");
    let status = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    Reply { status, head: head.to_string(), body: body.to_string() }
}

fn header(reply: &Reply, name: &str) -> Option<String> {
    reply.head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim().to_string())
    })
}

const ASSETS: [(&str, &str); 3] = [("/", "text/html"), ("/app.react.js", "text/javascript"), ("/app.css", "text/css")];

#[test]
fn the_page_and_its_assets_are_served_without_a_token_with_safe_headers() {
    let server = start("serve");
    for (path, kind) in ASSETS {
        let reply = request(server.addr(), "GET", path, None);
        assert_eq!(reply.status, 200, "{path}: {}", reply.head);
        assert!(!reply.body.is_empty(), "{path}");
        assert!(header(&reply, "content-type").unwrap().starts_with(kind), "{path}: {}", reply.head);
        assert_eq!(header(&reply, "cache-control").as_deref(), Some("no-store"), "{path}");
        assert_eq!(header(&reply, "x-content-type-options").as_deref(), Some("nosniff"), "{path}");
        let csp = header(&reply, "content-security-policy").unwrap_or_else(|| panic!("{path}: no CSP"));
        for part in ["default-src 'none'", "script-src 'self'", "style-src 'self'", "connect-src 'self'", "frame-ancestors 'none'", "form-action 'none'", "base-uri 'none'"] {
            assert!(csp.contains(part), "{path}: {csp}");
        }
        assert!(!csp.contains("unsafe-inline") && !csp.contains("unsafe-eval") && !csp.contains('*'), "{path}: {csp}");

        let head = request(server.addr(), "HEAD", path, None);
        assert_eq!(head.status, 200, "{path}");
        assert!(head.body.is_empty(), "{path}");
        assert_eq!(header(&head, "content-type"), header(&reply, "content-type"), "{path}");
    }
}

#[test]
fn the_token_free_page_is_only_for_get_and_head_from_an_allowed_peer() {
    for peer in ["127.0.0.1", "::1", "100.64.0.1", "fd7a:115c:a1e0::1"] {
        for (path, _) in ASSETS {
            for method in ["GET", "HEAD"] {
                assert!(orq_remote::page_asset(peer.parse().unwrap(), method, path).is_some(), "{peer} {method} {path}");
            }
            assert!(orq_remote::page_asset(peer.parse().unwrap(), "POST", path).is_none(), "{peer} POST {path}");
        }
    }
    for peer in ["8.8.8.8", "192.168.1.5", "10.0.0.1", "100.63.255.255", "100.128.0.1", "2001:db8::1", "fd7a:115c:a1e1::1"] {
        for (path, _) in ASSETS {
            for method in ["GET", "HEAD"] {
                assert!(orq_remote::page_asset(peer.parse().unwrap(), method, path).is_none(), "{peer} {method} {path} must be refused");
            }
        }
    }
}

#[test]
fn the_page_pulls_in_its_script_and_style_by_url_and_carries_no_inline_code() {
    let server = start("page");
    let page = request(server.addr(), "GET", "/", None).body;
    assert!(page.contains("src=\"/app.react.js\"") && page.contains("href=\"/app.css\""), "{page}");
    let lower = page.to_lowercase();
    assert!(!lower.contains("<style") && !lower.contains(" style=") && !lower.contains(" onclick=") && !lower.contains("javascript:"), "{page}");
    assert!(!lower.contains("<form"), "{page}");
    assert_eq!(lower.matches("<script").count(), 1, "{page}");
    assert!(!lower.contains(TOKEN));
}

#[test]
fn only_get_and_head_are_served_and_other_methods_are_refused_like_everywhere_else() {
    let server = start("methods");
    let refusal = request(server.addr(), "POST", "/api/reviews", None);
    for (path, _) in ASSETS {
        for method in ["POST", "PUT", "DELETE", "PATCH"] {
            let reply = request(server.addr(), method, path, None);
            assert_eq!(reply.status, 401, "{method} {path}");
            assert_eq!(reply.body, refusal.body, "{method} {path}");
            // With the token the write is refused as read-only.
            assert_eq!(request(server.addr(), method, path, Some(TOKEN)).status, 405, "{method} {path}");
        }
    }
}

#[test]
fn data_paths_still_need_the_token_and_unknown_paths_get_the_usual_refusal() {
    let server = start("data");
    let refusal = request(server.addr(), "GET", "/api/reviews", None);
    assert_eq!(refusal.status, 401);
    for path in ["/nowhere", "/index.html", "/app.js", "/app.react.js/", "/app.js/extra", "/api", "//app.js", "/APP.JS"] {
        let reply = request(server.addr(), "GET", path, None);
        assert_eq!(reply.status, 401, "{path}");
        assert_eq!(reply.body, refusal.body, "{path}");
        assert_eq!(header(&reply, "content-type"), header(&refusal, "content-type"), "{path}");
    }
    assert_eq!(request(server.addr(), "GET", "/nowhere", Some(TOKEN)).status, 404);
}

#[test]
fn nothing_outside_the_compiled_in_set_can_be_read_through_the_page_path() {
    let server = start("escape");
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
    let marker = "[dev-dependencies]";
    assert!(manifest.contains(marker));
    for path in ["/../Cargo.toml", "/%2e%2e/Cargo.toml", "/app.js/../Cargo.toml", "/web/../Cargo.toml", "/web/index.html", "/src/lib.rs", "/Cargo.toml", "/%2e%2e%2fCargo.toml"] {
        for token in [None, Some(TOKEN)] {
            let reply = request(server.addr(), "GET", path, token);
            assert!(!reply.body.contains(marker) && !reply.body.contains("pub fn"), "{path}: {}", reply.body);
            assert!(reply.status == 401 || reply.status == 404 || reply.status == 400, "{path}: {}", reply.status);
        }
    }
}
