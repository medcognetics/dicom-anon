def anonymize(
    data: bytes,
    key: bytes,
    patient_scope: str | None = None,
) -> tuple[bytes, str]: ...

def validate(data: bytes) -> tuple[bool, str]: ...
