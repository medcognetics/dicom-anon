use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use dicom_anon_parquet::{
    convert_directory, ColumnNaming, ConversionOptions, ConversionSummary, Error, SortMode,
};
use dicom_core::dictionary::{DataDictionary, DataDictionaryEntry};
use dicom_core::Tag;
use dicom_dictionary_std::StandardDataDictionary;
use serde::Serialize;

const EXIT_SUCCESS: u8 = 0;
const EXIT_PROBLEMS: u8 = 1;
const EXIT_RUNTIME_ERROR: u8 = 2;
const DEFAULT_SHARD_SIZE_MIB: u64 = 128;
const DEFAULT_COMPRESSION_LEVEL: i32 = 6;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum OutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum ProgressMode {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Parser)]
#[command(
    name = "dicom-to-parquet",
    about = "Convert DICOM metadata directly to compressed Parquet shards"
)]
struct Args {
    /// Directory tree containing DICOM files.
    source_dir: PathBuf,

    /// Base output path; creates NAME.00000.parquet and later shards.
    output_path: PathBuf,

    /// Include a baseline-compatible XXH3-64 Pixel Data hash.
    #[arg(long = "hash")]
    hash_pixel_data: bool,

    /// Override a standard DICOM tag with a constant value.
    #[arg(short = 't', long = "tag", value_parser = parse_key_value, value_name = "TAG=VALUE")]
    tags: Vec<(String, String)>,

    /// Stop at the first invalid DICOM candidate and write no output.
    #[arg(long)]
    strict: bool,

    /// Select the Parquet column naming convention.
    #[arg(
        long,
        value_enum,
        default_value_t = ColumnNaming::Dicom,
        conflicts_with = "snake_case"
    )]
    column_naming: ColumnNaming,

    /// Compatibility alias for --column-naming snake-case.
    #[arg(short = 's', long, conflicts_with = "column_naming")]
    snake_case: bool,

    /// Approximate maximum compressed shard size in MiB.
    #[arg(
        short = 'S',
        long,
        default_value_t = DEFAULT_SHARD_SIZE_MIB,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    shard_size: u64,

    /// Zstandard compression level.
    #[arg(
        short = 'c',
        long,
        default_value_t = DEFAULT_COMPRESSION_LEVEL,
        value_parser = clap::value_parser!(i32).range(1..=22)
    )]
    compression_level: i32,

    /// Ordering applied after candidate discovery.
    #[arg(long, value_enum, default_value_t = SortMode::Inode)]
    sort: SortMode,

    /// Summary output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,

    /// Color policy for text summaries and diagnostics.
    #[arg(long, value_enum, default_value_t = ColorMode::Auto)]
    color: ColorMode,

    /// Disable color even if --color always is present.
    #[arg(long)]
    no_color: bool,

    /// Progress policy for bounded DICOM processing.
    #[arg(long, value_enum, default_value_t = ProgressMode::Auto)]
    progress: ProgressMode,

    /// Print only the status and summary.
    #[arg(long, conflicts_with = "verbose")]
    quiet: bool,

    /// Include configuration details in the text summary.
    #[arg(long, conflicts_with = "quiet")]
    verbose: bool,
}

#[derive(Serialize)]
struct JsonReport<'a> {
    status: &'static str,
    #[serde(flatten)]
    summary: &'a ConversionSummary,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(summary) => {
            render_summary(&args, &summary);
            render_failures(&args, &summary);
            if summary.failed_files == 0 {
                ExitCode::from(EXIT_SUCCESS)
            } else {
                ExitCode::from(EXIT_PROBLEMS)
            }
        }
        Err(error) => {
            render_error(&args, &error);
            ExitCode::from(EXIT_RUNTIME_ERROR)
        }
    }
}

fn run(args: &Args) -> Result<ConversionSummary, Error> {
    let naming = if args.snake_case {
        ColumnNaming::SnakeCase
    } else {
        args.column_naming
    };
    let show_progress = !args.quiet
        && match args.progress {
            ProgressMode::Always => true,
            ProgressMode::Never => false,
            ProgressMode::Auto => {
                args.format == OutputFormat::Text && std::io::stderr().is_terminal()
            }
        };
    convert_directory(
        &args.source_dir,
        &args.output_path,
        &ConversionOptions {
            hash_pixel_data: args.hash_pixel_data,
            overrides: resolve_overrides(&args.tags)?,
            strict: args.strict,
            naming,
            sort: args.sort,
            shard_size_mib: args.shard_size,
            compression_level: args.compression_level,
            show_progress,
            ..ConversionOptions::default()
        },
    )
}

fn resolve_overrides(tags: &[(String, String)]) -> Result<BTreeMap<Tag, String>, Error> {
    tags.iter()
        .map(|(name, value)| {
            StandardDataDictionary
                .by_name(name)
                .map(|entry| (entry.tag(), value.clone()))
                .ok_or_else(|| Error::UnknownTag(name.clone()))
        })
        .collect()
}

fn render_summary(args: &Args, summary: &ConversionSummary) {
    match args.format {
        OutputFormat::Json => {
            let report = JsonReport {
                status: status(summary),
                summary,
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&report).expect("summary serialization cannot fail")
            );
        }
        OutputFormat::Text => render_text_summary(args, summary),
    }
}

fn render_text_summary(args: &Args, summary: &ConversionSummary) {
    let styles = Styles::new(color_enabled(args));
    let status_word = if summary.failed_files == 0 {
        styles.success("OK")
    } else {
        styles.warning("WARN")
    };
    println!(
        "{status_word}  dicom-to-parquet  {}  ({})",
        summary.source.display(),
        format_duration(summary.elapsed)
    );
    println!();
    println!("{}", styles.section("Summary"));
    println!("  Discovered:  {}", format_count(summary.discovered_files));
    println!("  Processed:   {}", format_count(summary.processed_files));
    println!("  Failed:      {}", format_count(summary.failed_files));
    println!("  Shards:      {}", format_count(summary.shard_paths.len()));

    if args.quiet {
        return;
    }
    if !summary.shard_paths.is_empty() {
        println!();
        println!("{}", styles.section("Output Shards"));
        for path in &summary.shard_paths {
            println!("  {}", path.display());
        }
    }
    if args.verbose {
        let naming = if args.snake_case {
            ColumnNaming::SnakeCase
        } else {
            args.column_naming
        };
        println!();
        println!("{}", styles.section("Configuration"));
        println!("  Column naming:    {naming:?}");
        println!("  Pixel hash:       {}", args.hash_pixel_data);
        println!("  Sort:             {:?}", args.sort);
        println!("  Shard size:       {} MiB", args.shard_size);
        println!("  Zstandard level:  {}", args.compression_level);
        println!(
            "  Pixel bytes read: {}",
            format_count(summary.hashed_pixel_bytes)
        );
    }
}

fn render_failures(args: &Args, summary: &ConversionSummary) {
    if args.quiet {
        return;
    }
    let styles = Styles::new(color_enabled(args));
    for failure in &summary.failure_samples {
        eprintln!(
            "{}: {}: {}",
            styles.warning("warning"),
            failure.path.display(),
            failure.error
        );
    }
    let omitted = summary
        .failed_files
        .saturating_sub(summary.failure_samples.len());
    if omitted > 0 {
        eprintln!(
            "{}: {} additional failures omitted",
            styles.warning("warning"),
            format_count(omitted)
        );
    }
}

fn render_error(args: &Args, error: &Error) {
    let styles = Styles::new(color_enabled(args));
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(
        stderr,
        "{} failed: {error}",
        styles.failure("dicom-to-parquet")
    );
    let mut source = error.source();
    while let Some(cause) = source {
        let _ = writeln!(stderr, "  caused by: {cause}");
        source = cause.source();
    }
}

fn status(summary: &ConversionSummary) -> &'static str {
    if summary.failed_files == 0 {
        "ok"
    } else {
        "warning"
    }
}

fn color_enabled(args: &Args) -> bool {
    if args.no_color || args.format != OutputFormat::Text {
        return false;
    }
    match args.color {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => std::io::stdout().is_terminal(),
    }
}

fn parse_key_value(value: &str) -> Result<(String, String), String> {
    let (key, value) = value
        .split_once('=')
        .ok_or_else(|| format!("invalid TAG=VALUE: no `=` found in `{value}`"))?;
    if key.is_empty() {
        return Err("invalid TAG=VALUE: TAG is empty".to_owned());
    }
    Ok((key.to_owned(), value.to_owned()))
}

fn format_duration(duration: std::time::Duration) -> String {
    if duration.as_secs() > 0 {
        format!("{:.2}s", duration.as_secs_f64())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

fn format_count(value: impl ToString) -> String {
    let digits = value.to_string();
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    let first_group = digits.len() % 3;
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && index % 3 == first_group {
            output.push(',');
        }
        output.push(character);
    }
    output
}

struct Styles {
    enabled: bool,
}

impl Styles {
    fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    fn success(&self, value: &str) -> String {
        self.paint("1;32", value)
    }

    fn warning(&self, value: &str) -> String {
        self.paint("1;33", value)
    }

    fn failure(&self, value: &str) -> String {
        self.paint("1;31", value)
    }

    fn section(&self, value: &str) -> String {
        self.paint("1;36", value)
    }

    fn paint(&self, code: &str, value: &str) -> String {
        if self.enabled {
            format!("\u{1b}[{code}m{value}\u{1b}[0m")
        } else {
            value.to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{format_count, format_duration, parse_key_value, Styles};

    #[test]
    fn formatting_helpers_cover_boundaries() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_234_567), "1,234,567");
        assert_eq!(format_count(u64::MAX), "18,446,744,073,709,551,615");
        assert_eq!(format_duration(Duration::from_millis(42)), "42ms");
        assert_eq!(format_duration(Duration::from_millis(1_250)), "1.25s");
    }

    #[test]
    fn key_value_parser_and_styles_are_centralized() {
        assert_eq!(
            parse_key_value("Modality=MG").unwrap(),
            ("Modality".to_owned(), "MG".to_owned())
        );
        assert!(parse_key_value("missing-separator").is_err());
        assert!(parse_key_value("=empty-key").is_err());

        let styles = Styles::new(true);
        assert!(styles.success("OK").contains("\u{1b}[1;32m"));
        assert!(styles.warning("WARN").contains("\u{1b}[1;33m"));
        assert!(styles.failure("FAIL").contains("\u{1b}[1;31m"));
        assert!(styles.section("Summary").contains("\u{1b}[1;36m"));
    }
}
