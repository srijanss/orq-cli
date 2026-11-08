//! ES256 (ECDSA over P-256 with SHA-256) checks for passkeys, on the `p256` crate.

use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use p256::EncodedPoint;

/// The public key for the 32-byte coordinates, if it is a point on P-256.
fn key(x: &[u8], y: &[u8]) -> Option<VerifyingKey> {
    let (x, y): (&[u8; 32], &[u8; 32]) = (x.try_into().ok()?, y.try_into().ok()?);
    VerifyingKey::from_encoded_point(&EncodedPoint::from_affine_coordinates(x.into(), y.into(), false)).ok()
}

pub(crate) fn on_curve(x: &[u8], y: &[u8]) -> bool {
    key(x, y).is_some()
}

/// Whether `signature` (ASN.1 DER) is a valid ES256 signature of `message` under the key (x, y).
pub(crate) fn verify(x: &[u8], y: &[u8], message: &[u8], signature: &[u8]) -> bool {
    match (key(x, y), Signature::from_der(signature)) {
        (Some(key), Ok(signature)) => key.verify(message, &signature).is_ok(),
        _ => false,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use p256::ecdsa::signature::Signer;
    use p256::ecdsa::SigningKey;

    pub(crate) fn hex(text: &str) -> Vec<u8> {
        (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
    }

    /// RFC 6979 A.2.5's P-256 key, used to sign in tests.
    pub(crate) const KEY_X: &str = "60fed4ba255a9d31c961eb74c6356d68c049b8923b61fa6ce669622e60f29fb6";
    pub(crate) const KEY_Y: &str = "7903fe1008b8bc99a41ae9e95628bc64f2f1b20c2d7e9f5177a3c294d4462299";
    const KEY_D: &str = "c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721";
    const R: &str = "efd48b2aacb6a8fd1140dd9cd45e81d69d2c877b56aaf991c34d0ea84eaf3716";
    const S: &str = "f7cb1c942d657c41d436c7a1b6e29f65f3e900dbb9aff4064dc4ab2f843acda8";

    /// A DER signature of `message` with the test key.
    pub(crate) fn sign(message: &[u8]) -> Vec<u8> {
        let key = SigningKey::from_slice(&hex(KEY_D)).unwrap();
        let signature: Signature = key.sign(message);
        signature.to_der().as_bytes().to_vec()
    }

    fn der(r: &[u8], s: &[u8]) -> Vec<u8> {
        let int = |v: &[u8]| {
            let mut digits: Vec<u8> = v.iter().copied().skip_while(|b| *b == 0).collect();
            if digits.first().is_none_or(|b| b & 0x80 != 0) {
                digits.insert(0, 0);
            }
            [vec![0x02, digits.len() as u8], digits].concat()
        };
        let body = [int(r), int(s)].concat();
        [vec![0x30, body.len() as u8], body].concat()
    }

    #[test]
    fn the_rfc_6979_signature_verifies_and_changes_to_it_do_not() {
        let (x, y) = (hex(KEY_X), hex(KEY_Y));
        let good = der(&hex(R), &hex(S));
        assert!(verify(&x, &y, b"sample", &good));
        assert!(!verify(&x, &y, b"sample2", &good));
        let mut r = hex(R);
        r[31] ^= 1;
        assert!(!verify(&x, &y, b"sample", &der(&r, &hex(S))));
        assert!(!verify(&y, &x, b"sample", &good));
        for bad in [vec![], vec![0x30, 0x00], [good.clone(), vec![0]].concat()] {
            assert!(!verify(&x, &y, b"sample", &bad));
        }
        assert!(verify(&x, &y, b"anything", &sign(b"anything")));
    }

    #[test]
    fn only_points_on_the_curve_count() {
        let g = ("6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296", "4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5");
        assert!(on_curve(&hex(g.0), &hex(g.1)) && on_curve(&hex(KEY_X), &hex(KEY_Y)));
        let mut off = hex(g.1);
        off[31] ^= 1;
        assert!(!on_curve(&hex(g.0), &off));
        assert!(!on_curve(&[0; 32], &[0; 32]));
        assert!(!on_curve(&[0xff; 32], &hex(g.1)));
        assert!(!on_curve(&hex(g.0)[1..], &hex(g.1)));
    }
}
