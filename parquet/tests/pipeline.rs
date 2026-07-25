use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use arrow::array::{Array, StringArray};
use arrow::datatypes::DataType;
use dicom_anon_parquet::{
    convert_directory, discover_dicom_files, extract_metadata, snake_case, ColumnNaming,
    ConversionOptions, ExtractionOptions, SortMode,
};
use dicom_core::value::DataSetSequence;
use dicom_core::{DataElement, PrimitiveValue, Tag, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::Compression;
use parquet::file::reader::{FileReader, SerializedFileReader};
use xxhash_rust::xxh3::xxh3_64_with_seed;

const OUTPUT_BASENAME: &str = "output.parquet";
const HASH_FIELD: &str = "PixelDataHash";
const SNAKE_HASH_FIELD: &str = "pixel_data_hash";

fn fixture(name: &str) -> PathBuf {
    dicom_test_files::path(name).expect("fixture should be available")
}

fn copy_fixture(source: &Path, destination: &Path) {
    fs::copy(source, destination).expect("fixture copy should succeed");
}

fn read_rows(paths: &[PathBuf]) -> Vec<BTreeMap<String, Option<String>>> {
    let mut rows = Vec::new();
    for path in paths {
        let file = File::open(path).expect("shard should open");
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)
            .expect("Parquet reader should build")
            .build()
            .expect("record batch reader should build");
        for batch in reader {
            let batch = batch.expect("batch should read");
            for row_index in 0..batch.num_rows() {
                let mut row = BTreeMap::new();
                for (column_index, field) in batch.schema().fields().iter().enumerate() {
                    let values = batch
                        .column(column_index)
                        .as_any()
                        .downcast_ref::<StringArray>()
                        .expect("all output columns should be UTF-8");
                    row.insert(
                        field.name().clone(),
                        (!values.is_null(row_index)).then(|| values.value(row_index).to_owned()),
                    );
                }
                rows.push(row);
            }
        }
    }
    rows
}

fn extraction_options(hash_pixel_data: bool, naming: ColumnNaming) -> ExtractionOptions {
    ExtractionOptions {
        hash_pixel_data,
        naming,
        overrides: BTreeMap::new(),
    }
}

#[test]
fn discovery_accepts_dicom_extensions_and_prefixed_extensionless_files_only() {
    let directory = tempfile::tempdir().expect("temp directory should be created");
    fs::write(directory.path().join("image.dcm"), b"candidate").unwrap();
    fs::write(directory.path().join("upper.DICOM"), b"candidate").unwrap();

    let mut prefixed = vec![0_u8; 128];
    prefixed.extend_from_slice(b"DICM");
    fs::write(directory.path().join("extensionless"), &prefixed).unwrap();
    fs::write(directory.path().join("extensionless-invalid"), b"not DICOM").unwrap();
    fs::write(directory.path().join("ignored.txt"), &prefixed).unwrap();

    let files = discover_dicom_files(directory.path(), SortMode::Path).unwrap();
    let names = files
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    assert_eq!(names, ["extensionless", "image.dcm", "upper.DICOM"]);
}

#[test]
fn snake_case_matches_baseline_acronym_boundaries() {
    for (input, expected) in [
        ("SOPInstanceUID", "sop_instance_uid"),
        ("StudyInstanceUID", "study_instance_uid"),
        ("TransferSyntaxUID", "transfer_syntax_uid"),
        ("FrameOfReferenceUID", "frame_of_reference_uid"),
        ("ThisUIDString", "this_uid_string"),
        ("", ""),
    ] {
        assert_eq!(snake_case(input), expected);
    }
}

#[test]
fn streamed_pixel_hashes_match_the_baseline_tool() {
    for (name, expected) in [
        ("pydicom/SC_rgb.dcm", 10_240_938_377_863_354_873_u64),
        ("pydicom/CT_small.dcm", 1_951_840_166_433_920_010),
        ("pydicom/vlut_04.dcm", 7_432_445_857_076_937_611),
        ("pydicom/JPEG-LL.dcm", 9_362_116_028_437_860_605),
        ("pydicom/JPEG2000_UNC.dcm", 16_200_283_765_698_159_274),
        (
            "pydicom/MR-SIEMENS-DICOM-WithOverlays.dcm",
            865_158_459_484_170_052,
        ),
    ] {
        let row = extract_metadata(
            &fixture(name),
            &extraction_options(true, ColumnNaming::Dicom),
        )
        .unwrap();
        let expected = expected.to_string();
        assert_eq!(
            row.values.get(HASH_FIELD).map(String::as_str),
            Some(expected.as_str()),
            "{name}"
        );
    }
}

#[test]
fn empty_native_pixel_data_hash_matches_the_baseline() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("empty-pixels.dcm");
    let mut object = InMemDicomObject::new_empty();
    object.put_str(
        tags::SOP_CLASS_UID,
        VR::UI,
        uids::SECONDARY_CAPTURE_IMAGE_STORAGE,
    );
    object.put_str(tags::SOP_INSTANCE_UID, VR::UI, "1.2.826.0.1.0");
    object.put_str(tags::STUDY_INSTANCE_UID, VR::UI, "1.2.826.0.1");
    object.put(DataElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        PrimitiveValue::Empty,
    ));
    object
        .with_meta(FileMetaTableBuilder::new().transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN))
        .unwrap()
        .write_to_file(&path)
        .unwrap();

    let row = extract_metadata(&path, &extraction_options(true, ColumnNaming::Dicom)).unwrap();

    assert_eq!(
        row.values.get(HASH_FIELD),
        Some(&xxh3_64_with_seed(&[], 0).to_string())
    );
    assert_eq!(row.stats.hashed_pixel_bytes, 0);
}

#[test]
fn hash_off_skips_pixels_and_keeps_metadata_after_pixel_data() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("post-pixel.dcm");
    let mut object = InMemDicomObject::new_empty();
    object.put_str(
        tags::SOP_CLASS_UID,
        VR::UI,
        uids::SECONDARY_CAPTURE_IMAGE_STORAGE,
    );
    object.put_str(tags::SOP_INSTANCE_UID, VR::UI, "1.2.826.0.1.1");
    object.put_str(tags::STUDY_INSTANCE_UID, VR::UI, "1.2.826.0.1");
    object.put(DataElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        PrimitiveValue::from(vec![7_u8; 1_048_576]),
    ));
    let mut signature = InMemDicomObject::new_empty();
    signature.put(DataElement::new(
        tags::MACID_NUMBER,
        VR::US,
        PrimitiveValue::from(7_u16),
    ));
    object.put(DataElement::new(
        tags::DIGITAL_SIGNATURES_SEQUENCE,
        VR::SQ,
        DataSetSequence::from(vec![signature]),
    ));
    object
        .with_meta(FileMetaTableBuilder::new().transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN))
        .unwrap()
        .write_to_file(&path)
        .unwrap();

    let row = extract_metadata(&path, &extraction_options(false, ColumnNaming::Dicom)).unwrap();

    assert_eq!(row.stats.hashed_pixel_bytes, 0);
    assert!(!row.values.contains_key("PixelData"));
    assert!(!row.values.contains_key(HASH_FIELD));
    assert!(row.values.contains_key("DigitalSignaturesSequence"));
}

#[test]
fn pixel_data_cannot_be_reintroduced_with_an_override() {
    let mut options = extraction_options(false, ColumnNaming::Dicom);
    options
        .overrides
        .insert(tags::PIXEL_DATA, "forbidden".to_owned());

    let error = extract_metadata(&fixture("pydicom/SC_rgb.dcm"), &options).unwrap_err();

    assert!(error.to_string().contains("Pixel Data"));
}

#[test]
fn nested_sequence_json_omits_private_attributes() {
    const PRIVATE_TAG: Tag = Tag(0x0009, 0x1001);
    const PRIVATE_VALUE: &str = "PRIVATE-SECRET";

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("nested-private.dcm");
    let mut object = InMemDicomObject::new_empty();
    object.put_str(
        tags::SOP_CLASS_UID,
        VR::UI,
        uids::SECONDARY_CAPTURE_IMAGE_STORAGE,
    );
    object.put_str(tags::SOP_INSTANCE_UID, VR::UI, "1.2.826.0.1.2");
    object.put_str(tags::STUDY_INSTANCE_UID, VR::UI, "1.2.826.0.1");
    let mut nested = InMemDicomObject::new_empty();
    nested.put(DataElement::new(
        tags::MACID_NUMBER,
        VR::US,
        PrimitiveValue::from(7_u16),
    ));
    nested.put_str(PRIVATE_TAG, VR::LO, PRIVATE_VALUE);
    object.put(DataElement::new(
        tags::DIGITAL_SIGNATURES_SEQUENCE,
        VR::SQ,
        DataSetSequence::from(vec![nested]),
    ));
    object
        .with_meta(FileMetaTableBuilder::new().transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN))
        .unwrap()
        .write_to_file(&path)
        .unwrap();

    let row = extract_metadata(&path, &extraction_options(false, ColumnNaming::Dicom)).unwrap();
    let sequence_json = row.values.get("DigitalSignaturesSequence").unwrap();

    assert!(sequence_json.contains("04000005"));
    assert!(!sequence_json.contains(PRIVATE_VALUE));
    assert!(!sequence_json.contains("00091001"));
}

#[test]
fn conversion_is_metadata_only_uniform_utf8_zstd_and_accepts_extensionless_dicom() {
    let input = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    copy_fixture(
        &fixture("pydicom/SC_rgb.dcm"),
        &input.path().join("extensionless"),
    );
    copy_fixture(
        &fixture("pydicom/CT_small.dcm"),
        &input.path().join("second.dcm"),
    );
    fs::write(input.path().join("ignored.txt"), b"not DICOM").unwrap();

    let options = ConversionOptions {
        hash_pixel_data: true,
        naming: ColumnNaming::SnakeCase,
        sort: SortMode::Path,
        ..ConversionOptions::default()
    };
    let summary =
        convert_directory(input.path(), &output.path().join(OUTPUT_BASENAME), &options).unwrap();

    assert_eq!(summary.discovered_files, 2);
    assert_eq!(summary.processed_files, 2);
    assert_eq!(summary.failed_files, 0);
    assert_eq!(summary.shard_paths.len(), 1);
    assert_eq!(
        summary.shard_paths[0].file_name().unwrap(),
        "output.00000.parquet"
    );

    let parquet = SerializedFileReader::new(File::open(&summary.shard_paths[0]).unwrap()).unwrap();
    for row_group in parquet.metadata().row_groups() {
        for column in row_group.columns() {
            assert_eq!(column.compression(), Compression::ZSTD(Default::default()));
        }
    }

    let file = File::open(&summary.shard_paths[0]).unwrap();
    let schema = ParquetRecordBatchReaderBuilder::try_new(file)
        .unwrap()
        .schema()
        .clone();
    assert!(schema
        .fields()
        .iter()
        .all(|field| field.data_type() == &DataType::Utf8));
    assert!(schema.field_with_name("pixel_data").is_err());
    assert!(schema.field_with_name(SNAKE_HASH_FIELD).is_ok());

    let rows = read_rows(&summary.shard_paths);
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .all(|row| row.get(SNAKE_HASH_FIELD).and_then(Option::as_ref).is_some()));
}

#[test]
fn best_effort_reports_bad_candidates_and_strict_mode_writes_nothing() {
    let input = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    copy_fixture(
        &fixture("pydicom/SC_rgb.dcm"),
        &input.path().join("valid.dcm"),
    );
    fs::write(input.path().join("invalid.dcm"), b"not DICOM").unwrap();

    let best_effort = convert_directory(
        input.path(),
        &output.path().join(OUTPUT_BASENAME),
        &ConversionOptions {
            sort: SortMode::Path,
            ..ConversionOptions::default()
        },
    )
    .unwrap();
    assert_eq!(best_effort.processed_files, 1);
    assert_eq!(best_effort.failed_files, 1);
    assert_eq!(read_rows(&best_effort.shard_paths).len(), 1);

    let strict_output = output.path().join("strict.parquet");
    let strict_result = convert_directory(
        input.path(),
        &strict_output,
        &ConversionOptions {
            strict: true,
            sort: SortMode::Path,
            ..ConversionOptions::default()
        },
    );
    assert!(strict_result.is_err());
    assert!(!output.path().join("strict.00000.parquet").exists());
}

#[test]
fn big_endian_word_pixels_hash_like_the_baseline_in_memory_value() {
    const BIG_ENDIAN_TRANSFER_SYNTAX: &str = "1.2.840.10008.1.2.2";
    const PIXELS: [u16; 4] = [0x0102, 0xABCD, 0x0042, 0xFFFF];

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("big-endian.dcm");
    let mut object = InMemDicomObject::new_empty();
    object.put_str(
        tags::SOP_CLASS_UID,
        VR::UI,
        uids::SECONDARY_CAPTURE_IMAGE_STORAGE,
    );
    object.put_str(tags::SOP_INSTANCE_UID, VR::UI, "1.2.826.0.2.1");
    object.put_str(tags::STUDY_INSTANCE_UID, VR::UI, "1.2.826.0.2");
    object.put(DataElement::new(
        tags::PIXEL_DATA,
        VR::OW,
        PrimitiveValue::U16(PIXELS.as_slice().into()),
    ));
    object
        .with_meta(FileMetaTableBuilder::new().transfer_syntax(BIG_ENDIAN_TRANSFER_SYNTAX))
        .unwrap()
        .write_to_file(&path)
        .unwrap();

    let row = extract_metadata(&path, &extraction_options(true, ColumnNaming::Dicom)).unwrap();
    let native_bytes = PIXELS
        .iter()
        .flat_map(|pixel| pixel.to_ne_bytes())
        .collect::<Vec<_>>();
    assert_eq!(
        row.values.get(HASH_FIELD),
        Some(&xxh3_64_with_seed(&native_bytes, 0).to_string())
    );
}

#[test]
fn part10_without_a_preamble_is_still_parsed_for_extension_candidates() {
    let directory = tempfile::tempdir().unwrap();
    let source = fs::read(fixture("pydicom/SC_rgb.dcm")).unwrap();
    assert_eq!(&source[128..132], b"DICM");
    let path = directory.path().join("no-preamble.dcm");
    fs::write(&path, &source[128..]).unwrap();

    let row = extract_metadata(&path, &extraction_options(false, ColumnNaming::Dicom)).unwrap();
    assert!(row.values.contains_key("SOPInstanceUID"));
}

#[test]
fn missing_required_uids_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing-study.dcm");
    let mut object = InMemDicomObject::new_empty();
    object.put_str(
        tags::SOP_CLASS_UID,
        VR::UI,
        uids::SECONDARY_CAPTURE_IMAGE_STORAGE,
    );
    object.put_str(tags::SOP_INSTANCE_UID, VR::UI, "1.2.826.0.3.1");
    object
        .with_meta(FileMetaTableBuilder::new().transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN))
        .unwrap()
        .write_to_file(&path)
        .unwrap();

    let error =
        extract_metadata(&path, &extraction_options(false, ColumnNaming::Dicom)).unwrap_err();
    assert!(format!("{error:?}").contains("StudyInstanceUID"));
}

#[test]
fn directory_and_option_validation_fail_before_output() {
    let directory = tempfile::tempdir().unwrap();
    let file_source = directory.path().join("source.dcm");
    fs::write(&file_source, b"not relevant").unwrap();
    let output = directory.path().join("output.parquet");

    assert!(
        convert_directory(&file_source, &output, &ConversionOptions::default())
            .unwrap_err()
            .to_string()
            .contains("not a directory")
    );
    assert!(convert_directory(
        directory.path(),
        &directory.path().join("missing/output.parquet"),
        &ConversionOptions::default(),
    )
    .unwrap_err()
    .to_string()
    .contains("output directory"));
    assert!(convert_directory(
        directory.path(),
        &output,
        &ConversionOptions {
            compression_level: 0,
            ..ConversionOptions::default()
        },
    )
    .is_err());
    assert!(convert_directory(
        directory.path(),
        &output,
        &ConversionOptions {
            shard_size_mib: 0,
            ..ConversionOptions::default()
        },
    )
    .is_err());
    assert!(convert_directory(
        directory.path(),
        &output,
        &ConversionOptions {
            batch_rows: 0,
            ..ConversionOptions::default()
        },
    )
    .is_err());
}

#[test]
fn empty_and_all_invalid_candidate_sets_have_distinct_results() {
    let empty = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    assert!(convert_directory(
        empty.path(),
        &output.path().join("empty.parquet"),
        &ConversionOptions::default(),
    )
    .unwrap_err()
    .to_string()
    .contains("no DICOM files"));

    fs::write(empty.path().join("invalid.dcm"), b"not DICOM").unwrap();
    let error = convert_directory(
        empty.path(),
        &output.path().join("invalid.parquet"),
        &ConversionOptions::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("no usable DICOM files"));
    assert!(!output.path().join("invalid.00000.parquet").exists());
}

#[test]
fn existing_shards_are_never_overwritten() {
    let input = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    copy_fixture(
        &fixture("pydicom/SC_rgb.dcm"),
        &input.path().join("image.dcm"),
    );
    let existing = output.path().join("result.00000.parquet");
    fs::write(&existing, b"keep me").unwrap();

    let error = convert_directory(
        input.path(),
        &output.path().join("result.parquet"),
        &ConversionOptions::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("already exists"));
    assert_eq!(fs::read(existing).unwrap(), b"keep me");
}

#[test]
fn inode_and_none_sort_modes_discover_nested_candidates() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("nested")).unwrap();
    fs::write(directory.path().join("first.dcm"), b"candidate").unwrap();
    fs::write(
        directory.path().join("nested").join("second.dicom"),
        b"candidate",
    )
    .unwrap();

    for sort in [SortMode::Inode, SortMode::None] {
        assert_eq!(
            discover_dicom_files(directory.path(), sort).unwrap().len(),
            2
        );
    }
}
