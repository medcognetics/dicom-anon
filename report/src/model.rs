//! OpenAI Privacy Filter model loading and local ONNX inference.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ort::ep::CUDA;
use ort::inputs;
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::TensorRef;
use serde::Deserialize;
use tokenizers::Tokenizer;

use crate::decoder::ViterbiBiases;
use crate::install::{
    verify_pinned_model_file, PRIVACY_FILTER_REPOSITORY, PRIVACY_FILTER_REVISION,
};
use crate::{
    Category, DetectedSpan, DetectorMetadata, Error, PrivacyDetector, Result, ViterbiDecoder,
};

const WINDOW_TOKENS: usize = 4_096;
const CONTEXT_TOKENS: usize = 128;
const WINDOW_STRIDE: usize = WINDOW_TOKENS - 2 * CONTEXT_TOKENS;
const LABEL_COUNT: usize = 33;

/// Default maximum token count accepted by one model invocation.
pub const DEFAULT_MAX_MODEL_TOKENS: usize = 65_536;

static LOADED_RUNTIME: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Model execution device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    /// Quantized CPU execution.
    Cpu,
    /// Quantized execution through the CUDA provider.
    Cuda {
        /// CUDA device index.
        index: u32,
    },
}

impl Device {
    const fn variant_filename(self) -> &'static str {
        "model_q4.onnx"
    }

    const fn variant_name(self) -> &'static str {
        "q4"
    }
}

/// Resolved model, tokenizer, calibration, and native runtime paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelBundle {
    root: PathBuf,
    runtime_library: PathBuf,
    model_path: PathBuf,
    tokenizer_path: PathBuf,
    config_path: PathBuf,
    calibration_path: PathBuf,
    device: Device,
}

impl ModelBundle {
    /// Resolve and validate an official Hugging Face model directory.
    pub fn new(
        root: impl AsRef<Path>,
        runtime_library: impl AsRef<Path>,
        device: Device,
    ) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let runtime_library = runtime_library.as_ref().to_path_buf();
        require_file(&runtime_library)?;
        let model_path = find_file(&root, &["onnx"], device.variant_filename())?;
        let external_data_name = format!("{}_data", device.variant_filename());
        let model_directory = model_path.parent().ok_or_else(|| {
            Error::Model("model path does not have a parent directory".to_owned())
        })?;
        let external_data_path = model_directory.join(external_data_name);
        require_file(&external_data_path)?;
        let tokenizer_path = find_file(&root, &[], "tokenizer.json")?;
        let config_path = find_file(&root, &[], "config.json")?;
        let calibration_path = find_file(&root, &[], "viterbi_calibration.json")?;
        verify_pinned_model_file(&config_path, "config.json")?;
        verify_pinned_model_file(&tokenizer_path, "tokenizer.json")?;
        verify_pinned_model_file(&calibration_path, "viterbi_calibration.json")?;
        verify_pinned_model_file(&model_path, "onnx/model_q4.onnx")?;
        verify_pinned_model_file(&external_data_path, "onnx/model_q4.onnx_data")?;
        Ok(Self {
            root,
            runtime_library,
            model_path,
            tokenizer_path,
            config_path,
            calibration_path,
            device,
        })
    }

    /// Model graph selected for the device.
    #[must_use]
    pub fn model_path(&self) -> &Path {
        &self.model_path
    }

    /// Native ONNX Runtime library.
    #[must_use]
    pub fn runtime_library(&self) -> &Path {
        &self.runtime_library
    }

    /// Root containing the official model artifacts.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Local OpenAI Privacy Filter detector.
pub struct PrivacyFilter {
    tokenizer: Tokenizer,
    decoder: ViterbiDecoder,
    session: Mutex<Session>,
    device: Device,
    max_tokens: usize,
}

impl PrivacyFilter {
    /// Load and validate the pinned model bundle.
    pub fn load(bundle: ModelBundle) -> Result<Self> {
        Self::load_with_max_tokens(bundle, DEFAULT_MAX_MODEL_TOKENS)
    }

    /// Load and validate the pinned model bundle with an explicit token limit.
    pub fn load_with_max_tokens(bundle: ModelBundle, max_tokens: usize) -> Result<Self> {
        validate_token_count(0, max_tokens)?;
        let config: ModelConfig = read_json(&bundle.config_path, "model configuration")?;
        validate_labels(&config.id2label)?;
        let calibration: Calibration = read_json(&bundle.calibration_path, "Viterbi calibration")?;
        let tokenizer = Tokenizer::from_file(&bundle.tokenizer_path)
            .map_err(|error| Error::Model(format!("cannot load tokenizer: {error}")))?;
        initialize_runtime(&bundle.runtime_library)?;

        let mut builder = Session::builder()
            .map_err(model_error)?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(model_error)?;
        if let Device::Cuda { index } = bundle.device {
            let index = i32::try_from(index)
                .map_err(|_| Error::Model("CUDA device index is too large".to_owned()))?;
            builder = builder
                .with_execution_providers([CUDA::default()
                    .with_device_id(index)
                    .build()
                    .error_on_failure()])
                .map_err(model_error)?;
        }
        let session = builder
            .commit_from_file(&bundle.model_path)
            .map_err(model_error)?;
        Ok(Self {
            tokenizer,
            decoder: ViterbiDecoder::with_biases(calibration.operating_points.default.biases),
            session: Mutex::new(session),
            device: bundle.device,
            max_tokens,
        })
    }

    fn infer_window(&self, token_ids: &[u32]) -> Result<Vec<Vec<f32>>> {
        let input_ids = token_ids
            .iter()
            .map(|token| i64::from(*token))
            .collect::<Vec<_>>();
        let attention_mask = vec![1_i64; token_ids.len()];
        let ids =
            TensorRef::from_array_view(([1_i64, token_ids.len() as i64], input_ids.as_slice()))
                .map_err(model_error)?;
        let mask = TensorRef::from_array_view((
            [1_i64, token_ids.len() as i64],
            attention_mask.as_slice(),
        ))
        .map_err(model_error)?;
        let mut session = self
            .session
            .lock()
            .map_err(|_| Error::Model("model session lock is poisoned".to_owned()))?;
        let outputs = session
            .run(inputs!["input_ids" => ids, "attention_mask" => mask])
            .map_err(model_error)?;
        let (shape, flattened) = outputs["logits"]
            .try_extract_tensor::<f32>()
            .map_err(model_error)?;
        let expected_shape = [1_i64, token_ids.len() as i64, LABEL_COUNT as i64];
        if shape.as_ref() != expected_shape {
            return Err(Error::Model(format!(
                "unexpected logits shape {shape:?}; expected {expected_shape:?}"
            )));
        }
        Ok(flattened
            .chunks_exact(LABEL_COUNT)
            .map(<[f32]>::to_vec)
            .collect())
    }

    fn infer_all(&self, token_ids: &[u32]) -> Result<Vec<Vec<f32>>> {
        if token_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut aggregated = vec![None; token_ids.len()];
        let mut start = 0;
        loop {
            let end = (start + WINDOW_TOKENS).min(token_ids.len());
            let logits = self.infer_window(&token_ids[start..end])?;
            let keep_start = if start == 0 {
                start
            } else {
                start + CONTEXT_TOKENS
            };
            let keep_end = if end == token_ids.len() {
                end
            } else {
                end - CONTEXT_TOKENS
            };
            for absolute in keep_start..keep_end {
                aggregated[absolute] = Some(logits[absolute - start].clone());
            }
            if end == token_ids.len() {
                break;
            }
            start += WINDOW_STRIDE;
        }
        aggregated
            .into_iter()
            .enumerate()
            .map(|(index, logits)| {
                logits.ok_or_else(|| {
                    Error::Model(format!("missing aggregated logits for token {index}"))
                })
            })
            .collect()
    }
}

impl PrivacyDetector for PrivacyFilter {
    fn detect(&self, text: &str) -> Result<Vec<DetectedSpan>> {
        let encoding = self
            .tokenizer
            .encode(text, false)
            .map_err(|error| Error::Model(format!("cannot tokenize input: {error}")))?;
        validate_token_count(encoding.len(), self.max_tokens)?;
        let logits = self.infer_all(encoding.get_ids())?;
        let labels = self.decoder.decode(&logits)?;
        labels_to_spans(text, &labels, &logits, encoding.get_offsets())
    }

    fn metadata(&self) -> Option<DetectorMetadata> {
        Some(DetectorMetadata {
            name: PRIVACY_FILTER_REPOSITORY.to_owned(),
            revision: PRIVACY_FILTER_REVISION.to_owned(),
            variant: self.device.variant_name().to_owned(),
            device: match self.device {
                Device::Cpu => "cpu".to_owned(),
                Device::Cuda { index } => format!("cuda:{index}"),
            },
        })
    }
}

#[derive(Deserialize)]
struct ModelConfig {
    id2label: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Calibration {
    operating_points: OperatingPoints,
}

#[derive(Deserialize)]
struct OperatingPoints {
    default: DefaultOperatingPoint,
}

#[derive(Deserialize)]
struct DefaultOperatingPoint {
    biases: ViterbiBiases,
}

fn initialize_runtime(runtime_library: &Path) -> Result<()> {
    let mut loaded = LOADED_RUNTIME
        .lock()
        .map_err(|_| Error::Model("runtime initialization lock is poisoned".to_owned()))?;
    if let Some(existing) = loaded.as_deref() {
        if existing != runtime_library {
            return Err(Error::Model(format!(
                "ONNX Runtime is already loaded from {}; cannot switch to {} in one process",
                existing.display(),
                runtime_library.display()
            )));
        }
        return Ok(());
    }
    let builder = ort::init_from(runtime_library).map_err(model_error)?;
    if !builder.commit() {
        return Err(Error::Model(
            "ONNX Runtime was initialized before the report model loader".to_owned(),
        ));
    }
    *loaded = Some(runtime_library.to_path_buf());
    Ok(())
}

fn validate_labels(labels: &BTreeMap<String, String>) -> Result<()> {
    let expected = expected_labels();
    if labels.len() != expected.len() {
        return Err(Error::Model(format!(
            "model configuration declares {} labels; expected {}",
            labels.len(),
            expected.len()
        )));
    }
    for (index, expected_label) in expected.iter().enumerate() {
        let actual = labels.get(&index.to_string());
        if actual.is_none_or(|label| label != expected_label) {
            return Err(Error::Model(format!(
                "model label {index} does not match the pinned taxonomy"
            )));
        }
    }
    Ok(())
}

fn expected_labels() -> Vec<String> {
    let categories = [
        "account_number",
        "private_address",
        "private_date",
        "private_email",
        "private_person",
        "private_phone",
        "private_url",
        "secret",
    ];
    let mut labels = vec!["O".to_owned()];
    for category in categories {
        for boundary in ["B", "I", "E", "S"] {
            labels.push(format!("{boundary}-{category}"));
        }
    }
    labels
}

fn labels_to_spans(
    text: &str,
    labels: &[usize],
    logits: &[Vec<f32>],
    offsets: &[(usize, usize)],
) -> Result<Vec<DetectedSpan>> {
    if labels.len() != offsets.len() || labels.len() != logits.len() {
        return Err(Error::Model(
            "token labels, logits, and offsets have different lengths".to_owned(),
        ));
    }
    let mut spans = Vec::new();
    let mut token = 0;
    while token < labels.len() {
        let label = labels[token];
        if label == 0 {
            token += 1;
            continue;
        }
        let boundary = (label - 1) % 4;
        let category_index = (label - 1) / 4;
        let token_end = if boundary == 3 {
            token + 1
        } else if boundary == 0 {
            let mut end = token + 1;
            while end < labels.len() {
                let next = labels[end];
                if next > 0 && (next - 1) / 4 == category_index && (next - 1) % 4 == 2 {
                    end += 1;
                    break;
                }
                end += 1;
            }
            end
        } else {
            token += 1;
            continue;
        };
        let (mut start, _) = offsets[token];
        let (_, mut end) = offsets[token_end - 1];
        trim_whitespace(text, &mut start, &mut end);
        if start < end && end <= text.len() {
            let score = (token..token_end)
                .map(|index| label_probability(&logits[index], labels[index]))
                .sum::<f32>()
                / (token_end - token) as f32;
            spans.push(DetectedSpan::new(
                category_from_index(category_index)?,
                start,
                end,
                score.clamp(0.0, 1.0),
            )?);
        }
        token = token_end;
    }
    Ok(spans)
}

fn label_probability(logits: &[f32], label: usize) -> f32 {
    let maximum = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let denominator = logits
        .iter()
        .map(|value| (*value - maximum).exp())
        .sum::<f32>();
    (logits[label] - maximum).exp() / denominator
}

fn category_from_index(index: usize) -> Result<Category> {
    match index {
        0 => Ok(Category::AccountNumber),
        1 => Ok(Category::PrivateAddress),
        2 => Ok(Category::PrivateDate),
        3 => Ok(Category::PrivateEmail),
        4 => Ok(Category::PrivatePerson),
        5 => Ok(Category::PrivatePhone),
        6 => Ok(Category::PrivateUrl),
        7 => Ok(Category::Secret),
        _ => Err(Error::Model(format!(
            "decoded unknown span category {index}"
        ))),
    }
}

fn trim_whitespace(text: &str, start: &mut usize, end: &mut usize) {
    while *start < *end {
        let Some(character) = text[*start..*end].chars().next() else {
            break;
        };
        if !character.is_whitespace() {
            break;
        }
        *start += character.len_utf8();
    }
    while *end > *start {
        let Some(character) = text[*start..*end].chars().next_back() else {
            break;
        };
        if !character.is_whitespace() {
            break;
        }
        *end -= character.len_utf8();
    }
}

fn find_file(root: &Path, subdirectories: &[&str], filename: &str) -> Result<PathBuf> {
    let nested = subdirectories
        .iter()
        .fold(root.to_path_buf(), |path, directory| path.join(directory))
        .join(filename);
    if nested.is_file() {
        return Ok(nested);
    }
    let flat = root.join(filename);
    require_file(&flat)?;
    Ok(flat)
}

fn require_file(path: &Path) -> Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(Error::Model(format!(
            "required model bundle file is missing: {}",
            path.display()
        )))
    }
}

fn read_json<T>(path: &Path, description: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let bytes = fs::read(path).map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|error| Error::Model(format!("invalid {description}: {error}")))
}

fn model_error(error: impl std::fmt::Display) -> Error {
    Error::Model(error.to_string())
}

fn validate_token_count(token_count: usize, limit: usize) -> Result<()> {
    if limit == 0 {
        return Err(Error::Model(
            "model token limit must be greater than zero".to_owned(),
        ));
    }
    if token_count > limit {
        return Err(Error::InputTooManyTokens { limit });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_count_is_bounded_before_inference_allocation() {
        assert!(validate_token_count(65_536, 65_536).is_ok());
        assert!(validate_token_count(65_537, 65_536).is_err());
        assert!(validate_token_count(0, 0).is_err());
    }

    #[test]
    fn pinned_label_taxonomy_is_exact_and_ordered() {
        let labels = expected_labels();
        let mapping = labels
            .iter()
            .enumerate()
            .map(|(index, label)| (index.to_string(), label.clone()))
            .collect();

        assert_eq!(labels.len(), LABEL_COUNT);
        assert_eq!(labels[0], "O");
        assert_eq!(labels[1], "B-account_number");
        assert_eq!(labels[32], "S-secret");
        assert!(validate_labels(&mapping).is_ok());

        let mut missing = mapping.clone();
        missing.remove("32");
        assert!(validate_labels(&missing).is_err());

        let mut changed = mapping;
        changed.insert("17".to_owned(), "B-name".to_owned());
        assert!(validate_labels(&changed).is_err());
    }

    #[test]
    fn model_labels_become_trimmed_utf8_document_spans() {
        let text = " Alice Smith ";
        let labels = [17, 19];
        let logits = confident_logits(&labels);
        let spans = labels_to_spans(text, &labels, &logits, &[(0, 6), (6, 13)]).unwrap();

        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].category, Category::PrivatePerson);
        assert_eq!(&text[spans[0].start..spans[0].end], "Alice Smith");
        assert!(spans[0].score > 0.99);
    }

    #[test]
    fn singleton_and_interior_labels_follow_boundary_rules() {
        let text = "secret ignored";
        let labels = [32, 18];
        let logits = confident_logits(&labels);
        let spans = labels_to_spans(text, &labels, &logits, &[(0, 6), (7, 14)]).unwrap();

        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].category, Category::Secret);
        assert_eq!(&text[spans[0].start..spans[0].end], "secret");
    }

    #[test]
    fn label_conversion_rejects_misaligned_model_outputs() {
        assert!(labels_to_spans("x", &[20], &[], &[(0, 1)]).is_err());
        assert!(labels_to_spans("x", &[20], &confident_logits(&[20]), &[]).is_err());

        let labels = [33];
        let logits = confident_logits(&labels);
        assert!(labels_to_spans("x", &labels, &logits, &[(0, 1)]).is_err());
    }

    #[test]
    fn file_resolution_accepts_nested_and_flat_official_layouts() {
        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("onnx");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("model.onnx"), b"model").unwrap();
        fs::write(directory.path().join("config.json"), br#"{"id2label":{}}"#).unwrap();

        assert_eq!(
            find_file(directory.path(), &["onnx"], "model.onnx").unwrap(),
            nested.join("model.onnx")
        );
        assert_eq!(
            find_file(directory.path(), &["missing"], "config.json").unwrap(),
            directory.path().join("config.json")
        );
        assert!(find_file(directory.path(), &[], "absent.json").is_err());
        assert!(read_json::<ModelConfig>(&directory.path().join("config.json"), "config").is_ok());

        fs::write(directory.path().join("invalid.json"), b"not json").unwrap();
        assert!(
            read_json::<ModelConfig>(&directory.path().join("invalid.json"), "config").is_err()
        );
    }

    #[test]
    fn all_model_categories_have_stable_indices() {
        let expected = [
            Category::AccountNumber,
            Category::PrivateAddress,
            Category::PrivateDate,
            Category::PrivateEmail,
            Category::PrivatePerson,
            Category::PrivatePhone,
            Category::PrivateUrl,
            Category::Secret,
        ];

        for (index, category) in expected.into_iter().enumerate() {
            assert_eq!(category_from_index(index).unwrap(), category);
        }
        assert!(category_from_index(expected.len()).is_err());
    }

    fn confident_logits(labels: &[usize]) -> Vec<Vec<f32>> {
        labels
            .iter()
            .map(|label| {
                let mut logits = vec![0.0; LABEL_COUNT.max(*label + 1)];
                logits[*label] = 20.0;
                logits
            })
            .collect()
    }
}
