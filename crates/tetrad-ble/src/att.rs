//! ATT and the GATT server (Core Vol 3 Parts F and G): Tetrad's attribute table and the answers
//! to a client's requests. Values that change (the MIDI characteristic, the CCCDs) live in the
//! host; the server reaches them through [`Store`].

use crate::Uuid;

/// ATT opcodes.
pub mod op {
    pub const ERROR: u8 = 0x01;
    pub const MTU_REQUEST: u8 = 0x02;
    pub const MTU_RESPONSE: u8 = 0x03;
    pub const FIND_INFORMATION: u8 = 0x04;
    pub const FIND_INFORMATION_RESPONSE: u8 = 0x05;
    pub const FIND_BY_TYPE_VALUE: u8 = 0x06;
    pub const FIND_BY_TYPE_VALUE_RESPONSE: u8 = 0x07;
    pub const READ_BY_TYPE: u8 = 0x08;
    pub const READ_BY_TYPE_RESPONSE: u8 = 0x09;
    pub const READ: u8 = 0x0a;
    pub const READ_RESPONSE: u8 = 0x0b;
    pub const READ_BLOB: u8 = 0x0c;
    pub const READ_BLOB_RESPONSE: u8 = 0x0d;
    pub const READ_BY_GROUP_TYPE: u8 = 0x10;
    pub const READ_BY_GROUP_TYPE_RESPONSE: u8 = 0x11;
    pub const WRITE: u8 = 0x12;
    pub const WRITE_RESPONSE: u8 = 0x13;
    pub const NOTIFICATION: u8 = 0x1b;
    pub const INDICATION: u8 = 0x1d;
    pub const CONFIRMATION: u8 = 0x1e;
    pub const WRITE_COMMAND: u8 = 0x52;
}

/// ATT error codes.
pub mod error {
    pub const INVALID_HANDLE: u8 = 0x01;
    pub const READ_NOT_PERMITTED: u8 = 0x02;
    pub const WRITE_NOT_PERMITTED: u8 = 0x03;
    pub const INVALID_PDU: u8 = 0x04;
    pub const INSUFFICIENT_AUTHENTICATION: u8 = 0x05;
    pub const REQUEST_NOT_SUPPORTED: u8 = 0x06;
    pub const INVALID_OFFSET: u8 = 0x07;
    pub const ATTRIBUTE_NOT_FOUND: u8 = 0x0a;
    pub const INVALID_LENGTH: u8 = 0x0d;
    pub const UNLIKELY: u8 = 0x0e;
    pub const INSUFFICIENT_ENCRYPTION: u8 = 0x0f;
    pub const UNSUPPORTED_GROUP_TYPE: u8 = 0x10;
}

/// Characteristic properties.
pub mod property {
    pub const READ: u8 = 0x02;
    pub const WRITE_WITHOUT_RESPONSE: u8 = 0x04;
    pub const WRITE: u8 = 0x08;
    pub const NOTIFY: u8 = 0x10;
    pub const INDICATE: u8 = 0x20;
}

/// The default ATT MTU, and the largest that Tetrad offers.
pub const DEFAULT_MTU: u16 = 23;
pub const MAX_MTU: u16 = 247;

pub const PRIMARY_SERVICE: Uuid = Uuid::Short(0x2800);
pub const CHARACTERISTIC: Uuid = Uuid::Short(0x2803);
pub const CCCD: Uuid = Uuid::Short(0x2902);
pub const DEVICE_NAME: Uuid = Uuid::Short(0x2a00);

/// Who may read or write an attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Never,
    Open,
    /// Only on an encrypted link.
    Encrypted,
}

/// The notifications and indications a client can turn on, one bit each in a link's CCCDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notify {
    ServiceChanged = 0,
    Midi = 1,
    Link = 2,
}

/// A value that lives in the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    DeviceName,
    ServiceChanged,
    Midi,
    /// The Tetrad Link request characteristic (app → unit).
    LinkRequest,
    /// The Tetrad Link reply characteristic (unit → app, notify only).
    LinkReply,
    Cccd(Notify),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Fixed(Vec<u8>),
    Slot(Slot),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    pub handle: u16,
    pub kind: Uuid,
    pub read: Access,
    pub write: Access,
    pub value: Value,
}

/// The attribute table.
#[derive(Debug, Clone, Default)]
pub struct Database {
    attributes: Vec<Attribute>,
}

impl Database {
    fn push(&mut self, kind: Uuid, read: Access, write: Access, value: Value) -> u16 {
        let handle = self.attributes.len() as u16 + 1;
        self.attributes.push(Attribute {
            handle,
            kind,
            read,
            write,
            value,
        });
        handle
    }

    pub fn service(&mut self, uuid: Uuid) {
        let value = Value::Fixed(uuid.to_bytes());
        self.push(PRIMARY_SERVICE, Access::Open, Access::Never, value);
    }

    /// A characteristic declaration and its value; returns the value handle.
    pub fn characteristic(
        &mut self,
        uuid: Uuid,
        properties: u8,
        read: Access,
        write: Access,
        value: Value,
    ) -> u16 {
        let value_handle = self.attributes.len() as u16 + 2;
        let mut declaration = vec![properties];
        declaration.extend_from_slice(&value_handle.to_le_bytes());
        declaration.extend_from_slice(&uuid.to_bytes());
        let fixed = Value::Fixed(declaration);
        self.push(CHARACTERISTIC, Access::Open, Access::Never, fixed);
        self.push(uuid, read, write, value)
    }

    pub fn cccd(&mut self, notify: Notify, write: Access) {
        self.push(CCCD, Access::Open, write, Value::Slot(Slot::Cccd(notify)));
    }

    /// GAP, GATT (with Service Changed) and BLE MIDI; with `link`, also the Tetrad Link
    /// service of the app.
    pub fn tetrad(link: bool) -> Self {
        let mut db = Self::default();
        db.service(Uuid::Short(0x1800));
        db.characteristic(
            DEVICE_NAME,
            property::READ,
            Access::Open,
            Access::Never,
            Value::Slot(Slot::DeviceName),
        );
        db.characteristic(
            Uuid::Short(0x2a01),
            property::READ,
            Access::Open,
            Access::Never,
            Value::Fixed(vec![0, 0]),
        );
        db.service(Uuid::Short(0x1801));
        db.characteristic(
            Uuid::Short(0x2a05),
            property::INDICATE,
            Access::Never,
            Access::Never,
            Value::Slot(Slot::ServiceChanged),
        );
        db.cccd(Notify::ServiceChanged, Access::Open);
        db.service(crate::midi::SERVICE);
        db.characteristic(
            crate::midi::IO,
            property::READ | property::WRITE_WITHOUT_RESPONSE | property::NOTIFY,
            Access::Encrypted,
            Access::Encrypted,
            Value::Slot(Slot::Midi),
        );
        db.cccd(Notify::Midi, Access::Encrypted);
        if link {
            db.service(crate::link::SERVICE);
            db.characteristic(
                crate::link::REQUEST,
                property::WRITE_WITHOUT_RESPONSE | property::WRITE,
                Access::Never,
                Access::Encrypted,
                Value::Slot(Slot::LinkRequest),
            );
            db.characteristic(
                crate::link::REPLY,
                property::NOTIFY,
                Access::Never,
                Access::Never,
                Value::Slot(Slot::LinkReply),
            );
            db.cccd(Notify::Link, Access::Encrypted);
        }
        db
    }

    pub fn attributes(&self) -> &[Attribute] {
        &self.attributes
    }

    /// The handle of the attribute with `slot`.
    pub fn handle(&self, slot: Slot) -> Option<u16> {
        (self.attributes.iter())
            .find(|attribute| attribute.value == Value::Slot(slot))
            .map(|attribute| attribute.handle)
    }

    /// A digest of the table's layout. A bonded client that saw another digest gets a Service
    /// Changed indication, so it discovers the table again.
    pub fn digest(&self) -> u32 {
        let mut hash: u32 = 0x811c_9dc5;
        for attribute in &self.attributes {
            let mut bytes = attribute.handle.to_le_bytes().to_vec();
            bytes.extend(attribute.kind.as_u128().to_le_bytes());
            if let Value::Fixed(value) = &attribute.value {
                bytes.extend(value);
            }
            for byte in bytes {
                hash = (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193);
            }
        }
        hash
    }

    fn get(&self, handle: u16) -> Option<&Attribute> {
        self.attributes.get(usize::from(handle).checked_sub(1)?)
    }

    fn in_range(&self, start: u16, end: u16) -> impl Iterator<Item = &Attribute> {
        (self.attributes.iter()).filter(move |a| (start..=end).contains(&a.handle))
    }

    /// The last handle of the service that starts at `handle`.
    fn group_end(&self, handle: u16) -> u16 {
        (self.attributes.iter())
            .find(|a| a.handle > handle && a.kind == PRIMARY_SERVICE)
            .map_or(self.attributes.len() as u16, |next| next.handle - 1)
    }
}

/// The host side of the server: the values that change, and the link's security.
pub trait Store {
    fn read(&mut self, slot: Slot) -> Vec<u8>;
    /// Takes a written value; `Err` is an ATT error code.
    fn write(&mut self, slot: Slot, value: &[u8]) -> Result<(), u8>;
    fn encrypted(&self) -> bool;
    /// The peer has a bond with Tetrad, so it can encrypt the link without a pairing.
    fn bonded(&self) -> bool;
}

/// One link's ATT state.
#[derive(Debug, Clone)]
pub struct Bearer {
    pub mtu: u16,
    /// An indication waits for its confirmation; no other may go out.
    pub indicating: bool,
}

impl Default for Bearer {
    fn default() -> Self {
        Self {
            mtu: DEFAULT_MTU,
            indicating: false,
        }
    }
}

/// A PDU that a client sends to a server: a request, a command or a confirmation. The others
/// (responses, notifications, indications) are for Tetrad's client side.
pub fn for_server(opcode: u8) -> bool {
    matches!(
        opcode,
        op::MTU_REQUEST
            | op::FIND_INFORMATION
            | op::FIND_BY_TYPE_VALUE
            | op::READ_BY_TYPE
            | op::READ
            | op::READ_BLOB
            | op::READ_BY_GROUP_TYPE
            | op::WRITE
            | op::CONFIRMATION
            | op::WRITE_COMMAND
            | 0x0e
            | 0x16
            | 0x18
            | 0x20
            | 0xd2
    )
}

pub fn error_response(request: u8, handle: u16, code: u8) -> Vec<u8> {
    let mut out = vec![op::ERROR, request];
    out.extend_from_slice(&handle.to_le_bytes());
    out.push(code);
    out
}

fn u16_at(pdu: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*pdu.get(at)?, *pdu.get(at + 1)?]))
}

/// Checks `access` on the link; `Err` is the ATT error.
fn allowed(access: Access, store: &dyn Store, never: u8) -> Result<(), u8> {
    match access {
        Access::Open => Ok(()),
        Access::Never => Err(never),
        Access::Encrypted if store.encrypted() => Ok(()),
        // A bonded peer can encrypt with its key; a new one must pair first.
        Access::Encrypted if store.bonded() => Err(error::INSUFFICIENT_ENCRYPTION),
        Access::Encrypted => Err(error::INSUFFICIENT_AUTHENTICATION),
    }
}

fn read_value(attribute: &Attribute, store: &mut dyn Store) -> Vec<u8> {
    match &attribute.value {
        Value::Fixed(bytes) => bytes.clone(),
        Value::Slot(slot) => store.read(*slot),
    }
}

/// Answers one client PDU. `None`: no answer goes out (a command, a confirmation).
pub fn serve(
    db: &Database,
    bearer: &mut Bearer,
    pdu: &[u8],
    store: &mut dyn Store,
) -> Option<Vec<u8>> {
    let (&opcode, _) = pdu.split_first()?;
    let answer = match opcode {
        op::MTU_REQUEST => mtu(bearer, pdu),
        op::FIND_INFORMATION => find_information(db, bearer, pdu),
        op::FIND_BY_TYPE_VALUE => find_by_type_value(db, bearer, pdu),
        op::READ_BY_TYPE => read_by_type(db, bearer, pdu, store),
        op::READ | op::READ_BLOB => read(db, bearer, pdu, store),
        op::READ_BY_GROUP_TYPE => read_by_group_type(db, bearer, pdu),
        op::WRITE | op::WRITE_COMMAND => {
            let result = write(db, pdu, store);
            if opcode == op::WRITE_COMMAND {
                return None;
            }
            result.map(|()| vec![op::WRITE_RESPONSE])
        }
        op::CONFIRMATION => {
            bearer.indicating = false;
            return None;
        }
        // Signed writes and other commands: no answer.
        _ if opcode & 0x40 != 0 => return None,
        _ => Err((0, error::REQUEST_NOT_SUPPORTED)),
    };
    Some(answer.unwrap_or_else(|(handle, code)| error_response(opcode, handle, code)))
}

type Answer = Result<Vec<u8>, (u16, u8)>;

fn range(pdu: &[u8]) -> Result<(u16, u16), (u16, u8)> {
    let (start, end) = (u16_at(pdu, 1), u16_at(pdu, 3));
    let (Some(start), Some(end)) = (start, end) else {
        return Err((0, error::INVALID_PDU));
    };
    if start == 0 || start > end {
        return Err((start, error::INVALID_HANDLE));
    }
    Ok((start, end))
}

fn mtu(bearer: &mut Bearer, pdu: &[u8]) -> Answer {
    let client = u16_at(pdu, 1).ok_or((0, error::INVALID_PDU))?;
    bearer.mtu = client.clamp(DEFAULT_MTU, MAX_MTU);
    let mut out = vec![op::MTU_RESPONSE];
    out.extend_from_slice(&MAX_MTU.to_le_bytes());
    Ok(out)
}

fn find_information(db: &Database, bearer: &Bearer, pdu: &[u8]) -> Answer {
    let (start, end) = range(pdu)?;
    let mut out = vec![op::FIND_INFORMATION_RESPONSE, 0];
    for attribute in db.in_range(start, end) {
        let uuid = attribute.kind.to_bytes();
        let format = if uuid.len() == 2 { 1 } else { 2 };
        if out[1] == 0 {
            out[1] = format;
        } else if out[1] != format || out.len() + 2 + uuid.len() > usize::from(bearer.mtu) {
            break;
        }
        out.extend_from_slice(&attribute.handle.to_le_bytes());
        out.extend_from_slice(&uuid);
    }
    if out[1] == 0 {
        return Err((start, error::ATTRIBUTE_NOT_FOUND));
    }
    Ok(out)
}

fn find_by_type_value(db: &Database, bearer: &Bearer, pdu: &[u8]) -> Answer {
    let (start, end) = range(pdu)?;
    let kind = Uuid::Short(u16_at(pdu, 5).ok_or((0, error::INVALID_PDU))?);
    let value = &pdu[7..];
    let mut out = vec![op::FIND_BY_TYPE_VALUE_RESPONSE];
    for attribute in db.in_range(start, end) {
        let matches = attribute.kind == kind
            && matches!(&attribute.value, Value::Fixed(fixed) if fixed == value);
        if !matches {
            continue;
        }
        if out.len() + 4 > usize::from(bearer.mtu) {
            break;
        }
        let group_end = if kind == PRIMARY_SERVICE {
            db.group_end(attribute.handle)
        } else {
            attribute.handle
        };
        out.extend_from_slice(&attribute.handle.to_le_bytes());
        out.extend_from_slice(&group_end.to_le_bytes());
    }
    if out.len() == 1 {
        return Err((start, error::ATTRIBUTE_NOT_FOUND));
    }
    Ok(out)
}

fn read_by_type(db: &Database, bearer: &Bearer, pdu: &[u8], store: &mut dyn Store) -> Answer {
    let (start, end) = range(pdu)?;
    let kind = Uuid::from_bytes(&pdu[5..]).ok_or((0, error::INVALID_PDU))?;
    let mut out = vec![op::READ_BY_TYPE_RESPONSE, 0];
    for attribute in db.in_range(start, end).filter(|a| a.kind == kind) {
        if let Err(code) = allowed(attribute.read, store, error::READ_NOT_PERMITTED) {
            // The first match decides; after it, the list ends here.
            if out[1] == 0 {
                return Err((attribute.handle, code));
            }
            break;
        }
        let mut value = read_value(attribute, store);
        value.truncate((usize::from(bearer.mtu) - 4).min(253));
        let length = value.len() as u8 + 2;
        if out[1] == 0 {
            out[1] = length;
        } else if out[1] != length || out.len() + usize::from(length) > usize::from(bearer.mtu) {
            break;
        }
        out.extend_from_slice(&attribute.handle.to_le_bytes());
        out.extend_from_slice(&value);
    }
    if out[1] == 0 {
        return Err((start, error::ATTRIBUTE_NOT_FOUND));
    }
    Ok(out)
}

fn read(db: &Database, bearer: &Bearer, pdu: &[u8], store: &mut dyn Store) -> Answer {
    let handle = u16_at(pdu, 1).ok_or((0, error::INVALID_PDU))?;
    let attribute = db.get(handle).ok_or((handle, error::INVALID_HANDLE))?;
    allowed(attribute.read, store, error::READ_NOT_PERMITTED).map_err(|code| (handle, code))?;
    let value = read_value(attribute, store);
    let (response, offset) = if pdu[0] == op::READ_BLOB {
        let offset = usize::from(u16_at(pdu, 3).ok_or((0, error::INVALID_PDU))?);
        if offset > value.len() {
            return Err((handle, error::INVALID_OFFSET));
        }
        (op::READ_BLOB_RESPONSE, offset)
    } else {
        (op::READ_RESPONSE, 0)
    };
    let mut out = vec![response];
    let end = value.len().min(offset + usize::from(bearer.mtu) - 1);
    out.extend_from_slice(&value[offset..end]);
    Ok(out)
}

fn read_by_group_type(db: &Database, bearer: &Bearer, pdu: &[u8]) -> Answer {
    let (start, end) = range(pdu)?;
    let kind = Uuid::from_bytes(&pdu[5..]).ok_or((0, error::INVALID_PDU))?;
    if kind != PRIMARY_SERVICE {
        return Err((start, error::UNSUPPORTED_GROUP_TYPE));
    }
    let mut out = vec![op::READ_BY_GROUP_TYPE_RESPONSE, 0];
    for attribute in db
        .in_range(start, end)
        .filter(|a| a.kind == PRIMARY_SERVICE)
    {
        let Value::Fixed(uuid) = &attribute.value else {
            continue;
        };
        let length = uuid.len() as u8 + 4;
        if out[1] == 0 {
            out[1] = length;
        } else if out[1] != length || out.len() + usize::from(length) > usize::from(bearer.mtu) {
            break;
        }
        out.extend_from_slice(&attribute.handle.to_le_bytes());
        out.extend_from_slice(&db.group_end(attribute.handle).to_le_bytes());
        out.extend_from_slice(uuid);
    }
    if out[1] == 0 {
        return Err((start, error::ATTRIBUTE_NOT_FOUND));
    }
    Ok(out)
}

fn write(db: &Database, pdu: &[u8], store: &mut dyn Store) -> Result<(), (u16, u8)> {
    let handle = u16_at(pdu, 1).ok_or((0, error::INVALID_PDU))?;
    let attribute = db.get(handle).ok_or((handle, error::INVALID_HANDLE))?;
    allowed(attribute.write, store, error::WRITE_NOT_PERMITTED).map_err(|code| (handle, code))?;
    let Value::Slot(slot) = attribute.value else {
        return Err((handle, error::WRITE_NOT_PERMITTED));
    };
    store.write(slot, &pdu[3..]).map_err(|code| (handle, code))
}

/// A notification of `value` at `handle`, cut to the link's MTU.
pub fn notification(bearer: &Bearer, handle: u16, value: &[u8]) -> Vec<u8> {
    let mut out = vec![op::NOTIFICATION];
    out.extend_from_slice(&handle.to_le_bytes());
    let room = usize::from(bearer.mtu) - 3;
    out.extend_from_slice(&value[..value.len().min(room)]);
    out
}

/// An indication of `value` at `handle`.
pub fn indication(handle: u16, value: &[u8]) -> Vec<u8> {
    let mut out = vec![op::INDICATION];
    out.extend_from_slice(&handle.to_le_bytes());
    out.extend_from_slice(value);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        encrypted: bool,
        bonded: bool,
        cccd: u16,
        written: Vec<(Slot, Vec<u8>)>,
    }

    impl Store for Fake {
        fn read(&mut self, slot: Slot) -> Vec<u8> {
            match slot {
                Slot::DeviceName => b"Quad Cortex 1FEB".to_vec(),
                Slot::Cccd(_) => self.cccd.to_le_bytes().to_vec(),
                _ => Vec::new(),
            }
        }
        fn write(&mut self, slot: Slot, value: &[u8]) -> Result<(), u8> {
            self.written.push((slot, value.to_vec()));
            Ok(())
        }
        fn encrypted(&self) -> bool {
            self.encrypted
        }
        fn bonded(&self) -> bool {
            self.bonded
        }
    }

    fn ask(db: &Database, store: &mut Fake, pdu: &[u8]) -> Vec<u8> {
        serve(db, &mut Bearer::default(), pdu, store).unwrap()
    }

    #[test]
    fn services_are_discovered_by_group() {
        let db = Database::tetrad(false);
        let mut store = Fake::default();
        // Primary services from handle 1: GAP (1-5) and GATT (6-9), both 16-bit.
        let answer = ask(&db, &mut store, &[0x10, 0x01, 0x00, 0xff, 0xff, 0x00, 0x28]);
        assert_eq!(
            answer,
            [0x11, 6, 1, 0, 5, 0, 0x00, 0x18, 6, 0, 9, 0, 0x01, 0x18]
        );
        // From handle 10: the MIDI service with its 128-bit UUID, alone in the answer.
        let answer = ask(&db, &mut store, &[0x10, 0x0a, 0x00, 0xff, 0xff, 0x00, 0x28]);
        assert_eq!(answer[..6], [0x11, 20, 10, 0, 13, 0]);
        assert_eq!(answer[6..], crate::midi::SERVICE.to_bytes()[..]);
        // Past the end.
        let answer = ask(&db, &mut store, &[0x10, 0x0e, 0x00, 0xff, 0xff, 0x00, 0x28]);
        assert_eq!(answer, [0x01, 0x10, 0x0e, 0x00, error::ATTRIBUTE_NOT_FOUND]);
    }

    #[test]
    fn the_midi_characteristic_asks_for_pairing_then_for_encryption() {
        let db = Database::tetrad(false);
        let midi = db.handle(Slot::Midi).unwrap();
        let read = [op::READ, midi as u8, 0];
        let mut store = Fake::default();
        assert_eq!(
            ask(&db, &mut store, &read),
            [
                0x01,
                op::READ,
                midi as u8,
                0,
                error::INSUFFICIENT_AUTHENTICATION
            ]
        );
        store.bonded = true;
        assert_eq!(
            ask(&db, &mut store, &read),
            [
                0x01,
                op::READ,
                midi as u8,
                0,
                error::INSUFFICIENT_ENCRYPTION
            ]
        );
        store.encrypted = true;
        assert_eq!(ask(&db, &mut store, &read), [op::READ_RESPONSE]);
        // A write command reaches the store and gets no answer.
        let mut bearer = Bearer::default();
        let command = [op::WRITE_COMMAND, midi as u8, 0, 0x80, 0x80, 0x90, 60, 100];
        assert_eq!(serve(&db, &mut bearer, &command, &mut store), None);
        assert_eq!(
            store.written,
            vec![(Slot::Midi, vec![0x80, 0x80, 0x90, 60, 100])]
        );
    }

    #[test]
    fn characteristics_names_and_mtu() {
        let db = Database::tetrad(false);
        let mut store = Fake::default();
        // Characteristic declarations in the GAP service.
        let answer = ask(&db, &mut store, &[0x08, 0x01, 0x00, 0x05, 0x00, 0x03, 0x28]);
        assert_eq!(
            answer,
            [
                0x09, 7, 2, 0, 0x02, 3, 0, 0x00, 0x2a, 4, 0, 0x02, 5, 0, 0x01, 0x2a
            ]
        );
        // The device name by type.
        let answer = ask(&db, &mut store, &[0x08, 0x01, 0x00, 0xff, 0xff, 0x00, 0x2a]);
        assert_eq!(answer[..4], [0x09, 18, 3, 0]);
        assert_eq!(&answer[4..], b"Quad Cortex 1FEB");
        let mut bearer = Bearer::default();
        let answer = serve(&db, &mut bearer, &[0x02, 0x00, 0x02], &mut store).unwrap();
        assert_eq!(answer, [0x03, 247, 0]);
        assert_eq!(bearer.mtu, 247);
        // A long write is not supported.
        assert_eq!(
            ask(&db, &mut store, &[0x16, 0x0c, 0, 0, 0]),
            [0x01, 0x16, 0, 0, error::REQUEST_NOT_SUPPORTED]
        );
    }

    #[test]
    fn find_information_lists_one_uuid_size_at_a_time() {
        let db = Database::tetrad(false);
        let mut store = Fake::default();
        let answer = ask(&db, &mut store, &[0x04, 0x08, 0x00, 0x0d, 0x00]);
        // Handles 8 and 9 are 16-bit; the MIDI service declaration (10) is 16-bit too.
        assert_eq!(answer[..2], [0x05, 1]);
        assert_eq!(answer[2..6], [8, 0, 0x05, 0x2a]);
    }
}
