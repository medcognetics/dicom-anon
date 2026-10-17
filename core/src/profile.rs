//! DICOM PS3.15 confidentiality profile rules and option resolution.

use std::collections::{BTreeMap, BTreeSet};

use clap::ValueEnum;
use dicom_core::header::Header;
use dicom_core::value::Value;
use dicom_core::Tag;
use dicom_dictionary_std::tags;
use dicom_object::mem::InMemDicomObject;
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Human-readable name of the default extended DICOM profile.
pub const PROFILE_NAME: &str = "DICOM PS3.15 Basic Application Confidentiality Profile + Clean Descriptors + Modified Dates + Retain Patient Characteristics";
/// DICOM edition used to construct the rule table.
pub const PROFILE_VERSION: &str = "2026c";
/// Version of the `dicom-anon` crate that writes De-identification Method `(0012,0063)`.
pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");

const OPTION_COLUMN_COUNT: usize = 10;
const DICOM_EXTENDED_OPTIONS: [DicomOption; 3] = [
    DicomOption::CleanDescriptors,
    DicomOption::RetainModifiedDates,
    DicomOption::RetainPatientCharacteristics,
];

/// High-level transformation policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum DeidentificationPolicy {
    /// Configurable DICOM PS3.15 profile and options.
    Dicom,
    /// Safe Harbor-oriented metadata policy with external linkage mappings.
    SafeHarborMetadata,
}

/// Options defined for the DICOM Basic Application Level Confidentiality Profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum DicomOption {
    CleanPixelData,
    CleanRecognizableVisualFeatures,
    CleanGraphics,
    CleanStructuredContent,
    CleanDescriptors,
    RetainFullDates,
    RetainModifiedDates,
    RetainPatientCharacteristics,
    RetainDeviceIdentity,
    RetainInstitutionIdentity,
    RetainUids,
    RetainSafePrivate,
}

impl DicomOption {
    pub(crate) const fn matrix_index(self) -> Option<usize> {
        match self {
            Self::RetainSafePrivate => Some(0),
            Self::RetainUids => Some(1),
            Self::RetainDeviceIdentity => Some(2),
            Self::RetainInstitutionIdentity => Some(3),
            Self::RetainPatientCharacteristics => Some(4),
            Self::RetainFullDates => Some(5),
            Self::RetainModifiedDates => Some(6),
            Self::CleanDescriptors => Some(7),
            Self::CleanStructuredContent => Some(8),
            Self::CleanGraphics => Some(9),
            Self::CleanPixelData | Self::CleanRecognizableVisualFeatures => None,
        }
    }

    const fn from_matrix_index(index: u8) -> Self {
        match index {
            0 => Self::RetainSafePrivate,
            1 => Self::RetainUids,
            2 => Self::RetainDeviceIdentity,
            3 => Self::RetainInstitutionIdentity,
            4 => Self::RetainPatientCharacteristics,
            5 => Self::RetainFullDates,
            6 => Self::RetainModifiedDates,
            7 => Self::CleanDescriptors,
            8 => Self::CleanStructuredContent,
            9 => Self::CleanGraphics,
            _ => unreachable!(),
        }
    }

    /// DCM code and meaning for De-identification Method Code Sequence.
    #[must_use]
    pub const fn method_code(self) -> (&'static str, &'static str) {
        match self {
            Self::CleanPixelData => ("113101", "Clean Pixel Data Option"),
            Self::CleanRecognizableVisualFeatures => {
                ("113102", "Clean Recognizable Visual Features Option")
            }
            Self::CleanGraphics => ("113103", "Clean Graphics Option"),
            Self::CleanStructuredContent => ("113104", "Clean Structured Content Option"),
            Self::CleanDescriptors => ("113105", "Clean Descriptors Option"),
            Self::RetainFullDates => (
                "113106",
                "Retain Longitudinal Temporal Information Full Dates Option",
            ),
            Self::RetainModifiedDates => (
                "113107",
                "Retain Longitudinal Temporal Information Modified Dates Option",
            ),
            Self::RetainPatientCharacteristics => {
                ("113108", "Retain Patient Characteristics Option")
            }
            Self::RetainDeviceIdentity => ("113109", "Retain Device Identity Option"),
            Self::RetainUids => ("113110", "Retain UIDs Option"),
            Self::RetainSafePrivate => ("113111", "Retain Safe Private Option"),
            Self::RetainInstitutionIdentity => ("113112", "Retain Institution Identity Option"),
        }
    }
}

/// A private element known to be safe for a specific private creator.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SafePrivateElement {
    /// Private Creator value.
    pub creator: String,
    /// Low byte of the private data element within its reserved block.
    pub element_offset: u8,
}

/// Serializable profile configuration shared by all interfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileConfiguration {
    pub policy: DeidentificationPolicy,
    pub options: BTreeSet<DicomOption>,
    /// Canonical hexadecimal tag -> exact source value -> safe output value.
    #[serde(default)]
    pub descriptor_mappings: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    pub safe_private_allowlist: BTreeSet<SafePrivateElement>,
    /// Original UID -> independently assigned replacement UID.
    #[serde(default)]
    pub uid_mappings: BTreeMap<String, String>,
}

impl Default for ProfileConfiguration {
    fn default() -> Self {
        Self::dicom_extended()
    }
}

impl ProfileConfiguration {
    /// Construct the DICOM Basic Application Level Confidentiality Profile without options.
    #[must_use]
    pub fn dicom() -> Self {
        Self {
            policy: DeidentificationPolicy::Dicom,
            options: BTreeSet::new(),
            descriptor_mappings: BTreeMap::new(),
            safe_private_allowlist: BTreeSet::new(),
            uid_mappings: BTreeMap::new(),
        }
    }

    /// Construct the default extended DICOM profile.
    #[must_use]
    pub fn dicom_extended() -> Self {
        Self {
            options: DICOM_EXTENDED_OPTIONS.into_iter().collect(),
            ..Self::dicom()
        }
    }

    /// Construct the strict Safe Harbor metadata defaults.
    #[must_use]
    pub fn safe_harbor() -> Self {
        Self {
            policy: DeidentificationPolicy::SafeHarborMetadata,
            options: [
                DicomOption::CleanGraphics,
                DicomOption::CleanStructuredContent,
                DicomOption::CleanDescriptors,
                DicomOption::RetainModifiedDates,
                DicomOption::RetainPatientCharacteristics,
            ]
            .into_iter()
            .collect(),
            descriptor_mappings: BTreeMap::new(),
            safe_private_allowlist: BTreeSet::new(),
            uid_mappings: BTreeMap::new(),
        }
    }

    /// Validate option support, policy constraints, and action compatibility.
    pub fn validate(&self) -> Result<()> {
        for unsupported in [
            DicomOption::CleanPixelData,
            DicomOption::CleanRecognizableVisualFeatures,
        ] {
            if self.options.contains(&unsupported) {
                return Err(Error::UnsupportedProfileOption(format!("{unsupported:?}")));
            }
        }
        if self.options.contains(&DicomOption::RetainFullDates)
            && self.options.contains(&DicomOption::RetainModifiedDates)
        {
            return Err(Error::ConflictingProfileOptions(
                "retain-full-dates and retain-modified-dates are mutually exclusive".to_owned(),
            ));
        }
        if self.policy == DeidentificationPolicy::SafeHarborMetadata {
            for forbidden in [
                DicomOption::RetainFullDates,
                DicomOption::RetainDeviceIdentity,
                DicomOption::RetainInstitutionIdentity,
                DicomOption::RetainUids,
            ] {
                if self.options.contains(&forbidden) {
                    return Err(Error::UnsupportedSafeHarborOption(format!("{forbidden:?}")));
                }
            }
        }
        for &(left, right, tag, name) in OPTION_CONFLICTS {
            let left = DicomOption::from_matrix_index(left);
            let right = DicomOption::from_matrix_index(right);
            if self.has(left) && self.has(right) {
                return Err(Error::ConflictingProfileOptions(format!(
                    "{name} ({:04X},{:04X}) has conflicting actions from {left:?} and {right:?}",
                    tag >> 16,
                    tag as u16
                )));
            }
        }
        for (tag, mappings) in &self.descriptor_mappings {
            if tag.len() != 8
                || !tag
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
            {
                return Err(Error::InvalidProfileConfiguration(
                    "descriptor-map tags must be eight uppercase hexadecimal digits".to_owned(),
                ));
            }
            if mappings
                .values()
                .any(|value| value.is_empty() || value.contains('\0'))
            {
                return Err(Error::InvalidProfileConfiguration(
                    "descriptor-map outputs must be non-empty and contain no null characters"
                        .to_owned(),
                ));
            }
        }
        if self
            .safe_private_allowlist
            .iter()
            .any(|entry| entry.creator.is_empty() || entry.creator.contains('\0'))
        {
            return Err(Error::InvalidProfileConfiguration(
                "safe-private creators must be non-empty and contain no null characters".to_owned(),
            ));
        }
        let mut replacement_uids = BTreeSet::new();
        let source_uids = self.uid_mappings.keys().collect::<BTreeSet<_>>();
        for (source, replacement) in &self.uid_mappings {
            if !valid_uid(source) || !valid_uid(replacement) {
                return Err(Error::InvalidSafeHarborUidMapping(
                    "source and replacement values must be valid DICOM UIDs".to_owned(),
                ));
            }
            if !replacement_uids.insert(replacement) {
                return Err(Error::InvalidSafeHarborUidMapping(
                    "replacement UIDs must be unique".to_owned(),
                ));
            }
            if source == replacement || source_uids.contains(replacement) {
                return Err(Error::InvalidSafeHarborUidMapping(
                    "replacement UIDs must differ from every source UID".to_owned(),
                ));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn has(&self, option: DicomOption) -> bool {
        self.options.contains(&option)
    }

    #[must_use]
    pub fn descriptor_replacement(&self, tag: Tag, source: &str) -> Option<&str> {
        self.descriptor_mappings
            .get(&format!("{:04X}{:04X}", tag.0, tag.1))
            .and_then(|values| values.get(source))
            .map(String::as_str)
    }

    /// Implementation value of De-identification Method: tool version, policy, and DICOM edition.
    pub(crate) fn deidentification_implementation(&self) -> String {
        match self.policy {
            DeidentificationPolicy::Dicom => format!("dicom-anon {TOOL_VERSION} {PROFILE_VERSION}"),
            DeidentificationPolicy::SafeHarborMetadata => {
                format!("dicom-anon {TOOL_VERSION} safe-harbor-metadata {PROFILE_VERSION}")
            }
        }
    }

    pub(crate) fn deidentification_method(&self) -> String {
        let implementation = self.deidentification_implementation();
        std::iter::once("DICOM PS3.15 Basic Profile")
            .chain(self.options.iter().map(|option| option.method_code().1))
            .chain(std::iter::once(implementation.as_str()))
            .collect::<Vec<_>>()
            .join("\\")
    }

    pub(crate) fn allows_safe_private(&self, creator: &str, element_offset: u8) -> bool {
        self.safe_private_allowlist.contains(&SafePrivateElement {
            creator: creator.to_owned(),
            element_offset,
        }) || SAFE_PRIVATE_ATTRIBUTES
            .binary_search(&(creator, element_offset))
            .is_ok()
    }
}

fn valid_uid(uid: &str) -> bool {
    !uid.is_empty()
        && uid.len() <= 64
        && uid.split('.').all(|component| {
            !component.is_empty()
                && component.bytes().all(|byte| byte.is_ascii_digit())
                && (component == "0" || !component.starts_with('0'))
        })
}

/// Effective action for an attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileAction {
    Remove,
    Empty,
    Dummy,
    RemapUid,
    Keep,
    Clean,
}

/// One frozen Table E.1-1 row with all option-column actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileRule {
    tag: u32,
    pub name: &'static str,
    /// Effective action for `ProfileConfiguration::dicom_extended()`.
    pub action: ProfileAction,
    basic_action: ProfileAction,
    option_actions: [Option<ProfileAction>; OPTION_COLUMN_COUNT],
}

impl ProfileRule {
    pub(super) const fn new(
        tag: u32,
        name: &'static str,
        action: ProfileAction,
        basic_action: ProfileAction,
        option_actions: [Option<ProfileAction>; OPTION_COLUMN_COUNT],
    ) -> Self {
        Self {
            tag,
            name,
            action,
            basic_action,
            option_actions,
        }
    }

    #[must_use]
    pub const fn tag(self) -> Tag {
        Tag((self.tag >> 16) as u16, self.tag as u16)
    }

    fn resolve(self, configuration: &ProfileConfiguration) -> Result<ProfileAction> {
        let mut override_action = None;
        let mut override_option = None;
        for option in &configuration.options {
            let Some(index) = option.matrix_index() else {
                continue;
            };
            let Some(action) = self.option_actions[index] else {
                continue;
            };
            if let Some(existing) = override_action {
                if existing != action {
                    return Err(Error::ConflictingProfileOptions(format!(
                        "{} ({:04X},{:04X}) has conflicting actions from {:?} and {:?}",
                        self.name,
                        self.tag().0,
                        self.tag().1,
                        override_option.expect("set with action"),
                        option
                    )));
                }
            } else {
                override_action = Some(action);
                override_option = Some(*option);
            }
        }
        Ok(override_action.unwrap_or(self.basic_action))
    }

    const fn option_action(self, option: DicomOption) -> Option<ProfileAction> {
        match option.matrix_index() {
            Some(index) => self.option_actions[index],
            None => None,
        }
    }
}

/// A resolved rule for a specific configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedProfileRule {
    pub name: &'static str,
    pub action: ProfileAction,
    tag: Tag,
}

impl ResolvedProfileRule {
    #[must_use]
    pub const fn tag(self) -> Tag {
        self.tag
    }
}

include!("profile_generated.rs");

/// Resolve safe private data elements and their required creator attributes.
pub(crate) fn safe_private_tags(
    object: &InMemDicomObject,
    configuration: &ProfileConfiguration,
) -> BTreeSet<Tag> {
    let mut safe = BTreeSet::new();
    if !configuration.has(DicomOption::RetainSafePrivate) {
        return safe;
    }

    let creators = object
        .iter()
        .filter(|element| element.tag().0 % 2 == 1 && (0x0010..=0x00FF).contains(&element.tag().1))
        .filter_map(|element| {
            let creator = element.to_str().ok()?;
            Some((
                element.tag(),
                element.tag().1 as u8,
                creator.trim_matches([' ', '\0']).to_owned(),
            ))
        })
        .collect::<Vec<_>>();

    for &(creator_tag, block, ref creator) in &creators {
        for element in object.iter().filter(|element| {
            element.tag().0 == creator_tag.0 && element.tag().1 >> 8 == u16::from(block)
        }) {
            if configuration.allows_safe_private(creator, element.tag().1 as u8) {
                safe.insert(creator_tag);
                safe.insert(element.tag());
            }
        }
    }

    let Some(characteristics) = object.get(tags::PRIVATE_DATA_ELEMENT_CHARACTERISTICS_SEQUENCE)
    else {
        return safe;
    };
    let Value::Sequence(sequence) = characteristics.value() else {
        return safe;
    };
    for item in sequence.items() {
        let Some(group) = item
            .get(tags::PRIVATE_GROUP_REFERENCE)
            .and_then(|element| element.to_int::<u16>().ok())
        else {
            continue;
        };
        let Some(creator) = item
            .get(tags::PRIVATE_CREATOR_REFERENCE)
            .and_then(|element| element.to_str().ok())
        else {
            continue;
        };
        let creator = creator.trim_matches([' ', '\0']);
        let status = item
            .get(tags::BLOCK_IDENTIFYING_INFORMATION_STATUS)
            .and_then(|element| element.to_str().ok())
            .unwrap_or_default();
        let declared_offsets = if status.trim_matches([' ', '\0']) == "SAFE" {
            None
        } else if status.trim_matches([' ', '\0']) == "MIXED" {
            Some(
                item.get(tags::NONIDENTIFYING_PRIVATE_ELEMENTS)
                    .and_then(|element| element.to_multi_int::<u16>().ok())
                    .unwrap_or_default(),
            )
        } else {
            continue;
        };

        for &(creator_tag, block, ref actual_creator) in &creators {
            if creator_tag.0 != group || actual_creator != creator {
                continue;
            }
            for element in object.iter().filter(|element| {
                element.tag().0 == group && element.tag().1 >> 8 == u16::from(block)
            }) {
                let offset = element.tag().1 & 0x00FF;
                if declared_offsets
                    .as_ref()
                    .is_none_or(|offsets| offsets.contains(&offset))
                {
                    safe.insert(creator_tag);
                    safe.insert(element.tag());
                }
            }
        }
    }
    safe
}

const CURVE_DATA_RULE: ProfileRule = ProfileRule::new(
    0x50000000,
    "Curve Data",
    ProfileAction::Remove,
    ProfileAction::Remove,
    [
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(ProfileAction::Clean),
    ],
);
const OVERLAY_DATA_RULE: ProfileRule = ProfileRule::new(
    0x60003000,
    "Overlay Data",
    ProfileAction::Remove,
    ProfileAction::Remove,
    [
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(ProfileAction::Clean),
    ],
);
const OVERLAY_COMMENTS_RULE: ProfileRule = ProfileRule::new(
    0x60004000,
    "Overlay Comments",
    ProfileAction::Remove,
    ProfileAction::Remove,
    [
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(ProfileAction::Clean),
    ],
);

fn raw_profile_rule(tag: Tag) -> Option<&'static ProfileRule> {
    if tag.0 & 0xFF00 == 0x5000 {
        return Some(&CURVE_DATA_RULE);
    }
    if tag.0 & 0xFF00 == 0x6000 && matches!(tag.1, 0x3000 | 0x4000) {
        return Some(if tag.1 == 0x3000 {
            &OVERLAY_DATA_RULE
        } else {
            &OVERLAY_COMMENTS_RULE
        });
    }
    let key = u32::from(tag.0) << 16 | u32::from(tag.1);
    let rules = profile_rule_group(tag.0);
    rules
        .binary_search_by_key(&key, |rule| rule.tag)
        .ok()
        .map(|index| &rules[index])
}

/// Find the effective rule for the default configuration.
#[must_use]
pub fn profile_rule(tag: Tag) -> Option<&'static ProfileRule> {
    raw_profile_rule(tag)
}

/// Resolve a rule for an explicit configuration.
pub fn profile_rule_for(
    tag: Tag,
    configuration: &ProfileConfiguration,
) -> Result<Option<ResolvedProfileRule>> {
    raw_profile_rule(tag)
        .map(|rule| {
            Ok(ResolvedProfileRule {
                name: rule.name,
                action: rule.resolve(configuration)?,
                tag,
            })
        })
        .transpose()
}

/// Return the action contributed by one option column for a tag.
#[must_use]
pub(crate) fn profile_option_action(tag: Tag, option: DicomOption) -> Option<ProfileAction> {
    raw_profile_rule(tag).and_then(|rule| rule.option_action(option))
}

/// Return the Basic Profile action before option-column overrides.
#[must_use]
pub(crate) fn profile_basic_action(tag: Tag) -> Option<ProfileAction> {
    raw_profile_rule(tag).map(|rule| rule.basic_action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_is_sorted_unique_and_complete() {
        assert_eq!(PROFILE_RULES.len(), 652);
        assert!(PROFILE_RULES
            .windows(2)
            .all(|pair| pair[0].tag < pair[1].tag));
        let counts = PROFILE_RULES.iter().fold([0_usize; 6], |mut counts, rule| {
            let index = match rule.action {
                ProfileAction::Remove => 0,
                ProfileAction::Empty => 1,
                ProfileAction::Dummy => 2,
                ProfileAction::RemapUid => 3,
                ProfileAction::Keep => 4,
                ProfileAction::Clean => 5,
            };
            counts[index] += 1;
            counts
        });
        assert_eq!(counts, [200, 29, 41, 57, 14, 311]);
        assert_eq!(SAFE_PRIVATE_ATTRIBUTES.len(), 457);
        assert!(SAFE_PRIVATE_ATTRIBUTES
            .windows(2)
            .all(|pair| pair[0] < pair[1]));
        assert_eq!(OPTION_CONFLICTS.len(), 2);
        for rule in PROFILE_RULES {
            assert!(profile_rule_group(rule.tag().0)
                .iter()
                .any(|candidate| candidate.tag == rule.tag));
        }
    }

    #[test]
    fn default_extended_options_override_basic_actions() {
        assert_eq!(
            profile_rule(Tag(0x0008, 0x0020)).unwrap().action,
            ProfileAction::Clean
        );
        assert_eq!(
            profile_rule(Tag(0x0010, 0x1010)).unwrap().action,
            ProfileAction::Keep
        );
        assert_eq!(
            profile_rule(Tag(0x0008, 0x1030)).unwrap().action,
            ProfileAction::Clean
        );
        assert_eq!(
            profile_rule(Tag(0x0020, 0x000D)).unwrap().action,
            ProfileAction::RemapUid
        );
        assert_eq!(
            profile_rule(Tag(0x5002, 0x1234)).unwrap().name,
            "Curve Data"
        );
        assert!(profile_rule(Tag(0x7776, 0x0001)).is_none());
    }

    #[test]
    fn deidentification_method_identifies_tool_version_and_edition() {
        let extended = ProfileConfiguration::dicom_extended().deidentification_method();
        assert_eq!(
            extended.rsplit('\\').next(),
            Some(format!("dicom-anon {TOOL_VERSION} {PROFILE_VERSION}").as_str())
        );
        assert_eq!(TOOL_VERSION, env!("CARGO_PKG_VERSION"));
        let safe_harbor = ProfileConfiguration {
            policy: DeidentificationPolicy::SafeHarborMetadata,
            ..ProfileConfiguration::dicom()
        };
        assert_eq!(
            safe_harbor.deidentification_implementation(),
            format!("dicom-anon {TOOL_VERSION} safe-harbor-metadata {PROFILE_VERSION}")
        );
        // Each LO value is limited to 64 characters.
        for configuration in [ProfileConfiguration::dicom_extended(), safe_harbor] {
            assert!(configuration
                .deidentification_method()
                .split('\\')
                .all(|value| value.len() <= 64));
        }
    }

    #[test]
    fn dicom_presets_select_expected_options() {
        let basic = ProfileConfiguration::dicom();
        assert_eq!(basic.policy, DeidentificationPolicy::Dicom);
        assert!(basic.options.is_empty());

        let extended = ProfileConfiguration::dicom_extended();
        assert_eq!(extended.policy, DeidentificationPolicy::Dicom);
        assert_eq!(
            extended.options,
            DICOM_EXTENDED_OPTIONS.into_iter().collect()
        );
        assert_eq!(ProfileConfiguration::default(), extended);
    }

    #[test]
    fn safe_harbor_configuration_has_privacy_preserving_defaults() {
        let configuration = ProfileConfiguration::safe_harbor();
        assert_eq!(
            configuration.policy,
            DeidentificationPolicy::SafeHarborMetadata
        );
        assert!(configuration.has(DicomOption::CleanGraphics));
        assert!(configuration.has(DicomOption::CleanStructuredContent));
        assert!(configuration.validate().is_ok());
    }

    #[test]
    fn configuration_rejects_unsupported_and_conflicting_options() {
        let mut unsupported = ProfileConfiguration::default();
        unsupported.options.insert(DicomOption::CleanPixelData);
        assert!(unsupported.validate().is_err());

        let mut conflicting = ProfileConfiguration::default();
        conflicting.options.insert(DicomOption::RetainFullDates);
        assert!(conflicting.validate().is_err());

        let mut device_dates = ProfileConfiguration::default();
        device_dates
            .options
            .insert(DicomOption::RetainDeviceIdentity);
        assert!(device_dates.validate().is_err());
    }

    #[test]
    fn configuration_rejects_malformed_external_registries() {
        let mut descriptor = ProfileConfiguration::default();
        descriptor
            .descriptor_mappings
            .insert("0008-1030".to_owned(), BTreeMap::new());
        assert!(matches!(
            descriptor.validate(),
            Err(Error::InvalidProfileConfiguration(_))
        ));

        let mut safe_private = ProfileConfiguration::default();
        safe_private
            .safe_private_allowlist
            .insert(SafePrivateElement {
                creator: String::new(),
                element_offset: 1,
            });
        assert!(matches!(
            safe_private.validate(),
            Err(Error::InvalidProfileConfiguration(_))
        ));

        let mut uid_map = ProfileConfiguration::default();
        uid_map
            .uid_mappings
            .insert("01.invalid".to_owned(), "2.25.1".to_owned());
        assert!(matches!(
            uid_map.validate(),
            Err(Error::InvalidSafeHarborUidMapping(_))
        ));

        let mut retained_uid = ProfileConfiguration::safe_harbor();
        retained_uid
            .uid_mappings
            .insert("1.2.3".to_owned(), "1.2.3".to_owned());
        assert!(matches!(
            retained_uid.validate(),
            Err(Error::InvalidSafeHarborUidMapping(_))
        ));

        let mut cross_mapping_collision = ProfileConfiguration::safe_harbor();
        cross_mapping_collision.uid_mappings = [
            ("1.2.3".to_owned(), "1.2.4".to_owned()),
            ("1.2.4".to_owned(), "2.25.1".to_owned()),
        ]
        .into_iter()
        .collect();
        assert!(matches!(
            cross_mapping_collision.validate(),
            Err(Error::InvalidSafeHarborUidMapping(_))
        ));
    }

    #[test]
    fn metadata_options_resolve_representative_table_actions() {
        let mut configuration = ProfileConfiguration::dicom();
        assert_eq!(
            profile_rule_for(Tag(0x0008, 0x1030), &configuration)
                .unwrap()
                .unwrap()
                .action,
            ProfileAction::Remove
        );
        assert_eq!(
            profile_rule_for(Tag(0x0010, 0x1010), &configuration)
                .unwrap()
                .unwrap()
                .action,
            ProfileAction::Remove
        );

        configuration.options.insert(DicomOption::RetainFullDates);
        assert_eq!(
            profile_rule_for(Tag(0x0008, 0x0020), &configuration)
                .unwrap()
                .unwrap()
                .action,
            ProfileAction::Keep
        );
        configuration.options.clear();
        configuration.options.insert(DicomOption::RetainUids);
        assert_eq!(
            profile_rule_for(Tag(0x0020, 0x000D), &configuration)
                .unwrap()
                .unwrap()
                .action,
            ProfileAction::Keep
        );
        configuration.options.clear();
        configuration
            .options
            .insert(DicomOption::RetainInstitutionIdentity);
        assert_eq!(
            profile_rule_for(Tag(0x0008, 0x0080), &configuration)
                .unwrap()
                .unwrap()
                .action,
            ProfileAction::Keep
        );
        configuration.options.clear();
        configuration
            .options
            .insert(DicomOption::CleanStructuredContent);
        assert_eq!(
            profile_rule_for(Tag(0x0040, 0xA730), &configuration)
                .unwrap()
                .unwrap()
                .action,
            ProfileAction::Clean
        );
        configuration.options.clear();
        configuration.options.insert(DicomOption::CleanGraphics);
        assert_eq!(
            profile_rule_for(Tag(0x6000, 0x3000), &configuration)
                .unwrap()
                .unwrap()
                .action,
            ProfileAction::Clean
        );
    }

    #[test]
    fn every_metadata_option_column_is_populated_and_valid_independently() {
        for option in [
            DicomOption::CleanGraphics,
            DicomOption::CleanStructuredContent,
            DicomOption::CleanDescriptors,
            DicomOption::RetainFullDates,
            DicomOption::RetainModifiedDates,
            DicomOption::RetainPatientCharacteristics,
            DicomOption::RetainDeviceIdentity,
            DicomOption::RetainInstitutionIdentity,
            DicomOption::RetainUids,
            DicomOption::RetainSafePrivate,
        ] {
            let configuration = ProfileConfiguration {
                options: [option].into_iter().collect(),
                ..ProfileConfiguration::default()
            };
            assert!(configuration.validate().is_ok(), "{option:?}");
            if option == DicomOption::RetainSafePrivate {
                assert!(!SAFE_PRIVATE_ATTRIBUTES.is_empty());
            } else {
                assert!(
                    PROFILE_RULES
                        .iter()
                        .any(|rule| rule.option_action(option).is_some()),
                    "empty option column for {option:?}"
                );
            }
        }
    }
}
