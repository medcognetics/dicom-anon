#![doc = include_str!("../../README.md")]

mod anonymize;
#[doc(hidden)]
pub mod cli_common;
mod error;
mod io;
mod profile;
mod pseudonym;
mod temporal;
mod validate;

pub use anonymize::{AnonymizationReport, Anonymizer, Options};
pub use error::{Error, Result};
pub use io::{anonymize_file, anonymize_reader, read_dicom, write_dicom};
pub use profile::{profile_rule, ProfileAction, ProfileRule, PROFILE_NAME, PROFILE_VERSION};
pub use pseudonym::SecretKey;
pub use validate::{validate_file, validate_object, Severity, ValidationFinding, ValidationReport};

pub use dicom_object::DefaultDicomObject;
