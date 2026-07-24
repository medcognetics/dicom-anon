use std::path::PathBuf;

/// Errors produced by DICOM discovery, extraction, staging, and Parquet output.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("source path is not a directory: {}", .0.display())]
    SourceNotDirectory(PathBuf),

    #[error("output directory does not exist or is not a directory: {}", .0.display())]
    OutputDirectoryNotDirectory(PathBuf),

    #[error("no DICOM files found under {}", .0.display())]
    NoDicomFiles(PathBuf),

    #[error(
        "no usable DICOM files found under {}; {failed_files} candidate(s) failed",
        source_path.display()
    )]
    NoUsableDicomFiles {
        source_path: PathBuf,
        failed_files: usize,
    },

    #[error("failed to access {}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse DICOM file {}", path.display())]
    Dicom {
        path: PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("DICOM file {} is missing a non-empty {field}", path.display())]
    MissingRequiredField { path: PathBuf, field: &'static str },

    #[error("DICOM tag name is not recognized: {0}")]
    UnknownTag(String),

    #[error("Pixel Data cannot be supplied through metadata overrides")]
    PixelDataOverride,

    #[error("column naming maps both {first} and {second} to {name}")]
    ColumnCollision {
        name: String,
        first: String,
        second: String,
    },

    #[error("output shard already exists: {}", .0.display())]
    OutputExists(PathBuf),

    #[error(
        "failed to publish shard {}; rollback also failed for {}: {publish_error}",
        publish_path.display(),
        rollback_path.display()
    )]
    ShardRollback {
        publish_path: PathBuf,
        publish_error: String,
        rollback_path: PathBuf,
        #[source]
        rollback_error: std::io::Error,
    },

    #[error("invalid Zstandard compression level {0}; expected 1 through 22")]
    InvalidCompressionLevel(i32),

    #[error("a staged metadata row exceeds the supported 4 GiB field limit")]
    StageFieldTooLarge,

    #[error("staged metadata is truncated or corrupt")]
    InvalidStage,

    #[error("failed to create Arrow data")]
    Arrow(#[from] arrow::error::ArrowError),

    #[error("failed to write Parquet data")]
    Parquet(#[from] parquet::errors::ParquetError),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) trait IoContext<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|source| Error::Io {
            path: path.into(),
            source,
        })
    }
}
