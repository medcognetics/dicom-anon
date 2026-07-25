use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dicom_core::Tag;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use serde::Serialize;

use crate::dicom::{extract_metadata, ExtractionOptions};
use crate::discovery::{discover_dicom_files, SortMode};
use crate::error::{Error, Result};
use crate::naming::ColumnNaming;
use crate::staging::Stage;
use crate::writer::{write_shards, WriteOptions};

const BYTES_PER_MEBIBYTE: u64 = 1024 * 1024;
const DEFAULT_BATCH_ROWS: usize = 1024;
const DEFAULT_EXTRACTION_CHUNK_FILES: usize = 256;
const DEFAULT_SHARD_SIZE_MIB: u64 = 128;
const DEFAULT_COMPRESSION_LEVEL: i32 = 6;
const MAX_FAILURE_SAMPLES: usize = 20;

/// Complete conversion configuration.
#[derive(Clone, Debug)]
pub struct ConversionOptions {
    pub hash_pixel_data: bool,
    pub overrides: BTreeMap<Tag, String>,
    pub strict: bool,
    pub naming: ColumnNaming,
    pub sort: SortMode,
    pub shard_size_mib: u64,
    pub compression_level: i32,
    pub batch_rows: usize,
    pub extraction_chunk_files: usize,
    pub show_progress: bool,
}

impl Default for ConversionOptions {
    fn default() -> Self {
        Self {
            hash_pixel_data: false,
            overrides: BTreeMap::new(),
            strict: false,
            naming: ColumnNaming::Dicom,
            sort: SortMode::Inode,
            shard_size_mib: DEFAULT_SHARD_SIZE_MIB,
            compression_level: DEFAULT_COMPRESSION_LEVEL,
            batch_rows: DEFAULT_BATCH_ROWS,
            extraction_chunk_files: DEFAULT_EXTRACTION_CHUNK_FILES,
            show_progress: false,
        }
    }
}

/// A bounded sample of a best-effort file failure.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FailureSample {
    pub path: PathBuf,
    pub error: String,
}

/// Stable summary of a completed conversion.
#[derive(Clone, Debug, Serialize)]
pub struct ConversionSummary {
    pub source: PathBuf,
    pub output: PathBuf,
    pub discovered_files: usize,
    pub processed_files: usize,
    pub failed_files: usize,
    pub hashed_pixel_bytes: u64,
    pub shard_paths: Vec<PathBuf>,
    pub failure_samples: Vec<FailureSample>,
    #[serde(rename = "elapsed_ms", serialize_with = "serialize_duration_ms")]
    pub elapsed: Duration,
}

/// Convert a directory tree of DICOM Part 10 files into uniform Parquet shards.
pub fn convert_directory(
    source: &Path,
    output_path: &Path,
    options: &ConversionOptions,
) -> Result<ConversionSummary> {
    validate_options(output_path, options)?;
    let start = Instant::now();
    let paths = discover_dicom_files(source, options.sort)?;
    if paths.is_empty() {
        return Err(Error::NoDicomFiles(source.to_path_buf()));
    }

    let progress = processing_progress(paths.len(), options.show_progress);
    let extraction_options = ExtractionOptions {
        hash_pixel_data: options.hash_pixel_data,
        naming: options.naming,
        overrides: options.overrides.clone(),
    };
    let mut stage = Stage::new()?;
    let extraction = extract_files(
        &paths,
        options,
        &extraction_options,
        &mut stage,
        &progress,
        extract_metadata,
    )?;
    progress.finish_and_clear();

    if stage.rows() == 0 {
        return Err(Error::NoUsableDicomFiles {
            source_path: source.to_path_buf(),
            failed_files: extraction.failed_files,
        });
    }

    let shard_paths = write_shards(
        &mut stage,
        output_path,
        &WriteOptions {
            naming: options.naming,
            shard_size_bytes: options.shard_size_mib.saturating_mul(BYTES_PER_MEBIBYTE),
            compression_level: options.compression_level,
            batch_rows: options.batch_rows,
        },
    )?;

    Ok(ConversionSummary {
        source: source.to_path_buf(),
        output: output_path.to_path_buf(),
        discovered_files: paths.len(),
        processed_files: stage.rows(),
        failed_files: extraction.failed_files,
        hashed_pixel_bytes: extraction.hashed_pixel_bytes,
        shard_paths,
        failure_samples: extraction.failure_samples,
        elapsed: start.elapsed(),
    })
}

#[derive(Default)]
struct ExtractionSummary {
    failed_files: usize,
    failure_samples: Vec<FailureSample>,
    hashed_pixel_bytes: u64,
}

fn extract_files<F>(
    paths: &[PathBuf],
    options: &ConversionOptions,
    extraction_options: &ExtractionOptions,
    stage: &mut Stage,
    progress: &ProgressBar,
    extractor: F,
) -> Result<ExtractionSummary>
where
    F: Fn(&Path, &ExtractionOptions) -> Result<crate::dicom::MetadataRow> + Sync,
{
    let mut summary = ExtractionSummary::default();
    if options.strict {
        for path in paths {
            let row = match extractor(path, extraction_options) {
                Ok(row) => row,
                Err(error) => {
                    progress.abandon();
                    return Err(error);
                }
            };
            summary.hashed_pixel_bytes = summary
                .hashed_pixel_bytes
                .saturating_add(row.stats.hashed_pixel_bytes);
            stage.push(&row)?;
            progress.inc(1);
        }
        return Ok(summary);
    }

    let chunk_size = options.extraction_chunk_files.max(1);
    for chunk in paths.chunks(chunk_size) {
        let results = chunk
            .par_iter()
            .map(|path| (path, extractor(path.as_path(), extraction_options)))
            .collect::<Vec<_>>();
        for (path, result) in results {
            match result {
                Ok(row) => {
                    summary.hashed_pixel_bytes = summary
                        .hashed_pixel_bytes
                        .saturating_add(row.stats.hashed_pixel_bytes);
                    stage.push(&row)?;
                }
                Err(error) => {
                    summary.failed_files += 1;
                    if summary.failure_samples.len() < MAX_FAILURE_SAMPLES {
                        summary.failure_samples.push(FailureSample {
                            path: path.clone(),
                            error: error.to_string(),
                        });
                    }
                }
            }
            progress.inc(1);
        }
    }
    Ok(summary)
}

fn validate_options(output_path: &Path, options: &ConversionOptions) -> Result<()> {
    let parent = output_path.parent().unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(Error::OutputDirectoryNotDirectory(parent.to_path_buf()));
    }
    if !(1..=22).contains(&options.compression_level) {
        return Err(Error::InvalidCompressionLevel(options.compression_level));
    }
    if options.shard_size_mib == 0 || options.batch_rows == 0 {
        return Err(Error::InvalidStage);
    }
    Ok(())
}

fn processing_progress(file_count: usize, show: bool) -> ProgressBar {
    if !show {
        return ProgressBar::hidden();
    }
    let progress = ProgressBar::new(file_count as u64);
    if let Ok(style) = ProgressStyle::with_template(
        "Processing DICOM files [{bar:40.cyan/blue}] {pos}/{len} ({eta} @ {per_sec})",
    ) {
        progress.set_style(style.progress_chars("=> "));
    }
    progress
}

fn serialize_duration_ms<S>(
    duration: &Duration,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_u128(duration.as_millis())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{extract_files, ConversionOptions};
    use crate::dicom::{ExtractionOptions, MetadataRow};
    use crate::error::Error;
    use crate::staging::Stage;

    #[test]
    fn strict_extraction_does_not_process_paths_after_the_first_error() {
        let paths = [
            std::path::PathBuf::from("first.dcm"),
            std::path::PathBuf::from("second.dcm"),
        ];
        let options = ConversionOptions {
            strict: true,
            ..ConversionOptions::default()
        };
        let extraction_options = ExtractionOptions::default();
        let mut stage = Stage::new().unwrap();
        let progress = indicatif::ProgressBar::hidden();
        let calls = AtomicUsize::new(0);

        let result = extract_files(
            &paths,
            &options,
            &extraction_options,
            &mut stage,
            &progress,
            |path, _| {
                calls.fetch_add(1, Ordering::Relaxed);
                if path == paths[0] {
                    Err(Error::NoDicomFiles(path.to_path_buf()))
                } else {
                    Ok(MetadataRow::default())
                }
            },
        );

        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
}
