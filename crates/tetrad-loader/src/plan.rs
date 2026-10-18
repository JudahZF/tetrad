//! Typed SOC1 boot plans: the exact operations ZenUI (`0x8a8420`) and loadsharc
//! (`0x87e4`) perform to start SHARC cores 2 and 1 with the stock kernels.
use std::time::Duration;

use crate::booter::{AppMailbox, BooterImage, Placement, Source, StockBooter, StreamMailbox, Swap};
use crate::intent::{CoreAction, LoaderCore};
use crate::{Error, PhysAddr, RawLdr, StreamLayout};

/// Start vector loadsharc sets before the first start so the core idles in boot ROM.
pub const BOOT_ROM_IDLE: u32 = 0x0050_00a6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step<'a> {
    /// One `/dev/corectrl` request.
    Core {
        core: LoaderCore,
        action: CoreAction,
    },
    Write32 {
        address: PhysAddr,
        value: u32,
    },
    Write16 {
        address: PhysAddr,
        value: u16,
    },
    Delay(Duration),
    /// Copy bytes into physical memory, applying the stock byte order.
    Upload {
        address: PhysAddr,
        swap: Swap,
        source: Source<'a>,
    },
    /// loadsharc mailbox stream of one raw LDR file into the running booter.
    Stream {
        core: LoaderCore,
        mailbox: StreamMailbox,
        image: &'a [u8],
    },
    /// Wait for the kernel to start, then hand it the argument string.
    Arguments {
        core: LoaderCore,
        mailbox: AppMailbox,
        arguments: &'a [u8],
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BootPlan<'a> {
    pub steps: Vec<Step<'a>>,
}

/// Whether `/tmp/sharcs_started` existed when ZenUI booted the DSPs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    First,
    /// Quiesce shared peripherals and stop both cores before loading (`0x8a85a4`).
    Restart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    QuadCortex,
    Atma,
}

impl Variant {
    /// The `-a` string ZenUI passes to both loadsharc runs.
    pub const fn sharc_arguments(self) -> &'static [u8] {
        match self {
            Self::QuadCortex => b"QC",
            Self::Atma => b"Atma",
        }
    }
}

const fn zero32(address: u32) -> Step<'static> {
    Step::Write32 {
        address: PhysAddr(address),
        value: 0,
    }
}

const fn zero16(address: u32) -> Step<'static> {
    Step::Write16 {
        address: PhysAddr(address),
        value: 0,
    }
}

/// ZenUI restart quiesce `0x8a57b0`, in stock order. It clears SEC0 source control for the
/// peripherals a running kernel may own and disables those peripherals. It does not
/// wait for DMA to drain.
pub const RESTART_QUIESCE: [Step<'static>; 46] = [
    // SEC0_SCTL44..49: SYS_SOFT0..5.
    zero32(0x3108_9960),
    zero32(0x3108_9968),
    zero32(0x3108_9970),
    zero32(0x3108_9978),
    zero32(0x3108_9980),
    zero32(0x3108_9988),
    Step::Delay(Duration::from_millis(1)),
    // SCTL60..63: SPORT4 A/B DMA and status.
    zero32(0x3108_99e0),
    zero32(0x3108_99e8),
    zero32(0x3108_99f0),
    zero32(0x3108_99f8),
    // SCTL74..75: TIMER0 TMR6/7.
    zero32(0x3108_9a50),
    zero32(0x3108_9a58),
    // SCTL77..80: LP0/LP1 DMA and status.
    zero32(0x3108_9a68),
    zero32(0x3108_9a70),
    zero32(0x3108_9a78),
    zero32(0x3108_9a80),
    // SCTL150..154: FFTA0, FIR0.
    zero32(0x3108_9cb0),
    zero32(0x3108_9cb8),
    zero32(0x3108_9cc0),
    zero32(0x3108_9cc8),
    zero32(0x3108_9cd0),
    // SCTL166..171: MDMA2/3, EMDMA0/1.
    zero32(0x3108_9d30),
    zero32(0x3108_9d38),
    zero32(0x3108_9d40),
    zero32(0x3108_9d48),
    zero32(0x3108_9d50),
    zero32(0x3108_9d58),
    // SCTL217..224: MDMA0..3 error.
    zero32(0x3108_9ec8),
    zero32(0x3108_9ed0),
    zero32(0x3108_9ed8),
    zero32(0x3108_9ee0),
    zero32(0x3108_9ee8),
    zero32(0x3108_9ef0),
    zero32(0x3108_9ef8),
    zero32(0x3108_9f00),
    // SPORT4_CTL_A, TIMER0 TMR6/7 CFG (16-bit), LP0/LP1 CTL.
    zero32(0x3100_2400),
    zero16(0x3100_1120),
    zero16(0x3100_1140),
    zero32(0x30ff_e000),
    zero32(0x30ff_e100),
    // SEC0_CCTL1/2 (SHARC core interfaces), EMDMA0/1 CTL.
    zero32(0x3108_9440),
    zero32(0x3108_9480),
    zero32(0x310c_602c),
    zero32(0x310c_6030),
    // RCU0_MSG_CLR bits 19..21.
    Step::Write32 {
        address: PhysAddr(0x3108_b068),
        value: 0x0038_0000,
    },
];

/// One loadsharc run (`-i <image> -s <core> -a <arguments>`, no `-c`/`-r`).
/// The image must be the decrypted kernel for `booter.core`.
pub fn plan_core<'a>(
    booter: &BooterImage<'a>,
    image: &RawLdr<'a>,
    arguments: &'a [u8],
) -> Result<(Vec<Step<'a>>, StreamLayout), Error> {
    let core = booter.core;
    image.validate_signature(core.signature())?;
    let layout = image.stream_layout()?;
    let mut steps = vec![
        Step::Core {
            core,
            action: CoreAction::Stop,
        },
        // Stock always uses the core-1 vector request here, also for core 2 (`0x8b04`).
        Step::Core {
            core: LoaderCore::Core1,
            action: CoreAction::SetEntry(BOOT_ROM_IDLE),
        },
        Step::Core {
            core,
            action: CoreAction::Start,
        },
    ];
    steps.extend(booter.sections.iter().filter_map(|section| {
        section.placement.map(
            |Placement {
                 address,
                 swap,
                 source,
             }| Step::Upload {
                address,
                swap,
                source,
            },
        )
    }));
    steps.extend([
        Step::Core {
            core,
            action: CoreAction::SetEntry(booter.app_start),
        },
        Step::Core {
            core,
            action: CoreAction::Start,
        },
        Step::Stream {
            core,
            mailbox: booter.stream,
            image: image.as_bytes(),
        },
        Step::Arguments {
            core,
            mailbox: booter.map.app,
            arguments,
        },
    ]);
    Ok((steps, layout))
}

/// SOC1 boot as ZenUI runs it: the restart quiesce and stops (empty on a first boot),
/// then loadsharc for core 2 and for core 1 (`-s 2 && -s 1`).
#[derive(Debug, Clone)]
pub struct Soc1Plan<'a> {
    pub quiesce: BootPlan<'a>,
    pub core2: BootPlan<'a>,
    pub core1: BootPlan<'a>,
}

pub fn plan_soc1<'a>(
    booter: &StockBooter<'a>,
    core1: &RawLdr<'a>,
    core2: &RawLdr<'a>,
    variant: Variant,
    start: Start,
) -> Result<Soc1Plan<'a>, Error> {
    let arguments = variant.sharc_arguments();
    let mut quiesce = Vec::new();
    if start == Start::Restart {
        quiesce.extend(RESTART_QUIESCE);
        quiesce.extend(
            [LoaderCore::Core1, LoaderCore::Core2].map(|core| Step::Core {
                core,
                action: CoreAction::Stop,
            }),
        );
    }
    Ok(Soc1Plan {
        quiesce: BootPlan { steps: quiesce },
        core2: BootPlan {
            steps: plan_core(&booter.core2, core2, arguments)?.0,
        },
        core1: BootPlan {
            steps: plan_core(&booter.core1, core1, arguments)?.0,
        },
    })
}
