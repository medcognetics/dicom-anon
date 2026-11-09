//! Burned-in text detection and censoring in pixel samples.
//!
//! The detector is a deterministic heuristic. It decodes every frame and looks for
//! rendered text: groups of bright, character-sized, stroke-like components on the
//! image background. Each such region, with a margin, is overwritten with the
//! background value only when every pixel in it is background or one of the line's own
//! glyphs, so tissue is never written. A line of text with anything else in its region
//! fails closed. A frame or transfer syntax that cannot be
//! decoded also fails closed.
//!
//! The detector does not read text. It censors identifiers and laterality or view
//! markers alike. It can miss text: see `docs/conformance.md`.

use dicom_core::value::Value;
use dicom_core::{PrimitiveValue, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::mem::{InMemDicomObject, InMemElement};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Native transfer syntaxes that the check reads.
const NATIVE_TRANSFER_SYNTAXES: [&str; 2] = [
    uids::IMPLICIT_VR_LITTLE_ENDIAN,
    uids::EXPLICIT_VR_LITTLE_ENDIAN,
];
/// JPEG 2000 lossless only (`.90`) and lossless or lossy (`.91`).
const JPEG_2000: [&str; 2] = ["1.2.840.10008.1.2.4.90", "1.2.840.10008.1.2.4.91"];
/// The Lossy Image Compression Method of an accepted lossy transfer syntax: JPEG
/// baseline and extended (ISO 10918-1), and JPEG-LS near-lossless (ISO 14495-1).
pub(crate) fn lossy_method(transfer_syntax: &str) -> Option<&'static str> {
    match transfer_syntax {
        "1.2.840.10008.1.2.4.50" | "1.2.840.10008.1.2.4.51" => Some("ISO_10918_1"),
        "1.2.840.10008.1.2.4.81" => Some("ISO_14495_1"),
        _ => None,
    }
}
const JPEG_LS: [&str; 2] = ["1.2.840.10008.1.2.4.80", "1.2.840.10008.1.2.4.81"];
const JPEG: [&str; 4] = [
    "1.2.840.10008.1.2.4.50",
    "1.2.840.10008.1.2.4.51",
    "1.2.840.10008.1.2.4.57",
    "1.2.840.10008.1.2.4.70",
];

/// Smallest and largest character height, in pixels, that the detector considers.
const MIN_GLYPH_HEIGHT: usize = 5;
const MAX_GLYPH_HEIGHT: usize = 160;
/// A single component counts as text only from this height, such as a lone "L" marker.
const MIN_LONE_GLYPH_HEIGHT: usize = 9;
/// Share of the dynamic range above the background where text pixels start.
const TEXT_LEVEL: f64 = 0.8;
/// A frame has a background only when its darkest value is at most this share of its
/// brightest value.
const BACKGROUND_LEVEL: f64 = 0.05;
/// Most samples that one object may decode to, so that a header cannot demand an
/// unbounded allocation: 2^30 samples, or 2 GiB at 16 bits.
const MAX_DECODED_SAMPLES: usize = 1 << 30;
/// Most samples in one frame. The per-frame working set is about 11 bytes per sample,
/// plus a flood-fill stack of up to 8 bytes per sample in the worst case, so this keeps
/// it below 1.25 GiB. The largest mammography frames are about 2^24.5.
const MAX_FRAME_SAMPLES: usize = 1 << 26;
/// Most text lines in one frame after lone small candidates are dropped. A real frame
/// has a few dozen. The merge of overlapping boxes costs at most this squared.
const MAX_LINES: usize = 256;
/// Most candidate lines open at once during grouping, which bounds its cost per glyph.
const MAX_OPEN_LINES: usize = 4096;
/// Most bright components or dark regions in one frame, so that a noise or checkerboard
/// frame cannot force large region records. A real frame has far fewer.
const MAX_REGIONS: usize = 1 << 20;

/// What the router asks the anonymizer to do with burned-in text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum BurnedInText {
    /// `censor` under `safe-harbor-metadata`, and `off` under the other policies.
    #[default]
    Auto,
    /// Do not inspect pixel samples.
    Off,
    /// Decode every frame and censor detected text; fail closed when that is not possible.
    Censor,
}

/// Per-object counts for the anonymization report. They hold no pixel values or text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CensorCounts {
    pub frames_assessed: u64,
    pub regions_censored: u64,
    pub pixels_censored: u64,
    /// Regions of one or two glyphs left in place because their box holds more than
    /// background and text. They may be text that remains in the output.
    #[serde(default)]
    pub regions_left_in_place: u64,
}

/// The result of censoring one object.
pub(crate) struct Censored {
    pub counts: CensorCounts,
    /// The new native Pixel Data when a region was censored.
    pub pixel_data: Option<InMemElement>,
}

struct Geometry {
    rows: usize,
    columns: usize,
    frames: usize,
    bits_allocated: u16,
    bits_stored: u16,
    inverted: bool,
}

impl Geometry {
    /// The largest sample value that Bits Stored allows.
    fn max_value(&self) -> u16 {
        if self.bits_stored >= 16 {
            u16::MAX
        } else {
            (1_u16 << self.bits_stored) - 1
        }
    }
}

fn text(object: &InMemDicomObject, tag: dicom_core::Tag) -> Option<String> {
    object
        .get(tag)
        .and_then(|element| element.to_str().ok())
        .map(|value| {
            value
                .trim_matches(|c: char| c == ' ' || c == '\0')
                .to_owned()
        })
}

fn number(object: &InMemDicomObject, tag: dicom_core::Tag) -> Option<u64> {
    object
        .get(tag)
        .and_then(|element| element.to_int::<u64>().ok())
}

/// A numeric attribute: `default` when absent, an error when present but unreadable.
fn number_or(object: &InMemDicomObject, tag: dicom_core::Tag, default: u64) -> Result<u64> {
    match object.get(tag) {
        None => Ok(default),
        Some(element) => element
            .to_int::<u64>()
            .map_err(|_| not_assessed("a pixel description attribute is unreadable")),
    }
}

fn not_assessed(reason: &str) -> Error {
    Error::PixelTextNotAssessed(reason.to_owned())
}

fn geometry(object: &InMemDicomObject) -> Result<Geometry> {
    let samples = number_or(object, tags::SAMPLES_PER_PIXEL, 1)?;
    let photometric = text(object, tags::PHOTOMETRIC_INTERPRETATION).unwrap_or_default();
    if samples != 1 || !matches!(photometric.as_str(), "MONOCHROME1" | "MONOCHROME2") {
        return Err(not_assessed(
            "only single-sample MONOCHROME1 and MONOCHROME2 images are supported",
        ));
    }
    // With INVERSE, MONOCHROME2 text that displays bright is stored dark, which the
    // detector does not look for.
    if photometric == "MONOCHROME2"
        && text(object, tags::PRESENTATION_LUT_SHAPE).is_some_and(|shape| shape == "INVERSE")
    {
        return Err(not_assessed(
            "MONOCHROME2 with Presentation LUT Shape INVERSE is not supported",
        ));
    }
    // MG and DX pair MONOCHROME1 with INVERSE. With IDENTITY, text that the modality
    // stored bright displays bright, and the inverted view would not find it.
    if photometric == "MONOCHROME1"
        && text(object, tags::PRESENTATION_LUT_SHAPE).is_some_and(|shape| shape == "IDENTITY")
    {
        return Err(not_assessed(
            "MONOCHROME1 with Presentation LUT Shape IDENTITY is not supported",
        ));
    }
    // A Presentation LUT Sequence can invert the display, so text may not be stored bright.
    if object.get(tags::PRESENTATION_LUT_SEQUENCE).is_some() {
        return Err(not_assessed("a Presentation LUT Sequence is not supported"));
    }
    if number_or(object, tags::PIXEL_REPRESENTATION, 0)? != 0 {
        return Err(not_assessed("signed pixel samples are not supported"));
    }
    let size = |tag| {
        number(object, tag)
            .and_then(|value| usize::try_from(value).ok())
            .filter(|&value| value > 0)
    };
    let rows = size(tags::ROWS).ok_or_else(|| not_assessed("Rows is missing"))?;
    let columns = size(tags::COLUMNS).ok_or_else(|| not_assessed("Columns is missing"))?;
    let frames = text(object, tags::NUMBER_OF_FRAMES)
        .map(|value| value.parse::<usize>().ok().filter(|&count| count > 0))
        .unwrap_or(Some(1))
        .ok_or_else(|| not_assessed("Number of Frames is invalid"))?;
    let bits_allocated = number(object, tags::BITS_ALLOCATED)
        .and_then(|value| u16::try_from(value).ok())
        .filter(|&value| value == 8 || value == 16)
        .ok_or_else(|| not_assessed("only 8- and 16-bit samples are supported"))?;
    // A missing Bits Stored means all allocated bits. An invalid one is not guessed.
    let bits_stored = match object.get(tags::BITS_STORED) {
        None => bits_allocated,
        Some(element) => element
            .to_int::<u64>()
            .ok()
            .and_then(|value| u16::try_from(value).ok())
            .filter(|&value| value > 0 && value <= bits_allocated)
            .ok_or_else(|| not_assessed("Bits Stored is invalid"))?,
    };
    // Samples are read as the low Bits Stored bits, which needs High Bit = Bits Stored - 1.
    if number(object, tags::HIGH_BIT) != Some(u64::from(bits_stored) - 1) {
        return Err(not_assessed("High Bit is not Bits Stored - 1"));
    }
    Ok(Geometry {
        rows,
        columns,
        frames,
        bits_allocated,
        bits_stored,
        inverted: photometric == "MONOCHROME1",
    })
}

/// Decode every frame into one sample buffer, frame after frame.
fn decode_frames(
    object: &InMemDicomObject,
    transfer_syntax: &str,
    geometry: &Geometry,
) -> Result<Vec<u16>> {
    let element = object
        .get(tags::PIXEL_DATA)
        .ok_or_else(|| not_assessed("Pixel Data is missing"))?;
    let frame_samples = geometry
        .rows
        .checked_mul(geometry.columns)
        .filter(|&samples| samples <= MAX_FRAME_SAMPLES)
        .ok_or_else(|| not_assessed("Rows and Columns are too large"))?;
    if frame_samples
        .checked_mul(geometry.frames)
        .is_none_or(|total| total > MAX_DECODED_SAMPLES)
    {
        return Err(not_assessed(
            "the Pixel Data geometry is too large to decode",
        ));
    }
    if NATIVE_TRANSFER_SYNTAXES.contains(&transfer_syntax) {
        // Parsed OW words are already in host order; only raw bytes are little endian.
        // 8-bit samples in OW words, as in Implicit VR Little Endian, go through the byte
        // path below.
        if let (Value::Primitive(PrimitiveValue::U16(values)), 16) =
            (element.value(), geometry.bits_allocated)
        {
            if values.len() != frame_samples * geometry.frames {
                return Err(not_assessed(
                    "native Pixel Data does not match its geometry",
                ));
            }
            return Ok(values.to_vec());
        }
        let bytes = element
            .to_bytes()
            .map_err(|_| not_assessed("native Pixel Data cannot be read"))?;
        let width = usize::from(geometry.bits_allocated / 8);
        let needed = frame_samples
            .checked_mul(geometry.frames)
            .and_then(|samples| samples.checked_mul(width))
            .ok_or_else(|| not_assessed("the Pixel Data geometry is too large"))?;
        // One padding byte is allowed. More data than the geometry would go uninspected.
        if bytes.len() != needed && bytes.len() != needed + 1 {
            return Err(not_assessed(
                "native Pixel Data does not match its geometry",
            ));
        }
        let bytes = &bytes[..needed];
        return Ok(if width == 1 {
            bytes.iter().map(|&value| u16::from(value)).collect()
        } else {
            bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect()
        });
    }
    let Value::PixelSequence(sequence) = element.value() else {
        return Err(not_assessed("this transfer syntax is not supported"));
    };
    let fragments = sequence.fragments();
    let codestreams: Vec<std::borrow::Cow<'_, [u8]>> = if geometry.frames == 1 {
        vec![fragments.concat().into()]
    } else if fragments.len() == geometry.frames {
        fragments
            .iter()
            .map(|fragment| fragment[..].into())
            .collect()
    } else {
        return Err(not_assessed(
            "multi-frame Pixel Data needs one fragment per frame",
        ));
    };
    let (rows, columns) = (geometry.rows as u32, geometry.columns as u32);
    let mut decoded = Vec::new();
    for codestream in &codestreams {
        let frame = (|| {
            // An odd-length codestream is padded with one zero byte after its end marker.
            let codestream = if codestream.ends_with(&[0xFF, 0xD9, 0x00]) {
                &codestream[..codestream.len() - 1]
            } else {
                &codestream[..]
            };
            let samples = if JPEG_2000.contains(&transfer_syntax) {
                jpeg2k::decode(codestream, columns, rows)
                    .map(|(samples, _, _)| samples)
                    .map_err(|_| not_assessed("a JPEG 2000 frame cannot be decoded"))?
            } else if JPEG_LS.contains(&transfer_syntax) {
                jpegls::decode(codestream, columns, rows)
                    .map(|(samples, _, _)| samples)
                    .map_err(|_| not_assessed("a JPEG-LS frame cannot be decoded"))?
            } else if JPEG.contains(&transfer_syntax) {
                decode_jpeg(codestream, geometry)?
            } else {
                return Err(not_assessed("this transfer syntax is not supported"));
            };
            if samples.len() != frame_samples {
                return Err(not_assessed(
                    "a decoded frame does not match Rows and Columns",
                ));
            }
            Ok(samples)
        })()?;
        // Reserve all frames once the first one has decoded, so a header alone cannot
        // force a large allocation and the buffer does not grow past its need.
        if decoded.is_empty() {
            decoded.reserve_exact(frame_samples * geometry.frames);
        }
        decoded.extend_from_slice(&frame);
    }
    Ok(decoded)
}

fn decode_jpeg(codestream: &[u8], geometry: &Geometry) -> Result<Vec<u16>> {
    let mut decoder = jpeg_decoder::Decoder::new(codestream);
    // Check the frame size before decoding, so that a header cannot force a large decode.
    decoder
        .read_info()
        .map_err(|_| not_assessed("a JPEG frame cannot be decoded"))?;
    let info = decoder
        .info()
        .ok_or_else(|| not_assessed("a JPEG frame has no header"))?;
    if usize::from(info.width) != geometry.columns || usize::from(info.height) != geometry.rows {
        return Err(not_assessed("a JPEG frame does not match Rows and Columns"));
    }
    let pixels = decoder
        .decode()
        .map_err(|_| not_assessed("a JPEG frame cannot be decoded"))?;
    match info.pixel_format {
        jpeg_decoder::PixelFormat::L8 => Ok(pixels.into_iter().map(u16::from).collect()),
        jpeg_decoder::PixelFormat::L16 => Ok(pixels
            .chunks_exact(2)
            // jpeg-decoder returns 16-bit samples in native byte order.
            .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
            .collect()),
        _ => Err(not_assessed("a JPEG frame is not grayscale")),
    }
}

/// A connected component of text-level pixels.
#[derive(Clone, Copy)]
struct Component {
    /// Index of the component in `components`' result; labels refer to it.
    id: u32,
    top: usize,
    left: usize,
    bottom: usize,
    right: usize,
    area: usize,
}

impl Component {
    fn height(&self) -> usize {
        self.bottom - self.top + 1
    }
    fn width(&self) -> usize {
        self.right - self.left + 1
    }
    /// A dot, a dash or a stroke of text of glyph height `height`, such as `.`, `:`,
    /// `-` or a solid `I`, which `is_glyph` rejects for its size or fill.
    fn is_text_mark(&self, height: usize) -> bool {
        let thin = (height / 3).max(3);
        let long = height + height / 5;
        self.area >= 3
            && self.height().min(self.width()) <= thin
            && self.height().max(self.width()) <= long
    }
    fn is_glyph(&self, rows: usize) -> bool {
        let (height, width) = (self.height(), self.width());
        let fill = self.area as f64 / (height * width) as f64;
        (MIN_GLYPH_HEIGHT..=MAX_GLYPH_HEIGHT.min(rows / 4).max(MIN_GLYPH_HEIGHT)).contains(&height)
            && width <= 3 * height
            && self.area >= 6
            && (0.08..=0.9).contains(&fill)
    }
}

/// 8-connected components of the pixels at or above `level`, and a label for each pixel:
/// the component's id plus one, or zero for a pixel below `level`.
fn components(
    samples: &[u16],
    rows: usize,
    columns: usize,
    level: u16,
) -> Result<(Vec<Component>, Vec<u32>)> {
    let mut labels = vec![0_u32; samples.len()];
    let mut seen = vec![false; samples.len()];
    let mut found = Vec::new();
    let mut stack = Vec::new();
    for start in 0..samples.len() {
        if seen[start] || samples[start] < level {
            continue;
        }
        if found.len() == MAX_REGIONS {
            return Err(not_assessed("a frame has too many bright components"));
        }
        seen[start] = true;
        stack.push(start);
        let (row, column) = (start / columns, start % columns);
        let id = u32::try_from(found.len()).unwrap_or(u32::MAX);
        let mut component = Component {
            id,
            top: row,
            left: column,
            bottom: row,
            right: column,
            area: 0,
        };
        while let Some(index) = stack.pop() {
            let (row, column) = (index / columns, index % columns);
            labels[index] = id.saturating_add(1);
            component.area += 1;
            component.top = component.top.min(row);
            component.bottom = component.bottom.max(row);
            component.left = component.left.min(column);
            component.right = component.right.max(column);
            for (dr, dc) in [
                (-1, -1),
                (-1, 0),
                (-1, 1),
                (0, -1),
                (0, 1),
                (1, -1),
                (1, 0),
                (1, 1),
            ] {
                let (r, c) = (row as isize + dr, column as isize + dc);
                if r < 0 || c < 0 || r >= rows as isize || c >= columns as isize {
                    continue;
                }
                let neighbor = r as usize * columns + c as usize;
                if !seen[neighbor] && samples[neighbor] >= level {
                    seen[neighbor] = true;
                    stack.push(neighbor);
                }
            }
        }
        found.push(component);
    }
    Ok((found, labels))
}

/// A 4-connected region of pixels of the frame's darkest value.
#[derive(Clone, Copy)]
struct DarkRegion {
    top: usize,
    left: usize,
    bottom: usize,
    right: usize,
    /// The region reaches the frame border, so it lies outside the imaged anatomy.
    border: bool,
}

/// 4-connected regions of the pixels of value `low`, and a label for each pixel: the
/// region's index plus one, or zero for a brighter pixel.
fn dark_regions(
    samples: &[u16],
    rows: usize,
    columns: usize,
    low: u16,
) -> Result<(Vec<DarkRegion>, Vec<u32>)> {
    let mut labels = vec![0_u32; samples.len()];
    let mut regions = Vec::new();
    let mut stack = Vec::new();
    for start in 0..samples.len() {
        if labels[start] != 0 || samples[start] != low {
            continue;
        }
        if regions.len() == MAX_REGIONS {
            return Err(not_assessed("a frame has too many dark regions"));
        }
        let label = u32::try_from(regions.len() + 1).unwrap_or(u32::MAX);
        let (row, column) = (start / columns, start % columns);
        let mut region = DarkRegion {
            top: row,
            left: column,
            bottom: row,
            right: column,
            border: false,
        };
        labels[start] = label;
        stack.push(start);
        while let Some(index) = stack.pop() {
            let (row, column) = (index / columns, index % columns);
            region.top = region.top.min(row);
            region.bottom = region.bottom.max(row);
            region.left = region.left.min(column);
            region.right = region.right.max(column);
            region.border |= row == 0 || column == 0 || row + 1 == rows || column + 1 == columns;
            let mut visit = |neighbor: usize| {
                if labels[neighbor] == 0 && samples[neighbor] == low {
                    labels[neighbor] = label;
                    stack.push(neighbor);
                }
            };
            if row > 0 {
                visit(index - columns);
            }
            if row + 1 < rows {
                visit(index + columns);
            }
            if column > 0 {
                visit(index - 1);
            }
            if column + 1 < columns {
                visit(index + 1);
            }
        }
        regions.push(region);
    }
    Ok((regions, labels))
}

/// Group glyphs into text lines: similar height, aligned centers, small gaps.
fn text_lines(glyphs: &[Component]) -> Result<Vec<Vec<Component>>> {
    let mut order: Vec<usize> = (0..glyphs.len()).collect();
    order.sort_by_key(|&index| (glyphs[index].left, glyphs[index].top));
    let mut lines: Vec<Vec<Component>> = Vec::new();
    let mut open: Vec<Vec<Component>> = Vec::new();
    for index in order {
        let glyph = glyphs[index];
        // Glyphs come in order of `left`, and a gap is at most 2 * 2 heights of the last
        // glyph, so a line that ends further to the left can take no more glyphs.
        let mut index = 0;
        while index < open.len() {
            let last = open[index].last().expect("a line has a glyph");
            if glyph.left > last.right + 4 * last.height() {
                lines.push(open.remove(index));
            } else {
                index += 1;
            }
        }
        let joined = open.iter_mut().find(|line| {
            let last = line.last().expect("a line has a glyph");
            let (h1, h2) = (last.height() as f64, glyph.height() as f64);
            let center = |c: &Component| (c.top + c.bottom) as f64 / 2.0;
            let gap = glyph.left as isize - last.right as isize;
            (0.5..=2.0).contains(&(h1 / h2))
                && (center(last) - center(&glyph)).abs() <= 0.6 * h1.max(h2)
                && gap >= -1
                && (gap as f64) <= 2.0 * h1.max(h2)
        });
        match joined {
            Some(line) => line.push(glyph),
            None => {
                if open.len() >= MAX_OPEN_LINES {
                    return Err(not_assessed("a frame has too many text-like lines"));
                }
                open.push(vec![glyph]);
            }
        }
    }
    lines.extend(open);
    let lines: Vec<_> = lines
        .into_iter()
        .filter(|line| line.len() >= 2 || line[0].height() >= MIN_LONE_GLYPH_HEIGHT)
        .collect();
    if lines.len() > MAX_LINES {
        return Err(not_assessed("a frame has too many text-like lines"));
    }
    Ok(lines)
}

/// Censor the text in one frame, in place. Returns the regions censored and pixels
/// censored.
#[cfg(test)]
fn censor_frame(samples: &mut [u16], geometry: &Geometry, frame: usize) -> Result<(u64, u64)> {
    censor_frame_counted(samples, geometry, frame).map(|counts| (counts.0, counts.1))
}

/// Merge boxes that overlap, directly or through other boxes, with a union-find over
/// all pairs. The result is in order of each region's first box.
fn merge_overlapping(boxes: Vec<Region>) -> Vec<Region> {
    fn root(parent: &mut [usize], mut index: usize) -> usize {
        while parent[index] != index {
            parent[index] = parent[parent[index]];
            index = parent[index];
        }
        index
    }
    let mut parent: Vec<usize> = (0..boxes.len()).collect();
    for a in 0..boxes.len() {
        for b in a + 1..boxes.len() {
            let (x, y) = (&boxes[a], &boxes[b]);
            if x.top <= y.bottom && y.top <= x.bottom && x.left <= y.right && y.left <= x.right {
                let (ra, rb) = (root(&mut parent, a), root(&mut parent, b));
                parent[ra.max(rb)] = ra.min(rb);
            }
        }
    }
    let mut merged: Vec<Region> = Vec::new();
    let mut slot = vec![usize::MAX; boxes.len()];
    for (index, region) in boxes.into_iter().enumerate() {
        let group = root(&mut parent, index);
        if slot[group] == usize::MAX {
            slot[group] = merged.len();
            merged.push(region);
        } else {
            let target = &mut merged[slot[group]];
            target.lines.extend(region.lines);
            target.top = target.top.min(region.top);
            target.left = target.left.min(region.left);
            target.bottom = target.bottom.max(region.bottom);
            target.right = target.right.max(region.right);
        }
    }
    merged
}

/// One text region: lines whose padded boxes overlap, merged.
struct Region {
    lines: Vec<Vec<Component>>,
    top: usize,
    left: usize,
    bottom: usize,
    right: usize,
}

/// Censor the text in one frame, in place. Returns the regions censored, the pixels
/// censored and the regions left in place.
fn censor_frame_counted(
    samples: &mut [u16],
    geometry: &Geometry,
    frame: usize,
) -> Result<(u64, u64, u64)> {
    let (rows, columns) = (geometry.rows, geometry.columns);
    // Work in a view where text is bright and the background is dark.
    let max_value = geometry.max_value();
    let view: Vec<u16> = if geometry.inverted {
        samples
            .iter()
            .map(|&value| max_value - value.min(max_value))
            .collect()
    } else {
        samples.to_vec()
    };
    let (Some(&low), Some(&high)) = (view.iter().min(), view.iter().max()) else {
        return Ok((0, 0, 0));
    };
    let range = f64::from(high - low);
    if range == 0.0 {
        return Ok((0, 0, 0));
    }
    let text_level = low + (TEXT_LEVEL * range).ceil() as u16;
    // The background is the dark end of the frame's own range, which does not depend on
    // Bits Stored. A frame whose darkest value is not near black has no background, so
    // every candidate there is over tissue.
    let has_background = f64::from(low) <= BACKGROUND_LEVEL * f64::from(high);
    // Only pixels of the frame's darkest value are background, because any brighter pixel
    // may be faint tissue. A dark region that reaches the frame border lies outside the
    // imaged anatomy. Any other dark region, such as a glyph's hole or a dark patch in
    // tissue, counts only when it lies strictly inside the box: then every pixel next to
    // it is in the box and checked, so it is closed in by text alone.
    let (found, labels) = components(&view, rows, columns, text_level)?;
    let glyphs: Vec<Component> = found
        .iter()
        .copied()
        .filter(|component| component.is_glyph(rows))
        .collect();

    let lines = text_lines(&glyphs)?;
    if lines.is_empty() {
        return Ok((0, 0, 0));
    }
    let (dark, dark_labels) = if has_background {
        dark_regions(&view, rows, columns, low)?
    } else {
        (Vec::new(), vec![0; view.len()])
    };

    // Each line gets a box with a margin of a quarter of its glyph height. Lines whose
    // boxes overlap, such as stacked lines of a header, form one region, so that one
    // line's glyphs do not stop the write of the next.
    let boxes: Vec<Region> = lines
        .into_iter()
        .map(|line| {
            let height = line.iter().map(Component::height).max().unwrap_or(0);
            let margin = (height / 4).max(2);
            let top = line.iter().map(|g| g.top).min().unwrap_or(0);
            let left = line.iter().map(|g| g.left).min().unwrap_or(0);
            let bottom = line.iter().map(|g| g.bottom).max().unwrap_or(0);
            let right = line.iter().map(|g| g.right).max().unwrap_or(0);
            Region {
                lines: vec![line],
                top: top.saturating_sub(margin),
                left: left.saturating_sub(margin),
                bottom: (bottom + margin).min(rows - 1),
                right: (right + margin).min(columns - 1),
            }
        })
        .collect();
    // A merged box can reach a box that overlapped neither part, so merge until stable.
    let mut text_regions = merge_overlapping(boxes);
    loop {
        let count = text_regions.len();
        text_regions = merge_overlapping(text_regions);
        if text_regions.len() == count {
            break;
        }
    }

    // The region of each glyph, by component label.
    let mut owner = vec![0_u32; found.len()];
    for (index, region) in text_regions.iter().enumerate() {
        for glyph in region.lines.iter().flatten() {
            owner[glyph.id as usize] = index as u32 + 1;
        }
    }
    let fill = if geometry.inverted {
        max_value - low
    } else {
        low
    };
    let (mut censored, mut pixels, mut left_in_place) = (0, 0, 0);
    for (index, region) in text_regions.iter().enumerate() {
        let (top, left, bottom, right) = (region.top, region.left, region.bottom, region.right);
        let region_label = index as u32 + 1;
        // Every pixel in the box must be background or text: one of the region's glyphs,
        // or a dot, dash or stroke. Only text is written; background pixels are not.
        // Anything else, such as tissue, a bright structure that is not a text mark or
        // extends out of the box, or a glyph's anti-aliased edge, which cannot be told
        // apart from tissue next to it, stops the write. A text mark must lie in the rows
        // of one line's glyphs, not in a margin, and inside the box, so that every pixel
        // next to it is in the box and checked.
        // The glyph rows and height of each line.
        let bands: Vec<(usize, usize, usize)> = region
            .lines
            .iter()
            .map(|line| {
                (
                    line.iter().map(|g| g.top).min().unwrap_or(0),
                    line.iter().map(|g| g.bottom).max().unwrap_or(0),
                    line.iter().map(Component::height).max().unwrap_or(0),
                )
            })
            .collect();
        let is_mark = |component: &Component| {
            component.left > left
                && component.right < right
                && component.top > top
                && component.bottom < bottom
                && bands.iter().any(|&(band_top, band_bottom, height)| {
                    component.top >= band_top
                        && component.bottom <= band_bottom
                        && component.is_text_mark(height)
                })
        };
        // Each component is judged once per region.
        let mut judged: std::collections::HashMap<u32, bool> = std::collections::HashMap::new();
        let mut inside = |label: u32| {
            label > 0
                && (owner[(label - 1) as usize] == region_label
                    || *judged
                        .entry(label)
                        .or_insert_with(|| is_mark(&found[(label - 1) as usize])))
        };
        // Only pixels of the frame's darkest value are background, because any brighter
        // pixel may be faint tissue. A dark region that reaches the frame border lies
        // outside the imaged anatomy. Any other dark region, such as a glyph's hole or a
        // dark patch in tissue, counts only when it lies strictly inside the box: then
        // every pixel next to it is in the box and checked, so it is closed in by text.
        let background = |label: u32| {
            label > 0 && {
                let dark = &dark[(label - 1) as usize];
                dark.border
                    || (dark.top > top
                        && dark.bottom < bottom
                        && dark.left > left
                        && dark.right < right)
            }
        };
        let mut other = !has_background;
        let mut text = Vec::new();
        'scan: for row in top..=bottom {
            for column in left..=right {
                let index = row * columns + column;
                if inside(labels[index]) {
                    text.push(index);
                } else if !background(dark_labels[index]) {
                    other = true;
                    break 'scan;
                }
            }
        }
        if other {
            if region.lines.iter().any(|line| line.len() >= 3) {
                // Text over tissue cannot be censored without censoring tissue.
                return Err(Error::BurnedInTextOverTissue { frame });
            }
            // One or two glyphs, such as a calcification or a marker on the skin line.
            left_in_place += 1;
            continue;
        }
        for &index in &text {
            // A pixel in two text lists is counted once.
            if samples[index] != fill {
                samples[index] = fill;
                pixels += 1;
            }
        }
        censored += 1;
    }
    Ok((censored, pixels, left_in_place))
}

/// Decode, inspect and censor every frame of an object.
pub(crate) fn censor(object: &InMemDicomObject, transfer_syntax: &str) -> Result<Censored> {
    if object.get(tags::PIXEL_DATA).is_none()
        && object.get(tags::FLOAT_PIXEL_DATA).is_none()
        && object.get(tags::DOUBLE_FLOAT_PIXEL_DATA).is_none()
    {
        return Ok(Censored {
            counts: CensorCounts::default(),
            pixel_data: None,
        });
    }
    if object.get(tags::FLOAT_PIXEL_DATA).is_some()
        || object.get(tags::DOUBLE_FLOAT_PIXEL_DATA).is_some()
    {
        return Err(not_assessed("floating-point Pixel Data is not supported"));
    }
    let geometry = geometry(object)?;
    let mut samples = decode_frames(object, transfer_syntax, &geometry)?;
    // A sample above Bits Stored is content that the detector would not see.
    let max_value = geometry.max_value();
    if samples.iter().any(|&value| value > max_value) {
        return Err(not_assessed("a sample uses bits above Bits Stored"));
    }
    let mut counts = CensorCounts::default();
    for (index, frame) in samples
        .chunks_mut(geometry.rows * geometry.columns)
        .enumerate()
    {
        let (regions, pixels, left_in_place) = censor_frame_counted(frame, &geometry, index)?;
        counts.frames_assessed += 1;
        counts.regions_censored += regions;
        counts.pixels_censored += pixels;
        counts.regions_left_in_place += left_in_place;
    }
    if counts.regions_censored == 0 {
        return Ok(Censored {
            counts,
            pixel_data: None,
        });
    }
    // The 16-bit output takes the decoded buffer without a copy.
    let pixel_data = if geometry.bits_allocated == 8 {
        let mut bytes: Vec<u8> = samples.into_iter().map(|value| value as u8).collect();
        // PS3.5 7.1: a value has even length.
        if bytes.len() % 2 == 1 {
            bytes.push(0);
        }
        InMemElement::new(tags::PIXEL_DATA, VR::OB, PrimitiveValue::U8(bytes.into()))
    } else {
        InMemElement::new(
            tags::PIXEL_DATA,
            VR::OW,
            PrimitiveValue::U16(samples.into()),
        )
    };
    Ok(Censored {
        counts,
        pixel_data: Some(pixel_data),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw_block_text(
        samples: &mut [u16],
        columns: usize,
        top: usize,
        left: usize,
        glyphs: usize,
        value: u16,
    ) {
        // Each glyph is an 8x6 "H"-like shape with a gap between glyphs.
        for glyph in 0..glyphs {
            let x = left + glyph * 9;
            for row in top..top + 10 {
                samples[row * columns + x] = value;
                samples[row * columns + x + 5] = value;
            }
            for column in x..x + 6 {
                samples[(top + 5) * columns + column] = value;
            }
        }
    }

    fn geometry(rows: usize, columns: usize) -> Geometry {
        Geometry {
            rows,
            columns,
            frames: 1,
            bits_allocated: 16,
            bits_stored: 12,
            inverted: false,
        }
    }

    #[test]
    fn text_on_the_background_is_censored_and_the_rest_is_unchanged() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![0_u16; rows * columns];
        // Tissue on the right half.
        for row in 0..rows {
            for column in 80..columns {
                samples[row * columns + column] = 1500;
            }
        }
        draw_block_text(&mut samples, columns, 4, 4, 5, 4095);
        let original = samples.clone();
        let (regions, pixels) = censor_frame(&mut samples, &geometry(rows, columns), 0).unwrap();
        assert_eq!(regions, 1);
        assert!(pixels > 0);
        assert!(samples[..].iter().all(|&value| value != 4095));
        for row in 0..rows {
            for column in 80..columns {
                assert_eq!(
                    samples[row * columns + column],
                    original[row * columns + column]
                );
            }
        }
    }

    #[test]
    fn a_line_with_tissue_in_its_margin_fails_closed_and_writes_nothing() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![0_u16; rows * columns];
        // Tissue touches the text, inside the censor margin.
        for row in 0..rows {
            for column in 46..columns {
                samples[row * columns + column] = 1500;
            }
        }
        draw_block_text(&mut samples, columns, 4, 4, 5, 4095);
        let original = samples.clone();
        assert!(matches!(
            censor_frame(&mut samples, &geometry(rows, columns), 0),
            Err(Error::BurnedInTextOverTissue { frame: 0 })
        ));
        assert_eq!(samples, original);
    }

    #[test]
    fn thin_tissue_touching_a_glyph_fails_closed_and_is_unchanged() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![0_u16; rows * columns];
        draw_block_text(&mut samples, columns, 4, 4, 5, 4095);
        // One row of tissue directly above the glyphs.
        for column in 4..40 {
            samples[3 * columns + column] = 1500;
        }
        let original = samples.clone();
        assert!(censor_frame(&mut samples, &geometry(rows, columns), 0).is_err());
        assert_eq!(samples, original);
    }

    #[test]
    fn a_bright_structure_that_is_not_a_glyph_stops_the_write() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![0_u16; rows * columns];
        draw_block_text(&mut samples, columns, 4, 4, 5, 4095);
        // A large bright block that is too big to be a glyph. The last glyph ends at
        // column 45, so its margin reaches column 47.
        for row in 0..rows {
            for column in 47..columns {
                samples[row * columns + column] = 4000;
            }
        }
        let original = samples.clone();
        assert!(censor_frame(&mut samples, &geometry(rows, columns), 0).is_err());
        assert_eq!(samples, original);
    }

    #[test]
    fn tissue_just_outside_the_margin_is_unchanged() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![0_u16; rows * columns];
        // The line ends at column 49, and its margin of 2 at column 51.
        for row in 0..rows {
            for column in 52..columns {
                samples[row * columns + column] = 1500;
            }
        }
        draw_block_text(&mut samples, columns, 4, 4, 5, 4095);
        let original = samples.clone();
        let (regions, _) = censor_frame(&mut samples, &geometry(rows, columns), 0).unwrap();
        assert_eq!(regions, 1);
        for row in 0..rows {
            for column in 52..columns {
                assert_eq!(
                    samples[row * columns + column],
                    original[row * columns + column]
                );
            }
        }
    }

    #[test]
    fn faint_tissue_in_the_margin_fails_closed_and_is_unchanged() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![0_u16; rows * columns];
        draw_block_text(&mut samples, columns, 4, 4, 5, 4095);
        // Tissue at 2.4% of the range, inside the margin of the last glyph.
        for row in 0..rows {
            for column in 47..columns {
                samples[row * columns + column] = 100;
            }
        }
        let original = samples.clone();
        assert!(matches!(
            censor_frame(&mut samples, &geometry(rows, columns), 0),
            Err(Error::BurnedInTextOverTissue { frame: 0 })
        ));
        assert_eq!(samples, original);
    }

    #[test]
    fn only_text_pixels_are_written() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![0_u16; rows * columns];
        draw_block_text(&mut samples, columns, 4, 4, 5, 4095);
        // Faint tissue outside the box stays as it is.
        for column in 0..60 {
            samples[30 * columns + column] = 15;
        }
        let original = samples.clone();
        let (regions, pixels) = censor_frame(&mut samples, &geometry(rows, columns), 0).unwrap();
        assert_eq!(regions, 1);
        let changed = samples
            .iter()
            .zip(&original)
            .filter(|(new, old)| new != old)
            .count();
        assert_eq!(pixels, changed as u64);
        assert!(samples.iter().all(|&value| value != 4095));
        assert!(samples[30 * columns..30 * columns + 60]
            .iter()
            .all(|&value| value == 15));
    }

    #[test]
    fn punctuation_and_solid_strokes_in_a_line_are_censored() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![0_u16; rows * columns];
        draw_block_text(&mut samples, columns, 4, 4, 3, 4095);
        // A colon of two 2x2 dots and a solid 2x10 bar, such as "I" or "1".
        for (row, column) in [
            (6, 31),
            (6, 32),
            (7, 31),
            (7, 32),
            (11, 31),
            (11, 32),
            (12, 31),
            (12, 32),
        ] {
            samples[row * columns + column] = 4095;
        }
        for row in 4..14 {
            samples[row * columns + 36] = 4095;
            samples[row * columns + 37] = 4095;
        }
        draw_block_text(&mut samples, columns, 4, 40, 2, 4095);
        let (regions, _) = censor_frame(&mut samples, &geometry(rows, columns), 0).unwrap();
        assert_eq!(regions, 1);
        assert!(samples.iter().all(|&value| value != 4095));
    }

    #[test]
    fn a_solid_block_between_glyphs_fails_closed_and_is_unchanged() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![0_u16; rows * columns];
        draw_block_text(&mut samples, columns, 4, 4, 2, 4095);
        // A 10x10 bright block, as high as the glyphs, then one more glyph.
        for row in 4..14 {
            for column in 22..32 {
                samples[row * columns + column] = 4095;
            }
        }
        draw_block_text(&mut samples, columns, 4, 34, 1, 4095);
        let original = samples.clone();
        assert!(matches!(
            censor_frame(&mut samples, &geometry(rows, columns), 0),
            Err(Error::BurnedInTextOverTissue { frame: 0 })
        ));
        assert_eq!(samples, original);
    }

    #[test]
    fn a_bright_disk_in_faint_tissue_is_left_alone() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![0_u16; rows * columns];
        for row in 0..rows {
            for column in 64..columns {
                samples[row * columns + column] = 15;
            }
        }
        for row in 0..rows {
            for column in 0..columns {
                let (dy, dx) = (row as isize - 32, column as isize - 96);
                if dy * dy + dx * dx <= 25 {
                    samples[row * columns + column] = 4095;
                }
            }
        }
        let original = samples.clone();
        assert_eq!(
            censor_frame(&mut samples, &geometry(rows, columns), 0).unwrap(),
            (0, 0)
        );
        assert_eq!(samples, original);
    }

    #[test]
    fn the_censored_pixel_count_is_the_number_of_changed_pixels() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![0_u16; rows * columns];
        draw_block_text(&mut samples, columns, 4, 4, 5, 4095);
        draw_block_text(&mut samples, columns, 16, 4, 5, 4095);
        let original = samples.clone();
        let (_, pixels) = censor_frame(&mut samples, &geometry(rows, columns), 0).unwrap();
        let changed = samples
            .iter()
            .zip(&original)
            .filter(|(new, old)| new != old)
            .count();
        assert_eq!(pixels, changed as u64);
    }

    #[test]
    fn a_detached_speck_in_the_margin_fails_closed_and_is_unchanged() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![0_u16; rows * columns];
        draw_block_text(&mut samples, columns, 4, 4, 5, 4095);
        // A 1x3 bright structure in the top margin, above the glyph rows.
        for column in 20..23 {
            samples[2 * columns + column] = 4095;
        }
        let original = samples.clone();
        assert!(matches!(
            censor_frame(&mut samples, &geometry(rows, columns), 0),
            Err(Error::BurnedInTextOverTissue { frame: 0 })
        ));
        assert_eq!(samples, original);
    }

    #[test]
    fn a_bright_disk_in_a_dark_patch_inside_tissue_is_left_alone() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![1500_u16; rows * columns];
        // A dark patch of the frame's darkest value, closed in by tissue, with a disk.
        for row in 20..44 {
            for column in 40..80 {
                samples[row * columns + column] = 0;
            }
        }
        for row in 0..rows {
            for column in 0..columns {
                let (dy, dx) = (row as isize - 32, column as isize - 60);
                if dy * dy + dx * dx <= 25 {
                    samples[row * columns + column] = 4095;
                }
            }
        }
        let original = samples.clone();
        assert_eq!(
            censor_frame(&mut samples, &geometry(rows, columns), 0).unwrap(),
            (0, 0)
        );
        assert_eq!(samples, original);
    }

    #[test]
    fn text_in_a_dark_box_over_tissue_fails_closed() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![1500_u16; rows * columns];
        for row in 10..30 {
            for column in 10..70 {
                samples[row * columns + column] = 0;
            }
        }
        draw_block_text(&mut samples, columns, 14, 16, 5, 4095);
        let original = samples.clone();
        assert!(matches!(
            censor_frame(&mut samples, &geometry(rows, columns), 0),
            Err(Error::BurnedInTextOverTissue { frame: 0 })
        ));
        assert_eq!(samples, original);
    }

    #[test]
    fn a_checkerboard_frame_fails_closed_before_it_records_every_region() {
        let (rows, columns) = (2048, 2048);
        // A text line on a clean strip, and a checkerboard of isolated dark pixels.
        let mut samples: Vec<u16> = (0..rows * columns)
            .map(|index| {
                let (row, column) = (index / columns, index % columns);
                if column >= 64 && (row + column) % 2 == 1 {
                    4095
                } else {
                    0
                }
            })
            .collect();
        draw_block_text(&mut samples, columns, 4, 4, 5, 4095);
        assert!(matches!(
            censor_frame(&mut samples, &geometry(rows, columns), 0),
            Err(Error::PixelTextNotAssessed(_))
        ));
    }

    #[test]
    fn stacked_lines_whose_margins_overlap_are_censored_together() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![0_u16; rows * columns];
        // Two lines with a gap of 1 row: each line's glyphs are in the other's margin.
        draw_block_text(&mut samples, columns, 4, 4, 5, 4095);
        draw_block_text(&mut samples, columns, 15, 4, 5, 4095);
        let (regions, _) = censor_frame(&mut samples, &geometry(rows, columns), 0).unwrap();
        assert_eq!(regions, 1);
        assert!(samples.iter().all(|&value| value != 4095));
    }

    #[test]
    fn a_short_line_left_in_place_is_counted() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![0_u16; rows * columns];
        draw_block_text(&mut samples, columns, 4, 4, 2, 4095);
        // Faint tissue next to the two glyphs.
        samples[3 * columns + 6] = 100;
        let original = samples.clone();
        assert_eq!(
            censor_frame_counted(&mut samples, &geometry(rows, columns), 0).unwrap(),
            (0, 0, 1)
        );
        assert_eq!(samples, original);
    }

    #[test]
    fn a_frame_with_too_many_text_like_lines_fails_closed() {
        let (rows, columns) = (1024, 1024);
        let mut samples = vec![0_u16; rows * columns];
        // A grid of separate glyphs, 30 pixels apart: more than 1000 lines.
        for top in (2..rows - 12).step_by(30) {
            for left in (2..columns - 8).step_by(30) {
                draw_block_text(&mut samples, columns, top, left, 1, 4095);
            }
        }
        assert!(matches!(
            censor_frame(&mut samples, &geometry(rows, columns), 0),
            Err(Error::PixelTextNotAssessed(_))
        ));
    }

    #[test]
    fn a_one_or_two_pixel_speck_between_glyphs_fails_closed() {
        for speck in [1, 2] {
            let (rows, columns) = (64, 128);
            let mut samples = vec![0_u16; rows * columns];
            draw_block_text(&mut samples, columns, 4, 4, 5, 4095);
            // Column 11 is two pixels from each neighbouring glyph.
            for row in 0..speck {
                samples[(7 + row) * columns + 11] = 4095;
            }
            let original = samples.clone();
            assert!(censor_frame(&mut samples, &geometry(rows, columns), 0).is_err());
            assert_eq!(samples, original);
        }
    }

    #[test]
    fn a_mark_at_the_frame_edge_fails_closed() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![0_u16; rows * columns];
        draw_block_text(&mut samples, columns, 0, 4, 5, 4095);
        // A 1x3 component touching row 0 between two glyphs.
        for row in 0..3 {
            samples[row * columns + 11] = 4095;
        }
        let original = samples.clone();
        assert!(censor_frame(&mut samples, &geometry(rows, columns), 0).is_err());
        assert_eq!(samples, original);
    }

    #[test]
    fn boxes_that_a_merged_box_reaches_are_merged_too() {
        let region = |top, left, bottom, right| Region {
            lines: Vec::new(),
            top,
            left,
            bottom,
            right,
        };
        // A overlaps B; their union reaches C, which overlaps neither.
        let boxes = vec![
            region(0, 0, 10, 100),
            region(8, 0, 30, 10),
            region(20, 90, 40, 120),
        ];
        let mut merged = merge_overlapping(boxes);
        assert_eq!(merged.len(), 2);
        merged = merge_overlapping(merged);
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn stacked_lone_structures_in_tissue_are_left_in_place() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![1500_u16; rows * columns];
        for row in 0..rows {
            samples[row * columns] = 0;
        }
        // Three disks 12 pixels apart: three one-glyph lines whose boxes overlap.
        for center in [14_isize, 26, 38] {
            for row in 0..rows {
                for column in 0..columns {
                    let (dy, dx) = (row as isize - center, column as isize - 60);
                    if dy * dy + dx * dx <= 25 {
                        samples[row * columns + column] = 4095;
                    }
                }
            }
        }
        let original = samples.clone();
        let (regions, pixels, left) =
            censor_frame_counted(&mut samples, &geometry(rows, columns), 0).unwrap();
        assert_eq!((regions, pixels), (0, 0));
        assert!(left > 0);
        assert_eq!(samples, original);
    }

    #[test]
    fn a_text_line_over_tissue_fails_closed() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![1500_u16; rows * columns];
        draw_block_text(&mut samples, columns, 20, 20, 5, 4095);
        assert!(matches!(
            censor_frame(&mut samples, &geometry(rows, columns), 3),
            Err(Error::BurnedInTextOverTissue { frame: 3 })
        ));
    }

    #[test]
    fn a_lone_bright_spot_in_tissue_is_left_alone() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![1500_u16; rows * columns];
        draw_block_text(&mut samples, columns, 20, 20, 1, 4095);
        let original = samples.clone();
        assert_eq!(
            censor_frame(&mut samples, &geometry(rows, columns), 0).unwrap(),
            (0, 0)
        );
        assert_eq!(samples, original);
    }

    #[test]
    fn a_frame_without_text_is_unchanged() {
        let (rows, columns) = (64, 128);
        let mut samples: Vec<u16> = (0..rows * columns)
            .map(|index| (index % 300) as u16)
            .collect();
        let original = samples.clone();
        assert_eq!(
            censor_frame(&mut samples, &geometry(rows, columns), 0).unwrap(),
            (0, 0)
        );
        assert_eq!(samples, original);
    }

    #[test]
    fn monochrome1_text_is_dark_and_is_censored_to_the_background() {
        let (rows, columns) = (64, 128);
        let mut samples = vec![4095_u16; rows * columns];
        draw_block_text(&mut samples, columns, 4, 4, 4, 0);
        let mut inverted = geometry(rows, columns);
        inverted.inverted = true;
        let (regions, _) = censor_frame(&mut samples, &inverted, 0).unwrap();
        assert_eq!(regions, 1);
        assert!(samples.iter().all(|&value| value == 4095));
    }
}
