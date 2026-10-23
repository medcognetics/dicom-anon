# Known anomalies

This list records the known anomalies of `dicom-anon` for SOUP evaluation (IEC 62304 7.1.3). Each release reviews and updates it before the tag. The release notes link to the copy of this file at the tag, and they include the dependency audit result from the release build.

The list applies to the version in the heading. It is not a risk assessment. The user of this software must evaluate each anomaly against the intended use and record the result in the user's own quality system.

## Version 0.2.0

Reviewed: 2026-10-19, at `master` commit `d06f0fc` plus the release-tooling change for [#22](https://github.com/medcognetics/dicom-anon/issues/22). Updated 2026-10-23 for the move to GitHub-hosted CI ([#10](https://github.com/medcognetics/dicom-anon/issues/10), [#45](https://github.com/medcognetics/dicom-anon/issues/45)).

Closed before this release: [#18](https://github.com/medcognetics/dicom-anon/issues/18) (pixel-risk gates and JPEG 2000 comment removal; JPEG and JPEG-LS segment removal followed in [#28](https://github.com/medcognetics/dicom-anon/pull/28)), [#19](https://github.com/medcognetics/dicom-anon/issues/19) (Safe Harbor UID listing and generation), [#20](https://github.com/medcognetics/dicom-anon/issues/20) (fail-closed descriptor dummies), and [#21](https://github.com/medcognetics/dicom-anon/issues/21) (mammography and JPEG 2000 fixtures). See the changelog.

### Open defects and gaps

| ID | Anomaly | Effect | Workaround or control |
|---|---|---|---|
| [#10](https://github.com/medcognetics/dicom-anon/issues/10) | The scheduled nightly job failed or did not complete on the self-hosted `beryl` runner, and once the repository became public, beryl no longer accepted its jobs. Nightly validation now runs on GitHub-hosted Linux. | Until a hosted Nightly run passes on the release commit, the release precondition "nightly validation passes on the tagged commit" is not met. This is a CI defect, not a product defect. | Run the nightly workflow on the release commit before the tag. |
| [#45](https://github.com/medcognetics/dicom-anon/issues/45) | GitHub Actions no longer runs the CUDA report job (CPU and CUDA semantic parity, CUDA CLI), because the public repository uses only GitHub-hosted runners. | CUDA execution-provider behavior is not verified automatically. The removed job was also the only automated run of the pinned model on CPU, for the parity comparison. CI now runs only the self-contained report suite; asset-backed CPU model, PDF, and OCR integration stays the local `make report-integration-coverage` gate. | Before relying on `report-anon` model output, run `make report-integration-coverage`, and before relying on `--device cuda`, run `make report-cuda-integration` on a controlled GPU host. Keep the results. |
| [#14](https://github.com/medcognetics/dicom-anon/issues/14) | The scheduled security audit fails. The 2026-10-12 run reported RUSTSEC-2026-0285 (`rustls` 0.23.42, a dependency of the `report-anon model install` downloader), npm, pip, and workflow findings. | The release workflow fails while any audit finding is blocking. | Resolve the findings (open pull request [#15](https://github.com/medcognetics/dicom-anon/pull/15) addresses part of them), then rerun the audit. |

### Supplier risks (unmaintained dependencies)

| Advisory | Crate | Reason it is present | Status |
|---|---|---|---|
| [RUSTSEC-2021-0153](https://rustsec.org/advisories/RUSTSEC-2021-0153.html) | `encoding` 0.2.33 (unmaintained) | `dicom-rs` 0.10 uses it to decode DICOM Specific Character Sets. | No vulnerability is reported. A replacement needs an upstream change or a behavior-sensitive fork. Reassess when `dicom-rs` changes. |
| [RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436.html) | `paste` 1.0.15 (unmaintained) | Transitive dependency of the Parquet and tokenizer crates. It is a compile-time macro crate. | No vulnerability is reported. Reassess when `parquet` or `tokenizers` changes. |

`make audit` reports both advisories as non-blocking warnings. See [Key management and disclosure safety](security.md#dependency-audit).

### Scope limits

These limits are design boundaries, not defects. [Conformance and scope](conformance.md) is the normative statement.

- The tool processes DICOM metadata. It does not decode or inspect pixel samples for burned-in text or faces, and it does not claim the Clean Pixel Data or Clean Recognizable Visual Features options. The optional Burned In Annotation and SOP Class gates and the removal of JPEG 2000 comment segments and JPEG and JPEG-LS comment and application segments reduce, but do not remove, this risk. Text in other encapsulated transfer syntaxes is not removed. Burned-in text stays a caller risk, also when Burned In Annotation is `NO`.
- A passing `dicom-validate` report is a confidentiality-policy result. It is not a complete SOP Class or IOD validation, and it does not show that Type 1 and Type 2 requirements are satisfied.
- In `safe-harbor-metadata` mode, the caller is responsible for the patient scope, the UID map, descriptor outputs, and safe-private entries, and for whether they contain or encode PHI.
- The covered entity keeps the Safe Harbor re-identification-code controls and the "actual knowledge" determination.
- Filenames, paths, archives, manifests, logs, and linked databases are outside the tool.
- Cross-file consistency is a caller-level batch check.
- `report-anon` detection is fallible. Its output always has `human_review_required=true`.
- Release binaries and the wheel are for Linux x86_64 with glibc. They are built in an Ubuntu 24.04 container (glibc 2.39). The wheel platform tag states the minimum glibc version that maturin found. Test the binaries on the target system. Other platforms must build from source.
- The software makes no HIPAA, GDPR, FDA, or ISO 13485 compliance claim.
