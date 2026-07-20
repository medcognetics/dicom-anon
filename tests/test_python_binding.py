"""Runtime smoke tests for the Python native binding."""

import pytest

import dicom_anon_python


INVALID_DICOM = b"not a DICOM Part 10 object"


def test_python_binding_returns_a_structured_error() -> None:
    with pytest.raises(ValueError) as error:
        dicom_anon_python.validate(INVALID_DICOM)

    assert str(error.value)
