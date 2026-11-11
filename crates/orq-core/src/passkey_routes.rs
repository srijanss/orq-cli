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
/// Sessions last twelve hours.
const SESSION_SECS: u64 = 12 * 60 * 60;

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
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl PasskeyAuth {
    pub fn new(host: PasskeyHost) -> PasskeyAuth {
        PasskeyAuth { host, challenges: Challenges::new(), confirmations: Confirmations::new(), authorizer: EnrolAuthorizer::new(), sessions: Sessions::new(SESSION_SECS), loopback_token: true }
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
        cookie.and_then(session::id_from_cookie_header).and_then(|id| self.sessions.credential(id, now()))
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
        cookie.and_then(session::id_from_cookie_header).and_then(|id| self.sessions.csrf(id, now()))
    }

    /// Whether a state-changing request may go on: it comes from a live session, names this server's own
    /// origin, and carries that session's CSRF token. The access token alone never passes.
    pub fn change_allowed(&self, roots: &StorageRoots, cookie: Option<&str>, origin: Option<&str>, presented: Option<&str>) -> bool {
        let (Some(origin), Some(presented), Some(id)) = (origin, presented, cookie.and_then(session::id_from_cookie_header)) else { return false };
        origin == self.host.origin() && self.session_live(roots, cookie) && self.sessions.csrf_matches(id, presented, now())
    }

    /// Whether `assertion` confirms `purpose` for the live session in the `Cookie` header: it answers a
    /// challenge issued to that session for that purpose, from a registered passkey with user verification.
    pub fn confirm(&self, roots: &StorageRoots, cookie: Option<&str>, purpose: &str, assertion: &Value) -> bool {
        let Some(session) = cookie.and_then(session::id_from_cookie_header).filter(|_| self.session_live(roots, cookie)) else { return false };
        let Some(response) = passkey_confirm::assertion_from(assertion) else { return false };
        passkey_confirm::check(&self.host, &self.confirmations, &PasskeyStore::open(roots), purpose, session, &response, now()).is_ok()
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

    /// The answer for an `/auth/...` request; `None` for any other path.
    pub fn handle(&self, roots: &StorageRoots, config: &RemoteConfig, request: &AuthRequest) -> Option<AuthReply> {
        let route = request.path.strip_prefix("/auth/")?;
        if !peer_allowed(request.peer) {
            return Some(refuse());
        }
        let store = PasskeyStore::open(roots);
        let at = now();
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
        Some(match (request.method, route) {
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

/// What a change refused for want of a passkey session says.
fn needs_sign_in(action: &str) -> &'static str {
    match action {
        END_ALL => "Signing out every device needs a passkey sign-in",
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
