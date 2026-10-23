//! Core de-identification engine.

use std::collections::BTreeMap;

use dicom_core::header::Header;
use dicom_core::value::{DataSetSequence, Value};
use dicom_core::{DataElement, PrimitiveValue, Tag, VR};
use dicom_dictionary_std::tags;
use dicom_object::mem::{InMemDicomObject, InMemElement};
use dicom_object::{DefaultDicomObject, FileMetaTableBuilder};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;

use crate::pixel;
use crate::profile::{
    profile_basic_action, profile_option_action, profile_rule_for, safe_private_tags,
    DeidentificationPolicy, DicomOption, ProfileAction, ProfileConfiguration, PROFILE_VERSION,
};
use crate::temporal::{
    shift_date_values, shift_date_values_with_adjustments, shift_datetime_values, shift_time_values,
};
use crate::uid_map::{RequiredUid, UidResolver};
use crate::{Error, Result, SecretKey};

const DEFAULT_DATE_SHIFT_RANGE_DAYS: i32 = 3_650;
const DEFAULT_UID_ROOT: &str = "2.25";
const IMPLEMENTATION_CLASS_UID: &str = "2.25.328578283022210450206729724989524158300";
const IMPLEMENTATION_VERSION_NAME: &str = "DICOMANON_010";

/// Configuration for deterministic de-identification.
#[derive(Clone, Debug)]
pub struct Options {
    /// Secret used for keyed pseudonyms and date shifts.
    pub secret: SecretKey,
    /// Root used for replacement UIDs. Defaults to the UUID-derived `2.25` root.
    pub uid_root: String,
    /// Explicit patient-level association key for inputs lacking patient identity attributes.
    pub patient_scope: Option<String>,
    /// Reject an input that already declares itself de-identified.
    pub reject_already_deidentified: bool,
    /// Maximum absolute patient-specific date shift.
    pub date_shift_range_days: i32,
    /// Confidentiality profile and enabled options.
    pub profile: ProfileConfiguration,
}

impl Options {
    /// Construct secure defaults around a caller-provided key.
    #[must_use]
    pub fn new(secret: SecretKey) -> Self {
        Self {
            secret,
            uid_root: DEFAULT_UID_ROOT.to_owned(),
            patient_scope: None,
            reject_already_deidentified: true,
            date_shift_range_days: DEFAULT_DATE_SHIFT_RANGE_DAYS,
            profile: ProfileConfiguration::default(),
        }
    }
}

/// Counts and stable patient-level outputs from one de-identification pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnonymizationReport {
    /// DICOM edition used by the rule table.
    pub profile_version: String,
    /// High-level transformation policy.
    pub policy: DeidentificationPolicy,
    /// Sorted DICOM options applied to the output.
    pub active_options: Vec<DicomOption>,
    /// Deterministic replacement Patient ID.
    pub patient_pseudonym: String,
    /// Patient-scoped whole-day temporal shift.
    pub date_shift_days: i32,
    /// Patient-scoped whole-hour temporal shift.
    pub time_shift_hours: i8,
    /// Attributes removed by profile rules.
    pub removed_attributes: u64,
    /// Private attributes removed.
    pub removed_private_attributes: u64,
    /// Attributes replaced with zero-length values.
    pub emptied_attributes: u64,
    /// Attributes replaced with non-empty dummy or pseudonymous values.
    pub replaced_attributes: u64,
    /// UID attributes remapped.
    pub remapped_uid_attributes: u64,
    /// Date, DateTime, or Time attributes processed by the modified-dates option.
    pub shifted_temporal_attributes: u64,
    /// Temporal attributes generalized to year-level Safe Harbor values.
    pub generalized_temporal_attributes: u64,
    /// JPEG 2000 comment (COM) marker segments removed from Pixel Data.
    #[serde(default)]
    pub removed_jpeg2000_comment_segments: u64,
    /// JPEG and JPEG-LS comment (COM) marker segments removed from Pixel Data.
    #[serde(default)]
    pub removed_jpeg_comment_segments: u64,
    /// JPEG and JPEG-LS application (APPn) marker segments removed from Pixel Data.
    #[serde(default)]
    pub removed_jpeg_application_segments: u64,
    /// Dummy-value replacements by canonical hexadecimal tag, at all nesting levels.
    #[serde(default)]
    pub dummy_replacements: BTreeMap<String, u64>,
    /// Subset of `dummy_replacements` where a clean action found no descriptor-map entry.
    #[serde(default)]
    pub unmapped_dummy_replacements: BTreeMap<String, u64>,
}

impl AnonymizationReport {
    fn new(
        patient_pseudonym: String,
        date_shift_days: i32,
        time_shift_hours: i8,
        profile: &ProfileConfiguration,
    ) -> Self {
        Self {
            profile_version: PROFILE_VERSION.to_owned(),
            policy: profile.policy,
            active_options: profile.options.iter().copied().collect(),
            patient_pseudonym,
            date_shift_days,
            time_shift_hours,
            removed_attributes: 0,
            removed_private_attributes: 0,
            emptied_attributes: 0,
            replaced_attributes: 0,
            remapped_uid_attributes: 0,
            shifted_temporal_attributes: 0,
            generalized_temporal_attributes: 0,
            removed_jpeg2000_comment_segments: 0,
            removed_jpeg_comment_segments: 0,
            removed_jpeg_application_segments: 0,
            dummy_replacements: BTreeMap::new(),
            unmapped_dummy_replacements: BTreeMap::new(),
        }
    }
}

/// Reusable deterministic de-identifier.
#[derive(Clone, Debug)]
pub struct Anonymizer {
    options: Options,
}

impl Anonymizer {
    /// Validate options and construct an anonymizer.
    pub fn new(options: Options) -> Result<Self> {
        if options.date_shift_range_days < 1 {
            return Err(Error::InvalidDateShiftRange);
        }
        if !valid_uid_root(&options.uid_root) {
            return Err(Error::InvalidUidRoot(options.uid_root));
        }
        options.profile.validate()?;
        if options.profile.policy == DeidentificationPolicy::SafeHarborMetadata
            && options.patient_scope.as_deref().is_none_or(str::is_empty)
        {
            return Err(Error::SafeHarborPatientScopeRequired);
        }
        Ok(Self { options })
    }

    /// De-identify a parsed DICOM object without modifying the source.
    pub fn anonymize(
        &self,
        source: &DefaultDicomObject,
    ) -> Result<(DefaultDicomObject, AnonymizationReport)> {
        let (output, report, _) = self.anonymize_with(source, UidResolver::mapped())?;
        Ok((output, report))
    }

    /// List each source UID occurrence that the configured profile replaces.
    ///
    /// This runs the same rule decisions as [`Self::anonymize`] and discards the
    /// output. It fails for the same input errors, except a missing UID mapping.
    /// The result contains source UIDs; keep it under source-data custody.
    pub fn required_uids(&self, source: &DefaultDicomObject) -> Result<Vec<RequiredUid>> {
        let (_, _, uids) = self.anonymize_with(source, UidResolver::collect())?;
        Ok(uids.into_required())
    }

    /// De-identify an object and assign each missing Safe Harbor UID replacement.
    ///
    /// The configured UID map is the starting map. Each missing replacement is a
    /// random version 4 UUID integer from the operating-system CSPRNG under the
    /// UID root. It does not depend on the source UID, the key, or patient data,
    /// and it differs from every UID in the source object and the starting map.
    /// The result returns the complete map only when de-identification succeeds.
    pub fn anonymize_with_generated_uid_map(
        &self,
        source: &DefaultDicomObject,
    ) -> Result<(
        DefaultDicomObject,
        AnonymizationReport,
        BTreeMap<String, String>,
    )> {
        if self.options.profile.policy != DeidentificationPolicy::SafeHarborMetadata {
            return Err(Error::UidMapGenerationRequiresSafeHarbor);
        }
        let resolver = UidResolver::generate(source, self.options.profile.uid_mappings.clone())?;
        let (output, report, uids) = self.anonymize_with(source, resolver)?;
        Ok((output, report, uids.into_map()))
    }

    fn anonymize_with(
        &self,
        source: &DefaultDicomObject,
        uids: UidResolver,
    ) -> Result<(DefaultDicomObject, AnonymizationReport, UidResolver)> {
        if self.options.reject_already_deidentified
            && element_text(source, tags::PATIENT_IDENTITY_REMOVED)
                .is_some_and(|value| value.eq_ignore_ascii_case("YES"))
        {
            return Err(Error::AlreadyDeidentified);
        }
        pixel::check_gates(
            source,
            Some(source.meta().media_storage_sop_class_uid()),
            &self.options.profile,
        )?;
        let transfer_syntax = source.meta().transfer_syntax().trim_end_matches('\0');
        let codestream = pixel::Codestream::for_transfer_syntax(transfer_syntax)
            .map(|kind| pixel::clean_codestreams(source, kind).map(|cleaning| (kind, cleaning)))
            .transpose()?;

        let patient_scope = self.patient_scope(source)?;
        let patient_pseudonym = self.options.secret.patient_id(&patient_scope);
        let (date_shift_days, time_shift_hours) =
            if self.options.profile.policy == DeidentificationPolicy::SafeHarborMetadata {
                (0, 0)
            } else {
                self.options
                    .secret
                    .temporal_shifts(&patient_scope, self.options.date_shift_range_days)
            };
        let mut report = AnonymizationReport::new(
            patient_pseudonym.clone(),
            date_shift_days,
            time_shift_hours,
            &self.options.profile,
        );
        let uids = RefCell::new(uids);
        let context = Context {
            patient_pseudonym: &patient_pseudonym,
            date_shift_days,
            time_shift_hours: i32::from(time_shift_hours),
            path: String::new(),
            uids: &uids,
        };

        let replacements = match codestream {
            Some((kind, cleaning)) => {
                let removed = cleaning.removed;
                if kind == pixel::Codestream::Jpeg2000 {
                    report.removed_jpeg2000_comment_segments = removed.comments;
                } else {
                    report.removed_jpeg_comment_segments = removed.comments;
                    report.removed_jpeg_application_segments = removed.applications;
                }
                cleaning.replacement
            }
            None => Vec::new(),
        };
        let mut output = self.process_dataset(source, &context, &mut report, replacements)?;
        // A source value that profile processing kept can be stale or wrong.
        match pixel::repair_total_length(&mut output, transfer_syntax) {
            Some(pixel::TotalLengthRepair::Replaced) => report.replaced_attributes += 1,
            Some(pixel::TotalLengthRepair::Removed) => report.removed_attributes += 1,
            None => {}
        }
        if pixel::remove_single_sample_planar_configuration(&mut output) {
            report.removed_attributes += 1;
        }
        add_deidentification_markers(&mut output, &self.options.profile, &patient_pseudonym);
        let meta = FileMetaTableBuilder::new()
            .transfer_syntax(transfer_syntax)
            .implementation_class_uid(IMPLEMENTATION_CLASS_UID)
            .implementation_version_name(IMPLEMENTATION_VERSION_NAME);
        let output = output
            .with_meta(meta)
            .map_err(|error| Error::Write(error.to_string()))?;
        Ok((output, report, uids.into_inner()))
    }

    fn patient_scope(&self, source: &DefaultDicomObject) -> Result<String> {
        if let Some(scope) = self
            .options
            .patient_scope
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            return Ok(format!("EXPLICIT={scope}"));
        }
        let patient_id = element_text(source, tags::PATIENT_ID).unwrap_or_default();
        let issuer = element_text(source, tags::ISSUER_OF_PATIENT_ID).unwrap_or_default();
        if !patient_id.is_empty() {
            return Ok(format!("ID={patient_id}\0ISSUER={issuer}"));
        }
        if let Some(name) =
            element_text(source, tags::PATIENT_NAME).filter(|value| !value.is_empty())
        {
            return Ok(format!("NAME={name}"));
        }
        Err(Error::MissingPatientScope)
    }

    /// Process one data set.
    ///
    /// Each element of `replacements` replaces the element with the same tag when profile
    /// processing keeps that element. A kept element is not copied from the source, so peak
    /// memory does not include a third copy of rewritten Pixel Data.
    fn process_dataset(
        &self,
        source: &InMemDicomObject,
        context: &Context<'_>,
        report: &mut AnonymizationReport,
        mut replacements: Vec<InMemElement>,
    ) -> Result<InMemDicomObject> {
        let mut output = InMemDicomObject::new_empty();
        let mut date_adjustments = Vec::new();
        let safe_private_tags = safe_private_tags(source, &self.options.profile);
        for element in source.iter() {
            let tag = element.tag();
            if is_private(tag) {
                if !safe_private_tags.contains(&tag) {
                    report.removed_private_attributes += 1;
                    continue;
                }
                if let Some(processed) =
                    self.process_element(element, ProfileAction::Keep, context, report)?
                {
                    output.put(processed);
                }
                continue;
            }
            if tag.0 == 0x0004 {
                report.removed_attributes += 1;
                continue;
            }

            let mut action = profile_rule_for(tag, &self.options.profile)?
                .map(|rule| rule.action)
                .or_else(|| {
                    matches!(element.vr(), VR::DA | VR::DT | VR::TM).then_some(
                        if self.options.profile.has(DicomOption::RetainFullDates) {
                            ProfileAction::Keep
                        } else if self.options.profile.has(DicomOption::RetainModifiedDates) {
                            ProfileAction::Clean
                        } else {
                            ProfileAction::Remove
                        },
                    )
                })
                .unwrap_or_else(|| self.unknown_action(tag, element.vr()));
            if self.options.profile.has(DicomOption::CleanGraphics)
                && profile_option_action(tag, DicomOption::CleanGraphics).is_some()
            {
                action = ProfileAction::Remove;
            }
            if action == ProfileAction::Clean
                && element.vr() == VR::TM
                && self.options.profile.policy == DeidentificationPolicy::Dicom
            {
                if let Some(adjustment) =
                    paired_day_adjustment(source, element, context.time_shift_hours)?
                {
                    date_adjustments.push(adjustment);
                }
            }
            if let Some(index) = replacements
                .iter()
                .position(|replacement| replacement.tag() == tag)
            {
                let replacement = replacements.swap_remove(index);
                if action == ProfileAction::Keep
                    || self
                        .process_element(element, action, context, report)?
                        .is_some()
                {
                    output.put(replacement);
                }
                continue;
            }
            if let Some(processed) = self.process_element(element, action, context, report)? {
                output.put(processed);
            }
        }
        for (date_tag, adjustments) in date_adjustments {
            let source_date = source
                .get(date_tag)
                .expect("paired date was checked while processing its time");
            let original = source_date.to_str().map_err(|_| Error::InvalidTemporal {
                tag: format_tag(date_tag),
                vr: "DA",
            })?;
            let shifted = shift_date_values_with_adjustments(
                &original,
                context.date_shift_days,
                &adjustments,
                date_tag,
            )?;
            output.put(InMemElement::new(date_tag, source_date.vr(), shifted));
        }
        Ok(output)
    }

    fn process_element(
        &self,
        element: &InMemElement,
        action: ProfileAction,
        context: &Context<'_>,
        report: &mut AnonymizationReport,
    ) -> Result<Option<InMemElement>> {
        let tag = element.tag();
        if tag == tags::PATIENT_ID {
            if self.options.profile.policy == DeidentificationPolicy::SafeHarborMetadata {
                report.emptied_attributes += 1;
                return Ok(Some(InMemElement::empty(tag, element.vr())));
            }
            report.replaced_attributes += 1;
            return Ok(Some(InMemElement::new(
                tag,
                element.vr(),
                context.patient_pseudonym,
            )));
        }
        if tag == tags::PATIENT_NAME {
            if self.options.profile.policy == DeidentificationPolicy::SafeHarborMetadata {
                report.emptied_attributes += 1;
                return Ok(Some(InMemElement::empty(tag, element.vr())));
            }
            report.replaced_attributes += 1;
            let name = format!("ANON^{}", &context.patient_pseudonym[4..16]);
            return Ok(Some(InMemElement::new(tag, element.vr(), name)));
        }
        if tag == tags::PATIENT_AGE
            && self
                .options
                .profile
                .has(DicomOption::RetainPatientCharacteristics)
        {
            report.replaced_attributes += 1;
            return clean_patient_age(element).map(Some);
        }
        match action {
            ProfileAction::Remove => {
                report.removed_attributes += 1;
                Ok(None)
            }
            ProfileAction::Empty => {
                report.emptied_attributes += 1;
                Ok(Some(InMemElement::empty(tag, element.vr())))
            }
            ProfileAction::Dummy => {
                report.replaced_attributes += 1;
                self.dummy_element(element, context, report).map(Some)
            }
            ProfileAction::RemapUid => {
                if element.vr() == VR::SQ {
                    return self
                        .clean_sequence_or_clone(element, context, report)
                        .map(Some);
                }
                report.remapped_uid_attributes += 1;
                let original = element
                    .to_str()
                    .map_err(|_| Error::MissingAttribute("UID value"))?;
                let remapped = original
                    .split('\\')
                    .map(|uid| self.remap_uid(uid, tag, context))
                    .collect::<Result<Vec<_>>>()?
                    .join("\\");
                Ok(Some(InMemElement::new(tag, VR::UI, remapped)))
            }
            ProfileAction::Keep => self
                .clean_sequence_or_clone(element, context, report)
                .map(Some),
            ProfileAction::Clean => {
                if element.vr() == VR::SQ {
                    let clean_structured = self
                        .options
                        .profile
                        .has(DicomOption::CleanStructuredContent)
                        && profile_option_action(tag, DicomOption::CleanStructuredContent)
                            .is_some();
                    let clean_descriptor = self.options.profile.has(DicomOption::CleanDescriptors)
                        && profile_option_action(tag, DicomOption::CleanDescriptors).is_some();
                    if clean_descriptor
                        && !clean_structured
                        && profile_basic_action(tag) == Some(ProfileAction::Remove)
                    {
                        report.removed_attributes += 1;
                        return Ok(None);
                    }
                    if clean_descriptor || clean_structured {
                        report.replaced_attributes += 1;
                        return self.dummy_sequence(element, context, report).map(Some);
                    }
                    return self
                        .clean_sequence_or_clone(element, context, report)
                        .map(Some);
                }
                if element.vr() == VR::DA {
                    let original = element.to_str().map_err(|_| Error::InvalidTemporal {
                        tag: format_tag(tag),
                        vr: vr_name(element.vr()),
                    })?;
                    if self.options.profile.policy == DeidentificationPolicy::SafeHarborMetadata {
                        let generalized = generalize_date_values(&original, tag)?;
                        report.generalized_temporal_attributes += 1;
                        return Ok(Some(InMemElement::new(tag, element.vr(), generalized)));
                    }
                    let shifted = shift_date_values(&original, context.date_shift_days, tag, "DA")?;
                    report.shifted_temporal_attributes += 1;
                    return Ok(Some(InMemElement::new(tag, element.vr(), shifted)));
                }
                if element.vr() == VR::DT {
                    let original = element.to_str().map_err(|_| Error::InvalidTemporal {
                        tag: format_tag(tag),
                        vr: "DT",
                    })?;
                    if self.options.profile.policy == DeidentificationPolicy::SafeHarborMetadata {
                        let generalized = generalize_datetime_values(&original, tag)?;
                        report.generalized_temporal_attributes += 1;
                        return Ok(Some(InMemElement::new(tag, element.vr(), generalized)));
                    }
                    let shifted = shift_datetime_values(
                        &original,
                        context.date_shift_days,
                        context.time_shift_hours,
                        tag,
                    )?;
                    report.shifted_temporal_attributes += 1;
                    return Ok(Some(InMemElement::new(tag, element.vr(), shifted)));
                }
                if element.vr() == VR::TM {
                    let value = element.to_str().map_err(|_| Error::InvalidTemporal {
                        tag: format_tag(tag),
                        vr: "TM",
                    })?;
                    if self.options.profile.policy == DeidentificationPolicy::SafeHarborMetadata {
                        let generalized = generalize_time_values(&value, tag)?;
                        report.generalized_temporal_attributes += 1;
                        return Ok(Some(InMemElement::new(tag, VR::TM, generalized)));
                    }
                    let (shifted, _) = shift_time_values(&value, context.time_shift_hours, tag)?;
                    report.shifted_temporal_attributes += 1;
                    return Ok(Some(InMemElement::new(tag, VR::TM, shifted)));
                }
                if let Ok(source) = element.to_str() {
                    if let Some(replacement) =
                        self.options.profile.descriptor_replacement(tag, &source)
                    {
                        validate_descriptor_replacement(element.vr(), replacement)?;
                        report.replaced_attributes += 1;
                        return Ok(Some(InMemElement::new(tag, element.vr(), replacement)));
                    }
                }
                if self.options.profile.has(DicomOption::CleanDescriptors)
                    && profile_option_action(tag, DicomOption::CleanDescriptors).is_some()
                    && profile_basic_action(tag) == Some(ProfileAction::Remove)
                {
                    report.removed_attributes += 1;
                    return Ok(None);
                }
                self.record_unmapped(tag, context, report)?;
                report.replaced_attributes += 1;
                self.dummy_element(element, context, report).map(Some)
            }
        }
    }

    fn unknown_action(&self, tag: Tag, vr: VR) -> ProfileAction {
        if vr == VR::UI && is_nonidentifying_uid_tag(tag) {
            return ProfileAction::Keep;
        }
        if self.options.profile.policy == DeidentificationPolicy::SafeHarborMetadata
            && matches!(
                vr,
                VR::AE
                    | VR::LO
                    | VR::LT
                    | VR::PN
                    | VR::SH
                    | VR::ST
                    | VR::UC
                    | VR::UR
                    | VR::UT
                    | VR::UI
            )
        {
            return if vr == VR::UI {
                ProfileAction::RemapUid
            } else {
                ProfileAction::Clean
            };
        }
        ProfileAction::Keep
    }

    fn remap_uid(&self, original: &str, tag: Tag, context: &Context<'_>) -> Result<String> {
        context.uids.borrow_mut().resolve(
            original.trim_end_matches('\0'),
            || context.element_path(tag),
            &self.options,
        )
    }

    fn clean_sequence_or_clone(
        &self,
        element: &InMemElement,
        context: &Context<'_>,
        report: &mut AnonymizationReport,
    ) -> Result<InMemElement> {
        let Value::Sequence(sequence) = element.value() else {
            return Ok(element.clone());
        };
        let items = sequence
            .items()
            .iter()
            .enumerate()
            .map(|(index, item)| {
                self.process_dataset(
                    item,
                    &context.item(element.tag(), index),
                    report,
                    Vec::new(),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(InMemElement::new(
            element.tag(),
            VR::SQ,
            DataSetSequence::from(items),
        ))
    }

    fn dummy_element(
        &self,
        element: &InMemElement,
        context: &Context<'_>,
        report: &mut AnonymizationReport,
    ) -> Result<InMemElement> {
        let tag = element.tag();
        if element.vr() != VR::SQ {
            record_dummy(tag, report);
        }
        let value = match element.vr() {
            VR::SQ => return self.dummy_sequence(element, context, report),
            VR::AE | VR::CS | VR::LO | VR::LT | VR::SH | VR::ST | VR::UC | VR::UR | VR::UT => {
                PrimitiveValue::from("ANONYMIZED")
            }
            VR::PN => PrimitiveValue::from("ANON^ANON"),
            VR::DA => PrimitiveValue::from("19000101"),
            VR::DT => PrimitiveValue::from("19000101000000"),
            VR::TM => PrimitiveValue::from("000000"),
            VR::AS => PrimitiveValue::from("000Y"),
            VR::DS | VR::IS => PrimitiveValue::from("0"),
            VR::UI => PrimitiveValue::from(self.options.secret.uid(
                &self.options.uid_root,
                &format!("MISSING:{}", format_tag(tag)),
            )),
            VR::US => PrimitiveValue::from(0_u16),
            VR::SS => PrimitiveValue::from(0_i16),
            VR::UL => PrimitiveValue::from(0_u32),
            VR::SL => PrimitiveValue::from(0_i32),
            VR::UV => PrimitiveValue::from(0_u64),
            VR::SV => PrimitiveValue::from(0_i64),
            VR::FL => PrimitiveValue::from(0_f32),
            VR::FD => PrimitiveValue::from(0_f64),
            VR::AT => PrimitiveValue::from(Tag(0, 0)),
            VR::OB | VR::UN => PrimitiveValue::from(vec![0_u8, 0]),
            VR::OD => PrimitiveValue::from(0_f64),
            VR::OF => PrimitiveValue::from(0_f32),
            VR::OL => PrimitiveValue::from(0_u32),
            VR::OV => PrimitiveValue::from(0_u64),
            VR::OW => PrimitiveValue::from(0_u16),
        };
        Ok(DataElement::new(tag, element.vr(), value))
    }

    fn dummy_sequence(
        &self,
        element: &InMemElement,
        context: &Context<'_>,
        report: &mut AnonymizationReport,
    ) -> Result<InMemElement> {
        let Value::Sequence(sequence) = element.value() else {
            return Err(Error::InvalidSequence(format_tag(element.tag())));
        };
        record_dummy(element.tag(), report);
        let mut items = sequence
            .items()
            .iter()
            .enumerate()
            .map(|(index, item)| {
                self.dummy_dataset(item, &context.item(element.tag(), index), report)
            })
            .collect::<Result<Vec<_>>>()?;
        if items.is_empty() {
            items.push(InMemDicomObject::new_empty());
        }
        Ok(InMemElement::new(
            element.tag(),
            VR::SQ,
            DataSetSequence::from(items),
        ))
    }

    fn dummy_dataset(
        &self,
        source: &InMemDicomObject,
        context: &Context<'_>,
        report: &mut AnonymizationReport,
    ) -> Result<InMemDicomObject> {
        let mut output = InMemDicomObject::new_empty();
        for element in source.iter() {
            if is_private(element.tag()) {
                report.removed_private_attributes += 1;
                continue;
            }
            if element.tag().0 == 0x0004 {
                report.removed_attributes += 1;
                continue;
            }
            report.replaced_attributes += 1;
            output.put(self.dummy_element(element, context, report)?);
        }
        Ok(output)
    }

    /// Record a clean action that found no descriptor-map entry, or fail closed.
    fn record_unmapped(
        &self,
        tag: Tag,
        context: &Context<'_>,
        report: &mut AnonymizationReport,
    ) -> Result<()> {
        let key = format!("{:04X}{:04X}", tag.0, tag.1);
        if self.options.profile.fail_on_unmapped.contains(&key) {
            return Err(Error::UnmappedDummyReplacement {
                tag: format_tag(tag),
                path: context.element_path(tag),
            });
        }
        *report.unmapped_dummy_replacements.entry(key).or_default() += 1;
        Ok(())
    }
}

fn record_dummy(tag: Tag, report: &mut AnonymizationReport) {
    *report
        .dummy_replacements
        .entry(format!("{:04X}{:04X}", tag.0, tag.1))
        .or_default() += 1;
}

struct Context<'a> {
    patient_pseudonym: &'a str,
    date_shift_days: i32,
    time_shift_hours: i32,
    /// Path of the enclosing sequence item; empty at the top level.
    path: String,
    /// UID resolution state shared by every item of one object.
    uids: &'a RefCell<UidResolver>,
}

impl<'a> Context<'a> {
    fn item(&self, tag: Tag, index: usize) -> Context<'a> {
        Context {
            patient_pseudonym: self.patient_pseudonym,
            date_shift_days: self.date_shift_days,
            time_shift_hours: self.time_shift_hours,
            path: format!("{}[{index}]", self.element_path(tag)),
            uids: self.uids,
        }
    }

    fn element_path(&self, tag: Tag) -> String {
        if self.path.is_empty() {
            format_tag(tag)
        } else {
            format!("{}/{}", self.path, format_tag(tag))
        }
    }
}

fn paired_day_adjustment(
    source: &InMemDicomObject,
    time_element: &InMemElement,
    time_shift_hours: i32,
) -> Result<Option<(Tag, Vec<i32>)>> {
    let time_tag = time_element.tag();
    let Some(date_tag) = paired_date_tag(time_tag) else {
        return Ok(None);
    };
    let Some(date_element) = source.get(date_tag) else {
        return Ok(None);
    };
    let time_value = time_element.to_str().map_err(|_| Error::InvalidTemporal {
        tag: format_tag(time_tag),
        vr: "TM",
    })?;
    let date_value = date_element.to_str().map_err(|_| Error::InvalidTemporal {
        tag: format_tag(date_tag),
        vr: "DA",
    })?;
    let (_, day_adjustments) = shift_time_values(&time_value, time_shift_hours, time_tag)?;
    if date_value.split('\\').count() != day_adjustments.len() {
        return Err(Error::MismatchedTemporalPair {
            date_tag: format_tag(date_tag),
            time_tag: format_tag(time_tag),
        });
    }
    Ok(Some((date_tag, day_adjustments)))
}

const fn paired_date_tag(time_tag: Tag) -> Option<Tag> {
    let tag = match (time_tag.0, time_tag.1) {
        (0x0008, 0x0013) => Tag(0x0008, 0x0012),
        (0x0008, 0x0030) => Tag(0x0008, 0x0020),
        (0x0008, 0x0031) => Tag(0x0008, 0x0021),
        (0x0008, 0x0032) => Tag(0x0008, 0x0022),
        (0x0008, 0x0033) => Tag(0x0008, 0x0023),
        (0x0008, 0x0034) => Tag(0x0008, 0x0024),
        (0x0008, 0x0035) => Tag(0x0008, 0x0025),
        (0x0014, 0x407C) => Tag(0x0014, 0x407E),
        (0x0018, 0x1014) => Tag(0x0018, 0x1012),
        (0x0018, 0x1201) => Tag(0x0018, 0x1200),
        (0x0018, 0x700E) => Tag(0x0018, 0x700C),
        (0x0020, 0x3405) => Tag(0x0020, 0x3403),
        (0x0032, 0x0033) => Tag(0x0032, 0x0032),
        (0x0032, 0x0035) => Tag(0x0032, 0x0034),
        (0x0032, 0x1001) => Tag(0x0032, 0x1000),
        (0x0032, 0x1011) => Tag(0x0032, 0x1010),
        (0x0032, 0x1041) => Tag(0x0032, 0x1040),
        (0x0032, 0x1051) => Tag(0x0032, 0x1050),
        (0x0038, 0x001B) => Tag(0x0038, 0x001A),
        (0x0038, 0x001D) => Tag(0x0038, 0x001C),
        (0x0038, 0x0021) => Tag(0x0038, 0x0020),
        (0x0038, 0x0032) => Tag(0x0038, 0x0030),
        (0x0040, 0x0003) => Tag(0x0040, 0x0002),
        (0x0040, 0x0005) => Tag(0x0040, 0x0004),
        (0x0040, 0x0245) => Tag(0x0040, 0x0244),
        (0x0040, 0x0251) => Tag(0x0040, 0x0250),
        (0x0040, 0x2005) => Tag(0x0040, 0x2004),
        (0x0040, 0xA024) => Tag(0x0040, 0xA023),
        (0x0040, 0xA112) => Tag(0x0040, 0xA110),
        (0x0040, 0xA122) => Tag(0x0040, 0xA121),
        (0x0040, 0xA193) => Tag(0x0040, 0xA192),
        (0x0070, 0x0083) => Tag(0x0070, 0x0082),
        (0x2100, 0x0050) => Tag(0x2100, 0x0040),
        (0x3006, 0x0009) => Tag(0x3006, 0x0008),
        (0x3008, 0x0025) => Tag(0x3008, 0x0024),
        (0x3008, 0x0164) => Tag(0x3008, 0x0162),
        (0x3008, 0x0168) => Tag(0x3008, 0x0166),
        (0x3008, 0x0251) => Tag(0x3008, 0x0250),
        (0x300A, 0x0007) => Tag(0x300A, 0x0006),
        (0x300A, 0x022E) => Tag(0x300A, 0x022C),
        (0x300E, 0x0005) => Tag(0x300E, 0x0004),
        (0x4008, 0x0101) => Tag(0x4008, 0x0100),
        (0x4008, 0x0109) => Tag(0x4008, 0x0108),
        (0x4008, 0x0113) => Tag(0x4008, 0x0112),
        _ => return None,
    };
    Some(tag)
}

fn add_deidentification_markers(
    output: &mut InMemDicomObject,
    profile: &ProfileConfiguration,
    patient_pseudonym: &str,
) {
    output.put_str(tags::PATIENT_IDENTITY_REMOVED, VR::CS, "YES");
    let method = profile.deidentification_method();
    output.put_str(tags::DEIDENTIFICATION_METHOD, VR::LO, method);
    output.put_str(
        tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED,
        VR::CS,
        if profile.has(DicomOption::RetainFullDates) {
            "UNMODIFIED"
        } else if profile.has(DicomOption::RetainModifiedDates) {
            "MODIFIED"
        } else {
            "REMOVED"
        },
    );
    if profile.policy == DeidentificationPolicy::SafeHarborMetadata {
        output.put_str(tags::CLINICAL_TRIAL_SUBJECT_ID, VR::LO, patient_pseudonym);
    }

    let codes: Vec<_> = std::iter::once(("113100", "Basic Application Confidentiality Profile"))
        .chain(profile.options.iter().map(|option| option.method_code()))
        .map(|(code, meaning)| {
            let mut item = InMemDicomObject::new_empty();
            item.put_str(tags::CODE_VALUE, VR::SH, code);
            item.put_str(tags::CODING_SCHEME_DESIGNATOR, VR::SH, "DCM");
            item.put_str(tags::CODE_MEANING, VR::LO, meaning);
            item
        })
        .collect();
    output.put(InMemElement::new(
        tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE,
        VR::SQ,
        DataSetSequence::from(codes),
    ));
}

fn clean_patient_age(element: &InMemElement) -> Result<InMemElement> {
    let value = element.to_str().map_err(|_| Error::InvalidPatientAge)?;
    if value.contains('\\') || value.len() != 4 {
        return Err(Error::InvalidPatientAge);
    }
    let bytes = value.as_bytes();
    if !bytes[..3].iter().all(u8::is_ascii_digit) || !matches!(bytes[3], b'D' | b'W' | b'M' | b'Y')
    {
        return Err(Error::InvalidPatientAge);
    }
    let cleaned = if bytes[3] == b'Y'
        && value[..3]
            .parse::<u16>()
            .map_err(|_| Error::InvalidPatientAge)?
            >= 90
    {
        "090Y"
    } else {
        &value
    };
    Ok(InMemElement::new(element.tag(), VR::AS, cleaned))
}

fn generalize_date_values(value: &str, tag: Tag) -> Result<String> {
    let validated = shift_date_values(value, 0, tag, "DA")?;
    Ok(validated
        .split('\\')
        .map(|date| format!("{}0101", &date[..4]))
        .collect::<Vec<_>>()
        .join("\\"))
}

fn generalize_datetime_values(value: &str, tag: Tag) -> Result<String> {
    let validated = shift_datetime_values(value, 0, 0, tag)?;
    Ok(validated
        .split('\\')
        .map(|datetime| datetime[..4].to_owned())
        .collect::<Vec<_>>()
        .join("\\"))
}

fn generalize_time_values(value: &str, tag: Tag) -> Result<String> {
    let (validated, _) = shift_time_values(value, 0, tag)?;
    Ok(validated
        .split('\\')
        .map(|_| "000000")
        .collect::<Vec<_>>()
        .join("\\"))
}

fn validate_descriptor_replacement(vr: VR, value: &str) -> Result<()> {
    let maximum = match vr {
        VR::AE => 16,
        VR::CS | VR::SH => 16,
        VR::LO => 64,
        VR::ST => 1_024,
        VR::LT => 10_240,
        _ => usize::MAX,
    };
    if value.is_empty() || value.len() > maximum || value.contains('\0') {
        return Err(Error::MissingAttribute("valid descriptor replacement"));
    }
    Ok(())
}

const fn is_nonidentifying_uid_tag(tag: Tag) -> bool {
    matches!(
        (tag.0, tag.1),
        (0x0002, 0x0002)
            | (0x0002, 0x0010)
            | (0x0008, 0x0016)
            | (0x0008, 0x001A)
            | (0x0008, 0x001B)
            | (0x0008, 0x1150)
    )
}

fn element_text(object: &InMemDicomObject, tag: Tag) -> Option<String> {
    object
        .get(tag)
        .and_then(|element| element.to_str().ok())
        .map(|value| value.trim_matches([' ', '\0']).to_owned())
}

fn is_private(tag: Tag) -> bool {
    tag.0 % 2 == 1
}

fn valid_uid_root(root: &str) -> bool {
    !root.is_empty()
        && root.len() <= 24
        && root.split('.').all(|component| {
            !component.is_empty()
                && component.bytes().all(|byte| byte.is_ascii_digit())
                && (component == "0" || !component.starts_with('0'))
        })
}

fn format_tag(tag: Tag) -> String {
    format!("({:04X},{:04X})", tag.0, tag.1)
}

const fn vr_name(vr: VR) -> &'static str {
    match vr {
        VR::DA => "DA",
        VR::DT => "DT",
        VR::TM => "TM",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use dicom_core::value::DataSetSequence;
    use dicom_dictionary_std::uids;

    const KEY: [u8; 32] = [42; 32];

    fn test_object(patient_id: &str, study_date: &str, study_uid: &str) -> DefaultDicomObject {
        let mut object = InMemDicomObject::new_empty();
        object.put_str(
            tags::SOP_CLASS_UID,
            VR::UI,
            uids::SECONDARY_CAPTURE_IMAGE_STORAGE,
        );
        object.put_str(tags::SOP_INSTANCE_UID, VR::UI, format!("{study_uid}.1"));
        object.put_str(tags::STUDY_INSTANCE_UID, VR::UI, study_uid);
        object.put_str(tags::SERIES_INSTANCE_UID, VR::UI, format!("{study_uid}.2"));
        object.put_str(tags::PATIENT_ID, VR::LO, patient_id);
        object.put_str(tags::PATIENT_NAME, VR::PN, "Doe^Jane");
        object.put_str(tags::STUDY_DATE, VR::DA, study_date);
        object.put_str(Tag(0x0011, 0x0010), VR::LO, "PRIVATE CREATOR");
        object
            .with_meta(FileMetaTableBuilder::new().transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN))
            .unwrap()
    }

    fn anonymizer() -> Anonymizer {
        Anonymizer::new(Options::new(SecretKey::new(KEY).unwrap())).unwrap()
    }

    #[test]
    fn same_patient_keeps_association_and_chronology_across_studies() {
        let first = test_object("PATIENT-1", "20200101", "1.2.3.1");
        let second = test_object("PATIENT-1", "20200301", "1.2.3.2");
        let (first, first_report) = anonymizer().anonymize(&first).unwrap();
        let (second, second_report) = anonymizer().anonymize(&second).unwrap();

        assert_eq!(
            first_report.patient_pseudonym,
            second_report.patient_pseudonym
        );
        assert_eq!(first_report.date_shift_days, second_report.date_shift_days);
        assert_eq!(
            first_report.time_shift_hours,
            second_report.time_shift_hours
        );
        assert_eq!(
            element_text(&first, tags::PATIENT_ID),
            element_text(&second, tags::PATIENT_ID)
        );
        assert!(element_text(&first, tags::STUDY_DATE) < element_text(&second, tags::STUDY_DATE));
    }

    #[test]
    fn different_patients_receive_different_linkage_values() {
        let one = test_object("PATIENT-1", "20200101", "1.2.3.1");
        let two = test_object("PATIENT-2", "20200101", "1.2.3.1");
        let (one, one_report) = anonymizer().anonymize(&one).unwrap();
        let (two, two_report) = anonymizer().anonymize(&two).unwrap();
        assert_ne!(one_report.patient_pseudonym, two_report.patient_pseudonym);
        assert_ne!(
            element_text(&one, tags::PATIENT_ID),
            element_text(&two, tags::PATIENT_ID)
        );
    }

    #[test]
    fn report_from_an_earlier_release_deserializes_with_default_counters() {
        let (_, report) = anonymizer()
            .anonymize(&test_object("patient", "20200101", "1.2.3"))
            .unwrap();
        let mut json = serde_json::to_value(&report).unwrap();
        let fields = json.as_object_mut().unwrap();
        for added in [
            "dummy_replacements",
            "unmapped_dummy_replacements",
            "removed_jpeg2000_comment_segments",
            "removed_jpeg_comment_segments",
            "removed_jpeg_application_segments",
        ] {
            fields.remove(added).unwrap();
        }
        let parsed: AnonymizationReport = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.removed_jpeg2000_comment_segments, 0);
        assert_eq!(parsed.removed_jpeg_comment_segments, 0);
        assert_eq!(parsed.removed_jpeg_application_segments, 0);
        assert!(parsed.dummy_replacements.is_empty());
    }

    #[test]
    fn output_is_deterministic_and_private_tags_are_removed() {
        let source = test_object("PATIENT-1", "20200101", "1.2.3.1");
        let (first, first_report) = anonymizer().anonymize(&source).unwrap();
        let (second, second_report) = anonymizer().anonymize(&source).unwrap();
        let mut first_bytes = Vec::new();
        let mut second_bytes = Vec::new();
        crate::write_dicom(&first, &mut first_bytes).unwrap();
        crate::write_dicom(&second, &mut second_bytes).unwrap();
        assert_eq!(first_bytes, second_bytes);
        assert_eq!(first_report, second_report);
        assert!(first.get(Tag(0x0011, 0x0010)).is_none());
        assert_eq!(
            element_text(&first, tags::PATIENT_IDENTITY_REMOVED).as_deref(),
            Some("YES")
        );
        let method_codes = first
            .get(tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE)
            .unwrap()
            .value()
            .items()
            .unwrap()
            .iter()
            .map(|item| element_text(item, tags::CODE_VALUE).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(method_codes, ["113100", "113105", "113107", "113108"]);
    }

    #[test]
    fn retain_safe_private_uses_registry_and_embedded_declarations() {
        let mut source = test_object("PATIENT-1", "20200101", "1.2.3.1");
        source.put_str(Tag(0x0011, 0x0010), VR::LO, "GEMS_ACQU_01");
        source.put_str(Tag(0x0011, 0x1023), VR::DS, "1.25");
        source.put_str(Tag(0x0011, 0x1025), VR::LO, "UNSAFE");
        source.put_str(Tag(0x0013, 0x0010), VR::LO, "DECLARED_SAFE");
        source.put_str(Tag(0x0013, 0x1010), VR::LO, "SAFE VALUE");

        let mut declaration = InMemDicomObject::new_empty();
        declaration.put(InMemElement::new(
            tags::PRIVATE_GROUP_REFERENCE,
            VR::US,
            PrimitiveValue::from(0x0013_u16),
        ));
        declaration.put_str(tags::PRIVATE_CREATOR_REFERENCE, VR::LO, "DECLARED_SAFE");
        declaration.put_str(tags::BLOCK_IDENTIFYING_INFORMATION_STATUS, VR::CS, "SAFE");
        source.put(InMemElement::new(
            tags::PRIVATE_DATA_ELEMENT_CHARACTERISTICS_SEQUENCE,
            VR::SQ,
            DataSetSequence::from(vec![declaration]),
        ));

        let mut options = Options::new(SecretKey::new(KEY).unwrap());
        options
            .profile
            .options
            .insert(DicomOption::RetainSafePrivate);
        let (output, _) = Anonymizer::new(options)
            .unwrap()
            .anonymize(&source)
            .unwrap();

        assert!(output.get(Tag(0x0011, 0x0010)).is_some());
        assert!(output.get(Tag(0x0011, 0x1023)).is_some());
        assert!(output.get(Tag(0x0011, 0x1025)).is_none());
        assert!(output.get(Tag(0x0013, 0x0010)).is_some());
        assert!(output.get(Tag(0x0013, 0x1010)).is_some());
    }

    #[test]
    fn missing_patient_identity_fails_closed_without_explicit_scope() {
        let mut source = test_object("", "20200101", "1.2.3.1");
        source.remove_element(tags::PATIENT_NAME);
        assert!(matches!(
            anonymizer().anonymize(&source),
            Err(Error::MissingPatientScope)
        ));
    }

    #[test]
    fn remaps_referenced_uids_and_removes_nested_private_attributes() {
        let mut source = test_object("PATIENT-1", "20200101", "1.2.3.1");
        let mut reference = InMemDicomObject::new_empty();
        reference.put_str(
            tags::REFERENCED_SOP_CLASS_UID,
            VR::UI,
            uids::SECONDARY_CAPTURE_IMAGE_STORAGE,
        );
        reference.put_str(tags::REFERENCED_SOP_INSTANCE_UID, VR::UI, "1.2.3.1.1");
        reference.put_str(Tag(0x0011, 0x1010), VR::LO, "NESTED PRIVATE");
        source.put(InMemElement::new(
            tags::SOURCE_IMAGE_SEQUENCE,
            VR::SQ,
            DataSetSequence::from(vec![reference]),
        ));

        let (output, _) = anonymizer().anonymize(&source).unwrap();
        let remapped_sop = element_text(&output, tags::SOP_INSTANCE_UID).unwrap();
        let item = output
            .get(tags::SOURCE_IMAGE_SEQUENCE)
            .unwrap()
            .value()
            .items()
            .unwrap()
            .first()
            .unwrap();
        assert_eq!(
            element_text(item, tags::REFERENCED_SOP_INSTANCE_UID).as_deref(),
            Some(remapped_sop.as_str())
        );
        assert!(item.get(Tag(0x0011, 0x1010)).is_none());
        let validation = crate::validate_object(&output);
        assert!(validation.conformant, "{:#?}", validation.findings);
    }

    #[test]
    fn dummy_sequences_replace_every_nested_value() {
        let mut source = test_object("PATIENT-1", "20200101", "1.2.3.1");
        let mut institution = InMemDicomObject::new_empty();
        institution.put_str(tags::CODE_VALUE, VR::SH, "HOSPITAL-123");
        institution.put_str(tags::CODING_SCHEME_DESIGNATOR, VR::SH, "LOCAL");
        institution.put_str(tags::CODE_MEANING, VR::LO, "Identifying Hospital");
        source.put(InMemElement::new(
            tags::INSTITUTION_CODE_SEQUENCE,
            VR::SQ,
            DataSetSequence::from(vec![institution]),
        ));

        let mut strict = Options::new(SecretKey::new(KEY).unwrap());
        strict
            .profile
            .fail_on_unmapped
            .insert("00080100".to_owned());
        let (output, report) = Anonymizer::new(strict).unwrap().anonymize(&source).unwrap();
        // A profile-mandated dummy sequence is not a descriptor-map miss.
        assert_eq!(report.dummy_replacements["00080082"], 1);
        assert_eq!(report.dummy_replacements["00080100"], 1);
        assert!(report.unmapped_dummy_replacements.is_empty());
        let item = output
            .get(tags::INSTITUTION_CODE_SEQUENCE)
            .unwrap()
            .value()
            .items()
            .unwrap()
            .first()
            .unwrap();
        for (tag, original) in [
            (tags::CODE_VALUE, "HOSPITAL-123"),
            (tags::CODING_SCHEME_DESIGNATOR, "LOCAL"),
            (tags::CODE_MEANING, "Identifying Hospital"),
        ] {
            let replacement = element_text(item, tag).unwrap();
            assert!(!replacement.is_empty());
            assert_ne!(replacement, original);
        }
        let validation = crate::validate_object(&output);
        assert!(validation.conformant, "{:#?}", validation.findings);
    }

    #[test]
    fn modifies_times_without_disrupting_paired_timestamp_intervals() {
        let mut first = test_object("PATIENT-1", "20200101", "1.2.3.1");
        first.put_str(tags::STUDY_TIME, VR::TM, "230000");
        first.put_str(tags::INTERVENTION_DRUG_START_TIME, VR::TM, "221500");
        let mut second = test_object("PATIENT-1", "20200102", "1.2.3.2");
        second.put_str(tags::STUDY_TIME, VR::TM, "010000");

        let source_interval = dataset_timestamp(&second) - dataset_timestamp(&first);
        let (first_output, _) = anonymizer().anonymize(&first).unwrap();
        let (second_output, _) = anonymizer().anonymize(&second).unwrap();

        assert_ne!(
            element_text(&first_output, tags::STUDY_TIME).as_deref(),
            Some("230000")
        );
        assert_ne!(
            element_text(&first_output, tags::INTERVENTION_DRUG_START_TIME).as_deref(),
            Some("221500")
        );
        assert_eq!(
            dataset_timestamp(&second_output) - dataset_timestamp(&first_output),
            source_interval
        );
    }

    #[test]
    fn rejects_mismatched_paired_date_and_time_multiplicities() {
        let mut source = test_object("PATIENT-1", "20200101", "1.2.3.1");
        source.put_str(tags::STUDY_DATE, VR::DA, "20200101\\20200102");
        source.put_str(tags::STUDY_TIME, VR::TM, "120000");
        assert!(matches!(
            anonymizer().anonymize(&source),
            Err(Error::MismatchedTemporalPair { .. })
        ));
    }

    fn dataset_timestamp(object: &InMemDicomObject) -> NaiveDateTime {
        let encoded = format!(
            "{}{}",
            element_text(object, tags::STUDY_DATE).unwrap(),
            element_text(object, tags::STUDY_TIME).unwrap()
        );
        NaiveDateTime::parse_from_str(&encoded, "%Y%m%d%H%M%S").unwrap()
    }

    #[test]
    fn validates_options_and_patient_scope_fallbacks() {
        let secret = SecretKey::new(KEY).unwrap();
        let mut invalid_range = Options::new(secret.clone());
        invalid_range.date_shift_range_days = 0;
        assert!(matches!(
            Anonymizer::new(invalid_range),
            Err(Error::InvalidDateShiftRange)
        ));

        let mut invalid_root = Options::new(secret.clone());
        invalid_root.uid_root = "2.invalid".to_owned();
        assert!(matches!(
            Anonymizer::new(invalid_root),
            Err(Error::InvalidUidRoot(_))
        ));

        let mut name_only = test_object("", "20200101", "1.2.3.1");
        name_only.remove_element(tags::PATIENT_ID);
        assert!(anonymizer().anonymize(&name_only).is_ok());

        let mut no_identity = name_only;
        no_identity.remove_element(tags::PATIENT_NAME);
        let mut explicit_options = Options::new(secret);
        explicit_options.patient_scope = Some("external-patient-1".to_owned());
        let explicit = Anonymizer::new(explicit_options).unwrap();
        assert!(explicit.anonymize(&no_identity).is_ok());
    }

    #[test]
    fn rejects_reanonymization_unless_explicitly_enabled() {
        let mut source = test_object("PATIENT-1", "20200101", "1.2.3.1");
        source.put_str(tags::PATIENT_IDENTITY_REMOVED, VR::CS, "YES");
        assert!(matches!(
            anonymizer().anonymize(&source),
            Err(Error::AlreadyDeidentified)
        ));

        let mut options = Options::new(SecretKey::new(KEY).unwrap());
        options.reject_already_deidentified = false;
        assert!(Anonymizer::new(options).unwrap().anonymize(&source).is_ok());
    }

    #[test]
    fn covers_profile_actions_and_extension_rules() {
        let mut source = test_object("PATIENT-1", "20200101", "1.2.3.1");
        source.put_str(tags::STUDY_DESCRIPTION, VR::LO, "remove me");
        source.put_str(tags::ACCESSION_NUMBER, VR::SH, "empty me");
        source.put_str(tags::STUDY_TIME, VR::TM, "123456");
        source.put_str(Tag(0x0004, 0x1500), VR::CS, "FILEID");
        source.put_str(Tag(0x7776, 0x0001), VR::LO, "safe extension");
        source.put_str(Tag(0x7776, 0x0002), VR::DA, "20200102");

        let (output, report) = anonymizer().anonymize(&source).unwrap();
        assert_eq!(
            element_text(&output, tags::STUDY_DESCRIPTION).as_deref(),
            None
        );
        assert_eq!(
            element_text(&output, tags::ACCESSION_NUMBER).as_deref(),
            Some("")
        );
        assert_ne!(
            element_text(&output, tags::STUDY_TIME).as_deref(),
            Some("123456")
        );
        assert!(output.get(Tag(0x0004, 0x1500)).is_none());
        assert_eq!(
            element_text(&output, Tag(0x7776, 0x0001)).as_deref(),
            Some("safe extension")
        );
        assert_ne!(
            element_text(&output, Tag(0x7776, 0x0002)).as_deref(),
            Some("20200102")
        );
        assert!(report.removed_attributes >= 1);
        assert!(report.emptied_attributes >= 1);
        assert!(report.shifted_temporal_attributes >= 3);
    }

    #[test]
    fn unknown_temporal_attributes_follow_the_selected_temporal_option() {
        let unknown_date = Tag(0x7776, 0x0002);
        let mut source = test_object("PATIENT-1", "20200101", "1.2.3.1");
        source.put_str(unknown_date, VR::DA, "20200102");

        let mut full_options = Options::new(SecretKey::new(KEY).unwrap());
        full_options
            .profile
            .options
            .remove(&DicomOption::RetainModifiedDates);
        full_options
            .profile
            .options
            .insert(DicomOption::RetainFullDates);
        let (full, _) = Anonymizer::new(full_options)
            .unwrap()
            .anonymize(&source)
            .unwrap();
        assert_eq!(
            element_text(&full, unknown_date).as_deref(),
            Some("20200102")
        );

        let mut removed_options = Options::new(SecretKey::new(KEY).unwrap());
        removed_options
            .profile
            .options
            .remove(&DicomOption::RetainModifiedDates);
        let (removed, _) = Anonymizer::new(removed_options)
            .unwrap()
            .anonymize(&source)
            .unwrap();
        assert!(removed.get(unknown_date).is_none());
    }

    #[test]
    fn default_profile_retains_and_caps_patient_age() {
        for (source_age, expected_age) in [
            ("089Y", "089Y"),
            ("090Y", "090Y"),
            ("091Y", "090Y"),
            ("999Y", "090Y"),
            ("018M", "018M"),
        ] {
            let mut source = test_object("PATIENT-1", "20200101", "1.2.3.1");
            source.put_str(tags::PATIENT_AGE, VR::AS, source_age);
            let (output, _) = anonymizer().anonymize(&source).unwrap();
            assert_eq!(
                element_text(&output, tags::PATIENT_AGE).as_deref(),
                Some(expected_age),
                "source age {source_age}"
            );
        }
    }

    #[test]
    fn clean_descriptors_uses_exact_mapping_and_safe_fallback() {
        let mut options = Options::new(SecretKey::new(KEY).unwrap());
        options
            .profile
            .descriptor_mappings
            .entry("00081030".to_owned())
            .or_default()
            .insert(
                "MAMMO SCRN BIL W/CAD TOMO".to_owned(),
                "MAMMO SCREENING BILATERAL".to_owned(),
            );
        let anonymizer = Anonymizer::new(options).unwrap();

        let mut source = test_object("PATIENT-1", "20200101", "1.2.3.1");
        source.put_str(tags::STUDY_DESCRIPTION, VR::LO, "MAMMO SCRN BIL W/CAD TOMO");
        source.put_str(tags::SERIES_DESCRIPTION, VR::LO, "Private protocol");
        let (output, _) = anonymizer.anonymize(&source).unwrap();

        assert_eq!(
            element_text(&output, tags::STUDY_DESCRIPTION).as_deref(),
            Some("MAMMO SCREENING BILATERAL")
        );
        assert!(output.get(tags::SERIES_DESCRIPTION).is_none());
    }

    #[test]
    fn safe_harbor_requires_external_linkage_and_generalizes_temporal_values() {
        let mut options = Options::new(SecretKey::new(KEY).unwrap());
        options.profile = ProfileConfiguration::safe_harbor();
        options.patient_scope = Some("independent-subject-001".to_owned());
        options.profile.uid_mappings = [
            ("1.2.3.1", "2.25.101"),
            ("1.2.3.1.1", "2.25.102"),
            ("1.2.3.1.2", "2.25.103"),
        ]
        .into_iter()
        .map(|(source, replacement)| (source.to_owned(), replacement.to_owned()))
        .collect();
        let anonymizer = Anonymizer::new(options).unwrap();

        let mut source = test_object("PATIENT-1", "20200615", "1.2.3.1");
        source.put_str(tags::STUDY_TIME, VR::TM, "121314");
        source.put_str(tags::PATIENT_BIRTH_DATE, VR::DA, "19100102");
        source.put_str(tags::PATIENT_AGE, VR::AS, "115Y");
        let (output, report) = anonymizer.anonymize(&source).unwrap();

        assert_eq!(element_text(&output, tags::PATIENT_ID).as_deref(), Some(""));
        assert_eq!(
            element_text(&output, tags::PATIENT_NAME).as_deref(),
            Some("")
        );
        assert_eq!(
            element_text(&output, tags::CLINICAL_TRIAL_SUBJECT_ID).as_deref(),
            Some(report.patient_pseudonym.as_str())
        );
        assert_eq!(
            element_text(&output, tags::PATIENT_BIRTH_DATE).as_deref(),
            Some("")
        );
        assert_eq!(
            element_text(&output, tags::PATIENT_AGE).as_deref(),
            Some("090Y")
        );
        assert_eq!(
            element_text(&output, tags::STUDY_DATE).as_deref(),
            Some("20200101")
        );
        assert_eq!(
            element_text(&output, tags::STUDY_TIME).as_deref(),
            Some("000000")
        );
        assert_eq!(report.generalized_temporal_attributes, 2);
    }

    #[test]
    fn safe_harbor_rejects_missing_and_colliding_uid_mappings() {
        let source = test_object("PATIENT-1", "20200615", "1.2.3.1");
        let mut no_scope = Options::new(SecretKey::new(KEY).unwrap());
        no_scope.profile = ProfileConfiguration::safe_harbor();
        assert!(matches!(
            Anonymizer::new(no_scope),
            Err(Error::SafeHarborPatientScopeRequired)
        ));

        let mut missing = Options::new(SecretKey::new(KEY).unwrap());
        missing.profile = ProfileConfiguration::safe_harbor();
        missing.patient_scope = Some("independent-subject-001".to_owned());
        assert!(matches!(
            Anonymizer::new(missing).unwrap().anonymize(&source),
            Err(Error::MissingSafeHarborUidMapping { .. })
        ));

        let mut colliding = Options::new(SecretKey::new(KEY).unwrap());
        colliding.profile = ProfileConfiguration::safe_harbor();
        colliding.patient_scope = Some("independent-subject-001".to_owned());
        colliding.profile.uid_mappings = [
            ("1.2.3.1".to_owned(), "2.25.101".to_owned()),
            ("1.2.3.1.1".to_owned(), "2.25.101".to_owned()),
        ]
        .into_iter()
        .collect();
        assert!(matches!(
            Anonymizer::new(colliding),
            Err(Error::InvalidSafeHarborUidMapping(_))
        ));
    }

    #[test]
    fn creates_vr_compatible_nonempty_dummy_values() {
        let anonymizer = anonymizer();
        let patient_pseudonym = "ANON0123456789abcdef01234567";
        let uids = RefCell::new(UidResolver::mapped());
        let context = Context {
            patient_pseudonym,
            date_shift_days: 1,
            time_shift_hours: 1,
            path: String::new(),
            uids: &uids,
        };
        let mut report = AnonymizationReport::new(
            patient_pseudonym.to_owned(),
            1,
            1,
            &ProfileConfiguration::default(),
        );
        let value_representations = [
            VR::AE,
            VR::CS,
            VR::LO,
            VR::LT,
            VR::SH,
            VR::ST,
            VR::UC,
            VR::UR,
            VR::UT,
            VR::PN,
            VR::DA,
            VR::DT,
            VR::TM,
            VR::AS,
            VR::DS,
            VR::IS,
            VR::UI,
            VR::US,
            VR::SS,
            VR::UL,
            VR::SL,
            VR::UV,
            VR::SV,
            VR::FL,
            VR::FD,
            VR::AT,
            VR::OB,
            VR::OD,
            VR::OF,
            VR::OL,
            VR::OV,
            VR::OW,
            VR::UN,
        ];
        let mut dummy_object = InMemDicomObject::new_empty();
        for (index, vr) in value_representations.into_iter().enumerate() {
            let element = InMemElement::empty(Tag(0x7776, index as u16), vr);
            let dummy = anonymizer
                .dummy_element(&element, &context, &mut report)
                .unwrap();
            assert_eq!(dummy.vr(), vr);
            assert!(dummy.value().multiplicity() > 0, "{vr:?}");
            dummy_object.put(dummy);
        }
        dummy_object.put_str(
            tags::SOP_CLASS_UID,
            VR::UI,
            uids::SECONDARY_CAPTURE_IMAGE_STORAGE,
        );
        dummy_object.put_str(tags::SOP_INSTANCE_UID, VR::UI, "1.2.3.4.5");
        let dummy_file = dummy_object
            .with_meta(FileMetaTableBuilder::new().transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN))
            .unwrap();
        let mut encoded = Vec::new();
        dummy_file.write_all(&mut encoded).unwrap();
        let decoded = crate::read_dicom(&encoded[..]).unwrap();
        for (index, vr) in value_representations.into_iter().enumerate() {
            assert_eq!(decoded.get(Tag(0x7776, index as u16)).unwrap().vr(), vr);
        }

        let mut item = InMemDicomObject::new_empty();
        item.put_str(tags::PATIENT_NAME, VR::PN, "Private^Person");
        let sequence = InMemElement::new(
            tags::CONTENT_SEQUENCE,
            VR::SQ,
            DataSetSequence::from(vec![item]),
        );
        let cleaned = anonymizer
            .dummy_element(&sequence, &context, &mut report)
            .unwrap();
        assert_eq!(cleaned.value().items().unwrap().len(), 1);

        let clean_text = InMemElement::new(Tag(0x7776, 0x1000), VR::LO, "free text");
        let cleaned = anonymizer
            .process_element(&clean_text, ProfileAction::Clean, &context, &mut report)
            .unwrap()
            .unwrap();
        assert_eq!(cleaned.to_str().unwrap(), "ANONYMIZED");
    }
}
