//! Fail-closed pixel-risk gates and codestream metadata-segment removal.
//!
//! The tool does not decode or inspect pixel samples. These checks control only
//! the declared Burned In Annotation value, the SOP Class, JPEG 2000 comment (COM)
//! marker segments, and JPEG and JPEG-LS comment (COM) and application (APPn)
//! marker segments. These segments can carry text outside the image samples.

use std::borrow::Cow;
use std::ops::{Range, RangeInclusive};

use dicom_core::value::{PixelFragmentSequence, Value};
use dicom_core::{PrimitiveValue, Tag, VR};
use dicom_dictionary_std::tags;
use dicom_object::mem::{InMemDicomObject, InMemElement};

use crate::profile::ProfileConfiguration;
use crate::{Error, Result};

/// JPEG 2000 Part 1, Part 2, and HTJ2K transfer syntaxes with in-band codestreams.
const JPEG_2000_TRANSFER_SYNTAXES: [&str; 7] = [
    "1.2.840.10008.1.2.4.90",
    "1.2.840.10008.1.2.4.91",
    "1.2.840.10008.1.2.4.92",
    "1.2.840.10008.1.2.4.93",
    "1.2.840.10008.1.2.4.201",
    "1.2.840.10008.1.2.4.202",
    "1.2.840.10008.1.2.4.203",
];
/// JPEG (ITU-T T.81) transfer syntaxes: baseline, extended, and lossless Process 14 (`.50`,
/// `.51`, `.57`, `.70`), and the retired `.52` to `.56` and `.58` to `.66`. The DICOM parser
/// does not read the retired ones now; they are listed so that a parser that reads them cleans
/// the non-hierarchical ones and fails closed on the hierarchical DHP, EXP, and differential
/// frame headers.
const JPEG_TRANSFER_SYNTAXES: [&str; 18] = [
    "1.2.840.10008.1.2.4.50",
    "1.2.840.10008.1.2.4.51",
    "1.2.840.10008.1.2.4.52",
    "1.2.840.10008.1.2.4.53",
    "1.2.840.10008.1.2.4.54",
    "1.2.840.10008.1.2.4.55",
    "1.2.840.10008.1.2.4.56",
    "1.2.840.10008.1.2.4.57",
    "1.2.840.10008.1.2.4.58",
    "1.2.840.10008.1.2.4.59",
    "1.2.840.10008.1.2.4.60",
    "1.2.840.10008.1.2.4.61",
    "1.2.840.10008.1.2.4.62",
    "1.2.840.10008.1.2.4.63",
    "1.2.840.10008.1.2.4.64",
    "1.2.840.10008.1.2.4.65",
    "1.2.840.10008.1.2.4.66",
    "1.2.840.10008.1.2.4.70",
];
/// JPEG-LS (ITU-T T.87) lossless and near-lossless transfer syntaxes.
const JPEG_LS_TRANSFER_SYNTAXES: [&str; 2] = ["1.2.840.10008.1.2.4.80", "1.2.840.10008.1.2.4.81"];
const EXTENDED_OFFSET_TABLE: Tag = Tag(0x7FE0, 0x0001);
const EXTENDED_OFFSET_TABLE_LENGTHS: Tag = Tag(0x7FE0, 0x0002);

const SOC: u16 = 0xFF4F;
const SIZ: u16 = 0xFF51;
const TLM: u16 = 0xFF55;
const COM: u16 = 0xFF64;
const SOT: u16 = 0xFF90;
const SOD: u16 = 0xFF93;
const EOC: u16 = 0xFFD9;

const SOI: u16 = 0xFFD8;
const EOI: u16 = 0xFFD9;
const SOS: u16 = 0xFFDA;
const DHT: u16 = 0xFFC4;
const DAC: u16 = 0xFFCC;
const DNL: u16 = 0xFFDC;
const DQT: u16 = 0xFFDB;
const DRI: u16 = 0xFFDD;
const APP0: u16 = 0xFFE0;
const APP14: u16 = 0xFFEE;
const APP15: u16 = 0xFFEF;
const SOF55: u16 = 0xFFF7;
const LSE: u16 = 0xFFF8;
const JPEG_COM: u16 = 0xFFFE;
/// Non-differential T.81 frame headers: Huffman SOF0 to SOF3, arithmetic SOF9 to SOF11.
const JPEG_FRAME_HEADERS: [u16; 7] = [0xFFC0, 0xFFC1, 0xFFC2, 0xFFC3, 0xFFC9, 0xFFCA, 0xFFCB];

/// Internal parse result. The reason becomes the codestream-specific error.
type Parse<T> = std::result::Result<T, &'static str>;

/// A pixel-gate condition that the configuration rejects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PixelGateViolation {
    BurnedInAnnotationNotNo,
    SopClassNotAllowed,
}

/// Return every configured gate that the object fails.
///
/// `media_storage_sop_class` is the File Meta Information Media Storage SOP Class UID, if
/// the object has File Meta Information. The SOP Class allowlist passes only when it and the
/// data-set SOP Class UID agree, so a spoofed data-set value cannot pass the gate.
pub(crate) fn gate_violations(
    object: &InMemDicomObject,
    media_storage_sop_class: Option<&str>,
    profile: &ProfileConfiguration,
) -> Vec<PixelGateViolation> {
    let mut violations = Vec::new();
    if profile.require_no_burned_in_annotation
        && text(object, tags::BURNED_IN_ANNOTATION).as_deref() != Some("NO")
    {
        violations.push(PixelGateViolation::BurnedInAnnotationNotNo);
    }
    if !profile.sop_class_allowlist.is_empty()
        && !text(object, tags::SOP_CLASS_UID).is_some_and(|uid| {
            profile.sop_class_allowlist.contains(&uid)
                && media_storage_sop_class
                    .is_none_or(|meta| meta.trim_end_matches(['\0', ' ']) == uid)
        })
    {
        violations.push(PixelGateViolation::SopClassNotAllowed);
    }
    violations
}

/// Fail closed on the first configured gate that the object fails.
pub(crate) fn check_gates(
    object: &InMemDicomObject,
    media_storage_sop_class: Option<&str>,
    profile: &ProfileConfiguration,
) -> Result<()> {
    match gate_violations(object, media_storage_sop_class, profile).first() {
        Some(PixelGateViolation::BurnedInAnnotationNotNo) => Err(Error::BurnedInAnnotationNotNo),
        Some(PixelGateViolation::SopClassNotAllowed) => Err(Error::SopClassNotAllowed),
        None => Ok(()),
    }
}

/// Encapsulated codestream family whose metadata segments the anonymizer removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Codestream {
    Jpeg2000,
    Jpeg,
    JpegLs,
}

impl Codestream {
    /// Return the codestream family that a transfer syntax requires, if any.
    #[must_use]
    pub(crate) fn for_transfer_syntax(transfer_syntax: &str) -> Option<Self> {
        let transfer_syntax = transfer_syntax.trim_end_matches(['\0', ' ']);
        if JPEG_2000_TRANSFER_SYNTAXES.contains(&transfer_syntax) {
            Some(Self::Jpeg2000)
        } else if JPEG_TRANSFER_SYNTAXES.contains(&transfer_syntax) {
            Some(Self::Jpeg)
        } else if JPEG_LS_TRANSFER_SYNTAXES.contains(&transfer_syntax) {
            Some(Self::JpegLs)
        } else {
            None
        }
    }

    fn error(self, reason: &str) -> Error {
        match self {
            Self::Jpeg2000 => Error::UnsupportedJpeg2000(reason.to_owned()),
            Self::Jpeg | Self::JpegLs => Error::UnsupportedJpeg(reason.to_owned()),
        }
    }

    fn strip(self, codestream: &[u8]) -> Parse<(Vec<u8>, RemovedSegments)> {
        match self {
            Self::Jpeg2000 => strip_comments(codestream).map(|(output, comments)| {
                (
                    output,
                    RemovedSegments {
                        comments,
                        applications: 0,
                    },
                )
            }),
            Self::Jpeg => strip_jpeg_segments(codestream, false),
            Self::JpegLs => strip_jpeg_segments(codestream, true),
        }
    }
}

/// Numbers of marker segments removed from every frame of one Pixel Data element.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RemovedSegments {
    /// COM marker segments.
    pub comments: u64,
    /// APPn marker segments. Always zero for JPEG 2000.
    pub applications: u64,
}

impl RemovedSegments {
    fn is_empty(self) -> bool {
        self.comments == 0 && self.applications == 0
    }
}

/// Result of metadata-segment removal for one Pixel Data element.
#[derive(Debug)]
pub(crate) struct CodestreamCleaning {
    /// Replacement Pixel Data and, when present in the source, Encapsulated Pixel Data Value
    /// Total Length. Empty when no removable segment was present.
    pub replacement: Vec<InMemElement>,
    /// Removed marker segments.
    pub removed: RemovedSegments,
}

/// Parse every frame codestream and remove its removable metadata segments.
///
/// Any codestream structure that cannot be parsed and rewritten safely is an error.
pub(crate) fn clean_codestreams(
    object: &InMemDicomObject,
    kind: Codestream,
) -> Result<CodestreamCleaning> {
    clean_frames(object, kind).map_err(|reason| kind.error(reason))
}

/// Count removable metadata segments in every frame without changing the object.
pub(crate) fn count_removable_segments(
    object: &InMemDicomObject,
    kind: Codestream,
) -> Result<RemovedSegments> {
    encapsulated_frames(object)
        .and_then(|frames| match frames {
            Some((_, _, frames)) => {
                let stripped = strip_frames(&frames, kind, false)?;
                check_extended_offset_table(object, stripped.removed)?;
                Ok(stripped.removed)
            }
            None => Ok(RemovedSegments::default()),
        })
        .map_err(|reason| kind.error(reason))
}

/// Pixel Data element, its fragment sequence, and the fragments of each frame.
type Frames<'a> = Option<(
    &'a InMemElement,
    &'a PixelFragmentSequence<Vec<u8>>,
    Vec<&'a [Vec<u8>]>,
)>;

fn encapsulated_frames(object: &InMemDicomObject) -> Parse<Frames<'_>> {
    let Some(element) = object.get(tags::PIXEL_DATA) else {
        return Ok(None);
    };
    let Value::PixelSequence(sequence) = element.value() else {
        return Err("Pixel Data is not encapsulated");
    };
    let frames = frame_fragments(object, sequence)?;
    Ok(Some((element, sequence, frames)))
}

/// Result of one parse over every frame.
struct StrippedFrames {
    removed: RemovedSegments,
    /// Rewritten codestream of each changed frame, and `None` for each unchanged frame.
    changed: Vec<Option<Vec<u8>>>,
}

/// Parse every frame once and count its removable segments.
///
/// Without `keep_changed`, `changed` stays empty, and no rewritten frame is retained.
fn strip_frames(
    frames: &[&[Vec<u8>]],
    kind: Codestream,
    keep_changed: bool,
) -> Parse<StrippedFrames> {
    let mut removed = RemovedSegments::default();
    let mut changed = Vec::with_capacity(if keep_changed { frames.len() } else { 0 });
    for frame in frames {
        let (output, frame_removed) = kind.strip(&frame_codestream(frame))?;
        removed.comments += frame_removed.comments;
        removed.applications += frame_removed.applications;
        if keep_changed {
            changed.push((!frame_removed.is_empty()).then_some(output));
        }
    }
    Ok(StrippedFrames { removed, changed })
}

/// Fragment byte total without, and with, the trailing zero bytes of the last fragment.
///
/// PS3.3 C.7.6.6 counts every fragment, without the trailing padding of the last fragment.
fn fragment_total_bounds(fragments: &[Vec<u8>]) -> (u64, u64) {
    let total = fragments.iter().map(Vec::len).sum::<usize>();
    let last_zeros = fragments
        .last()
        .map_or(0, |last| last.len() - without_zero_padding(last));
    ((total - last_zeros) as u64, total as u64)
}

/// Encapsulated Pixel Data Value Total Length of rewritten fragments.
///
/// Each codestream ends with a marker, so trailing zero bytes are padding.
fn encapsulated_total_length(fragments: &[Vec<u8>]) -> InMemElement {
    InMemElement::new(
        tags::ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH,
        VR::UV,
        PrimitiveValue::U64(vec![fragment_total_bounds(fragments).0].into()),
    )
}

/// Valid Encapsulated Pixel Data Value Total Length values under PS3.3 C.7.6.6, or `None`
/// when Pixel Data is absent or native.
///
/// For a codestream transfer syntax, every trailing zero byte of the last fragment is
/// padding, because each codestream ends with a marker. For another encapsulated transfer
/// syntax, a trailing zero byte can be data or the one alignment byte, so the value can
/// exclude at most one zero byte.
fn valid_total_lengths(
    object: &InMemDicomObject,
    transfer_syntax: &str,
) -> Option<RangeInclusive<u64>> {
    let Some(Value::PixelSequence(sequence)) =
        object.get(tags::PIXEL_DATA).map(InMemElement::value)
    else {
        return None;
    };
    let (unpadded, total) = fragment_total_bounds(sequence.fragments());
    Some(
        if Codestream::for_transfer_syntax(transfer_syntax).is_some() {
            unpadded..=unpadded
        } else {
            total.saturating_sub(u64::from(unpadded < total))..=total
        },
    )
}

/// Return whether Encapsulated Pixel Data Value Total Length (7FE0,0003) is present and is
/// not one UV value in [`valid_total_lengths`].
pub(crate) fn total_length_mismatch(object: &InMemDicomObject, transfer_syntax: &str) -> bool {
    total_length_check(object, transfer_syntax).is_some()
}

/// `None` when (7FE0,0003) is absent or valid. Otherwise, the valid values, or `None` inside
/// when Pixel Data is absent or native.
fn total_length_check(
    object: &InMemDicomObject,
    transfer_syntax: &str,
) -> Option<Option<RangeInclusive<u64>>> {
    let element = object.get(tags::ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH)?;
    let valid = valid_total_lengths(object, transfer_syntax);
    let matches = element.vr() == VR::UV
        && match (element.to_multi_int::<u64>().as_deref(), &valid) {
            (Ok([value]), Some(valid)) => valid.contains(value),
            _ => false,
        };
    (!matches).then_some(valid)
}

/// Change made by [`repair_total_length`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TotalLengthRepair {
    Replaced,
    Removed,
}

/// Replace a mismatched Encapsulated Pixel Data Value Total Length (7FE0,0003) with the
/// one valid value. Remove the optional attribute when Pixel Data is absent or native, or
/// when a final zero byte can be either data or padding.
pub(crate) fn repair_total_length(
    object: &mut InMemDicomObject,
    transfer_syntax: &str,
) -> Option<TotalLengthRepair> {
    match total_length_check(object, transfer_syntax)? {
        Some(valid) if valid.start() == valid.end() => {
            object.put(InMemElement::new(
                tags::ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH,
                VR::UV,
                PrimitiveValue::U64(vec![*valid.start()].into()),
            ));
            Some(TotalLengthRepair::Replaced)
        }
        _ => {
            object.remove_element(tags::ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH);
            Some(TotalLengthRepair::Removed)
        }
    }
}

/// Remove Planar Configuration (0028,0006) when Samples per Pixel (0028,0002) is 1. PS3.3
/// C.7.6.3.1.3 defines Planar Configuration only for more than one sample per pixel. Returns
/// whether the attribute was removed.
pub(crate) fn remove_single_sample_planar_configuration(object: &mut InMemDicomObject) -> bool {
    let single_sample = object
        .get(tags::SAMPLES_PER_PIXEL)
        .and_then(|element| element.to_int::<u16>().ok())
        == Some(1);
    single_sample && object.remove_element(tags::PLANAR_CONFIGURATION)
}

/// Fail when segment removal would change fragment offsets or lengths that an Extended Offset
/// Table or its lengths attribute lists, also when only one of the two is present.
fn check_extended_offset_table(object: &InMemDicomObject, removed: RemovedSegments) -> Parse<()> {
    let extended = [EXTENDED_OFFSET_TABLE, EXTENDED_OFFSET_TABLE_LENGTHS];
    if !removed.is_empty() && extended.iter().any(|&tag| object.get(tag).is_some()) {
        return Err("segment removal would invalidate the Extended Offset Table");
    }
    Ok(())
}

/// Join the fragments of one frame. A single-fragment frame is borrowed without a copy.
fn frame_codestream(frame: &[Vec<u8>]) -> Cow<'_, [u8]> {
    match frame {
        [fragment] => Cow::Borrowed(fragment),
        _ => Cow::Owned(frame.concat()),
    }
}

fn clean_frames(object: &InMemDicomObject, kind: Codestream) -> Parse<CodestreamCleaning> {
    let Some((element, sequence, frames)) = encapsulated_frames(object)? else {
        return Ok(CodestreamCleaning {
            replacement: Vec::new(),
            removed: RemovedSegments::default(),
        });
    };
    let StrippedFrames { removed, changed } = strip_frames(&frames, kind, true)?;
    if removed.is_empty() {
        return Ok(CodestreamCleaning {
            replacement: Vec::new(),
            removed,
        });
    }
    check_extended_offset_table(object, removed)?;

    let keep_offset_table = !sequence.offset_table().is_empty();
    let mut fragments = Vec::with_capacity(frames.len());
    let mut offsets = Vec::new();
    let mut offset = 0_u64;
    for (frame, output) in frames.iter().zip(changed) {
        // An unchanged frame keeps its source bytes, including its padding.
        let mut fragment = output.unwrap_or_else(|| frame_codestream(frame).into_owned());
        if fragment.len() % 2 == 1 {
            fragment.push(0);
        }
        if keep_offset_table {
            offsets.push(u32::try_from(offset).map_err(|_| "Basic Offset Table overflow")?);
            offset += 8 + fragment.len() as u64;
        }
        fragments.push(fragment);
    }
    let total_length = object
        .get(tags::ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH)
        .map(|_| encapsulated_total_length(&fragments));
    let pixel_data = InMemElement::new(
        tags::PIXEL_DATA,
        element.vr(),
        Value::PixelSequence(PixelFragmentSequence::new(offsets, fragments)),
    );
    Ok(CodestreamCleaning {
        replacement: std::iter::once(pixel_data).chain(total_length).collect(),
        removed,
    })
}

/// Group fragments into frames without guessing from fragment contents.
fn frame_fragments<'a>(
    object: &InMemDicomObject,
    sequence: &'a PixelFragmentSequence<Vec<u8>>,
) -> Parse<Vec<&'a [Vec<u8>]>> {
    let fragments = sequence.fragments();
    if fragments.is_empty() {
        return Err("Pixel Data has no fragments");
    }
    let frame_count = match text(object, tags::NUMBER_OF_FRAMES) {
        None => 1,
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|&count| count > 0)
            .ok_or("Number of Frames is invalid")?,
    };
    if frame_count == 1 {
        return Ok(vec![fragments]);
    }
    let offset_table = sequence.offset_table();
    if offset_table.is_empty() {
        if fragments.len() == frame_count {
            return Ok(fragments.chunks(1).collect());
        }
        return Err("multi-frame Pixel Data needs a Basic Offset Table or one fragment per frame");
    }
    if offset_table.len() != frame_count {
        return Err("Basic Offset Table does not match Number of Frames");
    }
    let mut starts = Vec::with_capacity(frame_count);
    let mut offset = 0_u64;
    let mut table = offset_table.iter().peekable();
    for (index, fragment) in fragments.iter().enumerate() {
        if table
            .peek()
            .is_some_and(|&&start| u64::from(start) == offset)
        {
            table.next();
            starts.push(index);
        }
        offset += 8 + fragment.len() as u64;
    }
    if table.next().is_some() || starts.first() != Some(&0) {
        return Err("Basic Offset Table does not match fragment boundaries");
    }
    starts.push(fragments.len());
    Ok(starts
        .windows(2)
        .map(|bounds| &fragments[bounds[0]..bounds[1]])
        .collect())
}

/// Remove COM segments from the main header and every tile-part header.
///
/// Returns the rewritten codestream without trailing padding and the number of removed segments.
fn strip_comments(input: &[u8]) -> Parse<(Vec<u8>, u64)> {
    let end = without_zero_padding(input);
    if end < 4 || marker(input, end - 2) != Some(EOC) {
        return Err("codestream does not end with EOC");
    }
    let data_end = end - 2;
    if marker(input, 0) != Some(SOC) {
        return Err("codestream does not start with SOC");
    }
    if marker(input, 2) != Some(SIZ) {
        return Err("SIZ does not follow SOC");
    }

    let mut output = Vec::with_capacity(end);
    output.extend_from_slice(&input[..2]);
    let mut removed = 0_u64;
    let mut position = 2;
    let mut has_tlm = false;
    loop {
        let code = marker(input, position)
            .filter(|_| position < data_end)
            .ok_or("main header is truncated")?;
        if code == SOT {
            break;
        }
        let length = segment_length(input, position, data_end, code)?;
        match code {
            COM => removed += 1,
            TLM => {
                has_tlm = true;
                output.extend_from_slice(&input[position..position + length]);
            }
            _ => output.extend_from_slice(&input[position..position + length]),
        }
        position += length;
    }

    let mut tile_comments = false;
    while position < data_end {
        if marker(input, position) != Some(SOT) {
            return Err("expected SOT");
        }
        let start = position;
        if segment_length(input, position, data_end, SOT)? != 12 {
            return Err("SOT segment length is invalid");
        }
        let psot = u32::from_be_bytes(
            input[position + 6..position + 10]
                .try_into()
                .expect("slice has four bytes"),
        );
        let tile_end = if psot == 0 {
            data_end
        } else {
            start
                .checked_add(psot as usize)
                .filter(|&tile_end| tile_end <= data_end && tile_end >= start + 14)
                .ok_or("tile-part length is invalid")?
        };
        let sot_output = output.len();
        output.extend_from_slice(&input[position..position + 12]);
        position += 12;
        let mut tile_removed = 0_usize;
        loop {
            let code = marker(input, position)
                .filter(|_| position + 2 <= tile_end)
                .ok_or("tile-part header is truncated")?;
            if code == SOD {
                break;
            }
            let length = segment_length(input, position, tile_end, code)?;
            if code == COM {
                removed += 1;
                tile_removed += length;
            } else {
                output.extend_from_slice(&input[position..position + length]);
            }
            position += length;
        }
        if has_marker_code(&input[position + 2..tile_end]) {
            return Err("tile-part data contains a marker code");
        }
        output.extend_from_slice(&input[position..tile_end]);
        position = tile_end;
        if tile_removed > 0 {
            tile_comments = true;
            if psot != 0 {
                let psot = psot - u32::try_from(tile_removed).expect("bounded by Psot");
                output[sot_output + 6..sot_output + 10].copy_from_slice(&psot.to_be_bytes());
            }
        }
    }
    if position != data_end {
        return Err("codestream has no tile-parts");
    }
    if tile_comments && has_tlm {
        return Err("tile-part comment removal would invalidate the TLM marker segment");
    }
    output.extend_from_slice(&input[data_end..end]);
    Ok((output, removed))
}

/// Return true when tile-part data contains a marker code other than SOP or EPH.
///
/// Bit stuffing in packet headers and code-block data never puts a byte above `0x8F`
/// after `0xFF`. A marker code in the data therefore shows appended content, such as a
/// second EOC followed by a COM segment, that must not be copied as tile data. The
/// sequence number in a six-byte SOP segment is not stuffed, so the scan skips it.
fn has_marker_code(data: &[u8]) -> bool {
    let mut position = 0;
    while position + 1 < data.len() {
        if data[position] != 0xFF {
            position += 1;
            continue;
        }
        match data[position + 1] {
            0x91 if data
                .get(position + 2..position + 6)
                .is_some_and(|sop| sop[..2] == [0, 4]) =>
            {
                position += 6;
            }
            0x92 => position += 2,
            byte if byte > 0x8F => return true,
            _ => position += 2,
        }
    }
    false
}

/// Remove COM segments and removable APPn segments from a JPEG or JPEG-LS codestream.
///
/// Marker segments are parsed from SOI to EOI. Entropy-coded scan data, including
/// restart markers and fill bytes, is copied byte for byte. An APP14 Adobe segment is
/// kept when the frame has three or four components, because its color transform flag
/// can change the decoded colors. Removal of a JFIF APP0 segment from a three-component
/// frame fails when libjpeg-compatible decoders would then infer RGB instead of YCbCr.
fn strip_jpeg_segments(input: &[u8], jpeg_ls: bool) -> Parse<(Vec<u8>, RemovedSegments)> {
    let end = without_zero_padding(input);
    if end < 4 || marker(input, end - 2) != Some(EOI) {
        return Err("codestream does not end with EOI");
    }
    if marker(input, 0) != Some(SOI) {
        return Err("codestream does not start with SOI");
    }
    let data_end = end - 2;

    // Kept byte ranges, and the APPn segments whose removal depends on the frame header.
    let mut pieces: Vec<(Range<usize>, Option<u16>)> = vec![(0..2, None)];
    let mut removed = RemovedSegments::default();
    let mut frame: Option<Range<usize>> = None;
    let mut first_scan = None;
    let mut scans = 0_usize;
    let mut position = 2;
    while position < data_end {
        let code = marker(input, position).ok_or("expected a marker segment")?;
        if code == 0xFFFF {
            // Fill bytes before a marker, kept as one piece. The last 0xFF of the run starts
            // the next marker, or the run ends at EOI. Each iteration consumes at least one byte.
            let start = position;
            let run_end = input[position..data_end]
                .iter()
                .position(|&byte| byte != 0xFF)
                .map_or(data_end, |offset| position + offset - 1);
            pieces.push((start..run_end, None));
            position = run_end;
            continue;
        }
        let length = jpeg_segment_length(input, position, data_end)?;
        let segment = position..position + length;
        position += length;
        match code {
            SOS => {
                if frame.is_none() {
                    return Err("SOS precedes the frame header");
                }
                let scan_end = entropy_coded_end(input, position, data_end, jpeg_ls);
                first_scan.get_or_insert(pieces.len());
                pieces.push((segment.start..scan_end, None));
                position = scan_end;
                scans += 1;
            }
            JPEG_COM => removed.comments += 1,
            APP0..=APP15 => pieces.push((segment, Some(code))),
            _ if is_frame_header(code, jpeg_ls) => {
                if frame.is_some() || scans > 0 {
                    return Err("codestream has more than one frame header");
                }
                frame = Some(segment.clone());
                pieces.push((segment, None));
            }
            DRI => pieces.push((segment, None)),
            LSE if jpeg_ls => pieces.push((segment, None)),
            DQT | DHT | DAC if !jpeg_ls => pieces.push((segment, None)),
            DNL if scans > 0 => pieces.push((segment, None)),
            _ => return Err("unsupported marker in a codestream header"),
        }
    }
    if scans == 0 {
        return Err("codestream has no scan");
    }
    let frame = frame.ok_or("codestream has no frame header")?;
    let components = frame_components(input, &frame)?;
    let component_count = components.len() / 3;
    let header_pieces = first_scan.expect("a scan was parsed");
    let adobe = adobe_segment(input, &pieces, header_pieces, component_count)?;
    let has_jfif = pieces[..header_pieces].iter().any(|(range, application)| {
        *application == Some(APP0) && input[range.start + 4..range.end].starts_with(b"JFIF\0")
    });
    if !jpeg_ls && has_jfif && component_count == 3 {
        // libjpeg-turbo reads JFIF as YCbCr before it reads Adobe or the component
        // identifiers. Without either marker, it reads lossless frames as RGB.
        let lossless = matches!(marker(input, frame.start), Some(0xFFC3 | 0xFFCB));
        // Only the Adobe transform value 1 means YCbCr for three components.
        let ycbcr_without_jfif = match adobe {
            Some(index) => input[pieces[index].0.end - 1] == 1,
            None => !lossless && !is_rgb_component_ids(components),
        };
        if !ycbcr_without_jfif {
            return Err("JFIF segment removal would change the decoded color space");
        }
    }

    let mut output = Vec::with_capacity(end);
    for (index, (range, application)) in pieces.iter().enumerate() {
        if application.is_some() && adobe != Some(index) {
            removed.applications += 1;
        } else {
            output.extend_from_slice(&input[range.clone()]);
        }
    }
    output.extend_from_slice(&input[data_end..end]);
    Ok((output, removed))
}

/// Return the index of the APP14 Adobe segment that a color frame needs, if any.
///
/// For three or four components, the Adobe transform flag selects RGB, YCbCr, CMYK, or
/// YCCK. libjpeg reads it only before the first scan, at index `header_pieces`, so a later
/// Adobe segment is ambiguous. A JPEG-LS `mrfx` color transform segment cannot be kept or
/// removed safely.
fn adobe_segment(
    input: &[u8],
    pieces: &[(Range<usize>, Option<u16>)],
    header_pieces: usize,
    component_count: usize,
) -> Parse<Option<usize>> {
    if !matches!(component_count, 3 | 4) {
        return Ok(None);
    }
    let mut adobe = None;
    for (index, (range, application)) in pieces.iter().enumerate() {
        let Some(code) = *application else {
            continue;
        };
        let payload = &input[range.start + 4..range.end];
        if payload.starts_with(b"mrfx") {
            return Err("JPEG-LS color transform segment cannot be removed safely");
        }
        if code != APP14 || !payload.starts_with(b"Adobe") {
            continue;
        }
        if index > header_pieces {
            return Err("Adobe APP14 segment follows the first scan");
        }
        // Identifier, version, two flag words, and the transform byte.
        if payload.len() != 12 {
            return Err("Adobe APP14 segment length is invalid");
        }
        if adobe.replace(index).is_some() {
            return Err("codestream has more than one Adobe APP14 segment");
        }
    }
    Ok(adobe)
}

fn is_frame_header(code: u16, jpeg_ls: bool) -> bool {
    if jpeg_ls {
        code == SOF55
    } else {
        JPEG_FRAME_HEADERS.contains(&code)
    }
}

/// Component specifications from a frame header with a consistent length.
fn frame_components<'a>(input: &'a [u8], frame: &Range<usize>) -> Parse<&'a [u8]> {
    let header = &input[frame.clone()];
    let count = usize::from(*header.get(9).ok_or("frame header is truncated")?);
    if count == 0 || header.len() != 10 + 3 * count {
        return Err("frame header length is invalid");
    }
    // Each component specification is identifier, sampling factors, and table selector.
    Ok(&header[10..])
}

/// Return true when libjpeg-compatible decoders infer RGB from the component identifiers.
fn is_rgb_component_ids(components: &[u8]) -> bool {
    [components[0], components[3], components[6]] == *b"RGB"
}

/// Total JPEG marker-segment size, including the two-byte marker.
fn jpeg_segment_length(input: &[u8], position: usize, limit: usize) -> Parse<usize> {
    input
        .get(position + 2..position + 4)
        .map(|bytes| usize::from(u16::from_be_bytes([bytes[0], bytes[1]])))
        .filter(|&length| length >= 2 && position + 2 + length <= limit)
        .map(|length| length + 2)
        .ok_or("marker segment is truncated")
}

/// Return the offset of the marker that ends the entropy-coded data starting at `start`.
///
/// In T.81 data, `0xFF` is followed by a stuffed `0x00`. In T.87 data, `0xFF` is followed
/// by a byte whose most significant bit is zero. Restart markers and fill bytes stay
/// inside the scan data.
fn entropy_coded_end(input: &[u8], start: usize, data_end: usize, jpeg_ls: bool) -> usize {
    let mut position = start;
    while position < data_end {
        if input[position] != 0xFF {
            position += 1;
            continue;
        }
        let mut next = position + 1;
        while next < data_end && input[next] == 0xFF {
            next += 1;
        }
        if next >= data_end {
            return data_end;
        }
        let byte = input[next];
        let continues = if jpeg_ls { byte < 0x80 } else { byte == 0x00 };
        if !continues && !(0xD0..=0xD7).contains(&byte) {
            return next - 1;
        }
        position = next + 1;
    }
    data_end
}

/// Length without the trailing zero bytes that pad odd-length fragments.
fn without_zero_padding(input: &[u8]) -> usize {
    let mut end = input.len();
    while end > 0 && input[end - 1] == 0 {
        end -= 1;
    }
    end
}

fn marker(input: &[u8], position: usize) -> Option<u16> {
    let bytes = input.get(position..position + 2)?;
    (bytes[0] == 0xFF).then(|| u16::from_be_bytes([bytes[0], bytes[1]]))
}

/// Total marker-segment size, including the two-byte marker.
fn segment_length(input: &[u8], position: usize, limit: usize, code: u16) -> Parse<usize> {
    if (0xFF30..=0xFF3F).contains(&code) || matches!(code, SOC | SOD | EOC) || code < 0xFF01 {
        return Err("unexpected marker in a codestream header");
    }
    let length = input
        .get(position + 2..position + 4)
        .map(|bytes| usize::from(u16::from_be_bytes([bytes[0], bytes[1]])))
        .filter(|&length| length >= 2 && position + 2 + length <= limit)
        .ok_or("marker segment is truncated")?;
    Ok(length + 2)
}

fn text(object: &InMemDicomObject, tag: Tag) -> Option<String> {
    object
        .get(tag)
        .and_then(|element| element.to_str().ok())
        .map(|value| value.trim_matches(['\0', ' ']).to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dicom_core::{PrimitiveValue, VR};

    fn comment(text: &[u8]) -> Vec<u8> {
        let mut segment = vec![0xFF, 0x64];
        segment.extend_from_slice(&u16::try_from(text.len() + 4).unwrap().to_be_bytes());
        segment.extend_from_slice(&[0x00, 0x01]);
        segment.extend_from_slice(text);
        segment
    }

    /// Structural codestream: SOC, SIZ, optional main segments, one tile-part, EOC.
    fn codestream(main: &[u8], tile_header: &[u8], body: &[u8], explicit_psot: bool) -> Vec<u8> {
        let mut output = vec![0xFF, 0x4F, 0xFF, 0x51, 0x00, 0x04, 0xAA, 0xBB];
        output.extend_from_slice(main);
        let psot = if explicit_psot {
            u32::try_from(12 + tile_header.len() + 2 + body.len()).unwrap()
        } else {
            0
        };
        output.extend_from_slice(&[0xFF, 0x90, 0x00, 0x0A, 0x00, 0x00]);
        output.extend_from_slice(&psot.to_be_bytes());
        output.extend_from_slice(&[0x00, 0x01]);
        output.extend_from_slice(tile_header);
        output.extend_from_slice(&[0xFF, 0x93]);
        output.extend_from_slice(body);
        output.extend_from_slice(&[0xFF, 0xD9]);
        output
    }

    fn object(
        frames: Option<&str>,
        offsets: Vec<u32>,
        fragments: Vec<Vec<u8>>,
    ) -> InMemDicomObject {
        let mut object = InMemDicomObject::new_empty();
        if let Some(frames) = frames {
            object.put_str(tags::NUMBER_OF_FRAMES, VR::IS, frames);
        }
        object.put(InMemElement::new(
            tags::PIXEL_DATA,
            VR::OB,
            Value::PixelSequence(PixelFragmentSequence::new(offsets, fragments)),
        ));
        object
    }

    fn pad(mut fragment: Vec<u8>) -> Vec<u8> {
        if fragment.len() % 2 == 1 {
            fragment.push(0);
        }
        fragment
    }

    #[test]
    fn strips_main_and_tile_comments_and_updates_psot() {
        // Tile data contains bytes that look like a COM marker segment.
        let body = [0xFF, 0x64, 0x00, 0x04, 0xFF, 0x8F, 0x12];
        let clean = codestream(&[], &[], &body, true);
        for explicit in [true, false] {
            let commented = codestream(&comment(b"MAIN"), &comment(b"TILE"), &body, explicit);
            let (output, removed) = strip_comments(&commented).unwrap();
            assert_eq!(removed, 2);
            assert_eq!(output, codestream(&[], &[], &body, explicit));
        }
        assert_eq!(strip_comments(&clean).unwrap(), (clean.clone(), 0));
        let mut padded = clean.clone();
        padded.push(0);
        assert_eq!(strip_comments(&padded).unwrap(), (clean, 0));
    }

    #[test]
    fn rejects_unparseable_codestreams() {
        let valid = codestream(&comment(b"MAIN"), &[], &[1, 2], true);
        let mut jp2 = vec![0, 0, 0, 0x0C, b'j', b'P', b' ', b' '];
        jp2.extend_from_slice(&valid);
        let mut trailing = valid.clone();
        trailing.extend_from_slice(b"TEXT");
        let mut truncated_segment = valid[..8].to_vec();
        truncated_segment.extend_from_slice(&[0xFF, 0x64, 0x00, 0x40, 0xFF, 0xD9]);
        let mut bad_psot = valid.clone();
        bad_psot[8 + 8 + 6..8 + 8 + 10].copy_from_slice(&u32::MAX.to_be_bytes());
        let mut no_siz = vec![0xFF, 0x4F];
        no_siz.extend_from_slice(&comment(b"SYNTHJ2KCOMMENT!"));
        no_siz.extend_from_slice(&[0xFF, 0xD9]);
        let no_tiles = vec![0xFF, 0x4F, 0xFF, 0x51, 0x00, 0x02, 0xFF, 0xD9];
        let delimiter = codestream(&[0xFF, 0x30], &[], &[], true);
        let eoc_in_body = codestream(&[], &[], &[1, 0xFF, 0xD9, 0x00], true);
        let bad_sop = codestream(&[], &[], &[0xFF, 0x91, 0x00, 0x05, 0x00, 0x01, 0x00], true);
        let short_sop = codestream(&[], &[], &[0xFF, 0x91, 0x00, 0x04, 0x00], true);
        for (name, input) in [
            ("bad-sop", bad_sop),
            ("short-sop", short_sop),
            ("eoc-in-body", eoc_in_body),
            ("jp2", jp2),
            ("trailing", trailing),
            ("truncated", truncated_segment),
            ("psot", bad_psot),
            ("no-siz", no_siz),
            ("no-tiles", no_tiles),
            ("delimiter", delimiter),
            ("empty", Vec::new()),
        ] {
            assert!(strip_comments(&input).is_err(), "{name}");
        }
    }

    #[test]
    fn updates_psot_in_every_commented_tile_part() {
        let tile_part = |index: u8, header: &[u8], body: &[u8]| {
            let psot = u32::try_from(12 + header.len() + 2 + body.len()).unwrap();
            let mut part = vec![0xFF, 0x90, 0x00, 0x0A, 0x00, index];
            part.extend_from_slice(&psot.to_be_bytes());
            part.extend_from_slice(&[0x00, 0x01]);
            part.extend_from_slice(header);
            part.extend_from_slice(&[0xFF, 0x93]);
            part.extend_from_slice(body);
            part
        };
        let build = |comments: bool| {
            let mut output = vec![0xFF, 0x4F, 0xFF, 0x51, 0x00, 0x04, 0xAA, 0xBB];
            if comments {
                output.extend_from_slice(&comment(b"MAIN"));
            }
            for index in 0..3 {
                let header = if comments && index != 1 {
                    comment(&vec![b'T'; usize::from(index) + 1])
                } else {
                    Vec::new()
                };
                output.extend_from_slice(&tile_part(index, &header, &[index]));
            }
            output.extend_from_slice(&[0xFF, 0xD9]);
            output
        };
        assert_eq!(strip_comments(&build(true)).unwrap(), (build(false), 3));
    }

    #[test]
    fn marker_codes_in_tile_data_fail_closed() {
        // A comment and a second EOC appended after the real EOC look like tile data.
        let mut appended = vec![1, 2, 0xFF, 0xD9];
        appended.extend_from_slice(&comment(b"SYNTHAPPENDED"));
        for explicit in [false, true] {
            let input = codestream(&[], &[], &appended, explicit);
            assert!(strip_comments(&input).is_err(), "explicit Psot: {explicit}");
        }
        // SOP and EPH markers and stuffed bytes are valid in tile data. The SOP sequence
        // number is not stuffed, so marker-like values must not count as marker codes.
        for nsop in [[0x00, 0x00], [0xFF, 0x90], [0xFF, 0xD9], [0x00, 0xFF]] {
            let mut body = vec![0xFF, 0x91, 0x00, 0x04];
            body.extend_from_slice(&nsop);
            body.extend_from_slice(&[0xC0, 0xFF, 0x92, 0xFF, 0x8F, 0x12]);
            for explicit in [false, true] {
                assert!(
                    strip_comments(&codestream(&[], &[], &body, explicit)).is_ok(),
                    "{nsop:02X?}"
                );
            }
        }
        let truncated_sop = [0x12, 0xFF, 0x91, 0x00, 0x04, 0x00];
        assert!(strip_comments(&codestream(&[], &[], &truncated_sop, false)).is_err());
    }

    #[test]
    fn tile_comment_with_tlm_fails_closed() {
        let tlm = [0xFF, 0x55, 0x00, 0x04, 0x00, 0x00];
        let main_only = codestream(&[&tlm[..], &comment(b"MAIN")].concat(), &[], &[1], true);
        assert_eq!(strip_comments(&main_only).unwrap().1, 1);
        let tile = codestream(&tlm, &comment(b"TILE"), &[1], true);
        assert!(strip_comments(&tile).is_err());
    }

    #[test]
    fn rewrites_multi_frame_fragments_and_offset_table() {
        let first = codestream(&comment(b"ONE"), &[], &[1, 2, 3], true);
        let second = codestream(&[], &[], &[4, 5], true);
        // The first frame spans two fragments.
        let fragments = vec![
            first[..10].to_vec(),
            pad(first[10..].to_vec()),
            pad(second.clone()),
        ];
        let offsets = vec![0, 8 + 10 + 8 + fragments[1].len() as u32];
        let cleaning =
            clean_codestreams(&object(Some("2"), offsets, fragments), Codestream::Jpeg2000)
                .unwrap();
        assert_eq!(cleaning.removed.comments, 1);
        let [replacement] = <[_; 1]>::try_from(cleaning.replacement).unwrap();
        let Value::PixelSequence(sequence) = replacement.value() else {
            panic!("encapsulated output");
        };
        let expected_first = pad(codestream(&[], &[], &[1, 2, 3], true));
        assert_eq!(
            sequence.fragments(),
            &[expected_first.clone(), pad(second.clone())]
        );
        assert_eq!(
            sequence.offset_table(),
            &[0, 8 + expected_first.len() as u32]
        );

        let without_table = clean_codestreams(
            &object(
                Some("2"),
                Vec::new(),
                vec![pad(first.clone()), pad(second.clone())],
            ),
            Codestream::Jpeg2000,
        )
        .unwrap()
        .replacement
        .remove(0);
        let Value::PixelSequence(sequence) = without_table.value() else {
            panic!("encapsulated output");
        };
        assert!(sequence.offset_table().is_empty());
        assert_eq!(sequence.fragments().len(), 2);
    }

    #[test]
    fn ambiguous_frames_and_extended_offsets_fail_closed() {
        let first = pad(codestream(&comment(b"ONE"), &[], &[1], true));
        let second = pad(codestream(&[], &[], &[2], true));
        let half = first.len() / 2;
        let split = vec![
            first[..half].to_vec(),
            first[half..].to_vec(),
            second.clone(),
        ];
        let cases = [
            object(Some("2"), Vec::new(), split.clone()),
            object(Some("2"), vec![0, 4], split.clone()),
            object(Some("2"), vec![0], split),
            object(Some("0"), Vec::new(), vec![first.clone()]),
            object(None, Vec::new(), Vec::new()),
        ];
        for (index, case) in cases.iter().enumerate() {
            assert!(
                matches!(
                    clean_codestreams(case, Codestream::Jpeg2000),
                    Err(Error::UnsupportedJpeg2000(_))
                ),
                "case {index}"
            );
        }

        let mut extended = object(None, Vec::new(), vec![first.clone()]);
        extended.put(InMemElement::new(
            EXTENDED_OFFSET_TABLE,
            VR::OV,
            PrimitiveValue::U64(vec![0_u64].into()),
        ));
        assert!(matches!(
            clean_codestreams(&extended, Codestream::Jpeg2000),
            Err(Error::UnsupportedJpeg2000(_))
        ));
        assert!(matches!(
            count_removable_segments(&extended, Codestream::Jpeg2000),
            Err(Error::UnsupportedJpeg2000(_))
        ));
        let mut lengths_only = object(None, Vec::new(), vec![first.clone()]);
        lengths_only.put(InMemElement::new(
            EXTENDED_OFFSET_TABLE_LENGTHS,
            VR::OV,
            PrimitiveValue::U64(vec![0_u64].into()),
        ));
        assert!(clean_codestreams(&lengths_only, Codestream::Jpeg2000).is_err());
        assert!(count_removable_segments(&lengths_only, Codestream::Jpeg2000).is_err());
        let mut clean_extended = object(None, Vec::new(), vec![second]);
        clean_extended.put(InMemElement::new(
            EXTENDED_OFFSET_TABLE,
            VR::OV,
            PrimitiveValue::U64(vec![0_u64].into()),
        ));
        assert!(clean_codestreams(&clean_extended, Codestream::Jpeg2000)
            .unwrap()
            .replacement
            .is_empty());

        let mut native = InMemDicomObject::new_empty();
        native.put(InMemElement::new(
            tags::PIXEL_DATA,
            VR::OW,
            PrimitiveValue::from(vec![0_u8; 4]),
        ));
        assert!(clean_codestreams(&native, Codestream::Jpeg2000).is_err());
        assert_eq!(
            count_removable_segments(&InMemDicomObject::new_empty(), Codestream::Jpeg).unwrap(),
            RemovedSegments::default()
        );
    }

    #[test]
    fn recognizes_codestream_transfer_syntaxes() {
        for (uids, kind) in [
            (&JPEG_2000_TRANSFER_SYNTAXES[..], Codestream::Jpeg2000),
            (&JPEG_TRANSFER_SYNTAXES[..], Codestream::Jpeg),
            (&JPEG_LS_TRANSFER_SYNTAXES[..], Codestream::JpegLs),
        ] {
            for uid in uids {
                assert_eq!(Codestream::for_transfer_syntax(uid), Some(kind), "{uid}");
            }
        }
        assert_eq!(
            Codestream::for_transfer_syntax("1.2.840.10008.1.2.4.90\0"),
            Some(Codestream::Jpeg2000)
        );
        // Retired JPEG transfer syntaxes: non-hierarchical ones are cleaned, and hierarchical
        // ones fail closed in the parser on DHP, EXP, or differential frame headers.
        for minor in (52..=56).chain(58..=66) {
            let uid = format!("1.2.840.10008.1.2.4.{minor}");
            assert_eq!(
                Codestream::for_transfer_syntax(&uid),
                Some(Codestream::Jpeg),
                "{uid}"
            );
        }
        for uid in [
            "1.2.840.10008.1.2.1",
            "1.2.840.10008.1.2.4.100",
            "1.2.840.10008.1.2.4.110",
            "1.2.840.10008.1.2.5",
        ] {
            assert_eq!(Codestream::for_transfer_syntax(uid), None, "{uid}");
        }
    }

    fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut output = vec![0xFF, marker];
        output.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
        output.extend_from_slice(payload);
        output
    }

    /// Frame header with the given marker and component identifiers.
    fn frame_header(marker: u8, ids: &[u8]) -> Vec<u8> {
        let mut payload = vec![8, 0, 2, 0, 2, u8::try_from(ids.len()).unwrap()];
        for &id in ids {
            payload.extend_from_slice(&[id, 0x11, 0]);
        }
        segment(marker, &payload)
    }

    fn adobe(transform: u8) -> Vec<u8> {
        segment(
            0xEE,
            &[b"Adobe".as_slice(), &[0, 100, 0, 0, 0, 0, transform]].concat(),
        )
    }

    fn jfif() -> Vec<u8> {
        segment(0xE0, b"JFIF\0\x01\x02\0\0\x01\0\x01\0\0")
    }

    /// Structural JPEG codestream: SOI, header, frame, DHT except for JPEG-LS, scans, EOI.
    fn jpeg(header: &[u8], frame: &[u8], between: &[u8], scans: &[&[u8]]) -> Vec<u8> {
        let mut output = vec![0xFF, 0xD8];
        output.extend_from_slice(header);
        output.extend_from_slice(frame);
        if frame[1] != 0xF7 {
            output.extend(segment(0xC4, &[0x00, 0x01]));
        }
        for (index, scan) in scans.iter().enumerate() {
            if index > 0 {
                output.extend_from_slice(between);
            }
            output.extend(segment(0xDA, &[1, 1, 0, 1, 0, 0]));
            output.extend_from_slice(scan);
        }
        output.extend_from_slice(&[0xFF, 0xD9]);
        output
    }

    // Stuffed 0xFF00, a restart marker, and fill bytes before the marker that ends the scan.
    const T81_SCAN: &[u8] = &[0x12, 0xFF, 0x00, 0xFE, 0xFF, 0xD0, 0xE1, 0xFF, 0xFF];
    // In JPEG-LS data, 0xFF followed by a byte below 0x80 is not a marker.
    const T87_SCAN: &[u8] = &[0x12, 0xFF, 0x7F, 0xFF, 0x00, 0xFE];

    #[test]
    fn strips_jpeg_comments_and_applications_and_copies_scans() {
        let gray = frame_header(0xC3, &[1]);
        let metadata = [
            jfif(),
            segment(0xE1, b"Exif\0\0SYNTH"),
            segment(0xFE, b"SYNTH"),
            adobe(1),
        ]
        .concat();
        let between = [segment(0xFE, b"SCAN"), segment(0xE2, b"ICC")].concat();
        let fill = [0xFF];
        let annotated = jpeg(
            &[&metadata[..], &fill].concat(),
            &gray,
            &between,
            &[T81_SCAN, T81_SCAN],
        );
        let expected = jpeg(&fill, &gray, &[], &[T81_SCAN, T81_SCAN]);
        let (output, removed) = strip_jpeg_segments(&annotated, false).unwrap();
        assert_eq!(output, expected);
        assert_eq!(
            removed,
            RemovedSegments {
                comments: 2,
                applications: 4
            }
        );
        let mut padded = expected.clone();
        padded.push(0);
        assert_eq!(
            strip_jpeg_segments(&padded, false).unwrap(),
            (expected, RemovedSegments::default())
        );

        let sof55 = frame_header(0xF7, &[1]);
        let lse = segment(0xF8, &[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let annotated = jpeg(
            &[segment(0xE8, b"SPIFF\0"), lse.clone()].concat(),
            &sof55,
            &[],
            &[T87_SCAN],
        );
        let (output, removed) = strip_jpeg_segments(&annotated, true).unwrap();
        assert_eq!(output, jpeg(&lse, &sof55, &[], &[T87_SCAN]));
        assert_eq!(removed.applications, 1);
    }

    #[test]
    fn jpeg_fill_byte_runs_are_copied_as_one_piece() {
        let gray = frame_header(0xC3, &[1]);
        let fill = vec![0xFF; 1000];
        let input = jpeg(
            &[&fill[..], &segment(0xFE, b"SYNTH")].concat(),
            &gray,
            &[],
            &[T81_SCAN],
        );
        let (output, removed) = strip_jpeg_segments(&input, false).unwrap();
        assert_eq!(output, jpeg(&fill, &gray, &[], &[T81_SCAN]));
        assert_eq!(removed.comments, 1);

        // Fill bytes directly before EOI, after a post-scan segment.
        let scanned = jpeg(&[], &gray, &[], &[T81_SCAN]);
        let body = &scanned[..scanned.len() - 2];
        for fill in [&[0xFF][..], &[0xFF, 0xFF, 0xFF]] {
            let input = [body, &segment(0xFE, b"SYNTH"), fill, &[0xFF, 0xD9]].concat();
            let (output, removed) = strip_jpeg_segments(&input, false).unwrap();
            assert_eq!(output, [body, fill, &[0xFF, 0xD9]].concat());
            assert_eq!(removed.comments, 1);
        }
        assert!(strip_jpeg_segments(&[0xFF, 0xD8, 0xFF, 0xFF, 0xD9], false).is_err());
    }

    #[test]
    fn jpeg_color_segments_are_kept_or_fail_closed() {
        let ycbcr = frame_header(0xC0, &[1, 2, 3]);
        let rgb = frame_header(0xC0, b"RGB");
        let cmyk = frame_header(0xC0, &[1, 2, 3, 4]);
        let strip = |header: &[u8], frame: &[u8], jpeg_ls: bool| {
            strip_jpeg_segments(&jpeg(header, frame, &[], &[T81_SCAN]), jpeg_ls)
        };

        // The Adobe transform flag stays for three and four components.
        for frame in [&ycbcr, &rgb, &cmyk] {
            let header = [adobe(0), segment(0xFE, b"SYNTH")].concat();
            let (output, removed) = strip(&header, frame, false).unwrap();
            assert_eq!(output, jpeg(&adobe(0), frame, &[], &[T81_SCAN]));
            assert_eq!(removed.applications, 0);
        }
        // JFIF removal keeps YCbCr when the remaining evidence also selects YCbCr.
        for (header, frame) in [
            ([jfif()].concat(), &ycbcr),
            ([jfif(), adobe(1)].concat(), &rgb),
            ([jfif()].concat(), &cmyk),
        ] {
            let (_, removed) = strip(&header, frame, false).unwrap();
            assert_eq!(removed.applications, 1);
        }
        let lossless_ycbcr_ids = frame_header(0xC3, &[1, 2, 3]);
        let (_, removed) = strip(&[jfif(), adobe(1)].concat(), &lossless_ycbcr_ids, false).unwrap();
        assert_eq!(removed.applications, 1);
        // JPEG-LS decoders do not use JFIF for the color space.
        let (_, removed) = strip(&jfif(), &frame_header(0xF7, b"RGB"), true).unwrap();
        assert_eq!(removed.applications, 1);
        // libjpeg fixes the color space at the first SOS, so a later Adobe segment does not count.
        let late_adobe = jpeg(&jfif(), &rgb, &adobe(1), &[T81_SCAN, T81_SCAN]);
        assert!(strip_jpeg_segments(&late_adobe, false).is_err());
        for (name, header, frame, jpeg_ls) in [
            (
                "jfif-lossless-default-rgb",
                jfif(),
                &lossless_ycbcr_ids,
                false,
            ),
            ("jfif-rgb-ids", jfif(), &rgb, false),
            ("jfif-adobe-rgb", [jfif(), adobe(0)].concat(), &ycbcr, false),
            (
                "jfif-adobe-ycck",
                [jfif(), adobe(2)].concat(),
                &ycbcr,
                false,
            ),
            ("two-adobe", [adobe(1), adobe(1)].concat(), &ycbcr, false),
            (
                "adobe-length",
                segment(0xEE, b"Adobe\0\0SYNTHTEXT"),
                &ycbcr,
                false,
            ),
            (
                "mrfx",
                segment(0xE8, b"mrfx\0\x01"),
                &frame_header(0xF7, &[1, 2, 3]),
                true,
            ),
        ] {
            assert!(strip(&header, frame, jpeg_ls).is_err(), "{name}");
        }
        // Gray frames need no color segment.
        let gray_ls = frame_header(0xF7, &[1]);
        let header = [segment(0xE8, b"mrfx\0\x01"), adobe(0)].concat();
        let (output, removed) = strip(&header, &gray_ls, true).unwrap();
        assert_eq!(output, jpeg(&[], &gray_ls, &[], &[T81_SCAN]));
        let with_table = jpeg(&segment(0xC4, &[0x00, 0x01]), &gray_ls, &[], &[T87_SCAN]);
        assert!(strip_jpeg_segments(&with_table, true).is_err());
        assert_eq!(removed.applications, 2);
    }

    #[test]
    fn rejects_unparseable_jpeg_codestreams() {
        let gray = frame_header(0xC1, &[1]);
        let valid = jpeg(&[], &gray, &[], &[T81_SCAN]);
        let mut trailing = valid.clone();
        trailing.extend_from_slice(b"TEXT");
        let mut no_soi = valid.clone();
        no_soi[1] = 0xD9;
        let mut truncated = valid[..2].to_vec();
        truncated.extend_from_slice(&[0xFF, 0xFE, 0x00, 0x40, 0xFF, 0xD9]);
        let mut sos_first = vec![0xFF, 0xD8];
        sos_first.extend(segment(0xDA, &[1, 1, 0, 1, 0, 0]));
        sos_first.extend_from_slice(&[0x12, 0xFF, 0xD9]);
        let mut header_only = vec![0xFF, 0xD8];
        header_only.extend(gray.clone());
        header_only.extend_from_slice(&[0xFF, 0xD9]);
        let mut bad_frame = frame_header(0xC0, &[1]);
        bad_frame[3] += 1;
        bad_frame.push(0);
        for (name, input, jpeg_ls) in [
            ("trailing", trailing, false),
            ("no-soi", no_soi, false),
            ("no-eoi", valid[..valid.len() - 2].to_vec(), false),
            ("truncated", truncated, false),
            ("sos-first", sos_first, false),
            ("no-scan", header_only, false),
            ("two-frames", jpeg(&gray, &gray, &[], &[T81_SCAN]), false),
            (
                "frame-after-scan",
                jpeg(&[], &gray, &gray, &[T81_SCAN, T81_SCAN]),
                false,
            ),
            (
                "frame-length",
                jpeg(&[], &bad_frame, &[], &[T81_SCAN]),
                false,
            ),
            (
                "hierarchical",
                jpeg(&segment(0xDE, &[0; 6]), &gray, &[], &[T81_SCAN]),
                false,
            ),
            (
                "differential",
                jpeg(&[], &frame_header(0xC5, &[1]), &[], &[T81_SCAN]),
                false,
            ),
            (
                "jpeg-ls-frame",
                jpeg(&[], &frame_header(0xF7, &[1]), &[], &[T81_SCAN]),
                false,
            ),
            (
                "lse-in-t81",
                jpeg(&segment(0xF8, &[1]), &gray, &[], &[T81_SCAN]),
                false,
            ),
            (
                "dnl-first",
                jpeg(&segment(0xDC, &[0, 2]), &gray, &[], &[T81_SCAN]),
                false,
            ),
            ("t81-frame-in-ls", valid.clone(), true),
            ("tem", jpeg(&[0xFF, 0x01], &gray, &[], &[T81_SCAN]), false),
            ("empty", Vec::new(), false),
        ] {
            assert!(strip_jpeg_segments(&input, jpeg_ls).is_err(), "{name}");
        }
        let dnl = jpeg(&[], &gray, &segment(0xDC, &[0, 2]), &[T81_SCAN, T81_SCAN]);
        assert!(strip_jpeg_segments(&dnl, false).is_ok());
        // T.87 also permits DNL after the first scan.
        let gray_ls = frame_header(0xF7, &[1]);
        let dnl = jpeg(
            &[],
            &gray_ls,
            &segment(0xDC, &[0, 2]),
            &[T87_SCAN, T87_SCAN],
        );
        assert!(strip_jpeg_segments(&dnl, true).is_ok());
        let dnl_first = jpeg(&segment(0xDC, &[0, 2]), &gray_ls, &[], &[T87_SCAN]);
        assert!(strip_jpeg_segments(&dnl_first, true).is_err());
    }

    #[test]
    fn planar_configuration_is_removed_only_for_one_sample_per_pixel() {
        let with = |samples: Option<u16>| {
            let mut object = InMemDicomObject::new_empty();
            if let Some(samples) = samples {
                object.put(InMemElement::new(
                    tags::SAMPLES_PER_PIXEL,
                    VR::US,
                    PrimitiveValue::from(samples),
                ));
            }
            object.put(InMemElement::new(
                tags::PLANAR_CONFIGURATION,
                VR::US,
                PrimitiveValue::from(0_u16),
            ));
            object
        };
        for (samples, removed) in [(Some(1), true), (Some(3), false), (None, false)] {
            let mut object = with(samples);
            assert_eq!(
                remove_single_sample_planar_configuration(&mut object),
                removed,
                "{samples:?}"
            );
            assert_eq!(
                object.get(tags::PLANAR_CONFIGURATION).is_none(),
                removed,
                "{samples:?}"
            );
        }
        let mut absent = InMemDicomObject::new_empty();
        absent.put(InMemElement::new(
            tags::SAMPLES_PER_PIXEL,
            VR::US,
            PrimitiveValue::from(1_u16),
        ));
        assert!(!remove_single_sample_planar_configuration(&mut absent));
    }

    #[test]
    fn repair_total_length_replaces_or_removes_a_mismatched_value() {
        const RLE: &str = "1.2.840.10008.1.2.5";
        let total_length = |object: &InMemDicomObject| {
            object
                .get(tags::ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH)
                .map(|element| element.to_int::<u64>().unwrap())
        };
        let with_value = |mut object: InMemDicomObject, value: u64| {
            object.put(InMemElement::new(
                tags::ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH,
                VR::UV,
                PrimitiveValue::U64(vec![value].into()),
            ));
            object
        };
        let zero_end = || vec![vec![1, 2, 3, 4], vec![5, 0, 0, 0]];
        let nonzero_end = || vec![vec![1, 2, 3, 4], vec![5, 0, 0, 6]];
        // A final zero byte without an end marker can be data, so an RLE value is removed.
        for (transfer_syntax, fragments, source, expected) in [
            ("1.2.840.10008.1.2.4.70", zero_end(), 8, Some(5)),
            ("1.2.840.10008.1.2.4.70", zero_end(), 5, Some(5)),
            (RLE, zero_end(), 5, None),
            (RLE, zero_end(), 8, Some(8)),
            (RLE, zero_end(), 7, Some(7)),
            (RLE, nonzero_end(), 7, Some(8)),
        ] {
            let mut object = with_value(object(None, Vec::new(), fragments), source);
            let repair = repair_total_length(&mut object, transfer_syntax);
            assert_eq!(
                repair,
                match expected {
                    None => Some(TotalLengthRepair::Removed),
                    Some(value) if value == source => None,
                    Some(_) => Some(TotalLengthRepair::Replaced),
                },
                "{transfer_syntax}: {source}"
            );
            assert_eq!(
                total_length(&object),
                expected,
                "{transfer_syntax}: {source}"
            );
            assert!(!total_length_mismatch(&object, transfer_syntax));
        }
        // A matching number with the wrong VR is malformed and is rewritten as UV.
        for wrong_vr in [
            InMemElement::new(
                tags::ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH,
                VR::UL,
                PrimitiveValue::from(5_u32),
            ),
            InMemElement::new(
                tags::ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH,
                VR::LO,
                PrimitiveValue::from("5"),
            ),
        ] {
            let mut object = object(None, Vec::new(), zero_end());
            object.put(wrong_vr);
            let transfer_syntax = "1.2.840.10008.1.2.4.70";
            assert!(total_length_mismatch(&object, transfer_syntax));
            assert_eq!(
                repair_total_length(&mut object, transfer_syntax),
                Some(TotalLengthRepair::Replaced)
            );
            let repaired = object
                .get(tags::ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH)
                .unwrap();
            assert_eq!(repaired.vr(), VR::UV);
            assert_eq!(repaired.to_int::<u64>().unwrap(), 5);
        }
        let mut native = with_value(InMemDicomObject::new_empty(), 2);
        native.put(InMemElement::new(
            tags::PIXEL_DATA,
            VR::OB,
            PrimitiveValue::from(vec![0_u8, 1]),
        ));
        assert_eq!(
            repair_total_length(&mut native, "1.2.840.10008.1.2.1"),
            Some(TotalLengthRepair::Removed)
        );
        assert_eq!(total_length(&native), None);
        assert!(native.get(tags::PIXEL_DATA).is_some());
    }

    #[test]
    fn total_length_matches_rewritten_fragments() {
        let gray = frame_header(0xC3, &[1]);
        let scan = [&[0x34][..], T81_SCAN].concat();
        let clean = jpeg(&[], &gray, &[], &[&scan]);
        assert_eq!(clean.len() % 2, 1, "the clean frame needs one padding byte");
        let commented = |text: &[u8]| jpeg(&segment(0xFE, text), &gray, &[], &[&scan]);
        let total_length = |frames: Vec<Vec<u8>>| {
            let count = frames.len().to_string();
            let fragments = frames.into_iter().map(pad).collect();
            let mut object = object(Some(&count), Vec::new(), fragments);
            // A stale source value is replaced, not adjusted.
            object.put(InMemElement::new(
                tags::ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH,
                VR::UV,
                PrimitiveValue::U64(vec![1].into()),
            ));
            object
        };
        // PS3.3 C.7.6.6: all fragments, without the trailing padding of the last fragment.
        // Removal of the 7-byte segment adds a padding byte to the first frame.
        for (frames, expected) in [
            (
                vec![commented(b"ODD"), commented(b"EVEN")],
                clean.len() + 1 + clean.len(),
            ),
            (vec![commented(b"ODD")], clean.len()),
            (
                vec![commented(b"ODD"), clean.clone()],
                clean.len() + 1 + clean.len(),
            ),
        ] {
            let cleaning = clean_codestreams(&total_length(frames), Codestream::Jpeg).unwrap();
            let [pixel_data, length] = <[_; 2]>::try_from(cleaning.replacement).unwrap();
            let Value::PixelSequence(sequence) = pixel_data.value() else {
                panic!("encapsulated output");
            };
            let fragments: usize = sequence.fragments().iter().map(Vec::len).sum();
            assert_eq!(fragments, expected + 1);
            assert_eq!(length.to_int::<u64>().unwrap(), expected as u64);
        }
    }

    #[test]
    fn rewrites_multi_frame_jpeg_and_reports_jpeg_errors() {
        let gray = frame_header(0xC3, &[1]);
        let first = jpeg(&segment(0xFE, b"ONE"), &gray, &[], &[T81_SCAN]);
        let second = jpeg(&segment(0xE1, b"TWO"), &gray, &[], &[T81_SCAN]);
        let fragments = vec![pad(first.clone()), pad(second.clone())];
        let offsets = vec![0, 8 + fragments[0].len() as u32];
        let cleaning =
            clean_codestreams(&object(Some("2"), offsets, fragments), Codestream::Jpeg).unwrap();
        assert_eq!(
            cleaning.removed,
            RemovedSegments {
                comments: 1,
                applications: 1
            }
        );
        let [replacement] = <[_; 1]>::try_from(cleaning.replacement).unwrap();
        let Value::PixelSequence(sequence) = replacement.value() else {
            panic!("encapsulated output");
        };
        let clean = pad(jpeg(&[], &gray, &[], &[T81_SCAN]));
        assert_eq!(sequence.fragments(), &[clean.clone(), clean.clone()]);
        assert_eq!(sequence.offset_table(), &[0, 8 + clean.len() as u32]);

        let split = object(
            Some("2"),
            Vec::new(),
            vec![first[..4].to_vec(), first[4..].to_vec(), second],
        );
        assert!(matches!(
            clean_codestreams(&split, Codestream::JpegLs),
            Err(Error::UnsupportedJpeg(_))
        ));
    }

    #[test]
    fn gates_require_explicit_no_and_allowlisted_sop_class() {
        let mut profile = ProfileConfiguration::default();
        let mut object = InMemDicomObject::new_empty();
        assert!(gate_violations(&object, None, &profile).is_empty());
        profile.require_no_burned_in_annotation = true;
        profile
            .sop_class_allowlist
            .insert("1.2.840.10008.5.1.4.1.1.1.2".to_owned());
        assert_eq!(
            gate_violations(&object, None, &profile),
            [
                PixelGateViolation::BurnedInAnnotationNotNo,
                PixelGateViolation::SopClassNotAllowed
            ]
        );
        assert!(matches!(
            check_gates(&object, None, &profile),
            Err(Error::BurnedInAnnotationNotNo)
        ));
        for value in ["YES", "", "no", "NO\\YES"] {
            object.put_str(tags::BURNED_IN_ANNOTATION, VR::CS, value);
            assert!(
                gate_violations(&object, None, &profile)
                    .contains(&PixelGateViolation::BurnedInAnnotationNotNo),
                "{value:?}"
            );
        }
        object.put_str(tags::BURNED_IN_ANNOTATION, VR::CS, "NO ");
        object.put_str(tags::SOP_CLASS_UID, VR::UI, "1.2.840.10008.5.1.4.1.1.1.2\0");
        assert!(check_gates(&object, None, &profile).is_ok());
        let allowed = Some("1.2.840.10008.5.1.4.1.1.1.2\0");
        assert!(check_gates(&object, allowed, &profile).is_ok());
        assert!(matches!(
            check_gates(&object, Some("1.2.840.10008.5.1.4.1.1.7"), &profile),
            Err(Error::SopClassNotAllowed)
        ));
        object.put_str(tags::SOP_CLASS_UID, VR::UI, "1.2.840.10008.5.1.4.1.1.7");
        assert!(matches!(
            check_gates(&object, None, &profile),
            Err(Error::SopClassNotAllowed)
        ));
    }
}
