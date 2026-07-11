use std::hint::black_box;
use std::io::Cursor;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use dicom_anon::{anonymize_reader, profile_rule, validate_object, Anonymizer, Options, SecretKey};
use dicom_core::{PrimitiveValue, Tag, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

const PIXEL_BYTES: usize = 1024 * 1024;
const KEY: [u8; 32] = [17; 32];

fn profile_lookup(c: &mut Criterion) {
    let common_tags = [
        Tag(0x0010, 0x0010),
        Tag(0x0010, 0x0020),
        Tag(0x0008, 0x0020),
        Tag(0x0020, 0x000D),
        Tag(0x0008, 0x0018),
        Tag(0x7FE0, 0x0010),
    ];
    let mut group = c.benchmark_group("profile_rule_lookup");
    group.throughput(Throughput::Elements(common_tags.len() as u64));
    group.bench_with_input(
        BenchmarkId::new("grouped_dispatch", common_tags.len()),
        &common_tags,
        |bencher, tags| {
            bencher.iter(|| {
                for tag in tags {
                    black_box(profile_rule(black_box(*tag)));
                }
            });
        },
    );
    group.finish();
}

fn representative_file() -> (dicom_object::DefaultDicomObject, Vec<u8>) {
    let mut object = InMemDicomObject::new_empty();
    object.put_str(
        tags::SOP_CLASS_UID,
        VR::UI,
        uids::SECONDARY_CAPTURE_IMAGE_STORAGE,
    );
    object.put_str(tags::SOP_INSTANCE_UID, VR::UI, "1.2.840.99999.1.1");
    object.put_str(tags::STUDY_INSTANCE_UID, VR::UI, "1.2.840.99999.1");
    object.put_str(tags::SERIES_INSTANCE_UID, VR::UI, "1.2.840.99999.1.2");
    object.put_str(tags::PATIENT_ID, VR::LO, "BENCHMARK-PATIENT");
    object.put_str(tags::PATIENT_NAME, VR::PN, "Benchmark^Patient");
    object.put_str(tags::STUDY_DATE, VR::DA, "20240101");
    object.put_str(tags::SERIES_DATE, VR::DA, "20240102");
    object.put_str(tags::ACQUISITION_DATE, VR::DA, "20240103");
    object.put_str(tags::MODALITY, VR::CS, "OT");
    object.put_str(tags::ROWS, VR::US, "1024");
    object.put_str(tags::COLUMNS, VR::US, "1024");
    object.put_str(tags::BITS_ALLOCATED, VR::US, "8");
    object.put_str(Tag(0x0011, 0x0010), VR::LO, "PRIVATE CREATOR");
    object.put_str(Tag(0x0011, 0x1010), VR::LO, "PRIVATE VALUE");
    object.put(dicom_core::DataElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        PrimitiveValue::from(vec![127_u8; PIXEL_BYTES]),
    ));
    let object = object
        .with_meta(FileMetaTableBuilder::new().transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN))
        .unwrap();
    let mut bytes = Vec::new();
    object.write_all(&mut bytes).unwrap();
    (object, bytes)
}

fn end_to_end(c: &mut Criterion) {
    let (source, bytes) = representative_file();
    let anonymizer = Anonymizer::new(Options::new(SecretKey::new(KEY).unwrap())).unwrap();
    let (anonymized, _) = anonymizer.anonymize(&source).unwrap();

    let mut group = c.benchmark_group("dicom_processing_1m_pixel");
    group.sample_size(50);
    group.throughput(Throughput::Bytes(bytes.len() as u64));
    group.bench_function("anonymize_object", |bencher| {
        bencher.iter(|| anonymizer.anonymize(black_box(&source)).unwrap());
    });
    group.bench_function("anonymize_parse_write", |bencher| {
        bencher.iter(|| {
            anonymize_reader(&anonymizer, Cursor::new(black_box(bytes.as_slice()))).unwrap()
        });
    });
    group.bench_function("validate_object", |bencher| {
        bencher.iter(|| validate_object(black_box(&anonymized)));
    });
    group.finish();
}

criterion_group!(benches, profile_lookup, end_to_end);
criterion_main!(benches);
