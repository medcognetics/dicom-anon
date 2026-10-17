//! Fail-closed pixel-risk gates and JPEG 2000 comment removal.
//!
//! The tool does not decode or inspect pixel samples. These checks control only
//! the declared Burned In Annotation value, the SOP Class, and JPEG 2000 comment
//! (COM) marker segments, which can carry text outside the image samples.

use std::borrow::Cow;
use std::ops::Range;

use dicom_core::value::{PixelFragmentSequence, Value};
use dicom_core::Tag;
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
const EXTENDED_OFFSET_TABLE: Tag = Tag(0x7FE0, 0x0001);
const ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH: Tag = Tag(0x7FE0, 0x0003);

const SOC: u16 = 0xFF4F;
const SIZ: u16 = 0xFF51;
const TLM: u16 = 0xFF55;
const COM: u16 = 0xFF64;
const SOT: u16 = 0xFF90;
const SOD: u16 = 0xFF93;
const EOC: u16 = 0xFFD9;

/// A pixel-gate condition that the configuration rejects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PixelGateViolation {
    BurnedInAnnotationNotNo,
    SopClassNotAllowed,
}

/// Return every configured gate that the object fails.
pub(crate) fn gate_violations(
    object: &InMemDicomObject,
    profile: &ProfileConfiguration,
) -> Vec<PixelGateViolation> {
    let mut violations = Vec::new();
    if profile.require_no_burned_in_annotation
        && text(object, tags::BURNED_IN_ANNOTATION).as_deref() != Some("NO")
    {
        violations.push(PixelGateViolation::BurnedInAnnotationNotNo);
    }
    if !profile.sop_class_allowlist.is_empty()
        && !text(object, tags::SOP_CLASS_UID)
            .is_some_and(|uid| profile.sop_class_allowlist.contains(&uid))
    {
        violations.push(PixelGateViolation::SopClassNotAllowed);
    }
    violations
}

/// Fail closed on the first configured gate that the object fails.
pub(crate) fn check_gates(object: &InMemDicomObject, profile: &ProfileConfiguration) -> Result<()> {
    match gate_violations(object, profile).first() {
        Some(PixelGateViolation::BurnedInAnnotationNotNo) => Err(Error::BurnedInAnnotationNotNo),
        Some(PixelGateViolation::SopClassNotAllowed) => Err(Error::SopClassNotAllowed),
        None => Ok(()),
    }
}

#[must_use]
pub(crate) fn is_jpeg2000(transfer_syntax: &str) -> bool {
    JPEG_2000_TRANSFER_SYNTAXES.contains(&transfer_syntax.trim_end_matches(['\0', ' ']))
}

/// Result of JPEG 2000 comment removal for one Pixel Data element.
#[derive(Debug)]
pub(crate) struct Jpeg2000Cleaning {
    /// Replacement Pixel Data, or `None` when no COM segment was present.
    pub replacement: Option<InMemElement>,
    /// Number of removed COM marker segments.
    pub removed_segments: u64,
}

/// Parse every JPEG 2000 frame and remove COM marker segments.
///
/// Any codestream structure that cannot be parsed and rewritten safely is an error.
pub(crate) fn clean_jpeg2000(object: &InMemDicomObject) -> Result<Jpeg2000Cleaning> {
    let Some(element) = object.get(tags::PIXEL_DATA) else {
        return Ok(Jpeg2000Cleaning {
            replacement: None,
            removed_segments: 0,
        });
    };
    let Value::PixelSequence(sequence) = element.value() else {
        return Err(unsupported("Pixel Data is not encapsulated"));
    };
    let frames = frame_fragments(object, sequence)?;
    // Keep only scan results so that concatenated frame copies do not stay alive.
    let mut scans = Vec::with_capacity(frames.len());
    let mut removed_segments = 0_u64;
    for frame in &frames {
        let scan = scan_comments(&frame_codestream(frame))?;
        removed_segments += scan.comments.len() as u64;
        scans.push(scan);
    }
    if removed_segments == 0 {
        return Ok(Jpeg2000Cleaning {
            replacement: None,
            removed_segments,
        });
    }
    if object.get(EXTENDED_OFFSET_TABLE).is_some() {
        return Err(unsupported(
            "comment removal would invalidate the Extended Offset Table",
        ));
    }
    if object
        .get(ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH)
        .is_some()
    {
        return Err(unsupported(
            "comment removal would invalidate Encapsulated Pixel Data Value Total Length",
        ));
    }

    // Rebuild the Basic Offset Table only when the source has one.
    let write_offsets = !sequence.offset_table().is_empty();
    let mut fragments = Vec::with_capacity(frames.len());
    let mut offsets = Vec::new();
    let mut offset = 0_u64;
    for (frame, scan) in frames.iter().zip(&scans) {
        let mut fragment = rewrite_without_comments(&frame_codestream(frame), scan);
        if fragment.len() % 2 == 1 {
            fragment.push(0);
        }
        if write_offsets {
            offsets.push(
                u32::try_from(offset).map_err(|_| unsupported("Basic Offset Table overflow"))?,
            );
            offset += 8 + fragment.len() as u64;
        }
        fragments.push(fragment);
    }
    Ok(Jpeg2000Cleaning {
        replacement: Some(InMemElement::new(
            tags::PIXEL_DATA,
            element.vr(),
            Value::PixelSequence(PixelFragmentSequence::new(offsets, fragments)),
        )),
        removed_segments,
    })
}

/// Count COM marker segments in every frame without changing or copying the object.
pub(crate) fn count_jpeg2000_comments(object: &InMemDicomObject) -> Result<u64> {
    let Some(element) = object.get(tags::PIXEL_DATA) else {
        return Ok(0);
    };
    let Value::PixelSequence(sequence) = element.value() else {
        return Err(unsupported("Pixel Data is not encapsulated"));
    };
    let mut count = 0_u64;
    for frame in frame_fragments(object, sequence)? {
        count += scan_comments(&frame_codestream(frame))?.comments.len() as u64;
    }
    Ok(count)
}

/// Borrow a single-fragment frame; concatenate only a frame split across fragments.
fn frame_codestream(frame: &[Vec<u8>]) -> Cow<'_, [u8]> {
    match frame {
        [fragment] => Cow::Borrowed(fragment.as_slice()),
        _ => Cow::Owned(frame.concat()),
    }
}

/// Group fragments into frames without guessing from fragment contents.
fn frame_fragments<'a>(
    object: &InMemDicomObject,
    sequence: &'a PixelFragmentSequence<Vec<u8>>,
) -> Result<Vec<&'a [Vec<u8>]>> {
    let fragments = sequence.fragments();
    if fragments.is_empty() {
        return Err(unsupported("Pixel Data has no fragments"));
    }
    let frame_count = match text(object, tags::NUMBER_OF_FRAMES) {
        None => 1,
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|&count| count > 0)
            .ok_or_else(|| unsupported("Number of Frames is invalid"))?,
    };
    if frame_count == 1 {
        return Ok(vec![fragments]);
    }
    let offset_table = sequence.offset_table();
    if offset_table.is_empty() {
        if fragments.len() == frame_count {
            return Ok(fragments.chunks(1).collect());
        }
        return Err(unsupported(
            "multi-frame Pixel Data needs a Basic Offset Table or one fragment per frame",
        ));
    }
    if offset_table.len() != frame_count {
        return Err(unsupported(
            "Basic Offset Table does not match Number of Frames",
        ));
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
        return Err(unsupported(
            "Basic Offset Table does not match fragment boundaries",
        ));
    }
    starts.push(fragments.len());
    Ok(starts
        .windows(2)
        .map(|bounds| &fragments[bounds[0]..bounds[1]])
        .collect())
}

/// COM segment locations found by a structural codestream scan.
#[derive(Debug, PartialEq, Eq)]
struct CommentScan {
    /// End of the codestream after EOC, without trailing padding.
    end: usize,
    /// Byte ranges of COM marker segments in codestream order.
    comments: Vec<Range<usize>>,
    /// Psot field offset and removed byte count for each explicit-length tile-part with comments.
    psot_updates: Vec<(usize, u32)>,
}

/// Find COM segments in the main header and every tile-part header without copying data.
///
/// The scan fails when the structure cannot be rewritten safely.
fn scan_comments(input: &[u8]) -> Result<CommentScan> {
    let mut end = input.len();
    while end > 0 && input[end - 1] == 0 {
        end -= 1;
    }
    if end < 4 || marker(input, end - 2) != Some(EOC) {
        return Err(unsupported("codestream does not end with EOC"));
    }
    let data_end = end - 2;
    if marker(input, 0) != Some(SOC) {
        return Err(unsupported("codestream does not start with SOC"));
    }
    if marker(input, 2) != Some(SIZ) {
        return Err(unsupported("SIZ does not follow SOC"));
    }

    let mut comments = Vec::new();
    let mut psot_updates = Vec::new();
    let mut position = 2;
    let mut has_tlm = false;
    loop {
        let code = marker(input, position)
            .filter(|_| position < data_end)
            .ok_or_else(|| unsupported("main header is truncated"))?;
        if code == SOT {
            break;
        }
        let length = segment_length(input, position, data_end, code)?;
        match code {
            COM => comments.push(position..position + length),
            TLM => has_tlm = true,
            _ => {}
        }
        position += length;
    }

    let mut tile_comments = false;
    while position < data_end {
        if marker(input, position) != Some(SOT) {
            return Err(unsupported("expected SOT"));
        }
        let start = position;
        if segment_length(input, position, data_end, SOT)? != 12 {
            return Err(unsupported("SOT segment length is invalid"));
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
                .ok_or_else(|| unsupported("tile-part length is invalid"))?
        };
        position += 12;
        let mut tile_removed = 0_usize;
        loop {
            let code = marker(input, position)
                .filter(|_| position + 2 <= tile_end)
                .ok_or_else(|| unsupported("tile-part header is truncated"))?;
            if code == SOD {
                break;
            }
            let length = segment_length(input, position, tile_end, code)?;
            if code == COM {
                comments.push(position..position + length);
                tile_removed += length;
            }
            position += length;
        }
        check_tile_data(&input[position + 2..tile_end])?;
        position = tile_end;
        if tile_removed > 0 {
            tile_comments = true;
            if psot != 0 {
                psot_updates.push((
                    start + 6,
                    u32::try_from(tile_removed).expect("bounded by Psot"),
                ));
            }
        }
    }
    if position != data_end {
        return Err(unsupported("codestream has no tile-parts"));
    }
    if tile_comments && has_tlm {
        return Err(unsupported(
            "tile-part comment removal would invalidate the TLM marker segment",
        ));
    }
    Ok(CommentScan {
        end,
        comments,
        psot_updates,
    })
}

/// Reject marker codes in tile-part data.
///
/// T.800 bit stuffing keeps packet headers and code-block data free of 0xFF followed by a
/// byte above 0x8F. Only the in-band SOP segment and EPH marker may use that range. The SOP
/// segment fields are skipped because Nsop can contain any byte value. Any other such code,
/// for example an EOC before the final EOC, indicates content that is not tile data.
fn check_tile_data(data: &[u8]) -> Result<()> {
    let mut index = 0;
    while index + 1 < data.len() {
        if data[index] != 0xFF || data[index + 1] <= 0x8F {
            index += 1;
            continue;
        }
        match data[index + 1] {
            0x91 => {
                if data.get(index + 2..index + 4) != Some(&[0x00, 0x04]) || index + 6 > data.len() {
                    return Err(unsupported("SOP marker segment is invalid"));
                }
                index += 6;
            }
            0x92 => index += 2,
            _ => return Err(unsupported("tile-part data contains a marker code")),
        }
    }
    Ok(())
}

/// Copy the codestream without its COM segments, without padding, and with updated Psot values.
fn rewrite_without_comments(input: &[u8], scan: &CommentScan) -> Vec<u8> {
    let removed_bytes = scan
        .comments
        .iter()
        .map(ExactSizeIterator::len)
        .sum::<usize>();
    let mut output = Vec::with_capacity(scan.end - removed_bytes);
    let mut cursor = 0;
    for comment in &scan.comments {
        output.extend_from_slice(&input[cursor..comment.start]);
        cursor = comment.end;
    }
    output.extend_from_slice(&input[cursor..scan.end]);
    // Both lists are in codestream order, so one pass finds each field's output offset.
    let mut comments = scan.comments.iter().peekable();
    let mut shift = 0;
    for &(field, removed) in &scan.psot_updates {
        while let Some(comment) = comments.next_if(|comment| comment.end <= field) {
            shift += comment.len();
        }
        let field = field - shift;
        let psot = u32::from_be_bytes(
            output[field..field + 4]
                .try_into()
                .expect("slice has four bytes"),
        );
        output[field..field + 4].copy_from_slice(&(psot - removed).to_be_bytes());
    }
    output
}

/// Scan and rewrite one codestream. Returns the output without padding and the removed count.
#[cfg(test)]
fn strip_comments(input: &[u8]) -> Result<(Vec<u8>, u64)> {
    let scan = scan_comments(input)?;
    Ok((
        rewrite_without_comments(input, &scan),
        scan.comments.len() as u64,
    ))
}

fn marker(input: &[u8], position: usize) -> Option<u16> {
    let bytes = input.get(position..position + 2)?;
    (bytes[0] == 0xFF).then(|| u16::from_be_bytes([bytes[0], bytes[1]]))
}

/// Total marker-segment size, including the two-byte marker.
fn segment_length(input: &[u8], position: usize, limit: usize, code: u16) -> Result<usize> {
    if (0xFF30..=0xFF3F).contains(&code) || matches!(code, SOC | SOD | EOC) || code < 0xFF01 {
        return Err(unsupported("unexpected marker in a codestream header"));
    }
    let length = input
        .get(position + 2..position + 4)
        .map(|bytes| usize::from(u16::from_be_bytes([bytes[0], bytes[1]])))
        .filter(|&length| length >= 2 && position + 2 + length <= limit)
        .ok_or_else(|| unsupported("marker segment is truncated"))?;
    Ok(length + 2)
}

fn text(object: &InMemDicomObject, tag: Tag) -> Option<String> {
    object
        .get(tag)
        .and_then(|element| element.to_str().ok())
        .map(|value| value.trim_matches(['\0', ' ']).to_owned())
        .filter(|value| !value.is_empty())
}

fn unsupported(reason: &str) -> Error {
    Error::UnsupportedJpeg2000(reason.to_owned())
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
        // Tile data contains bit-stuffed 0xFF bytes, in-band SOP segments whose Nsop
        // values contain 0xFF and 0xFFD9, and an EPH marker.
        let body = [
            0xFF, 0x00, 0xFF, 0x8F, 0xFF, 0x91, 0x00, 0x04, 0x00, 0xFF, 0xC0, 0xFF, 0x91, 0x00,
            0x04, 0xFF, 0xD9, 0xFF, 0x92, 0x12,
        ];
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
        // A zero Psot tile-part must not hide an earlier EOC and an appended comment.
        let hidden_after_eoc = codestream(
            &[],
            &[],
            &[&[1, 0xFF, 0xD9][..], &comment(b"SYNTHHIDDEN")].concat(),
            false,
        );
        let eoc_in_body = codestream(&[], &[], &[1, 0xFF, 0xD9, 0x00], true);
        let bad_sop = codestream(&[], &[], &[0xFF, 0x91, 0x00, 0x05, 0x00, 0x01, 0x00], true);
        let short_sop = codestream(&[], &[], &[0xFF, 0x91, 0x00, 0x04, 0x00], true);
        for (name, input) in [
            ("bad-sop", bad_sop),
            ("short-sop", short_sop),
            ("hidden-after-eoc", hidden_after_eoc),
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
            assert!(
                matches!(strip_comments(&input), Err(Error::UnsupportedJpeg2000(_))),
                "{name}"
            );
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
    fn tile_comment_with_tlm_fails_closed() {
        let tlm = [0xFF, 0x55, 0x00, 0x04, 0x00, 0x00];
        let main_only = codestream(&[&tlm[..], &comment(b"MAIN")].concat(), &[], &[1], true);
        assert_eq!(strip_comments(&main_only).unwrap().1, 1);
        let tile = codestream(&tlm, &comment(b"TILE"), &[1], true);
        assert!(matches!(
            strip_comments(&tile),
            Err(Error::UnsupportedJpeg2000(_))
        ));
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
        let cleaning = clean_jpeg2000(&object(Some("2"), offsets, fragments)).unwrap();
        assert_eq!(cleaning.removed_segments, 1);
        let replacement = cleaning.replacement.unwrap();
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

        let without_table = clean_jpeg2000(&object(
            Some("2"),
            Vec::new(),
            vec![pad(first.clone()), pad(second.clone())],
        ))
        .unwrap()
        .replacement
        .unwrap();
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
                matches!(clean_jpeg2000(case), Err(Error::UnsupportedJpeg2000(_))),
                "case {index}"
            );
        }

        let mut extended = object(None, Vec::new(), vec![first]);
        extended.put(InMemElement::new(
            EXTENDED_OFFSET_TABLE,
            VR::OV,
            PrimitiveValue::U64(vec![0_u64].into()),
        ));
        assert!(matches!(
            clean_jpeg2000(&extended),
            Err(Error::UnsupportedJpeg2000(_))
        ));
        let mut clean_extended = object(None, Vec::new(), vec![second]);
        clean_extended.put(InMemElement::new(
            EXTENDED_OFFSET_TABLE,
            VR::OV,
            PrimitiveValue::U64(vec![0_u64].into()),
        ));
        assert!(clean_jpeg2000(&clean_extended)
            .unwrap()
            .replacement
            .is_none());
        let mut total_length = object(
            None,
            Vec::new(),
            vec![pad(codestream(&comment(b"ONE"), &[], &[1], true))],
        );
        total_length.put(InMemElement::new(
            ENCAPSULATED_PIXEL_DATA_VALUE_TOTAL_LENGTH,
            VR::UV,
            PrimitiveValue::U64(vec![0_u64].into()),
        ));
        assert!(matches!(
            clean_jpeg2000(&total_length),
            Err(Error::UnsupportedJpeg2000(_))
        ));

        let mut native = InMemDicomObject::new_empty();
        native.put(InMemElement::new(
            tags::PIXEL_DATA,
            VR::OW,
            PrimitiveValue::from(vec![0_u8; 4]),
        ));
        assert!(clean_jpeg2000(&native).is_err());
        assert_eq!(
            count_jpeg2000_comments(&InMemDicomObject::new_empty()).unwrap(),
            0
        );
    }

    #[test]
    fn recognizes_jpeg2000_transfer_syntaxes() {
        for uid in JPEG_2000_TRANSFER_SYNTAXES {
            assert!(is_jpeg2000(uid));
        }
        assert!(is_jpeg2000("1.2.840.10008.1.2.4.90\0"));
        assert!(!is_jpeg2000("1.2.840.10008.1.2.4.50"));
        assert!(!is_jpeg2000("1.2.840.10008.1.2.1"));
    }

    #[test]
    fn gates_require_explicit_no_and_allowlisted_sop_class() {
        let mut profile = ProfileConfiguration::default();
        let mut object = InMemDicomObject::new_empty();
        assert!(gate_violations(&object, &profile).is_empty());
        profile.require_no_burned_in_annotation = true;
        profile
            .sop_class_allowlist
            .insert("1.2.840.10008.5.1.4.1.1.1.2".to_owned());
        assert_eq!(
            gate_violations(&object, &profile),
            [
                PixelGateViolation::BurnedInAnnotationNotNo,
                PixelGateViolation::SopClassNotAllowed
            ]
        );
        assert!(matches!(
            check_gates(&object, &profile),
            Err(Error::BurnedInAnnotationNotNo)
        ));
        for value in ["YES", "", "no", "NO\\YES"] {
            object.put_str(tags::BURNED_IN_ANNOTATION, VR::CS, value);
            assert!(
                gate_violations(&object, &profile)
                    .contains(&PixelGateViolation::BurnedInAnnotationNotNo),
                "{value:?}"
            );
        }
        object.put_str(tags::BURNED_IN_ANNOTATION, VR::CS, "NO ");
        object.put_str(tags::SOP_CLASS_UID, VR::UI, "1.2.840.10008.5.1.4.1.1.1.2\0");
        assert!(check_gates(&object, &profile).is_ok());
        object.put_str(tags::SOP_CLASS_UID, VR::UI, "1.2.840.10008.5.1.4.1.1.7");
        assert!(matches!(
            check_gates(&object, &profile),
            Err(Error::SopClassNotAllowed)
        ));
    }
}
