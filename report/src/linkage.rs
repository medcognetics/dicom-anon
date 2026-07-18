//! DICOM-derived report association without exporting source identifiers.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Cursor;

use dicom_anon::{read_dicom, Anonymizer, DeidentificationPolicy, DicomOption, Options};
use dicom_core::Tag;
use dicom_dictionary_std::tags;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::{Error, Result};

const ASSOCIATION_SCHEMA_VERSION: u32 = 1;

/// Safe association values for one anonymized DICOM object.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DicomAssociation {
    /// Study Instance UID present in the anonymized DICOM.
    pub study_instance_uid: String,
    /// Series Instance UID present in the anonymized DICOM.
    pub series_instance_uid: String,
    /// SOP Instance UID present in the anonymized DICOM.
    pub sop_instance_uid: String,
}

/// Exportable report-to-DICOM association manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssociationManifest {
    /// Manifest schema version.
    pub schema_version: u32,
    /// DICOM profile edition used for the linked anonymization pass.
    pub profile_version: String,
    /// Applied DICOM policy.
    pub policy: DeidentificationPolicy,
    /// Sorted DICOM options applied to the output.
    pub active_options: Vec<DicomOption>,
    /// DICOM keyword containing `patient_output_id` in anonymized objects.
    pub patient_output_attribute: String,
    /// Patient pseudonym shared by the anonymized report and DICOMs.
    pub patient_output_id: String,
    /// Safe output UIDs for all linked DICOM objects.
    pub dicom_objects: Vec<DicomAssociation>,
    /// SHA-256 of the anonymized report output, when bound after rendering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_sha256: Option<String>,
}

impl AssociationManifest {
    /// Bind this manifest to anonymized report bytes. Source bytes are never hashed.
    pub fn bind_output(&mut self, output: &[u8]) {
        let digest = Sha256::digest(output);
        let mut encoded = String::with_capacity(digest.len() * 2);
        for byte in digest {
            write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
        }
        self.output_sha256 = Some(encoded);
    }
}

/// In-memory sensitive mapping. This type deliberately implements neither
/// `Debug` nor `Serialize` so source identifiers cannot enter reports by accident.
pub struct SensitiveLinkageMap {
    mappings: Vec<SensitiveMapping>,
}

struct SensitiveMapping {
    source: Zeroizing<String>,
    replacement: String,
    case_insensitive: bool,
}

pub(crate) struct ReplacementSpan {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) replacement: String,
}

impl SensitiveLinkageMap {
    pub(crate) fn replacement_spans(&self, text: &str) -> Vec<ReplacementSpan> {
        let mut candidates = Vec::new();
        for mapping in &self.mappings {
            if mapping.source.is_empty() || mapping.replacement.is_empty() {
                continue;
            }
            let source = mapping.source.as_bytes();
            if mapping.case_insensitive && source.is_ascii() {
                for start in 0..=text.len().saturating_sub(source.len()) {
                    let end = start + source.len();
                    if text.is_char_boundary(start)
                        && text.is_char_boundary(end)
                        && text.as_bytes()[start..end].eq_ignore_ascii_case(source)
                        && has_identifier_boundaries(text, start, end)
                    {
                        candidates.push(ReplacementSpan {
                            start,
                            end,
                            replacement: mapping.replacement.clone(),
                        });
                    }
                }
            } else {
                for (start, _) in text.match_indices(mapping.source.as_str()) {
                    let end = start + source.len();
                    if has_identifier_boundaries(text, start, end) {
                        candidates.push(ReplacementSpan {
                            start,
                            end,
                            replacement: mapping.replacement.clone(),
                        });
                    }
                }
            }
        }
        candidates.sort_by(|left, right| {
            left.start
                .cmp(&right.start)
                .then_with(|| right.end.cmp(&left.end))
                .then_with(|| left.replacement.cmp(&right.replacement))
        });
        let mut selected: Vec<ReplacementSpan> = Vec::new();
        for candidate in candidates {
            if selected
                .last()
                .is_none_or(|previous| candidate.start >= previous.end)
            {
                selected.push(candidate);
            }
        }
        selected
    }
}

/// DICOM linkage state containing a sensitive in-memory map and safe manifest.
pub struct DicomLinkage {
    sensitive_map: SensitiveLinkageMap,
    manifest: AssociationManifest,
}

impl DicomLinkage {
    /// Derive report replacements from DICOM inputs using the same anonymizer
    /// configuration that will be applied to the accompanying DICOM objects.
    pub fn from_dicom_bytes(inputs: &[Vec<u8>], options: Options) -> Result<Self> {
        if inputs.is_empty() {
            return Err(Error::Dicom(
                "at least one DICOM input is required".to_owned(),
            ));
        }
        let anonymizer = Anonymizer::new(options)
            .map_err(|_| Error::Dicom("the DICOM anonymization options are invalid".to_owned()))?;
        let mut mappings = BTreeMap::<String, (String, bool)>::new();
        let mut patient_pseudonym = None;
        let mut manifest = None;
        let mut associations = BTreeSet::new();

        for bytes in inputs {
            let source = read_dicom(Cursor::new(bytes))
                .map_err(|_| Error::Dicom("could not parse a DICOM input".to_owned()))?;
            let (output, report) = anonymizer
                .anonymize(&source)
                .map_err(|_| Error::Dicom("could not anonymize a DICOM input".to_owned()))?;
            if patient_pseudonym
                .as_ref()
                .is_some_and(|expected| expected != &report.patient_pseudonym)
            {
                return Err(Error::MixedPatients);
            }
            patient_pseudonym = Some(report.patient_pseudonym.clone());
            manifest.get_or_insert_with(|| AssociationManifest {
                schema_version: ASSOCIATION_SCHEMA_VERSION,
                profile_version: report.profile_version.clone(),
                policy: report.policy,
                active_options: report.active_options.clone(),
                patient_output_attribute: if report.policy
                    == DeidentificationPolicy::SafeHarborMetadata
                {
                    "ClinicalTrialSubjectID".to_owned()
                } else {
                    "PatientID".to_owned()
                },
                patient_output_id: report.patient_pseudonym.clone(),
                dicom_objects: Vec::new(),
                output_sha256: None,
            });

            add_mapping(
                &mut mappings,
                element_text(&source, tags::PATIENT_ID),
                Some(report.patient_pseudonym.clone()),
                false,
            )?;
            add_patient_name_mappings(
                &mut mappings,
                element_text(&source, tags::PATIENT_NAME),
                element_text(&output, tags::PATIENT_NAME)
                    .filter(|value| !value.is_empty())
                    .or_else(|| Some(report.patient_pseudonym.clone())),
            )?;

            for tag in identifier_and_temporal_tags() {
                add_mapping(
                    &mut mappings,
                    element_text(&source, tag),
                    element_text(&output, tag),
                    false,
                )?;
            }
            associations.insert(DicomAssociation {
                study_instance_uid: element_text(&output, tags::STUDY_INSTANCE_UID)
                    .unwrap_or_default(),
                series_instance_uid: element_text(&output, tags::SERIES_INSTANCE_UID)
                    .unwrap_or_default(),
                sop_instance_uid: element_text(&output, tags::SOP_INSTANCE_UID).unwrap_or_default(),
            });
        }

        let mut manifest = manifest.expect("non-empty inputs create a manifest");
        manifest.dicom_objects = associations.into_iter().collect();
        let sensitive_map = SensitiveLinkageMap {
            mappings: mappings
                .into_iter()
                .map(
                    |(source, (replacement, case_insensitive))| SensitiveMapping {
                        source: Zeroizing::new(source),
                        replacement,
                        case_insensitive,
                    },
                )
                .collect(),
        };
        Ok(Self {
            sensitive_map,
            manifest,
        })
    }

    /// Access sensitive in-memory mappings for a report anonymization pass.
    #[must_use]
    pub const fn sensitive_map(&self) -> &SensitiveLinkageMap {
        &self.sensitive_map
    }

    /// Access the safe exportable association manifest.
    #[must_use]
    pub const fn manifest(&self) -> &AssociationManifest {
        &self.manifest
    }

    /// Mutably access the safe manifest to bind final output bytes.
    #[must_use]
    pub fn manifest_mut(&mut self) -> &mut AssociationManifest {
        &mut self.manifest
    }
}

fn add_patient_name_mappings(
    mappings: &mut BTreeMap<String, (String, bool)>,
    source: Option<String>,
    replacement: Option<String>,
) -> Result<()> {
    let Some(source) = source.filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let components: Vec<_> = source.split('^').map(str::trim).collect();
    let mut variants = BTreeSet::from([source.clone()]);
    if let (Some(family), Some(given)) = (components.first(), components.get(1)) {
        if !family.is_empty() && !given.is_empty() {
            variants.insert(format!("{family} {given}"));
            variants.insert(format!("{given} {family}"));
            variants.insert(format!("{family}, {given}"));
        }
    }
    for variant in variants {
        add_mapping(mappings, Some(variant), replacement.clone(), true)?;
    }
    Ok(())
}

fn add_mapping(
    mappings: &mut BTreeMap<String, (String, bool)>,
    source: Option<String>,
    replacement: Option<String>,
    case_insensitive: bool,
) -> Result<()> {
    let (Some(source), Some(replacement)) = (source, replacement) else {
        return Ok(());
    };
    if source.is_empty() || replacement.is_empty() {
        return Ok(());
    }
    if let Some((existing, _)) = mappings.get(&source) {
        if existing != &replacement {
            return Err(Error::ConflictingLinkage);
        }
        return Ok(());
    }
    mappings.insert(source, (replacement, case_insensitive));
    Ok(())
}

fn element_text(object: &dicom_anon::DefaultDicomObject, tag: Tag) -> Option<String> {
    object
        .element(tag)
        .ok()?
        .to_str()
        .ok()
        .map(|value| value.trim_end_matches(['\0', ' ']).to_owned())
}

fn identifier_and_temporal_tags() -> [Tag; 17] {
    [
        tags::STUDY_INSTANCE_UID,
        tags::SERIES_INSTANCE_UID,
        tags::SOP_INSTANCE_UID,
        tags::ACCESSION_NUMBER,
        tags::STUDY_ID,
        tags::INSTANCE_CREATION_DATE,
        tags::INSTANCE_CREATION_TIME,
        tags::STUDY_DATE,
        tags::SERIES_DATE,
        tags::ACQUISITION_DATE,
        tags::CONTENT_DATE,
        tags::PATIENT_BIRTH_DATE,
        tags::ACQUISITION_DATE_TIME,
        tags::STUDY_TIME,
        tags::SERIES_TIME,
        tags::ACQUISITION_TIME,
        tags::CONTENT_TIME,
    ]
}

fn has_identifier_boundaries(text: &str, start: usize, end: usize) -> bool {
    let left = &text[..start];
    let right = &text[end..];
    let left_ok = left.chars().next_back().is_none_or(|character| {
        !(is_identifier_character(character)
            || (character == '.'
                && left
                    .strip_suffix('.')
                    .and_then(|prefix| prefix.chars().next_back())
                    .is_some_and(|previous| previous.is_ascii_digit())))
    });
    let right_ok = right.chars().next().is_none_or(|character| {
        !(is_identifier_character(character)
            || (character == '.'
                && right
                    .strip_prefix('.')
                    .and_then(|suffix| suffix.chars().next())
                    .is_some_and(|next| next.is_ascii_digit())))
    });
    left_ok && right_ok
}

fn is_identifier_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '-' | '_')
}
