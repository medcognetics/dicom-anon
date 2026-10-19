//! Unix-style local report anonymization command.

use std::fs::{self, File};
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use dicom_anon::cli_common::{load_key, ColorMode, ProfileArgs, ProfilePreset, Styles};
use dicom_anon::Options;
use dicom_anon_report::{
    install_assets, AssetBackend, AssetInstallOptions, Device, DicomLinkage,
    InstalledAssetManifest, ModelBundle, PrivacyFilter, ReportAnonymizer, ReportOptions,
    DEFAULT_MAX_MODEL_TOKENS, ONNX_RUNTIME_VERSION, PRIVACY_FILTER_REPOSITORY,
    PRIVACY_FILTER_REVISION,
};
#[cfg(feature = "pdf")]
use dicom_anon_report::{PdfAnonymizer, PdfOptions, PdfTextMode};
use indicatif::{ProgressBar, ProgressStyle};

const DEFAULT_MAX_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MODEL_METADATA_FILENAMES: [&str; 3] =
    ["config.json", "tokenizer.json", "viterbi_calibration.json"];
const MODEL_GRAPH_FILENAMES: [&str; 2] = ["model_q4.onnx", "model_q4.onnx_data"];
const MODEL_PACKAGE_FILENAMES: [&str; 4] = [
    "README.md",
    "MODEL_LICENSE-APACHE",
    "MODEL_NOTICE.txt",
    "asset-manifest.json",
];
const ASSET_MANIFEST_SCHEMA_VERSION: u32 = 1;
#[cfg(feature = "pdf")]
const DEFAULT_PDF_DPI: u16 = 200;
#[cfg(feature = "pdf")]
const DEFAULT_MAX_PDF_PAGES: u32 = 500;
#[cfg(feature = "pdf")]
const DEFAULT_MAX_RENDER_PIXELS: u64 = 100_000_000;
#[cfg(feature = "pdf")]
const DEFAULT_MAX_TOTAL_RENDER_PIXELS: u64 = 250_000_000;
const PDF_HEADER_SEARCH_BYTES: usize = 1_024;

#[derive(Debug, Parser)]
#[command(
    name = "report-anon",
    version = dicom_anon::cli_common::LONG_VERSION,
    about = "Filter private information from local text and medical reports"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Anonymize plaintext or PDF data from a file or stdin.
    Anonymize(Box<AnonymizeArgs>),
    /// Install or inspect pinned privacy-model assets.
    Model(ModelArgs),
}

#[derive(Debug, Args)]
struct ModelArgs {
    #[command(subcommand)]
    command: ModelCommand,
}

#[derive(Debug, Subcommand)]
enum ModelCommand {
    /// Download and checksum-verify a model, ONNX Runtime, and optional PDFium bundle.
    Install(ModelInstallArgs),
    /// Print the default model bundle path.
    Path,
}

#[derive(Debug, Args)]
struct ModelInstallArgs {
    /// Final bundle directory. The installer never overwrites an existing path.
    #[arg(long, value_name = "PATH")]
    target: Option<PathBuf>,
    /// Native inference backend to package.
    #[arg(long, value_enum, default_value_t = AssetBackend::Cpu)]
    backend: AssetBackend,
    /// Do not install PDFium. Plaintext operation remains available.
    #[arg(long)]
    without_pdfium: bool,
    /// Suppress the installed-path result.
    #[arg(short, long)]
    quiet: bool,
    /// Download progress policy.
    #[arg(long, value_enum, default_value_t)]
    progress: ProgressMode,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Exit status: 0 success, 2 usage or runtime error. Input defaults to stdin; anonymized output defaults to stdout."
)]
struct AnonymizeArgs {
    #[command(flatten)]
    profile: ProfileArgs,
    /// Input UTF-8 text or PDF file, or '-' for stdin. Defaults to stdin.
    #[arg(value_name = "INPUT", default_value = "-")]
    input: String,
    /// Input type. Auto recognizes PDF headers within the first 1,024 bytes and .pdf files.
    #[arg(long, value_enum, default_value_t)]
    input_format: InputFormat,
    /// Output file, or '-' for stdout. Defaults to stdout.
    #[arg(value_name = "OUTPUT", conflicts_with = "in_place")]
    output: Option<String>,
    /// Atomically replace the input file.
    #[arg(long, conflicts_with = "output")]
    in_place: bool,
    /// Directory containing the pinned model, tokenizer, and calibration files.
    #[arg(long, value_name = "PATH", env = "DICOM_ANON_REPORT_MODEL_DIR")]
    model_dir: Option<PathBuf>,
    /// Exact libonnxruntime shared-library path; requires an explicit --device.
    #[arg(long, value_name = "PATH", env = "DICOM_ANON_REPORT_ORT_LIBRARY")]
    runtime_library: Option<PathBuf>,
    /// Exact PDFium shared-library path. Defaults to the packaged runtime for PDF input.
    #[cfg(feature = "pdf")]
    #[arg(long, value_name = "PATH", env = "DICOM_ANON_REPORT_PDFIUM_LIBRARY")]
    pdfium_library: Option<PathBuf>,
    /// PDF text localization: auto, embedded, or ocr.
    #[cfg(feature = "pdf")]
    #[arg(long, value_enum, default_value_t)]
    pdf_text_mode: PdfTextMode,
    /// PDF rasterization and OCR resolution in dots per inch.
    #[cfg(feature = "pdf")]
    #[arg(long, default_value_t = DEFAULT_PDF_DPI)]
    pdf_dpi: u16,
    /// Tesseract language used for scanned PDF pages.
    #[cfg(feature = "pdf")]
    #[arg(long, default_value = "eng")]
    ocr_language: String,
    /// Maximum accepted PDF page count.
    #[cfg(feature = "pdf")]
    #[arg(long, default_value_t = DEFAULT_MAX_PDF_PAGES)]
    max_pdf_pages: u32,
    /// Maximum rendered pixels accepted for any PDF page.
    #[cfg(feature = "pdf")]
    #[arg(long, default_value_t = DEFAULT_MAX_RENDER_PIXELS)]
    max_render_pixels: u64,
    /// Maximum rendered pixels accepted across the complete PDF.
    #[cfg(feature = "pdf")]
    #[arg(long, default_value_t = DEFAULT_MAX_TOTAL_RENDER_PIXELS)]
    max_total_render_pixels: u64,
    /// Execution device: auto, cpu, cuda, or cuda:N.
    #[arg(long, default_value = "auto")]
    device: DeviceArg,
    /// Maximum accepted input bytes.
    #[arg(long, default_value_t = DEFAULT_MAX_TEXT_BYTES)]
    max_input_bytes: usize,
    /// Maximum tokenizer output accepted before model inference.
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_MODEL_TOKENS,
        value_parser = parse_positive_usize
    )]
    max_input_tokens: usize,
    /// Write the safe JSON anonymization report to a file.
    #[arg(long, value_name = "PATH")]
    report: Option<PathBuf>,
    /// Source DICOM used to reassociate matching patient, study, and temporal values; repeat as needed.
    #[arg(long, value_name = "PATH", action = ArgAction::Append)]
    dicom: Vec<PathBuf>,
    /// Write the safe report-to-DICOM association manifest to this file.
    /// Required for linked stdout output; defaults to OUTPUT.association.json for file output.
    #[arg(long, value_name = "PATH", requires = "dicom")]
    association: Option<PathBuf>,
    /// File containing at least 32 bytes of DICOM pseudonymization key material.
    #[arg(long, value_name = "PATH", requires = "dicom")]
    key_file: Option<PathBuf>,
    /// Environment variable containing key material when --key-file is absent.
    #[arg(
        long,
        value_name = "NAME",
        default_value = "DICOM_ANON_KEY",
        requires = "dicom"
    )]
    key_env: String,
    /// Stable scope for DICOM inputs with no usable Patient ID or Patient Name.
    #[arg(long, value_name = "SCOPE", requires = "dicom")]
    patient_scope: Option<String>,
    /// Numeric UID root used by the corresponding DICOM anonymization pass.
    #[arg(long, default_value = "2.25", requires = "dicom")]
    uid_root: String,
    /// Permit linkage from input marked Patient Identity Removed=YES.
    #[arg(long, requires = "dicom")]
    allow_reanonymize: bool,
    /// Suppress non-error diagnostics.
    #[arg(short, long, conflicts_with = "verbose")]
    quiet: bool,
    /// Print the safe JSON anonymization report to stderr.
    #[arg(short, long, conflicts_with = "quiet")]
    verbose: bool,
    /// Color policy for diagnostics.
    #[arg(long, value_enum, default_value_t)]
    color: ColorMode,
    /// Disable color, overriding --color.
    #[arg(long, action = ArgAction::SetTrue)]
    no_color: bool,
    /// Progress policy for model loading and document processing.
    #[arg(long, value_enum, default_value_t)]
    progress: ProgressMode,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum ProgressMode {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum InputFormat {
    #[default]
    Auto,
    Text,
    Pdf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocumentKind {
    Text,
    Pdf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceArg {
    Auto,
    Cpu,
    Cuda(u32),
}

impl FromStr for DeviceArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "auto" => Ok(Self::Auto),
            "cpu" => Ok(Self::Cpu),
            "cuda" => Ok(Self::Cuda(0)),
            _ => {
                let Some(index) = value.strip_prefix("cuda:") else {
                    return Err("expected auto, cpu, cuda, or cuda:N".to_owned());
                };
                let index = index
                    .parse::<u32>()
                    .map_err(|_| "CUDA device index must be a non-negative integer".to_owned())?;
                Ok(Self::Cuda(index))
            }
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Anonymize(args) => match anonymize(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let styles =
                    Styles::new(args.color, args.no_color, false, io::stderr().is_terminal());
                eprintln!("{}: {error}", styles.failure("report-anon failed"));
                ExitCode::from(2)
            }
        },
        Command::Model(args) => match model(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("report-anon failed: {error}");
                ExitCode::from(2)
            }
        },
    }
}

fn model(args: ModelArgs) -> Result<(), String> {
    match args.command {
        ModelCommand::Path => {
            println!("{}", default_model_dir().display());
            Ok(())
        }
        ModelCommand::Install(args) => {
            let target = args.target.unwrap_or_else(default_model_dir);
            install_assets(&AssetInstallOptions {
                target: target.clone(),
                backend: args.backend,
                include_pdfium: !args.without_pdfium,
                progress: progress_enabled(args.progress, args.quiet),
            })
            .map_err(|error| error.to_string())?;
            if !args.quiet {
                println!("{}", target.display());
            }
            Ok(())
        }
    }
}

fn anonymize(args: &AnonymizeArgs) -> Result<(), String> {
    validate_dicom_options(args)?;
    let input_path = (args.input != "-").then(|| Path::new(&args.input));
    if args.in_place && input_path.is_none() {
        return Err("--in-place requires a file input".to_owned());
    }
    if args.in_place
        && input_path.is_some_and(|path| {
            fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
        })
    {
        return Err("--in-place does not accept a symbolic-link input".to_owned());
    }
    let output = if args.in_place {
        args.input.as_str()
    } else {
        args.output.as_deref().unwrap_or("-")
    };
    let association_path = association_path(args, output)?;
    let model_dir = args.model_dir.clone().unwrap_or_else(default_model_dir);
    let (device, runtime_library) =
        resolve_device_and_runtime(&model_dir, args.runtime_library.as_deref(), args.device)?;
    #[cfg(feature = "pdf")]
    let pdfium_library = resolve_pdfium_library(&model_dir, args.pdfium_library.as_deref());
    #[cfg(feature = "pdf")]
    let native_runtime_paths = vec![runtime_library.as_path(), pdfium_library.as_path()];
    #[cfg(not(feature = "pdf"))]
    let native_runtime_paths = vec![runtime_library.as_path()];
    validate_output_paths(
        args,
        input_path,
        output,
        association_path.as_deref(),
        &model_asset_paths(&model_dir)?,
        &native_runtime_paths,
    )?;

    let progress = OperationProgress::new(args.progress, args.quiet);
    progress.set_message("Reading input");
    let input = read_input(&args.input, args.max_input_bytes)?;
    let document_kind = classify_input(args.input_format, input_path, &input)?;
    #[cfg(not(feature = "pdf"))]
    if document_kind == DocumentKind::Pdf {
        return Err(
            "PDF input requires a report-anon build with the default `pdf` feature".to_owned(),
        );
    }
    let bundle =
        ModelBundle::new(&model_dir, runtime_library, device).map_err(|error| error.to_string())?;
    progress.set_message("Loading privacy model");
    let detector = PrivacyFilter::load_with_max_tokens(bundle, args.max_input_tokens)
        .map_err(|error| error.to_string())?;
    progress.set_message("Anonymizing document");
    let mut linkage = load_linkage(args)?;
    let report_options = ReportOptions {
        max_text_bytes: args.max_input_bytes,
    };
    let (anonymized_bytes, report_json) = match document_kind {
        #[cfg(feature = "pdf")]
        DocumentKind::Pdf => {
            let anonymizer = PdfAnonymizer::new(
                detector,
                report_options,
                PdfOptions {
                    pdfium_library,
                    dpi: args.pdf_dpi,
                    text_mode: args.pdf_text_mode,
                    ocr_language: args.ocr_language.clone(),
                    max_pages: args.max_pdf_pages,
                    max_render_pixels_per_page: args.max_render_pixels,
                    max_total_render_pixels: args.max_total_render_pixels,
                },
            );
            let result = if let Some(linkage) = &linkage {
                anonymizer.anonymize_pdf_with_linkage(&input, linkage.sensitive_map())
            } else {
                anonymizer.anonymize_pdf(&input)
            }
            .map_err(|error| error.to_string())?;
            let report_json = serde_json::to_vec_pretty(&result.report)
                .map_err(|error| format!("cannot serialize anonymization report: {error}"))?;
            (result.bytes, report_json)
        }
        #[cfg(not(feature = "pdf"))]
        DocumentKind::Pdf => unreachable!("PDF input was rejected before model loading"),
        DocumentKind::Text => {
            let anonymizer = ReportAnonymizer::new(detector, report_options);
            let result = if let Some(linkage) = &linkage {
                anonymizer.anonymize_text_with_linkage(&input, linkage.sensitive_map())
            } else {
                anonymizer.anonymize_text(&input)
            }
            .map_err(|error| error.to_string())?;
            let report_json = serde_json::to_vec_pretty(&result.report)
                .map_err(|error| format!("cannot serialize anonymization report: {error}"))?;
            (result.bytes, report_json)
        }
    };
    let association_json = if let Some(linkage) = &mut linkage {
        linkage.manifest_mut().bind_output(&anonymized_bytes);
        Some(
            serde_json::to_vec_pretty(linkage.manifest())
                .map_err(|error| format!("cannot serialize association manifest: {error}"))?,
        )
    } else {
        None
    };

    progress.set_message("Writing output");
    persist_outputs(
        || {
            if output == "-" {
                let mut stdout = io::stdout().lock();
                stdout
                    .write_all(&anonymized_bytes)
                    .map_err(|error| format!("cannot write stdout: {error}"))?;
                stdout
                    .flush()
                    .map_err(|error| format!("cannot flush stdout: {error}"))
            } else {
                let permissions = input_path
                    .filter(|path| *path == Path::new(output))
                    .and_then(|path| fs::metadata(path).ok())
                    .map(|metadata| metadata.permissions());
                write_atomic(Path::new(output), &anonymized_bytes, permissions)
            }
        },
        args.report
            .as_deref()
            .map(|path| (path, report_json.as_slice())),
        association_path.as_deref().zip(association_json.as_deref()),
    )?;
    if args.verbose && !args.quiet {
        eprintln!("{}", String::from_utf8_lossy(&report_json));
    }
    progress.finish();
    Ok(())
}

struct OperationProgress(ProgressBar);

impl OperationProgress {
    fn new(mode: ProgressMode, quiet: bool) -> Self {
        if !progress_enabled(mode, quiet) {
            return Self(ProgressBar::hidden());
        }
        let bar = ProgressBar::new_spinner();
        bar.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg}")
                .expect("the static progress template is valid"),
        );
        bar.enable_steady_tick(std::time::Duration::from_millis(80));
        Self(bar)
    }

    fn set_message(&self, message: &'static str) {
        self.0.set_message(message);
    }

    fn finish(&self) {
        self.0.finish_and_clear();
    }
}

impl Drop for OperationProgress {
    fn drop(&mut self) {
        self.0.finish_and_clear();
    }
}

fn progress_enabled(mode: ProgressMode, quiet: bool) -> bool {
    !quiet
        && match mode {
            ProgressMode::Auto => io::stderr().is_terminal(),
            ProgressMode::Always => true,
            ProgressMode::Never => false,
        }
}

fn association_path(args: &AnonymizeArgs, output: &str) -> Result<Option<PathBuf>, String> {
    if args.dicom.is_empty() {
        return Ok(None);
    }
    if let Some(path) = &args.association {
        return Ok(Some(path.clone()));
    }
    if output == "-" {
        return Err("--association is required when linked output is written to stdout".to_owned());
    }
    Ok(Some(PathBuf::from(format!("{output}.association.json"))))
}

fn validate_output_paths(
    args: &AnonymizeArgs,
    input_path: Option<&Path>,
    output: &str,
    association_path: Option<&Path>,
    model_asset_paths: &[PathBuf],
    native_runtime_paths: &[&Path],
) -> Result<(), String> {
    let auxiliary_input_paths = args
        .dicom
        .iter()
        .map(PathBuf::as_path)
        .chain(args.key_file.as_deref())
        .chain(args.profile.descriptor_map.iter().map(PathBuf::as_path))
        .chain(args.profile.safe_private_allowlist.as_deref())
        .chain(args.profile.uid_map.as_deref())
        .collect::<Vec<_>>();
    let protected_model_paths = model_asset_paths
        .iter()
        .map(PathBuf::as_path)
        .chain(native_runtime_paths.iter().copied())
        .collect::<Vec<_>>();

    if output != "-" {
        let output_path = Path::new(output);
        reject_model_asset_alias(output_path, "output", &protected_model_paths)?;
        if let Some(input_path) = input_path {
            if same_path(output_path, input_path)? && !args.in_place {
                return Err("use --in-place to replace the input file".to_owned());
            }
        }
        reject_input_alias(output_path, "output", &auxiliary_input_paths)?;
    }

    if let Some(report_path) = args.report.as_deref() {
        if output != "-" && same_path(report_path, Path::new(output))? {
            return Err("--report must not overwrite the anonymized document".to_owned());
        }
        reject_input_alias(
            report_path,
            "--report",
            &input_path
                .into_iter()
                .chain(auxiliary_input_paths.iter().copied())
                .collect::<Vec<_>>(),
        )?;
        reject_model_asset_alias(report_path, "--report", &protected_model_paths)?;
    }
    if let Some(path) = association_path {
        if output != "-" && same_path(path, Path::new(output))? {
            return Err("--association must not overwrite the anonymized document".to_owned());
        }
        if let Some(report) = args.report.as_deref() {
            if same_path(report, path)? {
                return Err("--association and --report must use different files".to_owned());
            }
        }
        reject_input_alias(
            path,
            "--association",
            &input_path
                .into_iter()
                .chain(auxiliary_input_paths.iter().copied())
                .collect::<Vec<_>>(),
        )?;
        reject_model_asset_alias(path, "--association", &protected_model_paths)?;
    }
    Ok(())
}

fn reject_input_alias(destination: &Path, label: &str, sources: &[&Path]) -> Result<(), String> {
    for source in sources {
        if same_path(destination, source)? {
            return Err(format!("{label} must not overwrite an input file"));
        }
    }
    Ok(())
}

fn reject_model_asset_alias(
    destination: &Path,
    label: &str,
    protected_paths: &[&Path],
) -> Result<(), String> {
    for protected_path in protected_paths {
        if same_path(destination, protected_path)? {
            return Err(format!("{label} must not overwrite a model asset"));
        }
    }
    Ok(())
}

fn model_asset_paths(model_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = MODEL_METADATA_FILENAMES
        .iter()
        .map(|filename| model_dir.join(filename))
        .chain(MODEL_GRAPH_FILENAMES.iter().flat_map(|filename| {
            [
                model_dir.join(filename),
                model_dir.join("onnx").join(filename),
            ]
        }))
        .chain(
            MODEL_PACKAGE_FILENAMES
                .iter()
                .map(|filename| model_dir.join(filename)),
        )
        .collect::<Vec<_>>();
    let manifest_path = model_dir.join("asset-manifest.json");
    if !manifest_path.is_file() {
        return Ok(paths);
    }
    let bytes = fs::read(&manifest_path).map_err(|error| {
        format!(
            "cannot read installed asset manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    let manifest: InstalledAssetManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("cannot parse installed asset manifest: {error}"))?;
    if manifest.schema_version != ASSET_MANIFEST_SCHEMA_VERSION
        || manifest.model_repository != PRIVACY_FILTER_REPOSITORY
        || manifest.model_revision != PRIVACY_FILTER_REVISION
        || manifest.onnx_runtime_version != ONNX_RUNTIME_VERSION
    {
        return Err("installed asset manifest does not match the pinned bundle".to_owned());
    }
    for file in manifest.files {
        let relative = Path::new(&file.path);
        if relative.as_os_str().is_empty()
            || !relative
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            return Err("installed asset manifest contains an invalid file path".to_owned());
        }
        paths.push(model_dir.join(relative));
    }
    Ok(paths)
}

fn same_path(left: &Path, right: &Path) -> Result<bool, String> {
    Ok(normalized_path(left)? == normalized_path(right)?)
}

fn normalized_path(path: &Path) -> Result<PathBuf, String> {
    if let Ok(path) = path.canonicalize() {
        return Ok(path);
    }
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("cannot resolve current directory: {error}"))?
            .join(path)
    };
    let Some(file_name) = absolute.file_name() else {
        return Ok(absolute);
    };
    let parent = absolute.parent().unwrap_or_else(|| Path::new("."));
    Ok(parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_owned())
        .join(file_name))
}

fn validate_dicom_options(args: &AnonymizeArgs) -> Result<(), String> {
    if !args.dicom.is_empty() {
        return Ok(());
    }
    // Destructure every field so that a new profile flag must be classified here.
    let ProfileArgs {
        policy,
        enable_option,
        disable_option,
        descriptor_map,
        fail_on_unmapped,
        safe_private_allowlist,
        uid_map,
        require_no_burned_in_annotation,
        allow_sop_class,
    } = &args.profile;
    let option = if !matches!(policy, ProfilePreset::DicomExtended) {
        Some("--policy")
    } else if !enable_option.is_empty() {
        Some("--enable-option")
    } else if !disable_option.is_empty() {
        Some("--disable-option")
    } else if !descriptor_map.is_empty() {
        Some("--descriptor-map")
    } else if !fail_on_unmapped.is_empty() {
        Some("--fail-on-unmapped")
    } else if safe_private_allowlist.is_some() {
        Some("--safe-private-allowlist")
    } else if uid_map.is_some() {
        Some("--uid-map")
    } else if *require_no_burned_in_annotation {
        Some("--require-no-burned-in-annotation")
    } else if !allow_sop_class.is_empty() {
        Some("--allow-sop-class")
    } else {
        None
    };
    if let Some(option) = option {
        return Err(format!("{option} requires at least one --dicom input"));
    }
    Ok(())
}

fn load_linkage(args: &AnonymizeArgs) -> Result<Option<DicomLinkage>, String> {
    if args.dicom.is_empty() {
        return Ok(None);
    }
    let secret = load_key(args.key_file.clone(), &args.key_env)?;
    let mut options = Options::new(secret);
    options.profile = args.profile.load()?;
    options.patient_scope.clone_from(&args.patient_scope);
    options.uid_root.clone_from(&args.uid_root);
    options.reject_already_deidentified = !args.allow_reanonymize;
    let inputs = args
        .dicom
        .iter()
        .map(|path| {
            fs::read(path).map_err(|error| format!("cannot read DICOM {}: {error}", path.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    DicomLinkage::from_dicom_bytes(&inputs, options)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn read_input(input: &str, limit: usize) -> Result<Vec<u8>, String> {
    let reader: Box<dyn Read> = if input == "-" {
        Box::new(io::stdin().lock())
    } else {
        Box::new(File::open(input).map_err(|error| format!("cannot open {input}: {error}"))?)
    };
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {input}: {error}"))?;
    if bytes.len() > limit {
        return Err(format!("input exceeds the configured {limit}-byte limit"));
    }
    Ok(bytes)
}

fn classify_input(
    requested: InputFormat,
    input_path: Option<&Path>,
    input: &[u8],
) -> Result<DocumentKind, String> {
    let prefix = &input[..input.len().min(PDF_HEADER_SEARCH_BYTES)];
    let has_pdf_header = prefix
        .windows(b"%PDF-".len())
        .any(|value| value == b"%PDF-");
    let has_pdf_extension = input_path
        .and_then(Path::extension)
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"));
    let appears_to_be_pdf = has_pdf_header || has_pdf_extension;

    match requested {
        InputFormat::Auto => Ok(if appears_to_be_pdf {
            DocumentKind::Pdf
        } else {
            DocumentKind::Text
        }),
        InputFormat::Pdf => Ok(DocumentKind::Pdf),
        InputFormat::Text => Ok(DocumentKind::Text),
    }
}

fn parse_positive_usize(value: &str) -> Result<usize, String> {
    let value = value
        .parse::<usize>()
        .map_err(|_| "expected a positive integer".to_owned())?;
    if value == 0 {
        return Err("value must be greater than zero".to_owned());
    }
    Ok(value)
}

fn resolve_device_and_runtime(
    model_dir: &Path,
    explicit_runtime: Option<&Path>,
    requested: DeviceArg,
) -> Result<(Device, PathBuf), String> {
    if let Some(runtime) = explicit_runtime {
        let device = match requested {
            DeviceArg::Auto => {
                return Err(
                    "--runtime-library requires an explicit --device cpu, cuda, or cuda:N"
                        .to_owned(),
                )
            }
            DeviceArg::Cpu => Device::Cpu,
            DeviceArg::Cuda(index) => Device::Cuda { index },
        };
        return Ok((device, runtime.to_path_buf()));
    }

    let cpu = runtime_candidate(model_dir, "cpu");
    let cuda13 = runtime_candidate(model_dir, "cuda13");
    let cuda12 = runtime_candidate(model_dir, "cuda12");
    match requested {
        DeviceArg::Cpu => Ok((Device::Cpu, cpu)),
        DeviceArg::Cuda(index) => [cuda13, cuda12]
            .into_iter()
            .find(|path| path.is_file())
            .map(|path| (Device::Cuda { index }, path))
            .ok_or_else(|| {
                "no installed CUDA 12 or CUDA 13 ONNX Runtime bundle was found".to_owned()
            }),
        DeviceArg::Auto => {
            let has_cuda_model = model_dir.join("onnx/model_q4.onnx_data").is_file()
                || model_dir.join("model_q4.onnx_data").is_file();
            if has_cuda_model {
                if let Some(runtime) = [cuda13, cuda12].into_iter().find(|path| path.is_file()) {
                    return Ok((Device::Cuda { index: 0 }, runtime));
                }
            }
            Ok((Device::Cpu, cpu))
        }
    }
}

fn runtime_candidate(model_dir: &Path, backend: &str) -> PathBuf {
    let nested = model_dir
        .join("runtime")
        .join(backend)
        .join("lib")
        .join(format!("libonnxruntime.so.{ONNX_RUNTIME_VERSION}"));
    if nested.is_file() {
        nested
    } else {
        model_dir
            .join("lib")
            .join(format!("libonnxruntime.so.{ONNX_RUNTIME_VERSION}"))
    }
}

#[cfg(feature = "pdf")]
fn resolve_pdfium_library(model_dir: &Path, explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_owned();
    }
    let packaged = model_dir
        .join("runtime")
        .join("pdfium")
        .join("lib")
        .join(PdfiumLibraryName::platform());
    if packaged.is_file() {
        packaged
    } else {
        PathBuf::from(PdfiumLibraryName::platform())
    }
}

#[cfg(feature = "pdf")]
struct PdfiumLibraryName;

#[cfg(feature = "pdf")]
impl PdfiumLibraryName {
    #[cfg(target_os = "windows")]
    const fn platform() -> &'static str {
        "pdfium.dll"
    }

    #[cfg(target_os = "macos")]
    const fn platform() -> &'static str {
        "libpdfium.dylib"
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    const fn platform() -> &'static str {
        "libpdfium.so"
    }
}

fn default_model_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".local/share"))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("dicom-anon")
        .join("models")
        .join(PRIVACY_FILTER_REVISION)
}

fn write_atomic(
    output: &Path,
    bytes: &[u8],
    permissions: Option<fs::Permissions>,
) -> Result<(), String> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        format!(
            "cannot create temporary output in {}: {error}",
            parent.display()
        )
    })?;
    if let Some(permissions) = permissions {
        temporary
            .as_file()
            .set_permissions(permissions)
            .map_err(|error| format!("cannot preserve output permissions: {error}"))?;
    }
    temporary
        .write_all(bytes)
        .map_err(|error| format!("cannot write temporary output: {error}"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("cannot sync temporary output: {error}"))?;
    temporary
        .persist(output)
        .map_err(|error| format!("cannot replace {}: {}", output.display(), error.error))?;
    Ok(())
}

fn persist_outputs(
    write_primary: impl FnOnce() -> Result<(), String>,
    report: Option<(&Path, &[u8])>,
    association: Option<(&Path, &[u8])>,
) -> Result<(), String> {
    write_primary()?;
    if let Some((path, bytes)) = report {
        write_atomic(path, bytes, None)?;
    }
    if let Some((path, bytes)) = association {
        write_atomic(path, bytes, None)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_input_format_recognizes_a_pdf_header_after_leading_bytes() {
        let input = b"% leading transport comment\n%PDF-1.7\n";

        assert_eq!(
            classify_input(InputFormat::Auto, None, input).unwrap(),
            DocumentKind::Pdf
        );
    }

    #[test]
    fn explicit_input_formats_override_automatic_pdf_classification() {
        assert_eq!(
            classify_input(InputFormat::Pdf, None, b"plain text").unwrap(),
            DocumentKind::Pdf
        );
        assert_eq!(
            classify_input(InputFormat::Text, None, b"plain text").unwrap(),
            DocumentKind::Text
        );
        assert_eq!(
            classify_input(InputFormat::Text, None, b"%PDF-1.7").unwrap(),
            DocumentKind::Text
        );
        assert_eq!(
            classify_input(
                InputFormat::Text,
                Some(Path::new("report.pdf")),
                b"plain text"
            )
            .unwrap(),
            DocumentKind::Text
        );
        assert_eq!(
            classify_input(InputFormat::Auto, Some(Path::new("REPORT.PDF")), b"").unwrap(),
            DocumentKind::Pdf
        );
    }

    #[test]
    fn device_argument_accepts_documented_cpu_and_cuda_forms() {
        assert_eq!(DeviceArg::from_str("auto").unwrap(), DeviceArg::Auto);
        assert_eq!(DeviceArg::from_str("cpu").unwrap(), DeviceArg::Cpu);
        assert_eq!(DeviceArg::from_str("cuda").unwrap(), DeviceArg::Cuda(0));
        assert_eq!(DeviceArg::from_str("cuda:2").unwrap(), DeviceArg::Cuda(2));
        assert!(DeviceArg::from_str("cuda:-1").is_err());
        assert!(DeviceArg::from_str("gpu").is_err());
    }

    #[test]
    fn progress_policy_respects_quiet_and_explicit_modes() {
        assert!(!progress_enabled(ProgressMode::Always, true));
        assert!(progress_enabled(ProgressMode::Always, false));
        assert!(!progress_enabled(ProgressMode::Never, false));
    }

    #[test]
    fn token_limit_parser_requires_a_positive_integer() {
        assert_eq!(parse_positive_usize("42").unwrap(), 42);
        assert!(parse_positive_usize("0").is_err());
        assert!(parse_positive_usize("invalid").is_err());
    }

    #[test]
    fn runtime_resolution_prefers_installed_cuda_then_cpu() {
        let directory = tempfile::tempdir().unwrap();
        let cuda = directory
            .path()
            .join("runtime/cuda13/lib")
            .join(format!("libonnxruntime.so.{ONNX_RUNTIME_VERSION}"));
        fs::create_dir_all(cuda.parent().unwrap()).unwrap();
        fs::write(&cuda, b"runtime").unwrap();
        fs::create_dir_all(directory.path().join("onnx")).unwrap();
        fs::write(directory.path().join("onnx/model_q4.onnx_data"), b"weights").unwrap();

        assert_eq!(
            resolve_device_and_runtime(directory.path(), None, DeviceArg::Auto).unwrap(),
            (Device::Cuda { index: 0 }, cuda.clone())
        );
        assert_eq!(
            resolve_device_and_runtime(directory.path(), Some(&cuda), DeviceArg::Cpu).unwrap(),
            (Device::Cpu, cuda)
        );

        let empty = tempfile::tempdir().unwrap();
        assert!(resolve_device_and_runtime(empty.path(), None, DeviceArg::Cuda(0)).is_err());
        assert_eq!(
            resolve_device_and_runtime(empty.path(), None, DeviceArg::Cpu)
                .unwrap()
                .0,
            Device::Cpu
        );
    }

    #[test]
    fn explicit_runtime_requires_an_explicit_device() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = directory.path().join("runtime.so");

        let error = resolve_device_and_runtime(directory.path(), Some(&runtime), DeviceArg::Auto)
            .unwrap_err();

        assert!(error.contains("--runtime-library requires an explicit --device"));
    }

    #[test]
    fn association_destination_follows_output_contract() {
        let file_args = parsed_anonymize_args(&["input.txt", "output.txt", "--dicom", "x.dcm"]);
        assert_eq!(
            association_path(&file_args, "output.txt").unwrap(),
            Some(PathBuf::from("output.txt.association.json"))
        );

        let pipe_args = parsed_anonymize_args(&["--dicom", "x.dcm"]);
        assert!(association_path(&pipe_args, "-").is_err());

        let plain_args = parsed_anonymize_args(&[]);
        assert_eq!(association_path(&plain_args, "-").unwrap(), None);
    }

    #[test]
    fn atomic_writer_replaces_complete_file_and_preserves_requested_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("output.txt");
        fs::write(&output, b"old").unwrap();
        let permissions = fs::metadata(&output).unwrap().permissions();

        write_atomic(&output, b"new content", Some(permissions)).unwrap();

        assert_eq!(fs::read(output).unwrap(), b"new content");
    }

    #[test]
    fn primary_output_failure_prevents_sidecar_persistence() {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("report.json");
        let association = directory.path().join("association.json");

        let error = persist_outputs(
            || Err("primary output failed".to_owned()),
            Some((&report, b"report")),
            Some((&association, b"association")),
        )
        .unwrap_err();

        assert_eq!(error, "primary output failed");
        assert!(!report.exists());
        assert!(!association.exists());
    }

    fn parsed_anonymize_args(arguments: &[&str]) -> Box<AnonymizeArgs> {
        let arguments = ["report-anon", "anonymize"]
            .into_iter()
            .chain(arguments.iter().copied());
        let cli = Cli::try_parse_from(arguments).unwrap();
        let Command::Anonymize(args) = cli.command else {
            panic!("expected anonymize arguments")
        };
        args
    }
}
