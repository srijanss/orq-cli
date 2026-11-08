//! Single-use challenges for passkey registration and sign-in: random, short-lived, remembered in
//! memory only, and spent the first time they are presented, whether or not the answer was good.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Mutex;

/// What a challenge was issued for; one issued for one purpose cannot be spent on the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Register,
    Login,
}

/// How long a challenge can be answered.
pub const LIFETIME_SECS: u64 = 300;

/// Most challenges kept at once; past this the oldest go first.
const MAX_PENDING: usize = 256;

#[derive(Default)]
pub struct Challenges {
    pending: Mutex<HashMap<String, (Purpose, u64)>>,
}

impl Challenges {
    pub fn new() -> Challenges {
        Challenges::default()
    }

    /// A new challenge as base64url text of 32 random bytes. `now` is seconds since the epoch.
    pub fn issue(&self, purpose: Purpose, now: u64) -> Result<String, String> {
        let text = random_b64url(32)?;
        let mut pending = self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        pending.retain(|_, (_, issued)| now.saturating_sub(*issued) < LIFETIME_SECS);
        while pending.len() >= MAX_PENDING {
            let Some(oldest) = pending.iter().min_by_key(|(_, (_, issued))| *issued).map(|(c, _)| c.clone()) else { break };
            pending.remove(&oldest);
        }
        pending.insert(text.clone(), (purpose, now));
        Ok(text)
    }

    /// Spends `challenge`: true only if it was issued for `purpose`, is not expired, and has not
    /// been spent. It is gone afterwards either way.
    pub fn spend(&self, purpose: Purpose, challenge: &str, now: u64) -> bool {
        let mut pending = self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match pending.remove(challenge) {
            Some((issued_for, issued)) => issued_for == purpose && now.saturating_sub(issued) < LIFETIME_SECS,
            None => false,
        }
    }

    pub fn count(&self) -> usize {
        self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).len()
    }
}

/// `len` random bytes from the system, as unpadded base64url.
pub fn random_b64url(len: usize) -> Result<String, String> {
    let mut bytes = vec![0u8; len];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).map_err(|e| format!("cannot generate random bytes: {}", e.kind()))?;
    Ok(encode_b64url(&bytes))
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub fn encode_b64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 3);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// Unpadded base64url only; anything else (padding, other alphabets, stray characters, a length
/// that cannot be valid) is an error.
pub fn decode_b64url(text: &str) -> Result<Vec<u8>, String> {
    if text.len() % 4 == 1 {
        return Err("not base64url".into());
    }
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let v = ALPHABET.iter().position(|a| *a == c).ok_or("not base64url")? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if acc != 0 {
        return Err("not canonical base64url".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_round_trips_and_rejects_what_is_not_canonical() {
        for len in 0..40 {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(decode_b64url(&encode_b64url(&bytes)).unwrap(), bytes);
        }
        for bad in ["a", "ab=", "ab==", "a+b/", "ab cd", "ab\\n", "QQ=="] {
            assert!(decode_b64url(bad).is_err(), "{bad}");
        }
        assert!(decode_b64url("QR").is_err(), "stray low bits");
        assert_eq!(decode_b64url("QQ").unwrap(), b"A");
    }

    #[test]
    fn a_challenge_is_random_single_use_and_for_one_purpose() {
        let c = Challenges::new();
        let (a, b) = (c.issue(Purpose::Register, 10).unwrap(), c.issue(Purpose::Register, 10).unwrap());
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
        assert!(c.spend(Purpose::Register, &a, 11));
        assert!(!c.spend(Purpose::Register, &a, 11), "spent once");
        assert!(!c.spend(Purpose::Login, &b, 11), "wrong purpose");
        assert!(!c.spend(Purpose::Register, &b, 11), "and a wrong try spends it");
        assert!(!c.spend(Purpose::Register, "unknown", 11));
    }

    #[test]
    fn a_challenge_expires_and_the_table_is_bounded() {
        let c = Challenges::new();
        let a = c.issue(Purpose::Login, 100).unwrap();
        assert!(!c.spend(Purpose::Login, &a, 100 + LIFETIME_SECS));
        let b = c.issue(Purpose::Login, 100).unwrap();
        assert!(c.spend(Purpose::Login, &b, 100 + LIFETIME_SECS - 1));
        let first = c.issue(Purpose::Login, 1).unwrap();
        for i in 0..MAX_PENDING as u64 {
            c.issue(Purpose::Login, 2 + i).unwrap();
        }
        assert_eq!(c.count(), MAX_PENDING);
        assert!(!c.spend(Purpose::Login, &first, 3), "the oldest made room");
    }
}
