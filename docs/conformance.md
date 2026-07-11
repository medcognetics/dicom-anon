# DICOM confidentiality conformance statement

## Profile

The implementation targets the metadata actions in the DICOM PS3.15 2026c Basic Application Level Confidentiality Profile and the Retain Longitudinal Temporal Information With Modified Dates option. The source rule set is Table E.1-1. The action definitions are in Table E.1-1a.

The implementation does not claim these options:

- Clean Pixel Data;
- Clean Recognizable Visual Features;
- Clean Graphics;
- Clean Structured Content;
- Clean Descriptors;
- Retain Longitudinal Temporal Information With Full Dates;
- Retain Patient Characteristics;
- Retain Device Identity;
- Retain Institution Identity;
- Retain UIDs;
- Retain Safe Private.

The library does not create Encrypted Attributes Data Sets and does not support re-identification.

## Action implementation

| PS3.15 action | Implementation |
|---|---|
| `X` | Remove the attribute and all sequence content. |
| `Z` | Write a zero-length value. Patient Name receives a dummy pseudonym to retain association. |
| `D` | Write a non-empty VR-compatible dummy. For sequences, retain the item structure and replace every contained non-private value recursively with a non-empty VR-compatible dummy. An empty sequence receives one empty item so the sequence itself has a non-zero-length replacement. |
| `U` | Replace each UID with a keyed deterministic UID under the configured root. Recurse for `X/Z/U*` sequences. |
| `K` | Keep primitive values; recursively clean sequence items. |
| `C` under Modified Dates | Shift DA values by the patient day offset and TM values by the patient whole-hour offset. Apply the TM midnight carry to known paired DA attributes. Shift full-precision DT values by both offsets. Shift year-only and year-month DT values by a nonzero whole-year or whole-month offset while retaining their encoded precision. Recurse through sequences. |
| `Z/D`, `X/D`, `X/Z/D` | Choose `D` to avoid removing required Type 1 values. |
| `X/Z` | Choose `Z` to avoid removing Type 2 values. |
| `X/Z/U*` | Retain the sequence structure and recursively remap contained UIDs. |

Patient ID always receives the patient pseudonym, including when its effective table action is `Z/D`. This prevents all patients from collapsing to one dummy identifier.

All odd-group private attributes are removed. Group `0004` attributes are removed because this tool does not create or update DICOMDIR files. Curve Data and Overlay Data/Comments use their repeating-group rules. DA, DT, and TM attributes added after the frozen table edition receive the modified-dates action based on their Value Representation.

## Deterministic scope

The patient scope is selected in this order:

1. explicit caller scope;
2. Patient ID plus Issuer of Patient ID;
3. Patient Name;
4. error if none is available.

The same secret and patient scope produce the same Patient ID, date offset, and time offset across invocations. The date offset is a nonzero value in the inclusive range from -3,650 through 3,650 days by default. The time offset is a nonzero value from -12 through 12 whole hours. Full-precision DT values receive both offsets. TM values retain their encoded precision and receive the hour offset modulo 24.

For standard DA/TM pairs such as Study Date and Study Time, the day before or after midnight is carried into the paired DA value. The tool fails closed when a known pair has different value multiplicities. This preserves ordering and exact intervals across paired timestamps processed in separate invocations. Standalone TM values receive the same cyclic hour offset, which preserves their relative time-of-day spacing. A partial DT that encodes only a year or year and month uses the day offset rounded away from zero to whole years or months. This preserves the source precision; exact day intervals are not present in those source values.

UID replacement uses the secret and original UID, not patient scope. The same UID therefore maps to the same replacement wherever it appears, including nested references and files processed in separate invocations. The default root is `2.25`. A custom numeric root is accepted when the final UID can remain within the 64-character DICOM limit.

## Inserted attributes

The output contains:

- Patient Identity Removed `(0012,0062)` = `YES`;
- De-identification Method `(0012,0063)` with the profile, option, and implementation edition;
- De-identification Method Code Sequence `(0012,0064)` with DCM codes `113100` and `113107`;
- Longitudinal Temporal Information Modified `(0028,0303)` = `MODIFIED`.

File Meta Information is rebuilt. Source, sending, and receiving AE titles and private file-meta information are not copied. The 128-byte preamble is zeroed by the writer.

## Validator scope

The validator checks profile attestations, method codes, File Meta Information, private and group `0004` attributes, required removals, non-empty `D` and `U` replacements, UID syntax, repeating groups, and nested sequence items. It returns findings in stable path, tag, and code order.

The validator is not an Information Object Definition validator. It does not establish that all Type 1 and Type 2 requirements for a specific SOP Class are satisfied. It also cannot determine whether a retained or dummy string appeared in the original file. These limits are inherent when only the candidate output is available.

## Pixel and disclosure limits

Pixel Data is copied without decoding or inspection. The validator emits `pixel_data_not_assessed` when Pixel Data is present and `burned_in_annotation_present` when Burned In Annotation is `YES`. These are warnings because the selected metadata profile does not claim a pixel-cleaning option.

Do not treat a passing metadata report as authorization to disclose a file. DICOM PS3.15 states that applying an attribute confidentiality profile does not guarantee that the complete information object is de-identified. A release process still needs a use-context review, applicable legal analysis, re-identification risk assessment, and pixel review where images can contain identifying content.

## Regulatory use

ISO 13485:2016 specifies requirements for a medical-device quality management system. It does not certify an algorithm. The FDA Quality Management System Regulation incorporated ISO 13485:2016 by reference and became effective February 2, 2026. Organizations using this library in a regulated product remain responsible for design controls, risk management, software validation, configuration control, supplier control, complaint handling, records, and the other requirements that apply to that product.

Primary references:

- [DICOM PS3.15 Annex E](https://dicom.nema.org/medical/dicom/current/output/chtml/part15/chapter_E.html)
- [DICOM PS3.15 modified-dates option](https://dicom.nema.org/medical/dicom/current/output/chtml/part15/sect_e.3.6.html)
- [FDA Quality Management System Regulation](https://www.fda.gov/medical-devices/postmarket-requirements-devices/quality-management-system-regulation-qmsr)
