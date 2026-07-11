//! Node.js byte-oriented bindings for dicom-anon.

use std::io::Cursor;

use dicom_anon::{anonymize_reader, read_dicom, validate_object, Anonymizer, Options, SecretKey};
use napi::bindgen_prelude::Buffer;
use napi::{Error, Result, Status};
use napi_derive::napi;

/// Result returned by `anonymize`.
#[napi(object)]
pub struct AnonymizeResult {
    /// Complete DICOM Part 10 output.
    pub data: Buffer,
    /// Serialized `AnonymizationReport`.
    pub report_json: String,
}

/// Result returned by `validate`.
#[napi(object)]
pub struct ValidateResult {
    /// Whether the metadata conforms to the selected profile.
    pub conformant: bool,
    /// Serialized `ValidationReport`.
    pub report_json: String,
}

/// De-identify DICOM Part 10 bytes.
#[napi]
pub fn anonymize(
    data: Buffer,
    key: Buffer,
    patient_scope: Option<String>,
) -> Result<AnonymizeResult> {
    let secret = SecretKey::new(key.as_ref()).map_err(napi_error)?;
    let mut options = Options::new(secret);
    options.patient_scope = patient_scope;
    let anonymizer = Anonymizer::new(options).map_err(napi_error)?;
    let (output, report) =
        anonymize_reader(&anonymizer, Cursor::new(data.as_ref())).map_err(napi_error)?;
    Ok(AnonymizeResult {
        data: output.into(),
        report_json: serde_json::to_string(&report).map_err(napi_error)?,
    })
}

/// Validate DICOM Part 10 bytes.
#[napi]
pub fn validate(data: Buffer) -> Result<ValidateResult> {
    let object = read_dicom(Cursor::new(data.as_ref())).map_err(napi_error)?;
    let report = validate_object(&object);
    Ok(ValidateResult {
        conformant: report.conformant,
        report_json: serde_json::to_string(&report).map_err(napi_error)?,
    })
}

fn napi_error(error: impl std::fmt::Display) -> Error {
    Error::new(Status::InvalidArg, error.to_string())
}
