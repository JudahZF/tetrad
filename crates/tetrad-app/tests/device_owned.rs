//! The owned stock-ID map (`w4-f-wire`, review-w4-dsp F2): off by default, accepted only in a
//! bring-up run with `allow_owned`, served only on DSP core 0, and an owned blob deploys
//! through the stock `BlobLoad` path. The catalog is synthetic; the only blob bytes are the
//! owned Volume block's.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use tetrad_app::device::bringup::{RunOptions, Stage};
use tetrad_app::device::owned::{self, InstallError, OwnedLoad};
use tetrad_catalog::owned::OwnedModel;
use tetrad_catalog::{Catalog, Specs};
use tetrad_dsp_host::blob;
use tetrad_dsp_host::model::{CoreWindows, ModelMemory};
use tetrad_dsp_host::session::{Clock, CoreLoad, DspSession, RunSlot};
use tetrad_dsp_host::transport::{DspTransport, MESSAGE_MEMORY_BYTES, RingLayout};
use tetrad_dsp_host::{MemoryError, SharedMemory};
use tetrad_dsp_protocol::address::Address;
use tetrad_dsp_protocol::{CoreId, DspCore};

const VOLUME: u32 = 16_006;
const STOCK: u32 = 1;
const WINDOW_BASE: u32 = 0x24_0000;

const MODELS: &str = "<Models><Category id='16' name='Utility'>\
<Model id='16006' name='Volume' blob='volume'>\
<Parameter name='LEVEL' type='float' min='0' max='100' defaultValue='100'/>\
<Parameter name='CURVE' type='switch' min='0' max='1' defaultValue='0'/></Model>\
<Model id='1' name='Other' blob='other'>\
<Parameter name='GAIN' type='float' min='0' max='10' defaultValue='5'/></Model>\
</Category></Models>";

const REPORT: &str = "<ModuleReport>\
<Module id='16006' name='Volume' blob_name='volume' category='16' dm_heap='0' pm_heap='0' \
sd_heap='0' dma='0' dm='32' pm='0' sw='1000' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='1'/></Module>\
<Module id='1' name='Other' blob_name='other' category='16' dm_heap='0' pm_heap='0' \
sd_heap='0' dma='0' dm='32' pm='0' sw='1000' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='1'/></Module></ModuleReport>";

fn specs() -> Specs {
    Specs::resolve(&Catalog::parse(MODELS, REPORT).unwrap()).unwrap()
}

fn bringup() -> RunOptions {
    RunOptions {
        preset: Some("p.pb".into()),
        ..RunOptions::bringup(Stage::Deploy, Duration::from_secs(60), PathBuf::from("/x"))
    }
}

/// Item 3 and F2: both option sets start with the map off, and with the map off nothing
/// changes. `run` refuses a map (options and install); a bring-up run refuses it unless
/// `allow_owned` is given.
#[test]
fn owned_map_is_off_by_default_and_bringup_only() {
    for options in [&RunOptions::STOCK, &bringup()] {
        assert!(options.owned.is_empty() && !options.allow_owned);
        let mut specs = specs();
        let before = specs.models[&VOLUME].stats;
        let blobs = owned::install(options, &mut specs).unwrap();
        assert!(blobs.load(DspCore::Core0, VOLUME).is_none());
        assert_eq!(specs.models[&VOLUME].stats, before);
    }

    let run = RunOptions {
        owned: BTreeSet::from([OwnedModel::Volume]),
        allow_owned: true,
        ..RunOptions::STOCK
    };
    let refused = RunOptions {
        owned: BTreeSet::from([OwnedModel::Volume]),
        ..bringup()
    };
    for options in [&run, &refused] {
        assert!(options.validate().is_err());
        let mut specs = specs();
        let before = specs.models[&VOLUME].stats;
        assert!(matches!(
            owned::install(options, &mut specs),
            Err(InstallError::Refused)
        ));
        assert_eq!(specs.models[&VOLUME].stats, before);
    }

    let allowed = RunOptions {
        allow_owned: true,
        ..refused
    };
    allowed.validate().unwrap();
    let blobs = owned::install(&allowed, &mut specs()).unwrap();
    assert!(matches!(
        blobs.load(DspCore::Core0, VOLUME),
        Some(OwnedLoad::Owned(OwnedModel::Volume, _))
    ));
}

/// Item 2 and F2: with Volume mapped, the spec takes the owned stats (no code reuse) and the
/// other model stays stock. Only DSP core 0 gets the owned blob; the other cores keep the
/// stock model. Two owned instances deploy on core 0 through `deploy_loads`: the window holds
/// each relocated section and `0x1c` starts each placement's entry.
#[test]
fn owned_blob_deploys_only_on_core_0() {
    let options = RunOptions {
        owned: BTreeSet::from([OwnedModel::Volume]),
        allow_owned: true,
        ..bringup()
    };
    let mut specs = specs();
    let blobs = owned::install(&options, &mut specs).unwrap();
    let stats = specs.models[&VOLUME].stats.unwrap();
    assert!(!stats.reuse_pmco && !specs.models[&VOLUME].reuse_pmco);
    assert!(blobs.load(DspCore::Core0, STOCK).is_none());
    for core in [DspCore::Core1, DspCore::Core2, DspCore::Core3] {
        assert!(matches!(
            blobs.load(core, VOLUME),
            Some(OwnedLoad::Stock(OwnedModel::Volume))
        ));
    }
    let Some(OwnedLoad::Owned(_, load)) = blobs.load(DspCore::Core0, VOLUME) else {
        panic!("core 0 must get the owned blob");
    };
    assert_eq!(load.model_id, VOLUME);
    let module = load.blob.clone().unwrap();
    assert!(stats.dmda as usize >= module.contents[0].bytes.len());

    let memory = Memory::new(MESSAGE_MEMORY_BYTES);
    let sent = Sent::default();
    let clock = Peer {
        now: Duration::ZERO,
        memory: memory.clone(),
        sent: sent.clone(),
    };
    let mut session = DspSession::new(DspTransport::new(memory).unwrap(), clock);
    let core0 = Memory::new(0x16_0000);
    let window = |core, memory| ModelMemory::new(core, Address::new(WINDOW_BASE), memory).unwrap();
    let mut windows = CoreWindows::new(
        window(DspCore::Core0, core0.clone()),
        window(DspCore::Core1, Memory::new(0x16_0000)),
    )
    .unwrap();
    let slot = |index| {
        Some(RunSlot {
            model_id: VOLUME,
            blob: Some(index),
            layout_word: 0,
            block_word: -1,
        })
    };
    let core_load = CoreLoad {
        core: DspCore::Core0,
        blobs: vec![load.clone(), load],
        slots: vec![slot(0), slot(1)],
    };
    session.deploy_loads(&mut windows, &[core_load]).unwrap();

    let placed: Vec<_> = blob::place(&[Some(&module), Some(&module)])
        .unwrap()
        .into_iter()
        .map(|placement| blob::relocate(&module, &placement.unwrap()).unwrap())
        .collect();
    assert_ne!(placed[0].entry, placed[1].entry);
    for transfer in placed.iter().flat_map(|model| &model.transfers) {
        let mut bytes = vec![0; transfer.bytes.len()];
        core0
            .read((transfer.address - WINDOW_BASE) as usize, &mut bytes)
            .unwrap();
        assert_eq!(bytes, transfer.bytes);
    }
    let sent = sent.borrow();
    let (_, start) = sent
        .iter()
        .find(|(id, packet)| *id == CoreId::Dsp(DspCore::Core0) && packet[0] == 0x1c)
        .unwrap();
    for (index, model) in placed.iter().enumerate() {
        assert_eq!(word(start, 16 + 12 * index), model.entry);
    }
}

fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

#[derive(Clone)]
struct Memory(Rc<RefCell<Vec<u8>>>);

impl Memory {
    fn new(bytes: usize) -> Self {
        Self(Rc::new(RefCell::new(vec![0; bytes])))
    }
}

impl SharedMemory for Memory {
    fn len(&self) -> usize {
        self.0.borrow().len()
    }
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<(), MemoryError> {
        let bytes = self.0.borrow();
        let window = bytes.len();
        let data = bytes
            .get(offset..offset + buf.len())
            .ok_or(MemoryError::OutOfBounds {
                offset,
                len: buf.len(),
                window,
            })?;
        buf.copy_from_slice(data);
        Ok(())
    }
    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), MemoryError> {
        let mut bytes = self.0.borrow_mut();
        let window = bytes.len();
        bytes
            .get_mut(offset..offset + data.len())
            .ok_or(MemoryError::OutOfBounds {
                offset,
                len: data.len(),
                window,
            })?
            .copy_from_slice(data);
        Ok(())
    }
    fn read_u32(&self, offset: usize) -> Result<u32, MemoryError> {
        let mut bytes = [0; 4];
        self.read(offset, &mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }
    fn write_u32(&mut self, offset: usize, value: u32) -> Result<(), MemoryError> {
        self.write(offset, &value.to_le_bytes())
    }
    fn acquire_barrier(&mut self) {}
    fn publish_barrier(&mut self) {}
}

type Sent = Rc<RefCell<Vec<(CoreId, Vec<u8>)>>>;

/// A DSP that consumes every outgoing ring frame when the host sleeps.
struct Peer {
    now: Duration,
    memory: Memory,
    sent: Sent,
}

impl Clock for Peer {
    fn now(&self) -> Duration {
        self.now
    }
    fn sleep(&mut self, duration: Duration) {
        self.now += duration;
        let mut memory = self.memory.0.borrow_mut();
        for core in [
            DspCore::Core0,
            DspCore::Core1,
            DspCore::Core2,
            DspCore::Core3,
        ] {
            let core = CoreId::Dsp(core);
            let ring = RingLayout::outgoing(core);
            while memory[ring.consumer_offset] != memory[ring.producer_offset] {
                let slot = (memory[ring.consumer_offset] + 1) % ring.slot_count;
                let length = word(&memory, ring.length_offset + usize::from(slot) * 4) as usize;
                let payload = ring.payload_offset + usize::from(slot) * ring.slot_bytes;
                self.sent
                    .borrow_mut()
                    .push((core, memory[payload..payload + length].to_vec()));
                memory[ring.consumer_offset] = slot;
            }
        }
    }
}
