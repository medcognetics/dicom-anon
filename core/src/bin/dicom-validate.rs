//! Unix-style DICOM confidentiality-profile validation command.

use std::fs::File;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{ArgAction, Parser};
use dicom_anon::cli_common::{load_key, ColorMode, OutputFormat, ProfileArgs, Styles};
use dicom_anon::{
    read_dicom, validate_object_with_options, validate_source_object, Options, Severity,
    ValidationOptions, ValidationReport,
};

#[derive(Debug, Parser)]
#[command(
    name = "dicom-validate",
    version = dicom_anon::cli_common::LONG_VERSION,
    about = "Assess DICOM PS3.15 metadata de-identification conformance",
    after_help = "Exit status: 0 conformant (warnings allowed), 1 metadata non-conformance, 2 usage or runtime error. Reports go to stdout; errors go to stderr."
)]
struct Args {
    #[command(flatten)]
    profile: ProfileArgs,
    /// Input DICOM Part 10 file, or '-' for stdin.
    #[arg(value_name = "INPUT", default_value = "-")]
    input: String,
    /// Original DICOM file for exact source-aware comparison.
    #[arg(long, value_name = "PATH")]
    source: Option<PathBuf>,
    /// File containing the anonymization key for source-aware validation.
    #[arg(long, value_name = "PATH", requires = "source")]
    key_file: Option<PathBuf>,
    /// Environment variable containing key material for source-aware validation.
    #[arg(long, value_name = "NAME", default_value = "DICOM_ANON_KEY")]
    key_env: String,
    /// Independently assigned patient scope used during anonymization.
    #[arg(long, value_name = "SCOPE", requires = "source")]
    patient_scope: Option<String>,
    /// Report format.
    #[arg(long, value_enum, default_value_t)]
    format: OutputFormat,
    /// Print only the status line and summary in text mode.
    #[arg(short, long, conflicts_with = "verbose")]
    quiet: bool,
    /// Include profile metadata and checked-attribute count in text mode.
    #[arg(short, long, conflicts_with = "quiet")]
    verbose: bool,
    /// Color policy for text output.
    #[arg(long, value_enum, default_value_t)]
    color: ColorMode,
    /// Disable color, overriding --color.
    #[arg(long, action = ArgAction::SetTrue)]
    no_color: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let report = match load_and_validate(&args) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("dicom-validate failed: {error}");
            return ExitCode::from(2);
        }
    };

    if let Err(error) = render(&args, &report) {
        eprintln!("dicom-validate failed: {error}");
        return ExitCode::from(2);
    }
    if report.conformant {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn load_and_validate(args: &Args) -> Result<ValidationReport, String> {
    let profile = args.profile.load()?;
    let mut reader: Box<dyn Read> = if args.input == "-" {
        Box::new(io::stdin().lock())
    } else {
        Box::new(
            File::open(&args.input)
                .map_err(|error| format!("cannot open {}: {error}", args.input))?,
        )
    };
    let candidate = read_dicom(&mut reader).map_err(|error| error.to_string())?;
    let Some(source_path) = &args.source else {
        return validate_object_with_options(&candidate, &ValidationOptions { profile })
            .map_err(|error| error.to_string());
    };
    let source = read_dicom(
        File::open(source_path)
            .map_err(|error| format!("cannot open {}: {error}", source_path.display()))?,
    )
    .map_err(|error| error.to_string())?;
    let secret = load_key(args.key_file.clone(), &args.key_env)?;
    let mut options = Options::new(secret);
    options.profile = profile;
    options.patient_scope.clone_from(&args.patient_scope);
    validate_source_object(&source, &candidate, options).map_err(|error| error.to_string())
}

fn render(args: &Args, report: &ValidationReport) -> Result<(), String> {
    if matches!(args.format, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::to_string_pretty(report).map_err(|error| error.to_string())?
        );
        return Ok(());
    }

    let styles = Styles::new(args.color, args.no_color, false, io::stdout().is_terminal());
    let status_text = if report.conformant {
        styles.success("PASS")
    } else {
        styles.failure("FAIL")
    };
    println!("{status_text}  dicom-validate  {}", args.input);
    println!();
    println!("{}", styles.heading("Summary"));
    let errors = report
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Error)
        .count();
    let warnings = report.findings.len() - errors;
    println!(
        "  Conformant:  {}",
        if report.conformant { "yes" } else { "no" }
    );
    println!("  Errors:      {errors}");
    println!("  Warnings:    {warnings}");

    if args.quiet {
        return Ok(());
    }
    if args.verbose {
        println!();
        println!("{}", styles.heading("Profile"));
        println!("  Name:                {}", report.profile);
        println!("  DICOM edition:       {}", report.profile_version);
        println!("  Policy:              {:?}", report.policy);
        println!("  Validation mode:     {:?}", report.validation_mode);
        println!(
            "  Active options:      {}",
            report
                .active_options
                .iter()
                .map(|option| format!("{option:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!("  Checked attributes:  {}", report.checked_attributes);
    }
    if !report.findings.is_empty() {
        println!();
        println!("{}", styles.heading("Findings"));
        for finding in &report.findings {
            let label = match finding.severity {
                Severity::Error => styles.failure("ERROR"),
                Severity::Warning => styles.warning("WARN"),
            };
            let tag = finding.tag.as_deref().unwrap_or("-");
            println!(
                "  {label} {} {tag} {}: {}",
                finding.code, finding.path, finding.message
            );
        }
    }
    Ok(())
}
