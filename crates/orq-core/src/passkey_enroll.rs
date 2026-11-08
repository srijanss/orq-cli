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
        // Without a session only the token can succeed, so a request with no token, or from an
        // address that is locked out, is turned away before the store is touched.
        if !request.session_live && (request.authorization.is_none() || self.locked_out(request.peer, now)) {
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

    /// Stores the passkey a request was allowed to enrol. A token enrolment goes through the step
    /// that only an empty store accepts, so of several that race, one lands; a session enrolment
    /// adds to what is there. This is the only way an authorization turns into a stored passkey.
    pub fn complete(&self, store: &PasskeyStore, how: Enrolment, key: crate::passkey_store::Passkey) -> Result<(), String> {
        match how {
            Enrolment::WithToken => store.add_first(key),
            Enrolment::WithSession => store.add(key),
        }
    }

    fn locked_out(&self, peer: IpAddr, now: u64) -> bool {
        let wrong = self.wrong.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        wrong.get(&peer).is_some_and(|(start, count)| now.saturating_sub(*start) < WINDOW_SECS && *count >= MAX_WRONG_TOKENS)
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.wrong.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).len()
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
                    // Addresses that are only part-way to a lockout go first, oldest first, so a flood of
                    // new addresses does not wipe out the lockouts that matter.
                    if let Some(drop) = wrong.iter().min_by_key(|(_, (start, count))| (*count >= MAX_WRONG_TOKENS, *start)).map(|(ip, _)| *ip) {
                        wrong.remove(&drop);
                    }
                }
            }
            let (start, count) = entry.unwrap_or((now, 0));
            wrong.insert(request.peer, (start, count + 1));
        }
        Err(Refused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrong_from(auth: &EnrolAuthorizer, store: &PasskeyStore, peer: IpAddr, now: u64) {
        let config = RemoteConfig { token: "t".repeat(40) };
        let bad = format!("Bearer {}", "w".repeat(40));
        let _ = auth.authorize(store, &config, &EnrolRequest { peer, authorization: Some(&bad), session_live: false }, now);
    }

    fn ip(i: u32) -> IpAddr {
        IpAddr::from([100, 64 + ((i >> 16) % 64) as u8, (i >> 8) as u8, i as u8])
    }

    fn empty_store(tag: &str) -> PasskeyStore {
        let base = std::env::temp_dir().join(format!("orq-enrol-unit-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let roots = crate::storage::StorageRoots { data: base.join("d"), config: base.join("c"), state: base.join("s"), cache: base.join("k") };
        PasskeyStore::open(&roots)
    }

    #[test]
    fn the_tracked_addresses_never_exceed_the_limit_and_expired_ones_are_cleaned_first() {
        let (auth, store) = (EnrolAuthorizer::new(), empty_store("cap"));
        for i in 0..(MAX_TRACKED as u32 + 500) {
            wrong_from(&auth, &store, ip(i), 1);
            assert!(auth.tracked() <= MAX_TRACKED);
        }
        assert_eq!(auth.tracked(), MAX_TRACKED);
        // Once the window has passed, the next new address clears the expired ones away.
        wrong_from(&auth, &store, ip(900_000), 1 + WINDOW_SECS);
        assert_eq!(auth.tracked(), 1);
    }

    #[test]
    fn a_flood_of_new_addresses_does_not_evict_an_address_that_is_locked_out() {
        let (auth, store) = (EnrolAuthorizer::new(), empty_store("flood"));
        let target = ip(5_000_000);
        for _ in 0..MAX_WRONG_TOKENS {
            wrong_from(&auth, &store, target, 10);
        }
        assert!(auth.locked_out(target, 11));
        for i in 0..(MAX_TRACKED as u32 * 3) {
            wrong_from(&auth, &store, ip(i), 20 + u64::from(i / 100));
        }
        assert!(auth.tracked() <= MAX_TRACKED);
        assert!(auth.locked_out(target, 100), "the lockout survived the churn");
    }

    #[test]
    fn locked_out_and_tokenless_requests_do_not_read_the_store_but_others_do() {
        use crate::passkey_store::LISTS;
        let (auth, store) = (EnrolAuthorizer::new(), empty_store("early"));
        let config = RemoteConfig { token: "t".repeat(40) };
        let good = format!("Bearer {}", "t".repeat(40));
        let reads = |auth: &EnrolAuthorizer, authorization: Option<&str>, peer: IpAddr, now: u64| {
            let before = LISTS.with(|n| n.get());
            let _ = auth.authorize(&store, &config, &EnrolRequest { peer, authorization, session_live: false }, now);
            LISTS.with(|n| n.get()) - before
        };
        assert_eq!(reads(&auth, None, ip(1), 5), 0, "no token");
        assert_eq!(reads(&auth, Some(&good), ip(1), 5), 1, "a token that could succeed reads the store");
        for _ in 0..MAX_WRONG_TOKENS {
            wrong_from(&auth, &store, ip(2), 5);
        }
        assert_eq!(reads(&auth, Some(&good), ip(2), 6), 0, "locked out");
        assert_eq!(reads(&auth, Some(&good), ip(2), 5 + WINDOW_SECS), 1, "and after the lockout it reads again");
        let session = auth.authorize(&store, &config, &EnrolRequest { peer: ip(2), authorization: None, session_live: true }, 6);
        assert!(session.is_err(), "a session with no passkey behind it");
        assert!(LISTS.with(|n| n.get()) > 0);
    }
}
