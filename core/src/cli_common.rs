//! Shared implementation details for the bundled commands.

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
    /// JSON exact descriptor source-to-safe-output mappings.
    #[arg(long, value_name = "PATH")]
    pub descriptor_map: Option<PathBuf>,
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
        if let Some(path) = &self.descriptor_map {
            profile.descriptor_mappings = read_json(path, "descriptor map")?;
        }
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
