//! The SMP security functions (Core 5.4 Vol 3 Part H §2.2): the legacy `e`, `c1`, `s1`, `ah`
//! and the LE Secure Connections `f4`, `f5`, `f6` over AES-CMAC, plus P-256 key agreement.
//!
//! Every value here is in the order it has on the air and in HCI: least significant octet
//! first. The specification writes these functions most significant octet first, so `e` and
//! [`cmac`] reverse their inputs and output, as the Linux kernel (`net/bluetooth/smp.c`) does.
//! The tests are the kernel's self-test values, which come from the specification.

use aes::Aes128;
use aes::cipher::{BlockCipherEncrypt, KeyInit};
use cmac::{Cmac, Mac};
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::{PublicKey, SecretKey};

pub type Key = [u8; 16];

fn reversed<const N: usize>(bytes: &[u8; N]) -> [u8; N] {
    let mut out = *bytes;
    out.reverse();
    out
}

/// `e(key, plaintext)`: AES-128.
pub fn e(key: &Key, plaintext: &[u8; 16]) -> [u8; 16] {
    let cipher = Aes128::new(&reversed(key).into());
    let mut block = reversed(plaintext).into();
    cipher.encrypt_block(&mut block);
    reversed(&block.into())
}

/// AES-CMAC (RFC 4493) of `message` with `key`.
pub fn cmac(key: &Key, message: &[u8]) -> [u8; 16] {
    let mut mac = <Cmac<Aes128> as KeyInit>::new(&reversed(key).into());
    let mut msb = message.to_vec();
    msb.reverse();
    mac.update(&msb);
    reversed(&mac.finalize().into_bytes().into())
}

/// The legacy confirm value. `preq` and `pres` are the two pairing PDUs (with their codes),
/// `iat`/`rat` the address types (0 public, 1 random) of the initiator and the responder.
#[expect(
    clippy::too_many_arguments,
    reason = "the arguments of c1 in the specification"
)]
pub fn c1(
    key: &Key,
    r: &[u8; 16],
    preq: &[u8; 7],
    pres: &[u8; 7],
    iat: u8,
    ia: &[u8; 6],
    rat: u8,
    ra: &[u8; 6],
) -> [u8; 16] {
    let mut p1 = [0; 16];
    p1[0] = iat;
    p1[1] = rat;
    p1[2..9].copy_from_slice(preq);
    p1[9..16].copy_from_slice(pres);
    let mut p2 = [0; 16];
    p2[..6].copy_from_slice(ra);
    p2[6..12].copy_from_slice(ia);
    let step = e(key, &xor(r, &p1));
    e(key, &xor(&step, &p2))
}

/// The legacy short-term key: `r1` is the responder's random, `r2` the initiator's.
pub fn s1(key: &Key, r1: &[u8; 16], r2: &[u8; 16]) -> Key {
    let mut r = [0; 16];
    r[..8].copy_from_slice(&r2[..8]);
    r[8..].copy_from_slice(&r1[..8]);
    e(key, &r)
}

/// The random address hash: a resolvable private address is `hash || prand` (hash in the low
/// three octets), and `ah(irk, prand) == hash` when `irk` made it.
pub fn ah(irk: &Key, prand: &[u8; 3]) -> [u8; 3] {
    let mut r = [0; 16];
    r[..3].copy_from_slice(prand);
    let out = e(irk, &r);
    [out[0], out[1], out[2]]
}

/// Whether `irk` resolves the resolvable private address `address` (on-air order).
pub fn resolves(irk: &Key, address: &[u8; 6]) -> bool {
    address[5] & 0xc0 == 0x40 && ah(irk, &[address[3], address[4], address[5]]) == address[..3]
}

/// The LE Secure Connections confirm value `f4(U, V, X, Z)`.
pub fn f4(u: &[u8; 32], v: &[u8; 32], x: &Key, z: u8) -> [u8; 16] {
    let mut m = [0; 65];
    m[0] = z;
    m[1..33].copy_from_slice(v);
    m[33..].copy_from_slice(u);
    cmac(x, &m)
}

/// The LE Secure Connections key generation `f5(W, N1, N2, A1, A2)`: `(MacKey, LTK)`. `a1` and
/// `a2` are the initiator's and responder's addresses, each followed by its type octet.
pub fn f5(w: &[u8; 32], n1: &Key, n2: &Key, a1: &[u8; 7], a2: &[u8; 7]) -> (Key, Key) {
    const BTLE: [u8; 4] = [0x65, 0x6c, 0x74, 0x62];
    const SALT: Key = [
        0xbe, 0x83, 0x60, 0x5a, 0xdb, 0x0b, 0x37, 0x60, 0x38, 0xa5, 0xf5, 0xaa, 0x91, 0x83, 0x88,
        0x6c,
    ];
    let t = cmac(&SALT, w);
    let mut m = [0; 53];
    m[..2].copy_from_slice(&[0x00, 0x01]);
    m[2..9].copy_from_slice(a2);
    m[9..16].copy_from_slice(a1);
    m[16..32].copy_from_slice(n2);
    m[32..48].copy_from_slice(n1);
    m[48..52].copy_from_slice(&BTLE);
    let mac_key = cmac(&t, &m);
    m[52] = 1;
    (mac_key, cmac(&t, &m))
}

/// The LE Secure Connections check value `f6(W, N1, N2, R, IOcap, A1, A2)`. `io_cap` is the
/// IO capability, OOB flag and AuthReq octets of the pairing PDU, in PDU order.
pub fn f6(
    w: &Key,
    n1: &Key,
    n2: &Key,
    r: &Key,
    io_cap: &[u8; 3],
    a1: &[u8; 7],
    a2: &[u8; 7],
) -> [u8; 16] {
    let mut m = [0; 65];
    m[..7].copy_from_slice(a2);
    m[7..14].copy_from_slice(a1);
    m[14..17].copy_from_slice(io_cap);
    m[17..33].copy_from_slice(r);
    m[33..49].copy_from_slice(n2);
    m[49..].copy_from_slice(n1);
    cmac(w, &m)
}

fn xor(a: &[u8; 16], b: &[u8; 16]) -> [u8; 16] {
    std::array::from_fn(|i| a[i] ^ b[i])
}

/// A P-256 key pair for one LE Secure Connections pairing.
pub struct KeyPair {
    secret: SecretKey,
    /// The public key as the Pairing Public Key PDU carries it: X then Y, each least
    /// significant octet first.
    pub public: [u8; 64],
}

impl KeyPair {
    /// A key pair from 32 random bytes; `None` when they are not a valid scalar (zero or not
    /// below the group order), so the caller draws again.
    pub fn from_random(bytes: &[u8; 32]) -> Option<Self> {
        let secret = SecretKey::from_slice(bytes).ok()?;
        let point = secret.public_key().to_sec1_point(false);
        let (x, y) = (point.x()?, point.y()?);
        let mut public = [0; 64];
        public[..32].copy_from_slice(x);
        public[32..].copy_from_slice(y);
        public[..32].reverse();
        public[32..].reverse();
        Some(Self { secret, public })
    }

    /// The DHKey with the peer's public key (PDU order), least significant octet first.
    /// `None` when the peer's key is not a point on the curve: the pairing must fail.
    pub fn dh_key(&self, peer: &[u8; 64]) -> Option<[u8; 32]> {
        let mut sec1 = [0x04; 65];
        sec1[1..33].copy_from_slice(&peer[..32]);
        sec1[33..].copy_from_slice(&peer[32..]);
        sec1[1..33].reverse();
        sec1[33..].reverse();
        let peer = PublicKey::from_sec1_bytes(&sec1).ok()?;
        let shared = p256::ecdh::diffie_hellman(self.secret.to_nonzero_scalar(), peer.as_affine());
        let mut key: [u8; 32] = (*shared.raw_secret_bytes()).into();
        key.reverse();
        Some(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ah_matches_the_specification_sample() {
        let irk = [
            0x9b, 0x7d, 0x39, 0x0a, 0xa6, 0x10, 0x10, 0x34, 0x05, 0xad, 0xc8, 0x57, 0xa3, 0x34,
            0x02, 0xec,
        ];
        assert_eq!(ah(&irk, &[0x94, 0x81, 0x70]), [0xaa, 0xfb, 0x0d]);
        assert!(resolves(&irk, &[0xaa, 0xfb, 0x0d, 0x94, 0x81, 0x70]));
        assert!(!resolves(&irk, &[0xab, 0xfb, 0x0d, 0x94, 0x81, 0x70]));
    }

    #[test]
    fn c1_and_s1_match_the_specification_samples() {
        let r = [
            0xe0, 0x2e, 0x70, 0xc6, 0x4e, 0x27, 0x88, 0x63, 0x0e, 0x6f, 0xad, 0x56, 0x21, 0xd5,
            0x83, 0x57,
        ];
        let preq = [0x01, 0x01, 0x00, 0x00, 0x10, 0x07, 0x07];
        let pres = [0x02, 0x03, 0x00, 0x00, 0x08, 0x00, 0x05];
        let ia = [0xa6, 0xa5, 0xa4, 0xa3, 0xa2, 0xa1];
        let ra = [0xb6, 0xb5, 0xb4, 0xb3, 0xb2, 0xb1];
        let expected = [
            0x86, 0x3b, 0xf1, 0xbe, 0xc5, 0x4d, 0xa7, 0xd2, 0xea, 0x88, 0x89, 0x87, 0xef, 0x3f,
            0x1e, 0x1e,
        ];
        assert_eq!(c1(&[0; 16], &r, &preq, &pres, 1, &ia, 0, &ra), expected);
        let mut r1 = [0; 16];
        r1[..8].copy_from_slice(&[0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]);
        let mut r2 = [0; 16];
        r2[..8].copy_from_slice(&[0x00, 0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99]);
        let expected = [
            0x62, 0xa0, 0x6d, 0x79, 0xae, 0x16, 0x42, 0x5b, 0x9b, 0xf4, 0xb0, 0xe8, 0xf0, 0xe1,
            0x1f, 0x9a,
        ];
        assert_eq!(s1(&[0; 16], &r1, &r2), expected);
    }

    const U: [u8; 32] = [
        0xe6, 0x9d, 0x35, 0x0e, 0x48, 0x01, 0x03, 0xcc, 0xdb, 0xfd, 0xf4, 0xac, 0x11, 0x91, 0xf4,
        0xef, 0xb9, 0xa5, 0xf9, 0xe9, 0xa7, 0x83, 0x2c, 0x5e, 0x2c, 0xbe, 0x97, 0xf2, 0xd2, 0x03,
        0xb0, 0x20,
    ];
    const V: [u8; 32] = [
        0xfd, 0xc5, 0x7f, 0xf4, 0x49, 0xdd, 0x4f, 0x6b, 0xfb, 0x7c, 0x9d, 0xf1, 0xc2, 0x9a, 0xcb,
        0x59, 0x2a, 0xe7, 0xd4, 0xee, 0xfb, 0xfc, 0x0a, 0x90, 0x9a, 0xbb, 0xf6, 0x32, 0x3d, 0x8b,
        0x18, 0x55,
    ];
    const N1: Key = [
        0xab, 0xae, 0x2b, 0x71, 0xec, 0xb2, 0xff, 0xff, 0x3e, 0x73, 0x77, 0xd1, 0x54, 0x84, 0xcb,
        0xd5,
    ];
    const N2: Key = [
        0xcf, 0xc4, 0x3d, 0xff, 0xf7, 0x83, 0x65, 0x21, 0x6e, 0x5f, 0xa7, 0x25, 0xcc, 0xe7, 0xe8,
        0xa6,
    ];
    const A1: [u8; 7] = [0xce, 0xbf, 0x37, 0x37, 0x12, 0x56, 0x00];
    const A2: [u8; 7] = [0xc1, 0xcf, 0x2d, 0x70, 0x13, 0xa7, 0x00];
    const MAC_KEY: Key = [
        0x20, 0x6e, 0x63, 0xce, 0x20, 0x6a, 0x3f, 0xfd, 0x02, 0x4a, 0x08, 0xa1, 0x76, 0xf1, 0x65,
        0x29,
    ];

    #[test]
    fn f4_f5_f6_match_the_specification_samples() {
        let expected = [
            0x2d, 0x87, 0x74, 0xa9, 0xbe, 0xa1, 0xed, 0xf1, 0x1c, 0xbd, 0xa9, 0x07, 0xf1, 0x16,
            0xc9, 0xf2,
        ];
        assert_eq!(f4(&U, &V, &N1, 0), expected);

        let w = [
            0x98, 0xa6, 0xbf, 0x73, 0xf3, 0x34, 0x8d, 0x86, 0xf1, 0x66, 0xf8, 0xb4, 0x13, 0x6b,
            0x79, 0x99, 0x9b, 0x7d, 0x39, 0x0a, 0xa6, 0x10, 0x10, 0x34, 0x05, 0xad, 0xc8, 0x57,
            0xa3, 0x34, 0x02, 0xec,
        ];
        let ltk = [
            0x38, 0x0a, 0x75, 0x94, 0xb5, 0x22, 0x05, 0x98, 0x23, 0xcd, 0xd7, 0x69, 0x11, 0x79,
            0x86, 0x69,
        ];
        assert_eq!(f5(&w, &N1, &N2, &A1, &A2), (MAC_KEY, ltk));

        let r = [
            0xc8, 0x0f, 0x2d, 0x0c, 0xd2, 0x42, 0xda, 0x08, 0x54, 0xbb, 0x53, 0xb4, 0x3b, 0x34,
            0xa3, 0x12,
        ];
        let expected = [
            0x61, 0x8f, 0x95, 0xda, 0x09, 0x0b, 0x6c, 0xd2, 0xc5, 0xe8, 0xd0, 0x9c, 0x98, 0x73,
            0xc4, 0xe3,
        ];
        assert_eq!(
            f6(&MAC_KEY, &N1, &N2, &r, &[0x02, 0x01, 0x01], &A1, &A2),
            expected
        );
    }

    #[test]
    fn both_sides_of_a_key_agreement_get_the_same_dh_key() {
        let a = KeyPair::from_random(&[0x11; 32]).unwrap();
        let b = KeyPair::from_random(&[0x22; 32]).unwrap();
        assert_eq!(a.dh_key(&b.public), b.dh_key(&a.public));
        assert!(a.dh_key(&b.public).is_some());
        // Not a point on the curve.
        assert_eq!(a.dh_key(&[1; 64]), None);
        // Zero is not a scalar.
        assert!(KeyPair::from_random(&[0; 32]).is_none());
    }

    #[test]
    fn the_debug_key_pair_gives_the_specification_public_key() {
        // Core Vol 3 Part H §2.3.5.6.1, the debug private key (most significant octet first).
        let mut private = [0; 32];
        let hex = "3f49f6d4a3c55f3874c9b3e3d2103f504aff607beb40b7995899b8a6cd3c1abd";
        for (i, byte) in private.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        let pair = KeyPair::from_random(&private).unwrap();
        let x_hex = "20b003d2f297be2c5e2c83a7e9f9a5b9eff49111acf4fddbcc0301480e359de6";
        let mut x: Vec<u8> = (0..32)
            .map(|i| u8::from_str_radix(&x_hex[i * 2..i * 2 + 2], 16).unwrap())
            .collect();
        x.reverse();
        assert_eq!(&pair.public[..32], &x[..]);
    }
}
