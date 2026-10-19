# Benchmark results

## One-read DICOM-to-Parquet pipeline

The Parquet harness compares one lazy parse with an equivalent two-read reference that separately materializes schema and row metadata. Native and encapsulated fixtures run with Pixel Data hashing both disabled and enabled. A second group converts eight native DICOM copies through discovery, parallel extraction, compact staging, Arrow conversion, Zstandard compression, and atomic shard publication.

```console
cargo bench -p dicom-anon-parquet --bench pipeline
```

Criterion uses 20 samples, a 3-second warm-up, and a 10-second measurement period. Measurements were taken on July 24, 2026, on Linux 7.0.10 with Rust 1.92.0 and an AMD Ryzen Threadripper 3960X. The native fixture is 31,300 bytes and the encapsulated fixture is 118,986 bytes. The intervals below are Criterion 95% confidence intervals for the mean.

| Extraction workload | Two-read reference | One-read pipeline | Midpoint change | Decision |
|---|---:|---:|---:|---|
| Native, hash off | 69.454 to 70.152 µs | 52.217 to 52.511 µs | 25.0% faster | Ship |
| Native, hash on | 76.188 to 76.551 µs | 54.611 to 54.770 µs | 28.4% faster | Ship |
| Encapsulated, hash off | 205.835 to 206.666 µs | 154.829 to 155.185 µs | 24.9% faster | Ship |
| Encapsulated, hash on | 234.676 to 235.895 µs | 161.594 to 161.939 µs | 31.2% faster | Ship |

The eight-file end-to-end workload measured 15.431 to 15.527 ms with hashing disabled and 13.571 to 14.856 ms with hashing enabled. The overlapping sources of filesystem and temporary-output variance make the relative ordering of those two modes unsuitable as a hashing-cost claim. They serve as absolute regression budgets for the complete path.

Decision: ship the one-read pipeline. It improves every equivalent extraction comparison and avoids reopening source DICOMs for schema inference. With hashing disabled, the extractor reports zero hashed pixel bytes and skips Pixel Data through the lazy reader. With hashing enabled, Pixel Data is streamed through XXH3 without being retained. Network operations are not applicable.

A release-process check used one synthetic 16,777,824-byte Part 10 file with 16 MiB of native Pixel Data. Hash-off conversion completed in 0.02 seconds with 13,388 KiB peak resident memory; hash-on completed in 0.01 seconds with 12,520 KiB. The measurements ran against a warm page cache and are snapshots, not statistical timing comparisons. The respective compressed Parquet files were 8,143 and 8,521 bytes. Peak resident memory stayed below the Pixel Data size in both modes, consistent with skipping or streaming the value instead of retaining it.

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

## Encapsulated segment-removal memory check

The JPEG, JPEG-LS, and JPEG 2000 rewrite path previously held three Pixel Data copies at its peak: the source, the replacement fragments, and a copy of the source Pixel Data that profile processing made before the replacement overwrote it. The file and stream paths also kept the source object while the encoder wrote the output, and the `dicom-object` 0.10 encoder copies each element while it writes. The candidate passes the pending replacement elements into root data-set processing, which inserts a replacement for a kept element without copying the source value. `anonymize_file` and `anonymize_reader` drop the source object before encoding. Codestream parsing, segment removal, and the Extended Offset Table check still complete before any output element is built.

The new `dicom_processing_jpeg_8x1m_segments` group uses a synthetic JPEG Lossless SV1 object with 8 frames. Each frame has one APP1 segment, one COM segment, and 1 MiB of scan data. The group uses 20 samples; the other groups keep their earlier sample counts.

```console
cargo bench --locked -p dicom-anon --bench anonymization -- --sample-size 100
```

Measurements were taken on October 17, 2026, on macOS 27.0.1 with Rust 1.96.0 and an Apple M4 Max. These results are not comparable with the earlier Linux sections. The intervals are Criterion 95% confidence intervals for the mean. The first two columns are the saved baseline and the first candidate run. The last column gives the midpoint range of two more alternating baseline and candidate runs.

| Workload | Baseline interval | Candidate interval | Criterion result | Repeat midpoints, baseline / candidate |
|---|---:|---:|---|---|
| Six common rule lookups | 16.933 to 17.063 ns | 16.793 to 16.989 ns | Within noise threshold | Not repeated |
| Anonymize parsed 1 MiB object | 14.893 to 15.326 µs | 16.925 to 18.033 µs | 14.8% slower | 18.070, 18.352 / 22.040, 18.945 µs |
| Parse, anonymize, and write 1 MiB | 74.653 to 76.292 µs | 78.386 to 78.798 µs | 4.6% slower | 72.552, 73.912 / 91.402, 78.629 µs |
| Validate parsed 1 MiB object | 2.0080 to 2.0329 µs | 1.9696 to 1.9882 µs | 2.7% faster | 2.0084, 1.9940 / 2.2210, 2.0369 µs |
| Anonymize parsed 8-frame JPEG | 2.2814 to 2.3477 ms | 2.1600 to 2.3104 ms | Within noise threshold | 2.3452, 2.2712 / 2.4500, 2.1556 ms |
| Parse, anonymize, and write 8-frame JPEG | 3.5519 to 3.6854 ms | 3.4402 to 3.6947 ms | No change detected | 3.5759, 3.3320 / 3.6495, 3.2804 ms |
| Validate parsed 8-frame JPEG | 2.1734 to 2.2138 ms | 2.3408 to 2.5384 ms | 9.7% slower | 3.0952, 2.1336 / 2.1893, 2.1407 ms |

Run-to-run variation on this host was up to about 20% for the parsed-object and JPEG validation workloads, and the repeat runs do not reproduce the parsed 1 MiB and JPEG validation changes in a consistent direction. The new validation check sums fragment lengths and scans trailing zero bytes of the last fragment, which is small relative to the codestream parse. The 1 MiB parse/anonymize/write change is reproducible. An isolation run with and without only the source drop measured 72.184 to 72.624 µs and 71.569 to 71.796 µs without the drop, and 77.711 to 78.098 µs and 77.661 to 77.985 µs with the drop. The early release of a 1 MiB buffer before the encoder allocates its element copy adds about 6 µs on this allocator. The same isolation run showed no difference for the 8-frame JPEG workload.

Peak memory was measured separately with a synthetic 134,220,786-byte Part 10 file: 32 JPEG Lossless SV1 frames, each with one APP1 segment, one COM segment, and 4 MiB of scan data. A counting global allocator in a scratch program recorded peak live heap bytes after parsing. Anonymization peaked at 384.0 MiB before the change and 256.0 MiB after it, which is three and two Pixel Data copies. The `dicom-anonymize` release binary peaked at 417,218,560 to 417,267,712 bytes of maximum resident set size in three runs before the change, and at 281,952,256 to 282,001,408 bytes in three runs after it. Both binaries wrote byte-identical output. These are snapshots against a warm page cache, not statistical comparisons. No network operations occur.

Decision: ship. The change removes one complete Pixel Data copy from the peak memory of the rewrite path and of the file-to-file command, about 32% of peak resident memory for the 128 MiB fixture. This matters for multi-gigabyte breast tomosynthesis objects. The accepted cost is about 6 µs for each in-memory parse/anonymize/write of a 1 MiB native object. The remaining peak during encoding comes from the element copy in the `dicom-object` writer, and the parsed source still occupies one copy during anonymization because the public API borrows it.

## Safe Harbor UID resolver check

Issue #19 routes every `U` action through a per-object UID resolver and records the sequence item path for each nested dataset. The ordinary anonymization path adds one `RefCell` borrow per remapped UID and two per sequence item. A before-and-after run on October 17, 2026 used the unchanged workload with `--sample-size 100`. Master `7dda93a` was the saved baseline.

| Workload | Baseline 95% interval | Candidate 95% interval | Criterion result |
|---|---:|---:|---|
| Six common rule lookups | 16.989 to 17.119 ns | 17.942 to 18.093 ns | +5.6%, code unchanged |
| Anonymize parsed 1 MiB object | 18.470 to 19.364 µs | 14.965 to 15.622 µs | −19.9% |
| Parse, anonymize, and write 1 MiB | 73.925 to 74.496 µs | 73.617 to 74.221 µs | No change detected |
| Validate parsed 1 MiB object | 2.0111 to 2.0375 µs | 1.9506 to 1.9636 µs | −3.4% |

The rule-lookup and validation code did not change, so their shifts show the noise on this shared host. The parsed-object improvement is also inside that noise and is not an optimization claim. The end-to-end workload showed no change. Decision: ship the resolver. Path-aware errors and the shared listing and generation logic need it, and no workload showed a regression larger than host noise.
