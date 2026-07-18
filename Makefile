.PHONY: check format lint test coverage docs binding-types audit release bench report-integration report-integration-coverage

CORE_MIN_LINE_COVERAGE := 90
# Native model, OCR, and PDFium paths use the asset-backed report-integration target. This floor
# covers the self-contained report suite, including model decoding and PDF geometry seams.
REPORT_MIN_LINE_COVERAGE := 67
REPORT_INTEGRATION_MIN_LINE_COVERAGE := 84

check: format lint test docs binding-types

format:
	cargo fmt --all --check

lint:
	cargo clippy --workspace --all-targets --all-features -- -D warnings
	cargo check -p dicom-anon-report --no-default-features

test:
	cargo test --workspace --all-targets

# profile.rs is generated normative data with exhaustive table-invariant tests.
coverage:
	cargo llvm-cov -p dicom-anon --all-targets --ignore-filename-regex 'profile.rs' --fail-under-lines $(CORE_MIN_LINE_COVERAGE)
	cargo llvm-cov -p dicom-anon-report --all-targets --fail-under-lines $(REPORT_MIN_LINE_COVERAGE)

docs:
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

binding-types:
	cd bindings/node && npm ci && npm run typecheck
	uvx --from basedpyright==1.39.9 basedpyright bindings/python/dicom_anon_python.pyi

audit:
	cargo audit

release:
	cargo build --workspace --release

bench:
	cargo bench -p dicom-anon --bench anonymization -- --sample-size 100

report-integration:
	@test -d "$$DICOM_ANON_REPORT_MODEL_DIR" -a -f "$$DICOM_ANON_REPORT_ORT_LIBRARY" -a -f "$$DICOM_ANON_REPORT_PDFIUM_LIBRARY" || (echo "report integration asset paths are not configured" >&2; exit 2)
	cargo test -p dicom-anon-report --test model -- --ignored
	cargo test -p dicom-anon-report --test cli -- --ignored
	cargo test -p dicom-anon-report --test pdf -- --ignored --test-threads=1
	cargo test -p dicom-anon-report --lib sanitized_pdf_renders_black_pixels_over_detected_text -- --ignored --test-threads=1

report-integration-coverage:
	@test -d "$$DICOM_ANON_REPORT_MODEL_DIR" -a -f "$$DICOM_ANON_REPORT_ORT_LIBRARY" -a -f "$$DICOM_ANON_REPORT_PDFIUM_LIBRARY" || (echo "report integration asset paths are not configured" >&2; exit 2)
	cargo llvm-cov clean --workspace
	env -u DICOM_ANON_REPORT_MODEL_DIR -u DICOM_ANON_REPORT_ORT_LIBRARY -u DICOM_ANON_REPORT_PDFIUM_LIBRARY cargo llvm-cov --no-report -p dicom-anon-report --all-targets
	cargo llvm-cov --no-clean -p dicom-anon-report --test model -- --ignored
	cargo llvm-cov --no-clean -p dicom-anon-report --test cli -- --ignored --test-threads=1
	cargo llvm-cov --no-clean -p dicom-anon-report --test pdf -- --ignored --test-threads=1
	cargo llvm-cov --no-clean -p dicom-anon-report --lib -- --ignored --test-threads=1
	cargo llvm-cov report -p dicom-anon-report --fail-under-lines $(REPORT_INTEGRATION_MIN_LINE_COVERAGE)
