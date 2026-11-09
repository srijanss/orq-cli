//! Sign-in sessions for the remote inbox: a random server-side id carried in a cookie. A session
//! is issued only after a passkey sign-in succeeds, expires, can be ended by logout, and holds
//! nothing but its own expiry: no passkey data, no identity, no token.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Mutex;

/// The cookie's name. The `__Host-` prefix makes browsers accept it only over HTTPS, with
/// `Path=/` and no `Domain`, so a sibling host cannot plant or overwrite it.
pub const COOKIE_NAME: &str = "__Host-orq-session";

/// How long a session lasts when no other time is given.
pub const DEFAULT_LIFETIME_SECS: u64 = 12 * 60 * 60;

/// Most sessions kept at once; past this the oldest go first, so sign-ins cannot grow memory.
const MAX_SESSIONS: usize = 256;

/// An id is 32 random bytes as 43 base64url characters.
const ID_LEN: usize = 43;

/// Proof that a passkey sign-in just succeeded. Only code in this crate can make one, so the
/// passkey login step is the only way to a session.
pub struct PasskeySignIn(());

impl PasskeySignIn {
    #[allow(dead_code)] // the passkey login step is the only caller
    pub(crate) fn verified() -> Self {
        PasskeySignIn(())
    }
}

/// A new session: the id to put in the cookie and the lifetime to give the cookie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issued {
    pub id: String,
    pub cookie: String,
}

/// The live sessions, in memory only: a restart signs everyone out.
pub struct Sessions {
    lifetime: u64,
    live: Mutex<HashMap<String, Entry>>,
}

struct Entry {
    credential: String,
    created: u64,
    expires: u64,
}

impl Sessions {
    pub fn new(lifetime_secs: u64) -> Sessions {
        Sessions { lifetime: lifetime_secs.max(1), live: Mutex::new(HashMap::new()) }
    }

    /// Starts a session at `now` (seconds since the epoch) for the signed-in passkey with this credential id.
    pub fn issue(&self, _proof: PasskeySignIn, credential: &str, now: u64) -> Result<Issued, String> {
        let id = new_id()?;
        let mut live = self.live.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        live.retain(|_, entry| entry.expires > now);
        while live.len() >= MAX_SESSIONS {
            let Some(oldest) = live.iter().min_by_key(|(_, entry)| entry.created).map(|(id, _)| id.clone()) else { break };
            live.remove(&oldest);
        }
        live.insert(id.clone(), Entry { credential: credential.to_string(), created: now, expires: now.saturating_add(self.lifetime) });
        Ok(Issued { cookie: set_cookie(&id, self.lifetime), id })
    }

    /// Whether `id` is a live session at `now`. An expired session is forgotten.
    pub fn is_live(&self, id: &str, now: u64) -> bool {
        let mut live = self.live.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match live.get(id) {
            Some(entry) if entry.expires > now => true,
            Some(_) => {
                live.remove(id);
                false
            }
            None => false,
        }
    }

    /// The credential id of the passkey that opened this session, if it is live at `now`.
    pub fn credential(&self, id: &str, now: u64) -> Option<String> {
        let live = self.live.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        live.get(id).filter(|entry| entry.expires > now).map(|entry| entry.credential.clone())
    }

    /// Logout: ends the session, if it exists, and gives the header that clears the cookie.
    pub fn end(&self, id: &str) -> String {
        self.live.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(id);
        clear_cookie()
    }

    /// Ends every session, for when the passkeys behind them change.
    pub fn end_all(&self) {
        self.live.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
    }

    pub fn count(&self) -> usize {
        self.live.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).len()
    }
}

/// The `Set-Cookie` value for a session: not readable by scripts, sent only over HTTPS, not sent
/// on cross-site subrequests, for the whole site, and gone when the session is.
pub fn set_cookie(id: &str, lifetime_secs: u64) -> String {
    format!("{COOKIE_NAME}={id}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age={lifetime_secs}")
}

/// The `Set-Cookie` value that removes the cookie from the browser.
pub fn clear_cookie() -> String {
    format!("{COOKIE_NAME}=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0")
}

/// The session id in a request's `Cookie` header: only a cookie with exactly our name and a
/// well-formed id counts, and a header that sets it twice is ambiguous, so it counts for nothing.
pub fn id_from_cookie_header(header: &str) -> Option<&str> {
    let mut found = None;
    for pair in header.split(';') {
        let Some((name, value)) = pair.trim().split_once('=') else { continue };
        if name == COOKIE_NAME {
            if found.is_some() {
                return None;
            }
            found = Some(value);
        }
    }
    found.filter(|id| id.len() == ID_LEN && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
}

fn new_id() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).map_err(|e| format!("cannot generate a session id: {}", e.kind()))?;
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut id = String::with_capacity(ID_LEN);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            id.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sessions() -> Sessions {
        Sessions::new(100)
    }

    fn start(s: &Sessions, now: u64) -> Issued {
        s.issue(PasskeySignIn::verified(), "credential", now).unwrap()
    }

    #[test]
    fn a_session_has_a_random_url_safe_id_and_a_protected_cookie() {
        let s = sessions();
        let a = start(&s, 1000);
        let b = start(&s, 1000);
        assert_ne!(a.id, b.id);
        assert_eq!(a.id.len(), 43);
        assert!(a.id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'), "{}", a.id);
        let attrs: Vec<&str> = a.cookie.split("; ").collect();
        assert_eq!(attrs, [format!("{COOKIE_NAME}={}", a.id).as_str(), "HttpOnly", "Secure", "SameSite=Lax", "Path=/", "Max-Age=100"]);
        assert!(!a.cookie.contains("Domain"));
    }

    #[test]
    fn a_session_is_live_until_it_expires_and_then_forgotten() {
        let s = sessions();
        let a = start(&s, 1000);
        assert!(s.is_live(&a.id, 1000) && s.is_live(&a.id, 1099));
        assert!(!s.is_live(&a.id, 1100), "expired at the stated time");
        assert_eq!(s.count(), 0, "an expired session is dropped");
        assert!(!s.is_live(&a.id, 1000), "and does not come back");
    }

    #[test]
    fn unknown_or_malformed_ids_are_not_sessions() {
        let s = sessions();
        start(&s, 1000);
        for id in ["", "x", &"A".repeat(43), "../../etc", &"A".repeat(500)] {
            assert!(!s.is_live(id, 1000), "{id}");
        }
    }

    #[test]
    fn logout_ends_only_that_session_and_clears_the_cookie() {
        let s = sessions();
        let (a, b) = (start(&s, 1000), start(&s, 1000));
        let cleared = s.end(&a.id);
        assert!(cleared.starts_with(&format!("{COOKIE_NAME}=;")) && cleared.contains("Max-Age=0") && cleared.contains("HttpOnly") && cleared.contains("Secure"));
        assert!(!s.is_live(&a.id, 1001) && s.is_live(&b.id, 1001));
        s.end(&a.id); // ending twice is harmless
        s.end_all();
        assert!(!s.is_live(&b.id, 1001));
    }

    #[test]
    fn the_session_table_is_bounded_and_drops_the_oldest_first() {
        let s = Sessions::new(1_000_000);
        let first = start(&s, 1);
        let ids: Vec<Issued> = (0..MAX_SESSIONS as u64).map(|i| start(&s, 2 + i)).collect();
        assert_eq!(s.count(), MAX_SESSIONS);
        assert!(!s.is_live(&first.id, 10), "the oldest made room");
        assert!(s.is_live(&ids[0].id, 10) && s.is_live(&ids[MAX_SESSIONS - 1].id, 10));
    }

    #[test]
    fn expired_sessions_make_room_before_live_ones_are_dropped() {
        let s = Sessions::new(10);
        let old: Vec<Issued> = (0..MAX_SESSIONS).map(|_| start(&s, 1)).collect();
        let fresh = start(&s, 500);
        assert_eq!(s.count(), 1);
        assert!(s.is_live(&fresh.id, 501) && !s.is_live(&old[0].id, 501));
    }

    #[test]
    fn the_cookie_header_yields_an_id_only_when_it_is_ours_and_unambiguous() {
        let id = "A".repeat(43);
        let ok = |h: &str| id_from_cookie_header(h).map(str::to_string);
        assert_eq!(ok(&format!("{COOKIE_NAME}={id}")), Some(id.clone()));
        assert_eq!(ok(&format!("theme=dark; {COOKIE_NAME}={id}; other=1")), Some(id.clone()));
        assert_eq!(ok(&format!("orq-session={id}")), None, "another name, such as one without the prefix");
        assert_eq!(ok(&format!("x{COOKIE_NAME}={id}")), None);
        assert_eq!(ok(&format!("{COOKIE_NAME}={id}; {COOKIE_NAME}={id}")), None, "set twice");
        assert_eq!(ok(&format!("{COOKIE_NAME}={}", "A".repeat(42))), None);
        assert_eq!(ok(&format!("{COOKIE_NAME}={}!", "A".repeat(42))), None);
        assert_eq!(ok(&format!("{COOKIE_NAME}=\"{id}\"")), None);
        assert_eq!(ok(""), None);
    }
}
