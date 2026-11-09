#!/usr/bin/env python3
"""Generate the shipped breast-imaging coded-concept descriptor maps.

Run with:

    uv run tools/generate_breast_descriptor_map.py

The source URLs point at ``current``, but generation fails unless every page
identifies itself as DICOM PS3.16 2026d. This prevents a later edition from
silently changing a reviewed map. Each map keeps every Code Value, Coding
Scheme Designator, and Code Meaning of its selected context groups unchanged.
SRT entries use the SNOMED-RT ID column of the same table rows, when a group has it.
"""

from __future__ import annotations

from html.parser import HTMLParser
import json
import re
from pathlib import Path
from urllib.request import urlopen


EDITION = "2026d"
BASE_URL = "https://dicom.nema.org/medical/dicom/current/output/chtml/part16"
OUTPUT_DIR = Path(__file__).parents[1] / "descriptor-maps"

# Context group -> predicate that selects the rows to include.
VIEW_CONTEXT_GROUPS = {
    "4013": lambda meaning: True,
    "4014": lambda meaning: True,
    "4015": lambda meaning: True,
    "4031": lambda meaning: meaning == "Breast",
}
# Every PS3.16 context group whose title names a Purpose of Reference, because the same
# Purpose of Reference Code Sequence (0040,A170) takes a different group in each context.
# CID 7003 is left out: it applies to report references, and it has a UMLS row.
PURPOSE_OF_REFERENCE_GROUPS = (
    "7004", "7005", "7006", "7009", "7013", "7019", "7022", "7060", "7062",
    "7201", "7202", "7205", "7210", "7215", "7221", "7222",
)
REFERENCE_CONTEXT_GROUPS = {
    "4005": lambda meaning: True,
    **{cid: (lambda meaning: True) for cid in PURPOSE_OF_REFERENCE_GROUPS},
    "7203": lambda meaning: True,
}
HEADER = ["Coding Scheme Designator", "Code Value", "Code Meaning"]
SNOMED_RT_COLUMN = "SNOMED-RT ID"
# These groups have no SNOMED-RT column in the edition. CID 7019 and CID 7022 have SCT rows
# and so have one.
GROUPS_WITHOUT_SNOMED_RT = {*PURPOSE_OF_REFERENCE_GROUPS, "7203"} - {"7019", "7022"}
CODE_VALUE = "00080100"
CODING_SCHEME_DESIGNATOR = "00080102"
CODE_MEANING = "00080104"


class TableParser(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.title = ""
        self.rows: list[list[str]] = []
        self._row: list[str] | None = None
        self._cell: str | None = None
        self._in_title = False

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        del attrs
        if tag == "title":
            self._in_title = True
        elif tag == "tr":
            self._row = []
        elif tag in {"td", "th"} and self._row is not None:
            self._cell = ""

    def handle_endtag(self, tag: str) -> None:
        if tag == "title":
            self._in_title = False
        elif tag in {"td", "th"} and self._cell is not None and self._row is not None:
            self._row.append(" ".join(self._cell.split()))
            self._cell = None
        elif tag == "tr" and self._row is not None:
            self.rows.append(self._row)
            self._row = None

    def handle_data(self, data: str) -> None:
        if self._in_title:
            self.title += data
        if self._cell is not None:
            self._cell += data


def context_group_rows(cid: str) -> list[tuple[str, str, str, str]]:
    url = f"{BASE_URL}/sect_CID_{cid}.html"
    with urlopen(url, timeout=60) as response:
        page = response.read().decode("utf-8")
    parser = TableParser()
    parser.feed(page)
    if f"DICOM PS3.16 {EDITION}" not in page:
        raise SystemExit(f"CID {cid} is not from DICOM PS3.16 {EDITION}: {url}")
    if not " ".join(parser.title.split()).startswith(f"CID {cid} "):
        raise SystemExit(f"unexpected page title for CID {cid}: {parser.title!r}")
    header_index = next(
        (index for index, row in enumerate(parser.rows) if row[:3] == HEADER), None
    )
    if header_index is None:
        raise SystemExit(f"CID {cid} has no recognized code table")
    has_snomed_rt = parser.rows[header_index][3:4] == [SNOMED_RT_COLUMN]
    if has_snomed_rt == (cid in GROUPS_WITHOUT_SNOMED_RT):
        raise SystemExit(f"CID {cid} has an unexpected {SNOMED_RT_COLUMN} column state")
    rows = []
    for row in parser.rows[header_index + 1 :]:
        # The page navigation row ends the table.
        if row[:1] == ["Prev"]:
            break
        # An Include row names another group. `mappings_for` checks that it is selected too.
        included = re.match(r"Include CID (\d+)", row[0]) if len(row) == 1 else None
        if included:
            rows.append(("INCLUDE", included.group(1), "", ""))
            continue
        # Any other row must be a code row, so that no code is dropped without an error.
        if len(row) < (4 if has_snomed_rt else 3) or row[0] not in {"DCM", "SCT"}:
            raise SystemExit(f"CID {cid} has an unexpected table row")
        rows.append((row[0], row[1], row[2], row[3] if has_snomed_rt else ""))
    if not rows:
        raise SystemExit(f"CID {cid} has no code rows")
    return rows


def mappings_for(context_groups: dict) -> dict[str, dict[str, str]]:
    mappings: dict[str, dict[str, str]] = {
        CODE_VALUE: {},
        CODING_SCHEME_DESIGNATOR: {"DCM": "DCM", "SCT": "SCT", "SRT": "SRT"},
        CODE_MEANING: {},
    }
    for cid, selected in context_groups.items():
        for scheme, value, meaning, snomed_rt in context_group_rows(cid):
            if scheme == "INCLUDE":
                if value not in context_groups:
                    raise SystemExit(f"CID {cid} includes CID {value}, which is not selected")
                continue
            if not selected(meaning):
                continue
            mappings[CODE_VALUE][value] = value
            mappings[CODING_SCHEME_DESIGNATOR][scheme] = scheme
            if snomed_rt:
                mappings[CODE_VALUE][snomed_rt] = snomed_rt
            mappings[CODE_MEANING][meaning] = meaning
    return mappings


def write_map(file_name: str, name: str, source: str, notes: list[str], mappings: dict) -> None:
    document = {
        "format": "dicom-anon-descriptor-map/1",
        "name": name,
        "version": f"{EDITION}.1",
        "source": source,
        "notes": notes,
        "mappings": mappings,
    }
    OUTPUT_DIR.mkdir(exist_ok=True)
    (OUTPUT_DIR / file_name).write_text(
        json.dumps(document, indent=2, sort_keys=True) + "\n"
    )


def main() -> None:
    write_map(
        f"breast-imaging-ps3.16-{EDITION}.json",
        "breast-imaging-coded-concepts",
        (
            f"DICOM PS3.16 {EDITION}: CID 4013 Anatomic Region for Mammography, "
            "CID 4014 View for Mammography, CID 4015 View Modifier for Mammography, "
            "and the Breast row of CID 4031 Common Anatomic Region"
        ),
        [
            "Generated by tools/generate_breast_descriptor_map.py; do not edit by hand.",
            "Every entry maps a standard value to itself.",
            "SCT values come from the Code Value column. SRT values come from the "
            "SNOMED-RT ID column of the same rows.",
            "Code Meaning values match the CID text exactly. No row of these "
            f"context groups uses the DCM scheme in {EDITION}.",
            "Mappings apply to the tag at every nesting level, not only in the "
            "View Code, View Modifier Code, and Anatomic Region sequences.",
        ],
        mappings_for(VIEW_CONTEXT_GROUPS),
    )
    write_map(
        f"breast-imaging-references-ps3.16-{EDITION}.json",
        "breast-imaging-reference-coded-concepts",
        (
            f"DICOM PS3.16 {EDITION}: CID 4005 Partial View Section for Mammography, every "
            "Purpose of Reference context group except CID 7003 ("
            + ", ".join(f"CID {cid}" for cid in PURPOSE_OF_REFERENCE_GROUPS)
            + "), and CID 7203 Image Derivation"
        ),
        [
            "Generated by tools/generate_breast_descriptor_map.py; do not edit by hand.",
            "Every entry maps a standard value to itself.",
            "CID 4005 gives Partial View Code Sequence (0028,1352) values. The Purpose of Reference "
            "groups give Purpose of Reference Code Sequence (0040,A170) values in each context where "
            "it occurs, such as Source Image, Referenced Image, Related Series and Contributing "
            "Equipment Sequences. CID 7003 applies to report references and has a UMLS row. CID 7203 gives "
            "Derivation Code Sequence (0008,9215) values.",
            "SCT values come from the Code Value column. SRT values come from the "
            "SNOMED-RT ID column of the same rows, where a group has that column.",
            "Code Meaning values match the CID text exactly.",
            "Mappings apply to the tag at every nesting level, not only in these sequences.",
        ],
        mappings_for(REFERENCE_CONTEXT_GROUPS),
    )


if __name__ == "__main__":
    main()
