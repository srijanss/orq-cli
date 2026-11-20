//! Web push: the server's VAPID key, the stored subscriptions, what is sent and when.

mod web_deps;
mod web_push_session;

use std::os::unix::fs::PermissionsExt;

use orq_core::storage::StorageRoots;
use p256::elliptic_curve::sec1::ToEncodedPoint;

fn roots(label: &str) -> StorageRoots {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

/// A push service on loopback: records each request and answers with the next status in `answers`
/// (200-class `201` when none is left).
struct FakePush {
    port: u16,
    seen: std::sync::Arc<std::sync::Mutex<Vec<Seen>>>,
    answers: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<u16>>>,
}

#[derive(Clone)]
struct Seen {
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

impl FakePush {
    fn start() -> FakePush {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: std::sync::Arc<std::sync::Mutex<Vec<Seen>>> = Default::default();
        let answers: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<u16>>> = Default::default();
        let (record, script) = (seen.clone(), answers.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split(' ').nth(1).unwrap_or("").to_string();
                let mut headers = Vec::new();
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    let header = header.trim_end().to_string();
                    if header.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':') {
                        headers.push((name.trim().to_string(), value.trim().to_string()));
                    }
                }
                let length = headers.iter().find(|(n, _)| n.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
                let mut body = vec![0u8; length];
                reader.read_exact(&mut body).unwrap();
                record.lock().unwrap().push(Seen { path, headers, body });
                let status = script.lock().unwrap().pop_front().unwrap_or(201);
                let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
        });
        FakePush { port, seen, answers }
    }

    fn endpoint(&self, name: &str) -> String {
        format!("http://127.0.0.1:{}/push/{name}", self.port)
    }

    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

/// A browser's side of a subscription: its keys, so a test can decrypt what was sent to it.
struct Device {
    secret: p256::SecretKey,
    auth: [u8; 16],
}

impl Device {
    fn new() -> Device {
        Device { secret: p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng), auth: [7u8; 16] }
    }

    fn subscription(&self, endpoint: String) -> orq_remote::web_push::Subscription {
        let public = self.secret.public_key().to_encoded_point(false);
        orq_remote::web_push::Subscription { endpoint, p256dh: orq_remote::web_push::base64url_encode(public.as_bytes()), auth: orq_remote::web_push::base64url_encode(&self.auth) }
    }

    /// RFC 8291 decryption of an aes128gcm body.
    fn decrypt(&self, body: &[u8]) -> Vec<u8> {
        use aes_gcm::aead::Aead;
        use aes_gcm::KeyInit;
        let (salt, rest) = body.split_at(16);
        let (_size, rest) = rest.split_at(4);
        let (id_len, rest) = (rest[0] as usize, &rest[1..]);
        let (sender_public, cipher) = rest.split_at(id_len);
        let sender = p256::PublicKey::from_sec1_bytes(sender_public).unwrap();
        let shared = p256::ecdh::diffie_hellman(self.secret.to_nonzero_scalar(), sender.as_affine());
        let own_public = self.secret.public_key().to_encoded_point(false);
        let mut info = b"WebPush: info\0".to_vec();
        info.extend_from_slice(own_public.as_bytes());
        info.extend_from_slice(sender_public);
        let mut ikm = [0u8; 32];
        hkdf::Hkdf::<sha2::Sha256>::new(Some(&self.auth), shared.raw_secret_bytes()).expand(&info, &mut ikm).unwrap();
        let prk = hkdf::Hkdf::<sha2::Sha256>::new(Some(salt), &ikm);
        let (mut key, mut nonce) = ([0u8; 16], [0u8; 12]);
        prk.expand(b"Content-Encoding: aes128gcm\0", &mut key).unwrap();
        prk.expand(b"Content-Encoding: nonce\0", &mut nonce).unwrap();
        let mut plain = aes_gcm::Aes128Gcm::new(&key.into()).decrypt(&nonce.into(), cipher).unwrap();
        assert_eq!(plain.pop(), Some(2), "a single, last record");
        plain
    }
}

#[test]
fn a_push_is_encrypted_for_the_device_and_signed_with_the_servers_key() {
    let roots = roots("push-send");
    let (service, device) = (FakePush::start(), Device::new());
    let subscription = device.subscription(service.endpoint("one"));
    orq_remote::web_push::send(&roots, &subscription, br#"{"kind":"needs_input"}"#).expect("sent");

    let requests = service.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.path, "/push/one");
    assert_eq!(request.header("content-encoding"), Some("aes128gcm"));
    assert!(request.header("ttl").is_some());
    let public = orq_remote::web_push::vapid_public_key(&roots).unwrap();
    let authorization = request.header("authorization").unwrap();
    assert!(authorization.starts_with("vapid t=") && authorization.ends_with(&format!(", k={public}")), "{authorization}");
    assert_eq!(device.decrypt(&request.body), br#"{"kind":"needs_input"}"#);
    // Nothing readable travels in the clear.
    assert!(!String::from_utf8_lossy(&request.body).contains("needs_input"));
}

#[test]
fn an_endpoint_with_curl_glob_characters_is_one_literal_request() {
    let roots = roots("push-glob");
    let (service, device) = (FakePush::start(), Device::new());
    let subscription = device.subscription(service.endpoint("{a,b}[1-2]"));
    let _ = orq_remote::web_push::send(&roots, &subscription, b"{}");
    let paths: Vec<String> = service.requests().into_iter().map(|r| r.path).collect();
    assert_eq!(paths, vec!["/push/{a,b}[1-2]".to_string()], "the endpoint is taken literally, never expanded");
}

#[test]
fn the_longest_message_fills_a_4096_byte_body_and_one_more_byte_is_refused() {
    let roots = roots("push-limit");
    let (service, device) = (FakePush::start(), Device::new());
    let subscription = device.subscription(service.endpoint("one"));
    // RFC 8291: 4096 less the 86-byte header, the 16-byte tag and the padding delimiter.
    orq_remote::web_push::send(&roots, &subscription, &[b'x'; 3993]).expect("the longest message is sent");
    let requests = service.requests();
    assert_eq!(requests[0].body.len(), 4096);
    assert_eq!(device.decrypt(&requests[0].body).len(), 3993);

    assert!(orq_remote::web_push::send(&roots, &subscription, &[b'x'; 3994]).is_err());
    assert_eq!(service.requests().len(), 1, "a message too long for a 4096-byte body is never sent");
}

fn b64(text: &str) -> Vec<u8> {
    orq_remote::web_push::base64url_decode(text).expect("base64url")
}

#[test]
fn the_vapid_key_file_is_private_and_only_the_public_key_is_served() {
    let roots = roots("push-vapid");
    let public = orq_remote::web_push::vapid_public_key(&roots).unwrap();
    let raw = b64(&public);
    assert_eq!((raw.len(), raw[0]), (65, 4), "an uncompressed P-256 point");

    let file = orq_remote::web_push::vapid_key_file(&roots);
    assert!(file.starts_with(&roots.config), "{file:?}");
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);

    // The same key comes back, and the private half is nowhere in what is served.
    assert_eq!(orq_remote::web_push::vapid_public_key(&roots).unwrap(), public);
    let stored = std::fs::read_to_string(&file).unwrap();
    assert!(!stored.trim().is_empty() && !public.contains(stored.trim()));
}

#[test]
fn first_uses_at_once_all_get_the_one_key_that_was_kept() {
    for round in 0..20 {
        let roots = roots(&format!("push-vapid-race-{round}"));
        let start = std::sync::Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (roots, start) = (roots.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    orq_remote::web_push::vapid_public_key(&roots)
                })
            })
            .collect();
        let keys: Vec<String> = threads.into_iter().map(|t| t.join().unwrap().expect("a first use never sees a half-written key")).collect();
        assert!(keys.iter().all(|k| *k == keys[0]), "one key for everyone: {keys:?}");
        assert_eq!(orq_remote::web_push::vapid_public_key(&roots).unwrap(), keys[0], "the key kept is the one handed out");
        let left: Vec<_> = std::fs::read_dir(&roots.config).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, vec![std::ffi::OsString::from("vapid-key")], "no scratch files are left behind");
    }
}

const TOKEN: &str ="web-push-test-token-0123456789abcdef";

struct NoProjects;
impl orq_core::remote_inbox::ProjectsProvider for NoProjects {
    fn projects(&self) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!([]))
    }
}

/// A server with passkey sign-in, with or without the bearer token being honoured on this machine's loopback.
fn serve(roots: &StorageRoots, loopback_token: bool) -> orq_remote::Server {
    let state = std::sync::Arc::new(orq_remote::ServerState { roots: roots.clone(), config: orq_core::remote_inbox::RemoteConfig { token: TOKEN.to_string() }, project_id: "p-0a1b2c3d".into(), project_name: "orq-cli".into() });
    let host = orq_core::passkey_register::PasskeyHost::from_setting("box.tail1234.ts.net").unwrap();
    let auth = std::sync::Arc::new(orq_core::passkey_routes::PasskeyAuth::new(host).with_loopback_token(loopback_token));
    orq_remote::Server::start_with_auth(std::net::TcpListener::bind("127.0.0.1:0").unwrap(), state, std::sync::Arc::new(NoProjects), auth).unwrap()
}

/// One request; the status and the JSON body. With a session it sends the cookie, origin and CSRF token a signed-in page does.
fn call(server: &orq_remote::Server, method: &str, path: &str, session: Option<&web_push_session::Session>, body: &serde_json::Value) -> (u16, serde_json::Value) {
    let body = if body.is_null() { Vec::new() } else { body.to_string().into_bytes() };
    let origin = format!("https://{}", web_push_session::RP);
    let headers: Vec<(&str, &str)> = match session {
        Some(s) => vec![("Cookie", &s.cookie), ("Origin", &origin), ("X-CSRF-Token", &s.csrf)],
        None => vec![],
    };
    let (status, _, body) = web_push_session::raw(server.addr(), method, path, &headers, &body);
    (status, serde_json::from_str(&body).unwrap_or(serde_json::Value::Null))
}

#[test]
fn send_test_pushes_one_test_message_to_that_device_and_a_410_deletes_it() {
    let roots = roots("push-test");
    let (service, one, two) = (FakePush::start(), Device::new(), Device::new());
    let server = serve(&roots, false);
    let session = web_push_session::sign_in(server.addr(), TOKEN);
    let session = Some(&session);
    let none = serde_json::Value::Null;
    let id_one = call(&server, "POST", "/api/push/subscriptions", session, &subscribe_body(&one, service.endpoint("one"))).1["id"].as_i64().unwrap();
    let id_two = call(&server, "POST", "/api/push/subscriptions", session, &subscribe_body(&two, service.endpoint("two"))).1["id"].as_i64().unwrap();

    // Signed out, nothing is pushed.
    let (status, _) = call(&server, "POST", &format!("/api/push/subscriptions/{id_one}/test"), None, &none);
    assert_eq!((status, service.requests().len()), (403, 0));

    let (status, result) = call(&server, "POST", &format!("/api/push/subscriptions/{id_one}/test"), session, &none);
    assert_eq!((status, &result["sent"]), (200, &serde_json::json!(true)), "{result}");
    let requests = service.requests();
    assert_eq!((requests.len(), requests[0].path.as_str()), (1, "/push/one"), "that device only");
    assert_eq!(one.decrypt(&requests[0].body), br#"{"kind":"test"}"#);

    // The push service's own error comes back next to the device, and a gone subscription is deleted.
    service.answers.lock().unwrap().push_back(500);
    let (_, result) = call(&server, "POST", &format!("/api/push/subscriptions/{id_two}/test"), session, &none);
    assert_eq!(result["sent"], false);
    assert!(result["error"].as_str().unwrap().contains("500"), "{result}");
    assert_eq!(call(&server, "GET", "/api/push/subscriptions", session, &none).1["subscriptions"].as_array().unwrap().len(), 2);

    service.answers.lock().unwrap().push_back(410);
    let (_, result) = call(&server, "POST", &format!("/api/push/subscriptions/{id_two}/test"), session, &none);
    assert_eq!(result["sent"], false);
    let listed = call(&server, "GET", "/api/push/subscriptions", session, &none).1;
    let ids: Vec<i64> = listed["subscriptions"].as_array().unwrap().iter().map(|d| d["id"].as_i64().unwrap()).collect();
    assert_eq!(ids, vec![id_one]);
    assert_eq!(call(&server, "POST", "/api/push/subscriptions/999/test", session, &none).0, 404);
}

/// A store, a push service and one subscribed device.
fn subscribed(label: &str) -> (StorageRoots, orq_core::store::Store, FakePush, Device) {
    let roots = roots(label);
    let (service, device) = (FakePush::start(), Device::new());
    let store = orq_core::store::Store::open(&roots.database()).unwrap();
    let subscription = device.subscription(service.endpoint("one"));
    store.add_push_subscription(&orq_core::store::push::NewSubscription { endpoint: subscription.endpoint, p256dh: subscription.p256dh, auth: subscription.auth, label: "phone".into() }, 1).unwrap();
    (roots, store, service, device)
}

fn event<'a>(kind: &'a str, run: Option<&'a str>) -> orq_remote::web_push::PushEvent<'a> {
    orq_remote::web_push::PushEvent { kind, project_id: "p-1", run_id: run, event_id: Some("e-7") }
}

#[test]
fn a_needs_input_event_sends_one_push_with_only_the_four_ids_and_a_repeat_sends_none() {
    let (roots, _store, service, device) = subscribed("push-needs");
    assert_eq!(orq_remote::web_push::notify(&roots, &event("needs_input", Some("run-1"))), 1);
    let requests = service.requests();
    assert_eq!(requests.len(), 1);
    let payload: serde_json::Value = serde_json::from_slice(&device.decrypt(&requests[0].body)).unwrap();
    assert_eq!(payload, serde_json::json!({"kind": "needs_input", "project": "p-1", "run": "run-1", "event": "e-7"}));

    // The same run and kind inside five minutes: nothing new. Another run, or another kind, still goes.
    assert_eq!(orq_remote::web_push::notify(&roots, &event("needs_input", Some("run-1"))), 0);
    assert_eq!(service.requests().len(), 1);
    assert_eq!(orq_remote::web_push::notify(&roots, &event("failed", Some("run-1"))), 1);
    assert_eq!(orq_remote::web_push::notify(&roots, &event("needs_input", Some("run-2"))), 1);
    assert_eq!(service.requests().len(), 3);
}

#[test]
fn a_muted_project_or_a_switched_off_kind_sends_nothing() {
    let (roots, store, service, _device) = subscribed("push-muted");
    store.set_push_project_muted("p-1", true).unwrap();
    assert_eq!(orq_remote::web_push::notify(&roots, &event("failed", Some("run-1"))), 0);
    store.set_push_project_muted("p-1", false).unwrap();
    store.set_push_kind("failed", false).unwrap();
    assert_eq!(orq_remote::web_push::notify(&roots, &event("failed", Some("run-2"))), 0);
    assert_eq!(service.requests().len(), 0);
    // A muted project's run did not use up its window.
    store.set_push_kind("failed", true).unwrap();
    assert_eq!(orq_remote::web_push::notify(&roots, &event("failed", Some("run-1"))), 1);
}

#[test]
fn finished_and_queue_kinds_send_and_started_waits_until_switched_on() {
    let (roots, store, service, _device) = subscribed("push-kinds");
    assert_eq!(orq_remote::web_push::notify(&roots, &event("finished", Some("run-1"))), 1);
    assert_eq!(orq_remote::web_push::notify(&roots, &event("finished", Some("run-2"))), 1);
    assert_eq!(orq_remote::web_push::notify(&roots, &event("started", Some("run-3"))), 0, "off by default");
    store.set_push_kind("started", true).unwrap();
    assert_eq!(orq_remote::web_push::notify(&roots, &event("started", Some("run-3"))), 1);
    assert_eq!(service.requests().len(), 3);

    // A queue is the project's: no run, one push each time it pauses or runs out.
    assert_eq!(orq_remote::web_push::notify(&roots, &event("queue_paused", None)), 1);
    assert_eq!(orq_remote::web_push::notify(&roots, &event("queue_paused", None)), 0);
    assert_eq!(orq_remote::web_push::notify(&roots, &event("queue_finished", None)), 1);
    assert_eq!(service.requests().len(), 5);
}

fn subscribe_body(device: &Device, endpoint: String) -> serde_json::Value {
    let subscription = device.subscription(endpoint);
    serde_json::json!({"endpoint": subscription.endpoint, "keys": {"p256dh": subscription.p256dh, "auth": subscription.auth}, "label": "Firefox on Linux"})
}

#[test]
fn subscribing_listing_and_removing_a_device_needs_a_signed_in_session() {
    let roots = roots("push-routes");
    let (service, device) = (FakePush::start(), Device::new());
    let body = subscribe_body(&device, service.endpoint("one"));
    let none = serde_json::Value::Null;

    // Signed out (no passkey session, and the token does not stand in for one): nothing is stored or shown.
    let server = serve(&roots, false);
    assert_eq!(call(&server, "POST", "/api/push/subscriptions", None, &body).0, 403);
    assert_eq!(call(&server, "DELETE", "/api/push/subscriptions/1", None, &none).0, 403);
    assert_eq!(call(&server, "GET", "/api/push/subscriptions", None, &none).0, 401);
    assert_eq!(call(&server, "GET", "/api/push/key", None, &none).0, 401);

    let session = web_push_session::sign_in(server.addr(), TOKEN);
    let session = Some(&session);
    let (status, key) = call(&server, "GET", "/api/push/key", session, &none);
    assert_eq!((status, key["public_key"].as_str()), (200, Some(orq_remote::web_push::vapid_public_key(&roots).unwrap().as_str())));
    assert_eq!(key.as_object().unwrap().len(), 1, "only the public key: {key}");

    let (status, made) = call(&server, "POST", "/api/push/subscriptions", session, &body);
    assert_eq!(status, 201, "{made}");
    let id = made["id"].as_i64().expect("an id");
    // The same device subscribing again is still one device.
    assert_eq!(call(&server, "POST", "/api/push/subscriptions", session, &body).0, 201);

    let (status, listed) = call(&server, "GET", "/api/push/subscriptions", session, &none);
    let devices = listed["subscriptions"].as_array().unwrap();
    assert_eq!((status, devices.len()), (200, 1), "{listed}");
    assert_eq!((devices[0]["id"].as_i64(), devices[0]["label"].as_str()), (Some(id), Some("Firefox on Linux")));
    assert!(devices[0]["created_at"].as_i64().unwrap() > 0);
    // The device's keys and address never come back out.
    assert!(!listed.to_string().contains("127.0.0.1") && !listed.to_string().contains("p256dh"), "{listed}");

    // Not a subscription: refused, nothing stored.
    assert_eq!(call(&server, "POST", "/api/push/subscriptions", session, &serde_json::json!({"endpoint": "ftp://x", "keys": {"p256dh": "a", "auth": "b"}})).0, 400);

    assert_eq!(call(&server, "DELETE", &format!("/api/push/subscriptions/{id}"), session, &none).0, 200);
    assert_eq!(call(&server, "DELETE", &format!("/api/push/subscriptions/{id}"), session, &none).0, 404);
    assert_eq!(call(&server, "GET", "/api/push/subscriptions", session, &none).1["subscriptions"].as_array().unwrap().len(), 0);
}
