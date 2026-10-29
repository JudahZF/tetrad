//! The Security Manager Protocol (Core Vol 3 Part H): Just Works pairing, with LE Secure
//! Connections when both sides have it and legacy pairing when not, then the key distribution
//! that makes a bond. Tetrad has no display or keyboard in this protocol (IO capability
//! NoInputNoOutput); the Tetrad app's PIN is a separate check on top (docs/bluetooth.md).
//!
//! Two roles: a responder answers a central (a phone pairs with Tetrad), an initiator pairs with
//! a peripheral (Tetrad pairs with a device). The machine only makes PDUs and keys; the host
//! sends them and starts or answers the encryption.

use crate::crypto::{self, Key, KeyPair};
use crate::hci::Address;

/// SMP command codes.
pub mod code {
    pub const PAIRING_REQUEST: u8 = 0x01;
    pub const PAIRING_RESPONSE: u8 = 0x02;
    pub const CONFIRM: u8 = 0x03;
    pub const RANDOM: u8 = 0x04;
    pub const FAILED: u8 = 0x05;
    pub const ENCRYPTION_INFORMATION: u8 = 0x06;
    pub const CENTRAL_IDENTIFICATION: u8 = 0x07;
    pub const IDENTITY_INFORMATION: u8 = 0x08;
    pub const IDENTITY_ADDRESS: u8 = 0x09;
    pub const SIGNING_INFORMATION: u8 = 0x0a;
    pub const SECURITY_REQUEST: u8 = 0x0b;
    pub const PUBLIC_KEY: u8 = 0x0c;
    pub const DHKEY_CHECK: u8 = 0x0d;
}

/// Pairing Failed reasons.
pub mod reason {
    pub const CONFIRM_VALUE_FAILED: u8 = 0x04;
    pub const PAIRING_NOT_SUPPORTED: u8 = 0x05;
    pub const ENCRYPTION_KEY_SIZE: u8 = 0x06;
    pub const UNSPECIFIED: u8 = 0x08;
    pub const INVALID_PARAMETERS: u8 = 0x0a;
    pub const DHKEY_CHECK_FAILED: u8 = 0x0b;
}

/// AuthReq bits.
const BONDING: u8 = 0x01;
const SECURE_CONNECTIONS: u8 = 0x08;
/// Key distribution bits.
const ENC_KEY: u8 = 0x01;
const ID_KEY: u8 = 0x02;
const NO_INPUT_NO_OUTPUT: u8 = 0x03;
/// The only key size Tetrad takes.
const KEY_SIZE: u8 = 16;

/// The two addresses of the link, each with its type (0 public, 1 random).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Addresses {
    pub initiator: (u8, Address),
    pub responder: (u8, Address),
}

impl Addresses {
    fn with_type((kind, address): (u8, Address)) -> [u8; 7] {
        let mut out = [0; 7];
        out[..6].copy_from_slice(&address);
        out[6] = kind;
        out
    }
}

/// The keys of a finished pairing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keys {
    pub ltk: Key,
    pub ediv: u16,
    pub rand: [u8; 8],
    pub secure: bool,
    /// Both sides asked for a bond: keep the keys.
    pub bond: bool,
    pub peer_irk: Option<Key>,
    pub peer_identity: Option<(u8, Address)>,
}

/// What the host does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Send(Vec<u8>),
    /// The initiator encrypts the link with this key (EDIV and Rand 0).
    Encrypt(Key),
    /// The pairing ended without keys. `sent`: a Pairing Failed went to the peer.
    Failed {
        reason: u8,
    },
    Done(Keys),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// LE SC: the peer's public key (responder), or the peer's (initiator) after ours.
    PublicKey,
    /// The peer's confirm value.
    Confirm,
    /// The peer's random value.
    Random,
    /// LE SC: the peer's DHKey check.
    DhKeyCheck,
    /// The link encrypts with the new key.
    Encryption,
    /// Keys from the peer.
    Distribution,
    Done,
}

/// One pairing on one link.
pub struct Pairing {
    initiator: bool,
    phase: Phase,
    preq: [u8; 7],
    pres: [u8; 7],
    secure: bool,
    bond: bool,
    addresses: Addresses,
    key_pair: Option<KeyPair>,
    dh_key: [u8; 32],
    peer_public: [u8; 64],
    local_nonce: Key,
    peer_nonce: Key,
    peer_confirm: Key,
    mac_key: Key,
    /// The LTK (LE SC) or STK (legacy) that encrypts the link at the end of the pairing.
    key: Key,
    /// Keys still to come from the peer.
    expected: u8,
    /// Keys Tetrad gives (legacy, as responder: the LTK it makes).
    give: u8,
    keys: Keys,
}

fn random<const N: usize>(rng: &mut dyn FnMut(&mut [u8])) -> [u8; N] {
    let mut out = [0; N];
    rng(&mut out);
    out
}

fn new_key_pair(rng: &mut dyn FnMut(&mut [u8])) -> KeyPair {
    loop {
        if let Some(pair) = KeyPair::from_random(&random(rng)) {
            return pair;
        }
    }
}

fn pdu(code: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![code];
    out.extend_from_slice(body);
    out
}

fn array<const N: usize>(bytes: &[u8]) -> Option<[u8; N]> {
    bytes.get(..N)?.try_into().ok()
}

/// A Pairing Failed PDU.
pub fn failed(reason: u8) -> Vec<u8> {
    vec![code::FAILED, reason]
}

impl Pairing {
    /// Answers a central's Pairing Request. `Err` is the reason for a Pairing Failed.
    pub fn respond(request: &[u8], addresses: Addresses) -> Result<(Self, Vec<u8>), u8> {
        let preq: [u8; 7] = array(request).ok_or(reason::INVALID_PARAMETERS)?;
        let [_, _io, _oob, auth, max_key, initiator_keys, responder_keys] = preq;
        if max_key < KEY_SIZE {
            return Err(reason::ENCRYPTION_KEY_SIZE);
        }
        let secure = auth & SECURE_CONNECTIONS != 0;
        // A legacy bond needs the LTK that Tetrad gives: without it the pairing only lasts the
        // link (a bond would keep an all-zero key).
        let bond = auth & BONDING != 0 && (secure || responder_keys & ENC_KEY != 0);
        // The central's identity key (it resolves the central's private address); in legacy
        // pairing also an LTK from Tetrad, as the STK only lasts this link.
        let wanted_from_initiator = if bond { ID_KEY } else { 0 };
        let wanted_from_responder = if bond && !secure { ENC_KEY } else { 0 };
        let expected = initiator_keys & wanted_from_initiator;
        let give = responder_keys & wanted_from_responder;
        let our_auth = (if bond { BONDING } else { 0 }) | SECURE_CONNECTIONS;
        let pres = [
            code::PAIRING_RESPONSE,
            NO_INPUT_NO_OUTPUT,
            0,
            our_auth,
            KEY_SIZE,
            expected,
            give,
        ];
        let pairing = Self {
            initiator: false,
            phase: if secure {
                Phase::PublicKey
            } else {
                Phase::Confirm
            },
            preq,
            pres,
            secure,
            bond,
            addresses,
            key_pair: None,
            dh_key: [0; 32],
            peer_public: [0; 64],
            local_nonce: [0; 16],
            peer_nonce: [0; 16],
            peer_confirm: [0; 16],
            mac_key: [0; 16],
            key: [0; 16],
            expected,
            give,
            keys: Keys {
                ltk: [0; 16],
                ediv: 0,
                rand: [0; 8],
                secure,
                bond,
                peer_irk: None,
                peer_identity: None,
            },
        };
        Ok((pairing, pres.to_vec()))
    }

    /// Starts a pairing with a peripheral: the Pairing Request to send.
    pub fn initiate(addresses: Addresses) -> (Self, Vec<u8>) {
        let preq = [
            code::PAIRING_REQUEST,
            NO_INPUT_NO_OUTPUT,
            0,
            BONDING | SECURE_CONNECTIONS,
            KEY_SIZE,
            0,
            ENC_KEY | ID_KEY,
        ];
        let pairing = Self {
            initiator: true,
            phase: Phase::Confirm,
            preq,
            pres: [0; 7],
            secure: false,
            bond: true,
            addresses,
            key_pair: None,
            dh_key: [0; 32],
            peer_public: [0; 64],
            local_nonce: [0; 16],
            peer_nonce: [0; 16],
            peer_confirm: [0; 16],
            mac_key: [0; 16],
            key: [0; 16],
            expected: 0,
            give: 0,
            keys: Keys {
                ltk: [0; 16],
                ediv: 0,
                rand: [0; 8],
                secure: false,
                bond: true,
                peer_irk: None,
                peer_identity: None,
            },
        };
        (pairing, preq.to_vec())
    }

    pub fn secure(&self) -> bool {
        self.secure
    }

    /// The key for the encryption that ends the pairing, once the pairing has one.
    pub fn link_key(&self) -> Option<Key> {
        (self.phase == Phase::Encryption).then_some(self.key)
    }

    fn fail(&mut self, reason: u8) -> Vec<Step> {
        self.phase = Phase::Done;
        vec![Step::Send(failed(reason)), Step::Failed { reason }]
    }

    fn legacy_confirm(&self, rand: &Key) -> Key {
        let (iat, ia) = self.addresses.initiator;
        let (rat, ra) = self.addresses.responder;
        crypto::c1(&[0; 16], rand, &self.preq, &self.pres, iat, &ia, rat, &ra)
    }

    /// The local and the peer's public key X, in the order f4 takes them for `local` first.
    fn x(&self, local: bool) -> [u8; 32] {
        let key = if local {
            self.key_pair.as_ref().map_or([0; 64], |pair| pair.public)
        } else {
            self.peer_public
        };
        array(&key).unwrap_or([0; 32])
    }

    /// One SMP PDU from the peer.
    pub fn on_pdu(&mut self, packet: &[u8], rng: &mut dyn FnMut(&mut [u8])) -> Vec<Step> {
        let Some((&command, body)) = packet.split_first() else {
            return self.fail(reason::INVALID_PARAMETERS);
        };
        if command == code::FAILED {
            self.phase = Phase::Done;
            return vec![Step::Failed {
                reason: body.first().copied().unwrap_or(reason::UNSPECIFIED),
            }];
        }
        match (self.phase, command) {
            (Phase::Confirm, code::PAIRING_RESPONSE) if self.initiator => {
                self.on_response(packet, rng)
            }
            (Phase::PublicKey, code::PUBLIC_KEY) => self.on_public_key(body, rng),
            (Phase::Confirm, code::CONFIRM) => {
                let Some(confirm) = array(body) else {
                    return self.fail(reason::INVALID_PARAMETERS);
                };
                self.peer_confirm = confirm;
                self.on_confirm(rng)
            }
            (Phase::Random, code::RANDOM) => {
                let Some(nonce) = array(body) else {
                    return self.fail(reason::INVALID_PARAMETERS);
                };
                self.peer_nonce = nonce;
                self.on_random()
            }
            (Phase::DhKeyCheck, code::DHKEY_CHECK) => self.on_dh_key_check(body),
            (Phase::Distribution, _) => self.on_key(command, body),
            // A peripheral asks for security while Tetrad pairs: nothing more to do.
            (_, code::SECURITY_REQUEST) => Vec::new(),
            _ => self.fail(reason::UNSPECIFIED),
        }
    }

    /// Initiator: the peripheral answered the request.
    fn on_response(&mut self, packet: &[u8], rng: &mut dyn FnMut(&mut [u8])) -> Vec<Step> {
        let Some(pres) = array::<7>(packet) else {
            return self.fail(reason::INVALID_PARAMETERS);
        };
        if pres[4] < KEY_SIZE {
            return self.fail(reason::ENCRYPTION_KEY_SIZE);
        }
        self.pres = pres;
        // Each side must set the bit; the request may be legacy (the tests make one).
        self.secure = self.preq[3] & pres[3] & SECURE_CONNECTIONS != 0;
        // As in `respond`: a legacy bond needs the peripheral's LTK.
        self.bond =
            self.preq[3] & pres[3] & BONDING != 0 && (self.secure || pres[6] & ENC_KEY != 0);
        self.keys.secure = self.secure;
        self.keys.bond = self.bond;
        // Keys the peripheral gives: its LTK (legacy only) and its identity.
        self.expected = pres[6]
            & if self.secure {
                ID_KEY
            } else {
                ENC_KEY | ID_KEY
            };
        self.give = 0;
        if self.secure {
            let pair = new_key_pair(rng);
            let public = pair.public;
            self.key_pair = Some(pair);
            self.phase = Phase::PublicKey;
            return vec![Step::Send(pdu(code::PUBLIC_KEY, &public))];
        }
        self.local_nonce = random(rng);
        let confirm = self.legacy_confirm(&self.local_nonce);
        self.phase = Phase::Confirm;
        vec![Step::Send(pdu(code::CONFIRM, &confirm))]
    }

    fn on_public_key(&mut self, body: &[u8], rng: &mut dyn FnMut(&mut [u8])) -> Vec<Step> {
        let Some(peer) = array::<64>(body) else {
            return self.fail(reason::INVALID_PARAMETERS);
        };
        self.peer_public = peer;
        let mut steps = Vec::new();
        if !self.initiator {
            let pair = new_key_pair(rng);
            steps.push(Step::Send(pdu(code::PUBLIC_KEY, &pair.public)));
            self.key_pair = Some(pair);
        }
        let Some(dh_key) = self.key_pair.as_ref().and_then(|pair| pair.dh_key(&peer)) else {
            return self.fail(reason::DHKEY_CHECK_FAILED);
        };
        self.dh_key = dh_key;
        self.local_nonce = random(rng);
        if self.initiator {
            // Just Works: the responder sends its confirm first.
            self.phase = Phase::Confirm;
        } else {
            let confirm = crypto::f4(&self.x(true), &self.x(false), &self.local_nonce, 0);
            steps.push(Step::Send(pdu(code::CONFIRM, &confirm)));
            self.phase = Phase::Random;
        }
        steps
    }

    fn on_confirm(&mut self, rng: &mut dyn FnMut(&mut [u8])) -> Vec<Step> {
        self.phase = Phase::Random;
        if self.secure {
            // Initiator: send our nonce; the check of the confirm waits for theirs.
            return vec![Step::Send(pdu(code::RANDOM, &self.local_nonce))];
        }
        if self.initiator {
            return vec![Step::Send(pdu(code::RANDOM, &self.local_nonce))];
        }
        self.local_nonce = random(rng);
        let confirm = self.legacy_confirm(&self.local_nonce);
        vec![Step::Send(pdu(code::CONFIRM, &confirm))]
    }

    fn on_random(&mut self) -> Vec<Step> {
        if self.secure {
            let a = Addresses::with_type(self.addresses.initiator);
            let b = Addresses::with_type(self.addresses.responder);
            let (na, nb) = if self.initiator {
                // The responder's confirm: f4(PKbx, PKax, Nb, 0).
                let expected = crypto::f4(&self.x(false), &self.x(true), &self.peer_nonce, 0);
                if expected != self.peer_confirm {
                    return self.fail(reason::CONFIRM_VALUE_FAILED);
                }
                (self.local_nonce, self.peer_nonce)
            } else {
                (self.peer_nonce, self.local_nonce)
            };
            let (mac_key, ltk) = crypto::f5(&self.dh_key, &na, &nb, &a, &b);
            self.mac_key = mac_key;
            self.key = ltk;
            self.keys.ltk = ltk;
            self.phase = Phase::DhKeyCheck;
            if self.initiator {
                let io_a = array(&self.preq[1..]).unwrap_or([0; 3]);
                let ea = crypto::f6(&mac_key, &na, &nb, &[0; 16], &io_a, &a, &b);
                return vec![Step::Send(pdu(code::DHKEY_CHECK, &ea))];
            }
            return vec![Step::Send(pdu(code::RANDOM, &self.local_nonce))];
        }
        // Legacy: the peer's confirm must come from its random.
        if self.legacy_confirm(&self.peer_nonce) != self.peer_confirm {
            return self.fail(reason::CONFIRM_VALUE_FAILED);
        }
        let (srand, mrand) = if self.initiator {
            (self.peer_nonce, self.local_nonce)
        } else {
            (self.local_nonce, self.peer_nonce)
        };
        self.key = crypto::s1(&[0; 16], &srand, &mrand);
        self.phase = Phase::Encryption;
        if self.initiator {
            vec![Step::Encrypt(self.key)]
        } else {
            vec![Step::Send(pdu(code::RANDOM, &self.local_nonce))]
        }
    }

    fn on_dh_key_check(&mut self, body: &[u8]) -> Vec<Step> {
        let Some(check) = array::<16>(body) else {
            return self.fail(reason::INVALID_PARAMETERS);
        };
        let a = Addresses::with_type(self.addresses.initiator);
        let b = Addresses::with_type(self.addresses.responder);
        let io_a = array(&self.preq[1..]).unwrap_or([0; 3]);
        let io_b = array(&self.pres[1..]).unwrap_or([0; 3]);
        let (na, nb) = if self.initiator {
            (self.local_nonce, self.peer_nonce)
        } else {
            (self.peer_nonce, self.local_nonce)
        };
        let expected = if self.initiator {
            crypto::f6(&self.mac_key, &nb, &na, &[0; 16], &io_b, &b, &a)
        } else {
            crypto::f6(&self.mac_key, &na, &nb, &[0; 16], &io_a, &a, &b)
        };
        if expected != check {
            return self.fail(reason::DHKEY_CHECK_FAILED);
        }
        self.phase = Phase::Encryption;
        if self.initiator {
            return vec![Step::Encrypt(self.key)];
        }
        let eb = crypto::f6(&self.mac_key, &nb, &na, &[0; 16], &io_b, &b, &a);
        vec![Step::Send(pdu(code::DHKEY_CHECK, &eb))]
    }

    /// The link is now encrypted with [`Self::link_key`]: the keys go out.
    pub fn on_encrypted(&mut self, rng: &mut dyn FnMut(&mut [u8])) -> Vec<Step> {
        if self.phase != Phase::Encryption {
            return Vec::new();
        }
        let mut steps = Vec::new();
        if self.secure {
            self.keys.ltk = self.key;
        }
        // The responder gives its keys first.
        if !self.initiator && self.give & ENC_KEY != 0 {
            let ltk: Key = random(rng);
            let ediv = u16::from_le_bytes(random(rng));
            let rand: [u8; 8] = random(rng);
            steps.push(Step::Send(pdu(code::ENCRYPTION_INFORMATION, &ltk)));
            let mut identification = ediv.to_le_bytes().to_vec();
            identification.extend_from_slice(&rand);
            steps.push(Step::Send(pdu(
                code::CENTRAL_IDENTIFICATION,
                &identification,
            )));
            self.keys.ltk = ltk;
            self.keys.ediv = ediv;
            self.keys.rand = rand;
        }
        self.phase = Phase::Distribution;
        steps.extend(self.finish_if_complete());
        steps
    }

    fn finish_if_complete(&mut self) -> Option<Step> {
        if self.expected != 0 {
            return None;
        }
        self.phase = Phase::Done;
        Some(Step::Done(self.keys.clone()))
    }

    fn on_key(&mut self, command: u8, body: &[u8]) -> Vec<Step> {
        match command {
            code::ENCRYPTION_INFORMATION if self.expected & ENC_KEY != 0 => {
                // Legacy, as the initiator: the peripheral's LTK; its EDIV and Rand follow.
                let Some(ltk) = array(body) else {
                    return self.fail(reason::INVALID_PARAMETERS);
                };
                self.keys.ltk = ltk;
            }
            code::CENTRAL_IDENTIFICATION if self.expected & ENC_KEY != 0 => {
                let (Some(ediv), Some(rand)) =
                    (array::<2>(body), array::<8>(&body[2.min(body.len())..]))
                else {
                    return self.fail(reason::INVALID_PARAMETERS);
                };
                self.keys.ediv = u16::from_le_bytes(ediv);
                self.keys.rand = rand;
                self.expected &= !ENC_KEY;
            }
            code::IDENTITY_INFORMATION if self.expected & ID_KEY != 0 => {
                let Some(irk) = array(body) else {
                    return self.fail(reason::INVALID_PARAMETERS);
                };
                self.keys.peer_irk = Some(irk);
            }
            code::IDENTITY_ADDRESS if self.expected & ID_KEY != 0 => {
                let (Some(&kind), Some(address)) =
                    (body.first(), array::<6>(&body[1.min(body.len())..]))
                else {
                    return self.fail(reason::INVALID_PARAMETERS);
                };
                self.keys.peer_identity = Some((kind, address));
                self.expected &= !ID_KEY;
            }
            // A signing key, or a key Tetrad did not ask for: not kept.
            code::SIGNING_INFORMATION
            | code::ENCRYPTION_INFORMATION
            | code::CENTRAL_IDENTIFICATION => {}
            code::IDENTITY_INFORMATION | code::IDENTITY_ADDRESS => {}
            _ => return self.fail(reason::UNSPECIFIED),
        }
        self.finish_if_complete().into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic source of "random" bytes for both sides.
    fn counter(seed: u8) -> impl FnMut(&mut [u8]) {
        let mut next = seed;
        move |out: &mut [u8]| {
            for byte in out {
                next = next.wrapping_mul(31).wrapping_add(7);
                *byte = next;
            }
        }
    }

    const ADDRESSES: Addresses = Addresses {
        initiator: (1, [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x5f]),
        responder: (0, [0xeb, 0x1f, 0x5a, 0x05, 0xf0, 0xf8]),
    };

    fn sent(steps: &[Step]) -> Vec<Vec<u8>> {
        (steps.iter())
            .filter_map(|step| match step {
                Step::Send(pdu) => Some(pdu.clone()),
                _ => None,
            })
            .collect()
    }

    /// Runs Tetrad's initiator against Tetrad's responder; both machines must agree on the key.
    fn pair(request_auth: Option<u8>) -> (Keys, Keys) {
        let mut rng_i = counter(3);
        let mut rng_r = counter(200);
        let (mut initiator, mut request) = Pairing::initiate(ADDRESSES);
        if let Some(auth) = request_auth {
            request[3] = auth;
            initiator.preq[3] = auth;
        }
        let (mut responder, response) = Pairing::respond(&request, ADDRESSES).unwrap();
        let mut to_responder = sent(&initiator.on_pdu(&response, &mut rng_i));
        let mut done = (None, None);
        for _ in 0..10 {
            let mut to_initiator = Vec::new();
            for pdu in to_responder.drain(..) {
                to_initiator.extend(sent(&responder.on_pdu(&pdu, &mut rng_r)));
            }
            for pdu in to_initiator {
                let steps = initiator.on_pdu(&pdu, &mut rng_i);
                if steps.iter().any(|s| matches!(s, Step::Encrypt(_))) {
                    // The controllers encrypt: both sides have the same key.
                    assert_eq!(initiator.link_key(), responder.link_key());
                    assert!(initiator.link_key().is_some());
                    let from_responder = responder.on_encrypted(&mut rng_r);
                    for step in initiator.on_encrypted(&mut rng_i) {
                        if let Step::Done(keys) = step {
                            done.0 = Some(keys);
                        }
                    }
                    for step in &from_responder {
                        if let Step::Done(keys) = step {
                            done.1 = Some(keys.clone());
                        }
                    }
                    for pdu in sent(&from_responder) {
                        for step in initiator.on_pdu(&pdu, &mut rng_i) {
                            if let Step::Done(keys) = step {
                                done.0 = Some(keys);
                            }
                        }
                    }
                }
                to_responder.extend(sent(&steps));
            }
            if let (Some(i), Some(r)) = &done {
                return (i.clone(), r.clone());
            }
        }
        panic!("the pairing did not finish");
    }

    #[test]
    fn secure_connections_just_works_gives_both_sides_one_ltk() {
        let (initiator, responder) = pair(None);
        assert!(initiator.secure && responder.secure);
        assert_eq!(initiator.ltk, responder.ltk);
        assert_eq!((initiator.ediv, initiator.rand), (0, [0; 8]));
    }

    #[test]
    fn legacy_just_works_hands_over_the_responders_ltk() {
        let (initiator, responder) = pair(Some(BONDING));
        assert!(!initiator.secure && !responder.secure);
        assert_eq!(initiator.ltk, responder.ltk);
        assert_eq!(initiator.ediv, responder.ediv);
        assert_eq!(initiator.rand, responder.rand);
        assert_ne!(initiator.ltk, [0; 16]);
    }

    #[test]
    fn a_wrong_confirm_or_a_short_key_fails_the_pairing() {
        let (_, mut request) = Pairing::initiate(ADDRESSES);
        request[4] = 7;
        assert_eq!(
            Pairing::respond(&request, ADDRESSES).err(),
            Some(reason::ENCRYPTION_KEY_SIZE)
        );
        // Legacy: a random that does not match the confirm.
        request[4] = 16;
        request[3] = BONDING;
        let (mut responder, _) = Pairing::respond(&request, ADDRESSES).unwrap();
        let mut rng = counter(9);
        let _ = responder.on_pdu(&pdu(code::CONFIRM, &[1; 16]), &mut rng);
        let steps = responder.on_pdu(&pdu(code::RANDOM, &[2; 16]), &mut rng);
        assert_eq!(
            steps,
            vec![
                Step::Send(failed(reason::CONFIRM_VALUE_FAILED)),
                Step::Failed {
                    reason: reason::CONFIRM_VALUE_FAILED
                }
            ]
        );
    }

    #[test]
    fn a_legacy_bond_without_tetrads_ltk_is_no_bond() {
        let (_, mut request) = Pairing::initiate(ADDRESSES);
        // Legacy, bonding, but the central does not ask for Tetrad's LTK.
        request[3] &= !SECURE_CONNECTIONS;
        request[6] = ID_KEY;
        let (responder, response) = Pairing::respond(&request, ADDRESSES).unwrap();
        assert_eq!(response[3] & BONDING, 0);
        assert_eq!(response[5..], [0, 0]);
        assert!(!responder.keys.bond);
    }

    #[test]
    fn the_responder_takes_the_peers_identity() {
        let (_, mut request) = Pairing::initiate(ADDRESSES);
        // A phone offers its identity and its LTK.
        request[5] = ID_KEY | ENC_KEY;
        let (mut responder, response) = Pairing::respond(&request, ADDRESSES).unwrap();
        // LE SC with bonding: Tetrad asks for the central's identity and gives no keys.
        assert_eq!(response[5..], [ID_KEY, 0]);
        responder.phase = Phase::Encryption;
        let mut rng = counter(1);
        assert!(sent(&responder.on_encrypted(&mut rng)).is_empty());
        assert!(
            responder
                .on_pdu(&pdu(code::IDENTITY_INFORMATION, &[9; 16]), &mut rng)
                .is_empty()
        );
        let mut address = vec![0];
        address.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        let steps = responder.on_pdu(&pdu(code::IDENTITY_ADDRESS, &address), &mut rng);
        let Some(Step::Done(keys)) = steps.first() else {
            panic!("{steps:?}");
        };
        assert_eq!(keys.peer_irk, Some([9; 16]));
        assert_eq!(keys.peer_identity, Some((0, [1, 2, 3, 4, 5, 6])));
    }
}
