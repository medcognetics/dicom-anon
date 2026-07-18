# CLI contract

## `dicom-anonymize`

```text
dicom-anonymize [OPTIONS] INPUT [OUTPUT]
```

`INPUT` and `OUTPUT` name DICOM Part 10 files. `-` selects stdin or stdout. Omitting `OUTPUT` selects stdout. `--in-place` requires a file input, conflicts with `OUTPUT`, and replaces the input atomically after a successful parse, transformation, encode, flush, and file sync. Existing permissions are retained for in-place output.

The command reads key material from `--key-file PATH`. If that option is absent, it reads the environment variable named by `--key-env`, which defaults to `DICOM_ANON_KEY`. There is no raw `--key` argument because command-line arguments are commonly visible through process inspection.

Anonymization and validation share these profile arguments:

| Argument | Meaning |
|---|---|
| `--policy dicom\|dicom-extended\|safe-harbor-metadata` | Select a profile preset. The default is `dicom-extended`. |
| `--enable-option OPTION` | Enable an option; repeat for multiple options. |
| `--disable-option OPTION` | Disable an option enabled by the selected preset; repeat for multiple options. |
| `--descriptor-map PATH` | Load a JSON object keyed by canonical eight-digit tag, then exact source value and safe output value. |
| `--safe-private-allowlist PATH` | Load a JSON array of `{ "creator": STRING, "element_offset": INTEGER }`. |
| `--uid-map PATH` | Load a JSON object from original UID to replacement UID. Required for Safe Harbor anonymization. |

The presets enable these options before applying `--enable-option` and `--disable-option`:

| Preset | Enabled options |
|---|---|
| `dicom` | None. This is the DICOM Basic Application Level Confidentiality Profile. |
| `dicom-extended` | `clean-descriptors`, `retain-modified-dates`, `retain-patient-characteristics` |
| `safe-harbor-metadata` | `clean-graphics`, `clean-structured-content`, `clean-descriptors`, `retain-modified-dates`, `retain-patient-characteristics` |

Both commands default to `dicom-extended`. The `dicom` and `dicom-extended` presets use the serialized policy value `dicom`; reports distinguish them through `active_options`. `clean-pixel-data` and `clean-recognizable-visual-features` are accepted names but produce unsupported-option errors.

Example descriptor map:

```json
{
  "00081030": {
    "MAMMO SCRN BIL W/CAD TOMO": "MAMMO SCREENING BILATERAL"
  }
}
```

The default successful invocation writes no text. `--verbose` writes the JSON anonymization report to stderr. DICOM bytes are the only stdout content. `--quiet` and `--verbose` are mutually exclusive. Color affects diagnostics only; `--no-color` overrides `--color`. Auto mode tests stderr because diagnostics are written there.

Exit status:

- `0`: the complete output was written;
- `2`: invalid arguments, missing key material, parse failure, policy failure, or output failure.

The command has no text or JSON primary-output mode because its primary output is a DICOM byte stream. The report is secondary diagnostic data and stays on stderr.

## `dicom-validate`

```text
dicom-validate [OPTIONS] [INPUT]
```

`INPUT` defaults to stdin. The report is written to stdout. Errors are written to stderr.

`--format text` is the default. It prints a status line, summary, and findings. `--quiet` limits text to the status and summary. `--verbose` adds the profile edition and checked-attribute count. `--format json` writes one pretty-printed JSON document, ignores color, and retains deterministic finding order.

Candidate-only validation reports `validation_mode=candidate_only` and one aggregate `source_not_compared` warning. Add `--source ORIGINAL --key-file KEY` for exact source comparison. `--source` must be a file path, so the candidate may still be read from stdin. `--key-env` is also available. Safe Harbor source comparison additionally requires the same `--patient-scope` and `--uid-map` used during anonymization.

Color policy is `--color auto|always|never`; `--no-color` wins if both flags are present. Auto mode enables color only for terminal stdout.

Exit status:

- `0`: no error-severity metadata findings; warning findings may remain;
- `1`: one or more metadata conformance errors;
- `2`: invalid arguments, parse failure, or report failure.

No progress option is provided. Each invocation processes one file, and progress output would add noise without a bounded multi-item workload.

## `report-anon`

```text
report-anon anonymize [OPTIONS] [INPUT] [OUTPUT]
report-anon model install [OPTIONS]
report-anon model path
```

`INPUT` accepts UTF-8 plaintext or a PDF and defaults to stdin. `OUTPUT` defaults to stdout. `--input-format auto` treats a `.pdf` file or input containing `%PDF-` within its first 1,024 bytes as PDF. The bounded header scan recognizes PDF data with leading transport bytes. `--input-format pdf` forces PDF parsing. `--input-format text` forces UTF-8 plaintext processing even when the filename or content matches the PDF heuristic.

`--in-place` requires a file path that is not a symbolic link and replaces it atomically only after model inference and output construction succeed. It rejects a symbolic-link input because an atomic rename would replace the link entry instead of its target. A positional `OUTPUT` that resolves to the input is rejected, including relative-path and symbolic-link aliases, so replacing the source always requires the explicit flag. The command reads the complete bounded input before writing sanitized bytes. A model, PDF, OCR, linkage, or primary-output write error therefore produces no partial primary output. The primary output is persisted and flushed before either sidecar. If a later sidecar write fails, the complete primary output remains and the command returns status 2.

Sanitized text or PDF bytes are the primary output. They are the only stdout content. `--report PATH` writes a pretty-printed JSON transformation report. `--verbose` writes the same safe report to stderr. A `--format` option is not provided because the primary output can be binary PDF data; JSON is secondary data and always uses an explicit sidecar or stderr.

`--progress auto|always|never` controls a spinner on stderr. Auto mode requires a terminal stderr. `--quiet` disables progress and other non-error diagnostics. `--color` and `--no-color` apply only to errors. `--no-color` takes precedence.

`--device auto` uses an installed CUDA runtime when the bundle contains the Q4 model and otherwise selects CPU. `--device cpu`, `--device cuda`, and `--device cuda:N` select an explicit execution provider. `--runtime-library PATH` requires one of those explicit device values because filenames do not identify provider capabilities reliably. `--model-dir`, `--runtime-library`, and `--pdfium-library` override the installed paths. Equivalent environment variables are available for service processes.

`--max-input-bytes` bounds the buffered source document. `--max-input-tokens` defaults to 65,536 and rejects tokenizer output above that count before inference logits and decoder state are allocated. For PDFs, the token limit applies independently to each embedded-text or OCR pass on a page. A zero token limit is a usage error. `--max-render-pixels` defaults to 100,000,000 pixels for one PDF page. `--max-total-render-pixels` defaults to 250,000,000 pixels across the complete PDF. The command checks the cumulative limit before rendering each page.

PDF input defaults to `--pdf-text-mode auto`. Auto mode uses character geometry from usable embedded text and also applies Tesseract to every rendered page so an incomplete text layer cannot hide text stored in an image. `embedded` uses only embedded text and fails when a page has none. `ocr` uses only Tesseract. Every output page is a raster image inside a new PDF document. In auto mode, aggregate detection counts describe both localization passes and can count the same visible span twice.

Repeat `--dicom PATH` to derive source-to-output identifier mappings from related original DICOMs. All selected DICOMs must resolve to one patient scope. Linkage requires the same key and DICOM profile arguments used by `dicom-anonymize`. DICOM profile, mapping, key, patient-scope, UID-root, and re-anonymization options are rejected unless at least one `--dicom` is present. File output defaults to `OUTPUT.association.json`; stdout output requires `--association PATH`. The association JSON never shares stdout with sanitized data.

`OUTPUT`, `--report`, and `--association` cannot replace a DICOM, key file, descriptor map, safe-private allowlist, UID map, or file inventoried by an installed bundle's `asset-manifest.json`. Required model paths and resolved ONNX Runtime and PDFium libraries receive the same protection when no installed manifest is present. The sidecars must also use distinct paths and cannot replace the primary document output or source report. Existing paths and parent directories are canonicalized before comparison so relative and symbolic-link aliases receive the same check.

Exit status:

- `0`: the complete sanitized output and requested sidecars were written;
- `2`: invalid arguments, missing assets, model failure, PDF or OCR failure, linkage failure, or output failure.

The command has no exit-1 result because it does not implement a pass/fail validation outcome. Every successful output still sets `human_review_required` in its report.

`report-anon model install` currently packages Linux x86-64 native runtimes. It downloads one immutable OpenAI Privacy Filter revision, the selected CPU/CUDA ONNX Runtime 1.27.1 archive, and PDFium 7881 unless `--without-pdfium` is set. SHA-256 checks precede extraction. Model graph, external-weight, tokenizer, configuration, and calibration digests are checked again before every model load. The target path must be unoccupied; an existing file, directory, symbolic link, or dangling symbolic link is rejected. The successful command prints the installed path to stdout; `--quiet` suppresses it. Download progress uses stderr.

A plaintext-only build is available with `cargo build -p dicom-anon-report --no-default-features --release`. That binary rejects detected or explicit PDF input before model loading. Its help omits PDF-specific runtime and OCR flags.

## Pipeline examples

```console
dicom-anonymize input.dcm - --key-file deidentification.key \
  | dicom-validate - --format json \
  | jq -e '.conformant'
```

```console
dicom-validate candidate.dcm \
  --source original.dcm \
  --key-file deidentification.key \
  --descriptor-map descriptor-map.json \
  --format json
```

```console
dicom-anonymize input.dcm output.dcm \
  --policy safe-harbor-metadata \
  --patient-scope SUBJECT-42 \
  --uid-map uid-map.json \
  --key-file deidentification.key
```

```console
set -o pipefail
report-anon anonymize --progress never < request-with-phi.txt \
  | cloud-agent-client
```

```console
report-anon anonymize report.pdf report.safe.pdf \
  --pdf-text-mode auto \
  --report report.safe.json
```

```console
report-anon anonymize report.txt report.safe.txt \
  --dicom source.dcm \
  --key-file deidentification.key \
  --association report.safe.association.json
```

```console
if dicom-validate candidate.dcm --quiet; then
  printf '%s\n' 'metadata profile passed'
else
  status=$?
  if [ "$status" -eq 1 ]; then
    printf '%s\n' 'metadata profile failed' >&2
  else
    printf '%s\n' 'validator could not run' >&2
  fi
fi
```
