//! The Tetrad Link (owned, Tetrad; docs/bluetooth.md): the requests of the Tetrad app over
//! Bluetooth. A message is the USB link's format (`tetrad_usb::encode`: a JSON header, then a
//! body). The app gives the Bluetooth PIN once per pairing (`auth`); after that its requests
//! reach the same handlers as the Remote page's, and `extension` reaches an extension's
//! `ctx.ble.onMessage`.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value as Json, json};
use tetrad_ble::Command;
use tetrad_remote::{Reply, Request};

use super::extensions::{BleReply, Outcome};
use super::platform::Platform;
use super::{Device, Subsystem};

/// Request IDs of the link carry this bit, so that a late answer of an extension (a tab call)
/// goes back to the app and not to the Remote page.
pub const LINK_CALL: u64 = 1 << 63;
/// The link's own protocol level, in `hello`.
pub const PROTOCOL: u32 = 1;
/// Wrong PINs before the unit makes a new one, as for Remote.
const MAX_WRONG_PINS: u8 = 10;
/// After a wrong PIN, the next try waits this long.
const WRONG_PIN_WAIT: Duration = Duration::from_secs(1);

#[derive(Debug, Deserialize)]
struct LinkRequest {
    id: u64,
    #[serde(flatten)]
    op: Op,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
enum Op {
    Hello,
    Auth {
        pin: String,
    },
    Status,
    IrUpload {
        name: String,
    },
    Files,
    /// `extension` is the extension's manifest ID (the header's `id` is the request's).
    FileChunk {
        extension: String,
        name: String,
        offset: u64,
        #[serde(default)]
        last: bool,
    },
    FileDelete {
        extension: String,
        name: String,
    },
    Tabs,
    TabPage {
        extension: String,
    },
    TabCall {
        extension: String,
        message: Json,
    },
    Support,
    Extension {
        extension: String,
        message: Json,
    },
}

/// The link state of the connected app.
#[derive(Debug, Default)]
pub struct LinkState {
    /// The PIN the app gives; new at each start of Bluetooth.
    pub pin: Option<String>,
    /// The connected host gave the PIN, now or with an earlier link of its bond.
    pub authorized: bool,
    wrong: u8,
    /// No PIN is taken before this time.
    wait_until: Duration,
}

impl LinkState {
    /// A wrong PIN: wait, and after [`MAX_WRONG_PINS`] the PIN is no longer good.
    fn wrong_pin(&mut self, now: Duration) -> bool {
        self.wrong += 1;
        self.wait_until = now + WRONG_PIN_WAIT;
        if self.wrong >= MAX_WRONG_PINS {
            self.wrong = 0;
            return true;
        }
        false
    }
}

/// The header and body of a reply.
fn reply(id: u64, mut header: Json, body: &[u8]) -> Vec<u8> {
    if let Some(object) = header.as_object_mut() {
        object.insert("id".into(), json!(id));
    }
    tetrad_usb::encode(&header, body)
}

fn error(id: u64, text: impl Into<String>) -> Vec<u8> {
    reply(id, json!({ "error": text.into() }), &[])
}

/// A Remote reply as a link reply: its JSON, and the zip of a download as the body.
fn remote_reply(id: u64, answer: &Reply) -> Vec<u8> {
    let (status, value) = answer.response();
    let body: &[u8] = match answer {
        Reply::File { zip, .. } => zip,
        _ => &[],
    };
    let mut header = value;
    if status >= 400 && header.get("error").is_none() {
        header = json!({ "error": format!("refused ({status})") });
    }
    reply(id, header, body)
}

impl<P: Platform> Device<P> {
    /// One message of the app.
    pub(super) fn on_link_message(&mut self, message: &[u8], now: Duration) {
        let (request, body) = match tetrad_usb::decode::<LinkRequest>(message) {
            Ok(decoded) => decoded,
            Err(problem) => {
                self.notice(Subsystem::Bluetooth, format!("app request: {problem}"));
                // The ID is unknown, so the reply names none.
                let text = format!("not a request: {problem}");
                let answer = tetrad_usb::encode(&json!({ "error": text }), &[]);
                self.link_send(answer);
                return;
            }
        };
        let id = request.id;
        if let Some(answer) = self.link_answer(id, request.op, body, now) {
            self.link_send(answer);
        }
    }

    fn link_send(&mut self, message: Vec<u8>) {
        if let Some(backend) = self.platform.bluetooth() {
            backend.send(Command::LinkSend(message));
        }
    }

    /// The reply to one request; `None` when an extension answers later.
    fn link_answer(&mut self, id: u64, op: Op, body: &[u8], now: Duration) -> Option<Vec<u8>> {
        match op {
            Op::Hello => {
                let bluetooth = &self.bridges.bluetooth;
                let header = json!({
                    "reply": "hello",
                    "tetrad": env!("CARGO_PKG_VERSION"),
                    "protocol": PROTOCOL,
                    "name": bluetooth.view().name,
                    "authorized": bluetooth.link.authorized,
                });
                return Some(reply(id, header, &[]));
            }
            Op::Auth { pin } => {
                let link = &mut self.bridges.bluetooth.link;
                if now < link.wait_until {
                    return Some(error(id, "Wait a second before the next PIN"));
                }
                if link.pin.as_deref() != Some(pin.as_str()) {
                    if link.wrong_pin(now) {
                        self.new_link_pin();
                        let text = format!("a new PIN after {MAX_WRONG_PINS} wrong PINs");
                        self.notice(Subsystem::Bluetooth, text);
                    }
                    return Some(error(id, "Wrong PIN"));
                }
                link.wrong = 0;
                self.authorize_host();
                return Some(reply(
                    id,
                    json!({ "reply": "auth", "authorized": true }),
                    &[],
                ));
            }
            _ if !self.bridges.bluetooth.link.authorized => {
                return Some(error(id, "Give the PIN first (auth)"));
            }
            _ => {}
        }
        let call = LINK_CALL | id;
        let request = match op {
            Op::Hello | Op::Auth { .. } => unreachable!("answered above"),
            Op::Extension { extension, message } => {
                return self.link_extension(call, &extension, message);
            }
            // The app's link has no PIN of its own: the pairing and `auth` stand for it.
            Op::Status => Request::Status { pin: String::new() },
            Op::IrUpload { name } => Request::Upload {
                pin: String::new(),
                name,
                wav: body.to_vec(),
            },
            Op::Files => Request::Files { pin: String::new() },
            Op::FileChunk {
                extension,
                name,
                offset,
                last,
            } => Request::FileChunk {
                pin: String::new(),
                id: extension,
                name,
                offset,
                last,
                bytes: body.to_vec(),
            },
            Op::FileDelete { extension, name } => Request::FileDelete {
                pin: String::new(),
                id: extension,
                name,
            },
            Op::Tabs => Request::Tabs { pin: String::new() },
            Op::TabPage { extension } => Request::TabPage {
                pin: String::new(),
                id: extension,
            },
            Op::TabCall { extension, message } => Request::TabCall {
                pin: String::new(),
                id: extension,
                message: message.to_string().into_bytes(),
            },
            Op::Support => Request::Support { pin: String::new() },
        };
        self.remote_answer(call, request)
            .map(|answer| remote_reply(id, &answer))
    }

    /// The `extension` request: the extension's `ctx.ble.onMessage` answers it later.
    fn link_extension(&mut self, call: u64, extension: &str, message: Json) -> Option<Vec<u8>> {
        let id = call & !LINK_CALL;
        if self.platform.extensions().is_none() {
            return Some(error(id, "extensions do not run in this mode"));
        }
        let mut refused = None;
        let effects = self.extensions(|extensions, backend, _| {
            (extensions.ble_message(extension, call, message, backend)).unwrap_or_else(|e| {
                refused = Some(e);
                Outcome::default()
            })
        });
        self.apply(Ok::<_, super::DeviceError>(effects));
        refused.map(|text| error(id, text))
    }

    /// An extension answered an `extension` request.
    pub(super) fn answer_link_extension(&mut self, answer: BleReply) {
        let id = answer.request & !LINK_CALL;
        let message = match answer.result {
            Ok(value) => reply(id, json!({ "reply": "extension", "value": value }), &[]),
            Err(text) => error(id, text),
        };
        self.link_send(message);
    }

    /// An extension answered a tab call that came over the link.
    pub(super) fn answer_link_tab(&mut self, call: u64, answer: Reply) {
        self.link_send(remote_reply(call & !LINK_CALL, &answer));
    }

    /// `scope.ble.notify`: a value pushed to the app.
    pub(super) fn link_event(&mut self, extension: String, value: Json) {
        let header = json!({ "event": "extension", "extension": extension, "value": value });
        self.link_send(tetrad_usb::encode(&header, &[]));
    }

    /// A new PIN (start of Bluetooth, or too many wrong PINs).
    pub(super) fn new_link_pin(&mut self) {
        let pin = self
            .platform
            .random_u32()
            .map(|value| format!("{:04}", value % 10_000));
        self.bridges.bluetooth.link.pin = pin;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_decode_from_the_usb_message_format() {
        let header = json!({
            "id": 7, "op": "file-chunk", "extension": "tetrad.nam", "name": "Plexi.nam",
            "offset": 0, "last": true,
        });
        let message = tetrad_usb::encode(&header, b"abc");
        let (request, body) = tetrad_usb::decode::<LinkRequest>(&message).unwrap();
        assert_eq!(request.id, 7);
        assert!(matches!(
            request.op,
            Op::FileChunk { ref extension, last: true, .. } if extension == "tetrad.nam"
        ));
        assert_eq!(body, b"abc");
        // A field missing: not a request.
        let message = tetrad_usb::encode(&json!({"id": 8, "op": "file-delete"}), &[]);
        assert!(tetrad_usb::decode::<LinkRequest>(&message).is_err());
        let message =
            tetrad_usb::encode(&json!({"id": 9, "op": "ir-upload", "name": "V30"}), b"RIFF");
        let (request, body) = tetrad_usb::decode::<LinkRequest>(&message).unwrap();
        assert_eq!(request.id, 9);
        assert!(matches!(request.op, Op::IrUpload { ref name } if name == "V30"));
        assert_eq!(body, b"RIFF");
    }

    #[test]
    fn ten_wrong_pins_make_a_new_pin_and_each_one_waits() {
        let mut link = LinkState::default();
        let at = Duration::from_secs(5);
        for _ in 0..9 {
            assert!(!link.wrong_pin(at));
        }
        assert_eq!(link.wait_until, at + WRONG_PIN_WAIT);
        assert!(link.wrong_pin(at));
        assert_eq!(link.wrong, 0);
    }
}
