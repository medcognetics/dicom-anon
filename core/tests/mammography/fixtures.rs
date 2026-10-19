//! Deterministic synthetic mammography fixtures.
//!
//! The objects use placeholder text and fixed `2.25` UUID-derived UIDs only. Every identifier
//! value contains [`PLACEHOLDER`] so that a recursive output scan can find any retained value.
//! The JPEG 2000 Pixel Data fragments are opaque synthetic bytes; no test decodes them.

use dicom_core::value::{DataSetSequence, PixelFragmentSequence};
use dicom_core::{DataElement, PrimitiveValue, Tag, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{DefaultDicomObject, FileMetaTableBuilder, InMemDicomObject};

/// Marker text in every placeholder identifier value.
pub const PLACEHOLDER: &str = "PHIPLACEHOLDER";

pub const JPEG_2000_LOSSLESS: &str = uids::JPEG2000_LOSSLESS;
pub const MG_FOR_PRESENTATION: &str =
    uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION;
pub const BREAST_TOMOSYNTHESIS: &str = uids::BREAST_TOMOSYNTHESIS_IMAGE_STORAGE;

pub const STUDY: &str = "2.25.210984244814974183529861740079675587945";
pub const SERIES_MG: &str = "2.25.201833201629288074079120896699571356319";
pub const SERIES_DBT: &str = "2.25.21114888448868935956878602175919478444";
pub const SERIES_SYNTHESIZED: &str = "2.25.98874656527168277097211593304718648072";
pub const SOP_MG: &str = "2.25.309255262607761120477397225530494604531";
pub const SOP_DBT: &str = "2.25.38882444943042484871042846103488966940";
pub const SOP_SYNTHESIZED: &str = "2.25.227806567056346720553890928495465829260";
pub const FRAME_OF_REFERENCE: &str = "2.25.43431546053537617066324693989241495221";
pub const DIMENSION_ORGANIZATION: &str = "2.25.193932060374869486928755781973294209458";
pub const IRRADIATION_MG: &str = "2.25.92511808622166677347526962320623309077";
pub const IRRADIATION_DBT: &str = "2.25.55326387672220098226473384195354741449";
pub const META_PRIVATE_CREATOR: &str = "2.25.168051611731127086742947718903262501060";
pub const REFERENCED_STUDY: &str = "2.25.269393523532887438344010613905772096798";
pub const DEVICE: &str = "2.25.232199849780097777472964749570931662673";

/// Every source UID that identifies an instance, study, series, device, or organization.
pub const SOURCE_UIDS: [&str; 14] = [
    STUDY,
    SERIES_MG,
    SERIES_DBT,
    SERIES_SYNTHESIZED,
    SOP_MG,
    SOP_DBT,
    SOP_SYNTHESIZED,
    FRAME_OF_REFERENCE,
    DIMENSION_ORGANIZATION,
    IRRADIATION_MG,
    IRRADIATION_DBT,
    META_PRIVATE_CREATOR,
    REFERENCED_STUDY,
    DEVICE,
];

/// Image Pixel module attributes, other than Pixel Data, that must be identical in the output.
pub const PIXEL_MODULE_TAGS: [Tag; 10] = [
    tags::SAMPLES_PER_PIXEL,
    tags::PHOTOMETRIC_INTERPRETATION,
    tags::NUMBER_OF_FRAMES,
    tags::ROWS,
    tags::COLUMNS,
    tags::BITS_ALLOCATED,
    tags::BITS_STORED,
    tags::HIGH_BIT,
    tags::PIXEL_REPRESENTATION,
    tags::LOSSY_IMAGE_COMPRESSION,
];

/// Root-level view, laterality, and image-type attributes that must stay unchanged.
pub const VIEW_TAGS: [Tag; 5] = [
    tags::IMAGE_LATERALITY,
    tags::LATERALITY,
    tags::VIEW_POSITION,
    tags::PRESENTATION_INTENT_TYPE,
    tags::IMAGE_TYPE,
];

const PRIVATE_CREATOR: &str = "PLACEHOLDER VENDOR";
/// Retired Detached Study Management SOP Class, which legacy Referenced Study Sequence items use.
const DETACHED_STUDY_MANAGEMENT: &str = "1.2.840.10008.3.1.2.3.1";

/// The three synthetic mammography objects.
pub struct Fixtures {
    pub mammogram: DefaultDicomObject,
    pub tomosynthesis: DefaultDicomObject,
    pub synthesized: DefaultDicomObject,
}

impl Fixtures {
    pub fn new() -> Self {
        Self {
            mammogram: mammogram(),
            tomosynthesis: tomosynthesis(),
            synthesized: synthesized(),
        }
    }

    pub fn named(&self) -> [(&'static str, &DefaultDicomObject); 3] {
        [
            ("mg-for-presentation", &self.mammogram),
            ("dbt", &self.tomosynthesis),
            ("synthesized-2d", &self.synthesized),
        ]
    }
}

/// Digital Mammography X-Ray Image Storage - For Presentation, one JPEG 2000 Lossless frame.
pub fn mammogram() -> DefaultDicomObject {
    let mut object = common(MG_FOR_PRESENTATION, SOP_MG, SERIES_MG, IRRADIATION_MG);
    object.put_str(tags::IMAGE_TYPE, VR::CS, "ORIGINAL\\PRIMARY");
    object.put_str(tags::IMAGE_LATERALITY, VR::CS, "R");
    object.put_str(tags::VIEW_POSITION, VR::CS, "MLO");
    object.put(view_code("399368009", "medio-lateral oblique"));
    pixel_module(&mut object, None);
    object.put(pixel_data(vec![], vec![frame(0, 32)]));
    finish(object, MG_FOR_PRESENTATION, SOP_MG)
}

/// Breast Tomosynthesis Image Storage, two JPEG 2000 Lossless frames with functional groups.
pub fn tomosynthesis() -> DefaultDicomObject {
    let mut object = common(BREAST_TOMOSYNTHESIS, SOP_DBT, SERIES_DBT, IRRADIATION_DBT);
    object.put_str(
        tags::IMAGE_TYPE,
        VR::CS,
        "ORIGINAL\\PRIMARY\\TOMOSYNTHESIS\\NONE",
    );
    object.put_str(tags::IMAGE_LATERALITY, VR::CS, "L");
    object.put_str(tags::VIEW_POSITION, VR::CS, "CC");
    object.put(view_code("399162004", "cranio-caudal"));
    object.put_str(
        tags::ACQUISITION_DATE_TIME,
        VR::DT,
        "20240315093012.123456+0100",
    );
    object.put_str(tags::FRAME_OF_REFERENCE_UID, VR::UI, FRAME_OF_REFERENCE);

    let mut organization = InMemDicomObject::new_empty();
    organization.put_str(
        tags::DIMENSION_ORGANIZATION_UID,
        VR::UI,
        DIMENSION_ORGANIZATION,
    );
    object.put(sequence(
        tags::DIMENSION_ORGANIZATION_SEQUENCE,
        vec![organization],
    ));
    object.put(sequence(
        tags::DIMENSION_INDEX_SEQUENCE,
        [tags::STACK_ID, tags::IN_STACK_POSITION_NUMBER]
            .into_iter()
            .map(|pointer| {
                let mut item = InMemDicomObject::new_empty();
                item.put_str(
                    tags::DIMENSION_ORGANIZATION_UID,
                    VR::UI,
                    DIMENSION_ORGANIZATION,
                );
                item.put(DataElement::new(
                    tags::DIMENSION_INDEX_POINTER,
                    VR::AT,
                    PrimitiveValue::from(pointer),
                ));
                item.put(DataElement::new(
                    tags::FUNCTIONAL_GROUP_POINTER,
                    VR::AT,
                    PrimitiveValue::from(tags::FRAME_CONTENT_SEQUENCE),
                ));
                item
            })
            .collect(),
    ));

    let mut measures = InMemDicomObject::new_empty();
    measures.put_str(tags::PIXEL_SPACING, VR::DS, "0.085\\0.085");
    measures.put_str(tags::SLICE_THICKNESS, VR::DS, "1");
    let mut frame_type = InMemDicomObject::new_empty();
    frame_type.put_str(
        tags::FRAME_TYPE,
        VR::CS,
        "ORIGINAL\\PRIMARY\\TOMOSYNTHESIS\\NONE",
    );
    let mut anatomy = InMemDicomObject::new_empty();
    anatomy.put_str(tags::FRAME_LATERALITY, VR::CS, "L");
    let mut shared = InMemDicomObject::new_empty();
    shared.put(sequence(tags::PIXEL_MEASURES_SEQUENCE, vec![measures]));
    shared.put(sequence(
        tags::X_RAY3_D_FRAME_TYPE_SEQUENCE,
        vec![frame_type],
    ));
    shared.put(sequence(tags::FRAME_ANATOMY_SEQUENCE, vec![anatomy]));
    private_attributes(&mut shared);
    object.put(sequence(
        tags::SHARED_FUNCTIONAL_GROUPS_SEQUENCE,
        vec![shared],
    ));

    let per_frame = (1_u32..=2)
        .map(|index| {
            let mut content = InMemDicomObject::new_empty();
            content.put_str(
                tags::FRAME_ACQUISITION_DATE_TIME,
                VR::DT,
                format!("2024031509301{index}.250000"),
            );
            content.put_str(
                tags::FRAME_REFERENCE_DATE_TIME,
                VR::DT,
                format!("2024031509301{index}"),
            );
            content.put_str(tags::STACK_ID, VR::SH, "1");
            content.put(DataElement::new(
                tags::IN_STACK_POSITION_NUMBER,
                VR::UL,
                PrimitiveValue::from(index),
            ));
            content.put(DataElement::new(
                tags::DIMENSION_INDEX_VALUES,
                VR::UL,
                PrimitiveValue::U32([1, index].into()),
            ));
            content.put_str(
                tags::FRAME_COMMENTS,
                VR::LT,
                format!("{PLACEHOLDER} frame comment {index}"),
            );
            private_attributes(&mut content);
            let mut position = InMemDicomObject::new_empty();
            position.put_str(
                tags::IMAGE_POSITION_PATIENT,
                VR::DS,
                format!("0\\0\\{index}"),
            );
            let mut item = InMemDicomObject::new_empty();
            item.put(sequence(tags::FRAME_CONTENT_SEQUENCE, vec![content]));
            item.put(sequence(tags::PLANE_POSITION_SEQUENCE, vec![position]));
            item
        })
        .collect();
    object.put(sequence(
        tags::PER_FRAME_FUNCTIONAL_GROUPS_SEQUENCE,
        per_frame,
    ));

    pixel_module(&mut object, Some(2));
    // Frame 2 spans two fragments, so the Basic Offset Table holds the frame boundaries.
    let first = frame(1, 40);
    let second = [frame(2, 24), frame(3, 16)];
    let first_frame_length = u32::try_from(first.len() + 8).unwrap();
    object.put(pixel_data(
        vec![0, first_frame_length],
        vec![first, second[0].clone(), second[1].clone()],
    ));
    finish(object, BREAST_TOMOSYNTHESIS, SOP_DBT)
}

/// Synthesized 2D MG image derived from the DBT instance.
pub fn synthesized() -> DefaultDicomObject {
    let mut object = common(
        MG_FOR_PRESENTATION,
        SOP_SYNTHESIZED,
        SERIES_SYNTHESIZED,
        IRRADIATION_DBT,
    );
    object.put_str(
        tags::IMAGE_TYPE,
        VR::CS,
        "DERIVED\\PRIMARY\\TOMOSYNTHESIS\\GENERATED_2D",
    );
    object.put_str(tags::IMAGE_LATERALITY, VR::CS, "L");
    object.put_str(tags::VIEW_POSITION, VR::CS, "CC");
    object.put(view_code("399162004", "cranio-caudal"));
    object.put_str(tags::FRAME_OF_REFERENCE_UID, VR::UI, FRAME_OF_REFERENCE);
    object.put_str(
        tags::DERIVATION_DESCRIPTION,
        VR::ST,
        format!("{PLACEHOLDER} derivation description"),
    );
    let mut source = InMemDicomObject::new_empty();
    source.put_str(tags::REFERENCED_SOP_CLASS_UID, VR::UI, BREAST_TOMOSYNTHESIS);
    source.put_str(tags::REFERENCED_SOP_INSTANCE_UID, VR::UI, SOP_DBT);
    object.put(sequence(tags::SOURCE_IMAGE_SEQUENCE, vec![source]));
    pixel_module(&mut object, None);
    object.put(pixel_data(vec![0], vec![frame(4, 48)]));
    finish(object, MG_FOR_PRESENTATION, SOP_SYNTHESIZED)
}

/// Attributes shared by every fixture, with a placeholder for each Safe Harbor header category.
fn common(
    sop_class: &str,
    sop_instance: &str,
    series: &str,
    irradiation: &str,
) -> InMemDicomObject {
    let mut object = InMemDicomObject::new_empty();
    object.put_str(tags::SPECIFIC_CHARACTER_SET, VR::CS, "ISO_IR 100");
    object.put_str(tags::SOP_CLASS_UID, VR::UI, sop_class);
    object.put_str(tags::SOP_INSTANCE_UID, VR::UI, sop_instance);
    object.put_str(tags::STUDY_INSTANCE_UID, VR::UI, STUDY);
    object.put_str(tags::SERIES_INSTANCE_UID, VR::UI, series);
    object.put_str(tags::IRRADIATION_EVENT_UID, VR::UI, irradiation);
    object.put_str(tags::DEVICE_UID, VR::UI, DEVICE);
    object.put_str(tags::MODALITY, VR::CS, "MG");
    object.put_str(tags::PRESENTATION_INTENT_TYPE, VR::CS, "FOR PRESENTATION");
    object.put_str(tags::LATERALITY, VR::CS, "B");

    // Names, identifiers, and contact details.
    object.put_str(tags::PATIENT_NAME, VR::PN, format!("{PLACEHOLDER}^Name"));
    object.put_str(tags::PATIENT_ID, VR::LO, format!("{PLACEHOLDER}-MRN"));
    object.put_str(
        tags::ISSUER_OF_PATIENT_ID,
        VR::LO,
        format!("{PLACEHOLDER} issuer"),
    );
    let mut other_id = InMemDicomObject::new_empty();
    other_id.put_str(tags::PATIENT_ID, VR::LO, format!("{PLACEHOLDER}-OTHER"));
    other_id.put_str(
        tags::ISSUER_OF_PATIENT_ID,
        VR::LO,
        format!("{PLACEHOLDER} issuer"),
    );
    object.put(sequence(tags::OTHER_PATIENT_I_DS_SEQUENCE, vec![other_id]));
    object.put_str(
        tags::PATIENT_ADDRESS,
        VR::LO,
        format!("{PLACEHOLDER} street"),
    );
    object.put_str(tags::PATIENT_TELEPHONE_NUMBERS, VR::SH, PLACEHOLDER);
    object.put_str(tags::PATIENT_BIRTH_DATE, VR::DA, "19320229");
    object.put_str(tags::PATIENT_BIRTH_TIME, VR::TM, "061500");
    object.put_str(tags::PATIENT_AGE, VR::AS, "092Y");
    object.put_str(tags::PATIENT_SEX, VR::CS, "F");
    object.put_str(
        tags::PATIENT_COMMENTS,
        VR::LT,
        format!("{PLACEHOLDER} patient comment"),
    );
    object.put_str(tags::ACCESSION_NUMBER, VR::SH, format!("{PLACEHOLDER}AN"));
    object.put_str(tags::STUDY_ID, VR::SH, format!("{PLACEHOLDER}SI"));
    object.put_str(
        tags::REQUESTED_PROCEDURE_ID,
        VR::SH,
        format!("{PLACEHOLDER}RP"),
    );
    let mut referenced_study = InMemDicomObject::new_empty();
    referenced_study.put_str(
        tags::REFERENCED_SOP_CLASS_UID,
        VR::UI,
        DETACHED_STUDY_MANAGEMENT,
    );
    referenced_study.put_str(tags::REFERENCED_SOP_INSTANCE_UID, VR::UI, REFERENCED_STUDY);
    object.put(sequence(
        tags::REFERENCED_STUDY_SEQUENCE,
        vec![referenced_study],
    ));

    // People, institution, and device.
    object.put_str(
        tags::REFERRING_PHYSICIAN_NAME,
        VR::PN,
        format!("{PLACEHOLDER}^Referrer"),
    );
    object.put_str(
        tags::REFERRING_PHYSICIAN_ADDRESS,
        VR::ST,
        format!("{PLACEHOLDER} referrer street"),
    );
    object.put_str(
        tags::REFERRING_PHYSICIAN_TELEPHONE_NUMBERS,
        VR::SH,
        PLACEHOLDER,
    );
    object.put_str(
        tags::OPERATORS_NAME,
        VR::PN,
        format!("{PLACEHOLDER}^Operator"),
    );
    object.put_str(
        tags::PERFORMING_PHYSICIAN_NAME,
        VR::PN,
        format!("{PLACEHOLDER}^Performer"),
    );
    object.put_str(
        tags::INSTITUTION_NAME,
        VR::LO,
        format!("{PLACEHOLDER} institution"),
    );
    object.put_str(
        tags::INSTITUTION_ADDRESS,
        VR::ST,
        format!("{PLACEHOLDER} institution street"),
    );
    object.put_str(
        tags::INSTITUTIONAL_DEPARTMENT_NAME,
        VR::LO,
        format!("{PLACEHOLDER} department"),
    );
    object.put_str(tags::STATION_NAME, VR::SH, PLACEHOLDER);
    object.put_str(
        tags::DEVICE_SERIAL_NUMBER,
        VR::LO,
        format!("{PLACEHOLDER}-SERIAL"),
    );
    object.put_str(tags::DETECTOR_ID, VR::SH, format!("{PLACEHOLDER}DI"));

    // Dates and times.
    for (date, time) in [
        (tags::STUDY_DATE, tags::STUDY_TIME),
        (tags::SERIES_DATE, tags::SERIES_TIME),
        (tags::ACQUISITION_DATE, tags::ACQUISITION_TIME),
        (tags::CONTENT_DATE, tags::CONTENT_TIME),
    ] {
        object.put_str(date, VR::DA, "20240315");
        object.put_str(time, VR::TM, "093012.123456");
    }

    // Free text.
    object.put_str(
        tags::STUDY_DESCRIPTION,
        VR::LO,
        format!("{PLACEHOLDER} study description"),
    );
    object.put_str(
        tags::SERIES_DESCRIPTION,
        VR::LO,
        format!("{PLACEHOLDER} series description"),
    );
    object.put_str(
        tags::IMAGE_COMMENTS,
        VR::LT,
        format!("{PLACEHOLDER} image comment"),
    );
    object.put_str(
        tags::PROTOCOL_NAME,
        VR::LO,
        format!("{PLACEHOLDER} protocol"),
    );
    private_attributes(&mut object);
    object
}

/// Private block 0x0009,0x0010 with text and binary placeholder values.
fn private_attributes(object: &mut InMemDicomObject) {
    object.put_str(Tag(0x0009, 0x0010), VR::LO, PRIVATE_CREATOR);
    object.put_str(
        Tag(0x0009, 0x1001),
        VR::LO,
        format!("{PLACEHOLDER} private text"),
    );
    object.put(DataElement::new(
        Tag(0x0009, 0x1002),
        VR::OB,
        PrimitiveValue::from(format!("{PLACEHOLDER}..").into_bytes()),
    ));
}

fn view_code(value: &str, meaning: &str) -> DataElement<InMemDicomObject> {
    let mut item = InMemDicomObject::new_empty();
    item.put_str(tags::CODE_VALUE, VR::SH, value);
    item.put_str(tags::CODING_SCHEME_DESIGNATOR, VR::SH, "SCT");
    item.put_str(tags::CODE_MEANING, VR::LO, meaning);
    sequence(tags::VIEW_CODE_SEQUENCE, vec![item])
}

fn pixel_module(object: &mut InMemDicomObject, frames: Option<u32>) {
    for (tag, value) in [
        (tags::SAMPLES_PER_PIXEL, 1_u16),
        (tags::ROWS, 4),
        (tags::COLUMNS, 4),
        (tags::BITS_ALLOCATED, 16),
        (tags::BITS_STORED, 12),
        (tags::HIGH_BIT, 11),
        (tags::PIXEL_REPRESENTATION, 0),
    ] {
        object.put(DataElement::new(tag, VR::US, PrimitiveValue::from(value)));
    }
    object.put_str(tags::PHOTOMETRIC_INTERPRETATION, VR::CS, "MONOCHROME2");
    object.put_str(tags::LOSSY_IMAGE_COMPRESSION, VR::CS, "00");
    if let Some(frames) = frames {
        object.put_str(tags::NUMBER_OF_FRAMES, VR::IS, frames.to_string());
    }
}

/// Opaque, even-length fragment that starts with a JPEG 2000 SOC and SIZ marker.
fn frame(seed: u8, length: usize) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0x4F, 0xFF, 0x51];
    bytes.extend((0..length - 4).map(|index| seed.wrapping_mul(31).wrapping_add(index as u8)));
    bytes
}

fn pixel_data(offset_table: Vec<u32>, fragments: Vec<Vec<u8>>) -> DataElement<InMemDicomObject> {
    DataElement::new(
        tags::PIXEL_DATA,
        VR::OB,
        PixelFragmentSequence::new(offset_table, fragments),
    )
}

fn sequence(tag: Tag, items: Vec<InMemDicomObject>) -> DataElement<InMemDicomObject> {
    DataElement::new(tag, VR::SQ, DataSetSequence::from(items))
}

fn finish(object: InMemDicomObject, sop_class: &str, sop_instance: &str) -> DefaultDicomObject {
    object
        .with_meta(
            FileMetaTableBuilder::new()
                .transfer_syntax(JPEG_2000_LOSSLESS)
                .media_storage_sop_class_uid(sop_class)
                .media_storage_sop_instance_uid(sop_instance)
                .source_application_entity_title(PLACEHOLDER)
                .private_information_creator_uid(META_PRIVATE_CREATOR)
                .private_information(format!("{PLACEHOLDER}..").into_bytes()),
        )
        .unwrap()
}
