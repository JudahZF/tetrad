//! The host against a scripted controller: set-up, a phone that pairs and plays BLE MIDI, and
//! the same phone again with its bond. The phone's side of the pairing is Tetrad's own SMP
//! initiator, so both ends of the key agreement run.

use std::time::Duration;

use tetrad_ble::att::{self, Database, Slot};
use tetrad_ble::smp::{self, Addresses, Pairing, Step};
use tetrad_ble::{Bond, Config, Event, Host, hci, l2cap};

const ADDRESS: [u8; 6] = [0xeb, 0x1f, 0x5a, 0x05, 0xf0, 0xf8];
const PHONE: [u8; 6] = [0x2b, 0xa0, 0xfa, 0x1b, 0x36, 0xac];
const HANDLE: u16 = 0x0040;

#[derive(Debug, PartialEq)]
enum Out {
    Command(u16, Vec<u8>),
    /// One ACL fragment: first, data.
    Acl(bool, Vec<u8>),
}

fn parse_output(bytes: &[u8]) -> Vec<Out> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            0x01 => {
                let opcode = u16::from_le_bytes([bytes[at + 1], bytes[at + 2]]);
                let length = usize::from(bytes[at + 3]);
                out.push(Out::Command(
                    opcode,
                    bytes[at + 4..at + 4 + length].to_vec(),
                ));
                at += 4 + length;
            }
            0x02 => {
                let header = u16::from_le_bytes([bytes[at + 1], bytes[at + 2]]);
                let length = usize::from(u16::from_le_bytes([bytes[at + 3], bytes[at + 4]]));
                out.push(Out::Acl(
                    header & 0x3000 == 0,
                    bytes[at + 5..at + 5 + length].to_vec(),
                ));
                at += 5 + length;
            }
            other => panic!("not H4: {other:#x}"),
        }
    }
    out
}

fn complete(opcode: u16, parameters: &[u8]) -> Vec<u8> {
    let mut event = vec![0x04, 0x0e, (3 + parameters.len()) as u8, 1];
    event.extend_from_slice(&opcode.to_le_bytes());
    event.extend_from_slice(parameters);
    event
}

fn le_event(sub: u8, parameters: &[u8]) -> Vec<u8> {
    let mut event = vec![0x04, 0x3e, (1 + parameters.len()) as u8, sub];
    event.extend_from_slice(parameters);
    event
}

/// One L2CAP frame from the phone, in 27-byte fragments.
fn acl_in(channel: u16, payload: &[u8]) -> Vec<u8> {
    let frame = l2cap::frame(channel, payload);
    let mut out = Vec::new();
    for (i, fragment) in frame.chunks(27).enumerate() {
        let flags: u16 = if i == 0 { 0x2000 } else { 0x1000 };
        out.push(0x02);
        out.extend_from_slice(&(HANDLE | flags).to_le_bytes());
        out.extend_from_slice(&(fragment.len() as u16).to_le_bytes());
        out.extend_from_slice(fragment);
    }
    out
}

struct Rig {
    host: Host,
    now: Duration,
    /// Commands the host sent and the test has not answered.
    commands: Vec<(u16, Vec<u8>)>,
    /// L2CAP frames the host sent: channel, payload.
    frames: Vec<(u16, Vec<u8>)>,
    events: Vec<Event>,
    rx: l2cap::Reassembler,
}

impl Rig {
    fn new(pairable: bool, bonds: Vec<Bond>) -> Self {
        Self::with_link(pairable, bonds, false)
    }

    fn with_link(pairable: bool, bonds: Vec<Bond>, link: bool) -> Self {
        let mut next = 1u8;
        let rng = Box::new(move |out: &mut [u8]| {
            for byte in out {
                next = next.wrapping_mul(13).wrapping_add(11);
                *byte = next;
            }
        });
        let config = Config {
            name: None,
            advertise: true,
            pairable,
            link,
        };
        let mut rig = Self {
            host: Host::new(config, bonds, rng),
            now: Duration::ZERO,
            commands: Vec::new(),
            frames: Vec::new(),
            events: Vec::new(),
            rx: l2cap::Reassembler::default(),
        };
        rig.collect();
        rig
    }

    fn collect(&mut self) {
        for out in parse_output(&self.host.take_output()) {
            match out {
                Out::Command(opcode, parameters) => self.commands.push((opcode, parameters)),
                Out::Acl(first, data) => {
                    if let Some(frame) = self.rx.push(first, &data).unwrap() {
                        self.frames.push(frame);
                    }
                }
            }
        }
        while let Some(event) = self.host.next_event() {
            self.events.push(event);
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.host.receive(bytes, self.now);
        self.collect();
    }

    /// Answers each command in order until none waits; returns their opcodes.
    fn answer_commands(&mut self) -> Vec<u16> {
        let mut answered = Vec::new();
        while !self.commands.is_empty() {
            let (opcode, _) = self.commands.remove(0);
            let parameters = match opcode {
                hci::op::LE_READ_BUFFER_SIZE => vec![0, 27, 0, 30],
                hci::op::READ_BD_ADDR => [&[0][..], &ADDRESS].concat(),
                _ => vec![0],
            };
            answered.push(opcode);
            self.feed(&complete(opcode, &parameters));
        }
        answered
    }

    fn take_frames(&mut self) -> Vec<(u16, Vec<u8>)> {
        std::mem::take(&mut self.frames)
    }

    fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    fn connect(&mut self) {
        let mut parameters = vec![0];
        parameters.extend_from_slice(&HANDLE.to_le_bytes());
        // Peripheral role, public peer address, 45 ms interval.
        parameters.extend_from_slice(&[1, 0]);
        parameters.extend_from_slice(&PHONE);
        parameters.extend_from_slice(&[0x24, 0, 0, 0, 0x2a, 0, 0]);
        self.feed(&le_event(0x01, &parameters));
    }

    fn encrypted(&mut self) {
        let mut event = vec![0x04, 0x08, 4, 0];
        event.extend_from_slice(&HANDLE.to_le_bytes());
        event.push(1);
        self.feed(&event);
    }

    fn ltk_request(&mut self, ediv: u16) -> Vec<u8> {
        let mut parameters = HANDLE.to_le_bytes().to_vec();
        parameters.extend_from_slice(&[0; 8]);
        parameters.extend_from_slice(&ediv.to_le_bytes());
        self.feed(&le_event(0x05, &parameters));
        let (opcode, reply) = self.commands.remove(0);
        assert_eq!(opcode, hci::op::LE_LTK_REPLY);
        self.feed(&complete(opcode, &[0]));
        reply[2..].to_vec()
    }
}

fn midi_handle() -> u16 {
    Database::tetrad(false).handle(Slot::Midi).unwrap()
}

fn cccd(value: u16) -> Vec<u8> {
    let mut pdu = vec![att::op::WRITE];
    pdu.extend_from_slice(&(midi_handle() + 1).to_le_bytes());
    pdu.extend_from_slice(&value.to_le_bytes());
    pdu
}

#[test]
fn set_up_names_the_unit_and_advertises() {
    let mut rig = Rig::new(true, Vec::new());
    let answered = rig.answer_commands();
    assert_eq!(answered[0], hci::op::RESET);
    assert_eq!(answered.last(), Some(&hci::op::LE_SET_ADVERTISE_ENABLE));
    assert_eq!(
        rig.take_events(),
        vec![Event::Ready {
            address: ADDRESS,
            name: "Quad Cortex 1FEB".into()
        }]
    );
}

#[test]
fn a_phone_pairs_plays_midi_and_comes_back_with_its_bond() {
    let mut rig = Rig::new(true, Vec::new());
    rig.answer_commands();
    rig.take_events();
    rig.connect();
    assert_eq!(
        rig.take_events(),
        vec![Event::HostConnected {
            address: PHONE,
            bonded: false
        }]
    );

    // MIDI before pairing: the phone must authenticate.
    rig.feed(&acl_in(l2cap::ATT, &cccd(1)));
    let frames = rig.take_frames();
    assert_eq!(
        frames,
        vec![(
            l2cap::ATT,
            att::error_response(
                att::op::WRITE,
                midi_handle() + 1,
                att::error::INSUFFICIENT_AUTHENTICATION
            )
        )]
    );

    // The phone pairs, with Tetrad's own initiator.
    let addresses = Addresses {
        initiator: (0, PHONE),
        responder: (0, ADDRESS),
    };
    let (mut phone, mut request) = Pairing::initiate(addresses);
    // A phone gives its identity.
    request[5] = 0x02;
    let mut phone_rng = |out: &mut [u8]| out.fill(0x5a);
    rig.feed(&acl_in(l2cap::SMP, &request));
    let mut phone_key = None;
    for _ in 0..8 {
        let frames = rig.take_frames();
        if frames.is_empty() {
            break;
        }
        for (channel, pdu) in frames {
            assert_eq!(channel, l2cap::SMP);
            for step in phone.on_pdu(&pdu, &mut phone_rng) {
                match step {
                    Step::Send(reply) => rig.feed(&acl_in(l2cap::SMP, &reply)),
                    Step::Encrypt(key) => phone_key = Some(key),
                    other => panic!("{other:?}"),
                }
            }
        }
    }
    let phone_key = phone_key.expect("the phone encrypts");
    assert_eq!(rig.ltk_request(0), phone_key);
    rig.encrypted();
    // The phone gives its identity; the host makes the bond.
    rig.feed(&acl_in(l2cap::SMP, &[smp::code::IDENTITY_INFORMATION; 17]));
    let mut identity = vec![smp::code::IDENTITY_ADDRESS, 0];
    identity.extend_from_slice(&PHONE);
    rig.feed(&acl_in(l2cap::SMP, &identity));
    let events = rig.take_events();
    let Some(Event::Bonded(bond)) = events.first() else {
        panic!("{events:?}");
    };
    assert_eq!(bond.address, PHONE);
    assert_eq!(bond.ltk, phone_key);
    assert!(bond.secure);

    // The host reads the phone's name for the device list.
    let frames = rig.take_frames();
    assert_eq!(frames[0].1[..1], [att::op::READ_BY_TYPE]);
    let mut name = vec![att::op::READ_BY_TYPE_RESPONSE, 2 + 6, 3, 0];
    name.extend_from_slice(b"iPhone");
    rig.feed(&acl_in(l2cap::ATT, &name));
    let events = rig.take_events();
    let [Event::Bonded(bond)] = &events[..] else {
        panic!("{events:?}");
    };
    assert_eq!(bond.name.as_deref(), Some("iPhone"));

    // Now encrypted: notifications on, MIDI both ways.
    rig.feed(&acl_in(l2cap::ATT, &cccd(1)));
    assert_eq!(
        rig.take_frames(),
        vec![(l2cap::ATT, vec![att::op::WRITE_RESPONSE])]
    );
    let events = rig.take_events();
    assert!(events.contains(&Event::MidiListening(true)), "{events:?}");
    // The bond keeps the switch for the next link.
    let bond = (events.iter())
        .find_map(|event| match event {
            Event::Bonded(bond) => Some(bond.clone()),
            _ => None,
        })
        .expect("the bond keeps the notification switch");
    assert_eq!(bond.notify, 1 << att::Notify::Midi as u8);
    let mut write = vec![att::op::WRITE_COMMAND];
    write.extend_from_slice(&midi_handle().to_le_bytes());
    write.extend_from_slice(&[0x80, 0x80, 0xb0, 7, 100, 8, 64]);
    rig.feed(&acl_in(l2cap::ATT, &write));
    assert_eq!(
        rig.take_events(),
        vec![
            Event::Midi(vec![0xb0, 7, 100]),
            Event::Midi(vec![0xb0, 8, 64])
        ]
    );
    rig.host.send_midi(&[0xc0, 5]);
    rig.collect();
    let frames = rig.take_frames();
    assert_eq!(frames.len(), 1);
    assert_eq!(
        frames[0].1[..3],
        [att::op::NOTIFICATION, midi_handle() as u8, 0]
    );
    assert_eq!(frames[0].1[5..], [0xc0, 5]);

    // The phone leaves and comes back: its key encrypts the link, its switches stay on.
    let mut rig = Rig::new(false, vec![bond]);
    rig.answer_commands();
    rig.take_events();
    rig.connect();
    assert_eq!(
        rig.take_events(),
        vec![Event::HostConnected {
            address: PHONE,
            bonded: true
        }]
    );
    assert_eq!(rig.ltk_request(0), phone_key);
    rig.encrypted();
    assert_eq!(
        rig.take_events(),
        vec![
            Event::MidiListening(true),
            Event::HostSecured {
                address: PHONE,
                paired: false,
                connection: 0,
            }
        ]
    );
}

#[test]
fn a_new_phone_cannot_pair_while_tetrad_is_not_pairable() {
    let mut rig = Rig::new(false, Vec::new());
    rig.answer_commands();
    rig.connect();
    let addresses = Addresses {
        initiator: (0, PHONE),
        responder: (0, ADDRESS),
    };
    let (_, request) = Pairing::initiate(addresses);
    rig.feed(&acl_in(l2cap::SMP, &request));
    assert_eq!(
        rig.take_frames(),
        vec![(l2cap::SMP, smp::failed(smp::reason::PAIRING_NOT_SUPPORTED))]
    );
    assert!(rig.take_events().contains(&Event::PairingFailed {
        reason: smp::reason::PAIRING_NOT_SUPPORTED,
        refused: true
    }));
    // The unknown host does not keep the connection.
    assert!(
        rig.commands
            .iter()
            .any(|(opcode, _)| *opcode == hci::op::DISCONNECT),
        "{:?}",
        rig.commands
    );
}

#[test]
fn a_silent_or_broken_controller_is_a_fault() {
    let mut rig = Rig::new(true, Vec::new());
    rig.now = Duration::from_secs(4);
    rig.host.tick(rig.now);
    rig.collect();
    assert!(matches!(&rig.take_events()[..], [Event::Fault(_)]));

    let mut rig = Rig::new(true, Vec::new());
    rig.feed(&[0x04, 0x10, 0x01, 0x00]);
    assert_eq!(
        rig.take_events(),
        vec![Event::Fault(
            "the controller reported hardware error 0x00".into()
        )]
    );
}

#[test]
fn midi_waits_until_a_link_reply_packet_is_whole() {
    let phone = Bond {
        address: PHONE,
        address_type: 0,
        kind: tetrad_ble::bond::Kind::Host,
        irk: None,
        ltk: [1; 16],
        ediv: 0,
        rand: [0; 8],
        secure: true,
        name: Some("Phone".into()),
        notify: 0,
        digest: Database::tetrad(true).digest(),
        app: true,
    };
    let mut rig = Rig::with_link(false, vec![phone], true);
    rig.answer_commands();
    rig.connect();
    assert_eq!(rig.ltk_request(0), [1; 16]);
    rig.encrypted();
    rig.feed(&acl_in(l2cap::ATT, &[att::op::MTU_REQUEST, 247, 0]));
    let db = Database::tetrad(true);
    for notify in [att::Notify::Link, att::Notify::Midi] {
        let cccd = db.handle(Slot::Cccd(notify)).unwrap();
        let mut write = vec![att::op::WRITE];
        write.extend_from_slice(&cccd.to_le_bytes());
        write.extend_from_slice(&[1, 0]);
        rig.feed(&acl_in(l2cap::ATT, &write));
    }
    rig.take_frames();
    // A reply for another connection (one that ended) never reaches this host.
    rig.host.apply(tetrad_ble::Command::LinkSend {
        connection: 1,
        message: vec![1, 2, 3],
    });
    rig.collect();
    assert!(rig.take_frames().is_empty());
    // A reply of many 27-byte fragments, then MIDI while the controller's 30 buffers are full.
    let message: Vec<u8> = (0..2000).map(|i| i as u8).collect();
    rig.host.apply(tetrad_ble::Command::LinkSend {
        connection: 0,
        message: message.clone(),
    });
    rig.host.send_midi(&[0xb0, 1, 2]);
    rig.collect();
    for _ in 0..20 {
        let mut done = vec![0x04, 0x13, 5, 1];
        done.extend_from_slice(&HANDLE.to_le_bytes());
        done.extend_from_slice(&30u16.to_le_bytes());
        rig.feed(&done);
    }
    // Every L2CAP packet arrived whole (the reassembler would fail on a cut one).
    let reply = db.handle(Slot::LinkReply).unwrap();
    let midi = db.handle(Slot::Midi).unwrap();
    let mut joined = tetrad_ble::link::Reassembler::default();
    let mut replies = Vec::new();
    let mut midi_seen = false;
    for (_, pdu) in rig.take_frames() {
        let handle = u16::from_le_bytes([pdu[1], pdu[2]]);
        if handle == reply {
            replies.extend(joined.push(&pdu[3..]).unwrap());
        } else if handle == midi {
            midi_seen = true;
        }
    }
    assert_eq!(replies, vec![message]);
    assert!(midi_seen);
}
