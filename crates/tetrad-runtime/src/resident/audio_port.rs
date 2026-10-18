//! Inert plan of the stock SOC1 SPORT4 TDM audio port, PCG C clocks and DAI1
//! route, from static analysis. A plan is planned configuration,
//! not physical acceptance: nothing here writes MMIO, builds DMA descriptors,
//! routes interrupts, maps slots to codecs or proves a sample rate. The open
//! hardware questions are listed in [`OPEN_GATES`].

use super::Direction;

/// Slot count in struct `w0xb` (stock model; 8-channel window inferred from WSIZE + 1). Active slots: see [`AudioGate::ActiveSlots`].
pub const TDM_SLOTS: u32 = 8;
/// Frame count in struct `w0xb` (stock model). Physical frames per DMA buffer: [`AudioGate::ActiveSlots`].
pub const PORT_FRAMES: u32 = 32;
/// SPORT SLEN 31 and DMA PSIZE/MSIZE 4 bytes.
pub const WORD_BYTES: u32 = 4;
/// Stock `w7 − w6` = `0x400`.
pub const BUFFER_BYTES: u32 = TDM_SLOTS * PORT_FRAMES * WORD_BYTES;

/// A SHARC system (physical) byte address. It is a distinct type from
/// `tetrad_sharc::address::DmWordAddress32`; no core-local promotion is applied.
///
/// ```compile_fail
/// use tetrad_runtime::resident::audio_port::PingPong;
/// use tetrad_sharc::address::DmWordAddress32;
/// PingPong::new(DmWordAddress32::new(0x1000), DmWordAddress32::new(0x2000));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhysicalByteAddress(u32);

impl PhysicalByteAddress {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// A recovered register value whose physical meaning is not proved. Read it
/// with [`Unverified::raw`]; do not derive channels or phase units from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unverified(u32);

impl Unverified {
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// Open hardware questions (report §6). A plan is not usable audio until each closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioGate {
    /// G1: CLKIN0 frequency, so FS = CLKIN0 / 512 is not proved to be 48 kHz.
    AbsoluteRate,
    /// G2: stock PCG phase split differs from the ADI reference driver.
    FsPhase,
    /// G3: CS0 = `0x80` enables only channel 7 (both halves); disabled-channel data, drive and DMA effects are unknown.
    ActiveSlots,
    /// G4: descriptor start/prefetch and the purpose of the stock
    /// `DSCPTR_CUR = 2` write. XCNT = 256 and the two-descriptor pairing are
    /// confirmed (`resident-dma-sequence.md`) and used by `firmware::audio`.
    DescriptorOrder,
    /// G5: slot → codec/jack map, level pins 8–11/16 and `PIN_STAT` read
    /// semantics. The static DAI1 route (PCG C → SPORT4, pins 1–7) is
    /// confirmed in [`Dai1RoutePlan`] but not physically observed.
    ClockAndCodecRoute,
    /// G6: SOFT0 → block-notify chain.
    BlockEventChain,
    /// G7: core-ID register behind SEC SCTL CTG, so SID → core is inferred.
    InterruptCore,
}
pub const OPEN_GATES: [AudioGate; 7] = [
    AudioGate::AbsoluteRate,
    AudioGate::FsPhase,
    AudioGate::ActiveSlots,
    AudioGate::DescriptorOrder,
    AudioGate::ClockAndCodecRoute,
    AudioGate::BlockEventChain,
    AudioGate::InterruptCore,
];

// ADSP-SC58x.h field positions.
const SPORT_CTL_DTYPE_RJUST_SIGN: u32 = 1 << 1;
const SPORT_CTL_SLEN_32: u32 = 31 << 4;
const SPORT_CTL_CKRE: u32 = 1 << 12;
const SPORT_CTL_DIFS: u32 = 1 << 15;
const SPORT_CTL_FSED: u32 = 1 << 19;
const SPORT_CTL_SPTRAN: u32 = 1 << 25;
const SPORT_MCTL_MCE: u32 = 1 << 0;
const SPORT_MCTL_MFD_1: u32 = 1 << 4;
const SPORT_MCTL_WSIZE_8: u32 = (TDM_SLOTS - 1) << 8;
const DMA_CFG_WNR_WRITE: u32 = 1 << 1;
const DMA_CFG_PSIZE_4: u32 = 2 << 4;
const DMA_CFG_MSIZE_4: u32 = 2 << 8;
const DMA_CFG_FLOW_DSCL: u32 = 4 << 12;
const DMA_CFG_NDSIZE_FETCH2: u32 = 1 << 16;
const DMA_CFG_INT_XCNT: u32 = 1 << 20;
const DMA_CFG_TRIG_XCNT: u32 = 1 << 22;

/// The two stock SPORT4 halves (struct `w8 = 4`, `w9`, `w0xa`, index tables
/// at core 1 `0x282c0510..` and core 2 `0x28a41dd0..`). Other SPORT indices
/// have no recovered DMA/SID evidence and are not representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StockPort {
    /// SPORT4A receive, configured by core 1 (struct `0x282c05f8`).
    Sport4aReceive,
    /// SPORT4B transmit, configured by core 2 (struct `0x28a42734`).
    Sport4bTransmit,
}

/// Values written by the generic SPORT routine (`0xbb32eb`/`0xbba709`). SPEN stays 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SportRegisters {
    pub ctl_address: PhysicalByteAddress,
    pub ctl: u32,
    pub mctl: u32,
    /// Raw `0x80`: only channel 7 bit set (gate G3).
    pub cs0: Unverified,
}

/// DMA channel record for the 1-D descriptor-list path. EN stays 0. The owned
/// bring-up (`firmware::audio`) adds XCNT and the descriptor ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DmaChannel {
    pub channel: u8,
    pub base: PhysicalByteAddress,
    pub stat: PhysicalByteAddress,
    /// SEC source ID. The target core is inferred (gate G7).
    pub sec_source: u16,
    pub cfg: u32,
}

impl StockPort {
    pub const fn direction(self) -> Direction {
        match self {
            Self::Sport4aReceive => Direction::Receive,
            Self::Sport4bTransmit => Direction::Transmit,
        }
    }
    pub const fn sport(self) -> SportRegisters {
        let base = SPORT_CTL_DTYPE_RJUST_SIGN
            | SPORT_CTL_SLEN_32
            | SPORT_CTL_CKRE
            | SPORT_CTL_DIFS
            | SPORT_CTL_FSED;
        let (ctl_address, ctl) = match self {
            Self::Sport4aReceive => (0x3100_2400, base),
            Self::Sport4bTransmit => (0x3100_2480, base | SPORT_CTL_SPTRAN),
        };
        SportRegisters {
            ctl_address: PhysicalByteAddress(ctl_address),
            ctl,
            mctl: SPORT_MCTL_MCE | SPORT_MCTL_MFD_1 | SPORT_MCTL_WSIZE_8,
            cs0: Unverified(0x80),
        }
    }
    pub const fn dma(self) -> DmaChannel {
        let cfg = DMA_CFG_PSIZE_4
            | DMA_CFG_MSIZE_4
            | DMA_CFG_FLOW_DSCL
            | DMA_CFG_NDSIZE_FETCH2
            | DMA_CFG_INT_XCNT
            | DMA_CFG_TRIG_XCNT;
        let (channel, base, stat, sec_source, cfg) = match self {
            Self::Sport4aReceive => (10, 0x3102_4000, 0x3102_4030, 60, cfg | DMA_CFG_WNR_WRITE),
            Self::Sport4bTransmit => (11, 0x3102_4080, 0x3102_40b0, 62, cfg),
        };
        DmaChannel {
            channel,
            base: PhysicalByteAddress(base),
            stat: PhysicalByteAddress(stat),
            sec_source,
            cfg,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("buffer {0:?} is not aligned to {WORD_BYTES} bytes")]
    Misaligned(PhysicalByteAddress),
    #[error("buffer {0:?} + {BUFFER_BYTES} bytes exceeds the 32-bit address space")]
    EndOverflow(PhysicalByteAddress),
    #[error("ping {ping:?} and pong {pong:?} buffers overlap")]
    Overlap {
        ping: PhysicalByteAddress,
        pong: PhysicalByteAddress,
    },
}

/// Two caller-owned DMA buffers of [`BUFFER_BYTES`] each. There is no default:
/// stock buffer addresses are not reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PingPong {
    ping: PhysicalByteAddress,
    pong: PhysicalByteAddress,
}

impl PingPong {
    pub fn new(ping: PhysicalByteAddress, pong: PhysicalByteAddress) -> Result<Self, PlanError> {
        let ping_last = last_byte(ping)?;
        let pong_last = last_byte(pong)?;
        if ping.0 <= pong_last && pong.0 <= ping_last {
            return Err(PlanError::Overlap { ping, pong });
        }
        Ok(Self { ping, pong })
    }
    pub const fn ping(self) -> PhysicalByteAddress {
        self.ping
    }
    pub const fn pong(self) -> PhysicalByteAddress {
        self.pong
    }
}

fn last_byte(buffer: PhysicalByteAddress) -> Result<u32, PlanError> {
    if !buffer.0.is_multiple_of(WORD_BYTES) {
        return Err(PlanError::Misaligned(buffer));
    }
    buffer
        .0
        .checked_add(BUFFER_BYTES - 1)
        .ok_or(PlanError::EndOverflow(buffer))
}

/// A planned audio port: one stock SPORT4 half and caller buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPortPlan {
    pub port: StockPort,
    pub buffers: PingPong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcgClockSource {
    Clkin0,
}

/// Recovered SelectOutput(3): both clock and frame-sync outputs selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcgOutputs {
    ClockAndFrameSync,
}

/// PCG C as configured by core 1 (`0xbb3742..0xbb379d`). Core 2 has no PCG table.
/// The DAI1 route of its clock and FS to SPORT4A/B is static-confirmed
/// ([`Dai1RoutePlan`]); it is not physically observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PcgPlan {
    pub outputs: PcgOutputs,
    pub clock_source: PcgClockSource,
    pub frame_sync_source: PcgClockSource,
    pub clock_divide: u32,
    pub frame_sync_divide: u32,
    pub frame_sync_pulse_width: u32,
    /// Raw `0x1ff` argument; the effective split and units are gate G2.
    pub frame_sync_phase: Unverified,
}

impl PcgPlan {
    pub const STOCK_PCG_C: Self = Self {
        outputs: PcgOutputs::ClockAndFrameSync,
        clock_source: PcgClockSource::Clkin0,
        frame_sync_source: PcgClockSource::Clkin0,
        clock_divide: 2,
        frame_sync_divide: 512,
        frame_sync_pulse_width: 8,
        frame_sync_phase: Unverified(0x1ff),
    };

    /// Bit clocks per frame sync, `None` unless the dividers divide exactly.
    /// Both share CLKIN0, so this is a ratio only: the absolute rate is gate G1.
    pub const fn bit_clocks_per_frame(&self) -> Option<u32> {
        match self.frame_sync_divide.checked_rem(self.clock_divide) {
            Some(0) => Some(self.frame_sync_divide / self.clock_divide),
            _ => None,
        }
    }
}

/// DAI1 signal names from the SC589 SRU XML. Each destination group encodes
/// a signal with a different source code, so no code is stored here; the
/// per-group encoding belongs to a later emitter seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dai1Signal {
    /// `PCG0_CLKC_O`.
    PcgCClock,
    /// `PCG0_FSC_O`.
    PcgCFrameSync,
    /// `DAI1_PB04_O`: the level seen on pin 4.
    Pin4,
    /// `SPT4B_D0_O`.
    Sport4bData0,
    /// `DAI1_LOW`.
    Low,
}

/// SPORT input fields written by the stock route (`DAI1_CLK0`, `FS0`, `DAT0`).
/// SPORT4B data inputs and SPORT5B/6 are not written and not representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SportInput {
    Sport4aClock,
    Sport4bClock,
    /// Routed, but no SPORT5 configuration exists on either core.
    Sport5aClock,
    Sport4aFrameSync,
    Sport4bFrameSync,
    Sport5aFrameSync,
    Sport4aData0,
    Sport4aData1,
}

/// Stock pin-buffer driver (`PBEN`) state. `On` describes the stock route;
/// it is not permission to drive the pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinDriver {
    On,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SportInputRoute {
    pub destination: SportInput,
    pub source: Dai1Signal,
}

/// One DAI1 pin buffer (pin n = `DAI1_PBnn`): its input signal and driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PinRoute {
    pub pin: u8,
    pub source: Dai1Signal,
    pub driver: PinDriver,
}

/// Final DAI1 route of the core-1 SRU init routine `0xbb3a78..0xbb3d2a`, which
/// runs before PCG C is enabled. Descriptive record, not a validated hardware
/// configuration: fields are public and copies can be changed. Which codec
/// sits on each pin, slot → channel → jack and the absolute rate stay open
/// (gates G1, G5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Dai1RoutePlan {
    /// Stores `0xbb3be7..0xbb3c33`.
    pub sport_inputs: [SportInputRoute; 8],
    /// Pins 1–7, stores `0xbb3c3e..0xbb3d0a`. Pins 8–20 take `DAI1_LOW`.
    pub pins: [PinRoute; 7],
    /// Every pin whose driver the routine leaves on, ascending. Pins 8–11 and
    /// 16 drive `DAI1_LOW` and change at run time (setter `0x16628a`).
    pub driver_on_pins: [u8; 11],
}

impl Dai1RoutePlan {
    pub const STOCK_DAI1: Self = {
        use Dai1Signal::{Low, PcgCClock, PcgCFrameSync, Pin4, Sport4bData0};
        use PinDriver::{Off, On};
        const fn input(destination: SportInput, source: Dai1Signal) -> SportInputRoute {
            SportInputRoute {
                destination,
                source,
            }
        }
        const fn pin(pin: u8, source: Dai1Signal, driver: PinDriver) -> PinRoute {
            PinRoute {
                pin,
                source,
                driver,
            }
        }
        Self {
            sport_inputs: [
                input(SportInput::Sport4aClock, PcgCClock),
                input(SportInput::Sport4bClock, PcgCClock),
                input(SportInput::Sport5aClock, PcgCClock),
                input(SportInput::Sport4aFrameSync, PcgCFrameSync),
                input(SportInput::Sport4bFrameSync, PcgCFrameSync),
                input(SportInput::Sport5aFrameSync, PcgCFrameSync),
                input(SportInput::Sport4aData0, Pin4),
                input(SportInput::Sport4aData1, Low),
            ],
            pins: [
                pin(1, PcgCClock, On),
                pin(2, PcgCClock, On),
                pin(3, PcgCFrameSync, On),
                pin(4, Low, Off),
                pin(5, PcgCClock, On),
                pin(6, PcgCFrameSync, On),
                pin(7, Sport4bData0, On),
            ],
            driver_on_pins: [1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 16],
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_halves_map_to_recovered_registers_and_dma() {
        let rx = StockPort::Sport4aReceive;
        let tx = StockPort::Sport4bTransmit;
        assert_eq!(rx.direction(), Direction::Receive);
        assert_eq!(tx.direction(), Direction::Transmit);
        assert_eq!(rx.sport().ctl_address.get(), 0x3100_2400);
        assert_eq!(tx.sport().ctl_address.get(), 0x3100_2480);
        let at = PhysicalByteAddress::new;
        assert_eq!(
            rx.dma(),
            DmaChannel {
                channel: 10,
                base: at(0x3102_4000),
                stat: at(0x3102_4030),
                sec_source: 60,
                cfg: 0x0051_4222,
            }
        );
        assert_eq!(
            tx.dma(),
            DmaChannel {
                channel: 11,
                base: at(0x3102_4080),
                stat: at(0x3102_40b0),
                sec_source: 62,
                cfg: 0x0051_4220,
            }
        );
    }

    #[test]
    fn composed_fields_reproduce_recovered_words() {
        let (rx, tx) = (StockPort::Sport4aReceive, StockPort::Sport4bTransmit);
        assert_eq!((rx.sport().ctl, tx.sport().ctl), (0x0008_91f2, 0x0208_91f2));
        assert_eq!(rx.sport().ctl ^ tx.sport().ctl, SPORT_CTL_SPTRAN);
        for port in [rx, tx] {
            assert_eq!(port.sport().mctl, 0x711);
            assert_eq!(port.sport().cs0.raw(), 0x80);
        }
        assert_eq!(BUFFER_BYTES, 0x400);
    }

    #[test]
    fn stock_pcg_gives_256_bit_clocks_per_frame() {
        let pcg = PcgPlan::STOCK_PCG_C;
        assert_eq!(pcg.outputs, PcgOutputs::ClockAndFrameSync);
        assert_eq!(pcg.bit_clocks_per_frame(), Some(256));
        assert_eq!(pcg.frame_sync_phase.raw(), 0x1ff);
    }

    #[test]
    fn stock_dai1_route_clocks_sport4_from_pcg_c_over_one_wire_per_direction() {
        use Dai1Signal::{Low, PcgCClock, PcgCFrameSync, Pin4, Sport4bData0};
        let plan = Dai1RoutePlan::STOCK_DAI1;
        let source_of = |destination| {
            let rows = plan
                .sport_inputs
                .iter()
                .filter(|r| r.destination == destination);
            let sources: Vec<_> = rows.map(|r| r.source).collect();
            assert_eq!(sources.len(), 1, "{destination:?} must have one row");
            sources[0]
        };
        for (clock, frame_sync) in [
            (SportInput::Sport4aClock, SportInput::Sport4aFrameSync),
            (SportInput::Sport4bClock, SportInput::Sport4bFrameSync),
            (SportInput::Sport5aClock, SportInput::Sport5aFrameSync),
        ] {
            assert_eq!(source_of(clock), PcgCClock);
            assert_eq!(source_of(frame_sync), PcgCFrameSync);
        }
        assert_eq!(source_of(SportInput::Sport4aData0), Pin4);
        assert_eq!(source_of(SportInput::Sport4aData1), Low);

        let pins: Vec<_> = plan
            .pins
            .iter()
            .map(|p| (p.pin, p.source, p.driver))
            .collect();
        assert_eq!(
            pins,
            [
                (1, PcgCClock, PinDriver::On),
                (2, PcgCClock, PinDriver::On),
                (3, PcgCFrameSync, PinDriver::On),
                (4, Low, PinDriver::Off),
                (5, PcgCClock, PinDriver::On),
                (6, PcgCFrameSync, PinDriver::On),
                (7, Sport4bData0, PinDriver::On),
            ]
        );
        let tx_wires: Vec<_> = plan
            .pins
            .iter()
            .filter(|p| p.source == Sport4bData0)
            .collect();
        assert_eq!(tx_wires.len(), 1);
        let rx_wires = plan.sport_inputs.iter().filter(|r| r.source == Pin4);
        assert_eq!(rx_wires.count(), 1);

        assert_eq!(plan.driver_on_pins, [1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 16]);
        for pin in plan.pins {
            let listed = plan.driver_on_pins.contains(&pin.pin);
            assert_eq!(listed, pin.driver == PinDriver::On, "pin {}", pin.pin);
        }
    }

    #[test]
    fn sport_frame_bits_match_pcg_bit_clocks_per_frame() {
        // ADSP-SC58x.h BITM_SPORT_CTL_SLEN / BITM_SPORT_MCTL_WSIZE.
        const SLEN_MASK: u32 = 0x1f0;
        const WSIZE_MASK: u32 = 0x7f00;
        for port in [StockPort::Sport4aReceive, StockPort::Sport4bTransmit] {
            let sport = port.sport();
            let word_bits = ((sport.ctl & SLEN_MASK) >> 4) + 1;
            let words = ((sport.mctl & WSIZE_MASK) >> 8) + 1;
            assert_eq!((word_bits, words), (32, 8), "{port:?}");
            assert_eq!(
                Some(word_bits * words),
                PcgPlan::STOCK_PCG_C.bit_clocks_per_frame(),
                "{port:?}"
            );
        }
    }

    #[test]
    fn ping_pong_rejects_misalignment_overflow_and_overlap_at_the_boundary() {
        let at = PhysicalByteAddress::new;
        let base = 0x2000_0000;
        assert!(PingPong::new(at(base), at(base + BUFFER_BYTES)).is_ok());
        assert!(PingPong::new(at(base + BUFFER_BYTES), at(base)).is_ok());
        assert_eq!(
            PingPong::new(at(base), at(base + BUFFER_BYTES - WORD_BYTES)),
            Err(PlanError::Overlap {
                ping: at(base),
                pong: at(base + BUFFER_BYTES - WORD_BYTES)
            })
        );
        assert_eq!(
            PingPong::new(at(base + BUFFER_BYTES - WORD_BYTES), at(base)),
            Err(PlanError::Overlap {
                ping: at(base + BUFFER_BYTES - WORD_BYTES),
                pong: at(base)
            })
        );
        assert_eq!(
            PingPong::new(at(base + 2), at(base + BUFFER_BYTES)),
            Err(PlanError::Misaligned(at(base + 2)))
        );
        let last = u32::MAX - BUFFER_BYTES + 1;
        assert!(PingPong::new(at(base), at(last)).is_ok());
        assert_eq!(
            PingPong::new(at(base), at(last + WORD_BYTES)),
            Err(PlanError::EndOverflow(at(last + WORD_BYTES)))
        );
    }
}
