use dicom_anon_report::{Device, ModelBundle, PrivacyDetector, PrivacyFilter};

#[test]
#[ignore = "requires the Q4 model, CUDA, cuDNN, and CUDA ONNX Runtime"]
fn real_model_detects_a_person_on_cuda() {
    let model_root = std::env::var("DICOM_ANON_REPORT_MODEL_DIR").unwrap();
    let runtime = std::env::var("DICOM_ANON_REPORT_CUDA_ORT_LIBRARY").unwrap();
    let filter = PrivacyFilter::load(
        ModelBundle::new(model_root, runtime, Device::Cuda { index: 0 }).unwrap(),
    )
    .unwrap();

    let input = "Patient Alice Smith has MRN 123456.";
    let spans = filter.detect(input).unwrap();

    assert!(
        spans.iter().any(|span| {
            span.category == dicom_anon_report::Category::PrivatePerson
                && &input[span.start..span.end] == "Alice Smith"
        }),
        "{spans:?}"
    );
}
