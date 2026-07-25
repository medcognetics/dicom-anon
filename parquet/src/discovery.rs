use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use clap::ValueEnum;
use serde::Serialize;

use crate::error::{Error, IoContext, Result};

const DICM_OFFSET: u64 = 128;
const DICM_PREFIX: [u8; 4] = *b"DICM";

/// Ordering applied after DICOM candidates are discovered.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum SortMode {
    /// Group nearby inode numbers to improve locality on local file systems.
    #[default]
    Inode,
    /// Sort lexicographically by complete path.
    Path,
    /// Preserve the file-system traversal order.
    None,
}

/// Recursively discover DICOM candidates.
///
/// Files with a `.dcm` or `.dicom` extension are accepted without opening
/// them. Extensionless files are accepted only when `DICM` occurs at byte
/// offset 128. Files with any other extension are rejected without opening.
pub fn discover_dicom_files(source: &Path, sort: SortMode) -> Result<Vec<PathBuf>> {
    if !source.is_dir() {
        return Err(Error::SourceNotDirectory(source.to_path_buf()));
    }

    let mut directories = vec![source.to_path_buf()];
    let mut candidates = Vec::new();
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(&directory).at(&directory)? {
            let entry = entry.at(&directory)?;
            let file_type = entry.file_type().at(entry.path())?;
            if file_type.is_dir() {
                directories.push(entry.path());
            } else if file_type.is_file() && is_candidate(&entry.path()) {
                candidates.push(entry.path());
            }
        }
    }

    sort_candidates(&mut candidates, sort);
    Ok(candidates)
}

fn is_candidate(path: &Path) -> bool {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension)
            if extension.eq_ignore_ascii_case("dcm") || extension.eq_ignore_ascii_case("dicom") =>
        {
            true
        }
        Some(_) => false,
        None => has_part10_prefix(path),
    }
}

fn has_part10_prefix(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    if file.seek(SeekFrom::Start(DICM_OFFSET)).is_err() {
        return false;
    }
    let mut prefix = [0_u8; DICM_PREFIX.len()];
    file.read_exact(&mut prefix).is_ok() && prefix == DICM_PREFIX
}

fn sort_candidates(candidates: &mut [PathBuf], sort: SortMode) {
    match sort {
        SortMode::Path => candidates.sort_unstable(),
        SortMode::None => {}
        SortMode::Inode => sort_by_inode(candidates),
    }
}

#[cfg(unix)]
fn sort_by_inode(candidates: &mut [PathBuf]) {
    use std::os::unix::fs::MetadataExt;

    candidates.sort_by(|left, right| {
        let left_inode = left.metadata().ok().map(|metadata| metadata.ino());
        let right_inode = right.metadata().ok().map(|metadata| metadata.ino());
        left_inode.cmp(&right_inode).then_with(|| left.cmp(right))
    });
}

#[cfg(not(unix))]
fn sort_by_inode(candidates: &mut [PathBuf]) {
    candidates.sort_unstable();
}
