# Known anomalies

This list records the known anomalies of `dicom-anon` for SOUP evaluation (IEC 62304 7.1.3). Each release reviews and updates it before the tag. The release notes link to the copy of this file at the tag, and they include the dependency audit result from the release build.

The list applies to the version in the heading. It is not a risk assessment. The user of this software must evaluate each anomaly against the intended use and record the result in the user's own quality system.

## Version 0.2.0

Reviewed: 2026-10-17, at commit `7dda93a` plus the release-tooling change for [#22](https://github.com/medcognetics/dicom-anon/issues/22).

### Open defects and gaps

| ID | Anomaly | Effect | Workaround or control |
|---|---|---|---|
| [#18](https://github.com/medcognetics/dicom-anon/issues/18) | Pixel Data has no fail-closed gate. The anonymizer copies Pixel Data in every policy. It does not refuse Burned In Annotation `(0028,0301)`=`YES` or a missing value, does not restrict SOP Classes, and copies JPEG 2000 comment (COM) marker segments unchanged. | Burned-in text or codestream comments can contain identifiers that remain in output marked Patient Identity Removed=`YES`. `dicom-validate` gives only the warning `pixel_data_not_assessed`. | Inspect pixel data and encapsulated codestreams before disclosure. Restrict input to SOP Classes and transfer syntaxes that the user has validated. |
| [#19](https://github.com/medcognetics/dicom-anon/issues/19) | In `safe-harbor-metadata` mode, the library cannot list the source UIDs that need a map entry, cannot generate a random UID map, and does not identify the tag or path of a missing UID. | A caller that processes studies as they arrive must write its own recursive UID walk. A difference from the library rules fails at run time without a location. | Build the complete UID map before processing and test it on representative objects. |
| [#20](https://github.com/medcognetics/dicom-anon/issues/20) | In `safe-harbor-metadata` mode, coded entries (View Code Sequence, View Modifier Code Sequence, Anatomic Region Sequence) and Manufacturer and Manufacturer's Model Name become `ANONYMIZED` unless an exact descriptor map entry keeps them. The replacement is silent. | Downstream interpretation or AI inference that reads view codes or vendor values gets wrong input without an error. | Supply an exact `--descriptor-map` for each required code and vendor value, and validate with `--source`. |
| [#21](https://github.com/medcognetics/dicom-anon/issues/21) | The automated tests do not use mammography, breast tomosynthesis, synthesized 2D, or JPEG 2000 objects. | Behavior on these SOP Classes and transfer syntaxes has no automated regression protection. | Verify representative objects of each type in the user's own validation. |
| [#10](https://github.com/medcognetics/dicom-anon/issues/10) | The scheduled nightly job fails or does not complete on the `beryl` runner. Recent runs failed with "no space left on device" or were cancelled while the CUDA job waited for a runner. | The release precondition "nightly validation passes on the tagged commit" is not met automatically. This is a CI defect, not a product defect. | Repair runner capacity, then run the nightly workflow on the release commit before the tag. |
| [#14](https://github.com/medcognetics/dicom-anon/issues/14) | The scheduled security audit fails. The 2026-10-12 run reported RUSTSEC-2026-0285 (`rustls` 0.23.42, a dependency of the `report-anon model install` downloader), npm, pip, and workflow findings. | The release workflow fails while any audit finding is blocking. | Resolve the findings (open pull request [#15](https://github.com/medcognetics/dicom-anon/pull/15) addresses part of them), then rerun the audit. |

### Supplier risks (unmaintained dependencies)

| Advisory | Crate | Reason it is present | Status |
|---|---|---|---|
| [RUSTSEC-2021-0153](https://rustsec.org/advisories/RUSTSEC-2021-0153.html) | `encoding` 0.2.33 (unmaintained) | `dicom-rs` 0.10 uses it to decode DICOM Specific Character Sets. | No vulnerability is reported. A replacement needs an upstream change or a behavior-sensitive fork. Reassess when `dicom-rs` changes. |
| [RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436.html) | `paste` 1.0.15 (unmaintained) | Transitive dependency of the Parquet and tokenizer crates. It is a compile-time macro crate. | No vulnerability is reported. Reassess when `parquet` or `tokenizers` changes. |

`make audit` reports both advisories as non-blocking warnings. See [Key management and disclosure safety](security.md#dependency-audit).

### Scope limits

These limits are design boundaries, not defects. [Conformance and scope](conformance.md) is the normative statement.

- The tool processes DICOM metadata only. It does not decode or inspect Pixel Data, and it does not claim the Clean Pixel Data or Clean Recognizable Visual Features options.
- A passing `dicom-validate` report is a confidentiality-policy result. It is not a complete SOP Class or IOD validation, and it does not show that Type 1 and Type 2 requirements are satisfied.
- In `safe-harbor-metadata` mode, the caller is responsible for the patient scope, the UID map, descriptor outputs, and safe-private entries, and for whether they contain or encode PHI.
- The covered entity keeps the Safe Harbor re-identification-code controls and the "actual knowledge" determination.
- Filenames, paths, archives, manifests, logs, and linked databases are outside the tool.
- Cross-file consistency is a caller-level batch check.
- `report-anon` detection is fallible. Its output always has `human_review_required=true`.
- Release binaries and the wheel are for Linux x86_64 with glibc. They are built in an Ubuntu 24.04 container (glibc 2.39). The wheel platform tag states the minimum glibc version that maturin found. Test the binaries on the target system. Other platforms must build from source.
- The software makes no HIPAA, GDPR, FDA, or ISO 13485 compliance claim.
