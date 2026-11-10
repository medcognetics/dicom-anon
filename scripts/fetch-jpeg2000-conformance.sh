#!/usr/bin/env bash
# Download the ITU-T T.803 | ISO/IEC 15444-4 class 1 conformance codestreams and their
# reference components from the OpenJPEG test data, at a pinned commit, into DIR.
#
# The files are not in this repository: their copyright notice permits use only for
# JPEG 2000 conformance testing. Each file is checked against
# scripts/jpeg2000-conformance.sha256. Run the test with:
#   DICOM_ANON_J2K_CONFORMANCE=DIR cargo test -p dicom-anon --test jpeg2000_conformance
set -euo pipefail
dir=${1:?usage: fetch-jpeg2000-conformance.sh DIR}
revision=39524bd3a601d90ed8e0177559400d23945f96a9
base=https://raw.githubusercontent.com/uclouvain/openjpeg-data/$revision
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$dir"
# Components compared per file, as in OpenJPEG's tests/conformance/CMakeLists.txt.
p0=(x 1 1 1 3 4 4 3 3 1 3 1 1 4 3 1 1)
p1=(x 1 3 4 1 3 3 2)
paths=(input/conformance/COPYRIGHT)
for n in $(seq 1 16); do
  paths+=("input/conformance/$(printf p0_%02d "$n").j2k")
  for c in $(seq 0 $((p0[n] - 1))); do paths+=("baseline/conformance/$(printf c1p0_%02d_%d "$n" "$c").pgx"); done
done
for n in $(seq 1 7); do
  paths+=("input/conformance/$(printf p1_%02d "$n").j2k")
  for c in $(seq 0 $((p1[n] - 1))); do paths+=("baseline/conformance/$(printf c1p1_%02d_%d "$n" "$c").pgx"); done
done
for path in "${paths[@]}"; do
  name=$(basename "$path")
  # Download to a temporary name, so an interrupted transfer is fetched again next time.
  if [ ! -f "$dir/$name" ]; then
    curl -sSfL --retry 3 --max-time 120 -o "$dir/$name.part" "$base/$path"
    mv "$dir/$name.part" "$dir/$name"
  fi
done
if [ -f "$here/jpeg2000-conformance.sha256" ]; then
  (cd "$dir" && shasum -a 256 -c --quiet "$here/jpeg2000-conformance.sha256")
fi
