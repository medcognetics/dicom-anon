"""The codec pin check fails on drift from dicom-preprocessing (#47)."""

import subprocess
import sys
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / "scripts" / "check_codec_pins.py"
LOCK = Path(__file__).resolve().parent.parent / "Cargo.lock"


def run(lock: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--preprocessing-lock", str(lock)],
        capture_output=True,
        text=True,
        check=False,
    )


def test_matching_pins_pass() -> None:
    result = run(LOCK)
    assert result.returncode == 0, result.stdout + result.stderr


def test_a_different_codec_version_fails(tmp_path: Path) -> None:
    drifted = tmp_path / "Cargo.lock"
    drifted.write_text(
        LOCK.read_text().replace(
            'name = "openjpeg-sys"\nversion = "1.0.12"',
            'name = "openjpeg-sys"\nversion = "1.0.11"',
        )
    )
    assert drifted.read_text() != LOCK.read_text()
    result = run(drifted)
    assert result.returncode == 1
    assert "openjpeg-sys" in result.stdout
