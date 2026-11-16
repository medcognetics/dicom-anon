//! Install and check the pinned text detector of the burned-in text check.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "dicom-text-detector",
    version = dicom_anon::cli_common::LONG_VERSION,
    about = "Install or check the pinned text detector for --burned-in-text-detector",
    after_help = "Exit status: 0 success, 2 usage or runtime error."
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Download the pinned model and ONNX Runtime library into DIR, which must not exist.
    Install {
        /// Folder to create.
        #[arg(value_name = "DIR")]
        dir: PathBuf,
    },
    /// Verify the files in DIR against their pinned SHA-256 values and run the model once.
    Check {
        /// Installed folder.
        #[arg(value_name = "DIR")]
        dir: PathBuf,
    },
}

fn main() -> ExitCode {
    let args = Args::parse();
    let result = match &args.command {
        Command::Install { dir } => dicom_anon::install_text_detector(dir),
        Command::Check { dir } => dicom_anon::check_text_detector(dir),
    };
    match result {
        Ok(()) => {
            eprintln!(
                "text detector ready: {} with ONNX Runtime {}",
                dicom_anon::MODEL_FILE,
                dicom_anon::ONNX_RUNTIME_VERSION
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("dicom-text-detector: {error}");
            ExitCode::from(2)
        }
    }
}
