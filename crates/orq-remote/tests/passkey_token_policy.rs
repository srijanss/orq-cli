//! Where the access token still works once passkey sign-in is on: to enrol the first passkey, and on
//! a direct local connection unless that is switched off. Everything else needs a session.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;

use orq_core::passkey_register::PasskeyHost;
use orq_core::passkey_routes::PasskeyAuth;
use orq_core::remote_inbox::{ProjectsProvider, RemoteConfig};
use orq_core::storage::StorageRoots;
use orq_remote::{Server, ServerState};

const TOKEN: &str = "passkey-policy-test-token-0123456789ab";
const PROXIED: (&str, &str) = ("X-Forwarded-For", "100.64.0.9");

struct NoProjects;
impl ProjectsProvider for NoProjects {
    fn projects(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!([]))
    }
}

fn start(tag: &str, loopback_token: bool) -> Server {
    let base = orq_test_support::process_temp_dir().join(format!("orq-remote-policy-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let state = Arc::new(ServerState { roots, config: RemoteConfig { token: TOKEN.to_string() }, project_id: "p-0a1b2c3d".into(), project_name: "orq-cli".into() });
    let auth = Arc::new(PasskeyAuth::new(PasskeyHost::from_setting("box.tail1234.ts.net").unwrap()).with_loopback_token(loopback_token));
    Server::start_with_auth(TcpListener::bind("127.0.0.1:0").unwrap(), state, Arc::new(NoProjects), auth).unwrap()
}

fn status_of(addr: SocketAddr, method: &str, path: &str, headers: &[(&str, &str)]) -> String {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    let extra: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
    let body = if method == "POST" { "{}" } else { "" };
    let head = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{extra}Content-Length: {}\r\n\r\n{body}", body.len());
    stream.write_all(head.as_bytes()).unwrap();
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).lines().next().unwrap_or("").to_string()
}

#[test]
fn through_a_proxy_the_token_only_enrols_the_first_passkey_and_opens_nothing_else_even_before_one_exists() {
    let server = start("proxied", true);
    let auth = format!("Bearer {TOKEN}");
    assert!(status_of(server.addr(), "GET", "/api/projects", &[("Authorization", &auth), PROXIED]).contains("401"));
    assert!(status_of(server.addr(), "POST", "/auth/register/begin", &[("Authorization", &auth), PROXIED]).contains("200"));
}

#[test]
fn on_a_direct_local_connection_the_token_still_works_unless_that_is_switched_off() {
    let auth = format!("Bearer {TOKEN}");
    let allowed = start("loopback-on", true);
    assert!(status_of(allowed.addr(), "GET", "/api/projects", &[("Authorization", &auth)]).contains("200"));
    let refused = start("loopback-off", false);
    assert!(status_of(refused.addr(), "GET", "/api/projects", &[("Authorization", &auth)]).contains("401"));
    assert!(status_of(refused.addr(), "POST", "/auth/register/begin", &[("Authorization", &auth)]).contains("200"), "enrolling still works");
}
