//! Generate a small uncompressed DICOM fixture for examples and smoke tests.

use std::path::PathBuf;

use dicom_core::{DataElement, PrimitiveValue, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

const PIXEL_ROWS: u16 = 1_024;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let output = arguments
        .next()
        .map(PathBuf::from)
        .ok_or("usage: generate_minimal OUTPUT [PIXEL_BYTES]")?;
    let pixel_bytes = arguments
        .next()
        .map(|value| value.to_string_lossy().parse::<usize>())
        .transpose()?;
    let mut object = InMemDicomObject::new_empty();
    object.put_str(
        tags::SOP_CLASS_UID,
        VR::UI,
        uids::SECONDARY_CAPTURE_IMAGE_STORAGE,
    );
    object.put_str(tags::SOP_INSTANCE_UID, VR::UI, "1.2.840.99999.1.1");
    object.put_str(tags::STUDY_INSTANCE_UID, VR::UI, "1.2.840.99999.1");
    object.put_str(tags::SERIES_INSTANCE_UID, VR::UI, "1.2.840.99999.1.2");
    object.put_str(tags::PATIENT_ID, VR::LO, "EXAMPLE-PATIENT");
    object.put_str(tags::PATIENT_NAME, VR::PN, "Example^Patient");
    object.put_str(tags::STUDY_DATE, VR::DA, "20240101");
    object.put_str(tags::MODALITY, VR::CS, "OT");
    if let Some(pixel_bytes) = pixel_bytes {
        if pixel_bytes % usize::from(PIXEL_ROWS) != 0 {
            return Err("PIXEL_BYTES must be divisible by 1024".into());
        }
        let columns = u16::try_from(pixel_bytes / usize::from(PIXEL_ROWS))?;
        object.put(DataElement::new(
            tags::ROWS,
            VR::US,
            PrimitiveValue::from(PIXEL_ROWS),
        ));
        object.put(DataElement::new(
            tags::COLUMNS,
            VR::US,
            PrimitiveValue::from(columns),
        ));
        object.put(DataElement::new(
            tags::BITS_ALLOCATED,
            VR::US,
            PrimitiveValue::from(8_u16),
        ));
        object.put(DataElement::new(
            tags::BITS_STORED,
            VR::US,
            PrimitiveValue::from(8_u16),
        ));
        object.put(DataElement::new(
            tags::HIGH_BIT,
            VR::US,
            PrimitiveValue::from(7_u16),
        ));
        object.put(DataElement::new(
            tags::SAMPLES_PER_PIXEL,
            VR::US,
            PrimitiveValue::from(1_u16),
        ));
        object.put(DataElement::new(
            tags::PIXEL_REPRESENTATION,
            VR::US,
            PrimitiveValue::from(0_u16),
        ));
        object.put_str(tags::PHOTOMETRIC_INTERPRETATION, VR::CS, "MONOCHROME2");
        object.put(DataElement::new(
            tags::PIXEL_DATA,
            VR::OB,
            PrimitiveValue::from(vec![127_u8; pixel_bytes]),
        ));
    }
    object
        .with_meta(FileMetaTableBuilder::new().transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN))?
        .write_to_file(output)?;
    Ok(())
}
