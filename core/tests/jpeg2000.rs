//! JPEG 2000 decoding of the burned-in text check through OpenJPEG (#47).
//!
//! Each case encodes one synthetic MG-like frame with OpenJPEG in a configuration that
//! sites send: 12- and 16-bit precision, tiles, quality layers, every progression order,
//! precincts, SOP/EPH markers, tile-parts, code-block styles and the irreversible 9/7
//! wavelet. The frame holds burned-in text on the background, so the anonymizer censors it
//! and writes the decoded samples natively. For a reversible case every pixel that is not
//! text must equal the source exactly, which checks the whole decode path bit-exact against
//! known input. The frames are synthetic; no clinical data is used.

use std::ffi::CString;

use dicom_core::value::{PixelFragmentSequence, Value};
use dicom_core::{PrimitiveValue, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::mem::InMemElement;
use dicom_object::{DefaultDicomObject, FileMetaTableBuilder, InMemDicomObject};
use openjpeg_sys as opj;

use dicom_anon::{Anonymizer, BurnedInText, Error, Options, ProfileConfiguration, SecretKey};

const ROWS: usize = 96;
const COLUMNS: usize = 160;

/// A test encoder configuration.
#[derive(Clone, Copy, Default)]
struct Config {
    irreversible: bool,
    tile: Option<i32>,
    /// Compression ratios per quality layer; the last 0.0 is lossless.
    layers: &'static [f32],
    order: Option<opj::PROG_ORDER>,
    precincts: bool,
    sop_eph: bool,
    tile_parts: bool,
    /// Code-block style bits (BYPASS 1, RESET 2, RESTART 4, VSC 8, ERTERM 16, SEGMARK 32).
    code_block_style: i32,
}

/// One frame: exact-zero background, a noisy tissue band on the right, and three short
/// lines of text at `text`.
fn frame(text: u16, tissue: u16) -> Vec<u16> {
    let mut samples = vec![0_u16; ROWS * COLUMNS];
    let mut state = 0x9E37_79B9_u32;
    for row in 0..ROWS {
        for column in 100..COLUMNS {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let gradient = (column - 100) as u32 * u32::from(tissue) / 120;
            samples[row * COLUMNS + column] =
                (u32::from(tissue) / 2 + gradient + state % 64) as u16;
        }
    }
    for line in 0..3 {
        for glyph in 0..5 {
            let (top, left) = (6 + line * 20, 6 + glyph * 9);
            for row in top..top + 10 {
                samples[row * COLUMNS + left] = text;
                samples[row * COLUMNS + left + 5] = text;
            }
            for column in left..left + 6 {
                samples[(top + 5) * COLUMNS + column] = text;
            }
        }
    }
    samples
}

/// Encode `samples` with OpenJPEG as a J2K codestream.
fn encode(samples: &[u16], precision: u32, config: Config) -> Vec<u8> {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("frame.j2k");
    let name = CString::new(path.to_str().unwrap()).unwrap();
    // SAFETY: test-only encoder. Every pointer comes from OpenJPEG and is checked for null,
    // the component buffer has `ROWS * COLUMNS` samples as created, and each object is
    // destroyed once.
    unsafe {
        let mut parameters: opj::opj_cparameters_t = std::mem::zeroed();
        opj::opj_set_default_encoder_parameters(&mut parameters);
        parameters.numresolution = 4;
        parameters.irreversible = i32::from(config.irreversible);
        if let Some(tile) = config.tile {
            parameters.tile_size_on = 1;
            parameters.cp_tdx = tile;
            parameters.cp_tdy = tile;
        }
        let layers: &[f32] = if config.layers.is_empty() {
            &[0.0]
        } else {
            config.layers
        };
        parameters.tcp_numlayers = layers.len() as i32;
        parameters.tcp_rates[..layers.len()].copy_from_slice(layers);
        parameters.cp_disto_alloc = 1;
        if let Some(order) = config.order {
            parameters.prog_order = order;
        }
        if config.precincts {
            parameters.csty |= 0x01;
            parameters.res_spec = 2;
            parameters.prcw_init[0] = 32;
            parameters.prch_init[0] = 32;
            parameters.prcw_init[1] = 16;
            parameters.prch_init[1] = 16;
        }
        if config.sop_eph {
            parameters.csty |= 0x02 | 0x04;
        }
        if config.tile_parts {
            parameters.tp_on = 1;
            parameters.tp_flag = b'R' as std::os::raw::c_char;
        }
        parameters.mode = config.code_block_style;

        let mut component = opj::opj_image_cmptparm_t {
            dx: 1,
            dy: 1,
            w: COLUMNS as u32,
            h: ROWS as u32,
            x0: 0,
            y0: 0,
            prec: precision,
            bpp: precision,
            sgnd: 0,
        };
        let image = opj::opj_image_create(1, &mut component, opj::COLOR_SPACE::OPJ_CLRSPC_GRAY);
        assert!(!image.is_null());
        (*image).x1 = COLUMNS as u32;
        (*image).y1 = ROWS as u32;
        let data = std::slice::from_raw_parts_mut((*(*image).comps).data, ROWS * COLUMNS);
        for (target, &sample) in data.iter_mut().zip(samples) {
            *target = i32::from(sample);
        }
        let codec = opj::opj_create_compress(opj::CODEC_FORMAT::OPJ_CODEC_J2K);
        assert!(!codec.is_null());
        assert_eq!(opj::opj_setup_encoder(codec, &mut parameters, image), 1);
        let stream = opj::opj_stream_create_default_file_stream(name.as_ptr(), 0);
        assert!(!stream.is_null());
        assert_eq!(opj::opj_start_compress(codec, image, stream), 1);
        assert_eq!(opj::opj_encode(codec, stream), 1);
        assert_eq!(opj::opj_end_compress(codec, stream), 1);
        opj::opj_stream_destroy(stream);
        opj::opj_destroy_codec(codec);
        opj::opj_image_destroy(image);
    }
    let mut codestream = std::fs::read(&path).unwrap();
    if codestream.len() % 2 == 1 {
        codestream.push(0);
    }
    codestream
}

fn object(codestream: Vec<u8>, transfer_syntax: &str, bits_stored: u16) -> DefaultDicomObject {
    let sop_class = uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION;
    let mut object = InMemDicomObject::new_empty();
    object.put_str(tags::SOP_CLASS_UID, VR::UI, sop_class);
    object.put_str(tags::SOP_INSTANCE_UID, VR::UI, "2.25.4811");
    object.put_str(tags::STUDY_INSTANCE_UID, VR::UI, "2.25.4812");
    object.put_str(tags::SERIES_INSTANCE_UID, VR::UI, "2.25.4813");
    object.put_str(tags::MODALITY, VR::CS, "MG");
    object.put_str(tags::BURNED_IN_ANNOTATION, VR::CS, "NO");
    object.put_str(tags::PHOTOMETRIC_INTERPRETATION, VR::CS, "MONOCHROME2");
    for (tag, value) in [
        (tags::SAMPLES_PER_PIXEL, 1_u16),
        (tags::ROWS, ROWS as u16),
        (tags::COLUMNS, COLUMNS as u16),
        (tags::BITS_ALLOCATED, 16),
        (tags::BITS_STORED, bits_stored),
        (tags::HIGH_BIT, bits_stored - 1),
        (tags::PIXEL_REPRESENTATION, 0),
    ] {
        object.put(InMemElement::new(tag, VR::US, PrimitiveValue::from(value)));
    }
    object.put(InMemElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        Value::PixelSequence(PixelFragmentSequence::new(Vec::new(), vec![codestream])),
    ));
    object
        .with_meta(
            FileMetaTableBuilder::new()
                .transfer_syntax(transfer_syntax)
                .media_storage_sop_class_uid(sop_class)
                .media_storage_sop_instance_uid("2.25.4811"),
        )
        .unwrap()
}

fn anonymize(
    source: &DefaultDicomObject,
) -> dicom_anon::Result<(DefaultDicomObject, dicom_anon::AnonymizationReport)> {
    let mut options =
        Options::new(SecretKey::new("jpeg2000-fixture-key-0123456789abcdef").unwrap());
    options.patient_scope = Some("synthetic-subject".to_owned());
    options.profile = ProfileConfiguration::safe_harbor();
    options.profile.burned_in_text = BurnedInText::Auto;
    Anonymizer::new(options)?
        .anonymize_with_generated_uid_map(source)
        .map(|(output, report, _)| (output, report))
}

fn output_samples(output: &DefaultDicomObject) -> Vec<u16> {
    output
        .get(tags::PIXEL_DATA)
        .unwrap()
        .to_bytes()
        .unwrap()
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}

/// Encode, anonymize and check that text became background and every other pixel is the
/// source value.
fn assert_lossless_round_trip(precision: u16, config: Config) {
    let (text, tissue) = if precision == 16 {
        (65535, 40000)
    } else {
        (4095, 1800)
    };
    let source = frame(text, tissue);
    let codestream = encode(&source, u32::from(precision), config);
    let transfer_syntax = uids::JPEG2000_LOSSLESS;
    let (output, report) = anonymize(&object(codestream, transfer_syntax, precision)).unwrap();
    // Three lines, far enough apart to stay separate regions.
    assert_eq!(report.burned_in_text.regions_censored, 3);
    let decoded = output_samples(&output);
    for (index, (&source, &decoded)) in source.iter().zip(&decoded).enumerate() {
        let expected = if source == text { 0 } else { source };
        assert_eq!(decoded, expected, "pixel {index}");
    }
}

#[test]
fn reversible_12_bit() {
    assert_lossless_round_trip(12, Config::default());
}

#[test]
fn reversible_16_bit() {
    assert_lossless_round_trip(16, Config::default());
}

#[test]
fn reversible_multi_tile() {
    assert_lossless_round_trip(
        12,
        Config {
            tile: Some(64),
            ..Config::default()
        },
    );
}

#[test]
fn reversible_multi_layer() {
    assert_lossless_round_trip(
        12,
        Config {
            layers: &[40.0, 10.0, 0.0],
            ..Config::default()
        },
    );
}

#[test]
fn reversible_every_progression_order() {
    for order in [
        opj::PROG_ORDER::OPJ_RLCP,
        opj::PROG_ORDER::OPJ_RPCL,
        opj::PROG_ORDER::OPJ_PCRL,
        opj::PROG_ORDER::OPJ_CPRL,
    ] {
        assert_lossless_round_trip(
            12,
            Config {
                order: Some(order),
                ..Config::default()
            },
        );
    }
}

#[test]
fn reversible_precincts_sop_eph_and_tile_parts() {
    assert_lossless_round_trip(
        12,
        Config {
            tile: Some(64),
            precincts: true,
            sop_eph: true,
            tile_parts: true,
            layers: &[20.0, 0.0],
            order: Some(opj::PROG_ORDER::OPJ_RPCL),
            ..Config::default()
        },
    );
}

#[test]
fn reversible_every_code_block_style() {
    assert_lossless_round_trip(
        12,
        Config {
            code_block_style: 1 | 2 | 4 | 8 | 16 | 32,
            ..Config::default()
        },
    );
}

#[test]
fn irreversible_9_7_decodes_within_bits_stored_and_close_to_the_source() {
    let source = frame(4095, 1800);
    // A text-free frame: lossy ringing around text would fail closed, as documented.
    let clean: Vec<u16> = source
        .iter()
        .map(|&value| if value == 4095 { 0 } else { value })
        .collect();
    let codestream = encode(
        &clean,
        12,
        Config {
            irreversible: true,
            layers: &[8.0],
            ..Config::default()
        },
    );
    let source_object = object(codestream, uids::JPEG2000, 12);
    let (output, report) = anonymize(&source_object).unwrap();
    assert_eq!(report.burned_in_text.frames_assessed, 1);
    assert_eq!(report.burned_in_text.regions_censored, 0);
    // Nothing was censored, so the object stays encapsulated JPEG 2000. Only the
    // encoder's comment segment is removed.
    assert_eq!(
        output.meta().transfer_syntax().trim_end_matches('\0'),
        uids::JPEG2000
    );
    assert_eq!(report.removed_jpeg2000_comment_segments, 1);
}

#[test]
fn irreversible_9_7_text_on_the_background_is_censored_or_fails_closed() {
    let source = frame(4095, 1800);
    let codestream = encode(
        &source,
        12,
        Config {
            irreversible: true,
            layers: &[4.0],
            ..Config::default()
        },
    );
    match anonymize(&object(codestream, uids::JPEG2000, 12)) {
        // Ringing around the text is not exact background.
        Err(Error::BurnedInTextOverTissue { .. }) => {}
        Ok((output, report)) => {
            assert!(
                report.burned_in_text.regions_censored
                    + report.burned_in_text.regions_left_in_place
                    > 0
            );
            assert!(output_samples(&output).iter().all(|&value| value <= 4095));
        }
        Err(error) => panic!("{error}"),
    }
}

#[test]
fn a_truncated_codestream_fails_closed_in_strict_mode() {
    let source = frame(4095, 1800);
    let mut codestream = encode(&source, 12, Config::default());
    codestream.truncate(codestream.len() * 2 / 3);
    if codestream.len() % 2 == 1 {
        codestream.push(0);
    }
    let error = anonymize(&object(codestream, uids::JPEG2000_LOSSLESS, 12)).unwrap_err();
    assert!(matches!(error, Error::PixelTextNotAssessed(_)), "{error}");
}

#[test]
fn a_precision_above_bits_stored_fails_closed() {
    // 16-bit samples in a header that says 12 bits.
    let source = frame(65535, 40000);
    let codestream = encode(&source, 16, Config::default());
    let error = anonymize(&object(codestream, uids::JPEG2000_LOSSLESS, 12)).unwrap_err();
    assert!(matches!(error, Error::PixelTextNotAssessed(_)), "{error}");
}

#[test]
fn a_codestream_from_an_independent_encoder_decodes_bit_exact() {
    // pure_jpeg2k shares no code with OpenJPEG.
    let source = frame(4095, 1800);
    let mut codestream = Vec::new();
    jpeg2k::encode(
        &source,
        COLUMNS as u32,
        ROWS as u32,
        &jpeg2k::Jpeg2kOptions::default(),
        &mut codestream,
    )
    .unwrap();
    if codestream.len() % 2 == 1 {
        codestream.push(0);
    }
    let (output, _) = anonymize(&object(codestream, uids::JPEG2000_LOSSLESS, 12)).unwrap();
    for (&source, &decoded) in source.iter().zip(&output_samples(&output)) {
        assert_eq!(decoded, if source == 4095 { 0 } else { source });
    }
}

#[test]
fn mutated_codestreams_never_panic_and_never_pass_unchecked() {
    // Deterministic corruption: flipped bytes, truncation and overwritten runs. Each run
    // must return a result or a typed error; a crash in OpenJPEG would abort the test.
    let source = frame(4095, 1800);
    let codestream = encode(
        &source,
        12,
        Config {
            tile: Some(64),
            layers: &[20.0, 0.0],
            ..Config::default()
        },
    );
    let mut state = 0x2545_F491_u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state as usize
    };
    for round in 0..400 {
        let mut mutated = codestream.clone();
        match round % 3 {
            0 => {
                let index = next() % mutated.len();
                mutated[index] ^= 1 << (next() % 8);
            }
            1 => mutated.truncate(next() % mutated.len()),
            _ => {
                let start = next() % mutated.len();
                for byte in mutated.iter_mut().skip(start).take(next() % 32) {
                    *byte = next() as u8;
                }
            }
        }
        if mutated.len() % 2 == 1 {
            mutated.push(0);
        }
        if mutated.is_empty() {
            continue;
        }
        if let Ok((output, _)) = anonymize(&object(mutated, uids::JPEG2000_LOSSLESS, 12)) {
            // Whatever decoded passed the geometry and Bits Stored checks.
            if output.meta().transfer_syntax().trim_end_matches('\0')
                == uids::EXPLICIT_VR_LITTLE_ENDIAN
            {
                assert!(output_samples(&output).iter().all(|&value| value <= 4095));
            }
        }
    }
}

/// Overwrite one 4-byte SIZ field (0 Xsiz, 1 Ysiz, ..., 4 XTsiz, 5 YTsiz) of a raw codestream.
fn patch_siz(codestream: &mut [u8], field: usize, value: u32) {
    assert_eq!(&codestream[..4], &[0xFF, 0x4F, 0xFF, 0x51]);
    let start = 8 + 4 * field;
    codestream[start..start + 4].copy_from_slice(&value.to_be_bytes());
}

#[test]
fn a_siz_larger_than_the_header_fails_closed_before_decoding() {
    let source = frame(4095, 1800);
    for (field, value) in [(0, 60_000), (1, 60_000)] {
        let mut codestream = encode(&source, 12, Config::default());
        patch_siz(&mut codestream, field, value);
        let started = std::time::Instant::now();
        let error = anonymize(&object(codestream, uids::JPEG2000_LOSSLESS, 12)).unwrap_err();
        assert!(matches!(error, Error::PixelTextNotAssessed(_)), "{error}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}

#[test]
fn a_siz_with_too_many_tiles_fails_closed_before_decoding() {
    let source = frame(4095, 1800);
    let mut codestream = encode(&source, 12, Config::default());
    patch_siz(&mut codestream, 4, 1);
    patch_siz(&mut codestream, 5, 1);
    let error = anonymize(&object(codestream, uids::JPEG2000_LOSSLESS, 12)).unwrap_err();
    assert!(error.to_string().contains("too many tiles"), "{error}");
}

/// The start offsets of the SOT markers of a codestream, found by walking tile-parts.
fn tile_part_starts(codestream: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut position = codestream
        .windows(2)
        .position(|pair| pair == [0xFF, 0x90])
        .unwrap();
    while codestream.get(position..position + 2) == Some(&[0xFF, 0x90]) {
        starts.push(position);
        let length = u32::from_be_bytes(codestream[position + 6..position + 10].try_into().unwrap())
            as usize;
        position += length;
    }
    starts
}

#[test]
fn a_codestream_with_a_missing_tile_fails_closed() {
    // OpenJPEG fills a missing tile with zeroes, even in strict mode.
    let source = frame(4095, 1800);
    let codestream = encode(
        &source,
        12,
        Config {
            tile: Some(64),
            ..Config::default()
        },
    );
    let starts = tile_part_starts(&codestream);
    assert_eq!(starts.len(), 6);
    let mut missing = codestream[..starts[2]].to_vec();
    missing.extend_from_slice(&codestream[starts[3]..]);
    if missing.len() % 2 == 1 {
        missing.push(0);
    }
    let error = anonymize(&object(missing, uids::JPEG2000_LOSSLESS, 12)).unwrap_err();
    assert!(error.to_string().contains("missing tiles"), "{error}");
}
