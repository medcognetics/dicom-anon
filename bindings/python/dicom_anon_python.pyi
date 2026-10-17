def anonymize(
    data: bytes,
    key: bytes,
    patient_scope: str | None = None,
    configuration_json: str | None = None,
) -> tuple[bytes, str]: ...

def required_uids(
    data: bytes,
    key: bytes,
    patient_scope: str | None = None,
    configuration_json: str | None = None,
) -> str: ...

def anonymize_with_generated_uid_map(
    data: bytes,
    key: bytes,
    patient_scope: str | None = None,
    configuration_json: str | None = None,
) -> tuple[bytes, str, str]: ...

def validate(
    data: bytes,
    configuration_json: str | None = None,
) -> tuple[bool, str]: ...

def validate_with_source(
    source: bytes,
    candidate: bytes,
    key: bytes,
    patient_scope: str | None = None,
    configuration_json: str | None = None,
) -> tuple[bool, str]: ...
