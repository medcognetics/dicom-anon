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
| `--policy dicom\|safe-harbor-metadata` | Select the normal DICOM preset or strict Safe Harbor metadata preset. |
| `--enable-option OPTION` | Enable an option; repeat for multiple options. |
| `--disable-option OPTION` | Disable a preset option; repeat for multiple options. |
| `--descriptor-map PATH` | Load a JSON object keyed by canonical eight-digit tag, then exact source value and safe output value. |
| `--safe-private-allowlist PATH` | Load a JSON array of `{ "creator": STRING, "element_offset": INTEGER }`. |
| `--uid-map PATH` | Load a JSON object from original UID to replacement UID. Required for Safe Harbor anonymization. |

The default DICOM options are `clean-descriptors`, `retain-modified-dates`, and `retain-patient-characteristics`. The Safe Harbor preset additionally enables `clean-graphics` and `clean-structured-content`. `clean-pixel-data` and `clean-recognizable-visual-features` are accepted names but produce unsupported-option errors.

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
