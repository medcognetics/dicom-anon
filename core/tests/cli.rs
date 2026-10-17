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

const SENTINEL_SOURCE_UID: &str = "2.25.271828182845904523536028747135266249";

fn nested_uid_source() -> Vec<u8> {
    let mut object = dicom_anon::read_dicom(&source_bytes()[..]).unwrap();
    let mut reference = InMemDicomObject::new_empty();
    reference.put_str(
        tags::REFERENCED_SOP_CLASS_UID,
        VR::UI,
        uids::SECONDARY_CAPTURE_IMAGE_STORAGE,
    );
    reference.put_str(
        tags::REFERENCED_SOP_INSTANCE_UID,
        VR::UI,
        SENTINEL_SOURCE_UID,
    );
    object.put(dicom_object::mem::InMemElement::new(
        tags::SOURCE_IMAGE_SEQUENCE,
        VR::SQ,
        dicom_core::value::DataSetSequence::from(vec![reference]),
    ));
    let mut bytes = Vec::new();
    object.write_all(&mut bytes).unwrap();
    bytes
}

fn safe_harbor_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dicom-anonymize"));
    command
        .args(["--policy", "safe-harbor-metadata"])
        .args(["--patient-scope", "SUBJ-0001", "--key-env", KEY_ENV])
        .env(KEY_ENV, KEY);
    command
}

#[test]
fn list_required_uids_prints_paths_as_json_without_dicom_output() {
    let output = run_with_stdin(
        safe_harbor_command().args(["-", "--list-required-uids"]),
        &nested_uid_source(),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        listed,
        serde_json::json!({"required_uids": [
            {"path": "(0008,0018)", "uid": "1.2.840.999.1.1"},
            {"path": "(0008,2112)[0]/(0008,1155)", "uid": SENTINEL_SOURCE_UID},
            {"path": "(0020,000D)", "uid": "1.2.840.999.1"},
            {"path": "(0020,000E)", "uid": "1.2.840.999.1.2"},
        ]})
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn missing_uid_mapping_error_names_the_path_and_not_the_value() {
    let directory = tempfile::tempdir().unwrap();
    let uid_map = directory.path().join("uid-map.json");
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
    let output = run_with_stdin(
        safe_harbor_command()
            .arg("-")
            .arg("--uid-map")
            .arg(&uid_map),
        &nested_uid_source(),
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("(0008,2112)[0]/(0008,1155)"), "{stderr}");
    for value in [SENTINEL_SOURCE_UID, "2.25.101", "1.2.840.999.1"] {
        assert!(!stderr.contains(value), "{stderr}");
    }
}

#[test]
fn generate_uid_map_writes_a_random_complete_map_before_the_output() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.dcm");
    std::fs::write(&source, nested_uid_source()).unwrap();
    let run = |output: &str, map: &str, existing: bool| {
        let map_path = directory.path().join(map);
        let mut command = safe_harbor_command();
        command
            .arg(&source)
            .arg(directory.path().join(output))
            .arg("--generate-uid-map")
            .arg(&map_path)
            .arg("--verbose");
        if existing {
            command.arg("--uid-map").arg(&map_path);
        }
        let result = command.output().unwrap();
        assert_eq!(
            result.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
        let map: std::collections::BTreeMap<String, String> =
            serde_json::from_slice(&std::fs::read(&map_path).unwrap()).unwrap();
        let output = std::fs::read(directory.path().join(output)).unwrap();
        (map, String::from_utf8_lossy(&output).into_owned(), stderr)
    };

    let (first, first_output, first_report) = run("first.dcm", "first.json", false);
    let (second, _, _) = run("second.dcm", "second.json", false);
    let sources = [
        "1.2.840.999.1",
        "1.2.840.999.1.1",
        "1.2.840.999.1.2",
        SENTINEL_SOURCE_UID,
    ];
    assert_eq!(
        first.keys().map(String::as_str).collect::<Vec<_>>(),
        sources
    );
    for source_uid in sources {
        assert_ne!(first[source_uid], second[source_uid]);
        assert!(first[source_uid].starts_with("2.25."));
        assert!(!first_output.contains(source_uid));
        assert!(!first_report.contains(source_uid));
    }
    for replacement in first.values() {
        assert!(!first_report.contains(replacement.as_str()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(directory.path().join("first.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }

    let (reused, reused_output, _) = run("first-again.dcm", "first.json", true);
    assert_eq!(reused, first);
    assert_eq!(reused_output, first_output);

    let alias = safe_harbor_command()
        .arg(&source)
        .arg(directory.path().join("third.dcm"))
        .arg("--generate-uid-map")
        .arg(&source)
        .output()
        .unwrap();
    assert_eq!(alias.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&alias.stderr).contains("--generate-uid-map"));

    let keyed = Command::new(env!("CARGO_BIN_EXE_dicom-anonymize"))
        .arg(&source)
        .arg(directory.path().join("keyed.dcm"))
        .arg("--generate-uid-map")
        .arg(directory.path().join("keyed.json"))
        .args(["--key-env", KEY_ENV])
        .env(KEY_ENV, KEY)
        .output()
        .unwrap();
    assert_eq!(keyed.status.code(), Some(2));
    assert!(!directory.path().join("keyed.json").exists());
}

#[test]
fn generate_uid_map_rejects_aliases_of_outputs_and_configuration_inputs() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.dcm");
    std::fs::write(&source, nested_uid_source()).unwrap();
    let key_file = directory.path().join("key");
    std::fs::write(&key_file, KEY).unwrap();
    let real = directory.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let alias = directory.path().join("alias");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    #[cfg(not(unix))]
    std::fs::create_dir(&alias).unwrap();

    let key_alias = Command::new(env!("CARGO_BIN_EXE_dicom-anonymize"))
        .arg(&source)
        .arg(directory.path().join("output.dcm"))
        .args([
            "--policy",
            "safe-harbor-metadata",
            "--patient-scope",
            "SUBJ-0001",
        ])
        .arg("--key-file")
        .arg(&key_file)
        .arg("--generate-uid-map")
        .arg(&key_file)
        .output()
        .unwrap();
    assert_eq!(key_alias.status.code(), Some(2));
    assert_eq!(std::fs::read_to_string(&key_file).unwrap(), KEY);

    #[cfg(unix)]
    {
        let output_alias = safe_harbor_command()
            .arg(&source)
            .arg(real.join("new.dcm"))
            .arg("--generate-uid-map")
            .arg(alias.join("new.dcm"))
            .output()
            .unwrap();
        assert_eq!(output_alias.status.code(), Some(2));
        assert!(!real.join("new.dcm").exists());
    }
}

#[test]
fn generate_uid_map_does_not_replace_a_map_that_was_not_loaded() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.dcm");
    std::fs::write(&source, nested_uid_source()).unwrap();
    let study_map = directory.path().join("study.json");
    let existing = "{\"2.25.1001\": \"2.25.1002\"}\n";
    std::fs::write(&study_map, existing).unwrap();
    let output = safe_harbor_command()
        .arg(&source)
        .arg(directory.path().join("output.dcm"))
        .arg("--generate-uid-map")
        .arg(&study_map)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--uid-map"));
    assert_eq!(std::fs::read_to_string(&study_map).unwrap(), existing);
    assert!(!directory.path().join("output.dcm").exists());
}

#[cfg(unix)]
#[test]
fn generate_uid_map_updates_the_target_of_a_symbolically_linked_map() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.dcm");
    std::fs::write(&source, nested_uid_source()).unwrap();
    let target = directory.path().join("study.json");
    std::fs::write(&target, "{}").unwrap();
    let link = directory.path().join("current.json");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let output = safe_harbor_command()
        .arg(&source)
        .arg(directory.path().join("output.dcm"))
        .arg("--uid-map")
        .arg(&link)
        .arg("--generate-uid-map")
        .arg(&link)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    let map: std::collections::BTreeMap<String, String> =
        serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
    assert_eq!(map.len(), 4);

    let dangling = directory.path().join("dangling.json");
    std::os::unix::fs::symlink(directory.path().join("missing.json"), &dangling).unwrap();
    let rejected = safe_harbor_command()
        .arg(&source)
        .arg(directory.path().join("second.dcm"))
        .arg("--generate-uid-map")
        .arg(&dangling)
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(2));
    assert!(!directory.path().join("missing.json").exists());
}
