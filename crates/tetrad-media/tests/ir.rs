use tetrad_media::factory::{
    DeviceCipher, DeviceSerial, FACTORY_IR_BYTES, FactoryIrSeed, SEED_LEN,
};
use tetrad_media::ir::{
    IR_FINAL_FLAG, IR_PARAMETER_ID, IrChannel, IrError, IrInputFormat, IrLimits, IrProvenance,
    IrSelector, IrSourceFormat, IrTransferPlan, MAX_IR_SAMPLES, UNSUPPORTED_OPERATIONS,
    UnsupportedIrOperation, WavChunkKind, WavField, WavPolicy, decode, decode_with_cipher,
};

const LIMITS: IrLimits = IrLimits {
    max_asset_bytes: 64 * 1024,
    max_chunks: 64,
};
const POLICIES: [WavPolicy; 2] = [WavPolicy::RecoveredHeader38, WavPolicy::StandardRiff];
// Synthetic words include both NaN kinds and signs. They are codec fixtures,
// not accepted audio amplitudes or stock IR coefficients.
const WORDS: [u32; 11] = [
    0,
    0x8000_0000,
    0x3f80_0000,
    0xbf00_0000,
    1,
    0x8000_0001,
    0x7f80_0000,
    0xff80_0000,
    0x7fc1_2345,
    0x7f81_2345,
    0xffc5_4321,
];

fn raw(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

fn format(size: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(3_u16.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(48_000_u32.to_le_bytes());
    bytes.extend(192_000_u32.to_le_bytes());
    bytes.extend(4_u16.to_le_bytes());
    bytes.extend(32_u16.to_le_bytes());
    bytes.resize(size, 0);
    bytes
}

fn chunk(id: &[u8; 4], payload: &[u8], padding: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(id);
    bytes.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
    bytes.extend(payload);
    if padding && payload.len() % 2 == 1 {
        bytes.push(0);
    }
    bytes
}

fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = b"RIFF\0\0\0\0WAVE".to_vec();
    for chunk in chunks {
        bytes.extend(chunk);
    }
    update_riff_size(&mut bytes);
    bytes
}

fn update_riff_size(bytes: &mut [u8]) {
    let size = u32::try_from(bytes.len() - 8).unwrap();
    bytes[4..8].copy_from_slice(&size.to_le_bytes());
}

fn wav(payload: &[u8], fmt_size: usize, padding: bool) -> Vec<u8> {
    riff(&[
        chunk(b"fmt ", &format(fmt_size), padding),
        chunk(b"data", payload, padding),
    ])
}

#[test]
fn raw_preserves_all_float_bits_including_nonfinite_payloads() {
    let bytes = raw(&WORDS);
    let samples = decode(&bytes, IrInputFormat::RawLeFloat32, LIMITS).unwrap();
    assert_eq!(samples.bits(), WORDS);
    assert_eq!(samples.to_le_bytes(), bytes);
    assert_eq!(samples.ignored_partial_bytes(), 0);
}

#[test]
fn raw_words_are_little_endian_independent_of_host_float_operations() {
    let bytes = [
        0x45, 0x23, 0xc1, 0x7f, 0, 0, 0, 0x80, 0x12, 0x34, 0x56, 0x78,
    ];
    let samples = decode(&bytes, IrInputFormat::RawLeFloat32, LIMITS).unwrap();
    assert_eq!(samples.bits(), [0x7fc1_2345, 0x8000_0000, 0x7856_3412]);
    assert_eq!(samples.to_le_bytes(), bytes);
}

#[test]
fn raw_selection_tracks_source_count_at_capacity_boundaries() {
    for count in [0, 1, 1023, 1024, 1025, 4097] {
        let words: Vec<u32> = (0..count).map(|index| WORDS[index % WORDS.len()]).collect();
        let bytes = raw(&words);
        let samples = decode(&bytes, IrInputFormat::RawLeFloat32, LIMITS).unwrap();
        assert_eq!(samples.original_sample_count(), count);
        assert_eq!(samples.bits(), &words[..count.min(MAX_IR_SAMPLES)]);
        assert_eq!(samples.is_truncated(), count > MAX_IR_SAMPLES);
        assert_eq!(
            samples.to_le_bytes(),
            &bytes[..4 * count.min(MAX_IR_SAMPLES)]
        );
    }
}

#[test]
fn raw_alignment_checks_the_entire_input_before_bounded_selection() {
    for bytes in (0..8).chain(4096..4104) {
        let input = vec![0; bytes];
        let result = decode(&input, IrInputFormat::RawLeFloat32, LIMITS);
        if bytes % 4 == 0 {
            assert!(result.is_ok());
        } else {
            assert_eq!(result, Err(IrError::RawByteAlignment { bytes }));
        }
    }
}

#[test]
fn explicit_asset_limit_applies_to_full_raw_and_wav_input() {
    let inputs = [
        (raw(&WORDS), IrInputFormat::RawLeFloat32),
        (
            wav(&raw(&WORDS), 18, true),
            IrInputFormat::Wav(WavPolicy::StandardRiff),
        ),
    ];
    for (bytes, format) in inputs {
        assert!(
            decode(
                &bytes,
                format,
                IrLimits {
                    max_asset_bytes: bytes.len(),
                    max_chunks: 2
                }
            )
            .is_ok()
        );
        assert_eq!(
            decode(
                &bytes,
                format,
                IrLimits {
                    max_asset_bytes: bytes.len() - 1,
                    max_chunks: 2
                }
            ),
            Err(IrError::AssetLimit {
                actual: bytes.len(),
                limit: bytes.len() - 1
            })
        );
    }
    assert!(
        decode(
            &[],
            IrInputFormat::RawLeFloat32,
            IrLimits {
                max_asset_bytes: 0,
                max_chunks: 0
            }
        )
        .is_ok()
    );
}

#[test]
fn provenance_assigns_rate_and_channels_only_to_validated_wav() {
    let samples = decode(&raw(&WORDS), IrInputFormat::RawLeFloat32, LIMITS).unwrap();
    assert_eq!(samples.provenance(), IrProvenance::RawProductType1);
    assert_eq!(samples.source_format(), IrSourceFormat::RawLeFloat32);
    assert_eq!(samples.source_format().sample_rate_hz(), None);
    assert_eq!(samples.source_format().channel_count(), None);
    for policy in POLICIES {
        let samples = decode(
            &wav(&raw(&WORDS), 18, true),
            IrInputFormat::Wav(policy),
            LIMITS,
        )
        .unwrap();
        assert_eq!(samples.provenance(), IrProvenance::Wav(policy));
        assert_eq!(samples.source_format(), IrSourceFormat::WavMono48kFloat32);
        assert_eq!(samples.source_format().sample_rate_hz(), Some(48_000));
        assert_eq!(samples.source_format().channel_count(), Some(1));
    }
}

#[test]
fn checked_channel_indices_accept_only_zero_and_one() {
    assert_eq!(IrChannel::try_from(0), Ok(IrChannel::Channel0));
    assert_eq!(IrChannel::try_from(1), Ok(IrChannel::Channel1));
    assert_eq!(IrChannel::Channel0.index(), 0);
    assert_eq!(IrChannel::Channel1.index(), 1);
    for channel in [2, 10, u32::MAX] {
        assert_eq!(IrChannel::try_from(channel), Err(IrError::Channel(channel)));
    }
}

#[test]
fn setter_selectors_are_separate_from_channel_indices() {
    assert_eq!(
        IrSelector::try_from(2).unwrap().channel(),
        IrChannel::Channel0
    );
    assert_eq!(
        IrSelector::try_from(10).unwrap().channel(),
        IrChannel::Channel1
    );
    for selector in [0, 1, 3, 11, u32::MAX] {
        assert_eq!(
            IrSelector::try_from(selector),
            Err(IrError::Selector(selector))
        );
    }
}

#[test]
fn transfer_plan_uses_current_words_without_header_or_zero_padding() {
    for count in [0, 1, 1023, 1024, 1025] {
        let words: Vec<u32> = (0..count).map(|index| WORDS[index % WORDS.len()]).collect();
        let bytes = raw(&words);
        let samples = decode(&bytes, IrInputFormat::RawLeFloat32, LIMITS).unwrap();
        for channel in [IrChannel::Channel0, IrChannel::Channel1] {
            let plan = IrTransferPlan::new(&samples, channel);
            assert_eq!(plan.channel(), channel);
            assert_eq!(plan.parameter_id(), IR_PARAMETER_ID);
            assert_eq!(IR_PARAMETER_ID, 11);
            assert_eq!(plan.final_flag(), IR_FINAL_FLAG);
            assert_eq!(IR_FINAL_FLAG, 1);
            assert_eq!(plan.sample_count(), count.min(1024));
            assert_eq!(plan.byte_count(), 4 * count.min(1024));
            assert_eq!(plan.payload(), &bytes[..4 * count.min(1024)]);
        }
    }
}

#[test]
fn factory_decoding_is_explicitly_unsupported_even_for_aligned_input() {
    for bytes in [
        vec![],
        vec![0; 4096],
        b"RIFFnot a coefficient source".to_vec(),
    ] {
        assert_eq!(
            decode(
                &bytes,
                IrInputFormat::FactoryBin,
                IrLimits {
                    max_asset_bytes: 0,
                    max_chunks: 0
                }
            ),
            Err(IrError::Unsupported(
                UnsupportedIrOperation::FactoryBinDecoder
            ))
        );
    }
    assert_eq!(
        UNSUPPORTED_OPERATIONS,
        [
            UnsupportedIrOperation::RawPointerOverload,
            UnsupportedIrOperation::IrConvolution,
        ]
    );
}

#[test]
fn factory_bin_decodes_only_with_a_device_cipher() {
    let cipher = DeviceCipher::seed_and_serial(
        &FactoryIrSeed::new(&[0x33; SEED_LEN]).unwrap(),
        &DeviceSerial::parse(b"QC1A2B3C4").unwrap(),
    );
    let mut bytes = vec![0; FACTORY_IR_BYTES];
    cipher.apply(&mut bytes);
    let samples = decode_with_cipher(&bytes, IrInputFormat::FactoryBin, LIMITS, &cipher).unwrap();
    assert_eq!(samples.bits().len(), MAX_IR_SAMPLES);
    assert_eq!(
        decode_with_cipher(
            &bytes,
            IrInputFormat::FactoryBin,
            IrLimits {
                max_asset_bytes: FACTORY_IR_BYTES - 1,
                max_chunks: 0,
            },
            &cipher,
        ),
        Err(IrError::AssetLimit {
            actual: FACTORY_IR_BYTES,
            limit: FACTORY_IR_BYTES - 1,
        })
    );
}

#[test]
fn both_wav_policies_preserve_every_selected_float_bit() {
    let payload = raw(&WORDS);
    let bytes = wav(&payload, 18, true);
    for policy in POLICIES {
        let samples = decode(&bytes, IrInputFormat::Wav(policy), LIMITS).unwrap();
        assert_eq!(samples.bits(), WORDS);
        assert_eq!(samples.to_le_bytes(), payload);
        assert_eq!(samples.original_sample_count(), WORDS.len());
        assert!(!samples.is_truncated());
    }
}

#[test]
fn standard_format_16_is_a_deliberate_improvement_over_fixed_header_38() {
    let bytes = wav(&raw(&WORDS), 16, true);
    assert_eq!(&bytes[36..40], b"data");
    assert_eq!(
        decode(&bytes, IrInputFormat::Wav(WavPolicy::StandardRiff), LIMITS)
            .unwrap()
            .bits(),
        WORDS
    );
    assert_eq!(
        decode(
            &bytes,
            IrInputFormat::Wav(WavPolicy::RecoveredHeader38),
            LIMITS
        ),
        Err(IrError::RecoveredHeaderLayout)
    );
}

#[test]
fn standard_walks_unknown_chunks_before_fmt_while_recovered_requires_fmt_first() {
    let bytes = riff(&[
        chunk(b"JUNK", b"ab", true),
        chunk(b"fmt ", &format(18), true),
        chunk(b"data", &raw(&WORDS), true),
    ]);
    assert_eq!(
        decode(&bytes, IrInputFormat::Wav(WavPolicy::StandardRiff), LIMITS)
            .unwrap()
            .bits(),
        WORDS
    );
    assert_eq!(
        decode(
            &bytes,
            IrInputFormat::Wav(WavPolicy::RecoveredHeader38),
            LIMITS
        ),
        Err(IrError::RecoveredHeaderLayout)
    );
}

#[test]
fn wav_rejects_each_unverified_format_channel_rate_and_width() {
    let cases = [
        (20, 1_u16.to_le_bytes().to_vec(), WavField::FormatTag, 1, 3),
        (
            20,
            0xfffe_u16.to_le_bytes().to_vec(),
            WavField::FormatTag,
            0xfffe,
            3,
        ),
        (22, 2_u16.to_le_bytes().to_vec(), WavField::Channels, 2, 1),
        (
            24,
            44_100_u32.to_le_bytes().to_vec(),
            WavField::SampleRate,
            44_100,
            48_000,
        ),
        (
            34,
            16_u16.to_le_bytes().to_vec(),
            WavField::BitsPerSample,
            16,
            32,
        ),
    ];
    for (offset, replacement, field, actual, expected) in cases {
        let mut bytes = wav(&raw(&WORDS), 18, true);
        bytes[offset..offset + replacement.len()].copy_from_slice(&replacement);
        for policy in POLICIES {
            assert_eq!(
                decode(&bytes, IrInputFormat::Wav(policy), LIMITS),
                Err(IrError::WavField {
                    field,
                    actual,
                    expected
                })
            );
        }
    }
}

#[test]
fn arbitrary_fmt_sizes_are_not_generalized_into_float_formats() {
    for size in [0, 15, 17, 19, 40] {
        let bytes = wav(&raw(&WORDS), size, true);
        assert_eq!(
            decode(&bytes, IrInputFormat::Wav(WavPolicy::StandardRiff), LIMITS),
            Err(IrError::UnsupportedFormatSize(size))
        );
        assert_eq!(
            decode(
                &bytes,
                IrInputFormat::Wav(WavPolicy::RecoveredHeader38),
                LIMITS
            ),
            Err(IrError::RecoveredHeaderLayout)
        );
    }
}

#[test]
fn standard_checks_format_consistency_beyond_recovered_four_fields() {
    let cases = [
        (
            28,
            123_u32.to_le_bytes().to_vec(),
            WavField::ByteRate,
            123,
            192_000,
        ),
        (32, 8_u16.to_le_bytes().to_vec(), WavField::BlockAlign, 8, 4),
        (
            36,
            1_u16.to_le_bytes().to_vec(),
            WavField::ExtensionSize,
            1,
            0,
        ),
    ];
    for (offset, replacement, field, actual, expected) in cases {
        let mut bytes = wav(&raw(&WORDS), 18, true);
        bytes[offset..offset + replacement.len()].copy_from_slice(&replacement);
        assert_eq!(
            decode(&bytes, IrInputFormat::Wav(WavPolicy::StandardRiff), LIMITS),
            Err(IrError::WavField {
                field,
                actual,
                expected
            })
        );
        assert_eq!(
            decode(
                &bytes,
                IrInputFormat::Wav(WavPolicy::RecoveredHeader38),
                LIMITS
            )
            .unwrap()
            .bits(),
            WORDS
        );
    }
}

#[test]
fn only_little_endian_riff_wave_containers_are_supported() {
    for signature in [*b"RIFX", *b"RF64", *b"NOPE"] {
        let mut bytes = wav(&[], 18, true);
        bytes[..4].copy_from_slice(&signature);
        for policy in POLICIES {
            assert_eq!(
                decode(&bytes, IrInputFormat::Wav(policy), LIMITS),
                Err(IrError::UnsupportedContainer(signature))
            );
        }
    }
    let mut bytes = wav(&[], 18, true);
    bytes[8..12].copy_from_slice(b"AVI ");
    for policy in POLICIES {
        assert_eq!(
            decode(&bytes, IrInputFormat::Wav(policy), LIMITS),
            Err(IrError::NotWave)
        );
    }
}

#[test]
fn declared_riff_size_is_exact_and_external_trailing_bytes_are_rejected() {
    let source = wav(&raw(&WORDS), 18, true);
    for size in [
        0,
        u32::try_from(source.len() - 9).unwrap(),
        u32::try_from(source.len() - 7).unwrap(),
        u32::MAX,
    ] {
        let mut bytes = source.clone();
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        for policy in POLICIES {
            assert!(matches!(
                decode(&bytes, IrInputFormat::Wav(policy), LIMITS),
                Err(IrError::RiffSize { .. } | IrError::SizeOverflow)
            ));
        }
    }
    let mut bytes = source.clone();
    bytes.push(0);
    for policy in POLICIES {
        assert_eq!(
            decode(&bytes, IrInputFormat::Wav(policy), LIMITS),
            Err(IrError::RiffSize {
                declared_total: source.len(),
                actual: bytes.len()
            })
        );
    }
}

#[test]
fn every_header_and_payload_prefix_is_rejected_with_and_without_size_repair() {
    let source = wav(&raw(&WORDS), 18, true);
    for length in 0..source.len() {
        let mut bytes = source[..length].to_vec();
        for policy in POLICIES {
            assert!(
                decode(&bytes, IrInputFormat::Wav(policy), LIMITS).is_err(),
                "prefix {length} {policy:?}"
            );
        }
        if length >= 12 {
            update_riff_size(&mut bytes);
            for policy in POLICIES {
                assert!(
                    decode(&bytes, IrInputFormat::Wav(policy), LIMITS).is_err(),
                    "repaired prefix {length} {policy:?}"
                );
            }
        }
    }
}

#[test]
fn chunk_bounds_are_checked_after_data_and_for_maximum_declared_payloads() {
    for id in [*b"JUNK", *b"data"] {
        let mut bad_chunk = id.to_vec();
        bad_chunk.extend(u32::MAX.to_le_bytes());
        let before = riff(&[chunk(b"fmt ", &format(18), true), bad_chunk.clone()]);
        let after = riff(&[
            chunk(b"fmt ", &format(18), true),
            chunk(b"data", &raw(&WORDS), true),
            bad_chunk,
        ]);
        for bytes in [before, after] {
            for policy in POLICIES {
                assert!(matches!(
                    decode(&bytes, IrInputFormat::Wav(policy), LIMITS),
                    Err(IrError::ChunkPayloadTruncated { .. } | IrError::SizeOverflow)
                ));
            }
        }
    }
    for size in 1..8 {
        let mut bytes = wav(&raw(&WORDS), 18, true);
        let offset = bytes.len();
        bytes.extend(vec![0; size]);
        update_riff_size(&mut bytes);
        for policy in POLICIES {
            assert_eq!(
                decode(&bytes, IrInputFormat::Wav(policy), LIMITS),
                Err(IrError::ChunkHeaderTruncated { offset })
            );
        }
    }
}

#[test]
fn chunk_budget_counts_fmt_unknown_data_and_chunks_after_data() {
    let bytes = riff(&[
        chunk(b"fmt ", &format(18), true),
        chunk(b"JUNK", &[], true),
        chunk(b"data", &[], true),
        chunk(b"TAIL", &[], true),
    ]);
    for policy in POLICIES {
        for limit in 0..4 {
            assert_eq!(
                decode(
                    &bytes,
                    IrInputFormat::Wav(policy),
                    IrLimits {
                        max_chunks: limit,
                        ..LIMITS
                    }
                ),
                Err(IrError::ChunkLimit { limit })
            );
        }
        assert!(
            decode(
                &bytes,
                IrInputFormat::Wav(policy),
                IrLimits {
                    max_chunks: 4,
                    ..LIMITS
                }
            )
            .is_ok()
        );
    }
}

#[test]
fn odd_chunk_padding_differs_explicitly_between_named_policies() {
    for padding in [false, true] {
        let mut unknown = chunk(b"JUNK", b"x", padding);
        if padding {
            // RIFF pad bytes need not be zero.
            *unknown.last_mut().unwrap() = 0xa5;
        }
        let bytes = riff(&[
            chunk(b"fmt ", &format(18), padding),
            unknown,
            chunk(b"data", &raw(&WORDS), padding),
        ]);
        let recovered = decode(
            &bytes,
            IrInputFormat::Wav(WavPolicy::RecoveredHeader38),
            LIMITS,
        );
        let standard = decode(&bytes, IrInputFormat::Wav(WavPolicy::StandardRiff), LIMITS);
        if padding {
            assert!(recovered.is_err());
            assert_eq!(standard.unwrap().bits(), WORDS);
        } else {
            assert_eq!(recovered.unwrap().bits(), WORDS);
            assert!(standard.is_err());
        }
    }
    let bytes = riff(&[
        chunk(b"fmt ", &format(18), false),
        chunk(b"data", &[], false),
        chunk(b"TAIL", b"x", false),
    ]);
    assert!(
        decode(
            &bytes,
            IrInputFormat::Wav(WavPolicy::RecoveredHeader38),
            LIMITS
        )
        .is_ok()
    );
    assert_eq!(
        decode(&bytes, IrInputFormat::Wav(WavPolicy::StandardRiff), LIMITS),
        Err(IrError::ChunkPaddingTruncated {
            offset: bytes.len()
        })
    );
}

#[test]
fn recovered_floors_partial_data_words_while_standard_requires_alignment() {
    for complete_words in [0, 1, WORDS.len()] {
        for partial in 1..4 {
            let mut payload = raw(&WORDS[..complete_words]);
            payload.extend(vec![0x5a; partial]);
            let bytes = wav(&payload, 18, false);
            let samples = decode(
                &bytes,
                IrInputFormat::Wav(WavPolicy::RecoveredHeader38),
                LIMITS,
            )
            .unwrap();
            assert_eq!(samples.bits(), &WORDS[..complete_words]);
            assert_eq!(samples.original_sample_count(), complete_words);
            assert_eq!(
                samples.ignored_partial_bytes(),
                u8::try_from(partial).unwrap()
            );
            assert!(!samples.is_truncated());
            assert_eq!(samples.to_le_bytes(), raw(&WORDS[..complete_words]));
            let standard_bytes = wav(&payload, 18, true);
            assert_eq!(
                decode(
                    &standard_bytes,
                    IrInputFormat::Wav(WavPolicy::StandardRiff),
                    LIMITS
                ),
                Err(IrError::WavDataAlignment {
                    bytes: payload.len()
                })
            );
        }
    }
}

#[test]
fn wav_chunk_uniqueness_presence_and_order_are_explicit() {
    let fmt = chunk(b"fmt ", &format(18), true);
    let data = chunk(b"data", &[], true);
    let duplicate_fmt = riff(&[fmt.clone(), data.clone(), fmt.clone()]);
    let duplicate_data = riff(&[fmt.clone(), data.clone(), data.clone()]);
    for policy in POLICIES {
        assert_eq!(
            decode(&duplicate_fmt, IrInputFormat::Wav(policy), LIMITS),
            Err(IrError::DuplicateChunk(WavChunkKind::Format))
        );
        assert_eq!(
            decode(&duplicate_data, IrInputFormat::Wav(policy), LIMITS),
            Err(IrError::DuplicateChunk(WavChunkKind::Data))
        );
        assert_eq!(
            decode(
                &riff(std::slice::from_ref(&fmt)),
                IrInputFormat::Wav(policy),
                LIMITS
            ),
            Err(IrError::MissingChunk(WavChunkKind::Data))
        );
    }
    assert_eq!(
        decode(
            &riff(&[]),
            IrInputFormat::Wav(WavPolicy::StandardRiff),
            LIMITS
        ),
        Err(IrError::MissingChunk(WavChunkKind::Format))
    );
    assert_eq!(
        decode(
            &riff(&[data.clone(), fmt]),
            IrInputFormat::Wav(WavPolicy::StandardRiff),
            LIMITS
        ),
        Err(IrError::DataBeforeFormat)
    );
    assert_eq!(
        decode(
            &riff(&[data]),
            IrInputFormat::Wav(WavPolicy::StandardRiff),
            LIMITS
        ),
        Err(IrError::DataBeforeFormat)
    );
}

#[test]
fn complete_even_unknown_chunks_after_data_do_not_change_selected_samples() {
    for words in [&[][..], &WORDS[..]] {
        let bytes = riff(&[
            chunk(b"fmt ", &format(18), true),
            chunk(b"JUNK", b"ab", true),
            chunk(b"data", &raw(words), true),
            chunk(b"TAIL", b"cdef", true),
        ]);
        for policy in POLICIES {
            let samples = decode(&bytes, IrInputFormat::Wav(policy), LIMITS).unwrap();
            assert_eq!(samples.bits(), words);
            assert_eq!(samples.original_sample_count(), words.len());
            let plan = IrTransferPlan::new(&samples, IrChannel::Channel0);
            assert_eq!(plan.payload(), raw(words));
        }
    }
}

#[test]
fn wav_count_truncation_keeps_original_count_without_allocating_declared_capacity() {
    for count in [0, 1, 1023, 1024, 1025, 4097] {
        let words: Vec<u32> = (0..count).map(|index| WORDS[index % WORDS.len()]).collect();
        let bytes = wav(&raw(&words), 18, true);
        for policy in POLICIES {
            let samples = decode(&bytes, IrInputFormat::Wav(policy), LIMITS).unwrap();
            assert_eq!(samples.bits(), &words[..count.min(1024)]);
            assert_eq!(samples.original_sample_count(), count);
            assert_eq!(samples.is_truncated(), count > 1024);
            assert_eq!(samples.ignored_partial_bytes(), 0);
            let plan = IrTransferPlan::new(&samples, IrChannel::Channel1);
            assert_eq!(plan.byte_count(), 4 * count.min(1024));
            assert_eq!(plan.payload(), raw(&words[..count.min(1024)]));
        }
    }
}
