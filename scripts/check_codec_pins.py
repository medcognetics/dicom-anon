"""Fail when the codec pins drift from medcognetics/dicom-preprocessing (#47).

dicom-anon decodes Pixel Data with the OpenJPEG and libjpeg-turbo builds that
dicom-preprocessing uses, so the burned-in text check sees the samples that the model
pipeline sees and the device has one SOUP entry for each. This compares the locked
versions of the codec crates in both Cargo.lock files. dicom-preprocessing is public,
so the lockfile is read from GitHub without credentials; pass a local path instead with
`--preprocessing-lock`.
"""

import argparse
import sys
import tomllib
import urllib.request
from pathlib import Path

CODEC_CRATES = ("jpeg2k", "openjpeg-sys", "turbojpeg-sys")
UPSTREAM_LOCK = "https://raw.githubusercontent.com/medcognetics/dicom-preprocessing/master/Cargo.lock"


def locked_versions(lock_text: str) -> dict[str, set[str]]:
    versions: dict[str, set[str]] = {}
    for package in tomllib.loads(lock_text).get("package", []):
        if package["name"] in CODEC_CRATES:
            versions.setdefault(package["name"], set()).add(package["version"])
    return versions


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--preprocessing-lock", type=Path, help="local dicom-preprocessing Cargo.lock"
    )
    arguments = parser.parse_args()
    if arguments.preprocessing_lock:
        upstream_text = arguments.preprocessing_lock.read_text()
    else:
        with urllib.request.urlopen(UPSTREAM_LOCK, timeout=60) as response:  # noqa: S310 (fixed https URL)
            upstream_text = response.read().decode()
    ours = locked_versions(
        (Path(__file__).resolve().parent.parent / "Cargo.lock").read_text()
    )
    upstream = locked_versions(upstream_text)
    drift = [
        f"{crate}: dicom-anon {sorted(ours.get(crate, set()))}, dicom-preprocessing {sorted(upstream.get(crate, set()))}"
        for crate in CODEC_CRATES
        if ours.get(crate) != upstream.get(crate)
    ]
    if drift:
        print(
            "Codec pins differ from medcognetics/dicom-preprocessing:",
            *drift,
            sep="\n  ",
        )
        return 1
    print(
        "Codec pins match medcognetics/dicom-preprocessing:",
        ", ".join(f"{c} {sorted(ours[c])[0]}" for c in CODEC_CRATES),
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
