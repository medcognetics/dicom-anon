use clap::ValueEnum;
use serde::Serialize;

/// Naming convention used for Parquet columns.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ColumnNaming {
    /// Preserve standard DICOM attribute aliases such as `SOPInstanceUID`.
    #[default]
    Dicom,
    /// Convert aliases to lowercase snake case, suitable for Athena.
    SnakeCase,
}

impl ColumnNaming {
    pub(crate) fn apply(self, name: &str) -> String {
        match self {
            Self::Dicom => name.to_owned(),
            Self::SnakeCase => snake_case(name),
        }
    }
}

/// Convert a DICOM alias to snake case while preserving acronym boundaries.
pub fn snake_case(input: &str) -> String {
    let characters = input.chars().collect::<Vec<_>>();
    let Some(&first) = characters.first() else {
        return String::new();
    };

    let mut output = String::with_capacity(input.len() + input.len() / 4);
    output.extend(first.to_lowercase());
    for index in 1..characters.len() {
        let current = characters[index];
        let previous = characters[index - 1];
        let next_is_lowercase = characters
            .get(index + 1)
            .is_some_and(|next| next.is_lowercase());
        if current.is_uppercase() && (previous.is_lowercase() || next_is_lowercase) {
            output.push('_');
        }
        output.extend(current.to_lowercase());
    }
    output
}
