//! The `/auth/*` routes: passkey enrolment and sign-in as plain request/response values, so the
//! HTTP server only has to carry bytes. Every decision lives in the modules these call.

use std::net::IpAddr;

use serde_json::{json, Value};

use crate::passkey_challenge::Challenges;
use crate::passkey_confirm::{self, Confirmations};
use crate::passkey_enroll::{EnrolAuthorizer, EnrolRequest};
use crate::passkey_login;
use crate::passkey_register::{self, PasskeyHost, RegistrationResponse};
use crate::passkey_store::PasskeyStore;
use crate::remote_inbox::{peer_allowed, RemoteConfig};
use crate::session::{self, Sessions};
use crate::storage::StorageRoots;

/// Longest request body any `/auth` route reads.
pub const MAX_AUTH_BODY: usize = 32 * 1024;
/// A session ends after twelve hours unused unless Settings say otherwise.
pub const SESSION_IDLE_SECS: u64 = 12 * 60 * 60;
/// A session ends seven days after sign-in unless Settings say otherwise.
pub const SESSION_MAX_SECS: u64 = 7 * 24 * 60 * 60;

pub struct AuthRequest<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub peer: IpAddr,
    pub authorization: Option<&'a str>,
    pub cookie: Option<&'a str>,
    /// The `Origin` header, which a change to the passkeys must carry as this server's own.
    pub origin: Option<&'a str>,
    /// The `X-CSRF-Token` header, which a change to the passkeys must carry as the session's own.
    pub csrf: Option<&'a str>,
    pub body: &'a [u8],
}

pub struct AuthReply {
    pub status: u16,
    pub body: Value,
    pub set_cookie: Option<String>,
}

fn reply(status: u16, body: Value) -> AuthReply {
    AuthReply { status, body, set_cookie: None }
}

fn refuse() -> AuthReply {
    reply(401, json!({ "error": "not allowed" }))
}

/// Everything passkey sign-in keeps in memory, for one server.
pub struct PasskeyAuth {
    host: PasskeyHost,
    challenges: Challenges,
    confirmations: Confirmations,
    authorizer: EnrolAuthorizer,
    sessions: Sessions,
    loopback_token: bool,
    clock: Clock,
    /// The access token once it has been rotated from the web; until then the one the server started with.
    rotated: std::sync::Mutex<Option<String>>,
}

/// Seconds since the epoch; a test can give its own.
pub type Clock = std::sync::Arc<dyn Fn() -> u64 + Send + Sync>;

/// How long a step-up opens the risky actions for, per session.
pub const STEP_UP_SECS: u64 = 5 * 60;

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl PasskeyAuth {
    pub fn new(host: PasskeyHost) -> PasskeyAuth {
        PasskeyAuth {
            host,
            challenges: Challenges::new(),
            confirmations: Confirmations::new(),
            authorizer: EnrolAuthorizer::new(),
            sessions: Sessions::with_limits(SESSION_IDLE_SECS, SESSION_MAX_SECS),
            loopback_token: true,
            clock: std::sync::Arc::new(now),
            rotated: std::sync::Mutex::new(None),
        }
    }

    /// The access token in force: the one rotated to from the web, or else `started_with`.
    pub fn token(&self, started_with: &str) -> String {
        self.rotated.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone().unwrap_or_else(|| started_with.to_string())
    }

    /// Reads the time from `clock` instead of the system's, for sessions, challenges and step-ups.
    pub fn with_clock(mut self, clock: Clock) -> PasskeyAuth {
        self.clock = clock;
        self
    }

    fn now(&self) -> u64 {
        (self.clock)()
    }

    /// Sets how long a session may go unused and how long it lasts in total, in seconds; the sessions
    /// already open are held to them at once.
    pub fn set_session_limits(&self, (idle_secs, max_secs): (u64, u64)) {
        self.sessions.set_limits(idle_secs, max_secs);
    }

    /// Whether the session in the `Cookie` header made a step-up less than [`STEP_UP_SECS`] ago.
    pub fn stepped_up(&self, cookie: Option<&str>) -> bool {
        cookie.and_then(session::id_from_cookie_header).is_some_and(|id| self.sessions.stepped_up_within(id, self.now(), STEP_UP_SECS))
    }

    /// Whether the access token opens the API on a direct local connection (it does unless switched off).
    /// Through a proxy the token never opens the API; it only enrols the first passkey.
    pub fn with_loopback_token(mut self, allowed: bool) -> PasskeyAuth {
        self.loopback_token = allowed;
        self
    }

    pub fn loopback_token(&self) -> bool {
        self.loopback_token
    }

    pub fn host(&self) -> &PasskeyHost {
        &self.host
    }

    /// Whether the `Cookie` header names a session that is live in memory; the passkey behind it may
    /// since have been removed, which only `session_live` looks at.
    pub fn has_session(&self, cookie: Option<&str>) -> bool {
        self.session_credential(cookie).is_some()
    }

    fn session_credential(&self, cookie: Option<&str>) -> Option<String> {
        cookie.and_then(session::id_from_cookie_header).and_then(|id| self.sessions.credential(id, self.now()))
    }

    /// Whether the `Cookie` header names a live session whose passkey is still registered. A session
    /// whose passkey was removed (on the machine, by another process) ends here; a store that cannot
    /// be read counts as not signed in.
    pub fn session_live(&self, roots: &StorageRoots, cookie: Option<&str>) -> bool {
        let Some(credential) = self.session_credential(cookie) else { return false };
        match PasskeyStore::open(roots).find(&credential) {
            Ok(Some(_)) => true,
            Ok(None) => {
                if let Some(id) = cookie.and_then(session::id_from_cookie_header) {
                    self.sessions.end(id);
                }
                false
            }
            Err(_) => false,
        }
    }

    /// The CSRF token of the live session in the `Cookie` header, for the signed-in page to send back.
    fn csrf_token(&self, cookie: Option<&str>) -> Option<String> {
        cookie.and_then(session::id_from_cookie_header).and_then(|id| self.sessions.csrf(id, self.now()))
    }

    /// Whether a state-changing request may go on: it comes from a live session, names this server's own
    /// origin, and carries that session's CSRF token. The access token alone never passes.
    pub fn change_allowed(&self, roots: &StorageRoots, cookie: Option<&str>, origin: Option<&str>, presented: Option<&str>) -> bool {
        let (Some(origin), Some(presented), Some(id)) = (origin, presented, cookie.and_then(session::id_from_cookie_header)) else { return false };
        origin == self.host.origin() && self.session_live(roots, cookie) && self.sessions.csrf_matches(id, presented, self.now())
    }

    /// Whether `assertion` confirms `purpose` for the live session in the `Cookie` header: it answers a
    /// challenge issued to that session for that purpose, from a registered passkey with user verification.
    pub fn confirm(&self, roots: &StorageRoots, cookie: Option<&str>, purpose: &str, assertion: &Value) -> bool {
        let Some(session) = cookie.and_then(session::id_from_cookie_header).filter(|_| self.session_live(roots, cookie)) else { return false };
        let Some(response) = passkey_confirm::assertion_from(assertion) else { return false };
        passkey_confirm::check(&self.host, &self.confirmations, &PasskeyStore::open(roots), purpose, session, &response, self.now()).is_ok()
    }

    /// The label of the passkey behind the live session in the `Cookie` header, which names who a web
    /// request came from in the audit log.
    pub fn session_device(&self, roots: &StorageRoots, cookie: Option<&str>) -> Option<String> {
        self.session_label(&PasskeyStore::open(roots), cookie)
    }

    /// The label of the passkey behind the session in the `Cookie` header, for the audit log.
    fn session_label(&self, store: &PasskeyStore, cookie: Option<&str>) -> Option<String> {
        let credential = self.session_credential(cookie)?;
        store.find(&credential).ok().flatten().map(|key| key.label)
    }

    /// The gate every passkey or session change goes through: a passkey session presenting its CSRF token
    /// from this server's origin. `act` runs only past the gate; every attempt is audited under `action`.
    fn manage(&self, roots: &StorageRoots, store: &PasskeyStore, request: &AuthRequest, live: bool, action: &'static str, act: impl FnOnce(&Audit) -> AuthReply) -> AuthReply {
        let identity = self.session_label(store, request.cookie).filter(|_| live).unwrap_or_else(|| "web".to_string());
        let audit = Audit { roots, identity, action };
        if !live {
            return audit.refused(403, "needs a passkey sign-in", needs_sign_in(action));
        }
        if !self.change_allowed(roots, request.cookie, request.origin, request.csrf) {
            return audit.refused(403, "missing or wrong CSRF token or origin", "not allowed");
        }
        act(&audit)
    }

    /// POST /auth/passkeys/{id}/label `{label}`: renames that passkey. Audited as passkey.rename.
    fn rename(&self, roots: &StorageRoots, store: &PasskeyStore, request: &AuthRequest, id: &str, live: bool) -> AuthReply {
        use crate::passkey_store::RenameError;
        self.manage(roots, store, request, live, "passkey.rename", |audit| {
            let body = serde_json::from_slice::<Value>(request.body).ok().filter(|body| body.as_object().is_some_and(|keys| keys.len() == 1));
            let Some(label) = body.as_ref().and_then(|body| body.get("label")).and_then(Value::as_str) else {
                return audit.refused(400, "bad request body", "the request body is not one this action takes");
            };
            let old = store.find(id).ok().flatten().map(|key| key.label);
            match store.rename(id, label) {
                Ok(()) => {
                    audit.allowed(&format!("renamed {} to {label}", old.unwrap_or_default()));
                    reply(200, json!({ "id": id, "label": label }))
                }
                Err(RenameError::BadLabel) => audit.refused(422, "bad label", "A name is 1 to 40 characters of printable text"),
                Err(RenameError::Unknown) => audit.refused(404, "no such passkey", "No passkey has that id"),
                Err(RenameError::Taken) => audit.refused(409, "label taken", "Another passkey already has that name"),
                Err(RenameError::Store(_)) => audit.refused(500, "the passkeys could not be written", "The passkeys could not be changed"),
            }
        })
    }

    /// POST /auth/passkeys/{id}/remove: removes that passkey, never the last one, and ends every session it
    /// opened. When that includes this session, the answer says `signed_out` and clears the cookie.
    /// Audited as passkey.remove.
    fn remove(&self, roots: &StorageRoots, store: &PasskeyStore, request: &AuthRequest, id: &str, live: bool) -> AuthReply {
        use crate::passkey_store::RemoveError;
        self.manage(roots, store, request, live, "passkey.remove", |audit| {
            let own = self.session_credential(request.cookie).is_some_and(|credential| credential == id);
            match store.remove_by_id(id) {
                Ok(label) => {
                    self.sessions.end_for(id);
                    audit.allowed(&format!("removed {label}"));
                    let body = json!({ "id": id, "label": label, "signed_out": own });
                    AuthReply { status: 200, body, set_cookie: own.then(session::clear_cookie) }
                }
                Err(RemoveError::Unknown) => audit.refused(404, "no such passkey", "No passkey has that id"),
                Err(RemoveError::Last) => audit.refused(409, "the last passkey", "Add another passkey before removing this one"),
                Err(RemoveError::Store(_)) => audit.refused(500, "the passkeys could not be written", "The passkeys could not be changed"),
            }
        })
    }

    /// POST /auth/sessions/end-all: ends every passkey session, this one included, and clears this session's
    /// cookie. Audited as sessions.end_all.
    fn end_all(&self, roots: &StorageRoots, store: &PasskeyStore, request: &AuthRequest, live: bool) -> AuthReply {
        self.manage(roots, store, request, live, END_ALL, |audit| {
            let ended = self.sessions.count();
            self.sessions.end_all();
            audit.allowed(&format!("ended {ended} sessions"));
            AuthReply { status: 200, body: json!({ "signed_out": true, "ended": ended }), set_cookie: Some(session::clear_cookie()) }
        })
    }

    /// GET /auth/sessions: the open sessions, newest first, each with the passkey (device) that opened it.
    fn list_sessions(&self, store: &PasskeyStore, request: &AuthRequest) -> AuthReply {
        let Ok(keys) = store.list() else { return refuse() };
        let own = request.cookie.and_then(session::id_from_cookie_header).unwrap_or_default();
        let label = |credential: &str| keys.iter().find(|key| key.credential_id == credential).map(|key| key.label.clone());
        let rows: Vec<Value> = self
            .sessions
            .list(own, self.now())
            .into_iter()
            .map(|row| json!({ "id": row.handle, "device": label(&row.credential), "created": row.created, "last_used": row.last_used, "this_session": row.this_session }))
            .collect();
        reply(200, json!({ "sessions": rows }))
    }

    /// POST /auth/sessions/{id}/revoke: ends the session the list names `handle`. Revoking this session signs
    /// it out and clears its cookie. Audited as session.revoke.
    fn revoke(&self, roots: &StorageRoots, store: &PasskeyStore, request: &AuthRequest, handle: &str, live: bool) -> AuthReply {
        self.manage(roots, store, request, live, REVOKE, |audit| {
            let own = request.cookie.and_then(session::id_from_cookie_header).unwrap_or_default();
            let own = self.sessions.list(own, self.now()).into_iter().any(|row| row.this_session && row.handle == handle);
            if !self.sessions.revoke(handle) {
                return audit.refused(404, "no such session", "No open session has that id");
            }
            audit.allowed("ended one session");
            AuthReply { status: 200, body: json!({ "revoked": true, "signed_out": own }), set_cookie: own.then(session::clear_cookie) }
        })
    }

    /// POST /auth/token/rotate: a new access token, written to the token file and answered once; the old one
    /// stops working at once. Needs a recent step-up. Audited as token.rotate, never with either token.
    fn rotate_token(&self, roots: &StorageRoots, store: &PasskeyStore, request: &AuthRequest, live: bool) -> AuthReply {
        self.manage(roots, store, request, live, ROTATE, |audit| {
            let empty = request.body.iter().all(u8::is_ascii_whitespace) || serde_json::from_slice::<Value>(request.body).ok() == Some(json!({}));
            if !empty {
                return audit.refused(400, "bad request body", "the request body is not one this action takes");
            }
            if !self.stepped_up(request.cookie) {
                return audit.refused(401, "step-up required", "step_up_required");
            }
            match RemoteConfig::rotate(roots) {
                Ok(config) => {
                    *self.rotated.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(config.token.clone());
                    audit.allowed("rotated the access token");
                    reply(200, json!({ "token": config.token }))
                }
                Err(_) => audit.refused(500, "the token file could not be written", "The token could not be rotated"),
            }
        })
    }

    /// POST /auth/step-up `{assertion}`: the passkey answers a `step_up` challenge issued to this session, which
    /// then may take the risky actions for [`STEP_UP_SECS`]. Audited as session.step_up.
    fn step_up(&self, roots: &StorageRoots, store: &PasskeyStore, request: &AuthRequest, live: bool) -> AuthReply {
        self.manage(roots, store, request, live, STEP_UP, |audit| {
            let body = serde_json::from_slice::<Value>(request.body).ok().filter(|body| body.as_object().is_some_and(|keys| keys.len() == 1));
            let Some(assertion) = body.as_ref().and_then(|body| body.get("assertion")) else {
                return audit.refused(400, "bad request body", "the request body is not one this action takes");
            };
            if !self.confirm(roots, request.cookie, "step_up", assertion) {
                return audit.refused(401, "passkey check failed", "The passkey check did not pass");
            }
            let marked = request.cookie.and_then(session::id_from_cookie_header).is_some_and(|id| self.sessions.mark_stepped_up(id, self.now()));
            if !marked {
                return audit.refused(401, "the session ended", "not allowed");
            }
            audit.allowed("confirmed with the passkey");
            reply(200, json!({ "stepped_up": true, "for_secs": STEP_UP_SECS }))
        })
    }

    /// The answer for an `/auth/...` request; `None` for any other path.
    pub fn handle(&self, roots: &StorageRoots, config: &RemoteConfig, request: &AuthRequest) -> Option<AuthReply> {
        let route = request.path.strip_prefix("/auth/")?;
        if !peer_allowed(request.peer) {
            return Some(refuse());
        }
        let store = PasskeyStore::open(roots);
        let at = self.now();
        let live = self.session_live(roots, request.cookie);
        let post = request.method == "POST";
        let enrol = || self.authorizer.authorize(&store, config, &EnrolRequest { peer: request.peer, authorization: request.authorization, session_live: live }, at);
        let fields = || serde_json::from_slice::<Value>(request.body).ok().filter(Value::is_object);
        let text = |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
        if let Some((id, verb)) = route.strip_prefix("passkeys/").and_then(|rest| rest.split_once('/')).filter(|(id, _)| !id.is_empty()) {
            match (post, verb) {
                (true, "label") => return Some(self.rename(roots, &store, request, id, live)),
                (true, "remove") => return Some(self.remove(roots, &store, request, id, live)),
                _ => {}
            }
        }
        if let Some(handle) = route.strip_prefix("sessions/").and_then(|rest| rest.strip_suffix("/revoke")).filter(|handle| !handle.is_empty() && !handle.contains('/')) {
            if post {
                return Some(self.revoke(roots, &store, request, handle, live));
            }
        }
        Some(match (request.method, route) {
            ("GET", "sessions") if live => self.list_sessions(&store, request),
            ("GET", "sessions") => refuse(),
            ("GET", "status") => {
                let mut status = json!({ "passkeys": store.any_registered(), "signed_in": live, "host": self.host.rp_id() });
                if let Some(token) = self.csrf_token(request.cookie).filter(|_| live) {
                    status["csrf"] = json!(token);
                }
                reply(200, status)
            }
            ("GET", "passkeys") if live => match store.list() {
                Ok(keys) => {
                    let own = self.session_credential(request.cookie);
                    let rows = keys.iter().map(|key| json!({ "label": key.label, "id": key.credential_id, "this_device": own.as_deref() == Some(key.credential_id.as_str()) }));
                    reply(200, json!({ "passkeys": rows.collect::<Vec<_>>() }))
                }
                Err(_) => refuse(),
            },
            ("GET", "passkeys") => refuse(),
            ("POST", "register/begin") if post => match enrol() {
                Err(_) => refuse(),
                Ok(_) => match passkey_register::begin(&self.host, &self.challenges, &store, at) {
                    Ok(options) => reply(200, options),
                    Err(_) => reply(500, json!({ "error": "could not start" })),
                },
            },
            ("POST", "register/finish") => match (enrol(), fields()) {
                (Err(_), _) => refuse(),
                (Ok(_), None) => reply(400, json!({ "error": "bad request" })),
                (Ok(how), Some(body)) => {
                    let (Some(client_data_json), Some(attestation_object)) = (text(&body, "clientDataJSON"), text(&body, "attestationObject")) else {
                        return Some(reply(400, json!({ "error": "bad request" })));
                    };
                    let label = text(&body, "label").unwrap_or_else(|| "passkey".to_string());
                    let response = RegistrationResponse { client_data_json, attestation_object, label };
                    match passkey_register::finish(&self.host, &self.challenges, &self.authorizer, &store, how, &response, at) {
                        Ok(key) => reply(200, json!({ "label": key.label })),
                        Err(_) => reply(400, json!({ "error": "the passkey was not accepted" })),
                    }
                }
            },
            ("POST", "login/begin") => match passkey_login::begin(&self.host, &self.challenges, &store, at) {
                Ok(options) => reply(200, options),
                Err(_) => refuse(),
            },
            ("POST", "login/finish") => {
                let Some(response) = fields().as_ref().and_then(passkey_confirm::assertion_from) else { return Some(refuse()) };
                match passkey_login::finish(&self.host, &self.challenges, &store, &self.sessions, &response, at) {
                    Ok(issued) => AuthReply { status: 200, body: json!({ "signed_in": true }), set_cookie: Some(issued.cookie) },
                    Err(_) => refuse(),
                }
            }
            ("POST", "confirm/begin") => {
                let Some(session) = request.cookie.and_then(session::id_from_cookie_header).filter(|_| live) else { return Some(refuse()) };
                let purpose = fields().filter(|body| body.as_object().is_some_and(|keys| keys.len() == 1)).and_then(|body| text(&body, "purpose"));
                let Some(purpose) = purpose.filter(|purpose| passkey_confirm::PURPOSES.contains(&purpose.as_str())) else {
                    return Some(reply(400, json!({ "error": "bad request" })));
                };
                match passkey_confirm::begin(&self.host, &self.confirmations, &store, &purpose, session, at) {
                    Ok(options) => reply(200, options),
                    Err(_) => reply(500, json!({ "error": "could not start" })),
                }
            }
            ("POST", "sessions/end-all") => self.end_all(roots, &store, request, live),
            ("POST", "step-up") => self.step_up(roots, &store, request, live),
            ("POST", "token/rotate") => self.rotate_token(roots, &store, request, live),
            ("POST", "logout") => {
                if let Some(id) = request.cookie.and_then(session::id_from_cookie_header) {
                    self.sessions.end(id);
                }
                AuthReply { status: 200, body: json!({ "signed_in": false }), set_cookie: Some(session::clear_cookie()) }
            }
            _ => reply(404, json!({ "error": "not found" })),
        })
    }
}

const END_ALL: &str = "sessions.end_all";
const STEP_UP: &str = "session.step_up";
const REVOKE: &str = "session.revoke";
const ROTATE: &str = "token.rotate";

/// What a change refused for want of a passkey session says.
fn needs_sign_in(action: &str) -> &'static str {
    match action {
        END_ALL => "Signing out every device needs a passkey sign-in",
        REVOKE => "Ending a session needs a passkey sign-in",
        ROTATE => "Rotating the access token needs a passkey sign-in",
        STEP_UP => "Confirming with the passkey needs a passkey sign-in",
        _ => "Managing passkeys needs a passkey sign-in",
    }
}

/// One passkey change's audit trail: who asked and what for.
struct Audit<'a> {
    roots: &'a StorageRoots,
    identity: String,
    action: &'static str,
}

impl Audit<'_> {
    fn record(&self, outcome: crate::remote_audit::Outcome, reason: &str) {
        use crate::remote_audit::{record, Attempt};
        let _ = record(self.roots, &Attempt { identity: &self.identity, action: self.action, project: "passkeys", run: None, outcome, reason: Some(reason) }, now());
    }

    fn allowed(&self, reason: &str) {
        self.record(crate::remote_audit::Outcome::Allowed, reason);
    }

    fn refused(&self, status: u16, reason: &str, error: &str) -> AuthReply {
        self.record(crate::remote_audit::Outcome::Refused, reason);
        reply(status, json!({ "error": error }))
    }
}
