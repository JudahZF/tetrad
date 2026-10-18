//! Recovered V1 tone stages: three fixed second-order sections after output HP. Laws, recurrence,
//! reset and per-callback update come from nc section-2 parcels. Host tan/sqrt/pow/division
//! substitute for the resident helpers, so this is a mathematical reference, not SHARC parity.

pub mod emitted;
pub mod emitted_stack;

use crate::{Error, Result, filters::finite};

/// Normalized-to-dB scale and offset shared by all three tone cases (`0x211..0x251`).
const DB_SCALE: f32 = 12.0;
const DB_OFFSET: f32 = 6.0;
/// Constructor `0x801..0x820` stores these bits in all three width slots (B+0x384..0x38c).
const WIDTH: u32 = 0x3fb5_04d5;

/// Stock callback order: calls at `0x37b`, `0x387`, `0x393` pass selectors 0, 1, 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V1Tone {
    First,
    Second,
    Third,
}

/// Coefficient law selected by setter routine; names follow the recovered algebra.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V1ToneLaw {
    /// Setter `0x66`: unity at DC and Nyquist, linear gain `A` at the warp frequency.
    Peaking,
    /// Setter `0xef`: unity at DC, `A*A` at Nyquist; `sqrt(A)` scales the width term.
    HighShelf,
}

impl V1Tone {
    pub const ALL: [Self; 3] = [Self::First, Self::Second, Self::Third];

    fn index(self) -> usize {
        self as usize
    }

    pub fn law(self) -> V1ToneLaw {
        match self {
            Self::First | Self::Second => V1ToneLaw::Peaking,
            Self::Third => V1ToneLaw::HighShelf,
        }
    }

    /// `pi*f/48000` immediates passed to resident tan at `0x7da/0x7e7/0x7f4` for
    /// f = 120, 750 and 1000 Hz. The constructor also stores those plain frequencies
    /// at B+0x36c..0x374; the setters never read them.
    pub fn warp_argument(self) -> f32 {
        f32::from_bits(match self {
            Self::First => 0x3c00_adfd,
            Self::Second => 0x3d49_0fdb,
            Self::Third => 0x3d86_0a92,
        })
    }

    /// dB-to-exponent multiplier: 1/20 for peaking (`0x217`), 1/40 for the shelf (`0x253`).
    fn exponent_scale(self) -> f32 {
        f32::from_bits(match self.law() {
            V1ToneLaw::Peaking => 0x3d4c_cccd,
            V1ToneLaw::HighShelf => 0x3ccc_cccd,
        })
    }
}

/// Updater `0x178` gain law: `10^((12*p - 6) * scale)` with separate f32 operations.
/// Peaking tones receive linear gain; the shelf receives its square root.
/// Host `powf` substitutes for resident `0x169da0`.
pub fn v1_tone_gain_reference(tone: V1Tone, normalized: f32) -> Result<f32> {
    if !normalized.is_finite() {
        return Err(Error::NonFinite);
    }
    if !(0.0..=1.0).contains(&normalized) {
        return Err(Error::NormalizedRange);
    }
    let decibels = finite(finite(normalized * DB_SCALE)? - DB_OFFSET)?;
    let exponent = finite(decibels * tone.exponent_scale())?;
    finite(10.0_f32.powf(exponent))
}

/// Stored order `[b0,b1,b2]` at B+0x390+12*k and `[a1,a2]` at B+0x3b8+12*k.
/// The setters also store a0 = 1.0 at B+0x3b4+12*k; processing never reads it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct V1ToneCoefficients {
    pub feedforward: [f32; 3],
    pub feedback: [f32; 2],
}

impl V1ToneCoefficients {
    fn validate(self) -> Result<Self> {
        if self
            .feedforward
            .iter()
            .chain(&self.feedback)
            .all(|value| value.is_finite())
        {
            Ok(self)
        } else {
            Err(Error::NonFinite)
        }
    }
}

/// Setter law for one tone. `gain` is the setter argument (linear gain for peaking,
/// its square root for the shelf). Owned domain: finite gain > 0, finite
/// intermediates, positive denominator and nonzero reciprocal.
pub fn v1_tone_coefficients_reference(tone: V1Tone, gain: f32) -> Result<V1ToneCoefficients> {
    if !gain.is_finite() {
        return Err(Error::NonFinite);
    }
    if gain <= 0.0 {
        return Err(Error::ToneGainDomain);
    }
    let warp = finite(tone.warp_argument().tan())?;
    let width = f32::from_bits(WIDTH);
    match tone.law() {
        V1ToneLaw::Peaking => peaking(warp, width, gain),
        V1ToneLaw::HighShelf => high_shelf(warp, width, gain),
    }
}

fn reciprocal(denominator: f32) -> Result<f32> {
    if denominator <= 0.0 {
        return Err(Error::ToneGainDomain);
    }
    let d = finite(1.0 / denominator)?;
    if d == 0.0 {
        return Err(Error::NumericOverflow);
    }
    Ok(d)
}

/// Setter `0x66..0xc2`. Dual ALU sites `0x92`/`0x97` use the old operand for both results.
fn peaking(warp: f32, width: f32, gain: f32) -> Result<V1ToneCoefficients> {
    let warp_squared = finite(warp * warp)?;
    let width_term = finite(width * warp)?;
    let gain_term = finite(gain * width_term)?;
    let plus_width = finite(1.0 + width_term)?;
    let minus_width = finite(1.0 - width_term)?;
    let plus_gain = finite(1.0 + gain_term)?;
    let minus_gain = finite(1.0 - gain_term)?;
    let warp_less_one = finite(warp_squared - 1.0)?;
    let d = reciprocal(finite(warp_squared + plus_width)?)?;
    let b0 = finite(finite(warp_squared + plus_gain)? * d)?;
    let b2 = finite(finite(warp_squared + minus_gain)? * d)?;
    let a2 = finite(finite(warp_squared + minus_width)? * d)?;
    // One register is stored to both b1 and a1 (`0xbd`, `0xc0`).
    let first = finite(finite(warp_less_one + warp_less_one)? * d)?;
    V1ToneCoefficients {
        feedforward: [b0, first, b2],
        feedback: [first, a2],
    }
    .validate()
}

/// Setter `0xef..0x161`; host sqrt substitutes for resident `0x16e4e6`.
fn high_shelf(warp: f32, width: f32, gain: f32) -> Result<V1ToneCoefficients> {
    let root = finite(gain.sqrt())?;
    let gain_squared = finite(gain * gain)?;
    let root_width = finite(root * width)?;
    let warp_squared = finite(warp * warp)?;
    let width_term = finite(root_width * warp)?;
    let gain_warp = finite(gain * warp_squared)?;
    let plus_width = finite(1.0 + width_term)?;
    let minus_width = finite(1.0 - width_term)?;
    let gain_width = finite(gain * width_term)?;
    let shelf = finite(gain_warp - gain_squared)?;
    let plus_gain = finite(gain_squared + gain_width)?;
    let minus_gain = finite(gain_squared - gain_width)?;
    let warp_less_one = finite(gain_warp - 1.0)?;
    let d = reciprocal(finite(gain_warp + plus_width)?)?;
    let b0 = finite(finite(gain_warp + plus_gain)? * d)?;
    let a2 = finite(finite(gain_warp + minus_width)? * d)?;
    let b2 = finite(finite(gain_warp + minus_gain)? * d)?;
    let a1 = finite(finite(warp_less_one + warp_less_one)? * d)?;
    let b1 = finite(finite(shelf + shelf)? * d)?;
    V1ToneCoefficients {
        feedforward: [b0, b1, b2],
        feedback: [a1, a2],
    }
    .validate()
}

/// Process routine `0x10..0x65`: direct form I with input and output histories
/// `[x1,x2,y1,y2]`. Stock also writes x3/y3 words that nothing reads; they are omitted.
#[derive(Debug, Clone, PartialEq)]
pub struct V1ToneReference {
    coefficients: V1ToneCoefficients,
    history: [f32; 4],
}

impl V1ToneReference {
    /// Explicit finite caller coefficients; histories start at zero as after reset `0x492`.
    pub fn new(coefficients: V1ToneCoefficients) -> Result<Self> {
        Ok(Self {
            coefficients: coefficients.validate()?,
            history: [0.0; 4],
        })
    }

    pub fn coefficients(&self) -> V1ToneCoefficients {
        self.coefficients
    }

    /// `[x1, x2, y1, y2]`, newest first.
    pub fn history(&self) -> [f32; 4] {
        self.history
    }

    /// Stock setters rewrite coefficients without touching history.
    pub fn update_coefficients(&mut self, coefficients: V1ToneCoefficients) -> Result<()> {
        self.coefficients = coefficients.validate()?;
        Ok(())
    }

    /// Reset `0x492` fills both history blocks with zero; coefficients stay.
    pub fn reset(&mut self) {
        self.history = [0.0; 4];
    }

    fn process(&mut self, input: f32) -> Result<f32> {
        if !input.is_finite() {
            return Err(Error::NonFinite);
        }
        let [b0, b1, b2] = self.coefficients.feedforward;
        let [a1, a2] = self.coefficients.feedback;
        let [x1, x2, y1, y2] = self.history;
        let p0 = finite(b0 * input)?;
        let p1 = finite(x1 * b1)?;
        let p2 = finite(x2 * b2)?;
        let sum = finite(p1 + p0)?;
        let q1 = finite(y1 * a1)?;
        let sum = finite(p2 + sum)?;
        let q2 = finite(y2 * a2)?;
        let output = finite(finite(sum - q1)? - q2)?;
        self.history = [input, x1, output, y1];
        Ok(output)
    }

    /// Stock 32-sample loop; output and history commit only after every sample succeeds.
    pub fn process_block(&mut self, inputs: &[f32; 32]) -> Result<[f32; 32]> {
        let mut next = self.clone();
        let mut outputs = [0.0; 32];
        for (&input, output) in inputs.iter().zip(&mut outputs) {
            *output = next.process(input)?;
        }
        *self = next;
        Ok(outputs)
    }
}

/// The three tones in stock order with their normalized parameters.
/// Stock recomputes every tone's coefficients from its current ramped parameter at the
/// start of each callback (`0x2ef` → `0x178`). `set_normalized` is that block-boundary
/// update; parameter-service ramp/publication timing (`0x163112`) is NOT modeled.
#[derive(Debug, Clone, PartialEq)]
pub struct V1ToneStackReference {
    normalized: [f32; 3],
    sections: [V1ToneReference; 3],
}

impl V1ToneStackReference {
    /// No default parameters: the constructor never calls the setters, so stock
    /// coefficients before the first updater pass are not a recovered value.
    pub fn from_normalized(normalized: [f32; 3]) -> Result<Self> {
        let section = |tone: V1Tone| {
            let gain = v1_tone_gain_reference(tone, normalized[tone.index()])?;
            V1ToneReference::new(v1_tone_coefficients_reference(tone, gain)?)
        };
        Ok(Self {
            normalized,
            sections: [
                section(V1Tone::First)?,
                section(V1Tone::Second)?,
                section(V1Tone::Third)?,
            ],
        })
    }

    pub fn normalized(&self) -> [f32; 3] {
        self.normalized
    }

    pub fn section(&self, tone: V1Tone) -> &V1ToneReference {
        &self.sections[tone.index()]
    }

    /// Validates all three before committing any; histories are preserved.
    pub fn set_normalized(&mut self, normalized: [f32; 3]) -> Result<()> {
        let replacement = Self::from_normalized(normalized)?;
        for (section, new) in self.sections.iter_mut().zip(replacement.sections) {
            section.coefficients = new.coefficients;
        }
        self.normalized = normalized;
        Ok(())
    }

    pub fn reset(&mut self) {
        self.sections.iter_mut().for_each(V1ToneReference::reset);
    }

    /// First → second → third in place, as the callback calls `0x10` three times.
    /// Any failure retains all three histories.
    pub fn process_block(&mut self, inputs: &[f32; 32]) -> Result<[f32; 32]> {
        let mut next = self.clone();
        let mut block = *inputs;
        for section in &mut next.sections {
            block = section.process_block(&block)?;
        }
        *self = next;
        Ok(block)
    }

    #[cfg(test)]
    pub(crate) fn replace_section_for_test(&mut self, tone: V1Tone, section: V1ToneReference) {
        self.sections[tone.index()] = section;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// f64 closed form of the documented setter algebra: `([b0,b1,b2],[a1,a2])`.
    fn closed_form(tone: V1Tone, gain: f64) -> ([f64; 3], [f64; 2]) {
        let p = f64::from(tone.warp_argument()).tan();
        let r = f64::from(f32::from_bits(WIDTH));
        match tone.law() {
            V1ToneLaw::Peaking => {
                let d = p * p + 1.0 + r * p;
                (
                    [
                        (p * p + 1.0 + gain * r * p) / d,
                        2.0 * (p * p - 1.0) / d,
                        (p * p + 1.0 - gain * r * p) / d,
                    ],
                    [2.0 * (p * p - 1.0) / d, (p * p + 1.0 - r * p) / d],
                )
            }
            V1ToneLaw::HighShelf => {
                let (g, s) = (gain, gain.sqrt() * r * p);
                let d = g * p * p + 1.0 + s;
                (
                    [
                        (g * p * p + g * g + g * s) / d,
                        2.0 * (g * p * p - g * g) / d,
                        (g * p * p + g * g - g * s) / d,
                    ],
                    [2.0 * (g * p * p - 1.0) / d, (g * p * p + 1.0 - s) / d],
                )
            }
        }
    }

    fn magnitude((b, a): ([f64; 3], [f64; 2]), w: f64) -> f64 {
        let response = |c: [f64; 3]| {
            let re = c[0] + c[1] * w.cos() + c[2] * (2.0 * w).cos();
            let im = c[1] * w.sin() + c[2] * (2.0 * w).sin();
            re.hypot(im)
        };
        response(b) / response([1.0, a[0], a[1]])
    }

    #[test]
    fn gain_law_and_coefficient_laws_match_recovered_setter_algebra() {
        // 12*p - 6 dB; shelf exponent uses 1/40, so its setter receives sqrt of gain.
        let peak = v1_tone_gain_reference(V1Tone::First, 1.0).unwrap();
        let shelf = v1_tone_gain_reference(V1Tone::Third, 1.0).unwrap();
        assert!((f64::from(peak) - 10f64.powf(0.3)).abs() < 1e-6);
        assert!((f64::from(shelf) - 10f64.powf(0.15)).abs() < 1e-6);
        assert_eq!(v1_tone_gain_reference(V1Tone::Second, 0.5).unwrap(), 1.0);
        assert_eq!(
            v1_tone_gain_reference(V1Tone::First, 1.5),
            Err(Error::NormalizedRange)
        );

        for tone in V1Tone::ALL {
            // Unity gain makes numerator equal denominator in both laws.
            let unity = v1_tone_coefficients_reference(tone, 1.0).unwrap();
            assert_eq!(unity.feedforward[1], unity.feedback[0]);
            assert_eq!(unity.feedforward[2], unity.feedback[1]);
            assert!((unity.feedforward[0] - 1.0).abs() < 2e-7);

            let center = 2.0 * f64::from(tone.warp_argument());
            for p in [0.0, 0.25, 1.0] {
                let gain = v1_tone_gain_reference(tone, p).unwrap();
                let c = v1_tone_coefficients_reference(tone, gain).unwrap();
                let law = closed_form(tone, f64::from(gain));
                for (actual, expected) in c
                    .feedforward
                    .into_iter()
                    .chain(c.feedback)
                    .zip(law.0.into_iter().chain(law.1))
                {
                    assert!((f64::from(actual) - expected).abs() < 4e-7 * expected.abs().max(1.0));
                }
                // Identify the law: unity at DC; peaking gain at center, shelf at Nyquist.
                let linear = 10f64.powf((12.0 * f64::from(p) - 6.0) / 20.0);
                assert!((magnitude(law, 0.0) - 1.0).abs() < 1e-9);
                match tone.law() {
                    V1ToneLaw::Peaking => {
                        assert_eq!(c.feedforward[1].to_bits(), c.feedback[0].to_bits());
                        assert!((magnitude(law, center) - linear).abs() < 1e-6);
                        assert!((magnitude(law, std::f64::consts::PI) - 1.0).abs() < 1e-9);
                    }
                    V1ToneLaw::HighShelf => {
                        let nyquist = magnitude(law, std::f64::consts::PI);
                        assert!((nyquist - linear).abs() < 1e-6);
                    }
                }
            }
        }
    }

    #[test]
    fn coefficient_domain_is_checked_before_returning() {
        for tone in V1Tone::ALL {
            for gain in [0.0, -0.0, -1.0] {
                assert_eq!(
                    v1_tone_coefficients_reference(tone, gain),
                    Err(Error::ToneGainDomain)
                );
            }
            assert_eq!(
                v1_tone_coefficients_reference(tone, f32::NAN),
                Err(Error::NonFinite)
            );
        }
        // Only the shelf squares its argument; peaking stays finite for finite gain.
        assert!(v1_tone_coefficients_reference(V1Tone::First, f32::MAX).is_ok());
        assert_eq!(
            v1_tone_coefficients_reference(V1Tone::Third, f32::MAX),
            Err(Error::NumericOverflow)
        );
    }

    #[test]
    fn direct_form_one_order_histories_and_atomic_failure() {
        let dyadic = V1ToneCoefficients {
            feedforward: [0.5, 0.25, 0.125],
            feedback: [-0.5, 0.25],
        };
        let mut tone = V1ToneReference::new(dyadic).unwrap();
        let mut impulse = [0.0; 32];
        impulse[0] = 1.0;
        let outputs = tone.process_block(&impulse).unwrap();
        // Feedback is subtracted: y0=.5; y1=.25+.25=.5; y2=.125+.25-.125=.25; y3=0.
        assert_eq!(&outputs[..4], &[0.5, 0.5, 0.25, 0.0]);
        let history = tone.history();

        // Coefficient update preserves history; reset zeros it and keeps coefficients.
        let mut updated = tone.clone();
        updated.update_coefficients(dyadic).unwrap();
        assert_eq!(updated.history(), history);
        updated.reset();
        assert_eq!(updated.history(), [0.0; 4]);
        assert_eq!(updated.coefficients(), dyadic);

        // ((x1*b1 + b0*x) + x2*b2): 2^24 + 1 rounds to 2^24 before -2^24 cancels it.
        let mut order = V1ToneReference::new(V1ToneCoefficients {
            feedforward: [1.0, 16_777_216.0, -16_777_216.0],
            feedback: [0.0, 0.0],
        })
        .unwrap();
        assert_eq!(order.process_block(&[1.0; 32]).unwrap()[2], 0.0);

        let before = tone.clone();
        let mut bad = [0.0; 32];
        bad[31] = f32::NAN;
        assert_eq!(tone.process_block(&bad), Err(Error::NonFinite));
        let mut huge = V1ToneReference::new(V1ToneCoefficients {
            feedforward: [f32::MAX, 0.0, 0.0],
            feedback: [0.0, 0.0],
        })
        .unwrap();
        assert_eq!(huge.process_block(&[2.0; 32]), Err(Error::NumericOverflow));
        assert_eq!(huge.history(), [0.0; 4]);
        assert_eq!(tone, before);
        assert_eq!(
            tone.update_coefficients(V1ToneCoefficients {
                feedforward: [f32::INFINITY, 0.0, 0.0],
                feedback: [0.0, 0.0],
            }),
            Err(Error::NonFinite)
        );
        assert_eq!(tone, before);
    }

    #[test]
    fn stack_runs_in_call_order_and_updates_atomically_at_block_boundaries() {
        let normalized = [0.9, 0.2, 0.75];
        let mut stack = V1ToneStackReference::from_normalized(normalized).unwrap();
        let inputs: [f32; 32] = std::array::from_fn(|n| ((n * 7 % 11) as f32 - 5.0) / 8.0);
        let mut expected = inputs;
        let mut sections = V1Tone::ALL.map(|tone| stack.section(tone).clone());
        for section in &mut sections {
            expected = section.process_block(&expected).unwrap();
        }
        assert_eq!(stack.process_block(&inputs).unwrap(), expected);

        // Update keeps all histories and changes only coefficients.
        let histories = V1Tone::ALL.map(|tone| stack.section(tone).history());
        stack.set_normalized([0.1, 0.5, 1.0]).unwrap();
        assert_eq!(
            V1Tone::ALL.map(|tone| stack.section(tone).history()),
            histories
        );
        assert_eq!(
            stack.section(V1Tone::Third).coefficients(),
            v1_tone_coefficients_reference(
                V1Tone::Third,
                v1_tone_gain_reference(V1Tone::Third, 1.0).unwrap()
            )
            .unwrap()
        );

        // An invalid third parameter refuses the whole update.
        let before = stack.clone();
        assert_eq!(
            stack.set_normalized([0.5, 0.5, f32::NAN]),
            Err(Error::NonFinite)
        );
        assert_eq!(stack, before);

        // A late third-section failure retains the first two histories too.
        stack.replace_section_for_test(
            V1Tone::Third,
            V1ToneReference::new(V1ToneCoefficients {
                feedforward: [f32::MAX, 0.0, 0.0],
                feedback: [0.0, 0.0],
            })
            .unwrap(),
        );
        let before = stack.clone();
        assert_eq!(stack.process_block(&[4.0; 32]), Err(Error::NumericOverflow));
        assert_eq!(stack, before);

        stack.reset();
        assert!(
            V1Tone::ALL
                .iter()
                .all(|&tone| stack.section(tone).history() == [0.0; 4])
        );
        assert_eq!(stack.normalized(), [0.1, 0.5, 1.0]);
    }
}
