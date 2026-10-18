//! Stock-ordered flows over [`DspSession`]: boot handshake and key pulls,
//! SOC2 slave boot, multi-core global controls, parameters, bulk scene
//! updates, meters, diagnostics, latency, run-list clears, model starts,
//! model data and section transfers.

use std::time::Duration;

use tetrad_dsp_protocol::address::Address;
use tetrad_dsp_protocol::fragment::MessageMetadata;
use tetrad_dsp_protocol::packet::{BulkMode, BulkRecord, ParameterChange};
use tetrad_dsp_protocol::requests::{
    BULK_PARAMETER_RECORDS_MAX, BULK_SEND_POLICY, BulkFrame, BulkRequest, BypassRecord,
    BypassState, Request, RequestFrame, StartRecord,
};
use tetrad_dsp_protocol::responses::{DiagnosticsBlock, Dispatcher, ModuleLatency};
use tetrad_dsp_protocol::{CoreId, DeploymentTarget, DspCore, MessageType, ModuleSlot};

use qc_format::blob::ModuleBlob;
use tetrad_loader::soc2::{IMAGE_ALIGN, SlaveBootKind, SlaveBootRequest};

use super::{Clock, Delivery, DspSession, RESPONSE_POLL_INTERVAL, SendPolicy, SessionError};
use crate::SharedMemory;
use crate::blob::{self, PlacedModel};
use crate::model::CoreWindows;
use crate::responses::{CrashCause, Event, KeyMaterial};
use crate::transport::{ControlPacket, SendReceipt};

const DSP_CORES: [DspCore; 4] = [
    DspCore::Core0,
    DspCore::Core1,
    DspCore::Core2,
    DspCore::Core3,
];
const ALL_CORES: [CoreId; 5] = [
    CoreId::Dsp(DspCore::Core0),
    CoreId::Dsp(DspCore::Core1),
    CoreId::Dsp(DspCore::Core2),
    CoreId::Dsp(DspCore::Core3),
    CoreId::Control,
];

/// Stock key polls: 10000 × `usleep(100)` (VAs 0x875394, 0x876598).
pub const KEY_WAIT: Duration = Duration::from_secs(1);
/// Run-list-cleared wait after `DeleteAllModels`. Stock polls 100 ×
/// `usleep(100)` (VA 0x515890) but only after FitChecker ran for the changed
/// cores; the session fits before `0x1e`, so this owned wait is longer.
pub const RUN_LIST_WAIT: Duration = Duration::from_millis(50);
/// Owned bound for the event-driven slave boot responses `0x05`/`0x06`.
/// Stock reacts to them without a timeout.
pub const SLAVE_BOOT_RESPONSE_WAIT: Duration = Duration::from_secs(30);
/// Owned (O): how long core 0 may stay silent after the handshake before the boot reports
/// [`CORE0_SILENT`]. On the QC (2026-10-15) a good boot answers within milliseconds; about one
/// boot in ten after a SOC1 restart answered nothing at all.
pub const CORE0_ANSWER_WAIT: Duration = Duration::from_secs(2);
/// The [`SessionError::ResponseTimeout`] text of a silent core 0: the caller may restart SOC1.
pub const CORE0_SILENT: &str = "core 0 after the SOC1 restart";
/// `syncAllCores` (VA 0x8763e8) sends `0x0a` with wait 1 / 1000 ms.
const SYNC_POLICY: SendPolicy = SendPolicy::new(true, 1000);

/// Boot vectors in stock pull order. `None` is a stock timeout (empty vector).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootKeys {
    /// Response `0x24`: decrypts `ModelRepo.xml` (VA 0x52913c).
    pub repo: Option<KeyMaterial>,
    /// Response `0x25`: decrypts every model blob (VA 0x5e0c4c).
    pub blob: Option<KeyMaterial>,
    /// Response `0x27`: consumer unresolved.
    pub third: Option<KeyMaterial>,
}

/// Byte lengths of the two SOC2 images (VA 0x89bae0): `first` at offset 0,
/// `second` at the next 16-byte boundary. `0x13` carries `first` at `+12`
/// and `second` at `+4` (VA 0x89bdec).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Soc2ImageLengths {
    pub first: u32,
    pub second: u32,
}

/// Platform steps of the SOC2 slave boot that touch files, `/dev/mem` or GPIO.
/// The frames come from `tetrad_loader::soc2::SlaveBootRequest`.
pub trait Soc2Boot {
    type Error: std::error::Error + Send + Sync + 'static;
    /// `0x12` argument: `align16(first) + second` (VA 0x8a5a74), the slave
    /// boot buffer size (`DSPCommsARM+0x39c`).
    fn buffer_bytes(&mut self) -> Result<u32, Self::Error>;
    /// Compatibility path: copies both images to the DSP-supplied physical
    /// buffer and returns only the first length. Implement
    /// [`Soc2Boot::write_image_pair`] instead.
    fn write_images(&mut self, physical_address: u32) -> Result<u32, Self::Error>;
    /// Copies both images and returns both lengths. `None` (the default,
    /// which calls [`Soc2Boot::write_images`]) makes [`DspSession::boot`]
    /// derive `second = buffer_bytes − align16(first)` from the stock size
    /// rule.
    fn write_image_pair(
        &mut self,
        physical_address: u32,
    ) -> Result<(u32, Option<Soc2ImageLengths>), Self::Error> {
        self.write_images(physical_address)
            .map(|first| (first, None))
    }
    /// After `0x14`: stock sleeps 1 s, probes GPIO 97 and opens the SOC2
    /// rings (VA 0x89c0bc). Returns whether SOC2 comms opened.
    fn finish(&mut self) -> Result<bool, Self::Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlaveBootOutcome {
    Soc2Open,
    /// Boot finished but the platform could not open SOC2 comms.
    Soc2CommsFailed,
    /// The DSP reported failure; cores 2, 3 and the control core are dead.
    Soc2Dead,
}

/// Built-in model IDs: their start record carries the ID, not an entry
/// address (VA 0x879100); 22004 has no blob to load (VA 0x878aa0).
pub const BUILTIN_MODELS: std::ops::RangeInclusive<u32> = 22000..=22004;

/// One run-list slot with its own blob, for [`DspSession::deploy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotModel {
    pub model_id: u32,
    /// The per-core variant ([`blob::for_core`]); `None` only for built-ins.
    pub blob: Option<ModuleBlob>,
    /// Start record `+6`: compiler plan node byte (VA 0x87fc2c). Meaning
    /// unresolved.
    pub layout_word: u16,
    /// Start record `+8`: ModelRepo `plugin_id`, or -1 (VA 0x879100).
    pub block_word: i32,
}

/// One entry of a core's blob list (compiler `+0x1c` names, `+0x28` model
/// ids; tetrad-engine `CorePlan::blobs`). Stock places and copies each entry
/// once, and every slot that names it starts at the same entry address
/// (VAs 0x5e62a0, 0x878aa0, 0x879100).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobLoad {
    pub model_id: u32,
    /// The per-core variant ([`blob::for_core`]); `None` when the model has
    /// no blob name (stock places nothing for it).
    pub blob: Option<ModuleBlob>,
}

/// One run-list slot that names its blob by list index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunSlot {
    pub model_id: u32,
    /// Index into [`CoreLoad::blobs`] (compiler `+0xc58`, VA 0x87a338);
    /// `None` for built-ins.
    pub blob: Option<usize>,
    /// Start record `+6` (VA 0x87a314).
    pub layout_word: u16,
    /// Start record `+8`: ModelRepo `plugin_id`, or -1 (VA 0x879100).
    pub block_word: i32,
}

/// One core's blob list and run list for [`DspSession::deploy_loads`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreLoad {
    pub core: DspCore,
    pub blobs: Vec<BlobLoad>,
    pub slots: Vec<Option<RunSlot>>,
}

impl CoreLoad {
    /// The per-slot form: one blob load per slot that has a blob, in slot
    /// order, so nothing is shared.
    pub fn per_slot(core: DspCore, slots: Vec<Option<SlotModel>>) -> Self {
        let mut blobs = Vec::new();
        let slots = slots
            .into_iter()
            .map(|slot| {
                slot.map(|model| RunSlot {
                    model_id: model.model_id,
                    blob: model.blob.map(|blob| {
                        blobs.push(BlobLoad {
                            model_id: model.model_id,
                            blob: Some(blob),
                        });
                        blobs.len() - 1
                    }),
                    layout_word: model.layout_word,
                    block_word: model.block_word,
                })
            })
            .collect();
        Self { core, blobs, slots }
    }
}

struct CorePlan {
    core: DspCore,
    /// Relocated blob loads by blob-list index.
    loads: Vec<(ModuleSlot, Option<PlacedModel>)>,
    records: Vec<StartRecord>,
}

/// FitChecker (VA 0x5e62a0) packs the blob list in order; 0x878aa0 relocates
/// each entry once and returns its handle (the entry address, or 22004 for
/// the built-in that has no blob). Start records take the built-in ID or the
/// handle of the slot's blob (VA 0x879100).
fn plan(load: &CoreLoad) -> Result<CorePlan, SessionError> {
    let core = load.core;
    let blobs: Vec<_> = load.blobs.iter().map(|load| load.blob.as_ref()).collect();
    let placements = blob::place(&blobs).map_err(|error| match error {
        blob::BlobError::WontFit(section) => SessionError::WontFit { core, section },
        error => error.into(),
    })?;
    let mut loads = Vec::with_capacity(blobs.len());
    for (index, (blob, placement)) in blobs.iter().zip(placements).enumerate() {
        // The handle table is `[4][30]`, like the run list.
        let index = ModuleSlot::try_from(u32::try_from(index).unwrap_or(u32::MAX))?;
        let placed = match (blob, placement) {
            (Some(blob), Some(placement)) => Some(blob::relocate(blob, &placement)?),
            _ => None,
        };
        loads.push((index, placed));
    }
    let mut records = Vec::new();
    for (index, slot) in load.slots.iter().enumerate() {
        let Some(slot) = slot else {
            continue;
        };
        let entry = if BUILTIN_MODELS.contains(&slot.model_id) {
            slot.model_id
        } else {
            slot.blob
                .and_then(|index| loads.get(index)?.1.as_ref())
                .ok_or(SessionError::Refused("model without a blob"))?
                .entry
        };
        records.push(StartRecord {
            model: entry,
            slot: ModuleSlot::try_from(u32::try_from(index).unwrap_or(u32::MAX))?,
            layout_word: slot.layout_word,
            block_word: slot.block_word,
        });
    }
    Ok(CorePlan {
        core,
        loads,
        records,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParameterUpdate {
    pub target: DeploymentTarget,
    pub index: u32,
    /// Raw f32 bits; NaN payloads survive.
    pub value_bits: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BypassUpdate {
    pub target: DeploymentTarget,
    pub state: BypassState,
}

/// Stock loops ignore a timed-out or refused send and carry on. Keep the
/// first such error; stop at once on anything else (for example a fault).
fn tolerate<T>(
    first: &mut Option<SessionError>,
    result: Result<T, SessionError>,
) -> Result<(), SessionError> {
    match result {
        Ok(_) => Ok(()),
        Err(
            error @ (SessionError::SendTimeout { .. }
            | SessionError::DeadCore(_)
            | SessionError::RunListTimeout(_)),
        ) => {
            first.get_or_insert(error);
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn optional<T>(result: Result<T, SessionError>) -> Result<Option<T>, SessionError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(SessionError::ResponseTimeout(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

fn platform(error: impl std::error::Error + Send + Sync + 'static) -> SessionError {
    SessionError::Platform(Box::new(error))
}

impl<M: SharedMemory, C: Clock> DspSession<M, C> {
    /// One recovered request with its stock wait flag and timeout.
    pub fn request(&mut self, request: Request) -> Result<Delivery, SessionError> {
        let policy = request.send_policy();
        self.request_with(request, policy)
    }

    /// `MeterRequest` always takes the strike-free meter path (vtable
    /// `+0x3c`, VA 0x89b474), whatever the policy.
    fn request_with(
        &mut self,
        request: Request,
        policy: SendPolicy,
    ) -> Result<Delivery, SessionError> {
        if let Request::MeterRequest { core, slot, word } = request {
            let receipt = self.request_meter(core, slot, word)?;
            return Ok(Delivery {
                receipt: Some(receipt),
                consumed: false,
            });
        }
        let frame = RequestFrame::new(request)?;
        self.send(ControlPacket::Request(&frame), policy)
    }

    /// Sends every request; see [`tolerate`].
    fn request_each(
        &mut self,
        requests: impl IntoIterator<Item = (SendPolicy, Request)>,
    ) -> Result<(), SessionError> {
        let mut first = None;
        for (policy, request) in requests {
            let result = self.request_with(request, policy);
            tolerate(&mut first, result)?;
        }
        first.map_or(Ok(()), Err)
    }

    // Boot.

    /// `DSPCommsARM::initialize` (VA 0x89c848) after the platform reports
    /// SOC1 comms ready: `0x54`, `0x55`, `0x57` to core 0 (wait 1 / 500 ms).
    /// With slave boot disabled stock ignores the result; see [`DspSession::boot`].
    pub fn handshake(&mut self) -> Result<(), SessionError> {
        self.request_each(
            [Request::DdrTestA, Request::DdrTestB, Request::DataiMessage]
                .map(|request| (request.send_policy(), request)),
        )
    }

    /// One SOC2 slave-boot frame (tetrad-loader layout), synchronous with the
    /// stock timeout.
    fn slave_boot(
        &mut self,
        kind: SlaveBootKind,
        argument: u32,
        core0_len: u32,
    ) -> Result<Delivery, SessionError> {
        let request = SlaveBootRequest {
            kind,
            argument,
            core0_len,
        };
        let timeout_ms = u32::try_from(request.timeout().as_millis()).unwrap_or(u32::MAX);
        self.send(
            ControlPacket::SlaveBoot(request),
            SendPolicy::new(true, timeout_ms),
        )
    }

    /// Handshake, then the SOC2 slave boot. A failed handshake or `0x12`
    /// marks cores 0..=3 dead, as stock does. `retry_once` is stock flag
    /// `DAT_015630b1` (meaning unresolved): one more `0x12` after a failed boot.
    pub fn boot<S: Soc2Boot>(
        &mut self,
        soc2: &mut S,
        retry_once: bool,
    ) -> Result<SlaveBootOutcome, SessionError> {
        let mut total = 0;
        let started = self.handshake().and_then(|()| {
            total = soc2.buffer_bytes().map_err(platform)?;
            self.slave_boot(SlaveBootKind::BufferMemAddr, total, 0)
        });
        if let Err(error) = started {
            self.mark_dead(&ALL_CORES[..4]);
            return Err(error);
        }
        if std::mem::take(&mut self.drill_core0) {
            return Err(SessionError::ResponseTimeout(CORE0_SILENT));
        }
        let deadline = self.clock.now() + CORE0_ANSWER_WAIT;
        while !(self.events.iter()).any(|(ring, _)| *ring == CoreId::Dsp(DspCore::Core0)) {
            if self.clock.now() >= deadline {
                return Err(SessionError::ResponseTimeout(CORE0_SILENT));
            }
            self.clock.sleep(RESPONSE_POLL_INTERVAL);
            self.poll()?;
        }
        let mut attempts = 0;
        loop {
            let address = self.await_event(
                "slave boot buffer address",
                SLAVE_BOOT_RESPONSE_WAIT,
                |_, event| match event {
                    Event::SlaveBootBufferAddress(body) => Some(
                        body.first_chunk::<4>()
                            .map(|word| u32::from_le_bytes(*word)),
                    ),
                    _ => None,
                },
            )?;
            let address = address.ok_or(SessionError::Refused(
                "slave boot buffer address body is shorter than one word",
            ))?;
            let lengths = match soc2.write_image_pair(address).map_err(platform)? {
                (_, Some(lengths)) => lengths,
                (first, None) => first
                    .checked_next_multiple_of(IMAGE_ALIGN)
                    .and_then(|aligned| total.checked_sub(aligned))
                    .map(|second| Soc2ImageLengths { first, second })
                    .ok_or(SessionError::Refused(
                        "SOC2 buffer size is smaller than the aligned first image",
                    ))?,
            };
            // Stock ignores this send's result (VA 0x89bdec); the response decides.
            tolerate(
                &mut None,
                self.slave_boot(SlaveBootKind::Boot, lengths.first, lengths.second),
            )?;
            attempts += 1;
            let status = self.await_event(
                "slave boot response",
                SLAVE_BOOT_RESPONSE_WAIT,
                |_, event| match event {
                    Event::SlaveBootResponse(word) => Some(*word),
                    _ => None,
                },
            )?;
            if status == 0 {
                // Stock sends stale stack bytes as the argument; this sends 0.
                tolerate(&mut None, self.slave_boot(SlaveBootKind::Finished, 0, 0))?;
                if !soc2.finish().map_err(platform)? {
                    return Ok(SlaveBootOutcome::Soc2CommsFailed);
                }
                self.open_soc2();
                return Ok(SlaveBootOutcome::Soc2Open);
            }
            if !(retry_once && attempts == 1) {
                self.mark_dead(&ALL_CORES[2..]);
                return Ok(SlaveBootOutcome::Soc2Dead);
            }
            total = soc2.buffer_bytes().map_err(platform)?;
            if let Err(error) = self.slave_boot(SlaveBootKind::BufferMemAddr, total, 0) {
                self.mark_dead(&ALL_CORES);
                return Err(error);
            }
        }
    }

    /// Pulls the boot vectors in stock order (VAs 0x875394, 0x876598), each
    /// within `KEY_WAIT` (1 s). The order and the polls are confirmed; what makes
    /// the DSP send them is not visible on the ARM side (inferred: the
    /// handshake).
    pub fn pull_boot_keys(&mut self) -> Result<BootKeys, SessionError> {
        let repo = optional(
            self.await_event("ModelRepo key", KEY_WAIT, |_, event| match event {
                Event::Vector24(key) => Some(key.clone()),
                _ => None,
            }),
        )?;
        let blob = optional(
            self.await_event("blob key", KEY_WAIT, |_, event| match event {
                Event::Vector25(key) => Some(key.clone()),
                _ => None,
            }),
        )?;
        let third =
            optional(
                self.await_event("boot vector 0x27", KEY_WAIT, |_, event| match event {
                    Event::Vector27(key) => Some(key.clone()),
                    _ => None,
                }),
            )?;
        Ok(BootKeys { repo, blob, third })
    }

    // Global controls that stock sends to several cores.

    /// `0x20` to core 0, then core 1 (VA 0x876470). `value = mute | flag << 1`.
    pub fn set_master_mute(&mut self, value: u32) -> Result<(), SessionError> {
        self.request_each([DspCore::Core0, DspCore::Core1].map(|core| {
            let request = Request::MasterMute { core, value };
            (request.send_policy(), request)
        }))
    }

    /// `0x39` to core 2, then the control core (VA 0x514d9c).
    pub fn switch_usb_inputs(&mut self, value_bits: u32) -> Result<(), SessionError> {
        self.request_each(
            [CoreId::Dsp(DspCore::Core2), CoreId::Control].map(|destination| {
                let request = Request::SwitchUsbIns {
                    destination,
                    value_bits,
                };
                (request.send_policy(), request)
            }),
        )
    }

    /// `0x4c` to every DSP core (VA 0x515934).
    pub fn set_dynamic_delay_compensation(&mut self, enabled: bool) -> Result<(), SessionError> {
        self.request_each(DSP_CORES.map(|core| {
            let request = Request::DynamicDelayCompensation {
                core,
                value: u32::from(enabled),
            };
            (request.send_policy(), request)
        }))
    }

    /// `0x48`, or `0x49` for a same-core source, to every DSP core (VA 0x5156b8).
    pub fn route_to_sidechain(
        &mut self,
        same_core: bool,
        channel: u8,
        source_slot: u32,
        source_core: u32,
    ) -> Result<(), SessionError> {
        self.request_each(DSP_CORES.map(|core| {
            let request = Request::RouteToSidechainBuffer {
                core,
                same_core,
                channel,
                source_slot,
                source_core,
            };
            (request.send_policy(), request)
        }))
    }

    // Diagnostics, meters and latency.

    /// `dspDiagnosticsTimerSlot` (VA 0x50ea74): `0x0a` then `0x1a` to cores
    /// 0..=4, then one more `0x1a` to the control core. Stock skips the tick
    /// while `/opt/neuraldsp/nopoll` exists; that check is the caller's.
    pub fn poll_diagnostics(&mut self) -> Result<(), SessionError> {
        let mut requests = Vec::with_capacity(11);
        for core in ALL_CORES {
            requests.push(Request::DiagnosticsRequest { core });
            requests.push(Request::DebugCommsRequest { core });
        }
        requests.push(Request::DebugCommsRequest {
            core: CoreId::Control,
        });
        self.request_each(
            requests
                .into_iter()
                .map(|request| (request.send_policy(), request)),
        )
    }

    /// `syncAllCores` (VA 0x8763e8): `0x0a` to cores 0..=3 with wait 1 /
    /// 1000 ms. Stock logs each failure and continues.
    pub fn sync_all_cores(&mut self) -> Result<(), SessionError> {
        let mut first = None;
        for core in DSP_CORES {
            let request = Request::DiagnosticsRequest {
                core: CoreId::Dsp(core),
            };
            match self.request_with(request, SYNC_POLICY) {
                // Stock `sendMessage` refuses a dead core quietly; the live cores still sync,
                // so one dead SOC2 core must not fail every later deploy.
                Err(SessionError::DeadCore(_)) => {}
                result => tolerate(&mut first, result)?,
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// `0x0a` to one core, then that core's next diagnostics block. Which
    /// word carries CPU load is unresolved; FPEx and error flags are known.
    pub fn diagnostics(
        &mut self,
        core: DspCore,
        timeout: Duration,
    ) -> Result<DiagnosticsBlock, SessionError> {
        self.request(Request::DiagnosticsRequest {
            core: CoreId::Dsp(core),
        })?;
        self.await_event("diagnostics", timeout, |_, event| match event {
            Event::Diagnostics { core: from, block } if *from == core => Some(*block),
            _ => None,
        })
    }

    /// Comms vtable `+0x3c` (VA 0x89b474): `0x0e` with wait 0 / 100 and no
    /// crash accounting. Stock sends it only while DSP polling is enabled.
    pub fn request_meter(
        &mut self,
        core: CoreId,
        slot: u8,
        word: u32,
    ) -> Result<SendReceipt, SessionError> {
        if !self.comms.can_send(core) {
            return Err(SessionError::DeadCore(core));
        }
        let frame = RequestFrame::new(Request::MeterRequest { core, slot, word })?;
        self.publish_waiting(100, |transport| {
            transport.send_control(ControlPacket::Request(&frame))
        })?
        .ok_or(SessionError::NoFreeSlot {
            core,
            message_type: MessageType::MeterRequest as u8,
        })
    }

    /// `0x1b` (VA 0x50edb4, wait 0 / 1000 ms), then the next `0x0b`
    /// response. Stock pairs them only through a Qt signal; taking the next
    /// latency response is inferred.
    pub fn module_latency(
        &mut self,
        target: DeploymentTarget,
        timeout: Duration,
    ) -> Result<ModuleLatency, SessionError> {
        self.request(Request::ModuleLatencyRequest { target })?;
        self.await_event("module latency", timeout, |_, event| match event {
            Event::ModuleLatency(latency) => Some(*latency),
            _ => None,
        })
    }

    // Parameters and bypass.

    /// `0x02` (VA 0x517010), wait 0 / 100.
    pub fn set_parameter(&mut self, update: ParameterUpdate) -> Result<Delivery, SessionError> {
        let packet = ParameterChange::from_template(
            [0; 20],
            update.target,
            update.index,
            f32::from_bits(update.value_bits),
        );
        self.send(
            ControlPacket::Parameter(&packet),
            SendPolicy::new(false, 100),
        )
    }

    /// `0x0c` (VA 0x51711c), wait 0 / 100.
    pub fn set_bypass(&mut self, update: BypassUpdate) -> Result<Delivery, SessionError> {
        self.request(Request::ModelBypass {
            target: update.target,
            state: update.state,
        })
    }

    /// Stock bulk mode (VAs 0x51267c, 0x517010, 0x51711c, 0x511084,
    /// 0x511188). Parameters and bypass states queue per DSP core in order.
    /// A parameter queue that reaches 169 records flushes every core's
    /// parameters. The final flush sends parameters for cores 0..=3, then
    /// bypass records; when any bypass message was sent it syncs all cores
    /// and sends `0x3e` to core 0. Stock deploy uses `Value16` after a full
    /// redeploy and `Value8` for in-place updates (meaning inferred).
    pub fn apply_bulk(
        &mut self,
        mode: BulkMode,
        parameters: &[ParameterUpdate],
        bypasses: &[BypassUpdate],
    ) -> Result<(), SessionError> {
        let mut first = None;
        let mut queues: [Vec<BulkRecord>; 4] = Default::default();
        for update in parameters {
            let queue = &mut queues[update.target.core as usize];
            queue.push(BulkRecord {
                slot: update.target.slot,
                parameter_index: update.index,
                value_bits: update.value_bits,
            });
            if queue.len() == BULK_PARAMETER_RECORDS_MAX {
                self.flush_parameters(mode, &mut queues, &mut first)?;
            }
        }
        self.flush_parameters(mode, &mut queues, &mut first)?;
        let mut bypass: [Vec<BypassRecord>; 4] = Default::default();
        for update in bypasses {
            bypass[update.target.core as usize].push(BypassRecord {
                slot: update.target.slot,
                state: update.state,
            });
        }
        let mut sent = false;
        for (core, records) in DSP_CORES.into_iter().zip(bypass) {
            if records.is_empty() {
                continue;
            }
            let frame = BulkFrame::new(BulkRequest::Bypass { core, records })?;
            let result = self.send(ControlPacket::Bulk(&frame), BULK_SEND_POLICY);
            tolerate(&mut first, result)?;
            sent = true;
        }
        if sent {
            let result = self.sync_all_cores();
            tolerate(&mut first, result)?;
            let result = self.request(Request::BulkModelBypassSync);
            tolerate(&mut first, result)?;
        }
        first.map_or(Ok(()), Err)
    }

    fn flush_parameters(
        &mut self,
        mode: BulkMode,
        queues: &mut [Vec<BulkRecord>; 4],
        first: &mut Option<SessionError>,
    ) -> Result<(), SessionError> {
        for (core, queue) in DSP_CORES.into_iter().zip(queues.iter_mut()) {
            if queue.is_empty() {
                continue;
            }
            let frame = BulkFrame::new(BulkRequest::Parameters {
                core,
                mode,
                records: std::mem::take(queue),
            })?;
            let result = self.send(ControlPacket::Bulk(&frame), BULK_SEND_POLICY);
            tolerate(first, result)?;
        }
        Ok(())
    }

    // Model lifecycle.

    /// `0x1e` to each core (VA 0x582570 → 0x51082c, wait 1 / 500 ms), then
    /// each core's run-list-cleared acknowledgement `0x1c` within
    /// `RUN_LIST_WAIT` (50 ms, VA 0x515890). A missing acknowledgement is a crash
    /// in stock (VA 0x89c680): the core is marked dead and a crash is queued.
    pub fn delete_all_models(&mut self, cores: &[DspCore]) -> Result<(), SessionError> {
        let mut first = None;
        for &core in cores {
            let result = self.request(Request::DeleteAllModels { core });
            tolerate(&mut first, result)?;
        }
        for &core in cores {
            let acknowledged = optional(self.await_event("run list cleared", RUN_LIST_WAIT, {
                |ring, event| match event {
                    Event::Word1c(word)
                        if Dispatcher::for_ring(ring).run_list_core(*word) == core =>
                    {
                        Some(())
                    }
                    _ => None,
                }
            }))?;
            if acknowledged.is_none() {
                let id = CoreId::Dsp(core);
                self.comms.mark_dead(id);
                self.events
                    .push_back((id, Event::Crash(CrashCause::RunListTimeout { core })));
                first.get_or_insert(SessionError::RunListTimeout(core));
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// `0x1c` BulkStartModel for one core (VA 0x510894, wait 0 / 100). The
    /// DSP answers asynchronously with `0x0c` or `0x04`.
    pub fn start_models(
        &mut self,
        core: DspCore,
        records: Vec<StartRecord>,
    ) -> Result<Delivery, SessionError> {
        let frame = BulkFrame::new(BulkRequest::Start { core, records })?;
        self.send(ControlPacket::Bulk(&frame), BULK_SEND_POLICY)
    }

    /// Owned convenience: collects one start result per slot, in arrival
    /// order. Stock only reacts to them (a failure raises the crash dialog);
    /// the matching crash events stay queued.
    pub fn await_model_starts(
        &mut self,
        core: DspCore,
        slots: &[u8],
        timeout: Duration,
    ) -> Result<Vec<(u8, bool)>, SessionError> {
        let deadline = self.clock.now() + timeout;
        let mut pending = slots.to_vec();
        let mut results = Vec::with_capacity(slots.len());
        while !pending.is_empty() {
            let left = deadline.saturating_sub(self.clock.now());
            let wire = core as u32;
            let (slot, started) = self.await_event("model start", left, |_, event| {
                let (from, slot, started) = match *event {
                    Event::ModelStart {
                        core,
                        slot,
                        started,
                    } => (core, slot, started),
                    Event::ModuleStartFailed { core, slot } => (core, slot, false),
                    _ => return None,
                };
                let slot = u8::try_from(slot).ok()?;
                (from == wire && pending.contains(&slot)).then_some((slot, started))
            })?;
            pending.retain(|&other| other != slot);
            results.push((slot, started));
        }
        Ok(results)
    }

    /// [`DspSession::deploy_loads`] with one blob load per slot (no shared
    /// loads; see [`CoreLoad::per_slot`]).
    pub fn deploy<W: SharedMemory>(
        &mut self,
        windows: &mut CoreWindows<W>,
        cores: &[(DspCore, Vec<Option<SlotModel>>)],
    ) -> Result<(), SessionError> {
        let loads: Vec<_> = cores
            .iter()
            .map(|(core, slots)| CoreLoad::per_slot(*core, slots.clone()))
            .collect();
        self.deploy_loads(windows, &loads)
    }

    /// Stock deploy of whole run lists (VAs 0x585464, 0x582570, 0x879100).
    /// Owned order: every listed core is packed and relocated first, so
    /// nothing is sent when one does not fit ([`SessionError::WontFit`]; stock
    /// has already sent `0x1e`). Then: `0x1e` and the run-list wait for
    /// **every** core that holds models (VA 0x582570), listed or not; the
    /// blob loads round-robin over cores 0..=3, one entry per core per turn,
    /// each copied once with type 7 `+2` = its blob-list index (VA 0x878aa0);
    /// `0x1c` per core with models in core order; and `syncAllCores`. After
    /// `0x1e` nothing stops the deploy: each failure is kept and the first is
    /// returned at the end. The master mute and the 20 ms pause are the
    /// caller's. Per-model setup follows through [`crate::setup`].
    pub fn deploy_loads<W: SharedMemory>(
        &mut self,
        windows: &mut CoreWindows<W>,
        cores: &[CoreLoad],
    ) -> Result<(), SessionError> {
        let mut plans: [Option<CorePlan>; 4] = Default::default();
        for load in cores {
            let plan = plan(load)?;
            let entry = &mut plans[load.core as usize];
            if entry.is_some() {
                return Err(SessionError::Refused("core listed twice"));
            }
            *entry = Some(plan);
        }
        let mut first = None;
        let mut keep = |result: Result<(), SessionError>| {
            if let Err(error) = result {
                first.get_or_insert(error);
            }
        };
        let delete: Vec<_> = DSP_CORES
            .into_iter()
            .filter(|core| self.deployed[*core as usize])
            .collect();
        if !delete.is_empty() {
            let result = self.delete_all_models(&delete);
            for core in delete {
                self.deployed[core as usize] = false;
            }
            keep(result);
        }
        let turns = plans.iter().flatten().map(|plan| plan.loads.len()).max();
        for turn in 0..turns.unwrap_or(0) {
            for plan in plans.iter().flatten() {
                let Some(&(slot, Some(ref model))) = plan.loads.get(turn) else {
                    continue;
                };
                let target = DeploymentTarget {
                    core: plan.core,
                    slot,
                };
                for transfer in &model.transfers {
                    keep(self.load_section(
                        windows,
                        target,
                        slot.index(),
                        transfer.address,
                        &transfer.bytes,
                    ));
                }
            }
        }
        for plan in plans.into_iter().flatten() {
            if plan.records.is_empty() {
                continue;
            }
            let core = plan.core;
            keep(self.start_models(core, plan.records).map(drop));
            self.deployed[core as usize] = true;
        }
        keep(self.sync_all_cores());
        first.map_or(Ok(()), Err)
    }

    /// One relocated section to its placed address (stock type 7, VAs
    /// 0x878aa0 → 0x51965c). Cores 0/1: direct copy into the core window.
    /// Cores 2/3: generic fragments with `+2` = `run_index`, which stock sets
    /// to the blob-list index (objdump 0x878d34), and `+4` = the same
    /// window-relative byte address.
    pub fn load_section<W: SharedMemory>(
        &mut self,
        windows: &mut CoreWindows<W>,
        target: DeploymentTarget,
        run_index: u8,
        address: u32,
        bytes: &[u8],
    ) -> Result<(), SessionError> {
        match windows.get_mut(target.core) {
            Some(window) => {
                window.load_model(target, Address::new(address), bytes)?;
            }
            None => {
                let metadata = MessageMetadata {
                    message_type: MessageType::LoadModel as u8,
                    core: CoreId::Dsp(target.core),
                    slot: run_index,
                    argument: address,
                };
                self.send_generic(metadata, bytes)?;
            }
        }
        Ok(())
    }

    /// `ReceiveData` (VA 0x51965c, type `0x0b`). Cores 0/1: read the
    /// DSP-published receive-buffer table (VA 0x54e710), copy into the core
    /// window, then send `0x17` ReceiveDataEnd (wait 1 / 100). Cores 2/3:
    /// generic fragments with `+4` = buffer index. Stock drops oversized or
    /// unaddressed data with a log; this returns `Refused` without writes.
    /// Stock falls back to fragments on cores 0/1 when comms vtable `+0x48`
    /// reports a problem; that fallback is not implemented.
    pub fn send_model_data<W: SharedMemory>(
        &mut self,
        windows: &mut CoreWindows<W>,
        target: DeploymentTarget,
        buffer: u8,
        bytes: &[u8],
    ) -> Result<(), SessionError> {
        let Some(window) = windows.get_mut(target.core) else {
            let metadata = MessageMetadata {
                message_type: MessageType::ReceiveData as u8,
                core: CoreId::Dsp(target.core),
                slot: target.slot.index(),
                argument: u32::from(buffer),
            };
            return self.send_generic(metadata, bytes).map(drop);
        };
        let table = self
            .rings
            .lock()
            .transport
            .receive_buffer(target.core, target.slot, buffer)?
            .ok_or(SessionError::Refused("receive buffer index out of range"))?;
        let length =
            u32::try_from(bytes.len()).map_err(|_| SessionError::Refused("model data length"))?;
        if length > table.bytes {
            return Err(SessionError::Refused("receive buffer is too small"));
        }
        if table.address == 0 {
            return Err(SessionError::Refused(
                "DSP published no receive buffer address",
            ));
        }
        // An empty ReceiveData (the Capture 2 clear, VA 0x5661ec) copies nothing (0x529d88
        // with size 0) and still ends with `0x17`.
        if !bytes.is_empty() {
            window.load_model(target, Address::new(table.address), bytes)?;
        }
        self.request(Request::ReceiveDataEnd {
            target,
            buffer: u32::from(buffer),
            bytes: length,
        })
        .map(drop)
    }
}
