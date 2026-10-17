"""Runtime smoke tests for the Python native binding."""

import json

import pytest

import dicom_anon_python


INVALID_DICOM = b"not a DICOM Part 10 object"


def test_python_binding_returns_a_structured_error() -> None:
    with pytest.raises(ValueError) as error:
        dicom_anon_python.validate(INVALID_DICOM)

    assert str(error.value)


def _pixel_gate_configuration(sop_class: str) -> str:
    return json.dumps(
        {
            "policy": "dicom",
            "options": [],
            "require_no_burned_in_annotation": True,
            "sop_class_allowlist": [sop_class],
        }
    )


def test_python_binding_accepts_pixel_gate_configuration() -> None:
    key = b"0" * 32
    with pytest.raises(ValueError) as invalid:
        dicom_anon_python.anonymize(
            INVALID_DICOM, key, "SUBJ-0001", _pixel_gate_configuration("not-a-uid")
        )
    assert "SOP Class allowlist" in str(invalid.value)

    with pytest.raises(ValueError) as unreadable:
        dicom_anon_python.anonymize(
            INVALID_DICOM,
            key,
            "SUBJ-0001",
            _pixel_gate_configuration("1.2.840.10008.5.1.4.1.1.1.2"),
        )
    assert "SOP Class allowlist" not in str(unreadable.value)
