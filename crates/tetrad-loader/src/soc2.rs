//! SOC2 slave boot as `DSPCommsARM` drives it through protocol core 0 (SOC1 SHARC core 1).
//!
//! Sans-IO: the DSP host sends the returned requests and feeds responses back. ARM only
//! places the two SOC2 images in DDR at the address SOC1 supplies; SOC1 then boots SOC2.
//! Flow: `0x12` (`0x89b7d4`) → type 5 address (`0x89bdec`) → copy (`0x89bae0`) → `0x13`
//! → type 6 status (`0x89c0bc`) → `0x14`, 1 s settle, open SOC2 comms; or retry/dead.
use std::time::Duration;

use thiserror::Error;

use crate::PhysAddr;
use crate::booter::{Source, Swap};
use crate::plan::{BootPlan, Step};

/// Images are placed at 16-byte alignment: SPL at +0, core-0 image at `align16(SPL)`.
pub const IMAGE_ALIGN: u32 = 16;
/// `(base, len)` of the DDR address space a SOC2 buffer may use: the SC589 DMC0 window
/// (inferred from the SC58x memory map; the shipped `zpu.dtb`/`atma.dtb` `/memory@83000000`
/// node confirms DDR there). Stock copies to any address; owned guard (review B4): a wrong
/// reply never reaches L2, L1 or MMIO. Linux System RAM inside it is refused by the memory
/// layer (tetrad-sys `BootHardware::map_soc2_buffer`, `/proc/iomem`).
pub const SOC2_DDR: (u32, u32) = (0x8000_0000, 0x4000_0000);
/// `usleep(1000000)` after `0x14` before ZenUI opens SOC2 comms.
pub const SETTLE: Duration = Duration::from_secs(1);
pub const MESSAGE_BYTES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SlaveBootKind {
    BufferMemAddr = 0x12,
    Boot = 0x13,
    Finished = 0x14,
}

/// One synchronous request to protocol core 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlaveBootRequest {
    pub kind: SlaveBootKind,
    pub argument: u32,
    /// Bytes 4..8. Set for `0x13` only: the core-0 file size.
    pub core0_len: u32,
}

impl SlaveBootRequest {
    /// Byte 0 type, byte 1 sub-core 0, bytes 4..8 core-0 size, u32 length `0x10` at +8,
    /// argument at +0xc. Bytes 2..4 stay zero; stock leaves some other bytes stale.
    pub fn encode(self) -> [u8; MESSAGE_BYTES] {
        let mut message = [0; MESSAGE_BYTES];
        message[0] = self.kind as u8;
        message[4..8].copy_from_slice(&self.core0_len.to_le_bytes());
        message[8..12].copy_from_slice(&(MESSAGE_BYTES as u32).to_le_bytes());
        message[12..].copy_from_slice(&self.argument.to_le_bytes());
        message
    }
    /// Synchronous wait budget ZenUI passes to the send.
    pub const fn timeout(self) -> Duration {
        Duration::from_millis(match self.kind {
            SlaveBootKind::BufferMemAddr => 2000,
            SlaveBootKind::Boot => 4000,
            SlaveBootKind::Finished => 20_000,
        })
    }
}

/// SPL plus core-0 image for one attempt, copied verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Soc2Images<'a> {
    spl: &'a [u8],
    core0: &'a [u8],
    total: u32,
}

impl<'a> Soc2Images<'a> {
    /// Stock pairs: `SOC2_SPL_Core0[_Atma].blwldr` + `ZPU_SC589_SHARC_Core0.blwldr`, or the
    /// factory pair `SOC2_SPL_Emu_Core0.ldr` + `SOC2_FACTORY.blwldr`. Both files are copied
    /// unchanged, as stock does; no `BLw` frame check gates the copy.
    pub fn new(spl: &'a [u8], core0: &'a [u8]) -> Result<Self, crate::Error> {
        if spl.is_empty() || core0.is_empty() {
            return Err(crate::Error::Empty);
        }
        let size = |bytes: &[u8]| u32::try_from(bytes.len()).ok();
        let total = size(spl)
            .and_then(|spl| spl.checked_next_multiple_of(IMAGE_ALIGN))
            .zip(size(core0))
            .and_then(|(spl, core0)| spl.checked_add(core0))
            .ok_or(crate::Error::Overflow("SOC2 image size"))?;
        Ok(Self { spl, core0, total })
    }
    /// `0x8a5a74`: `align16(len(SPL)) + len(core0)`; the `0x12` argument.
    pub fn total_len(&self) -> u32 {
        self.total
    }
    /// The `0x13` argument. Fits u32: [`Self::new`] checked it.
    pub fn spl_len(&self) -> u32 {
        self.spl.len() as u32
    }
    /// The `0x13` core-0 size, placed at bytes 4..8. Fits u32: [`Self::new`] checked it.
    pub fn core0_len(&self) -> u32 {
        self.core0.len() as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum Soc2Error {
    #[error("unexpected {0} in the current slave-boot state")]
    UnexpectedResponse(&'static str),
    #[error("SOC2 buffer {0} cannot hold the images")]
    BufferOverflow(PhysAddr),
    #[error("SOC2 buffer {0} + {1:#x} is outside the DDR window {SOC2_DDR:#x?}")]
    BufferOutsideDdr(PhysAddr, u32),
}

/// Failure contract (`0x89c0bc`, `0x89b7d4`, `0x89bdec`, `0x89bae0`):
/// - Status failure with no retry left: [`Self::Dead`]. Mark protocol cores 2, 3 and SOC2 dead.
/// - A retry `0x12` that fails to send: mark all five protocol cores dead, including SOC1.
/// - A copy failure in [`Soc2Boot::on_buffer_address`]: stock stops silently and marks
///   nothing dead. The caller must stall the same way and must not mark cores dead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Soc2Outcome {
    /// Send `finished` (wait 20 s), sleep [`SETTLE`], then open SOC2 comms. Stock passes
    /// stale stack bytes as the argument; this sends 0 (deliberate difference).
    Booted { finished: SlaveBootRequest },
    /// Send this `0x12` again with the next image pair. If that send fails, see the
    /// contract above: all five protocol cores are dead.
    Retry(SlaveBootRequest),
    /// No retry left after a status failure.
    Dead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    AwaitAddress,
    AwaitStatus,
    Done,
}

/// Attempt order: factory pair first when `/opt/neuraldsp/factory_image_flag` exists,
/// then the normal pair once. Without a factory pair a failure is final (`0x89c0bc`).
#[derive(Debug, Clone)]
pub struct Soc2Boot<'a> {
    attempts: Vec<Soc2Images<'a>>,
    attempt: usize,
    state: State,
}

impl<'a> Soc2Boot<'a> {
    pub fn new(normal: Soc2Images<'a>, factory: Option<Soc2Images<'a>>) -> Self {
        Self {
            attempts: factory.into_iter().chain([normal]).collect(),
            attempt: 0,
            state: State::Idle,
        }
    }
    fn images(&self) -> Soc2Images<'a> {
        self.attempts[self.attempt]
    }
    /// The first `0x12`, sent after DDR tests `0x54`, `0x55` and `0x57` succeed.
    pub fn begin(&mut self) -> Result<SlaveBootRequest, Soc2Error> {
        if self.state != State::Idle {
            return Err(Soc2Error::UnexpectedResponse("begin"));
        }
        self.state = State::AwaitAddress;
        Ok(self.buffer_request())
    }
    fn buffer_request(&self) -> SlaveBootRequest {
        SlaveBootRequest {
            kind: SlaveBootKind::BufferMemAddr,
            argument: self.images().total_len(),
            core0_len: 0,
        }
    }
    /// Type 5 response: word at body +0 is the physical DDR buffer. Returns the copy
    /// plan (run it with [`crate::exec::execute`]) and the `0x13` to send after it. A buffer
    /// range outside [`SOC2_DDR`] is refused before any write (a copy failure: the caller
    /// stalls, see [`Soc2Outcome`]).
    pub fn on_buffer_address(
        &mut self,
        buffer: PhysAddr,
    ) -> Result<(BootPlan<'a>, SlaveBootRequest), Soc2Error> {
        if self.state != State::AwaitAddress {
            return Err(Soc2Error::UnexpectedResponse("buffer address"));
        }
        let images = self.images();
        let end = buffer
            .checked_add(images.total_len())
            .ok_or(Soc2Error::BufferOverflow(buffer))?;
        let (ddr, ddr_len) = SOC2_DDR;
        if buffer.0 < ddr || end.0 - ddr > ddr_len {
            return Err(Soc2Error::BufferOutsideDdr(buffer, images.total_len()));
        }
        let spl_len = images.spl_len();
        let core0_at = PhysAddr(buffer.0 + spl_len.next_multiple_of(IMAGE_ALIGN));
        let upload = |address, bytes| Step::Upload {
            address,
            swap: Swap::None,
            source: Source::Bytes(bytes),
        };
        let plan = BootPlan {
            steps: vec![upload(buffer, images.spl), upload(core0_at, images.core0)],
        };
        self.state = State::AwaitStatus;
        Ok((
            plan,
            SlaveBootRequest {
                kind: SlaveBootKind::Boot,
                argument: spl_len,
                core0_len: images.core0_len(),
            },
        ))
    }
    /// Type 6 response: status word at body +0; zero is success.
    pub fn on_status(&mut self, status: u32) -> Result<Soc2Outcome, Soc2Error> {
        if self.state != State::AwaitStatus {
            return Err(Soc2Error::UnexpectedResponse("boot status"));
        }
        if status == 0 {
            self.state = State::Done;
            return Ok(Soc2Outcome::Booted {
                finished: SlaveBootRequest {
                    kind: SlaveBootKind::Finished,
                    argument: 0,
                    core0_len: 0,
                },
            });
        }
        if self.attempt + 1 < self.attempts.len() {
            self.attempt += 1;
            self.state = State::AwaitAddress;
            return Ok(Soc2Outcome::Retry(self.buffer_request()));
        }
        self.state = State::Done;
        Ok(Soc2Outcome::Dead)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review B4: a type 5 address outside the DDR window (L2, MMIO, past the window end)
    /// gives no copy plan, so nothing is written; a buffer inside it does.
    #[test]
    fn buffer_outside_the_ddr_window_is_refused() {
        let (spl, core0) = ([0x5a; 40], [0xc0; 24]);
        let images = Soc2Images::new(&spl, &core0).unwrap();
        let (ddr, ddr_len) = SOC2_DDR;
        for buffer in [0x2008_0000, 0x3100_0000, ddr + ddr_len - 16, ddr - 16] {
            let mut boot = Soc2Boot::new(images, None);
            boot.begin().unwrap();
            assert_eq!(
                boot.on_buffer_address(PhysAddr(buffer)).unwrap_err(),
                Soc2Error::BufferOutsideDdr(PhysAddr(buffer), images.total_len()),
                "{buffer:#x}"
            );
        }
        let mut boot = Soc2Boot::new(images, None);
        boot.begin().unwrap();
        let last = ddr + ddr_len - images.total_len();
        assert!(boot.on_buffer_address(PhysAddr(last)).is_ok());
    }
}
