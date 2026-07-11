# Rust API and language bindings

## Rust

Create one `Anonymizer` and reuse it for files that share a de-identification key and policy.

```rust
use dicom_anon::{Anonymizer, Options, SecretKey};

# fn run() -> Result<(), Box<dyn std::error::Error>> {
let secret = SecretKey::new([42_u8; 32])?;
let mut options = Options::new(secret);
options.uid_root = "2.25".to_owned();
let anonymizer = Anonymizer::new(options)?;
# Ok(())
# }
```

File API:

```rust
# use dicom_anon::{anonymize_file, Anonymizer, Options, SecretKey};
# fn run() -> Result<(), Box<dyn std::error::Error>> {
# let anonymizer = Anonymizer::new(Options::new(SecretKey::new([42_u8; 32])?))?;
let report = anonymize_file(&anonymizer, "input.dcm", "output.dcm")?;
assert_eq!(report.profile_version, "2026c");
# Ok(())
# }
```

Stream API:

```rust
# use dicom_anon::{anonymize_reader, Anonymizer, Options, SecretKey};
# fn run() -> Result<(), Box<dyn std::error::Error>> {
# let anonymizer = Anonymizer::new(Options::new(SecretKey::new([42_u8; 32])?))?;
let input = std::fs::File::open("input.dcm")?;
let (bytes, report) = anonymize_reader(&anonymizer, input)?;
std::fs::write("output.dcm", bytes)?;
# let _ = report;
# Ok(())
```

Parsed-object API:

```rust
# use dicom_anon::{read_dicom, validate_object, Anonymizer, Options, SecretKey};
# fn run() -> Result<(), Box<dyn std::error::Error>> {
# let anonymizer = Anonymizer::new(Options::new(SecretKey::new([42_u8; 32])?))?;
let source = read_dicom(std::fs::File::open("input.dcm")?)?;
let (output, anonymization_report) = anonymizer.anonymize(&source)?;
let validation_report = validate_object(&output);
assert!(validation_report.conformant);
# let _ = anonymization_report;
# Ok(())
```

`AnonymizationReport` is serializable and records the patient pseudonym, whole-day date offset, whole-hour time offset, and action counts. `ValidationReport` contains a Boolean result and structured findings. `DefaultDicomObject` is re-exported for callers that do not otherwise depend on `dicom-object`.

## Python

Build and install the extension with maturin:

```console
cd bindings/python
uv venv
source .venv/bin/activate
uvx maturin develop --release
```

The binding is byte-oriented:

```python
import json
from pathlib import Path

import dicom_anon_python

source = Path("input.dcm").read_bytes()
key = Path("deidentification.key").read_bytes()
output, report_json = dicom_anon_python.anonymize(source, key)
Path("output.dcm").write_bytes(output)
report = json.loads(report_json)

conformant, validation_json = dicom_anon_python.validate(output)
```

`anonymize` accepts an optional third `patient_scope` argument. Errors become `ValueError`. The JSON fields match the Rust report models.

## Node.js and TypeScript

Build the platform module with the pinned N-API CLI:

```console
cd bindings/node
npm ci
npm run build
```

The N-API surface accepts and returns `Buffer` values:

```typescript
import { readFileSync, writeFileSync } from "node:fs";
import { anonymize, validate } from "@dicom-anon/core";

const source = readFileSync("input.dcm");
const key = readFileSync("deidentification.key");
const result = anonymize(source, key);
writeFileSync("output.dcm", result.data);

const validation = validate(result.data);
if (!validation.conformant) {
  throw new Error(validation.reportJson);
}
```

The optional third `patientScope` argument serves the same purpose as the Rust and Python options. N-API errors use `InvalidArg` with the Rust error message.

## Binding consistency

Rust performs parsing, transformation, validation, and report serialization for every language. The wrappers do not contain independent tag policy. Runtime smoke tests load both native modules, process the same Part 10 fixture twice, compare the resulting bytes, and validate the result.
