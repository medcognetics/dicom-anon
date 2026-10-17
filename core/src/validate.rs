//! Metadata conformance validation.

use std::path::Path;

use dicom_core::header::Header;
use dicom_core::value::Value;
use dicom_core::{Tag, VR};
use dicom_dictionary_std::tags;
use dicom_object::mem::{InMemDicomObject, InMemElement};
use dicom_object::DefaultDicomObject;
use serde::{Deserialize, Serialize};

use crate::pixel::{self, Codestream, PixelGateViolation};
use crate::profile::{
    profile_basic_action, profile_option_action, profile_rule_for, safe_private_tags,
    DeidentificationPolicy, DicomOption, ProfileAction, ProfileConfiguration, PROFILE_NAME,
    PROFILE_VERSION,
};
use crate::pseudonym::is_patient_id;
use crate::{read_dicom, Anonymizer, Error, Options, Result};

/// Finding severity. Only `error` findings make a report non-conformant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The metadata violates the selected profile.
    Error,
    /// The condition is outside the metadata-only conformance scope.
    Warning,
}

/// Amount of source information used by validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationMode {
    CandidateOnly,
    SourceComparison,
}

/// Configuration used for candidate-only validation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ValidationOptions {
    pub profile: ProfileConfiguration,
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
    /// High-level policy assessed.
    pub policy: DeidentificationPolicy,
    /// Sorted option set assessed.
    pub active_options: Vec<DicomOption>,
    /// Whether only the candidate or an original/candidate pair was assessed.
    pub validation_mode: ValidationMode,
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

/// Validate a DICOM file using an explicit profile configuration.
pub fn validate_file_with_options(
    path: impl AsRef<Path>,
    options: &ValidationOptions,
) -> Result<ValidationReport> {
    let path = path.as_ref();
    let file = std::fs::File::open(path).map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })?;
    let object = read_dicom(file)?;
    validate_object_with_options(&object, options)
}

/// Validate a parsed object against the metadata portion of the selected profile.
#[must_use]
pub fn validate_object(object: &DefaultDicomObject) -> ValidationReport {
    validate_object_with_options(object, &ValidationOptions::default())
        .expect("default validation configuration is valid")
}

/// Validate a parsed candidate against an explicit profile configuration.
pub fn validate_object_with_options(
    object: &DefaultDicomObject,
    options: &ValidationOptions,
) -> Result<ValidationReport> {
    options.profile.validate()?;
    let mut report = ValidationReport {
        conformant: true,
        profile: profile_name(&options.profile),
        profile_version: PROFILE_VERSION.to_owned(),
        policy: options.profile.policy,
        active_options: options.profile.options.iter().copied().collect(),
        validation_mode: ValidationMode::CandidateOnly,
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
        expected_temporal_marker(&options.profile),
        "longitudinal_dates_not_modified",
        &mut report,
    );
    require_method_description(object, &options.profile, &mut report);
    require_method_codes(object, &options.profile, &mut report);
    validate_meta(object, &mut report);
    validate_dataset(object, "root", &options.profile, &mut report)?;

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
    validate_pixel_gates(object, &options.profile, &mut report);
    if !options.profile.require_no_burned_in_annotation
        && element_text(object, tags::BURNED_IN_ANNOTATION)
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
    push_finding(
        &mut report,
        Severity::Warning,
        "source_not_compared",
        None,
        "root",
        "candidate-only validation cannot prove exact keep, clean, or UID transformations without the original object",
    );
    if options.profile.policy == DeidentificationPolicy::SafeHarborMetadata {
        if !element_text(object, tags::CLINICAL_TRIAL_SUBJECT_ID)
            .is_some_and(|value| is_patient_id(&value))
        {
            push_finding(
                &mut report,
                Severity::Error,
                "safe_harbor_subject_token_invalid",
                Some(tags::CLINICAL_TRIAL_SUBJECT_ID),
                "root",
                "Safe Harbor metadata output requires the generated subject-token format",
            );
        }
        push_finding(
            &mut report,
            Severity::Warning,
            "safe_harbor_external_review_required",
            None,
            "root",
            "pixel content, external mapping controls, and the covered entity's actual-knowledge assessment require external review",
        );
    }

    report.findings.sort_by(|left, right| {
        (&left.path, &left.tag, &left.code).cmp(&(&right.path, &right.tag, &right.code))
    });
    report.conformant = !report
        .findings
        .iter()
        .any(|finding| finding.severity == Severity::Error);
    Ok(report)
}

fn validate_pixel_gates(
    object: &DefaultDicomObject,
    profile: &ProfileConfiguration,
    report: &mut ValidationReport,
) {
    for violation in pixel::gate_violations(
        object,
        Some(object.meta().media_storage_sop_class_uid()),
        profile,
    ) {
        let (code, tag, message) = match violation {
            PixelGateViolation::BurnedInAnnotationNotNo => (
                "burned_in_annotation_not_no",
                tags::BURNED_IN_ANNOTATION,
                "Burned In Annotation is not NO; the configured gate requires NO",
            ),
            PixelGateViolation::SopClassNotAllowed => (
                "sop_class_not_allowed",
                tags::SOP_CLASS_UID,
                "SOP Class UID is not in the configured SOP Class allowlist",
            ),
        };
        push_finding(report, Severity::Error, code, Some(tag), "root", message);
    }
    if pixel::total_length_mismatch(object, object.meta().transfer_syntax()) {
        push_finding(
            report,
            Severity::Error,
            "encapsulated_total_length_mismatch",
            Some(tags::ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH),
            "root",
            "Encapsulated Pixel Data Value Total Length does not match the encapsulated Pixel Data fragments (PS3.3 C.7.6.6)",
        );
    }
    let Some(kind) = Codestream::for_transfer_syntax(object.meta().transfer_syntax()) else {
        return;
    };
    let jpeg2000 = kind == Codestream::Jpeg2000;
    let mut findings = Vec::new();
    match pixel::count_removable_segments(object, kind) {
        Err(_) if jpeg2000 => findings.push((
            "jpeg2000_codestream_unparseable",
            "JPEG 2000 Pixel Data could not be parsed for comment (COM) marker segments",
        )),
        Err(_) => findings.push((
            "jpeg_codestream_unparseable",
            "JPEG or JPEG-LS Pixel Data could not be parsed for comment (COM) and application (APPn) marker segments",
        )),
        Ok(removed) => {
            if removed.comments > 0 {
                findings.push(if jpeg2000 {
                    (
                        "jpeg2000_comment_present",
                        "JPEG 2000 Pixel Data contains comment (COM) marker segments",
                    )
                } else {
                    (
                        "jpeg_comment_present",
                        "JPEG or JPEG-LS Pixel Data contains comment (COM) marker segments",
                    )
                });
            }
            if removed.applications > 0 {
                findings.push((
                    "jpeg_application_segment_present",
                    "JPEG or JPEG-LS Pixel Data contains application (APPn) marker segments that decoding does not need",
                ));
            }
        }
    }
    for (code, message) in findings {
        push_finding(
            report,
            Severity::Error,
            code,
            Some(tags::PIXEL_DATA),
            "root",
            message,
        );
    }
}

fn require_method_description(
    object: &InMemDicomObject,
    profile: &ProfileConfiguration,
    report: &mut ValidationReport,
) {
    if !element_text(object, tags::DEIDENTIFICATION_METHOD)
        .is_some_and(|value| method_description_matches(&value, profile))
    {
        push_finding(
            report,
            Severity::Error,
            "deidentification_method_text_mismatch",
            Some(tags::DEIDENTIFICATION_METHOD),
            "root",
            "De-identification Method does not match the configured policy and options",
        );
    }
}

fn method_description_matches(value: &str, profile: &ProfileConfiguration) -> bool {
    let mut parts = value.split('\\');
    if parts.next() != Some("DICOM PS3.15 Basic Profile") {
        return false;
    }
    if !profile
        .options
        .iter()
        .all(|option| parts.next() == Some(option.method_code().1))
    {
        return false;
    }
    let expected_prefix = match profile.policy {
        DeidentificationPolicy::Dicom => "dicom-anon ",
        DeidentificationPolicy::SafeHarborMetadata => "dicom-anon safe-harbor-metadata ",
    };
    parts
        .next()
        .and_then(|implementation| implementation.strip_prefix(expected_prefix))
        == Some(PROFILE_VERSION)
        && parts.next().is_none()
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

fn validate_dataset(
    object: &InMemDicomObject,
    path: &str,
    profile: &ProfileConfiguration,
    report: &mut ValidationReport,
) -> Result<()> {
    let safe_private_tags = safe_private_tags(object, profile);
    for element in object.iter() {
        report.checked_attributes += 1;
        let tag = element.tag();
        if tag.0 % 2 == 1 && !safe_private_tags.contains(&tag) {
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

        let rule = profile_rule_for(tag, profile)?;
        let action = rule.map(|rule| {
            if profile.has(DicomOption::CleanGraphics)
                && profile_option_action(tag, DicomOption::CleanGraphics).is_some()
            {
                ProfileAction::Remove
            } else {
                rule.action
            }
        });
        if let (Some(rule), Some(action)) = (rule, action) {
            if action == ProfileAction::Clean
                && element.vr() == VR::SQ
                && profile.has(DicomOption::CleanDescriptors)
                && profile_option_action(tag, DicomOption::CleanDescriptors).is_some()
                && !(profile.has(DicomOption::CleanStructuredContent)
                    && profile_option_action(tag, DicomOption::CleanStructuredContent).is_some())
                && profile_basic_action(tag) == Some(ProfileAction::Remove)
            {
                push_finding(
                    report,
                    Severity::Error,
                    "removed_attribute_present",
                    Some(tag),
                    path,
                    &format!("{} has no configured safe descriptor mapping", rule.name),
                );
            }
            match action {
                ProfileAction::Remove => push_finding(
                    report,
                    Severity::Error,
                    "removed_attribute_present",
                    Some(tag),
                    path,
                    &format!("{} must be removed by the selected profile", rule.name),
                ),
                ProfileAction::Dummy | ProfileAction::RemapUid
                    if value_is_empty(element)
                        && !(profile.policy == DeidentificationPolicy::SafeHarborMetadata
                            && tag == tags::PATIENT_ID) =>
                {
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
                ProfileAction::Clean
                    if !matches!(element.vr(), VR::SQ | VR::DA | VR::DT | VR::TM)
                        && !valid_configured_cleaned_value(element, profile) =>
                {
                    push_finding(
                        report,
                        Severity::Error,
                        "unapproved_cleaned_descriptor",
                        Some(tag),
                        path,
                        &format!("{} is not an approved cleaned value", rule.name),
                    )
                }
                _ => {}
            }
        }

        if tag == tags::PATIENT_AGE && !valid_patient_age(element) {
            push_finding(
                report,
                Severity::Error,
                "invalid_patient_age",
                Some(tag),
                path,
                "Patient's Age must be a valid AS value and may not exceed 090Y",
            );
        }

        if rule.is_none()
            && profile.policy == DeidentificationPolicy::SafeHarborMetadata
            && !is_safe_harbor_attestation_attribute(tag, path)
        {
            match element.vr() {
                VR::UI if !valid_uid_values(element) => push_finding(
                    report,
                    Severity::Error,
                    "invalid_replacement_uid",
                    Some(tag),
                    path,
                    "unknown UID attribute does not contain valid DICOM UID values",
                ),
                VR::AE | VR::LO | VR::LT | VR::PN | VR::SH | VR::ST | VR::UC | VR::UR | VR::UT
                    if !valid_cleaned_descriptor(element, profile) =>
                {
                    push_finding(
                        report,
                        Severity::Error,
                        "unapproved_cleaned_descriptor",
                        Some(tag),
                        path,
                        "unknown text attribute is not an approved cleaned value",
                    )
                }
                _ => {}
            }
        }

        if profile.policy == DeidentificationPolicy::SafeHarborMetadata {
            validate_safe_harbor_value(element, path, report);
        }

        if let Value::Sequence(sequence) = element.value() {
            let sequence_is_dummy = action == Some(ProfileAction::Dummy)
                || (action == Some(ProfileAction::Clean)
                    && [
                        DicomOption::CleanDescriptors,
                        DicomOption::CleanStructuredContent,
                    ]
                    .into_iter()
                    .any(|option| {
                        profile.has(option) && profile_option_action(tag, option).is_some()
                    }));
            for (index, item) in sequence.items().iter().enumerate() {
                let item_path = format!("{path}/{}/item[{}]", format_tag(tag), index + 1);
                if sequence_is_dummy {
                    validate_dummy_dataset(item, &item_path, report);
                } else {
                    validate_dataset(item, &item_path, profile, report)?;
                }
            }
        }
    }
    Ok(())
}

fn is_safe_harbor_attestation_attribute(tag: Tag, path: &str) -> bool {
    matches!(
        tag,
        tags::PATIENT_IDENTITY_REMOVED
            | tags::DEIDENTIFICATION_METHOD
            | tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED
            | tags::CLINICAL_TRIAL_SUBJECT_ID
    ) || path.starts_with("root/(0012,0064)/item[")
        && matches!(
            tag,
            tags::CODE_VALUE | tags::CODING_SCHEME_DESIGNATOR | tags::CODE_MEANING
        )
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

fn require_method_codes(
    object: &InMemDicomObject,
    profile: &ProfileConfiguration,
    report: &mut ValidationReport,
) {
    let items = object
        .get(tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE)
        .and_then(|element| element.value().items())
        .unwrap_or_default();
    let expected_count = profile.options.len() + 1;
    let mut seen = 0_u16;
    let exact = items.len() == expected_count
        && items.iter().all(|item| {
            let Some(code) = element_text(item, tags::CODE_VALUE) else {
                return false;
            };
            let Some((index, (_, expected_meaning))) = expected_method_codes(profile)
                .enumerate()
                .find(|(_, (expected_code, _))| *expected_code == code)
            else {
                return false;
            };
            let bit = 1_u16 << index;
            let unique = seen & bit == 0;
            seen |= bit;
            unique
                && element_text(item, tags::CODING_SCHEME_DESIGNATOR).as_deref() == Some("DCM")
                && element_text(item, tags::CODE_MEANING).as_deref() == Some(expected_meaning)
        });
    if !exact {
        push_finding(
            report,
            Severity::Error,
            "deidentification_method_code_mismatch",
            Some(tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE),
            "root",
            "De-identification Method Code Sequence does not exactly match the configured DCM codes",
        );
    }
}

fn expected_method_codes(
    profile: &ProfileConfiguration,
) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
    std::iter::once(("113100", "Basic Application Confidentiality Profile"))
        .chain(profile.options.iter().map(|option| option.method_code()))
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

fn profile_name(profile: &ProfileConfiguration) -> String {
    match profile.policy {
        DeidentificationPolicy::Dicom if *profile == ProfileConfiguration::dicom_extended() => {
            PROFILE_NAME.to_owned()
        }
        DeidentificationPolicy::Dicom if *profile == ProfileConfiguration::dicom() => {
            "DICOM PS3.15 Basic Application Level Confidentiality Profile".to_owned()
        }
        DeidentificationPolicy::Dicom => {
            "Configured DICOM PS3.15 Basic Application Confidentiality Profile".to_owned()
        }
        DeidentificationPolicy::SafeHarborMetadata => {
            "DICOM PS3.15 profile with Safe Harbor metadata policy".to_owned()
        }
    }
}

fn expected_temporal_marker(profile: &ProfileConfiguration) -> &'static str {
    if profile.has(DicomOption::RetainFullDates) {
        "UNMODIFIED"
    } else if profile.has(DicomOption::RetainModifiedDates) {
        "MODIFIED"
    } else {
        "REMOVED"
    }
}

fn valid_cleaned_descriptor(element: &InMemElement, profile: &ProfileConfiguration) -> bool {
    if !matches!(
        element.vr(),
        VR::AE | VR::CS | VR::LO | VR::LT | VR::PN | VR::SH | VR::ST | VR::UC | VR::UR | VR::UT
    ) {
        return true;
    }
    let Ok(value) = element.to_str() else {
        return false;
    };
    let value = value.trim_matches([' ', '\0']);
    if matches!(value, "ANONYMIZED" | "ANON^ANON") {
        return true;
    }
    profile
        .descriptor_mappings
        .get(&format!("{:04X}{:04X}", element.tag().0, element.tag().1))
        .is_some_and(|mappings| mappings.values().any(|allowed| allowed == value))
}

fn valid_configured_cleaned_value(element: &InMemElement, profile: &ProfileConfiguration) -> bool {
    if profile.has(DicomOption::CleanDescriptors)
        && profile_option_action(element.tag(), DicomOption::CleanDescriptors).is_some()
        && profile_basic_action(element.tag()) == Some(ProfileAction::Remove)
    {
        let Ok(value) = element.to_str() else {
            return false;
        };
        let value = value.trim_matches([' ', '\0']);
        return profile
            .descriptor_mappings
            .get(&format!("{:04X}{:04X}", element.tag().0, element.tag().1))
            .is_some_and(|mappings| mappings.values().any(|allowed| allowed == value));
    }
    valid_cleaned_descriptor(element, profile)
}

fn valid_patient_age(element: &InMemElement) -> bool {
    let Ok(value) = element.to_str() else {
        return false;
    };
    if value.is_empty() {
        return true;
    }
    let bytes = value.as_bytes();
    bytes.len() == 4
        && bytes[..3].iter().all(u8::is_ascii_digit)
        && matches!(bytes[3], b'D' | b'W' | b'M' | b'Y')
        && (bytes[3] != b'Y' || value[..3].parse::<u16>().is_ok_and(|age| age <= 90))
}

fn validate_safe_harbor_value(element: &InMemElement, path: &str, report: &mut ValidationReport) {
    let tag = element.tag();
    if matches!(
        tag,
        tags::PATIENT_ID | tags::PATIENT_NAME | tags::PATIENT_BIRTH_DATE
    ) && !value_is_empty(element)
    {
        push_finding(
            report,
            Severity::Error,
            "safe_harbor_identifier_present",
            Some(tag),
            path,
            "Safe Harbor metadata policy requires this patient identifier to be empty",
        );
    }
    let valid = match element.vr() {
        VR::DA => element.to_str().is_ok_and(|value| {
            value.is_empty()
                || value.split('\\').all(|date| {
                    date == "19000101"
                        || (date.len() == 8
                            && date.ends_with("0101")
                            && date.bytes().all(|byte| byte.is_ascii_digit()))
                })
        }),
        VR::DT => element.to_str().is_ok_and(|value| {
            value.is_empty()
                || value.split('\\').all(|datetime| {
                    datetime == "19000101000000"
                        || (datetime.len() == 4
                            && datetime.bytes().all(|byte| byte.is_ascii_digit()))
                })
        }),
        VR::TM => element
            .to_str()
            .is_ok_and(|value| value.is_empty() || value.split('\\').all(|time| time == "000000")),
        _ => true,
    };
    if !valid {
        push_finding(
            report,
            Severity::Error,
            "safe_harbor_temporal_detail_present",
            Some(tag),
            path,
            "temporal value retains detail beyond the configured Safe Harbor convention",
        );
    }
}

/// Compare a candidate with the exact output produced from its original source.
pub fn validate_source_object(
    source: &DefaultDicomObject,
    candidate: &DefaultDicomObject,
    anonymization_options: Options,
) -> Result<ValidationReport> {
    let validation_options = ValidationOptions {
        profile: anonymization_options.profile.clone(),
    };
    let anonymizer = Anonymizer::new(anonymization_options)?;
    let (expected, _) = anonymizer.anonymize(source)?;
    let mut expected_bytes = Vec::new();
    crate::write_dicom(&expected, &mut expected_bytes)?;
    let expected = read_dicom(&expected_bytes[..])?;
    let mut candidate_bytes = Vec::new();
    crate::write_dicom(candidate, &mut candidate_bytes)?;
    let normalized_candidate = read_dicom(&candidate_bytes[..])?;
    let mut report = validate_object_with_options(candidate, &validation_options)?;
    report.validation_mode = ValidationMode::SourceComparison;
    report
        .findings
        .retain(|finding| finding.code != "source_not_compared");
    compare_datasets(&expected, &normalized_candidate, "root", &mut report);
    report.findings.sort_by(|left, right| {
        (&left.path, &left.tag, &left.code).cmp(&(&right.path, &right.tag, &right.code))
    });
    report.conformant = !report
        .findings
        .iter()
        .any(|finding| finding.severity == Severity::Error);
    Ok(report)
}

fn compare_datasets(
    expected: &InMemDicomObject,
    candidate: &InMemDicomObject,
    path: &str,
    report: &mut ValidationReport,
) {
    let tags = expected
        .iter()
        .map(|element| element.tag())
        .chain(candidate.iter().map(|element| element.tag()))
        .collect::<std::collections::BTreeSet<_>>();
    for tag in tags {
        let expected_element = expected.get(tag);
        let candidate_element = candidate.get(tag);
        match (expected_element, candidate_element) {
            (Some(expected_element), Some(candidate_element)) => {
                if expected_element.vr() != candidate_element.vr() {
                    push_finding(
                        report,
                        Severity::Error,
                        "source_transformation_mismatch",
                        Some(tag),
                        path,
                        "candidate value does not match the configured transformation of the source",
                    );
                    continue;
                }
                match (expected_element.value(), candidate_element.value()) {
                    (Value::Sequence(expected), Value::Sequence(candidate)) => {
                        if expected.items().len() != candidate.items().len() {
                            push_finding(
                                report,
                                Severity::Error,
                                "source_transformation_mismatch",
                                Some(tag),
                                path,
                                "candidate sequence length does not match the configured transformation of the source",
                            );
                            continue;
                        }
                        for (index, (expected_item, candidate_item)) in expected
                            .items()
                            .iter()
                            .zip(candidate.items())
                            .enumerate()
                        {
                            compare_datasets(
                                expected_item,
                                candidate_item,
                                &format!("{path}/{}[{index}]", format_tag(tag)),
                                report,
                            );
                        }
                    }
                    (expected_value, candidate_value) if expected_value != candidate_value => {
                        push_finding(
                            report,
                            Severity::Error,
                            "source_transformation_mismatch",
                            Some(tag),
                            path,
                            "candidate value does not match the configured transformation of the source",
                        );
                    }
                    _ => {}
                }
            }
            _ => push_finding(
                report,
                Severity::Error,
                "source_transformation_mismatch",
                Some(tag),
                path,
                "candidate attribute presence does not match the configured transformation of the source",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Anonymizer, Options, SecretKey};
    use dicom_core::value::DataSetSequence;
    use dicom_core::PrimitiveValue;
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

    fn safe_harbor_options() -> Options {
        let mut options = Options::new(SecretKey::new(KEY).unwrap());
        options.profile = ProfileConfiguration::safe_harbor();
        options.patient_scope = Some("external-scope".to_owned());
        options.profile.uid_mappings = [
            ("1.2.3.4", "2.25.101"),
            ("1.2.3", "2.25.102"),
            ("1.2.3.1", "2.25.103"),
        ]
        .into_iter()
        .map(|(source, replacement)| (source.to_owned(), replacement.to_owned()))
        .collect();
        options
    }

    #[test]
    fn encapsulated_total_length_must_match_fragments() {
        const RLE: &str = "1.2.840.10008.1.2.5";
        let encapsulated = || {
            Some(Value::PixelSequence(
                dicom_core::value::PixelFragmentSequence::new(
                    Vec::<u32>::new(),
                    vec![vec![1, 2, 3, 4], vec![5, 6, 7, 0]],
                ),
            ))
        };
        let native = || Some(Value::Primitive(PrimitiveValue::from(vec![1_u8, 2])));
        let mismatch = |transfer_syntax: &str,
                        pixel_data: Option<Value<InMemDicomObject, Vec<u8>>>,
                        total_length: Option<Vec<u64>>| {
            let mut object = InMemDicomObject::new_empty();
            if let Some(value) = pixel_data {
                object.put(InMemElement::new(tags::PIXEL_DATA, VR::OB, value));
            }
            if let Some(values) = total_length {
                object.put(InMemElement::new(
                    tags::ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH,
                    VR::UV,
                    PrimitiveValue::U64(values.into()),
                ));
            }
            let object = object
                .with_meta(FileMetaTableBuilder::new().transfer_syntax(transfer_syntax))
                .unwrap();
            let report = validate_object(&object);
            let findings = report
                .findings
                .iter()
                .filter(|finding| finding.code == "encapsulated_total_length_mismatch")
                .collect::<Vec<_>>();
            if let [finding] = findings.as_slice() {
                assert_eq!(finding.severity, Severity::Error);
                assert_eq!(finding.tag.as_deref(), Some("(7FE0,0003)"));
                assert!(!report.conformant);
            }
            findings.len() == 1
        };
        // PS3.3 C.7.6.6: 8 fragment bytes, without the trailing padding byte of the last one.
        let jpeg = uids::JPEG_LOSSLESS_SV1;
        assert!(!mismatch(jpeg, encapsulated(), Some(vec![7])));
        assert!(!mismatch(jpeg, encapsulated(), None));
        assert!(!mismatch(jpeg, None, None));
        for (transfer_syntax, pixel_data, total_length) in [
            (jpeg, encapsulated(), Some(vec![8])),
            (jpeg, encapsulated(), Some(vec![6])),
            (jpeg, encapsulated(), Some(vec![7, 7])),
            (jpeg, encapsulated(), Some(Vec::new())),
            (jpeg, None, Some(vec![7])),
            (uids::EXPLICIT_VR_LITTLE_ENDIAN, native(), Some(vec![2])),
            (RLE, encapsulated(), Some(vec![6])),
            (RLE, encapsulated(), Some(vec![9])),
        ] {
            assert!(
                mismatch(transfer_syntax, pixel_data, total_length.clone()),
                "{transfer_syntax}: {total_length:?}"
            );
        }
        // Without a codestream end marker, a trailing zero byte can be data or padding.
        for total_length in [7, 8] {
            assert!(!mismatch(RLE, encapsulated(), Some(vec![total_length])));
        }
        // Padding is at most one alignment byte. Earlier zero bytes are encoded data.
        let zeros = || {
            Some(Value::PixelSequence(
                dicom_core::value::PixelFragmentSequence::new(
                    Vec::<u32>::new(),
                    vec![vec![1, 2, 3, 4], vec![5, 0, 0, 0]],
                ),
            ))
        };
        for (total_length, expected) in [(8, false), (7, false), (6, true), (5, true)] {
            assert_eq!(
                mismatch(RLE, zeros(), Some(vec![total_length])),
                expected,
                "{total_length}"
            );
        }
    }

    #[test]
    fn anonymizer_output_is_conformant() {
        let anonymizer = Anonymizer::new(Options::new(SecretKey::new(KEY).unwrap())).unwrap();
        let (object, _) = anonymizer.anonymize(&source()).unwrap();
        let report = validate_object(&object);
        assert!(report.conformant, "{:#?}", report.findings);
        assert_eq!(report.validation_mode, ValidationMode::CandidateOnly);
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "source_not_compared"));
    }

    #[test]
    fn source_comparison_verifies_exact_configured_output_without_leaking_values() {
        let options = Options::new(SecretKey::new(KEY).unwrap());
        let anonymizer = Anonymizer::new(options.clone()).unwrap();
        let source = source();
        let (candidate, _) = anonymizer.anonymize(&source).unwrap();

        let report = validate_source_object(&source, &candidate, options.clone()).unwrap();
        assert!(report.conformant, "{:#?}", report.findings);
        assert_eq!(report.validation_mode, ValidationMode::SourceComparison);
        assert!(!report
            .findings
            .iter()
            .any(|finding| finding.code == "source_not_compared"));

        let mut altered = candidate.clone();
        altered.put_str(tags::PATIENT_ID, VR::LO, "WRONG-OUTPUT");
        let report = validate_source_object(&source, &altered, options.clone()).unwrap();
        assert!(!report.conformant);
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.code == "source_transformation_mismatch")
            .unwrap();
        assert!(!finding.message.contains("WRONG-OUTPUT"));
        assert!(!finding.message.contains("patient"));

        let mut wrong_vr = candidate.clone();
        wrong_vr.put_str(tags::PATIENT_ID, VR::SH, "WRONG-VR");
        let report = validate_source_object(&source, &wrong_vr, options.clone()).unwrap();
        assert!(!report.conformant);

        let mut missing_attribute = candidate.clone();
        missing_attribute.remove_element(tags::STUDY_DATE);
        let report = validate_source_object(&source, &missing_attribute, options.clone()).unwrap();
        assert!(!report.conformant);

        let wrong_key = Options::new(SecretKey::new([8_u8; 32]).unwrap());
        let report = validate_source_object(&source, &candidate, wrong_key).unwrap();
        assert!(!report.conformant);
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("Person^Private"));
        assert!(!serialized.contains("PATIENT"));

        const SENSITIVE_INVALID_DATE: &str = "20201399";
        let mut invalid_source = source.clone();
        invalid_source.put_str(tags::STUDY_DATE, VR::DA, SENSITIVE_INVALID_DATE);
        let error = validate_source_object(&invalid_source, &candidate, options)
            .unwrap_err()
            .to_string();
        assert!(!error.contains(SENSITIVE_INVALID_DATE));
    }

    #[test]
    fn candidate_validation_requires_exact_deidentification_markers() {
        let options = Options::new(SecretKey::new(KEY).unwrap());
        let (mut candidate, _) = Anonymizer::new(options)
            .unwrap()
            .anonymize(&source())
            .unwrap();
        candidate.put_str(
            tags::DEIDENTIFICATION_METHOD,
            VR::LO,
            "identifying free text",
        );
        let mut code_items = candidate
            .get(tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE)
            .unwrap()
            .value()
            .items()
            .unwrap()
            .to_vec();
        code_items[0].put_str(tags::CODING_SCHEME_DESIGNATOR, VR::SH, "LOCAL");
        let mut extra = InMemDicomObject::new_empty();
        extra.put_str(tags::CODE_VALUE, VR::SH, "999999");
        extra.put_str(tags::CODING_SCHEME_DESIGNATOR, VR::SH, "DCM");
        extra.put_str(tags::CODE_MEANING, VR::LO, "identifying free text");
        code_items.push(extra);
        candidate.put(InMemElement::new(
            tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE,
            VR::SQ,
            DataSetSequence::from(code_items),
        ));

        let report = validate_object(&candidate);
        for code in [
            "deidentification_method_text_mismatch",
            "deidentification_method_code_mismatch",
        ] {
            assert!(
                report.findings.iter().any(|finding| finding.code == code),
                "missing {code}: {:#?}",
                report.findings
            );
        }
    }

    #[test]
    fn safe_harbor_candidate_requires_generated_subject_token() {
        let options = safe_harbor_options();
        let profile = options.profile.clone();
        let (mut candidate, _) = Anonymizer::new(options)
            .unwrap()
            .anonymize(&source())
            .unwrap();
        candidate.remove_element(tags::CLINICAL_TRIAL_SUBJECT_ID);
        let report = validate_object_with_options(
            &candidate,
            &ValidationOptions {
                profile: profile.clone(),
            },
        )
        .unwrap();
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "safe_harbor_subject_token_invalid"));

        candidate.put_str(tags::CLINICAL_TRIAL_SUBJECT_ID, VR::LO, "PATIENT-RAW");
        let report =
            validate_object_with_options(&candidate, &ValidationOptions { profile }).unwrap();
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "safe_harbor_subject_token_invalid"));
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
            "required_replacement_empty",
            "unapproved_cleaned_descriptor",
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
    fn safe_harbor_candidate_validation_rejects_identifiers_temporal_detail_and_unknown_text() {
        let options = safe_harbor_options();
        let profile = options.profile.clone();
        let (mut candidate, _) = Anonymizer::new(options)
            .unwrap()
            .anonymize(&source())
            .unwrap();
        candidate.put_str(tags::PATIENT_ID, VR::LO, "IDENTIFIER");
        candidate.put_str(tags::PATIENT_AGE, VR::AS, "091Y");
        candidate.put_str(Tag(0x7776, 0x0001), VR::LO, "UNAPPROVED TEXT");
        candidate.put_str(Tag(0x7776, 0x0002), VR::UI, "01.invalid");
        candidate.put_str(Tag(0x7776, 0x0003), VR::DA, "20240203");
        candidate.put_str(Tag(0x7776, 0x0004), VR::DT, "20240203121212");
        candidate.put_str(Tag(0x7776, 0x0005), VR::TM, "121212");

        let report =
            validate_object_with_options(&candidate, &ValidationOptions { profile }).unwrap();
        for code in [
            "invalid_patient_age",
            "invalid_replacement_uid",
            "safe_harbor_identifier_present",
            "safe_harbor_temporal_detail_present",
            "unapproved_cleaned_descriptor",
        ] {
            assert!(
                report.findings.iter().any(|finding| finding.code == code),
                "missing {code}: {:#?}",
                report.findings
            );
        }
    }

    #[test]
    fn configured_validator_checks_clean_graphics_and_every_dummy_sequence_constraint() {
        let mut options = Options::new(SecretKey::new(KEY).unwrap());
        options.profile.options.insert(DicomOption::CleanGraphics);
        let profile = options.profile.clone();
        let (mut candidate, _) = Anonymizer::new(options)
            .unwrap()
            .anonymize(&source())
            .unwrap();
        candidate.put_str(Tag(0x6000, 0x4000), VR::LT, "ANNOTATION");

        let mut nested = InMemDicomObject::new_empty();
        nested.put(InMemElement::empty(tags::CODE_MEANING, VR::LO));
        let mut item = InMemDicomObject::new_empty();
        item.put_str(Tag(0x0011, 0x0010), VR::LO, "PRIVATE");
        item.put_str(Tag(0x0004, 0x1500), VR::CS, "FILEID");
        item.put(InMemElement::empty(tags::CODE_MEANING, VR::LO));
        item.put_str(tags::SOP_INSTANCE_UID, VR::UI, "01.invalid");
        item.put(InMemElement::new(
            tags::CONTENT_SEQUENCE,
            VR::SQ,
            dicom_core::value::DataSetSequence::from(vec![nested]),
        ));
        candidate.put(InMemElement::new(
            tags::INSTITUTION_CODE_SEQUENCE,
            VR::SQ,
            dicom_core::value::DataSetSequence::from(vec![item]),
        ));

        let report =
            validate_object_with_options(&candidate, &ValidationOptions { profile }).unwrap();
        for code in [
            "file_set_attribute_present",
            "invalid_replacement_uid",
            "private_attribute_present",
            "removed_attribute_present",
            "required_replacement_empty",
        ] {
            assert!(
                report.findings.iter().any(|finding| finding.code == code),
                "missing {code}: {:#?}",
                report.findings
            );
        }
    }

    #[test]
    fn configured_validator_accepts_only_mapped_optional_descriptors() {
        let mut source = source();
        source.put_str(tags::STUDY_DESCRIPTION, VR::LO, "SOURCE DESCRIPTION");
        let mut options = Options::new(SecretKey::new(KEY).unwrap());
        options
            .profile
            .descriptor_mappings
            .entry("00081030".to_owned())
            .or_default()
            .insert("SOURCE DESCRIPTION".to_owned(), "SAFE STUDY".to_owned());
        let profile = options.profile.clone();
        let (candidate, _) = Anonymizer::new(options)
            .unwrap()
            .anonymize(&source)
            .unwrap();
        assert!(
            validate_object_with_options(
                &candidate,
                &ValidationOptions {
                    profile: profile.clone()
                }
            )
            .unwrap()
            .conformant
        );

        let mut altered = candidate;
        altered.put_str(tags::STUDY_DESCRIPTION, VR::LO, "ANONYMIZED");
        let report =
            validate_object_with_options(&altered, &ValidationOptions { profile }).unwrap();
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "unapproved_cleaned_descriptor"));
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
