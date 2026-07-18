# Report and PDF anonymization

## Scope

`report-anon` runs [OpenAI Privacy Filter](https://huggingface.co/openai/privacy-filter) locally. It accepts UTF-8 plaintext and PDFs, detects eight private-information categories, and replaces or covers the detected spans. No report content is sent to OpenAI or another model service.

The detector categories are account number, private address, private date, private email, private person, private phone, private URL, and secret. Medical reports can contain identifying context outside that taxonomy. OCR can also omit or misread text. Every JSON report therefore contains `human_review_required: true`.

This feature does not establish HIPAA, GDPR, or other legal compliance. A disclosure process still needs a documented review procedure, access controls, validation on representative report formats, and a decision about residual identification risk.

## Runtime design

The Rust crate reads the upstream tokenizer and calibration JSON directly, runs `model_q4.onnx` with ONNX Runtime, and applies the calibrated 33-label BIOES Viterbi decoder in Rust. CPU uses the default execution provider. CUDA uses the same graph with an explicit CUDA execution provider. Keeping one graph also lets CPU and CUDA tests compare the same weights and calibration.

The decoder uses the upstream transition rules, start and end constraints, calibrated biases, and log-softmax scoring. Long input uses 4,096-token windows with 128 tokens of overlap on each internal boundary, then keeps the center of each window. The overlap is a deliberate difference from the current Python runtime's disjoint windows and reduces the chance that a span is split at a model boundary.

This design does not use the PyTorch-to-Inductor wrapper pattern from `inductor-rs`. The released Privacy Filter repository already contains ONNX graphs and external weight data. Loading those graphs directly removes Python and PyTorch from the deployed process, keeps CPU and CUDA selection in one CLI, and avoids compiling a device-specific graph during installation.

The model is not embedded in `report-anon`. The Q4 external weights are about 917 MB. Keeping the bundle separate makes the executable small and lets operators inventory model and native-runtime files independently.

## Build and install

Install Rust 1.88 or newer. PDF and OCR support also need Tesseract, Leptonica, and the selected language data at build and run time. Debian and Ubuntu packages are:

```console
sudo apt-get install libleptonica-dev libtesseract-dev tesseract-ocr-eng
cargo build --workspace --release
```

Build only plaintext support when the host should not carry PDFium, Tesseract, or Leptonica:

```console
cargo build -p dicom-anon-report --no-default-features --release
```

The bundled installer currently supports Linux x86-64. CPU installation downloads the pinned Q4 graph, ONNX Runtime 1.27.1, and PDFium 7881:

```console
target/release/report-anon model install --backend cpu
```

Select the installed CUDA major version when a host has CUDA and cuDNN configured for ONNX Runtime:

```console
target/release/report-anon model install --backend cuda12
target/release/report-anon model install --backend cuda13
```

The installer downloads from immutable model and release URLs, checks pinned SHA-256 digests, extracts only named runtime files, writes `asset-manifest.json`, and atomically moves the completed bundle into place. It refuses any occupied target path, including a dangling symbolic link. Run `report-anon model path` to print the default location. Use `--target PATH` to select a service-owned directory.

Before inference, the loader independently hashes `config.json`, `tokenizer.json`, `viterbi_calibration.json`, `model_q4.onnx`, and `model_q4.onnx_data` and compares them with the immutable revision's expected values. Editing or substituting a required model file makes loading fail closed.

When `--runtime-library PATH` overrides automatic runtime resolution, also pass `--device cpu`, `--device cuda`, or `--device cuda:N`. The path text is never used to guess the execution provider.

Tesseract is not bundled. Install the language named by `--ocr-language`; the default is `eng`. `--without-pdfium` produces a plaintext-only asset bundle.

## Plaintext and agent pipelines

`INPUT` and `OUTPUT` both default to `-`, so the command works as a filter:

```console
printf '%s\n' 'Patient Alice Smith has MRN 123456.' \
  | report-anon anonymize --device cpu \
  > request.safe.txt
```

The command buffers the bounded input, loads the model, completes inference, and constructs the full result before writing stdout. A failure does not pass through the original text or emit a partial sanitized document. Errors and progress use stderr.

`--max-input-tokens` defaults to 65,536. The command rejects larger tokenizer output before allocating inference logits and decoder state. This limit is separate from `--max-input-bytes` and can be lowered for constrained agent hosts.

Set shell pipeline failure handling when a downstream program sends the result to a cloud service:

```console
set -o pipefail
report-anon anonymize --progress never < request-with-phi.txt \
  | cloud-agent-client
```

The downstream program should reject empty input and the caller should check the pipeline status. For stronger operational isolation, write to a protected temporary file, verify exit status and the JSON report, and only then submit the sanitized file.

Placeholders are stable strings such as `[PRIVATE_PERSON]` and `[ACCOUNT_NUMBER]`. The JSON report contains counts and model identity, not original text, offsets, logits, or replacement-map contents.

## PDF processing

Auto input typing recognizes `.pdf` files and `%PDF-` headers anywhere in the first 1,024 bytes. The bounded scan also catches a PDF preceded by transport comments or a byte-order marker. `--input-format pdf` forces PDF parsing. `--input-format text` forces UTF-8 plaintext processing even when the filename or content matches that heuristic. A plaintext-only build rejects automatically detected or explicitly selected PDF input before loading the model.

Each PDF page follows this sequence:

1. PDFium renders the complete page at `--pdf-dpi`, which defaults to 200.
2. Auto mode uses embedded Unicode text and character geometry when the page contains enough usable text.
3. Auto mode also passes every rendered page to Tesseract. This second pass covers text stored in images when a page has a partial text layer. `--pdf-text-mode ocr` uses only this path; `embedded` disables OCR and fails when a page lacks usable embedded text.
4. The privacy model processes the localized page text.
5. Detected glyph or OCR-word rectangles are painted black in the rendered image.
6. The image is placed on a blank page in a new PDF.

The new PDF contains image pages only. Source text layers, document metadata, attachments, annotations, form fields, JavaScript, and incremental revisions are not copied. This prevents covered text or a stale object revision from remaining recoverable in the output file.

The image-only policy has costs. Text selection and screen-reader access are lost, vector graphics become raster images, and output size depends on page count and DPI. The current PDF output uses black visual redactions instead of inserting replacement text. A DICOM association manifest links the complete PDF to anonymized DICOM identifiers through the output digest. If any non-whitespace character in a protected span lacks a valid paintable rectangle, PDF processing fails without writing sanitized output.

OCR confidence is not a disclosure guarantee. Handwriting, rotated text, low contrast, unusual fonts, tables, and multilingual pages need representative testing and human review. `--max-pdf-pages`, `--max-render-pixels`, and `--max-total-render-pixels` bound page count, per-page pixels, and cumulative rendered pixels. Their defaults are 500 pages, 100,000,000 pixels per page, and 250,000,000 pixels per document.

In auto mode, `embedded_text_pages` and `ocr_pages` can both include the same page. Aggregate detection counts also cover both localization passes, so one visible span found by both passes can contribute twice. Redaction rectangles are deduplicated before the output image is painted.

## DICOM reassociation workflow

Use one key and one profile configuration for the DICOM and report passes. `report-anon` reads original DICOMs to reconstruct the deterministic patient pseudonym, UID replacements, and temporal transformations. It does not modify those DICOMs.

First anonymize each DICOM:

```console
dicom-anonymize source-1.dcm safe-1.dcm \
  --key-file deidentification.key \
  --policy dicom-extended

dicom-anonymize source-2.dcm safe-2.dcm \
  --key-file deidentification.key \
  --policy dicom-extended
```

Then pass the related original DICOMs to the report command with the same key and profile arguments:

```console
report-anon anonymize report.txt report.safe.txt \
  --dicom source-1.dcm \
  --dicom source-2.dcm \
  --key-file deidentification.key \
  --policy dicom-extended
```

The file-output form writes `report.safe.txt.association.json` unless `--association PATH` overrides it. A pipeline must provide an association destination so JSON never enters the sanitized byte stream:

```console
report-anon anonymize \
  --dicom source-1.dcm \
  --key-file deidentification.key \
  --association report.safe.association.json \
  < report.txt > report.safe.txt
```

All selected DICOMs must resolve to one patient pseudonym. Mixed-patient input fails without placing either patient identifier in the error. The in-memory source map has no `Debug` or `Serialize` implementation and is discarded with the process.

The command recognizes these report values when the corresponding DICOM attribute exists:

- Patient ID and common Patient Name renderings;
- Study, Series, and SOP Instance UIDs;
- Accession Number and Study ID;
- common study, series, acquisition, content, creation, and birth dates or times.

Exact linked values take precedence over a model placeholder. Plaintext receives the same safe Patient ID, Patient Name, UID, or shifted temporal value present in the anonymized DICOM. For image-only PDFs, the matched area is covered and the manifest provides document-level association.

The association manifest contains the DICOM profile edition, policy, active options, output patient identifier, output Study/Series/SOP UIDs, and a SHA-256 digest of the sanitized report. It contains no source identifier, source report digest, patient scope, key material, or UID-map contents. Treat the manifest as controlled linkage data even though its identifiers are pseudonymous.

All file destinations are normalized and checked before any source is read. A positional output cannot alias the input unless `--in-place` is present, and in-place mode rejects symbolic-link inputs so the link target cannot retain the original report unexpectedly. Primary and sidecar outputs cannot overwrite a linked DICOM, key file, profile mapping, required model path, resolved native runtime, or any file inventoried by an installed bundle's `asset-manifest.json`. Report and association sidecars also cannot overwrite the input report, sanitized document, or each other.

The sanitized document is persisted or flushed before either sidecar is written. This prevents a report or association manifest from surviving when primary delivery fails. A sidecar failure can leave the complete primary output in place; the command reports status 2 so callers can retry or quarantine that output.

Verify the workflow before release:

1. Run source-aware `dicom-validate` on each anonymized DICOM with the same key and profile inputs.
2. Confirm the manifest patient output attribute and ID match each anonymized DICOM.
3. Confirm the listed Study, Series, and SOP Instance UIDs exist in the anonymized DICOM set.
4. Recompute SHA-256 over the sanitized report and compare it with `output_sha256`.
5. Review the sanitized report and, for PDFs, inspect every rendered page.

Filenames, directory names, archive names, external databases, and identifiers not present in the selected DICOM attributes remain caller responsibilities.

## Safe Harbor metadata profile

Safe Harbor linkage uses the same strict inputs as DICOM anonymization: an independently assigned non-PHI `--patient-scope` and a complete `--uid-map`. Pass both commands the same files and values.

```console
dicom-anonymize source.dcm safe.dcm \
  --policy safe-harbor-metadata \
  --patient-scope SUBJECT-42 \
  --uid-map uid-map.json \
  --key-file deidentification.key

report-anon anonymize report.txt report.safe.txt \
  --dicom source.dcm \
  --policy safe-harbor-metadata \
  --patient-scope SUBJECT-42 \
  --uid-map uid-map.json \
  --key-file deidentification.key
```

Patient ID and Patient Name are empty in Safe Harbor DICOM output. Linked report occurrences receive the keyed Clinical Trial Subject ID so the report remains associable. Original UIDs receive the caller-assigned safe UIDs. Dates and times follow the Safe Harbor metadata transformations documented in [Conformance and scope](conformance.md).

A stricter deployment can omit `--dicom`. The model then emits category placeholders and no association manifest. DICOM profile and mapping options are rejected in this mode instead of being silently ignored. Removing deterministic DICOM linkage does not change the model or OCR limitations.

## Pinned components

The initial package pins these components:

| Component | Pinned value |
|---|---|
| OpenAI Privacy Filter | revision `7ffa9a043d54d1be65afb281eddf0ffbe629385b` |
| ONNX Runtime | `1.27.1` |
| PDFium API/build | `7881` |
| Rust `ort` crate | `2.0.0-rc.12`, API 24 |
| Rust `pdfium-render` crate | `0.9.3` |
| Tesseract binding | `leptess 0.14.0` |

Changing any pin requires CPU and CUDA inference tests, embedded-text and OCR PDF tests, checksum updates, and a review of the installed licenses and notices.

After installing the CPU model and setting `DICOM_ANON_REPORT_MODEL_DIR`, `DICOM_ANON_REPORT_ORT_LIBRARY`, and `DICOM_ANON_REPORT_PDFIUM_LIBRARY`, run `make report-integration` for the asset-backed model, pipe, embedded-text, OCR, and rendered-output fixtures. Run `make report-integration-coverage` to combine those fixtures with the self-contained suite and enforce the 84% native line-coverage floor. The report integration workflow runs that coverage target on pull requests and `master` pushes. Run the CUDA fixture separately on the supported CUDA host:

```console
cargo test -p dicom-anon-report --test model_cuda -- --ignored
```
