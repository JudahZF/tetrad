//! Cortex Cloud clients over an injected [`crate::http::HttpClient`].

pub mod api;
pub mod endpoint;
pub mod products;
pub mod services;
pub mod session;
pub mod signing;

use std::fmt;

use thiserror::Error;
use zeroize::Zeroizing;

use crate::http::HttpRequest;

/// `X-Cloud-Api-Client-Type` / `Hardware-Source-Type` value (VA 0x0045b930).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientType {
    QuadCortex,
    /// Device types 2 and 3 (VA 0x00496238).
    QuadCortexMini,
}

impl ClientType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QuadCortex => "quad_cortex",
            Self::QuadCortexMini => "quad_cortex_mini",
        }
    }
}

/// What the device says about itself in request headers.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceIdentity {
    pub client_type: ClientType,
    /// CorOS version string (`DAT_015399b4`), e.g. `4.1.0`.
    pub firmware: String,
    /// Device serial (stock reads 9–10 bytes from the file named by `QC_SERIAL_NUMBER`).
    pub serial: String,
}

impl DeviceIdentity {
    /// The firmware before its first `-`, as pair and licence messages use it (VA 0x00668bec).
    pub fn firmware_prefix(&self) -> &str {
        self.firmware
            .split_once('-')
            .map_or(&self.firmware, |(prefix, _)| prefix)
    }
}

impl fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceIdentity")
            .field("client_type", &self.client_type)
            .field("firmware", &self.firmware)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("request signing failed: {0}")]
pub struct SignError(pub String);

/// Device-held request signing: `sign` gives `X-Cloud-Api-Client-Signature`, `client_id` the
/// base64 `X-Cloud-Api-Client-Id`. [`signing::SharcSigner`] is the stock implementation over
/// SHARC replies; no secret is in this crate.
pub trait ClientSigner {
    fn sign(&mut self, message: &str) -> Result<String, SignError>;
    fn client_id(&mut self) -> Result<String, SignError>;
}

/// The signed requests: three of the four callers of the message builder (VA 0x004a08e0); the
/// fourth verifies licence replies ([`signing::SigningMaterial::validate`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignedRequest {
    /// `GET /firmware/v1/…` (VA 0x00469d90): full firmware, no payload.
    UpdateCheck,
    /// `POST|DELETE /hardware/v1/pair` (VA 0x00300d68): firmware prefix, no payload.
    Pair,
    /// `POST /purchase/v1/licenses/activate` (VA 0x001efbb8): firmware prefix, payload = the
    /// base64 client id.
    LicenceActivate,
}

const SIGNATURE: &str = "X-Cloud-Api-Client-Signature";
const DATE: &str = "X-Cloud-Api-Date";
const SERIAL: &str = "X-Cloud-Api-Client-Serial-Number";
const CLIENT_ID: &str = "X-Cloud-Api-Client-Id";

impl SignedRequest {
    fn firmware(self, device: &DeviceIdentity) -> &str {
        match self {
            Self::UpdateCheck => &device.firmware,
            Self::Pair | Self::LicenceActivate => device.firmware_prefix(),
        }
    }

    /// Stock header set order (w4 §6). Headers not listed keep their place in front.
    fn header_order(self) -> &'static [&'static str] {
        match self {
            // VA 0x00473050.
            Self::UpdateCheck => &[
                SIGNATURE,
                "X-Cloud-Api-Client-Type",
                "X-Cloud-Api-Client-Version",
                SERIAL,
                DATE,
                "Authorization",
                "Content-Type",
                CLIENT_ID,
            ],
            // VA 0x00301ec0.
            Self::Pair => &[DATE, SIGNATURE, SERIAL, CLIENT_ID],
            // VAs 0x001eed80, 0x001eea58.
            Self::LicenceActivate => &[SERIAL, CLIENT_ID, SIGNATURE, DATE],
        }
    }
}

/// `%1-%2-with-firmware-%3-at-%4`, then `-payload` when the payload is not empty
/// (VA 0x004a08e0). The signer appends its own suffix.
pub fn signing_message(
    device: &DeviceIdentity,
    firmware: &str,
    unix_seconds: i64,
    payload: &str,
) -> String {
    let mut message = format!(
        "{}-{}-with-firmware-{firmware}-at-{unix_seconds}",
        device.client_type.as_str(),
        device.serial,
    );
    if !payload.is_empty() {
        message.push('-');
        message.push_str(payload);
    }
    message
}

/// Adds the signature, date, serial and client id headers to `http` in the stock order for
/// `kind`. The date is the same `unix_seconds` the message carries.
pub fn sign_request(
    http: &mut HttpRequest,
    signer: &mut impl ClientSigner,
    device: &DeviceIdentity,
    kind: SignedRequest,
    unix_seconds: i64,
) -> Result<(), SignError> {
    // The header value is the one copy that must outlive this call.
    let client_id = Zeroizing::new(signer.client_id()?);
    let payload = match kind {
        SignedRequest::LicenceActivate => client_id.as_str(),
        SignedRequest::UpdateCheck | SignedRequest::Pair => "",
    };
    let message = Zeroizing::new(signing_message(
        device,
        kind.firmware(device),
        unix_seconds,
        payload,
    ));
    http.headers.extend([
        (SIGNATURE.to_owned(), signer.sign(&message)?),
        (DATE.to_owned(), unix_seconds.to_string()),
        (SERIAL.to_owned(), device.serial.clone()),
        (CLIENT_ID.to_owned(), client_id.as_str().to_owned()),
    ]);
    let order = kind.header_order();
    http.headers.sort_by_key(|(name, _)| {
        order
            .iter()
            .position(|listed| listed.eq_ignore_ascii_case(name))
            .map_or(0, |at| at + 1)
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::http::{Method, Route};

    fn device(firmware: &str) -> DeviceIdentity {
        DeviceIdentity {
            client_type: ClientType::QuadCortex,
            firmware: firmware.into(),
            serial: "Q0000TEST".into(),
        }
    }

    #[test]
    fn message_template_and_firmware_forms() {
        let beta = device("4.1.0-beta-7");
        assert_eq!(beta.firmware_prefix(), "4.1.0");
        assert_eq!(device("4.1.0").firmware_prefix(), "4.1.0");
        assert_eq!(
            signing_message(&beta, &beta.firmware, 1_700_000_000, ""),
            "quad_cortex-Q0000TEST-with-firmware-4.1.0-beta-7-at-1700000000"
        );
        assert_eq!(
            signing_message(&beta, beta.firmware_prefix(), 5, "cGF5"),
            "quad_cortex-Q0000TEST-with-firmware-4.1.0-at-5-cGF5"
        );
    }

    /// Message, firmware form, payload and header order per request, with a real
    /// [`signing::SharcSigner`] over made-up bytes.
    #[test]
    fn signed_requests_match_the_recorded_shapes() {
        let beta = device("4.1.0-beta-7");
        let request = || {
            HttpRequest::new(Method::Post, "u", Route::Product, Duration::ZERO)
                .header("X-Cloud-Api-Client-Type", "quad_cortex")
                .header("X-Cloud-Api-Client-Version", "4.1.0-beta-7")
                .header("Content-Type", "application/json")
                .header("Authorization", "Bearer t")
        };
        let mut signer = signing::tests::signer();
        let client_id = signer.client_id().unwrap();
        let names = |http: &HttpRequest| -> Vec<String> {
            http.headers.iter().map(|(name, _)| name.clone()).collect()
        };
        let cases = [
            (
                SignedRequest::UpdateCheck,
                signing_message(&beta, "4.1.0-beta-7", 9, ""),
                vec![
                    SIGNATURE,
                    "X-Cloud-Api-Client-Type",
                    "X-Cloud-Api-Client-Version",
                    SERIAL,
                    DATE,
                    "Authorization",
                    "Content-Type",
                    CLIENT_ID,
                ],
            ),
            (
                SignedRequest::Pair,
                signing_message(&beta, "4.1.0", 9, ""),
                vec![
                    "X-Cloud-Api-Client-Type",
                    "X-Cloud-Api-Client-Version",
                    "Content-Type",
                    "Authorization",
                    DATE,
                    SIGNATURE,
                    SERIAL,
                    CLIENT_ID,
                ],
            ),
            (
                SignedRequest::LicenceActivate,
                signing_message(&beta, "4.1.0", 9, &client_id),
                vec![
                    "X-Cloud-Api-Client-Type",
                    "X-Cloud-Api-Client-Version",
                    "Content-Type",
                    "Authorization",
                    SERIAL,
                    CLIENT_ID,
                    SIGNATURE,
                    DATE,
                ],
            ),
        ];
        for (kind, message, order) in cases {
            let mut http = request();
            sign_request(&mut http, &mut signer, &beta, kind, 9).unwrap();
            assert_eq!(names(&http), order, "{kind:?}");
            assert_eq!(
                http.header_value(SIGNATURE),
                Some(signer.material.sign(&message).as_str()),
                "{kind:?}"
            );
            assert_eq!(http.header_value(DATE), Some("9"));
            assert_eq!(http.header_value(SERIAL), Some("Q0000TEST"));
            assert_eq!(http.header_value(CLIENT_ID), Some(client_id.as_str()));
        }
    }
}
