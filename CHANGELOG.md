# Changelog

This file records each released version of `dicom-anon`. The format follows [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/). Versions follow [Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html) and the output-byte rule in [CONTRIBUTING.md](CONTRIBUTING.md#versioning). Known anomalies for each release are in [docs/known-anomalies.md](docs/known-anomalies.md).

The release workflow copies the section of the tagged version into the GitHub release notes. Before you tag a release, replace `Unreleased` in that section heading with the release date in `YYYY-MM-DD` format. The workflow refuses a heading without a date.

## [0.2.0] - Unreleased

This is the first tagged release. Version 0.1.0 was not tagged or released.

### Added

- DICOM PS3.15 2026c Basic Application Level Confidentiality Profile with every metadata option column, the default `dicom-extended` preset, the `dicom` preset, and the strict `safe-harbor-metadata` policy.
- `dicom-anonymize` and `dicom-validate` commands, with candidate-only and source-aware validation.
- `report-anon` for local plaintext and PDF privacy filtering with the pinned OpenAI Privacy Filter model. Its output always requires human review.
- `dicom-to-parquet` for one-read, metadata-only Parquet export.
- Python (PyO3, abi3) and Node.js (N-API) bindings.
- `--version` for `dicom-anonymize`, `dicom-validate`, and `report-anon` prints the crate version and the source commit that the build recorded.
- A tag-triggered release workflow. It builds Linux x86_64 `dicom-anonymize` and `dicom-validate` binaries, an abi3 Python wheel, CycloneDX SBOMs of the Rust lock and of the Python and Node build-tool locks, SHA-256 checksums, and the dependency audit evidence. It attaches them to a draft GitHub release.
- This changelog and the [known anomaly list](docs/known-anomalies.md).
- Safe Harbor UID support ([#19](https://github.com/medcognetics/dicom-anon/issues/19)): `--list-required-uids` and the matching library and binding calls list each source UID that needs a map entry, with its sequence path. `--generate-uid-map PATH` assigns missing replacements from the operating-system CSPRNG and returns them in the caller map. A missing mapping error gives the tag path and never the value.
- Fail-closed descriptor option ([#20](https://github.com/medcognetics/dicom-anon/issues/20)): `--fail-on-unmapped TAG` stops when a descriptor-map miss would write a dummy value for that tag. Reports count dummy and unmapped dummy replacements by tag.
- Pixel-risk gates ([#18](https://github.com/medcognetics/dicom-anon/issues/18)): `--require-no-burned-in-annotation` and `--allow-sop-class UID` reject risky input. The anonymizer removes JPEG 2000 comment (COM) segments and fails closed on codestreams it cannot parse. `dicom-validate` reports `jpeg2000_comment_present` and `jpeg2000_codestream_unparseable`.
- JPEG and JPEG-LS segment removal ([#28](https://github.com/medcognetics/dicom-anon/pull/28)): the anonymizer removes COM and APPn segments from JPEG (`1.2.840.10008.1.2.4.50`, `.51`, `.57`, `.70`) and JPEG-LS (`.80`, `.81`) codestreams. It keeps one APP14 `Adobe` segment only where it changes decoded colors, and fails closed on codestreams it cannot parse or clean safely. `dicom-validate` reports `jpeg_comment_present`, `jpeg_application_segment_present`, and `jpeg_codestream_unparseable`.
- `dicom-validate` reports `encapsulated_total_length_mismatch` when Encapsulated Pixel Data Value Total Length `(7FE0,0003)` does not match the fragments ([#31](https://github.com/medcognetics/dicom-anon/pull/31)).
- Mammography, breast tomosynthesis, synthesized 2D, and JPEG 2000 conformance fixtures in CI ([#21](https://github.com/medcognetics/dicom-anon/issues/21)).
- The mammography suite also runs `safe-harbor-metadata` with the shipped breast-imaging descriptor map and the fail-on-unmapped guard for the code tags, through the library and the CLI. With the map, View Code Sequence must be unchanged. Without a map, Safe Harbor mode still replaces it with `ANONYMIZED` ([#32](https://github.com/medcognetics/dicom-anon/issues/32)).

### Changed

- De-identification Method `(0012,0063)` now includes the crate version. The last value is `dicom-anon <version> <edition>`, or `dicom-anon <version> safe-harbor-metadata <edition>` for the Safe Harbor policy. This changes output bytes. `dicom-validate` accepts only the value that its own version writes and reports another version as `deidentification_method_version_mismatch`. Validate an output with the same version that made it.
- JPEG 2000, JPEG, and JPEG-LS Pixel Data that contains comment or application segments changes bytes: the segments are removed, and scan data and decoded samples do not change.
- The anonymizer uses less peak memory when it rewrites encapsulated Pixel Data ([#31](https://github.com/medcognetics/dicom-anon/pull/31)).
- The Cargo `repository` field and the README badge identify `medcognetics/dicom-anon`.

[0.2.0]: https://github.com/medcognetics/dicom-anon/releases/tag/v0.2.0
