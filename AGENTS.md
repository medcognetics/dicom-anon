# AGENTS.md

## Project purpose

This workspace implements deterministic, configurable DICOM metadata de-identification and validation. Normative behavior comes from DICOM PS3.15 2026c Table E.1-1 and the selected option columns. The default `dicom-extended` preset enables Clean Descriptors, Retain Patient Characteristics, and Retain Longitudinal Temporal Information With Modified Dates. The `dicom` preset enables no option columns. A separate strict Safe Harbor metadata policy is available.

## Repository map

- `core/src/anonymize.rs`: de-identification engine and profile marker insertion
- `core/src/validate.rs`: metadata conformance checks
- `core/src/profile.rs`: profile configuration and option resolution
- `core/src/profile_generated.rs`: generated frozen PS3.15 rule matrix and safe-private registry
- `tools/generate_profile.py`: reproducible edition-checked profile generator
- `core/src/pseudonym.rs`: keyed patient and UID transformations
- `core/src/temporal.rs`: DICOM DA, DT, and TM shifting with paired-date rollover
- `core/src/io.rs`: stream and atomic file operations
- `core/src/bin/`: command implementations
- `core/tests/cli.rs`: process-level CLI contract tests
- `bindings/python/`: PyO3 API and Python stub
- `bindings/node/`: N-API API and TypeScript declaration
- `core/benches/anonymization.rs`: Criterion workloads
- `docs/conformance.md`: supported profile and explicit limitations

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
- Keep anonymization deterministic. Do not add timestamps, random UIDs, random date offsets, or nondeterministic report ordering.
- Keep the patient-scoped time offset stable and apply TM midnight rollover to its known paired DA attribute.
- Never include originals, key material, patient scope, UID-map contents, or identifying mismatch values in validation reports or logs.
- In Safe Harbor metadata mode, require an external non-PHI patient scope and a complete collision-free UID map; empty Patient ID, Patient Name, and Birth Date; and retain no temporal detail below year.

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
```

`make ci` is the canonical latest-toolchain gate. `make ci-minimum` checks Rust 1.88, Python 3.10, and Node.js 22 when those runtimes are selected by the caller or CI. `make audit` deliberately returns nonzero for every security finding after writing all reports to `target/ci-reports/security/`; do not suppress the known RustSec warning to make the gate green. `make deprecations` keeps findings informational and fails only when a check is incomplete.

GitHub Actions uses the persistent single-slot `beryl` runner only for trusted repository events. Keep the same-repository pull-request guard, read-only permissions, non-persisted checkout credentials, full-SHA action pins, and the stable `Linux / Required` aggregate check. Keep cross-platform, CUDA, publishing, and report-integration work out of these workflows until their runner and artifact contracts are approved.

Run Criterion before and after any performance change:

```console
make bench
```

Keep the workload and sample size unchanged when comparing results. Record the baseline, candidate, confidence interval, and decision in `docs/benchmarks.md`.

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
