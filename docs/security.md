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

Safe Harbor metadata mode adds two sensitive inputs: the independently assigned patient scope and the original-to-output UID map. Treat both as controlled linkage material. Do not place either in reports, logs, test fixtures, or public configuration repositories. The implementation reports missing, invalid, identity-preserving, colliding, and mismatched mappings without printing their contents. A missing-mapping error identifies only the attribute path, for example `(0020,9221)[0]/(0020,9164)`, and never the UID value. Temporal diagnostics identify only the affected tag and VR, never the source value.

### Generated UID maps

`--generate-uid-map` and `anonymize_with_generated_uid_map` assign each missing Safe Harbor replacement from the operating-system CSPRNG. Each replacement is a random version 4 UUID integer under the UID root. It does not depend on the source UID, the key, the patient scope, or other patient data, so it meets the intent of a re-identification code that is not derived from information about the individual. Each replacement differs from every UID in the source object and its File Meta Information, and from every source and replacement in the starting map. Generation fails, with the attribute path only, when a source UID that needs a replacement is empty or is not a valid DICOM UID. It also fails when a starting replacement equals a UID in the source object.

The generated map is the only link from output UIDs to source UIDs. It is a re-identification mechanism. Apply these custody rules:

1. Keep the map only with the party that is permitted to re-identify. Do not send it with the de-identified output or to its recipient.
2. Do not write the map, or the output of `--list-required-uids`, to logs, reports, tickets, or source control. Both contain source UIDs.
3. Store the map with access control and encryption at rest. The command writes the map file atomically with owner-only permissions (`0600`) on Unix. It replaces an existing file at that path only when that file is also the `--uid-map` input. When the path is a symbolic link, the command replaces the link target and keeps the link. It rejects a dangling link.
4. Use one map for all files of one study or other linkage domain. Pass the current map as `--uid-map` and as the `--generate-uid-map` destination for each file, or keep the returned map and pass it in the next configuration. Serialize updates to one map. Concurrent writers can lose entries.
5. The command writes and syncs the map, and on Unix its directory entry, before the DICOM output. If the file system cannot sync the directory, the command fails before it writes the DICOM output. If the output write fails, the map can contain entries that no output uses. These entries do not disclose output UIDs.
6. Lost map entries cannot be recovered from the key. Later files then receive different UIDs, which breaks UID referential integrity across the study. Back up the map under the same controls.
7. Destroy the map when the retention period for re-identification ends.

The anonymization report, error text, and DICOM output never contain map contents or source UIDs. `--verbose` prints only the report.

Descriptor maps and safe-private allowlists are disclosure decisions. Review every approved output and private creator/offset against the intended release. An exact configuration match proves that the configured transformation occurred; it does not prove that the approved output is non-identifying. The shipped breast-imaging map contains only standard DICOM PS3.16 terminology. Review any map that adds site values, such as Manufacturer, before release. The `--fail-on-unmapped` error gives only the tag and sequence path, never the source value.

## Input behavior

The default rejects files already marked Patient Identity Removed=`YES`. This prevents accidental repeated transformation. `--allow-reanonymize` is intended for deliberate re-keying; it does not preserve the pseudonyms from the first pass.

For pixel risk, enable `--require-no-burned-in-annotation` and `--allow-sop-class` for each permitted SOP Class when the release depends on those declarations. A failing input exits with status 2 and writes no output. JPEG 2000 comment segments and JPEG and JPEG-LS comment and application segments are always removed. A JPEG 2000, JPEG, or JPEG-LS codestream that cannot be rewritten safely fails. The removal copies the entropy-coded data byte for byte and does not change the decoded samples. These controls use declared attributes and codestream structure. They do not inspect the pixel samples. The [conformance statement](conformance.md#pixel-and-disclosure-limits) lists the risks that stay with the caller.

Invalid DA, DT, and TM values fail the operation instead of being copied. A wholly zero-length value is not invalid: it stays zero length where the profile keeps, shifts, or generalizes it. A dummy (`D`) action still writes a dummy value, and a remove (`X`) action still removes it. An empty component inside a multi-valued value that the profile shifts or generalizes still fails. Missing patient scope also fails. Atomic file output is created only after parsing and transformation succeed, so an input error does not truncate the destination.

## Release review

Before disclosing output:

1. run `dicom-validate` and retain the JSON report;
2. use `--source` comparison when the original and linkage inputs are available;
3. resolve every error-severity finding;
4. inspect pixel data for burned-in text and recognizable features, also when the pixel-risk gates passed, unless the approved method record states why that review is unnecessary;
5. inspect filenames, paths, archives, manifests, and linked records outside the DICOM object;
6. confirm that the intended recipient and purpose match the de-identification risk assessment;
7. sample representative SOP Classes and transfer syntaxes with an independent DICOM validator;
8. retain the software version and commit from `--version` (De-identification Method also records the version and edition in each output), the profile edition, configuration, non-secret key identifier, validation record, and release approval in the applicable quality system.

Do not include the key itself in those records. Use a non-secret key identifier or controlled configuration reference.

A passing `safe-harbor-metadata` report covers only metadata processed under the supplied configuration. Pixel inspection beyond the declared-attribute gates and codestream comment and application segment removal, external mapping provenance and controls, re-identification-code safeguards, and the covered entity's “actual knowledge” determination remain outside the tool. The report does not claim HIPAA compliance.

## Dependency audit

`make audit` runs cargo-audit, npm audit, pip-audit, zizmor, and an action-pin validator. It writes the complete scanner outputs, tool versions, advisory revisions, audited lockfiles, aggregate status, and enforcement policy to `target/ci-reports/security/`. Every scanner runs before policy enforcement. The command succeeds when every scanner is `clean` or `warnings`. Cargo-audit results containing only unmaintained advisories and zizmor results containing only Low-confidence findings receive the `warnings` status. Vulnerabilities, other cargo-audit warning classes, Medium- or High-confidence zizmor findings, npm or pip findings, unpinned actions, scanner errors, database or network errors, and unparseable or structurally invalid output remain blocking. The command prints a summary with each scanner status, the advisory or rule identifiers of each finding, and the cause of each incomplete scan. For each incomplete scan, the summary also shows the last 20 lines of scanner standard error, with a maximum of 2,048 characters. The summary replaces the `GH_TOKEN` and `GITHUB_TOKEN` values with `***`. The scheduled workflow shows this summary in the job log and step summary and does not upload report files.

The August 3, 2026 audit found no vulnerability advisories in the Rust, Node, or Python dependency locks. Cargo-audit reports [RUSTSEC-2021-0153](https://rustsec.org/advisories/RUSTSEC-2021-0153.html) because dicom-rs 0.10 depends on the unmaintained `encoding` 0.2.33 crate for DICOM character sets. It also reports [RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436.html) because transitive Parquet and tokenizer dependencies use the unmaintained `paste` 1.0.15 crate. Replacing either dependency requires behavior-sensitive upstream or local forks, so both remain tracked supplier risks. Keep them visible in every report and reassess them when their parent crates change.

The CI tool lock pins zizmor 1.29.0. This replaces 1.27.0, whose configured GitHub credentials could be written to logs; the defect was fixed in [zizmor 1.28.0](https://github.com/zizmorcore/zizmor/releases/tag/v1.28.0). Zizmor 1.29.0 still reports Low-confidence cache-poisoning findings for cache actions in the pull-request-capable Linux workflow. Pull requests cannot save the Rust or uv caches, and the npm cache step runs only for trusted non-PR events. These findings remain unsuppressed and visible as non-blocking warnings. Review them when the workflow structure or zizmor rules change.

`make deprecations` separately reports yanked or deprecated direct dependencies, Rust future-incompatibility warnings, and runtime support status to `target/ci-reports/deprecations/`. Findings do not fail that command. Failure to retrieve or interpret a required source does. The command prints its findings and incomplete checks, including affected Rust packages and the first Cargo error. The scheduled workflow shows them in the job log and step summary. The scheduled all-features Rust check installs Clang, LibClang, Leptonica, Tesseract, and `pkg-config`, then verifies the `lept` and `tesseract` package metadata before generating the report.
