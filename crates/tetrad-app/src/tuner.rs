//! Owned monophonic tuner: pitch detection and note/cents conversion.
//!
//! The stock QC runs pitch detection on the DSP and ZenUI only shows meter values, so the detector
//! here is an owned algorithm, not a reconstruction. It is the McLeod Pitch Method: normalized
//! square difference function (NSDF), key-maximum peak picking and parabolic interpolation. Only
//! the reference-pitch bounds come from the stock binary.

use std::fmt;

use thiserror::Error;

/// Lowest reference (A4) frequency ZenUI allows. `MIN_TUNER_DETUNE_FREQ`, VA 0x0009932c.
pub const MIN_REFERENCE_HZ: f32 = 400.0;
/// Highest reference (A4) frequency ZenUI allows. `MAX_TUNER_DETUNE_FREQ`, VA 0x0009932c.
pub const MAX_REFERENCE_HZ: f32 = 480.0;
/// Concert pitch. Stock default not recovered; 440 Hz is the conventional value.
pub const DEFAULT_REFERENCE_HZ: f32 = 440.0;

/// Upper bound on frame length, so one detection costs at most `MAX_FRAME_LEN * max_lag` steps.
pub const MAX_FRAME_LEN: usize = 16_384;
const SAMPLE_RATES: std::ops::RangeInclusive<u32> = 8_000..=192_000;
/// Frames with RMS below this (about -60 dBFS) are reported as silent.
const SILENCE_RMS: f64 = 1e-3;
/// A key maximum must reach this fraction of the highest one to be chosen (McLeod's `k`).
const PEAK_RATIO: f64 = 0.9;
/// Minimum NSDF value at the chosen peak; below this the frame is not periodic enough.
const MIN_CLARITY: f64 = 0.8;

#[derive(Debug, Clone, Copy, PartialEq, Error)]
pub enum TunerError {
    #[error("sample rate {0} Hz is outside 8000..=192000")]
    SampleRate(u32),
    #[error("detection range {min_hz}..={max_hz} Hz is invalid for sample rate {sample_rate} Hz")]
    Range {
        min_hz: f32,
        max_hz: f32,
        sample_rate: u32,
    },
    #[error("reference {0} Hz is outside {MIN_REFERENCE_HZ}..={MAX_REFERENCE_HZ}")]
    Reference(f32),
    #[error("frequency {0} Hz is not finite and positive")]
    Frequency(f32),
    #[error("frame has {len} samples; expected {min}..={MAX_FRAME_LEN}")]
    FrameLength { len: usize, min: usize },
    #[error("sample {0} is not finite")]
    NonFiniteSample(usize),
}

/// Detector settings. `Default` is 48 kHz (the QC rate) and 25..=2000 Hz, which covers a
/// detuned 5-string bass low B up to high guitar harmonics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TunerConfig {
    pub sample_rate: u32,
    pub min_hz: f32,
    pub max_hz: f32,
}

impl Default for TunerConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            min_hz: 25.0,
            max_hz: 2_000.0,
        }
    }
}

/// Result of analysing one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Detection {
    /// Signal level is below the silence threshold.
    Silent,
    /// Signal is present but the detector's estimate is unclear or outside the detection range.
    Unpitched,
    Pitched(Pitch),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pitch {
    /// Selected frequency estimate in Hz, always inside the configured range.
    /// Weak fundamentals can cause the detector to select a harmonic.
    pub hz: f32,
    /// NSDF value at the chosen peak, in `MIN_CLARITY..=1`. Higher is more periodic.
    pub clarity: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tuner {
    config: TunerConfig,
    /// Largest lag examined: one past the period of `min_hz`, so the peak can be interpolated.
    max_lag: usize,
}

impl Tuner {
    pub fn new(config: TunerConfig) -> Result<Self, TunerError> {
        let TunerConfig {
            sample_rate,
            min_hz,
            max_hz,
        } = config;
        if !SAMPLE_RATES.contains(&sample_rate) {
            return Err(TunerError::SampleRate(sample_rate));
        }
        let range_error = TunerError::Range {
            min_hz,
            max_hz,
            sample_rate,
        };
        // Above a quarter of the sample rate (under 4 samples per period) parabolic interpolation
        // stops being accurate.
        if !(min_hz > 0.0 && min_hz < max_hz && max_hz <= sample_rate as f32 / 4.0) {
            return Err(range_error);
        }
        let max_lag = (f64::from(sample_rate) / f64::from(min_hz)).ceil() + 1.0;
        if 2.0 * max_lag > MAX_FRAME_LEN as f64 {
            return Err(range_error);
        }
        Ok(Self {
            config,
            max_lag: max_lag as usize,
        })
    }

    pub fn config(&self) -> TunerConfig {
        self.config
    }

    /// Shortest frame `detect` accepts: two periods of the lowest detectable pitch.
    pub fn min_frame_len(&self) -> usize {
        2 * self.max_lag
    }

    /// Detects the fundamental of one mono frame of samples in `-1.0..=1.0` scale.
    pub fn detect(&self, frame: &[f32]) -> Result<Detection, TunerError> {
        let min = self.min_frame_len();
        if !(min..=MAX_FRAME_LEN).contains(&frame.len()) {
            return Err(TunerError::FrameLength {
                len: frame.len(),
                min,
            });
        }
        if let Some(index) = frame.iter().position(|sample| !sample.is_finite()) {
            return Err(TunerError::NonFiniteSample(index));
        }

        let mean = frame.iter().map(|&s| f64::from(s)).sum::<f64>() / frame.len() as f64;
        let x: Vec<f64> = frame.iter().map(|&s| f64::from(s) - mean).collect();
        let energy: f64 = x.iter().map(|s| s * s).sum();
        if (energy / x.len() as f64).sqrt() < SILENCE_RMS {
            return Ok(Detection::Silent);
        }

        let nsdf = nsdf(&x, self.max_lag);
        let Some((lag, clarity)) = pick_peak(&nsdf) else {
            return Ok(Detection::Unpitched);
        };
        let hz = (f64::from(self.config.sample_rate) / lag) as f32;
        // `max_hz` filters the estimate and never steers peak selection: preferring an in-range key
        // maximum folds tones above the range onto a subharmonic. See "Range policy" in tuner.md.
        if clarity < MIN_CLARITY || !(self.config.min_hz..=self.config.max_hz).contains(&hz) {
            return Ok(Detection::Unpitched);
        }
        Ok(Detection::Pitched(Pitch {
            hz,
            clarity: clarity.min(1.0) as f32,
        }))
    }
}

/// `nsdf[t] = 2 * sum(x[j] * x[j + t]) / sum(x[j]^2 + x[j + t]^2)` for `t` in `0..=max_lag`.
fn nsdf(x: &[f64], max_lag: usize) -> Vec<f64> {
    (0..=max_lag)
        .map(|lag| {
            let (head, tail) = (&x[..x.len() - lag], &x[lag..]);
            let acf: f64 = head.iter().zip(tail).map(|(a, b)| a * b).sum();
            let norm: f64 = head.iter().zip(tail).map(|(a, b)| a * a + b * b).sum();
            if norm > 0.0 { 2.0 * acf / norm } else { 0.0 }
        })
        .collect()
}

/// McLeod key-maximum selection. Skips the lobe around lag 0, takes the highest NSDF value in each
/// positive region, then returns the first one that reaches `PEAK_RATIO` of the overall highest.
/// Returns the interpolated lag (in samples) and the interpolated NSDF value there.
fn pick_peak(nsdf: &[f64]) -> Option<(f64, f64)> {
    let first_negative = nsdf.iter().position(|&v| v < 0.0)?;
    let mut key_maxima: Vec<usize> = Vec::new();
    let mut current: Option<usize> = None;
    for lag in first_negative..nsdf.len() - 1 {
        if nsdf[lag] > 0.0 {
            if current.is_none_or(|best| nsdf[lag] > nsdf[best]) {
                current = Some(lag);
            }
        } else if let Some(best) = current.take() {
            key_maxima.push(best);
        }
    }
    // A region still open at the last lag is kept only if its peak is already behind us;
    // otherwise the true maximum may lie past `max_lag`.
    if let Some(best) = current.filter(|&best| nsdf[best] > nsdf[best + 1]) {
        key_maxima.push(best);
    }

    let highest = key_maxima.iter().map(|&lag| nsdf[lag]).fold(0.0, f64::max);
    let lag = *key_maxima
        .iter()
        .find(|&&lag| nsdf[lag] >= PEAK_RATIO * highest)?;
    let (a, b, c) = (nsdf[lag - 1], nsdf[lag], nsdf[lag + 1]);
    let curvature = a - 2.0 * b + c;
    if curvature >= 0.0 {
        return Some((lag as f64, b));
    }
    let shift = 0.5 * (a - c) / curvature;
    Some((lag as f64 + shift, b - 0.25 * (a - c) * shift))
}

/// Equal-tempered reference pitch for A4, bounded like the stock setting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReferencePitch(f32);

impl Default for ReferencePitch {
    fn default() -> Self {
        Self(DEFAULT_REFERENCE_HZ)
    }
}

impl ReferencePitch {
    pub fn new(hz: f32) -> Result<Self, TunerError> {
        if (MIN_REFERENCE_HZ..=MAX_REFERENCE_HZ).contains(&hz) {
            Ok(Self(hz))
        } else {
            Err(TunerError::Reference(hz))
        }
    }

    pub fn hz(self) -> f32 {
        self.0
    }

    /// Nearest equal-tempered note to `hz` and the offset from it in cents (`-50.0..=50.0`).
    pub fn reading(self, hz: f32) -> Result<NoteReading, TunerError> {
        if !hz.is_finite() || hz <= 0.0 {
            return Err(TunerError::Frequency(hz));
        }
        let semitones = 69.0 + 12.0 * (f64::from(hz) / f64::from(self.0)).log2();
        let nearest = semitones.round();
        Ok(NoteReading {
            note: Note {
                midi: nearest as i32,
            },
            cents: ((semitones - nearest) * 100.0) as f32,
        })
    }

    /// Exact frequency of `note` under this reference. Notes far outside the audible range
    /// saturate to `0.0` or `f32::INFINITY`.
    pub fn frequency(self, note: Note) -> f32 {
        (f64::from(self.0) * 2f64.powf((f64::from(note.midi) - 69.0) / 12.0)) as f32
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoteReading {
    pub note: Note,
    /// Positive when sharp, negative when flat.
    pub cents: f32,
}

/// Equal-tempered note by MIDI number (69 = A4). Not limited to 0..=127 so that any positive
/// frequency has a note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Note {
    pub midi: i32,
}

impl Note {
    /// Sharp-only names; the stock tuner's spelling (sharps or flats) is not recovered.
    const NAMES: [&'static str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];

    pub fn name(self) -> &'static str {
        Self::NAMES[self.midi.rem_euclid(12) as usize]
    }

    /// Scientific pitch notation octave (MIDI 60 = C4).
    pub fn octave(self) -> i32 {
        self.midi.div_euclid(12) - 1
    }
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.name(), self.octave())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const RATE: u32 = 48_000;
    const FRAME: usize = 4_096;

    fn tuner() -> Tuner {
        Tuner::new(TunerConfig::default()).unwrap()
    }

    /// Sum of harmonics `(multiple, amplitude)` of `hz`, with a phase offset so frames do not start at 0.
    fn tone(hz: f64, harmonics: &[(f64, f64)]) -> Vec<f32> {
        (0..FRAME)
            .map(|n| {
                let t = n as f64 / f64::from(RATE);
                harmonics
                    .iter()
                    .map(|&(k, amp)| amp * (TAU * k * hz * t + 0.3 * k).sin())
                    .sum::<f64>() as f32
            })
            .collect()
    }

    fn cents_error(detected: f32, expected: f64) -> f64 {
        1200.0 * (f64::from(detected) / expected).log2()
    }

    fn pitch(detection: Detection) -> Pitch {
        match detection {
            Detection::Pitched(pitch) => pitch,
            other => panic!("expected a pitch, got {other:?}"),
        }
    }

    /// Guards tuner accuracy: a tuner must be well inside +-1 cent on clean tones across its range.
    #[test]
    fn sine_pitch_within_one_cent() {
        let tuner = tuner();
        for hz in [
            30.87, 41.20, 82.41, 110.0, 196.0, 329.63, 440.0, 1318.51, 1975.5,
        ] {
            let found = pitch(tuner.detect(&tone(hz, &[(1.0, 0.5)])).unwrap());
            let error = cents_error(found.hz, hz);
            assert!(
                error.abs() < 1.0,
                "{hz} Hz detected as {} ({error:+.3} c)",
                found.hz
            );
            assert!(found.clarity > 0.95);
        }
    }

    /// Guards against octave errors on guitar-like tones whose 2nd harmonic is louder than the
    /// fundamental, plus a DC offset that must not bias detection.
    #[test]
    fn harmonic_tone_reports_fundamental() {
        let mut frame = tone(
            82.41,
            &[(1.0, 0.2), (2.0, 0.5), (3.0, 0.3), (4.0, 0.15), (5.0, 0.1)],
        );
        frame.iter_mut().for_each(|s| *s += 0.1);
        let found = pitch(tuner().detect(&frame).unwrap());
        assert!(cents_error(found.hz, 82.41).abs() < 1.0, "got {}", found.hz);
    }

    /// Guards the no-signal paths the UI depends on: silence, a quiet hum below the gate, and
    /// broadband noise must never produce a note.
    #[test]
    fn silence_and_noise_produce_no_pitch() {
        let tuner = tuner();
        assert_eq!(tuner.detect(&[0.0; FRAME]).unwrap(), Detection::Silent);
        assert_eq!(
            tuner.detect(&tone(110.0, &[(1.0, 0.0005)])).unwrap(),
            Detection::Silent
        );

        let mut state = 0x2545_f491_u32;
        let noise: Vec<f32> = (0..FRAME)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as f32 / u32::MAX as f32 - 0.5
            })
            .collect();
        assert_eq!(tuner.detect(&noise).unwrap(), Detection::Unpitched);
    }

    /// Guards the detection range: tones outside it are unpitched, never folded to an octave inside.
    #[test]
    fn pitches_outside_range_are_unpitched() {
        let tuner = Tuner::new(TunerConfig {
            sample_rate: RATE,
            min_hz: 60.0,
            max_hz: 1_000.0,
        })
        .unwrap();
        // The harmonic 2571.1 Hz tone folds to 857 Hz under the rejected range-steered fallback
        // (tuner.md): its NSDF interpolation error looks like subharmonic evidence.
        for (hz, harmonics) in [
            (40.0, &[(1.0, 0.5)][..]),
            (1_500.0, &[(1.0, 0.5)]),
            (2_571.1, &[(1.0, 0.5), (2.0, 0.3), (3.0, 0.2)]),
        ] {
            assert_eq!(
                tuner.detect(&tone(hz, harmonics)).unwrap(),
                Detection::Unpitched,
                "{hz} Hz"
            );
        }
        assert!(matches!(
            tuner.detect(&tone(440.0, &[(1.0, 0.5)])).unwrap(),
            Detection::Pitched(_)
        ));
    }

    /// Guards the range policy (app review F2): `max_hz` filters the estimate, it does not pick
    /// another octave. With the 2nd harmonic 20 dB above the fundamental, plain MPM reads the
    /// harmonic; with that harmonic above `max_hz` the frame is `Unpitched`, not the in-range 80 Hz.
    #[test]
    fn range_filters_estimate_without_steering_octave() {
        let frame = tone(80.0, &[(1.0, 0.1), (2.0, 1.0)]);
        let found = pitch(tuner().detect(&frame).unwrap());
        // The fundamental biases this partial-period peak by a few cents; only the octave matters.
        assert!(
            cents_error(found.hz, 160.0).abs() < 50.0,
            "got {}",
            found.hz
        );
        let bass = Tuner::new(TunerConfig {
            max_hz: 100.0,
            ..TunerConfig::default()
        })
        .unwrap();
        assert_eq!(bass.detect(&frame).unwrap(), Detection::Unpitched);
    }

    /// Guards input validation so bad host buffers fail loudly instead of returning garbage.
    #[test]
    fn invalid_config_and_frames_are_rejected() {
        let bad = |sample_rate, min_hz, max_hz| {
            Tuner::new(TunerConfig {
                sample_rate,
                min_hz,
                max_hz,
            })
            .unwrap_err()
        };
        assert_eq!(bad(4_000, 25.0, 900.0), TunerError::SampleRate(4_000));
        for (min_hz, max_hz) in [
            (0.0, 100.0),
            (1e-30, 100.0),
            (200.0, 100.0),
            (25.0, 20_000.0),
            (2.0, 100.0),
        ] {
            assert!(matches!(
                bad(RATE, min_hz, max_hz),
                TunerError::Range { .. }
            ));
        }
        assert!(matches!(
            bad(RATE, f32::NAN, 100.0),
            TunerError::Range { .. }
        ));

        let tuner = tuner();
        let min = tuner.min_frame_len();
        assert_eq!(min, 2 * (1_920 + 1));
        for len in [min - 1, MAX_FRAME_LEN + 1] {
            assert_eq!(
                tuner.detect(&vec![0.0; len]).unwrap_err(),
                TunerError::FrameLength { len, min }
            );
        }
        let mut frame = vec![0.0; FRAME];
        frame[7] = f32::INFINITY;
        assert_eq!(
            tuner.detect(&frame).unwrap_err(),
            TunerError::NonFiniteSample(7)
        );
    }

    /// Guards note/cents math and the stock reference bounds (VA 0x0009932c).
    #[test]
    fn note_readings_follow_reference() {
        let a440 = ReferencePitch::default();
        let reading = a440.reading(440.0).unwrap();
        assert_eq!(reading.note.to_string(), "A4");
        assert!(reading.cents.abs() < 1e-4);

        let sharp = a440.reading(446.0).unwrap();
        assert_eq!(sharp.note.midi, 69);
        assert!((sharp.cents - 23.45).abs() < 0.01, "{}", sharp.cents);

        let flat_e = a440.reading(81.0).unwrap();
        assert_eq!(flat_e.note.to_string(), "E2");
        assert!((flat_e.cents + 29.8).abs() < 0.1, "{}", flat_e.cents);

        let a432 = ReferencePitch::new(432.0).unwrap();
        assert_eq!(a432.reading(432.0).unwrap().note.midi, 69);
        let middle_c = Note { midi: 60 };
        assert!((a440.frequency(middle_c) - 261.6256).abs() < 1e-3);

        assert!(ReferencePitch::new(MIN_REFERENCE_HZ).is_ok());
        assert!(ReferencePitch::new(MAX_REFERENCE_HZ).is_ok());
        for hz in [399.9, 480.1, f32::NAN] {
            assert!(ReferencePitch::new(hz).is_err());
        }
        for hz in [0.0, -1.0, f32::INFINITY] {
            assert!(matches!(a440.reading(hz), Err(TunerError::Frequency(_))));
        }
        assert_eq!(Note { midi: -1 }.to_string(), "B-2");
    }

    /// Guards that every public `Note` value is safe: no overflow at the `i32` extremes.
    #[test]
    fn extreme_notes_do_not_overflow() {
        let a440 = ReferencePitch::default();
        let (low, high) = (Note { midi: i32::MIN }, Note { midi: i32::MAX });
        assert_eq!(a440.frequency(low), 0.0);
        assert_eq!(a440.frequency(high), f32::INFINITY);
        assert_eq!(low.to_string(), "E-178956972");
        assert_eq!(high.to_string(), "G178956969");
    }
}
