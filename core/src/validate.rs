//! Metadata conformance validation.

use std::path::Path;

use dicom_core::header::Header;
use dicom_core::value::Value;
use dicom_core::{Tag, VR};
use dicom_dictionary_std::tags;
use dicom_object::mem::{InMemDicomObject, InMemElement};
use dicom_object::DefaultDicomObject;
use serde::{Deserialize, Serialize};

use crate::profile::{profile_rule, ProfileAction, PROFILE_NAME, PROFILE_VERSION};
use crate::{read_dicom, Error, Result};

/// Finding severity. Only `error` findings make a report non-conformant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The metadata violates the selected profile.
    Error,
    /// The condition is outside the metadata-only conformance scope.
    Warning,
}

/// One deterministic validation finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationFinding {
    /// Stable machine-readable identifier.
    pub code: String,
    /// Severity.
    pub severity: Severity,
    /// Attribute tag, when applicable.
    pub tag: Option<String>,
    /// Nested data-set path from the root object.
    pub path: String,
    /// Human-readable description.
    pub message: String,
}

/// Metadata conformance report for the selected PS3.15 profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationReport {
    /// True when no error-severity findings were found.
    pub conformant: bool,
    /// Profile assessed.
    pub profile: String,
    /// Standard edition used by the rule table.
    pub profile_version: String,
    /// Number of attributes inspected, including nested items.
    pub checked_attributes: u64,
    /// Findings in deterministic path/tag/code order.
    pub findings: Vec<ValidationFinding>,
}

/// Validate a DICOM file from disk.
pub fn validate_file(path: impl AsRef<Path>) -> Result<ValidationReport> {
    let path = path.as_ref();
    let file = std::fs::File::open(path).map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })?;
    let object = read_dicom(file)?;
    Ok(validate_object(&object))
}

/// Validate a parsed object against the metadata portion of the selected profile.
#[must_use]
pub fn validate_object(object: &DefaultDicomObject) -> ValidationReport {
    let mut report = ValidationReport {
        conformant: true,
        profile: PROFILE_NAME.to_owned(),
        profile_version: PROFILE_VERSION.to_owned(),
        checked_attributes: 0,
        findings: Vec::new(),
    };

    require_value(
        object,
        tags::PATIENT_IDENTITY_REMOVED,
        "YES",
        "patient_identity_not_removed",
        &mut report,
    );
    require_value(
        object,
        tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED,
        "MODIFIED",
        "longitudinal_dates_not_modified",
        &mut report,
    );
    require_method_codes(object, &mut report);
    validate_meta(object, &mut report);
    validate_dataset(object, "root", &mut report);

    if object.get(tags::PIXEL_DATA).is_some() {
        push_finding(
            &mut report,
            Severity::Warning,
            "pixel_data_not_assessed",
            Some(tags::PIXEL_DATA),
            "root",
            "pixel data was not inspected for burned-in identifying information; the Clean Pixel Data Option is not claimed",
        );
    }
    if element_text(object, tags::BURNED_IN_ANNOTATION)
        .is_some_and(|value| value.eq_ignore_ascii_case("YES"))
    {
        push_finding(
            &mut report,
            Severity::Warning,
            "burned_in_annotation_present",
            Some(tags::BURNED_IN_ANNOTATION),
            "root",
            "Burned In Annotation is YES; pixel-level review is required before disclosure",
        );
    }

    report.findings.sort_by(|left, right| {
        (&left.path, &left.tag, &left.code).cmp(&(&right.path, &right.tag, &right.code))
    });
    report.conformant = !report
        .findings
        .iter()
        .any(|finding| finding.severity == Severity::Error);
    report
}

fn validate_meta(object: &DefaultDicomObject, report: &mut ValidationReport) {
    let meta = object.meta();
    let private_information_present = meta
        .to_element_iter()
        .any(|element| element.tag() == Tag(0x0002, 0x0102));
    for (name, present) in [
        (
            "Source Application Entity Title",
            meta.source_application_entity_title().is_some(),
        ),
        (
            "Sending Application Entity Title",
            meta.sending_application_entity_title().is_some(),
        ),
        (
            "Receiving Application Entity Title",
            meta.receiving_application_entity_title().is_some(),
        ),
        (
            "Private Information Creator UID",
            meta.private_information_creator_uid().is_some(),
        ),
        ("Private Information", private_information_present),
    ] {
        if present {
            push_finding(
                report,
                Severity::Error,
                "unsafe_file_meta_information",
                None,
                "file-meta",
                &format!("{name} remains in File Meta Information"),
            );
        }
    }

    for (name, meta_value, data_set_tag) in [
        (
            "Media Storage SOP Class UID",
            meta.media_storage_sop_class_uid(),
            tags::SOP_CLASS_UID,
        ),
        (
            "Media Storage SOP Instance UID",
            meta.media_storage_sop_instance_uid(),
            tags::SOP_INSTANCE_UID,
        ),
    ] {
        if element_text(object, data_set_tag).as_deref() != Some(meta_value) {
            push_finding(
                report,
                Severity::Error,
                "file_meta_uid_mismatch",
                Some(data_set_tag),
                "file-meta",
                &format!("{name} does not match the corresponding data-set UID"),
            );
        }
    }
}

fn validate_dataset(object: &InMemDicomObject, path: &str, report: &mut ValidationReport) {
    for element in object.iter() {
        report.checked_attributes += 1;
        let tag = element.tag();
        if tag.0 % 2 == 1 {
            push_finding(
                report,
                Severity::Error,
                "private_attribute_present",
                Some(tag),
                path,
                "private attribute was not removed",
            );
        }
        if tag.0 == 0x0004 {
            push_finding(
                report,
                Severity::Error,
                "file_set_attribute_present",
                Some(tag),
                path,
                "group 0004 attributes must not remain outside a rebuilt DICOMDIR",
            );
        }

        let rule = profile_rule(tag);
        if let Some(rule) = rule {
            match rule.action {
                ProfileAction::Remove => push_finding(
                    report,
                    Severity::Error,
                    "removed_attribute_present",
                    Some(tag),
                    path,
                    &format!("{} must be removed by the selected profile", rule.name),
                ),
                ProfileAction::Dummy | ProfileAction::RemapUid if value_is_empty(element) => {
                    push_finding(
                        report,
                        Severity::Error,
                        "required_replacement_empty",
                        Some(tag),
                        path,
                        &format!("{} requires a non-empty replacement", rule.name),
                    );
                }
                ProfileAction::RemapUid if element.vr() != VR::SQ && !valid_uid_values(element) => {
                    push_finding(
                        report,
                        Severity::Error,
                        "invalid_replacement_uid",
                        Some(tag),
                        path,
                        &format!("{} does not contain valid DICOM UID values", rule.name),
                    )
                }
                _ => {}
            }
        }

        if let Value::Sequence(sequence) = element.value() {
            let sequence_is_dummy = rule.is_some_and(|rule| rule.action == ProfileAction::Dummy);
            for (index, item) in sequence.items().iter().enumerate() {
                let item_path = format!("{path}/{}/item[{}]", format_tag(tag), index + 1);
                if sequence_is_dummy {
                    validate_dummy_dataset(item, &item_path, report);
                } else {
                    validate_dataset(item, &item_path, report);
                }
            }
        }
    }
}

#[cold]
#[inline(never)]
fn validate_dummy_dataset(object: &InMemDicomObject, path: &str, report: &mut ValidationReport) {
    for element in object.iter() {
        report.checked_attributes += 1;
        let tag = element.tag();
        if tag.0 % 2 == 1 {
            push_finding(
                report,
                Severity::Error,
                "private_attribute_present",
                Some(tag),
                path,
                "private attribute was not removed",
            );
        }
        if tag.0 == 0x0004 {
            push_finding(
                report,
                Severity::Error,
                "file_set_attribute_present",
                Some(tag),
                path,
                "group 0004 attributes must not remain outside a rebuilt DICOMDIR",
            );
        }
        if value_is_empty(element) {
            push_finding(
                report,
                Severity::Error,
                "required_replacement_empty",
                Some(tag),
                path,
                "attribute inside a dummy sequence requires a non-empty replacement",
            );
        }
        if element.vr() == VR::UI && !valid_uid_values(element) {
            push_finding(
                report,
                Severity::Error,
                "invalid_replacement_uid",
                Some(tag),
                path,
                "UID inside a dummy sequence is not a valid DICOM UID",
            );
        }
        if let Value::Sequence(sequence) = element.value() {
            for (index, item) in sequence.items().iter().enumerate() {
                validate_dummy_dataset(
                    item,
                    &format!("{path}/{}/item[{}]", format_tag(tag), index + 1),
                    report,
                );
            }
        }
    }
}

fn require_value(
    object: &InMemDicomObject,
    tag: Tag,
    expected: &str,
    code: &str,
    report: &mut ValidationReport,
) {
    if !element_text(object, tag).is_some_and(|value| value.eq_ignore_ascii_case(expected)) {
        push_finding(
            report,
            Severity::Error,
            code,
            Some(tag),
            "root",
            &format!("{} must be {expected}", format_tag(tag)),
        );
    }
}

fn require_method_codes(object: &InMemDicomObject, report: &mut ValidationReport) {
    const REQUIRED_CODES: [&str; 2] = ["113100", "113107"];
    let found = object
        .get(tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE)
        .and_then(|element| element.value().items())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| element_text(item, tags::CODE_VALUE))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for code in REQUIRED_CODES {
        if !found.iter().any(|found| found == code) {
            push_finding(
                report,
                Severity::Error,
                "deidentification_method_code_missing",
                Some(tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE),
                "root",
                &format!("De-identification Method Code Sequence is missing DCM code {code}"),
            );
        }
    }
}

fn value_is_empty(element: &InMemElement) -> bool {
    match element.value() {
        Value::Primitive(value) => value.to_str().trim_matches([' ', '\0']).is_empty(),
        Value::Sequence(value) => value.items().is_empty(),
        Value::PixelSequence(value) => value.fragments().is_empty(),
    }
}

fn valid_uid_values(element: &InMemElement) -> bool {
    element.to_str().is_ok_and(|value| {
        value.split('\\').all(|uid| {
            let uid = uid.trim_end_matches('\0');
            !uid.is_empty()
                && uid.len() <= 64
                && uid.split('.').all(|component| {
                    !component.is_empty()
                        && component.bytes().all(|byte| byte.is_ascii_digit())
                        && (component == "0" || !component.starts_with('0'))
                })
        })
    })
}

fn element_text(object: &InMemDicomObject, tag: Tag) -> Option<String> {
    object
        .get(tag)
        .and_then(|element| element.to_str().ok())
        .map(|value| value.trim_matches([' ', '\0']).to_owned())
}

fn push_finding(
    report: &mut ValidationReport,
    severity: Severity,
    code: &str,
    tag: Option<Tag>,
    path: &str,
    message: &str,
) {
    report.findings.push(ValidationFinding {
        code: code.to_owned(),
        severity,
        tag: tag.map(format_tag),
        path: path.to_owned(),
        message: message.to_owned(),
    });
}

fn format_tag(tag: Tag) -> String {
    format!("({:04X},{:04X})", tag.0, tag.1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Anonymizer, Options, SecretKey};
    use dicom_dictionary_std::uids;
    use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

    const KEY: [u8; 32] = [9; 32];

    fn source() -> DefaultDicomObject {
        let mut object = InMemDicomObject::new_empty();
        object.put_str(
            tags::SOP_CLASS_UID,
            VR::UI,
            uids::SECONDARY_CAPTURE_IMAGE_STORAGE,
        );
        object.put_str(tags::SOP_INSTANCE_UID, VR::UI, "1.2.3.4");
        object.put_str(tags::STUDY_INSTANCE_UID, VR::UI, "1.2.3");
        object.put_str(tags::SERIES_INSTANCE_UID, VR::UI, "1.2.3.1");
        object.put_str(tags::PATIENT_ID, VR::LO, "patient");
        object.put_str(tags::PATIENT_NAME, VR::PN, "Person^Private");
        object.put_str(tags::STUDY_DATE, VR::DA, "20200101");
        object
            .with_meta(FileMetaTableBuilder::new().transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN))
            .unwrap()
    }

    #[test]
    fn anonymizer_output_is_conformant() {
        let anonymizer = Anonymizer::new(Options::new(SecretKey::new(KEY).unwrap())).unwrap();
        let (object, _) = anonymizer.anonymize(&source()).unwrap();
        let report = validate_object(&object);
        assert!(report.conformant, "{:#?}", report.findings);
    }

    #[test]
    fn identifies_missing_attestation_and_private_data() {
        let mut object = source();
        object.put_str(Tag(0x0011, 0x0010), VR::LO, "PRIVATE");
        let report = validate_object(&object);
        assert!(!report.conformant);
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "patient_identity_not_removed"));
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "private_attribute_present"));
    }

    #[test]
    fn identifies_identifying_file_meta_information() {
        let object = source()
            .into_inner()
            .with_meta(
                FileMetaTableBuilder::new()
                    .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
                    .source_application_entity_title("SOURCE_AE")
                    .private_information_creator_uid("1.2.3")
                    .private_information(vec![1_u8, 2]),
            )
            .unwrap();
        let report = validate_object(&object);
        let unsafe_meta_count = report
            .findings
            .iter()
            .filter(|finding| finding.code == "unsafe_file_meta_information")
            .count();
        assert_eq!(unsafe_meta_count, 3);
    }

    #[test]
    fn reports_each_supported_failure_category_and_pixel_warnings() {
        let anonymizer = Anonymizer::new(Options::new(SecretKey::new(KEY).unwrap())).unwrap();
        let (mut object, _) = anonymizer.anonymize(&source()).unwrap();
        object.put_str(tags::STUDY_DESCRIPTION, VR::LO, "must be removed");
        object.put(InMemElement::empty(tags::PATIENT_ID, VR::LO));
        object.put_str(tags::STUDY_INSTANCE_UID, VR::UI, "01.invalid");
        object.put(InMemElement::empty(tags::SOURCE_IMAGE_SEQUENCE, VR::SQ));
        object.put_str(Tag(0x0004, 0x1500), VR::CS, "FILEID");
        object.put_str(Tag(0x0011, 0x0010), VR::LO, "PRIVATE");
        object.put_str(tags::BURNED_IN_ANNOTATION, VR::CS, "YES");
        object.put(InMemElement::new(
            tags::PIXEL_DATA,
            VR::OB,
            dicom_core::PrimitiveValue::from(vec![0_u8, 1]),
        ));

        let report = validate_object(&object);
        for expected in [
            "burned_in_annotation_present",
            "file_set_attribute_present",
            "invalid_replacement_uid",
            "pixel_data_not_assessed",
            "private_attribute_present",
            "removed_attribute_present",
            "required_replacement_empty",
        ] {
            assert!(
                report
                    .findings
                    .iter()
                    .any(|finding| finding.code == expected),
                "missing {expected}: {:#?}",
                report.findings
            );
        }
        assert!(!report.conformant);
    }

    #[test]
    fn validate_file_reads_from_disk_and_checks_meta_uids() {
        let anonymizer = Anonymizer::new(Options::new(SecretKey::new(KEY).unwrap())).unwrap();
        let (object, _) = anonymizer.anonymize(&source()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("candidate.dcm");
        object.write_to_file(&path).unwrap();
        assert!(validate_file(&path).unwrap().conformant);

        let meta = FileMetaTableBuilder::new()
            .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
            .media_storage_sop_class_uid(uids::SECONDARY_CAPTURE_IMAGE_STORAGE)
            .media_storage_sop_instance_uid("9.9.9")
            .build()
            .unwrap();
        let mismatched = object.into_inner().with_exact_meta(meta);
        let report = validate_object(&mismatched);
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "file_meta_uid_mismatch"));

        let missing = directory.path().join("missing.dcm");
        assert!(matches!(validate_file(missing), Err(Error::Io { .. })));
    }
}
