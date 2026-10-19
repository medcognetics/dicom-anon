# AGENTS.md

## Project purpose

This workspace implements deterministic, configurable DICOM metadata de-identification and validation, plus local plaintext and PDF privacy filtering. Normative DICOM behavior comes from DICOM PS3.15 2026c Table E.1-1 and the selected option columns. The default `dicom-extended` preset enables Clean Descriptors, Retain Patient Characteristics, and Retain Longitudinal Temporal Information With Modified Dates. The `dicom` preset enables no option columns. A separate strict Safe Harbor metadata policy is available. Report filtering uses the pinned OpenAI Privacy Filter model and always requires human review.

## Repository map

- `core/src/anonymize.rs`: de-identification engine and profile marker insertion
- `core/src/validate.rs`: metadata conformance checks
- `core/src/profile.rs`: profile configuration and option resolution
- `core/src/profile_generated.rs`: generated frozen PS3.15 rule matrix and safe-private registry
- `tools/generate_profile.py`: reproducible edition-checked profile generator
- `core/src/pixel.rs`: Burned In Annotation and SOP Class gates and JPEG 2000 comment removal
- `tools/generate_breast_descriptor_map.py`: edition-checked generator for the shipped PS3.16 breast-imaging descriptor map
- `descriptor-maps/`: reviewed, versioned descriptor maps; regenerate rather than edit by hand
- `core/src/pseudonym.rs`: keyed patient and UID transformations
- `core/src/uid_map.rs`: UID resolution with attribute paths, required-UID listing, and random Safe Harbor map generation
- `core/src/temporal.rs`: DICOM DA, DT, and TM shifting with paired-date rollover
- `core/src/io.rs`: stream and atomic file operations
- `core/build.rs`: offline source-commit capture for `--version`
- `core/src/bin/`: command implementations
- `core/tests/cli.rs`: process-level CLI contract tests
- `report/src/model.rs`: pinned ONNX model loading and constrained Viterbi decoding
- `report/src/redact.rs`: deterministic plaintext span replacement
- `report/src/pdf.rs`: PDFium rendering, embedded text geometry, OCR fallback, and image-only output
- `report/src/linkage.rs`: sensitive in-memory DICOM mappings and safe association manifests
- `report/src/install.rs`: checksum-verified model and native-runtime packaging
- `report/src/bin/report-anon.rs`: pipe-safe report CLI
- `report/tests/`: text, model, linkage, PDF, and process-level CLI tests
- `parquet/src/dicom.rs`: one-read lazy metadata extraction and streaming Pixel Data hashing
- `parquet/src/pipeline.rs`: parallel extraction, bounded staging, and conversion orchestration
- `parquet/src/writer.rs`: Arrow batches, Zstandard compression, and atomic Parquet shards
- `parquet/src/bin/dicom-to-parquet.rs`: directory-to-Parquet CLI
- `parquet/tests/`: baseline compatibility, filesystem, Parquet, and process-level CLI tests
- `parquet/benches/pipeline.rs`: one-read comparison and end-to-end Criterion workloads
- `bindings/python/`: PyO3 API and Python stub
- `bindings/node/`: N-API API and TypeScript declaration
- `core/benches/anonymization.rs`: Criterion workloads
- `docs/conformance.md`: supported profile and explicit limitations
- `docs/known-anomalies.md`: per-release known anomaly list for SOUP evaluation
- `CHANGELOG.md`: per-release changes; the release notes copy the tagged section
- `tools/ci/release.py` and `.github/workflows/release.yml`: tag-triggered release assets and draft release

## Required invariants

- Never log or serialize secret key material.
- Require at least 32 bytes of key material.
- Preserve patient association by deriving Patient ID and date shift from the same patient scope and key.
- Preserve UID referential integrity by deriving every replacement from the original UID and key.
- Apply rules recursively to every sequence item.
- For a `D` sequence, dummy every contained value rather than applying ordinary keep/remove rules.
- Remove all odd-group private attributes unless Retain Safe Private is enabled and the exact creator/offset is approved by the frozen registry, an embedded declaration, or the caller registry. Process nested private content independently.
- Replace File Meta Information instead of copying source AE titles or private information.
- Fail closed on invalid dates, missing patient scope, unsupported DICOM input, and already de-identified input unless the caller opts in.
- Treat pixel data as unassessed. Do not claim a pixel-cleaning option without an implemented and tested pixel inspection path.
- Remove JPEG 2000 COM segments only from main and tile-part headers. Copy entropy-coded data byte for byte, and fail closed when a codestream, frame mapping, or offset table cannot be rewritten safely.
- Keep Parquet output metadata-only. Never add Pixel Data to the schema or staged rows.
- When Parquet pixel hashing is disabled, skip Pixel Data through the lazy parser without loading its value. When enabled, stream it through the baseline-compatible XXH3-64 calculation without retaining it.
- Parse each Parquet input DICOM once. Schema discovery may reread the compact unlinked staging file, but must not reopen source DICOMs.
- Reject named files without a `.dcm` or `.dicom` extension before opening them. Check extensionless files for `DICM` at byte offset 128.
- Write the crate version in De-identification Method `(0012,0063)`. A change to any output byte for the same input and configuration requires at least a minor version change; record it in `CHANGELOG.md`. This rule excludes the crate version text in De-identification Method: compare outputs with the version text removed.
- Keep anonymization deterministic. Do not add timestamps, random UIDs, random date offsets, or nondeterministic report ordering. The only exception is opt-in Safe Harbor UID map generation. It draws missing replacements from the operating-system CSPRNG and returns them only in the caller map.
- Keep the patient-scoped time offset stable and apply TM midnight rollover to its known paired DA attribute.
- Never include originals, key material, patient scope, UID-map contents, or identifying mismatch values in validation reports or logs. Missing-mapping errors name only the attribute path.
- In Safe Harbor metadata mode, require an external non-PHI patient scope and a complete collision-free UID map; empty Patient ID, Patient Name, and Birth Date; and retain no temporal detail below year.
- Pin the report model by immutable revision and validate its exact label taxonomy before inference.
- Verify the model graph, external weights, tokenizer, configuration, and calibration against their pinned SHA-256 digests before loading them.
- Bound tokenizer output before allocating inference logits or decoder state.
- Keep CPU and CUDA on the Q4 graph unless semantic fixtures pass on every supported execution provider for a replacement graph.
- Keep report source spans and DICOM source-to-output mappings in memory only. Do not implement `Debug` or `Serialize` for sensitive linkage maps.
- Never include source report text, source identifiers, source hashes, or detector logits in report or association JSON.
- DICOM-linked report output must use the same anonymizer options and reject inputs that resolve to different patient scopes.
- Rebuild sanitized PDFs from rendered pages. Do not copy source text layers, metadata, attachments, forms, or incremental history.
- Fail PDF processing when any non-whitespace character in a protected span lacks paintable coordinates, and bound both per-page and cumulative rendered pixels.
- Serialize PDFium document operations. Concurrent use of the native library can corrupt process state.
- Normalize every report output path. Require `--in-place` when the primary output aliases the input, reject symbolic-link inputs in that mode, and reject destinations that alias another input, mapping, key, installed bundle file, primary output, or sidecar.
- Persist and flush the sanitized primary output before writing report or association sidecars.
- Treat OCR and model detection as fallible. Keep `human_review_required=true` and do not make HIPAA, GDPR, or regulatory compliance claims.
- Verify downloaded report assets before extraction and never overwrite an occupied install target, including a dangling symbolic link.

## Development sequence

For a bug fix, first add a focused regression and confirm that it fails. Apply the smallest fix, rerun that regression, then run the full gates.

Use these commands:

```console
make format
make lint
make test
make coverage
make docs
make binding-types
make test-python
make test-node
make test-ci
make ci
make ci-minimum
make audit
make deprecations
make check
make release
make release-assets RELEASE_TAG=vX.Y.Z
make report-cuda-integration
```

`make ci` is the canonical latest-toolchain gate. `make ci-minimum` checks Rust 1.88, Python 3.10, and Node.js 22 when those runtimes are selected by the caller or CI. `make audit` writes all evidence to `target/ci-reports/security/` and succeeds only when every scanner is `clean` or `warnings`. Keep unmaintained-only RustSec advisories and Low-confidence zizmor findings visible as non-blocking supplier-risk warnings. Vulnerabilities, other findings, malformed output, scanner errors, and incomplete checks must fail. `make deprecations` keeps findings informational and fails only when a check is incomplete.

GitHub Actions uses the persistent single-slot `beryl` runner only for trusted repository events. Keep the same-repository pull-request guard, read-only permissions, non-persisted checkout credentials, full-SHA action pins, and the stable `Linux / Required` aggregate check. Run that check on `beryl` for trusted events and on `ubuntu-24.04` only for fork pull requests, so that the required status does not need GitHub-hosted runner minutes. Do not upload GitHub Actions artifacts; report diagnostics in the job log or step summary. The approved release workflow is the only publishing path. It runs on `beryl` for a version tag or a manual dry-run dispatch, installs its own pinned and checksum-verified Rust, Python, uv, and Node.js into a fresh job directory, does not use the runner tool cache or build caches, gives write permission only to its job, runs `make audit`, and creates a draft GitHub release only for an annotated tag on a `master` commit where `Linux / Required` and Nightly passed. Its release assets are Linux x86_64 `dicom-anonymize` and `dicom-validate` binaries, one abi3 wheel, CycloneDX SBOMs, audit evidence, and `SHA256SUMS`. Do not create or push tags or releases unless the user asks. Keep cross-platform builds, other publishing, and full CPU/PDF/OCR report integration out of the workflows until their runner and artifact contracts are approved. The approved nightly CUDA job runs only after validation and release, requires the `cuda` runner label, and uses the pinned CUDA container and versioned `dicom-anon-report-cuda-assets` volume. GPU preflight, provider dependency or initialization, checksum, semantic parity, and CLI failures must fail the job.

Run Criterion before and after any performance change:

```console
make bench
```

Keep the workload and sample size unchanged when comparing results. Record the baseline, candidate, confidence interval, and decision in `docs/benchmarks.md`.

Full native report integration requires the three `DICOM_ANON_REPORT_*` asset paths. Run `make report-integration-coverage`; it combines the self-contained suite with CPU ONNX Runtime, PDFium, OCR, CLI, and rendered-redaction fixtures and enforces the asset-backed coverage floor. Standard Linux and validation jobs run in an Ubuntu container pinned by immutable manifest digest. Install missing compiler, Tesseract, and Leptonica prerequisites inside that container without mutating the self-hosted runner. The nightly `make report-cuda-integration` target is narrower: it verifies the cached CUDA bundle, compares CPU and CUDA semantic spans, and tests the plaintext CLI without PDF or OCR dependencies. Never delete or replace a failed CUDA cache automatically.

## Profile changes

The rule table is normative data. When changing it:

1. Use the current final DICOM PS3.15 Table E.1-1 as the source.
2. Run the edition-checked `tools/generate_profile.py`; do not hand-edit `profile_generated.rs`.
3. Record the DICOM edition in `PROFILE_VERSION` and `docs/conformance.md`.
4. Apply enabled option columns after the Basic Profile column and reject conflicting overrides.
5. Update rule-count, every-option, safe-private, representative-action, nested-sequence, and validation tests.
6. Run `make check` and `make bench`.
7. Review the conformance statement for changed actions or limitations.

Do not infer an action from a keyword or Value Representation when Table E.1-1 defines the tag. Unknown DA, DT, and TM attributes are shifted as an extension-safety rule. Unknown private attributes are removed.

## Binding changes

Keep bindings byte-oriented. Rust owns DICOM parsing and behavior; Python and Node wrappers must not duplicate policy. Return the same JSON-serialized Rust report model from both bindings. Update `dicom_anon_python.pyi` and `index.d.ts` when a binding signature changes.

## Documentation rules

Document the metadata-only scope next to every conformance claim. Do not state that this repository is ISO 13485, FDA, HIPAA, or GDPR compliant. ISO 13485 and FDA QMSR claims apply to a manufacturer's controlled quality system and records.
