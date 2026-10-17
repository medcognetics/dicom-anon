//! Record the source commit for `--version` without network access.

use std::path::{Path, PathBuf};
use std::process::Command;

const COMMIT_OVERRIDE: &str = "DICOM_ANON_GIT_COMMIT";

fn main() {
    println!("cargo:rerun-if-env-changed={COMMIT_OVERRIDE}");
    let commit = std::env::var(COMMIT_OVERRIDE)
        .ok()
        .filter(|value| is_commit(value))
        .or_else(git_commit)
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=DICOM_ANON_GIT_COMMIT={commit}");
}

fn git_commit() -> Option<String> {
    // Ignore an enclosing repository that does not track this crate, such as a vendored copy.
    git(&["ls-files", "--error-unmatch", "build.rs"])?;
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
            rerun_if_changed(&common_dir.join(reference));
        }
    }
    Some(commit)
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
