//! Node.js byte-oriented bindings for dicom-anon.

use std::io::Cursor;

use dicom_anon::{
    anonymize_reader, read_dicom, validate_object_with_options, validate_source_object, Anonymizer,
    Options, ProfileConfiguration, SecretKey, ValidationOptions,
};
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
    configuration_json: Option<String>,
) -> Result<AnonymizeResult> {
    let secret = SecretKey::new(key.as_ref()).map_err(napi_error)?;
    let mut options = Options::new(secret);
    options.patient_scope = patient_scope;
    if let Some(configuration_json) = configuration_json {
        options.profile = parse_configuration(&configuration_json)?;
    }
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
pub fn validate(data: Buffer, configuration_json: Option<String>) -> Result<ValidateResult> {
    let object = read_dicom(Cursor::new(data.as_ref())).map_err(napi_error)?;
    let profile = configuration_json
        .as_deref()
        .map(parse_configuration)
        .transpose()?
        .unwrap_or_default();
    let report = validate_object_with_options(&object, &ValidationOptions { profile })
        .map_err(napi_error)?;
    Ok(ValidateResult {
        conformant: report.conformant,
        report_json: serde_json::to_string(&report).map_err(napi_error)?,
    })
}

/// Validate candidate bytes by comparing them with the original DICOM bytes.
#[napi]
pub fn validate_with_source(
    source: Buffer,
    candidate: Buffer,
    key: Buffer,
    patient_scope: Option<String>,
    configuration_json: Option<String>,
) -> Result<ValidateResult> {
    let source = read_dicom(Cursor::new(source.as_ref())).map_err(napi_error)?;
    let candidate = read_dicom(Cursor::new(candidate.as_ref())).map_err(napi_error)?;
    let mut options = Options::new(SecretKey::new(key.as_ref()).map_err(napi_error)?);
    options.patient_scope = patient_scope;
    if let Some(configuration_json) = configuration_json {
        options.profile = parse_configuration(&configuration_json)?;
    }
    let report = validate_source_object(&source, &candidate, options).map_err(napi_error)?;
    Ok(ValidateResult {
        conformant: report.conformant,
        report_json: serde_json::to_string(&report).map_err(napi_error)?,
    })
}

fn parse_configuration(value: &str) -> Result<ProfileConfiguration> {
    serde_json::from_str(value).map_err(napi_error)
}

fn napi_error(error: impl std::fmt::Display) -> Error {
    Error::new(Status::InvalidArg, error.to_string())
}
