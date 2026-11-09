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
use dicom_dictionary_std::tags;
use dicom_object::mem::{InMemDicomObject, InMemElement};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Explicit VR Little Endian, written when censoring changes a compressed object.
pub(crate) const EXPLICIT_VR_LITTLE_ENDIAN: &str = "1.2.840.10008.1.2.1";
const NATIVE_TRANSFER_SYNTAXES: [&str; 2] = ["1.2.840.10008.1.2", EXPLICIT_VR_LITTLE_ENDIAN];
const JPEG_2000_LOSSLESS: [&str; 2] = ["1.2.840.10008.1.2.4.90", "1.2.840.10008.1.2.4.91"];
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
/// Share of the dynamic range above the background that still counts as background.
const BACKGROUND_LEVEL: f64 = 0.05;
/// Most samples that one object may decode to, so that a header cannot demand an
/// unbounded allocation: 2^30 samples, or 2 GiB at 16 bits.
const MAX_DECODED_SAMPLES: usize = 1 << 30;

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

fn not_assessed(reason: &str) -> Error {
    Error::PixelTextNotAssessed(reason.to_owned())
}

fn geometry(object: &InMemDicomObject) -> Result<Geometry> {
    let samples = number(object, tags::SAMPLES_PER_PIXEL).unwrap_or(1);
    let photometric = text(object, tags::PHOTOMETRIC_INTERPRETATION).unwrap_or_default();
    if samples != 1 || !matches!(photometric.as_str(), "MONOCHROME1" | "MONOCHROME2") {
        return Err(not_assessed(
            "only single-sample MONOCHROME1 and MONOCHROME2 images are supported",
        ));
    }
    if number(object, tags::PIXEL_REPRESENTATION).unwrap_or(0) != 0 {
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
    let bits_stored = number(object, tags::BITS_STORED)
        .and_then(|value| u16::try_from(value).ok())
        .filter(|&value| value > 0 && value <= bits_allocated)
        .unwrap_or(bits_allocated);
    Ok(Geometry {
        rows,
        columns,
        frames,
        bits_allocated,
        bits_stored,
        inverted: photometric == "MONOCHROME1",
    })
}

/// Decode every frame into one sample buffer per frame.
fn decode_frames(
    object: &InMemDicomObject,
    transfer_syntax: &str,
    geometry: &Geometry,
) -> Result<Vec<Vec<u16>>> {
    let element = object
        .get(tags::PIXEL_DATA)
        .ok_or_else(|| not_assessed("Pixel Data is missing"))?;
    let frame_samples = geometry
        .rows
        .checked_mul(geometry.columns)
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
        return Ok(bytes[..needed]
            .chunks(frame_samples * width)
            .map(|frame| {
                if width == 1 {
                    frame.iter().map(|&value| u16::from(value)).collect()
                } else {
                    frame
                        .chunks_exact(2)
                        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                        .collect()
                }
            })
            .collect());
    }
    let Value::PixelSequence(sequence) = element.value() else {
        return Err(not_assessed("this transfer syntax is not supported"));
    };
    let fragments = sequence.fragments();
    let codestreams: Vec<Vec<u8>> = if geometry.frames == 1 {
        vec![fragments.concat()]
    } else if fragments.len() == geometry.frames {
        fragments.to_vec()
    } else {
        return Err(not_assessed(
            "multi-frame Pixel Data needs one fragment per frame",
        ));
    };
    let (rows, columns) = (geometry.rows as u32, geometry.columns as u32);
    codestreams
        .iter()
        .map(|codestream| {
            // An odd-length codestream is padded with one zero byte after its end marker.
            let codestream = if codestream.ends_with(&[0xFF, 0xD9, 0x00]) {
                &codestream[..codestream.len() - 1]
            } else {
                &codestream[..]
            };
            let samples = if JPEG_2000_LOSSLESS.contains(&transfer_syntax) {
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
        })
        .collect()
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
    fn is_glyph(&self, rows: usize) -> bool {
        let (height, width) = (self.height(), self.width());
        let fill = self.area as f64 / (height * width) as f64;
        (MIN_GLYPH_HEIGHT..=MAX_GLYPH_HEIGHT.min(rows / 4).max(MIN_GLYPH_HEIGHT)).contains(&height)
            && width <= 3 * height
            && self.area >= 6
            && (0.08..=0.9).contains(&fill)
    }
}

/// 8-connected components of the pixels at or above `level`.
/// 8-connected components of the pixels at or above `level`, and a label for each pixel:
/// the component's id plus one, or zero for a pixel below `level`.
fn components(
    samples: &[u16],
    rows: usize,
    columns: usize,
    level: u16,
) -> (Vec<Component>, Vec<u32>) {
    let mut labels = vec![0_u32; samples.len()];
    let mut seen = vec![false; samples.len()];
    let mut found = Vec::new();
    let mut stack = Vec::new();
    for start in 0..samples.len() {
        if seen[start] || samples[start] < level {
            continue;
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
    (found, labels)
}

/// Group glyphs into text lines: similar height, aligned centers, small gaps.
fn text_lines(glyphs: &[Component]) -> Vec<Vec<Component>> {
    let mut order: Vec<usize> = (0..glyphs.len()).collect();
    order.sort_by_key(|&index| (glyphs[index].left, glyphs[index].top));
    let mut lines: Vec<Vec<Component>> = Vec::new();
    for index in order {
        let glyph = glyphs[index];
        let joined = lines.iter_mut().find(|line| {
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
            None => lines.push(vec![glyph]),
        }
    }
    lines
        .into_iter()
        .filter(|line| line.len() >= 2 || line[0].height() >= MIN_LONE_GLYPH_HEIGHT)
        .collect()
}

/// Censor the text in one frame, in place. Returns the regions and pixels censored.
fn censor_frame(samples: &mut [u16], geometry: &Geometry, frame: usize) -> Result<(u64, u64)> {
    let (rows, columns) = (geometry.rows, geometry.columns);
    // Work in a view where text is bright and the background is dark.
    let max_value = if geometry.bits_stored >= 16 {
        u16::MAX
    } else {
        (1_u16 << geometry.bits_stored) - 1
    };
    let view: Vec<u16> = if geometry.inverted {
        samples
            .iter()
            .map(|&value| max_value - value.min(max_value))
            .collect()
    } else {
        samples.to_vec()
    };
    let (Some(&low), Some(&high)) = (view.iter().min(), view.iter().max()) else {
        return Ok((0, 0));
    };
    let range = f64::from(high - low);
    if range == 0.0 {
        return Ok((0, 0));
    }
    let text_level = low + (TEXT_LEVEL * range).ceil() as u16;
    // The background is the dark end of the frame's own range, which does not depend on
    // Bits Stored. A frame whose darkest value is not near black has no background, so
    // every candidate there is over tissue.
    let background_level = if f64::from(low) <= BACKGROUND_LEVEL * f64::from(high) {
        low + (BACKGROUND_LEVEL * range) as u16
    } else {
        0
    };
    let (found, labels) = components(&view, rows, columns, text_level);
    let glyphs: Vec<Component> = found
        .into_iter()
        .filter(|component| component.is_glyph(rows))
        .collect();

    let mut regions = 0;
    let mut pixels = 0;
    for line in text_lines(&glyphs) {
        let height = line.iter().map(Component::height).max().unwrap_or(0);
        let margin = (height / 4).max(2);
        let top = line
            .iter()
            .map(|g| g.top)
            .min()
            .unwrap_or(0)
            .saturating_sub(margin);
        let left = line
            .iter()
            .map(|g| g.left)
            .min()
            .unwrap_or(0)
            .saturating_sub(margin);
        let bottom = (line.iter().map(|g| g.bottom).max().unwrap_or(0) + margin).min(rows - 1);
        let right = (line.iter().map(|g| g.right).max().unwrap_or(0) + margin).min(columns - 1);

        // Only background and this line's own glyph pixels may be written. Anything else,
        // such as tissue, a bright structure that is not a glyph, or a glyph's anti-aliased
        // edge, which cannot be told apart from tissue next to it, stops the write.
        let line_labels: Vec<u32> = line.iter().map(|glyph| glyph.id + 1).collect();
        let mut other = 0_usize;
        for row in top..=bottom {
            for column in left..=right {
                let index = row * columns + column;
                if view[index] > background_level && !line_labels.contains(&labels[index]) {
                    other += 1;
                }
            }
        }
        let area = (bottom - top + 1) * (right - left + 1);
        if other > 0 {
            if line.len() >= 3 {
                // A line of text over tissue cannot be censored without censoring tissue.
                return Err(Error::BurnedInTextOverTissue { frame });
            }
            // One or two glyphs, such as a calcification or a marker on the skin line.
            continue;
        }
        let fill = if geometry.inverted {
            max_value - low
        } else {
            low
        };
        for row in top..=bottom {
            samples[row * columns + left..=row * columns + right].fill(fill);
        }
        regions += 1;
        pixels += area as u64;
    }
    Ok((regions, pixels))
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
    let mut frames = decode_frames(object, transfer_syntax, &geometry)?;
    // A sample above Bits Stored is content that the detector would not see.
    let max_value = if geometry.bits_stored >= 16 {
        u16::MAX
    } else {
        (1_u16 << geometry.bits_stored) - 1
    };
    if frames.iter().flatten().any(|&value| value > max_value) {
        return Err(not_assessed("a sample uses bits above Bits Stored"));
    }
    // `frames` is consumed below to build the output without another copy.
    let mut counts = CensorCounts::default();
    for (index, frame) in frames.iter_mut().enumerate() {
        let (regions, pixels) = censor_frame(frame, &geometry, index)?;
        counts.frames_assessed += 1;
        counts.regions_censored += regions;
        counts.pixels_censored += pixels;
    }
    if counts.regions_censored == 0 {
        return Ok(Censored {
            counts,
            pixel_data: None,
        });
    }
    // Consume the frames, so the output holds no second copy of the samples.
    let mut samples: Vec<u16> = Vec::with_capacity(frames.iter().map(Vec::len).sum());
    for frame in frames {
        samples.extend(frame);
    }
    let pixel_data = if geometry.bits_allocated == 8 {
        let bytes: Vec<u8> = samples.into_iter().map(|value| value as u8).collect();
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
