//! Process-level tests for the pixel-risk gates and for JPEG 2000, JPEG, and JPEG-LS
//! comment and application segment removal.
//!
//! All objects are synthetic. UIDs use the `2.25` root and text values are placeholders.

use std::path::Path;
use std::process::{Command, Output};

use dicom_core::value::{PixelFragmentSequence, Value};
use dicom_core::{PrimitiveValue, VR};
use dicom_dictionary_std::tags;
use dicom_object::mem::InMemElement;
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

const KEY_ENV: &str = "DICOM_ANON_TEST_KEY";
const KEY: &str = "0123456789abcdef0123456789abcdef";
const MAMMOGRAPHY_FOR_PRESENTATION: &str = "1.2.840.10008.5.1.4.1.1.1.2";
const BREAST_TOMOSYNTHESIS: &str = "1.2.840.10008.5.1.4.1.1.13.1.3";
const JPEG_2000_LOSSLESS: &str = "1.2.840.10008.1.2.4.90";
const JPEG_LOSSLESS: &str = "1.2.840.10008.1.2.4.57";
const JPEG_LOSSLESS_SV1: &str = "1.2.840.10008.1.2.4.70";
const JPEG_LS_LOSSLESS: &str = "1.2.840.10008.1.2.4.80";
const JPEG_LS_NEAR_LOSSLESS: &str = "1.2.840.10008.1.2.4.81";
const JPEG_COMMENT_TEXT: &[u8] = b"SYNTHJPEGCOMMENT";
const JPEG_TABLE_COMMENT_TEXT: &[u8] = b"SYNTHTABLECOMMENT";
const JFIF_TEXT: &[u8] = b"SYNTHJFIFPAYLOAD";
const EXIF_TEXT: &[u8] = b"SYNTHEXIFPAYLOAD";
const COMMENT_TEXT: &[u8] = b"SYNTHJ2KCOMMENT!";
const TILE_COMMENT_TEXT: &[u8] = b"SYNTHTILECOMMENT";
const STUDY_UID: &str = "2.25.1217391901722963736713713331934083248";
const SERIES_UID: &str = "2.25.233714667944513138427482917746719619";
const INSTANCE_UID: &str = "2.25.300172963390693794128141873112323210";
const WIDTH: u16 = 24;
const HEIGHT: u16 = 16;

/// Synthetic 16-bit samples that use the full unsigned range.
fn synthetic_samples() -> Vec<u16> {
    (0..u32::from(WIDTH) * u32::from(HEIGHT))
        .map(|index| (index.wrapping_mul(40_503) ^ (index << 7)) as u16)
        .collect()
}

/// Lossless codestream from the pinned test encoder.
fn encoded_codestream(samples: &[u16]) -> Vec<u8> {
    let mut encoded = Vec::new();
    jpeg2k::encode(
        samples,
        u32::from(WIDTH),
        u32::from(HEIGHT),
        &jpeg2k::Jpeg2kOptions::default(),
        &mut encoded,
    )
    .unwrap();
    encoded
}

/// Codestream from the pinned JPEG Lossless (Process 14, SV1) test encoder.
///
/// The encoder writes a JFIF APP0 segment after SOI. This function removes it, so
/// the result is the expected anonymizer output for every annotated fixture.
fn jpeg_lossless_codestream(samples: &[u16]) -> Vec<u8> {
    let mut encoded = Vec::new();
    jpegli::encode(samples, u32::from(WIDTH), u32::from(HEIGHT), &mut encoded).unwrap();
    assert_eq!(&encoded[2..4], &[0xFF, 0xE0]);
    assert_eq!(&encoded[6..11], b"JFIF\0");
    let app0_end = 4 + usize::from(u16::from_be_bytes([encoded[4], encoded[5]]));
    encoded.drain(2..app0_end);
    encoded
}

/// Codestream from the pinned JPEG-LS lossless test encoder.
fn jpeg_ls_codestream(samples: &[u16]) -> Vec<u8> {
    let mut encoded = Vec::new();
    jpegls::encode(samples, u32::from(WIDTH), u32::from(HEIGHT), &mut encoded).unwrap();
    encoded
}

fn jpeg_segment(marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut segment = vec![0xFF, marker];
    segment.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
    segment.extend_from_slice(payload);
    segment
}

/// Insert APP0, APP1, and COM after SOI, and COM plus APP14 before the first SOS.
///
/// The single-component frame does not need the Adobe color transform.
fn with_jpeg_metadata(codestream: &[u8]) -> Vec<u8> {
    assert_eq!(&codestream[..2], &[0xFF, 0xD8]);
    let mut sos = 2;
    while codestream[sos + 1] != 0xDA {
        assert_eq!(codestream[sos], 0xFF);
        assert!(
            codestream[sos + 1] != 0xFE && codestream[sos + 1] & 0xF0 != 0xE0,
            "the encoder output must not contain COM or APPn segments"
        );
        sos += 2 + usize::from(u16::from_be_bytes([
            codestream[sos + 2],
            codestream[sos + 3],
        ]));
    }
    let mut output = codestream[..2].to_vec();
    output.extend(jpeg_segment(
        0xE0,
        &[b"JFIF\0".as_slice(), JFIF_TEXT].concat(),
    ));
    output.extend(jpeg_segment(
        0xE1,
        &[b"Exif\0\0".as_slice(), EXIF_TEXT].concat(),
    ));
    output.extend(jpeg_segment(0xFE, JPEG_COMMENT_TEXT));
    output.extend_from_slice(&codestream[2..sos]);
    output.extend(jpeg_segment(0xFE, JPEG_TABLE_COMMENT_TEXT));
    output.extend(jpeg_segment(
        0xEE,
        &[b"Adobe".as_slice(), &[0, 100, 0, 0, 0, 0, 0]].concat(),
    ));
    output.extend_from_slice(&codestream[sos..]);
    output
}

fn comment_segment(text: &[u8]) -> Vec<u8> {
    let length = u16::try_from(text.len() + 4).unwrap();
    let mut segment = vec![0xFF, 0x64];
    segment.extend_from_slice(&length.to_be_bytes());
    segment.extend_from_slice(&[0x00, 0x01]);
    segment.extend_from_slice(text);
    segment
}

fn segment_length(codestream: &[u8], offset: usize) -> usize {
    usize::from(u16::from_be_bytes([
        codestream[offset + 2],
        codestream[offset + 3],
    ])) + 2
}

/// Insert one COM segment after SIZ and one after the first SOT segment.
fn with_comments(codestream: &[u8]) -> Vec<u8> {
    assert_eq!(&codestream[..4], &[0xFF, 0x4F, 0xFF, 0x51]);
    let after_siz = 2 + segment_length(codestream, 2);
    let sot = (after_siz..codestream.len() - 1)
        .find(|&offset| codestream[offset] == 0xFF && codestream[offset + 1] == 0x90)
        .unwrap();
    let main_comment = comment_segment(COMMENT_TEXT);
    let tile_comment = comment_segment(TILE_COMMENT_TEXT);

    let mut output = codestream[..after_siz].to_vec();
    output.extend_from_slice(&main_comment);
    output.extend_from_slice(&codestream[after_siz..sot + 12]);
    output.extend_from_slice(&tile_comment);
    output.extend_from_slice(&codestream[sot + 12..]);

    let psot_offset = sot + main_comment.len() + 6;
    let psot = u32::from_be_bytes(output[psot_offset..psot_offset + 4].try_into().unwrap());
    assert_ne!(
        psot, 0,
        "the test encoder writes explicit tile-part lengths"
    );
    let psot = psot + u32::try_from(tile_comment.len()).unwrap();
    output[psot_offset..psot_offset + 4].copy_from_slice(&psot.to_be_bytes());
    output
}

fn mammography_bytes(
    sop_class: &str,
    burned_in_annotation: Option<&str>,
    transfer_syntax: &str,
    fragment: Vec<u8>,
) -> Vec<u8> {
    let mut object = InMemDicomObject::new_empty();
    object.put_str(tags::SOP_CLASS_UID, VR::UI, sop_class);
    object.put_str(tags::SOP_INSTANCE_UID, VR::UI, INSTANCE_UID);
    object.put_str(tags::STUDY_INSTANCE_UID, VR::UI, STUDY_UID);
    object.put_str(tags::SERIES_INSTANCE_UID, VR::UI, SERIES_UID);
    object.put_str(tags::PATIENT_ID, VR::LO, "SYNTHETIC-ID");
    object.put_str(tags::PATIENT_NAME, VR::PN, "Placeholder^Name");
    object.put_str(tags::STUDY_DATE, VR::DA, "20240101");
    object.put_str(tags::MODALITY, VR::CS, "MG");
    object.put(InMemElement::new(
        tags::SAMPLES_PER_PIXEL,
        VR::US,
        PrimitiveValue::from(1_u16),
    ));
    object.put_str(tags::PHOTOMETRIC_INTERPRETATION, VR::CS, "MONOCHROME2");
    object.put(InMemElement::new(
        tags::ROWS,
        VR::US,
        PrimitiveValue::from(HEIGHT),
    ));
    object.put(InMemElement::new(
        tags::COLUMNS,
        VR::US,
        PrimitiveValue::from(WIDTH),
    ));
    for tag in [tags::BITS_ALLOCATED, tags::BITS_STORED] {
        object.put(InMemElement::new(tag, VR::US, PrimitiveValue::from(16_u16)));
    }
    object.put(InMemElement::new(
        tags::HIGH_BIT,
        VR::US,
        PrimitiveValue::from(15_u16),
    ));
    object.put(InMemElement::new(
        tags::PIXEL_REPRESENTATION,
        VR::US,
        PrimitiveValue::from(0_u16),
    ));
    if let Some(value) = burned_in_annotation {
        object.put_str(tags::BURNED_IN_ANNOTATION, VR::CS, value);
    }
    let mut fragment = fragment;
    if fragment.len() % 2 == 1 {
        fragment.push(0);
    }
    object.put(InMemElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        Value::PixelSequence(PixelFragmentSequence::new(
            Vec::<u32>::new(),
            vec![fragment],
        )),
    ));
    let object = object
        .with_meta(FileMetaTableBuilder::new().transfer_syntax(transfer_syntax))
        .unwrap();
    let mut bytes = Vec::new();
    object.write_all(&mut bytes).unwrap();
    bytes
}

fn plain_mammography(burned_in_annotation: Option<&str>) -> Vec<u8> {
    mammography_bytes(
        MAMMOGRAPHY_FOR_PRESENTATION,
        burned_in_annotation,
        JPEG_2000_LOSSLESS,
        encoded_codestream(&synthetic_samples()),
    )
}

fn anonymize(input: &Path, output: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dicom-anonymize"))
        .arg(input)
        .arg(output)
        .args(["--key-env", KEY_ENV])
        .args(extra)
        .env(KEY_ENV, KEY)
        .output()
        .unwrap()
}

fn validate(input: &Path, extra: &[&str]) -> (Option<i32>, serde_json::Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_dicom-validate"))
        .arg(input)
        .args(["--format", "json"])
        .args(extra)
        .output()
        .unwrap();
    let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid report ({error}): {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code(), report)
}

fn finding_severity(report: &serde_json::Value, code: &str) -> Option<String> {
    report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["code"] == code)
        .map(|finding| finding["severity"].as_str().unwrap().to_owned())
}

fn pixel_fragments(path: &Path) -> Vec<Vec<u8>> {
    let object = dicom_anon::read_dicom(std::fs::File::open(path).unwrap()).unwrap();
    object
        .element(tags::PIXEL_DATA)
        .unwrap()
        .value()
        .fragments()
        .unwrap()
        .to_vec()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn burned_in_annotation_gate_accepts_only_an_explicit_no() {
    let directory = tempfile::tempdir().unwrap();
    for (variant, value, expected) in [
        ("no", Some("NO"), Some(0)),
        ("yes", Some("YES"), Some(2)),
        ("absent", None, Some(2)),
    ] {
        let input = directory.path().join(format!("{variant}.dcm"));
        let output = directory.path().join(format!("{variant}-out.dcm"));
        std::fs::write(&input, plain_mammography(value)).unwrap();

        let ungated = anonymize(&input, &output, &[]);
        assert_eq!(
            ungated.status.code(),
            Some(0),
            "{variant}: default behavior"
        );
        std::fs::remove_file(&output).unwrap();

        let gated = anonymize(&input, &output, &["--require-no-burned-in-annotation"]);
        assert_eq!(gated.status.code(), expected, "{variant}");
        assert_eq!(output.exists(), expected == Some(0), "{variant}: output");
        if expected == Some(2) {
            let stderr = String::from_utf8(gated.stderr).unwrap();
            assert!(
                stderr.contains("Burned In Annotation"),
                "{variant}: {stderr}"
            );
        }

        let (status, report) = validate(&input, &["--require-no-burned-in-annotation"]);
        let severity = finding_severity(&report, "burned_in_annotation_not_no");
        if expected == Some(0) {
            assert_eq!(severity, None, "{variant}");
        } else {
            assert_eq!(severity.as_deref(), Some("error"), "{variant}");
            assert_eq!(status, Some(1), "{variant}");
        }
        let (_, ungated_report) = validate(&input, &[]);
        assert_eq!(
            finding_severity(&ungated_report, "burned_in_annotation_not_no"),
            None
        );
    }
}

#[test]
fn sop_class_allowlist_rejects_other_sop_classes() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.dcm");
    let output = directory.path().join("output.dcm");
    std::fs::write(&input, plain_mammography(Some("NO"))).unwrap();

    let rejected = anonymize(
        &input,
        &output,
        &["--allow-sop-class", BREAST_TOMOSYNTHESIS],
    );
    assert_eq!(rejected.status.code(), Some(2));
    assert!(!output.exists());
    assert!(String::from_utf8(rejected.stderr)
        .unwrap()
        .contains("SOP Class"));
    let (status, report) = validate(&input, &["--allow-sop-class", BREAST_TOMOSYNTHESIS]);
    assert_eq!(status, Some(1));
    assert_eq!(
        finding_severity(&report, "sop_class_not_allowed").as_deref(),
        Some("error")
    );

    let accepted = anonymize(
        &input,
        &output,
        &[
            "--allow-sop-class",
            BREAST_TOMOSYNTHESIS,
            "--allow-sop-class",
            MAMMOGRAPHY_FOR_PRESENTATION,
        ],
    );
    assert_eq!(accepted.status.code(), Some(0));
    assert!(output.exists());
    let (_, report) = validate(
        &output,
        &["--allow-sop-class", MAMMOGRAPHY_FOR_PRESENTATION],
    );
    assert_eq!(finding_severity(&report, "sop_class_not_allowed"), None);

    let invalid = anonymize(&input, &output, &["--allow-sop-class", "not-a-uid"]);
    assert_eq!(invalid.status.code(), Some(2));
}

#[test]
fn jpeg2000_comments_are_removed_without_changing_decoded_samples() {
    let directory = tempfile::tempdir().unwrap();
    let samples = synthetic_samples();
    let clean = encoded_codestream(&samples);
    let commented = with_comments(&clean);
    let (decoded, _, _) = jpeg2k::decode(&commented, u32::from(WIDTH), u32::from(HEIGHT)).unwrap();
    assert_eq!(decoded, samples, "the commented fixture must decode");

    let input = directory.path().join("commented.dcm");
    let output = directory.path().join("commented-out.dcm");
    std::fs::write(
        &input,
        mammography_bytes(
            MAMMOGRAPHY_FOR_PRESENTATION,
            Some("NO"),
            JPEG_2000_LOSSLESS,
            commented,
        ),
    )
    .unwrap();
    let (status, report) = validate(&input, &[]);
    assert_eq!(status, Some(1));
    assert_eq!(
        finding_severity(&report, "jpeg2000_comment_present").as_deref(),
        Some("error")
    );

    let result = anonymize(&input, &output, &["--verbose"]);
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let anonymization: serde_json::Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(anonymization["removed_jpeg2000_comment_segments"], 2);
    let output_bytes = std::fs::read(&output).unwrap();
    assert!(!contains(&output_bytes, COMMENT_TEXT));
    assert!(!contains(&output_bytes, TILE_COMMENT_TEXT));
    let fragments = pixel_fragments(&output);
    assert_eq!(fragments.len(), 1);
    let codestream = fragments[0].strip_suffix(&[0]).unwrap_or(&fragments[0]);
    assert_eq!(
        codestream,
        clean.as_slice(),
        "COM removal restores the encoder output"
    );
    let (decoded, _, _) = jpeg2k::decode(codestream, u32::from(WIDTH), u32::from(HEIGHT)).unwrap();
    assert_eq!(decoded, samples, "decoded samples must be bit-identical");
    let (_, report) = validate(&output, &[]);
    assert_eq!(finding_severity(&report, "jpeg2000_comment_present"), None);
    assert_eq!(
        finding_severity(&report, "jpeg2000_codestream_unparseable"),
        None
    );
}

#[test]
fn jpeg2000_without_comments_is_copied_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("clean.dcm");
    let output = directory.path().join("clean-out.dcm");
    std::fs::write(&input, plain_mammography(Some("NO"))).unwrap();
    let result = anonymize(&input, &output, &[]);
    assert_eq!(result.status.code(), Some(0));
    assert_eq!(pixel_fragments(&input), pixel_fragments(&output));
}

#[test]
fn malformed_jpeg2000_codestream_fails_closed() {
    // The issue reproduction: SOC, one COM segment, and EOC, without SIZ.
    let mut fragment = vec![0xFF, 0x4F];
    fragment.extend_from_slice(&comment_segment(COMMENT_TEXT));
    fragment.extend_from_slice(&[0xFF, 0xD9]);

    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("malformed.dcm");
    let output = directory.path().join("malformed-out.dcm");
    std::fs::write(
        &input,
        mammography_bytes(
            MAMMOGRAPHY_FOR_PRESENTATION,
            Some("NO"),
            JPEG_2000_LOSSLESS,
            fragment,
        ),
    )
    .unwrap();
    for policy in [
        vec![],
        vec!["--policy", "dicom"],
        vec![
            "--policy",
            "safe-harbor-metadata",
            "--patient-scope",
            "SUBJ-0001",
        ],
    ] {
        let mut arguments = policy.clone();
        let uid_map = directory.path().join("uid-map.json");
        if policy.contains(&"safe-harbor-metadata") {
            std::fs::write(
                &uid_map,
                serde_json::json!({
                    STUDY_UID: "2.25.11",
                    SERIES_UID: "2.25.12",
                    INSTANCE_UID: "2.25.13",
                })
                .to_string(),
            )
            .unwrap();
            arguments.extend(["--uid-map", uid_map.to_str().unwrap()]);
        }
        let result = anonymize(&input, &output, &arguments);
        assert_eq!(result.status.code(), Some(2), "{policy:?}");
        assert!(!output.exists(), "{policy:?}");
        assert!(String::from_utf8(result.stderr)
            .unwrap()
            .contains("JPEG 2000"));
    }
    let (status, report) = validate(&input, &[]);
    assert_eq!(status, Some(1));
    assert_eq!(
        finding_severity(&report, "jpeg2000_codestream_unparseable").as_deref(),
        Some("error")
    );
}

type Codec = (&'static str, fn(&[u16]) -> Vec<u8>, fn(&[u8]) -> Vec<u16>);

fn jpeg_codecs() -> [Codec; 4] {
    fn decode_lossless(codestream: &[u8]) -> Vec<u16> {
        jpegli::decode(codestream, u32::from(WIDTH), u32::from(HEIGHT))
            .unwrap()
            .0
    }
    fn decode_ls(codestream: &[u8]) -> Vec<u16> {
        jpegls::decode(codestream, u32::from(WIDTH), u32::from(HEIGHT))
            .unwrap()
            .0
    }
    [
        (JPEG_LOSSLESS, jpeg_lossless_codestream, decode_lossless),
        (JPEG_LOSSLESS_SV1, jpeg_lossless_codestream, decode_lossless),
        (JPEG_LS_LOSSLESS, jpeg_ls_codestream, decode_ls),
        (JPEG_LS_NEAR_LOSSLESS, jpeg_ls_codestream, decode_ls),
    ]
}

#[test]
fn jpeg_comment_and_application_segments_are_removed_byte_identically() {
    let directory = tempfile::tempdir().unwrap();
    let samples = synthetic_samples();
    for (transfer_syntax, encode, decode) in jpeg_codecs() {
        let clean = encode(&samples);
        let annotated = with_jpeg_metadata(&clean);
        assert_eq!(decode(&clean), samples, "{transfer_syntax}: lossless");
        assert_eq!(decode(&annotated), samples, "{transfer_syntax}: fixture");

        let input = directory.path().join(format!("{transfer_syntax}.dcm"));
        let output = directory.path().join(format!("{transfer_syntax}-out.dcm"));
        std::fs::write(
            &input,
            mammography_bytes(
                MAMMOGRAPHY_FOR_PRESENTATION,
                Some("NO"),
                transfer_syntax,
                annotated,
            ),
        )
        .unwrap();
        let (status, report) = validate(&input, &[]);
        assert_eq!(status, Some(1), "{transfer_syntax}");
        for code in ["jpeg_comment_present", "jpeg_application_segment_present"] {
            assert_eq!(
                finding_severity(&report, code).as_deref(),
                Some("error"),
                "{transfer_syntax}: {code}"
            );
        }

        let result = anonymize(&input, &output, &["--verbose"]);
        assert_eq!(
            result.status.code(),
            Some(0),
            "{transfer_syntax}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let anonymization: serde_json::Value = serde_json::from_slice(&result.stderr).unwrap();
        assert_eq!(anonymization["removed_jpeg_comment_segments"], 2);
        assert_eq!(anonymization["removed_jpeg_application_segments"], 3);
        assert_eq!(anonymization["removed_jpeg2000_comment_segments"], 0);
        let output_bytes = std::fs::read(&output).unwrap();
        for text in [
            JPEG_COMMENT_TEXT,
            JPEG_TABLE_COMMENT_TEXT,
            JFIF_TEXT,
            EXIF_TEXT,
            b"Adobe",
        ] {
            assert!(!contains(&output_bytes, text), "{transfer_syntax}");
        }
        let fragments = pixel_fragments(&output);
        assert_eq!(fragments.len(), 1);
        let codestream = fragments[0].strip_suffix(&[0]).unwrap_or(&fragments[0]);
        assert_eq!(
            codestream,
            clean.as_slice(),
            "{transfer_syntax}: segment removal restores the encoder output"
        );
        assert_eq!(decode(codestream), samples, "{transfer_syntax}: samples");
        let (_, report) = validate(&output, &[]);
        for code in [
            "jpeg_comment_present",
            "jpeg_application_segment_present",
            "jpeg_codestream_unparseable",
        ] {
            assert_eq!(finding_severity(&report, code), None, "{transfer_syntax}");
        }
    }
}

#[test]
fn jpeg_without_metadata_segments_is_copied_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let samples = synthetic_samples();
    for (transfer_syntax, encode, _) in jpeg_codecs() {
        let input = directory.path().join(format!("{transfer_syntax}.dcm"));
        let output = directory.path().join(format!("{transfer_syntax}-out.dcm"));
        std::fs::write(
            &input,
            mammography_bytes(
                MAMMOGRAPHY_FOR_PRESENTATION,
                Some("NO"),
                transfer_syntax,
                encode(&samples),
            ),
        )
        .unwrap();
        let result = anonymize(&input, &output, &[]);
        assert_eq!(result.status.code(), Some(0), "{transfer_syntax}");
        assert_eq!(pixel_fragments(&input), pixel_fragments(&output));
        let (_, report) = validate(&output, &[]);
        assert_eq!(
            finding_severity(&report, "jpeg_codestream_unparseable"),
            None
        );
    }
}

#[test]
fn malformed_jpeg_codestreams_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let clean = jpeg_lossless_codestream(&synthetic_samples());
    // SOI, one COM segment, and EOI, without a frame or scan.
    let mut header_only = vec![0xFF, 0xD8];
    header_only.extend(jpeg_segment(0xFE, JPEG_COMMENT_TEXT));
    header_only.extend_from_slice(&[0xFF, 0xD9]);
    let mut trailing = clean.clone();
    trailing.extend_from_slice(JPEG_COMMENT_TEXT);
    for (name, transfer_syntax, fragment) in [
        ("header-only", JPEG_LOSSLESS_SV1, header_only),
        ("trailing", JPEG_LOSSLESS_SV1, trailing),
        ("wrong-family", JPEG_LS_LOSSLESS, clean),
    ] {
        let input = directory.path().join(format!("{name}.dcm"));
        let output = directory.path().join(format!("{name}-out.dcm"));
        std::fs::write(
            &input,
            mammography_bytes(
                MAMMOGRAPHY_FOR_PRESENTATION,
                Some("NO"),
                transfer_syntax,
                fragment,
            ),
        )
        .unwrap();
        let result = anonymize(&input, &output, &[]);
        assert_eq!(result.status.code(), Some(2), "{name}");
        assert!(!output.exists(), "{name}");
        let stderr = String::from_utf8(result.stderr).unwrap();
        assert!(stderr.contains("JPEG"), "{name}: {stderr}");
        let (status, report) = validate(&input, &[]);
        assert_eq!(status, Some(1), "{name}");
        assert_eq!(
            finding_severity(&report, "jpeg_codestream_unparseable").as_deref(),
            Some("error"),
            "{name}"
        );
    }
}

#[test]
fn encapsulated_total_length_follows_segment_removal_and_is_validated() {
    const CODE: &str = "encapsulated_total_length_mismatch";
    fn write_with_total_length(
        object: &mut dicom_anon::DefaultDicomObject,
        value: usize,
        path: &Path,
    ) {
        object.put(InMemElement::new(
            tags::ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH,
            VR::UV,
            PrimitiveValue::U64(vec![u64::try_from(value).unwrap()].into()),
        ));
        let mut bytes = Vec::new();
        object.write_all(&mut bytes).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    let directory = tempfile::tempdir().unwrap();
    let samples = synthetic_samples();
    for (transfer_syntax, encode, _) in jpeg_codecs() {
        let clean = encode(&samples);
        let annotated = with_jpeg_metadata(&clean);
        let input = directory.path().join(format!("{transfer_syntax}.dcm"));
        let output = directory.path().join(format!("{transfer_syntax}-out.dcm"));
        let stale = directory
            .path()
            .join(format!("{transfer_syntax}-stale.dcm"));
        let bytes = mammography_bytes(
            MAMMOGRAPHY_FOR_PRESENTATION,
            Some("NO"),
            transfer_syntax,
            annotated.clone(),
        );
        // Each codestream ends with EOI, so the source value excludes only the padding.
        let mut object = dicom_anon::read_dicom(bytes.as_slice()).unwrap();
        write_with_total_length(&mut object, annotated.len(), &input);
        let (_, report) = validate(&input, &[]);
        assert_eq!(finding_severity(&report, CODE), None, "{transfer_syntax}");

        let result = anonymize(&input, &output, &[]);
        assert_eq!(result.status.code(), Some(0), "{transfer_syntax}");
        let mut candidate = dicom_anon::read_dicom(std::fs::File::open(&output).unwrap()).unwrap();
        let total_length = candidate
            .element(tags::ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH)
            .unwrap()
            .to_int::<u64>()
            .unwrap();
        assert_eq!(
            total_length,
            u64::try_from(clean.len()).unwrap(),
            "{transfer_syntax}: the declared length must match the rewritten stream"
        );
        let (_, report) = validate(&output, &[]);
        assert_eq!(finding_severity(&report, CODE), None, "{transfer_syntax}");

        // A value that still counts the removed segments is stale.
        write_with_total_length(&mut candidate, annotated.len(), &stale);
        let (status, report) = validate(&stale, &[]);
        assert_eq!(status, Some(1), "{transfer_syntax}");
        assert_eq!(
            finding_severity(&report, CODE).as_deref(),
            Some("error"),
            "{transfer_syntax}"
        );
    }
}
