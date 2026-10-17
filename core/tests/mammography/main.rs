//! Mammography, DBT, synthesized 2D, and JPEG 2000 conformance tests (issue #21).
//!
//! Each synthetic fixture goes through the `dicom`, `dicom-extended`, and
//! `safe-harbor-metadata` policies. The checks cover metadata only; Pixel Data stays unassessed.

mod fixtures;

use std::collections::BTreeMap;
use std::process::Command;

use dicom_anon::{
    read_dicom, validate_source_object, write_dicom, Anonymizer, DefaultDicomObject, Options,
    ProfileConfiguration, SecretKey,
};
use dicom_core::header::Header;
use dicom_core::value::Value;
use dicom_core::{Tag, VR};
use dicom_dictionary_std::tags;
use dicom_object::mem::{InMemDicomObject, InMemElement};

use fixtures::{Fixtures, PLACEHOLDER, SOURCE_UIDS};

const KEY: &str = "mammography-fixture-key-0123456789abcdef";
const KEY_ENV: &str = "DICOM_ANON_MAMMOGRAPHY_TEST_KEY";
const PATIENT_SCOPE: &str = "synthetic-subject-001";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Policy {
    Dicom,
    DicomExtended,
    SafeHarbor,
}

const POLICIES: [Policy; 3] = [Policy::Dicom, Policy::DicomExtended, Policy::SafeHarbor];

impl Policy {
    const fn name(self) -> &'static str {
        match self {
            Self::Dicom => "dicom",
            Self::DicomExtended => "dicom-extended",
            Self::SafeHarbor => "safe-harbor-metadata",
        }
    }

    fn options(self) -> Options {
        let mut options = Options::new(SecretKey::new(KEY).unwrap());
        options.profile = match self {
            Self::Dicom => ProfileConfiguration::dicom(),
            Self::DicomExtended => ProfileConfiguration::dicom_extended(),
            Self::SafeHarbor => {
                options.patient_scope = Some(PATIENT_SCOPE.to_owned());
                let mut profile = ProfileConfiguration::safe_harbor();
                profile.uid_mappings = uid_map();
                profile
            }
        };
        options
    }
}

/// Test-only Safe Harbor UID map with short replacement UIDs.
fn uid_map() -> BTreeMap<String, String> {
    SOURCE_UIDS
        .iter()
        .enumerate()
        .map(|(index, uid)| ((*uid).to_owned(), format!("2.25.{}", 1_000 + index)))
        .collect()
}

/// Serialize and parse again, as a file or network transfer does.
fn round_trip(object: &DefaultDicomObject) -> DefaultDicomObject {
    let mut bytes = Vec::new();
    write_dicom(object, &mut bytes).unwrap();
    read_dicom(&bytes[..]).unwrap()
}

fn anonymize(policy: Policy, source: &DefaultDicomObject) -> DefaultDicomObject {
    let (output, _) = Anonymizer::new(policy.options())
        .unwrap()
        .anonymize(source)
        .unwrap_or_else(|error| panic!("{}: {error}", policy.name()));
    round_trip(&output)
}

/// Source and output for every fixture and policy.
fn cases() -> Vec<(&'static str, Policy, DefaultDicomObject, DefaultDicomObject)> {
    let fixtures = Fixtures::new();
    let mut cases = Vec::new();
    for (name, fixture) in fixtures.named() {
        let source = round_trip(fixture);
        for policy in POLICIES {
            let output = anonymize(policy, &source);
            cases.push((name, policy, source.clone(), output));
        }
    }
    cases
}

/// Visit every element at every sequence depth. The callback receives the item depth.
fn visit(object: &InMemDicomObject, depth: usize, callback: &mut impl FnMut(&InMemElement, usize)) {
    for element in object.iter() {
        callback(element, depth);
        if let Value::Sequence(sequence) = element.value() {
            for item in sequence.items() {
                visit(item, depth + 1, callback);
            }
        }
    }
}

fn text(object: &InMemDicomObject, tag: Tag) -> Option<String> {
    object
        .get(tag)
        .and_then(|element| element.to_str().ok())
        .map(|value| value.trim_end_matches(['\0', ' ']).to_owned())
}

fn item(object: &InMemDicomObject, tag: Tag, index: usize) -> &InMemDicomObject {
    &object
        .get(tag)
        .unwrap_or_else(|| panic!("missing {tag}"))
        .items()
        .unwrap_or_else(|| panic!("{tag} is not a sequence"))[index]
}

fn contains_placeholder(bytes: &[u8]) -> bool {
    bytes
        .windows(PLACEHOLDER.len())
        .any(|window| window == PLACEHOLDER.as_bytes())
}

fn source_uid_in(value: &str) -> bool {
    value
        .split('\\')
        .map(|uid| uid.trim_end_matches(['\0', ' ']))
        .any(|uid| SOURCE_UIDS.contains(&uid))
}

/// Guard against a vacuous scan: each fixture must hold every placeholder category.
#[test]
fn fixtures_hold_placeholder_identifiers_at_every_level() {
    const ROOT: [Tag; 25] = [
        tags::PATIENT_NAME,
        tags::PATIENT_ID,
        tags::ISSUER_OF_PATIENT_ID,
        tags::PATIENT_ADDRESS,
        tags::PATIENT_TELEPHONE_NUMBERS,
        tags::PATIENT_COMMENTS,
        tags::ACCESSION_NUMBER,
        tags::STUDY_ID,
        tags::REQUESTED_PROCEDURE_ID,
        tags::REFERRING_PHYSICIAN_NAME,
        tags::REFERRING_PHYSICIAN_ADDRESS,
        tags::REFERRING_PHYSICIAN_TELEPHONE_NUMBERS,
        tags::OPERATORS_NAME,
        tags::PERFORMING_PHYSICIAN_NAME,
        tags::INSTITUTION_NAME,
        tags::INSTITUTION_ADDRESS,
        tags::INSTITUTIONAL_DEPARTMENT_NAME,
        tags::STATION_NAME,
        tags::DEVICE_SERIAL_NUMBER,
        tags::DETECTOR_ID,
        tags::STUDY_DESCRIPTION,
        tags::SERIES_DESCRIPTION,
        tags::IMAGE_COMMENTS,
        tags::PROTOCOL_NAME,
        Tag(0x0009, 0x1001),
    ];
    let fixtures = Fixtures::new();
    for (name, fixture) in fixtures.named() {
        let source = round_trip(fixture);
        for tag in ROOT {
            assert!(
                text(&source, tag).is_some_and(|value| value.contains(PLACEHOLDER)),
                "{name}: {tag}"
            );
        }
        assert_eq!(text(&source, tags::PATIENT_AGE).as_deref(), Some("092Y"));
        assert_eq!(
            source.meta().source_application_entity_title(),
            Some(PLACEHOLDER)
        );
        assert!(contains_placeholder(
            source
                .meta()
                .private_information
                .as_deref()
                .unwrap_or_default()
        ));
        let mut nested = Vec::new();
        visit(&source, 0, &mut |element, depth| {
            if depth > 0
                && element
                    .to_str()
                    .is_ok_and(|value| value.contains(PLACEHOLDER))
            {
                nested.push((element.tag(), depth));
            }
        });
        assert!(nested.contains(&(tags::PATIENT_ID, 1)), "{name}");
        if name == "dbt" {
            assert!(nested.contains(&(tags::FRAME_COMMENTS, 2)), "{name}");
            assert!(nested.contains(&(Tag(0x0009, 0x1001), 2)), "{name}");
        }
        if name == "synthesized-2d" {
            assert!(text(&source, tags::DERIVATION_DESCRIPTION)
                .is_some_and(|value| value.contains(PLACEHOLDER)));
        }
    }
}

#[test]
fn outputs_contain_no_placeholder_text_or_source_uid_at_any_depth() {
    for (name, policy, _, output) in cases() {
        let context = format!("{name} / {}", policy.name());
        for element in output.meta().to_element_iter() {
            let bytes = element.value().to_bytes().unwrap_or_default();
            assert!(
                !contains_placeholder(&bytes),
                "{context}: meta {}",
                element.tag()
            );
            if let Ok(value) = element.to_str() {
                assert!(!source_uid_in(&value), "{context}: meta {}", element.tag());
            }
        }
        let mut checked = 0;
        visit(&output, 0, &mut |element, _| {
            checked += 1;
            if let Value::Primitive(value) = element.value() {
                assert!(
                    !contains_placeholder(&value.to_bytes()),
                    "{context}: {}",
                    element.tag()
                );
                if element.vr() == VR::UI {
                    assert!(
                        !source_uid_in(&value.to_str()),
                        "{context}: {}",
                        element.tag()
                    );
                }
            }
            if element.tag().0 % 2 == 1 {
                panic!("{context}: private attribute {} retained", element.tag());
            }
        });
        assert!(checked > 40, "{context}: only {checked} attributes checked");
    }
}

#[test]
fn transfer_syntax_and_pixel_module_are_bit_identical() {
    for (name, policy, source, output) in cases() {
        let context = format!("{name} / {}", policy.name());
        assert_eq!(
            source.meta().transfer_syntax(),
            output.meta().transfer_syntax(),
            "{context}"
        );
        assert_eq!(
            output.meta().transfer_syntax().trim_end_matches('\0'),
            fixtures::JPEG_2000_LOSSLESS,
            "{context}"
        );
        for tag in fixtures::PIXEL_MODULE_TAGS {
            assert_eq!(source.get(tag), output.get(tag), "{context}: {tag}");
        }
        // Compare the encapsulated value field by field. In dicom-core 0.10, an undefined
        // element length never equals another undefined length.
        let source_pixels = source.get(tags::PIXEL_DATA).unwrap();
        let output_pixels = output.get(tags::PIXEL_DATA).unwrap();
        assert_eq!(source_pixels.vr(), output_pixels.vr(), "{context}");
        let (source_pixels, output_pixels) = (source_pixels.value(), output_pixels.value());
        assert_eq!(
            source_pixels.offset_table(),
            output_pixels.offset_table(),
            "{context}"
        );
        assert_eq!(
            source_pixels.fragments(),
            output_pixels.fragments(),
            "{context}"
        );
        assert!(output_pixels
            .fragments()
            .is_some_and(|fragments| !fragments.is_empty()));
    }

    let fixtures = Fixtures::new();
    let dbt = round_trip(&fixtures.tomosynthesis);
    let pixels = dbt.get(tags::PIXEL_DATA).unwrap().value();
    assert_eq!(text(&dbt, tags::NUMBER_OF_FRAMES).as_deref(), Some("2"));
    assert_eq!(pixels.offset_table().unwrap().len(), 2);
    assert_eq!(pixels.fragments().unwrap().len(), 3);
}

#[test]
fn view_laterality_presentation_intent_and_frame_type_are_unchanged() {
    for (name, policy, source, output) in cases() {
        let context = format!("{name} / {}", policy.name());
        for tag in fixtures::VIEW_TAGS {
            assert!(source.get(tag).is_some(), "{context}: fixture lacks {tag}");
            assert_eq!(source.get(tag), output.get(tag), "{context}: {tag}");
        }
        if name == "dbt" {
            for (sequence, tag) in [
                (tags::X_RAY3_D_FRAME_TYPE_SEQUENCE, tags::FRAME_TYPE),
                (tags::FRAME_ANATOMY_SEQUENCE, tags::FRAME_LATERALITY),
            ] {
                let read = |object: &DefaultDicomObject| {
                    let shared = item(object, tags::SHARED_FUNCTIONAL_GROUPS_SEQUENCE, 0);
                    item(shared, sequence, 0).get(tag).cloned()
                };
                assert!(read(&source).is_some(), "{context}: fixture lacks {tag}");
                assert_eq!(read(&source), read(&output), "{context}: {tag}");
            }
        }
    }
}

/// View Code Sequence handling depends on issue #20. These assertions record the behavior at
/// this revision.
// FOLLOW-UP(#20): when code sequences keep reviewed coded values, require an unchanged View
// Code Sequence in Safe Harbor mode too.
#[test]
fn view_code_sequence_records_current_behavior() {
    for (name, policy, source, output) in cases() {
        let context = format!("{name} / {}", policy.name());
        let source_code = item(&source, tags::VIEW_CODE_SEQUENCE, 0);
        let output_code = item(&output, tags::VIEW_CODE_SEQUENCE, 0);
        match policy {
            Policy::Dicom | Policy::DicomExtended => {
                assert_eq!(source_code, output_code, "{context}");
            }
            Policy::SafeHarbor => {
                for tag in [
                    tags::CODE_VALUE,
                    tags::CODING_SCHEME_DESIGNATOR,
                    tags::CODE_MEANING,
                ] {
                    assert_eq!(
                        text(output_code, tag).as_deref(),
                        Some("ANONYMIZED"),
                        "{context}: {tag}"
                    );
                }
            }
        }
    }
}

#[test]
fn safe_harbor_generalizes_temporal_values_at_every_depth_and_caps_age() {
    for (name, policy, _, output) in cases() {
        let context = format!("{name} / {}", policy.name());
        let age = text(&output, tags::PATIENT_AGE);
        match policy {
            Policy::Dicom => assert_eq!(age, None, "{context}"),
            Policy::DicomExtended | Policy::SafeHarbor => {
                assert_eq!(age.as_deref(), Some("090Y"), "{context}");
            }
        }
        if policy != Policy::SafeHarbor {
            continue;
        }
        let mut deepest = 0;
        visit(&output, 0, &mut |element, depth| {
            let Ok(value) = element.to_str() else {
                return;
            };
            let value = value.trim_end_matches(['\0', ' ']);
            if value.is_empty() || !matches!(element.vr(), VR::DA | VR::DT | VR::TM) {
                return;
            }
            deepest = deepest.max(depth);
            for value in value.split('\\') {
                let digits = value.bytes().all(|byte| byte.is_ascii_digit());
                let valid = match element.vr() {
                    VR::DA => value.len() == 8 && digits && value.ends_with("0101"),
                    VR::DT => value.len() == 4 && digits,
                    _ => value == "000000",
                };
                assert!(valid, "{context}: {} = {value}", element.tag());
            }
        });
        if name == "dbt" {
            // Frame Acquisition DateTime sits in Per-Frame > Frame Content.
            assert_eq!(deepest, 2, "{context}");
            let frame = item(
                item(&output, tags::PER_FRAME_FUNCTIONAL_GROUPS_SEQUENCE, 1),
                tags::FRAME_CONTENT_SEQUENCE,
                0,
            );
            assert_eq!(
                text(frame, tags::FRAME_ACQUISITION_DATE_TIME).as_deref(),
                Some("2024")
            );
        }
        assert_eq!(
            text(&output, tags::STUDY_DATE).as_deref(),
            Some("20240101"),
            "{context}"
        );
        assert_eq!(
            text(&output, tags::STUDY_TIME).as_deref(),
            Some("000000"),
            "{context}"
        );
        assert_eq!(
            text(&output, tags::PATIENT_BIRTH_DATE).as_deref(),
            Some(""),
            "{context}"
        );
    }
}

#[test]
fn references_and_file_meta_stay_consistent_across_objects() {
    let fixtures = Fixtures::new();
    for policy in POLICIES {
        let context = policy.name();
        let [mammogram, dbt, synthesized] = [
            &fixtures.mammogram,
            &fixtures.tomosynthesis,
            &fixtures.synthesized,
        ]
        .map(|fixture| anonymize(policy, &round_trip(fixture)));

        let dbt_sop = text(&dbt, tags::SOP_INSTANCE_UID).unwrap();
        let source_image = item(&synthesized, tags::SOURCE_IMAGE_SEQUENCE, 0);
        assert_eq!(
            text(source_image, tags::REFERENCED_SOP_INSTANCE_UID).as_deref(),
            Some(dbt_sop.as_str()),
            "{context}"
        );
        assert_eq!(
            text(source_image, tags::REFERENCED_SOP_CLASS_UID).as_deref(),
            Some(fixtures::BREAST_TOMOSYNTHESIS),
            "{context}"
        );

        let organization = text(
            item(&dbt, tags::DIMENSION_ORGANIZATION_SEQUENCE, 0),
            tags::DIMENSION_ORGANIZATION_UID,
        )
        .unwrap();
        assert!(!organization.is_empty(), "{context}");
        for index in 0..2 {
            let dimension = item(&dbt, tags::DIMENSION_INDEX_SEQUENCE, index);
            assert_eq!(
                text(dimension, tags::DIMENSION_ORGANIZATION_UID).as_deref(),
                Some(organization.as_str()),
                "{context}"
            );
        }

        for tag in [tags::STUDY_INSTANCE_UID, tags::PATIENT_ID, tags::DEVICE_UID] {
            assert_eq!(text(&mammogram, tag), text(&dbt, tag), "{context}: {tag}");
            assert_eq!(text(&dbt, tag), text(&synthesized, tag), "{context}: {tag}");
        }
        assert_eq!(
            text(&dbt, tags::FRAME_OF_REFERENCE_UID),
            text(&synthesized, tags::FRAME_OF_REFERENCE_UID),
            "{context}"
        );

        for (object, sop_class) in [
            (&mammogram, fixtures::MG_FOR_PRESENTATION),
            (&dbt, fixtures::BREAST_TOMOSYNTHESIS),
            (&synthesized, fixtures::MG_FOR_PRESENTATION),
        ] {
            let meta = object.meta();
            assert_eq!(
                meta.media_storage_sop_instance_uid().trim_end_matches('\0'),
                text(object, tags::SOP_INSTANCE_UID).unwrap(),
                "{context}"
            );
            assert_eq!(
                meta.media_storage_sop_class_uid().trim_end_matches('\0'),
                sop_class,
                "{context}"
            );
            assert_eq!(meta.source_application_entity_title(), None, "{context}");
            assert_eq!(meta.private_information_creator_uid(), None, "{context}");
        }
        if policy == Policy::SafeHarbor {
            assert_eq!(dbt_sop, uid_map()[fixtures::SOP_DBT], "{context}");
        }
    }
}

#[test]
fn output_is_deterministic() {
    let fixtures = Fixtures::new();
    for (name, fixture) in fixtures.named() {
        for policy in POLICIES {
            let source = round_trip(fixture);
            let encode = |object: DefaultDicomObject| {
                let mut bytes = Vec::new();
                write_dicom(&object, &mut bytes).unwrap();
                bytes
            };
            assert_eq!(
                encode(anonymize(policy, &source)),
                encode(anonymize(policy, &source)),
                "{name} / {}",
                policy.name()
            );
        }
    }
}

#[test]
fn source_aware_validation_passes() {
    for (name, policy, source, output) in cases() {
        let report = validate_source_object(&source, &output, policy.options()).unwrap();
        assert!(
            report.conformant,
            "{name} / {}: {:?}",
            policy.name(),
            report.findings
        );
    }
}

#[test]
fn command_line_source_aware_validation_passes() {
    let directory = tempfile::tempdir().unwrap();
    let uid_map_path = directory.path().join("uid-map.json");
    std::fs::write(&uid_map_path, serde_json::to_vec(&uid_map()).unwrap()).unwrap();
    let fixtures = Fixtures::new();
    for (name, fixture) in fixtures.named() {
        let source_path = directory.path().join(format!("{name}.dcm"));
        fixture.write_to_file(&source_path).unwrap();
        for policy in POLICIES {
            let context = format!("{name} / {}", policy.name());
            let output_path = directory
                .path()
                .join(format!("{name}-{}.dcm", policy.name()));
            let mut profile = vec!["--policy", policy.name()];
            if policy == Policy::SafeHarbor {
                profile.extend([
                    "--uid-map",
                    uid_map_path.to_str().unwrap(),
                    "--patient-scope",
                    PATIENT_SCOPE,
                ]);
            }
            let anonymized = Command::new(env!("CARGO_BIN_EXE_dicom-anonymize"))
                .arg(&source_path)
                .arg(&output_path)
                .args(&profile)
                .args(["--key-env", KEY_ENV])
                .env(KEY_ENV, KEY)
                .output()
                .unwrap();
            assert_eq!(
                anonymized.status.code(),
                Some(0),
                "{context}: {}",
                String::from_utf8_lossy(&anonymized.stderr)
            );
            let validated = Command::new(env!("CARGO_BIN_EXE_dicom-validate"))
                .arg(&output_path)
                .arg("--source")
                .arg(&source_path)
                .args(&profile)
                .args(["--key-env", KEY_ENV, "--format", "json"])
                .env(KEY_ENV, KEY)
                .output()
                .unwrap();
            assert_eq!(
                validated.status.code(),
                Some(0),
                "{context}: {}{}",
                String::from_utf8_lossy(&validated.stdout),
                String::from_utf8_lossy(&validated.stderr)
            );
            let report: serde_json::Value = serde_json::from_slice(&validated.stdout).unwrap();
            assert_eq!(report["conformant"], true, "{context}");
            assert_eq!(report["validation_mode"], "source_comparison", "{context}");
        }
    }
}
