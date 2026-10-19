"""Build and describe the release assets for one version tag.

Subcommands:

- ``manifest-tag``: print the tag that the workspace version requires.
- ``check-version``: require the tag, every package manifest, and the changelog to agree.
- ``build``: build the Linux binaries, SHA-256 checksums, abi3 wheel, and CycloneDX SBOMs.
- ``notes``: write the release notes from the changelog section and the audit summary.

The script uses only local tools and locks. It does not publish anything.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
from collections.abc import Sequence
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_OUTPUT = REPOSITORY_ROOT / "target" / "release-assets"
SECURITY_REPORTS = REPOSITORY_ROOT / "target" / "ci-reports" / "security"
REPOSITORY_URL = "https://github.com/medcognetics/dicom-anon"
RUST_TARGET = "x86_64-unknown-linux-gnu"
BINARIES = ("dicom-anonymize", "dicom-validate")
CHECKSUM_FILE = "SHA256SUMS"
# Prerelease tags are not supported: PEP 440 and SemVer spell them differently.
TAG_PATTERN = re.compile(
    r"^v(?P<version>(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*))$"
)
OUTPUT_MARKER = ".dicom-anon-release-assets"
CHANGELOG_HEADING = re.compile(r"^## \[(?P<version>[^\]]+)\](?P<rest>.*)$")
PROFILE_VERSION_PATTERN = re.compile(
    r'^pub const PROFILE_VERSION: &str = "(?P<edition>[^"]+)";$', re.MULTILINE
)
COMMIT_PATTERN = re.compile(r"^[0-9a-f]{40}$")
TOML_STRING = re.compile(
    r'^(?P<key>[A-Za-z0-9_-]+)\s*=\s*"(?P<value>[^"]*)"\s*(?:#.*)?$'
)
WORKSPACE_MEMBERS = re.compile(r"^members\s*=\s*\[(?P<members>[^\]]*)\]", re.MULTILINE)


class ReleaseError(Exception):
    """A release precondition is not satisfied."""


def version_from_tag(tag: str) -> str:
    match = TAG_PATTERN.match(tag)
    if match is None:
        raise ReleaseError(f"tag {tag!r} is not v<MAJOR>.<MINOR>.<PATCH>")
    return match["version"]


def toml_string(text: str, section: str, key: str) -> str:
    """Return a top-level string ``key`` from ``[section]`` of a simple TOML manifest."""
    in_section = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_section = stripped == f"[{section}]"
        elif (
            in_section
            and (match := TOML_STRING.match(stripped))
            and match["key"] == key
        ):
            return match["value"]
    raise ReleaseError(f"[{section}] has no string {key!r}")


def workspace_members(root: Path = REPOSITORY_ROOT) -> list[str]:
    cargo = (root / "Cargo.toml").read_text(encoding="utf-8")
    match = WORKSPACE_MEMBERS.search(cargo)
    if match is None:
        raise ReleaseError("Cargo.toml declares no workspace members")
    return re.findall(r'"([^"]+)"', match["members"])


def manifest_versions(root: Path = REPOSITORY_ROOT) -> dict[str, str]:
    """Return the version declared by every published package manifest."""
    cargo = (root / "Cargo.toml").read_text(encoding="utf-8")
    python = (root / "bindings" / "python" / "pyproject.toml").read_text(
        encoding="utf-8"
    )
    node = json.loads(
        (root / "bindings" / "node" / "package.json").read_text(encoding="utf-8")
    )
    node_lock = json.loads(
        (root / "bindings" / "node" / "package-lock.json").read_text(encoding="utf-8")
    )
    return {
        "Cargo.toml workspace.package.version": toml_string(
            cargo, "workspace.package", "version"
        ),
        "bindings/python/pyproject.toml project.version": toml_string(
            python, "project", "version"
        ),
        "bindings/node/package.json version": node["version"],
        "bindings/node/package-lock.json version": node_lock["version"],
    }


def changelog_section(changelog: str, version: str) -> str:
    """Return the body of the ``## [version]`` section without its heading."""
    lines = changelog.splitlines()
    start = None
    for index, line in enumerate(lines):
        match = CHANGELOG_HEADING.match(line)
        if match is None:
            continue
        if start is not None:
            return "\n".join(lines[start:index]).strip()
        if match["version"] == version:
            start = index + 1
    if start is None:
        raise ReleaseError(f"CHANGELOG.md has no '## [{version}]' section")
    return "\n".join(lines[start:]).strip()


def profile_edition(root: Path = REPOSITORY_ROOT) -> str:
    source = (root / "core" / "src" / "profile.rs").read_text(encoding="utf-8")
    match = PROFILE_VERSION_PATTERN.search(source)
    if match is None:
        raise ReleaseError("core/src/profile.rs does not declare PROFILE_VERSION")
    return match["edition"]


def check_version(tag: str, root: Path = REPOSITORY_ROOT) -> str:
    version = version_from_tag(tag)
    mismatches = [
        f"{source} is {declared}"
        for source, declared in manifest_versions(root).items()
        if declared != version
    ]
    if mismatches:
        raise ReleaseError(
            f"tag {tag} requires version {version}, but " + "; ".join(mismatches)
        )
    changelog = (root / "CHANGELOG.md").read_text(encoding="utf-8")
    if not re.search(
        rf"^## \[{re.escape(version)}\] - \d{{4}}-\d{{2}}-\d{{2}}$",
        changelog,
        re.MULTILINE,
    ):
        raise ReleaseError(
            f"CHANGELOG.md heading for {version} must be '## [{version}] - YYYY-MM-DD'"
        )
    section = changelog_section(changelog, version)
    if not section:
        raise ReleaseError(f"CHANGELOG.md section {version} is empty")
    return version


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as file:
        for block in iter(lambda: file.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def write_checksums(directory: Path, names: Sequence[str]) -> Path:
    """Write ``sha256sum``-compatible lines for the named files in ``directory``."""
    lines = [f"{sha256(directory / name)}  {name}\n" for name in sorted(names)]
    checksum_path = directory / CHECKSUM_FILE
    checksum_path.write_text("".join(lines), encoding="utf-8")
    return checksum_path


def run(
    command: Sequence[str], *, cwd: Path = REPOSITORY_ROOT, capture: bool = False
) -> subprocess.CompletedProcess[str]:
    print("+ " + " ".join(command), file=sys.stderr, flush=True)
    return subprocess.run(
        list(command), cwd=cwd, check=True, text=True, capture_output=capture
    )


def git_commit() -> str:
    commit = run(["git", "rev-parse", "HEAD"], capture=True).stdout.strip()
    if not COMMIT_PATTERN.match(commit):
        raise ReleaseError("cannot resolve the source commit")
    return commit


def require_version_text(text: str, binary: str, version: str, commit: str) -> None:
    expected = f"{binary} {version} (commit {commit})"
    if text.strip() != expected:
        raise ReleaseError(
            f"{binary} --version printed {text.strip()!r}, not {expected!r}"
        )


def build_binaries(output: Path, version: str, commit: str) -> list[str]:
    run(
        [
            "cargo",
            "build",
            "--locked",
            "--release",
            "--target",
            RUST_TARGET,
            "-p",
            "dicom-anon",
            *(argument for binary in BINARIES for argument in ("--bin", binary)),
        ]
    )
    names = []
    for binary in BINARIES:
        source = REPOSITORY_ROOT / "target" / RUST_TARGET / "release" / binary
        text = run([str(source), "--version"], capture=True).stdout
        require_version_text(text, binary, version, commit)
        name = f"{binary}-{version}-{RUST_TARGET}"
        shutil.copy2(source, output / name)
        names.append(name)
    return names


def build_wheel(output: Path, version: str) -> str:
    wheel_directory = output / "wheel"
    run(
        [
            "maturin",
            "build",
            "--release",
            "--locked",
            "--manifest-path",
            "bindings/python/Cargo.toml",
            "--target",
            RUST_TARGET,
            "--out",
            str(wheel_directory),
        ]
    )
    wheels = sorted(wheel_directory.glob("*.whl"))
    expected = re.compile(
        rf"^dicom_anon-{re.escape(version)}-cp310-abi3-manylinux_\d+_\d+_x86_64"
        r"(?:\.manylinux_\d+_\d+_x86_64)*\.whl$"
    )
    if len(wheels) != 1 or not expected.match(wheels[0].name):
        raise ReleaseError(f"expected one abi3 manylinux wheel, found {wheels}")
    destination = output / wheels[0].name
    shutil.move(wheels[0], destination)
    wheel_directory.rmdir()
    # Import the built wheel, not a development install, in a temporary environment.
    run(
        [
            "uv",
            "run",
            "--project",
            "tools/ci",
            "--frozen",
            "--isolated",
            "--with",
            str(destination),
            "pytest",
            "-q",
            "tests/test_python_binding.py",
        ]
    )
    return destination.name


def build_sboms(output: Path, version: str) -> list[str]:
    """Write CycloneDX 1.5 documents for the Rust, Python, and Node locks."""
    names = []
    run(
        [
            "cargo",
            "cyclonedx",
            "--format",
            "json",
            "--spec-version",
            "1.5",
            "--all-features",
            "--target",
            RUST_TARGET,
        ]
    )
    generated_files = (
        generated
        for member in workspace_members()
        for generated in sorted((REPOSITORY_ROOT / member).glob("*.cdx.json"))
    )
    for generated in generated_files:
        name = (
            f"sbom-rust-{generated.name.removesuffix('.cdx.json')}-{version}.cdx.json"
        )
        shutil.move(generated, output / name)
        names.append(name)
    if not names:
        raise ReleaseError("cargo cyclonedx wrote no SBOM")

    python_sbom = run(
        [
            "uv",
            "export",
            "--project",
            "tools/ci",
            "--frozen",
            "--format",
            "cyclonedx1.5",
            "--preview-features",
            "sbom-export",
        ],
        capture=True,
    ).stdout
    node_sbom = run(
        ["npm", "sbom", "--sbom-format", "cyclonedx", "--package-lock-only"],
        cwd=REPOSITORY_ROOT / "bindings" / "node",
        capture=True,
    ).stdout
    # The wheel has no Python runtime dependencies; sbom-rust-dicom-anon-python lists its
    # contents. These two locks hold only build, test, and audit tools.
    for ecosystem, document in (("python", python_sbom), ("node", node_sbom)):
        parsed = json.loads(document)
        if parsed.get("bomFormat") != "CycloneDX":
            raise ReleaseError(f"{ecosystem} SBOM is not a CycloneDX document")
        name = f"sbom-{ecosystem}-build-tools-{version}.cdx.json"
        (output / name).write_text(document, encoding="utf-8")
        names.append(name)
    return names


def require_current_audit(commit: str, reports: Path = SECURITY_REPORTS) -> None:
    """Require a passing ``make audit`` manifest for ``commit``."""
    manifest_path = reports / "manifest.json"
    if not manifest_path.is_file():
        raise ReleaseError(f"run 'make audit' first: {manifest_path} is missing")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if manifest.get("commit") != commit:
        raise ReleaseError("the audit evidence is not from the release commit")
    statuses = [result.get("status") for result in manifest.get("results", [])]
    if not statuses or any(status not in ("clean", "warnings") for status in statuses):
        raise ReleaseError("the audit evidence has blocking or incomplete results")


def copy_audit_evidence(output: Path, version: str, commit: str) -> list[str]:
    require_current_audit(commit)
    names = []
    for source, name in (
        ("summary.md", f"security-audit-{version}.md"),
        ("cargo-audit.json", f"cargo-audit-{version}.json"),
    ):
        path = SECURITY_REPORTS / source
        if not path.is_file():
            raise ReleaseError(f"run 'make audit' first: {path} is missing")
        shutil.copy2(path, output / name)
        names.append(name)
    return names


def prepare_output(output: Path) -> None:
    """Create an empty asset directory, replacing only a directory this script made."""
    if output.exists():
        if any(output.iterdir()) and not (output / OUTPUT_MARKER).is_file():
            raise ReleaseError(
                f"{output} is not empty and is not a release asset directory"
            )
        shutil.rmtree(output)
    output.mkdir(parents=True)
    (output / OUTPUT_MARKER).write_text(
        "Created by tools/ci/release.py. The next build replaces this directory.\n",
        encoding="utf-8",
    )


def build(tag: str, output: Path) -> None:
    version = check_version(tag)
    commit = git_commit()
    prepare_output(output)
    names = [
        *build_binaries(output, version, commit),
        build_wheel(output, version),
        *build_sboms(output, version),
        *copy_audit_evidence(output, version, commit),
    ]
    write_checksums(output, names)
    print(f"Wrote {len(names)} assets and {CHECKSUM_FILE} to {output}")


def release_notes(
    *,
    tag: str,
    version: str,
    commit: str,
    changelog: str,
    edition: str,
    audit_summary: str,
    checksums: str,
) -> str:
    source = f"{REPOSITORY_URL}/blob/{tag}"
    audit_body = re.sub(r"^# Dependency security audit\s*", "", audit_summary.strip())
    return "\n".join(
        [
            f"`dicom-anon` {version}, built in CI from tag `{tag}` at commit `{commit}`.",
            "",
            f"- DICOM PS3.15 edition: `{edition}` (Table E.1-1).",
            f"- Known anomalies: [docs/known-anomalies.md]({source}/docs/known-anomalies.md).",
            f"- Conformance and scope limits: [docs/conformance.md]({source}/docs/conformance.md).",
            "- Scope: DICOM metadata. Pixel samples are not inspected for burned-in text. "
            "This release makes no HIPAA, GDPR, or regulatory compliance claim.",
            "",
            "## Changes",
            "",
            changelog,
            "",
            "## Dependency audit",
            "",
            audit_body,
            "",
            "## Assets",
            "",
            "Linux x86_64 (glibc) binaries, an abi3 CPython 3.10+ wheel, CycloneDX 1.5 "
            "SBOMs, and the audit evidence. Verify downloads with "
            f"`sha256sum --check {CHECKSUM_FILE}`.",
            "",
            "- `sbom-rust-<crate>-*`: each workspace crate and its resolved Rust "
            "dependencies from `Cargo.lock`. `sbom-rust-dicom-anon-python-*` describes "
            "the wheel contents; the wheel has no Python runtime dependencies.",
            "- `sbom-python-build-tools-*`: `tools/ci/uv.lock`, the build, test, and "
            "audit tools. None of them ship in an asset.",
            "- `sbom-node-build-tools-*`: `bindings/node/package-lock.json`, the Node.js "
            "binding build tools. No Node.js package is released.",
            "",
            "```text",
            checksums.strip(),
            "```",
            "",
        ]
    )


def notes(tag: str, output: Path, notes_path: Path) -> None:
    version = check_version(tag)
    text = release_notes(
        tag=tag,
        version=version,
        commit=git_commit(),
        changelog=changelog_section(
            (REPOSITORY_ROOT / "CHANGELOG.md").read_text(encoding="utf-8"), version
        ),
        edition=profile_edition(),
        audit_summary=(SECURITY_REPORTS / "summary.md").read_text(encoding="utf-8"),
        checksums=(output / CHECKSUM_FILE).read_text(encoding="utf-8"),
    )
    notes_path.write_text(text, encoding="utf-8")
    if step_summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(step_summary).open("a", encoding="utf-8") as summary_file:
            summary_file.write(text)


def parse_arguments(arguments: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Build and describe release assets.")
    subcommands = parser.add_subparsers(dest="command", required=True)
    subcommands.add_parser("manifest-tag")
    for name in ("check-version", "build", "notes"):
        subcommand = subcommands.add_parser(name)
        subcommand.add_argument("--tag", required=True)
        if name != "check-version":
            subcommand.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
        if name == "notes":
            subcommand.add_argument("--notes", type=Path, required=True)
    return parser.parse_args(arguments)


def main(arguments: Sequence[str] | None = None) -> int:
    options = parse_arguments(sys.argv[1:] if arguments is None else arguments)
    try:
        if options.command == "manifest-tag":
            print("v" + manifest_versions()["Cargo.toml workspace.package.version"])
        elif options.command == "check-version":
            print(check_version(options.tag))
        elif options.command == "build":
            build(options.tag, options.output)
        else:
            notes(options.tag, options.output, options.notes)
    except (ReleaseError, subprocess.CalledProcessError) as error:
        print(f"release: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
