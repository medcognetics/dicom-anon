//! Caller UID maps for the Safe Harbor metadata policy.
//!
//! The anonymizer resolves every `U` action through [`UidResolver`], so the
//! listing and generation modes follow the same rule decisions as ordinary
//! de-identification.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use dicom_core::value::Value;
use dicom_core::{Tag, VR};
use dicom_object::{DefaultDicomObject, InMemDicomObject};
use serde::{Deserialize, Serialize};

use crate::profile::{valid_uid, DeidentificationPolicy};
use crate::{Error, Options, Result};

const MAXIMUM_GENERATION_ATTEMPTS: usize = 16;

/// One source UID occurrence that the configured profile replaces.
///
/// The value is a source identifier. Keep it under the same custody as the
/// source object and the UID map. `Debug` output redacts it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredUid {
    /// Attribute path, for example `(0020,9221)[0]/(0020,9164)`.
    pub path: String,
    /// Source UID value.
    pub uid: String,
}

impl fmt::Debug for RequiredUid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequiredUid")
            .field("path", &self.path)
            .field("uid", &"[REDACTED]")
            .finish()
    }
}

enum Mode {
    /// Use the configured map, or keyed UIDs outside Safe Harbor mode.
    Mapped,
    /// Record each source UID and its path.
    Collect(Vec<RequiredUid>),
    /// Use the configured map and assign random replacements for missing entries.
    Generate {
        map: BTreeMap<String, String>,
        reserved: BTreeSet<String>,
    },
}

/// Per-object UID resolution state shared by all anonymizer modes.
pub(crate) struct UidResolver {
    mode: Mode,
    path: Vec<(Tag, usize)>,
}

impl UidResolver {
    pub(crate) const fn mapped() -> Self {
        Self {
            mode: Mode::Mapped,
            path: Vec::new(),
        }
    }

    pub(crate) const fn collect() -> Self {
        Self {
            mode: Mode::Collect(Vec::new()),
            path: Vec::new(),
        }
    }

    /// Start generation from `map`, reserving every UID in `source`, its file meta, and the map.
    ///
    /// Fail when a starting replacement equals a UID in `source` or its file meta.
    pub(crate) fn generate(
        source: &DefaultDicomObject,
        map: BTreeMap<String, String>,
    ) -> Result<Self> {
        let mut reserved = BTreeSet::new();
        collect_all_uids(source, &mut reserved);
        let meta = source.meta();
        reserved.extend(
            [
                meta.media_storage_sop_class_uid(),
                meta.media_storage_sop_instance_uid(),
                meta.transfer_syntax(),
                meta.implementation_class_uid(),
            ]
            .into_iter()
            .chain(meta.private_information_creator_uid())
            .map(|value| value.trim_end_matches(['\0', ' ']).to_owned()),
        );
        if map
            .values()
            .any(|replacement| reserved.contains(replacement))
        {
            return Err(Error::InvalidSafeHarborUidMapping(
                "replacement UIDs must differ from every source UID".to_owned(),
            ));
        }
        reserved.extend(map.keys().cloned());
        reserved.extend(map.values().cloned());
        Ok(Self {
            mode: Mode::Generate { map, reserved },
            path: Vec::new(),
        })
    }

    pub(crate) fn enter_item(&mut self, sequence: Tag, index: usize) {
        self.path.push((sequence, index));
    }

    pub(crate) fn leave_item(&mut self) {
        self.path.pop();
    }

    /// Return the replacement for one source UID at `tag` in the current item.
    pub(crate) fn resolve(
        &mut self,
        original: &str,
        tag: Tag,
        options: &Options,
    ) -> Result<String> {
        if let Mode::Collect(found) = &mut self.mode {
            found.push(RequiredUid {
                path: format_path(&self.path, tag),
                uid: original.to_owned(),
            });
            return Ok(original.to_owned());
        }
        if options.profile.policy != DeidentificationPolicy::SafeHarborMetadata {
            return Ok(options.secret.uid(&options.uid_root, original));
        }
        match &mut self.mode {
            Mode::Generate { map, reserved } => {
                if let Some(replacement) = map.get(original) {
                    return Ok(replacement.clone());
                }
                if !valid_uid(original) {
                    return Err(Error::InvalidSourceUid {
                        path: format_path(&self.path, tag),
                    });
                }
                let replacement = unique_random_uid(&options.uid_root, reserved)?;
                map.insert(original.to_owned(), replacement.clone());
                Ok(replacement)
            }
            _ => options
                .profile
                .uid_mappings
                .get(original)
                .cloned()
                .ok_or_else(|| Error::MissingSafeHarborUidMapping {
                    path: format_path(&self.path, tag),
                }),
        }
    }

    /// Return the recorded occurrences in traversal order.
    pub(crate) fn into_required(self) -> Vec<RequiredUid> {
        match self.mode {
            Mode::Collect(found) => found,
            _ => Vec::new(),
        }
    }

    /// Return the complete generated map.
    pub(crate) fn into_map(self) -> BTreeMap<String, String> {
        match self.mode {
            Mode::Generate { map, .. } => map,
            _ => BTreeMap::new(),
        }
    }
}

fn format_path(items: &[(Tag, usize)], tag: Tag) -> String {
    let mut path = String::new();
    for (sequence, index) in items {
        path.push_str(&format!(
            "({:04X},{:04X})[{index}]/",
            sequence.0, sequence.1
        ));
    }
    path.push_str(&format!("({:04X},{:04X})", tag.0, tag.1));
    path
}

/// Add every UI value in `object`, at every depth, to `uids`.
fn collect_all_uids(object: &InMemDicomObject, uids: &mut BTreeSet<String>) {
    for element in object.iter() {
        match element.value() {
            Value::Sequence(sequence) => {
                for item in sequence.items() {
                    collect_all_uids(item, uids);
                }
            }
            _ if element.vr() == VR::UI => {
                if let Ok(values) = element.to_str() {
                    uids.extend(
                        values
                            .split('\\')
                            .map(|value| value.trim_end_matches(['\0', ' ']).to_owned()),
                    );
                }
            }
            _ => {}
        }
    }
}

/// Draw a replacement from the operating-system CSPRNG that is not in `reserved`.
fn unique_random_uid(uid_root: &str, reserved: &mut BTreeSet<String>) -> Result<String> {
    for _ in 0..MAXIMUM_GENERATION_ATTEMPTS {
        let candidate = random_uid(uid_root)?;
        if reserved.insert(candidate.clone()) {
            return Ok(candidate);
        }
    }
    Err(Error::RandomUidGeneration(
        "no collision-free replacement was found".to_owned(),
    ))
}

/// Format a random version 4 UUID integer under `uid_root` (ISO/IEC 9834-8 for `2.25`).
fn random_uid(uid_root: &str) -> Result<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| Error::RandomUidGeneration(error.to_string()))?;
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    Ok(format!("{uid_root}.{}", u128::from_be_bytes(bytes)))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use dicom_core::value::DataSetSequence;
    use dicom_core::{PrimitiveValue, Tag, VR};
    use dicom_dictionary_std::{tags, uids};
    use dicom_object::mem::InMemElement;
    use dicom_object::{DefaultDicomObject, FileMetaTableBuilder, InMemDicomObject};

    use super::{collect_all_uids, unique_random_uid};
    use crate::{Anonymizer, Error, Options, ProfileConfiguration, SecretKey};

    const KEY: [u8; 32] = [19; 32];
    const STUDY: &str = "2.25.82751374713386041856794579278177576159";
    const SERIES: &str = "2.25.55250158356079243228430942758422479484";
    const INSTANCE: &str = "2.25.53888643650370042957917281792383333513";
    const FRAME_OF_REFERENCE: &str = "2.25.46895189170140016371398513567695111293";
    const DIMENSION_ORGANIZATION: &str = "2.25.81788688900708354383623266621354476385";
    const SOURCE_IMAGE: &str = "2.25.77137079572787260023021057247543626131";
    const BREAST_TOMOSYNTHESIS: &str = "1.2.840.10008.5.1.4.1.1.13.1.3";
    const SENTINEL_REPLACEMENT: &str = "2.25.38769576073193516533726834964495232606";

    fn item(elements: Vec<InMemElement>) -> InMemDicomObject {
        InMemDicomObject::from_element_iter(elements)
    }

    fn sequence(tag: Tag, items: Vec<InMemDicomObject>) -> InMemElement {
        InMemElement::new(tag, VR::SQ, DataSetSequence::from(items))
    }

    fn uid(tag: Tag, value: &str) -> InMemElement {
        InMemElement::new(tag, VR::UI, PrimitiveValue::from(value))
    }

    /// Synthetic two-frame Breast Tomosynthesis Image metadata without Pixel Data.
    pub(super) fn tomosynthesis() -> DefaultDicomObject {
        let mut object = InMemDicomObject::new_empty();
        object.put(uid(tags::SOP_CLASS_UID, BREAST_TOMOSYNTHESIS));
        object.put(uid(tags::SOP_INSTANCE_UID, INSTANCE));
        object.put(uid(tags::STUDY_INSTANCE_UID, STUDY));
        object.put(uid(tags::SERIES_INSTANCE_UID, SERIES));
        object.put(uid(tags::FRAME_OF_REFERENCE_UID, FRAME_OF_REFERENCE));
        object.put_str(tags::PATIENT_ID, VR::LO, "SYNTHETIC-PATIENT");
        object.put_str(tags::STUDY_DATE, VR::DA, "20240101");
        object.put_str(tags::MODALITY, VR::CS, "MG");
        object.put_str(tags::NUMBER_OF_FRAMES, VR::IS, "2");
        object.put(sequence(
            tags::SOURCE_IMAGE_SEQUENCE,
            vec![item(vec![
                uid(
                    tags::REFERENCED_SOP_CLASS_UID,
                    uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
                ),
                uid(tags::REFERENCED_SOP_INSTANCE_UID, SOURCE_IMAGE),
            ])],
        ));
        object.put(sequence(
            tags::DIMENSION_ORGANIZATION_SEQUENCE,
            vec![item(vec![uid(
                tags::DIMENSION_ORGANIZATION_UID,
                DIMENSION_ORGANIZATION,
            )])],
        ));
        let index = |pointer: Tag| {
            item(vec![
                uid(tags::DIMENSION_ORGANIZATION_UID, DIMENSION_ORGANIZATION),
                InMemElement::new(
                    tags::DIMENSION_INDEX_POINTER,
                    VR::AT,
                    PrimitiveValue::from(pointer),
                ),
            ])
        };
        object.put(sequence(
            tags::DIMENSION_INDEX_SEQUENCE,
            vec![index(tags::STACK_ID), index(tags::IN_STACK_POSITION_NUMBER)],
        ));
        let frame = |position: u32| {
            item(vec![sequence(
                tags::FRAME_CONTENT_SEQUENCE,
                vec![item(vec![
                    InMemElement::new(tags::STACK_ID, VR::SH, PrimitiveValue::from("1")),
                    InMemElement::new(
                        tags::IN_STACK_POSITION_NUMBER,
                        VR::UL,
                        PrimitiveValue::from(position),
                    ),
                    InMemElement::new(
                        tags::DIMENSION_INDEX_VALUES,
                        VR::UL,
                        PrimitiveValue::U32([1, position].into_iter().collect()),
                    ),
                ])],
            )])
        };
        object.put(sequence(
            tags::PER_FRAME_FUNCTIONAL_GROUPS_SEQUENCE,
            vec![frame(1), frame(2)],
        ));
        object
            .with_meta(
                FileMetaTableBuilder::new()
                    .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
                    .media_storage_sop_class_uid(BREAST_TOMOSYNTHESIS)
                    .media_storage_sop_instance_uid(INSTANCE),
            )
            .unwrap()
    }

    fn safe_harbor(uid_mappings: BTreeMap<String, String>) -> Anonymizer {
        let mut options = Options::new(SecretKey::new(KEY).unwrap());
        options.profile = ProfileConfiguration::safe_harbor();
        options.profile.uid_mappings = uid_mappings;
        options.patient_scope = Some("SUBJ-0001".to_owned());
        Anonymizer::new(options).unwrap()
    }

    fn map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(source, replacement)| ((*source).to_owned(), (*replacement).to_owned()))
            .collect()
    }

    #[test]
    fn missing_mapping_error_names_the_nested_path_and_not_the_value() {
        let anonymizer = safe_harbor(map(&[
            (STUDY, "2.25.81946782669268777770025166272609719793"),
            (SERIES, "2.25.60756160252525405890473755640066903379"),
            (INSTANCE, SENTINEL_REPLACEMENT),
            (FRAME_OF_REFERENCE, "2.25.1001"),
            (SOURCE_IMAGE, "2.25.1002"),
        ]));
        let message = anonymizer
            .anonymize(&tomosynthesis())
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("(0020,9221)[0]/(0020,9164)"),
            "unexpected message: {message}"
        );
        for secret in [DIMENSION_ORGANIZATION, SENTINEL_REPLACEMENT, STUDY] {
            assert!(!message.contains(secret));
        }
    }

    fn written(object: &DefaultDicomObject) -> String {
        let mut bytes = Vec::new();
        crate::write_dicom(object, &mut bytes).unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn text(object: &InMemDicomObject, tag: Tag) -> String {
        object.element(tag).unwrap().to_str().unwrap().into_owned()
    }

    fn first_item(object: &InMemDicomObject, tag: Tag, index: usize) -> &InMemDicomObject {
        &object.element(tag).unwrap().items().unwrap()[index]
    }

    #[test]
    fn required_uids_lists_every_remapped_occurrence_with_its_path() {
        let required = safe_harbor(BTreeMap::new())
            .required_uids(&tomosynthesis())
            .unwrap();
        let listed: Vec<(&str, &str)> = required
            .iter()
            .map(|entry| (entry.path.as_str(), entry.uid.as_str()))
            .collect();
        assert_eq!(
            listed,
            [
                ("(0008,0018)", INSTANCE),
                ("(0008,2112)[0]/(0008,1155)", SOURCE_IMAGE),
                ("(0020,000D)", STUDY),
                ("(0020,000E)", SERIES),
                ("(0020,0052)", FRAME_OF_REFERENCE),
                ("(0020,9221)[0]/(0020,9164)", DIMENSION_ORGANIZATION),
                ("(0020,9222)[0]/(0020,9164)", DIMENSION_ORGANIZATION),
                ("(0020,9222)[1]/(0020,9164)", DIMENSION_ORGANIZATION),
            ]
        );
        let debug = format!("{required:?}");
        assert!(debug.contains("(0008,0018)") && !debug.contains(INSTANCE));

        let mut options = Options::new(SecretKey::new(KEY).unwrap());
        options.profile = ProfileConfiguration::dicom();
        let keyed = Anonymizer::new(options).unwrap();
        assert_eq!(keyed.required_uids(&tomosynthesis()).unwrap(), required);
    }

    #[test]
    fn generated_map_round_trips_a_two_frame_tomosynthesis_object() {
        let source = tomosynthesis();
        let anonymizer = safe_harbor(map(&[(STUDY, SENTINEL_REPLACEMENT)]));
        let (output, report, generated) = anonymizer
            .anonymize_with_generated_uid_map(&source)
            .unwrap();

        let sources = [
            STUDY,
            SERIES,
            INSTANCE,
            FRAME_OF_REFERENCE,
            SOURCE_IMAGE,
            DIMENSION_ORGANIZATION,
        ];
        assert_eq!(
            generated
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            sources.into_iter().collect()
        );
        assert_eq!(generated[STUDY], SENTINEL_REPLACEMENT);
        let replacements: BTreeSet<_> = generated.values().collect();
        assert_eq!(replacements.len(), sources.len());
        for replacement in &replacements {
            assert!(replacement.starts_with("2.25.") && replacement.len() <= 64);
            assert!(!sources.contains(&replacement.as_str()));
        }

        assert_eq!(
            text(&output, tags::STUDY_INSTANCE_UID),
            SENTINEL_REPLACEMENT
        );
        assert_eq!(text(&output, tags::SOP_INSTANCE_UID), generated[INSTANCE]);
        let organization = &generated[DIMENSION_ORGANIZATION];
        assert_eq!(
            &text(
                first_item(&output, tags::DIMENSION_ORGANIZATION_SEQUENCE, 0),
                tags::DIMENSION_ORGANIZATION_UID
            ),
            organization
        );
        for index in 0..2 {
            assert_eq!(
                &text(
                    first_item(&output, tags::DIMENSION_INDEX_SEQUENCE, index),
                    tags::DIMENSION_ORGANIZATION_UID
                ),
                organization
            );
        }
        assert_eq!(
            output
                .element(tags::PER_FRAME_FUNCTIONAL_GROUPS_SEQUENCE)
                .unwrap()
                .items()
                .unwrap()
                .len(),
            2
        );

        let encoded = written(&output);
        let report_json = serde_json::to_string(&report).unwrap();
        for source_uid in sources {
            assert!(!encoded.contains(source_uid));
            assert!(!report_json.contains(source_uid));
        }
        assert!(!report_json.contains(SENTINEL_REPLACEMENT));

        let mut options = Options::new(SecretKey::new(KEY).unwrap());
        options.profile = ProfileConfiguration::safe_harbor();
        options.profile.uid_mappings = generated.clone();
        options.patient_scope = Some("SUBJ-0001".to_owned());
        let validation = crate::validate_source_object(&source, &output, options).unwrap();
        assert!(validation.conformant, "{validation:?}");

        let (again, _, unchanged) = safe_harbor(generated.clone())
            .anonymize_with_generated_uid_map(&source)
            .unwrap();
        assert_eq!(unchanged, generated);
        assert_eq!(written(&again), encoded);
    }

    #[test]
    fn generated_replacements_are_random_and_use_the_uid_root() {
        let source = tomosynthesis();
        let (_, _, first) = safe_harbor(BTreeMap::new())
            .anonymize_with_generated_uid_map(&source)
            .unwrap();
        let (_, _, second) = safe_harbor(BTreeMap::new())
            .anonymize_with_generated_uid_map(&source)
            .unwrap();
        assert_eq!(
            first.keys().collect::<Vec<_>>(),
            second.keys().collect::<Vec<_>>()
        );
        for (source_uid, replacement) in &first {
            assert_ne!(&second[source_uid], replacement);
        }

        let mut options = Options::new(SecretKey::new(KEY).unwrap());
        options.profile = ProfileConfiguration::safe_harbor();
        options.patient_scope = Some("SUBJ-0001".to_owned());
        options.uid_root = "1.2.826.0.1.3680043.999".to_owned();
        let (_, _, rooted) = Anonymizer::new(options)
            .unwrap()
            .anonymize_with_generated_uid_map(&source)
            .unwrap();
        assert!(rooted
            .values()
            .all(|uid| { uid.starts_with("1.2.826.0.1.3680043.999.") && uid.len() <= 64 }));
    }

    #[test]
    fn generation_requires_safe_harbor_and_fails_without_partial_maps() {
        let mut options = Options::new(SecretKey::new(KEY).unwrap());
        options.profile = ProfileConfiguration::dicom();
        assert!(matches!(
            Anonymizer::new(options)
                .unwrap()
                .anonymize_with_generated_uid_map(&tomosynthesis()),
            Err(Error::UidMapGenerationRequiresSafeHarbor)
        ));

        let mut invalid = tomosynthesis();
        invalid.put_str(tags::STUDY_DATE, VR::DA, "20241301");
        assert!(matches!(
            safe_harbor(BTreeMap::new()).anonymize_with_generated_uid_map(&invalid),
            Err(Error::InvalidTemporal { .. })
        ));
    }

    #[test]
    fn reserved_set_contains_nested_and_multivalued_uids() {
        let mut reserved = BTreeSet::new();
        let mut object = InMemDicomObject::new_empty();
        object.put_str(tags::RELATED_GENERAL_SOP_CLASS_UID, VR::UI, "1.2.3\\1.2.4");
        object.put(sequence(
            tags::SOURCE_IMAGE_SEQUENCE,
            vec![item(vec![uid(
                tags::REFERENCED_SOP_INSTANCE_UID,
                "1.2.5\0",
            )])],
        ));
        collect_all_uids(&object, &mut reserved);
        assert_eq!(
            reserved,
            ["1.2.3", "1.2.4", "1.2.5"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );

        let mut occupied = BTreeSet::new();
        let first = unique_random_uid("2.25", &mut occupied).unwrap();
        assert!(occupied.contains(&first));
        let version = first[5..].parse::<u128>().unwrap();
        assert_eq!((version >> 76) & 0xF, 4);
        assert_eq!((version >> 62) & 0x3, 2);
    }

    #[test]
    fn generation_rejects_invalid_or_empty_source_uids_by_path() {
        for (value, path) in [("1.02.3", "(0020,000D)"), ("", "(0020,0052)")] {
            let mut source = tomosynthesis();
            let tag = if path == "(0020,000D)" {
                tags::STUDY_INSTANCE_UID
            } else {
                tags::FRAME_OF_REFERENCE_UID
            };
            source.put(uid(tag, value));
            let error = safe_harbor(BTreeMap::new())
                .anonymize_with_generated_uid_map(&source)
                .unwrap_err();
            assert!(
                matches!(&error, Error::InvalidSourceUid { path: found } if found == path),
                "{error}"
            );
            if !value.is_empty() {
                assert!(!error.to_string().contains(value));
            }
        }
    }

    #[test]
    fn generation_rejects_starting_replacements_that_equal_object_uids() {
        let error = safe_harbor(map(&[(INSTANCE, STUDY)]))
            .anonymize_with_generated_uid_map(&tomosynthesis())
            .unwrap_err();
        assert!(matches!(error, Error::InvalidSafeHarborUidMapping(_)));
        assert!(!error.to_string().contains(STUDY));
    }

    #[test]
    fn generation_rejects_starting_replacements_that_equal_file_meta_uids() {
        let source = tomosynthesis();
        let implementation = source
            .meta()
            .implementation_class_uid()
            .trim_end_matches('\0')
            .to_owned();
        let error = safe_harbor(map(&[(STUDY, implementation.as_str())]))
            .anonymize_with_generated_uid_map(&source)
            .unwrap_err();
        assert!(matches!(error, Error::InvalidSafeHarborUidMapping(_)));
    }

    #[test]
    fn generation_rejects_starting_replacements_that_equal_the_private_creator_uid() {
        let creator = "2.25.60756160252525405890473755640066903379";
        let source = tomosynthesis();
        let meta = FileMetaTableBuilder::new()
            .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
            .media_storage_sop_class_uid(BREAST_TOMOSYNTHESIS)
            .media_storage_sop_instance_uid(INSTANCE)
            .private_information_creator_uid(creator);
        let source = source.into_inner().with_meta(meta).unwrap();
        let error = safe_harbor(map(&[(STUDY, creator)]))
            .anonymize_with_generated_uid_map(&source)
            .unwrap_err();
        assert!(matches!(error, Error::InvalidSafeHarborUidMapping(_)));
    }
}
