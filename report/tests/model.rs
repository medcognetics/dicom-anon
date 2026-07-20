use std::fs;

use dicom_anon_report::{Device, ModelBundle, PrivacyDetector, PrivacyFilter};

#[test]
fn model_bundle_rejects_substituted_files_in_the_official_layout() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::create_dir(root.join("onnx")).unwrap();
    for file in ["config.json", "tokenizer.json", "viterbi_calibration.json"] {
        fs::write(root.join(file), b"{}").unwrap();
    }
    fs::write(root.join("onnx/model_q4.onnx"), b"graph").unwrap();
    fs::write(root.join("onnx/model_q4.onnx_data"), b"weights").unwrap();
    let runtime = root.join("libonnxruntime.so.1.27.1");
    fs::write(&runtime, b"runtime").unwrap();

    let error = ModelBundle::new(root, &runtime, Device::Cpu).unwrap_err();

    assert!(error.to_string().contains("checksum mismatch"));
    assert!(error.to_string().contains("config.json"));
}

#[test]
fn model_bundle_requires_external_weight_data() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::create_dir(root.join("onnx")).unwrap();
    for file in ["config.json", "tokenizer.json", "viterbi_calibration.json"] {
        fs::write(root.join(file), b"{}").unwrap();
    }
    fs::write(root.join("onnx/model_q4.onnx"), b"graph").unwrap();
    let runtime = root.join("libonnxruntime.so.1.27.1");
    fs::write(&runtime, b"runtime").unwrap();

    let error = ModelBundle::new(root, runtime, Device::Cpu).unwrap_err();

    assert!(error.to_string().contains("model_q4.onnx_data"));
}

#[test]
#[ignore = "requires the downloaded OpenAI model and ONNX Runtime"]
fn real_model_detects_a_person_with_rust_onnx_runtime() {
    let model_root = std::env::var("DICOM_ANON_REPORT_MODEL_DIR").unwrap();
    let runtime = std::env::var("DICOM_ANON_REPORT_ORT_LIBRARY").unwrap();
    let filter =
        PrivacyFilter::load(ModelBundle::new(model_root, runtime, Device::Cpu).unwrap()).unwrap();
    let input = "Patient Alice Smith has MRN 123456.";

    let spans = filter.detect(input).unwrap();

    assert!(spans.iter().any(|span| {
        span.category == dicom_anon_report::Category::PrivatePerson
            && &input[span.start..span.end] == "Alice Smith"
    }));
}
