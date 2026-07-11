//! Unix-style DICOM confidentiality-profile validation command.

use std::fs::File;
use std::io::{self, IsTerminal, Read};
use std::process::ExitCode;

use clap::{ArgAction, Parser};
use dicom_anon::cli_common::{ColorMode, OutputFormat, Styles};
use dicom_anon::{read_dicom, validate_object, Severity, ValidationReport};

#[derive(Debug, Parser)]
#[command(
    name = "dicom-validate",
    version,
    about = "Assess DICOM PS3.15 metadata de-identification conformance",
    after_help = "Exit status: 0 conformant (warnings allowed), 1 metadata non-conformance, 2 usage or runtime error. Reports go to stdout; errors go to stderr."
)]
struct Args {
    /// Input DICOM Part 10 file, or '-' for stdin.
    #[arg(value_name = "INPUT", default_value = "-")]
    input: String,
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
    let report = match load_and_validate(&args.input) {
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

fn load_and_validate(input: &str) -> Result<ValidationReport, String> {
    let mut reader: Box<dyn Read> = if input == "-" {
        Box::new(io::stdin().lock())
    } else {
        Box::new(File::open(input).map_err(|error| format!("cannot open {input}: {error}"))?)
    };
    let object = read_dicom(&mut reader).map_err(|error| error.to_string())?;
    Ok(validate_object(&object))
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
