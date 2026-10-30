/** GATT UUIDs of the unit (crates/tetrad-ble/src/link.rs and midi.rs). */
export const LINK_SERVICE = "5162a6fe-ad05-405f-b22e-6e4047a5b524";
/** App → unit: write without response. Needs an encrypted link. */
export const LINK_REQUEST = "5162a6fe-ad05-405f-b22e-6e4047a5b525";
/** Unit → app: notify. Needs an encrypted link. */
export const LINK_REPLY = "5162a6fe-ad05-405f-b22e-6e4047a5b526";
/** The unit advertises the BLE MIDI service, not the Link: scans look for this one. */
export const MIDI_SERVICE = "03b80e5a-ede8-4b33-a751-6ce34ec4c700";
/** The MTU the unit takes. */
export const MTU = 247;
