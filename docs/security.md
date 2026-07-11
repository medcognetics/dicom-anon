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

## Input behavior

The default rejects files already marked Patient Identity Removed=`YES`. This prevents accidental repeated transformation. `--allow-reanonymize` is intended for deliberate re-keying; it does not preserve the pseudonyms from the first pass.

Invalid DA, DT, and TM values fail the operation instead of being copied. Missing patient scope also fails. Atomic file output is created only after parsing and transformation succeed, so an input error does not truncate the destination.

## Release review

Before disclosing output:

1. run `dicom-validate` and retain the JSON report;
2. resolve every error-severity finding;
3. inspect pixel data for burned-in text and recognizable features;
4. confirm that the intended recipient and purpose match the de-identification risk assessment;
5. sample representative SOP Classes and transfer syntaxes with an independent DICOM validator;
6. retain the software version, profile edition, configuration, key identifier, validation record, and release approval in the applicable quality system.

Do not include the key itself in those records. Use a non-secret key identifier or controlled configuration reference.

## Dependency audit

`cargo audit` on July 10, 2026 found no known vulnerabilities. It reported RUSTSEC-2021-0153 because dicom-rs 0.10 depends on the unmaintained `encoding` 0.2.33 crate for DICOM character sets. No maintained replacement is available through the current dicom-rs release. Treat this as a tracked supplier risk, rerun `make audit` with each dependency update, and reassess when dicom-rs changes its text-encoding dependency.
