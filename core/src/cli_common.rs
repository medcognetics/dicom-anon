//! Shared implementation details for the bundled commands.

use std::path::PathBuf;

use clap::ValueEnum;

use crate::SecretKey;

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
