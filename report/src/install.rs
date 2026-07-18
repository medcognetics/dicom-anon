//! Reproducible, checksum-verified report-model and native-runtime installation.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use clap::ValueEnum;
use flate2::read::GzDecoder;
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::Archive;

use crate::{Error, Result};

/// Immutable OpenAI Privacy Filter revision packaged by the installer.
pub const PRIVACY_FILTER_REVISION: &str = "7ffa9a043d54d1be65afb281eddf0ffbe629385b";
/// Upstream model repository.
pub const PRIVACY_FILTER_REPOSITORY: &str = "openai/privacy-filter";
/// ONNX Runtime release used by the Rust runtime.
pub const ONNX_RUNTIME_VERSION: &str = "1.27.1";
/// PDFium API/build revision selected by `pdfium-render`.
pub const PDFIUM_REVISION: &str = "7881";

const HUGGING_FACE_BASE: &str = "https://huggingface.co/openai/privacy-filter/resolve";
const ONNX_RUNTIME_RELEASE_BASE: &str =
    "https://github.com/microsoft/onnxruntime/releases/download/v1.27.1";
const PDFIUM_ARCHIVE_URL: &str =
    "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/7881/pdfium-linux-x64.tgz";
const PDFIUM_ARCHIVE_SHA256: &str =
    "1470e21b8b4a3b4ad7f85684e2da11d94f3b69a86d81dee11b9b6709d927ac1d";

const COMMON_MODEL_FILES: [DownloadSpec; 4] = [
    DownloadSpec::new(
        "config.json",
        "b2b26a4a4a000639ad30b0c264adbefe365bdb567fbd7bb27303b8c438375bd1",
    ),
    DownloadSpec::new(
        "tokenizer.json",
        "0614fe83cadab421296e664e1f48f4261fa8fef6e03e63bb75c20f38e37d07d3",
    ),
    DownloadSpec::new(
        "viterbi_calibration.json",
        "bbc8611ef08a55ed72d64856cbbbb9a91db8dfa881f0a92e2afbad6e4bbc775a",
    ),
    DownloadSpec::new(
        "README.md",
        "48318ff48819858192dbb59f106dd709c3017e447bcc540807df66840a03a695",
    ),
];
const CPU_MODEL_FILES: [DownloadSpec; 2] = [
    DownloadSpec::new(
        "onnx/model_q4.onnx",
        "8f7dee8b46d096f052b359375dfba5d983cc4d18c44a783bf548615c472f8dea",
    ),
    DownloadSpec::new(
        "onnx/model_q4.onnx_data",
        "f30998e28c71c5374cc7e8b7de8f0f83e981592c0c2d652d2ad4928454dbb496",
    ),
];

pub(crate) fn verify_pinned_model_file(path: &Path, expected_bundle_path: &str) -> Result<()> {
    let expected = COMMON_MODEL_FILES
        .iter()
        .chain(CPU_MODEL_FILES.iter())
        .find(|spec| spec.path == expected_bundle_path)
        .ok_or_else(|| {
            Error::Model(format!(
                "no pinned checksum is defined for {expected_bundle_path}"
            ))
        })?;
    let actual = checksum_file(path).map_err(|error| {
        Error::Model(format!(
            "cannot verify pinned model asset {}: {error}",
            path.display()
        ))
    })?;
    if actual != expected.sha256 {
        return Err(Error::Model(format!(
            "checksum mismatch for pinned model asset {expected_bundle_path}"
        )));
    }
    Ok(())
}

/// Native execution backend installed alongside the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum AssetBackend {
    /// Quantized CPU model and CPU ONNX Runtime.
    Cpu,
    /// Quantized model and CUDA 12 ONNX Runtime.
    Cuda12,
    /// Quantized model and CUDA 13 ONNX Runtime.
    Cuda13,
}

impl AssetBackend {
    const fn directory(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Cuda12 => "cuda12",
            Self::Cuda13 => "cuda13",
        }
    }

    const fn model_files(self) -> &'static [DownloadSpec] {
        match self {
            Self::Cpu | Self::Cuda12 | Self::Cuda13 => &CPU_MODEL_FILES,
        }
    }

    const fn runtime_archive(self) -> ArchiveSpec {
        match self {
            Self::Cpu => ArchiveSpec::new(
                "onnxruntime-linux-x64-1.27.1.tgz",
                "25b1ef1fea1acd210d63f8f24dc870ad6e077795ce1f54876252c6d3803c15af",
            ),
            Self::Cuda12 => ArchiveSpec::new(
                "onnxruntime-linux-x64-gpu_cuda12-1.27.1.tgz",
                "08b568bd69500c36606aff7c3896ee4fa7d3531719f6b00f43e6a34db41dc4bf",
            ),
            Self::Cuda13 => ArchiveSpec::new(
                "onnxruntime-linux-x64-gpu_cuda13-1.27.1.tgz",
                "33ef9d3136914455fc5232acff80703245b464c7cb5b1666edbdf98d0caf257c",
            ),
        }
    }
}

/// Asset-installer configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetInstallOptions {
    /// Final model bundle directory. It must not already exist.
    pub target: PathBuf,
    /// CPU or CUDA runtime bundle.
    pub backend: AssetBackend,
    /// Include PDFium for PDF input. Tesseract remains a system dependency.
    pub include_pdfium: bool,
    /// Render download progress on stderr.
    pub progress: bool,
}

/// Checksum entry in the installed manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledFile {
    /// Relative bundle path.
    pub path: String,
    /// Lowercase SHA-256 digest.
    pub sha256: String,
}

/// Reproducible asset manifest written into every installed bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledAssetManifest {
    /// Manifest schema version.
    pub schema_version: u32,
    /// Upstream model repository.
    pub model_repository: String,
    /// Immutable upstream model revision.
    pub model_revision: String,
    /// Installed native backend.
    pub backend: AssetBackend,
    /// ONNX Runtime release.
    pub onnx_runtime_version: String,
    /// Included PDFium revision, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pdfium_revision: Option<String>,
    /// Deterministically sorted installed-file checksums.
    pub files: Vec<InstalledFile>,
}

/// Download, verify, and atomically install a model/runtime bundle.
pub fn install_assets(options: &AssetInstallOptions) -> Result<InstalledAssetManifest> {
    if cfg!(not(all(target_os = "linux", target_arch = "x86_64"))) {
        return Err(Error::Install(
            "the bundled native-runtime installer currently supports Linux x86-64 only".to_owned(),
        ));
    }
    if path_entry_exists(&options.target)? {
        return Err(Error::Install(format!(
            "target already exists: {}",
            options.target.display()
        )));
    }
    let parent = options
        .target
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| install_io(parent, error))?;
    let staging = tempfile::Builder::new()
        .prefix(".report-assets-")
        .tempdir_in(parent)
        .map_err(|error| install_io(parent, error))?;
    let bundle = staging.path().join("bundle");
    fs::create_dir(&bundle).map_err(|error| install_io(&bundle, error))?;

    for spec in COMMON_MODEL_FILES
        .iter()
        .chain(options.backend.model_files())
    {
        let url = format!(
            "{HUGGING_FACE_BASE}/{PRIVACY_FILTER_REVISION}/{}",
            spec.path
        );
        download_verified(&url, &bundle.join(spec.path), spec.sha256, options.progress)?;
    }
    fs::write(
        bundle.join("MODEL_LICENSE-APACHE"),
        include_bytes!("../../LICENSE-APACHE"),
    )
    .map_err(|error| install_io(&bundle.join("MODEL_LICENSE-APACHE"), error))?;
    fs::write(
        bundle.join("MODEL_NOTICE.txt"),
        format!(
            "OpenAI Privacy Filter\nRepository: {PRIVACY_FILTER_REPOSITORY}\nRevision: {PRIVACY_FILTER_REVISION}\nLicense declared by the upstream model card: Apache-2.0\n"
        ),
    )
    .map_err(|error| install_io(&bundle.join("MODEL_NOTICE.txt"), error))?;

    let runtime = options.backend.runtime_archive();
    let archive_path = staging.path().join(runtime.filename);
    download_verified(
        &format!("{ONNX_RUNTIME_RELEASE_BASE}/{}", runtime.filename),
        &archive_path,
        runtime.sha256,
        options.progress,
    )?;
    let runtime_dir = bundle.join("runtime").join(options.backend.directory());
    extract_runtime(&archive_path, &runtime_dir, options.backend)?;

    if options.include_pdfium {
        let archive_path = staging.path().join("pdfium-linux-x64.tgz");
        download_verified(
            PDFIUM_ARCHIVE_URL,
            &archive_path,
            PDFIUM_ARCHIVE_SHA256,
            options.progress,
        )?;
        extract_pdfium(&archive_path, &bundle.join("runtime/pdfium"))?;
    }

    let mut manifest = InstalledAssetManifest {
        schema_version: 1,
        model_repository: PRIVACY_FILTER_REPOSITORY.to_owned(),
        model_revision: PRIVACY_FILTER_REVISION.to_owned(),
        backend: options.backend,
        onnx_runtime_version: ONNX_RUNTIME_VERSION.to_owned(),
        pdfium_revision: options.include_pdfium.then(|| PDFIUM_REVISION.to_owned()),
        files: checksum_tree(&bundle)?,
    };
    manifest
        .files
        .sort_by(|left, right| left.path.cmp(&right.path));
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| Error::Install(format!("cannot serialize asset manifest: {error}")))?;
    fs::write(bundle.join("asset-manifest.json"), manifest_bytes)
        .map_err(|error| install_io(&bundle.join("asset-manifest.json"), error))?;
    fs::rename(&bundle, &options.target).map_err(|error| install_io(&options.target, error))?;
    Ok(manifest)
}

fn path_entry_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(install_io(path, error)),
    }
}

#[derive(Clone, Copy)]
struct DownloadSpec {
    path: &'static str,
    sha256: &'static str,
}

impl DownloadSpec {
    const fn new(path: &'static str, sha256: &'static str) -> Self {
        Self { path, sha256 }
    }
}

#[derive(Clone, Copy)]
struct ArchiveSpec {
    filename: &'static str,
    sha256: &'static str,
}

impl ArchiveSpec {
    const fn new(filename: &'static str, sha256: &'static str) -> Self {
        Self { filename, sha256 }
    }
}

fn download_verified(url: &str, destination: &Path, expected: &str, progress: bool) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| install_io(parent, error))?;
    }
    let mut response = ureq::get(url)
        .call()
        .map_err(|error| Error::Install(format!("download failed for {url}: {error}")))?;
    let length = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let bar = download_progress(length, destination, progress);
    let mut output = File::create(destination).map_err(|error| install_io(destination, error))?;
    let mut reader = response.body_mut().as_reader();
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| Error::Install(format!("download read failed for {url}: {error}")))?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .map_err(|error| install_io(destination, error))?;
        digest.update(&buffer[..count]);
        bar.inc(count as u64);
    }
    output
        .sync_all()
        .map_err(|error| install_io(destination, error))?;
    bar.finish_and_clear();
    let actual = encode_digest(digest.finalize());
    if actual != expected {
        return Err(Error::Install(format!(
            "checksum mismatch for {}",
            destination.display()
        )));
    }
    Ok(())
}

fn download_progress(length: Option<u64>, destination: &Path, enabled: bool) -> ProgressBar {
    if !enabled {
        return ProgressBar::hidden();
    }
    let bar = length.map_or_else(ProgressBar::new_spinner, ProgressBar::new);
    let template = if length.is_some() {
        "{spinner:.cyan} {msg} [{bar:30.cyan/blue}] {bytes}/{total_bytes}"
    } else {
        "{spinner:.cyan} {msg} {bytes}"
    };
    bar.set_style(
        ProgressStyle::with_template(template)
            .expect("the static progress template is valid")
            .progress_chars("=> "),
    );
    bar.set_message(
        destination
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("asset")
            .to_owned(),
    );
    bar.enable_steady_tick(std::time::Duration::from_millis(80));
    bar
}

fn extract_runtime(archive: &Path, destination: &Path, backend: AssetBackend) -> Result<()> {
    let mut expected = vec![
        "libonnxruntime.so.1.27.1",
        "libonnxruntime_providers_shared.so",
        "LICENSE",
        "ThirdPartyNotices.txt",
    ];
    if backend != AssetBackend::Cpu {
        expected.push("libonnxruntime_providers_cuda.so");
    }
    extract_named_files(archive, destination, &expected)
}

fn extract_pdfium(archive: &Path, destination: &Path) -> Result<()> {
    extract_named_files(
        archive,
        destination,
        &["libpdfium.so", "LICENSE", "VERSION"],
    )?;
    extract_archive_subtree(archive, destination, "licenses")
}

fn extract_archive_subtree(archive: &Path, destination: &Path, subtree: &str) -> Result<()> {
    let file = File::open(archive).map_err(|error| install_io(archive, error))?;
    let decoder = GzDecoder::new(file);
    let mut archive_reader = Archive::new(decoder);
    let entries = archive_reader
        .entries()
        .map_err(|error| Error::Install(format!("cannot read archive: {error}")))?;
    let mut extracted = 0_u64;
    for entry in entries {
        let mut entry =
            entry.map_err(|error| Error::Install(format!("cannot read archive entry: {error}")))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|error| Error::Install(format!("invalid archive path: {error}")))?;
        let components: Vec<_> = path.components().collect();
        let Some(start) = components
            .iter()
            .position(|component| component.as_os_str() == subtree)
        else {
            continue;
        };
        let relative =
            components[start..]
                .iter()
                .try_fold(PathBuf::new(), |mut result, component| {
                    if let std::path::Component::Normal(value) = component {
                        result.push(value);
                        Some(result)
                    } else {
                        None
                    }
                });
        let Some(relative) = relative else {
            continue;
        };
        let output_path = destination.join(relative);
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent).map_err(|error| install_io(parent, error))?;
        }
        let mut output =
            File::create(&output_path).map_err(|error| install_io(&output_path, error))?;
        std::io::copy(&mut entry, &mut output).map_err(|error| install_io(&output_path, error))?;
        extracted += 1;
    }
    if extracted == 0 {
        return Err(Error::Install(format!(
            "archive does not contain the required {subtree} notices"
        )));
    }
    Ok(())
}

fn extract_named_files(archive: &Path, destination: &Path, expected: &[&str]) -> Result<()> {
    fs::create_dir_all(destination).map_err(|error| install_io(destination, error))?;
    let file = File::open(archive).map_err(|error| install_io(archive, error))?;
    let decoder = GzDecoder::new(file);
    let mut archive_reader = Archive::new(decoder);
    let mut found = std::collections::BTreeSet::new();
    let entries = archive_reader
        .entries()
        .map_err(|error| Error::Install(format!("cannot read archive: {error}")))?;
    for entry in entries {
        let mut entry =
            entry.map_err(|error| Error::Install(format!("cannot read archive entry: {error}")))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|error| Error::Install(format!("invalid archive path: {error}")))?;
        let Some(filename) = path
            .file_name()
            .and_then(|value| value.to_str())
            .map(str::to_owned)
        else {
            continue;
        };
        if !expected.contains(&filename.as_str()) || found.contains(&filename) {
            continue;
        }
        let output_path = if filename.starts_with("lib") {
            destination.join("lib").join(&filename)
        } else {
            destination.join(&filename)
        };
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent).map_err(|error| install_io(parent, error))?;
        }
        let mut output =
            File::create(&output_path).map_err(|error| install_io(&output_path, error))?;
        std::io::copy(&mut entry, &mut output).map_err(|error| install_io(&output_path, error))?;
        found.insert(filename);
    }
    let missing: Vec<_> = expected
        .iter()
        .filter(|filename| !found.contains(**filename))
        .copied()
        .collect();
    if !missing.is_empty() {
        return Err(Error::Install(format!(
            "archive is missing required files: {}",
            missing.join(", ")
        )));
    }
    Ok(())
}

fn checksum_tree(root: &Path) -> Result<Vec<InstalledFile>> {
    fn visit(root: &Path, directory: &Path, output: &mut Vec<InstalledFile>) -> Result<()> {
        let mut entries = fs::read_dir(directory)
            .map_err(|error| install_io(directory, error))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| install_io(directory, error))?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, output)?;
            } else if path.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .expect("visited paths are beneath the bundle root")
                    .to_string_lossy()
                    .replace('\\', "/");
                output.push(InstalledFile {
                    path: relative,
                    sha256: checksum_file(&path)?,
                });
            }
        }
        Ok(())
    }

    let mut output = Vec::new();
    visit(root, root, &mut output)?;
    Ok(output)
}

fn checksum_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|error| install_io(path, error))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| install_io(path, error))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(encode_digest(digest.finalize()))
}

fn encode_digest(digest: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;

    let digest = digest.as_ref();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

fn install_io(path: &Path, source: std::io::Error) -> Error {
    Error::Install(format!("I/O error for {}: {source}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_install_target_is_never_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let error = install_assets(&AssetInstallOptions {
            target: directory.path().to_owned(),
            backend: AssetBackend::Cpu,
            include_pdfium: false,
            progress: false,
        })
        .unwrap_err();

        assert!(error.to_string().contains("target already exists"));
    }

    #[cfg(unix)]
    #[test]
    fn broken_symlink_counts_as_an_existing_install_target() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("reserved-target");
        symlink(directory.path().join("missing-target"), &target).unwrap();

        assert!(path_entry_exists(&target).unwrap());
        let error = install_assets(&AssetInstallOptions {
            target: target.clone(),
            backend: AssetBackend::Cpu,
            include_pdfium: false,
            progress: false,
        })
        .unwrap_err();

        assert!(error.to_string().contains("target already exists"));
        assert!(fs::symlink_metadata(target)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn digest_encoding_is_lowercase_and_stable() {
        assert_eq!(
            encode_digest(Sha256::digest(b"safe output")),
            "561c03f56ace489bb56fec63df72ebb01e73641954fdceae941253b0c99b6c65"
        );
    }

    #[test]
    fn archive_extraction_copies_only_required_named_files() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("librequired.so"), b"runtime").unwrap();
        fs::write(source.join("ignored.txt"), b"do not extract").unwrap();
        fs::write(source.join("component-license.txt"), b"license").unwrap();
        let archive_path = directory.path().join("runtime.tgz");
        let encoder = flate2::write::GzEncoder::new(
            File::create(&archive_path).unwrap(),
            flate2::Compression::default(),
        );
        let mut archive = tar::Builder::new(encoder);
        archive
            .append_path_with_name(source.join("librequired.so"), "bundle/lib/librequired.so")
            .unwrap();
        archive
            .append_path_with_name(source.join("ignored.txt"), "bundle/ignored.txt")
            .unwrap();
        archive
            .append_path_with_name(
                source.join("component-license.txt"),
                "bundle/licenses/component.txt",
            )
            .unwrap();
        archive.into_inner().unwrap().finish().unwrap();

        let output = directory.path().join("output");
        extract_named_files(&archive_path, &output, &["librequired.so"]).unwrap();
        extract_archive_subtree(&archive_path, &output, "licenses").unwrap();

        assert_eq!(
            fs::read(output.join("lib/librequired.so")).unwrap(),
            b"runtime"
        );
        assert_eq!(
            fs::read(output.join("licenses/component.txt")).unwrap(),
            b"license"
        );
        assert!(!output.join("ignored.txt").exists());
    }

    #[test]
    fn backend_metadata_selects_the_expected_runtime() {
        assert_eq!(AssetBackend::Cpu.directory(), "cpu");
        assert_eq!(AssetBackend::Cuda12.directory(), "cuda12");
        assert_eq!(AssetBackend::Cuda13.directory(), "cuda13");
        assert_eq!(AssetBackend::Cpu.model_files().len(), CPU_MODEL_FILES.len());
        assert!(AssetBackend::Cpu
            .runtime_archive()
            .filename
            .contains("linux-x64-"));
        assert!(AssetBackend::Cuda12
            .runtime_archive()
            .filename
            .contains("cuda12"));
        assert!(AssetBackend::Cuda13
            .runtime_archive()
            .filename
            .contains("cuda13"));
    }

    #[test]
    fn archive_extraction_fails_when_required_content_is_missing() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("present.txt");
        fs::write(&source, b"present").unwrap();
        let archive_path = directory.path().join("incomplete.tgz");
        let encoder = flate2::write::GzEncoder::new(
            File::create(&archive_path).unwrap(),
            flate2::Compression::default(),
        );
        let mut archive = tar::Builder::new(encoder);
        archive
            .append_path_with_name(&source, "bundle/present.txt")
            .unwrap();
        archive.into_inner().unwrap().finish().unwrap();

        assert!(extract_named_files(
            &archive_path,
            &directory.path().join("named"),
            &["missing.txt"]
        )
        .is_err());
        assert!(extract_archive_subtree(
            &archive_path,
            &directory.path().join("subtree"),
            "licenses"
        )
        .is_err());
    }

    #[test]
    fn checksum_tree_is_sorted_and_uses_relative_paths() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("nested")).unwrap();
        fs::write(directory.path().join("z.txt"), b"z").unwrap();
        fs::write(directory.path().join("nested/a.txt"), b"a").unwrap();

        let files = checksum_tree(directory.path()).unwrap();

        assert_eq!(files[0].path, "nested/a.txt");
        assert_eq!(files[1].path, "z.txt");
        assert_eq!(files[0].sha256.len(), 64);
        assert_eq!(
            checksum_file(&directory.path().join("z.txt"))
                .unwrap()
                .len(),
            64
        );
    }

    #[test]
    fn disabled_download_progress_is_hidden() {
        let progress = download_progress(Some(10), Path::new("model.onnx"), false);

        assert!(progress.is_hidden());
    }
}
