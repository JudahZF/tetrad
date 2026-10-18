//! Synthetic contract tests for the ES codec kernel ABI. Values come from static
//! analysis notes; no firmware bytes. Expected
//! numbers, bits and nibbles are written out here, not derived from the API.
use std::ops::RangeInclusive;
use tetrad_hal::analog::{DeviceEndpoint, GainControl, GainEndpoint, HardwareType};
use tetrad_hal::codec::*;
use tetrad_hal::requests::IoctlPayload;

/// Every typed target with its exact SET and GET `nr` (doc §2).
const TARGETS: [(Query, u32, u32); 10] = [
    (Query::Impedance(ImpedanceInput::Inst1), 0x00, 0x12),
    (Query::Impedance(ImpedanceInput::Inst2), 0x01, 0x13),
    (Query::Gain(GainChannel::Inst1), 0x02, 0x0a),
    (Query::Gain(GainChannel::Inst2), 0x03, 0x0b),
    (Query::Gain(GainChannel::Ret1), 0x04, 0x0c),
    (Query::Gain(GainChannel::Ret2), 0x05, 0x0d),
    (Query::Flag(Flag::GndLiftXlr1), 0x06, 0x0e),
    (Query::Flag(Flag::GndLiftXlr2), 0x07, 0x0f),
    (Query::Flag(Flag::GndLiftCombo), 0x08, 0x10),
    (Query::Flag(Flag::Phantom48v), 0x09, 0x11),
];

/// Each flag's GPIO mask bit and whether the mask stores it inverted (doc §2).
const FLAG_BITS: [(Flag, u8, bool); 4] = [
    (Flag::GndLiftXlr1, 0x10, false),
    (Flag::GndLiftXlr2, 0x08, false),
    (Flag::GndLiftCombo, 0x08, true),
    (Flag::Phantom48v, 0x10, false),
];

fn masks() -> impl Iterator<Item = GpioMask> {
    (0..8).map(|i| Impedance::try_from(i).unwrap().mask())
}

/// Every code is `_IOW(0xe2, nr, int)`, getters included; adc/kind/direction
/// follow the per-case walk, and each named target reaches its exact pair.
#[test]
fn namespace_has_twenty_iow_codes_and_typed_targets_reach_each_once() {
    let expected: [(Adc, ValueKind); 20] = {
        use Adc::*;
        use ValueKind::*;
        [
            (One, Impedance),
            (Two, Impedance),
            (One, Gain),
            (One, Gain),
            (Two, Gain),
            (Two, Gain),
            (One, Flag),
            (One, Flag),
            (Two, Flag),
            (Two, Flag),
            (One, Gain),
            (One, Gain),
            (Two, Gain),
            (Two, Gain),
            (One, Flag),
            (One, Flag),
            (Two, Flag),
            (Two, Flag),
            (One, Impedance),
            (Two, Impedance),
        ]
    };
    for (nr, (code, (adc, kind))) in CodecIoctl::ALL.into_iter().zip(expected).enumerate() {
        let number = code as u32;
        assert_eq!(number, 0x4004_e200 + nr as u32);
        assert_eq!(number >> 30, 0b01, "{code:?} is _IOW");
        assert_eq!((number >> 16) & 0x3fff, 4, "{code:?} carries one word");
        assert_eq!((number >> 8) & 0xff, 0xe2);
        assert_eq!(number & 0xff, nr as u32);
        let direction = if nr < 10 {
            Direction::Set
        } else {
            Direction::Get
        };
        assert_eq!(
            (code.direction(), code.adc(), code.kind()),
            (direction, adc, kind)
        );
    }

    let mut reached = Vec::new();
    for (query, set_nr, get_nr) in TARGETS {
        let get = query.ioctl();
        assert_eq!(get as u32, 0x4004_e200 + get_nr, "{query:?} GET");
        let request = query.request();
        assert_eq!(request.number, get as u32);
        assert_eq!(
            request.payload,
            IoctlPayload::Integer(GETTER_SENTINEL.to_le_bytes())
        );
        let setting = match query {
            Query::Impedance(input) => Setting::Impedance(input, Impedance::try_from(5).unwrap()),
            Query::Gain(channel) => Setting::Gain(channel, KernelGain::try_from(13).unwrap()),
            Query::Flag(flag) => Setting::Flag(flag, true),
        };
        let set = setting.ioctl();
        assert_eq!(set as u32, 0x4004_e200 + set_nr, "{query:?} SET");
        assert_eq!(setting.request().number, set as u32);
        assert_eq!(setting.target(), query);
        assert_eq!((set.adc(), set.kind()), (get.adc(), get.kind()));
        reached.extend([set, get]);
    }
    reached.sort_by_key(|code| *code as u32);
    assert_eq!(reached, CodecIoctl::ALL);
}

/// Setter words are exact LE values; the kernel nibble range is wider than the
/// HAL PGA cap and is not the same type.
#[test]
fn setter_domains_and_words() {
    for (value, ok) in [(0, true), (8, true), (9, false), (0xff, false)] {
        assert_eq!(Impedance::try_from(value).is_ok(), ok, "impedance {value}");
    }
    for (value, ok) in [(0, true), (10, true), (11, true), (15, true), (16, false)] {
        assert_eq!(KernelGain::try_from(value).is_ok(), ok, "gain {value}");
    }
    let hal = GainControl::new(HardwareType::Type2, GainEndpoint::Input1);
    assert!(hal.write_request(11).is_err());

    for (setting, word) in [
        (
            Setting::Impedance(ImpedanceInput::Inst2, Impedance::try_from(8).unwrap()),
            8u32,
        ),
        (
            Setting::Gain(GainChannel::Ret2, KernelGain::try_from(15).unwrap()),
            15,
        ),
        (Setting::Flag(Flag::GndLiftCombo, true), 1),
        (Setting::Flag(Flag::Phantom48v, false), 0),
    ] {
        assert_eq!(
            setting.request().payload,
            IoctlPayload::Integer(word.to_le_bytes())
        );
    }
    assert_eq!(Acknowledged::from_return(0), Ok(Acknowledged));
    for raw in [1, -1, -5, -14, -22] {
        assert_eq!(Acknowledged::from_return(raw), Err(CodecError::Return(raw)));
    }
}

/// `instz_lut`: i -> i << 3, 8 -> 0. The inverse takes the first match, so a
/// written 8 reads back as 0 and a getter 8 is never a valid index.
#[test]
fn impedance_lut_inverse_and_zero_ambiguity() {
    for index in 0..=8u8 {
        let impedance = Impedance::try_from(index).unwrap();
        let mask = impedance.mask();
        assert_eq!(mask.bits(), if index == 8 { 0 } else { index << 3 });
        assert_eq!(mask.bits() & !0x38, 0);
        assert_eq!(mask.impedance().index(), index % 8);
        assert_eq!(mask.impedance().mask(), mask);
    }
    let query = Query::Impedance(ImpedanceInput::Inst1);
    assert_eq!(
        query.decode(0, 0u32.to_le_bytes()),
        Ok(Reading::Impedance(Impedance::try_from(0).unwrap()))
    );
    assert_eq!(
        query.decode(0, 8u32.to_le_bytes()),
        Err(CodecError::ImpedanceUnavailable)
    );
}

/// gpio_set/gpio_get: each mask bit owns one exact nibble, each flag changes
/// only its exact bit with its polarity, `r0x1b[3:0]` survives, and odd
/// nibbles normalize.
#[test]
fn gpio_nibbles_preserve_and_flags_round_trip() {
    assert_eq!(GpioMask::default().bits(), 0);
    // One-hot truth: bit 3 = r0x1b[7:4], bit 4 = r0x1c[3:0], bit 5 = r0x1c[7:4].
    for (index, written, lone_f) in [
        (1, (0xf0, 0xee), (0xf0, 0x00)),
        (2, (0xe0, 0xef), (0x00, 0x0f)),
        (4, (0xe0, 0xfe), (0x00, 0xf0)),
    ] {
        let mask = Impedance::try_from(index).unwrap().mask();
        assert_eq!(mask.registers(0x00), written, "{mask:?} gpio_set");
        assert_eq!(
            GpioMask::from_registers(lone_f.0, lone_f.1),
            mask,
            "gpio_get"
        );
    }
    for mask in masks() {
        for (flag, bit, inverted) in FLAG_BITS {
            assert_eq!(
                mask.flag(flag),
                (mask.bits() & bit != 0) != inverted,
                "{mask:?} {flag:?}"
            );
            for on in [false, true] {
                let next = mask.with_flag(flag, on);
                let stored = if on != inverted { bit } else { 0 };
                assert_eq!(
                    next.bits(),
                    (mask.bits() & !bit) | stored,
                    "{mask:?} {flag:?} {on}"
                );
                assert_eq!(next.flag(flag), on);
                for low in [0x0, 0x5, 0xf] {
                    let (r1b, r1c) = next.registers(0xa0 | low);
                    assert_eq!(r1b & 0x0f, low);
                    for nibble in [r1b >> 4, r1c & 0xf, r1c >> 4] {
                        assert!(nibble == 0xe || nibble == 0xf);
                    }
                    assert_eq!(GpioMask::from_registers(r1b, r1c), next);
                }
            }
        }
    }
    let odd = GpioMask::from_registers(0x7a, 0x7f);
    assert_eq!(odd.bits(), 0x10);
    assert_eq!(odd.registers(0x7a), (0xea, 0xef));
}

/// Getter decoding never treats a zero return as an answer by itself, and
/// accepts every valid interior word.
#[test]
fn getter_decoding_rejects_non_answers() {
    for (query, ..) in TARGETS {
        for raw in [-13, -5, 1] {
            assert_eq!(
                query.decode(raw, 0u32.to_le_bytes()),
                Err(CodecError::Return(raw))
            );
        }
        assert_eq!(
            query.decode(0, GETTER_SENTINEL.to_le_bytes()),
            Err(CodecError::NoResponse)
        );
        assert_eq!(
            query.decode(0, 0x100u32.to_le_bytes()),
            Err(CodecError::UnexpectedWord(0x100))
        );
        let (valid, rejected): (RangeInclusive<u32>, &[(u32, CodecError)]) = match query {
            Query::Impedance(_) => (0..=7, &[(8, CodecError::ImpedanceUnavailable)]),
            Query::Gain(_) => (
                0..=15,
                &[
                    (16, CodecError::UnexpectedWord(16)),
                    (0xff, CodecError::GainReadFailed),
                ],
            ),
            Query::Flag(_) => (0..=1, &[(2, CodecError::UnexpectedWord(2))]),
        };
        for word in valid {
            let expected = match query {
                Query::Impedance(_) => Reading::Impedance(Impedance::try_from(word as u8).unwrap()),
                Query::Gain(_) => Reading::Gain(KernelGain::try_from(word as u8).unwrap()),
                Query::Flag(_) => Reading::Flag(word != 0),
            };
            assert_eq!(
                query.decode(0, word.to_le_bytes()),
                Ok(expected),
                "{query:?}"
            );
        }
        for &(word, error) in rejected {
            assert_eq!(query.decode(0, word.to_le_bytes()), Err(error), "{query:?}");
        }
    }
}

/// The revision-specific PGA requests in `analog` hit exact kernel channels:
/// inputs are INST1/INST2 on both types, Type1 swaps the returns, Type2 does
/// not (libzenhal `0xa548/0xa55c`, `0xa4a8/0xa480`).
#[test]
fn analog_codec_requests_are_gain_codes_of_this_namespace() {
    use GainEndpoint::{Input1, Input2, Return1, Return2};
    use HardwareType::{Type1, Type2};
    for (hardware, endpoint, channel) in [
        (Type1, Input1, GainChannel::Inst1),
        (Type1, Input2, GainChannel::Inst2),
        (Type1, Return1, GainChannel::Ret2),
        (Type1, Return2, GainChannel::Ret1),
        (Type2, Input1, GainChannel::Inst1),
        (Type2, Input2, GainChannel::Inst2),
        (Type2, Return1, GainChannel::Ret1),
        (Type2, Return2, GainChannel::Ret2),
    ] {
        let control = GainControl::new(hardware, endpoint);
        let write = control.write_request(7).unwrap();
        let setting = Setting::Gain(channel, KernelGain::try_from(7).unwrap());
        assert_eq!(
            (write.device, write.request),
            (DeviceEndpoint::Codec, setting.request()),
            "{hardware:?} {endpoint:?}"
        );
        let read = control.observed_request();
        let get = Query::Gain(channel).ioctl() as u32;
        assert_eq!(
            (read.device, read.number, read.output_length),
            (DeviceEndpoint::Codec, get, 4),
            "{hardware:?} {endpoint:?}"
        );
    }
}
