//! Deterministic plaintext redaction.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::linkage::SensitiveLinkageMap;
use crate::{Error, Result};

const DEFAULT_MAX_TEXT_BYTES: usize = 64 * 1024 * 1024;
const REPORT_SCHEMA_VERSION: u32 = 1;

/// Privacy span category emitted by OpenAI Privacy Filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Account, record, or identifier number.
    AccountNumber,
    /// Private street or mailing address.
    PrivateAddress,
    /// Private date.
    PrivateDate,
    /// Private email address.
    PrivateEmail,
    /// Private person name.
    PrivatePerson,
    /// Private telephone number.
    PrivatePhone,
    /// Private URL.
    PrivateUrl,
    /// Secret or credential.
    Secret,
}

impl Category {
    /// Stable replacement emitted for this category.
    #[must_use]
    pub const fn placeholder(self) -> &'static str {
        match self {
            Self::AccountNumber => "[ACCOUNT_NUMBER]",
            Self::PrivateAddress => "[PRIVATE_ADDRESS]",
            Self::PrivateDate => "[PRIVATE_DATE]",
            Self::PrivateEmail => "[PRIVATE_EMAIL]",
            Self::PrivatePerson => "[PRIVATE_PERSON]",
            Self::PrivatePhone => "[PRIVATE_PHONE]",
            Self::PrivateUrl => "[PRIVATE_URL]",
            Self::Secret => "[SECRET]",
        }
    }
}

/// One detected privacy span using UTF-8 byte offsets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectedSpan {
    /// Privacy category.
    pub category: Category,
    /// Inclusive UTF-8 byte offset.
    pub start: usize,
    /// Exclusive UTF-8 byte offset.
    pub end: usize,
    /// Detector confidence in the inclusive range `[0, 1]`.
    pub score: f32,
}

impl DetectedSpan {
    /// Construct and validate a detected span.
    pub fn new(category: Category, start: usize, end: usize, score: f32) -> Result<Self> {
        if start >= end {
            return Err(Error::InvalidSpan(
                "the start offset must be less than the end offset".to_owned(),
            ));
        }
        if !score.is_finite() || !(0.0..=1.0).contains(&score) {
            return Err(Error::InvalidSpan(
                "the confidence score must be finite and between zero and one".to_owned(),
            ));
        }
        Ok(Self {
            category,
            start,
            end,
            score,
        })
    }
}

/// Pluggable local privacy detector.
pub trait PrivacyDetector {
    /// Detect privacy spans without modifying the source text.
    fn detect(&self, text: &str) -> Result<Vec<DetectedSpan>>;

    /// Safe model metadata for audit reports.
    fn metadata(&self) -> Option<DetectorMetadata> {
        None
    }
}

/// Non-sensitive detector identity recorded in reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectorMetadata {
    /// Detector implementation or upstream repository.
    pub name: String,
    /// Immutable model revision.
    pub revision: String,
    /// Selected weight variant.
    pub variant: String,
    /// Selected execution device.
    pub device: String,
}

/// Plaintext anonymization limits and behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportOptions {
    /// Maximum accepted UTF-8 plaintext size.
    pub max_text_bytes: usize,
}

impl Default for ReportOptions {
    fn default() -> Self {
        Self {
            max_text_bytes: DEFAULT_MAX_TEXT_BYTES,
        }
    }
}

/// Safe summary of one report-anonymization pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportAnonymizationReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Number of spans replaced in the output.
    pub detected_spans: u64,
    /// Number of spans replaced using DICOM-derived associations.
    pub linked_spans: u64,
    /// Replaced-span counts grouped by category.
    pub category_counts: BTreeMap<Category, u64>,
    /// Model identity when supplied by the detector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<DetectorMetadata>,
    /// Medical report output always requires human review in v1.
    pub human_review_required: bool,
}

/// Anonymized bytes and their safe report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnonymizedDocument {
    /// Anonymized UTF-8 bytes.
    pub bytes: Vec<u8>,
    /// Safe transformation summary.
    pub report: ReportAnonymizationReport,
}

/// Reusable deterministic report anonymizer.
pub struct ReportAnonymizer<D> {
    detector: D,
    options: ReportOptions,
}

impl<D> ReportAnonymizer<D>
where
    D: PrivacyDetector,
{
    /// Construct a report anonymizer around a local detector.
    #[must_use]
    pub fn new(detector: D, options: ReportOptions) -> Self {
        Self { detector, options }
    }

    /// Anonymize one complete UTF-8 document atomically in memory.
    pub fn anonymize_text(&self, input: &[u8]) -> Result<AnonymizedDocument> {
        self.anonymize_text_internal(input, None)
    }

    /// Anonymize plaintext while replacing matching DICOM identifiers with the
    /// exact pseudonyms present in the corresponding anonymized DICOM objects.
    pub fn anonymize_text_with_linkage(
        &self,
        input: &[u8],
        linkage: &SensitiveLinkageMap,
    ) -> Result<AnonymizedDocument> {
        self.anonymize_text_internal(input, Some(linkage))
    }

    fn anonymize_text_internal(
        &self,
        input: &[u8],
        linkage: Option<&SensitiveLinkageMap>,
    ) -> Result<AnonymizedDocument> {
        if input.len() > self.options.max_text_bytes {
            return Err(Error::InputTooLarge {
                limit: self.options.max_text_bytes,
            });
        }
        let text = std::str::from_utf8(input).map_err(|_| Error::InvalidUtf8)?;
        let plan = self.plan_text(text, linkage)?;
        let mut replacements = Vec::with_capacity(plan.detected.len() + plan.linked.len());
        replacements.extend(plan.detected.iter().map(|span| Replacement {
            start: span.start,
            end: span.end,
            value: span.category.placeholder(),
        }));
        replacements.extend(plan.linked.iter().map(|span| Replacement {
            start: span.start,
            end: span.end,
            value: span.replacement.as_str(),
        }));
        replacements.sort_by_key(|span| span.start);

        let mut output = Vec::with_capacity(input.len());
        let mut cursor = 0;
        for span in &replacements {
            output.extend_from_slice(&input[cursor..span.start]);
            output.extend_from_slice(span.value.as_bytes());
            cursor = span.end;
        }
        output.extend_from_slice(&input[cursor..]);

        Ok(AnonymizedDocument {
            bytes: output,
            report: self.report_for_plan(&plan),
        })
    }

    pub(crate) fn plan_text(
        &self,
        text: &str,
        linkage: Option<&SensitiveLinkageMap>,
    ) -> Result<RedactionPlan> {
        if text.len() > self.options.max_text_bytes {
            return Err(Error::InputTooLarge {
                limit: self.options.max_text_bytes,
            });
        }
        let detected = self.detector.detect(text)?;
        let detected = validated_non_overlapping_spans(text, detected)?;
        let linked = linkage.map_or_else(Vec::new, |values| values.replacement_spans(text));
        let detected = subtract_linked_spans(detected, &linked);
        Ok(RedactionPlan { detected, linked })
    }

    pub(crate) fn report_for_plan(&self, plan: &RedactionPlan) -> ReportAnonymizationReport {
        let mut category_counts = BTreeMap::new();
        for span in &plan.detected {
            *category_counts.entry(span.category).or_insert(0) += 1;
        }
        ReportAnonymizationReport {
            schema_version: REPORT_SCHEMA_VERSION,
            detected_spans: plan.detected.len() as u64,
            linked_spans: plan.linked.len() as u64,
            category_counts,
            model: self.detector.metadata(),
            human_review_required: true,
        }
    }
}

pub(crate) struct RedactionPlan {
    pub(crate) detected: Vec<DetectedSpan>,
    pub(crate) linked: Vec<crate::linkage::ReplacementSpan>,
}

struct Replacement<'a> {
    start: usize,
    end: usize,
    value: &'a str,
}

fn subtract_linked_spans(
    detected: Vec<DetectedSpan>,
    linked: &[crate::linkage::ReplacementSpan],
) -> Vec<DetectedSpan> {
    let mut remaining = Vec::new();
    for span in detected {
        let mut cursor = span.start;
        for protected in linked
            .iter()
            .filter(|protected| protected.start < span.end && protected.end > span.start)
        {
            if cursor < protected.start {
                remaining.push(DetectedSpan {
                    start: cursor,
                    end: protected.start,
                    ..span.clone()
                });
            }
            cursor = cursor.max(protected.end);
        }
        if cursor < span.end {
            remaining.push(DetectedSpan {
                start: cursor,
                ..span
            });
        }
    }
    remaining
}

fn validated_non_overlapping_spans(
    text: &str,
    mut spans: Vec<DetectedSpan>,
) -> Result<Vec<DetectedSpan>> {
    for span in &spans {
        if span.start >= span.end || span.end > text.len() {
            return Err(Error::InvalidSpan(
                "the byte range is outside the source document".to_owned(),
            ));
        }
        if !text.is_char_boundary(span.start) || !text.is_char_boundary(span.end) {
            return Err(Error::InvalidSpan(
                "the byte range does not follow a UTF-8 boundary".to_owned(),
            ));
        }
        if !span.score.is_finite() || !(0.0..=1.0).contains(&span.score) {
            return Err(Error::InvalidSpan(
                "the confidence score must be finite and between zero and one".to_owned(),
            ));
        }
    }

    spans.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| right.end.cmp(&left.end))
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.category.cmp(&right.category))
    });
    let mut kept: Vec<DetectedSpan> = Vec::with_capacity(spans.len());
    for span in spans {
        if kept
            .last()
            .is_none_or(|previous| span.start >= previous.end)
        {
            kept.push(span);
        }
    }
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linkage::ReplacementSpan;

    struct StaticDetector {
        spans: Vec<DetectedSpan>,
        metadata: Option<DetectorMetadata>,
    }

    impl PrivacyDetector for StaticDetector {
        fn detect(&self, _text: &str) -> Result<Vec<DetectedSpan>> {
            Ok(self.spans.clone())
        }

        fn metadata(&self) -> Option<DetectorMetadata> {
            self.metadata.clone()
        }
    }

    #[test]
    fn category_placeholders_are_stable() {
        let placeholders = [
            Category::AccountNumber.placeholder(),
            Category::PrivateAddress.placeholder(),
            Category::PrivateDate.placeholder(),
            Category::PrivateEmail.placeholder(),
            Category::PrivatePerson.placeholder(),
            Category::PrivatePhone.placeholder(),
            Category::PrivateUrl.placeholder(),
            Category::Secret.placeholder(),
        ];

        assert_eq!(placeholders[0], "[ACCOUNT_NUMBER]");
        assert_eq!(placeholders[7], "[SECRET]");
        assert_eq!(placeholders.len(), 8);
    }

    #[test]
    fn detected_span_constructor_rejects_invalid_ranges_and_scores() {
        assert!(DetectedSpan::new(Category::Secret, 1, 1, 0.5).is_err());
        assert!(DetectedSpan::new(Category::Secret, 0, 1, -0.1).is_err());
        assert!(DetectedSpan::new(Category::Secret, 0, 1, f32::NAN).is_err());
        assert!(DetectedSpan::new(Category::Secret, 0, 1, 1.1).is_err());
    }

    #[test]
    fn anonymizer_rejects_oversized_and_non_utf8_documents() {
        let anonymizer = ReportAnonymizer::new(
            StaticDetector {
                spans: Vec::new(),
                metadata: None,
            },
            ReportOptions { max_text_bytes: 2 },
        );

        assert!(matches!(
            anonymizer.anonymize_text(b"abc"),
            Err(Error::InputTooLarge { limit: 2 })
        ));
        assert!(matches!(
            anonymizer.anonymize_text(&[0xff]),
            Err(Error::InvalidUtf8)
        ));
    }

    #[test]
    fn overlapping_detector_spans_are_reduced_deterministically() {
        let text = "Alice Smith";
        let spans = vec![
            DetectedSpan::new(Category::PrivatePerson, 0, 5, 0.8).unwrap(),
            DetectedSpan::new(Category::PrivatePerson, 0, text.len(), 0.9).unwrap(),
            DetectedSpan::new(Category::Secret, 6, text.len(), 0.99).unwrap(),
        ];
        let anonymizer = ReportAnonymizer::new(
            StaticDetector {
                spans,
                metadata: Some(DetectorMetadata {
                    name: "test detector".to_owned(),
                    revision: "test revision".to_owned(),
                    variant: "test variant".to_owned(),
                    device: "cpu".to_owned(),
                }),
            },
            ReportOptions::default(),
        );

        let output = anonymizer.anonymize_text(text.as_bytes()).unwrap();

        assert_eq!(output.bytes, b"[PRIVATE_PERSON]");
        assert_eq!(output.report.detected_spans, 1);
        assert_eq!(output.report.model.unwrap().device, "cpu");
    }

    #[test]
    fn detector_spans_are_split_around_linked_replacements() {
        let detected = vec![DetectedSpan::new(Category::PrivatePerson, 0, 10, 0.9).unwrap()];
        let linked = vec![ReplacementSpan {
            start: 3,
            end: 7,
            replacement: "safe".to_owned(),
        }];

        let remaining = subtract_linked_spans(detected, &linked);

        assert_eq!(remaining.len(), 2);
        assert_eq!((remaining[0].start, remaining[0].end), (0, 3));
        assert_eq!((remaining[1].start, remaining[1].end), (7, 10));
    }

    #[test]
    fn span_validation_rejects_out_of_range_and_invalid_utf8_offsets() {
        let out_of_range = DetectedSpan {
            category: Category::Secret,
            start: 0,
            end: 10,
            score: 0.5,
        };
        let invalid_boundary = DetectedSpan {
            category: Category::Secret,
            start: 1,
            end: 2,
            score: 0.5,
        };
        let invalid_score = DetectedSpan {
            category: Category::Secret,
            start: 0,
            end: 1,
            score: f32::INFINITY,
        };

        assert!(validated_non_overlapping_spans("abc", vec![out_of_range]).is_err());
        assert!(validated_non_overlapping_spans("é", vec![invalid_boundary]).is_err());
        assert!(validated_non_overlapping_spans("a", vec![invalid_score]).is_err());
    }
}
