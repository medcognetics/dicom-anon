def anonymize(
    data: bytes,
    key: bytes,
    patient_scope: str | None = None,
    configuration_json: str | None = None,
) -> tuple[bytes, str]: ...

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
