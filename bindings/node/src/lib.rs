//! Node.js byte-oriented bindings for dicom-anon.

use std::io::Cursor;

use dicom_anon::{
    anonymize_reader, read_dicom, validate_object_with_options, validate_source_object,
    write_dicom, Anonymizer, Options, ProfileConfiguration, SecretKey, ValidationOptions,
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

/// Result returned by `anonymizeWithGeneratedUidMap`.
#[napi(object)]
pub struct GeneratedUidMapResult {
    /// Complete DICOM Part 10 output.
    pub data: Buffer,
    /// Serialized `AnonymizationReport`.
    pub report_json: String,
    /// Complete source-to-replacement UID map. Keep it under re-identification custody.
    pub uid_map_json: String,
}

/// Result returned by `anonymizeStudy`.
#[napi(object)]
pub struct StudyResult {
    /// Complete DICOM Part 10 outputs, in input order.
    pub data: Vec<Buffer>,
    /// Serialized `AnonymizationReport` for each output.
    pub report_jsons: Vec<String>,
    /// Complete source-to-replacement UID map for the study. Keep it under re-identification custody.
    pub uid_map_json: String,
    /// Serialized source-aware `ValidationReport` for each output.
    pub validation_jsons: Vec<String>,
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
    let anonymizer = anonymizer(&key, patient_scope, configuration_json)?;
    let (output, report) =
        anonymize_reader(&anonymizer, Cursor::new(data.as_ref())).map_err(napi_error)?;
    Ok(AnonymizeResult {
        data: output.into(),
        report_json: serde_json::to_string(&report).map_err(napi_error)?,
    })
}

/// Return a JSON list of `{path, uid}` source UID occurrences that the profile replaces.
#[napi]
pub fn required_uids(
    data: Buffer,
    key: Buffer,
    patient_scope: Option<String>,
    configuration_json: Option<String>,
) -> Result<String> {
    let anonymizer = anonymizer(&key, patient_scope, configuration_json)?;
    let object = read_dicom(Cursor::new(data.as_ref())).map_err(napi_error)?;
    let required = anonymizer.required_uids(&object).map_err(napi_error)?;
    serde_json::to_string(&required).map_err(napi_error)
}

/// De-identify DICOM Part 10 bytes and assign random Safe Harbor replacement UIDs.
#[napi]
pub fn anonymize_with_generated_uid_map(
    data: Buffer,
    key: Buffer,
    patient_scope: Option<String>,
    configuration_json: Option<String>,
) -> Result<GeneratedUidMapResult> {
    let anonymizer = anonymizer(&key, patient_scope, configuration_json)?;
    let object = read_dicom(Cursor::new(data.as_ref())).map_err(napi_error)?;
    let (output, report, uid_map) = anonymizer
        .anonymize_with_generated_uid_map(&object)
        .map_err(napi_error)?;
    let mut bytes = Vec::new();
    write_dicom(&output, &mut bytes).map_err(napi_error)?;
    Ok(GeneratedUidMapResult {
        data: bytes.into(),
        report_json: serde_json::to_string(&report).map_err(napi_error)?,
        uid_map_json: serde_json::to_string(&uid_map).map_err(napi_error)?,
    })
}

/// De-identify every instance of one study under Safe Harbor with one generated UID map.
/// Fails as a whole when any instance fails; the error names only the instance index.
#[napi]
pub fn anonymize_study(
    instances: Vec<Buffer>,
    key: Buffer,
    patient_scope: String,
    configuration_json: Option<String>,
) -> Result<StudyResult> {
    let anonymizer = anonymizer(&key, Some(patient_scope), configuration_json)?;
    let sources = instances
        .iter()
        .enumerate()
        .map(|(index, data)| {
            read_dicom(Cursor::new(data.as_ref()))
                // The parser error can echo source values, such as an unknown transfer syntax.
                .map_err(|_| {
                    napi_error(format!(
                        "study instance {index}: not a readable DICOM Part 10 object"
                    ))
                })
        })
        .collect::<Result<Vec<_>>>()?;
    let study = anonymizer.anonymize_study(&sources).map_err(napi_error)?;
    let mut result = StudyResult {
        data: Vec::with_capacity(study.instances.len()),
        report_jsons: Vec::with_capacity(study.instances.len()),
        uid_map_json: serde_json::to_string(&study.uid_map).map_err(napi_error)?,
        validation_jsons: Vec::with_capacity(study.instances.len()),
    };
    for instance in &study.instances {
        let mut bytes = Vec::new();
        write_dicom(&instance.output, &mut bytes).map_err(napi_error)?;
        result.data.push(bytes.into());
        result
            .report_jsons
            .push(serde_json::to_string(&instance.report).map_err(napi_error)?);
        result
            .validation_jsons
            .push(serde_json::to_string(&instance.validation).map_err(napi_error)?);
    }
    Ok(result)
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

fn anonymizer(
    key: &Buffer,
    patient_scope: Option<String>,
    configuration_json: Option<String>,
) -> Result<Anonymizer> {
    let mut options = Options::new(SecretKey::new(key.as_ref()).map_err(napi_error)?);
    options.patient_scope = patient_scope;
    if let Some(configuration_json) = configuration_json {
        options.profile = parse_configuration(&configuration_json)?;
    }
    Anonymizer::new(options).map_err(napi_error)
}

fn parse_configuration(value: &str) -> Result<ProfileConfiguration> {
    serde_json::from_str(value).map_err(napi_error)
}

fn napi_error(error: impl std::fmt::Display) -> Error {
    Error::new(Status::InvalidArg, error.to_string())
}
