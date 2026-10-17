"""Unit tests for release version checks, checksums, and notes."""

from __future__ import annotations

import hashlib
import json
import shutil
from pathlib import Path

import pytest

from tools.ci import release
from tools.ci.release import ReleaseError


REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
CHANGELOG = """# Changelog

## [1.2.0] - 2026-11-01

### Added

- New output marker.

## [1.1.0] - 2026-10-01

- Older change.
"""


def make_repository(tmp_path: Path, version: str, changelog: str = CHANGELOG) -> Path:
    """Copy the manifests that carry the version into a temporary tree."""
    for relative in (
        "Cargo.toml",
        "bindings/python/pyproject.toml",
        "bindings/node/package.json",
        "bindings/node/package-lock.json",
        "core/src/profile.rs",
    ):
        destination = tmp_path / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(REPOSITORY_ROOT / relative, destination)
    current = release.manifest_versions(tmp_path)[
        "Cargo.toml workspace.package.version"
    ]
    for relative in ("Cargo.toml", "bindings/python/pyproject.toml"):
        path = tmp_path / relative
        path.write_text(
            path.read_text(encoding="utf-8").replace(
                f'version = "{current}"', f'version = "{version}"', 1
            ),
            encoding="utf-8",
        )
    for relative in ("bindings/node/package.json", "bindings/node/package-lock.json"):
        path = tmp_path / relative
        document = json.loads(path.read_text(encoding="utf-8"))
        document["version"] = version
        path.write_text(json.dumps(document), encoding="utf-8")
    (tmp_path / "CHANGELOG.md").write_text(changelog, encoding="utf-8")
    return tmp_path


def test_repository_manifests_declare_one_version() -> None:
    versions = set(release.manifest_versions().values())

    assert len(versions) == 1
    assert release.workspace_members() == [
        "core",
        "report",
        "parquet",
        "bindings/python",
        "bindings/node",
    ]


def test_repository_changelog_has_a_section_for_the_manifest_version() -> None:
    version = release.manifest_versions()["Cargo.toml workspace.package.version"]
    changelog = (REPOSITORY_ROOT / "CHANGELOG.md").read_text(encoding="utf-8")

    assert release.changelog_section(changelog, version)
    assert release.profile_edition() == "2026c"


@pytest.mark.parametrize(
    ("tag", "version"),
    [("v0.2.0", "0.2.0"), ("v10.0.3", "10.0.3"), ("v1.0.0-rc.1", "1.0.0-rc.1")],
)
def test_version_from_tag_accepts_semantic_versions(tag: str, version: str) -> None:
    assert release.version_from_tag(tag) == version


@pytest.mark.parametrize("tag", ["0.2.0", "v0.2", "v01.2.0", "v0.2.0+build", "v0.2.0-"])
def test_version_from_tag_rejects_other_names(tag: str) -> None:
    with pytest.raises(ReleaseError):
        release.version_from_tag(tag)


def test_changelog_section_stops_at_the_next_release() -> None:
    assert release.changelog_section(CHANGELOG, "1.2.0") == (
        "### Added\n\n- New output marker."
    )
    assert release.changelog_section(CHANGELOG, "1.1.0") == "- Older change."
    with pytest.raises(ReleaseError, match=r"no '## \[9.9.9\]' section"):
        release.changelog_section(CHANGELOG, "9.9.9")


def test_check_version_accepts_a_consistent_dated_release(tmp_path: Path) -> None:
    root = make_repository(tmp_path, "1.2.0")

    assert release.check_version("v1.2.0", root) == "1.2.0"


def test_check_version_rejects_a_manifest_mismatch(tmp_path: Path) -> None:
    root = make_repository(tmp_path, "1.2.0")
    package = root / "bindings" / "node" / "package.json"
    package.write_text(json.dumps({"version": "1.1.0"}), encoding="utf-8")

    with pytest.raises(ReleaseError, match="package.json version is 1.1.0"):
        release.check_version("v1.2.0", root)
    with pytest.raises(ReleaseError, match="requires version 1.1.0"):
        release.check_version("v1.1.0", root)


def test_check_version_rejects_an_undated_changelog_section(tmp_path: Path) -> None:
    root = make_repository(
        tmp_path,
        "1.2.0",
        CHANGELOG.replace("[1.2.0] - 2026-11-01", "[1.2.0] - Unreleased"),
    )

    with pytest.raises(ReleaseError, match="YYYY-MM-DD"):
        release.check_version("v1.2.0", root)


def test_checksums_use_sha256sum_format(tmp_path: Path) -> None:
    (tmp_path / "b.bin").write_bytes(b"second")
    (tmp_path / "a.bin").write_bytes(b"first")

    checksum_path = release.write_checksums(tmp_path, ["b.bin", "a.bin"])

    assert checksum_path.read_text(encoding="utf-8") == (
        f"{hashlib.sha256(b'first').hexdigest()}  a.bin\n"
        f"{hashlib.sha256(b'second').hexdigest()}  b.bin\n"
    )


def test_version_text_must_name_the_release_commit() -> None:
    commit = "a" * 40
    release.require_version_text(
        f"dicom-anonymize 1.2.0 (commit {commit})\n", "dicom-anonymize", "1.2.0", commit
    )
    for text in (
        "dicom-anonymize 1.2.0 (commit unknown)",
        f"dicom-anonymize 1.1.0 (commit {commit})",
    ):
        with pytest.raises(ReleaseError):
            release.require_version_text(text, "dicom-anonymize", "1.2.0", commit)


def test_release_notes_contain_required_sections() -> None:
    text = release.release_notes(
        tag="v1.2.0",
        version="1.2.0",
        commit="b" * 40,
        changelog="- New output marker.",
        edition="2026c",
        audit_summary="# Dependency security audit\n\n| `cargo-audit` | warnings | 0 |\n",
        checksums="0" * 64 + "  dicom-anonymize-1.2.0-x86_64-unknown-linux-gnu\n",
    )

    assert "DICOM PS3.15 edition: `2026c`" in text
    assert "blob/v1.2.0/docs/known-anomalies.md" in text
    assert "blob/v1.2.0/docs/conformance.md" in text
    assert "- New output marker." in text
    assert "| `cargo-audit` | warnings | 0 |" in text
    assert "# Dependency security audit" not in text
    assert "dicom-anonymize-1.2.0-x86_64-unknown-linux-gnu" in text
    assert "no HIPAA, GDPR, or regulatory compliance claim" in text
