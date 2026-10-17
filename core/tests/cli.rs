use std::io::Write;
use std::process::{Command, Output, Stdio};

use dicom_core::VR;
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

const KEY_ENV: &str = "DICOM_ANON_TEST_KEY";
const KEY: &str = "0123456789abcdef0123456789abcdef";
const BASIC_PROFILE_NAME: &str = "DICOM PS3.15 Basic Application Level Confidentiality Profile";
const DICOM_EXTENDED_OPTIONS: [&str; 3] = [
    "clean-descriptors",
    "retain-modified-dates",
    "retain-patient-characteristics",
];

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
fn help_describes_the_default_dicom_extended_profile() {
    for binary in ["dicom-anonymize", "dicom-validate"] {
        let executable = match binary {
            "dicom-anonymize" => env!("CARGO_BIN_EXE_dicom-anonymize"),
            "dicom-validate" => env!("CARGO_BIN_EXE_dicom-validate"),
            _ => unreachable!(),
        };
        let output = Command::new(executable).arg("--help").output().unwrap();
        assert_eq!(output.status.code(), Some(0));
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("[default: dicom-extended]"), "{binary}");
        assert!(
            help.contains("DICOM Basic Profile with no options enabled"),
            "{binary}"
        );
        let extended_help = help
            .split("- dicom-extended:")
            .nth(1)
            .expect("dicom-extended help entry")
            .split("- safe-harbor-metadata:")
            .next()
            .unwrap();
        for option in DICOM_EXTENDED_OPTIONS {
            assert!(extended_help.contains(option), "{binary}: {option}");
        }
    }
}

#[test]
fn implicit_and_explicit_dicom_extended_profiles_match() {
    let default = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_dicom-anonymize"))
            .args(["-", "--key-env", KEY_ENV, "--verbose"])
            .env(KEY_ENV, KEY),
        &source_bytes(),
    );
    let explicit = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_dicom-anonymize"))
            .args([
                "-",
                "--policy",
                "dicom-extended",
                "--key-env",
                KEY_ENV,
                "--verbose",
            ])
            .env(KEY_ENV, KEY),
        &source_bytes(),
    );

    assert_eq!(default.status.code(), Some(0));
    assert_eq!(explicit.status.code(), Some(0));
    assert_eq!(default.stdout, explicit.stdout);
    let default_report: serde_json::Value = serde_json::from_slice(&default.stderr).unwrap();
    let explicit_report: serde_json::Value = serde_json::from_slice(&explicit.stderr).unwrap();
    assert_eq!(default_report, explicit_report);
    assert_eq!(default_report["policy"], "dicom");
    assert_eq!(
        default_report["active_options"],
        serde_json::json!(DICOM_EXTENDED_OPTIONS)
    );
}

#[test]
fn explicit_dicom_profile_uses_no_options() {
    let anonymized = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_dicom-anonymize"))
            .args(["-", "--policy", "dicom", "--key-env", KEY_ENV, "--verbose"])
            .env(KEY_ENV, KEY),
        &source_bytes(),
    );
    assert_eq!(anonymized.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&anonymized.stderr).unwrap();
    assert_eq!(report["policy"], "dicom");
    assert_eq!(report["active_options"], serde_json::json!([]));

    let object = dicom_anon::read_dicom(&anonymized.stdout[..]).unwrap();
    assert_eq!(
        object
            .element(tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED)
            .unwrap()
            .to_str()
            .unwrap(),
        "REMOVED"
    );
    assert_eq!(
        object.element(tags::STUDY_DATE).unwrap().to_str().unwrap(),
        ""
    );

    let basic_validation = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_dicom-validate"))
            .args(["-", "--policy", "dicom", "--format", "json"]),
        &anonymized.stdout,
    );
    assert_eq!(
        basic_validation.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&basic_validation.stdout)
    );
    let basic_report: serde_json::Value = serde_json::from_slice(&basic_validation.stdout).unwrap();
    assert_eq!(basic_report["profile"], BASIC_PROFILE_NAME);
    assert_eq!(basic_report["active_options"], serde_json::json!([]));

    let extended_validation = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_dicom-validate")).args(["-", "--format", "json"]),
        &anonymized.stdout,
    );
    assert_eq!(extended_validation.status.code(), Some(1));
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
    assert_eq!(report["policy"], "dicom");
    assert_eq!(
        report["active_options"],
        serde_json::json!(DICOM_EXTENDED_OPTIONS)
    );
    assert!(report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| finding["code"] == "source_not_compared"));
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
fn version_reports_crate_version_and_source_commit() {
    for (name, binary) in [
        ("dicom-anonymize", env!("CARGO_BIN_EXE_dicom-anonymize")),
        ("dicom-validate", env!("CARGO_BIN_EXE_dicom-validate")),
    ] {
        let output = Command::new(binary).arg("--version").output().unwrap();
        assert_eq!(output.status.code(), Some(0));
        let text = String::from_utf8(output.stdout).unwrap();
        let commit = text
            .trim_end()
            .strip_prefix(&format!("{name} {} (commit ", env!("CARGO_PKG_VERSION")))
            .and_then(|rest| rest.strip_suffix(')'))
            .unwrap_or_else(|| panic!("unexpected version text: {text:?}"));
        let commit = commit.strip_suffix("-dirty").unwrap_or(commit);
        assert!(
            commit == "unknown"
                || (matches!(commit.len(), 40 | 64)
                    && commit.bytes().all(|byte| byte.is_ascii_hexdigit())),
            "unexpected commit: {commit:?}"
        );
    }
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

#[test]
fn source_aware_cli_validation_uses_the_same_configuration_and_key() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.dcm");
    let candidate = directory.path().join("candidate.dcm");
    std::fs::write(&source, source_bytes()).unwrap();

    let anonymized = Command::new(env!("CARGO_BIN_EXE_dicom-anonymize"))
        .arg(&source)
        .arg(&candidate)
        .args(["--key-env", KEY_ENV])
        .env(KEY_ENV, KEY)
        .output()
        .unwrap();
    assert_eq!(anonymized.status.code(), Some(0));

    let validated = Command::new(env!("CARGO_BIN_EXE_dicom-validate"))
        .arg(&candidate)
        .arg("--source")
        .arg(&source)
        .args(["--key-env", KEY_ENV, "--format", "json"])
        .env(KEY_ENV, KEY)
        .output()
        .unwrap();
    assert_eq!(validated.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&validated.stdout).unwrap();
    assert_eq!(report["validation_mode"], "source_comparison");
    assert!(!report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| finding["code"] == "source_not_compared"));
}

#[test]
fn safe_harbor_pipeline_uses_external_scope_uid_map_and_year_aggregation() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.dcm");
    let candidate = directory.path().join("candidate.dcm");
    let uid_map = directory.path().join("uid-map.json");
    let mut source_object = dicom_anon::read_dicom(&source_bytes()[..]).unwrap();
    source_object.put_str(tags::PATIENT_AGE, VR::AS, "097Y");
    source_object.put_str(tags::PATIENT_BIRTH_DATE, VR::DA, "19270102");
    source_object.put_str(tags::STUDY_TIME, VR::TM, "112233");
    let mut bytes = Vec::new();
    source_object.write_all(&mut bytes).unwrap();
    std::fs::write(&source, bytes).unwrap();
    std::fs::write(
        &uid_map,
        serde_json::to_vec(&serde_json::json!({
            "1.2.840.999.1.1": "2.25.101",
            "1.2.840.999.1": "2.25.102",
            "1.2.840.999.1.2": "2.25.103"
        }))
        .unwrap(),
    )
    .unwrap();

    let profile_args = [
        "--policy",
        "safe-harbor-metadata",
        "--uid-map",
        uid_map.to_str().unwrap(),
    ];
    let anonymized = Command::new(env!("CARGO_BIN_EXE_dicom-anonymize"))
        .arg(&source)
        .arg(&candidate)
        .args(profile_args)
        .args(["--patient-scope", "SUBJECT-42", "--key-env", KEY_ENV])
        .env(KEY_ENV, KEY)
        .output()
        .unwrap();
    assert_eq!(
        anonymized.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&anonymized.stderr)
    );
    let object = dicom_anon::read_dicom(std::fs::File::open(&candidate).unwrap()).unwrap();
    assert_eq!(
        object.element(tags::PATIENT_ID).unwrap().to_str().unwrap(),
        ""
    );
    assert_eq!(
        object
            .element(tags::PATIENT_NAME)
            .unwrap()
            .to_str()
            .unwrap(),
        ""
    );
    assert_eq!(
        object
            .element(tags::PATIENT_BIRTH_DATE)
            .unwrap()
            .to_str()
            .unwrap(),
        ""
    );
    assert_eq!(
        object.element(tags::PATIENT_AGE).unwrap().to_str().unwrap(),
        "090Y"
    );
    assert_eq!(
        object.element(tags::STUDY_DATE).unwrap().to_str().unwrap(),
        "20240101"
    );
    assert_eq!(
        object.element(tags::STUDY_TIME).unwrap().to_str().unwrap(),
        "000000"
    );

    let candidate_only = Command::new(env!("CARGO_BIN_EXE_dicom-validate"))
        .arg(&candidate)
        .args(profile_args)
        .args(["--format", "json"])
        .output()
        .unwrap();
    assert_eq!(
        candidate_only.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&candidate_only.stdout)
    );
    let report: serde_json::Value = serde_json::from_slice(&candidate_only.stdout).unwrap();
    assert_eq!(report["policy"], "safe-harbor-metadata");
    assert_eq!(report["validation_mode"], "candidate_only");

    let source_comparison = Command::new(env!("CARGO_BIN_EXE_dicom-validate"))
        .arg(&candidate)
        .arg("--source")
        .arg(&source)
        .args(profile_args)
        .args([
            "--patient-scope",
            "SUBJECT-42",
            "--key-env",
            KEY_ENV,
            "--format",
            "json",
        ])
        .env(KEY_ENV, KEY)
        .output()
        .unwrap();
    assert_eq!(source_comparison.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&source_comparison.stdout).unwrap();
    assert_eq!(report["validation_mode"], "source_comparison");
}

#[test]
fn unsupported_pixel_option_is_rejected_before_processing() {
    let output = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_dicom-anonymize"))
            .args([
                "-",
                "--key-env",
                KEY_ENV,
                "--enable-option",
                "clean-pixel-data",
            ])
            .env(KEY_ENV, KEY),
        &source_bytes(),
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported"));
}
