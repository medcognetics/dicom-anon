# dicom-anon

[![Linux CI](https://github.com/TidalPaladin/dicom-anon/actions/workflows/linux-ci.yml/badge.svg?branch=master)](https://github.com/TidalPaladin/dicom-anon/actions/workflows/linux-ci.yml)

`dicom-anon` de-identifies DICOM Part 10 files, plaintext reports, and PDFs. The workspace provides Rust libraries, three Unix-style commands, a Python extension, and a Node.js N-API module. Report filtering runs OpenAI Privacy Filter locally through ONNX Runtime.

The default `dicom-extended` preset implements the metadata actions from the [DICOM PS3.15 2026c Basic Application Level Confidentiality Profile](https://dicom.nema.org/medical/dicom/current/output/chtml/part15/chapter_E.html) with Clean Descriptors, Retain Patient Characteristics, and Retain Longitudinal Temporal Information With Modified Dates. Patient identifiers and UIDs receive keyed deterministic replacements. Dates and times move by patient-specific whole-day and whole-hour offsets. Patient ages from `090Y` through `999Y` are categorized as `090Y`. Study and Series Description are retained only when an exact caller-supplied mapping approves a safe replacement; otherwise they receive a safe dummy. The `dicom` preset applies the Basic Profile without optional columns.

This software does not inspect pixel data for burned-in text or recognizable faces. It does not claim the Clean Pixel Data or Clean Recognizable Visual Features options. Read [Conformance and scope](docs/conformance.md) before using output outside a controlled environment.

## Install and build

The workspace requires Rust 1.88 or newer. The Python binding supports Python 3.10 or newer, and the Node.js binding supports Node.js 22 or newer. PDF support also requires Tesseract 5, Leptonica, and the development headers needed by the `leptess` crate. On Debian or Ubuntu:

```console
sudo apt-get install libleptonica-dev libtesseract-dev tesseract-ocr-eng
```

```console
cargo build --workspace --release
```

For a plaintext-only binary without Tesseract, Leptonica, or PDFium support:

```console
cargo build -p dicom-anon-report --no-default-features --release
```

The three commands are written to `target/release/`:

```text
dicom-anonymize
dicom-validate
report-anon
```

Install the pinned CPU model, ONNX Runtime 1.27.1, and PDFium 7881 after the build. The installer verifies every download with SHA-256 and refuses to replace an existing bundle. The model loader verifies the graph, external weights, tokenizer, configuration, and calibration against the pinned digests before each load.

```console
target/release/report-anon model install --backend cpu
```

CUDA 12 and CUDA 13 packages use the same checksum-verified Q4 model with a matching ONNX Runtime release. CUDA and cuDNN remain system dependencies.

When `--runtime-library PATH` overrides runtime resolution, also pass `--device cpu`, `--device cuda`, or `--device cuda:N`. The tool does not infer an execution provider from a filename.

```console
target/release/report-anon model install --backend cuda13
```

Run formatting, lint, tests, Rustdoc, and binding type checks with:

```console
make check
```

`make check` also checks the TypeScript declaration and Python stub. It requires Node.js, npm, and `uv` in addition to Rust.

For the complete CI-equivalent gate, including native binding smoke tests, workflow policy tests, and at least 90% Rust line coverage, install actionlint 1.7.12 and cargo-llvm-cov 0.8.7, then run:

```console
uv sync --project tools/ci --frozen
make ci
```

GitHub Actions runs the latest and minimum supported Linux toolchains on pull requests and `master` pushes using the trusted `beryl` runner. A release-profile build runs nightly at 06:17 UTC. Dependency security and deprecation reports run each Monday at 06:37 UTC on GitHub-hosted Linux. The scheduled security audit reports every scanner result before failing on any finding or incomplete scan.

Asset-backed report integration remains a local gate. After installing the CPU bundle and setting the three `DICOM_ANON_REPORT_*` paths, run `make report-integration-coverage` to exercise real ONNX Runtime, PDFium, OCR, CLI, and rendered-redaction paths and enforce 84% report line coverage.

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

Every metadata option in Table E.1-1 can be enabled or disabled. For example:

```console
target/release/dicom-anonymize input.dcm output.dcm \
  --key-file deidentification.key \
  --policy dicom \
  --enable-option retain-device-identity
```

`clean-pixel-data` and `clean-recognizable-visual-features` are represented but rejected because this tool does not process pixels. Conflicting option columns are rejected before input is processed.

The anonymizer and validator use the same presets:

| `--policy` value | Enabled options |
|---|---|
| `dicom` | None |
| `dicom-extended` (default) | `clean-descriptors`, `retain-modified-dates`, `retain-patient-characteristics` |
| `safe-harbor-metadata` | `clean-graphics`, `clean-structured-content`, `clean-descriptors`, `retain-modified-dates`, `retain-patient-characteristics` |

The separate `safe-harbor-metadata` policy applies strict Safe Harbor transformations to metadata controlled by the tool. It requires an independently assigned non-PHI patient scope and a complete original-to-output UID map:

```console
target/release/dicom-anonymize input.dcm output.dcm \
  --policy safe-harbor-metadata \
  --patient-scope SUBJECT-42 \
  --uid-map uid-map.json \
  --key-file deidentification.key
```

This policy empties Patient ID, Patient Name, and Patient Birth Date; stores a keyed subject token in Clinical Trial Subject ID; aggregates DA to `YYYY0101`, DT to `YYYY`, and TM to `000000`; and caps age at `090Y`. It is a metadata policy result, not a claim that a file or organization is HIPAA compliant.

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

Candidate-only validation accepts the same profile flags as anonymization and reports one `source_not_compared` warning for transformations it cannot prove. Source-aware validation verifies the exact configured transformation without placing source values or secrets in findings:

```console
target/release/dicom-validate output.dcm \
  --source input.dcm \
  --key-file deidentification.key
```

## Filter plaintext and PDF reports

Plaintext works as a Unix filter. Sanitized data is the only stdout content; progress and errors use stderr.

```console
printf '%s\n' 'Patient Alice Smith has MRN 123456.' \
  | target/release/report-anon anonymize --device cpu \
  > report.safe.txt
```

PDF input is detected from a `.pdf` filename or a `%PDF-` header within the first 1,024 bytes, including stdin with leading transport bytes. Use `--input-format text|pdf` to override automatic typing; explicit text mode treats the input as UTF-8 plaintext even when it matches the PDF heuristic. The output is a newly constructed image-only PDF, so the source text layer, attachments, metadata, and incremental history are not copied. Auto mode uses embedded text geometry when available and also runs Tesseract on every rendered page.

PDF rendering is limited per page and across the complete document. The defaults are 100,000,000 pixels per page and 250,000,000 total pixels; use `--max-render-pixels` and `--max-total-render-pixels` to reduce them. A detected span fails the PDF operation unless every non-whitespace character maps to a paintable page rectangle.

```console
target/release/report-anon anonymize report.pdf report.safe.pdf \
  --report report.safe.report.json
```

Add one or more original DICOM files to replace matching report identifiers with the same patient pseudonym and UIDs used by DICOM anonymization:

```console
target/release/report-anon anonymize report.txt report.safe.txt \
  --dicom source-1.dcm \
  --dicom source-2.dcm \
  --key-file deidentification.key
```

This writes `report.safe.txt.association.json`. The manifest contains only output identifiers and a SHA-256 digest of the sanitized report. Use the same key, profile, patient scope, UID root, and mapping files for `dicom-anonymize`. Read [Report and PDF anonymization](docs/report-anonymization.md) for the exact workflow, Safe Harbor behavior, and limitations.

Every file destination is normalized before comparison. A positional output cannot alias the input unless `--in-place` is present, and in-place mode rejects a symbolic-link input instead of replacing the link entry. Primary and sidecar outputs cannot replace DICOM inputs, key or mapping files, or files inventoried in an installed model and runtime bundle.

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
| `report/` | Rust Privacy Filter runtime, plaintext/PDF anonymization, DICOM linkage, installer, and CLI |
| `bindings/python/` | PyO3 extension and Python type stub |
| `bindings/node/` | N-API module and TypeScript declaration |
| `docs/` | CLI, conformance, security, API, and benchmark documentation |

## Documentation

- [CLI contract](docs/cli.md)
- [Conformance and scope](docs/conformance.md)
- [Report and PDF anonymization](docs/report-anonymization.md)
- [Rust API and bindings](docs/api-and-bindings.md)
- [Key management and disclosure safety](docs/security.md)
- [Benchmarks](docs/benchmarks.md)
- [Contributing](CONTRIBUTING.md)

## Regulatory status

DICOM PS3.15 defines the implemented confidentiality profile. ISO 13485:2016 defines quality-management-system requirements, not an anonymization algorithm. The FDA’s Quality Management System Regulation incorporated ISO 13485:2016 by reference and became effective on February 2, 2026. A manufacturer must place this software within its own controlled design, risk-management, verification, validation, release, and postmarket processes. Repository tests and traceability help that work, but do not establish FDA, ISO 13485, HIPAA, GDPR, or other legal compliance.

## License

Licensed under either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT) at your option.
