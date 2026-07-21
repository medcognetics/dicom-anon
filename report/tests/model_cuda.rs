use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use dicom_anon_report::{
    AssetBackend, Category, DetectedSpan, Device, InstalledAssetManifest, ModelBundle,
    PrivacyDetector, PrivacyFilter, ONNX_RUNTIME_VERSION, PRIVACY_FILTER_REPOSITORY,
    PRIVACY_FILTER_REVISION,
};
use sha2::{Digest, Sha256};

const CHECKSUM_BUFFER_BYTES: usize = 64 * 1024;
const PERSON_INPUT: &str = "Patient Alice Smith has MRN 123456.";
const SEMANTIC_INPUTS: [&str; 3] = [
    PERSON_INPUT,
    "Email alice.smith@example.com or call 555-867-5309.",
    "The appointment for Alice Smith is on January 15, 2026.",
];

fn model_root() -> PathBuf {
    std::env::var("DICOM_ANON_REPORT_MODEL_DIR")
        .map(PathBuf::from)
        .expect("DICOM_ANON_REPORT_MODEL_DIR must identify the pinned CUDA bundle")
}

fn cuda_runtime() -> PathBuf {
    std::env::var("DICOM_ANON_REPORT_CUDA_ORT_LIBRARY")
        .map(PathBuf::from)
        .expect("DICOM_ANON_REPORT_CUDA_ORT_LIBRARY must identify the CUDA runtime")
}

fn checksum(path: &Path) -> String {
    let mut file = File::open(path).unwrap();
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; CHECKSUM_BUFFER_BYTES];
    loop {
        let bytes_read = file.read(&mut buffer).unwrap();
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }
    format!("{:x}", hasher.finalize())
}

fn semantic_spans(spans: Vec<DetectedSpan>) -> Vec<(Category, usize, usize)> {
    spans
        .into_iter()
        .map(|span| (span.category, span.start, span.end))
        .collect()
}

#[test]
#[ignore = "requires the checksum-verified CUDA report bundle"]
fn cached_cuda_bundle_matches_its_manifest() {
    let root = model_root();
    let manifest: InstalledAssetManifest =
        serde_json::from_reader(File::open(root.join("asset-manifest.json")).unwrap()).unwrap();

    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.model_repository, PRIVACY_FILTER_REPOSITORY);
    assert_eq!(manifest.model_revision, PRIVACY_FILTER_REVISION);
    assert_eq!(manifest.backend, AssetBackend::Cuda12);
    assert_eq!(manifest.onnx_runtime_version, ONNX_RUNTIME_VERSION);
    assert_eq!(manifest.pdfium_revision, None);
    assert!(!manifest.files.is_empty());
    assert!(
        manifest
            .files
            .iter()
            .any(|installed| installed.path.starts_with("runtime/cuda12/")),
        "manifest does not contain CUDA 12 runtime files"
    );
    for installed in manifest.files {
        assert!(
            Path::new(&installed.path)
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
            "manifest path is not relative and normalized: {}",
            installed.path
        );
        let path = root.join(&installed.path);
        assert!(
            path.is_file(),
            "manifested file is missing: {}",
            path.display()
        );
        assert_eq!(
            checksum(&path),
            installed.sha256,
            "manifested file checksum differs: {}",
            path.display()
        );
    }
}

#[test]
#[ignore = "requires the Q4 model, CUDA, cuDNN, and CUDA ONNX Runtime"]
fn cuda_matches_cpu_semantics_and_detects_a_person() {
    let model_root = model_root();
    let runtime = cuda_runtime();
    let cpu_results = {
        let filter =
            PrivacyFilter::load(ModelBundle::new(&model_root, &runtime, Device::Cpu).unwrap())
                .unwrap();
        SEMANTIC_INPUTS
            .iter()
            .map(|input| semantic_spans(filter.detect(input).unwrap()))
            .collect::<Vec<_>>()
    };
    let filter = PrivacyFilter::load(
        ModelBundle::new(&model_root, &runtime, Device::Cuda { index: 0 }).unwrap(),
    )
    .unwrap();
    let cuda_results = SEMANTIC_INPUTS
        .iter()
        .map(|input| semantic_spans(filter.detect(input).unwrap()))
        .collect::<Vec<_>>();

    assert_eq!(cuda_results, cpu_results);
    assert!(
        cuda_results[0].iter().any(|(category, start, end)| {
            *category == Category::PrivatePerson && &PERSON_INPUT[*start..*end] == "Alice Smith"
        }),
        "{:?}",
        cuda_results[0]
    );
}

#[test]
#[ignore = "requires the Q4 model, CUDA, cuDNN, and CUDA ONNX Runtime"]
fn report_cli_filters_plaintext_on_cuda() {
    let model_root = model_root();
    let runtime = cuda_runtime();
    let mut child = Command::new(env!("CARGO_BIN_EXE_report-anon"))
        .args([
            "anonymize",
            "--model-dir",
            model_root.to_str().unwrap(),
            "--runtime-library",
            runtime.to_str().unwrap(),
            "--device",
            "cuda",
            "--progress",
            "never",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(PERSON_INPUT.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();

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
