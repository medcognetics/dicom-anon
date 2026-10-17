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

`Options::new`, `ValidationOptions::default()`, and binding calls without configuration JSON select `dicom-extended`. It enables Clean Descriptors, Retain Modified Dates, and Retain Patient Characteristics. Use `ProfileConfiguration::dicom()` for the Basic Profile without option columns:

```rust
# use dicom_anon::ProfileConfiguration;
let basic = ProfileConfiguration::dicom();
let extended = ProfileConfiguration::dicom_extended();
assert!(basic.options.is_empty());
assert_eq!(extended, ProfileConfiguration::default());
```

Configuration can be changed before constructing the anonymizer:

```rust
# use dicom_anon::{DicomOption, Options, ProfileConfiguration, SecretKey};
# fn run() -> Result<(), Box<dyn std::error::Error>> {
let mut options = Options::new(SecretKey::new([42_u8; 32])?);
options.profile = ProfileConfiguration::safe_harbor();
options.patient_scope = Some("SUBJECT-42".to_owned());
options.profile.options.insert(DicomOption::RetainSafePrivate);
options.profile.uid_mappings.insert("1.2.3".to_owned(), "2.25.101".to_owned());
# Ok(())
# }
```

Use `validate_object_with_options` for configured candidate-only validation. Use `validate_source_object` with the original, candidate, and matching anonymization `Options` for exact source comparison. Existing `validate_object` and `validate_file` remain default-configuration wrappers.

## Python

Build and install the extension with maturin:

```console
uv sync --project tools/ci --frozen
uv run --project tools/ci --frozen \
  maturin develop --manifest-path bindings/python/Cargo.toml --release
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

configuration_json = json.dumps({
    "policy": "dicom",
    "options": [
        "clean-descriptors",
        "retain-modified-dates",
        "retain-patient-characteristics",
    ],
    "descriptor_mappings": {},
    "safe_private_allowlist": [],
    "uid_mappings": {},
    "fail_on_unmapped": [],
})
conformant, validation_json = dicom_anon_python.validate(
    output, configuration_json
)
exact, exact_json = dicom_anon_python.validate_with_source(
    source, output, key, None, configuration_json
)
```

The configuration JSON above is the explicit form of the `dicom-extended` preset. `fail_on_unmapped` lists canonical eight-digit tags that must not get a dummy value because no `descriptor_mappings` entry matched; a match is an error. To use a shipped versioned descriptor map, copy its `mappings` member into `descriptor_mappings`. The report JSON includes the per-tag `dummy_replacements` and `unmapped_dummy_replacements` counts. Omitting `configuration_json` selects the same options. Use `"policy": "dicom"` with an empty `options` array for the option-free Basic Profile.

`anonymize` accepts optional `patient_scope` and `configuration_json` arguments. `validate` accepts optional configuration JSON. `validate_with_source` is a separate function so candidate-only `validate` retains its original meaning. Errors become `ValueError`. The JSON fields match the Rust report models.

## Node.js and TypeScript

The Node.js binding requires Node.js 22 or newer.

Build the platform module with the pinned N-API CLI:

```console
cd bindings/node
npm ci
npm run build
```

The N-API surface accepts and returns `Buffer` values:

```typescript
import { readFileSync, writeFileSync } from "node:fs";
import { anonymize, validate, validateWithSource } from "@dicom-anon/core";

const source = readFileSync("input.dcm");
const key = readFileSync("deidentification.key");
const result = anonymize(source, key);
writeFileSync("output.dcm", result.data);

const validation = validate(result.data);
if (!validation.conformant) {
  throw new Error(validation.reportJson);
}

const exact = validateWithSource(source, result.data, key);
```

`anonymize` accepts optional `patientScope` and `configurationJson` arguments. `validate` accepts optional configuration JSON. `validateWithSource` accepts the source, candidate, key, optional patient scope, and optional configuration JSON. N-API errors use `InvalidArg` with the Rust error message.

## Binding consistency

Rust performs parsing, transformation, validation, and report serialization for every language. The wrappers do not contain independent tag policy. Runtime smoke tests load both native modules and verify that invalid Part 10 bytes return the documented structured language error instead of crashing or bypassing Rust parsing.
