//! The pinned text detector of the burned-in text check (issue #47).
//!
//! The model-backed tests need an installed detector:
//! `dicom-text-detector install DIR`, then `DICOM_ANON_TEXT_DETECTOR_DIR=DIR`. Without
//! the variable they do nothing. The configuration tests always run.
//!
//! All frames are synthetic: an exact-zero background, an elliptical breast of noisy
//! tissue on the right, and text drawn on the background with a 5x7 font at three times
//! its size, with or without anti-aliased edges.

use std::path::PathBuf;

use dicom_core::value::Value;
use dicom_core::{PrimitiveValue, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::mem::InMemElement;
use dicom_object::{DefaultDicomObject, FileMetaTableBuilder, InMemDicomObject};

use dicom_anon::{
    validate_source_object, Anonymizer, BurnedInText, Error, Options, ProfileConfiguration,
    SecretKey, TextDetector, TextOverTissue,
};

const KEY: &str = "text-detector-fixture-key-0123456789";
const ROWS: usize = 512;
const COLUMNS: usize = 640;
const TEXT: u16 = 4095;
const SCALE: usize = 3;

/// A 5x7 bitmap font, one string per row.
fn glyph(character: char) -> [&'static str; 7] {
    match character {
        'A' => [
            " ### ", "#   #", "#   #", "#####", "#   #", "#   #", "#   #",
        ],
        'C' => [
            " ####", "#    ", "#    ", "#    ", "#    ", "#    ", " ####",
        ],
        'D' => [
            "#### ", "#   #", "#   #", "#   #", "#   #", "#   #", "#### ",
        ],
        'E' => [
            "#####", "#    ", "#    ", "#### ", "#    ", "#    ", "#####",
        ],
        'J' => [
            "  ###", "    #", "    #", "    #", "    #", "#   #", " ### ",
        ],
        'L' => [
            "#    ", "#    ", "#    ", "#    ", "#    ", "#    ", "#####",
        ],
        'M' => [
            "#   #", "## ##", "# # #", "#   #", "#   #", "#   #", "#   #",
        ],
        'N' => [
            "#   #", "##  #", "# # #", "#  ##", "#   #", "#   #", "#   #",
        ],
        'O' => [
            " ### ", "#   #", "#   #", "#   #", "#   #", "#   #", " ### ",
        ],
        'R' => [
            "#### ", "#   #", "#   #", "#### ", "# #  ", "#  # ", "#   #",
        ],
        '0' => [
            " ### ", "#   #", "#  ##", "# # #", "##  #", "#   #", " ### ",
        ],
        '1' => [
            "  #  ", " ##  ", "  #  ", "  #  ", "  #  ", "  #  ", " ### ",
        ],
        '2' => [
            " ### ", "#   #", "    #", "   # ", "  #  ", " #   ", "#####",
        ],
        '3' => [
            "#### ", "    #", "    #", " ### ", "    #", "    #", "#### ",
        ],
        '4' => [
            "   # ", "  ## ", " # # ", "#  # ", "#####", "   # ", "   # ",
        ],
        '6' => [
            " ### ", "#    ", "#    ", "#### ", "#   #", "#   #", " ### ",
        ],
        '7' => [
            "#####", "    #", "   # ", "  #  ", " #   ", " #   ", " #   ",
        ],
        '9' => [
            " ### ", "#   #", "#   #", " ####", "    #", "    #", " ### ",
        ],
        '/' => [
            "    #", "    #", "   # ", "  #  ", " #   ", "#    ", "#    ",
        ],
        _ => ["     "; 7],
    }
}

/// Draw `text` with its top left at (`top`, `left`). Anti-aliased text has a one-pixel
/// edge at half the text value around each stroke.
fn draw(samples: &mut [u16], top: usize, left: usize, text: &str, anti_aliased: bool) {
    let mut mask = vec![false; ROWS * COLUMNS];
    for (index, character) in text.chars().enumerate() {
        let x = left + index * 6 * SCALE;
        for (row, line) in glyph(character).iter().enumerate() {
            for (column, cell) in line.chars().enumerate() {
                if cell != '#' {
                    continue;
                }
                for dy in 0..SCALE {
                    for dx in 0..SCALE {
                        mask[(top + SCALE * row + dy) * COLUMNS + x + SCALE * column + dx] = true;
                    }
                }
            }
        }
    }
    for row in 1..ROWS - 1 {
        for column in 1..COLUMNS - 1 {
            let index = row * COLUMNS + column;
            if mask[index] {
                samples[index] = TEXT;
            } else if anti_aliased
                && [index - 1, index + 1, index - COLUMNS, index + COLUMNS]
                    .iter()
                    .any(|&neighbor| mask[neighbor])
            {
                samples[index] = TEXT / 2;
            }
        }
    }
}

/// Whether a pixel is in the breast: an ellipse against the right edge.
fn in_breast(row: usize, column: usize) -> bool {
    let (dy, dx) = (
        (row as f64 - ROWS as f64 / 2.0) / (ROWS as f64 * 0.45),
        (column as f64 - COLUMNS as f64) / (COLUMNS as f64 * 0.45),
    );
    dy * dy + dx * dx <= 1.0
}

/// A frame with tissue and no text.
fn blank_frame() -> Vec<u16> {
    let mut samples = vec![0_u16; ROWS * COLUMNS];
    let mut state = 0x9E37_79B9_u32;
    for row in 0..ROWS {
        for column in 0..COLUMNS {
            if in_breast(row, column) {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                samples[row * COLUMNS + column] = 1200 + (state % 600) as u16;
            }
        }
    }
    samples
}

/// A frame with a name, an MRN and a birth date on the background.
fn frame(anti_aliased: bool) -> Vec<u16> {
    let mut samples = blank_frame();
    draw(&mut samples, 20, 20, "DOE JANE", anti_aliased);
    draw(&mut samples, 60, 20, "MRN 4711", anti_aliased);
    draw(&mut samples, 100, 20, "12/03/1961", anti_aliased);
    samples
}

fn object(frames: &[Vec<u16>]) -> DefaultDicomObject {
    let sop_class = uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION;
    let sop_instance = "2.25.5711";
    let mut object = InMemDicomObject::new_empty();
    object.put_str(tags::SOP_CLASS_UID, VR::UI, sop_class);
    object.put_str(tags::SOP_INSTANCE_UID, VR::UI, sop_instance);
    object.put_str(tags::STUDY_INSTANCE_UID, VR::UI, "2.25.5712");
    object.put_str(tags::SERIES_INSTANCE_UID, VR::UI, "2.25.5713");
    object.put_str(tags::MODALITY, VR::CS, "MG");
    object.put_str(tags::PATIENT_NAME, VR::PN, "Doe^Jane");
    object.put_str(tags::PATIENT_ID, VR::LO, "MRN-4711");
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
    if frames.len() > 1 {
        object.put_str(tags::NUMBER_OF_FRAMES, VR::IS, frames.len().to_string());
    }
    object.put(InMemElement::new(
        tags::PIXEL_DATA,
        VR::OW,
        PrimitiveValue::U16(frames.concat().into()),
    ));
    object
        .with_meta(
            FileMetaTableBuilder::new()
                .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
                .media_storage_sop_class_uid(sop_class)
                .media_storage_sop_instance_uid(sop_instance),
        )
        .unwrap()
}

fn options(detector: TextDetector, dir: Option<PathBuf>) -> Options {
    let mut options = Options::new(SecretKey::new(KEY).unwrap());
    options.patient_scope = Some("synthetic-subject".to_owned());
    options.profile = ProfileConfiguration::safe_harbor();
    options.profile.burned_in_text = BurnedInText::Censor;
    options.profile.burned_in_text_detector = detector;
    options.profile.text_detector_dir = dir.map(|dir| dir.to_string_lossy().into_owned());
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

/// No text pixel is left, and the breast is unchanged.
fn assert_censored(source: &[u16], output: &[u16]) {
    for row in 0..ROWS {
        for column in 0..COLUMNS {
            let index = row * COLUMNS + column;
            if in_breast(row, column) {
                assert_eq!(
                    output[index], source[index],
                    "tissue changed at {row},{column}"
                );
            } else {
                assert_eq!(output[index], 0, "text left at {row},{column}");
            }
        }
    }
}

fn installed() -> Option<PathBuf> {
    let dir = std::env::var_os("DICOM_ANON_TEXT_DETECTOR_DIR")?;
    Some(PathBuf::from(dir))
}

macro_rules! require_detector {
    () => {
        match installed() {
            Some(dir) => dir,
            None => {
                eprintln!("DICOM_ANON_TEXT_DETECTOR_DIR is not set; skipping");
                return;
            }
        }
    };
}

#[test]
fn the_model_needs_a_folder_and_the_check_on() {
    let error = Anonymizer::new(options(TextDetector::Model, None)).unwrap_err();
    assert!(error.to_string().contains("text_detector_dir"), "{error}");
    let mut off = options(TextDetector::Both, Some(PathBuf::from("/nonexistent")));
    off.profile.burned_in_text = BurnedInText::Off;
    let error = Anonymizer::new(off).unwrap_err();
    assert!(error.to_string().contains("check is off"), "{error}");
}

#[test]
fn a_missing_detector_fails_closed() {
    let empty = tempfile::tempdir().unwrap();
    let options = options(TextDetector::Model, Some(empty.path().to_path_buf()));
    let error = Anonymizer::new(options)
        .unwrap()
        .anonymize_with_generated_uid_map(&object(&[frame(false)]))
        .unwrap_err();
    assert!(matches!(error, Error::PixelTextNotAssessed(_)), "{error}");
    let hint = if cfg!(feature = "text-detector") {
        "install"
    } else {
        "text-detector feature"
    };
    assert!(error.to_string().contains(hint), "{error}");
}

#[test]
fn the_model_finds_and_censors_text_on_the_background() {
    let dir = require_detector!();
    for anti_aliased in [false, true] {
        let source = frame(anti_aliased);
        let (output, report, _) = Anonymizer::new(options(TextDetector::Model, Some(dir.clone())))
            .unwrap()
            .anonymize_with_generated_uid_map(&object(std::slice::from_ref(&source)))
            .unwrap();
        assert!(
            report.burned_in_text.model_boxes >= 3,
            "{:?}",
            report.burned_in_text
        );
        assert!(
            report.burned_in_text.regions_censored >= 3,
            "{:?}",
            report.burned_in_text
        );
        assert_eq!(report.burned_in_text_detector, TextDetector::Model);
        assert_censored(&source, &samples(&output));
    }
}

fn run(detector: TextDetector, dir: &std::path::Path, frames: &[Vec<u16>]) -> DefaultDicomObject {
    let mut options = options(detector, Some(dir.to_path_buf()));
    options.profile.text_over_tissue = TextOverTissue::Fail;
    Anonymizer::new(options)
        .unwrap()
        .anonymize_with_generated_uid_map(&object(frames))
        .unwrap()
        .0
}

#[test]
fn the_model_censors_anti_aliased_initials_that_the_heuristic_leaves() {
    let dir = require_detector!();
    let mut source = blank_frame();
    draw(&mut source, 200, 40, "JD", true);
    let (heuristic, report, _) = Anonymizer::new(options(TextDetector::Heuristic, None))
        .unwrap()
        .anonymize_with_generated_uid_map(&object(&[source.clone()]))
        .unwrap();
    assert_eq!(report.burned_in_text.regions_left_in_place, 1);
    assert_eq!(samples(&heuristic), source);
    let (both, report, _) = Anonymizer::new(options(TextDetector::Both, Some(dir)))
        .unwrap()
        .anonymize_with_generated_uid_map(&object(&[source.clone()]))
        .unwrap();
    assert!(
        report.burned_in_text.regions_censored >= 1,
        "{:?}",
        report.burned_in_text
    );
    assert_censored(&source, &samples(&both));
}

#[test]
fn a_frame_without_text_keeps_its_pixel_data() {
    let dir = require_detector!();
    let source = blank_frame();
    let output = run(TextDetector::Model, &dir, std::slice::from_ref(&source));
    assert_eq!(samples(&output), source);
}

#[test]
fn text_over_tissue_found_by_the_model_fails_closed() {
    let dir = require_detector!();
    let mut source = blank_frame();
    // A name that runs from the background into the breast.
    draw(&mut source, 240, 250, "DOE JANE", false);
    for detector in [TextDetector::Model, TextDetector::Both] {
        let error = Anonymizer::new(options(detector, Some(dir.clone())))
            .unwrap()
            .anonymize_with_generated_uid_map(&object(&[source.clone()]))
            .unwrap_err();
        assert!(
            matches!(error, Error::BurnedInTextOverTissue { .. }),
            "{error}"
        );
    }
}

#[test]
fn text_in_one_frame_of_several_is_found_through_the_projection() {
    let dir = require_detector!();
    let (blank, with_text) = (blank_frame(), frame(true));
    let output = samples(&run(
        TextDetector::Model,
        &dir,
        &[blank.clone(), with_text.clone(), blank.clone()],
    ));
    let frame_samples = ROWS * COLUMNS;
    assert_eq!(&output[..frame_samples], &blank[..]);
    assert_censored(&with_text, &output[frame_samples..2 * frame_samples]);
    assert_eq!(&output[2 * frame_samples..], &blank[..]);
}

#[test]
fn the_model_output_is_the_same_on_every_run() {
    let dir = require_detector!();
    let source = frame(true);
    let first = samples(&run(
        TextDetector::Both,
        &dir,
        std::slice::from_ref(&source),
    ));
    let second = samples(&run(TextDetector::Both, &dir, &[source]));
    assert_eq!(first, second);
}

#[test]
fn source_aware_validation_passes_with_the_model() {
    let dir = require_detector!();
    let options = options(TextDetector::Both, Some(dir));
    let source = object(&[frame(true)]);
    let (output, _, uid_map) = Anonymizer::new(options.clone())
        .unwrap()
        .anonymize_with_generated_uid_map(&source)
        .unwrap();
    let mut validation = options;
    validation.profile.uid_mappings = uid_map;
    let report = validate_source_object(&source, &output, validation).unwrap();
    assert!(report.conformant, "{:?}", report.findings);
}

#[test]
fn a_changed_model_file_fails_closed() {
    let dir = require_detector!();
    let copy = tempfile::tempdir().unwrap();
    for folder in ["model", "runtime"] {
        std::fs::create_dir(copy.path().join(folder)).unwrap();
        for entry in std::fs::read_dir(dir.join(folder)).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(
                entry.path(),
                copy.path().join(folder).join(entry.file_name()),
            )
            .unwrap();
        }
    }
    let model = copy.path().join("model").join(dicom_anon::MODEL_FILE);
    let mut bytes = std::fs::read(&model).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x01;
    std::fs::write(&model, bytes).unwrap();
    let error = Anonymizer::new(options(
        TextDetector::Model,
        Some(copy.path().to_path_buf()),
    ))
    .unwrap()
    .anonymize_with_generated_uid_map(&object(&[frame(false)]))
    .unwrap_err();
    assert!(error.to_string().contains("pinned SHA-256"), "{error}");
}
