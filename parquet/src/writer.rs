use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{ArrayRef, StringBuilder};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_writer::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use tempfile::TempPath;

use crate::error::{Error, Result};
use crate::naming::ColumnNaming;
use crate::staging::Stage;

const DEFAULT_ROW_GROUP_ROWS: usize = 1024 * 1024;

pub(crate) struct WriteOptions {
    pub(crate) naming: ColumnNaming,
    pub(crate) shard_size_bytes: u64,
    pub(crate) compression_level: i32,
    pub(crate) batch_rows: usize,
}

pub(crate) fn write_shards(
    stage: &mut Stage,
    output_path: &Path,
    options: &WriteOptions,
) -> Result<Vec<PathBuf>> {
    if stage.rows() == 0 {
        return Ok(Vec::new());
    }
    let columns = stage.columns();
    let schema = schema(&columns, options.naming);
    let column_indices = columns
        .iter()
        .enumerate()
        .map(|(index, name)| (name.as_str(), index))
        .collect::<HashMap<_, _>>();
    let properties = WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(options.compression_level)
                .map_err(|_| Error::InvalidCompressionLevel(options.compression_level))?,
        ))
        .set_max_row_group_size(DEFAULT_ROW_GROUP_ROWS)
        .build();

    stage.rewind()?;
    let mut pending_shards = Vec::new();
    let mut shard_index = 0_usize;
    let mut active = None;
    loop {
        let Some(batch) = read_batch(
            stage,
            &columns,
            &column_indices,
            Arc::clone(&schema),
            options.batch_rows,
        )?
        else {
            break;
        };

        if active
            .as_ref()
            .is_some_and(|writer: &ActiveShard| writer.size() >= options.shard_size_bytes)
        {
            let writer = active.take().expect("active shard was checked");
            pending_shards.push(writer.finish()?);
            shard_index += 1;
        }
        if active.is_none() {
            active = Some(ActiveShard::new(
                output_path,
                shard_index,
                Arc::clone(&schema),
                properties.clone(),
            )?);
        }
        active
            .as_mut()
            .expect("active shard was initialized")
            .write(&batch)?;
    }

    if let Some(writer) = active {
        pending_shards.push(writer.finish()?);
    }
    persist_shards(pending_shards)
}

fn schema(columns: &[String], naming: ColumnNaming) -> SchemaRef {
    let sop_instance_uid = naming.apply("SOPInstanceUID");
    let study_instance_uid = naming.apply("StudyInstanceUID");
    Arc::new(Schema::new(
        columns
            .iter()
            .map(|name| {
                let nullable = name != &sop_instance_uid && name != &study_instance_uid;
                Field::new(name, DataType::Utf8, nullable)
            })
            .collect::<Vec<_>>(),
    ))
}

fn read_batch(
    stage: &mut Stage,
    columns: &[String],
    column_indices: &HashMap<&str, usize>,
    schema: SchemaRef,
    batch_rows: usize,
) -> Result<Option<RecordBatch>> {
    let mut builders = columns
        .iter()
        .map(|_| StringBuilder::with_capacity(batch_rows, batch_rows * 16))
        .collect::<Vec<_>>();
    let mut row_count = 0_usize;
    while row_count < batch_rows {
        let Some(row) = stage.read_row()? else {
            break;
        };
        let mut values = vec![None; columns.len()];
        for (name, value) in row {
            let index = column_indices
                .get(name.as_str())
                .copied()
                .ok_or(Error::InvalidStage)?;
            values[index] = Some(value);
        }
        for (builder, value) in builders.iter_mut().zip(values) {
            match value {
                Some(value) => builder.append_value(value),
                None => builder.append_null(),
            }
        }
        row_count += 1;
    }
    if row_count == 0 {
        return Ok(None);
    }

    let arrays = builders
        .into_iter()
        .map(|mut builder| Arc::new(builder.finish()) as ArrayRef)
        .collect::<Vec<_>>();
    Ok(Some(RecordBatch::try_new(schema, arrays)?))
}

struct ActiveShard {
    writer: ArrowWriter<File>,
    temporary_path: TempPath,
    target_path: PathBuf,
}

impl ActiveShard {
    fn new(
        output_path: &Path,
        index: usize,
        schema: SchemaRef,
        properties: WriterProperties,
    ) -> Result<Self> {
        let target_path = shard_path(output_path, index);
        if target_path.exists() {
            return Err(Error::OutputExists(target_path));
        }
        let parent = target_path.parent().unwrap_or_else(|| Path::new("."));
        let temporary = tempfile::NamedTempFile::new_in(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        let (file, temporary_path) = temporary.into_parts();
        let writer = ArrowWriter::try_new(file, schema, Some(properties))?;
        Ok(Self {
            writer,
            temporary_path,
            target_path,
        })
    }

    fn write(&mut self, batch: &RecordBatch) -> Result<()> {
        self.writer.write(batch)?;
        Ok(())
    }

    fn size(&self) -> u64 {
        let buffered = self
            .writer
            .bytes_written()
            .saturating_add(self.writer.in_progress_size());
        u64::try_from(buffered).unwrap_or(u64::MAX)
    }

    fn finish(self) -> Result<PendingShard> {
        let file = self.writer.into_inner()?;
        file.sync_all().map_err(|source| Error::Io {
            path: self.temporary_path.to_path_buf(),
            source,
        })?;
        Ok(PendingShard {
            temporary_path: self.temporary_path,
            target_path: self.target_path,
        })
    }
}

struct PendingShard {
    temporary_path: TempPath,
    target_path: PathBuf,
}

fn persist_shards(shards: Vec<PendingShard>) -> Result<Vec<PathBuf>> {
    if let Some(existing) = shards.iter().find(|shard| shard.target_path.exists()) {
        return Err(Error::OutputExists(existing.target_path.clone()));
    }
    let mut paths = Vec::with_capacity(shards.len());
    for shard in shards {
        let target_path = shard.target_path.clone();
        if let Err(error) = shard.temporary_path.persist_noclobber(&shard.target_path) {
            let publish_error = error.error;
            if let Some((rollback_path, rollback_error)) = rollback_shards(&paths) {
                return Err(Error::ShardRollback {
                    publish_path: target_path,
                    publish_error: publish_error.to_string(),
                    rollback_path,
                    rollback_error,
                });
            }
            return Err(Error::Io {
                path: target_path,
                source: publish_error,
            });
        }
        paths.push(target_path);
    }
    Ok(paths)
}

fn rollback_shards(paths: &[PathBuf]) -> Option<(PathBuf, std::io::Error)> {
    let mut first_failure = None;
    for path in paths.iter().rev() {
        if let Err(error) = std::fs::remove_file(path) {
            first_failure.get_or_insert_with(|| (path.clone(), error));
        }
    }
    first_failure
}

pub(crate) fn shard_path(output_path: &Path, index: usize) -> PathBuf {
    output_path.with_extension(format!("{index:05}.parquet"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{persist_shards, shard_path, write_shards, PendingShard, WriteOptions};
    use crate::dicom::MetadataRow;
    use crate::error::Error;
    use crate::naming::ColumnNaming;
    use crate::staging::Stage;

    fn row(uid: &str) -> MetadataRow {
        MetadataRow {
            values: BTreeMap::from([
                ("SOPInstanceUID".to_owned(), uid.to_owned()),
                ("StudyInstanceUID".to_owned(), "1.2.3".to_owned()),
            ]),
            ..MetadataRow::default()
        }
    }

    #[test]
    fn shard_paths_preserve_dotted_base_names() {
        assert_eq!(
            shard_path(std::path::Path::new("study.v1.parquet"), 0),
            std::path::Path::new("study.v1.00000.parquet")
        );
    }

    #[test]
    fn shard_rollover_occurs_between_batches() {
        let output = tempfile::tempdir().unwrap();
        let mut stage = Stage::new().unwrap();
        stage.push(&row("1.2.3.1")).unwrap();
        stage.push(&row("1.2.3.2")).unwrap();
        let shards = write_shards(
            &mut stage,
            &output.path().join("rows.parquet"),
            &WriteOptions {
                naming: ColumnNaming::Dicom,
                shard_size_bytes: 1,
                compression_level: 6,
                batch_rows: 1,
            },
        )
        .unwrap();
        assert_eq!(shards.len(), 2);
    }

    #[test]
    fn writer_rejects_invalid_compression_and_late_output_collisions() {
        let output = tempfile::tempdir().unwrap();
        let mut stage = Stage::new().unwrap();
        stage.push(&row("1.2.3.1")).unwrap();
        assert!(matches!(
            write_shards(
                &mut stage,
                &output.path().join("invalid.parquet"),
                &WriteOptions {
                    naming: ColumnNaming::Dicom,
                    shard_size_bytes: 1024,
                    compression_level: 0,
                    batch_rows: 1,
                },
            ),
            Err(Error::InvalidCompressionLevel(0))
        ));

        let temporary = tempfile::NamedTempFile::new_in(output.path()).unwrap();
        let (_, temporary_path) = temporary.into_parts();
        let target_path = output.path().join("occupied.parquet");
        std::fs::write(&target_path, b"occupied").unwrap();
        assert!(matches!(
            persist_shards(vec![PendingShard {
                temporary_path,
                target_path,
            }]),
            Err(Error::OutputExists(_))
        ));
    }

    #[test]
    fn persistence_failure_rolls_back_already_published_shards() {
        let output = tempfile::tempdir().unwrap();
        let first = tempfile::NamedTempFile::new_in(output.path()).unwrap();
        let second = tempfile::NamedTempFile::new_in(output.path()).unwrap();
        let (_, first_temporary_path) = first.into_parts();
        let (_, second_temporary_path) = second.into_parts();
        let first_target = output.path().join("first.parquet");
        let second_target = output.path().join("missing").join("second.parquet");

        let result = persist_shards(vec![
            PendingShard {
                temporary_path: first_temporary_path,
                target_path: first_target.clone(),
            },
            PendingShard {
                temporary_path: second_temporary_path,
                target_path: second_target,
            },
        ]);

        assert!(result.is_err());
        assert!(!first_target.exists());
    }
}
