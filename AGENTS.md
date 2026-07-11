# AGENTS.md

## Project purpose

This workspace implements deterministic DICOM metadata de-identification and validation. The normative behavior comes from DICOM PS3.15 2026c Table E.1-1 with the Retain Longitudinal Temporal Information With Modified Dates option.

## Repository map

- `core/src/anonymize.rs`: de-identification engine and profile marker insertion
- `core/src/validate.rs`: metadata conformance checks
- `core/src/profile.rs`: frozen PS3.15 rule table and grouped lookup
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
- Remove all odd-group private attributes, including private creators and nested private attributes.
- Replace File Meta Information instead of copying source AE titles or private information.
- Fail closed on invalid dates, missing patient scope, unsupported DICOM input, and already de-identified input unless the caller opts in.
- Treat pixel data as unassessed. Do not claim a pixel-cleaning option without an implemented and tested pixel inspection path.
- Keep anonymization deterministic. Do not add timestamps, random UIDs, random date offsets, or nondeterministic report ordering.
- Keep the patient-scoped time offset stable and apply TM midnight rollover to its known paired DA attribute.

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
make audit
make check
make release
```

Run Criterion before and after any performance change:

```console
make bench
```

Keep the workload and sample size unchanged when comparing results. Record the baseline, candidate, confidence interval, and decision in `docs/benchmarks.md`.

## Profile changes

The rule table is normative data. When changing it:

1. Use the current final DICOM PS3.15 Table E.1-1 as the source.
2. Record the DICOM edition in `PROFILE_VERSION` and `docs/conformance.md`.
3. Apply option columns after the Basic Profile column.
4. Update the rule-count, representative-action, nested-sequence, and validation tests.
5. Run `make check` and `make bench`.
6. Review the conformance statement for changed actions or limitations.

Do not infer an action from a keyword or Value Representation when Table E.1-1 defines the tag. Unknown DA, DT, and TM attributes are shifted as an extension-safety rule. Unknown private attributes are removed.

## Binding changes

Keep bindings byte-oriented. Rust owns DICOM parsing and behavior; Python and Node wrappers must not duplicate policy. Return the same JSON-serialized Rust report model from both bindings. Update `dicom_anon_python.pyi` and `index.d.ts` when a binding signature changes.

## Documentation rules

Document the metadata-only scope next to every conformance claim. Do not state that this repository is ISO 13485, FDA, HIPAA, or GDPR compliant. ISO 13485 and FDA QMSR claims apply to a manufacturer's controlled quality system and records.
