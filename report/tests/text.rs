use dicom_anon_report::{
    Category, DetectedSpan, PrivacyDetector, ReportAnonymizer, ReportOptions, Result,
    ViterbiDecoder,
};

struct FixedDetector(Vec<DetectedSpan>);

impl PrivacyDetector for FixedDetector {
    fn detect(&self, _text: &str) -> Result<Vec<DetectedSpan>> {
        Ok(self.0.clone())
    }
}

#[test]
fn anonymizes_text_without_exposing_originals_in_the_report() {
    let input = "Patient Alice Smith can be reached at alice@example.com.";
    let detector = FixedDetector(vec![
        DetectedSpan::new(Category::PrivatePerson, 8, 19, 0.98).unwrap(),
        DetectedSpan::new(Category::PrivateEmail, 38, 55, 0.99).unwrap(),
    ]);
    let anonymizer = ReportAnonymizer::new(detector, ReportOptions::default());

    let result = anonymizer.anonymize_text(input.as_bytes()).unwrap();

    assert_eq!(
        String::from_utf8(result.bytes).unwrap(),
        "Patient [PRIVATE_PERSON] can be reached at [PRIVATE_EMAIL]."
    );
    assert_eq!(result.report.detected_spans, 2);
    let report = serde_json::to_string(&result.report).unwrap();
    assert!(!report.contains("Alice"));
    assert!(!report.contains("example.com"));
    assert!(result.report.human_review_required);
}

#[test]
fn replaces_unicode_spans_using_utf8_byte_offsets() {
    let input = "Patient José Álvarez arrived.";
    let start = input.find("José").unwrap();
    let end = input.find(" arrived").unwrap();
    let detector = FixedDetector(vec![DetectedSpan::new(
        Category::PrivatePerson,
        start,
        end,
        0.99,
    )
    .unwrap()]);
    let anonymizer = ReportAnonymizer::new(detector, ReportOptions::default());

    let result = anonymizer.anonymize_text(input.as_bytes()).unwrap();

    assert_eq!(
        String::from_utf8(result.bytes).unwrap(),
        "Patient [PRIVATE_PERSON] arrived."
    );
}

#[test]
fn rejects_spans_that_do_not_follow_utf8_boundaries() {
    let input = "Renée";
    let detector = FixedDetector(vec![DetectedSpan {
        category: Category::PrivatePerson,
        start: 4,
        end: 5,
        score: 0.9,
    }]);
    let anonymizer = ReportAnonymizer::new(detector, ReportOptions::default());

    let error = anonymizer.anonymize_text(input.as_bytes()).unwrap_err();

    assert!(error.to_string().contains("UTF-8 boundary"));
}

#[test]
fn viterbi_decoder_rejects_an_invalid_background_transition_inside_a_span() {
    let mut logits = vec![vec![-100.0_f32; 33]; 3];
    logits[0][17] = 10.0; // B-private_person
    logits[0][20] = 9.0; // S-private_person
    logits[1][0] = 10.0; // O, invalid after B
    logits[1][18] = 9.0; // I-private_person
    logits[2][19] = 10.0; // E-private_person
    logits[2][0] = 9.0; // O

    let labels = ViterbiDecoder::openai_default().decode(&logits).unwrap();

    assert_eq!(labels, vec![17, 18, 19]);
}

#[test]
fn viterbi_decoder_supports_single_token_spans() {
    let mut logits = vec![vec![-100.0_f32; 33]; 3];
    logits[0][0] = 10.0;
    logits[1][16] = 10.0; // S-private_email
    logits[2][0] = 10.0;

    let labels = ViterbiDecoder::openai_default().decode(&logits).unwrap();

    assert_eq!(labels, vec![0, 16, 0]);
}
