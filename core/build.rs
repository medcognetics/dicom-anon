//! Record the source commit for `--version` without network access.

use std::path::{Path, PathBuf};
use std::process::Command;

const COMMIT_OVERRIDE: &str = "DICOM_ANON_GIT_COMMIT";
const DIRTY_INPUTS: [&str; 5] = [
    "src",
    "Cargo.toml",
    "build.rs",
    "../Cargo.toml",
    "../Cargo.lock",
];

fn main() {
    println!("cargo:rerun-if-env-changed={COMMIT_OVERRIDE}");
    let commit = std::env::var(COMMIT_OVERRIDE)
        .ok()
        .filter(|value| is_commit(value.strip_suffix("-dirty").unwrap_or(value)))
        .or_else(git_commit)
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=DICOM_ANON_GIT_COMMIT={commit}");
}

fn git_commit() -> Option<String> {
    // Ignore an enclosing repository that does not hold this crate at `core/`, such as a
    // consumer repository with a committed vendored copy.
    git(&["ls-files", "--error-unmatch", "build.rs"])?;
    if git(&["rev-parse", "--show-prefix"])? != "core/" {
        return None;
    }
    let commit = git(&["rev-parse", "HEAD"])?;
    if !is_commit(&commit) {
        return None;
    }
    // Rebuild when HEAD moves: HEAD itself, the checked-out branch ref, and packed refs.
    if let Some(git_dir) = git(&["rev-parse", "--git-dir"]).map(PathBuf::from) {
        rerun_if_changed(&git_dir.join("HEAD"));
    }
    if let Some(common_dir) = git(&["rev-parse", "--git-common-dir"]).map(PathBuf::from) {
        rerun_if_changed(&common_dir.join("packed-refs"));
        if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"]) {
            let reference = common_dir.join(reference);
            if reference.exists() {
                rerun_if_changed(&reference);
            } else if let Some(directory) = reference.ancestors().skip(1).find(|path| path.exists())
            {
                // A packed branch ref has no loose file. The next commit creates one below
                // this directory, and Cargo checks the newest modification time in it.
                rerun_if_changed(directory);
            }
        }
    }
    // Mark a build whose `dicom-anon` inputs differ from the commit: this crate and the
    // workspace manifest and lock. The same paths are watched, so the marker stays current.
    // `--no-optional-locks` keeps the status check from rewriting the index.
    for path in DIRTY_INPUTS {
        rerun_if_changed(Path::new(path));
    }
    let mut status = vec![
        "--no-optional-locks",
        "status",
        "--porcelain",
        "--untracked-files=no",
        "--",
    ];
    status.extend(DIRTY_INPUTS);
    match git(&status) {
        Some(status) if status.is_empty() => Some(commit),
        Some(_) => Some(format!("{commit}-dirty")),
        None => None,
    }
}

fn git(arguments: &[&str]) -> Option<String> {
    let output = Command::new("git").args(arguments).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    Some(text.trim().to_owned())
}

fn rerun_if_changed(path: &Path) {
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn is_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
