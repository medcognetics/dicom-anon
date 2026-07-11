.PHONY: check format lint test coverage docs binding-types audit release bench

check: format lint test docs binding-types

format:
	cargo fmt --all --check

lint:
	cargo clippy --workspace --all-targets --all-features -- -D warnings

test:
	cargo test --workspace --all-targets

# profile.rs is generated normative data with exhaustive table-invariant tests.
coverage:
	cargo llvm-cov -p dicom-anon --all-targets --ignore-filename-regex 'profile.rs' --fail-under-lines 90

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
