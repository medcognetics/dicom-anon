//! Core de-identification engine.

use dicom_core::header::Header;
use dicom_core::value::{DataSetSequence, Value};
use dicom_core::{DataElement, PrimitiveValue, Tag, VR};
use dicom_dictionary_std::tags;
use dicom_object::mem::{InMemDicomObject, InMemElement};
use dicom_object::{DefaultDicomObject, FileMetaTableBuilder};
use serde::{Deserialize, Serialize};

use crate::profile::{profile_rule, ProfileAction, PROFILE_VERSION};
use crate::temporal::{
    shift_date_values, shift_date_values_with_adjustments, shift_datetime_values, shift_time_values,
};
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
        }
    }
}

/// Counts and stable patient-level outputs from one de-identification pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnonymizationReport {
    /// DICOM edition used by the rule table.
    pub profile_version: String,
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
}

impl AnonymizationReport {
    fn new(patient_pseudonym: String, date_shift_days: i32, time_shift_hours: i8) -> Self {
        Self {
            profile_version: PROFILE_VERSION.to_owned(),
            patient_pseudonym,
            date_shift_days,
            time_shift_hours,
            removed_attributes: 0,
            removed_private_attributes: 0,
            emptied_attributes: 0,
            replaced_attributes: 0,
            remapped_uid_attributes: 0,
            shifted_temporal_attributes: 0,
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
        Ok(Self { options })
    }

    /// De-identify a parsed DICOM object without modifying the source.
    pub fn anonymize(
        &self,
        source: &DefaultDicomObject,
    ) -> Result<(DefaultDicomObject, AnonymizationReport)> {
        if self.options.reject_already_deidentified
            && element_text(source, tags::PATIENT_IDENTITY_REMOVED)
                .is_some_and(|value| value.eq_ignore_ascii_case("YES"))
        {
            return Err(Error::AlreadyDeidentified);
        }

        let patient_scope = self.patient_scope(source)?;
        let patient_pseudonym = self.options.secret.patient_id(&patient_scope);
        let (date_shift_days, time_shift_hours) = self
            .options
            .secret
            .temporal_shifts(&patient_scope, self.options.date_shift_range_days);
        let mut report =
            AnonymizationReport::new(patient_pseudonym.clone(), date_shift_days, time_shift_hours);
        let context = Context {
            patient_pseudonym: &patient_pseudonym,
            date_shift_days,
            time_shift_hours: i32::from(time_shift_hours),
        };

        let mut output = self.process_dataset(source, &context, &mut report)?;
        add_deidentification_markers(&mut output);
        let meta = FileMetaTableBuilder::new()
            .transfer_syntax(source.meta().transfer_syntax().trim_end_matches('\0'))
            .implementation_class_uid(IMPLEMENTATION_CLASS_UID)
            .implementation_version_name(IMPLEMENTATION_VERSION_NAME);
        let output = output
            .with_meta(meta)
            .map_err(|error| Error::Write(error.to_string()))?;
        Ok((output, report))
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

    fn process_dataset(
        &self,
        source: &InMemDicomObject,
        context: &Context<'_>,
        report: &mut AnonymizationReport,
    ) -> Result<InMemDicomObject> {
        let mut output = InMemDicomObject::new_empty();
        let mut date_adjustments = Vec::new();
        for element in source.iter() {
            let tag = element.tag();
            if is_private(tag) {
                report.removed_private_attributes += 1;
                continue;
            }
            if tag.0 == 0x0004 {
                report.removed_attributes += 1;
                continue;
            }

            let action = profile_rule(tag)
                .map(|rule| rule.action)
                .or_else(|| {
                    matches!(element.vr(), VR::DA | VR::DT | VR::TM).then_some(ProfileAction::Clean)
                })
                .unwrap_or(ProfileAction::Keep);
            if action == ProfileAction::Clean && element.vr() == VR::TM {
                if let Some(adjustment) =
                    paired_day_adjustment(source, element, context.time_shift_hours)?
                {
                    date_adjustments.push(adjustment);
                }
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
                value: "<non-text value>".to_owned(),
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
            report.replaced_attributes += 1;
            return Ok(Some(InMemElement::new(
                tag,
                element.vr(),
                context.patient_pseudonym,
            )));
        }
        if tag == tags::PATIENT_NAME {
            report.replaced_attributes += 1;
            let name = format!("ANON^{}", &context.patient_pseudonym[4..16]);
            return Ok(Some(InMemElement::new(tag, element.vr(), name)));
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
                    .map(|uid| self.options.secret.uid(&self.options.uid_root, uid))
                    .collect::<Vec<_>>()
                    .join("\\");
                Ok(Some(InMemElement::new(tag, VR::UI, remapped)))
            }
            ProfileAction::Keep => self
                .clean_sequence_or_clone(element, context, report)
                .map(Some),
            ProfileAction::Clean => {
                if element.vr() == VR::SQ {
                    return self
                        .clean_sequence_or_clone(element, context, report)
                        .map(Some);
                }
                if element.vr() == VR::DA {
                    let original = element.to_str().map_err(|_| Error::InvalidTemporal {
                        tag: format_tag(tag),
                        vr: vr_name(element.vr()),
                        value: "<non-text value>".to_owned(),
                    })?;
                    let shifted = shift_date_values(&original, context.date_shift_days, tag, "DA")?;
                    report.shifted_temporal_attributes += 1;
                    return Ok(Some(InMemElement::new(tag, element.vr(), shifted)));
                }
                if element.vr() == VR::DT {
                    let original = element.to_str().map_err(|_| Error::InvalidTemporal {
                        tag: format_tag(tag),
                        vr: "DT",
                        value: "<non-text value>".to_owned(),
                    })?;
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
                        value: "<non-text value>".to_owned(),
                    })?;
                    let (shifted, _) = shift_time_values(&value, context.time_shift_hours, tag)?;
                    report.shifted_temporal_attributes += 1;
                    return Ok(Some(InMemElement::new(tag, VR::TM, shifted)));
                }
                report.replaced_attributes += 1;
                self.dummy_element(element, context, report).map(Some)
            }
        }
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
            .map(|item| self.process_dataset(item, context, report))
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
        let mut items = sequence
            .items()
            .iter()
            .map(|item| self.dummy_dataset(item, context, report))
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
}

struct Context<'a> {
    patient_pseudonym: &'a str,
    date_shift_days: i32,
    time_shift_hours: i32,
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
        value: "<non-text value>".to_owned(),
    })?;
    let date_value = date_element.to_str().map_err(|_| Error::InvalidTemporal {
        tag: format_tag(date_tag),
        vr: "DA",
        value: "<non-text value>".to_owned(),
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

fn add_deidentification_markers(output: &mut InMemDicomObject) {
    output.put_str(tags::PATIENT_IDENTITY_REMOVED, VR::CS, "YES");
    output.put_str(
        tags::DEIDENTIFICATION_METHOD,
        VR::LO,
        format!("DICOM PS3.15 Basic Profile\\Modified Dates Option\\dicom-anon {PROFILE_VERSION}"),
    );
    output.put_str(
        tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED,
        VR::CS,
        "MODIFIED",
    );

    let codes: Vec<_> = [
        ("113100", "Basic Application Confidentiality Profile"),
        (
            "113107",
            "Retain Longitudinal Temporal Information Modified Dates Option",
        ),
    ]
    .into_iter()
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

        let (output, _) = anonymizer().anonymize(&source).unwrap();
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
        assert!(output.get(tags::STUDY_DESCRIPTION).is_none());
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
        assert!(report.removed_attributes >= 2);
        assert!(report.emptied_attributes >= 1);
        assert!(report.shifted_temporal_attributes >= 3);
    }

    #[test]
    fn creates_vr_compatible_nonempty_dummy_values() {
        let anonymizer = anonymizer();
        let patient_pseudonym = "ANON0123456789abcdef01234567";
        let context = Context {
            patient_pseudonym,
            date_shift_days: 1,
            time_shift_hours: 1,
        };
        let mut report = AnonymizationReport::new(patient_pseudonym.to_owned(), 1, 1);
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
