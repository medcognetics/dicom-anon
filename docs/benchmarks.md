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
