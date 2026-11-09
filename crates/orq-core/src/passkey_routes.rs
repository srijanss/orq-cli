//! The `/auth/*` routes: passkey enrolment and sign-in as plain request/response values, so the
//! HTTP server only has to carry bytes. Every decision lives in the modules these call.

use std::net::IpAddr;

use serde_json::{json, Value};

use crate::passkey_challenge::Challenges;
use crate::passkey_enroll::{EnrolAuthorizer, EnrolRequest};
use crate::passkey_login::{self, AssertionResponse};
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
    authorizer: EnrolAuthorizer,
    sessions: Sessions,
    loopback_token: bool,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl PasskeyAuth {
    pub fn new(host: PasskeyHost) -> PasskeyAuth {
        PasskeyAuth { host, challenges: Challenges::new(), authorizer: EnrolAuthorizer::new(), sessions: Sessions::new(SESSION_SECS), loopback_token: true }
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

    /// Whether the `Cookie` header names a live session.
    pub fn session_live(&self, cookie: Option<&str>) -> bool {
        cookie.and_then(session::id_from_cookie_header).is_some_and(|id| self.sessions.is_live(id, now()))
    }

    /// The answer for an `/auth/...` request; `None` for any other path.
    pub fn handle(&self, roots: &StorageRoots, config: &RemoteConfig, request: &AuthRequest) -> Option<AuthReply> {
        let route = request.path.strip_prefix("/auth/")?;
        if !peer_allowed(request.peer) {
            return Some(refuse());
        }
        let store = PasskeyStore::open(roots);
        let at = now();
        let live = self.session_live(request.cookie);
        let post = request.method == "POST";
        let enrol = || self.authorizer.authorize(&store, config, &EnrolRequest { peer: request.peer, authorization: request.authorization, session_live: live }, at);
        let fields = || serde_json::from_slice::<Value>(request.body).ok().filter(Value::is_object);
        let text = |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
        Some(match (request.method, route) {
            ("GET", "status") => reply(200, json!({ "passkeys": store.any_registered(), "signed_in": live, "host": self.host.rp_id() })),
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
                let Some(body) = fields() else { return Some(refuse()) };
                let (Some(credential_id), Some(client_data_json), Some(authenticator_data), Some(signature)) = (text(&body, "id"), text(&body, "clientDataJSON"), text(&body, "authenticatorData"), text(&body, "signature")) else {
                    return Some(refuse());
                };
                let response = AssertionResponse { credential_id, client_data_json, authenticator_data, signature };
                match passkey_login::finish(&self.host, &self.challenges, &store, &self.sessions, &response, at) {
                    Ok(issued) => AuthReply { status: 200, body: json!({ "signed_in": true }), set_cookie: Some(issued.cookie) },
                    Err(_) => refuse(),
                }
            }
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
