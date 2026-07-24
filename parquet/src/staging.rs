use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

use crate::dicom::MetadataRow;
use crate::error::{Error, Result};

const LENGTH_BYTES: usize = std::mem::size_of::<u32>();

pub(crate) struct Stage {
    file: File,
    columns: BTreeMap<String, usize>,
    rows: usize,
}

impl Stage {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            file: tempfile::tempfile().map_err(|source| Error::Io {
                path: std::env::temp_dir(),
                source,
            })?,
            columns: BTreeMap::new(),
            rows: 0,
        })
    }

    pub(crate) fn push(&mut self, row: &MetadataRow) -> Result<()> {
        write_length(&mut self.file, row.values.len())?;
        for (name, value) in &row.values {
            let next_index = self.columns.len();
            self.columns.entry(name.clone()).or_insert(next_index);
            write_bytes(&mut self.file, name.as_bytes())?;
            write_bytes(&mut self.file, value.as_bytes())?;
        }
        self.rows += 1;
        Ok(())
    }

    pub(crate) fn rows(&self) -> usize {
        self.rows
    }

    pub(crate) fn columns(&self) -> Vec<String> {
        let mut columns = self
            .columns
            .iter()
            .map(|(name, &index)| (index, name.clone()))
            .collect::<Vec<_>>();
        columns.sort_unstable_by_key(|(index, _)| *index);
        columns.into_iter().map(|(_, name)| name).collect()
    }

    pub(crate) fn rewind(&mut self) -> Result<()> {
        self.file
            .seek(SeekFrom::Start(0))
            .map(|_| ())
            .map_err(|source| Error::Io {
                path: std::env::temp_dir(),
                source,
            })
    }

    pub(crate) fn read_row(&mut self) -> Result<Option<BTreeMap<String, String>>> {
        let Some(field_count) = read_optional_length(&mut self.file)? else {
            return Ok(None);
        };
        let mut row = BTreeMap::new();
        for _ in 0..field_count {
            let name = read_string(&mut self.file)?;
            let value = read_string(&mut self.file)?;
            row.insert(name, value);
        }
        Ok(Some(row))
    }
}

fn write_length(writer: &mut File, length: usize) -> Result<()> {
    let length = u32::try_from(length).map_err(|_| Error::StageFieldTooLarge)?;
    writer
        .write_all(&length.to_le_bytes())
        .map_err(stage_io_error)
}

fn write_bytes(writer: &mut File, bytes: &[u8]) -> Result<()> {
    write_length(writer, bytes.len())?;
    writer.write_all(bytes).map_err(stage_io_error)
}

fn read_optional_length(reader: &mut File) -> Result<Option<usize>> {
    let mut bytes = [0_u8; LENGTH_BYTES];
    let read = reader.read(&mut bytes).map_err(stage_io_error)?;
    if read == 0 {
        return Ok(None);
    }
    if read != LENGTH_BYTES {
        reader
            .read_exact(&mut bytes[read..])
            .map_err(|_| Error::InvalidStage)?;
    }
    Ok(Some(u32::from_le_bytes(bytes) as usize))
}

fn read_length(reader: &mut File) -> Result<usize> {
    read_optional_length(reader)?.ok_or(Error::InvalidStage)
}

fn read_string(reader: &mut File) -> Result<String> {
    let length = read_length(reader)?;
    let mut bytes = vec![0_u8; length];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| Error::InvalidStage)?;
    String::from_utf8(bytes).map_err(|_| Error::InvalidStage)
}

fn stage_io_error(source: std::io::Error) -> Error {
    Error::Io {
        path: std::env::temp_dir(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Seek, SeekFrom, Write};

    use super::{read_optional_length, stage_io_error, write_length, Stage};
    use crate::error::Error;

    #[test]
    fn truncated_lengths_and_invalid_utf8_are_rejected() {
        let mut stage = Stage::new().unwrap();
        stage.file.write_all(&[1]).unwrap();
        stage.file.seek(SeekFrom::Start(0)).unwrap();
        assert!(matches!(stage.read_row(), Err(Error::InvalidStage)));

        stage.file.set_len(0).unwrap();
        stage.file.seek(SeekFrom::Start(0)).unwrap();
        write_length(&mut stage.file, 1).unwrap();
        write_length(&mut stage.file, 1).unwrap();
        stage.file.write_all(&[0xFF]).unwrap();
        write_length(&mut stage.file, 1).unwrap();
        stage.file.write_all(b"x").unwrap();
        stage.file.seek(SeekFrom::Start(0)).unwrap();
        assert!(matches!(stage.read_row(), Err(Error::InvalidStage)));
    }

    #[test]
    fn length_bound_and_io_context_errors_are_structured() {
        let mut file = tempfile::tempfile().unwrap();
        if usize::BITS > u32::BITS {
            assert!(matches!(
                write_length(&mut file, usize::MAX),
                Err(Error::StageFieldTooLarge)
            ));
        }
        file.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(read_optional_length(&mut file).unwrap(), None);

        let error = stage_io_error(std::io::Error::other("test"));
        assert!(error.to_string().contains("failed to access"));
    }
}
