.PHONY: check ci ci-minimum format lint python-quality test coverage docs node-dependencies \
		binding-types test-python test-node test-ci audit deprecations release release-assets bench \
		report-integration report-integration-coverage report-cuda-integration

CI_UV_RUN = uv run --project tools/ci --frozen
ACTIONLINT ?= actionlint

CORE_MIN_LINE_COVERAGE := 90
PARQUET_MIN_LINE_COVERAGE := 90
# Native model, OCR, and PDFium paths use the asset-backed report-integration target. This floor
# covers the self-contained report suite, including model decoding and PDF geometry seams.
REPORT_MIN_LINE_COVERAGE := 67
REPORT_INTEGRATION_MIN_LINE_COVERAGE := 84

check: format lint test docs binding-types

ci: format lint python-quality docs binding-types test-python test-node test-ci coverage

ci-minimum: test test-python test-node

format:
	cargo fmt --all --check

lint:
	cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
	cargo check --locked -p dicom-anon-report --no-default-features

python-quality:
	$(CI_UV_RUN) ruff check tools/ci tests
	$(CI_UV_RUN) ruff format --check tools/ci tests
	$(CI_UV_RUN) basedpyright --project tools/ci

test:
	cargo test --locked --workspace --all-targets --all-features

# profile.rs is generated normative data with exhaustive table-invariant tests.
coverage:
	cargo llvm-cov --locked -p dicom-anon --all-targets --ignore-filename-regex 'profile.rs' --fail-under-lines $(CORE_MIN_LINE_COVERAGE)
	cargo llvm-cov --locked -p dicom-anon-parquet --all-targets --fail-under-lines $(PARQUET_MIN_LINE_COVERAGE)
	cargo llvm-cov --locked -p dicom-anon-report --all-targets --fail-under-lines $(REPORT_MIN_LINE_COVERAGE)

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
	PYTHONPATH=. $(CI_UV_RUN) pytest -q tests/test_ci.py tests/test_ci_reporting.py tests/test_release.py

audit:
	$(CI_UV_RUN) python tools/ci/security_audit.py

deprecations:
	$(CI_UV_RUN) python tools/ci/deprecation_report.py

release:
	cargo build --locked --workspace --release

# Requires `make audit` first, cargo-cyclonedx 0.5.9, and RELEASE_TAG, for example v0.2.0.
release-assets:
	@test -n "$(RELEASE_TAG)" || (echo "set RELEASE_TAG, for example RELEASE_TAG=v0.2.0" >&2; exit 2)
	$(CI_UV_RUN) python tools/ci/release.py build --tag "$(RELEASE_TAG)"

bench:
	cargo bench --locked -p dicom-anon --bench anonymization -- --sample-size 100
	cargo bench --locked -p dicom-anon-parquet --bench pipeline

report-integration:
	@test -d "$$DICOM_ANON_REPORT_MODEL_DIR" -a -f "$$DICOM_ANON_REPORT_ORT_LIBRARY" -a -f "$$DICOM_ANON_REPORT_PDFIUM_LIBRARY" || (echo "report integration asset paths are not configured" >&2; exit 2)
	cargo test --locked -p dicom-anon-report --test model -- --ignored
	cargo test --locked -p dicom-anon-report --test cli -- --ignored
	cargo test --locked -p dicom-anon-report --test pdf -- --ignored --test-threads=1
	cargo test --locked -p dicom-anon-report --lib sanitized_pdf_renders_black_pixels_over_detected_text -- --ignored --test-threads=1

report-integration-coverage:
	@test -d "$$DICOM_ANON_REPORT_MODEL_DIR" -a -f "$$DICOM_ANON_REPORT_ORT_LIBRARY" -a -f "$$DICOM_ANON_REPORT_PDFIUM_LIBRARY" || (echo "report integration asset paths are not configured" >&2; exit 2)
	cargo llvm-cov clean --workspace
	env -u DICOM_ANON_REPORT_MODEL_DIR -u DICOM_ANON_REPORT_ORT_LIBRARY -u DICOM_ANON_REPORT_PDFIUM_LIBRARY cargo llvm-cov --locked --no-report -p dicom-anon-report --all-targets
	cargo llvm-cov --locked --no-clean -p dicom-anon-report --test model -- --ignored
	cargo llvm-cov --locked --no-clean -p dicom-anon-report --test cli -- --ignored --test-threads=1
	cargo llvm-cov --locked --no-clean -p dicom-anon-report --test pdf -- --ignored --test-threads=1
	cargo llvm-cov --locked --no-clean -p dicom-anon-report --lib -- --ignored --test-threads=1
	cargo llvm-cov report -p dicom-anon-report --fail-under-lines $(REPORT_INTEGRATION_MIN_LINE_COVERAGE)

report-cuda-integration:
	@test -d "$$DICOM_ANON_REPORT_MODEL_DIR" -a -f "$$DICOM_ANON_REPORT_CUDA_ORT_LIBRARY" || (echo "CUDA report integration asset paths are not configured" >&2; exit 2)
	cargo test --locked -p dicom-anon-report --no-default-features --test model_cuda -- --ignored --test-threads=1
