# Benchmark results

## Target and method

The optimization target is confidentiality-profile rule dispatch. It runs once for every data element and again for elements in nested sequence items. The harness also measures complete in-memory anonymization, parse/anonymize/write, and validation on an object with 1 MiB of native Pixel Data.

Criterion configuration:

```console
cargo bench -p dicom-anon --bench anonymization -- --sample-size 100
```

The rule-lookup group uses 100 samples. The 1 MiB workloads set 50 samples in the harness. Criterion uses a 3-second warm-up and reports 95% confidence intervals. Measurements were taken on July 10, 2026, on Linux 7.0.10 with an AMD Ryzen Threadripper 3960X. No network operations occur in these workloads.

## Baseline and selected implementation

The baseline searched one sorted 652-rule slice. An exact generated match reduced lookup latency but increased the instruction footprint enough to regress the in-memory anonymization workload. The selected implementation first chooses a compact group slice, then binary-searches within that slice.

| Workload | Baseline interval | Selected interval | Midpoint change | Decision |
|---|---:|---:|---:|---|
| Six common rule lookups | 85.230 to 89.959 ns | 52.142 to 55.332 ns | 38.7% faster | Ship |
| Anonymize parsed 1 MiB object | 32.749 to 34.420 µs | 30.492 to 32.734 µs | 5.9% faster | Ship |
| Parse, anonymize, and write 1 MiB | 152.97 to 153.11 µs | 149.28 to 149.41 µs | 2.4% faster | Ship |
| Validate parsed 1 MiB object | 1.8718 to 1.8766 µs | 1.6705 to 1.7281 µs | 9.2% faster | Ship |

Throughput for the selected parse/anonymize/write workload was 6.5402 to 6.5458 GiB/s in Criterion's in-memory byte workload. This value reflects memory-backed parsing and encoding, not storage-device throughput.

## Rejected exact-match candidate

The exact generated match reduced six lookups to 25.708 to 25.841 ns and validation to 1.5760 to 1.5845 µs. Parsed-object anonymization increased to 34.841 to 35.212 µs, a measured regression against the baseline. The grouped implementation keeps improvements in all four workloads and uses much less generated code.

## Resource measurements

The in-memory benchmark reuses one source object and includes a 1 MiB pixel buffer. Criterion does not report allocation counts.

A separate release-build process measurement used a 16,777,824-byte Part 10 input with 16 MiB of native Pixel Data. One anonymization completed in 0.038 seconds of elapsed time after the binary was built. Child-process peak resident memory was 52,896 KiB. The output was 16,778,268 bytes.

`strace -c` recorded 11 `read` calls, 3 `write` calls, 1 `fsync`, and 1 `renameat`. The file path performs one temporary-file write, one flush, one `sync_all`, and one atomic persist operation. The snapshot did not measure steady-state allocator usage, and the elapsed-time value is not a statistical benchmark. Network throughput, requests, retries, and errors are not applicable.

## Correctness-fix regression check

The D-sequence replacement and coordinated TM shifting changes added work to anonymization and validation. The first correct implementation measured 35.663 to 35.742 µs for parsed-object anonymization, 154.32 to 154.55 µs for parse/anonymize/write, and 2.2706 to 2.2792 µs for validation. A follow-up removed an unconditional data-set scan, combined the patient temporal derivation into one keyed operation, and isolated dummy-sequence validation from the normal path.

The final implementation measured 35.263 to 35.633 µs, 148.37 to 149.30 µs, and 2.2605 to 2.2682 µs for those workloads, respectively. The end-to-end interval is consistent with the selected implementation recorded above. The smaller parsed-object and validation changes are close enough to run-to-run noise that they should not be treated as independent optimization claims. The correctness behavior differs from the pre-fix implementation, so its narrower microbenchmark timings are not a valid candidate for release.

Run on stable hardware with background work minimized before comparing future changes. Keep the fixture size, transfer syntax, sample counts, and release profile unchanged.

## Configurable option-matrix check

The 2026c configurable-option change increases each generated rule from one effective action to the Basic action plus ten metadata option columns. The first implementation searched the full 652-row matrix and rechecked every row for configuration conflicts during each candidate validation. Measurements on July 11, 2026 showed 88.102 to 88.292 ns for six common lookups and 14.444 to 14.463 µs for candidate validation.

The selected implementation generates the exact group ranges and the two conflicting option pairs with the normative data. It searches only the applicable group and validates the active option set against the generated pair list. Six lookups measured 59.236 to 59.339 ns, 32.8% faster than the first configurable implementation. Candidate validation measured 4.0022 to 4.0171 µs, 72.2% faster than the first configurable implementation.

With the selected dispatch, parsed-object anonymization measured 37.421 to 37.548 µs and parse/anonymize/write measured 1.8542 to 1.8586 ms. These end-to-end measurements were within Criterion's noise threshold compared with the immediately preceding configurable implementation. The remaining lookup and validation cost above the former fixed profile is accepted because resolving and validating caller-selected option columns is required behavior; the generated dispatch bounds that cost without weakening configuration checks.

## Exact-marker validation follow-up

Exact validation of De-identification Method text, DCM code values, coding schemes, meanings, duplicates, and the Safe Harbor subject-token format initially increased candidate validation from 4.0022 to 4.0171 µs to 4.6564 to 4.6901 µs. The first correct implementation allocated a map and set for every candidate.

The selected implementation compares the ordered method text and uses a fixed bit mask plus the configured option iterator for code membership. It measured 4.2424 to 4.2985 µs, 8.4% faster than the first correct implementation and about 6.5% slower than the pre-fix validator. The remaining cost is accepted because the previous validator allowed malformed or identifying attestation content to pass. The benchmark performs no disk or network I/O, and this change does not affect those resources. Decision: ship the allocation-free exact validation.

## Report PDF resource-guard check

The cumulative PDF pixel limit adds one checked multiplication and addition per page in the report crate. The existing Criterion suite does not load the native PDF stack, so it cannot measure that page loop directly. A before-and-after run on July 18, 2026 verifies that the unrelated DICOM hot paths remain stable:

| Workload | Baseline 95% interval | Candidate 95% interval | Criterion result |
|---|---:|---:|---|
| Six common rule lookups | 58.541 to 58.698 ns | 58.698 to 58.902 ns | Within noise threshold |
| Anonymize parsed 1 MiB object | 37.248 to 37.285 µs | 37.229 to 37.276 µs | Within noise threshold |
| Parse, anonymize, and write 1 MiB | 156.83 to 161.38 µs | 156.52 to 156.77 µs | No change detected |
| Validate parsed 1 MiB object | 4.2551 to 4.2610 µs | 4.2824 to 4.2894 µs | Within noise threshold |

Decision: ship the resource guard. Its fixed per-page accounting prevents unbounded aggregate rendering work, and no core DICOM benchmark showed a meaningful regression. The asset-backed report integration target remains the functional gate for native model, OCR, and PDFium execution.
