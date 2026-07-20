use std::io::Write;
use std::process::{Command, Output, Stdio};

use dicom_core::VR;
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};
#[cfg(feature = "pdf")]
use pdfium_render::prelude::*;

#[cfg(feature = "pdf")]
const PDF_POINTS_PER_INCH: f32 = 72.0;

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
fn help_documents_pipe_and_exit_contracts() {
    let output = Command::new(env!("CARGO_BIN_EXE_report-anon"))
        .args(["anonymize", "--help"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("stdin"));
    assert!(help.contains("stdout"));
    assert!(help.contains("Exit status: 0 success, 2 usage or runtime error"));
    assert!(help.contains("--device"));
    assert!(help.contains("--progress"));
    assert!(help.contains("--color"));
    assert!(help.contains("--dicom"));
    assert!(help.contains("--association"));
    assert!(help.contains("--input-format"));
    assert!(help.contains("--max-input-tokens"));
    #[cfg(feature = "pdf")]
    {
        assert!(help.contains("--pdfium-library"));
        assert!(help.contains("--pdf-text-mode"));
        assert!(help.contains("--max-total-render-pixels"));
    }
    #[cfg(not(feature = "pdf"))]
    {
        assert!(!help.contains("--pdfium-library"));
        assert!(!help.contains("--pdf-text-mode"));
        assert!(!help.contains("--max-total-render-pixels"));
    }
}

#[test]
fn dicom_profile_options_require_a_dicom_input() {
    let output = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_report-anon")).args([
            "anonymize",
            "--policy",
            "safe-harbor-metadata",
            "--model-dir",
            "/not/read/before/usage-validation",
        ]),
        b"Patient report",
    );

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--policy requires at least one --dicom")
    );

    let conflicting_options = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_report-anon")).args([
            "anonymize",
            "--enable-option",
            "clean-graphics",
            "--disable-option",
            "clean-graphics",
            "--model-dir",
            "/not/read/before/usage-validation",
        ]),
        b"Patient report",
    );
    assert_eq!(conflicting_options.status.code(), Some(2));
    assert!(conflicting_options.stdout.is_empty());
    assert!(String::from_utf8_lossy(&conflicting_options.stderr)
        .contains("--enable-option requires at least one --dicom"));
}

#[test]
fn piped_linkage_requires_an_explicit_association_destination() {
    let output = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_report-anon")).args([
            "anonymize",
            "--dicom",
            "/not/read/before/usage-validation.dcm",
        ]),
        b"Patient report",
    );

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("--association is required when linked output is written to stdout"));
}

#[test]
fn model_path_reports_the_pinned_default_bundle() {
    let output = Command::new(env!("CARGO_BIN_EXE_report-anon"))
        .args(["model", "path"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .trim_end()
        .ends_with("7ffa9a043d54d1be65afb281eddf0ffbe629385b"));
}

#[test]
fn model_installer_refuses_an_existing_target_before_network_access() {
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_report-anon"))
        .args([
            "model",
            "install",
            "--target",
            directory.path().to_str().unwrap(),
            "--progress",
            "never",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("target already exists"));
}

#[test]
fn model_failure_emits_no_partial_plaintext() {
    let output = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_report-anon")).args([
            "anonymize",
            "--model-dir",
            "/definitely/missing/report-model",
            "--device",
            "cpu",
        ]),
        b"Patient Alice Smith",
    );

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("report-anon failed"));
}

#[test]
fn report_sidecar_cannot_overwrite_the_plaintext_input() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("report.txt");
    let output = directory.path().join("sanitized.txt");
    std::fs::write(&input, b"Patient Alice Smith").unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_report-anon"))
        .args([
            "anonymize",
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "--report",
            input.to_str().unwrap(),
            "--model-dir",
            "/not/read/before/path-validation",
        ])
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr)
        .contains("--report must not overwrite an input file"));
    assert_eq!(std::fs::read(input).unwrap(), b"Patient Alice Smith");
}

#[test]
fn positional_output_cannot_overwrite_the_input_without_in_place() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("report.txt");
    std::fs::write(&input, b"Patient Alice Smith").unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_report-anon"))
        .args([
            "anonymize",
            input.to_str().unwrap(),
            input.to_str().unwrap(),
            "--model-dir",
            "/not/read/before/path-validation",
        ])
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr)
        .contains("use --in-place to replace the input file"));
    assert_eq!(std::fs::read(input).unwrap(), b"Patient Alice Smith");
}

#[cfg(unix)]
#[test]
fn in_place_rejects_a_symlinked_input_instead_of_replacing_the_link() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("report.txt");
    let input_link = directory.path().join("report-link.txt");
    std::fs::write(&target, b"Patient Alice Smith").unwrap();
    symlink(&target, &input_link).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_report-anon"))
        .args([
            "anonymize",
            input_link.to_str().unwrap(),
            "--in-place",
            "--model-dir",
            "/not/read/before/path-validation",
        ])
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr)
        .contains("--in-place does not accept a symbolic-link input"));
    assert!(input_link.is_symlink());
    assert_eq!(std::fs::read(target).unwrap(), b"Patient Alice Smith");
}

#[test]
fn report_sidecar_cannot_overwrite_an_automatically_resolved_model_asset() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("report.txt");
    let output = directory.path().join("sanitized.txt");
    let model_directory = directory.path().join("model");
    let model_configuration = model_directory.join("config.json");
    std::fs::create_dir(&model_directory).unwrap();
    std::fs::write(&input, b"Patient Alice Smith").unwrap();
    std::fs::write(&model_configuration, b"model configuration sentinel").unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_report-anon"))
        .args([
            "anonymize",
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "--model-dir",
            model_directory.to_str().unwrap(),
            "--report",
            model_configuration.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr)
        .contains("--report must not overwrite a model asset"));
    assert_eq!(
        std::fs::read(model_configuration).unwrap(),
        b"model configuration sentinel"
    );
}

#[test]
fn report_sidecar_cannot_overwrite_a_manifested_bundle_file() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("report.txt");
    let output = directory.path().join("sanitized.txt");
    let model_directory = directory.path().join("model");
    let runtime_license = model_directory.join("runtime/cpu/LICENSE");
    std::fs::create_dir_all(runtime_license.parent().unwrap()).unwrap();
    std::fs::write(&input, b"Patient Alice Smith").unwrap();
    std::fs::write(&runtime_license, b"runtime license sentinel").unwrap();
    std::fs::write(
        model_directory.join("asset-manifest.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "model_repository": "openai/privacy-filter",
            "model_revision": "7ffa9a043d54d1be65afb281eddf0ffbe629385b",
            "backend": "cpu",
            "onnx_runtime_version": "1.27.1",
            "files": [{
                "path": "runtime/cpu/LICENSE",
                "sha256": "unused-for-path-validation"
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_report-anon"))
        .args([
            "anonymize",
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "--model-dir",
            model_directory.to_str().unwrap(),
            "--report",
            runtime_license.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr)
        .contains("--report must not overwrite a model asset"));
    assert_eq!(
        std::fs::read(runtime_license).unwrap(),
        b"runtime license sentinel"
    );
}

#[test]
fn association_sidecar_cannot_overwrite_a_dicom_input() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("report.txt");
    let output = directory.path().join("sanitized.txt");
    let dicom = directory.path().join("source.dcm");
    let key = directory.path().join("key");
    std::fs::write(&input, b"Patient Alice Smith").unwrap();
    std::fs::write(&dicom, b"source DICOM sentinel").unwrap();
    std::fs::write(&key, b"0123456789abcdef0123456789abcdef").unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_report-anon"))
        .args([
            "anonymize",
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "--dicom",
            dicom.to_str().unwrap(),
            "--key-file",
            key.to_str().unwrap(),
            "--association",
            dicom.to_str().unwrap(),
            "--model-dir",
            "/not/read/before/path-validation",
        ])
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr)
        .contains("--association must not overwrite an input file"));
    assert_eq!(std::fs::read(dicom).unwrap(), b"source DICOM sentinel");
}

#[test]
fn usage_conflicts_return_two_without_stdout() {
    let output = Command::new(env!("CARGO_BIN_EXE_report-anon"))
        .args(["anonymize", "input.txt", "output.txt", "--in-place"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
}

#[test]
#[ignore = "requires the downloaded OpenAI model and ONNX Runtime"]
fn real_model_filters_a_stdin_stdout_pipeline() {
    let model_dir = std::env::var("DICOM_ANON_REPORT_MODEL_DIR").unwrap();
    let runtime = std::env::var("DICOM_ANON_REPORT_ORT_LIBRARY").unwrap();
    let output = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_report-anon")).args([
            "anonymize",
            "--model-dir",
            &model_dir,
            "--runtime-library",
            &runtime,
            "--device",
            "cpu",
        ]),
        b"Patient Alice Smith has MRN 123456.",
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("Alice Smith"));
    assert!(text.contains("[PRIVATE_PERSON]"));
}

#[test]
#[ignore = "requires the downloaded OpenAI model and ONNX Runtime"]
fn real_model_pipe_reassociates_report_with_dicom_outputs() {
    let directory = tempfile::tempdir().unwrap();
    let dicom_path = directory.path().join("source.dcm");
    let key_path = directory.path().join("key");
    let association_path = directory.path().join("association.json");
    std::fs::write(&key_path, b"0123456789abcdef0123456789abcdef").unwrap();
    write_source_dicom(&dicom_path);

    let model_dir = std::env::var("DICOM_ANON_REPORT_MODEL_DIR").unwrap();
    let runtime = std::env::var("DICOM_ANON_REPORT_ORT_LIBRARY").unwrap();
    let output = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_report-anon")).args([
            "anonymize",
            "--model-dir",
            &model_dir,
            "--runtime-library",
            &runtime,
            "--device",
            "cpu",
            "--dicom",
            dicom_path.to_str().unwrap(),
            "--key-file",
            key_path.to_str().unwrap(),
            "--association",
            association_path.to_str().unwrap(),
        ]),
        b"Patient Private Person has ID PATIENT-001 for study 1.2.840.999.1.",
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(association_path).unwrap()).unwrap();
    assert!(text.contains(manifest["patient_output_id"].as_str().unwrap()));
    assert!(!text.contains("PATIENT-001"));
    assert!(!text.contains("1.2.840.999.1"));
    assert_eq!(manifest["output_sha256"].as_str().unwrap().len(), 64);
}

#[test]
#[cfg(feature = "pdf")]
#[ignore = "requires the downloaded OpenAI model, ONNX Runtime, and PDFium"]
fn real_model_filters_pdf_to_image_only_stdout() {
    let directory = tempfile::tempdir().unwrap();
    let report_path = directory.path().join("report.json");
    let model_dir = std::env::var("DICOM_ANON_REPORT_MODEL_DIR").unwrap();
    let runtime = std::env::var("DICOM_ANON_REPORT_ORT_LIBRARY").unwrap();
    let pdfium = std::env::var("DICOM_ANON_REPORT_PDFIUM_LIBRARY").unwrap();
    let input = minimal_text_pdf("Patient Alice Smith has a normal exam.");
    let pdfium_runtime = Pdfium::new(Pdfium::bind_to_library(&pdfium).unwrap());
    let dpi = 200;
    let expected_redactions = source_text_rectangles(&pdfium_runtime, &input, "Alice Smith", dpi);
    let output = run_with_stdin(
        Command::new(env!("CARGO_BIN_EXE_report-anon")).args([
            "anonymize",
            "--model-dir",
            &model_dir,
            "--runtime-library",
            &runtime,
            "--pdfium-library",
            &pdfium,
            "--pdf-text-mode",
            "embedded",
            "--device",
            "cpu",
            "--report",
            report_path.to_str().unwrap(),
        ]),
        &input,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.starts_with(b"%PDF-"));
    let rendered = render_first_page(&pdfium_runtime, &output.stdout, dpi).to_rgba8();
    assert!(!expected_redactions.is_empty());
    for bounds in expected_redactions {
        let x = ((bounds.left + bounds.right) / 2).clamp(0, rendered.width() as i32 - 1);
        let y = ((bounds.top + bounds.bottom) / 2).clamp(0, rendered.height() as i32 - 1);
        let pixel = rendered.get_pixel(x as u32, y as u32);
        assert!(
            pixel.0[..3].iter().all(|channel| *channel <= 8),
            "expected dark redaction pixel at ({x}, {y}), got {pixel:?}"
        );
    }
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(report_path).unwrap()).unwrap();
    assert_eq!(report["image_only_output"], true);
    assert_eq!(report["privacy"]["detected_spans"], 1);
}

#[cfg(feature = "pdf")]
#[derive(Clone, Copy)]
struct PixelRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[cfg(feature = "pdf")]
fn source_text_rectangles(
    pdfium: &Pdfium,
    input: &[u8],
    protected_text: &str,
    dpi: u16,
) -> Vec<PixelRect> {
    let document = pdfium.load_pdf_from_byte_slice(input, None).unwrap();
    let page = document.pages().get(0).unwrap();
    let (pixel_width, pixel_height) = page_pixel_dimensions(&page, dpi);
    let scale_x = pixel_width as f32 / page.width().value;
    let scale_y = pixel_height as f32 / page.height().value;
    let page_height = page.height().value;
    let text = page.text().unwrap();
    let mut content = String::new();
    let mut ranges = Vec::new();
    for character in text.chars().iter() {
        let Some(value) = character.unicode_char().filter(|value| *value != '\0') else {
            continue;
        };
        let start = content.len();
        content.push(value);
        let end = content.len();
        if value.is_whitespace() {
            continue;
        }
        if let Ok(bounds) = character.tight_bounds() {
            ranges.push((
                start,
                end,
                PixelRect {
                    left: (bounds.left().value * scale_x).floor() as i32,
                    top: ((page_height - bounds.top().value) * scale_y).floor() as i32,
                    right: (bounds.right().value * scale_x).ceil() as i32,
                    bottom: ((page_height - bounds.bottom().value) * scale_y).ceil() as i32,
                },
            ));
        }
    }
    let start = content.find(protected_text).unwrap();
    let end = start + protected_text.len();
    ranges
        .into_iter()
        .filter(|(range_start, range_end, _)| *range_start < end && *range_end > start)
        .map(|(_, _, bounds)| bounds)
        .collect()
}

#[cfg(feature = "pdf")]
fn render_first_page(pdfium: &Pdfium, input: &[u8], dpi: u16) -> image::DynamicImage {
    let document = pdfium.load_pdf_from_byte_slice(input, None).unwrap();
    let page = document.pages().get(0).unwrap();
    let (width, height) = page_pixel_dimensions(&page, dpi);
    let image = page
        .render_with_config(&PdfRenderConfig::new().set_fixed_size(width, height))
        .unwrap()
        .as_image()
        .unwrap();
    image
}

#[cfg(feature = "pdf")]
fn page_pixel_dimensions(page: &PdfPage<'_>, dpi: u16) -> (i32, i32) {
    let scale = f32::from(dpi) / PDF_POINTS_PER_INCH;
    (
        (page.width().value * scale).ceil() as i32,
        (page.height().value * scale).ceil() as i32,
    )
}

fn write_source_dicom(path: &std::path::Path) {
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
    let object = object
        .with_meta(FileMetaTableBuilder::new().transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN))
        .unwrap();
    object.write_to_file(path).unwrap();
}

#[cfg(feature = "pdf")]
fn minimal_text_pdf(text: &str) -> Vec<u8> {
    let escaped = text
        .replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)");
    let stream = format!("BT /F1 18 Tf 72 720 Td ({escaped}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    let mut pdf = b"% leading transport comment\n%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}
