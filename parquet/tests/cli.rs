use std::fs;
use std::path::Path;
use std::process::Command;

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_dicom-to-parquet"))
}

fn copy_fixture(destination: &Path) {
    let source = dicom_test_files::path("pydicom/SC_rgb.dcm").expect("fixture should be available");
    fs::copy(source, destination).expect("fixture copy should succeed");
}

#[test]
fn json_success_is_machine_readable_and_uses_stdout_only() {
    let input = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    copy_fixture(&input.path().join("image.dcm"));

    let result = binary()
        .arg(input.path())
        .arg(output.path().join("result.parquet"))
        .args([
            "--hash",
            "--snake-case",
            "--format",
            "json",
            "--progress",
            "never",
        ])
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(0));
    assert!(result.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["status"], "ok");
    assert_eq!(report["discovered_files"], 1);
    assert_eq!(report["processed_files"], 1);
    assert_eq!(report["failed_files"], 0);
    assert_eq!(report["shard_paths"].as_array().unwrap().len(), 1);
    assert!(report["elapsed_ms"].is_number());
}

#[test]
fn best_effort_uses_exit_one_and_strict_uses_exit_two() {
    let input = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    copy_fixture(&input.path().join("valid.dcm"));
    fs::write(input.path().join("invalid.dcm"), b"not DICOM").unwrap();

    let best_effort = binary()
        .arg(input.path())
        .arg(output.path().join("best.parquet"))
        .args(["--sort", "path", "--format", "json", "--progress", "never"])
        .output()
        .unwrap();
    assert_eq!(best_effort.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&best_effort.stdout).unwrap();
    assert_eq!(report["status"], "warning");
    assert_eq!(report["processed_files"], 1);
    assert_eq!(report["failed_files"], 1);
    assert!(String::from_utf8_lossy(&best_effort.stderr).contains("warning:"));

    let strict = binary()
        .arg(input.path())
        .arg(output.path().join("strict.parquet"))
        .args(["--strict", "--sort", "path", "--progress", "never"])
        .output()
        .unwrap();
    assert_eq!(strict.status.code(), Some(2));
    assert!(strict.stdout.is_empty());
    assert!(String::from_utf8_lossy(&strict.stderr).contains("dicom-to-parquet failed:"));
    assert!(!output.path().join("strict.00000.parquet").exists());
}

#[test]
fn no_color_wins_and_conflicting_verbosity_is_usage_error() {
    let input = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    copy_fixture(&input.path().join("image.dcm"));

    let no_color = binary()
        .arg(input.path())
        .arg(output.path().join("plain.parquet"))
        .args(["--color", "always", "--no-color", "--progress", "never"])
        .output()
        .unwrap();
    assert_eq!(no_color.status.code(), Some(0));
    assert!(!no_color.stdout.contains(&0x1b));
    assert!(!no_color.stderr.contains(&0x1b));

    let conflict = binary()
        .arg(input.path())
        .arg(output.path().join("unused.parquet"))
        .args(["--quiet", "--verbose"])
        .output()
        .unwrap();
    assert_eq!(conflict.status.code(), Some(2));
}

#[test]
fn invalid_override_is_a_runtime_error_before_output() {
    let input = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    copy_fixture(&input.path().join("image.dcm"));

    let result = binary()
        .arg(input.path())
        .arg(output.path().join("invalid.parquet"))
        .args(["--tag", "DefinitelyNotADicomTag=value"])
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    assert!(!output.path().join("invalid.00000.parquet").exists());
}

#[test]
fn verbose_color_and_quiet_failure_modes_follow_the_contract() {
    let input = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    copy_fixture(&input.path().join("image.dcm"));

    let verbose = binary()
        .arg(input.path())
        .arg(output.path().join("verbose.parquet"))
        .args([
            "--hash",
            "--verbose",
            "--color",
            "always",
            "--progress",
            "always",
        ])
        .output()
        .unwrap();
    assert_eq!(verbose.status.code(), Some(0));
    assert!(verbose.stdout.contains(&0x1b));
    let text = String::from_utf8_lossy(&verbose.stdout);
    assert!(text.contains("Configuration"));
    assert!(text.contains("Pixel bytes read:"));

    let invalid = tempfile::tempdir().unwrap();
    fs::write(invalid.path().join("invalid.dcm"), b"not DICOM").unwrap();
    let quiet = binary()
        .arg(invalid.path())
        .arg(output.path().join("quiet.parquet"))
        .args(["--quiet", "--progress", "never"])
        .output()
        .unwrap();
    assert_eq!(quiet.status.code(), Some(2));
    assert!(quiet.stdout.is_empty());
    assert!(String::from_utf8_lossy(&quiet.stderr).contains("no usable DICOM files"));
}

#[test]
fn failure_diagnostics_are_bounded() {
    let input = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    for index in 0..22 {
        fs::write(input.path().join(format!("{index:02}.dcm")), b"not DICOM").unwrap();
    }
    copy_fixture(&input.path().join("valid.dcm"));

    let result = binary()
        .arg(input.path())
        .arg(output.path().join("failures.parquet"))
        .args(["--sort", "path", "--progress", "never"])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("2 additional failures omitted"));
}
