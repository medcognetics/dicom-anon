use std::hint::black_box;
use std::io::Cursor;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use dicom_anon::{anonymize_reader, profile_rule, validate_object, Anonymizer, Options, SecretKey};
use dicom_core::value::{PixelFragmentSequence, Value};
use dicom_core::{DataElement, PrimitiveValue, Tag, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

const PIXEL_BYTES: usize = 1024 * 1024;
const KEY: [u8; 32] = [17; 32];
const JPEG_FRAMES: usize = 8;

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

/// JPEG marker segment with a big-endian length that includes the length field.
fn jpeg_segment(marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut output = vec![0xFF, marker];
    output.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
    output.extend_from_slice(payload);
    output
}

/// Synthetic lossless JPEG frame with removable APP1 and COM segments and 1 MiB of scan data.
fn jpeg_frame(index: usize) -> Vec<u8> {
    let mut frame = vec![0xFF, 0xD8];
    frame.extend(jpeg_segment(0xE1, b"Exif\0\0SYNTHETIC-DEVICE"));
    frame.extend(jpeg_segment(
        0xFE,
        format!("SYNTHETIC FRAME {index}").as_bytes(),
    ));
    frame.extend(jpeg_segment(0xC3, &[8, 4, 0, 4, 0, 1, 1, 0x11, 0]));
    frame.extend(jpeg_segment(0xC4, &[0x00, 0x01]));
    frame.extend(jpeg_segment(0xDA, &[1, 1, 0, 1, 0, 0]));
    frame.extend(std::iter::repeat_n(0x34, PIXEL_BYTES));
    frame.extend_from_slice(&[0xFF, 0xD9]);
    if frame.len() % 2 == 1 {
        frame.push(0);
    }
    frame
}

/// Multi-frame JPEG Lossless object whose every frame has removable metadata segments.
fn encapsulated_jpeg_file() -> (dicom_object::DefaultDicomObject, Vec<u8>) {
    let (native, _) = representative_file();
    let mut object = native.into_inner();
    let fragments: Vec<Vec<u8>> = (0..JPEG_FRAMES).map(jpeg_frame).collect();
    let mut offsets = Vec::with_capacity(JPEG_FRAMES);
    let mut offset = 0_u32;
    for fragment in &fragments {
        offsets.push(offset);
        offset += 8 + u32::try_from(fragment.len()).unwrap();
    }
    let total = fragments.iter().map(Vec::len).sum::<usize>() as u64;
    object.put_str(tags::NUMBER_OF_FRAMES, VR::IS, JPEG_FRAMES.to_string());
    object.put(DataElement::new(
        tags::ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH,
        VR::UV,
        PrimitiveValue::U64(vec![total].into()),
    ));
    object.put(DataElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        Value::PixelSequence(PixelFragmentSequence::new(offsets, fragments)),
    ));
    let object = object
        .with_meta(FileMetaTableBuilder::new().transfer_syntax(uids::JPEG_LOSSLESS_SV1))
        .unwrap();
    let mut bytes = Vec::new();
    object.write_all(&mut bytes).unwrap();
    (object, bytes)
}

fn encapsulated_segment_removal(c: &mut Criterion) {
    let (source, bytes) = encapsulated_jpeg_file();
    let anonymizer = Anonymizer::new(Options::new(SecretKey::new(KEY).unwrap())).unwrap();
    let (anonymized, report) = anonymizer.anonymize(&source).unwrap();
    assert_eq!(report.removed_jpeg_comment_segments, JPEG_FRAMES as u64);
    assert_eq!(report.removed_jpeg_application_segments, JPEG_FRAMES as u64);

    let mut group = c.benchmark_group("dicom_processing_jpeg_8x1m_segments");
    group.sample_size(20);
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

criterion_group!(
    benches,
    profile_lookup,
    end_to_end,
    encapsulated_segment_removal
);
criterion_main!(benches);
