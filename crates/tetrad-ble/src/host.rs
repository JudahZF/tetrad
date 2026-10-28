//! The host: one state machine over the controller. The platform feeds it the bytes it reads
//! ([`Host::receive`]) and the time ([`Host::tick`]), writes what [`Host::take_output`] returns,
//! and hands each [`Event`] to the device.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use crate::att::{self, Bearer, Database, Notify, Slot};
use crate::bond::{Bond, Kind};
use crate::hci::{self, Address, Event as HciEvent, H4Reader, Packet, Role, op, status};
use crate::l2cap::{self, Reassembler, Signal};
use crate::smp::{self, Addresses, Pairing, Step};

/// How long a command may wait for its answer before the controller counts as gone.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
/// SMP gives a pairing 30 s (Vol 3 Part H §3.4).
const PAIRING_TIMEOUT: Duration = Duration::from_secs(30);
/// Soon after a host connects, Tetrad asks for its connection parameters: an interval of 15 to
/// 30 ms for BLE MIDI, and a supervision timeout of 6 s. The WILC3000 shares its radio with
/// Wi-Fi; with BlueZ's default timeout (420 ms) links dropped on a unit with a weak Wi-Fi signal
/// (2026-10-28).
const PARAMETER_DELAY: Duration = Duration::from_millis(300);
const MIDI_PARAMETERS: l2cap::Parameters = l2cap::Parameters {
    min_interval: 12,
    max_interval: 24,
    latency: 0,
    timeout: 600,
};
/// Fragments that may wait for controller buffers; past this, MIDI notifications drop.
const MAX_QUEUED_FRAGMENTS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The GAP name; `None`: "Quad Cortex" and the last four hexadecimal digits of the address.
    pub name: Option<String>,
    /// Advertise, so that a phone or computer can connect (Host mode).
    pub advertise: bool,
    /// New devices may pair.
    pub pairable: bool,
    /// Offer the Tetrad Link service of the app.
    pub link: bool,
}

/// What the device asks of the host ([`Host::apply`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Host mode: advertise while no host is connected. Off also ends a host's link.
    SetAdvertise(bool),
    /// New devices may pair.
    SetPairable(bool),
    /// One MIDI message, status first, for every host that listens.
    SendMidi(Vec<u8>),
    /// Drop the bond with this address, and end its link.
    Forget(Address),
}

/// What the host tells the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The controller is set up.
    Ready {
        address: Address,
        name: String,
    },
    /// The controller stopped answering or broke the stream. The host is dead: start the
    /// controller again and make a new host.
    Fault(String),
    /// A phone or computer connected. `bonded`: Tetrad knows it.
    HostConnected {
        address: Address,
        bonded: bool,
    },
    HostDisconnected {
        address: Address,
        reason: u8,
    },
    /// A pairing made a bond, or a bond changed (a name, the notification switches): save it in
    /// place of the bond with the same address.
    Bonded(Bond),
    /// A pairing ended without a bond. `refused`: Tetrad refused it, because it is not
    /// pairable.
    PairingFailed {
        reason: u8,
        refused: bool,
    },
    /// One MIDI message from a host, status first.
    Midi(Vec<u8>),
    /// Whether a host listens for MIDI notifications on an encrypted link.
    MidiListening(bool),
    /// Something for the log.
    Notice(String),
}

struct Link {
    role: Role,
    peer: (u8, Address),
    /// The bonded identity of the peer, when Tetrad knows it.
    identity: Option<Address>,
    encrypted: bool,
    rx: Reassembler,
    bearer: Bearer,
    /// CCCD bits ([`Notify`]).
    notify: u8,
    pairing: Option<(Pairing, Duration)>,
    midi: crate::midi::Decoder,
    /// When to ask for MIDI connection parameters.
    parameters_at: Option<Duration>,
    /// A GAP name read is out (one ATT request at a time on Tetrad's client side).
    asking_name: bool,
    listening: bool,
}

impl Link {
    fn listens(&self, notify: Notify) -> bool {
        self.encrypted && self.notify & (1 << notify as u8) != 0
    }
}

/// Fills a buffer with random bytes (keys and nonces).
pub type Rng = Box<dyn FnMut(&mut [u8]) + Send>;

pub struct Host {
    config: Config,
    db: Database,
    digest: u32,
    rng: Rng,
    reader: H4Reader,
    output: Vec<u8>,
    events: VecDeque<Event>,
    commands: VecDeque<Vec<u8>>,
    /// The command whose answer Tetrad waits for, and since when.
    awaiting: Option<(u16, Duration)>,
    address: Address,
    name: String,
    ready: bool,
    dead: bool,
    /// The advertising state Tetrad last asked for.
    advertising: bool,
    acl_size: usize,
    acl_free: u16,
    acl_total: u16,
    in_flight: BTreeMap<u16, u16>,
    acl: VecDeque<(u16, Vec<u8>)>,
    links: BTreeMap<u16, Link>,
    bonds: Vec<Bond>,
    now: Duration,
    signal_id: u8,
    dropped_midi: u64,
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

impl Host {
    /// A host for a controller that runs its firmware. `bonds` are the saved bonds; `rng` fills
    /// buffers with random bytes for keys and nonces.
    pub fn new(config: Config, bonds: Vec<Bond>, rng: Rng) -> Self {
        let db = Database::tetrad(config.link);
        let digest = db.digest();
        let mut host = Self {
            config,
            db,
            digest,
            rng,
            reader: H4Reader::default(),
            output: Vec::new(),
            events: VecDeque::new(),
            commands: VecDeque::new(),
            awaiting: None,
            address: [0; 6],
            name: String::new(),
            ready: false,
            dead: false,
            advertising: false,
            acl_size: 27,
            acl_free: 0,
            acl_total: 0,
            in_flight: BTreeMap::new(),
            acl: VecDeque::new(),
            links: BTreeMap::new(),
            bonds,
            now: Duration::ZERO,
            signal_id: 0,
            dropped_midi: 0,
        };
        // Disconnection, encryption change, hardware error, data buffer overflow, key refresh
        // and LE meta events.
        let mask: u64 = (1 << 4) | (1 << 7) | (1 << 15) | (1 << 25) | (1 << 47) | (1 << 61);
        host.command(op::RESET, &[]);
        host.command(op::SET_EVENT_MASK, &mask.to_le_bytes());
        // Connection complete, advertising report, connection update, remote features, LTK
        // request.
        host.command(op::LE_SET_EVENT_MASK, &[0x1f, 0, 0, 0, 0, 0, 0, 0]);
        host.command(op::LE_READ_BUFFER_SIZE, &[]);
        host.command(op::READ_BD_ADDR, &[]);
        host
    }

    /// Bytes to write to the controller.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output)
    }

    pub fn next_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// The next time [`Self::tick`] has work.
    pub fn deadline(&self) -> Option<Duration> {
        let command = self.awaiting.map(|(_, at)| at + COMMAND_TIMEOUT);
        let links = self.links.values().flat_map(|link| {
            let pairing = link.pairing.as_ref().map(|(_, at)| *at + PAIRING_TIMEOUT);
            [pairing, link.parameters_at]
        });
        command.into_iter().chain(links.flatten()).min()
    }

    pub fn tick(&mut self, now: Duration) {
        self.now = now;
        if self.dead {
            return;
        }
        if let Some((opcode, at)) = self.awaiting
            && now >= at + COMMAND_TIMEOUT
        {
            self.fault(format!("no answer to HCI command 0x{opcode:04x}"));
            return;
        }
        let handles: Vec<u16> = self.links.keys().copied().collect();
        for handle in handles {
            let Some(link) = self.links.get_mut(&handle) else {
                continue;
            };
            if let Some((_, at)) = &link.pairing
                && now >= *at + PAIRING_TIMEOUT
            {
                link.pairing = None;
                self.events.push_back(Event::PairingFailed {
                    reason: smp::reason::UNSPECIFIED,
                    refused: false,
                });
                self.disconnect(handle, status::AUTHENTICATION_FAILURE);
                continue;
            }
            if link.parameters_at.is_some_and(|at| now >= at) {
                link.parameters_at = None;
                self.signal_id = self.signal_id.wrapping_add(1).max(1);
                let request = l2cap::parameter_update_request(self.signal_id, MIDI_PARAMETERS);
                self.send(handle, l2cap::SIGNAL, &request);
            }
        }
    }

    /// Bytes from the controller.
    pub fn receive(&mut self, bytes: &[u8], now: Duration) {
        self.now = now;
        if self.dead {
            return;
        }
        let packets = match self.reader.push(bytes) {
            Ok(packets) => packets,
            Err(error) => {
                self.fault(error.to_string());
                return;
            }
        };
        for packet in packets {
            match packet {
                Packet::Event(event) => match HciEvent::parse(&event) {
                    Some(event) => self.on_event(event),
                    None => self.notice(format!("a short HCI event: {event:02x?}")),
                },
                Packet::Acl(data) => self.on_acl(&data),
            }
            if self.dead {
                return;
            }
        }
    }

    // ---- the device's requests ----

    pub fn apply(&mut self, command: Command) {
        match command {
            Command::SetAdvertise(on) => self.set_advertise(on),
            Command::SetPairable(on) => self.set_pairable(on),
            Command::SendMidi(message) => self.send_midi(&message),
            Command::Forget(address) => self.forget(&address),
        }
    }

    /// Host mode: advertise while no host is connected. Off also ends a host's link.
    pub fn set_advertise(&mut self, advertise: bool) {
        self.config.advertise = advertise;
        if !advertise {
            let hosts: Vec<u16> = (self.links.iter())
                .filter(|(_, link)| link.role == Role::Peripheral)
                .map(|(handle, _)| *handle)
                .collect();
            for handle in hosts {
                self.disconnect(handle, status::REMOTE_USER_TERMINATED);
            }
        }
        self.refresh_advertising();
    }

    pub fn set_pairable(&mut self, pairable: bool) {
        self.config.pairable = pairable;
    }

    /// Drops the bond with `address`, and ends its link.
    pub fn forget(&mut self, address: &Address) {
        self.bonds.retain(|bond| bond.address != *address);
        let handles: Vec<u16> = (self.links.iter())
            .filter(|(_, link)| link.identity == Some(*address) || link.peer.1 == *address)
            .map(|(handle, _)| *handle)
            .collect();
        for handle in handles {
            self.disconnect(handle, status::REMOTE_USER_TERMINATED);
        }
    }

    /// Sends one MIDI message (status first) to every host that listens.
    pub fn send_midi(&mut self, message: &[u8]) {
        let Some(handle) = self.db.handle(Slot::Midi) else {
            return;
        };
        let millis = self.now.as_millis() as u64;
        let targets: Vec<(u16, Bearer)> = (self.links.iter())
            .filter(|(_, link)| link.listens(Notify::Midi))
            .map(|(h, link)| (*h, link.bearer.clone()))
            .collect();
        for (link, bearer) in targets {
            if self.acl.len() > MAX_QUEUED_FRAGMENTS {
                self.dropped_midi += 1;
                if self.dropped_midi.is_power_of_two() {
                    let dropped = self.dropped_midi;
                    self.notice(format!(
                        "BLE MIDI out is behind: {dropped} messages dropped"
                    ));
                }
                continue;
            }
            for packet in crate::midi::encode(message, millis, usize::from(bearer.mtu) - 3) {
                let pdu = att::notification(&bearer, handle, &packet);
                self.send(link, l2cap::ATT, &pdu);
            }
        }
    }

    // ---- HCI ----

    fn command(&mut self, opcode: u16, parameters: &[u8]) {
        self.commands.push_back(hci::command(opcode, parameters));
        self.next_command();
    }

    fn next_command(&mut self) {
        if self.awaiting.is_some() {
            return;
        }
        if let Some(command) = self.commands.pop_front() {
            let opcode = u16::from_le_bytes([command[1], command[2]]);
            self.awaiting = Some((opcode, self.now));
            self.output.extend_from_slice(&command);
        }
    }

    fn fault(&mut self, reason: String) {
        self.dead = true;
        self.events.push_back(Event::Fault(reason));
    }

    fn notice(&mut self, text: String) {
        self.events.push_back(Event::Notice(text));
    }

    fn on_event(&mut self, event: HciEvent) {
        match event {
            HciEvent::CommandComplete {
                opcode, parameters, ..
            } => self.on_command_done(
                opcode,
                parameters.first().copied().unwrap_or(0),
                &parameters,
            ),
            HciEvent::CommandStatus { status, opcode, .. } => {
                self.on_command_done(opcode, status, &[status])
            }
            HciEvent::HardwareError(code) => self.fault(format!(
                "the controller reported hardware error 0x{code:02x}"
            )),
            HciEvent::CompletedPackets(pairs) => {
                for (handle, count) in pairs {
                    let sent = self.in_flight.entry(handle).or_default();
                    let count = count.min(*sent);
                    *sent -= count;
                    self.acl_free = (self.acl_free + count).min(self.acl_total);
                }
                self.pump();
            }
            HciEvent::Connected {
                status,
                handle,
                role,
                peer_type,
                peer,
                ..
            } => self.on_connected(status, handle, role, peer_type, peer),
            HciEvent::Disconnected { handle, reason } => self.on_disconnected(handle, reason),
            HciEvent::LtkRequest { handle, rand, ediv } => self.on_ltk_request(handle, rand, ediv),
            HciEvent::EncryptionChange {
                status,
                handle,
                enabled,
            } => self.on_encryption(handle, status == status::SUCCESS && enabled, status),
            HciEvent::KeyRefreshed { status, handle } => {
                self.on_encryption(handle, status == status::SUCCESS, status)
            }
            HciEvent::Advertising(_) | HciEvent::ConnectionUpdated { .. } | HciEvent::Other(_) => {}
        }
    }

    fn on_command_done(&mut self, opcode: u16, result: u8, parameters: &[u8]) {
        if self.awaiting.is_some_and(|(waiting, _)| waiting == opcode) {
            self.awaiting = None;
        }
        match opcode {
            op::LE_READ_BUFFER_SIZE if result == status::SUCCESS && parameters.len() >= 4 => {
                let size = u16::from_le_bytes([parameters[1], parameters[2]]);
                self.acl_size = usize::from(size).max(27);
                self.acl_total = u16::from(parameters[3]).max(1);
                self.acl_free = self.acl_total;
            }
            op::READ_BD_ADDR if result == status::SUCCESS && parameters.len() >= 7 => {
                self.address.copy_from_slice(&parameters[1..7]);
                self.name = (self.config.name.clone()).unwrap_or_else(|| {
                    format!("Quad Cortex {:02X}{:02X}", self.address[1], self.address[0])
                });
                self.setup_advertising();
            }
            op::LE_SET_SCAN_RESPONSE_DATA if !self.ready => {
                self.ready = true;
                let (address, name) = (self.address, self.name.clone());
                self.events.push_back(Event::Ready { address, name });
                self.refresh_advertising();
            }
            // Enabling advertising that already runs is refused; the state is as asked.
            op::LE_SET_ADVERTISE_ENABLE if result == status::COMMAND_DISALLOWED => {}
            _ if result != status::SUCCESS => {
                self.notice(format!("HCI command 0x{opcode:04x} failed: 0x{result:02x}"));
                if !self.ready {
                    self.fault(format!(
                        "set-up command 0x{opcode:04x} failed: 0x{result:02x}"
                    ));
                    return;
                }
            }
            _ => {}
        }
        self.next_command();
    }

    fn setup_advertising(&mut self) {
        let mut parameters = Vec::new();
        put_u16(&mut parameters, 0x00a0);
        put_u16(&mut parameters, 0x00f0);
        // Connectable undirected, public address, all three channels.
        parameters.extend_from_slice(&[0x00, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0x07, 0x00]);
        self.command(op::LE_SET_ADVERTISING_PARAMETERS, &parameters);
        // Flags (LE General Discoverable, BR/EDR not supported) and the BLE MIDI service, so
        // that MIDI apps list the unit.
        let mut data = vec![2, 0x01, 0x06, 17, 0x07];
        data.extend_from_slice(&crate::midi::SERVICE.to_bytes());
        self.command(op::LE_SET_ADVERTISING_DATA, &padded(&data));
        let name = self.name.as_bytes();
        let name = &name[..name.len().min(29)];
        let mut response = vec![name.len() as u8 + 1, 0x09];
        response.extend_from_slice(name);
        self.command(op::LE_SET_SCAN_RESPONSE_DATA, &padded(&response));
    }

    fn refresh_advertising(&mut self) {
        let host_linked = self
            .links
            .values()
            .any(|link| link.role == Role::Peripheral);
        let wanted = self.ready && self.config.advertise && !host_linked;
        if wanted != self.advertising {
            self.advertising = wanted;
            self.command(op::LE_SET_ADVERTISE_ENABLE, &[u8::from(wanted)]);
        }
    }

    fn disconnect(&mut self, handle: u16, reason: u8) {
        let mut parameters = Vec::new();
        put_u16(&mut parameters, handle);
        parameters.push(reason);
        self.command(op::DISCONNECT, &parameters);
    }

    // ---- links ----

    fn bond_of(&self, link: &Link) -> Option<&Bond> {
        let identity = link.identity?;
        self.bonds.iter().find(|bond| bond.address == identity)
    }

    /// The bond whose address or IRK matches the peer address.
    fn resolve(&self, peer_type: u8, peer: &Address) -> Option<Address> {
        (self.bonds.iter())
            .find(|bond| {
                (bond.address_type == peer_type && bond.address == *peer)
                    || (peer_type == 1
                        && bond
                            .irk
                            .is_some_and(|irk| crate::crypto::resolves(&irk, peer)))
            })
            .map(|bond| bond.address)
    }

    fn on_connected(&mut self, result: u8, handle: u16, role: Role, peer_type: u8, peer: Address) {
        if result != status::SUCCESS {
            self.notice(format!("a connection failed: 0x{result:02x}"));
            return;
        }
        if role == Role::Peripheral {
            // The controller stops advertising when a central connects.
            self.advertising = false;
        }
        let identity = self.resolve(peer_type, &peer);
        let notify = identity
            .and_then(|address| self.bonds.iter().find(|bond| bond.address == address))
            .map_or(0, |bond| bond.notify);
        self.links.insert(
            handle,
            Link {
                role,
                peer: (peer_type, peer),
                identity,
                encrypted: false,
                rx: Reassembler::default(),
                bearer: Bearer::default(),
                notify,
                pairing: None,
                midi: crate::midi::Decoder::default(),
                parameters_at: (role == Role::Peripheral).then_some(self.now + PARAMETER_DELAY),
                asking_name: false,
                listening: false,
            },
        );
        self.in_flight.insert(handle, 0);
        if role == Role::Peripheral {
            self.events.push_back(Event::HostConnected {
                address: identity.unwrap_or(peer),
                bonded: identity.is_some(),
            });
        }
    }

    fn on_disconnected(&mut self, handle: u16, reason: u8) {
        // The controller drops the data it had for the link: its buffers are free again.
        let sent = self.in_flight.remove(&handle).unwrap_or(0);
        self.acl_free = (self.acl_free + sent).min(self.acl_total);
        self.acl.retain(|(h, _)| *h != handle);
        if let Some(link) = self.links.remove(&handle) {
            if link.pairing.is_some() {
                self.events.push_back(Event::PairingFailed {
                    reason: smp::reason::UNSPECIFIED,
                    refused: false,
                });
            }
            if link.listening {
                self.events.push_back(Event::MidiListening(false));
            }
            if link.role == Role::Peripheral {
                self.events.push_back(Event::HostDisconnected {
                    address: link.identity.unwrap_or(link.peer.1),
                    reason,
                });
            }
        }
        self.refresh_advertising();
        self.pump();
    }

    fn on_ltk_request(&mut self, handle: u16, rand: [u8; 8], ediv: u16) {
        let key = self.links.get(&handle).and_then(|link| {
            if let Some((pairing, _)) = &link.pairing {
                return pairing.link_key();
            }
            let bond = self.bond_of(link)?;
            let matches = if bond.secure {
                ediv == 0 && rand == [0; 8]
            } else {
                ediv == bond.ediv && rand == bond.rand
            };
            matches.then_some(bond.ltk)
        });
        let mut parameters = Vec::new();
        put_u16(&mut parameters, handle);
        match key {
            Some(key) => {
                parameters.extend_from_slice(&key);
                self.command(op::LE_LTK_REPLY, &parameters);
            }
            None => self.command(op::LE_LTK_NEGATIVE_REPLY, &parameters),
        }
    }

    fn on_encryption(&mut self, handle: u16, on: bool, result: u8) {
        let Some(link) = self.links.get_mut(&handle) else {
            return;
        };
        if !on {
            link.encrypted = false;
            let paired = link.pairing.take().is_some();
            self.notice(format!("encryption failed on a link: 0x{result:02x}"));
            if paired {
                self.events.push_back(Event::PairingFailed {
                    reason: smp::reason::UNSPECIFIED,
                    refused: false,
                });
            }
            return;
        }
        link.encrypted = true;
        if let Some((mut pairing, started)) = link.pairing.take() {
            let steps = pairing.on_encrypted(&mut self.rng);
            if let Some(link) = self.links.get_mut(&handle) {
                link.pairing = Some((pairing, started));
            }
            self.run_steps(handle, steps);
        } else {
            self.on_secured(handle);
        }
    }

    /// The link is encrypted with a bond's key: restore what the bond keeps.
    fn on_secured(&mut self, handle: u16) {
        let Some(link) = self.links.get(&handle) else {
            return;
        };
        let identity = link.identity;
        let changed_table = link.listens(Notify::ServiceChanged)
            && self
                .bond_of(link)
                .is_some_and(|bond| bond.digest != self.digest);
        if changed_table && let Some(value) = self.db.handle(Slot::ServiceChanged) {
            let mut range = Vec::new();
            put_u16(&mut range, 0x0001);
            put_u16(&mut range, 0xffff);
            let pdu = att::indication(value, &range);
            self.send(handle, l2cap::ATT, &pdu);
            if let Some(link) = self.links.get_mut(&handle) {
                link.bearer.indicating = true;
            }
            self.update_bond(identity, |bond, digest| bond.digest = digest);
        }
        self.refresh_listening(handle);
        self.ask_name(handle);
    }

    fn update_bond(&mut self, identity: Option<Address>, change: impl FnOnce(&mut Bond, u32)) {
        let digest = self.digest;
        let Some(bond) = (self.bonds.iter_mut()).find(|bond| Some(bond.address) == identity) else {
            return;
        };
        let before = bond.clone();
        change(bond, digest);
        if *bond != before {
            self.events.push_back(Event::Bonded(bond.clone()));
        }
    }

    fn refresh_listening(&mut self, handle: u16) {
        let Some(link) = self.links.get_mut(&handle) else {
            return;
        };
        let listening = link.listens(Notify::Midi);
        if listening != link.listening {
            link.listening = listening;
            self.events.push_back(Event::MidiListening(listening));
        }
    }

    /// Reads the peer's GAP name for the device list, once per bonded link without a name.
    fn ask_name(&mut self, handle: u16) {
        let Some(link) = self.links.get(&handle) else {
            return;
        };
        let wants = !link.asking_name
            && link.encrypted
            && self.bond_of(link).is_some_and(|bond| bond.name.is_none());
        if !wants {
            return;
        }
        if let Some(link) = self.links.get_mut(&handle) {
            link.asking_name = true;
        }
        let mut request = vec![att::op::READ_BY_TYPE];
        put_u16(&mut request, 0x0001);
        put_u16(&mut request, 0xffff);
        request.extend_from_slice(&att::DEVICE_NAME.to_bytes());
        self.send(handle, l2cap::ATT, &request);
    }

    // ---- ACL, L2CAP ----

    fn send(&mut self, handle: u16, channel: u16, payload: &[u8]) {
        let frame = l2cap::frame(channel, payload);
        for (i, fragment) in frame.chunks(self.acl_size).enumerate() {
            self.acl
                .push_back((handle, hci::acl(handle, i == 0, fragment)));
        }
        self.pump();
    }

    fn pump(&mut self) {
        while self.acl_free > 0 {
            let Some((handle, packet)) = self.acl.pop_front() else {
                break;
            };
            self.acl_free -= 1;
            *self.in_flight.entry(handle).or_default() += 1;
            self.output.extend_from_slice(&packet);
        }
    }

    fn on_acl(&mut self, data: &[u8]) {
        if data.len() < 4 {
            return;
        }
        let header = u16::from_le_bytes([data[0], data[1]]);
        let handle = header & 0x0fff;
        // Packet boundary: 0b10 starts a frame, 0b01 continues one.
        let first = header & 0x3000 != 0x1000;
        let Some(link) = self.links.get_mut(&handle) else {
            return;
        };
        let frame = match link.rx.push(first, &data[4..]) {
            Ok(Some(frame)) => frame,
            Ok(None) => return,
            Err(error) => {
                self.notice(error.to_string());
                return;
            }
        };
        match frame {
            (l2cap::ATT, pdu) => self.on_att(handle, &pdu),
            (l2cap::SMP, pdu) => self.on_smp(handle, &pdu),
            (l2cap::SIGNAL, packet) => self.on_signal(handle, &packet),
            _ => {}
        }
    }

    fn on_signal(&mut self, handle: u16, packet: &[u8]) {
        match l2cap::parse_signal(packet) {
            Signal::UpdateRequest {
                identifier,
                parameters,
            } => {
                // A device asks Tetrad (the central) for other parameters: take them.
                self.send(
                    handle,
                    l2cap::SIGNAL,
                    &l2cap::update_response(identifier, true),
                );
                let mut update = Vec::new();
                put_u16(&mut update, handle);
                for value in [
                    parameters.min_interval,
                    parameters.max_interval,
                    parameters.latency,
                    parameters.timeout,
                    0,
                    0,
                ] {
                    put_u16(&mut update, value);
                }
                self.command(op::LE_CONNECTION_UPDATE, &update);
            }
            Signal::UpdateResponse { accepted } if !accepted => {
                self.notice("the host kept its connection interval".into());
            }
            Signal::Unknown { identifier } => {
                self.send(handle, l2cap::SIGNAL, &l2cap::reject(identifier));
            }
            _ => {}
        }
    }

    // ---- ATT ----

    fn on_att(&mut self, handle: u16, pdu: &[u8]) {
        let Some(&opcode) = pdu.first() else {
            return;
        };
        if !att::for_server(opcode) {
            self.on_att_client(handle, pdu);
            return;
        }
        let Some(mut link) = self.links.remove(&handle) else {
            return;
        };
        let bonded = self.bond_of(&link).is_some();
        let mut bearer = std::mem::take(&mut link.bearer);
        let mut store = LinkStore {
            link: &mut link,
            name: &self.name,
            bonded,
            events: &mut self.events,
        };
        let answer = att::serve(&self.db, &mut bearer, pdu, &mut store);
        link.bearer = bearer;
        let notify = link.notify;
        let identity = link.identity;
        self.links.insert(handle, link);
        if let Some(answer) = answer {
            self.send(handle, l2cap::ATT, &answer);
        }
        // A bonded host keeps its notification switches for the next link.
        self.update_bond(identity, |bond, _| bond.notify = notify);
        self.refresh_listening(handle);
    }

    fn on_att_client(&mut self, handle: u16, pdu: &[u8]) {
        let Some(link) = self.links.get_mut(&handle) else {
            return;
        };
        match pdu[0] {
            att::op::READ_BY_TYPE_RESPONSE if link.asking_name => {
                let length = usize::from(pdu.get(1).copied().unwrap_or(0));
                let name = (length > 2)
                    .then(|| pdu.get(4..2 + length))
                    .flatten()
                    .map(|name| String::from_utf8_lossy(name).trim().to_owned())
                    .filter(|name| !name.is_empty());
                let identity = link.identity;
                if let Some(name) = name {
                    self.update_bond(identity, |bond, _| bond.name = Some(name));
                }
            }
            // The name read failed: the device list shows the address.
            att::op::ERROR => {}
            _ => {}
        }
    }

    // ---- SMP ----

    fn on_smp(&mut self, handle: u16, pdu: &[u8]) {
        let Some(link) = self.links.get_mut(&handle) else {
            return;
        };
        if let Some((pairing, _)) = &mut link.pairing {
            let steps = pairing.on_pdu(pdu, &mut self.rng);
            self.run_steps(handle, steps);
            return;
        }
        if pdu.first() != Some(&smp::code::PAIRING_REQUEST) || link.role != Role::Peripheral {
            return;
        }
        if !self.config.pairable {
            self.send(
                handle,
                l2cap::SMP,
                &smp::failed(smp::reason::PAIRING_NOT_SUPPORTED),
            );
            self.events.push_back(Event::PairingFailed {
                reason: smp::reason::PAIRING_NOT_SUPPORTED,
                refused: true,
            });
            return;
        }
        let addresses = Addresses {
            initiator: link.peer,
            responder: (0, self.address),
        };
        match Pairing::respond(pdu, addresses) {
            Ok((pairing, response)) => {
                link.pairing = Some((pairing, self.now));
                self.send(handle, l2cap::SMP, &response);
            }
            Err(reason) => {
                self.send(handle, l2cap::SMP, &smp::failed(reason));
                self.events.push_back(Event::PairingFailed {
                    reason,
                    refused: false,
                });
            }
        }
    }

    fn run_steps(&mut self, handle: u16, steps: Vec<Step>) {
        for step in steps {
            match step {
                Step::Send(pdu) => self.send(handle, l2cap::SMP, &pdu),
                Step::Encrypt(key) => {
                    let mut parameters = Vec::new();
                    put_u16(&mut parameters, handle);
                    parameters.extend_from_slice(&[0; 10]);
                    parameters.extend_from_slice(&key);
                    self.command(op::LE_START_ENCRYPTION, &parameters);
                }
                Step::Failed { reason } => {
                    if let Some(link) = self.links.get_mut(&handle) {
                        link.pairing = None;
                    }
                    self.events.push_back(Event::PairingFailed {
                        reason,
                        refused: false,
                    });
                }
                Step::Done(keys) => self.on_paired(handle, keys),
            }
        }
    }

    fn on_paired(&mut self, handle: u16, keys: smp::Keys) {
        let Some(link) = self.links.get_mut(&handle) else {
            return;
        };
        link.pairing = None;
        if !keys.bond {
            self.notice("a host paired without a bond".into());
            self.refresh_listening(handle);
            return;
        }
        let (address_type, address) = keys.peer_identity.unwrap_or(link.peer);
        link.identity = Some(address);
        let bond = Bond {
            address,
            address_type,
            kind: if link.role == Role::Peripheral {
                Kind::Host
            } else {
                Kind::Device
            },
            irk: keys.peer_irk,
            ltk: keys.ltk,
            ediv: keys.ediv,
            rand: keys.rand,
            secure: keys.secure,
            name: None,
            notify: link.notify,
            digest: self.digest,
        };
        self.bonds.retain(|known| known.address != address);
        self.bonds.push(bond.clone());
        self.events.push_back(Event::Bonded(bond));
        self.refresh_listening(handle);
        self.ask_name(handle);
    }
}

/// Advertising data padded to the 31 octets of the command, after its length.
fn padded(data: &[u8]) -> Vec<u8> {
    let mut out = vec![data.len() as u8];
    out.extend_from_slice(data);
    out.resize(32, 0);
    out
}

/// The ATT server's view of one link.
struct LinkStore<'a> {
    link: &'a mut Link,
    name: &'a str,
    bonded: bool,
    events: &'a mut VecDeque<Event>,
}

impl att::Store for LinkStore<'_> {
    fn read(&mut self, slot: Slot) -> Vec<u8> {
        match slot {
            Slot::DeviceName => self.name.as_bytes().to_vec(),
            Slot::Cccd(notify) => {
                let on = self.link.notify & (1 << notify as u8) != 0;
                // Service Changed indicates; the others notify.
                let bit: u16 = if notify == Notify::ServiceChanged {
                    2
                } else {
                    1
                };
                (if on { bit } else { 0 }).to_le_bytes().to_vec()
            }
            _ => Vec::new(),
        }
    }

    fn write(&mut self, slot: Slot, value: &[u8]) -> Result<(), u8> {
        match slot {
            Slot::Midi => {
                for message in self.link.midi.packet(value) {
                    self.events.push_back(Event::Midi(message));
                }
                Ok(())
            }
            Slot::Cccd(notify) => {
                let [low, high] = value else {
                    return Err(att::error::INVALID_LENGTH);
                };
                let bit = 1 << notify as u8;
                if u16::from_le_bytes([*low, *high]) == 0 {
                    self.link.notify &= !bit;
                } else {
                    self.link.notify |= bit;
                }
                Ok(())
            }
            _ => Err(att::error::WRITE_NOT_PERMITTED),
        }
    }

    fn encrypted(&self) -> bool {
        self.link.encrypted
    }

    fn bonded(&self) -> bool {
        self.bonded
    }
}
