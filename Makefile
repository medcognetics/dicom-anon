.PHONY: check ci ci-minimum format lint python-quality test coverage docs node-dependencies \
	binding-types test-python test-node test-ci audit deprecations release bench

CI_UV_RUN = uv run --project tools/ci --frozen
ACTIONLINT ?= actionlint

check: format lint test docs binding-types

ci: format lint python-quality docs binding-types test-python test-node test-ci coverage

ci-minimum: test test-python test-node

format:
	cargo fmt --all --check

lint:
	cargo clippy --locked --workspace --all-targets --all-features -- -D warnings

python-quality:
	$(CI_UV_RUN) ruff check tools/ci tests
	$(CI_UV_RUN) ruff format --check tools/ci tests
	$(CI_UV_RUN) basedpyright --project tools/ci

test:
	cargo test --locked --workspace --all-targets --all-features

# profile.rs is generated normative data with exhaustive table-invariant tests.
coverage:
	cargo llvm-cov --locked -p dicom-anon --all-targets --ignore-filename-regex 'profile.rs' --fail-under-lines 90

docs:
	RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps

node-dependencies:
	cd bindings/node && npm ci

binding-types: node-dependencies
	cd bindings/node && npm run typecheck
	$(CI_UV_RUN) basedpyright bindings/python/dicom_anon_python.pyi

test-python:
	$(CI_UV_RUN) maturin develop --manifest-path bindings/python/Cargo.toml
	$(CI_UV_RUN) pytest -q tests/test_python_binding.py

test-node: node-dependencies
	cd bindings/node && npm run build:debug
	node tests/test_node_binding.js

test-ci:
	$(ACTIONLINT) -config-file .github/actionlint.yaml
	PYTHONPATH=. $(CI_UV_RUN) pytest -q tests/test_ci.py tests/test_ci_reporting.py

audit:
	$(CI_UV_RUN) python tools/ci/security_audit.py

deprecations:
	$(CI_UV_RUN) python tools/ci/deprecation_report.py

release:
	cargo build --locked --workspace --release

bench:
	cargo bench --locked -p dicom-anon --bench anonymization -- --sample-size 100
