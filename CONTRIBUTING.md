# Contributing

## Setup

Install Rust 1.88 or newer, Node.js with npm, and `uv`. The Node development dependencies are pinned in `bindings/node/package-lock.json`; the Makefile runs a pinned `basedpyright` through `uvx`.

Install the pinned coverage runner before using `make coverage`:

```console
cargo install cargo-llvm-cov --version 0.8.7 --locked
cargo install cargo-audit --version 0.22.1 --locked
rustup component add llvm-tools-preview
```

## Change workflow

1. Add or update a test that states the intended behavior.
2. For a bug, run the focused test and confirm the failure before changing production code.
3. Make the smallest behavior change that satisfies the test.
4. Run `make check`.
5. Run `make coverage` and keep executable-code line coverage above 90%. `profile.rs` is excluded because it is generated normative data with exhaustive table-invariant tests.
6. Run `make audit` and review warnings as well as vulnerabilities.
7. Run `make release` before handoff.
8. Run `make bench` when a hot path or rule-dispatch structure changes.

Changes to profile actions need a DICOM PS3.15 edition and table citation in the pull request description. Update `docs/conformance.md`, the rule count, and representative nested-sequence tests in the same change.

## Test expectations

Core logic needs unit tests. File, stream, atomic replacement, and process behavior need integration tests. Cover failure paths such as invalid DICOM input, invalid dates, missing patient scope, short keys, nested private tags, repeated anonymization, and non-conformant validation output.

Tests must not contain real patient data or production keys. Generate synthetic DICOM objects with `dicom-object`; `core/examples/generate_minimal.rs` is a small reference.

## Pull request notes

State the selected DICOM profile and any option changes. Include the commands run and their results. For performance changes, include before and after confidence intervals and say whether the result exceeded observed noise.
