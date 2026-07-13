# DICOM confidentiality conformance statement

## Profile and configuration

The implementation targets DICOM PS3.15 2026c Table E.1-1 and Table E.1-1a. The `dicom` preset applies the Basic Application Level Confidentiality Profile with no option columns. The default `dicom-extended` preset adds these options:

- Clean Descriptors, DCM `113105`;
- Retain Longitudinal Temporal Information With Modified Dates, DCM `113107`;
- Retain Patient Characteristics, DCM `113108`.

Both presets use the DICOM policy. Serialized reports identify the selected behavior through `policy: "dicom"` and the exact `active_options` list. The Safe Harbor metadata preset remains a separate policy because it applies additional transformations and requires external linkage mappings.

The frozen matrix contains all 652 exact-tag rows and every metadata option column. The generator also freezes the 457 unique creator/offset entries in Table E.3.10-1. `tools/generate_profile.py` checks that its source identifies itself as PS3.15 2026c before replacing `core/src/profile_generated.rs`.

The following metadata options are configurable: Clean Graphics, Clean Structured Content, Clean Descriptors, Retain Full Dates, Retain Modified Dates, Retain Patient Characteristics, Retain Device Identity, Retain Institution Identity, Retain UIDs, and Retain Safe Private. Clean Pixel Data and Clean Recognizable Visual Features are represented in configuration but always rejected. Full Dates and Modified Dates are mutually exclusive. Any pair of enabled option columns that assigns different actions to the same attribute is rejected with the affected tag and options.

## Action implementation

| PS3.15 action | Implementation |
|---|---|
| `X` | Remove the attribute and its sequence content. |
| `Z` | Write a zero-length value. Safe Harbor also requires empty Patient ID and Patient Name. |
| `D` | Write a non-empty VR-compatible dummy. For sequences, retain item structure and recursively dummy every non-private value. |
| `U` | In DICOM mode, derive a deterministic UID from the secret and original UID. In Safe Harbor mode, require the exact caller-supplied mapping. |
| `K` | Keep primitive values and recursively process sequence items. |
| `C` | Apply the transformation associated with the enabled option: descriptor mapping, age category, temporal transformation, or recursive content cleaning. |
| compound actions | Select the non-removal action needed by Type 1/2 uses: `D` for `Z/D`, `X/D`, and `X/Z/D`; `Z` for `X/Z`; recursive UID handling for `X/Z/U*`. |

Patient ID receives a keyed pseudonym in normal DICOM mode so distinct patients do not collapse to one dummy. Patient Age retains a valid source AS value except that every yearly age from `090Y` through `999Y` becomes `090Y`. Invalid or multi-valued ages fail closed.

Clean Descriptors uses an exact canonical-tag and exact-source-value mapping. A configured output is checked for a non-empty, null-free, VR-compatible length. An unmatched optional value is removed when the table permits removal; otherwise a canonical VR-compatible dummy is written. No source descriptor is trusted merely because it looks generic.

Clean Graphics removes governed curves, overlays, annotations, and graphics. Clean Structured Content conservatively replaces governed SR, Acquisition Context, and Specimen Preparation content with safe VR-compatible values while retaining required sequence shape. Unrecognized textual content is not copied.

Retain Safe Private accepts entries from the frozen PS3.15 safe-private table, a caller allowlist, and valid Private Data Element Characteristics Sequence declarations whose status is `SAFE` or whose `MIXED` item lists the low-byte offset. Only the matching private creator and declared elements are retained. All other odd-group elements are removed recursively. Safe private sequences are parsed and their nested attributes are independently processed.

Group `0004` attributes are removed because this tool does not create or update DICOMDIR files. Curve and overlay repeating groups use their repeating-group rules. Unknown DA, DT, and TM attributes receive the configured temporal safety action. File Meta Information is rebuilt rather than copied.

## Temporal behavior

Normal DICOM Modified Dates mode applies stable patient-scoped whole-day and whole-hour offsets. The same key and patient scope preserve longitudinal ordering. Known DA/TM pairs carry midnight rollover into the paired DA. A multiplicity mismatch fails closed. Full Dates keeps temporal values byte-for-value and emits `UNMODIFIED`; disabling both temporal options applies the Basic Profile and emits `REMOVED`.

Safe Harbor metadata mode uses the DICOM Modified Dates option as a year-aggregation mechanism:

- DA becomes `YYYY0101`;
- DT becomes `YYYY`;
- TM becomes `000000`;
- Patient Birth Date is empty;
- Patient Age is `090Y` for age 90 or older.

The month, day, and time are synthetic or removed. Only the source year is retained.

## Strict Safe Harbor metadata policy

The Safe Harbor preset enables Clean Graphics, Clean Structured Content, Clean Descriptors, Retain Patient Characteristics, and Retain Modified Dates. These options may be disabled for more conservative option-column actions; the Safe Harbor-specific transformations still apply. The policy rejects Full Dates, Retain Device Identity, Retain Institution Identity, Retain UIDs, and both unsupported pixel options.

The caller must provide an independently assigned non-PHI patient scope. The tool derives a keyed stable subject token from it, stores the token in Clinical Trial Subject ID, and empties Patient ID and Patient Name. The caller must also provide a complete original-to-output UID map. Invalid UIDs, duplicate outputs, replacements equal to any source UID, and missing source mappings fail before an output is written.

For metadata the tool controls, the policy removes or safely replaces the Safe Harbor identifier classes: names, address/geographic attributes, direct identifiers, dates below year precision, device and institution identity, original UIDs, unapproved descriptions, and unknown person-name, text, URI, and UID values. A report may state that the configured Safe Harbor metadata policy passed. It does not state that the DICOM instance or repository is HIPAA compliant.

The exact Safe Harbor responsibilities outside this result are:

- Pixel Data, recognizable visual features, burned-in annotations, and identifying graphics encoded only in unsupported pixel formats are not inspected.
- The caller controls whether descriptor outputs, safe-private entries, the independently assigned patient scope, and replacement UID assignments contain or encode PHI.
- The covered entity must satisfy the Safe Harbor re-identification-code controls and must not disclose the key or mapping mechanism.
- The covered entity retains the required “actual knowledge” assessment that remaining information could not identify an individual.
- Data outside the DICOM Part 10 object, including filenames, paths, manifests, archives, logs, and linked databases, is outside this validator.

These are scope boundaries, not relaxed transformations of metadata the policy processes.

## Inserted attributes

Every output contains Patient Identity Removed `(0012,0062)`=`YES`; De-identification Method `(0012,0063)`; De-identification Method Code Sequence `(0012,0064)` with DCM `113100` and exactly one CID 7050 code per active implemented option; and Longitudinal Temporal Information Modified `(0028,0303)`=`MODIFIED`, `UNMODIFIED`, or `REMOVED`. Safe Harbor output also contains the keyed Clinical Trial Subject ID.

Pixel-option codes are never emitted. Source, sending, and receiving AE titles and private File Meta Information are not copied. The writer emits a zeroed 128-byte preamble.

## Validator scope

Candidate-only validation uses an explicit caller configuration rather than trusting the candidate’s claims. It checks the exact De-identification Method text and DCM code items, the temporal marker, the Safe Harbor subject-token format, profile actions, cleaned descriptors, age syntax and ceiling, temporal syntax, graphics and structured-content cleaning, safe-private membership, unknown/private/group `0004` handling, file meta, and UID syntax recursively. It emits one aggregate `source_not_compared` warning because exact `K`, `C`, and `U` relationships cannot be proven without the original.

Source-aware validation reconstructs the exact expected output from the source, key, patient scope, profile, descriptor map, safe-private registry, and UID map, then compares every nested attribute path. Findings and errors contain only path, tag, rule category, and mismatch category. They never contain original values, key material, patient scope, or mapping contents. Cross-file consistency remains a caller-level batch check.

The validator is a confidentiality-policy validator, not a complete SOP Class or IOD validator. A passing report does not establish that Type 1 and Type 2 requirements for a particular SOP Class are satisfied.

## Pixel and disclosure limits

Pixel Data is copied without decoding or inspection. The validator emits `pixel_data_not_assessed` when Pixel Data is present and `burned_in_annotation_present` when Burned In Annotation is `YES`. DICOM PS3.15 notes that applying an attribute confidentiality profile does not guarantee that the complete information object is de-identified.

Primary references:

- [DICOM PS3.15 Annex E](https://dicom.nema.org/medical/dicom/current/output/chtml/part15/chapter_E.html)
- [DICOM PS3.15 temporal options](https://dicom.nema.org/medical/dicom/current/output/chtml/part15/sect_e.3.6.html)
- [DICOM PS3.15 Clean Descriptors](https://dicom.nema.org/medical/dicom/current/output/chtml/part15/sect_e.3.5.html)
- [DICOM PS3.15 Retain Safe Private](https://dicom.nema.org/medical/dicom/current/output/chtml/part15/sect_e.3.10.html)
- [CID 7050 De-identification Method](https://dicom.nema.org/medical/dicom/current/output/chtml/part16/sect_CID_7050.html)
- [HHS guidance on de-identification](https://www.hhs.gov/hipaa/for-professionals/special-topics/de-identification/index.html)
