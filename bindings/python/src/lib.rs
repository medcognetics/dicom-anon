//! Python byte-oriented bindings for dicom-anon.

use std::io::Cursor;

use dicom_anon::{
    anonymize_reader, read_dicom, validate_object_with_options, validate_source_object,
    write_dicom, Anonymizer, Options, ProfileConfiguration, SecretKey, ValidationOptions,
};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;

/// De-identify DICOM Part 10 bytes and return `(output_bytes, report_json)`.
#[pyfunction]
#[pyo3(signature = (data, key, patient_scope=None, configuration_json=None))]
fn anonymize(
    py: Python<'_>,
    data: &[u8],
    key: &[u8],
    patient_scope: Option<&str>,
    configuration_json: Option<&str>,
) -> PyResult<(Py<PyBytes>, String)> {
    let anonymizer = anonymizer(key, patient_scope, configuration_json)?;
    let (output, report) = anonymize_reader(&anonymizer, Cursor::new(data)).map_err(value_error)?;
    let report = serde_json::to_string(&report).map_err(value_error)?;
    Ok((PyBytes::new(py, &output).unbind(), report))
}

/// Return a JSON list of `{path, uid}` source UID occurrences that the profile replaces.
#[pyfunction]
#[pyo3(signature = (data, key, patient_scope=None, configuration_json=None))]
fn required_uids(
    data: &[u8],
    key: &[u8],
    patient_scope: Option<&str>,
    configuration_json: Option<&str>,
) -> PyResult<String> {
    let anonymizer = anonymizer(key, patient_scope, configuration_json)?;
    let object = read_dicom(Cursor::new(data)).map_err(value_error)?;
    let required = anonymizer.required_uids(&object).map_err(value_error)?;
    serde_json::to_string(&required).map_err(value_error)
}

/// De-identify with random Safe Harbor UIDs and return `(output_bytes, report_json, uid_map_json)`.
#[pyfunction]
#[pyo3(signature = (data, key, patient_scope=None, configuration_json=None))]
fn anonymize_with_generated_uid_map(
    py: Python<'_>,
    data: &[u8],
    key: &[u8],
    patient_scope: Option<&str>,
    configuration_json: Option<&str>,
) -> PyResult<(Py<PyBytes>, String, String)> {
    let anonymizer = anonymizer(key, patient_scope, configuration_json)?;
    let object = read_dicom(Cursor::new(data)).map_err(value_error)?;
    let (output, report, uid_map) = anonymizer
        .anonymize_with_generated_uid_map(&object)
        .map_err(value_error)?;
    let mut bytes = Vec::new();
    write_dicom(&output, &mut bytes).map_err(value_error)?;
    let report = serde_json::to_string(&report).map_err(value_error)?;
    let uid_map = serde_json::to_string(&uid_map).map_err(value_error)?;
    Ok((PyBytes::new(py, &bytes).unbind(), report, uid_map))
}

/// Validate DICOM Part 10 bytes and return `(conformant, report_json)`.
#[pyfunction]
#[pyo3(signature = (data, configuration_json=None))]
fn validate(data: &[u8], configuration_json: Option<&str>) -> PyResult<(bool, String)> {
    let object = read_dicom(Cursor::new(data)).map_err(value_error)?;
    let profile = configuration_json
        .map(parse_configuration)
        .transpose()?
        .unwrap_or_default();
    let report = validate_object_with_options(&object, &ValidationOptions { profile })
        .map_err(value_error)?;
    let conformant = report.conformant;
    let report = serde_json::to_string(&report).map_err(value_error)?;
    Ok((conformant, report))
}

/// Validate candidate bytes by comparing them with the original DICOM bytes.
#[pyfunction]
#[pyo3(signature = (source, candidate, key, patient_scope=None, configuration_json=None))]
fn validate_with_source(
    source: &[u8],
    candidate: &[u8],
    key: &[u8],
    patient_scope: Option<&str>,
    configuration_json: Option<&str>,
) -> PyResult<(bool, String)> {
    let source = read_dicom(Cursor::new(source)).map_err(value_error)?;
    let candidate = read_dicom(Cursor::new(candidate)).map_err(value_error)?;
    let mut options = Options::new(SecretKey::new(key).map_err(value_error)?);
    options.patient_scope = patient_scope.map(str::to_owned);
    if let Some(configuration_json) = configuration_json {
        options.profile = parse_configuration(configuration_json)?;
    }
    let report = validate_source_object(&source, &candidate, options).map_err(value_error)?;
    let conformant = report.conformant;
    let report = serde_json::to_string(&report).map_err(value_error)?;
    Ok((conformant, report))
}

#[pymodule]
fn dicom_anon_python(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(anonymize, module)?)?;
    module.add_function(wrap_pyfunction!(required_uids, module)?)?;
    module.add_function(wrap_pyfunction!(anonymize_with_generated_uid_map, module)?)?;
    module.add_function(wrap_pyfunction!(validate, module)?)?;
    module.add_function(wrap_pyfunction!(validate_with_source, module)?)?;
    Ok(())
}

fn anonymizer(
    key: &[u8],
    patient_scope: Option<&str>,
    configuration_json: Option<&str>,
) -> PyResult<Anonymizer> {
    let mut options = Options::new(SecretKey::new(key).map_err(value_error)?);
    options.patient_scope = patient_scope.map(str::to_owned);
    if let Some(configuration_json) = configuration_json {
        options.profile = parse_configuration(configuration_json)?;
    }
    Anonymizer::new(options).map_err(value_error)
}

fn parse_configuration(value: &str) -> PyResult<ProfileConfiguration> {
    serde_json::from_str(value).map_err(value_error)
}

fn value_error(error: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(error.to_string())
}
