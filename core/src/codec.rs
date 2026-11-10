//! Decoders for encapsulated Pixel Data of the burned-in text check (#47).
//!
//! JPEG 2000 is decoded by OpenJPEG 2.5.3 (`openjpeg-sys` 1.0.12, through the safe
//! `jpeg2k` 0.10.1 wrapper) and JPEG by libjpeg-turbo 3.1.0 (`turbojpeg-sys` 1.2.0). These
//! are the codec versions that medcognetics/dicom-preprocessing uses, so the check sees the
//! samples that the model pipeline sees. Both are built from source and linked
//! statically. Each decoder returns one frame of unsigned samples at their full
//! precision, and every mismatch with the DICOM header is an error.

use crate::{Error, Result};

fn not_assessed(reason: impl Into<String>) -> Error {
    Error::PixelTextNotAssessed(reason.into())
}

/// What the header says one frame holds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FrameInfo {
    pub rows: usize,
    pub columns: usize,
    pub bits_allocated: u16,
    pub bits_stored: u16,
}

impl FrameInfo {
    fn max_value(self) -> u32 {
        (1_u32 << self.bits_stored) - 1
    }
}

/// Decode one JPEG 2000 codestream into `rows * columns` samples.
///
/// OpenJPEG runs in strict mode, so a truncated codestream is an error and not a partial
/// image. The image must have one unsigned component with the frame's size and no
/// offset, a precision of at most Bits Allocated, and every sample within Bits Stored.
/// Every tile must be present: OpenJPEG fills a missing tile with zeroes even in strict
/// mode. The result says whether the codestream uses the irreversible 9/7 wavelet, which
/// makes the samples lossy whatever the transfer syntax says.
pub(crate) fn decode_jpeg2000(codestream: &[u8], info: FrameInfo) -> Result<Jpeg2000Frame> {
    // OpenJPEG sizes its buffers from the codestream, so the header is checked first.
    let tiles = check_jpeg2000_header(codestream, info)?;
    let irreversible = check_jpeg2000_tiles(codestream, tiles)?;
    let parameters = openjpeg::DecodeParameters::new().strict(true);
    let image = openjpeg::Image::from_bytes_with(codestream, parameters)
        .map_err(|_| not_assessed("a JPEG 2000 frame cannot be decoded"))?;
    let [component] = image.components() else {
        return Err(not_assessed(
            "a JPEG 2000 frame does not have exactly one component",
        ));
    };
    if image.x_offset() != 0 || image.y_offset() != 0 {
        return Err(not_assessed("a JPEG 2000 frame has an image offset"));
    }
    if component.width() as usize != info.columns || component.height() as usize != info.rows {
        return Err(not_assessed(
            "a JPEG 2000 frame does not match Rows and Columns",
        ));
    }
    if component.is_signed() {
        return Err(not_assessed("a JPEG 2000 frame has signed samples"));
    }
    if component.precision() == 0 || component.precision() > u32::from(info.bits_allocated) {
        return Err(not_assessed(
            "a JPEG 2000 frame's precision exceeds Bits Allocated",
        ));
    }
    let data = component.data();
    if data.len() != info.rows * info.columns {
        return Err(not_assessed("a decoded JPEG 2000 frame has the wrong size"));
    }
    let samples = data
        .iter()
        .map(|&sample| {
            u32::try_from(sample)
                .ok()
                .filter(|&value| value <= info.max_value())
                .map(|value| value as u16)
                .ok_or_else(|| not_assessed("a JPEG 2000 sample lies outside Bits Stored"))
        })
        .collect::<Result<Vec<u16>>>()?;
    Ok(Jpeg2000Frame {
        samples,
        irreversible,
    })
}

/// A decoded JPEG 2000 frame.
pub(crate) struct Jpeg2000Frame {
    pub samples: Vec<u16>,
    /// A COD or COC segment selects the irreversible 9/7 wavelet.
    pub irreversible: bool,
}

/// Walk the marker segments of the main header and of every tile-part header, by their
/// declared lengths. Every tile index below `tiles` must have a tile-part, and when a
/// tile-part gives the number of tile-parts of its tile, all of them must be present.
/// Returns whether a COD or COC segment selects the irreversible 9/7 wavelet.
fn check_jpeg2000_tiles(codestream: &[u8], tiles: u64) -> Result<bool> {
    let malformed = || not_assessed("a JPEG 2000 codestream has malformed or missing tiles");
    let mut irreversible = false;
    // A COD's transform is at SPcod byte 4: after Scod (1) and SGcod (4). A COC's is
    // after Ccoc (1 or 2 bytes) and Scoc (1).
    let components = jpeg2000_siz(codestream)
        .and_then(|siz| siz.get(34..36))
        .map_or(1, |bytes| u16::from_be_bytes([bytes[0], bytes[1]]));
    let mut note_transform = |marker: u8, segment: &[u8]| -> Result<()> {
        let index = match marker {
            0x52 => 9,
            // Ccoc is two bytes when there are more than 256 components.
            0x53 if components > 256 => 7,
            0x53 => 6,
            _ => return Ok(()),
        };
        match segment.get(index) {
            Some(0) => irreversible = true,
            Some(1) => {}
            _ => {
                return Err(not_assessed(
                    "a JPEG 2000 coding style segment is malformed",
                ))
            }
        }
        Ok(())
    };
    let segment_at = |position: usize| -> Option<(u8, &[u8], usize)> {
        let marker = codestream.get(position..position + 2)?;
        if marker[0] != 0xFF {
            return None;
        }
        // FF30 to FF3F are reserved markers without a segment (ITU-T T.800 A.1.3).
        if (0x30..=0x3F).contains(&marker[1]) {
            return Some((marker[1], &[], position + 2));
        }
        let length = usize::from(u16::from_be_bytes([
            *codestream.get(position + 2)?,
            *codestream.get(position + 3)?,
        ]));
        if length < 2 {
            return None;
        }
        let segment = codestream.get(position + 4..position + 2 + length)?;
        Some((marker[1], segment, position + 2 + length))
    };
    // The main header, after SOC, up to the first SOT.
    let mut position = 2;
    loop {
        let (marker, segment, next) = segment_at(position).ok_or_else(malformed)?;
        if marker == 0x90 {
            break;
        }
        note_transform(marker, segment)?;
        position = next;
    }
    let tile_count = usize::try_from(tiles).map_err(|_| malformed())?;
    let mut parts = vec![0_u16; tile_count];
    let mut declared = vec![0_u16; tile_count];
    loop {
        let (marker, sot, mut header) = segment_at(position).ok_or_else(malformed)?;
        if marker != 0x90 || sot.len() != 8 {
            return Err(malformed());
        }
        let tile = usize::from(u16::from_be_bytes([sot[0], sot[1]]));
        let length = u32::from_be_bytes([sot[2], sot[3], sot[4], sot[5]]) as usize;
        let total = sot[7];
        if tile >= tile_count {
            return Err(malformed());
        }
        // TPsot is one byte, so a tile has at most 255 tile-parts.
        parts[tile] += 1;
        if parts[tile] > 255 {
            return Err(malformed());
        }
        if total != 0 {
            declared[tile] = u16::from(total);
        }
        // The tile-part header, up to SOD.
        while codestream.get(header..header + 2) != Some(&[0xFF, 0x93]) {
            let (marker, segment, next) = segment_at(header).ok_or_else(malformed)?;
            note_transform(marker, segment)?;
            header = next;
        }
        // Psot 0 means the tile-part runs to the end of the codestream.
        if length == 0 {
            break;
        }
        position = position.checked_add(length).ok_or_else(malformed)?;
        match codestream.get(position..position + 2) {
            Some([0xFF, 0x90]) => {}
            Some([0xFF, 0xD9]) | None => break,
            _ => return Err(malformed()),
        }
    }
    let complete = parts
        .iter()
        .zip(&declared)
        .all(|(&found, &declared)| found > 0 && (declared == 0 || found == declared));
    if !complete {
        return Err(malformed());
    }
    Ok(irreversible)
}

/// The tile count from the SIZ segment, then the tile-part walk of
/// `check_jpeg2000_tiles`, for any codestream. Used by the conformance test to show that
/// the walk accepts every conformance codestream.
#[doc(hidden)]
pub fn walk_jpeg2000_codestream(codestream: &[u8]) -> Result<bool> {
    let siz = jpeg2000_siz(codestream).ok_or_else(|| not_assessed("no SIZ segment"))?;
    let field = |index: usize| -> u64 {
        let start = 2 + 4 * index;
        siz.get(start..start + 4).map_or(0, |bytes| {
            u64::from(u32::from_be_bytes(bytes.try_into().unwrap_or_default()))
        })
    };
    let (width, height, tile_width, tile_height, tile_x0, tile_y0) =
        (field(0), field(1), field(4), field(5), field(6), field(7));
    if tile_width == 0 || tile_height == 0 {
        return Err(not_assessed("invalid tile grid"));
    }
    let tiles = width.saturating_sub(tile_x0).div_ceil(tile_width)
        * height.saturating_sub(tile_y0).div_ceil(tile_height);
    check_jpeg2000_tiles(codestream, tiles)
}

/// Most tiles in one JPEG 2000 frame. Each tile costs OpenJPEG fixed work and memory.
const MAX_JPEG2000_TILES: u64 = 4096;

/// The SIZ segment of a raw codestream. DICOM does not allow the JP2 file header in
/// Pixel Data (PS3.5 A.4.4), so a fragment must start with SOC and SIZ.
fn jpeg2000_siz(codestream: &[u8]) -> Option<&[u8]> {
    if !codestream.starts_with(&[0xFF, 0x4F, 0xFF, 0x51]) {
        return None;
    }
    let length = usize::from(u16::from_be_bytes([
        *codestream.get(4)?,
        *codestream.get(5)?,
    ]));
    codestream.get(6..4 + length)
}

/// Check the SIZ segment against the header before OpenJPEG decodes anything: the image
/// is exactly Rows by Columns at the origin, with one unsubsampled component of at most
/// Bits Allocated bits and a bounded number of tiles.
fn check_jpeg2000_header(codestream: &[u8], info: FrameInfo) -> Result<u64> {
    let siz = jpeg2000_siz(codestream)
        .ok_or_else(|| not_assessed("a JPEG 2000 frame has no readable SIZ segment"))?;
    let field = |index: usize| -> Option<u64> {
        // Rsiz (2 bytes), then eight 4-byte fields, then Csiz (2 bytes).
        let start = 2 + 4 * index;
        siz.get(start..start + 4)
            .map(|bytes| u64::from(u32::from_be_bytes(bytes.try_into().unwrap_or_default())))
    };
    let (
        Some(width),
        Some(height),
        Some(x0),
        Some(y0),
        Some(tile_width),
        Some(tile_height),
        Some(tile_x0),
        Some(tile_y0),
    ) = (
        field(0),
        field(1),
        field(2),
        field(3),
        field(4),
        field(5),
        field(6),
        field(7),
    )
    else {
        return Err(not_assessed("a JPEG 2000 SIZ segment is truncated"));
    };
    if x0 != 0 || y0 != 0 || width != info.columns as u64 || height != info.rows as u64 {
        return Err(not_assessed(
            "a JPEG 2000 frame does not match Rows and Columns",
        ));
    }
    let components = siz
        .get(34..36)
        .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]));
    if components != Some(1) || siz.len() < 39 {
        return Err(not_assessed(
            "a JPEG 2000 frame does not have exactly one component",
        ));
    }
    let (depth, x_step, y_step) = (siz[36], siz[37], siz[38]);
    if depth & 0x80 != 0
        || u16::from(depth & 0x7F) + 1 > info.bits_allocated
        || x_step != 1
        || y_step != 1
    {
        return Err(not_assessed(
            "a JPEG 2000 component is signed, too deep or subsampled",
        ));
    }
    if tile_width == 0 || tile_height == 0 || tile_x0 > x0 || tile_y0 > y0 {
        return Err(not_assessed(
            "a JPEG 2000 SIZ segment has an invalid tile grid",
        ));
    }
    let tiles = (width - tile_x0).div_ceil(tile_width) * (height - tile_y0).div_ceil(tile_height);
    if tiles > MAX_JPEG2000_TILES {
        return Err(not_assessed("a JPEG 2000 frame has too many tiles"));
    }
    Ok(tiles)
}

/// Decode one JPEG codestream (baseline, extended 12-bit or lossless up to 16-bit) into
/// `rows * columns` samples with libjpeg-turbo's accurate integer IDCT.
pub(crate) fn decode_jpeg(codestream: &[u8], info: FrameInfo) -> Result<Vec<u16>> {
    turbo::decode(codestream, info)
}

/// The libjpeg-turbo FFI. This is the only `unsafe` code in the crate: libjpeg-turbo has
/// no safe binding that decodes 12-bit DCT images. Adapted from the reader in
/// medcognetics/dicom-preprocessing (`src/codec/jpeg.rs`, Apache-2.0).
#[allow(unsafe_code)]
mod turbo {
    use std::borrow::Cow;
    use std::ffi::{c_int, CStr};

    use turbojpeg_sys as sys;

    use super::{not_assessed, FrameInfo};
    use crate::Result;

    /// TurboJPEG functions that take `size_t`. The pregenerated bindings declare it as
    /// `c_ulong`, which is 32 bits on Windows; `usize` matches `size_t` on every target.
    mod ffi {
        use std::ffi::{c_int, c_short, c_uchar, c_ushort};

        use turbojpeg_sys::tjhandle;

        #[allow(clashing_extern_declarations)]
        extern "C" {
            pub fn tj3DecompressHeader(
                handle: tjhandle,
                jpeg: *const c_uchar,
                size: usize,
            ) -> c_int;
            pub fn tj3Decompress8(
                handle: tjhandle,
                jpeg: *const c_uchar,
                size: usize,
                dst: *mut c_uchar,
                pitch: c_int,
                pixel_format: c_int,
            ) -> c_int;
            pub fn tj3Decompress12(
                handle: tjhandle,
                jpeg: *const c_uchar,
                size: usize,
                dst: *mut c_short,
                pitch: c_int,
                pixel_format: c_int,
            ) -> c_int;
            pub fn tj3Decompress16(
                handle: tjhandle,
                jpeg: *const c_uchar,
                size: usize,
                dst: *mut c_ushort,
                pitch: c_int,
                pixel_format: c_int,
            ) -> c_int;
        }
    }

    /// An owned decompressor handle.
    struct Decompressor(sys::tjhandle);

    impl Decompressor {
        fn new() -> Result<Self> {
            // SAFETY: tj3Init has no preconditions; a null result is handled below.
            let handle = unsafe { sys::tj3Init(sys::TJINIT_TJINIT_DECOMPRESS as c_int) };
            if handle.is_null() {
                return Err(not_assessed("libjpeg-turbo cannot start a decompressor"));
            }
            let decompressor = Self(handle);
            // A corrupt-data warning is an error, not a partial image.
            decompressor.set(sys::TJPARAM_TJPARAM_STOPONWARNING, 1)?;
            // Bound the work that a crafted progressive image can cause.
            decompressor.set(sys::TJPARAM_TJPARAM_SCANLIMIT, 500)?;
            Ok(decompressor)
        }

        fn set(&self, parameter: sys::TJPARAM, value: c_int) -> Result<()> {
            // SAFETY: the handle is valid for the lifetime of `self`.
            match unsafe { sys::tj3Set(self.0, parameter as c_int, value) } {
                0 => Ok(()),
                _ => Err(self.error()),
            }
        }

        fn get(&self, parameter: sys::TJPARAM) -> c_int {
            // SAFETY: the handle is valid for the lifetime of `self`.
            unsafe { sys::tj3Get(self.0, parameter as c_int) }
        }

        fn check(&self, status: c_int) -> Result<()> {
            match status {
                0 => Ok(()),
                _ => Err(self.error()),
            }
        }

        fn error(&self) -> crate::Error {
            // SAFETY: tj3GetErrorStr returns a NUL-terminated string that the handle owns. It
            // is copied before any further call on the handle.
            let message = unsafe {
                let message = sys::tj3GetErrorStr(self.0);
                if message.is_null() {
                    "unknown error".to_owned()
                } else {
                    CStr::from_ptr(message).to_string_lossy().into_owned()
                }
            };
            not_assessed(format!("a JPEG frame cannot be decoded: {message}"))
        }
    }

    impl Drop for Decompressor {
        fn drop(&mut self) {
            // SAFETY: the handle came from tj3Init and is destroyed exactly once.
            unsafe { sys::tj3Destroy(self.0) }
        }
    }

    pub(super) fn decode(codestream: &[u8], info: FrameInfo) -> Result<Vec<u16>> {
        let codestream = normalize_sequential_scans(codestream);
        let codestream = &*codestream;
        let decompressor = Decompressor::new()?;
        // SAFETY: `codestream` is valid for `codestream.len()` bytes during the call.
        decompressor.check(unsafe {
            ffi::tj3DecompressHeader(decompressor.0, codestream.as_ptr(), codestream.len())
        })?;
        let width = decompressor.get(sys::TJPARAM_TJPARAM_JPEGWIDTH);
        let height = decompressor.get(sys::TJPARAM_TJPARAM_JPEGHEIGHT);
        if usize::try_from(width).ok() != Some(info.columns)
            || usize::try_from(height).ok() != Some(info.rows)
        {
            return Err(not_assessed("a JPEG frame does not match Rows and Columns"));
        }
        if decompressor.get(sys::TJPARAM_TJPARAM_COLORSPACE) as sys::TJCS != sys::TJCS_TJCS_GRAY {
            return Err(not_assessed("a JPEG frame is not grayscale"));
        }
        let samples = info.rows * info.columns;
        let gray = sys::TJPF_TJPF_GRAY as c_int;
        let precision = decompressor.get(sys::TJPARAM_TJPARAM_PRECISION);
        if precision > c_int::from(info.bits_allocated) {
            return Err(not_assessed(
                "a JPEG frame's precision exceeds Bits Allocated",
            ));
        }
        let decoded: Vec<u32> = match precision {
            2..=8 => {
                let mut buffer = vec![0_u8; samples];
                // SAFETY: `buffer` holds width * height samples, the size that TurboJPEG writes
                // with pitch 0 for a grayscale pixel format.
                decompressor.check(unsafe {
                    ffi::tj3Decompress8(
                        decompressor.0,
                        codestream.as_ptr(),
                        codestream.len(),
                        buffer.as_mut_ptr(),
                        0,
                        gray,
                    )
                })?;
                buffer.into_iter().map(u32::from).collect()
            }
            9..=12 => {
                let mut buffer = vec![0_i16; samples];
                // SAFETY: as above, with 16-bit sample storage.
                decompressor.check(unsafe {
                    ffi::tj3Decompress12(
                        decompressor.0,
                        codestream.as_ptr(),
                        codestream.len(),
                        buffer.as_mut_ptr(),
                        0,
                        gray,
                    )
                })?;
                // A negative sample is checked below, not reinterpreted.
                buffer
                    .into_iter()
                    .map(|sample| u32::try_from(sample).unwrap_or(u32::MAX))
                    .collect()
            }
            13..=16 => {
                let mut buffer = vec![0_u16; samples];
                // SAFETY: as above, with 16-bit sample storage.
                decompressor.check(unsafe {
                    ffi::tj3Decompress16(
                        decompressor.0,
                        codestream.as_ptr(),
                        codestream.len(),
                        buffer.as_mut_ptr(),
                        0,
                        gray,
                    )
                })?;
                buffer.into_iter().map(u32::from).collect()
            }
            _ => return Err(not_assessed("a JPEG frame has an unsupported precision")),
        };
        decoded
            .into_iter()
            .map(|value| {
                (value <= info.max_value())
                    .then_some(value as u16)
                    .ok_or_else(|| not_assessed("a JPEG sample lies outside Bits Stored"))
            })
            .collect()
    }

    /// Set the spectral selection and successive approximation fields of sequential DCT
    /// scans (SOF0, SOF1) to the values that T.81 requires (Ss 0, Se 63, Ah/Al 0).
    ///
    /// Some encoders write zeroes there. libjpeg-turbo ignores the fields for sequential
    /// scans but warns, and a warning is fatal here because the others mean corrupt data.
    /// Normalizing them keeps every other warning fatal and does not change the samples.
    pub(super) fn normalize_sequential_scans(data: &[u8]) -> Cow<'_, [u8]> {
        let mut data = Cow::Borrowed(data);
        let mut sequential = false;
        // Walk segments by their declared lengths, so bytes inside a payload are never read
        // as markers. A malformed stream is left to libjpeg-turbo.
        if !data.starts_with(&[0xFF, 0xD8]) {
            return data;
        }
        let mut position = 2;
        loop {
            // Fill bytes before a marker (T.81 B.1.1.2).
            while data.get(position) == Some(&0xFF) && data.get(position + 1) == Some(&0xFF) {
                position += 1;
            }
            let (Some(&0xFF), Some(&marker)) = (data.get(position), data.get(position + 1)) else {
                return data;
            };
            position += 2;
            match marker {
                0x01 | 0xD0..=0xD8 => continue,
                0xD9 => return data,
                _ => {}
            }
            let Some(length) = data
                .get(position..position + 2)
                .map(|bytes| usize::from(u16::from_be_bytes([bytes[0], bytes[1]])))
                .filter(|&length| length >= 2 && position + length <= data.len())
            else {
                return data;
            };
            let segment = position + 2..position + length;
            position += length;
            match marker {
                // SOFn; DHT, JPG and DAC share the range. Only sequential DCT frames are
                // normalized: progressive and lossless scans use these fields.
                0xC0..=0xCF if ![0xC4, 0xC8, 0xCC].contains(&marker) => {
                    if sequential || !matches!(marker, 0xC0 | 0xC1) {
                        return data;
                    }
                    sequential = true;
                }
                0xDA => {
                    if !sequential || segment.is_empty() {
                        return data;
                    }
                    // Ns, Ns selector pairs, then Ss, Se and Ah/Al.
                    let components = usize::from(data[segment.start]);
                    let fields = segment.start + 1 + 2 * components;
                    if fields + 3 != segment.end {
                        return data;
                    }
                    if data[fields..fields + 3] != [0, 63, 0] {
                        data.to_mut()[fields..fields + 3].copy_from_slice(&[0, 63, 0]);
                    }
                    // Skip the entropy-coded data to the next marker.
                    loop {
                        match (data.get(position), data.get(position + 1)) {
                            (Some(&0xFF), Some(&next))
                                if next != 0x00 && !(0xD0..=0xD7).contains(&next) =>
                            {
                                break
                            }
                            (Some(_), Some(_)) => position += 1,
                            _ => return data,
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(rows: usize, columns: usize, bits_stored: u16) -> FrameInfo {
        FrameInfo {
            rows,
            columns,
            bits_allocated: 16,
            bits_stored,
        }
    }

    /// A 16-bit lossless (SOF3, predictor 1) stream of `values` with a restart interval of
    /// `restart` samples (0 for none) and point transform `transform`. Prediction follows
    /// ITU-T T.81 H.1.2.1. Adapted from medcognetics/dicom-preprocessing (Apache-2.0).
    fn lossless_stream(values: &[u16], width: usize, restart: usize, transform: u8) -> Vec<u8> {
        fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
            let mut bytes = vec![0xFF, marker];
            bytes.extend_from_slice(&(payload.len() as u16 + 2).to_be_bytes());
            bytes.extend_from_slice(payload);
            bytes
        }
        let coded: Vec<u16> = values.iter().map(|&value| value >> transform).collect();
        let height = values.len() / width;
        let mut data = vec![0xFF, 0xD8];
        // DC table: 5-bit codes for categories 0-16.
        let mut dht = vec![0x00, 0, 0, 0, 0, 17];
        dht.extend([0; 11]);
        dht.extend(0..17_u8);
        data.extend(segment(0xC4, &dht));
        let mut sof = vec![16];
        sof.extend((height as u16).to_be_bytes());
        sof.extend((width as u16).to_be_bytes());
        sof.extend([1, 1, 0x11, 0]);
        data.extend(segment(0xC3, &sof));
        if restart > 0 {
            data.extend(segment(0xDD, &(restart as u16).to_be_bytes()));
        }
        data.extend(segment(0xDA, &[1, 1, 0, 1, 0, transform]));
        let interval = if restart > 0 { restart } else { coded.len() };
        let first = 1_i32 << (16 - 1 - u32::from(transform));
        for (number, start) in (0..coded.len()).step_by(interval).enumerate() {
            let end = (start + interval).min(coded.len());
            let mut bits = String::new();
            for i in start..end {
                let prediction = if i == start {
                    first
                } else if i / width == start / width || i % width != 0 {
                    i32::from(coded[i - 1])
                } else {
                    i32::from(coded[i - width])
                };
                let difference = i32::from(coded[i]) - prediction;
                let size = 32 - difference.unsigned_abs().leading_zeros();
                bits += &format!("{size:05b}");
                if size > 0 {
                    let magnitude = if difference >= 0 {
                        difference
                    } else {
                        difference + (1 << size) - 1
                    };
                    bits += &format!("{magnitude:0width$b}", width = size as usize);
                }
            }
            while !bits.len().is_multiple_of(8) {
                bits.push('1');
            }
            for byte in bits.as_bytes().chunks(8) {
                let byte = u8::from_str_radix(std::str::from_utf8(byte).unwrap(), 2).unwrap();
                data.push(byte);
                if byte == 0xFF {
                    data.push(0);
                }
            }
            if end < coded.len() {
                data.extend([0xFF, 0xD0 + (number % 8) as u8]);
            }
        }
        data.extend([0xFF, 0xD9]);
        data
    }

    const LOSSLESS: [u16; 12] = [
        32768, 32770, 32774, 32776, 32772, 32770, 32768, 32778, 32780, 32774, 32776, 32782,
    ];

    #[test]
    fn lossless_jpeg_with_and_without_restarts_is_bit_exact() {
        // The pure-Rust decoder of the first release got the restart case wrong.
        for restart in [0, 4] {
            let stream = lossless_stream(&LOSSLESS, 4, restart, 0);
            assert_eq!(
                decode_jpeg(&stream, info(3, 4, 16)).unwrap(),
                LOSSLESS,
                "restart {restart}"
            );
        }
    }

    #[test]
    fn lossless_jpeg_with_a_point_transform_is_bit_exact() {
        // Every value is even, so a point transform of 1 loses nothing.
        let stream = lossless_stream(&LOSSLESS, 4, 4, 1);
        assert_eq!(decode_jpeg(&stream, info(3, 4, 16)).unwrap(), LOSSLESS);
    }

    #[test]
    fn a_lossless_restart_inside_a_row_is_an_error() {
        // T.81 requires whole MCU rows; libjpeg-turbo rejects others instead of guessing.
        let stream = lossless_stream(&LOSSLESS, 4, 3, 0);
        assert!(decode_jpeg(&stream, info(3, 4, 16)).is_err());
    }

    #[test]
    fn jpeg_header_mismatches_and_truncation_are_errors() {
        let stream = lossless_stream(&LOSSLESS, 4, 4, 0);
        assert!(decode_jpeg(&stream, info(4, 3, 16)).is_err());
        assert!(decode_jpeg(
            &stream,
            FrameInfo {
                bits_allocated: 8,
                ..info(3, 4, 16)
            }
        )
        .is_err());
        // Samples above Bits Stored are not passed on.
        assert!(decode_jpeg(&stream, info(3, 4, 12)).is_err());
        assert!(decode_jpeg(&stream[..stream.len() / 2], info(3, 4, 16)).is_err());
    }

    #[test]
    fn extended_12_bit_jpeg_matches_the_libjpeg_turbo_references() {
        let directory =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jpeg12");
        let mut checked = 0;
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|extension| extension != "jpg") {
                continue;
            }
            let reference = image::open(path.with_extension("png"))
                .unwrap()
                .into_luma16();
            let (width, height) = reference.dimensions();
            let decoded = decode_jpeg(
                &std::fs::read(&path).unwrap(),
                info(height as usize, width as usize, 12),
            )
            .unwrap_or_else(|error| panic!("{path:?}: {error}"));
            assert_eq!(decoded, reference.into_raw(), "{path:?}");
            checked += 1;
        }
        assert_eq!(checked, 6);
    }

    #[test]
    fn mutated_jpeg_streams_never_panic_and_stay_within_bits_stored() {
        let directory =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jpeg12");
        let mut streams = vec![(lossless_stream(&LOSSLESS, 4, 4, 0), info(3, 4, 16))];
        for name in [
            "sequential_37x23_q90.jpg",
            "progressive_37x23_q90.jpg",
            "sequential_37x23_restart1.jpg",
        ] {
            streams.push((
                std::fs::read(directory.join(name)).unwrap(),
                info(23, 37, 12),
            ));
        }
        let mut state = 0x9E37_79B9_u32;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as usize
        };
        for round in 0..3000 {
            let (stream, frame) = &streams[round % streams.len()];
            let mut mutated = stream.clone();
            match round % 3 {
                0 => {
                    let index = next() % mutated.len();
                    mutated[index] ^= 1 << (next() % 8);
                }
                1 => mutated.truncate(next() % mutated.len()),
                _ => {
                    let start = next() % mutated.len();
                    for byte in mutated.iter_mut().skip(start).take(next() % 16) {
                        *byte = next() as u8;
                    }
                }
            }
            if let Ok(samples) = decode_jpeg(&mutated, *frame) {
                assert_eq!(samples.len(), frame.rows * frame.columns);
                assert!(samples
                    .iter()
                    .all(|&value| u32::from(value) <= frame.max_value()));
            }
        }
    }

    /// A minimal codestream skeleton: SOC, a zeroed SIZ, a COD with `transform`, and one
    /// tile-part per entry of `tiles` (tile index, number of tile-parts), then EOC.
    fn skeleton(transform: u8, tiles: &[(u16, u8)]) -> Vec<u8> {
        let mut data = vec![0xFF, 0x4F, 0xFF, 0x51, 0x00, 41];
        data.extend([0; 39]);
        data.extend([0xFF, 0x52, 0x00, 12, 0, 0, 0, 1, 0, 3, 4, 4, 0, transform]);
        for &(tile, parts) in tiles {
            data.extend([0xFF, 0x90, 0x00, 10]);
            data.extend(tile.to_be_bytes());
            data.extend(16_u32.to_be_bytes());
            data.extend([0, parts, 0xFF, 0x93, 0x12, 0x34]);
        }
        data.extend([0xFF, 0xD9]);
        data
    }

    #[test]
    fn the_wavelet_transform_and_every_tile_are_read_from_the_codestream() {
        assert!(check_jpeg2000_tiles(&skeleton(0, &[(0, 1)]), 1).unwrap());
        assert!(!check_jpeg2000_tiles(&skeleton(1, &[(0, 1), (1, 1)]), 2).unwrap());
        // A missing tile, a tile index out of range, and a missing declared tile-part.
        assert!(check_jpeg2000_tiles(&skeleton(1, &[(0, 1)]), 2).is_err());
        assert!(check_jpeg2000_tiles(&skeleton(1, &[(0, 1), (2, 1)]), 2).is_err());
        assert!(check_jpeg2000_tiles(&skeleton(1, &[(0, 2)]), 1).is_err());
        assert!(check_jpeg2000_tiles(&skeleton(1, &[(0, 2), (0, 2)]), 1).is_ok());
        // A reserved marker without a segment in the main header is skipped.
        let mut reserved = skeleton(1, &[(0, 1)]);
        reserved.splice(59..59, [0xFF, 0x30]);
        assert!(!check_jpeg2000_tiles(&reserved, 1).unwrap());
        // More than 255 tile-parts for one tile.
        let many: Vec<(u16, u8)> = vec![(0, 0); 256];
        assert!(check_jpeg2000_tiles(&skeleton(1, &many), 1).is_err());
        assert!(check_jpeg2000_tiles(&skeleton(1, &many[..255]), 1).is_ok());
        // An unknown transform value.
        assert!(check_jpeg2000_tiles(&skeleton(7, &[(0, 1)]), 1).is_err());
    }

    #[test]
    fn only_a_raw_codestream_is_read() {
        // A JP2 file, a JP2 box with a zero extended length, and a SIZ that runs past the
        // data are all rejected before OpenJPEG runs.
        for data in [
            [&[0, 0, 0, 12][..], b"jP  ", &[0x0D, 0x0A, 0x87, 0x0A]].concat(),
            [&[0, 0, 0, 1][..], b"free", &[0; 8]].concat(),
            vec![0xFF, 0x4F, 0xFF, 0x51, 0x00, 0x40, 0, 0],
        ] {
            assert!(jpeg2000_siz(&data).is_none(), "{data:?}");
            assert!(decode_jpeg2000(&data, info(4, 4, 12)).is_err());
        }
    }

    #[test]
    fn corrupt_and_empty_codestreams_are_errors() {
        for data in [
            &b""[..],
            b"\xFF\xD8",
            b"not a codestream",
            b"\xFF\x4F\xFF\x51\x00",
        ] {
            assert!(decode_jpeg(data, info(4, 4, 12)).is_err());
            assert!(decode_jpeg2000(data, info(4, 4, 12)).is_err());
        }
    }

    #[test]
    fn sequential_scan_fields_are_normalized_and_others_are_left() {
        // SOI, SOF0 (length 11, one component), SOS with Ss=0, Se=0, AhAl=0, data, EOI.
        let stream = [
            0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x11,
            0x00, 0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x12, 0xFF, 0xD9,
        ];
        let normalized = turbo::normalize_sequential_scans(&stream);
        assert_eq!(&normalized[22..25], &[0, 63, 0]);
        let mut progressive = stream;
        progressive[3] = 0xC2;
        assert_eq!(
            &*turbo::normalize_sequential_scans(&progressive),
            &progressive[..]
        );
    }
}
