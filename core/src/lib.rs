#![doc = include_str!("../../README.md")]

mod anonymize;
#[doc(hidden)]
pub mod cli_common;
mod error;
mod io;
mod pixel;
mod profile;
mod pseudonym;
mod temporal;
mod validate;

pub use anonymize::{AnonymizationReport, Anonymizer, Options};
pub use error::{Error, Result};
pub use io::{anonymize_file, anonymize_reader, read_dicom, write_dicom};
pub use profile::{
    profile_rule, profile_rule_for, DeidentificationPolicy, DicomOption, ProfileAction,
    ProfileConfiguration, ProfileRule, ResolvedProfileRule, SafePrivateElement, PROFILE_NAME,
    PROFILE_VERSION,
};
pub use pseudonym::SecretKey;
pub use validate::{
    validate_file, validate_file_with_options, validate_object, validate_object_with_options,
    validate_source_object, Severity, ValidationFinding, ValidationMode, ValidationOptions,
    ValidationReport,
};

pub use dicom_object::DefaultDicomObject;
