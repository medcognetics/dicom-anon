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
    #[error("invalid {vr} value in {tag}")]
    InvalidTemporal {
        /// DICOM tag rendered as `(gggg,eeee)`.
        tag: String,
        /// Value representation.
        vr: &'static str,
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
    /// A requested DICOM option requires unsupported pixel processing.
    #[error("unsupported DICOM confidentiality option: {0}")]
    UnsupportedProfileOption(String),
    /// Enabled option columns specify incompatible actions.
    #[error("conflicting DICOM confidentiality options: {0}")]
    ConflictingProfileOptions(String),
    /// A serialized profile mapping or registry entry was malformed.
    #[error("invalid profile configuration: {0}")]
    InvalidProfileConfiguration(String),
    /// An option would weaken the Safe Harbor metadata policy.
    #[error("option is not permitted by the Safe Harbor metadata policy: {0}")]
    UnsupportedSafeHarborOption(String),
    /// Safe Harbor mode requires an independently assigned patient scope.
    #[error("Safe Harbor metadata mode requires an explicit independently assigned patient scope")]
    SafeHarborPatientScopeRequired,
    /// Safe Harbor mode lacked an independently assigned UID replacement.
    ///
    /// The path identifies the attribute. The error never contains the UID value.
    #[error("Safe Harbor UID mapping has no entry for the source UID at {path}")]
    MissingSafeHarborUidMapping {
        /// Attribute path, for example `(0020,9221)[0]/(0020,9164)`.
        path: String,
    },
    /// A source UID that needs a generated replacement is empty or malformed.
    ///
    /// The path identifies the attribute. The error never contains the UID value.
    #[error("source UID at {path} is empty or is not a valid DICOM UID")]
    InvalidSourceUid {
        /// Attribute path, for example `(0020,000D)`.
        path: String,
    },
    /// UID map generation was requested outside the Safe Harbor metadata policy.
    #[error("UID map generation requires the safe-harbor-metadata policy")]
    UidMapGenerationRequiresSafeHarbor,
    /// The operating-system random source failed or gave no unique replacement.
    #[error("cannot generate a random replacement UID: {0}")]
    RandomUidGeneration(String),
    /// A configured UID replacement was invalid or collided with another mapping.
    #[error("invalid Safe Harbor UID mapping: {0}")]
    InvalidSafeHarborUidMapping(String),
    /// Patient's Age did not contain a valid DICOM AS value.
    #[error("invalid Patient's Age value")]
    InvalidPatientAge,
}
