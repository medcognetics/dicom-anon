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
| `U` | In DICOM mode, derive a deterministic UID from the secret and original UID. In Safe Harbor mode, require the exact caller-supplied mapping, or assign a random replacement when the caller requests map generation. |
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

The caller must provide an independently assigned non-PHI patient scope. The tool derives a keyed stable subject token from it, stores the token in Clinical Trial Subject ID, and empties Patient ID and Patient Name. The caller must also provide a complete original-to-output UID map. Invalid UIDs, duplicate outputs, replacements equal to any source UID, and missing source mappings fail before an output is written. A missing-mapping error names the attribute path and not the UID value. `required_uids` and `--list-required-uids` list each source UID that the `U` action replaces, with its path, through the same rule decisions. Map generation assigns random version 4 UUID-derived replacements from the operating-system CSPRNG under the UID root and returns the complete map. These functions apply to metadata only.

For metadata the tool controls, the policy removes or safely replaces the Safe Harbor identifier classes: names, address/geographic attributes, direct identifiers, dates below year precision, device and institution identity, original UIDs, unapproved descriptions, and unknown person-name, text, URI, and UID values. A report may state that the configured Safe Harbor metadata policy passed. It does not state that the DICOM instance or repository is HIPAA compliant.

The exact Safe Harbor responsibilities outside this result are:

- Pixel samples, recognizable visual features, burned-in annotations, and identifying graphics encoded only in unsupported pixel formats are not inspected. The optional gates in [Pixel and disclosure limits](#pixel-and-disclosure-limits) reject input by declared attributes; they do not inspect pixels.
- The caller controls whether descriptor outputs, safe-private entries, the independently assigned patient scope, and replacement UID assignments contain or encode PHI.
- The covered entity must satisfy the Safe Harbor re-identification-code controls and must not disclose the key or mapping mechanism.
- The covered entity retains the required “actual knowledge” assessment that remaining information could not identify an individual.
- Data outside the DICOM Part 10 object, including filenames, paths, manifests, archives, logs, and linked databases, is outside this validator.

These are scope boundaries, not relaxed transformations of metadata the policy processes.

### Text attributes outside Table E.1-1

This section applies to metadata only. Pixel Data is not assessed.

In Safe Harbor metadata mode, an attribute that Table E.1-1 does not list and that has VR AE, LO, LT, PN, SH, ST, UC, UR, or UT receives the `C` action at every nesting level. An exact descriptor-map entry gives the output value. If no entry matches, the value becomes `ANONYMIZED` (`ANON^ANON` for PN). An unlisted UI attribute is remapped through the UID map. CS, numeric, and binary attributes outside the table are kept. The `dicom` and `dicom-extended` presets keep all of these attributes.

The table below lists the attributes in mammography and breast tomosynthesis objects that this rule changes when no descriptor-map entry matches. It is not a complete list. Any other attribute with one of the VRs above is changed in the same way.

| Attribute | Tag | VR | Inference effect without a map entry |
|---|---|---|---|
| Code Value | `(0008,0100)` | SH | The view, view modifier, and anatomic region code becomes `ANONYMIZED`. The coded entry is not valid, and View Code Sequence no longer identifies CC, MLO, or another view. |
| Coding Scheme Designator | `(0008,0102)` | SH | The coding scheme is lost. A consumer cannot interpret any code. |
| Coding Scheme Version | `(0008,0103)` | SH | The scheme version is lost. |
| Code Meaning | `(0008,0104)` | LO | The human-readable concept is lost. |
| Manufacturer | `(0008,0070)` | LO | Vendor-specific preprocessing cannot select the vendor. |
| Manufacturer's Model Name | `(0008,1090)` | LO | Model-specific preprocessing cannot select the model. |
| Software Versions | `(0018,1020)` | LO | Software-dependent processing cannot select the version. |
| Acquisition Device Processing Code | `(0018,1401)` | LO | The vendor processing code is lost. |
| Detector Description | `(0018,7006)` | LT | The detector description is lost. |
| Paddle Description | `(0018,11A4)` | LO | The compression paddle description is lost. |
| Window Center & Width Explanation | `(0028,1055)` | LO | The names of the VOI windows are lost. The window values are kept. |
| LUT Explanation | `(0028,3003)` | LO | The name of the VOI LUT is lost. The LUT data is kept. |

Code Value, Coding Scheme Designator, and Code Meaning are changed in every code sequence that is kept, which includes View Code Sequence `(0054,0220)`, View Modifier Code Sequence `(0054,0222)`, and Anatomic Region Sequence `(0008,2218)`, also when they are nested in functional group sequences.

#### Shipped breast-imaging descriptor map

`descriptor-maps/breast-imaging-ps3.16-2026d.json` keeps the standard coded values of these DICOM PS3.16 2026d context groups:

- CID 4013 Anatomic Region for Mammography;
- CID 4014 View for Mammography;
- CID 4015 View Modifier for Mammography;
- the Breast row of CID 4031 Common Anatomic Region.

Each entry maps a value to itself. The map contains the SCT Code Value, the SRT value from the SNOMED-RT ID column of the same row, the exact Code Meaning text, and the designators `DCM`, `SCT`, and `SRT`. No row of these context groups uses the DCM scheme in 2026d. `tools/generate_breast_descriptor_map.py` regenerates the file and fails if the published standard is not the 2026d edition. Do not edit the file by hand.

A descriptor-map entry applies to its tag at every nesting level. Thus a value in the shipped map is also kept when it occurs in a different code sequence. All values in the map are standard terminology, not identifiers. A Code Meaning from an earlier edition of the standard, for example a different capitalization, does not match the map. Add such values in a separate reviewed map. Device attributes such as Manufacturer are not in the shipped map, because their values are site-specific. Review and add them in a separate map.

#### Detection and fail-closed control

The anonymization report gives two per-tag counts at every nesting level:

- `dummy_replacements`: all dummy values, including dummy values that Table E.1-1 or an option column requires;
- `unmapped_dummy_replacements`: dummy values written because a `C` action found no descriptor-map entry.

`--fail-on-unmapped TAG`, or the `fail_on_unmapped` configuration field, stops the run when an attribute with that tag gets a dummy value because no descriptor-map entry matched. The command writes no output and exits with status 2. The error gives the tag and the sequence path, for example `(0054,0220)[0]/(0054,0222)[0]/(0008,0100)`. It does not give the value. Dummy values that the profile requires do not stop the run, because the descriptor map does not apply to them. They are counted only in `dummy_replacements`. These include:

- attributes with the `D` action, and the items of a `D` sequence, such as Institution Code Sequence `(0008,0082)`;
- the items of a sequence that Clean Descriptors or Clean Structured Content replaces as a whole, such as Acquisition Context Sequence `(0040,0555)` and the diagnosis and reason code sequences.

Use `dummy_replacements` to detect a change to coded content in these sequences.

The guard applies only to anonymization. `dicom-validate` rejects `--fail-on-unmapped`. Library and binding validation ignore `fail_on_unmapped`, because a dummy value in a candidate does not show whether a descriptor-map miss or a profile requirement produced it.

The descriptor map and the guard apply to a tag at every nesting level, not to a sequence path. Thus `--fail-on-unmapped 00080100` also stops the run for any other kept code sequence whose codes are not in a map. MG and DBT objects often contain such sequences, for example Purpose of Reference Code Sequence `(0040,A170)` in Source Image Sequence `(0008,2112)`, Derivation Code Sequence `(0008,9215)`, and Partial View Code Sequence `(0028,1352)`. The shipped map does not contain these codes.

For an inference pipeline:

1. Run Safe Harbor anonymization with the shipped map and without the guard on a representative synthetic or approved sample. Read `unmapped_dummy_replacements` in the reports.
2. For each other code sequence that the sample contains, add a reviewed map with the standard codes of its context group.
3. Then use `--fail-on-unmapped` for `00080100`, `00080102`, and `00080104`. Add a reviewed map and `--fail-on-unmapped` entries for the device attributes that the model uses.

## Inserted attributes

Every output contains Patient Identity Removed `(0012,0062)`=`YES`; De-identification Method `(0012,0063)`; De-identification Method Code Sequence `(0012,0064)` with DCM `113100` and exactly one CID 7050 code per active implemented option; and Longitudinal Temporal Information Modified `(0028,0303)`=`MODIFIED`, `UNMODIFIED`, or `REMOVED`. Safe Harbor output also contains the keyed Clinical Trial Subject ID.

Pixel-option codes are never emitted. Source, sending, and receiving AE titles and private File Meta Information are not copied. The writer emits a zeroed 128-byte preamble.

## Validator scope

Candidate-only validation uses an explicit caller configuration rather than trusting the candidate’s claims. It checks the exact De-identification Method text and DCM code items, the temporal marker, the Safe Harbor subject-token format, profile actions, cleaned descriptors, age syntax and ceiling, temporal syntax, graphics and structured-content cleaning, safe-private membership, unknown/private/group `0004` handling, file meta, and UID syntax recursively. It emits one aggregate `source_not_compared` warning because exact `K`, `C`, and `U` relationships cannot be proven without the original.

Source-aware validation reconstructs the exact expected output from the source, key, patient scope, profile, descriptor map, safe-private registry, and UID map, then compares every nested attribute path. Findings and errors contain only path, tag, rule category, and mismatch category. They never contain original values, key material, patient scope, or mapping contents. Cross-file consistency remains a caller-level batch check.

The validator is a confidentiality-policy validator, not a complete SOP Class or IOD validator. A passing report does not establish that Type 1 and Type 2 requirements for a particular SOP Class are satisfied.

## Verified mammography object types

The `core/tests/mammography` suite generates synthetic Digital Mammography X-Ray Image Storage - For Presentation, Breast Tomosynthesis Image Storage with two frames and functional groups, and a synthesized 2D image that references the tomosynthesis instance. All three use JPEG 2000 Lossless encapsulated Pixel Data. The suite processes each object with the `dicom`, `dicom-extended`, and `safe-harbor-metadata` policies. It checks metadata only:

- No placeholder identifier or source UID remains in File Meta Information or in any nested item.
- Transfer Syntax UID, Image Pixel module attributes, the Basic Offset Table, and the Pixel Data fragments do not change.
- Image Laterality, Laterality, View Position, Presentation Intent Type, Image Type, Frame Type, and Frame Laterality do not change.
- In Safe Harbor mode, every nested DA, DT, and TM value is generalized to year level, and Patient Age `092Y` becomes `090Y`.
- The Source Image Sequence, Dimension Organization UID, and File Meta SOP Instance UID stay consistent with the output instances.
- Source-aware validation passes through the library and through `dicom-validate`.

In Safe Harbor mode, the View Code Sequence code values are replaced with `ANONYMIZED` at this revision. Issue #20 tracks this behavior.

## Pixel and disclosure limits

The tool does not decode pixel samples and does not claim the Clean Pixel Data Option. The validator emits the warning `pixel_data_not_assessed` when Pixel Data is present. DICOM PS3.15 notes that applying an attribute confidentiality profile does not guarantee that the complete information object is de-identified.

The tool controls these pixel risks:

| Risk | Control | Validator finding |
|---|---|---|
| Burned In Annotation `(0028,0301)` is `YES`, empty, absent, or another value | Optional `require_no_burned_in_annotation` gate. Only the exact value `NO` passes. The anonymizer fails and writes no output. | `burned_in_annotation_not_no` error when the gate is active. Without the gate, `YES` gives the `burned_in_annotation_present` warning. |
| SOP Class where burned-in text is expected | Optional `sop_class_allowlist` gate. An absent or other SOP Class UID fails, and no output is written. The Media Storage SOP Class UID `(0002,0002)` in the File Meta Information must also equal the SOP Class UID `(0008,0016)`, so a data-set value that differs from the declared file class cannot pass. An empty list disables the gate. | `sop_class_not_allowed` error when the gate is active. |
| JPEG 2000 comment (COM, `0xFF64`) marker segments | Always active for transfer syntaxes `1.2.840.10008.1.2.4.90`, `.91`, `.92`, `.93`, `.201`, `.202`, and `.203`. The anonymizer removes COM segments from the main header and every tile-part header and decrements each affected `Psot`. Entropy-coded tile data is copied byte for byte, so decoded samples do not change. | `jpeg2000_comment_present` error. |
| JPEG 2000 codestream that cannot be rewritten safely | The anonymizer fails and writes no output. This includes a codestream without `SOC` and `SIZ`, a JP2 file wrapper, a truncated or out-of-range marker segment or tile-part, data after `EOC` other than zero padding, a marker code other than `SOP` or `EPH` in tile-part data (for example, a second `EOC` and a COM segment appended to a frame), multi-frame Pixel Data without a Basic Offset Table or one fragment per frame, a Basic Offset Table that does not match fragment boundaries, a tile-part comment with a `TLM` segment, and a comment in an object with an Extended Offset Table or Extended Offset Table Lengths. | `jpeg2000_codestream_unparseable` error. |
| JPEG and JPEG-LS comment (COM, `0xFFFE`) marker segments | Always active for JPEG transfer syntaxes `1.2.840.10008.1.2.4.50`, `.51`, `.57`, and `.70`, and JPEG-LS transfer syntaxes `.80` and `.81`. The anonymizer removes every COM segment before and between scans. Input with a retired JPEG transfer syntax (`.52` to `.56` and `.58` to `.66`) fails when the file is read, and no output is written. | `jpeg_comment_present` error. |
| JPEG and JPEG-LS application (APPn, `0xFFE0` to `0xFFEF`) marker segments | Always active for the same transfer syntaxes. The anonymizer removes every APPn segment, including JFIF, JFXX and Exif thumbnails, ICC profiles, and SPIFF headers. Exception: for a frame with three or four components, it keeps one APP14 `Adobe` segment of the standard 12-byte payload, because its color transform flag changes the decoded colors. Entropy-coded scan data, restart markers, and fill bytes are copied byte for byte, so decoded samples do not change. | `jpeg_application_segment_present` error. A kept APP14 `Adobe` segment gives no finding. |
| JPEG or JPEG-LS codestream that cannot be rewritten safely | The anonymizer fails and writes no output. This includes a codestream without `SOI`, a frame header, or a scan; data after `EOI` other than zero padding, such as a second image; a truncated marker segment; a frame header with an inconsistent length; more than one frame header; a hierarchical, differential, or other frame type that does not match the transfer syntax family; a marker that is not valid in the header of that family; a JPEG-LS `mrfx` color transform segment in a frame with three or four components; an APP14 `Adobe` segment with a non-standard length, more than one, or one after the first scan, in such a frame; and a three-component JPEG frame where JFIF removal would change the color space that libjpeg-turbo infers at the first scan, including the RGB default for lossless frames. The frame, offset table, and Extended Offset Table rules are the same as for JPEG 2000. | `jpeg_codestream_unparseable` error. |

When no removable segment is present, the anonymizer copies the encapsulated Pixel Data unchanged. When it removes a segment, it writes one fragment for each frame, pads odd lengths with one zero byte, and rebuilds the Basic Offset Table if the source had one. An unchanged frame keeps its source bytes. If the source has Encapsulated Pixel Data Value Total Length `(7FE0,0003)`, the anonymizer replaces it with the length of the rewritten fragments, without the trailing padding of the last fragment (PS3.3 C.7.6.6). The gate result is not written to the De-identification Method or its code sequence.

These pixel risks stay with the caller:

- burned-in text, faces, and other identifying content in the pixel samples, also when Burned In Annotation is `NO`, because that value is a source declaration that the tool cannot verify;
- metadata segments in JPEG XL, RLE Lossless, Deflated Image Frame Compression, MPEG and HEVC video, and other encapsulated transfer syntaxes that are not in the table above;
- JPEG 2000 data in the entropy-coded tile-part bodies, and markers or boxes that are not COM segments;
- JPEG and JPEG-LS data in the entropy-coded scans, the numeric content of kept table, frame, scan, and APP14 `Adobe` segments, and the identity of the source encoder that the table values can disclose;
- Pixel Data in nested items, such as icon images, which the profile removes, and pixel data in referenced or external files;
- the choice of SOP Classes in the allowlist and the review of any input that a gate rejects.

Primary references:

- [DICOM PS3.15 Annex E](https://dicom.nema.org/medical/dicom/current/output/chtml/part15/chapter_E.html)
- [DICOM PS3.15 temporal options](https://dicom.nema.org/medical/dicom/current/output/chtml/part15/sect_e.3.6.html)
- [DICOM PS3.15 Clean Descriptors](https://dicom.nema.org/medical/dicom/current/output/chtml/part15/sect_e.3.5.html)
- [DICOM PS3.15 Retain Safe Private](https://dicom.nema.org/medical/dicom/current/output/chtml/part15/sect_e.3.10.html)
- [CID 4013 Anatomic Region for Mammography](https://dicom.nema.org/medical/dicom/current/output/chtml/part16/sect_CID_4013.html)
- [CID 4014 View for Mammography](https://dicom.nema.org/medical/dicom/current/output/chtml/part16/sect_CID_4014.html)
- [CID 4015 View Modifier for Mammography](https://dicom.nema.org/medical/dicom/current/output/chtml/part16/sect_CID_4015.html)
- [CID 4031 Common Anatomic Region](https://dicom.nema.org/medical/dicom/current/output/chtml/part16/sect_CID_4031.html)
- [CID 7050 De-identification Method](https://dicom.nema.org/medical/dicom/current/output/chtml/part16/sect_CID_7050.html)
- [HHS guidance on de-identification](https://www.hhs.gov/hipaa/for-professionals/special-topics/de-identification/index.html)
