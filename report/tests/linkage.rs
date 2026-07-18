use dicom_anon::{Options, ProfileConfiguration, SecretKey};
use dicom_anon_report::{
    Category, DetectedSpan, DicomLinkage, PrivacyDetector, ReportAnonymizer, ReportOptions, Result,
};
use dicom_core::VR;
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

struct NameDetector;

struct EmptyDetector;

impl PrivacyDetector for NameDetector {
    fn detect(&self, text: &str) -> Result<Vec<DetectedSpan>> {
        let start = text.find("Private Person").unwrap();
        Ok(vec![DetectedSpan::new(
            Category::PrivatePerson,
            start,
            start + "Private Person".len(),
            0.99,
        )?])
    }
}

impl PrivacyDetector for EmptyDetector {
    fn detect(&self, _text: &str) -> Result<Vec<DetectedSpan>> {
        Ok(Vec::new())
    }
}

fn source_bytes(patient_id: &str, patient_name: &str, study_uid: &str) -> Vec<u8> {
    let sop_uid = format!("{study_uid}.1");
    let series_uid = format!("{study_uid}.2");
    let mut object = InMemDicomObject::new_empty();
    object.put_str(
        tags::SOP_CLASS_UID,
        VR::UI,
        uids::SECONDARY_CAPTURE_IMAGE_STORAGE,
    );
    object.put_str(tags::SOP_INSTANCE_UID, VR::UI, &sop_uid);
    object.put_str(tags::STUDY_INSTANCE_UID, VR::UI, study_uid);
    object.put_str(tags::SERIES_INSTANCE_UID, VR::UI, &series_uid);
    object.put_str(tags::PATIENT_ID, VR::LO, patient_id);
    object.put_str(tags::PATIENT_NAME, VR::PN, patient_name);
    object.put_str(tags::STUDY_DATE, VR::DA, "20240101");
    object.put_str(tags::INSTANCE_CREATION_TIME, VR::TM, "101112");
    object.put_str(tags::MODALITY, VR::CS, "OT");
    let object = object
        .with_meta(FileMetaTableBuilder::new().transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN))
        .unwrap();
    let mut bytes = Vec::new();
    object.write_all(&mut bytes).unwrap();
    bytes
}

#[test]
fn dicom_linkage_overrides_model_redaction_and_emits_only_safe_manifest_values() {
    let source = source_bytes("PATIENT-001", "Private^Person", "1.2.840.999.1");
    let linkage =
        DicomLinkage::from_dicom_bytes(&[source], Options::new(SecretKey::new(KEY).unwrap()))
            .unwrap();
    let anonymizer = ReportAnonymizer::new(NameDetector, ReportOptions::default());
    let input = b"Patient Private Person has ID PATIENT-001 for study 1.2.840.999.1.";

    let result = anonymizer
        .anonymize_text_with_linkage(input, linkage.sensitive_map())
        .unwrap();
    let text = String::from_utf8(result.bytes).unwrap();

    assert!(!text.contains("Private Person"));
    assert!(!text.contains("PATIENT-001"));
    assert!(!text.contains("1.2.840.999.1"));
    assert!(text.contains("ANON^"));
    assert!(text.contains(&linkage.manifest().patient_output_id));
    assert!(text.contains(&linkage.manifest().dicom_objects[0].study_instance_uid));
    assert_eq!(result.report.linked_spans, 3);

    let manifest = serde_json::to_string(linkage.manifest()).unwrap();
    assert!(!manifest.contains("Private"));
    assert!(!manifest.contains("PATIENT-001"));
    assert!(!manifest.contains("1.2.840.999.1\""));
}

#[test]
fn linkage_rejects_dicom_inputs_from_different_patients() {
    let first = source_bytes("PATIENT-001", "Private^Person", "1.2.840.999.1");
    let second = source_bytes("PATIENT-002", "Other^Person", "1.2.840.999.2");

    let error = DicomLinkage::from_dicom_bytes(
        &[first, second],
        Options::new(SecretKey::new(KEY).unwrap()),
    )
    .err()
    .expect("mixed patient error");

    assert!(error.to_string().contains("different patient scopes"));
    assert!(!error.to_string().contains("PATIENT-001"));
    assert!(!error.to_string().contains("PATIENT-002"));
}

#[test]
fn association_manifest_binds_to_output_without_source_hashes() {
    let source = source_bytes("PATIENT-001", "Private^Person", "1.2.840.999.1");
    let mut linkage =
        DicomLinkage::from_dicom_bytes(&[source], Options::new(SecretKey::new(KEY).unwrap()))
            .unwrap();

    linkage.manifest_mut().bind_output(b"safe output");

    let digest = linkage.manifest().output_sha256.as_deref().unwrap();
    assert_eq!(digest.len(), 64);
    assert!(!serde_json::to_string(linkage.manifest())
        .unwrap()
        .contains("source_sha"));
}

#[test]
fn linkage_does_not_replace_a_uid_prefix_inside_another_uid() {
    let source = source_bytes("PATIENT-001", "Private^Person", "1.2.840.999.1");
    let linkage =
        DicomLinkage::from_dicom_bytes(&[source], Options::new(SecretKey::new(KEY).unwrap()))
            .unwrap();
    let anonymizer = ReportAnonymizer::new(EmptyDetector, ReportOptions::default());
    let input = b"Unrelated study 1.2.840.999.1.9 remains unchanged.";

    let result = anonymizer
        .anonymize_text_with_linkage(input, linkage.sensitive_map())
        .unwrap();

    assert_eq!(result.bytes, input);
    assert_eq!(result.report.linked_spans, 0);
}

#[test]
fn linkage_reassociates_instance_creation_time() {
    let source = source_bytes("PATIENT-001", "Private^Person", "1.2.840.999.1");
    let linkage =
        DicomLinkage::from_dicom_bytes(&[source], Options::new(SecretKey::new(KEY).unwrap()))
            .unwrap();
    let anonymizer = ReportAnonymizer::new(EmptyDetector, ReportOptions::default());

    let result = anonymizer
        .anonymize_text_with_linkage(b"Created at 101112.", linkage.sensitive_map())
        .unwrap();

    assert_eq!(result.report.linked_spans, 1);
    assert!(!String::from_utf8(result.bytes).unwrap().contains("101112"));
}

#[test]
fn safe_harbor_linkage_uses_the_clinical_trial_subject_id() {
    let source = source_bytes("PATIENT-001", "Private^Person", "1.2.840.999.1");
    let mut options = Options::new(SecretKey::new(KEY).unwrap());
    options.patient_scope = Some("SUBJECT-42".to_owned());
    options.profile = ProfileConfiguration::safe_harbor();
    options.profile.uid_mappings = [
        ("1.2.840.999.1".to_owned(), "2.25.101".to_owned()),
        ("1.2.840.999.1.1".to_owned(), "2.25.102".to_owned()),
        ("1.2.840.999.1.2".to_owned(), "2.25.103".to_owned()),
    ]
    .into_iter()
    .collect();
    let linkage = DicomLinkage::from_dicom_bytes(&[source], options).unwrap();
    let anonymizer = ReportAnonymizer::new(NameDetector, ReportOptions::default());

    let result = anonymizer
        .anonymize_text_with_linkage(b"Patient Private Person", linkage.sensitive_map())
        .unwrap();
    let text = String::from_utf8(result.bytes).unwrap();

    assert_eq!(
        linkage.manifest().patient_output_attribute,
        "ClinicalTrialSubjectID"
    );
    assert!(text.contains(&linkage.manifest().patient_output_id));
    assert!(!text.contains("Private Person"));
}
