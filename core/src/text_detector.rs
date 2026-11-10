//! A pinned text-detection model for the burned-in text check (#47).
//!
//! The model is the PaddleOCR PP-OCRv4 mobile text detector (DBNet, Apache-2.0), in the
//! ONNX export of the RapidOCR project. It runs on the CPU through ONNX Runtime, which
//! is loaded at run time. Both files are downloaded once by [`install_text_detector`]
//! and are checked against the pinned SHA-256 values below each time they are loaded.
//! The detector only proposes boxes: `pixel_text` decides whether a box can be censored,
//! with the same background rules as for the heuristic.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Which detector finds burned-in text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum TextDetector {
    /// The deterministic connected-component heuristic only.
    #[default]
    Heuristic,
    /// The pinned model only.
    Model,
    /// Both; a region either one finds is handled.
    Both,
}

impl TextDetector {
    /// Whether the pinned model runs.
    #[must_use]
    pub fn uses_model(self) -> bool {
        matches!(self, Self::Model | Self::Both)
    }

    /// Whether the heuristic runs.
    #[must_use]
    pub fn uses_heuristic(self) -> bool {
        matches!(self, Self::Heuristic | Self::Both)
    }
}

/// The model file, as installed under `<dir>/model/`.
pub const MODEL_FILE: &str = "ch_PP-OCRv4_det_infer.onnx";
/// SHA-256 of [`MODEL_FILE`].
pub const MODEL_SHA256: &str = "d2a7720d45a54257208b1e13e36a8479894cb74155a5efe29462512d42f49da9";
/// Immutable source of [`MODEL_FILE`]: RapidOCR revision 1cfba2e9, Apache-2.0.
pub const MODEL_URL: &str = "https://huggingface.co/SWHL/RapidOCR/resolve/1cfba2e90fc938db55889873735088de210cc173/PP-OCRv4/ch_PP-OCRv4_det_infer.onnx";
/// ONNX Runtime release that runs the model, as for the report models.
pub const ONNX_RUNTIME_VERSION: &str = "1.27.1";

/// One platform's pinned ONNX Runtime CPU library.
#[derive(Debug, Clone, Copy)]
pub struct RuntimePin {
    /// Release archive.
    pub archive_url: &'static str,
    /// SHA-256 of the archive.
    pub archive_sha256: &'static str,
    /// Library file name, inside the archive's `lib/` folder and under `<dir>/runtime/`.
    pub library: &'static str,
    /// SHA-256 of the library file.
    pub library_sha256: &'static str,
}

/// The pinned ONNX Runtime library for this platform, if one is pinned.
#[must_use]
pub fn runtime_pin() -> Option<RuntimePin> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(RuntimePin {
            archive_url: "https://github.com/microsoft/onnxruntime/releases/download/v1.27.1/onnxruntime-linux-x64-1.27.1.tgz",
            archive_sha256: "25b1ef1fea1acd210d63f8f24dc870ad6e077795ce1f54876252c6d3803c15af",
            library: "libonnxruntime.so.1.27.1",
            library_sha256: "50e214fa23276cf2f3fb96d90d8e7ca9fc5ba29f42e298eccf3b23f7d8a81ae2",
        })
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some(RuntimePin {
            archive_url: "https://github.com/microsoft/onnxruntime/releases/download/v1.27.1/onnxruntime-linux-aarch64-1.27.1.tgz",
            archive_sha256: "33c67e33d1e25b816878366ea276589a024f71f000e7ff955c4b33224d639edd",
            library: "libonnxruntime.so.1.27.1",
            library_sha256: "2aede08815e740c8c1fce21af1e94353d32190127219759cc7978fb8f2de30f7",
        })
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(RuntimePin {
            archive_url: "https://github.com/microsoft/onnxruntime/releases/download/v1.27.1/onnxruntime-osx-arm64-1.27.1.tgz",
            archive_sha256: "e42b77a7281cc6e55141bf44fcfbac2c782b823a491bbb6ac33c781dd991f8a6",
            library: "libonnxruntime.1.27.1.dylib",
            library_sha256: "a441a94433ebf38f1918009aba0199f88c6f877251b9946d89816427839964c8",
        })
    } else {
        None
    }
}

/// The model and library paths under an installed detector folder.
#[cfg_attr(not(feature = "text-detector"), allow(dead_code))]
fn asset_paths(dir: &Path) -> Result<(PathBuf, PathBuf, RuntimePin)> {
    let pin = runtime_pin()
        .ok_or_else(|| not_assessed("no ONNX Runtime library is pinned for this platform"))?;
    Ok((
        dir.join("model").join(MODEL_FILE),
        dir.join("runtime").join(pin.library),
        pin,
    ))
}

#[cfg_attr(not(feature = "text-detector"), allow(dead_code))]
fn not_assessed(reason: &str) -> Error {
    Error::PixelTextNotAssessed(format!("text detector: {reason}"))
}

/// A detected text box, in frame pixels, inclusive.
#[cfg_attr(not(feature = "text-detector"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TextBox {
    pub top: usize,
    pub left: usize,
    pub bottom: usize,
    pub right: usize,
}

/// Binarization threshold of the probability map (PaddleOCR `det_db_thresh`).
#[cfg_attr(not(feature = "text-detector"), allow(dead_code))]
const MAP_THRESHOLD: f32 = 0.3;
/// Lowest mean probability of a kept box (PaddleOCR `det_db_box_thresh`).
#[cfg_attr(not(feature = "text-detector"), allow(dead_code))]
const BOX_THRESHOLD: f32 = 0.6;
/// Box expansion (PaddleOCR `det_db_unclip_ratio`).
#[cfg_attr(not(feature = "text-detector"), allow(dead_code))]
const UNCLIP_RATIO: f64 = 1.5;
/// Smallest box side in map pixels (PaddleOCR `min_size`).
#[cfg_attr(not(feature = "text-detector"), allow(dead_code))]
const MIN_BOX_SIDE: usize = 3;
/// Longest model input side. Larger frames are scaled down.
#[cfg_attr(not(feature = "text-detector"), allow(dead_code))]
const MAX_INPUT_SIDE: usize = 1920;
/// ImageNet normalization that the model was trained with.
#[cfg_attr(not(feature = "text-detector"), allow(dead_code))]
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
#[cfg_attr(not(feature = "text-detector"), allow(dead_code))]
const STD: [f32; 3] = [0.229, 0.224, 0.225];

/// Model input size for a frame: scaled to at most `MAX_INPUT_SIDE`, rounded to a
/// multiple of 32 and at least 32.
#[cfg_attr(not(feature = "text-detector"), allow(dead_code))]
fn input_size(rows: usize, columns: usize) -> (usize, usize) {
    let scale = (MAX_INPUT_SIDE as f64 / rows.max(columns) as f64).min(1.0);
    let round = |side: usize| ((side as f64 * scale / 32.0).round() as usize).max(1) * 32;
    (round(rows), round(columns))
}

/// The model input tensor, `[1, 3, height, width]`: the frame mapped to 0..1 over its
/// own range, resized bilinearly, replicated to three channels and normalized.
#[cfg_attr(not(feature = "text-detector"), allow(dead_code))]
fn input_tensor(
    view: &[u16],
    rows: usize,
    columns: usize,
    low: u16,
    high: u16,
) -> (Vec<f32>, usize, usize) {
    let (height, width) = input_size(rows, columns);
    let range = f32::from(high - low).max(1.0);
    let at = |row: usize, column: usize| f32::from(view[row * columns + column] - low) / range;
    let mut plane = vec![0.0_f32; height * width];
    for y in 0..height {
        let source_y =
            ((y as f32 + 0.5) * rows as f32 / height as f32 - 0.5).clamp(0.0, (rows - 1) as f32);
        let (y0, fy) = (source_y.floor() as usize, source_y.fract());
        let y1 = (y0 + 1).min(rows - 1);
        for x in 0..width {
            let source_x = ((x as f32 + 0.5) * columns as f32 / width as f32 - 0.5)
                .clamp(0.0, (columns - 1) as f32);
            let (x0, fx) = (source_x.floor() as usize, source_x.fract());
            let x1 = (x0 + 1).min(columns - 1);
            let top = at(y0, x0) * (1.0 - fx) + at(y0, x1) * fx;
            let bottom = at(y1, x0) * (1.0 - fx) + at(y1, x1) * fx;
            plane[y * width + x] = top * (1.0 - fy) + bottom * fy;
        }
    }
    let mut tensor = Vec::with_capacity(3 * height * width);
    for channel in 0..3 {
        tensor.extend(
            plane
                .iter()
                .map(|value| (value - MEAN[channel]) / STD[channel]),
        );
    }
    (tensor, height, width)
}

/// DBNet post-processing: boxes of the 4-connected regions above `MAP_THRESHOLD` whose
/// mean probability reaches `BOX_THRESHOLD`, expanded by the unclip offset and scaled to
/// the frame.
#[cfg_attr(not(feature = "text-detector"), allow(dead_code))]
pub(crate) fn boxes_from_map(
    map: &[f32],
    height: usize,
    width: usize,
    rows: usize,
    columns: usize,
) -> Vec<TextBox> {
    let mut seen = vec![false; map.len()];
    let mut boxes = Vec::new();
    let mut stack = Vec::new();
    for start in 0..map.len() {
        if seen[start] || map[start] <= MAP_THRESHOLD {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let (mut top, mut left, mut bottom, mut right) = (height, width, 0, 0);
        let (mut sum, mut count) = (0.0_f64, 0_usize);
        while let Some(index) = stack.pop() {
            let (y, x) = (index / width, index % width);
            top = top.min(y);
            bottom = bottom.max(y);
            left = left.min(x);
            right = right.max(x);
            sum += f64::from(map[index]);
            count += 1;
            let mut visit = |neighbor: usize| {
                if !seen[neighbor] && map[neighbor] > MAP_THRESHOLD {
                    seen[neighbor] = true;
                    stack.push(neighbor);
                }
            };
            if y > 0 {
                visit(index - width);
            }
            if y + 1 < height {
                visit(index + width);
            }
            if x > 0 {
                visit(index - 1);
            }
            if x + 1 < width {
                visit(index + 1);
            }
        }
        let (box_height, box_width) = (bottom - top + 1, right - left + 1);
        if box_height.min(box_width) < MIN_BOX_SIDE
            || sum / (count as f64) < f64::from(BOX_THRESHOLD)
        {
            continue;
        }
        // The shrunk text kernel grows back by area * ratio / perimeter.
        let offset =
            (box_height * box_width) as f64 * UNCLIP_RATIO / (2 * (box_height + box_width)) as f64;
        let (scale_y, scale_x) = (rows as f64 / height as f64, columns as f64 / width as f64);
        let to_frame =
            |value: f64, scale: f64, limit: usize| (value * scale).clamp(0.0, (limit - 1) as f64);
        boxes.push(TextBox {
            top: to_frame(top as f64 - offset, scale_y, rows).floor() as usize,
            left: to_frame(left as f64 - offset, scale_x, columns).floor() as usize,
            bottom: to_frame(bottom as f64 + 1.0 + offset, scale_y, rows).ceil() as usize,
            right: to_frame(right as f64 + 1.0 + offset, scale_x, columns).ceil() as usize,
        });
    }
    boxes
}

#[cfg(feature = "text-detector")]
mod runtime {
    use std::collections::BTreeMap;
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, OnceLock};

    use ort::session::{builder::GraphOptimizationLevel, Session};
    use ort::value::TensorRef;
    use sha2::{Digest, Sha256};

    use super::{asset_paths, boxes_from_map, input_tensor, not_assessed, TextBox, MODEL_SHA256};
    use crate::Result;

    /// A loaded, verified detector.
    pub(crate) struct Detector {
        session: Mutex<Session>,
    }

    static LOADED_RUNTIME: Mutex<Option<PathBuf>> = Mutex::new(None);

    fn detectors() -> &'static Mutex<BTreeMap<PathBuf, Arc<Detector>>> {
        static DETECTORS: OnceLock<Mutex<BTreeMap<PathBuf, Arc<Detector>>>> = OnceLock::new();
        DETECTORS.get_or_init(|| Mutex::new(BTreeMap::new()))
    }

    fn sha256(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn read_verified(path: &Path, expected: &str, what: &str) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .and_then(|mut file| file.read_to_end(&mut bytes))
            .map_err(|_| {
                not_assessed(&format!(
                    "the {what} is missing or unreadable; install it with install_text_detector"
                ))
            })?;
        if sha256(&bytes) != expected {
            return Err(not_assessed(&format!(
                "the {what} does not match its pinned SHA-256"
            )));
        }
        Ok(bytes)
    }

    fn initialize_runtime(library: &Path) -> Result<()> {
        let mut loaded = LOADED_RUNTIME
            .lock()
            .map_err(|_| not_assessed("the runtime lock is poisoned"))?;
        if let Some(existing) = loaded.as_deref() {
            if existing != library {
                return Err(not_assessed(
                    "ONNX Runtime is already loaded from another folder in this process",
                ));
            }
            return Ok(());
        }
        let builder = ort::init_from(library)
            .map_err(|error| not_assessed(&format!("ONNX Runtime cannot be loaded: {error}")))?;
        if !builder.commit() {
            return Err(not_assessed(
                "ONNX Runtime was initialized before the text detector",
            ));
        }
        *loaded = Some(library.to_path_buf());
        Ok(())
    }

    /// The detector installed in `dir`, loaded once per process. Both files are hashed
    /// on first load; the model runs from the verified bytes.
    pub(crate) fn detector(dir: &Path) -> Result<Arc<Detector>> {
        let mut cache = detectors()
            .lock()
            .map_err(|_| not_assessed("the detector cache lock is poisoned"))?;
        if let Some(detector) = cache.get(dir) {
            return Ok(Arc::clone(detector));
        }
        let (model_path, library_path, pin) = asset_paths(dir)?;
        let model = read_verified(&model_path, MODEL_SHA256, "model file")?;
        read_verified(&library_path, pin.library_sha256, "ONNX Runtime library")?;
        initialize_runtime(&library_path)?;
        let session = Session::builder()
            .and_then(|builder| builder.with_optimization_level(GraphOptimizationLevel::Level3))
            // One thread on each axis, so that the probability map, and so the output,
            // is the same on every run.
            .and_then(|builder| builder.with_intra_threads(1))
            .and_then(|builder| builder.with_inter_threads(1))
            .and_then(|builder| builder.commit_from_memory(&model))
            .map_err(|error| not_assessed(&format!("the model cannot be loaded: {error}")))?;
        let detector = Arc::new(Detector {
            session: Mutex::new(session),
        });
        cache.insert(dir.to_path_buf(), Arc::clone(&detector));
        Ok(detector)
    }

    impl Detector {
        /// Text boxes in one frame `view`, where text is bright.
        pub(crate) fn detect(
            &self,
            view: &[u16],
            rows: usize,
            columns: usize,
            low: u16,
            high: u16,
        ) -> Result<Vec<TextBox>> {
            let (tensor, height, width) = input_tensor(view, rows, columns, low, high);
            let input = TensorRef::from_array_view((
                [1_i64, 3, height as i64, width as i64],
                tensor.as_slice(),
            ))
            .map_err(|error| not_assessed(&format!("the model input cannot be built: {error}")))?;
            let mut session = self
                .session
                .lock()
                .map_err(|_| not_assessed("the model session lock is poisoned"))?;
            let outputs = session
                .run(ort::inputs![input])
                .map_err(|error| not_assessed(&format!("the model run failed: {error}")))?;
            let (shape, map) = outputs[0].try_extract_tensor::<f32>().map_err(|error| {
                not_assessed(&format!(
                    "the model output is not a probability map: {error}"
                ))
            })?;
            if shape.as_ref() != [1_i64, 1, height as i64, width as i64] {
                return Err(not_assessed("the model output has an unexpected shape"));
            }
            if map.iter().any(|value| !value.is_finite()) {
                return Err(not_assessed("the model output is not finite"));
            }
            Ok(boxes_from_map(map, height, width, rows, columns))
        }
    }
}

#[cfg(feature = "text-detector")]
pub(crate) use runtime::{detector, Detector};

/// Download the pinned model and this platform's ONNX Runtime library into `target`,
/// which must not exist. Every file is checked against its pinned SHA-256. The folder
/// is filled in a temporary sibling and renamed into place, so a failed install leaves
/// nothing behind.
#[cfg(feature = "text-detector-install")]
pub fn install_text_detector(target: &Path) -> Result<()> {
    use std::fs::{self, File};
    use std::io::{Read, Write};

    use flate2::read::GzDecoder;
    use sha2::{Digest, Sha256};

    let install = |message: String| Error::TextDetectorInstall(message);
    if target.symlink_metadata().is_ok() {
        return Err(install(format!("{} already exists", target.display())));
    }
    let pin = runtime_pin()
        .ok_or_else(|| install("no ONNX Runtime library is pinned for this platform".to_owned()))?;
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|error| install(format!("cannot create {}: {error}", parent.display())))?;
    let staging = tempfile::Builder::new()
        .prefix(".text-detector-")
        .tempdir_in(parent)
        .map_err(|error| install(format!("cannot create a staging folder: {error}")))?;

    let download = |url: &str, destination: &Path, expected: &str| -> Result<()> {
        let mut response = ureq::get(url)
            .call()
            .map_err(|error| install(format!("download failed for {url}: {error}")))?;
        let mut reader = response.body_mut().as_reader();
        let mut output = File::create(destination)
            .map_err(|error| install(format!("cannot write {}: {error}", destination.display())))?;
        let mut digest = Sha256::new();
        let mut buffer = vec![0_u8; 1 << 20];
        loop {
            let count = reader
                .read(&mut buffer)
                .map_err(|error| install(format!("download read failed for {url}: {error}")))?;
            if count == 0 {
                break;
            }
            output.write_all(&buffer[..count]).map_err(|error| {
                install(format!("cannot write {}: {error}", destination.display()))
            })?;
            digest.update(&buffer[..count]);
        }
        output
            .sync_all()
            .map_err(|error| install(format!("cannot write {}: {error}", destination.display())))?;
        let actual: String = digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if actual != expected {
            return Err(install(format!("checksum mismatch for {url}")));
        }
        Ok(())
    };

    let model_dir = staging.path().join("model");
    let runtime_dir = staging.path().join("runtime");
    for dir in [&model_dir, &runtime_dir] {
        fs::create_dir(dir)
            .map_err(|error| install(format!("cannot create {}: {error}", dir.display())))?;
    }
    download(MODEL_URL, &model_dir.join(MODEL_FILE), MODEL_SHA256)?;
    let archive = staging.path().join("onnxruntime.tgz");
    download(pin.archive_url, &archive, pin.archive_sha256)?;

    // Extract the library, and the license notices that go with it, by file name only.
    let wanted = [pin.library, "LICENSE", "ThirdPartyNotices.txt"];
    let file = File::open(&archive)
        .map_err(|error| install(format!("cannot read the archive: {error}")))?;
    let mut entries = tar::Archive::new(GzDecoder::new(file));
    let mut found = Vec::new();
    for entry in entries
        .entries()
        .map_err(|error| install(format!("cannot read the archive: {error}")))?
    {
        let mut entry =
            entry.map_err(|error| install(format!("cannot read the archive: {error}")))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let name = entry.path().ok().and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        });
        let Some(name) =
            name.filter(|name| wanted.contains(&name.as_str()) && !found.contains(name))
        else {
            continue;
        };
        let destination = runtime_dir.join(&name);
        let mut output = File::create(&destination)
            .map_err(|error| install(format!("cannot write {}: {error}", destination.display())))?;
        std::io::copy(&mut entry, &mut output)
            .and_then(|_| output.sync_all())
            .map_err(|error| install(format!("cannot write {}: {error}", destination.display())))?;
        found.push(name);
    }
    fs::remove_file(&archive)
        .map_err(|error| install(format!("cannot remove the archive: {error}")))?;
    let library = runtime_dir.join(pin.library);
    let mut bytes = Vec::new();
    File::open(&library)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|_| install("the archive has no ONNX Runtime library".to_owned()))?;
    let actual: String = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if actual != pin.library_sha256 {
        return Err(install(
            "checksum mismatch for the ONNX Runtime library".to_owned(),
        ));
    }
    let staged = staging.keep();
    fs::rename(&staged, target).map_err(|error| {
        let _ = fs::remove_dir_all(&staged);
        install(format!(
            "cannot move the detector into {}: {error}",
            target.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_size_is_a_multiple_of_32_and_bounded() {
        assert_eq!(input_size(64, 128), (64, 128));
        assert_eq!(input_size(4096, 3328), (1920, 1568));
        assert_eq!(input_size(10, 10), (32, 32));
    }

    #[test]
    fn a_confident_region_becomes_an_expanded_box_in_frame_pixels() {
        // A 4 x 10 region of probability 0.9 in a 32 x 64 map of a 64 x 128 frame.
        let (height, width) = (32, 64);
        let mut map = vec![0.0_f32; height * width];
        for y in 10..14 {
            for x in 20..30 {
                map[y * width + x] = 0.9;
            }
        }
        let boxes = boxes_from_map(&map, height, width, 64, 128);
        assert_eq!(boxes.len(), 1);
        let text = boxes[0];
        // Offset 4 * 10 * 1.5 / 28 = 2.14 map pixels, times 2 for the frame.
        assert!(text.top <= 16 && text.bottom >= 32, "{text:?}");
        assert!(text.left <= 36 && text.right >= 64, "{text:?}");
    }

    #[test]
    fn weak_or_tiny_regions_are_dropped() {
        let (height, width) = (32, 64);
        let mut map = vec![0.0_f32; height * width];
        // Mean 0.5, below the box threshold.
        for y in 5..10 {
            for x in 5..20 {
                map[y * width + x] = 0.5;
            }
        }
        // Two map pixels high, below the smallest side.
        for y in 20..22 {
            for x in 30..40 {
                map[y * width + x] = 0.95;
            }
        }
        assert!(boxes_from_map(&map, height, width, 64, 128).is_empty());
    }

    #[test]
    fn the_input_is_normalized_per_channel() {
        let view = vec![0_u16, 4095, 0, 4095];
        let (tensor, height, width) = input_tensor(&view, 2, 2, 0, 4095);
        assert_eq!((height, width), (32, 32));
        assert_eq!(tensor.len(), 3 * 32 * 32);
        let first = |channel: usize| tensor[channel * 32 * 32];
        assert!((first(0) - (0.0 - MEAN[0]) / STD[0]).abs() < 0.6);
        assert!(tensor.iter().all(|value| value.is_finite()));
    }
}
