# Contributing

## Setup

Install Rust 1.88 or newer, Python 3.10 or newer, Node.js 22 or newer with npm, and uv 0.11.29. Python CI tools are pinned in `tools/ci/uv.lock`; Node development dependencies are pinned in `bindings/node/package-lock.json`.

Install the pinned Rust and workflow tools before using the full gate:

```console
cargo install cargo-llvm-cov --version 0.8.7 --locked
cargo install cargo-audit --version 0.22.2 --locked
go install github.com/rhysd/actionlint/cmd/actionlint@v1.7.12
rustup component add llvm-tools-preview
uv sync --project tools/ci --frozen
```

## Change workflow

1. Add or update a test that states the intended behavior.
2. For a bug, run the focused test and confirm the failure before changing production code.
3. Make the smallest behavior change that satisfies the test.
4. Run `make ci`. This runs formatting, linting, documentation, binding type and runtime checks, CI policy tests, and coverage. Keep executable-code line coverage above 90%; `profile.rs` is excluded because it is generated normative data with exhaustive table-invariant tests.
5. Run `make audit` and inspect every report under `target/ci-reports/security/`. This command intentionally fails for any finding or incomplete scan, including informational RustSec advisories.
6. Run `make deprecations`. Findings are informational, but incomplete checks fail.
7. Run `make release` before handoff.
8. Run `make bench` when a hot path or rule-dispatch structure changes.

Changes to profile actions need a DICOM PS3.15 edition and table citation in the pull request description. Regenerate the frozen Table E.1-1 matrix and safe-private registry with:

```console
uv run --with pandas==2.3.3 --with lxml==6.0.2 tools/generate_profile.py
```

The generator refuses to run against an edition other than 2026c. Review the generated diff, update `docs/conformance.md`, and update row-count, option-column, safe-private, repeating-group, and representative nested-sequence tests in the same change.

## Test expectations

Core logic needs unit tests. File, stream, atomic replacement, and process behavior need integration tests. Cover failure paths such as invalid DICOM input, invalid dates, missing patient scope, short keys, nested private tags, repeated anonymization, and non-conformant validation output.

Tests must not contain real patient data or production keys. Generate synthetic DICOM objects with `dicom-object`; `core/examples/generate_minimal.rs` is a small reference.

On a CUDA 12 host with the pinned model and GPU ONNX Runtime installed, set `DICOM_ANON_REPORT_MODEL_DIR` and `DICOM_ANON_REPORT_CUDA_ORT_LIBRARY`, then run:

```console
make report-cuda-integration
```

This target verifies the installed asset manifest, compares CPU and CUDA spans from the same Q4 graph, and exercises the plaintext CLI with `--device cuda`. It does not run the PDF or OCR fixtures.

## Pull request notes

State the selected DICOM profile and any option changes. Include the commands run and their results. For performance changes, include before and after confidence intervals and say whether the result exceeded observed noise.
