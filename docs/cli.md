# CLI contract

## `dicom-to-parquet`

```text
dicom-to-parquet [OPTIONS] SOURCE_DIR OUTPUT_PATH
```

The command recursively discovers regular files under `SOURCE_DIR`. A case-insensitive `.dcm` or `.dicom` extension makes a file a candidate without an open during discovery. Files with another named extension are rejected without opening. An extensionless file is opened only far enough to verify `DICM` at byte offset 128. Symbolic links are not followed.

`OUTPUT_PATH` is a base name. For example, `data.parquet` produces `data.00000.parquet` and, when needed, later numbered shards. Shards use a uniform all-UTF-8 schema and Zstandard compression. `--shard-size MIB` defaults to 128 and is an approximate compressed-size boundary applied between Arrow record batches. `--compression-level LEVEL` accepts Zstandard levels 1 through 22 and defaults to 6. Existing target shards are never overwritten.

Each DICOM is parsed once. File Meta Information and data-set metadata are staged in a compact unlinked temporary file while the union schema is collected; the staging file, not the source DICOM, is reread to build Arrow batches. Private elements and unknown dictionary aliases are omitted at every nesting level. Sequences retain the baseline representation as a JSON array whose entries are DICOM JSON strings. SOP Instance UID and Study Instance UID must be non-empty. Pixel Data is never included in Parquet.

Pixel hashing is disabled by default. The lazy parser skips the value without loading it while continuing to parse attributes after Pixel Data. `--hash` streams native values through XXH3-64 with seed 0. Encapsulated values exclude the Basic Offset Table and use the wrapping sum of each fragment's seed-0 XXH3-64 hash. This matches the baseline `dicom-structs` tool. The hash is written as an unsigned decimal string.

### Intended workflows

For secure metadata retention, write the Parquet shards to encrypted, access-controlled storage and enable `--hash`. An exact hash match can later reconnect the retained original metadata to a separately stored DICOM or pixel object when both use this command's Pixel Data hash contract. The Parquet output can contain protected health information and must remain inside the controlled storage boundary.

For database ingestion, use the shards as a uniform UTF-8 import source. `--column-naming snake-case` produces names suited to databases that require identifiers such as `sop_instance_uid`; `--column-naming dicom` preserves standard aliases such as `SOPInstanceUID`. The naming policy changes column names only. It does not de-identify values.

The Pixel Data hash is a linkage aid, not an authoritative identifier or security control. XXH3-64 is non-cryptographic and 64-bit. Files with identical Pixel Data values have the same hash, while transcoding, refragmentation, or pixel edits can produce a different hash. Retain DICOM UIDs or a separate controlled manifest when the workflow requires unambiguous one-to-one linkage.

`--column-naming dicom` preserves DICOM aliases such as `SOPInstanceUID`. `--column-naming snake-case` converts them to names such as `sop_instance_uid`; `-s` and `--snake-case` are compatibility aliases. Naming is a selectable policy so future database-specific conventions can be added without changing extraction behavior.

Repeat `--tag TAG=VALUE` to set a standard DICOM attribute to a constant after extraction. `TAG` is a standard dictionary keyword such as `Modality`; Pixel Data cannot be overridden. `--sort inode` is the default and favors local filesystem locality; `path` gives deterministic lexical order; `none` retains traversal order.

Invalid candidates are reported on stderr. The default best-effort mode converts valid files and returns status 1 if any candidate failed. `--strict` stops at the first failed candidate and publishes no shards. Diagnostics retain at most 20 failure samples.

`--format text` prints the status and summary to stdout. `--format json` emits one pretty-printed JSON document and disables color and automatic progress. `--progress auto|always|never` controls the bounded processing indicator on stderr; auto mode requires a terminal stderr and text output. `--quiet` prints only status and counts and disables progress and failure details. `--verbose` adds conversion configuration and the number of pixel bytes streamed for hashing. `--color auto|always|never` affects text output and diagnostics; `--no-color` takes precedence.

Exit status:

- `0`: every discovered candidate was converted and all shards were published;
- `1`: best-effort conversion published valid rows, but one or more candidates failed;
- `2`: invalid arguments, discovery failure, strict-mode DICOM failure, no usable input, staging failure, or output failure.

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
| `--descriptor-map PATH` | Load a descriptor map. Repeat to merge maps; two maps that give different outputs for the same tag and source value cause an error. |
| `--fail-on-unmapped TAG` | Fail when a cleaned attribute with this tag gets a dummy value because no descriptor-map entry matched. Dummy values that the profile requires do not fail; see [Conformance and scope](conformance.md#detection-and-fail-closed-control). `dicom-validate` rejects this option with exit status 2. `TAG` is eight hexadecimal digits, for example `00080100`. Repeat for multiple tags. |
| `--safe-private-allowlist PATH` | Load a JSON array of `{ "creator": STRING, "element_offset": INTEGER }`. |
| `--uid-map PATH` | Load a JSON object from original UID to replacement UID. Required for Safe Harbor anonymization unless `--generate-uid-map` assigns the missing entries. |
| `--require-no-burned-in-annotation` | Fail unless Burned In Annotation `(0028,0301)` is exactly `NO`. A `YES`, empty, or absent value fails. |
| `--allow-sop-class UID` | Permit only the given SOP Class UID; repeat for multiple SOP Classes. An absent or other SOP Class UID fails, and so does a Media Storage SOP Class UID `(0002,0002)` that differs from it. |

The presets enable these options before applying `--enable-option` and `--disable-option`:

| Preset | Enabled options |
|---|---|
| `dicom` | None. This is the DICOM Basic Application Level Confidentiality Profile. |
| `dicom-extended` | `clean-descriptors`, `retain-modified-dates`, `retain-patient-characteristics` |
| `safe-harbor-metadata` | `clean-graphics`, `clean-structured-content`, `clean-descriptors`, `retain-modified-dates`, `retain-patient-characteristics` |

Both commands default to `dicom-extended`. The `dicom` and `dicom-extended` presets use the serialized policy value `dicom`; reports distinguish them through `active_options`. `clean-pixel-data` and `clean-recognizable-visual-features` are accepted names but produce unsupported-option errors.

The two pixel-risk gates are off by default and apply to every preset. With a gate active, `dicom-anonymize` exits with status 2 and writes no output for a failing input, and `dicom-validate` reports the error finding `burned_in_annotation_not_no` or `sop_class_not_allowed`. For mammography, a typical gate is:

```console
dicom-anonymize IN OUT --policy safe-harbor-metadata --patient-scope SUBJ-0001 \
  --uid-map MAP --key-file KEY --require-no-burned-in-annotation \
  --allow-sop-class 1.2.840.10008.5.1.4.1.1.1.2 \
  --allow-sop-class 1.2.840.10008.5.1.4.1.1.13.1.3
```

For JPEG 2000 transfer syntaxes, `dicom-anonymize` always parses each frame codestream and removes comment (COM) marker segments. It fails with status 2 when it cannot do this safely. `--verbose` reports the number of removed segments as `removed_jpeg2000_comment_segments`. `dicom-validate` reports `jpeg2000_comment_present` or `jpeg2000_codestream_unparseable` as errors.

For JPEG transfer syntaxes `1.2.840.10008.1.2.4.50`, `.51`, `.57`, and `.70`, and JPEG-LS transfer syntaxes `.80` and `.81`, `dicom-anonymize` always parses each frame codestream and removes comment (COM) and application (APPn) marker segments. It keeps an APP14 `Adobe` segment only when a three- or four-component frame needs its color transform flag. It fails with status 2 when it cannot do this safely, and for input with a retired JPEG transfer syntax (`.52` to `.56` and `.58` to `.66`). `--verbose` reports `removed_jpeg_comment_segments` and `removed_jpeg_application_segments`. `dicom-validate` reports `jpeg_comment_present`, `jpeg_application_segment_present`, or `jpeg_codestream_unparseable` as errors.

See [Conformance](conformance.md#pixel-and-disclosure-limits) for the exact conditions.

Example descriptor map:

```json
{
  "00081030": {
    "MAMMO SCRN BIL W/CAD TOMO": "MAMMO SCREENING BILATERAL"
  }
}
```

Safe Harbor UID map arguments:

| Argument | Meaning |
|---|---|
| `--list-required-uids` | Print a JSON object `{"required_uids": [{"path": PATH, "uid": UID}, ...]}` to stdout and write no DICOM output. The list contains each source UID occurrence that the configured profile replaces, in traversal order. It uses the same rule decisions as anonymization and fails for the same input errors, except a missing UID mapping. It conflicts with `OUTPUT`, `--in-place`, `--generate-uid-map`, and `--verbose`. |
| `--generate-uid-map PATH` | Safe Harbor only. Start from the `--uid-map` contents, or an empty map. Assign each missing replacement from the operating-system CSPRNG under `--uid-root`, then write the complete map to `PATH` before the DICOM output. `PATH` must not name the input, output, key, descriptor-map, or safe-private allowlist file, also through a symbolic link. It may name the `--uid-map` file, so that one map grows over a study. If `PATH` already exists, it must be the same file as `--uid-map`; the command does not replace a map that it did not load. Conflicts with `--in-place`. |

A path has one `(gggg,eeee)[n]/` component for each enclosing sequence item and ends with the attribute tag. A missing Safe Harbor mapping fails with that path and never the UID value. See [Security model](security.md) for map custody rules.

```console
dicom-anonymize input.dcm \
  --policy safe-harbor-metadata \
  --patient-scope SUBJECT-42 \
  --key-file deidentification.key \
  --list-required-uids
```

```console
dicom-anonymize input.dcm output.dcm \
  --policy safe-harbor-metadata \
  --patient-scope SUBJECT-42 \
  --uid-map study-uid-map.json \
  --generate-uid-map study-uid-map.json \
  --key-file deidentification.key
```

A descriptor map is a JSON object keyed by canonical eight-digit tag, then exact source value and safe output value, as above. A map can also be a versioned document that records its provenance:

```json
{
  "format": "dicom-anon-descriptor-map/1",
  "name": "example-map",
  "version": "1",
  "source": "Reviewed site vocabulary",
  "notes": [],
  "mappings": {
    "00080070": { "SYNTHVENDOR": "SYNTHVENDOR" }
  }
}
```

`descriptor-maps/breast-imaging-ps3.16-2026d.json` is a reviewed versioned map for the mammography view, view modifier, and breast anatomic region codes. [Conformance and scope](conformance.md#text-attributes-outside-table-e1-1) gives its contents and the attributes that Safe Harbor mode changes. The guard applies to a tag at every nesting level. Before you enable it, add reviewed maps for the other code sequences that your objects contain, such as Purpose of Reference Code Sequence; [Conformance and scope](conformance.md#detection-and-fail-closed-control) gives the procedure. For a mammography inference pipeline:

```console
dicom-anonymize input.dcm output.dcm \
  --policy safe-harbor-metadata \
  --patient-scope SUBJ-0001 \
  --uid-map uid-map.json \
  --key-file deidentification.key \
  --descriptor-map descriptor-maps/breast-imaging-ps3.16-2026d.json \
  --fail-on-unmapped 00080100 \
  --fail-on-unmapped 00080102 \
  --fail-on-unmapped 00080104
```

The `--verbose` report includes `dummy_replacements` and `unmapped_dummy_replacements`, which count dummy values by tag without source values.

The default successful invocation writes no text. `--verbose` writes the JSON anonymization report to stderr. DICOM bytes are the only stdout content. `--quiet` and `--verbose` are mutually exclusive. Color affects diagnostics only; `--no-color` overrides `--color`. Auto mode tests stderr because diagnostics are written there.

Exit status:

- `0`: the complete output was written;
- `2`: invalid arguments, missing key material, parse failure, policy failure including a `--fail-on-unmapped` match, or output failure.

Except for `--list-required-uids`, the command has no text or JSON primary-output mode because its primary output is a DICOM byte stream. The report is secondary diagnostic data and stays on stderr.

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

Repeat `--dicom PATH` to derive source-to-output identifier mappings from related original DICOMs. All selected DICOMs must resolve to one patient scope. Linkage requires the same key and DICOM profile arguments used by `dicom-anonymize`. DICOM profile, mapping, pixel-risk gate (`--require-no-burned-in-annotation`, `--allow-sop-class`), key, patient-scope, UID-root, and re-anonymization options are rejected unless at least one `--dicom` is present. File output defaults to `OUTPUT.association.json`; stdout output requires `--association PATH`. The association JSON never shares stdout with sanitized data.

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
