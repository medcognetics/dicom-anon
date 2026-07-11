//! Python byte-oriented bindings for dicom-anon.

use std::io::Cursor;

use dicom_anon::{anonymize_reader, read_dicom, validate_object, Anonymizer, Options, SecretKey};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;

/// De-identify DICOM Part 10 bytes and return `(output_bytes, report_json)`.
#[pyfunction]
#[pyo3(signature = (data, key, patient_scope=None))]
fn anonymize(
    py: Python<'_>,
    data: &[u8],
    key: &[u8],
    patient_scope: Option<&str>,
) -> PyResult<(Py<PyBytes>, String)> {
    let secret = SecretKey::new(key).map_err(value_error)?;
    let mut options = Options::new(secret);
    options.patient_scope = patient_scope.map(str::to_owned);
    let anonymizer = Anonymizer::new(options).map_err(value_error)?;
    let (output, report) = anonymize_reader(&anonymizer, Cursor::new(data)).map_err(value_error)?;
    let report = serde_json::to_string(&report).map_err(value_error)?;
    Ok((PyBytes::new(py, &output).unbind(), report))
}

/// Validate DICOM Part 10 bytes and return `(conformant, report_json)`.
#[pyfunction]
fn validate(data: &[u8]) -> PyResult<(bool, String)> {
    let object = read_dicom(Cursor::new(data)).map_err(value_error)?;
    let report = validate_object(&object);
    let conformant = report.conformant;
    let report = serde_json::to_string(&report).map_err(value_error)?;
    Ok((conformant, report))
}

#[pymodule]
fn dicom_anon_python(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(anonymize, module)?)?;
    module.add_function(wrap_pyfunction!(validate, module)?)?;
    Ok(())
}

fn value_error(error: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(error.to_string())
}
