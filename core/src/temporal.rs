//! DICOM temporal transformations.

use chrono::{Datelike, Days, NaiveDate};
use dicom_core::Tag;

use crate::{Error, Result};

pub(crate) fn shift_date_values(
    value: &str,
    days: i32,
    tag: Tag,
    vr: &'static str,
) -> Result<String> {
    value
        .split('\\')
        .map(|component| shift_component(component, days, tag, vr))
        .collect::<Result<Vec<_>>>()
        .map(|values| values.join("\\"))
}

pub(crate) fn shift_date_values_with_adjustments(
    value: &str,
    days: i32,
    day_adjustments: &[i32],
    tag: Tag,
) -> Result<String> {
    let components = value.split('\\').collect::<Vec<_>>();
    if components.len() != day_adjustments.len() {
        return Err(invalid_temporal(tag, "DA", value));
    }
    components
        .into_iter()
        .zip(day_adjustments)
        .map(|(component, adjustment)| {
            let adjusted_days = days
                .checked_add(*adjustment)
                .ok_or_else(|| invalid_temporal(tag, "DA", value))?;
            shift_component(component, adjusted_days, tag, "DA")
        })
        .collect::<Result<Vec<_>>>()
        .map(|values| values.join("\\"))
}

pub(crate) fn shift_datetime_values(
    value: &str,
    days: i32,
    hours: i32,
    tag: Tag,
) -> Result<String> {
    value
        .split('\\')
        .map(|component| shift_datetime_component(component, days, hours, tag))
        .collect::<Result<Vec<_>>>()
        .map(|values| values.join("\\"))
}

pub(crate) fn shift_time_values(value: &str, hours: i32, tag: Tag) -> Result<(String, Vec<i32>)> {
    validate_time_values(value, tag)?;
    let shifted = value
        .split('\\')
        .map(|component| {
            let hour = component[..2]
                .parse::<i32>()
                .map_err(|_| invalid_temporal(tag, "TM", value))?;
            let shifted_hour = hour + hours;
            let day_adjustment = shifted_hour.div_euclid(24);
            Ok((
                format!("{:02}{}", shifted_hour.rem_euclid(24), &component[2..]),
                day_adjustment,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let values = shifted
        .iter()
        .map(|(value, _)| value.as_str())
        .collect::<Vec<_>>()
        .join("\\");
    let adjustments = shifted
        .into_iter()
        .map(|(_, adjustment)| adjustment)
        .collect();
    Ok((values, adjustments))
}

pub(crate) fn validate_time_values(value: &str, tag: Tag) -> Result<()> {
    if value.split('\\').all(valid_time_encoding) {
        Ok(())
    } else {
        Err(invalid_temporal(tag, "TM", value))
    }
}

fn shift_component(value: &str, days: i32, tag: Tag, vr: &'static str) -> Result<String> {
    let digits = value.bytes().take_while(u8::is_ascii_digit).count();
    if vr == "DA" && (digits != 8 || value.len() != 8) {
        return Err(invalid_temporal(tag, vr, value));
    }
    if vr == "DT" && !valid_datetime_encoding(value, digits) {
        return Err(invalid_temporal(tag, vr, value));
    }
    if vr == "DT" && digits == 4 {
        return shift_year_precision(value, days, tag, vr);
    }
    if vr == "DT" && digits == 6 {
        return shift_month_precision(value, days, tag, vr);
    }
    if digits < 8 || !value.as_bytes()[..8].iter().all(u8::is_ascii_digit) {
        return Err(invalid_temporal(tag, vr, value));
    }
    let date = NaiveDate::parse_from_str(&value[..8], "%Y%m%d")
        .map_err(|_| invalid_temporal(tag, vr, value))?;
    let shifted = if days >= 0 {
        date.checked_add_days(Days::new(days.unsigned_abs().into()))
    } else {
        date.checked_sub_days(Days::new(days.unsigned_abs().into()))
    }
    .ok_or_else(|| invalid_temporal(tag, vr, value))?;
    if !(1..=9_999).contains(&shifted.year()) {
        return Err(invalid_temporal(tag, vr, value));
    }
    Ok(format!("{}{}", shifted.format("%Y%m%d"), &value[8..]))
}

fn shift_datetime_component(value: &str, days: i32, hours: i32, tag: Tag) -> Result<String> {
    let digits = value.bytes().take_while(u8::is_ascii_digit).count();
    if !valid_datetime_encoding(value, digits) {
        return Err(invalid_temporal(tag, "DT", value));
    }
    if digits < 10 {
        return shift_component(value, days, tag, "DT");
    }
    let hour = value[8..10]
        .parse::<i32>()
        .map_err(|_| invalid_temporal(tag, "DT", value))?;
    let shifted_hour = hour + hours;
    let adjusted_days = days
        .checked_add(shifted_hour.div_euclid(24))
        .ok_or_else(|| invalid_temporal(tag, "DT", value))?;
    let shifted_date = shift_component(value, adjusted_days, tag, "DT")?;
    Ok(format!(
        "{}{:02}{}",
        &shifted_date[..8],
        shifted_hour.rem_euclid(24),
        &value[10..]
    ))
}

fn shift_year_precision(value: &str, days: i32, tag: Tag, vr: &'static str) -> Result<String> {
    let year = value[..4]
        .parse::<i32>()
        .map_err(|_| invalid_temporal(tag, vr, value))?;
    let shifted = year
        .checked_add(nonzero_coarse_offset(days, 365))
        .filter(|year| (1..=9_999).contains(year))
        .ok_or_else(|| invalid_temporal(tag, vr, value))?;
    Ok(format!("{shifted:04}{}", &value[4..]))
}

fn shift_month_precision(value: &str, days: i32, tag: Tag, vr: &'static str) -> Result<String> {
    let year = value[..4]
        .parse::<i32>()
        .map_err(|_| invalid_temporal(tag, vr, value))?;
    let month = value[4..6]
        .parse::<i32>()
        .map_err(|_| invalid_temporal(tag, vr, value))?;
    if !(1..=12).contains(&month) {
        return Err(invalid_temporal(tag, vr, value));
    }
    let total_months = year
        .checked_mul(12)
        .and_then(|months| months.checked_add(month - 1))
        .and_then(|months| months.checked_add(nonzero_coarse_offset(days, 30)))
        .ok_or_else(|| invalid_temporal(tag, vr, value))?;
    let shifted_year = total_months.div_euclid(12);
    let shifted_month = total_months.rem_euclid(12) + 1;
    if !(1..=9_999).contains(&shifted_year) {
        return Err(invalid_temporal(tag, vr, value));
    }
    Ok(format!(
        "{shifted_year:04}{shifted_month:02}{}",
        &value[6..]
    ))
}

fn nonzero_coarse_offset(days: i32, unit_days: u32) -> i32 {
    let magnitude = days.unsigned_abs().div_ceil(unit_days) as i32;
    if days.is_negative() {
        -magnitude
    } else {
        magnitude
    }
}

fn valid_datetime_encoding(value: &str, digits: usize) -> bool {
    if !matches!(digits, 4 | 6 | 8 | 10 | 12 | 14) {
        return false;
    }
    if digits >= 10 && !numeric_range(&value[8..10], 0, 23) {
        return false;
    }
    if digits >= 12 && !numeric_range(&value[10..12], 0, 59) {
        return false;
    }
    if digits == 14 && !numeric_range(&value[12..14], 0, 60) {
        return false;
    }

    let suffix = &value[digits..];
    if suffix.is_empty() {
        return true;
    }
    if let Some(fractional) = suffix.strip_prefix('.') {
        if digits != 14 {
            return false;
        }
        let timezone_start = fractional.find(['+', '-']);
        let (fraction, timezone) = timezone_start.map_or((fractional, ""), |index| {
            (&fractional[..index], &fractional[index..])
        });
        return (1..=6).contains(&fraction.len())
            && fraction.bytes().all(|byte| byte.is_ascii_digit())
            && (timezone.is_empty() || valid_timezone(timezone));
    }
    valid_timezone(suffix)
}

fn valid_time_encoding(value: &str) -> bool {
    let digits = value.bytes().take_while(u8::is_ascii_digit).count();
    if !matches!(digits, 2 | 4 | 6) {
        return false;
    }
    if !numeric_range(&value[..2], 0, 23)
        || (digits >= 4 && !numeric_range(&value[2..4], 0, 59))
        || (digits == 6 && !numeric_range(&value[4..6], 0, 60))
    {
        return false;
    }
    let suffix = &value[digits..];
    if suffix.is_empty() {
        return true;
    }
    let Some(fraction) = suffix.strip_prefix('.') else {
        return false;
    };
    digits == 6
        && (1..=6).contains(&fraction.len())
        && fraction.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_timezone(value: &str) -> bool {
    if value.len() != 5 || !matches!(value.as_bytes()[0], b'+' | b'-') {
        return false;
    }
    let hours = value[1..3].parse::<u8>();
    let minutes = value[3..5].parse::<u8>();
    matches!((hours, minutes), (Ok(0..=13), Ok(0..=59)) | (Ok(14), Ok(0)))
}

fn numeric_range(value: &str, minimum: u8, maximum: u8) -> bool {
    value
        .parse::<u8>()
        .is_ok_and(|value| (minimum..=maximum).contains(&value))
}

fn invalid_temporal(tag: Tag, vr: &'static str, _value: &str) -> Error {
    Error::InvalidTemporal {
        tag: format!("({:04X},{:04X})", tag.0, tag.1),
        vr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STUDY_DATE: Tag = Tag(0x0008, 0x0020);

    #[test]
    fn shifts_dates_without_losing_datetime_precision_or_zone() {
        assert_eq!(
            shift_date_values("20240228", 2, STUDY_DATE, "DA").unwrap(),
            "20240301"
        );
        assert_eq!(
            shift_date_values("20240228235959.12-0600", 2, STUDY_DATE, "DT").unwrap(),
            "20240301235959.12-0600"
        );
    }

    #[test]
    fn shifts_times_and_reports_midnight_adjustments() {
        assert_eq!(
            shift_time_values("230000\\0130", 2, STUDY_DATE).unwrap(),
            ("010000\\0330".to_owned(), vec![1, 0])
        );
        assert_eq!(
            shift_time_values("01\\235959.25", -2, STUDY_DATE).unwrap(),
            ("23\\215959.25".to_owned(), vec![-1, 0])
        );
    }

    #[test]
    fn shifts_datetimes_with_the_same_time_offset() {
        assert_eq!(
            shift_datetime_values("20240131230000.5-0600", 2, 2, STUDY_DATE).unwrap(),
            "20240203010000.5-0600"
        );
        assert_eq!(
            shift_date_values_with_adjustments("20240131", 2, &[1], STUDY_DATE).unwrap(),
            "20240203"
        );
    }

    #[test]
    fn shifts_partial_datetimes_at_their_encoded_precision() {
        assert_eq!(
            shift_date_values("2024", 370, STUDY_DATE, "DT").unwrap(),
            "2026"
        );
        assert_eq!(
            shift_date_values("202401+0600", 31, STUDY_DATE, "DT").unwrap(),
            "202403+0600"
        );
        assert_eq!(
            shift_date_values("202401", -31, STUDY_DATE, "DT").unwrap(),
            "202311"
        );
    }

    #[test]
    fn preserves_multi_value_structure() {
        assert_eq!(
            shift_date_values("20200101\\20200103", -1, STUDY_DATE, "DA").unwrap(),
            "20191231\\20200102"
        );
    }

    #[test]
    fn rejects_invalid_dates_instead_of_leaking_them() {
        assert!(matches!(
            shift_date_values("20240230", 1, STUDY_DATE, "DA"),
            Err(Error::InvalidTemporal { .. })
        ));
        assert!(matches!(
            shift_date_values("00010101", -1, STUDY_DATE, "DA"),
            Err(Error::InvalidTemporal { .. })
        ));
        for invalid in [
            "202401011",
            "20240101246000",
            "20240101120000.1234567",
            "20240101120000+1460",
        ] {
            assert!(matches!(
                shift_date_values(invalid, 1, STUDY_DATE, "DT"),
                Err(Error::InvalidTemporal { .. })
            ));
        }
        assert!(matches!(
            shift_date_values("20240101+0600", 1, STUDY_DATE, "DA"),
            Err(Error::InvalidTemporal { .. })
        ));
        assert!(validate_time_values("12\\235959.123456", STUDY_DATE).is_ok());
        assert!(matches!(
            validate_time_values("246000", STUDY_DATE),
            Err(Error::InvalidTemporal { .. })
        ));
    }
}
