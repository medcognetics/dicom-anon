# dicom-anon

`dicom-anon` de-identifies individual DICOM Part 10 files and checks metadata against the same confidentiality profile. The workspace provides a Rust library, two Unix-style commands, a Python extension, and a Node.js N-API module.

The default policy implements the metadata actions from the [DICOM PS3.15 2026c Basic Application Level Confidentiality Profile](https://dicom.nema.org/medical/dicom/current/output/chtml/part15/chapter_E.html) with the Retain Longitudinal Temporal Information With Modified Dates option. Patient identifiers and UIDs receive keyed deterministic replacements. Dates and times move by patient-specific whole-day and whole-hour offsets. Midnight adjustments are applied to known paired date/time attributes so intervals between studies remain unchanged.

This software does not inspect pixel data for burned-in text or recognizable faces. It does not claim the Clean Pixel Data or Clean Recognizable Visual Features options. Read [Conformance and scope](docs/conformance.md) before using output outside a controlled environment.

## Install and build

The workspace requires Rust 1.88 or newer.

```console
cargo build --workspace --release
```

The two commands are written to `target/release/`:

```text
dicom-anonymize
dicom-validate
```

Run formatting, lint, tests, Rustdoc, and binding type checks with:

```console
make check
```

`make check` also checks the TypeScript declaration and Python stub. It requires Node.js, npm, and `uv` in addition to Rust.

## De-identify a file

Create a key file with at least 32 random bytes. Keep the same key when files must retain patient, study, series, or instance relationships.

```console
head -c 32 /dev/urandom > deidentification.key
chmod 600 deidentification.key
target/release/dicom-anonymize input.dcm output.dcm \
  --key-file deidentification.key
```

Atomic in-place replacement is explicit:

```console
target/release/dicom-anonymize input.dcm --in-place \
  --key-file deidentification.key
```

The command also composes with pipes. DICOM bytes use stdout; errors and the optional verbose report use stderr.

```console
target/release/dicom-anonymize input.dcm - \
  --key-file deidentification.key \
  | target/release/dicom-validate - --format json
```

The command fails when Patient ID, Issuer of Patient ID, and Patient Name cannot provide a stable patient scope. Pass `--patient-scope VALUE` when the caller has a stable association key that is not stored in the file.

## Validate a file

```console
target/release/dicom-validate output.dcm
target/release/dicom-validate output.dcm --format json
```

Exit status is part of the interface:

| Status | `dicom-anonymize` | `dicom-validate` |
|---:|---|---|
| 0 | Output written | Metadata conforms; warnings may remain |
| 1 | Not used | Metadata does not conform |
| 2 | Usage, input, output, or runtime error | Usage, input, or runtime error |

Validation checks profile markers, required removals, private attributes, replacement UID syntax, nested sequences, and identifying File Meta Information. A warning reports pixel data that was not assessed. Validation cannot prove that a dummy text value differs from an original value, nor can it establish that pixels are safe to disclose.

## Rust API

```rust
use dicom_anon::{anonymize_file, Anonymizer, Options, SecretKey};

# fn run() -> Result<(), Box<dyn std::error::Error>> {
let key = SecretKey::new([7_u8; 32])?;
let anonymizer = Anonymizer::new(Options::new(key))?;
let report = anonymize_file(&anonymizer, "input.dcm", "output.dcm")?;
println!("{}", report.patient_pseudonym);
# Ok(())
# }
```

The public API also supports readers, writers, parsed `dicom-rs` objects, and validation reports. See [Rust API and bindings](docs/api-and-bindings.md).

## Workspace layout

| Path | Contents |
|---|---|
| `core/` | Rust library, commands, tests, examples, and Criterion benchmarks |
| `bindings/python/` | PyO3 extension and Python type stub |
| `bindings/node/` | N-API module and TypeScript declaration |
| `docs/` | CLI, conformance, security, API, and benchmark documentation |

## Documentation

- [CLI contract](docs/cli.md)
- [Conformance and scope](docs/conformance.md)
- [Rust API and bindings](docs/api-and-bindings.md)
- [Key management and disclosure safety](docs/security.md)
- [Benchmarks](docs/benchmarks.md)
- [Contributing](CONTRIBUTING.md)

## Regulatory status

DICOM PS3.15 defines the implemented confidentiality profile. ISO 13485:2016 defines quality-management-system requirements, not an anonymization algorithm. The FDA’s Quality Management System Regulation incorporated ISO 13485:2016 by reference and became effective on February 2, 2026. A manufacturer must place this software within its own controlled design, risk-management, verification, validation, release, and postmarket processes. Repository tests and traceability help that work, but do not establish FDA, ISO 13485, HIPAA, GDPR, or other legal compliance.

## License

Licensed under either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT) at your option.
