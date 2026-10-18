//! Owned SPORT4/DMA10-11/PCG C/SEC bring-up as an ordered MMIO plan.
//!
//! Register addresses and fields come from the pinned `ADSP-SC58x.h`
//! (`16fad8ff…`); stock values come from static analysis. The code generator emits each operation as
//! owned Type14 stores. Owned differences from stock, all **inferred**:
//!
//! - Core 1 runs both halves (stock: core 1 receives, core 2 transmits).
//! - Receive `CS0 = 0xff`: all eight window slots, matching `XCNT = 256 =
//!   8 x 32` and the stock `[[u32; 8]; 32]` buffer model. Transmit keeps the
//!   stock `0x80` (one slot): the board wiring is unknown, so driving seven
//!   more slots could contend with another driver. Until gates G3/G5 close,
//!   whether DMA moves disabled slots (block size) is unknown.
//! - No `RCU0_MSG` start wait: no stock SHARC code waits on it and the ARM
//!   restart quiesce clears bits 19-21, so a wait could hang before bring-up.
//! - The transmit DMA requests no interrupt (`INT = 0`); receive completion
//!   (SID 60) drives the frame. `DSCPTR_CUR = 2` (purpose unknown) is omitted.
//! - Enable order follows the owned design rules: both halves configured,
//!   then PCG C, then DMA enable before SPORT enable. The stock FS pin
//!   synchronization (`DAI1_PIN_STAT` edge waits) is not emitted.
//! - DAI1 pin/SRU routing is not emitted (gate `ClockAndCodecRoute`).

use super::memory::SharcCore;
use crate::resident::audio_port::{PcgPlan, StockPort, TDM_SLOTS};

pub const SEC0_GCTL: u32 = 0x3108_9000;
pub const SEC0_END: u32 = 0x3108_900c;
const SEC0_CCTL1: u32 = 0x3108_9440;
const SEC0_CCTL2: u32 = 0x3108_9480;
const SEC0_CSID1: u32 = 0x3108_945c;
const SEC0_CSID2: u32 = 0x3108_949c;
const SEC0_SCTL0: u32 = 0x3108_9800;
const SEC_SCTL_IEN: u32 = 1;
const SEC_SCTL_SEN: u32 = 1 << 2;
const SEC_SCTL_CTG: u32 = 24;
pub const TRU0_MTR: u32 = 0x3108_a7e0;
const TRU0_SSR84: u32 = 0x3108_a150;
const TRU0_SSR85: u32 = 0x3108_a154;
pub const RCU0_MSG: u32 = 0x3108_b060;
const SPU0_SECUREP74: u32 = 0x3108_cb28;
const SPU0_SECUREP75: u32 = 0x3108_cb2c;
const PCG0_CTLC0: u32 = 0x310c_b300;
const PCG0_CTLC1: u32 = 0x310c_b304;
const PCG0_PW2: u32 = 0x310c_b310;
const PCG_CTLC0_EN: u32 = (1 << 31) | (1 << 30);
const PCG_PHASE_SHIFT: u32 = 20;
// DMA register offsets from the channel base.
const DMA_DSCPTR_NXT: u32 = 0x00;
const DMA_CFG: u32 = 0x08;
const DMA_XCNT: u32 = 0x0c;
const DMA_XMOD: u32 = 0x10;
const DMA_YCNT: u32 = 0x14;
const DMA_YMOD: u32 = 0x18;
pub const DMA_STAT_IRQDONE: u32 = 1;
const DMA_CFG_EN: u32 = 1;
const DMA_CFG_INT_MASK: u32 = 3 << 20;
// SPORT register offsets from the half's CTL address.
const SPORT_MCTL: u32 = 0x08;
const SPORT_CS0: u32 = 0x0c;
const SPORT_CS1: u32 = 0x10;
const SPORT_CS2: u32 = 0x14;
const SPORT_CS3: u32 = 0x18;
const SPORT_ERR: u32 = 0x20;
const SPORT_CTL_SPENPRI: u32 = 1;
const RECEIVE_CS0: u32 = (1 << TDM_SLOTS) - 1;
/// Stock transmit window (`resident-sport-config.md`).
const TRANSMIT_CS0: u32 = 0x80;

/// SEC source of the receive DMA completion (`SPORT4_A_DMA`).
pub const RECEIVE_SID: u32 = 60;
/// Software interrupt source the stock cores route to the ARM through TRU.
pub const fn arm_doorbell(core: SharcCore) -> u32 {
    match core {
        SharcCore::Core1 => 97,
        SharcCore::Core2 => 98,
    }
}
/// `RCU0_MSG` bit the ARM sets to start a core (`1 << (core + 18)`). The
/// resident does not wait on it.
pub const fn start_bit(core: SharcCore) -> u32 {
    match core {
        SharcCore::Core1 => 1 << 19,
        SharcCore::Core2 => 1 << 20,
    }
}
/// The core's SEC interface source-ID register; reading it claims the source.
pub const fn sec_csid(core: SharcCore) -> u32 {
    match core {
        SharcCore::Core1 => SEC0_CSID1,
        SharcCore::Core2 => SEC0_CSID2,
    }
}
const fn sec_cctl(core: SharcCore) -> u32 {
    match core {
        SharcCore::Core1 => SEC0_CCTL1,
        SharcCore::Core2 => SEC0_CCTL2,
    }
}
const fn sec_target(core: SharcCore) -> u32 {
    match core {
        SharcCore::Core1 => 1,
        SharcCore::Core2 => 2,
    }
}
pub const fn sec_sctl(source: u32) -> u32 {
    SEC0_SCTL0 + 8 * source
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioOp {
    Write {
        address: u32,
        value: u32,
    },
    /// Read-modify-write OR.
    SetBits {
        address: u32,
        mask: u32,
    },
}

/// Ordered bring-up for `core`. Arguments are the system byte addresses of the
/// first descriptor of each two-descriptor ping/pong ring.
pub fn bring_up(core: SharcCore, receive: u32, transmit: u32) -> Vec<MmioOp> {
    use MmioOp::{SetBits, Write};
    let pcg = PcgPlan::STOCK_PCG_C;
    let phase = pcg.frame_sync_phase.raw();
    let mut ops = vec![
        Write {
            address: SEC0_GCTL,
            value: 1,
        },
        Write {
            address: sec_cctl(core),
            value: 1,
        },
        Write {
            address: SPU0_SECUREP74,
            value: 3,
        },
        Write {
            address: SPU0_SECUREP75,
            value: 3,
        },
        // Stock phase split: bits 9:5 to CTLC0 FSPHASEHI, 4:0 to CTLC1 FSPHASELO.
        Write {
            address: PCG0_CTLC1,
            value: pcg.clock_divide | ((phase & 0x1f) << PCG_PHASE_SHIFT),
        },
        Write {
            address: PCG0_CTLC0,
            value: pcg.frame_sync_divide | (((phase >> 5) & 0x1f) << PCG_PHASE_SHIFT),
        },
        Write {
            address: PCG0_PW2,
            value: pcg.frame_sync_pulse_width,
        },
    ];
    for (port, descriptor) in [
        (StockPort::Sport4aReceive, receive),
        (StockPort::Sport4bTransmit, transmit),
    ] {
        let dma = port.dma();
        let base = dma.base.get();
        let (cfg, cs0) = match port {
            StockPort::Sport4aReceive => (dma.cfg, RECEIVE_CS0),
            StockPort::Sport4bTransmit => (dma.cfg & !DMA_CFG_INT_MASK, TRANSMIT_CS0),
        };
        let sport = port.sport();
        let ctl = sport.ctl_address.get();
        ops.extend([
            Write {
                address: base + DMA_CFG,
                value: 0,
            },
            Write {
                address: base + DMA_DSCPTR_NXT,
                value: descriptor,
            },
            Write {
                address: base + DMA_XCNT,
                value: TDM_SLOTS * crate::resident::audio_port::PORT_FRAMES,
            },
            Write {
                address: base + DMA_YCNT,
                value: 0,
            },
            Write {
                address: base + DMA_XMOD,
                value: 4,
            },
            Write {
                address: base + DMA_YMOD,
                value: 0,
            },
            Write {
                address: base + DMA_CFG,
                value: cfg,
            },
        ]);
        for offset in [
            SPORT_MCTL, SPORT_CS0, SPORT_CS1, SPORT_CS2, SPORT_CS3, SPORT_ERR, 0,
        ] {
            ops.push(Write {
                address: ctl + offset,
                value: 0,
            });
        }
        ops.extend([
            Write {
                address: ctl,
                value: sport.ctl,
            },
            Write {
                address: ctl + SPORT_MCTL,
                value: sport.mctl,
            },
            Write {
                address: ctl + SPORT_CS0,
                value: cs0,
            },
        ]);
    }
    let (doorbell_route, doorbell) = match core {
        SharcCore::Core1 => (TRU0_SSR84, arm_doorbell(core)),
        SharcCore::Core2 => (TRU0_SSR85, arm_doorbell(core)),
    };
    ops.extend([
        Write {
            address: sec_sctl(RECEIVE_SID),
            value: (sec_target(core) << SEC_SCTL_CTG) | SEC_SCTL_SEN | SEC_SCTL_IEN,
        },
        Write {
            address: doorbell_route,
            value: doorbell,
        },
        SetBits {
            address: PCG0_CTLC0,
            mask: PCG_CTLC0_EN,
        },
    ]);
    for port in [StockPort::Sport4aReceive, StockPort::Sport4bTransmit] {
        ops.push(SetBits {
            address: port.dma().base.get() + DMA_CFG,
            mask: DMA_CFG_EN,
        });
    }
    for port in [StockPort::Sport4aReceive, StockPort::Sport4bTransmit] {
        ops.push(SetBits {
            address: port.sport().ctl_address.get(),
            mask: SPORT_CTL_SPENPRI,
        });
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bring_up_configures_both_halves_before_any_clock_or_enable() {
        let ops = bring_up(SharcCore::Core1, 0x282c_0000, 0x282c_1000);
        let position = |op: MmioOp| ops.iter().position(|o| *o == op).unwrap();
        let pcg_enable = position(MmioOp::SetBits {
            address: PCG0_CTLC0,
            mask: PCG_CTLC0_EN,
        });
        // Every configuration write precedes the first enable.
        let last_config = ops
            .iter()
            .rposition(|o| matches!(o, MmioOp::Write { .. }))
            .unwrap();
        assert!(last_config < pcg_enable);
        let rx_dma = position(MmioOp::SetBits {
            address: 0x3102_4008,
            mask: 1,
        });
        let rx_sport = position(MmioOp::SetBits {
            address: 0x3100_2400,
            mask: 1,
        });
        assert!(pcg_enable < rx_dma && rx_dma < rx_sport);
        // Recovered stock words survive where the owned plan keeps them.
        assert!(ops.contains(&MmioOp::Write {
            address: 0x3102_4008,
            value: 0x0051_4222
        }));
        assert!(ops.contains(&MmioOp::Write {
            address: 0x3102_4088,
            value: 0x0041_4220
        }));
        assert!(ops.contains(&MmioOp::Write {
            address: 0x3100_2480,
            value: 0x0208_91f2
        }));
        assert!(ops.contains(&MmioOp::Write {
            address: 0x3102_400c,
            value: 256
        }));
        // SID 60 targets core 1 with source and interrupt enabled.
        assert!(ops.contains(&MmioOp::Write {
            address: 0x3108_99e0,
            value: 0x0100_0005
        }));
        // Receive opens all eight slots; transmit keeps the stock single slot.
        assert!(ops.contains(&MmioOp::Write {
            address: 0x3100_240c,
            value: 0xff
        }));
        assert!(ops.contains(&MmioOp::Write {
            address: 0x3100_248c,
            value: 0x80
        }));
        // Bring-up starts at once; no unbounded wait on RCU0_MSG.
        assert_eq!(
            ops[0],
            MmioOp::Write {
                address: SEC0_GCTL,
                value: 1
            }
        );
    }
}
