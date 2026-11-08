//! Who may enrol a passkey. Two cases only: the current access token while no passkey is
//! registered yet, or a signed-in passkey session once there is one. Anything else is refused the
//! same way as any other stranger, and a store that cannot be read refuses too, because then it is
//! not known whether a passkey exists.

use crate::passkey_store::PasskeyStore;
use crate::remote_inbox::{peer_allowed, token_matches, RemoteConfig};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;

/// Wrong tokens allowed from one address within a window before it is locked out of the token.
const MAX_WRONG_TOKENS: u32 = 5;
/// How long a lockout lasts, counted from the first wrong token of the window.
const WINDOW_SECS: u64 = 300;
/// Addresses remembered at once.
const MAX_TRACKED: usize = 1024;

/// How an enrolment was allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enrolment {
    /// With the access token, for the first passkey.
    WithToken,
    /// From a signed-in passkey session.
    WithSession,
}

/// The one answer for every refusal, so nothing says which check failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused;

/// What the request brings. `session_live` is whether its cookie names a live session.
pub struct EnrolRequest<'a> {
    pub peer: IpAddr,
    pub authorization: Option<&'a str>,
    pub session_live: bool,
}

/// Decides enrolment requests and remembers wrong tokens, in memory only.
#[derive(Default)]
pub struct EnrolAuthorizer {
    wrong: Mutex<HashMap<IpAddr, (u64, u32)>>,
}

impl EnrolAuthorizer {
    pub fn new() -> EnrolAuthorizer {
        EnrolAuthorizer::default()
    }

    /// `now` is seconds since the epoch.
    pub fn authorize(&self, store: &PasskeyStore, config: &RemoteConfig, request: &EnrolRequest, now: u64) -> Result<Enrolment, Refused> {
        if !peer_allowed(request.peer) {
            return Err(Refused);
        }
        // Whether a passkey exists is read fresh each time, so the token closes the moment the first
        // passkey is stored and reopens only when the last one is gone. An unreadable or doubtful
        // store is not "empty": it refuses.
        let registered = match store.list() {
            Ok(keys) => !keys.is_empty(),
            Err(_) => return Err(Refused),
        };
        if registered {
            return if request.session_live { Ok(Enrolment::WithSession) } else { Err(Refused) };
        }
        self.try_token(config, request, now)
    }

    fn try_token(&self, config: &RemoteConfig, request: &EnrolRequest, now: u64) -> Result<Enrolment, Refused> {
        let mut wrong = self.wrong.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = wrong.get(&request.peer).copied().filter(|(start, _)| now.saturating_sub(*start) < WINDOW_SECS);
        if entry.is_some_and(|(_, count)| count >= MAX_WRONG_TOKENS) {
            return Err(Refused);
        }
        if token_matches(&config.token, request.authorization) {
            wrong.remove(&request.peer);
            return Ok(Enrolment::WithToken);
        }
        // A request with no token at all is not a guess; only a token that is wrong is counted.
        if request.authorization.is_some() {
            if wrong.len() >= MAX_TRACKED && !wrong.contains_key(&request.peer) {
                wrong.retain(|_, (start, _)| now.saturating_sub(*start) < WINDOW_SECS);
                if wrong.len() >= MAX_TRACKED {
                    if let Some(oldest) = wrong.iter().min_by_key(|(_, (start, _))| *start).map(|(ip, _)| *ip) {
                        wrong.remove(&oldest);
                    }
                }
            }
            let (start, count) = entry.unwrap_or((now, 0));
            wrong.insert(request.peer, (start, count + 1));
        }
        Err(Refused)
    }
}
