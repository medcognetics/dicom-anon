# Contributing

## Setup

Install Rust 1.88 or newer, Python 3.10 or newer, Node.js 22 or newer with npm, and uv 0.11.29. Python CI tools are pinned in `tools/ci/uv.lock`; Node development dependencies are pinned in `bindings/node/package-lock.json`.

Install the pinned Rust and workflow tools before using the full gate:

```console
cargo install cargo-llvm-cov --version 0.8.7 --locked
cargo install cargo-audit --version 0.22.2 --locked
cargo install cargo-cyclonedx --version 0.5.9 --locked
go install github.com/rhysd/actionlint/cmd/actionlint@v1.7.12
rustup component add llvm-tools-preview
uv sync --project tools/ci --frozen
```

## Change workflow

1. Add or update a test that states the intended behavior.
2. For a bug, run the focused test and confirm the failure before changing production code.
3. Make the smallest behavior change that satisfies the test.
4. Run `make ci`. This runs formatting, linting, documentation, binding type and runtime checks, CI policy tests, and coverage. Keep executable-code line coverage above 90% in the core and Parquet crates; `profile.rs` is excluded because it is generated normative data with exhaustive table-invariant tests.
5. Run `make audit` and inspect every report under `target/ci-reports/security/`. Unmaintained-only RustSec advisories and Low-confidence zizmor findings remain visible as non-blocking warnings. Vulnerabilities, other findings, malformed output, scanner errors, and incomplete checks fail.
6. Run `make deprecations`. Findings are informational, but incomplete checks fail.
7. Run `make release` before handoff.
8. Run `make bench` when a hot path, rule-dispatch structure, DICOM extraction path, or Parquet writer changes.

Changes to profile actions need a DICOM PS3.15 edition and table citation in the pull request description. Regenerate the frozen Table E.1-1 matrix and safe-private registry with:

```console
uv run --with pandas==2.3.3 --with lxml==6.0.2 tools/generate_profile.py
```

The generator refuses to run against an edition other than 2026c. Review the generated diff, update `docs/conformance.md`, and update row-count, option-column, safe-private, repeating-group, and representative nested-sequence tests in the same change.

## Versioning

Versions follow [Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html). The workspace crates, the Python package, and the Node.js package always have the same version.

A change to any output byte for the same input and configuration is a minor version change at least. This includes DICOM output, De-identification Method `(0012,0063)` text, validation and anonymization reports, report-filter output and sidecars, and Parquet output. A change that removes or renames a public API, CLI argument, configuration field, or report field, or that makes a previously accepted input fail, is a major version change. While the major version is 0, such a change is a minor version change. Only a change that keeps every output byte and every public interface is a patch version change.

De-identification Method contains the crate version, so every version change also changes that value. The rule above excludes this version text: compare outputs with the version text removed. `dicom-validate` accepts only the value of its own version.

Add each user-visible change to the `Unreleased` section of [CHANGELOG.md](CHANGELOG.md) in the same pull request. Update [docs/known-anomalies.md](docs/known-anomalies.md) when a change opens, closes, or changes a known anomaly.

## Release procedure

The release workflow, `.github/workflows/release.yml`, runs on beryl for a `vMAJOR.MINOR.PATCH` tag. It does not run for pull requests. Prerelease tags are not supported, because Python and Cargo spell prerelease versions differently.

1. In a pull request, set the version in `Cargo.toml`, `bindings/python/pyproject.toml`, `bindings/node/package.json`, and `bindings/node/package-lock.json`. Run `cargo update --workspace` and `make test-node`, which regenerates `bindings/node/index.js`.
2. In the same pull request, replace `Unreleased` in the version heading of `CHANGELOG.md` with the release date, and review `docs/known-anomalies.md`.
3. Merge the pull request after `Linux / Required` passes. Dispatch the Nightly workflow on `master` and confirm that it passes on the merge commit.
4. Optionally, dispatch the Release workflow on `master`. A dispatch builds and checks every asset and writes the notes to the job summary, but it does not create a release.
5. Create an annotated or signed tag on the merge commit and push it, for example `git tag --annotate v0.2.0 --message "dicom-anon 0.2.0"`.
6. The workflow requires an annotated tag on a `master` commit where `Linux / Required` and Nightly passed. It runs `make audit`, builds the assets from the tag, verifies `--version` and the checksums, and creates a draft GitHub release.
7. Review the draft release and its assets, then publish it.

The workflow installs its own pinned Rust, Python, uv, and Node.js into a fresh job directory. It verifies each download with a SHA-256 value pinned in the workflow, or, for Python, with the checksum built into uv. It does not read or write the runner tool cache or any build cache, so pull-request jobs on beryl cannot change the tools that build the released files. To build the same assets locally on Linux x86_64, install cargo-audit 0.22.2 and cargo-cyclonedx 0.5.9, then run `make audit` and `make release-assets RELEASE_TAG=v0.2.0`. The assets are written to `target/release-assets/`.

## Test expectations

Core logic needs unit tests. File, stream, atomic replacement, and process behavior need integration tests. Cover failure paths such as invalid DICOM input, invalid dates, missing patient scope, short keys, nested private tags, repeated anonymization, non-conformant validation output, extensionless discovery, shard collisions, and partial best-effort conversion.

Tests must not contain real patient data or production keys. Generate synthetic DICOM objects with `dicom-object`; `core/examples/generate_minimal.rs` is a small reference.

On a CUDA 12 host with the pinned model and GPU ONNX Runtime installed, set `DICOM_ANON_REPORT_MODEL_DIR` and `DICOM_ANON_REPORT_CUDA_ORT_LIBRARY`, then run:

```console
make report-cuda-integration
```

This target verifies the installed asset manifest, compares CPU and CUDA spans from the same Q4 graph, and exercises the plaintext CLI with `--device cuda`. It does not run the PDF or OCR fixtures.

## Pull request notes

State the selected DICOM profile and any option changes. Include the commands run and their results. For performance changes, include before and after confidence intervals and say whether the result exceeded observed noise.
