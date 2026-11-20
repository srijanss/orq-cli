//! Web push: the server's VAPID key pair, and (in later parts of this module) the encrypted
//! messages sent to subscribed devices. The private half of the key never leaves the config dir.

use std::io::{Error, ErrorKind, Result, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use orq_core::storage::StorageRoots;
use p256::ecdsa::SigningKey;
use p256::elliptic_curve::rand_core::OsRng;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Unpadded URL-safe base64, as push keys and tokens are written.
pub fn base64url_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// The bytes of unpadded (or padded) URL-safe base64; `None` when the text is not that.
pub fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=');
    if text.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    for chunk in text.as_bytes().chunks(4) {
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            let v = ALPHABET.iter().position(|a| a == c)? as u32;
            n |= v << (18 - 6 * i);
        }
        for i in 0..chunk.len() - 1 {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

/// Where the VAPID private key is kept: the config dir, readable by its owner only.
pub fn vapid_key_file(roots: &StorageRoots) -> PathBuf {
    roots.config.join("vapid-key")
}

/// The server's VAPID signing key, made on first use.
pub(crate) fn vapid_key(roots: &StorageRoots) -> Result<SigningKey> {
    let file = vapid_key_file(roots);
    for _ in 0..2 {
        match std::fs::read_to_string(&file) {
            Ok(text) => {
                let bytes = base64url_decode(text.trim()).ok_or_else(|| Error::new(ErrorKind::InvalidData, "the VAPID key file is not readable"))?;
                return SigningKey::from_slice(&bytes).map_err(|_| Error::new(ErrorKind::InvalidData, "the VAPID key file does not hold a key"));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let key = SigningKey::random(&mut OsRng);
        if publish_new(&file, base64url_encode(&key.to_bytes()).as_bytes())? {
            return Ok(key);
        }
    }
    Err(Error::other("the VAPID key file could not be made"))
}

/// Puts `contents` at `file` only if nothing is there yet, never showing a half-written file: it is written in
/// full to a private scratch file first, then linked into place. `false` when another first use got there first.
fn publish_new(file: &std::path::Path, contents: &[u8]) -> Result<bool> {
    use p256::elliptic_curve::rand_core::RngCore;
    let scratch = file.with_extension(format!("{}-{:016x}.tmp", std::process::id(), OsRng.next_u64()));
    let written = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&scratch).and_then(|mut out| {
        out.write_all(contents)?;
        out.sync_all()
    });
    let linked = written.and_then(|()| std::fs::hard_link(&scratch, file));
    let _ = std::fs::remove_file(&scratch);
    match linked {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

/// How long one push may take, connecting included.
pub const SEND_TIMEOUT_SECS: u64 = 10;
/// The most a push message carries (RFC 8291 §4): a 4096-byte body less the 86-byte header, the 16-byte
/// AES-GCM tag and the padding delimiter.
const MAX_PAYLOAD: usize = 4096 - 86 - 16 - 1;

/// A device's push subscription as the browser gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub endpoint: String,
    /// The device's public key, base64url.
    pub p256dh: String,
    /// The device's 16-byte authentication secret, base64url.
    pub auth: String,
}

/// Why a push was not delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// The push service no longer knows the subscription (404 or 410).
    Gone,
    /// Anything else, said in words that name no endpoint.
    Failed(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Gone => f.write_str("the push service no longer knows this subscription"),
            SendError::Failed(why) => f.write_str(why),
        }
    }
}

/// `scheme://host[:port]` of an endpoint that may be pushed to: https anywhere, http only on this machine.
fn endpoint_origin(endpoint: &str) -> Option<String> {
    let (scheme, rest) = endpoint.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().filter(|a| !a.is_empty() && !a.contains('@') && !a.chars().any(|c| c.is_whitespace() || c.is_control()))?;
    let host = authority.rsplit_once(':').map_or(authority, |(host, _)| host);
    let local = host == "127.0.0.1" || host == "localhost";
    (scheme == "https" || (scheme == "http" && local)).then(|| format!("{scheme}://{authority}"))
}

/// The message as RFC 8291 (`aes128gcm`) sends it to `subscription`.
pub(crate) fn encrypt(subscription: &Subscription, payload: &[u8]) -> std::result::Result<Vec<u8>, String> {
    use aes_gcm::aead::Aead;
    use aes_gcm::KeyInit;
    use p256::elliptic_curve::rand_core::RngCore;
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    if payload.len() > MAX_PAYLOAD {
        return Err("the push message is too long".to_string());
    }
    let device = base64url_decode(&subscription.p256dh).and_then(|bytes| p256::PublicKey::from_sec1_bytes(&bytes).ok()).ok_or("the subscription's key is not valid")?;
    let auth = base64url_decode(&subscription.auth).filter(|auth| auth.len() == 16).ok_or("the subscription's secret is not valid")?;
    let ephemeral = p256::ecdh::EphemeralSecret::random(&mut OsRng);
    let sender = p256::PublicKey::from(&ephemeral).to_encoded_point(false);
    let shared = ephemeral.diffie_hellman(&device);
    let mut info = b"WebPush: info\0".to_vec();
    info.extend_from_slice(device.to_encoded_point(false).as_bytes());
    info.extend_from_slice(sender.as_bytes());
    let mut ikm = [0u8; 32];
    hkdf::Hkdf::<sha2::Sha256>::new(Some(&auth), shared.raw_secret_bytes()).expand(&info, &mut ikm).map_err(|_| "key derivation failed")?;
    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    let prk = hkdf::Hkdf::<sha2::Sha256>::new(Some(&salt), &ikm);
    let (mut key, mut nonce) = ([0u8; 16], [0u8; 12]);
    prk.expand(b"Content-Encoding: aes128gcm\0", &mut key).map_err(|_| "key derivation failed")?;
    prk.expand(b"Content-Encoding: nonce\0", &mut nonce).map_err(|_| "key derivation failed")?;
    let mut record = payload.to_vec();
    record.push(2);
    let sealed = aes_gcm::Aes128Gcm::new(&key.into()).encrypt(&nonce.into(), record.as_slice()).map_err(|_| "encryption failed")?;
    let mut body = Vec::with_capacity(21 + sender.len() + sealed.len());
    body.extend_from_slice(&salt);
    body.extend_from_slice(&4096u32.to_be_bytes());
    body.push(sender.len() as u8);
    body.extend_from_slice(sender.as_bytes());
    body.extend_from_slice(&sealed);
    Ok(body)
}

/// The `Authorization` value that proves the push comes from this server (RFC 8292).
fn vapid_authorization(roots: &StorageRoots, origin: &str) -> Result<String> {
    use p256::ecdsa::signature::Signer;
    let key = vapid_key(roots)?;
    let expires = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) + 12 * 3600;
    let claims = serde_json::json!({"aud": origin, "exp": expires, "sub": "mailto:orq@localhost"});
    let signing_input = format!("{}.{}", base64url_encode(br#"{"typ":"JWT","alg":"ES256"}"#), base64url_encode(claims.to_string().as_bytes()));
    let signature: p256::ecdsa::Signature = key.sign(signing_input.as_bytes());
    let public = base64url_encode(key.verifying_key().to_encoded_point(false).as_bytes());
    Ok(format!("vapid t={signing_input}.{}, k={public}", base64url_encode(&signature.to_bytes())))
}

/// Pushes `payload` to `subscription`, encrypted for it; waits at most `SEND_TIMEOUT_SECS`.
pub fn send(roots: &StorageRoots, subscription: &Subscription, payload: &[u8]) -> std::result::Result<(), SendError> {
    use std::process::{Command, Stdio};
    let origin = endpoint_origin(&subscription.endpoint).ok_or_else(|| SendError::Failed("the subscription's endpoint is not one to push to".to_string()))?;
    let body = encrypt(subscription, payload).map_err(SendError::Failed)?;
    let authorization = vapid_authorization(roots, &origin).map_err(|error| SendError::Failed(format!("the server's push key could not be read: {error}")))?;
    let mut curl = Command::new("curl")
        // --globoff: an endpoint is one literal URL, never a curl pattern that fans out into many requests.
        .args(["--silent", "--show-error", "--globoff", "--max-time", &SEND_TIMEOUT_SECS.to_string(), "--proto", "=http,https", "--request", "POST", "--output", "/dev/null", "--write-out", "%{http_code}"])
        .args(["--header", "Content-Encoding: aes128gcm", "--header", "Content-Type: application/octet-stream", "--header", "TTL: 3600", "--header", "Urgency: high"])
        .args(["--header", &format!("Authorization: {authorization}"), "--data-binary", "@-", "--url", &subscription.endpoint])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| SendError::Failed("curl could not be started".to_string()))?;
    if let Some(mut stdin) = curl.stdin.take() {
        let _ = stdin.write_all(&body);
    }
    let output = curl.wait_with_output().map_err(|_| SendError::Failed("curl did not finish".to_string()))?;
    let status: u16 = String::from_utf8_lossy(&output.stdout).trim().parse().unwrap_or(0);
    match status {
        200..=299 => Ok(()),
        404 | 410 => Err(SendError::Gone),
        0 => Err(SendError::Failed("the push service could not be reached".to_string())),
        other => Err(SendError::Failed(format!("the push service answered {other}"))),
    }
}

/// Pushes `payload` to one stored device; a device the push service no longer knows is deleted.
pub fn deliver(roots: &StorageRoots, store: &orq_core::store::Store, device: &orq_core::store::push::PushSubscription, payload: &[u8]) -> std::result::Result<(), SendError> {
    let subscription = Subscription { endpoint: device.endpoint.clone(), p256dh: device.p256dh.clone(), auth: device.auth.clone() };
    let result = send(roots, &subscription, payload);
    if result == Err(SendError::Gone) {
        let _ = store.remove_push_subscription(device.id);
    }
    result
}

/// Every kind of notification Settings can switch on or off.
pub const KINDS: [&str; 10] = ["needs_input", "failed", "stalled", "review", "merged", "finished", "started", "queue_paused", "queue_finished", "restarted"];
/// The kinds that stay off until switched on.
const OFF_BY_DEFAULT: [&str; 1] = ["started"];
/// A (run, kind) is notified at most once in this long, in seconds.
pub const REPEAT_WINDOW_SECS: i64 = 300;

/// Something worth a notification. Only these four ids ever reach the push service.
pub struct PushEvent<'a> {
    pub kind: &'a str,
    pub project_id: &'a str,
    pub run_id: Option<&'a str>,
    pub event_id: Option<&'a str>,
}

/// Pushes `event` to every subscribed device, one at a time, unless its kind is off, its project is muted or
/// the same run and kind was notified in the last five minutes. Returns how many devices were pushed to.
/// A failure is never an error to the caller: a run must not feel a push service.
pub fn notify(roots: &StorageRoots, event: &PushEvent) -> usize {
    let Ok(store) = orq_core::store::Store::open(&roots.database()) else { return 0 };
    let Ok(choices) = store.push_kind_choices() else { return 0 };
    let enabled = choices.iter().find(|(kind, _)| kind == event.kind).map_or(!OFF_BY_DEFAULT.contains(&event.kind), |(_, on)| *on);
    if !enabled || !KINDS.contains(&event.kind) || store.push_muted_projects().is_ok_and(|muted| muted.iter().any(|p| p == event.project_id)) {
        return 0;
    }
    let devices = match store.push_subscriptions() {
        Ok(devices) if !devices.is_empty() => devices,
        _ => return 0,
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    // A queue has no run: it is the project's own.
    let subject = event.run_id.map_or_else(|| format!("project:{}", event.project_id), str::to_string);
    if !store.claim_push(&subject, event.kind, now, REPEAT_WINDOW_SECS).unwrap_or(false) {
        return 0;
    }
    let payload = serde_json::json!({ "kind": event.kind, "project": event.project_id, "run": event.run_id, "event": event.event_id }).to_string();
    let mut sent = 0;
    for device in &devices {
        match deliver(roots, &store, device, payload.as_bytes()) {
            Ok(()) => sent += 1,
            Err(error) => eprintln!("orq-remote: push to device {} failed: {error}", device.id),
        }
    }
    sent
}

/// What `/api/push/*` answers: its status and JSON body. The caller has already judged the request's
/// credentials; this only reads the request.
pub fn route(roots: &StorageRoots, method: &str, path: &str, body: &[u8]) -> (u16, serde_json::Value) {
    use serde_json::json;
    let refuse = |code: u16, why: &str| (code, json!({ "error": why }));
    let open = || orq_core::store::Store::open(&roots.database()).map_err(|_| refuse(500, "the push settings could not be opened"));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let rest = path.strip_prefix("/api/push/").unwrap_or("");
    match (method, rest) {
        ("GET", "key") => match vapid_public_key(roots) {
            Ok(key) => (200, json!({ "public_key": key })),
            Err(_) => refuse(500, "the server's push key could not be read"),
        },
        ("GET", "subscriptions") => match open().and_then(|store| store.push_subscriptions().map_err(|_| refuse(500, "the subscriptions could not be read"))) {
            Ok(devices) => (200, json!({ "subscriptions": devices.iter().map(|d| json!({ "id": d.id, "label": d.label, "created_at": d.created_at })).collect::<Vec<_>>() })),
            Err(failure) => failure,
        },
        ("POST", "subscriptions") => {
            let Ok(request) = serde_json::from_slice::<serde_json::Value>(body) else { return refuse(400, "the request body is not JSON") };
            let text = |value: &serde_json::Value| value.as_str().map(str::to_string);
            let (Some(endpoint), Some(p256dh), Some(auth)) = (text(&request["endpoint"]), text(&request["keys"]["p256dh"]), text(&request["keys"]["auth"])) else {
                return refuse(400, "a subscription needs an endpoint and keys");
            };
            let candidate = Subscription { endpoint, p256dh, auth };
            if endpoint_origin(&candidate.endpoint).is_none() || candidate.endpoint.len() > 2048 || encrypt(&candidate, b"").is_err() {
                return refuse(400, "that is not a push subscription this server can use");
            }
            let label: String = request["label"].as_str().unwrap_or("").chars().filter(|c| !c.is_control()).take(80).collect();
            let label = if label.trim().is_empty() { "A device".to_string() } else { label };
            let new = orq_core::store::push::NewSubscription { endpoint: candidate.endpoint, p256dh: candidate.p256dh, auth: candidate.auth, label };
            match open().and_then(|store| store.add_push_subscription(&new, now).map_err(|_| refuse(500, "the subscription could not be stored"))) {
                Ok(id) => (201, json!({ "id": id })),
                Err(failure) => failure,
            }
        }
        ("POST", test) if test.starts_with("subscriptions/") && test.ends_with("/test") => {
            let Ok(id) = test["subscriptions/".len()..test.len() - "/test".len()].parse::<i64>() else { return refuse(404, "not found") };
            let Ok(store) = orq_core::store::Store::open(&roots.database()) else { return refuse(500, "the push settings could not be opened") };
            let Some(device) = store.push_subscriptions().ok().and_then(|all| all.into_iter().find(|d| d.id == id)) else { return refuse(404, "not found") };
            match deliver(roots, &store, &device, br#"{"kind":"test"}"#) {
                Ok(()) => (200, json!({ "sent": true })),
                Err(error) => (200, json!({ "sent": false, "error": error.to_string() })),
            }
        }
        ("DELETE", id) if id.starts_with("subscriptions/") => {
            let Some(id) = id["subscriptions/".len()..].parse::<i64>().ok() else { return refuse(404, "not found") };
            match open().and_then(|store| store.remove_push_subscription(id).map_err(|_| refuse(500, "the subscription could not be removed"))) {
                Ok(true) => (200, json!({ "removed": true })),
                Ok(false) => refuse(404, "not found"),
                Err(failure) => failure,
            }
        }
        _ => refuse(404, "not found"),
    }
}

/// The public half, as a browser's `applicationServerKey` takes it: the uncompressed point, base64url.
pub fn vapid_public_key(roots: &StorageRoots) -> Result<String> {
    let key = vapid_key(roots)?;
    Ok(base64url_encode(key.verifying_key().to_encoded_point(false).as_bytes()))
}
