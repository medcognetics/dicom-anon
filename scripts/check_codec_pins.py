"""Fail when the codec pins drift from medcognetics/dicom-preprocessing (#47).

dicom-anon decodes Pixel Data with the OpenJPEG and libjpeg-turbo builds that
dicom-preprocessing uses, so the burned-in text check sees the samples that the model
pipeline sees and the device has one SOUP entry for each. This compares the locked
versions of the codec crates in both Cargo.lock files. dicom-preprocessing is public,
so the lockfile is read from GitHub without credentials; pass a local path instead with
`--preprocessing-lock`.

The required CI gate reads a pinned dicom-preprocessing commit, so its result is
reproducible and an upstream change cannot break unrelated pull requests. The nightly
workflow runs `--ref master`, so drift from the current upstream still fails before a
release. Update `PINNED_REF` when the pins move together.
"""

import argparse
import re
import sys
import urllib.request
from pathlib import Path

CODEC_CRATES = ("jpeg2k", "openjpeg-sys", "turbojpeg-sys")
PINNED_REF = "e02ef5faecefa5acaefcd0586754e91654deb124"
UPSTREAM_LOCK = "https://raw.githubusercontent.com/medcognetics/dicom-preprocessing/{ref}/Cargo.lock"
# A Cargo.lock package entry starts with its name and version lines. A regular
# expression keeps this working on Python 3.10, which has no tomllib.
PACKAGE = re.compile(
    r'^\[\[package\]\]\nname = "([^"]+)"\nversion = "([^"]+)"', re.MULTILINE
)


def locked_versions(lock_text: str) -> dict[str, set[str]]:
    versions: dict[str, set[str]] = {}
    for name, version in PACKAGE.findall(lock_text):
        if name in CODEC_CRATES:
            versions.setdefault(name, set()).add(version)
    return versions


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--preprocessing-lock", type=Path, help="local dicom-preprocessing Cargo.lock"
    )
    parser.add_argument(
        "--ref",
        default=PINNED_REF,
        help="dicom-preprocessing commit or branch to compare with",
    )
    arguments = parser.parse_args()
    if arguments.preprocessing_lock:
        upstream_text = arguments.preprocessing_lock.read_text()
    else:
        if not re.fullmatch(r"[0-9A-Za-z._/-]+", arguments.ref):
            parser.error("--ref holds characters that are not allowed")
        url = UPSTREAM_LOCK.format(ref=arguments.ref)
        with urllib.request.urlopen(url, timeout=60) as response:  # noqa: S310 (fixed https host)
            upstream_text = response.read().decode()
    ours = locked_versions(
        (Path(__file__).resolve().parent.parent / "Cargo.lock").read_text()
    )
    upstream = locked_versions(upstream_text)
    if not ours or not upstream:
        print("No codec crate was found in a Cargo.lock file.")
        return 1
    drift = [
        f"{crate}: dicom-anon {sorted(ours.get(crate, set()))}, "
        f"dicom-preprocessing {sorted(upstream.get(crate, set()))}"
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
