//! Pure intentions for the statically proved integer PGA subset. No syscalls.
use crate::requests::{IoctlPayload, IoctlRequest};
use std::{fmt, num::NonZeroU32};

/// These are stock `_hwType` values, not universal board revision numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareType {
    Type0,
    Type1,
    Type2,
}

impl TryFrom<i32> for HardwareType {
    type Error = AnalogError;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Type0),
            1 => Ok(Self::Type1),
            2 => Ok(Self::Type2),
            _ => Err(AnalogError::UnsupportedHardwareType(value)),
        }
    }
}

impl HardwareType {
    /// Only the two direct board-revision branches are supported. Other values
    /// require product detection and cannot select a default here.
    pub fn from_board_revision(value: i32) -> Result<Self, AnalogError> {
        match value {
            2 => Ok(Self::Type1),
            3 => Ok(Self::Type2),
            _ => Err(AnalogError::UnsupportedBoardRevision(value)),
        }
    }

    pub fn maximum_gain(self) -> i32 {
        match self {
            Self::Type0 => 31,
            Self::Type1 | Self::Type2 => 10,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GainEndpoint {
    Input1,
    Input2,
    Return1,
    Return2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceEndpoint {
    Soundcard,
    Codec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GainRead {
    pub device: DeviceEndpoint,
    pub number: u32,
    /// An output pointer to exactly four bytes, not NULL or an input payload.
    pub output_length: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GainWrite {
    pub device: DeviceEndpoint,
    pub request: IoctlRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalogError {
    UnsupportedHardwareType(i32),
    UnsupportedBoardRevision(i32),
    InvalidGain,
    InvalidPolicy,
    WrongOperation,
    ObservationFailed(i32),
    /// Stock can compare uninitialized/stale storage after an operation fails.
    UnsupportedVerificationFailure(i32),
}

impl fmt::Display for AnalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "analog contract error: {self:?}")
    }
}
impl std::error::Error for AnalogError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    Unverified,
    ReadBack {
        attempts: NonZeroU32,
        sleep_microseconds: Option<NonZeroU32>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GainOperation {
    Write(GainWrite),
    Read(GainRead),
    SleepMicroseconds(NonZeroU32),
    Finished { success: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Write,
    SleepBeforeRead,
    Read,
    SleepBeforeCompare,
    Finished(bool),
}

/// One explicitly selected target. No presumed cache initialization or open
/// state. A fresh observation can update the cache; a cache read cannot observe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GainControl {
    hardware: HardwareType,
    endpoint: GainEndpoint,
    cached: Option<i32>,
    observed: Option<i32>,
}

impl GainControl {
    pub fn new(hardware: HardwareType, endpoint: GainEndpoint) -> Self {
        Self {
            hardware,
            endpoint,
            cached: None,
            observed: None,
        }
    }

    pub fn cached(&self) -> Option<i32> {
        self.cached
    }

    pub fn last_observed(&self) -> Option<i32> {
        self.observed
    }

    fn numbers(&self) -> (u32, u32) {
        let low = match (self.hardware, self.endpoint) {
            (HardwareType::Type0, GainEndpoint::Input1) => (0xe000, 0xe012),
            (HardwareType::Type0, GainEndpoint::Input2) => (0xe003, 0xe015),
            (HardwareType::Type0, GainEndpoint::Return1) => (0xe009, 0xe01b),
            (HardwareType::Type0, GainEndpoint::Return2) => (0xe00a, 0xe01c),
            (_, GainEndpoint::Input1) => (0xe202, 0xe20a),
            (_, GainEndpoint::Input2) => (0xe203, 0xe20b),
            (HardwareType::Type1, GainEndpoint::Return1)
            | (HardwareType::Type2, GainEndpoint::Return2) => (0xe205, 0xe20d),
            (HardwareType::Type1, GainEndpoint::Return2)
            | (HardwareType::Type2, GainEndpoint::Return1) => (0xe204, 0xe20c),
        };
        (0x4004_0000 | low.0, 0x4004_0000 | low.1)
    }

    fn device(&self) -> DeviceEndpoint {
        match self.hardware {
            HardwareType::Type0 => DeviceEndpoint::Soundcard,
            HardwareType::Type1 | HardwareType::Type2 => DeviceEndpoint::Codec,
        }
    }

    /// Gain is an integer PGA code. No dB conversion or rounding is inferred.
    pub fn write_request(&self, value: i32) -> Result<GainWrite, AnalogError> {
        if !(0..=self.hardware.maximum_gain()).contains(&value) {
            return Err(AnalogError::InvalidGain);
        }
        Ok(GainWrite {
            device: self.device(),
            request: IoctlRequest {
                number: self.numbers().0,
                payload: IoctlPayload::Integer(value.to_le_bytes()),
            },
        })
    }

    /// Strict numeric boundary for callers with floats; fractional/nonfinite
    /// values are rejected instead of being truncated to a stock integer code.
    pub fn numeric_write_request(&self, value: f64) -> Result<GainWrite, AnalogError> {
        if !value.is_finite()
            || value.fract() != 0.0
            || value < 0.0
            || value > f64::from(self.hardware.maximum_gain())
        {
            return Err(AnalogError::InvalidGain);
        }
        self.write_request(value as i32)
    }

    pub fn observed_request(&self) -> GainRead {
        GainRead {
            device: self.device(),
            number: self.numbers().1,
            output_length: 4,
        }
    }

    /// `GetInputGain`/`GetRetGain` observed mode caches the output only on
    /// nonnegative ioctl return. Output values are not range checked by stock.
    pub fn complete_observation(
        &mut self,
        raw_return: i32,
        bytes: [u8; 4],
    ) -> Result<i32, AnalogError> {
        if raw_return < 0 {
            return Err(AnalogError::ObservationFailed(raw_return));
        }
        let value = i32::from_le_bytes(bytes);
        self.observed = Some(value);
        self.cached = Some(value);
        Ok(value)
    }

    pub fn begin_set(
        &mut self,
        value: i32,
        verification: Verification,
    ) -> Result<GainSet<'_>, AnalogError> {
        let write = self.write_request(value)?;
        if let Verification::ReadBack {
            attempts,
            sleep_microseconds,
        } = verification
            && (attempts.get() > i32::MAX as u32
                || sleep_microseconds.is_some_and(|v| v.get() > i32::MAX as u32))
        {
            return Err(AnalogError::InvalidPolicy);
        }
        Ok(GainSet {
            control: self,
            value,
            write,
            verification,
            stage: Stage::Write,
            attempts: 0,
            read_value: None,
        })
    }
}

/// Caller supplies completions; this state machine performs no operation.
/// Dropping it retains proved observations but does not cache an unfinished set.
pub struct GainSet<'a> {
    control: &'a mut GainControl,
    value: i32,
    write: GainWrite,
    verification: Verification,
    stage: Stage,
    attempts: u32,
    read_value: Option<i32>,
}

impl GainSet<'_> {
    pub fn operation(&self) -> GainOperation {
        match self.stage {
            Stage::Write => GainOperation::Write(self.write),
            Stage::Read => GainOperation::Read(self.control.observed_request()),
            Stage::SleepBeforeRead | Stage::SleepBeforeCompare => {
                let Verification::ReadBack {
                    sleep_microseconds: Some(duration),
                    ..
                } = self.verification
                else {
                    unreachable!()
                };
                GainOperation::SleepMicroseconds(duration)
            }
            Stage::Finished(success) => GainOperation::Finished { success },
        }
    }

    pub fn complete_write(&mut self, raw_return: i32) -> Result<(), AnalogError> {
        if self.stage != Stage::Write {
            return Err(AnalogError::WrongOperation);
        }
        self.attempts += 1;
        match self.verification {
            Verification::Unverified => {
                // Stock updates the cache even when SetGain returns false.
                self.control.cached = Some(self.value);
                self.stage = Stage::Finished(raw_return >= 0);
            }
            Verification::ReadBack {
                sleep_microseconds, ..
            } => {
                if raw_return < 0 {
                    self.stage = Stage::Finished(false);
                    return Err(AnalogError::UnsupportedVerificationFailure(raw_return));
                }
                self.stage = if sleep_microseconds.is_some() {
                    Stage::SleepBeforeRead
                } else {
                    Stage::Read
                };
            }
        }
        Ok(())
    }

    /// Stock ignores usleep's return, including interruption. No implicit
    /// remaining-duration sleep is added.
    pub fn complete_sleep(&mut self, _raw_return: i32) -> Result<(), AnalogError> {
        match self.stage {
            Stage::SleepBeforeRead => self.stage = Stage::Read,
            Stage::SleepBeforeCompare => self.compare(),
            _ => return Err(AnalogError::WrongOperation),
        }
        Ok(())
    }

    pub fn complete_read(&mut self, raw_return: i32, bytes: [u8; 4]) -> Result<(), AnalogError> {
        if self.stage != Stage::Read {
            return Err(AnalogError::WrongOperation);
        }
        if raw_return < 0 {
            self.stage = Stage::Finished(false);
            return Err(AnalogError::UnsupportedVerificationFailure(raw_return));
        }
        let value = i32::from_le_bytes(bytes);
        self.control.observed = Some(value);
        self.read_value = Some(value);
        let Verification::ReadBack {
            sleep_microseconds, ..
        } = self.verification
        else {
            unreachable!()
        };
        if sleep_microseconds.is_some() {
            self.stage = Stage::SleepBeforeCompare;
        } else {
            self.compare();
        }
        Ok(())
    }

    fn compare(&mut self) {
        let Verification::ReadBack { attempts, .. } = self.verification else {
            unreachable!()
        };
        if self.read_value == Some(self.value) {
            self.control.cached = Some(self.value);
            self.stage = Stage::Finished(true);
        } else if self.attempts >= attempts.get() {
            self.stage = Stage::Finished(false);
        } else {
            self.stage = Stage::Write;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verified(attempts: u32, sleep: Option<u32>) -> Verification {
        Verification::ReadBack {
            attempts: NonZeroU32::new(attempts).unwrap(),
            sleep_microseconds: sleep.map(|v| NonZeroU32::new(v).unwrap()),
        }
    }

    #[test]
    fn explicit_revisions_and_all_endpoint_pairs() {
        assert_eq!(
            HardwareType::from_board_revision(2),
            Ok(HardwareType::Type1)
        );
        assert_eq!(
            HardwareType::from_board_revision(3),
            Ok(HardwareType::Type2)
        );
        for value in [-1, 0, 1, 4, i32::MAX] {
            assert_eq!(
                HardwareType::from_board_revision(value),
                Err(AnalogError::UnsupportedBoardRevision(value))
            );
        }
        assert_eq!(
            HardwareType::try_from(3),
            Err(AnalogError::UnsupportedHardwareType(3))
        );
        let endpoints = [
            GainEndpoint::Input1,
            GainEndpoint::Input2,
            GainEndpoint::Return1,
            GainEndpoint::Return2,
        ];
        for (hardware, device, pairs) in [
            (
                HardwareType::Type0,
                DeviceEndpoint::Soundcard,
                [
                    (0xe000, 0xe012),
                    (0xe003, 0xe015),
                    (0xe009, 0xe01b),
                    (0xe00a, 0xe01c),
                ],
            ),
            (
                HardwareType::Type1,
                DeviceEndpoint::Codec,
                [
                    (0xe202, 0xe20a),
                    (0xe203, 0xe20b),
                    (0xe205, 0xe20d),
                    (0xe204, 0xe20c),
                ],
            ),
            (
                HardwareType::Type2,
                DeviceEndpoint::Codec,
                [
                    (0xe202, 0xe20a),
                    (0xe203, 0xe20b),
                    (0xe204, 0xe20c),
                    (0xe205, 0xe20d),
                ],
            ),
        ] {
            for (endpoint, (write, read)) in endpoints.into_iter().zip(pairs) {
                let control = GainControl::new(hardware, endpoint);
                let request = control.write_request(7).unwrap();
                assert_eq!(request.device, device);
                assert_eq!(request.request.number, 0x4004_0000 | write);
                assert_eq!(
                    request.request.payload.bytes(),
                    Some([7, 0, 0, 0].as_slice())
                );
                assert_eq!(
                    control.observed_request(),
                    GainRead {
                        device,
                        number: 0x4004_0000 | read,
                        output_length: 4
                    }
                );
            }
        }
    }

    #[test]
    fn integer_boundaries_and_strict_float_boundary() {
        for hardware in [
            HardwareType::Type0,
            HardwareType::Type1,
            HardwareType::Type2,
        ] {
            let control = GainControl::new(hardware, GainEndpoint::Input1);
            let maximum = hardware.maximum_gain();
            for value in [0, maximum] {
                assert_eq!(
                    control
                        .write_request(value)
                        .unwrap()
                        .request
                        .payload
                        .bytes(),
                    Some(value.to_le_bytes().as_slice())
                );
                assert!(control.numeric_write_request(f64::from(value)).is_ok());
            }
            for value in [-1, maximum + 1, i32::MIN, i32::MAX] {
                assert_eq!(control.write_request(value), Err(AnalogError::InvalidGain));
            }
            for value in [
                f64::NAN,
                f64::INFINITY,
                f64::NEG_INFINITY,
                0.5,
                -0.5,
                f64::MAX,
            ] {
                assert_eq!(
                    control.numeric_write_request(value),
                    Err(AnalogError::InvalidGain)
                );
            }
            assert_eq!(
                control.write_request(11).is_ok(),
                hardware == HardwareType::Type0
            );
        }
    }

    #[test]
    fn cache_and_observations_have_distinct_updates_and_error_policy() {
        let mut control = GainControl::new(HardwareType::Type2, GainEndpoint::Return1);
        assert_eq!(control.cached(), None);
        assert_eq!(control.last_observed(), None);
        assert_eq!(
            control.complete_observation(1, 123_i32.to_le_bytes()),
            Ok(123)
        );
        assert_eq!(
            control.complete_observation(-1, 99_i32.to_le_bytes()),
            Err(AnalogError::ObservationFailed(-1))
        );
        assert_eq!(control.cached(), Some(123));
        assert_eq!(control.last_observed(), Some(123));
        {
            let mut set = control.begin_set(5, Verification::Unverified).unwrap();
            set.complete_write(-1).unwrap();
            assert_eq!(set.operation(), GainOperation::Finished { success: false });
        }
        assert_eq!(control.cached(), Some(5));
        assert_eq!(control.last_observed(), Some(123));
    }

    #[test]
    fn retry_sleeps_precede_read_and_comparison_and_ignore_sleep_errors() {
        let mut control = GainControl::new(HardwareType::Type1, GainEndpoint::Input2);
        control
            .complete_observation(0, 2_i32.to_le_bytes())
            .unwrap();
        {
            let mut set = control.begin_set(7, verified(2, Some(50))).unwrap();
            for observed in [3_i32, 7] {
                assert!(matches!(set.operation(), GainOperation::Write(_)));
                assert_eq!(
                    set.complete_read(0, observed.to_le_bytes()),
                    Err(AnalogError::WrongOperation)
                );
                set.complete_write(0).unwrap();
                assert_eq!(
                    set.operation(),
                    GainOperation::SleepMicroseconds(NonZeroU32::new(50).unwrap())
                );
                set.complete_sleep(-1).unwrap();
                assert!(matches!(set.operation(), GainOperation::Read(_)));
                set.complete_read(9, observed.to_le_bytes()).unwrap();
                assert_eq!(set.control.cached(), Some(2));
                assert_eq!(set.control.last_observed(), Some(observed));
                assert!(matches!(
                    set.operation(),
                    GainOperation::SleepMicroseconds(_)
                ));
                set.complete_sleep(0).unwrap();
            }
            assert_eq!(set.operation(), GainOperation::Finished { success: true });
            assert_eq!(set.complete_write(0), Err(AnalogError::WrongOperation));
        }
        assert_eq!(control.cached(), Some(7));
    }

    #[test]
    fn mismatch_exhaustion_is_bounded_without_sleep_and_preserves_cache() {
        let mut control = GainControl::new(HardwareType::Type0, GainEndpoint::Input1);
        control
            .complete_observation(0, 4_i32.to_le_bytes())
            .unwrap();
        {
            let mut set = control.begin_set(31, verified(2, None)).unwrap();
            for _ in 0..2 {
                set.complete_write(0).unwrap();
                assert!(matches!(set.operation(), GainOperation::Read(_)));
                set.complete_read(0, 30_i32.to_le_bytes()).unwrap();
            }
            assert_eq!(set.operation(), GainOperation::Finished { success: false });
        }
        assert_eq!(control.cached(), Some(4));
        assert_eq!(control.last_observed(), Some(30));
    }

    #[test]
    fn failed_verification_is_unsupported_and_invalid_policy_is_rejected() {
        let mut control = GainControl::new(HardwareType::Type2, GainEndpoint::Input1);
        for policy in [verified(u32::MAX, None), verified(1, Some(u32::MAX))] {
            assert!(matches!(
                control.begin_set(0, policy),
                Err(AnalogError::InvalidPolicy)
            ));
        }
        for fail_write in [true, false] {
            let mut set = control.begin_set(0, verified(1, None)).unwrap();
            let error = if fail_write {
                set.complete_write(-1)
            } else {
                set.complete_write(0).unwrap();
                set.complete_read(-7, 0_i32.to_le_bytes())
            };
            assert_eq!(
                error,
                Err(AnalogError::UnsupportedVerificationFailure(if fail_write {
                    -1
                } else {
                    -7
                }))
            );
            assert_eq!(set.operation(), GainOperation::Finished { success: false });
        }
        assert_eq!(control.cached(), None);
        assert_eq!(control.last_observed(), None);
    }
}
