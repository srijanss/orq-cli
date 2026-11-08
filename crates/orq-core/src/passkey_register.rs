//! The server's side of registering a passkey: it hands out a single-use challenge and, when the
//! browser answers, checks the answer before storing the new passkey. Attestation is not requested
//! and not verified: the passkey is stored for what it can prove later, at sign-in.
//!
//! The relying-party id and the allowed origin come from a setting naming the HTTPS host, never
//! from the request, so a forged Host or Origin header changes nothing.

use crate::passkey_challenge::{decode_b64url, encode_b64url, random_b64url, Challenges, Purpose};
use crate::passkey_enroll::{EnrolAuthorizer, Enrolment};
use crate::passkey_store::{Passkey, PasskeyStore};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const MAX_CLIENT_DATA: usize = 4 * 1024;
const MAX_ATTESTATION: usize = 16 * 1024;
/// Credential ids are kept as base64url text, so they are bounded in bytes.
const MAX_CREDENTIAL_ID_BYTES: usize = 768;

/// The one host passkeys are bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasskeyHost {
    rp_id: String,
    origin: String,
}

impl PasskeyHost {
    /// From the setting's value: a plain DNS name such as `box.tailnet.ts.net`. An address, a
    /// scheme, a port, a path, an empty label or any character outside lower-case letters, digits
    /// and hyphens is refused, because browsers accept passkeys only on a host name over HTTPS.
    pub fn from_setting(host: &str) -> Result<PasskeyHost, String> {
        let bad = |why: &str| Err(format!("the passkey host {why}"));
        if host.is_empty() || host.len() > 253 {
            return bad("must be a host name of 1 to 253 characters");
        }
        let labels: Vec<&str> = host.split('.').collect();
        if labels.len() < 2 {
            return bad("must be a full host name with at least one dot, such as machine.tailnet.ts.net");
        }
        for label in &labels {
            let ok = !label.is_empty() && label.len() <= 63 && !label.starts_with('-') && !label.ends_with('-') && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
            if !ok {
                return bad("must be lower-case letters, digits and hyphens in dot-separated labels, with no scheme, port or path");
            }
        }
        if labels.last().is_some_and(|last| last.bytes().all(|b| b.is_ascii_digit())) {
            return bad("must be a name, not an IP address");
        }
        Ok(PasskeyHost { rp_id: host.to_string(), origin: format!("https://{host}") })
    }

    pub fn rp_id(&self) -> &str {
        &self.rp_id
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }
}

/// What the browser sends back, as the base64url text it was given as.
pub struct RegistrationResponse {
    pub client_data_json: String,
    pub attestation_object: String,
    pub label: String,
}

/// The options for `navigator.credentials.create`, with a fresh challenge. Passkeys already stored
/// are listed so the same device is not enrolled twice. Only ES256 is asked for, since that is what
/// sign-in checks.
pub fn begin(host: &PasskeyHost, challenges: &Challenges, store: &PasskeyStore, now: u64) -> Result<Value, String> {
    let existing: Vec<Value> = store.list()?.iter().map(|stored| json!({"type": "public-key", "id": stored.credential_id})).collect();
    Ok(json!({
        "challenge": challenges.issue(Purpose::Register, now)?,
        "rp": {"id": host.rp_id(), "name": "Orq"},
        // A fresh handle each time: a second passkey on the same device must not replace the first.
        "user": {"id": random_b64url(16)?, "name": "orq", "displayName": "Orq"},
        "pubKeyCredParams": [{"type": "public-key", "alg": -7}],
        "authenticatorSelection": {"residentKey": "preferred", "userVerification": "required"},
        "attestation": "none",
        "timeout": 60_000,
        "excludeCredentials": existing,
    }))
}

/// Checks the browser's answer and stores the new passkey. `how` is the result of authorizing this
/// very request; the challenge is spent whatever the outcome. The error text is for the log, not
/// for the caller of the web API.
pub fn finish(host: &PasskeyHost, challenges: &Challenges, authorizer: &EnrolAuthorizer, store: &PasskeyStore, how: Enrolment, response: &RegistrationResponse, now: u64) -> Result<Passkey, String> {
    // Encoded lengths are checked before decoding, so an oversized answer costs nothing to refuse.
    if response.client_data_json.len() > encoded_len(MAX_CLIENT_DATA) || response.attestation_object.len() > encoded_len(MAX_ATTESTATION) {
        return Err("the answer is too large".into());
    }
    let client_data = decode_b64url(&response.client_data_json).map_err(|_| "client data is not base64url")?;
    let data: Value = serde_json::from_slice(&client_data).map_err(|_| "client data is not JSON")?;
    let text = |name: &str| data.get(name).and_then(Value::as_str);
    // The challenge is spent before anything else is judged, so no answer can be retried.
    let challenge = text("challenge").ok_or("client data has no challenge")?;
    if !challenges.spend(Purpose::Register, challenge, now) {
        return Err("the challenge is unknown, expired or already used".into());
    }
    let attestation = decode_b64url(&response.attestation_object).map_err(|_| "attestation is not base64url")?;
    if attestation.len() > MAX_ATTESTATION {
        return Err("the answer is too large".into());
    }
    if text("type") != Some("webauthn.create") {
        return Err("client data is not for registration".into());
    }
    if text("origin") != Some(host.origin()) {
        return Err("the origin is not the passkey host".into());
    }
    if data.get("crossOrigin").is_some_and(|v| v != &Value::Bool(false)) {
        return Err("the answer came from a cross-origin frame".into());
    }
    let passkey = credential_from_attestation(host, &attestation, &response.label, now)?;
    authorizer.complete(store, how, passkey.clone())?;
    Ok(passkey)
}

/// The most base64url characters that `bytes` bytes can take.
fn encoded_len(bytes: usize) -> usize {
    bytes.div_ceil(3) * 4
}

fn credential_from_attestation(host: &PasskeyHost, attestation: &[u8], label: &str, now: u64) -> Result<Passkey, String> {
    let mut at = 0;
    let object = cbor(attestation, &mut at, 0)?;
    if at != attestation.len() {
        return Err("attestation has trailing bytes".into());
    }
    let auth_data = match map_get(&object, &Cbor::Text("authData".into())) {
        Some(Cbor::Bytes(bytes)) => bytes,
        _ => return Err("attestation has no authenticator data".into()),
    };
    if auth_data.len() < 55 {
        return Err("authenticator data is too short".into());
    }
    if auth_data[..32] != Sha256::digest(host.rp_id().as_bytes())[..] {
        return Err("the relying party id hash does not match".into());
    }
    let flags = auth_data[32];
    if flags & 0x01 == 0 {
        return Err("user presence was not confirmed".into());
    }
    if flags & 0x04 == 0 {
        return Err("user verification was not performed".into());
    }
    if flags & 0x40 == 0 {
        return Err("no credential in the answer".into());
    }
    let sign_count = u32::from_be_bytes([auth_data[33], auth_data[34], auth_data[35], auth_data[36]]);
    let id_len = u16::from_be_bytes([auth_data[53], auth_data[54]]) as usize;
    if id_len == 0 || id_len > MAX_CREDENTIAL_ID_BYTES || auth_data.len() < 55 + id_len {
        return Err("the credential id has a bad length".into());
    }
    let credential_id = &auth_data[55..55 + id_len];
    let cose_start = 55 + id_len;
    let mut end = cose_start;
    let cose = cbor(auth_data, &mut end, 0)?;
    check_es256(&cose)?;
    let cose_end = end;
    if flags & 0x80 != 0 && !matches!(cbor(auth_data, &mut end, 0)?, Cbor::Map(_)) {
        return Err("extension data is not a map".into());
    }
    if end != auth_data.len() {
        return Err("authenticator data has trailing bytes".into());
    }
    Ok(Passkey {
        credential_id: encode_b64url(credential_id),
        public_key: encode_b64url(&auth_data[cose_start..cose_end]),
        sign_count,
        label: label.to_string(),
        created_at: now as i64,
    })
}

/// An EC2 P-256 key for ES256 with exactly the five expected entries and 32-byte coordinates.
fn check_es256(cose: &Cbor) -> Result<(), String> {
    let Cbor::Map(entries) = cose else { return Err("the public key is not a COSE map".into()) };
    if entries.len() != 5 {
        return Err("the public key has unexpected entries".into());
    }
    let entry = |n: i64| map_get(cose, &Cbor::Int(n));
    let coordinate = |n: i64| matches!(entry(n), Some(Cbor::Bytes(b)) if b.len() == 32);
    let (Some(Cbor::Bytes(x)), Some(Cbor::Bytes(y))) = (entry(-2), entry(-3)) else { return Err("the public key has no coordinates".into()) };
    if entry(1) != Some(&Cbor::Int(2)) || entry(3) != Some(&Cbor::Int(-7)) || entry(-1) != Some(&Cbor::Int(1)) || !coordinate(-2) || !coordinate(-3) {
        return Err("the public key is not ES256 on P-256".into());
    }
    if !on_curve(x, y) {
        return Err("the public key is not a point on P-256".into());
    }
    Ok(())
}

type U256 = [u64; 4];

/// P-256's field prime and curve constant b, least significant limb first.
const P: U256 = [0xffff_ffff_ffff_ffff, 0x0000_0000_ffff_ffff, 0, 0xffff_ffff_0000_0001];
const B: U256 = [0x3bce_3c3e_27d2_604b, 0x651d_06b0_cc53_b0f6, 0xb3eb_bd55_7698_86bc, 0x5ac6_35d8_aa3a_93e7];

fn from_be(bytes: &[u8]) -> Option<U256> {
    let bytes: [u8; 32] = bytes.try_into().ok()?;
    let mut limbs = [0u64; 4];
    for (i, chunk) in bytes.chunks(8).enumerate() {
        limbs[3 - i] = u64::from_be_bytes(chunk.try_into().ok()?);
    }
    Some(limbs)
}

fn less(a: &U256, b: &U256) -> bool {
    (0..4).rev().find(|&i| a[i] != b[i]).is_some_and(|i| a[i] < b[i])
}

fn sub_wrapping(a: &U256, b: &U256) -> U256 {
    let mut out = [0u64; 4];
    let mut borrow = false;
    for i in 0..4 {
        let (d, b1) = a[i].overflowing_sub(b[i]);
        let (d, b2) = d.overflowing_sub(u64::from(borrow));
        out[i] = d;
        borrow = b1 || b2;
    }
    out
}

fn add_mod(a: &U256, b: &U256) -> U256 {
    let mut out = [0u64; 4];
    let mut carry = false;
    for i in 0..4 {
        let (s, c1) = a[i].overflowing_add(b[i]);
        let (s, c2) = s.overflowing_add(u64::from(carry));
        out[i] = s;
        carry = c1 || c2;
    }
    if carry || !less(&out, &P) { sub_wrapping(&out, &P) } else { out }
}

fn sub_mod(a: &U256, b: &U256) -> U256 {
    if less(a, b) { sub_wrapping(&P, &sub_wrapping(b, a)) } else { sub_wrapping(a, b) }
}

/// Double-and-add: slow, but there is one multiplication chain per registration.
fn mul_mod(a: &U256, b: &U256) -> U256 {
    let mut out = [0u64; 4];
    for bit in (0..256).rev() {
        out = add_mod(&out, &out);
        if b[bit / 64] >> (bit % 64) & 1 == 1 {
            out = add_mod(&out, a);
        }
    }
    out
}

/// Whether (x, y) satisfies y^2 = x^3 - 3x + b over the P-256 field, with both coordinates reduced.
fn on_curve(x: &[u8], y: &[u8]) -> bool {
    let (Some(x), Some(y)) = (from_be(x), from_be(y)) else { return false };
    if !less(&x, &P) || !less(&y, &P) {
        return false;
    }
    let cubed = mul_mod(&mul_mod(&x, &x), &x);
    let three_x = add_mod(&add_mod(&x, &x), &x);
    mul_mod(&y, &y) == add_mod(&sub_mod(&cubed, &three_x), &B)
}

#[cfg(test)]
mod tests {
    use super::on_curve;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn known_points_are_on_the_curve_and_nearby_ones_are_not() {
        let g = ("6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296", "4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5");
        let two_g = ("7cf27b188d034f7e8a52380304b51ac3c08969e277f21b35a60b48fc47669978", "07775510db8ed040293d9ac69f7430dbba7dade63ce982299e04b79d227873d1");
        for (x, y) in [g, two_g] {
            assert!(on_curve(&hex(x), &hex(y)));
        }
        let mut off = hex(g.1);
        off[31] ^= 1;
        assert!(!on_curve(&hex(g.0), &off));
        assert!(!on_curve(&[0; 32], &[0; 32]));
        assert!(!on_curve(&hex(g.0), &hex(g.0)));
        // Coordinates not reduced modulo the prime are refused, as is a wrong length.
        assert!(!on_curve(&[0xff; 32], &hex(g.1)));
        assert!(!on_curve(&hex(g.0)[1..], &hex(g.1)));
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Cbor {
    Int(i64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
    /// A boolean, null or float: valid CBOR that registration never needs the value of.
    Other,
}

/// The value for `wanted`, if it appears exactly once.
fn map_get<'a>(map: &'a Cbor, wanted: &Cbor) -> Option<&'a Cbor> {
    let Cbor::Map(entries) = map else { return None };
    let mut found = entries.iter().filter(|(k, _)| k == wanted);
    let first = found.next()?;
    found.next().is_none().then_some(&first.1)
}

/// One CBOR item at `*at`, definite lengths only, bounded in depth and count.
fn cbor(data: &[u8], at: &mut usize, depth: u8) -> Result<Cbor, String> {
    const MAX_DEPTH: u8 = 6;
    const MAX_ITEMS: u64 = 64;
    let bad = || "malformed CBOR".to_string();
    if depth > MAX_DEPTH {
        return Err("CBOR nests too deeply".into());
    }
    let take = |at: &mut usize, n: usize| -> Result<&[u8], String> {
        let end = at.checked_add(n).filter(|end| *end <= data.len()).ok_or_else(bad)?;
        let slice = &data[*at..end];
        *at = end;
        Ok(slice)
    };
    let head = take(at, 1)?[0];
    let (major, info) = (head >> 5, head & 31);
    let argument = |at: &mut usize| -> Result<u64, String> {
        Ok(match info {
            0..=23 => u64::from(info),
            24 => u64::from(take(at, 1)?[0]),
            25 => u64::from(u16::from_be_bytes(take(at, 2)?.try_into().map_err(|_| bad())?)),
            26 => u64::from(u32::from_be_bytes(take(at, 4)?.try_into().map_err(|_| bad())?)),
            27 => u64::from_be_bytes(take(at, 8)?.try_into().map_err(|_| bad())?),
            _ => return Err("CBOR with an indefinite length is not accepted".into()),
        })
    };
    match major {
        0 => Ok(Cbor::Int(i64::try_from(argument(at)?).map_err(|_| bad())?)),
        1 => Ok(Cbor::Int(-1 - i64::try_from(argument(at)?).map_err(|_| bad())?)),
        2 | 3 => {
            let len = usize::try_from(argument(at)?).map_err(|_| bad())?;
            let bytes = take(at, len)?.to_vec();
            if major == 2 {
                Ok(Cbor::Bytes(bytes))
            } else {
                Ok(Cbor::Text(String::from_utf8(bytes).map_err(|_| bad())?))
            }
        }
        4 => {
            let count = argument(at)?;
            if count > MAX_ITEMS {
                return Err("CBOR array is too long".into());
            }
            (0..count).map(|_| cbor(data, at, depth + 1)).collect::<Result<_, _>>().map(Cbor::Array)
        }
        5 => {
            let count = argument(at)?;
            if count > MAX_ITEMS {
                return Err("CBOR map is too long".into());
            }
            (0..count).map(|_| Ok((cbor(data, at, depth + 1)?, cbor(data, at, depth + 1)?))).collect::<Result<_, String>>().map(Cbor::Map)
        }
        7 if matches!(info, 20..=22) => Ok(Cbor::Other),
        7 if matches!(info, 25..=27) => {
            argument(at)?;
            Ok(Cbor::Other)
        }
        _ => Err("CBOR of a kind registration does not use".into()),
    }
}
