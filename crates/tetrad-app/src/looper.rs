//! Bounded stereo host looper. Transition and mixing rules are owned policy, not recovered stock
//! DSP behavior.

use thiserror::Error;

pub type StereoFrame = [f32; 2];

/// Owned host ceiling: five minutes worth of frames at 48 kHz, not a stock limit.
pub const MAX_LOOP_FRAMES: usize = 14_400_000;
pub const MAX_BLOCK_FRAMES: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LooperLimits {
    pub max_loop_frames: usize,
    pub max_block_frames: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LooperState {
    Empty,
    Recording,
    Playing,
    Overdubbing,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LooperCommand {
    Record,
    Play,
    Overdub,
    Stop,
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LooperStatus {
    pub state: LooperState,
    pub loop_frames: usize,
    /// Next playback/overdub frame; zero outside active playback.
    pub position: usize,
    pub limits: LooperLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LooperError {
    #[error("loop and block limits must be nonzero and within the owned host ceilings")]
    InvalidLimits,
    #[error("cannot allocate bounded looper buffers")]
    AllocationFailed,
    #[error("command {command:?} is not allowed in state {state:?}")]
    InvalidTransition {
        state: LooperState,
        command: LooperCommand,
    },
    #[error("cannot play or overdub an empty recording")]
    EmptyLoop,
    #[error("input has {input} frames but output has {output}")]
    LengthMismatch { input: usize, output: usize },
    #[error("block has {frames} frames; configured maximum is {maximum}")]
    BlockTooLarge { frames: usize, maximum: usize },
    #[error("input frame {frame}, channel {channel} is not finite")]
    NonFiniteInput { frame: usize, channel: usize },
    #[error("mix at frame {frame}, channel {channel} is not finite")]
    NonFiniteMix { frame: usize, channel: usize },
}

pub struct Looper {
    limits: LooperLimits,
    state: LooperState,
    samples: Vec<StereoFrame>,
    scratch: Vec<StereoFrame>,
    length: usize,
    position: usize,
}

impl Looper {
    /// Allocates both buffers up front. Processing and commands do not allocate.
    pub fn new(limits: LooperLimits) -> Result<Self, LooperError> {
        if !(1..=MAX_LOOP_FRAMES).contains(&limits.max_loop_frames)
            || !(1..=MAX_BLOCK_FRAMES).contains(&limits.max_block_frames)
        {
            return Err(LooperError::InvalidLimits);
        }
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(limits.max_loop_frames)
            .map_err(|_| LooperError::AllocationFailed)?;
        samples.resize(limits.max_loop_frames, [0.0; 2]);
        let mut scratch = Vec::new();
        scratch
            .try_reserve_exact(limits.max_block_frames)
            .map_err(|_| LooperError::AllocationFailed)?;
        scratch.resize(limits.max_block_frames, [0.0; 2]);
        Ok(Self {
            limits,
            state: LooperState::Empty,
            samples,
            scratch,
            length: 0,
            position: 0,
        })
    }

    pub fn status(&self) -> LooperStatus {
        LooperStatus {
            state: self.state,
            loop_frames: self.length,
            position: self.position,
            limits: self.limits,
        }
    }

    /// Shared tape inspection cannot change the transport or recording.
    pub fn loop_frames(&self) -> &[StereoFrame] {
        &self.samples[..self.length]
    }

    /// Applies immediately between blocks. Record never replaces an existing loop;
    /// Clear is required first. Stop rewinds; leaving overdub for Play keeps phase.
    pub fn command(&mut self, command: LooperCommand) -> Result<LooperStatus, LooperError> {
        use LooperCommand as Command;
        use LooperState as State;

        let (state, length, position) = match (self.state, command) {
            (_, Command::Clear) => (State::Empty, 0, 0),
            (State::Empty, Command::Record) => (State::Recording, 0, 0),
            (State::Recording, Command::Record) => (self.state, self.length, 0),
            (State::Empty, Command::Stop) => (State::Empty, 0, 0),
            (_, Command::Stop) => (
                if self.length == 0 {
                    State::Empty
                } else {
                    State::Stopped
                },
                self.length,
                0,
            ),
            (State::Empty | State::Recording, Command::Play | Command::Overdub)
                if self.length == 0 =>
            {
                return Err(LooperError::EmptyLoop);
            }
            (_, Command::Play | Command::Overdub) => (
                if command == Command::Play {
                    State::Playing
                } else {
                    State::Overdubbing
                },
                self.length,
                if matches!(self.state, State::Playing | State::Overdubbing) {
                    self.position
                } else {
                    0
                },
            ),
            (_, Command::Record) => {
                return Err(LooperError::InvalidTransition {
                    state: self.state,
                    command,
                });
            }
        };
        self.state = state;
        self.length = length;
        self.position = position;
        Ok(self.status())
    }

    /// Unity dry monitoring in every state. Playing adds tape to input; overdub
    /// stores that sum and emits it once. No clipping, normalization or feedback.
    /// A full recording switches to Playing before processing the next frame.
    /// All Result failures preserve transport, tape, and caller output; private
    /// scratch is disposable. Empty blocks are accepted without state changes.
    pub fn process_block(
        &mut self,
        input: &[StereoFrame],
        output: &mut [StereoFrame],
    ) -> Result<(), LooperError> {
        if input.len() != output.len() {
            return Err(LooperError::LengthMismatch {
                input: input.len(),
                output: output.len(),
            });
        }
        if input.len() > self.limits.max_block_frames {
            return Err(LooperError::BlockTooLarge {
                frames: input.len(),
                maximum: self.limits.max_block_frames,
            });
        }
        for (frame, samples) in input.iter().enumerate() {
            for (channel, sample) in samples.iter().enumerate() {
                if !sample.is_finite() {
                    return Err(LooperError::NonFiniteInput { frame, channel });
                }
            }
        }

        let mut state = self.state;
        let mut length = self.length;
        let mut position = self.position;
        for (frame, &dry) in input.iter().enumerate() {
            let mixed = match state {
                LooperState::Empty | LooperState::Stopped => dry,
                LooperState::Recording => {
                    length += 1;
                    if length == self.limits.max_loop_frames {
                        state = LooperState::Playing;
                    }
                    dry
                }
                LooperState::Playing | LooperState::Overdubbing => {
                    let tape = if state == LooperState::Overdubbing && frame >= length {
                        // Repeated visits in this block see the last staged overdub.
                        self.scratch[frame - length]
                    } else if position >= self.length {
                        // Playback can start mid-block after filling the recording.
                        self.scratch[position - self.length]
                    } else {
                        self.samples[position]
                    };
                    let mixed = [dry[0] + tape[0], dry[1] + tape[1]];
                    for (channel, sample) in mixed.iter().enumerate() {
                        if !sample.is_finite() {
                            return Err(LooperError::NonFiniteMix { frame, channel });
                        }
                    }
                    position += 1;
                    if position == length {
                        position = 0;
                    }
                    mixed
                }
            };
            self.scratch[frame] = mixed;
        }

        match self.state {
            LooperState::Recording => {
                self.samples[self.length..length]
                    .copy_from_slice(&self.scratch[..length - self.length]);
            }
            LooperState::Overdubbing => {
                let mut cursor = self.position;
                for &mixed in &self.scratch[..input.len()] {
                    self.samples[cursor] = mixed;
                    cursor += 1;
                    if cursor == length {
                        cursor = 0;
                    }
                }
            }
            _ => {}
        }
        output.copy_from_slice(&self.scratch[..input.len()]);
        self.state = state;
        self.length = length;
        self.position = position;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn looper(loop_frames: usize, block_frames: usize) -> Looper {
        Looper::new(LooperLimits {
            max_loop_frames: loop_frames,
            max_block_frames: block_frames,
        })
        .unwrap()
    }

    #[test]
    fn records_stereo_and_repeats_with_dry_monitoring() {
        let mut looper = looper(8, 8);
        let tape = [[1.0, -1.0], [2.0, -2.0], [3.0, -3.0]];
        let mut output = [[99.0; 2]; 3];
        looper.process_block(&tape, &mut output).unwrap();
        assert_eq!(output, tape);
        assert!(looper.loop_frames().is_empty());
        looper.command(LooperCommand::Record).unwrap();
        looper.process_block(&tape[..1], &mut output[..1]).unwrap();
        looper.process_block(&tape[1..], &mut output[1..]).unwrap();
        assert_eq!(looper.loop_frames(), tape);
        assert_eq!(looper.status().state, LooperState::Recording);
        looper.command(LooperCommand::Play).unwrap();
        let dry = [[0.5, 0.25]; 7];
        let mut played = [[0.0; 2]; 7];
        looper.process_block(&dry, &mut played).unwrap();
        for (index, frame) in played.iter().enumerate() {
            let recorded = tape[index % tape.len()];
            assert_eq!(*frame, [recorded[0] + 0.5, recorded[1] + 0.25]);
        }
        assert_eq!(looper.status().position, 1);
        assert_eq!(looper.loop_frames(), tape);
    }

    #[test]
    fn capacity_starts_playback_at_the_exact_frame_including_prior_blocks() {
        for split in 0..=6 {
            let mut looper = looper(3, 6);
            let input = [
                [1.0, -1.0],
                [2.0, -2.0],
                [4.0, -4.0],
                [0.5, 0.25],
                [0.25, 0.5],
                [1.0, -1.0],
            ];
            let mut output = [[99.0; 2]; 6];
            looper.command(LooperCommand::Record).unwrap();
            looper
                .process_block(&input[..split], &mut output[..split])
                .unwrap();
            looper
                .process_block(&input[split..], &mut output[split..])
                .unwrap();
            assert_eq!(
                output,
                [
                    input[0],
                    input[1],
                    input[2],
                    [1.5, -0.75],
                    [2.25, -1.5],
                    [5.0, -5.0]
                ]
            );
            assert_eq!(looper.loop_frames(), &input[..3]);
            assert_eq!(looper.status().state, LooperState::Playing);
            assert_eq!(looper.status().position, 0);
        }
        let mut looper = looper(1, 1);
        looper.command(LooperCommand::Record).unwrap();
        looper
            .process_block(&[[0.25, -0.5]], &mut [[0.0; 2]])
            .unwrap();
        assert_eq!(looper.status().state, LooperState::Playing);
        assert_eq!(looper.status().position, 0);
    }

    #[test]
    fn overdub_accumulates_across_wraps_and_preserves_phase_when_playing() {
        for split in 0..=5 {
            let mut looper = looper(2, 5);
            looper.command(LooperCommand::Record).unwrap();
            looper
                .process_block(&[[1.0, 10.0], [2.0, 20.0]], &mut [[0.0; 2]; 2])
                .unwrap();
            looper.process_block(&[[0.0; 2]], &mut [[0.0; 2]]).unwrap();
            assert_eq!(looper.command(LooperCommand::Overdub).unwrap().position, 1);
            let input = [[0.5, -1.0]; 5];
            let mut output = [[0.0; 2]; 5];
            looper
                .process_block(&input[..split], &mut output[..split])
                .unwrap();
            looper
                .process_block(&input[split..], &mut output[split..])
                .unwrap();
            assert_eq!(
                output,
                [
                    [2.5, 19.0],
                    [1.5, 9.0],
                    [3.0, 18.0],
                    [2.0, 8.0],
                    [3.5, 17.0]
                ]
            );
            assert_eq!(looper.loop_frames(), [[2.0, 8.0], [3.5, 17.0]]);
            assert_eq!(looper.command(LooperCommand::Play).unwrap().position, 0);
            looper.process_block(&[[0.0; 2]], &mut output[..1]).unwrap();
            assert_eq!(output[0], [2.0, 8.0]);
            looper.command(LooperCommand::Overdub).unwrap();
            assert_eq!(looper.command(LooperCommand::Play).unwrap().position, 1);
        }
    }

    #[test]
    fn controls_reject_empty_playback_and_require_explicit_clear_before_rerecord() {
        let mut looper = looper(8, 4);
        for state in [LooperState::Empty, LooperState::Recording] {
            if state == LooperState::Recording {
                looper.command(LooperCommand::Record).unwrap();
            }
            let before = looper.status();
            for command in [LooperCommand::Play, LooperCommand::Overdub] {
                assert_eq!(looper.command(command), Err(LooperError::EmptyLoop));
                assert_eq!(looper.status(), before);
            }
        }
        assert_eq!(
            looper.command(LooperCommand::Stop).unwrap().state,
            LooperState::Empty
        );
        looper.command(LooperCommand::Record).unwrap();
        looper
            .process_block(&[[1.0, 2.0], [3.0, 4.0]], &mut [[0.0; 2]; 2])
            .unwrap();
        looper.command(LooperCommand::Record).unwrap();
        assert_eq!(looper.loop_frames().len(), 2);
        looper.command(LooperCommand::Overdub).unwrap();
        for command in [
            LooperCommand::Play,
            LooperCommand::Overdub,
            LooperCommand::Stop,
        ] {
            looper.command(command).unwrap();
            let before = looper.status();
            assert_eq!(
                looper.command(LooperCommand::Record),
                Err(LooperError::InvalidTransition {
                    state: before.state,
                    command: LooperCommand::Record
                })
            );
            assert_eq!(looper.status(), before);
            assert_eq!(looper.loop_frames(), [[1.0, 2.0], [3.0, 4.0]]);
        }
        let mut dry = [[0.0; 2]];
        looper.process_block(&[[7.0, 8.0]], &mut dry).unwrap();
        assert_eq!(dry, [[7.0, 8.0]]);
        assert_eq!(looper.command(LooperCommand::Play).unwrap().position, 0);
        looper.process_block(&[[0.0; 2]], &mut dry).unwrap();
        assert_eq!(dry, [[1.0, 2.0]]);
        looper.command(LooperCommand::Clear).unwrap();
        assert_eq!(looper.status().state, LooperState::Empty);
        assert!(looper.loop_frames().is_empty());
        looper.command(LooperCommand::Record).unwrap();
        looper.process_block(&[[5.0, 6.0]], &mut dry).unwrap();
        assert_eq!(
            looper.command(LooperCommand::Stop).unwrap().state,
            LooperState::Stopped
        );
        assert_eq!(looper.loop_frames(), [[5.0, 6.0]]);
    }

    #[test]
    fn invalid_blocks_preserve_tape_transport_and_output() {
        assert!(matches!(
            Looper::new(LooperLimits {
                max_loop_frames: 0,
                max_block_frames: 1
            }),
            Err(LooperError::InvalidLimits)
        ));
        assert!(matches!(
            Looper::new(LooperLimits {
                max_loop_frames: 1,
                max_block_frames: 0
            }),
            Err(LooperError::InvalidLimits)
        ));
        assert!(matches!(
            Looper::new(LooperLimits {
                max_loop_frames: MAX_LOOP_FRAMES + 1,
                max_block_frames: 1
            }),
            Err(LooperError::InvalidLimits)
        ));
        assert!(matches!(
            Looper::new(LooperLimits {
                max_loop_frames: 1,
                max_block_frames: MAX_BLOCK_FRAMES + 1
            }),
            Err(LooperError::InvalidLimits)
        ));
        let mut looper = looper(2, 4);
        looper.command(LooperCommand::Record).unwrap();
        looper
            .process_block(&[[1.0, 2.0]], &mut [[0.0; 2]])
            .unwrap();
        let before = looper.status();
        let mut output = [[99.0; 2]; 5];
        assert_eq!(
            looper.process_block(&[[0.0; 2]], &mut output),
            Err(LooperError::LengthMismatch {
                input: 1,
                output: 5
            })
        );
        assert_eq!(
            looper.process_block(&[[0.0; 2]; 5], &mut output),
            Err(LooperError::BlockTooLarge {
                frames: 5,
                maximum: 4
            })
        );
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(
                looper.process_block(&[[3.0, 4.0], [0.0, bad]], &mut output[..2]),
                Err(LooperError::NonFiniteInput {
                    frame: 1,
                    channel: 1
                })
            );
            assert_eq!(looper.status(), before);
            assert_eq!(looper.loop_frames(), [[1.0, 2.0]]);
            assert_eq!(output, [[99.0; 2]; 5]);
        }
        looper.process_block(&[], &mut []).unwrap();
        assert_eq!(looper.status(), before);
        looper
            .process_block(&[[3.0, 4.0]], &mut output[..1])
            .unwrap();
        assert_eq!(looper.loop_frames(), [[1.0, 2.0], [3.0, 4.0]]);
    }

    #[test]
    fn late_overflow_rolls_back_record_completion_playback_and_repeated_overdub() {
        let mut looper = looper(2, 4);
        looper.command(LooperCommand::Record).unwrap();
        looper
            .process_block(&[[0.0, f32::MAX]], &mut [[0.0; 2]])
            .unwrap();
        let mut output = [[99.0; 2]; 4];
        let recording = looper.status();
        assert_eq!(
            looper.process_block(&[[1.0; 2], [0.0, f32::MAX]], &mut output[..2]),
            Err(LooperError::NonFiniteMix {
                frame: 1,
                channel: 1
            })
        );
        assert_eq!(looper.status(), recording);
        assert_eq!(looper.loop_frames(), [[0.0, f32::MAX]]);
        assert_eq!(output, [[99.0; 2]; 4]);
        looper.process_block(&[[1.0; 2]], &mut output[..1]).unwrap();
        let playing = looper.status();
        output.fill([99.0; 2]);
        assert_eq!(
            looper.process_block(&[[0.0; 2], [0.0; 2], [0.0, f32::MAX]], &mut output[..3]),
            Err(LooperError::NonFiniteMix {
                frame: 2,
                channel: 1
            })
        );
        assert_eq!(looper.status(), playing);
        assert_eq!(output, [[99.0; 2]; 4]);
        looper.command(LooperCommand::Clear).unwrap();
        looper.command(LooperCommand::Record).unwrap();
        looper.process_block(&[[0.0; 2]], &mut output[..1]).unwrap();
        looper.command(LooperCommand::Overdub).unwrap();
        let overdubbing = looper.status();
        output.fill([99.0; 2]);
        let dry = [[0.0, f32::MAX / 2.0]; 4];
        assert_eq!(
            looper.process_block(&dry, &mut output),
            Err(LooperError::NonFiniteMix {
                frame: 2,
                channel: 1
            })
        );
        assert_eq!(looper.status(), overdubbing);
        assert_eq!(looper.loop_frames(), [[0.0; 2]]);
        assert_eq!(output, [[99.0; 2]; 4]);
        looper.process_block(&dry[..1], &mut output[..1]).unwrap();
        assert_eq!(looper.loop_frames(), &dry[..1]);
    }
}
