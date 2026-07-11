use std::io::Write;
use std::process::{Command, Output, Stdio};

use dicom_core::VR;
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

const KEY_ENV: &str = "DICOM_ANON_TEST_KEY";
const KEY: &str = "0123456789abcdef0123456789abcdef";

fn source_bytes() -> Vec<u8> {
    let mut object = InMemDicomObject::new_empty();
    object.put_str(
        tags::SOP_CLASS_UID,
        VR::UI,
        uids::SECONDARY_CAPTURE_IMAGE_STORAGE,
    );
    object.put_str(tags::SOP_INSTANCE_UID, VR::UI, "1.2.840.999.1.1");
    object.put_str(tags::STUDY_INSTANCE_UID, VR::UI, "1.2.840.999.1");
    object.put_str(tags::SERIES_INSTANCE_UID, VR::UI, "1.2.840.999.1.2");
    object.put_str(tags::PATIENT_ID, VR::LO, "PATIENT-001");
    object.put_str(tags::PATIENT_NAME, VR::PN, "Private^Person");
    object.put_str(tags::STUDY_DATE, VR::DA, "20240101");
    object.put_str(tags::MODALITY, VR::CS, "OT");
    let object = object
        .with_meta(FileMetaTableBuilder::new().transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN))
        .unwrap();
    let mut bytes = Vec::new();
    object.write_all(&mut bytes).unwrap();
    bytes
}

fn run_with_stdin(command: &mut Command, input: &[u8]) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn anonymize_stdout_pipes_into_json_validation() {
    let anonymized = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_dicom-anonymize"))
            .args(["-", "-", "--key-env", KEY_ENV, "--color", "never"])
            .env(KEY_ENV, KEY),
        &source_bytes(),
    );
    assert_eq!(anonymized.status.code(), Some(0));
    assert!(anonymized.stderr.is_empty());
    assert!(anonymized.stdout.starts_with(&[0; 128]));

    let validated = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_dicom-validate"))
            .args(["-", "--format", "json", "--color", "always"]),
        &anonymized.stdout,
    );
    assert_eq!(validated.status.code(), Some(0));
    assert!(validated.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&validated.stdout).unwrap();
    assert_eq!(report["conformant"], true);
    assert!(report["findings"].as_array().unwrap().is_empty());
    assert!(!validated.stdout.windows(2).any(|window| window == b"\x1b["));
}

#[test]
fn validator_returns_one_for_a_valid_file_that_is_not_deidentified() {
    let output = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_dicom-validate")).args(["-", "--format", "json"]),
        &source_bytes(),
    );
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["conformant"], false);
    assert!(report["findings"].as_array().unwrap().len() >= 3);
}

#[test]
fn in_place_output_is_atomic_and_validated() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input.dcm");
    std::fs::write(&path, source_bytes()).unwrap();

    let anonymized = Command::new(env!("CARGO_BIN_EXE_dicom-anonymize"))
        .arg(&path)
        .args(["--in-place", "--key-env", KEY_ENV])
        .env(KEY_ENV, KEY)
        .output()
        .unwrap();
    assert_eq!(anonymized.status.code(), Some(0));
    assert!(anonymized.stdout.is_empty());

    let validated = Command::new(env!("CARGO_BIN_EXE_dicom-validate"))
        .arg(&path)
        .args(["--quiet", "--color", "never"])
        .output()
        .unwrap();
    assert_eq!(validated.status.code(), Some(0));
    let text = String::from_utf8(validated.stdout).unwrap();
    assert!(text.starts_with("PASS  dicom-validate"));
    assert!(text.contains("Conformant:  yes"));
    assert!(!text.contains("Findings"));
}

#[test]
fn usage_errors_return_two() {
    let output = Command::new(env!("CARGO_BIN_EXE_dicom-anonymize"))
        .args(["input.dcm", "output.dcm", "--in-place"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
}

#[test]
fn file_input_to_stdout_supports_key_files_and_verbose_reports() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.dcm");
    let key = directory.path().join("key.bin");
    std::fs::write(&input, source_bytes()).unwrap();
    std::fs::write(&key, KEY).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_dicom-anonymize"))
        .arg(&input)
        .args(["--key-file", key.to_str().unwrap(), "--verbose"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.starts_with(&[0; 128]));
    let report: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(report["profile_version"], "2026c");
}

#[test]
fn stdin_can_write_an_atomic_output_file() {
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("output.dcm");
    let output = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_dicom-anonymize"))
            .arg("-")
            .arg(&output_path)
            .args(["--key-env", KEY_ENV])
            .env(KEY_ENV, KEY),
        &source_bytes(),
    );
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(dicom_anon::validate_file(output_path).unwrap().conformant);
}

#[test]
fn anonymizer_runtime_errors_return_two_without_binary_stdout() {
    let missing_key = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_dicom-anonymize"))
            .args([
                "-",
                "--key-env",
                "MISSING_DICOM_ANON_KEY",
                "--color",
                "always",
            ])
            .env_remove("MISSING_DICOM_ANON_KEY"),
        &source_bytes(),
    );
    assert_eq!(missing_key.status.code(), Some(2));
    assert!(missing_key.stdout.is_empty());
    assert!(missing_key
        .stderr
        .windows(2)
        .any(|window| window == b"\x1b["));

    let in_place_stdin = Command::new(env!("CARGO_BIN_EXE_dicom-anonymize"))
        .args(["-", "--in-place", "--key-env", KEY_ENV])
        .env(KEY_ENV, KEY)
        .output()
        .unwrap();
    assert_eq!(in_place_stdin.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&in_place_stdin.stderr).contains("requires a file input"));
}

#[test]
fn validator_text_modes_color_status_and_show_warnings() {
    let anonymized = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_dicom-anonymize"))
            .args(["-", "--key-env", KEY_ENV])
            .env(KEY_ENV, KEY),
        &source_bytes(),
    );
    let mut object = dicom_anon::read_dicom(&anonymized.stdout[..]).unwrap();
    object.put_str(tags::BURNED_IN_ANNOTATION, VR::CS, "YES");
    object.put(dicom_core::DataElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        dicom_core::PrimitiveValue::from(vec![0_u8, 1]),
    ));
    let mut bytes = Vec::new();
    object.write_all(&mut bytes).unwrap();

    let warning = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_dicom-validate")).args([
            "-",
            "--verbose",
            "--color",
            "always",
        ]),
        &bytes,
    );
    assert_eq!(warning.status.code(), Some(0));
    let warning_text = String::from_utf8(warning.stdout).unwrap();
    assert!(warning_text.contains("WARN"));
    assert!(warning_text.contains("pixel_data_not_assessed"));
    assert!(warning_text.contains("DICOM edition:"));
    assert!(warning_text.contains("\x1b["));

    let failure = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_dicom-validate")).args(["-", "--color", "always"]),
        &source_bytes(),
    );
    assert_eq!(failure.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&failure.stdout).contains("\x1b[1;31mFAIL"));

    let runtime_error = run_with_stdin(
        &mut Command::new(env!("CARGO_BIN_EXE_dicom-validate")),
        b"not dicom",
    );
    assert_eq!(runtime_error.status.code(), Some(2));
    assert!(runtime_error.stdout.is_empty());
    assert!(String::from_utf8_lossy(&runtime_error.stderr).contains("failed to read DICOM"));
}
