//! Shared implementation details for the bundled commands.

use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::{Args, ValueEnum};

use crate::{DicomOption, ProfileConfiguration, SafePrivateElement, SecretKey};

/// Named profile presets exposed by both bundled commands.
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ProfilePreset {
    /// DICOM Basic Profile with no options enabled.
    Dicom,
    /// DICOM Basic Profile with clean-descriptors, retain-modified-dates, and retain-patient-characteristics.
    #[default]
    DicomExtended,
    /// Safe Harbor metadata preset with clean-graphics, clean-structured-content, clean-descriptors, retain-modified-dates, and retain-patient-characteristics.
    SafeHarborMetadata,
}

/// Shared confidentiality-profile command-line options.
#[derive(Debug, Clone, Args)]
pub struct ProfileArgs {
    /// Profile preset. dicom-extended is the default.
    #[arg(long, value_enum, default_value_t)]
    pub policy: ProfilePreset,
    /// Enable one DICOM confidentiality option; repeat as needed.
    #[arg(long, value_enum)]
    pub enable_option: Vec<DicomOption>,
    /// Disable one option enabled by the selected preset; repeat as needed.
    #[arg(long, value_enum)]
    pub disable_option: Vec<DicomOption>,
    /// JSON exact descriptor source-to-safe-output mappings; repeat to merge maps.
    #[arg(long, value_name = "PATH")]
    pub descriptor_map: Vec<PathBuf>,
    /// Fail when an attribute with this tag (eight hexadecimal digits) would receive a dummy value; repeat as needed.
    #[arg(long, value_name = "TAG", value_parser = parse_tag)]
    pub fail_on_unmapped: Vec<String>,
    /// JSON safe-private creator and element-offset allowlist.
    #[arg(long, value_name = "PATH")]
    pub safe_private_allowlist: Option<PathBuf>,
    /// JSON original-to-output UID map required by Safe Harbor mode.
    #[arg(long, value_name = "PATH")]
    pub uid_map: Option<PathBuf>,
}

impl ProfileArgs {
    pub fn load(&self) -> Result<ProfileConfiguration, String> {
        let mut profile = match self.policy {
            ProfilePreset::Dicom => ProfileConfiguration::dicom(),
            ProfilePreset::DicomExtended => ProfileConfiguration::dicom_extended(),
            ProfilePreset::SafeHarborMetadata => ProfileConfiguration::safe_harbor(),
        };
        for option in &self.enable_option {
            if self.disable_option.contains(option) {
                return Err(format!(
                    "option {option:?} cannot be both enabled and disabled"
                ));
            }
            profile.options.insert(*option);
        }
        for option in &self.disable_option {
            profile.options.remove(option);
        }
        for path in &self.descriptor_map {
            let document: DescriptorMapDocument = read_json(path, "descriptor map")?;
            merge_descriptor_mappings(
                &mut profile.descriptor_mappings,
                document.into_mappings(),
                path,
            )?;
        }
        profile
            .fail_on_unmapped
            .extend(self.fail_on_unmapped.iter().cloned());
        if let Some(path) = &self.safe_private_allowlist {
            let values: Vec<SafePrivateElement> = read_json(path, "safe-private allowlist")?;
            profile.safe_private_allowlist = values.into_iter().collect();
        }
        if let Some(path) = &self.uid_map {
            profile.uid_mappings = read_json(path, "UID map")?;
        }
        profile.validate().map_err(|error| error.to_string())?;
        Ok(profile)
    }
}

/// Descriptor-map file: a bare tag map, or a versioned document with provenance.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum DescriptorMapDocument {
    Versioned(VersionedDescriptorMap),
    Bare(BTreeMap<String, BTreeMap<String, String>>),
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionedDescriptorMap {
    #[serde(rename = "format")]
    _format: VersionedDescriptorMapFormat,
    #[serde(rename = "name")]
    _name: String,
    #[serde(rename = "version")]
    _version: String,
    #[serde(rename = "source")]
    _source: String,
    #[serde(rename = "notes", default)]
    _notes: Vec<String>,
    mappings: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(serde::Deserialize)]
enum VersionedDescriptorMapFormat {
    #[serde(rename = "dicom-anon-descriptor-map/1")]
    V1,
}

impl DescriptorMapDocument {
    fn into_mappings(self) -> BTreeMap<String, BTreeMap<String, String>> {
        match self {
            Self::Versioned(document) => document.mappings,
            Self::Bare(mappings) => mappings,
        }
    }
}

fn merge_descriptor_mappings(
    target: &mut BTreeMap<String, BTreeMap<String, String>>,
    source: BTreeMap<String, BTreeMap<String, String>>,
    path: &std::path::Path,
) -> Result<(), String> {
    for (tag, values) in source {
        let entries = target.entry(tag.clone()).or_default();
        for (input, output) in values {
            match entries.get(&input) {
                Some(existing) if *existing != output => {
                    return Err(format!(
                        "descriptor map {} conflicts with an earlier map for tag {tag}",
                        path.display()
                    ));
                }
                Some(_) => {}
                None => {
                    entries.insert(input, output);
                }
            }
        }
    }
    Ok(())
}

fn parse_tag(value: &str) -> Result<String, String> {
    let tag = value.to_ascii_uppercase();
    if tag.len() == 8 && tag.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(tag)
    } else {
        Err("expected eight hexadecimal digits, for example 00080100".to_owned())
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &PathBuf, name: &str) -> Result<T, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("cannot read {name} {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("cannot parse {name} {}: {error}", path.display()))
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
}

pub struct Styles {
    enabled: bool,
}

impl Styles {
    #[must_use]
    pub fn new(
        mode: ColorMode,
        no_color: bool,
        structured: bool,
        output_is_terminal: bool,
    ) -> Self {
        let enabled = !no_color
            && !structured
            && match mode {
                ColorMode::Auto => output_is_terminal,
                ColorMode::Always => true,
                ColorMode::Never => false,
            };
        Self { enabled }
    }

    #[must_use]
    pub fn success(&self, value: &str) -> String {
        self.paint("1;32", value)
    }

    #[must_use]
    pub fn warning(&self, value: &str) -> String {
        self.paint("1;33", value)
    }

    #[must_use]
    pub fn failure(&self, value: &str) -> String {
        self.paint("1;31", value)
    }

    #[must_use]
    pub fn heading(&self, value: &str) -> String {
        self.paint("1;36", value)
    }

    fn paint(&self, code: &str, value: &str) -> String {
        if self.enabled {
            format!("\x1b[{code}m{value}\x1b[0m")
        } else {
            value.to_owned()
        }
    }
}

pub fn load_key(key_file: Option<PathBuf>, key_env: &str) -> Result<SecretKey, String> {
    let bytes = if let Some(path) = key_file {
        std::fs::read(&path)
            .map_err(|error| format!("cannot read key file {}: {error}", path.display()))?
    } else {
        std::env::var_os(key_env)
            .ok_or_else(|| {
                format!(
                    "no key available; provide --key-file or set the environment variable named by --key-env ({key_env})"
                )
            })?
            .as_encoded_bytes()
            .to_vec()
    };
    SecretKey::new(bytes).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "0123456789abcdef0123456789abcdef";

    fn profile_args(descriptor_map: Vec<PathBuf>) -> ProfileArgs {
        ProfileArgs {
            policy: ProfilePreset::SafeHarborMetadata,
            enable_option: Vec::new(),
            disable_option: Vec::new(),
            descriptor_map,
            fail_on_unmapped: vec!["00080100".to_owned()],
            safe_private_allowlist: None,
            uid_map: None,
        }
    }

    #[test]
    fn descriptor_maps_merge_versioned_and_bare_documents() {
        let directory = tempfile::tempdir().unwrap();
        let shipped = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../descriptor-maps/breast-imaging-ps3.16-2026d.json"
        ));
        let bare = directory.path().join("bare.json");
        std::fs::write(
            &bare,
            r#"{"00080070": {"SYNTHVENDOR": "SYNTHVENDOR"}, "00080102": {"SRT": "SRT"}}"#,
        )
        .unwrap();
        let profile = profile_args(vec![shipped.clone(), bare]).load().unwrap();
        assert_eq!(
            profile.descriptor_replacement(dicom_core::Tag(0x0008, 0x0100), "R-10242"),
            Some("R-10242")
        );
        assert_eq!(
            profile.descriptor_replacement(dicom_core::Tag(0x0008, 0x0070), "SYNTHVENDOR"),
            Some("SYNTHVENDOR")
        );
        assert!(profile.fail_on_unmapped.contains("00080100"));
        for (tag, values) in &profile.descriptor_mappings {
            for (source, output) in values {
                assert_eq!(source, output, "{tag}");
            }
        }

        let conflicting = directory.path().join("conflicting.json");
        std::fs::write(&conflicting, r#"{"00080102": {"SRT": "OTHER"}}"#).unwrap();
        let error = profile_args(vec![shipped, conflicting]).load().unwrap_err();
        assert!(error.contains("conflicts"), "{error}");

        let unknown_format = directory.path().join("unknown-format.json");
        std::fs::write(
            &unknown_format,
            r#"{"format": "other/9", "name": "n", "version": "v", "source": "s", "mappings": {}}"#,
        )
        .unwrap();
        assert!(profile_args(vec![unknown_format]).load().is_err());
    }

    #[test]
    fn tag_parser_normalizes_case_and_rejects_other_forms() {
        assert_eq!(parse_tag("0008010a").unwrap(), "0008010A");
        for invalid in ["(0008,0100)", "0008010", "0008010G"] {
            assert!(parse_tag(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn styles_apply_color_only_when_enabled() {
        let auto_terminal = Styles::new(ColorMode::Auto, false, false, true);
        assert!(auto_terminal.success("PASS").starts_with("\x1b[1;32m"));
        let auto_redirected = Styles::new(ColorMode::Auto, false, false, false);
        assert_eq!(auto_redirected.success("PASS"), "PASS");

        let enabled = Styles::new(ColorMode::Always, false, false, false);
        assert!(enabled.success("PASS").starts_with("\x1b[1;32m"));
        assert!(enabled.warning("WARN").starts_with("\x1b[1;33m"));
        assert!(enabled.failure("FAIL").starts_with("\x1b[1;31m"));
        assert!(enabled.heading("Summary").starts_with("\x1b[1;36m"));

        for disabled in [
            Styles::new(ColorMode::Never, false, false, true),
            Styles::new(ColorMode::Always, true, false, true),
            Styles::new(ColorMode::Always, false, true, true),
        ] {
            assert_eq!(disabled.success("PASS"), "PASS");
        }
    }

    #[test]
    fn key_loader_supports_files_and_environment() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("key");
        std::fs::write(&path, KEY).unwrap();
        assert!(load_key(Some(path), "UNUSED").is_ok());

        const ENVIRONMENT_KEY: &str = "DICOM_ANON_CLI_COMMON_TEST_KEY";
        std::env::set_var(ENVIRONMENT_KEY, KEY);
        assert!(load_key(None, ENVIRONMENT_KEY).is_ok());
        std::env::remove_var(ENVIRONMENT_KEY);
        assert!(load_key(None, ENVIRONMENT_KEY).is_err());

        const SHORT_KEY: &str = "DICOM_ANON_CLI_COMMON_SHORT_KEY";
        std::env::set_var(SHORT_KEY, "short");
        assert!(load_key(None, SHORT_KEY).is_err());
        std::env::remove_var(SHORT_KEY);
    }
}
