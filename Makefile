.PHONY: check ci ci-minimum format lint python-quality test coverage docs node-dependencies \
		binding-types test-python test-node test-ci audit deprecations release release-assets bench \
		report-integration report-integration-coverage report-cuda-integration codec-pins \
		codec-pins-latest test-jpeg2000-conformance test-text-detector

CI_UV_RUN = uv run --project tools/ci --frozen
ACTIONLINT ?= actionlint

CORE_MIN_LINE_COVERAGE := 90
PARQUET_MIN_LINE_COVERAGE := 90
# Native model, OCR, and PDFium paths use the asset-backed report-integration target. This floor
# covers the self-contained report suite, including model decoding and PDF geometry seams.
REPORT_MIN_LINE_COVERAGE := 67
REPORT_INTEGRATION_MIN_LINE_COVERAGE := 84

check: format lint test docs binding-types

ci: format lint python-quality docs binding-types test-python test-node test-ci coverage \
	codec-pins test-jpeg2000-conformance test-text-detector

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

# The codec crates must match medcognetics/dicom-preprocessing (#47).
codec-pins:
	$(CI_UV_RUN) python scripts/check_codec_pins.py

# Nightly only: compare with the current dicom-preprocessing master.
codec-pins-latest:
	$(CI_UV_RUN) python scripts/check_codec_pins.py --ref master

# ITU-T T.803 class 1 conformance of the JPEG 2000 decoder. The data is fetched, pinned by
# SHA-256, because its license does not allow it in this repository.
JPEG2000_CONFORMANCE_DIR ?= target/jpeg2000-conformance
test-jpeg2000-conformance:
	scripts/fetch-jpeg2000-conformance.sh $(JPEG2000_CONFORMANCE_DIR)
	DICOM_ANON_J2K_CONFORMANCE=$(abspath $(JPEG2000_CONFORMANCE_DIR)) cargo test --locked -p dicom-anon --test jpeg2000_conformance

# The pinned text detector of the burned-in text check. The model and ONNX Runtime are
# installed once, checked against their pinned SHA-256 values, and kept in the folder.
TEXT_DETECTOR_DIR ?= target/text-detector
test-text-detector:
	cargo build --locked -p dicom-anon --bin dicom-text-detector
	if [ -d $(TEXT_DETECTOR_DIR) ]; then target/debug/dicom-text-detector check $(TEXT_DETECTOR_DIR); \
	else target/debug/dicom-text-detector install $(TEXT_DETECTOR_DIR); fi
	DICOM_ANON_TEXT_DETECTOR_DIR=$(abspath $(TEXT_DETECTOR_DIR)) cargo test --locked -p dicom-anon --test text_detector

test-ci:
	$(ACTIONLINT) -config-file .github/actionlint.yaml
	PYTHONPATH=. $(CI_UV_RUN) pytest -q tests/test_ci.py tests/test_ci_reporting.py tests/test_release.py tests/test_codec_pins.py

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

# The bundle ships only ONNX Runtime; the CUDA runtime, cuBLAS, and cuDNN come from the
# host loader path. Check the GPU and the provider's dependencies before the tests.
report-cuda-integration:
	@test -d "$$DICOM_ANON_REPORT_MODEL_DIR" -a -f "$$DICOM_ANON_REPORT_CUDA_ORT_LIBRARY" || (echo "CUDA report integration asset paths are not configured" >&2; exit 2)
	@nvidia-smi --query-gpu=name,driver_version,memory.total,compute_cap --format=csv,noheader || { echo "no usable NVIDIA GPU or driver" >&2; exit 2; }
	@provider="$$(dirname "$$DICOM_ANON_REPORT_CUDA_ORT_LIBRARY")/libonnxruntime_providers_cuda.so"; \
		test -f "$$provider" || { echo "$$provider is missing" >&2; exit 2; }; \
		report="$$(ldd "$$provider")" || { echo "ldd failed for $$provider" >&2; exit 2; }; \
		printf '%s\n' "$$report"; \
		if printf '%s\n' "$$report" | grep --fixed-strings --quiet 'not found'; then echo "unresolved CUDA provider dependencies; set LD_LIBRARY_PATH" >&2; exit 2; fi
	cargo test --locked -p dicom-anon-report --no-default-features --test model_cuda -- --ignored --test-threads=1
