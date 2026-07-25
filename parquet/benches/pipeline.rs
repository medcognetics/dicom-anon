use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use dicom_anon_parquet::{
    convert_directory, extract_metadata, snake_case, ColumnNaming, ConversionOptions,
    ExtractionOptions, SortMode,
};
use dicom_core::dictionary::{DataDictionary, DataDictionaryEntry};
use dicom_core::header::{HasLength, Header};
use dicom_core::{DicomValue, Tag};
use dicom_dictionary_std::{tags, StandardDataDictionary};
use dicom_json::DicomJson;
use dicom_object::{DefaultDicomObject, OpenFileOptions};
use xxhash_rust::xxh3::xxh3_64_with_seed;

const HASH_SEED: u64 = 0;
const BENCH_SAMPLE_SIZE: usize = 20;
const FIXTURE_COPIES: usize = 8;

fn fixture(name: &str) -> PathBuf {
    dicom_test_files::path(name).expect("benchmark fixture should be available")
}

fn extraction_options(hash_pixel_data: bool) -> ExtractionOptions {
    ExtractionOptions {
        hash_pixel_data,
        naming: ColumnNaming::SnakeCase,
        overrides: BTreeMap::new(),
    }
}

fn two_read_reference(path: &Path, hash_pixel_data: bool) -> usize {
    let schema_object = OpenFileOptions::new()
        .read_until(tags::PIXEL_DATA)
        .open_file(path)
        .expect("schema pass should parse");
    let object = OpenFileOptions::new()
        .read_until(if hash_pixel_data {
            Tag(0xFFFF, 0xFFFF)
        } else {
            tags::PIXEL_DATA
        })
        .open_file(path)
        .expect("row pass should parse");
    let schema = materialize_schema(&schema_object);
    let row = materialize_row(&object);
    if hash_pixel_data {
        let pixel_data = object
            .element(tags::PIXEL_DATA)
            .expect("fixture should contain Pixel Data");
        let hash = match pixel_data.value() {
            DicomValue::PixelSequence(sequence) => sequence
                .fragments()
                .iter()
                .map(Cow::from)
                .fold(0_u64, |sum, fragment| {
                    sum.wrapping_add(xxh3_64_with_seed(&fragment, HASH_SEED))
                }),
            DicomValue::Primitive(value) => xxh3_64_with_seed(value.to_bytes().as_ref(), HASH_SEED),
            DicomValue::Sequence(_) => unreachable!("Pixel Data cannot be a regular sequence"),
        };
        black_box(hash);
    }
    black_box(schema.len() + row.len())
}

fn materialize_schema(object: &DefaultDicomObject) -> Vec<String> {
    let mut schema = object
        .iter()
        .filter(|element| {
            element.tag() != tags::PIXEL_DATA
                && element.tag().group() % 2 == 0
                && !element.value().is_empty()
        })
        .filter_map(|element| {
            StandardDataDictionary
                .by_tag(element.tag())
                .map(|entry| snake_case(entry.alias()))
        })
        .collect::<Vec<_>>();
    schema.extend(
        object
            .meta()
            .to_element_iter()
            .filter(|element| !element.value().is_empty())
            .filter_map(|element| {
                StandardDataDictionary
                    .by_tag(element.tag())
                    .map(|entry| snake_case(entry.alias()))
            }),
    );
    schema
}

fn materialize_row(object: &DefaultDicomObject) -> BTreeMap<String, String> {
    let mut row = BTreeMap::new();
    for element in object.iter() {
        if element.tag() == tags::PIXEL_DATA
            || element.tag().group() % 2 == 1
            || element.value().is_empty()
        {
            continue;
        }
        let Some(entry) = StandardDataDictionary.by_tag(element.tag()) else {
            continue;
        };
        let value = match element.value() {
            DicomValue::Primitive(value) => value.to_string(),
            DicomValue::Sequence(sequence) => {
                let items = sequence
                    .items()
                    .iter()
                    .filter_map(|item| serde_json::to_value(DicomJson::from(item)).ok())
                    .map(|value| value.to_string())
                    .collect::<Vec<_>>();
                serde_json::to_string(&items).expect("sequence JSON should serialize")
            }
            DicomValue::PixelSequence(_) => continue,
        };
        row.insert(snake_case(entry.alias()), value);
    }
    for element in object.meta().to_element_iter() {
        let Some(entry) = StandardDataDictionary.by_tag(element.tag()) else {
            continue;
        };
        let Some(value) = element.into_value().into_primitive() else {
            continue;
        };
        if !value.is_empty() {
            row.insert(snake_case(entry.alias()), value.to_string());
        }
    }
    row
}

fn bench_extraction(c: &mut Criterion) {
    let mut group = c.benchmark_group("dicom_metadata_extraction");
    for (fixture_name, representation) in [
        ("pydicom/SC_rgb.dcm", "native"),
        ("pydicom/JPEG-LL.dcm", "encapsulated"),
    ] {
        let path = fixture(fixture_name);
        let bytes = fs::metadata(&path).unwrap().len();
        for hash in [false, true] {
            let mode = if hash { "hash_on" } else { "hash_off" };
            group.throughput(Throughput::Bytes(bytes));
            group.bench_with_input(
                BenchmarkId::new(format!("one_read_{representation}"), mode),
                &hash,
                |bench, hash| {
                    let options = extraction_options(*hash);
                    bench.iter(|| {
                        extract_metadata(black_box(&path), black_box(&options))
                            .expect("one-read extraction should succeed")
                    });
                },
            );
            group.bench_with_input(
                BenchmarkId::new(format!("two_read_reference_{representation}"), mode),
                &hash,
                |bench, hash| {
                    bench.iter(|| two_read_reference(black_box(&path), black_box(*hash)));
                },
            );
        }
    }
    group.finish();
}

fn bench_end_to_end(c: &mut Criterion) {
    let source = tempfile::tempdir().expect("benchmark source should be created");
    let source_fixture = fixture("pydicom/SC_rgb.dcm");
    for index in 0..FIXTURE_COPIES {
        fs::copy(
            &source_fixture,
            source.path().join(format!("{index:04}.dcm")),
        )
        .expect("fixture copy should succeed");
    }
    let input_bytes = fs::metadata(&source_fixture).unwrap().len() * FIXTURE_COPIES as u64;
    let mut group = c.benchmark_group("dicom_to_sharded_parquet");
    for hash in [false, true] {
        let options = ConversionOptions {
            hash_pixel_data: hash,
            naming: ColumnNaming::SnakeCase,
            sort: SortMode::Path,
            ..ConversionOptions::default()
        };
        group.throughput(Throughput::Bytes(input_bytes));
        group.bench_with_input(
            BenchmarkId::from_parameter(if hash { "hash_on" } else { "hash_off" }),
            &options,
            |bench, options| {
                bench.iter_batched(
                    || tempfile::tempdir().expect("benchmark output should be created"),
                    |output| {
                        convert_directory(
                            black_box(source.path()),
                            &output.path().join("output.parquet"),
                            black_box(options),
                        )
                        .expect("end-to-end conversion should succeed")
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

fn benchmark_config() -> Criterion {
    Criterion::default()
        .sample_size(BENCH_SAMPLE_SIZE)
        .warm_up_time(Duration::from_secs(3))
        .measurement_time(Duration::from_secs(10))
}

criterion_group!(
    name = benches;
    config = benchmark_config();
    targets = bench_extraction, bench_end_to_end
);
criterion_main!(benches);
