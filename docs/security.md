# Key management and disclosure safety

## Key requirements

The library requires at least 32 bytes of key material. `SecretKey` hashes that material to a fixed 256-bit key, uses domain-separated keyed BLAKE3 operations for each transformation, redacts `Debug` output, and clears its stored bytes on drop.

Generate keys with an operating-system cryptographic random source. Restrict file permissions and distribute keys through the organization's secret-management process.

```console
head -c 32 /dev/urandom > deidentification.key
chmod 600 deidentification.key
```

Do not pass a key through a command-line argument, commit it, place it in a test fixture, or print it in a report. The CLI accepts a key file or an environment variable. A key file avoids exposing the value to child processes that inherit the environment.

## Linkage and rotation

The same key creates stable linkage across separately processed files. Changing the key changes Patient IDs, UIDs, date offsets, and time offsets. Key rotation therefore creates a new linkage domain and must be planned with downstream datasets.

The transformation is one-way and does not store a lookup table. Losing the key prevents consistent processing of later files, but does not make existing output reversible. Keeping the key permits deterministic matching guesses against low-entropy source identifiers, so key access must remain restricted.

Safe Harbor metadata mode adds two sensitive inputs: the independently assigned patient scope and the original-to-output UID map. Treat both as controlled linkage material. Do not place either in reports, logs, test fixtures, or public configuration repositories. The implementation reports missing, invalid, identity-preserving, colliding, and mismatched mappings without printing their contents. Temporal diagnostics identify only the affected tag and VR, never the source value.

Descriptor maps and safe-private allowlists are disclosure decisions. Review every approved output and private creator/offset against the intended release. An exact configuration match proves that the configured transformation occurred; it does not prove that the approved output is non-identifying.

## Input behavior

The default rejects files already marked Patient Identity Removed=`YES`. This prevents accidental repeated transformation. `--allow-reanonymize` is intended for deliberate re-keying; it does not preserve the pseudonyms from the first pass.

Invalid DA, DT, and TM values fail the operation instead of being copied. Missing patient scope also fails. Atomic file output is created only after parsing and transformation succeed, so an input error does not truncate the destination.

## Release review

Before disclosing output:

1. run `dicom-validate` and retain the JSON report;
2. use `--source` comparison when the original and linkage inputs are available;
3. resolve every error-severity finding;
4. inspect pixel data for burned-in text and recognizable features;
5. inspect filenames, paths, archives, manifests, and linked records outside the DICOM object;
6. confirm that the intended recipient and purpose match the de-identification risk assessment;
7. sample representative SOP Classes and transfer syntaxes with an independent DICOM validator;
8. retain the software version, profile edition, configuration, non-secret key identifier, validation record, and release approval in the applicable quality system.

Do not include the key itself in those records. Use a non-secret key identifier or controlled configuration reference.

A passing `safe-harbor-metadata` report covers only metadata processed under the supplied configuration. Pixel inspection, external mapping provenance and controls, re-identification-code safeguards, and the covered entity's “actual knowledge” determination remain outside the tool. The report does not claim HIPAA compliance.

## Dependency audit

`make audit` runs cargo-audit, npm audit, pip-audit, zizmor, and an action-pin validator. It writes the complete scanner outputs, tool versions, advisory revisions, audited lockfiles, and aggregate status to `target/ci-reports/security/`. Every scanner runs before policy enforcement. Any security finding, warning-class RustSec advisory, scanner error, database error, network error, or unparseable result makes the command fail.

The July 20, 2026 audit found no vulnerability advisories in the Rust, Node, or Python dependency locks. Cargo-audit reported RUSTSEC-2021-0153 because dicom-rs 0.10 depends on the unmaintained `encoding` 0.2.33 crate for DICOM character sets. No maintained replacement is available through the current dicom-rs release. Treat this as a tracked supplier risk and reassess when dicom-rs changes its text-encoding dependency.

Zizmor also reports cache-poisoning findings for cache actions in the pull-request-capable Linux workflow. Pull requests cannot save the Rust or uv caches, and the npm cache step runs only for trusted non-PR events, but these findings remain unsuppressed so the complete evidence is visible. Review them when the workflow structure or zizmor rules change. The Monday security job remains red while either the RustSec advisory or these workflow findings are present.

`make deprecations` separately reports yanked or deprecated direct dependencies, Rust future-incompatibility warnings, and runtime support status to `target/ci-reports/deprecations/`. Findings do not fail that command. Failure to retrieve or interpret a required source does.
