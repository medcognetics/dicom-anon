//! Error types.

use thiserror::Error;

/// Library result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned by de-identification and validation operations.
#[derive(Debug, Error)]
pub enum Error {
    /// The input could not be parsed as a DICOM Part 10 file.
    #[error("failed to read DICOM input: {0}")]
    Read(String),
    /// The output DICOM could not be encoded.
    #[error("failed to write DICOM output: {0}")]
    Write(String),
    /// An input or output file operation failed.
    #[error("I/O error for {path}: {source}")]
    Io {
        /// Affected path or stream name.
        path: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The key did not meet the minimum entropy-bearing length.
    #[error("the de-identification key must contain at least 32 bytes")]
    KeyTooShort,
    /// No stable patient-level value was available.
    #[error(
        "Patient ID, Issuer of Patient ID, and Patient Name are empty; provide an explicit patient scope"
    )]
    MissingPatientScope,
    /// The input already declares that patient identity was removed.
    #[error("input already declares Patient Identity Removed=YES")]
    AlreadyDeidentified,
    /// A temporal value could not be transformed safely.
    #[error("invalid {vr} value in {tag}: {value:?}")]
    InvalidTemporal {
        /// DICOM tag rendered as `(gggg,eeee)`.
        tag: String,
        /// Value representation.
        vr: &'static str,
        /// Original value.
        value: String,
    },
    /// A paired DA/TM value had incompatible multiplicity.
    #[error("paired temporal attributes {date_tag} and {time_tag} have different multiplicities")]
    MismatchedTemporalPair {
        /// Paired date attribute.
        date_tag: String,
        /// Paired time attribute.
        time_tag: String,
    },
    /// A sequence VR did not contain sequence items.
    #[error("sequence attribute {0} does not contain a sequence value")]
    InvalidSequence(String),
    /// Required DICOM metadata was absent.
    #[error("required DICOM attribute {0} is missing or empty")]
    MissingAttribute(&'static str),
    /// A configured UID root was invalid.
    #[error("invalid UID root {0:?}; expected numeric components separated by dots")]
    InvalidUidRoot(String),
    /// Date-shift range was not positive.
    #[error("date shift range must be at least one day")]
    InvalidDateShiftRange,
}
