//! DICOM stream and atomic file I/O.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use dicom_object::DefaultDicomObject;

use crate::{AnonymizationReport, Anonymizer, Error, Result};

/// Parse a DICOM Part 10 object from a stream.
pub fn read_dicom(reader: impl Read) -> Result<DefaultDicomObject> {
    dicom_object::from_reader(reader).map_err(|error| Error::Read(error.to_string()))
}

/// Encode a complete DICOM Part 10 object to a stream.
pub fn write_dicom(object: &DefaultDicomObject, writer: impl Write) -> Result<()> {
    object
        .write_all(writer)
        .map_err(|error| Error::Write(error.to_string()))
}

/// Read, de-identify, and encode a DICOM object entirely in memory.
pub fn anonymize_reader(
    anonymizer: &Anonymizer,
    reader: impl Read,
) -> Result<(Vec<u8>, AnonymizationReport)> {
    let source = read_dicom(reader)?;
    let (output, report) = anonymizer.anonymize(&source)?;
    let mut bytes = Vec::new();
    write_dicom(&output, &mut bytes)?;
    Ok((bytes, report))
}

/// De-identify one file and atomically replace the destination.
///
/// Passing the same input and output path performs a safe in-place update.
pub fn anonymize_file(
    anonymizer: &Anonymizer,
    input: impl AsRef<Path>,
    output: impl AsRef<Path>,
) -> Result<AnonymizationReport> {
    let input = input.as_ref();
    let output = output.as_ref();
    let source = File::open(input).map_err(|source| io_error(input, source))?;
    let object = read_dicom(source)?;
    let (anonymized, report) = anonymizer.anonymize(&object)?;

    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| io_error(parent, source))?;
    if same_path(input, output) {
        let permissions = std::fs::metadata(input)
            .map_err(|source| io_error(input, source))?
            .permissions();
        temporary
            .as_file()
            .set_permissions(permissions)
            .map_err(|source| io_error(temporary.path(), source))?;
    }
    write_dicom(&anonymized, temporary.as_file_mut())?;
    temporary
        .as_file_mut()
        .flush()
        .map_err(|source| io_error(temporary.path(), source))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| io_error(temporary.path(), source))?;
    temporary
        .persist(output)
        .map_err(|error| io_error(output, error.error))?;
    Ok(report)
}

fn same_path(left: &Path, right: &Path) -> bool {
    canonical_or_absolute(left) == canonical_or_absolute(right)
}

fn canonical_or_absolute(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir().unwrap_or_default().join(path)
        }
    })
}

fn io_error(path: &Path, source: std::io::Error) -> Error {
    Error::Io {
        path: path.display().to_string(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dicom_core::VR;
    use dicom_dictionary_std::{tags, uids};
    use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

    const KEY: [u8; 32] = [13; 32];

    fn source_bytes() -> Vec<u8> {
        let mut object = InMemDicomObject::new_empty();
        object.put_str(
            tags::SOP_CLASS_UID,
            VR::UI,
            uids::SECONDARY_CAPTURE_IMAGE_STORAGE,
        );
        object.put_str(tags::SOP_INSTANCE_UID, VR::UI, "1.2.3.4");
        object.put_str(tags::STUDY_INSTANCE_UID, VR::UI, "1.2.3");
        object.put_str(tags::SERIES_INSTANCE_UID, VR::UI, "1.2.3.1");
        object.put_str(tags::PATIENT_ID, VR::LO, "patient");
        object.put_str(tags::STUDY_DATE, VR::DA, "20200101");
        let object = object
            .with_meta(FileMetaTableBuilder::new().transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN))
            .unwrap();
        let mut bytes = Vec::new();
        object.write_all(&mut bytes).unwrap();
        bytes
    }

    fn anonymizer() -> Anonymizer {
        Anonymizer::new(crate::Options::new(crate::SecretKey::new(KEY).unwrap())).unwrap()
    }

    #[test]
    fn stream_helpers_round_trip_and_reject_invalid_input() {
        assert!(matches!(read_dicom(&b"not dicom"[..]), Err(Error::Read(_))));
        let (bytes, report) = anonymize_reader(&anonymizer(), &source_bytes()[..]).unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(report.profile_version, crate::PROFILE_VERSION);
        let output = read_dicom(&bytes[..]).unwrap();
        let mut encoded = Vec::new();
        write_dicom(&output, &mut encoded).unwrap();
        assert_eq!(bytes, encoded);
    }

    #[test]
    fn file_helper_handles_out_of_place_and_in_place_paths() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.dcm");
        let output = directory.path().join("output.dcm");
        std::fs::write(&input, source_bytes()).unwrap();

        anonymize_file(&anonymizer(), &input, &output).unwrap();
        assert!(crate::validate_file(&output).unwrap().conformant);

        std::fs::write(&input, source_bytes()).unwrap();
        anonymize_file(&anonymizer(), &input, &input).unwrap();
        assert!(crate::validate_file(&input).unwrap().conformant);

        let missing = directory.path().join("missing.dcm");
        assert!(matches!(
            anonymize_file(&anonymizer(), &missing, &output),
            Err(Error::Io { .. })
        ));
    }

    #[test]
    fn path_normalization_handles_existing_absolute_and_new_relative_paths() {
        let directory = tempfile::tempdir().unwrap();
        let existing = directory.path().join("existing");
        std::fs::write(&existing, b"").unwrap();
        assert_eq!(
            canonical_or_absolute(&existing),
            existing.canonicalize().unwrap()
        );
        assert_eq!(
            canonical_or_absolute(Path::new("new-file"))
                .file_name()
                .unwrap(),
            "new-file"
        );
        assert_eq!(
            canonical_or_absolute(&directory.path().join("absolute-new")),
            directory.path().join("absolute-new")
        );
    }
}
