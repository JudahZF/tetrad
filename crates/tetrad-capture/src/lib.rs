//! Bounded capture record inspection and recovered scalar numerical references.
//!
//! Includes fixed V1 neural, FIR, high-pass, resampling, tone and partial-path references.
//! FIR, high-pass and optional input/output gain can execute owned ISA48 words offline.
//! Tones are opt-in on the partial path; without them it omits three stock stages.
//! V1 orchestration emits bounded requests with explicit completion/cleanup gates;
//! `session` executes them through injected DSP, SHARC-memory, storage and service traits
//! (hybrid: stock SHARC trainer). `entry` holds the DSPController capture-state reactions
//! and capture presets as data. `v2` is the cloud-job controller. `stored` turns decrypted
//! library records into DSP bytes; decryption and keys stay with the caller.
//! No capture decryption, owned trainer, complete neural graph or full audio runtime.
//! Host arithmetic is not proof of SHARC RECIPS/rounding equivalence.

pub mod affine;
pub mod entry;
pub mod filters;
pub mod gain;
pub mod network;
pub mod orchestration;
pub mod pipeline;
mod proto;
pub mod records;
pub mod reference;
pub mod resampling;
pub mod session;
pub mod stored;
pub mod tones;
pub mod v1;
pub mod v2;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error(transparent)]
    Fir(#[from] tetrad_sharc::fir::FirError),
    #[error("explicit FIR instruction budgets require both stages to use emitted words")]
    FirBudgetsRequireEmitted,
    #[error(transparent)]
    Gain(#[from] gain::emitted::Error),
    #[error(transparent)]
    HighPass(#[from] filters::emitted::high_pass::Error),
    #[error(transparent)]
    Tone(#[from] tones::emitted::Error),
    #[error("explicit tone instruction budgets require attached emitted tone stages")]
    ToneBudgetsRequireEmitted,
    #[error("explicit stage instruction budgets require all four stages to use emitted words")]
    StageBudgetsRequireEmitted,
    #[error("explicit input-gain budgets require configured gain and all four emitted stages")]
    InputGainBudgetsRequireEmitted,
    #[error("input-gain control requires a path constructed with input gain")]
    InputGainNotConfigured,
    #[error("output-gain control or budget requires an attached output gain")]
    OutputGainNotConfigured,
    #[error("output gain is already configured and cannot be replaced")]
    OutputGainAlreadyConfigured,
    #[error("tone control requires attached tone stages")]
    TonesNotConfigured,
    #[error("tone stages are already configured and cannot be replaced")]
    TonesAlreadyConfigured,
    #[error("tone coefficient gain must be positive with a positive denominator")]
    ToneGainDomain,
    #[error("capture record is truncated")]
    Truncated,
    #[error("capture record dimension or length arithmetic overflow")]
    Overflow,
    #[error("capture record exceeds the configured bound")]
    Limit,
    #[error("unsupported capture record tag {0}")]
    Tag(u32),
    #[error("unsupported tag-4 selector {selector} or format {format}")]
    Format { selector: u32, format: u32 },
    #[error("capture record dimensions must be nonzero")]
    Shape,
    #[error(
        "only the recovered 32-sample affine path with O=1 and N>=4 divisible by four is implemented"
    )]
    UnsupportedShape,
    #[error("capture reference inputs must be finite")]
    NonFinite,
    #[error("normalized gain must be in 0 through 1")]
    NormalizedRange,
    #[error("capture reference arithmetic overflow")]
    NumericOverflow,
    #[error("V1 filter lengths must be 512 and 2048, got {pre} and {post}")]
    V1FilterLengths { pre: u32, post: u32 },
    #[error("V1 sample rate must satisfy the recovered 48000 Hz tolerance")]
    SampleRate,
    #[error("bytes follow the complete V1 runtime-data record")]
    TrailingBytes,
    #[error("only the recovered V1 1→2→2→1 neural shape is implemented")]
    UnsupportedV1Shape,
    #[error("active reference high-pass requires 1 <= frequency < 24000 Hz and bandwidth > 0")]
    HighPassDomain,
    #[error("linear gain must be nonnegative")]
    LinearGainRange,
    #[error("ordinary serialized mathematical gains require pre > 0.5 and post > 0.1")]
    ConstantGainRange,
    #[error("V1 FIR reference supports only 512 or 2048 taps, got {0}")]
    UnsupportedV1FirLength(usize),
    #[error("caller-owned resampling table requires nonempty source/provenance text")]
    ResamplingTableProvenance,
}

pub type Result<T> = std::result::Result<T, Error>;
