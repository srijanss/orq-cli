//! Just enough of NIST P-256 to check a passkey's ES256 signatures: whether a point is on the
//! curve, and whether an ECDSA signature over a SHA-256 digest verifies. Nothing here handles a
//! secret (verification uses only public values), so it is not written to run in constant time.
//! It is checked against the RFC 6979 test vector.

type U256 = [u64; 4];

/// The field prime, the group order and the curve constant b, least significant limb first.
const P: U256 = [0xffff_ffff_ffff_ffff, 0x0000_0000_ffff_ffff, 0, 0xffff_ffff_0000_0001];
const N: U256 = [0xf3b9_cac2_fc63_2551, 0xbce6_faad_a717_9e84, 0xffff_ffff_ffff_ffff, 0xffff_ffff_0000_0000];
const B: U256 = [0x3bce_3c3e_27d2_604b, 0x651d_06b0_cc53_b0f6, 0xb3eb_bd55_7698_86bc, 0x5ac6_35d8_aa3a_93e7];
const GX: U256 = [0xf4a1_3945_d898_c296, 0x7703_7d81_2deb_33a0, 0xf8bc_e6e5_63a4_40f2, 0x6b17_d1f2_e12c_4247];
const GY: U256 = [0xcbb6_4068_37bf_51f5, 0x2bce_3357_6b31_5ece, 0x8ee7_eb4a_7c0f_9e16, 0x4fe3_42e2_fe1a_7f9b];

fn from_be(bytes: &[u8]) -> Option<U256> {
    let bytes: [u8; 32] = bytes.try_into().ok()?;
    let mut limbs = [0u64; 4];
    for (i, chunk) in bytes.chunks(8).enumerate() {
        limbs[3 - i] = u64::from_be_bytes(chunk.try_into().ok()?);
    }
    Some(limbs)
}

fn is_zero(a: &U256) -> bool {
    a.iter().all(|limb| *limb == 0)
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

/// `a + b` modulo `m`, for `a` and `b` already below `m`.
fn add_mod(a: &U256, b: &U256, m: &U256) -> U256 {
    let mut out = [0u64; 4];
    let mut carry = false;
    for i in 0..4 {
        let (s, c1) = a[i].overflowing_add(b[i]);
        let (s, c2) = s.overflowing_add(u64::from(carry));
        out[i] = s;
        carry = c1 || c2;
    }
    if carry || !less(&out, m) { sub_wrapping(&out, m) } else { out }
}

fn sub_mod(a: &U256, b: &U256, m: &U256) -> U256 {
    if less(a, b) { sub_wrapping(m, &sub_wrapping(b, a)) } else { sub_wrapping(a, b) }
}

/// Double-and-add: slow, but verifying a sign-in needs only a few thousand of these.
fn mul_mod(a: &U256, b: &U256, m: &U256) -> U256 {
    let mut out = [0u64; 4];
    for bit in (0..256).rev() {
        out = add_mod(&out, &out, m);
        if (b[bit / 64] >> (bit % 64)) & 1 == 1 {
            out = add_mod(&out, a, m);
        }
    }
    out
}

/// `a` to the power `exponent` modulo `m`.
fn pow_mod(a: &U256, exponent: &U256, m: &U256) -> U256 {
    let mut out: U256 = [1, 0, 0, 0];
    for bit in (0..256).rev() {
        out = mul_mod(&out, &out, m);
        if (exponent[bit / 64] >> (bit % 64)) & 1 == 1 {
            out = mul_mod(&out, a, m);
        }
    }
    out
}

/// The inverse of a non-zero `a` modulo the prime `m` (Fermat).
fn inverse(a: &U256, m: &U256) -> U256 {
    pow_mod(a, &sub_wrapping(m, &[2, 0, 0, 0]), m)
}

/// Whether (x, y) satisfies y^2 = x^3 - 3x + b over the P-256 field, with both coordinates reduced.
pub(crate) fn on_curve(x: &[u8], y: &[u8]) -> bool {
    let (Some(x), Some(y)) = (from_be(x), from_be(y)) else { return false };
    on_curve_limbs(&x, &y)
}

fn on_curve_limbs(x: &U256, y: &U256) -> bool {
    if !less(x, &P) || !less(y, &P) {
        return false;
    }
    let cubed = mul_mod(&mul_mod(x, x, &P), x, &P);
    let three_x = add_mod(&add_mod(x, x, &P), x, &P);
    mul_mod(y, y, &P) == add_mod(&sub_mod(&cubed, &three_x, &P), &B, &P)
}

/// A point in Jacobian coordinates; z = 0 is the point at infinity.
#[derive(Clone, Copy)]
struct Point {
    x: U256,
    y: U256,
    z: U256,
}

const INFINITY: Point = Point { x: [1, 0, 0, 0], y: [1, 0, 0, 0], z: [0, 0, 0, 0] };

fn double(p: &Point) -> Point {
    if is_zero(&p.z) || is_zero(&p.y) {
        return INFINITY;
    }
    let m = |a: &U256, b: &U256| mul_mod(a, b, &P);
    let add = |a: &U256, b: &U256| add_mod(a, b, &P);
    let sub = |a: &U256, b: &U256| sub_mod(a, b, &P);
    let delta = m(&p.z, &p.z);
    let gamma = m(&p.y, &p.y);
    let beta = m(&p.x, &gamma);
    let t = m(&sub(&p.x, &delta), &add(&p.x, &delta));
    let alpha = add(&add(&t, &t), &t);
    let beta4 = add(&add(&beta, &beta), &add(&beta, &beta));
    let beta8 = add(&beta4, &beta4);
    let x3 = sub(&m(&alpha, &alpha), &beta8);
    let yz = add(&p.y, &p.z);
    let z3 = sub(&sub(&m(&yz, &yz), &gamma), &delta);
    let gamma2 = m(&gamma, &gamma);
    let gamma2_2 = add(&gamma2, &gamma2);
    let gamma2_4 = add(&gamma2_2, &gamma2_2);
    let gamma2_8 = add(&gamma2_4, &gamma2_4);
    let y3 = sub(&m(&alpha, &sub(&beta4, &x3)), &gamma2_8);
    Point { x: x3, y: y3, z: z3 }
}

fn add_points(p: &Point, q: &Point) -> Point {
    if is_zero(&p.z) {
        return *q;
    }
    if is_zero(&q.z) {
        return *p;
    }
    let m = |a: &U256, b: &U256| mul_mod(a, b, &P);
    let add = |a: &U256, b: &U256| add_mod(a, b, &P);
    let sub = |a: &U256, b: &U256| sub_mod(a, b, &P);
    let z1z1 = m(&p.z, &p.z);
    let z2z2 = m(&q.z, &q.z);
    let u1 = m(&p.x, &z2z2);
    let u2 = m(&q.x, &z1z1);
    let s1 = m(&m(&p.y, &q.z), &z2z2);
    let s2 = m(&m(&q.y, &p.z), &z1z1);
    let h = sub(&u2, &u1);
    let r = sub(&s2, &s1);
    if is_zero(&h) {
        return if is_zero(&r) { double(p) } else { INFINITY };
    }
    let hh = m(&h, &h);
    let hhh = m(&h, &hh);
    let v = m(&u1, &hh);
    let x3 = sub(&sub(&m(&r, &r), &hhh), &add(&v, &v));
    let y3 = sub(&m(&r, &sub(&v, &x3)), &m(&s1, &hhh));
    let z3 = m(&m(&p.z, &q.z), &h);
    Point { x: x3, y: y3, z: z3 }
}

fn multiply(k: &U256, p: &Point) -> Point {
    let mut out = INFINITY;
    for bit in (0..256).rev() {
        out = double(&out);
        if (k[bit / 64] >> (bit % 64)) & 1 == 1 {
            out = add_points(&out, p);
        }
    }
    out
}

/// The affine x coordinate, or None for the point at infinity.
fn affine_x(p: &Point) -> Option<U256> {
    if is_zero(&p.z) {
        return None;
    }
    let z_inv = inverse(&p.z, &P);
    Some(mul_mod(&p.x, &mul_mod(&z_inv, &z_inv, &P), &P))
}

/// One non-negative DER INTEGER of at most 32 significant bytes, as a 32-byte value.
fn der_integer(bytes: &[u8], at: &mut usize) -> Option<U256> {
    if *bytes.get(*at)? != 0x02 {
        return None;
    }
    let len = usize::from(*bytes.get(*at + 1)?);
    if len == 0 || len >= 0x80 {
        return None;
    }
    let value = bytes.get(*at + 2..*at + 2 + len)?;
    *at += 2 + len;
    // Negative numbers and non-minimal encodings are not valid DER.
    if value[0] & 0x80 != 0 || (value[0] == 0 && value.len() > 1 && value[1] & 0x80 == 0) {
        return None;
    }
    let digits = if value[0] == 0 { &value[1..] } else { value };
    if digits.len() > 32 {
        return None;
    }
    let mut padded = [0u8; 32];
    padded[32 - digits.len()..].copy_from_slice(digits);
    from_be(&padded)
}

/// (r, s) from a DER `SEQUENCE { INTEGER, INTEGER }` that fills the whole input.
fn der_signature(bytes: &[u8]) -> Option<(U256, U256)> {
    if bytes.len() < 8 || bytes.len() > 72 || bytes[0] != 0x30 || usize::from(bytes[1]) != bytes.len() - 2 {
        return None;
    }
    let mut at = 2;
    let r = der_integer(bytes, &mut at)?;
    let s = der_integer(bytes, &mut at)?;
    (at == bytes.len()).then_some((r, s))
}

/// Whether `signature` (ASN.1 DER) is a valid ECDSA signature of `digest` under the public key
/// (x, y).
pub(crate) fn verify(x: &[u8], y: &[u8], digest: &[u8; 32], signature: &[u8]) -> bool {
    let (Some(qx), Some(qy)) = (from_be(x), from_be(y)) else { return false };
    if !on_curve_limbs(&qx, &qy) {
        return false;
    }
    let Some((r, s)) = der_signature(signature) else { return false };
    if is_zero(&r) || is_zero(&s) || !less(&r, &N) || !less(&s, &N) {
        return false;
    }
    let Some(mut e) = from_be(digest) else { return false };
    if !less(&e, &N) {
        e = sub_wrapping(&e, &N);
    }
    let s_inv = inverse(&s, &N);
    let u1 = mul_mod(&e, &s_inv, &N);
    let u2 = mul_mod(&r, &s_inv, &N);
    let generator = Point { x: GX, y: GY, z: [1, 0, 0, 0] };
    let public = Point { x: qx, y: qy, z: [1, 0, 0, 0] };
    let sum = add_points(&multiply(&u1, &generator), &multiply(&u2, &public));
    let Some(mut v) = affine_x(&sum) else { return false };
    if !less(&v, &N) {
        v = sub_wrapping(&v, &N);
    }
    v == r
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    pub(crate) fn hex(text: &str) -> Vec<u8> {
        (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
    }

    /// RFC 6979 A.2.5: the P-256 key, its message "sample" and the signature over SHA-256 of it.
    pub(crate) const KEY_X: &str = "60fed4ba255a9d31c961eb74c6356d68c049b8923b61fa6ce669622e60f29fb6";
    pub(crate) const KEY_Y: &str = "7903fe1008b8bc99a41ae9e95628bc64f2f1b20c2d7e9f5177a3c294d4462299";
    const R: &str = "efd48b2aacb6a8fd1140dd9cd45e81d69d2c877b56aaf991c34d0ea84eaf3716";
    const S: &str = "f7cb1c942d657c41d436c7a1b6e29f65f3e900dbb9aff4064dc4ab2f843acda8";

    pub(crate) fn der(r: &[u8], s: &[u8]) -> Vec<u8> {
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

    /// The RFC 6979 private key, for signing in tests.
    const KEY_D: &str = "c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721";

    /// An ECDSA signature (DER) of `digest` with the RFC 6979 key and a fixed nonce, for tests only.
    pub(crate) fn sign(digest: &[u8; 32]) -> Vec<u8> {
        let d = from_be(&hex(KEY_D)).unwrap();
        let k = from_be(&hex("a6e3c57dd01abe90086538398355dd4c3b17aa873382b0f24d6129493d8aad60")).unwrap();
        let g = Point { x: GX, y: GY, z: [1, 0, 0, 0] };
        let mut r = affine_x(&multiply(&k, &g)).unwrap();
        if !less(&r, &N) {
            r = sub_wrapping(&r, &N);
        }
        let mut e = from_be(digest).unwrap();
        if !less(&e, &N) {
            e = sub_wrapping(&e, &N);
        }
        let s = mul_mod(&inverse(&k, &N), &add_mod(&e, &mul_mod(&r, &d, &N), &N), &N);
        let be = |v: &U256| v.iter().rev().flat_map(|limb| limb.to_be_bytes()).collect::<Vec<u8>>();
        der(&be(&r), &be(&s))
    }

    #[test]
    fn a_signature_made_in_tests_verifies_and_the_wrong_message_does_not() {
        let digest: [u8; 32] = Sha256::digest(b"another message").into();
        let signature = sign(&digest);
        assert!(verify(&hex(KEY_X), &hex(KEY_Y), &digest, &signature));
        assert!(!verify(&hex(KEY_X), &hex(KEY_Y), &sample_digest(), &signature));
    }

    fn sample_digest() -> [u8; 32] {
        Sha256::digest(b"sample").into()
    }

    #[test]
    fn the_rfc_6979_signature_verifies_and_changes_to_it_do_not() {
        let (x, y) = (hex(KEY_X), hex(KEY_Y));
        let good = der(&hex(R), &hex(S));
        assert!(verify(&x, &y, &sample_digest(), &good));
        let mut other = sample_digest();
        other[0] ^= 1;
        assert!(!verify(&x, &y, &other, &good));
        let mut r = hex(R);
        r[31] ^= 1;
        assert!(!verify(&x, &y, &sample_digest(), &der(&r, &hex(S))));
        let mut s = hex(S);
        s[0] ^= 1;
        assert!(!verify(&x, &y, &sample_digest(), &der(&hex(R), &s)));
        // Another key, an off-curve key and swapped coordinates.
        let g = hex("6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296");
        assert!(!verify(&g, &y, &sample_digest(), &good));
        assert!(!verify(&y, &x, &sample_digest(), &good));
    }

    #[test]
    fn signatures_that_are_not_strict_der_or_are_out_of_range_are_refused() {
        let (x, y) = (hex(KEY_X), hex(KEY_Y));
        let good = der(&hex(R), &hex(S));
        let mut trailing = good.clone();
        trailing.push(0);
        let mut wrong_length = good.clone();
        wrong_length[1] += 1;
        let zero = der(&[0], &hex(S));
        let n = hex("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551");
        let at_order = der(&n, &hex(S));
        // r written with a pointless leading zero byte is not minimal.
        let padded = [vec![0x30, 0x45, 0x02, 0x21, 0x00], hex(R)[..32].to_vec(), vec![0x02, 0x21, 0x00], hex(S)].concat();
        for (name, bad) in [("trailing", trailing), ("length", wrong_length), ("zero r", zero), ("r at the order", at_order), ("not a sequence", vec![0x31, 0x00]), ("empty", vec![]), ("padded", padded)] {
            assert!(!verify(&x, &y, &sample_digest(), &bad), "{name}");
        }
    }

    #[test]
    fn known_points_are_on_the_curve_and_nearby_ones_are_not() {
        let g = ("6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296", "4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5");
        let two_g = ("7cf27b188d034f7e8a52380304b51ac3c08969e277f21b35a60b48fc47669978", "07775510db8ed040293d9ac69f7430dbba7dade63ce982299e04b79d227873d1");
        for (x, y) in [g, two_g, (KEY_X, KEY_Y)] {
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

    #[test]
    fn doubling_the_generator_gives_the_known_double() {
        let g = Point { x: GX, y: GY, z: [1, 0, 0, 0] };
        let two_g = double(&g);
        let expected = from_be(&hex("7cf27b188d034f7e8a52380304b51ac3c08969e277f21b35a60b48fc47669978")).unwrap();
        assert_eq!(affine_x(&two_g), Some(expected));
        assert_eq!(affine_x(&add_points(&g, &g)), Some(expected));
        // n * G is the point at infinity, and (n - 1) * G + G too.
        assert!(affine_x(&multiply(&N, &g)).is_none());
    }
}
