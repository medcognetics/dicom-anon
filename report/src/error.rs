//! Report-anonymization errors.

use thiserror::Error;

/// Library result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned by report anonymization and asset handling.
#[derive(Debug, Error)]
pub enum Error {
    /// Plaintext input was not valid UTF-8.
    #[error("plaintext input must be valid UTF-8")]
    InvalidUtf8,
    /// A detector returned an invalid span.
    #[error("detector returned an invalid span: {0}")]
    InvalidSpan(String),
    /// Plaintext input exceeded the configured limit.
    #[error("plaintext input exceeds the configured {limit}-byte limit")]
    InputTooLarge {
        /// Configured byte limit.
        limit: usize,
    },
    /// Tokenized input exceeded the configured model-inference limit.
    #[error("privacy model input exceeds the configured {limit}-token limit")]
    InputTooManyTokens {
        /// Configured token limit.
        limit: usize,
    },
    /// Model configuration or inference failed.
    #[error("privacy model error: {0}")]
    Model(String),
    /// PDF parsing, rendering, or sanitization failed.
    #[error("PDF processing error: {0}")]
    Pdf(String),
    /// OCR initialization or recognition failed.
    #[error("OCR error: {0}")]
    Ocr(String),
    /// Asset download, verification, or installation failed.
    #[error("asset installation error: {0}")]
    Install(String),
    /// A DICOM input could not be used to construct safe linkage values.
    #[error("DICOM linkage error: {0}")]
    Dicom(String),
    /// The selected DICOM inputs do not share one patient scope.
    #[error("DICOM linkage inputs resolve to different patient scopes")]
    MixedPatients,
    /// One source value resolved to more than one safe output.
    #[error("DICOM linkage inputs contain a conflicting identifier mapping")]
    ConflictingLinkage,
    /// An input or output operation failed.
    #[error("I/O error for {path}: {source}")]
    Io {
        /// Affected path or stream.
        path: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}
