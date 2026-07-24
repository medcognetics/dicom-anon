//! Streaming metadata-only DICOM to sharded Parquet conversion.

mod dicom;
mod discovery;
mod error;
mod naming;
mod pipeline;
mod staging;
mod writer;

pub use crate::dicom::{extract_metadata, ExtractionOptions, MetadataRow};
pub use crate::discovery::{discover_dicom_files, SortMode};
pub use crate::error::{Error, Result};
pub use crate::naming::{snake_case, ColumnNaming};
pub use crate::pipeline::{convert_directory, ConversionOptions, ConversionSummary};
