"""Runtime smoke tests for the Python native binding."""

import json
import struct

import pytest

import dicom_anon_python


INVALID_DICOM = b"not a DICOM Part 10 object"


def test_python_binding_returns_a_structured_error() -> None:
    with pytest.raises(ValueError) as error:
        dicom_anon_python.validate(INVALID_DICOM)

    assert str(error.value)


KEY = b"0123456789abcdef0123456789abcdef"
SECONDARY_CAPTURE = b"1.2.840.10008.5.1.4.1.1.7"
SENTINEL_UID = "2.25.271828182845904523536028747135266249"
SOURCE_UIDS = (
    SENTINEL_UID,
    "2.25.82751374713386041856794579278177576159",
    "2.25.55250158356079243228430942758422479484",
)
SAFE_HARBOR = json.dumps(
    {
        "policy": "safe-harbor-metadata",
        "options": [
            "clean-graphics",
            "clean-structured-content",
            "clean-descriptors",
            "retain-modified-dates",
            "retain-patient-characteristics",
        ],
    }
)


def _element(group: int, element: int, vr: bytes, value: bytes) -> bytes:
    if len(value) % 2:
        value += b"\0" if vr == b"UI" else b" "
    header = struct.pack("<HH", group, element) + vr
    if vr == b"OB":
        return header + b"\0\0" + struct.pack("<I", len(value)) + value
    return header + struct.pack("<H", len(value)) + value


def _synthetic_dicom(instance_uid: str = SENTINEL_UID) -> bytes:
    instance = instance_uid.encode()
    meta = b"".join(
        [
            _element(0x0002, 0x0001, b"OB", b"\0\1"),
            _element(0x0002, 0x0002, b"UI", SECONDARY_CAPTURE),
            _element(0x0002, 0x0003, b"UI", instance),
            _element(0x0002, 0x0010, b"UI", b"1.2.840.10008.1.2.1"),
            _element(0x0002, 0x0012, b"UI", b"2.25.1"),
        ]
    )
    group_length = _element(0x0002, 0x0000, b"UL", struct.pack("<I", len(meta)))
    dataset = b"".join(
        [
            _element(0x0008, 0x0016, b"UI", SECONDARY_CAPTURE),
            _element(0x0008, 0x0018, b"UI", instance),
            _element(0x0010, 0x0020, b"LO", b"SYNTHETIC"),
            _element(0x0020, 0x000D, b"UI", SOURCE_UIDS[1].encode()),
            _element(0x0020, 0x000E, b"UI", SOURCE_UIDS[2].encode()),
        ]
    )
    return b"\0" * 128 + b"DICM" + group_length + meta + dataset


def test_python_binding_lists_required_uids_with_paths() -> None:
    required = json.loads(
        dicom_anon_python.required_uids(
            _synthetic_dicom(), KEY, "SUBJ-0001", SAFE_HARBOR
        )
    )

    assert [entry["path"] for entry in required] == [
        "(0008,0018)",
        "(0020,000D)",
        "(0020,000E)",
    ]
    assert {entry["uid"] for entry in required} == set(SOURCE_UIDS)


def test_python_binding_generates_random_complete_uid_maps() -> None:
    data = _synthetic_dicom()
    output, report, first = dicom_anon_python.anonymize_with_generated_uid_map(
        data, KEY, "SUBJ-0001", SAFE_HARBOR
    )
    _, _, second = dicom_anon_python.anonymize_with_generated_uid_map(
        data, KEY, "SUBJ-0001", SAFE_HARBOR
    )
    first_map = json.loads(first)
    second_map = json.loads(second)

    assert set(first_map) == set(SOURCE_UIDS)
    assert all(first_map[uid] != second_map[uid] for uid in SOURCE_UIDS)
    for uid in SOURCE_UIDS:
        assert uid.encode() not in output
        assert uid not in report


SECOND_INSTANCE_UID = "2.25.314159265358979323846264338327950288"


def test_python_binding_anonymizes_a_study_with_one_uid_map() -> None:
    outputs, reports, uid_map_json, validations = dicom_anon_python.anonymize_study(
        [_synthetic_dicom(), _synthetic_dicom(SECOND_INSTANCE_UID)],
        KEY,
        "SUBJ-0001",
        SAFE_HARBOR,
    )
    uid_map = json.loads(uid_map_json)

    assert len(outputs) == len(reports) == len(validations) == 2
    assert set(uid_map) == {*SOURCE_UIDS, SECOND_INSTANCE_UID}
    for output, report, validation in zip(outputs, reports, validations):
        assert json.loads(validation)["conformant"]
        # Both instances carry the same replacement study and series UIDs.
        assert uid_map[SOURCE_UIDS[1]].encode() in output
        assert uid_map[SOURCE_UIDS[2]].encode() in output
        for uid in (*SOURCE_UIDS, SECOND_INSTANCE_UID):
            assert uid.encode() not in output
            assert uid not in report
            assert uid not in validation
    assert uid_map[SENTINEL_UID].encode() in outputs[0]
    assert uid_map[SECOND_INSTANCE_UID].encode() in outputs[1]


def test_python_binding_study_error_names_instance_only() -> None:
    with pytest.raises(ValueError) as error:
        dicom_anon_python.anonymize_study(
            [_synthetic_dicom(), INVALID_DICOM], KEY, "SUBJ-0001", SAFE_HARBOR
        )

    assert "study instance 1" in str(error.value)
    assert SENTINEL_UID not in str(error.value)


def test_python_binding_study_parse_error_hides_source_values() -> None:
    # The parser echoes an unknown Transfer Syntax UID; the study call must not.
    tampered = _synthetic_dicom().replace(
        b"1.2.840.10008.1.2.1\0", b"MRN-SECRET-12345678\0"
    )
    with pytest.raises(ValueError) as error:
        dicom_anon_python.anonymize_study(
            [_synthetic_dicom(), tampered], KEY, "SUBJ-0001", SAFE_HARBOR
        )

    assert "study instance 1" in str(error.value)
    assert "MRN-SECRET" not in str(error.value)


def test_python_binding_missing_uid_error_names_path_only() -> None:
    with pytest.raises(ValueError) as error:
        dicom_anon_python.anonymize(_synthetic_dicom(), KEY, "SUBJ-0001", SAFE_HARBOR)

    assert "(0008,0018)" in str(error.value)
    assert SENTINEL_UID not in str(error.value)


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
