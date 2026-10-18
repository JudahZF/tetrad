//! Device-bound factory IR decoding. Key material is always injected.
//!
//! Stock files under [`FACTORY_IR_DIR`] are 4,096-byte NDSP-encrypted images of
//! 1,024 LE float32 words. The updater translates them for each device with
//! `zc_gtl` and `QC_SERIAL_NUMBER=/media/p4/qc_sn` (`sysupdate_run.sh`
//! "translate factory IRs"), so their key is the firmware seed plus the device
//! serial. The seed alone and caller secrets key other stock data; see
//! [`DeviceCipher`]. All modes share `qc_format::crypto::NdspCipher`.

use std::fmt;
use std::sync::Arc;

use qc_format::crypto::NdspCipher;
use zeroize::{Zeroize, Zeroizing};

use crate::ir::{IrProvenance, IrSamples, MAX_IR_SAMPLES, select_samples};

pub const FACTORY_IR_DIR: &str = "/opt/neuraldsp/impulse_responses";
pub const FACTORY_IR_BYTES: usize = MAX_IR_SAMPLES * 4;
pub const SEED_LEN: usize = 57;
pub const SERIAL_LEN: usize = 9;
/// Serial file the updater passes to `zc_gtl`.
pub const SERIAL_PATH: &str = "/media/p4/qc_sn";
/// Owned plausibility bound. CTR has no MAC, so a wrong seed or serial gives
/// noise words; refusing them protects the DSP from loud or nonfinite output.
pub const MAX_PLAUSIBLE_MAGNITUDE: f32 = 8.0;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FactoryIrError {
    #[error("factory IR seed must be {SEED_LEN} bytes, got {0}")]
    SeedLength(usize),
    #[error("ZenUI image has {0} bytes; too short for the seed offset")]
    ZenUiTooShort(usize),
    #[error("device serial must be {SERIAL_LEN} ASCII letters or digits")]
    Serial,
    #[error("factory IR must be {FACTORY_IR_BYTES} bytes, got {0}")]
    PayloadSize(usize),
    #[error(
        "decrypted factory IR word {index} is {value}; wrong seed or serial, or not a factory IR"
    )]
    Implausible { index: usize, value: f32 },
}

/// The global firmware seed. Debug output never shows its bytes; the bytes are zeroed on
/// drop and there is no `Clone`: derive a [`DeviceCipher`] and share that (review r18 F15).
pub struct FactoryIrSeed(Zeroizing<[u8; SEED_LEN]>);

impl FactoryIrSeed {
    pub fn new(bytes: &[u8]) -> Result<Self, FactoryIrError> {
        let mut seed = Zeroizing::new([0_u8; SEED_LEN]);
        if bytes.len() != SEED_LEN {
            return Err(FactoryIrError::SeedLength(bytes.len()));
        }
        seed.copy_from_slice(bytes);
        Ok(Self(seed))
    }

    /// Takes the seed from the device's own ZenUI image, at the offset of the running build
    /// (`MediaOffsets::cipher_seed`). This does not verify the build; a wrong image fails
    /// later as `Implausible`.
    pub fn from_stock_zenui(image: &[u8]) -> Result<Self, FactoryIrError> {
        let offset = tetrad_firmware::running().zenui.media.cipher_seed;
        image
            .get(offset..offset + SEED_LEN)
            .ok_or(FactoryIrError::ZenUiTooShort(image.len()))
            .and_then(Self::new)
    }
}

impl fmt::Debug for FactoryIrSeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FactoryIrSeed(<redacted>)")
    }
}

/// Device serial as stored in `qc_sn`. Debug output is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceSerial([u8; SERIAL_LEN]);

impl DeviceSerial {
    /// Accepts the `qc_sn` file contents; surrounding ASCII whitespace is ignored.
    pub fn parse(contents: &[u8]) -> Result<Self, FactoryIrError> {
        let serial: [u8; SERIAL_LEN] = contents
            .trim_ascii()
            .try_into()
            .map_err(|_| FactoryIrError::Serial)?;
        if serial.iter().all(u8::is_ascii_alphanumeric) {
            Ok(Self(serial))
        } else {
            Err(FactoryIrError::Serial)
        }
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("validated ASCII")
    }
}

impl fmt::Debug for DeviceSerial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceSerial(<redacted>)")
    }
}

/// AES-128-CTR state for one NDSP secret. Build it with the constructor that
/// matches the data: [`Self::seed`] for user captures, [`Self::seed_and_serial`]
/// for stock factory IRs, [`Self::caller_secret`] for blobs and ModelRepo.
///
/// Clones share one key behind an `Arc`; the key is zeroed when the last clone drops
/// (`NdspCipher`'s `Drop`).
#[derive(Clone, Debug)]
pub struct DeviceCipher(Arc<NdspCipher>);

/// A cipher derived elsewhere (the kernel cipher of the same seed and serial) becomes the
/// shared one.
impl From<NdspCipher> for DeviceCipher {
    fn from(cipher: NdspCipher) -> Self {
        Self(Arc::new(cipher))
    }
}

impl DeviceCipher {
    /// Secret mode 0: the firmware seed alone.
    pub fn seed(seed: &FactoryIrSeed) -> Self {
        NdspCipher::derive(&*seed.0).into()
    }

    /// Secret mode 1: the firmware seed followed by the device serial. The joined secret is
    /// zeroed here.
    pub fn seed_and_serial(seed: &FactoryIrSeed, serial: &DeviceSerial) -> Self {
        let mut secret = [0_u8; SEED_LEN + SERIAL_LEN];
        secret[..SEED_LEN].copy_from_slice(&*seed.0);
        secret[SEED_LEN..].copy_from_slice(&serial.0);
        let cipher = NdspCipher::derive(&secret);
        secret.zeroize();
        cipher.into()
    }

    /// Secret mode 2: a caller-owned secret, such as a blob or ModelRepo key.
    pub fn caller_secret(secret: &[u8]) -> Self {
        NdspCipher::derive(secret).into()
    }

    /// Encrypts or decrypts in place. Every file restarts at the derived counter.
    pub fn apply(&self, data: &mut [u8]) {
        self.0.apply_keystream(data);
    }

    /// Decrypts one stock factory IR file into 1,024 words. Use a cipher from
    /// [`Self::seed_and_serial`]. The magnitude check is owned policy (see
    /// [`MAX_PLAUSIBLE_MAGNITUDE`]), not a stock validator.
    pub fn decode_factory_ir(&self, bytes: &[u8]) -> Result<IrSamples, FactoryIrError> {
        if bytes.len() != FACTORY_IR_BYTES {
            return Err(FactoryIrError::PayloadSize(bytes.len()));
        }
        let mut plain = bytes.to_vec();
        self.apply(&mut plain);
        let samples = select_samples(&plain, IrProvenance::FactoryBin);
        if let Some((index, value)) = samples
            .samples()
            .enumerate()
            .find(|(_, value)| value.is_nan() || value.abs() > MAX_PLAUSIBLE_MAGNITUDE)
        {
            return Err(FactoryIrError::Implausible { index, value });
        }
        Ok(samples)
    }
}
