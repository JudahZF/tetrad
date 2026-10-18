use super::*;
use crate::test_support::{Event, FakeMemory};
use tetrad_dsp_protocol::ModuleSlot;

fn target(core: DspCore) -> DeploymentTarget {
    DeploymentTarget {
        core,
        slot: ModuleSlot::try_from(29).unwrap(),
    }
}

#[test]
fn direct_copy_uses_explicit_byte_window_without_ring_header_or_activation() {
    for core in [DspCore::Core0, DspCore::Core1] {
        let base = Address::new(0x2800_0000);
        let mut memory = FakeMemory::new(16);
        memory.bytes.fill(0xa5);
        let mut models = ModelMemory::new(core, base, memory).unwrap();
        let destination = Address::new(base.value() + 11);
        let bytes = [0xff, 0, 7, 2, 0x80];
        let receipt = models
            .load_model(target(core), destination, &bytes)
            .unwrap();
        assert_eq!(
            receipt,
            ModelWriteReceipt {
                target: target(core),
                destination,
                byte_length: 5
            }
        );
        assert_eq!(&models.memory().bytes[..11], &[0xa5; 11]);
        assert_eq!(&models.memory().bytes[11..], &bytes);
        assert_eq!(
            models.memory().events,
            [
                Event::Bytes {
                    offset: 11,
                    data: bytes.to_vec()
                },
                Event::Barrier
            ]
        );
        assert!(!models.is_faulted());
    }
}

#[test]
fn wrong_core_empty_and_out_of_window_requests_leave_memory_unchanged() {
    let base = Address::new(0x1000);
    let mut models = ModelMemory::new(DspCore::Core0, base, FakeMemory::new(16)).unwrap();
    for core in [DspCore::Core1, DspCore::Core2, DspCore::Core3] {
        assert_eq!(
            models.load_model(target(core), base, &[1]),
            Err(ModelError::CoreMismatch {
                expected: DspCore::Core0,
                requested: core
            })
        );
    }
    assert_eq!(
        models.load_model(target(DspCore::Core0), base, &[]),
        Err(ModelError::EmptyModel)
    );
    for (address, bytes) in [(0xfff, &[1][..]), (0x1010, &[1][..]), (0x100f, &[1, 2][..])] {
        assert_eq!(
            models.load_model(target(DspCore::Core0), Address::new(address), bytes),
            Err(ModelError::OutOfWindow {
                address,
                bytes: bytes.len(),
                base: 0x1000,
                window: 16
            })
        );
    }
    assert_eq!(
        models.load_model(target(DspCore::Core0), Address::new(u32::MAX), &[1, 2]),
        Err(ModelError::AddressOverflow)
    );
    assert_eq!(models.memory().bytes, vec![0; 16]);
    assert!(models.memory().events.is_empty());
    assert!(!models.is_faulted());
    // Refusal does not disable a later admitted write.
    models
        .load_model(target(DspCore::Core0), base, &[1])
        .unwrap();
}

#[test]
fn unsupported_core_and_wrapping_window_are_rejected_at_construction() {
    for core in [DspCore::Core2, DspCore::Core3] {
        assert!(
            matches!(ModelMemory::new(core, Address::new(0), FakeMemory::new(16)), Err(ModelError::UnsupportedCore(value)) if value == core)
        );
    }
    assert!(matches!(
        ModelMemory::new(
            DspCore::Core0,
            Address::new(u32::MAX - 3),
            FakeMemory::new(16)
        ),
        Err(ModelError::AddressOverflow)
    ));
}

#[test]
fn exclusive_end_at_2_to_32_is_valid_but_one_byte_beyond_is_not() {
    let core = DspCore::Core0;
    let base = Address::new(0xffff_fff0);
    let mut models = ModelMemory::new(core, base, FakeMemory::new(16)).unwrap();
    models.load_model(target(core), base, &[0xa5; 16]).unwrap();
    models
        .load_model(target(core), Address::new(u32::MAX), &[0x5a])
        .unwrap();
    assert_eq!(&models.memory().bytes[..15], &[0xa5; 15]);
    assert_eq!(models.memory().bytes[15], 0x5a);
    let events = models.memory().events.len();
    assert_eq!(
        models.load_model(target(core), Address::new(u32::MAX), &[1, 2]),
        Err(ModelError::AddressOverflow)
    );
    assert_eq!(models.memory().events.len(), events);
    assert!(matches!(
        ModelMemory::new(core, base, FakeMemory::new(17)),
        Err(ModelError::AddressOverflow)
    ));
}

#[test]
fn failed_direct_write_faults_window_without_rollback_or_success_receipt() {
    for mutate_on_failure in [false, true] {
        let mut memory = FakeMemory::new(16);
        memory.fail_write = Some(0);
        memory.mutate_on_failure = mutate_on_failure;
        let mut models = ModelMemory::new(DspCore::Core0, Address::new(0x1000), memory).unwrap();
        assert_eq!(
            models.load_model(target(DspCore::Core0), Address::new(0x1003), &[1, 2, 3]),
            Err(ModelError::Memory(MemoryError::Misaligned(3)))
        );
        assert!(models.is_faulted());
        assert_eq!(
            models.memory().events.len(),
            1,
            "failed write must not publish a barrier"
        );
        assert_eq!(
            models.load_model(target(DspCore::Core0), Address::new(0x1003), &[4]),
            Err(ModelError::Faulted)
        );
        let backend = models.into_memory();
        assert_eq!(
            &backend.bytes[3..6],
            if mutate_on_failure {
                &[1, 2, 3]
            } else {
                &[0, 0, 0]
            }
        );
        assert_eq!(backend.events.len(), 1);
    }
}

/// S3 on the QC (2026-10-15): `Worship V5` has a section at SHARC DDR `0x8000_0490` (10340
/// bytes). With an external region it lands there; the L1 window still wins when it fits, and
/// a copy that fits neither is refused with nothing written.
#[test]
fn a_section_outside_l1_goes_to_the_external_region() {
    let core = DspCore::Core0;
    let l1 = Address::new(0x24_0000);
    let section = Address::new(0x8000_0490);
    let mut without = ModelMemory::new(core, l1, FakeMemory::new(16)).unwrap();
    assert!(matches!(
        without.load_model(target(core), section, &[1; 8]),
        Err(ModelError::OutOfWindow { .. })
    ));
    let mut with = ModelMemory::new(core, l1, FakeMemory::new(16))
        .unwrap()
        .with_external(Address::new(0x8000_0000), FakeMemory::new(0x3000))
        .unwrap();
    with.load_model(target(core), section, &[7; 8]).unwrap();
    with.load_model(target(core), Address::new(0x24_0004), &[9; 4])
        .unwrap();
    assert!(matches!(
        with.load_model(target(core), Address::new(0x8000_2ffc), &[1; 8]),
        Err(ModelError::OutOfWindow { .. })
    ));
    let (_, external) = with.external.as_ref().unwrap();
    assert_eq!(&external.bytes[0x490..0x498], &[7; 8]);
    assert_eq!(external.bytes.iter().filter(|&&b| b != 0).count(), 8);
    assert_eq!(&with.memory().bytes[4..8], &[9; 4]);
}
