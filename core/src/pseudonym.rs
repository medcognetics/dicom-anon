//! Keyed deterministic pseudonyms.

use std::fmt;

use zeroize::Zeroize;

use crate::{Error, Result};

const MINIMUM_KEY_LENGTH: usize = 32;
const PATIENT_ID_PREFIX: &str = "ANON";
const PATIENT_ID_HEX_LENGTH: usize = 24;
const TIME_SHIFT_BOUND_HOURS: i8 = 12;

/// Secret key used for deterministic, domain-separated transformations.
///
/// The constructor hashes input key material to a fixed 256-bit BLAKE3 key.
/// Debug output never includes the key, and memory is cleared on drop.
#[derive(Clone)]
pub struct SecretKey([u8; 32]);

impl SecretKey {
    /// Construct a key from at least 32 bytes of secret input.
    pub fn new(key_material: impl AsRef<[u8]>) -> Result<Self> {
        let key_material = key_material.as_ref();
        if key_material.len() < MINIMUM_KEY_LENGTH {
            return Err(Error::KeyTooShort);
        }
        Ok(Self(*blake3::hash(key_material).as_bytes()))
    }

    pub(crate) fn digest(&self, domain: &str, value: &[u8]) -> blake3::Hash {
        let mut hasher = blake3::Hasher::new_keyed(&self.0);
        hasher.update(domain.as_bytes());
        hasher.update(&[0]);
        hasher.update(value);
        hasher.finalize()
    }

    pub(crate) fn patient_id(&self, patient_scope: &str) -> String {
        let digest = self.digest("dicom-anon/patient-id/v1", patient_scope.as_bytes());
        let encoded = digest.to_hex();
        format!("{PATIENT_ID_PREFIX}{}", &encoded[..PATIENT_ID_HEX_LENGTH])
    }

    pub(crate) fn uid(&self, uid_root: &str, original: &str) -> String {
        let digest = self.digest(
            "dicom-anon/uid/v1",
            original.trim_end_matches('\0').as_bytes(),
        );
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&digest.as_bytes()[..16]);
        format!("{uid_root}.{}", u128::from_be_bytes(bytes))
    }

    pub(crate) fn temporal_shifts(&self, patient_scope: &str, range_days: i32) -> (i32, i8) {
        let digest = self.digest("dicom-anon/temporal-shift/v1", patient_scope.as_bytes());
        let sample = u64::from_le_bytes(digest.as_bytes()[..8].try_into().expect("fixed slice"));
        let width = i64::from(range_days) * 2 + 1;
        let shift = i64::try_from(sample % u64::try_from(width).expect("positive range"))
            .expect("bounded sample")
            - i64::from(range_days);
        let day_shift = if shift == 0 { 1 } else { shift as i32 };
        let bucket = (digest.as_bytes()[8] % (TIME_SHIFT_BOUND_HOURS as u8 * 2)) as i8;
        let shift = bucket - TIME_SHIFT_BOUND_HOURS;
        let time_shift = if shift == 0 {
            TIME_SHIFT_BOUND_HOURS
        } else {
            shift
        };
        (day_shift, time_shift)
    }
}

pub(crate) fn is_patient_id(value: &str) -> bool {
    value.len() == PATIENT_ID_PREFIX.len() + PATIENT_ID_HEX_LENGTH
        && value.starts_with(PATIENT_ID_PREFIX)
        && value[PATIENT_ID_PREFIX.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretKey([REDACTED])")
    }
}

impl Drop for SecretKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7; 32];

    #[test]
    fn key_requires_adequate_length() {
        assert!(matches!(SecretKey::new([0; 31]), Err(Error::KeyTooShort)));
    }

    #[test]
    fn transformations_are_deterministic_and_domain_separated() {
        let key = SecretKey::new(KEY).unwrap();
        assert_eq!(key.patient_id("p1"), key.patient_id("p1"));
        assert_ne!(key.patient_id("p1"), key.patient_id("p2"));
        assert_ne!(key.digest("one", b"same"), key.digest("two", b"same"));
        assert_eq!(
            key.temporal_shifts("p1", 100),
            key.temporal_shifts("p1", 100)
        );
        assert_ne!(key.temporal_shifts("p1", 100).0, 0);
        assert_ne!(key.temporal_shifts("p1", 100).1, 0);
    }

    #[test]
    fn generated_uids_are_valid_length() {
        let key = SecretKey::new(KEY).unwrap();
        let uid = key.uid("2.25", "1.2.840.10008.1");
        assert!(uid.len() <= 64);
        assert!(uid
            .split('.')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit())));
    }

    #[test]
    fn debug_output_is_redacted_and_keys_can_be_dropped() {
        let key = SecretKey::new(KEY).unwrap();
        assert_eq!(format!("{key:?}"), "SecretKey([REDACTED])");
        drop(key);
    }
}
