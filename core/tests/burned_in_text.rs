//! Burned-in text censoring (issue #47).
//!
//! All objects are synthetic: a dark background, a band of tissue, and text drawn into the
//! pixels. The text is a name and an MRN, or a laterality marker. Burned In Annotation is
//! set to `NO`, which is wrong for these objects, so only the pixel check can find the text.

use dicom_core::value::{PixelFragmentSequence, Value};
use dicom_core::{PrimitiveValue, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::mem::InMemElement;
use dicom_object::{DefaultDicomObject, FileMetaTableBuilder, InMemDicomObject};

use dicom_anon::{
    validate_source_object, Anonymizer, BurnedInText, Error, Options, ProfileConfiguration,
    SecretKey,
};

const KEY: &str = "burned-in-text-fixture-key-0123456789";
const ROWS: usize = 96;
const COLUMNS: usize = 192;
const TISSUE: u16 = 1800;
const TEXT: u16 = 4095;
const TISSUE_COLUMNS: std::ops::Range<usize> = 120..192;

/// A 5x7 bitmap font for the characters used here, one string per row.
fn glyph(character: char) -> [&'static str; 7] {
    match character {
        'D' => [
            "#### ", "#   #", "#   #", "#   #", "#   #", "#   #", "#### ",
        ],
        'O' => [
            " ### ", "#   #", "#   #", "#   #", "#   #", "#   #", " ### ",
        ],
        'E' => [
            "#####", "#    ", "#    ", "#### ", "#    ", "#    ", "#####",
        ],
        'M' => [
            "#   #", "## ##", "# # #", "#   #", "#   #", "#   #", "#   #",
        ],
        'R' => [
            "#### ", "#   #", "#   #", "#### ", "# #  ", "#  # ", "#   #",
        ],
        'N' => [
            "#   #", "##  #", "# # #", "#  ##", "#   #", "#   #", "#   #",
        ],
        'L' => [
            "#    ", "#    ", "#    ", "#    ", "#    ", "#    ", "#####",
        ],
        '4' => [
            "   # ", "  ## ", " # # ", "#  # ", "#####", "   # ", "   # ",
        ],
        '2' => [
            " ### ", "#   #", "    #", "   # ", "  #  ", " #   ", "#####",
        ],
        _ => ["     "; 7],
    }
}

/// Draw `text` at twice the font size, so each glyph is 14 pixels high.
fn draw(samples: &mut [u16], top: usize, left: usize, text: &str, value: u16) {
    for (index, character) in text.chars().enumerate() {
        let x = left + index * 12;
        for (row, line) in glyph(character).iter().enumerate() {
            for (column, cell) in line.chars().enumerate() {
                if cell == '#' {
                    for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                        samples[(top + 2 * row + dy) * COLUMNS + x + 2 * column + dx] = value;
                    }
                }
            }
        }
    }
}

/// One frame: background, a tissue band on the right, a name, an MRN and a marker.
fn frame() -> Vec<u16> {
    let mut samples = vec![0_u16; ROWS * COLUMNS];
    for row in 0..ROWS {
        for column in TISSUE_COLUMNS {
            samples[row * COLUMNS + column] = TISSUE + (row as u16 % 7) * 10;
        }
    }
    draw(&mut samples, 4, 4, "DOE", TEXT);
    draw(&mut samples, 24, 4, "MRN 42", TEXT);
    draw(&mut samples, 70, 4, "L", TEXT);
    samples
}

fn object(
    sop_class: &str,
    frames: usize,
    pixel_data: InMemElement,
    transfer_syntax: &str,
) -> DefaultDicomObject {
    let sop_instance = "2.25.4711";
    let mut object = InMemDicomObject::new_empty();
    object.put_str(tags::SOP_CLASS_UID, VR::UI, sop_class);
    object.put_str(tags::SOP_INSTANCE_UID, VR::UI, sop_instance);
    object.put_str(tags::STUDY_INSTANCE_UID, VR::UI, "2.25.4712");
    object.put_str(tags::SERIES_INSTANCE_UID, VR::UI, "2.25.4713");
    object.put_str(tags::MODALITY, VR::CS, "MG");
    object.put_str(tags::PATIENT_NAME, VR::PN, "Doe^Jane");
    object.put_str(tags::PATIENT_ID, VR::LO, "MRN-42");
    object.put_str(tags::BURNED_IN_ANNOTATION, VR::CS, "NO");
    object.put_str(tags::PHOTOMETRIC_INTERPRETATION, VR::CS, "MONOCHROME2");
    for (tag, value) in [
        (tags::SAMPLES_PER_PIXEL, 1_u16),
        (tags::ROWS, ROWS as u16),
        (tags::COLUMNS, COLUMNS as u16),
        (tags::BITS_ALLOCATED, 16),
        (tags::BITS_STORED, 12),
        (tags::HIGH_BIT, 11),
        (tags::PIXEL_REPRESENTATION, 0),
    ] {
        object.put(InMemElement::new(tag, VR::US, PrimitiveValue::from(value)));
    }
    if frames > 1 {
        object.put_str(tags::NUMBER_OF_FRAMES, VR::IS, frames.to_string());
    }
    object.put(pixel_data);
    object
        .with_meta(
            FileMetaTableBuilder::new()
                .transfer_syntax(transfer_syntax)
                .media_storage_sop_class_uid(sop_class)
                .media_storage_sop_instance_uid(sop_instance),
        )
        .unwrap()
}

fn native(sop_class: &str, frames: &[Vec<u16>]) -> DefaultDicomObject {
    let pixel_data = InMemElement::new(
        tags::PIXEL_DATA,
        VR::OW,
        PrimitiveValue::U16(frames.concat().into()),
    );
    object(
        sop_class,
        frames.len(),
        pixel_data,
        uids::EXPLICIT_VR_LITTLE_ENDIAN,
    )
}

fn jpeg2000(frames: &[Vec<u16>]) -> DefaultDicomObject {
    jpeg2000_as(frames, uids::JPEG2000_LOSSLESS)
}

fn jpeg2000_as(frames: &[Vec<u16>], transfer_syntax: &str) -> DefaultDicomObject {
    let fragments: Vec<Vec<u8>> = frames
        .iter()
        .map(|samples| {
            let mut encoded = Vec::new();
            jpeg2k::encode(
                samples,
                COLUMNS as u32,
                ROWS as u32,
                &jpeg2k::Jpeg2kOptions::default(),
                &mut encoded,
            )
            .unwrap();
            if encoded.len() % 2 == 1 {
                encoded.push(0);
            }
            encoded
        })
        .collect();
    let pixel_data = InMemElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        Value::PixelSequence(PixelFragmentSequence::new(Vec::new(), fragments)),
    );
    object(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        frames.len(),
        pixel_data,
        transfer_syntax,
    )
}

fn options(burned_in_text: BurnedInText) -> Options {
    let mut options = Options::new(SecretKey::new(KEY).unwrap());
    options.patient_scope = Some("synthetic-subject".to_owned());
    options.profile = ProfileConfiguration::safe_harbor();
    options.profile.burned_in_text = burned_in_text;
    options
}

fn samples(object: &DefaultDicomObject) -> Vec<u16> {
    let element = object.get(tags::PIXEL_DATA).unwrap();
    match element.value() {
        Value::Primitive(PrimitiveValue::U16(values)) => values.to_vec(),
        _ => element
            .to_bytes()
            .unwrap()
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect(),
    }
}

fn assert_censored(source: &[u16], output: &[u16]) {
    assert!(
        output.iter().all(|&value| value != TEXT),
        "a text pixel remains"
    );
    for row in 0..ROWS {
        for column in TISSUE_COLUMNS {
            let index = row * COLUMNS + column;
            assert_eq!(
                output[index], source[index],
                "tissue changed at {row},{column}"
            );
        }
    }
}

#[test]
fn text_in_a_mammogram_with_a_wrong_header_is_censored() {
    let source_frame = frame();
    let source = native(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        std::slice::from_ref(&source_frame),
    );
    let mut source = source;
    source.put(InMemElement::new(
        tags::LARGEST_IMAGE_PIXEL_VALUE,
        VR::US,
        PrimitiveValue::from(TEXT),
    ));
    let anonymizer = Anonymizer::new(options(BurnedInText::Auto)).unwrap();
    let (output, report, uid_map) = anonymizer
        .anonymize_with_generated_uid_map(&source)
        .unwrap();

    assert!(report.burned_in_text_censoring);
    assert_eq!(report.burned_in_text.frames_assessed, 1);
    assert_eq!(report.burned_in_text.regions_censored, 3);
    assert!(report.burned_in_text.pixels_censored > 0);
    assert_censored(&source_frame, &samples(&output));
    assert!(output.get(tags::LARGEST_IMAGE_PIXEL_VALUE).is_none());
    assert_eq!(
        output
            .element(tags::BURNED_IN_ANNOTATION)
            .unwrap()
            .to_str()
            .unwrap()
            .trim(),
        "NO"
    );

    // Source-aware validation reconstructs the same censored output.
    let mut validation = options(BurnedInText::Auto);
    validation.profile.uid_mappings = uid_map;
    let report = validate_source_object(&source, &output, validation).unwrap();
    assert!(report.conformant, "{:#?}", report.findings);
}

#[test]
fn every_tomosynthesis_frame_is_censored() {
    let frames: Vec<Vec<u16>> = (0..3).map(|_| frame()).collect();
    let source = native(uids::BREAST_TOMOSYNTHESIS_IMAGE_STORAGE, &frames);
    let (output, report, _) = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap();

    assert_eq!(report.burned_in_text.frames_assessed, 3);
    assert_eq!(report.burned_in_text.regions_censored, 9);
    let output = samples(&output);
    for (index, frame) in frames.iter().enumerate() {
        assert_censored(
            frame,
            &output[index * ROWS * COLUMNS..(index + 1) * ROWS * COLUMNS],
        );
    }
}

#[test]
fn a_censored_jpeg2000_object_is_written_losslessly_as_native() {
    let source_frame = frame();
    let source = jpeg2000(std::slice::from_ref(&source_frame));
    let (output, report, _) = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap();

    assert_eq!(report.burned_in_text.regions_censored, 3);
    assert_eq!(
        output.meta().transfer_syntax().trim_end_matches('\0'),
        uids::EXPLICIT_VR_LITTLE_ENDIAN
    );
    assert!(output
        .get(tags::ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH)
        .is_none());
    assert_censored(&source_frame, &samples(&output));
}

#[test]
fn an_object_without_text_keeps_its_pixel_data_and_transfer_syntax() {
    let mut clean = vec![0_u16; ROWS * COLUMNS];
    for row in 0..ROWS {
        for column in TISSUE_COLUMNS {
            clean[row * COLUMNS + column] = TISSUE;
        }
    }
    let source = jpeg2000(&[clean]);
    let (output, report, _) = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap();

    assert_eq!(report.burned_in_text.regions_censored, 0);
    assert_eq!(
        output.meta().transfer_syntax(),
        source.meta().transfer_syntax()
    );
    assert_eq!(
        output.get(tags::PIXEL_DATA).unwrap().value(),
        source.get(tags::PIXEL_DATA).unwrap().value()
    );
}

#[test]
fn a_lossless_jpeg_object_keeps_its_untouched_samples() {
    let source_frame = frame();
    let mut encoded = Vec::new();
    jpegli::encode(&source_frame, COLUMNS as u32, ROWS as u32, &mut encoded).unwrap();
    if encoded.len() % 2 == 1 {
        encoded.push(0);
    }
    let pixel_data = InMemElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        Value::PixelSequence(PixelFragmentSequence::new(Vec::new(), vec![encoded])),
    );
    let source = object(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        1,
        pixel_data,
        uids::JPEG_LOSSLESS_SV1,
    );
    let (output, report, _) = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap();
    assert_eq!(report.burned_in_text.regions_censored, 3);
    assert_censored(&source_frame, &samples(&output));
}

#[test]
fn a_source_yes_is_kept_after_censoring() {
    let mut source = native(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        &[frame()],
    );
    source.put_str(tags::BURNED_IN_ANNOTATION, VR::CS, "YES");
    let (output, report, _) = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap();
    assert!(report.burned_in_text.regions_censored > 0);
    assert_eq!(
        output
            .element(tags::BURNED_IN_ANNOTATION)
            .unwrap()
            .to_str()
            .unwrap()
            .trim(),
        "YES"
    );
}

#[test]
fn native_data_longer_than_its_geometry_fails_closed() {
    let mut long = frame();
    long.extend([0; 8]);
    let source = native(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        &[long],
    );
    let error = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap_err();
    assert!(matches!(error, Error::PixelTextNotAssessed(_)), "{error}");
}

#[test]
fn a_sample_above_bits_stored_fails_closed() {
    let mut samples = frame();
    samples[0] = 0x1000;
    let source = native(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        &[samples],
    );
    let error = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap_err();
    assert!(matches!(error, Error::PixelTextNotAssessed(_)), "{error}");
}

#[test]
fn float_pixel_data_fails_closed() {
    let mut source = native(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        &[frame()],
    );
    source.remove_element(tags::PIXEL_DATA);
    source.put(InMemElement::new(
        tags::FLOAT_PIXEL_DATA,
        VR::OF,
        PrimitiveValue::F32(vec![0.0_f32; ROWS * COLUMNS].into()),
    ));
    let error = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap_err();
    assert!(matches!(error, Error::PixelTextNotAssessed(_)), "{error}");
}

#[test]
fn a_second_codestream_in_a_censored_frame_still_fails() {
    let source_frame = frame();
    let mut encoded = Vec::new();
    jpegli::encode(&source_frame, COLUMNS as u32, ROWS as u32, &mut encoded).unwrap();
    let mut doubled = encoded.clone();
    doubled.extend(&encoded);
    if doubled.len() % 2 == 1 {
        doubled.push(0);
    }
    let pixel_data = InMemElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        Value::PixelSequence(PixelFragmentSequence::new(Vec::new(), vec![doubled])),
    );
    let source = object(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        1,
        pixel_data,
        uids::JPEG_LOSSLESS_SV1,
    );
    assert!(Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .is_err());
}

#[test]
fn text_over_tissue_fails_closed() {
    let mut samples = frame();
    draw(&mut samples, 40, 124, "DOE", TEXT);
    let source = native(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        &[samples],
    );
    let error = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap_err();
    assert!(
        matches!(error, Error::BurnedInTextOverTissue { frame: 0 }),
        "{error}"
    );
}

#[test]
fn an_undecodable_frame_fails_closed() {
    let pixel_data = InMemElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        Value::PixelSequence(PixelFragmentSequence::new(
            Vec::new(),
            vec![vec![0xFF, 0x4F, 0, 0]],
        )),
    );
    let source = object(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        1,
        pixel_data,
        uids::JPEG2000_LOSSLESS,
    );
    let error = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap_err();
    assert!(matches!(error, Error::PixelTextNotAssessed(_)), "{error}");
}

#[test]
fn an_explicit_opt_out_is_recorded_and_leaves_the_pixels() {
    let source_frame = frame();
    let source = native(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        std::slice::from_ref(&source_frame),
    );
    let (output, report, _) = Anonymizer::new(options(BurnedInText::Off))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap();
    assert!(!report.burned_in_text_censoring);
    assert_eq!(report.burned_in_text.frames_assessed, 0);
    assert_eq!(samples(&output), source_frame);
}

#[test]
fn other_policies_do_not_inspect_pixels_by_default() {
    let profile = ProfileConfiguration::dicom_extended();
    assert!(!profile.censors_burned_in_text());
    assert!(ProfileConfiguration::safe_harbor().censors_burned_in_text());
}

#[test]
fn an_invalid_bits_stored_fails_closed() {
    for bits_stored in [
        PrimitiveValue::from(0_u16),
        PrimitiveValue::from(17_u16),
        PrimitiveValue::Empty,
    ] {
        let mut source = native(
            uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
            &[frame()],
        );
        source.put(InMemElement::new(tags::BITS_STORED, VR::US, bits_stored));
        let error = Anonymizer::new(options(BurnedInText::Auto))
            .unwrap()
            .anonymize_with_generated_uid_map(&source)
            .unwrap_err();
        assert!(matches!(error, Error::PixelTextNotAssessed(_)), "{error}");
    }
}

#[test]
fn censored_8_bit_pixel_data_with_an_odd_sample_count_has_even_length() {
    // 95 x 191 samples is odd, so the source carries one padding byte.
    let (rows, columns) = (ROWS - 1, COLUMNS - 1);
    let full = frame();
    let mut bytes: Vec<u8> = (0..rows)
        .flat_map(|row| (0..columns).map(move |column| (row, column)))
        .map(|(row, column)| (full[row * COLUMNS + column] >> 4) as u8)
        .collect();
    assert_eq!(bytes.len() % 2, 1);
    bytes.push(0);
    let mut source = native(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        &[full],
    );
    for (tag, value) in [
        (tags::ROWS, rows as u16),
        (tags::COLUMNS, columns as u16),
        (tags::BITS_ALLOCATED, 8),
        (tags::BITS_STORED, 8),
        (tags::HIGH_BIT, 7),
    ] {
        source.put(InMemElement::new(tag, VR::US, PrimitiveValue::from(value)));
    }
    source.put(InMemElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        PrimitiveValue::U8(bytes.into()),
    ));
    let (output, report, _) = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap();
    assert!(report.burned_in_text.regions_censored > 0);
    let pixel_data = output.get(tags::PIXEL_DATA).unwrap().to_bytes().unwrap();
    assert_eq!(pixel_data.len(), rows * columns + 1);
    assert!(pixel_data[..rows * columns]
        .iter()
        .all(|&value| value != 255));
}

#[test]
fn a_censored_jpeg2000_object_keeps_its_lossy_image_compression_value() {
    for (transfer_syntax, expected) in [(uids::JPEG2000, "00"), (uids::JPEG2000_LOSSLESS, "00")] {
        let mut source = jpeg2000_as(&[frame()], transfer_syntax);
        source.put_str(tags::LOSSY_IMAGE_COMPRESSION, VR::CS, "00");
        let (output, report, _) = Anonymizer::new(options(BurnedInText::Auto))
            .unwrap()
            .anonymize_with_generated_uid_map(&source)
            .unwrap();
        assert!(report.burned_in_text.regions_censored > 0);
        assert_eq!(
            output
                .element(tags::LOSSY_IMAGE_COMPRESSION)
                .unwrap()
                .to_str()
                .unwrap()
                .trim(),
            expected,
            "{transfer_syntax}"
        );
    }
}

#[test]
fn a_high_bit_other_than_bits_stored_minus_one_fails_closed() {
    let mut source = native(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        &[frame()],
    );
    source.put(InMemElement::new(
        tags::HIGH_BIT,
        VR::US,
        PrimitiveValue::from(15_u16),
    ));
    let error = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap_err();
    assert!(matches!(error, Error::PixelTextNotAssessed(_)), "{error}");
}

#[test]
fn an_empty_pixel_description_attribute_fails_closed() {
    for tag in [tags::PIXEL_REPRESENTATION, tags::SAMPLES_PER_PIXEL] {
        let mut source = native(
            uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
            &[frame()],
        );
        source.put(InMemElement::new(tag, VR::US, PrimitiveValue::Empty));
        let error = Anonymizer::new(options(BurnedInText::Auto))
            .unwrap()
            .anonymize_with_generated_uid_map(&source)
            .unwrap_err();
        assert!(matches!(error, Error::PixelTextNotAssessed(_)), "{error}");
    }
}

#[test]
fn eight_bit_samples_in_implicit_vr_ow_pixel_data_are_censored() {
    let full = frame();
    let bytes: Vec<u8> = full.iter().map(|&value| (value >> 4) as u8).collect();
    let words: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let pixel_data = InMemElement::new(tags::PIXEL_DATA, VR::OW, PrimitiveValue::U16(words.into()));
    let mut source = object(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        1,
        pixel_data,
        uids::IMPLICIT_VR_LITTLE_ENDIAN,
    );
    for (tag, value) in [
        (tags::BITS_ALLOCATED, 8_u16),
        (tags::BITS_STORED, 8),
        (tags::HIGH_BIT, 7),
    ] {
        source.put(InMemElement::new(tag, VR::US, PrimitiveValue::from(value)));
    }
    // Write and read the object back, as a receiver would.
    let mut file = Vec::new();
    source.write_all(&mut file).unwrap();
    let source = dicom_object::from_reader(&file[128..]).unwrap();
    let (output, report, _) = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap();
    assert_eq!(report.burned_in_text.regions_censored, 3);
    let pixel_data = output.get(tags::PIXEL_DATA).unwrap().to_bytes().unwrap();
    assert!(pixel_data.iter().all(|&value| value != 255));
}

#[test]
fn monochrome2_with_an_inverse_presentation_lut_fails_closed() {
    let mut source = native(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        &[frame()],
    );
    source.put_str(tags::PRESENTATION_LUT_SHAPE, VR::CS, "INVERSE");
    let error = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap_err();
    assert!(matches!(error, Error::PixelTextNotAssessed(_)), "{error}");
}

#[test]
fn a_censored_jpeg2000_object_without_a_lossy_value_is_marked_possibly_lossy() {
    let source = jpeg2000_as(&[frame()], uids::JPEG2000);
    let (output, report, _) = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap();
    assert!(report.burned_in_text.regions_censored > 0);
    let value = |tag| {
        output
            .element(tag)
            .unwrap()
            .to_str()
            .unwrap()
            .trim()
            .to_owned()
    };
    assert_eq!(value(tags::LOSSY_IMAGE_COMPRESSION), "01");
    assert_eq!(value(tags::LOSSY_IMAGE_COMPRESSION_METHOD), "ISO_15444_1");
}

#[test]
fn a_presentation_or_modality_lut_sequence_fails_closed() {
    use dicom_core::value::DataSetSequence;
    let mut source = native(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        &[frame()],
    );
    source.put(InMemElement::new(
        tags::PRESENTATION_LUT_SEQUENCE,
        VR::SQ,
        DataSetSequence::from(vec![InMemDicomObject::new_empty()]),
    ));
    let error = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap_err();
    assert!(matches!(error, Error::PixelTextNotAssessed(_)), "{error}");

    let mut source = native(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        &[frame()],
    );
    source.put(InMemElement::new(
        tags::MODALITY_LUT_SEQUENCE,
        VR::SQ,
        DataSetSequence::from(vec![InMemDicomObject::new_empty()]),
    ));
    let error = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap_err();
    assert!(matches!(error, Error::PixelTextNotAssessed(_)), "{error}");
}

#[test]
fn a_censored_jpeg_ls_near_lossless_object_is_marked_lossy() {
    let mut encoded = Vec::new();
    jpegls::encode(&frame(), COLUMNS as u32, ROWS as u32, &mut encoded).unwrap();
    if encoded.len() % 2 == 1 {
        encoded.push(0);
    }
    let pixel_data = InMemElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        Value::PixelSequence(PixelFragmentSequence::new(Vec::new(), vec![encoded])),
    );
    let source = object(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        1,
        pixel_data,
        uids::JPEGLS_NEAR_LOSSLESS,
    );
    let (output, report, _) = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap();
    assert_eq!(report.burned_in_text.regions_censored, 3);
    let value = |tag| {
        output
            .element(tag)
            .unwrap()
            .to_str()
            .unwrap()
            .trim()
            .to_owned()
    };
    assert_eq!(value(tags::LOSSY_IMAGE_COMPRESSION), "01");
    assert_eq!(value(tags::LOSSY_IMAGE_COMPRESSION_METHOD), "ISO_14495_1");
}

#[test]
fn monochrome1_with_an_identity_presentation_lut_fails_closed() {
    let mut source = native(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        &[frame()],
    );
    source.put_str(tags::PHOTOMETRIC_INTERPRETATION, VR::CS, "MONOCHROME1");
    source.put_str(tags::PRESENTATION_LUT_SHAPE, VR::CS, "IDENTITY");
    let error = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap_err();
    assert!(matches!(error, Error::PixelTextNotAssessed(_)), "{error}");
}

/// A lossless JPEG object from `codestream`.
fn lossless_jpeg(mut codestream: Vec<u8>) -> DefaultDicomObject {
    if codestream.len() % 2 == 1 {
        codestream.push(0);
    }
    let pixel_data = InMemElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        Value::PixelSequence(PixelFragmentSequence::new(Vec::new(), vec![codestream])),
    );
    object(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        1,
        pixel_data,
        uids::JPEG_LOSSLESS_SV1,
    )
}

fn assert_fails_with(source: &DefaultDicomObject, reason: &str) {
    let error = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(source)
        .unwrap_err();
    assert!(error.to_string().contains(reason), "{error}");
}

#[test]
fn lossless_jpeg_with_a_restart_interval_fails_closed() {
    let mut encoded = Vec::new();
    jpegli::encode(&frame(), COLUMNS as u32, ROWS as u32, &mut encoded).unwrap();
    // A DRI segment with a restart interval of 4096 after SOI, then two stray bytes,
    // which the decoder skips.
    let mut with_restarts = encoded[..2].to_vec();
    with_restarts.extend([0xFF, 0xDD, 0x00, 0x04, 0x10, 0x00, 0x12, 0x34]);
    with_restarts.extend(&encoded[2..]);
    assert_fails_with(&lossless_jpeg(with_restarts), "restart interval");
}

#[test]
fn lossless_jpeg_with_a_point_transform_fails_closed() {
    let mut encoded = Vec::new();
    jpegli::encode(&frame(), COLUMNS as u32, ROWS as u32, &mut encoded).unwrap();
    // Set Al, the point transform, in the last byte of the scan header.
    let sos = encoded
        .windows(2)
        .position(|pair| pair == [0xFF, 0xDA])
        .unwrap();
    let length = usize::from(u16::from_be_bytes([encoded[sos + 2], encoded[sos + 3]]));
    encoded[sos + 1 + length] |= 0x01;
    assert_fails_with(&lossless_jpeg(encoded), "point transform");
}

#[test]
fn a_jpeg_segment_with_an_invalid_length_fails_closed() {
    for codestream in [
        vec![0xFF, 0xD8, 0xFF, 0xDD, 0x00, 0x00],
        vec![0xFF, 0xD8, 0xFF, 0xDD, 0x00, 0x01],
        vec![0xFF, 0xD8, 0xFF, 0xDD, 0x00, 0x40],
    ] {
        assert_fails_with(&lossless_jpeg(codestream), "invalid length");
    }
    // EOI ends the scan, and the decoder reports the missing frame.
    let error = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&lossless_jpeg(vec![0xFF, 0xD8, 0xFF, 0xD9]))
        .unwrap_err();
    assert!(matches!(error, Error::PixelTextNotAssessed(_)), "{error}");
}

#[test]
fn a_negative_rescale_slope_fails_closed() {
    let mut source = native(
        uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
        &[frame()],
    );
    source.put_str(tags::RESCALE_SLOPE, VR::DS, "-1");
    source.put_str(tags::RESCALE_INTERCEPT, VR::DS, "4095");
    let error = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap_err();
    assert!(matches!(error, Error::PixelTextNotAssessed(_)), "{error}");
}

#[test]
fn a_negative_rescale_slope_in_a_functional_group_fails_closed() {
    use dicom_core::value::DataSetSequence;
    let mut transformation = InMemDicomObject::new_empty();
    transformation.put_str(tags::RESCALE_SLOPE, VR::DS, "-1");
    let mut group = InMemDicomObject::new_empty();
    group.put(InMemElement::new(
        tags::PIXEL_VALUE_TRANSFORMATION_SEQUENCE,
        VR::SQ,
        DataSetSequence::from(vec![transformation]),
    ));
    let mut source = native(uids::BREAST_TOMOSYNTHESIS_IMAGE_STORAGE, &[frame()]);
    source.put(InMemElement::new(
        tags::SHARED_FUNCTIONAL_GROUPS_SEQUENCE,
        VR::SQ,
        DataSetSequence::from(vec![group]),
    ));
    let error = Anonymizer::new(options(BurnedInText::Auto))
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap_err();
    assert!(error.to_string().contains("Rescale Slope"), "{error}");
}
