//! Unix-style DICOM de-identification command.

use std::fs::File;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{ArgAction, Parser};
use dicom_anon::cli_common::{load_key, ColorMode, ProfileArgs, Styles};
use dicom_anon::{anonymize_file, anonymize_reader, Anonymizer, Options};

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
    /// Numeric UID root for deterministic replacement UIDs.
    #[arg(long, default_value = "2.25")]
    uid_root: String,
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
        Ok(report) => {
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

fn run(args: &Args) -> Result<dicom_anon::AnonymizationReport, String> {
    let secret = load_key(args.key_file.clone(), &args.key_env)?;
    let mut options = Options::new(secret);
    options.profile = args.profile.load()?;
    options.patient_scope.clone_from(&args.patient_scope);
    options.uid_root.clone_from(&args.uid_root);
    options.reject_already_deidentified = !args.allow_reanonymize;
    let anonymizer = Anonymizer::new(options).map_err(|error| error.to_string())?;

    if args.in_place {
        if args.input == "-" {
            return Err("--in-place requires a file input".to_owned());
        }
        return anonymize_file(&anonymizer, &args.input, &args.input)
            .map_err(|error| error.to_string());
    }

    let output = args.output.as_deref().unwrap_or("-");
    if args.input != "-" && output != "-" {
        return anonymize_file(&anonymizer, &args.input, output).map_err(|error| error.to_string());
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
        anonymize_reader(&anonymizer, &mut input).map_err(|error| error.to_string())?;
    if output == "-" {
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(&bytes)
            .map_err(|error| format!("cannot write stdout: {error}"))?;
        stdout
            .flush()
            .map_err(|error| format!("cannot flush stdout: {error}"))?;
    } else {
        write_atomic(Path::new(output), &bytes)?;
    }
    Ok(report)
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
    Ok(())
}
