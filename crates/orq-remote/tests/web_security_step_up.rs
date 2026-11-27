//! Web security: the risky actions need a passkey step-up made in the last five minutes, a passkey session
//! ends when idle or too old and can be listed and revoked, the access token can be rotated from the web,
//! and the audit log can be filtered. All over the socket with a software authenticator and a clock the
//! test moves.

mod web_deps;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use orq_core::passkey_challenge::encode_b64url;
use orq_core::passkey_register::PasskeyHost;
use orq_core::passkey_routes::PasskeyAuth;
use orq_core::remote_inbox::{ProjectsProvider, RemoteConfig, RunDiscard};
use orq_core::storage::StorageRoots;
use orq_remote::{Server, ServerState};
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const TOKEN: &str = "step-up-test-token-0123456789abcdefghij";
const RP: &str = "box.tail1234.ts.net";
const START: u64 = 1_800_000_000;

/// Answers the risky actions as done, so a request past the step-up gate is seen to go through.
struct Projects;
impl ProjectsProvider for Projects {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }
    fn discard_run(&self, _id: &str, _run: &str) -> Result<RunDiscard, String> {
        Ok(RunDiscard::Discarded { note: "discarded".into(), cleared: false })
    }
    fn resume_with_answer(&self, _id: &str, run: &str, _answer: &str) -> Result<orq_core::remote_inbox::RunControlOutcome, String> {
        Ok(orq_core::remote_inbox::RunControlOutcome::Done(run.to_string()))
    }
}

struct Rig {
    server: Server,
    roots: StorageRoots,
    clock: Arc<AtomicU64>,
    phone: Device,
}

struct Session {
    cookie: String,
    csrf: String,
}

fn rig(tag: &str) -> Rig {
    let base = orq_test_support::process_temp_dir().join(format!("orq-remote-step-up-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let state = Arc::new(ServerState { roots: roots.clone(), config: RemoteConfig { token: TOKEN.to_string() }, project_id: "p-0a1b2c3d".into(), project_name: "orq-cli".into() });
    let clock = Arc::new(AtomicU64::new(START));
    let read = clock.clone();
    let auth = Arc::new(PasskeyAuth::new(PasskeyHost::from_setting(RP).unwrap()).with_clock(Arc::new(move || read.load(Ordering::SeqCst))));
    let server = Server::start_with_auth(TcpListener::bind("127.0.0.1:0").unwrap(), state, Arc::new(Projects), auth).unwrap();
    let mut phone = Device::new(11);
    let bearer = format!("Bearer {TOKEN}");
    let (_, _, body) = send(server.addr(), "POST", "/auth/register/begin", &[("Authorization", &bearer)], b"{}");
    let (status, _, _) = send(server.addr(), "POST", "/auth/register/finish", &[("Authorization", &bearer)], &phone.register_body(&parse(&body), "phone"));
    assert!(status.contains("200"), "{status}");
    phone.count = 0;
    Rig { server, roots, clock, phone }
}

impl Rig {
    fn addr(&self) -> SocketAddr {
        self.server.addr()
    }

    fn advance(&self, secs: u64) {
        self.clock.fetch_add(secs, Ordering::SeqCst);
    }

    fn sign_in(&mut self) -> Session {
        let (_, _, options) = send(self.addr(), "POST", "/auth/login/begin", &[], b"{}");
        let (status, head, body) = send(self.addr(), "POST", "/auth/login/finish", &[], &self.phone.login_body(&parse(&options)));
        assert!(status.contains("200"), "{status} {body}");
        let cookie = cookie_from(&head);
        let (_, _, status) = send(self.addr(), "GET", "/auth/status", &[("Cookie", &cookie), ("X-Forwarded-For", "100.64.0.9")], b"");
        let csrf = parse(&status)["csrf"].as_str().expect("a csrf token").to_string();
        Session { cookie, csrf }
    }

    /// Answers a step-up challenge with the passkey; the status of the answer.
    fn step_up(&mut self, session: &Session) -> String {
        let (status, _, options) = self.auth_post(session, "/auth/confirm/begin", br#"{"purpose":"step_up"}"#);
        assert!(status.contains("200"), "{status} {options}");
        let assertion: Value = serde_json::from_slice(&self.phone.login_body(&parse(&options))).unwrap();
        let (status, _, body) = self.auth_post(session, "/auth/step-up", json!({ "assertion": assertion }).to_string().as_bytes());
        assert!(status.contains("200"), "{status} {body}");
        status
    }

    /// What the signed-in page sends with a change: its cookie, this server's origin and the CSRF token, through the proxy.
    fn headers(session: &Session) -> Vec<(&'static str, String)> {
        vec![("Cookie", session.cookie.clone()), ("X-Forwarded-For", "100.64.0.9".to_string()), ("Origin", format!("https://{RP}")), ("X-CSRF-Token", session.csrf.clone())]
    }

    fn auth_post(&self, session: &Session, path: &str, body: &[u8]) -> (String, String, String) {
        let headers = Self::headers(session);
        let refs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
        send(self.addr(), "POST", path, &refs, body)
    }

    fn post(&self, session: &Session, path: &str, body: &str) -> (u16, Value) {
        let (status, _, body) = self.auth_post(session, path, body.as_bytes());
        (code(&status), serde_json::from_str(&body).unwrap_or(Value::Null))
    }
}

fn code(status: &str) -> u16 {
    status.split_whitespace().nth(1).and_then(|code| code.parse().ok()).unwrap_or(0)
}

fn send(addr: SocketAddr, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> (String, String, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    let extra: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
    let head = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{extra}Content-Length: {}\r\n\r\n", body.len());
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    let text = String::from_utf8_lossy(&bytes).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (head.lines().next().unwrap_or("").to_string(), head.to_string(), body.to_string())
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|_| panic!("not json: {body}"))
}

fn cookie_from(head: &str) -> String {
    let line = head.lines().find(|l| l.to_lowercase().starts_with("set-cookie:")).expect("no cookie set");
    line.split_once(':').unwrap().1.trim().split(';').next().unwrap().to_string()
}

fn cbor_head(major: u8, n: u64) -> Vec<u8> {
    let m = major << 5;
    match n {
        0..=23 => vec![m | n as u8],
        24..=255 => vec![m | 24, n as u8],
        _ => vec![m | 25, (n >> 8) as u8, n as u8],
    }
}
fn int(n: i64) -> Vec<u8> {
    if n >= 0 {
        cbor_head(0, n as u64)
    } else {
        cbor_head(1, (-1 - n) as u64)
    }
}
fn bytes(b: &[u8]) -> Vec<u8> {
    [cbor_head(2, b.len() as u64), b.to_vec()].concat()
}
fn text(t: &str) -> Vec<u8> {
    [cbor_head(3, t.len() as u64), t.as_bytes().to_vec()].concat()
}
fn map(entries: Vec<(Vec<u8>, Vec<u8>)>) -> Vec<u8> {
    let mut out = cbor_head(5, entries.len() as u64);
    for (k, v) in entries {
        out.extend(k);
        out.extend(v);
    }
    out
}

struct Device {
    signer: SigningKey,
    id: Vec<u8>,
    count: u32,
}

impl Device {
    fn new(seed: u8) -> Device {
        Device { signer: SigningKey::from_bytes((&[seed; 32]).into()).unwrap(), id: vec![seed; 32], count: 0 }
    }

    fn client_data(kind: &str, options: &Value) -> String {
        json!({"type": kind, "challenge": options["challenge"], "origin": format!("https://{RP}"), "crossOrigin": false}).to_string()
    }

    fn register_body(&self, options: &Value, label: &str) -> Vec<u8> {
        let point = self.signer.verifying_key().to_encoded_point(false);
        let cose = map(vec![(int(1), int(2)), (int(3), int(-7)), (int(-1), int(1)), (int(-2), bytes(point.x().unwrap())), (int(-3), bytes(point.y().unwrap()))]);
        let mut auth = Sha256::digest(RP.as_bytes()).to_vec();
        auth.push(0x45);
        auth.extend(0u32.to_be_bytes());
        auth.extend([0u8; 16]);
        auth.extend((self.id.len() as u16).to_be_bytes());
        auth.extend(&self.id);
        auth.extend(cose);
        let attestation = map(vec![(text("fmt"), text("none")), (text("attStmt"), map(vec![])), (text("authData"), bytes(&auth))]);
        let client = Self::client_data("webauthn.create", options);
        json!({"clientDataJSON": encode_b64url(client.as_bytes()), "attestationObject": encode_b64url(&attestation), "label": label}).to_string().into_bytes()
    }

    fn login_body(&mut self, options: &Value) -> Vec<u8> {
        self.count += 1;
        let mut auth = Sha256::digest(RP.as_bytes()).to_vec();
        auth.push(0x05);
        auth.extend(self.count.to_be_bytes());
        let client = Self::client_data("webauthn.get", options);
        let mut signed = auth.clone();
        signed.extend(Sha256::digest(client.as_bytes()));
        let signature: Signature = self.signer.sign(&signed);
        json!({
            "id": encode_b64url(&self.id),
            "clientDataJSON": encode_b64url(client.as_bytes()),
            "authenticatorData": encode_b64url(&auth),
            "signature": encode_b64url(signature.to_der().as_bytes()),
        })
        .to_string()
        .into_bytes()
    }
}

#[test]
fn discard_needs_a_recent_step_up_and_goes_through_after_one() {
    let mut rig = rig("discard");
    let session = rig.sign_in();
    let path = "/api/projects/p-0a1b2c3d/runs/r1/discard";
    let (status, body) = rig.post(&session, path, "{}");
    assert_eq!((status, body["error"].as_str()), (401, Some("step_up_required")), "{body}");
    rig.step_up(&session);
    let (status, body) = rig.post(&session, path, "{}");
    assert_eq!((status, body["note"].as_str()), (202, Some("discarded")), "{body}");
}

#[test]
fn a_step_up_six_minutes_old_is_refused() {
    let mut rig = rig("stale");
    let session = rig.sign_in();
    let path = "/api/projects/p-0a1b2c3d/runs/r1/discard";
    rig.step_up(&session);
    rig.advance(4 * 60);
    assert_eq!(rig.post(&session, path, "{}").0, 202, "four minutes on, the step-up still holds");
    rig.advance(2 * 60);
    let (status, body) = rig.post(&session, path, "{}");
    assert_eq!((status, body["error"].as_str()), (401, Some("step_up_required")), "{body}");
}

impl Rig {
    fn signed_in(&self, session: &Session) -> bool {
        let (_, _, body) = send(self.addr(), "GET", "/auth/status", &[("Cookie", &session.cookie), ("X-Forwarded-For", "100.64.0.9")], b"");
        parse(&body)["signed_in"] == true
    }
}

const HOUR: u64 = 60 * 60;

#[test]
fn a_session_ends_after_twelve_hours_idle() {
    let mut rig = rig("idle");
    let session = rig.sign_in();
    rig.advance(11 * HOUR);
    assert!(rig.signed_in(&session), "eleven hours idle");
    rig.advance(11 * HOUR);
    assert!(rig.signed_in(&session), "used eleven hours ago, so still not idle for twelve");
    rig.advance(12 * HOUR);
    assert!(!rig.signed_in(&session), "twelve hours idle ends it");
}

#[test]
fn a_session_ends_seven_days_after_sign_in_however_much_it_is_used() {
    let mut rig = rig("absolute");
    let session = rig.sign_in();
    for _ in 0..15 {
        rig.advance(11 * HOUR);
        assert!(rig.signed_in(&session));
    }
    rig.advance(3 * HOUR);
    assert!(!rig.signed_in(&session), "seven days in total ends it");
}

impl Rig {
    fn get(&self, session: &Session, path: &str) -> (u16, Value) {
        let (status, _, body) = send(self.addr(), "GET", path, &[("Cookie", &session.cookie), ("X-Forwarded-For", "100.64.0.9")], b"");
        (code(&status), serde_json::from_str(&body).unwrap_or(Value::Null))
    }
}

#[test]
fn the_session_limits_are_set_in_settings_within_bounds() {
    let mut rig = rig("limits");
    let session = rig.sign_in();
    let (status, limits) = rig.get(&session, "/api/settings/sessions");
    assert_eq!(status, 200, "{limits}");
    assert_eq!((limits["idle_hours"].as_u64(), limits["max_days"].as_u64()), (Some(12), Some(7)), "{limits}");
    assert_eq!((limits["idle_hours_bounds"].clone(), limits["max_days_bounds"].clone()), (json!([1, 72]), json!([1, 30])), "{limits}");
    for out_of_bounds in [r#"{"idle_hours":0,"max_days":7}"#, r#"{"idle_hours":73,"max_days":7}"#, r#"{"idle_hours":12,"max_days":31}"#, r#"{"idle_hours":12}"#] {
        assert_eq!(rig.post(&session, "/api/settings/sessions", out_of_bounds).0, 422, "{out_of_bounds}");
    }
    let (status, saved) = rig.post(&session, "/api/settings/sessions", r#"{"idle_hours":1,"max_days":2}"#);
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved, json!({"idle_hours": 1, "max_days": 2, "idle_hours_bounds": [1, 72], "max_days_bounds": [1, 30]}), "the save answers as the read does");
    assert!(std::fs::read_to_string(rig.roots.config_file()).unwrap().contains("session_idle_hours = 1"), "written to the settings file");
    assert_eq!(rig.get(&session, "/api/settings/sessions").1["idle_hours"], 1);
    rig.advance(HOUR - 1);
    assert!(rig.signed_in(&session), "used under an hour ago");
    rig.advance(HOUR);
    assert!(!rig.signed_in(&session), "an hour idle now ends a session, the open ones too");
}

#[test]
fn session_limits_saved_at_an_address_with_a_query_apply_at_once() {
    let mut rig = rig("limits-query");
    let session = rig.sign_in();
    let (status, saved) = rig.post(&session, "/api/settings/sessions?from=settings", r#"{"idle_hours":1,"max_days":2}"#);
    assert_eq!(status, 200, "{saved}");
    rig.advance(HOUR + 1);
    assert!(!rig.signed_in(&session), "the shorter idle limit holds for the open session without a restart");
}

#[test]
fn the_session_list_shows_each_session_and_revoke_ends_just_that_one() {
    let mut rig = rig("list");
    let laptop = rig.sign_in();
    rig.advance(60);
    let phone = rig.sign_in();
    rig.advance(60);
    let (status, list) = rig.get(&phone, "/auth/sessions");
    assert_eq!(status, 200, "{list}");
    let rows = list["sessions"].as_array().expect("a session list").clone();
    assert_eq!(rows.len(), 2, "{list}");
    let mine = rows.iter().find(|row| row["this_session"] == true).expect("this session is marked");
    let other = rows.iter().find(|row| row["this_session"] == false).expect("the other session");
    assert_eq!((mine["device"].as_str(), mine["created"].as_u64(), mine["last_used"].as_u64()), (Some("phone"), Some(START + 60), Some(START + 120)), "{list}");
    assert_eq!((other["created"].as_u64(), other["last_used"].as_u64()), (Some(START), Some(START)), "{list}");
    let handle = other["id"].as_str().expect("a handle to revoke by").to_string();
    assert!(!laptop.cookie.contains(&handle), "the handle is not the session's cookie");
    assert_eq!(rig.get(&laptop, "/api/settings/sessions").0, 200, "the laptop is still in");
    let (status, body) = rig.post(&phone, &format!("/auth/sessions/{handle}/revoke"), "");
    assert_eq!(status, 200, "{body}");
    assert!(!rig.signed_in(&laptop), "revoked");
    assert!(rig.signed_in(&phone), "only that session");
    assert_eq!(rig.post(&phone, "/auth/sessions/no-such-session/revoke", "").0, 404);
    assert_eq!(rig.get(&phone, "/auth/sessions").1["sessions"].as_array().map(Vec::len), Some(1));
}

#[test]
fn sign_out_everywhere_ends_every_session() {
    let mut rig = rig("everywhere");
    let (a, b) = (rig.sign_in(), rig.sign_in());
    let (status, body) = rig.post(&a, "/auth/sessions/end-all", "");
    assert_eq!(status, 200, "{body}");
    assert!(!rig.signed_in(&a) && !rig.signed_in(&b));
}

/// The status of `GET /api/projects` with `token` as the bearer, from this machine's own loopback.
fn with_token(rig: &Rig, token: &str) -> u16 {
    let bearer = format!("Bearer {token}");
    code(&send(rig.addr(), "GET", "/api/projects", &[("Authorization", &bearer)], b"").0)
}

#[test]
fn rotating_the_token_needs_a_step_up_shows_the_new_one_once_and_ends_the_old_one() {
    let mut rig = rig("rotate");
    std::fs::write(RemoteConfig::token_path(&rig.roots), TOKEN).unwrap();
    assert_eq!(with_token(&rig, TOKEN), 200, "the token works before");
    let session = rig.sign_in();
    let (status, body) = rig.post(&session, "/auth/token/rotate", "");
    assert_eq!((status, body["error"].as_str()), (401, Some("step_up_required")), "{body}");
    rig.step_up(&session);
    let (status, body) = rig.post(&session, "/auth/token/rotate", "");
    assert_eq!(status, 200, "{body}");
    let token = body["token"].as_str().expect("the new token, shown once").to_string();
    assert!(token.len() >= 32 && token != TOKEN, "{token}");
    assert_eq!(std::fs::read_to_string(RemoteConfig::token_path(&rig.roots)).unwrap().trim(), token, "orq writes the token file itself");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(RemoteConfig::token_path(&rig.roots)).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(with_token(&rig, TOKEN), 401, "the old token no longer works");
    assert_eq!(with_token(&rig, &token), 200, "the new one does");
    assert!(rig.signed_in(&session), "the passkey session carries on");
    let (status, _) = rig.get(&session, "/api/projects");
    assert_eq!(status, 200, "and still opens the API");
}

impl Rig {
    /// A passkey answer to a fresh challenge `session` asked for with `purpose`.
    fn answer_for(&mut self, session: &Session, purpose: &str) -> Value {
        let (status, _, options) = self.auth_post(session, "/auth/confirm/begin", json!({ "purpose": purpose }).to_string().as_bytes());
        assert!(status.contains("200"), "{purpose}: {status} {options}");
        serde_json::from_slice(&self.phone.login_body(&parse(&options))).unwrap()
    }

    /// Whether a discard from `session` is still turned back for want of a step-up.
    fn still_needs_step_up(&self, session: &Session) -> bool {
        let (status, body) = self.post(session, "/api/projects/p-0a1b2c3d/runs/r1/discard", "{}");
        status == 401 && body["error"] == "step_up_required"
    }
}

fn step_up_body(assertion: &Value) -> Vec<u8> {
    json!({ "assertion": assertion }).to_string().into_bytes()
}

#[test]
fn a_step_up_answer_used_twice_opens_nothing_the_second_time() {
    let mut rig = rig("replay");
    let session = rig.sign_in();
    let answer = rig.answer_for(&session, "step_up");
    assert_eq!(rig.post(&session, "/auth/step-up", &json!({ "assertion": answer }).to_string()).0, 200);
    rig.advance(6 * 60);
    assert!(rig.still_needs_step_up(&session), "the step-up has lapsed");
    let (status, body) = rig.post(&session, "/auth/step-up", &json!({ "assertion": answer }).to_string());
    assert_eq!(status, 401, "a used answer is refused: {body}");
    assert!(rig.still_needs_step_up(&session), "and does not open the risky actions again");
}

#[test]
fn a_step_up_answer_to_another_sessions_challenge_is_refused() {
    let mut rig = rig("other-session");
    let mine = rig.sign_in();
    let theirs = rig.sign_in();
    let answer = rig.answer_for(&theirs, "step_up");
    let (status, body) = rig.post(&mine, "/auth/step-up", &json!({ "assertion": answer }).to_string());
    assert_eq!(status, 401, "{body}");
    assert!(rig.still_needs_step_up(&mine));
    assert!(rig.still_needs_step_up(&theirs), "nor does it step the other session up");
}

#[test]
fn a_step_up_answer_to_a_challenge_for_another_purpose_is_refused() {
    let mut rig = rig("other-purpose");
    let session = rig.sign_in();
    let answer = rig.answer_for(&session, "chat.allow");
    let (status, body) = rig.post(&session, "/auth/step-up", &json!({ "assertion": answer }).to_string());
    assert_eq!(status, 401, "{body}");
    assert!(rig.still_needs_step_up(&session));
}

#[test]
fn a_step_up_answer_with_a_bad_signature_is_refused() {
    let mut rig = rig("bad-signature");
    let session = rig.sign_in();
    let mut answer = rig.answer_for(&session, "step_up");
    let signature = answer["signature"].as_str().unwrap().to_string();
    let flipped = if signature.starts_with('A') { "B" } else { "A" };
    answer["signature"] = json!(format!("{flipped}{}", &signature[1..]));
    let (status, body) = rig.post(&session, "/auth/step-up", &json!({ "assertion": answer }).to_string());
    assert_eq!(status, 401, "{body}");
    assert!(rig.still_needs_step_up(&session));
}

#[test]
fn a_step_up_without_the_csrf_token_or_from_another_origin_is_refused() {
    let mut rig = rig("csrf-origin");
    let session = rig.sign_in();
    let cases: [(&str, Vec<(&str, String)>); 3] = [
        ("no CSRF token", vec![("Cookie", session.cookie.clone()), ("X-Forwarded-For", "100.64.0.9".into()), ("Origin", format!("https://{RP}"))]),
        ("no origin", vec![("Cookie", session.cookie.clone()), ("X-Forwarded-For", "100.64.0.9".into()), ("X-CSRF-Token", session.csrf.clone())]),
        ("another origin", vec![("Cookie", session.cookie.clone()), ("X-Forwarded-For", "100.64.0.9".into()), ("Origin", "https://elsewhere.example".into()), ("X-CSRF-Token", session.csrf.clone())]),
    ];
    for (case, headers) in cases {
        let answer = rig.answer_for(&session, "step_up");
        let refs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let (status, _, body) = send(rig.addr(), "POST", "/auth/step-up", &refs, &step_up_body(&answer));
        assert!(matches!(code(&status), 401 | 403), "{case}: {status} {body}");
        assert!(rig.still_needs_step_up(&session), "{case}: the risky actions stay closed");
    }
}

#[test]
fn rotations_at_the_same_time_leave_the_server_on_the_token_in_the_file() {
    let mut rig = rig("rotate-race");
    std::fs::write(RemoteConfig::token_path(&rig.roots), TOKEN).unwrap();
    let session = rig.sign_in();
    rig.step_up(&session);
    for round in 0..4 {
        let answered: Vec<String> = std::thread::scope(|scope| {
            let rotations: Vec<_> = (0..8).map(|_| scope.spawn(|| rig.post(&session, "/auth/token/rotate", ""))).collect();
            rotations.into_iter().map(|rotation| rotation.join().unwrap()).filter(|(status, _)| *status == 200).filter_map(|(_, body)| body["token"].as_str().map(String::from)).collect()
        });
        assert!(!answered.is_empty(), "round {round}: some rotation went through");
        let written = std::fs::read_to_string(RemoteConfig::token_path(&rig.roots)).unwrap().trim().to_string();
        assert_eq!(with_token(&rig, &written), 200, "round {round}: the token in the file is the one in force");
        for superseded in answered.iter().filter(|token| **token != written) {
            assert_eq!(with_token(&rig, superseded), 401, "round {round}: a superseded token stays ended");
        }
    }
}

#[test]
fn the_audit_filters_return_only_matching_lines() {
    use orq_core::remote_audit::{record, Attempt, Outcome};
    let mut rig = rig("audit");
    let old = 1_000_000_000;
    record(&rig.roots, &Attempt { identity: "orq serve", action: "queue.start", project: "p-0a1b2c3d", run: None, outcome: Outcome::Allowed, reason: None }, old).unwrap();
    let session = rig.sign_in();
    rig.post(&session, "/api/projects/p-0a1b2c3d/runs/r1/discard", "{}");
    rig.step_up(&session);
    rig.post(&session, "/api/projects/p-0a1b2c3d/runs/r1/discard", "{}");
    let lines = |query: &str| -> Vec<Value> {
        let (status, body) = rig.get(&session, &format!("/api/audit{query}"));
        assert_eq!(status, 200, "{query}: {body}");
        body["audit"].as_array().unwrap_or_else(|| panic!("{query}: {body}")).clone()
    };
    let all = lines("");
    assert!(all.len() >= 4, "{all:?}");
    let web = lines("?source=web");
    assert!(!web.is_empty() && web.iter().all(|line| line["source"] == "web"), "{web:?}");
    assert!(web.iter().any(|line| line["action"] == "run.discard" && line["identity"] == "phone"), "a web action names the passkey device: {web:?}");
    let cli = lines("?source=cli");
    assert_eq!(cli.iter().map(|line| line["action"].as_str()).collect::<Vec<_>>(), vec![Some("queue.start")], "{cli:?}");
    let phone = lines("?identity=phone");
    assert!(!phone.is_empty() && phone.iter().all(|line| line["identity"] == "phone"), "{phone:?}");
    assert!(lines("?identity=token").is_empty(), "no token lines here");
    let runs = lines("?action=run");
    assert!(runs.len() >= 2 && runs.iter().all(|line| line["action"].as_str().is_some_and(|action| action.starts_with("run."))), "{runs:?}");
    assert_eq!(lines("?action=run.discard").len(), runs.iter().filter(|line| line["action"] == "run.discard").count());
    let recent = lines(&format!("?since={}", old + 1));
    assert!(recent.iter().all(|line| line["action"] != "queue.start") && recent.len() == all.len() - 1, "{recent:?}");
    assert_eq!(lines(&format!("?until={old}")).len(), 1, "until is inclusive");
    let today_from_the_web = lines(&format!("?source=web&since={}", old + 1));
    assert_eq!(today_from_the_web.len(), web.len());
    assert_eq!(rig.get(&session, "/api/audit?source=elsewhere").0, 400, "an unknown source is refused, not ignored");
}

#[test]
fn the_unfiltered_audit_is_the_plain_newest_lines() {
    use orq_core::remote_audit::{read, read_filtered, record, Attempt, Filter, Outcome};
    let rig = rig("audit-plain");
    let reason = "x".repeat(4000);
    for n in 0..300u64 {
        record(&rig.roots, &Attempt { identity: "orq serve", action: "queue.start", project: "p-0a1b2c3d", run: None, outcome: Outcome::Allowed, reason: Some(&reason) }, 1_000 + n).unwrap();
    }
    let plain = read(&rig.roots, 200).unwrap();
    assert_eq!(plain.len(), 200);
    assert_eq!(read_filtered(&rig.roots, 200, &Filter::default()).unwrap(), plain, "no filter reads as the plain audit does");
}

#[test]
fn an_answer_line_shows_its_redacted_summary() {
    let mut rig = rig("answer");
    let session = rig.sign_in();
    let answer = "Use the staging key sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789 and go on";
    let (status, _) = rig.post(&session, "/api/projects/p-0a1b2c3d/runs/r1/resume", &json!({ "answer": answer }).to_string());
    assert_eq!(status, 202);
    let (_, body) = rig.get(&session, "/api/audit?action=run.resume");
    let line = &body["audit"][0];
    let reason = line["reason"].as_str().unwrap_or_else(|| panic!("{body}"));
    assert!(reason.starts_with("answer: Use the staging key"), "{reason}");
    assert!(!reason.contains("abcdefghijklmnopqrstuvwxyz0123456789"), "the secret is redacted: {reason}");
}

/// Every risky action, as the page sends it: the path and the body.
const RISKY: &[(&str, &str)] = &[
    ("/api/projects/p-0a1b2c3d/chats/c1/allow", r#"{"rule":"Bash(cargo test:*)"}"#),
    ("/api/projects/p-0a1b2c3d/runs/r1/accept", "{}"),
    ("/api/projects/p-0a1b2c3d/runs/r1/cleanup", r#"{"confirm":true}"#),
    ("/api/storage/worktrees/delete-merged", r#"{"items":[]}"#),
    ("/api/storage/worktrees/p-0a1b2c3d/feat--x/delete", r#"{"version":"v1"}"#),
    ("/api/server/restart", "{}"),
    ("/api/projects/p-0a1b2c3d/settings", r#"{"queue_hours":"09:00-17:00"}"#),
    ("/api/projects/p-0a1b2c3d/settings", r#"{"after_merge":"auto"}"#),
];

#[test]
fn every_risky_action_needs_a_recent_step_up_and_gets_past_it_after_one() {
    let mut rig = rig("risky");
    let session = rig.sign_in();
    for (path, body) in RISKY {
        let (status, reply) = rig.post(&session, path, body);
        assert_eq!((status, reply["error"].as_str()), (401, Some("step_up_required")), "{path} {body}: {reply}");
    }
    let (status, reply) = rig.post(&session, "/api/projects/p-0a1b2c3d/settings", r#"{"queue_hours":null,"after_merge":"ask"}"#);
    assert_ne!(reply["error"].as_str(), Some("step_up_required"), "settings that turn nothing risky on need no step-up: {status} {reply}");
    rig.step_up(&session);
    for (path, body) in RISKY {
        let (_, reply) = rig.post(&session, path, body);
        assert_ne!(reply["error"].as_str(), Some("step_up_required"), "{path} {body}: {reply}");
        assert_ne!(reply["error"].as_str(), Some("allowing a rule needs the passkey confirmation"), "{path}: {reply}");
    }
}

/// The page, in jsdom: a scenario for tests/web_react_harness.mjs, and what it saw and sent.
fn page(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = std::process::Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn server_settings(pending: bool) -> Value {
    json!({"status": 200, "json": {
        "settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": true, "version": "v-1"},
        "server": {"running": "orq 0.1.0", "installed": "orq 0.2.0", "newer_installed": true, "restart_pending": pending, "waiting_for": 1}
    }})
}

/// Signed in with a passkey on Settings, with a browser that answers the passkey prompt (or dismisses it).
fn settings_page(responses: Value, steps: Value, dismissed: bool) -> Option<Value> {
    let answer = json!({"rawId": [1, 2, 3], "clientDataJSON": [4, 5], "authenticatorData": [6, 7], "signature": [8, 9]});
    page(json!({
        "url": "/settings", "auth": {"passkeys": true, "signed_in": true, "csrf": "c-1"},
        "webauthn": {"secure": true, "api": true, "answer": answer, "cancel": dismissed},
        "auth_replies": {
            "/auth/passkeys": {"passkeys": [{"label": "phone", "id": "AAAAphoneID", "this_device": true}]},
            "/auth/confirm/begin": {"challenge": "AAAA", "rpId": RP, "userVerification": "required", "timeout": 60000, "allowCredentials": []},
            "/auth/step-up": {"stepped_up": true}
        },
        "responses": responses, "steps": steps, "selectors": ["[data-restart-server]", "[data-restart-cancel]", "[data-confirm]"]
    }))
}

fn page_texts(out: &Value) -> Vec<String> {
    let frame = out["frames"].as_array().unwrap().last().unwrap();
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

#[test]
fn the_page_asks_for_the_passkey_on_step_up_required_and_tries_once_more() {
    let needed = json!({"status": 401, "json": {"error": "step_up_required"}});
    let responses = json!([server_settings(false), needed, {"status": 202, "json": {"restart_pending": true, "waiting_for": 1}}, server_settings(true)]);
    let steps = json!([{"click": "[data-restart-server]"}, {"click": "[data-confirm]"}]);
    let Some(out) = settings_page(responses, steps, false) else { return };
    let auth: Vec<&Value> = out["authRequests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").collect();
    assert!(auth.iter().any(|r| r["url"] == "/auth/confirm/begin" && r["body"]["purpose"] == "step_up"), "{out}");
    let step_up = auth.iter().find(|r| r["url"] == "/auth/step-up").unwrap_or_else(|| panic!("no step-up sent: {out}"));
    assert_eq!(step_up["headers"]["X-CSRF-Token"], "c-1", "{out}");
    assert!(step_up["body"]["assertion"]["signature"].is_string(), "{out}");
    let restarts: Vec<&Value> = out["requests"].as_array().unwrap().iter().filter(|r| r["url"] == "/api/server/restart").collect();
    assert_eq!(restarts.len(), 2, "sent again once after the step-up: {out}");
    assert!(page_texts(&out).iter().any(|t| t.contains("Restart pending: waiting for 1 run")), "{out}");
}

#[test]
fn a_dismissed_passkey_prompt_sends_nothing_more_and_says_why() {
    let needed = json!({"status": 401, "json": {"error": "step_up_required"}});
    let steps = json!([{"click": "[data-restart-server]"}, {"click": "[data-confirm]"}]);
    let Some(out) = settings_page(json!([server_settings(false), needed]), steps, true) else { return };
    assert!(!out["authRequests"].as_array().unwrap().iter().any(|r| r["url"] == "/auth/step-up"), "{out}");
    assert_eq!(out["requests"].as_array().unwrap().iter().filter(|r| r["url"] == "/api/server/restart").count(), 1, "{out}");
    assert!(page_texts(&out).iter().any(|t| t == "The passkey check was cancelled."), "{out}");
}

/// Settings, signed in with a passkey, with the server's answers to the /auth routes given in `auth`.
fn account_page(auth: Value, steps: Value, selectors: Value) -> Option<Value> {
    let mut replies = json!({"/auth/passkeys": {"passkeys": [{"label": "phone", "id": "AAAAphoneID", "this_device": true}]}});
    for (path, reply) in auth.as_object().unwrap() {
        replies[path] = reply.clone();
    }
    page(json!({
        "url": "/settings", "auth": {"passkeys": true, "signed_in": true, "csrf": "c-1"},
        "auth_replies": replies, "responses": [server_settings(false)], "steps": steps, "selectors": selectors
    }))
}

#[test]
fn settings_lists_the_sessions_and_revoke_ends_the_one_chosen() {
    let now = 1_800_000_000u64;
    let sessions = json!({"sessions": [
        {"id": "s-phone", "device": "phone", "created": now - 3600, "last_used": now - 60, "this_session": true},
        {"id": "s-laptop", "device": "laptop", "created": now - 86_400, "last_used": now - 7200, "this_session": false}
    ]});
    let steps = json!([{"click": "[data-revoke-session]", "index": 1}]);
    let auth = json!({"/auth/sessions": sessions, "/auth/sessions/s-laptop/revoke": {"revoked": true}});
    let Some(out) = account_page(auth, steps, json!(["[data-session]", "[data-revoke-session]"])) else { return };
    let first = &out["frames"][0]["settled"];
    assert_eq!(first["matches"]["[data-session]"], 2, "{out}");
    assert_eq!(first["matches"]["[data-revoke-session]"], 2, "{out}");
    let texts = page_texts(&out);
    for wanted in ["phone", "laptop", "This device"] {
        assert!(texts.iter().any(|t| t == wanted), "{wanted}: {out}");
    }
    assert!(texts.iter().any(|t| t.starts_with("Signed in")) && texts.iter().any(|t| t.starts_with("Last used")), "{out}");
    let revoke = out["authRequests"].as_array().unwrap().iter().find(|r| r["method"] == "POST" && r["url"] == "/auth/sessions/s-laptop/revoke").cloned().unwrap_or_else(|| panic!("{out}"));
    assert_eq!(revoke["headers"]["X-CSRF-Token"], "c-1", "{out}");
}

const NEW_TOKEN: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

#[test]
fn rotate_access_token_asks_first_then_shows_the_new_token_once() {
    let steps = json!([{"click": "[data-rotate-token]"}, {"click": "[data-confirm]"}, {"click": "[data-token-done]"}]);
    let auth = json!({"/auth/sessions": {"sessions": []}, "/auth/token/rotate": {"token": NEW_TOKEN}});
    let Some(out) = account_page(auth, steps, json!(["[data-rotate-token]", "[data-new-token]", "[data-token-done]"])) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(frames[0]["settled"]["matches"]["[data-rotate-token]"], 1, "{out}");
    let texts = |frame: &Value| frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect::<Vec<_>>();
    assert!(texts(&frames[1]).iter().any(|t| t.starts_with("Rotate the access token?")), "asks first: {out}");
    let rotate: Vec<&Value> = out["authRequests"].as_array().unwrap().iter().filter(|r| r["url"] == "/auth/token/rotate").collect();
    assert_eq!(rotate.len(), 1, "{out}");
    assert_eq!(rotate[0]["headers"]["X-CSRF-Token"], "c-1", "{out}");
    assert_eq!(frames[2]["settled"]["matches"]["[data-new-token]"], 1, "{out}");
    assert!(texts(&frames[2]).iter().any(|t| t == NEW_TOKEN), "the new token is shown: {out}");
    assert_eq!(frames[3]["settled"]["matches"]["[data-new-token]"], 0, "and only once: {out}");
    assert!(!texts(&frames[3]).iter().any(|t| t == NEW_TOKEN), "{out}");
}

#[test]
fn rotating_without_a_recent_step_up_asks_for_the_passkey_and_tries_once_more() {
    let answer = json!({"rawId": [1, 2, 3], "clientDataJSON": [4, 5], "authenticatorData": [6, 7], "signature": [8, 9]});
    let Some(out) = page(json!({
        "url": "/settings", "auth": {"passkeys": true, "signed_in": true, "csrf": "c-1"},
        "webauthn": {"secure": true, "api": true, "answer": answer},
        "auth_replies": {
            "/auth/passkeys": {"passkeys": [{"label": "phone", "id": "AAAAphoneID", "this_device": true}]},
            "/auth/sessions": {"sessions": []},
            "/auth/confirm/begin": {"challenge": "AAAA", "rpId": RP, "userVerification": "required", "timeout": 60000, "allowCredentials": []},
            "/auth/step-up": {"stepped_up": true}
        },
        // The server keeps asking, so the page is seen to try only once more.
        "auth_failures": {"/auth/token/rotate": {"status": 401, "json": {"error": "step_up_required"}}},
        "responses": [server_settings(false)], "steps": [{"click": "[data-rotate-token]"}, {"click": "[data-confirm]"}], "selectors": ["[data-new-token]"]
    })) else { return };
    let auth = out["authRequests"].as_array().unwrap();
    assert_eq!(auth.iter().filter(|r| r["url"] == "/auth/step-up").count(), 1, "{out}");
    assert_eq!(auth.iter().filter(|r| r["url"] == "/auth/token/rotate").count(), 2, "{out}");
    assert!(page_texts(&out).iter().any(|t| t == "Confirm with your passkey to rotate the token."), "{out}");
}

fn query_of(url: &str) -> std::collections::HashMap<String, String> {
    url.split_once('?').map_or("", |(_, query)| query).split('&').filter(|pair| !pair.is_empty()).map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (key.to_string(), value.replace("%20", " "))
    }).collect()
}

#[test]
fn the_audit_page_filters_and_has_a_today_from_the_web_preset() {
    let line = json!({"time": START, "identity": "phone", "action": "run.resume", "project": "p-0a1b2c3d", "run": "r1", "outcome": "allowed", "reason": "answer: go on", "source": "web"});
    let audit = json!({"status": 200, "json": {"audit": [line]}});
    let steps = json!([
        {"click": "[data-audit-today-web]"},
        {"set": "[data-audit-identity]", "value": "phone"},
        {"set": "[data-audit-action]", "value": "run"},
        {"click": "[data-audit-apply]"},
        {"set": "[data-audit-source]", "value": "cli"},
        {"click": "[data-audit-apply]"}
    ]);
    let selectors = json!(["[data-audit-today-web]", "[data-audit-source]", "[data-audit-identity]", "[data-audit-action]", "[data-audit-date]"]);
    let Some(out) = page(json!({
        "url": "/settings/audit", "auth": {"passkeys": true, "signed_in": true, "csrf": "c-1"},
        "responses": [audit, audit, audit, audit], "steps": steps, "selectors": selectors
    })) else { return };
    let urls: Vec<String> = out["requests"].as_array().unwrap().iter().filter_map(|r| r["url"].as_str()).filter(|u| u.starts_with("/api/audit")).map(String::from).collect();
    assert_eq!(urls.first().map(String::as_str), Some("/api/audit"), "unfiltered at first: {out}");
    assert!(page_texts(&out).iter().any(|t| t == "answer: go on"), "an answer line shows its summary: {out}");
    let today = query_of(&urls[1]);
    assert_eq!(today.get("source").map(String::as_str), Some("web"), "{urls:?}");
    let (since, until): (u64, u64) = (today["since"].parse().unwrap(), today["until"].parse().unwrap());
    assert!([82_799, 86_399, 89_999].contains(&(until - since)), "one local day, both ends inclusive: {urls:?}");
    let narrowed = query_of(&urls[2]);
    assert_eq!((narrowed.get("identity").map(String::as_str), narrowed.get("action").map(String::as_str)), (Some("phone"), Some("run")), "{urls:?}");
    assert_eq!(narrowed.get("since"), today.get("since"), "the day stays: {urls:?}");
    assert_eq!(query_of(&urls[3]).get("source").map(String::as_str), Some("cli"), "{urls:?}");
}

#[test]
fn settings_edits_the_session_lifetimes_within_their_bounds() {
    let limits = json!({"status": 200, "json": {"idle_hours": 12, "max_days": 7, "idle_hours_bounds": [1, 72], "max_days_bounds": [1, 30]}});
    // Only the saved limits, as a reply from before the bounds were sent back: the page keeps the bounds it read.
    let saved = json!({"status": 200, "json": {"idle_hours": 4, "max_days": 2}});
    let steps = json!([
        {"click": "[data-edit-session-limits]"},
        {"set": "[data-session-idle]", "value": "4"},
        {"set": "[data-session-max]", "value": "2"},
        {"click": "[data-save-session-limits]"}
    ]);
    let selectors = json!(["[data-edit-session-limits]", "[data-session-idle]", "[data-session-max]"]);
    let Some(out) = page(json!({
        "url": "/settings", "auth": {"passkeys": true, "signed_in": true, "csrf": "c-1"},
        "auth_replies": {"/auth/passkeys": {"passkeys": []}, "/auth/sessions": {"sessions": []}},
        "responses": [server_settings(false), limits, saved], "steps": steps, "selectors": selectors
    })) else { return };
    let requests = out["requests"].as_array().unwrap();
    assert!(!requests.iter().take(1).any(|r| r["url"] == "/api/settings/sessions"), "nothing is read until Edit: {out}");
    let frames = out["frames"].as_array().unwrap();
    let texts = |frame: &Value| frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect::<Vec<_>>();
    assert!(texts(&frames[1]).iter().any(|t| t.contains("1–72 hours")) && texts(&frames[1]).iter().any(|t| t.contains("1–30 days")), "the bounds are shown: {out}");
    let post = requests.iter().find(|r| r["method"] == "POST" && r["url"] == "/api/settings/sessions").unwrap_or_else(|| panic!("{out}"));
    assert_eq!(serde_json::from_str::<Value>(post["body"].as_str().unwrap()).unwrap(), json!({"idle_hours": 4, "max_days": 2}), "{out}");
    assert_eq!(post["headers"]["X-CSRF-Token"], "c-1", "{out}");
    let last = texts(frames.last().unwrap());
    assert!(last.iter().any(|t| t.contains("4 hours idle")) && last.iter().any(|t| t.contains("2 days in all")), "{out}");
}
