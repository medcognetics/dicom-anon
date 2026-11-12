//! Vendor-representative variants of the synthetic mammography fixtures (issue #33).
//!
//! Each variant copies the *structure* that the main mammography vendors put in MG and DBT
//! objects: their private creator blocks (text, binary, and nested private sequences, at the
//! root and inside functional groups), device descriptor values, and a declared Burned In
//! Annotation. Private creator strings follow the vendors' published conformance statements.
//! Every private value is synthetic and carries [`PLACEHOLDER`]; no real object contributes
//! data. The variants go through `safe-harbor-metadata` with the shipped breast-imaging
//! descriptor map and the gates a mammography pipeline enables.

use std::collections::BTreeMap;

use dicom_anon::{
    validate_source_object, write_dicom, Anonymizer, DefaultDicomObject, Options,
    ProfileConfiguration, SecretKey,
};
use dicom_core::header::Header;
use dicom_core::value::{DataSetSequence, Value};
use dicom_core::{DataElement, PrimitiveValue, Tag, VR};
use dicom_dictionary_std::tags;
use dicom_object::mem::InMemDicomObject;

use super::fixtures::{self, Fixtures, PLACEHOLDER};
use super::{item, round_trip, text, uid_map, visit, KEY, PATIENT_SCOPE};

const SHIPPED_BREAST_MAP: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../descriptor-maps/breast-imaging-ps3.16-2026d.json"
);

struct Vendor {
    name: &'static str,
    manufacturer: &'static str,
    model: &'static str,
    /// `(group, private creator)` blocks written at the root.
    root_blocks: &'static [(u16, &'static str)],
    /// `(group, private creator)` block written inside each functional group item.
    functional_group_block: (u16, &'static str),
}

const VENDORS: [Vendor; 4] = [
    Vendor {
        name: "hologic",
        manufacturer: "HOLOGIC, Inc.",
        model: "Selenia Dimensions",
        root_blocks: &[(0x0019, "HOLOGIC, Inc."), (0x7E01, "HOLOGIC, Inc.")],
        functional_group_block: (0x7E01, "HOLOGIC, Inc."),
    },
    Vendor {
        name: "ge",
        manufacturer: "GE MEDICAL SYSTEMS",
        model: "Senographe Pristina",
        root_blocks: &[(0x0009, "GEMS_IDEN_01"), (0x0045, "GEMS_SENO_02")],
        functional_group_block: (0x0045, "GEMS_SENO_02"),
    },
    Vendor {
        name: "siemens",
        manufacturer: "SIEMENS",
        model: "MAMMOMAT Revelation",
        root_blocks: &[
            (0x0019, "SIEMENS MED SP DXMG WH AWS 1"),
            (0x0029, "SIEMENS CSA HEADER"),
        ],
        functional_group_block: (0x0019, "SIEMENS MED SP DXMG WH AWS 1"),
    },
    Vendor {
        name: "fujifilm",
        manufacturer: "FUJIFILM Corporation",
        model: "AMULET Innovality",
        root_blocks: &[
            (0x0009, "FUJI PHOTO FILM Co., ltd."),
            (0x0019, "FUJIFILM MEDICAL SYSTEMS"),
        ],
        functional_group_block: (0x0009, "FUJI PHOTO FILM Co., ltd."),
    },
];

/// A private block with text, binary, and a nested private sequence. `slot` picks the
/// creator element `(group,00slot)` and the `(group,slotXX)` element range.
fn private_block(object: &mut InMemDicomObject, group: u16, creator: &str, slot: u16) {
    let base = slot << 8;
    object.put_str(Tag(group, slot), VR::LO, creator);
    object.put_str(
        Tag(group, base | 0x01),
        VR::LO,
        format!("{PLACEHOLDER} {creator} text"),
    );
    object.put(DataElement::new(
        Tag(group, base | 0x02),
        VR::OB,
        PrimitiveValue::from(format!("{PLACEHOLDER}{creator}").into_bytes()),
    ));
    let mut nested = InMemDicomObject::new_empty();
    nested.put_str(Tag(group, slot), VR::LO, creator);
    nested.put_str(
        Tag(group, base | 0x01),
        VR::LT,
        format!("{PLACEHOLDER} nested {creator}"),
    );
    nested.put_str(tags::PATIENT_ID, VR::LO, format!("{PLACEHOLDER}-NESTED"));
    object.put(DataElement::new(
        Tag(group, base | 0x03),
        VR::SQ,
        DataSetSequence::from(vec![nested]),
    ));
}

fn put_vendor_attributes(object: &mut InMemDicomObject, vendor: &Vendor) {
    object.put_str(tags::MANUFACTURER, VR::LO, vendor.manufacturer);
    object.put_str(tags::MANUFACTURER_MODEL_NAME, VR::LO, vendor.model);
    object.put_str(tags::SOFTWARE_VERSIONS, VR::LO, "SYNTHETIC 1.0");
    object.put_str(
        tags::ACQUISITION_DEVICE_PROCESSING_DESCRIPTION,
        VR::LO,
        format!("{PLACEHOLDER} processing"),
    );
    object.put_str(tags::BURNED_IN_ANNOTATION, VR::CS, "NO");
    // Slot 0x10 holds the base fixture's private block; add the vendor blocks after it,
    // so that one odd group can carry two creators as real objects do.
    for (index, (group, creator)) in vendor.root_blocks.iter().enumerate() {
        private_block(object, *group, creator, 0x0011 + index as u16);
    }
    let (group, creator) = vendor.functional_group_block;
    for sequence in [
        tags::SHARED_FUNCTIONAL_GROUPS_SEQUENCE,
        tags::PER_FRAME_FUNCTIONAL_GROUPS_SEQUENCE,
    ] {
        let Some(element) = object.get(sequence).cloned() else {
            continue;
        };
        let Value::Sequence(items) = element.value() else {
            continue;
        };
        let items: Vec<InMemDicomObject> = items
            .items()
            .iter()
            .cloned()
            .map(|mut item| {
                private_block(&mut item, group, creator, 0x0020);
                item
            })
            .collect();
        object.put(DataElement::new(
            sequence,
            VR::SQ,
            DataSetSequence::from(items),
        ));
    }
}

/// MG For Processing, derived from the For Presentation fixture.
fn for_processing(mammogram: &DefaultDicomObject) -> DefaultDicomObject {
    let mut object = mammogram.clone().into_inner();
    object.put_str(tags::SOP_CLASS_UID, VR::UI, fixtures::MG_FOR_PROCESSING);
    object.put_str(tags::PRESENTATION_INTENT_TYPE, VR::CS, "FOR PROCESSING");
    fixtures::finish(object, fixtures::MG_FOR_PROCESSING, fixtures::SOP_MG)
}

/// Every vendor variant of every object type: `(label, sop_class, object)`.
fn variants() -> Vec<(String, &'static str, DefaultDicomObject)> {
    let fixtures = Fixtures::new();
    let processing = for_processing(&fixtures.mammogram);
    let bases = [
        (
            "mg-for-presentation",
            fixtures::MG_FOR_PRESENTATION,
            &fixtures.mammogram,
        ),
        (
            "mg-for-processing",
            fixtures::MG_FOR_PROCESSING,
            &processing,
        ),
        (
            "dbt",
            fixtures::BREAST_TOMOSYNTHESIS,
            &fixtures.tomosynthesis,
        ),
        (
            "synthesized-2d",
            fixtures::MG_FOR_PRESENTATION,
            &fixtures.synthesized,
        ),
    ];
    let mut variants = Vec::new();
    for vendor in &VENDORS {
        for (name, sop_class, base) in bases {
            let mut object = base.clone().into_inner();
            put_vendor_attributes(&mut object, vendor);
            let sop_instance = text(&object, tags::SOP_INSTANCE_UID).unwrap();
            let finished = fixtures::finish(object, sop_class, &sop_instance);
            variants.push((
                format!("{}/{name}", vendor.name),
                sop_class,
                round_trip(&finished),
            ));
        }
    }
    variants
}

fn breast_map() -> BTreeMap<String, BTreeMap<String, String>> {
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(SHIPPED_BREAST_MAP).unwrap()).unwrap();
    serde_json::from_value(document["mappings"].clone()).unwrap()
}

/// Safe Harbor with the breast map, the Burned In Annotation gate, and the MG/DBT allowlist.
fn pipeline_options() -> Options {
    let mut options = Options::new(SecretKey::new(KEY).unwrap());
    options.patient_scope = Some(PATIENT_SCOPE.to_owned());
    let mut profile = ProfileConfiguration::safe_harbor();
    profile.uid_mappings = uid_map();
    profile.descriptor_mappings = breast_map();
    profile.require_no_burned_in_annotation = true;
    profile.sop_class_allowlist = [
        fixtures::MG_FOR_PRESENTATION,
        fixtures::MG_FOR_PROCESSING,
        fixtures::BREAST_TOMOSYNTHESIS,
    ]
    .map(str::to_owned)
    .into();
    options.profile = profile;
    options
}

fn encode(object: &DefaultDicomObject) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_dicom(object, &mut bytes).unwrap();
    bytes
}

#[test]
fn vendor_variants_hold_private_blocks_at_every_level() {
    for (label, _, source) in variants() {
        let mut private_depths = std::collections::BTreeSet::new();
        visit(&source, 0, &mut |element, depth| {
            if element.tag().group() % 2 == 1 {
                private_depths.insert(depth);
            }
        });
        assert!(
            private_depths.contains(&0),
            "{label}: no root private block"
        );
        assert!(
            private_depths.contains(&1),
            "{label}: no nested private block"
        );
        if label.ends_with("/dbt") {
            assert!(
                private_depths.contains(&2),
                "{label}: no private block in a functional group sequence"
            );
            // The base fixture already has private blocks in the functional groups,
            // so check for the vendor's own creator in every item.
            let vendor = VENDORS
                .iter()
                .find(|vendor| label.starts_with(&format!("{}/", vendor.name)))
                .unwrap_or_else(|| panic!("{label}: unknown vendor"));
            let (group, creator) = vendor.functional_group_block;
            for sequence in [
                tags::SHARED_FUNCTIONAL_GROUPS_SEQUENCE,
                tags::PER_FRAME_FUNCTIONAL_GROUPS_SEQUENCE,
            ] {
                let items = source
                    .get(sequence)
                    .and_then(|element| element.items())
                    .unwrap_or_else(|| panic!("{label}: no {sequence}"));
                assert!(!items.is_empty(), "{label}: empty {sequence}");
                for item in items {
                    assert_eq!(
                        text(item, Tag(group, 0x0020)).as_deref(),
                        Some(creator),
                        "{label}: {sequence} item lacks the vendor block"
                    );
                }
            }
        }
        assert_eq!(
            text(&source, tags::BURNED_IN_ANNOTATION).as_deref(),
            Some("NO")
        );
    }
}

#[test]
fn vendor_variants_pass_the_safe_harbor_pipeline() {
    let anonymizer = Anonymizer::new(pipeline_options()).unwrap();
    for (label, sop_class, source) in variants() {
        let (output, _) = anonymizer
            .anonymize(&source)
            .unwrap_or_else(|error| panic!("{label}: {error}"));
        let output = round_trip(&output);

        let report = validate_source_object(&source, &output, pipeline_options()).unwrap();
        assert!(report.conformant, "{label}: {:?}", report.findings);

        // No private attribute survives at any depth, and no placeholder value remains.
        visit(&output, 0, &mut |element, depth| {
            assert!(
                element.tag().group() % 2 == 0,
                "{label}: private {} kept at depth {depth}",
                element.tag()
            );
        });
        let bytes = encode(&output);
        assert!(
            !bytes
                .windows(PLACEHOLDER.len())
                .any(|window| window == PLACEHOLDER.as_bytes()),
            "{label}: placeholder value in output"
        );

        // Device descriptors are not kept without a reviewed descriptor map.
        for tag in [tags::MANUFACTURER, tags::MANUFACTURER_MODEL_NAME] {
            let value = text(&output, tag);
            let source_value = text(&source, tag);
            assert_ne!(value, source_value, "{label}: {tag} kept");
        }

        // Inference inputs are unchanged.
        for tag in fixtures::VIEW_TAGS {
            assert_eq!(source.get(tag), output.get(tag), "{label}: {tag}");
        }
        assert_eq!(
            text(&output, tags::SOP_CLASS_UID).as_deref(),
            Some(sop_class),
            "{label}"
        );
        assert_eq!(
            item(&source, tags::VIEW_CODE_SEQUENCE, 0),
            item(&output, tags::VIEW_CODE_SEQUENCE, 0),
            "{label}: view code"
        );
        // Compare field by field: an undefined element length never equals another.
        let (source_pixels, output_pixels) = (
            source.get(tags::PIXEL_DATA).unwrap().value(),
            output.get(tags::PIXEL_DATA).unwrap().value(),
        );
        assert_eq!(
            source_pixels.offset_table(),
            output_pixels.offset_table(),
            "{label}: offset table"
        );
        assert_eq!(
            source_pixels.fragments(),
            output_pixels.fragments(),
            "{label}: Pixel Data fragments"
        );
    }
}

#[test]
fn vendor_variants_fail_closed_on_gates() {
    // Each object type is rejected when its SOP Class is not allowed.
    for (label, sop_class, source) in variants() {
        let mut options = pipeline_options();
        options.profile.sop_class_allowlist.remove(sop_class);
        let error = Anonymizer::new(options)
            .unwrap()
            .anonymize(&source)
            .expect_err(&label);
        assert!(
            matches!(error, dicom_anon::Error::SopClassNotAllowed),
            "{label}: {error}"
        );
        assert!(!error.to_string().contains(PLACEHOLDER), "{label}: {error}");
    }

    // A declared burned-in annotation, or none at all, is rejected.
    let anonymizer = Anonymizer::new(pipeline_options()).unwrap();
    for (label, sop_class, source) in variants() {
        for value in [Some("YES"), None] {
            let mut object = source.clone().into_inner();
            match value {
                Some(value) => {
                    object.put_str(tags::BURNED_IN_ANNOTATION, VR::CS, value);
                }
                None => {
                    object.remove_element(tags::BURNED_IN_ANNOTATION);
                }
            }
            let sop_instance = text(&object, tags::SOP_INSTANCE_UID).unwrap();
            let object = fixtures::finish(object, sop_class, &sop_instance);
            let error = anonymizer.anonymize(&object).map(|_| ()).unwrap_err();
            assert!(
                matches!(error, dicom_anon::Error::BurnedInAnnotationNotNo),
                "{label}: Burned In Annotation {value:?}: {error}"
            );
        }
    }
}
