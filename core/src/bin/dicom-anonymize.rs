//! Unix-style DICOM de-identification command.

use std::fs::File;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{ArgAction, Parser};
use dicom_anon::cli_common::{load_key, ColorMode, ProfileArgs, Styles};
use dicom_anon::{
    anonymize_file, anonymize_reader, read_dicom, write_dicom, AnonymizationReport, Anonymizer,
    DefaultDicomObject, Options,
};

#[derive(Debug, Parser)]
#[command(
    name = "dicom-anonymize",
    version,
    about = "De-identify one DICOM file using DICOM PS3.15",
    after_help = "Exit status: 0 success, 2 usage or runtime error. DICOM output goes to stdout when OUTPUT is omitted or '-'."
)]
struct Args {
    #[command(flatten)]
    profile: ProfileArgs,
    /// Input DICOM Part 10 file, or '-' for stdin.
    #[arg(value_name = "INPUT")]
    input: String,
    /// Output file, or '-' for stdout. Defaults to stdout.
    #[arg(value_name = "OUTPUT", conflicts_with = "in_place")]
    output: Option<String>,
    /// Atomically replace INPUT while preserving its permissions.
    #[arg(long, conflicts_with = "output")]
    in_place: bool,
    /// File containing at least 32 bytes of secret key material.
    #[arg(long, value_name = "PATH")]
    key_file: Option<PathBuf>,
    /// Environment variable containing key material when --key-file is absent.
    #[arg(long, value_name = "NAME", default_value = "DICOM_ANON_KEY")]
    key_env: String,
    /// Stable scope for inputs with no usable Patient ID or Patient Name.
    #[arg(long, value_name = "SCOPE")]
    patient_scope: Option<String>,
    /// Numeric UID root for keyed or generated replacement UIDs.
    #[arg(long, default_value = "2.25")]
    uid_root: String,
    /// Print each source UID that the profile replaces, with its attribute path, as JSON and write no DICOM output.
    #[arg(
        long,
        conflicts_with_all = ["output", "in_place", "generate_uid_map", "verbose"]
    )]
    list_required_uids: bool,
    /// Assign missing Safe Harbor UID replacements at random and write the complete UID map to PATH before the DICOM output.
    #[arg(long, value_name = "PATH", conflicts_with = "in_place")]
    generate_uid_map: Option<PathBuf>,
    /// Permit reprocessing input marked Patient Identity Removed=YES.
    #[arg(long)]
    allow_reanonymize: bool,
    /// Suppress success diagnostics.
    #[arg(short, long, conflicts_with = "verbose")]
    quiet: bool,
    /// Print the anonymization report to stderr.
    #[arg(short, long, conflicts_with = "quiet")]
    verbose: bool,
    /// Color policy for diagnostics.
    #[arg(long, value_enum, default_value_t)]
    color: ColorMode,
    /// Disable color, overriding --color.
    #[arg(long, action = ArgAction::SetTrue)]
    no_color: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(None) => ExitCode::SUCCESS,
        Ok(Some(report)) => {
            if args.verbose && !args.quiet {
                match serde_json::to_string_pretty(&report) {
                    Ok(json) => eprintln!("{json}"),
                    Err(error) => {
                        eprintln!("dicom-anonymize failed: cannot serialize report: {error}");
                        return ExitCode::from(2);
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            let styles = Styles::new(args.color, args.no_color, false, io::stderr().is_terminal());
            eprintln!("{}: {error}", styles.failure("dicom-anonymize failed"));
            ExitCode::from(2)
        }
    }
}

fn run(args: &Args) -> Result<Option<AnonymizationReport>, String> {
    let secret = load_key(args.key_file.clone(), &args.key_env)?;
    let mut options = Options::new(secret);
    options.profile = args.profile.load()?;
    options.patient_scope.clone_from(&args.patient_scope);
    options.uid_root.clone_from(&args.uid_root);
    options.reject_already_deidentified = !args.allow_reanonymize;
    let anonymizer = Anonymizer::new(options).map_err(|error| error.to_string())?;

    if args.list_required_uids {
        let source = read_source(&args.input)?;
        let required = anonymizer
            .required_uids(&source)
            .map_err(|error| error.to_string())?;
        let json = serde_json::to_string_pretty(&serde_json::json!({ "required_uids": required }))
            .map_err(|error| format!("cannot serialize required UIDs: {error}"))?;
        writeln!(io::stdout().lock(), "{json}")
            .map_err(|error| format!("cannot write stdout: {error}"))?;
        return Ok(None);
    }
    if let Some(map_path) = &args.generate_uid_map {
        return generate_uid_map(args, &anonymizer, map_path).map(Some);
    }
    anonymize(args, &anonymizer).map(Some)
}

fn anonymize(args: &Args, anonymizer: &Anonymizer) -> Result<AnonymizationReport, String> {
    if args.in_place {
        if args.input == "-" {
            return Err("--in-place requires a file input".to_owned());
        }
        return anonymize_file(anonymizer, &args.input, &args.input)
            .map_err(|error| error.to_string());
    }

    let output = args.output.as_deref().unwrap_or("-");
    if args.input != "-" && output != "-" {
        return anonymize_file(anonymizer, &args.input, output).map_err(|error| error.to_string());
    }

    let mut input: Box<dyn Read> = if args.input == "-" {
        Box::new(io::stdin().lock())
    } else {
        Box::new(
            File::open(&args.input)
                .map_err(|error| format!("cannot open {}: {error}", args.input))?,
        )
    };
    let (bytes, report) =
        anonymize_reader(anonymizer, &mut input).map_err(|error| error.to_string())?;
    write_output(output, &bytes)?;
    Ok(report)
}

/// Write the complete map first so that no output exists without its map entries.
fn generate_uid_map(
    args: &Args,
    anonymizer: &Anonymizer,
    map_path: &Path,
) -> Result<AnonymizationReport, String> {
    let output = args.output.as_deref().unwrap_or("-");
    let protected = [
        (args.input != "-").then(|| PathBuf::from(&args.input)),
        (output != "-").then(|| PathBuf::from(output)),
        args.key_file.clone(),
        args.profile.descriptor_map.clone(),
        args.profile.safe_private_allowlist.clone(),
    ];
    if protected
        .iter()
        .flatten()
        .any(|path| same_path(map_path, path))
    {
        return Err(
            "--generate-uid-map must not name the input, output, key, descriptor-map, or safe-private allowlist file"
                .to_owned(),
        );
    }
    if std::fs::symlink_metadata(map_path).is_ok()
        && !args
            .profile
            .uid_map
            .as_deref()
            .is_some_and(|loaded| same_path(map_path, loaded))
    {
        return Err(
            "--generate-uid-map names an existing file; pass the same file as --uid-map to extend it"
                .to_owned(),
        );
    }
    let source = read_source(&args.input)?;
    let (anonymized, report, map) = anonymizer
        .anonymize_with_generated_uid_map(&source)
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    write_dicom(&anonymized, &mut bytes).map_err(|error| error.to_string())?;
    let mut json = serde_json::to_vec_pretty(&map)
        .map_err(|error| format!("cannot serialize UID map: {error}"))?;
    json.push(b'\n');
    // Replace the link target, not a symbolic link that names the loaded map.
    write_atomic(&normalize(map_path), &json)?;
    write_output(output, &bytes)?;
    Ok(report)
}

fn read_source(input: &str) -> Result<DefaultDicomObject, String> {
    let result = if input == "-" {
        read_dicom(io::stdin().lock())
    } else {
        read_dicom(File::open(input).map_err(|error| format!("cannot open {input}: {error}"))?)
    };
    result.map_err(|error| error.to_string())
}

fn write_output(output: &str, bytes: &[u8]) -> Result<(), String> {
    if output != "-" {
        return write_atomic(Path::new(output), bytes);
    }
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(bytes)
        .map_err(|error| format!("cannot write stdout: {error}"))?;
    stdout
        .flush()
        .map_err(|error| format!("cannot flush stdout: {error}"))
}

/// Compare paths after resolving symbolic links, including the parent of a new file.
fn same_path(left: &Path, right: &Path) -> bool {
    normalize(left) == normalize(right)
}

fn normalize(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => parent
            .canonicalize()
            .map_or_else(|_| absolute.clone(), |parent| parent.join(name)),
        _ => absolute,
    }
}

fn write_atomic(output: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        format!(
            "cannot create temporary output in {}: {error}",
            parent.display()
        )
    })?;
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
    sync_directory(parent)
}

/// Persist the rename so that a crash cannot keep a later output without this file.
#[cfg(unix)]
fn sync_directory(directory: &Path) -> Result<(), String> {
    let directory = if directory.as_os_str().is_empty() {
        Path::new(".")
    } else {
        directory
    };
    File::open(directory)
        .and_then(|handle| handle.sync_all())
        .map_err(|error| format!("cannot sync {}: {error}", directory.display()))
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> Result<(), String> {
    Ok(())
}
