//! JPEG 2000 tier-2 packet walk for the burned-in text check (#53).
//!
//! OpenJPEG, even in strict mode, decodes a tile whose data ends before all of its
//! packets and reconstructs the missing detail as zero. This walk parses every packet
//! header of every tile, as ITU-T T.800 Annex B and OpenJPEG's `t2.c` do, and skips each
//! packet body by the lengths in its header. A tile is complete only when every packet
//! that its layers, resolutions, components and precincts imply is present, in the order
//! of its progression (with POC changes), within the tile data. Packed packet headers
//! (PPM, PPT), SOP and EPH markers, subsampled components and every code-block style are
//! supported, so every ITU-T T.803 class 1 codestream passes.

use std::collections::BTreeMap;

/// Most code-blocks in one tile, so that a crafted header cannot force a large
/// allocation. A 2^26-sample frame with 8 x 8 code-blocks has 2^20; smaller code-blocks
/// on so large a frame fail closed.
const MAX_CODE_BLOCKS: usize = 1 << 20;
/// Most progression order changes in one tile, as in OpenJPEG (`J2K_MAX_POCS`).
const MAX_CHANGES: usize = 32;

/// Most packets in one tile. Each packet has a header of at least one byte. The walk keeps
/// one byte per packet to tell which it has seen.
const MAX_PACKETS: u64 = 1 << 24;
/// Most precincts in one tile. Each precinct keeps a small state.
const MAX_PRECINCTS: u64 = 1 << 20;

type Walk<T> = Result<T, &'static str>;

fn read_u16(data: &[u8], at: usize) -> Walk<u16> {
    data.get(at..at + 2)
        .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
        .ok_or("a marker segment is truncated")
}

fn read_u32(data: &[u8], at: usize) -> Walk<u32> {
    data.get(at..at + 4)
        .map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .ok_or("a marker segment is truncated")
}

fn ceil_div(value: u64, divisor: u64) -> u64 {
    value.div_ceil(divisor)
}

fn ceil_div_pow2(value: u64, shift: u32) -> u64 {
    if shift >= 64 {
        u64::from(value > 0)
    } else {
        value.div_ceil(1 << shift)
    }
}

fn floor_div_pow2(value: u64, shift: u32) -> u64 {
    if shift >= 64 {
        0
    } else {
        value >> shift
    }
}

/// Coding style of one component.
#[derive(Clone)]
struct ComponentStyle {
    levels: u32,
    block_width: u32,
    block_height: u32,
    block_style: u8,
    /// Precinct size exponents (width, height) per resolution.
    precincts: Vec<(u32, u32)>,
}

impl ComponentStyle {
    /// SPcod or SPcoc, after Scod/Scoc's precinct flag is known.
    fn parse(data: &[u8], user_precincts: bool) -> Walk<Self> {
        let [levels, width, height, style, _transform, ..] = *data else {
            return Err("a coding style segment is truncated");
        };
        if levels > 32 || width > 8 || height > 8 || width + height > 8 {
            return Err("a coding style segment is invalid");
        }
        let precincts = if user_precincts {
            let sizes = data
                .get(5..5 + usize::from(levels) + 1)
                .ok_or("a coding style segment is truncated")?;
            sizes
                .iter()
                .map(|&size| (u32::from(size & 0x0F), u32::from(size >> 4)))
                .collect()
        } else {
            vec![(15, 15); usize::from(levels) + 1]
        };
        Ok(Self {
            levels: u32::from(levels),
            block_width: u32::from(width) + 2,
            block_height: u32::from(height) + 2,
            block_style: style,
            precincts,
        })
    }
}

/// One progression order change.
#[derive(Clone, Copy)]
struct Change {
    resolution_start: u32,
    component_start: u32,
    layer_end: u32,
    resolution_end: u32,
    component_end: u32,
    order: u8,
}

fn parse_poc(segment: &[u8], components: u32) -> Walk<Vec<Change>> {
    // RSpoc (1), CSpoc (1 or 2), LYEpoc (2), REpoc (1), CEpoc (1 or 2), Ppoc (1).
    let wide = components > 256;
    let size = if wide { 9 } else { 7 };
    if segment.is_empty() || !segment.len().is_multiple_of(size) {
        return Err("a POC segment is malformed");
    }
    let changes: Vec<Change> = segment
        .chunks_exact(size)
        .map(|entry| {
            let (component_start, rest) = if wide {
                (
                    u32::from(u16::from_be_bytes([entry[1], entry[2]])),
                    &entry[3..],
                )
            } else {
                (u32::from(entry[1]), &entry[2..])
            };
            let layer_end = u32::from(u16::from_be_bytes([rest[0], rest[1]]));
            let resolution_end = u32::from(rest[2]);
            let (component_end, order) = if wide {
                (u32::from(u16::from_be_bytes([rest[3], rest[4]])), rest[5])
            } else {
                // T.800 reads CEpoc 0 as 256, but the pinned OpenJPEG reads it as 0. The
                // value is rejected below, so that the walk and the decoder agree.
                (u32::from(rest[3]), rest[4])
            };
            Change {
                resolution_start: u32::from(entry[0]),
                component_start,
                layer_end,
                resolution_end,
                component_end,
                order,
            }
        })
        .collect();
    if changes.iter().any(|change| change.component_end == 0) {
        return Err("a JPEG 2000 POC entry has CEpoc 0, which decoders read differently");
    }
    Ok(changes)
}

/// The coding parameters of the main header, or of a tile after its header.
#[derive(Clone)]
struct Coding {
    order: u8,
    layers: u32,
    sop: bool,
    eph: bool,
    default: Option<ComponentStyle>,
    components: BTreeMap<u32, ComponentStyle>,
    changes: Vec<Change>,
    /// Whether `changes` came from this tile's own POC, which replaces the main header's.
    tile_changes: bool,
}

impl Coding {
    fn style(&self, component: u32) -> Walk<&ComponentStyle> {
        self.components
            .get(&component)
            .or(self.default.as_ref())
            .ok_or("a codestream has no COD segment")
    }

    /// Apply a COD, COC or POC segment.
    fn apply(&mut self, marker: u8, segment: &[u8], components: u32, tile: bool) -> Walk<()> {
        match marker {
            0x52 => {
                let [scod, order, layers_high, layers_low, _mct, ..] = *segment else {
                    return Err("a COD segment is truncated");
                };
                self.order = order;
                self.layers = u32::from(u16::from_be_bytes([layers_high, layers_low]));
                self.sop = scod & 0x02 != 0;
                self.eph = scod & 0x04 != 0;
                // A COD resets the defaults; a later COC still overrides it.
                self.default = Some(ComponentStyle::parse(&segment[5..], scod & 0x01 != 0)?);
                self.components.clear();
            }
            0x53 => {
                let (component, rest) = if components > 256 {
                    (u32::from(read_u16(segment, 0)?), segment.get(2..))
                } else {
                    (
                        u32::from(*segment.first().ok_or("a COC segment is truncated")?),
                        segment.get(1..),
                    )
                };
                let rest = rest.ok_or("a COC segment is truncated")?;
                let scoc = *rest.first().ok_or("a COC segment is truncated")?;
                if component >= components {
                    return Err("a COC segment names a missing component");
                }
                self.components.insert(
                    component,
                    ComponentStyle::parse(&rest[1..], scoc & 0x01 != 0)?,
                );
            }
            0x5F => {
                // T.800 says a tile's POC replaces the main header's; the pinned OpenJPEG
                // appends it. The two orders can differ, so a codestream with both is not
                // assessed.
                if tile && !self.tile_changes && !self.changes.is_empty() {
                    return Err("a JPEG 2000 codestream has POC segments in both the main and a tile header");
                }
                if tile {
                    self.tile_changes = true;
                }
                self.changes.extend(parse_poc(segment, components)?);
                if self.changes.len() > MAX_CHANGES {
                    return Err("a JPEG 2000 codestream has too many progression order changes");
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// A bit reader with JPEG 2000 bit stuffing: after a 0xFF byte, the next byte holds only
/// seven bits. Reading past the end is an error, unlike in OpenJPEG.
struct Bits<'a> {
    data: &'a [u8],
    position: usize,
    buffer: u32,
    count: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            buffer: 0,
            count: 0,
        }
    }

    fn byte_in(&mut self) -> Walk<()> {
        self.buffer = (self.buffer << 8) & 0xFFFF;
        self.count = if self.buffer == 0xFF00 { 7 } else { 8 };
        let byte = *self
            .data
            .get(self.position)
            .ok_or("a JPEG 2000 tile is missing packets")?;
        self.buffer |= u32::from(byte);
        self.position += 1;
        Ok(())
    }

    fn bit(&mut self) -> Walk<u32> {
        if self.count == 0 {
            self.byte_in()?;
        }
        self.count -= 1;
        Ok((self.buffer >> self.count) & 1)
    }

    fn bits(&mut self, count: u32) -> Walk<u32> {
        if count > 32 {
            return Err("a packet header length field is too long");
        }
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | self.bit()?;
        }
        Ok(value)
    }

    /// Byte alignment at the end of a packet header, with its stuffing byte.
    fn align(&mut self) -> Walk<usize> {
        if self.buffer & 0xFF == 0xFF {
            self.byte_in()?;
        }
        self.count = 0;
        Ok(self.position)
    }
}

/// A tag tree (T.800 B.10.2).
struct TagTree {
    parent: Vec<usize>,
    value: Vec<u32>,
    low: Vec<u32>,
}

impl TagTree {
    fn new(width: usize, height: usize) -> Self {
        let mut parent = Vec::new();
        let (mut level_width, mut level_height, mut start) = (width, height, 0);
        loop {
            let next_start = start + level_width * level_height;
            let (next_width, next_height) = (level_width.div_ceil(2), level_height.div_ceil(2));
            let root = level_width * level_height <= 1;
            for row in 0..level_height {
                for column in 0..level_width {
                    parent.push(if root {
                        usize::MAX
                    } else {
                        next_start + (row / 2) * next_width + column / 2
                    });
                }
            }
            if root {
                break;
            }
            (level_width, level_height, start) = (next_width, next_height, next_start);
        }
        let nodes = parent.len();
        Self {
            parent,
            value: vec![u32::MAX; nodes],
            low: vec![0; nodes],
        }
    }

    /// Whether the value of `leaf` is below `threshold`, reading bits as needed.
    fn decode(&mut self, bits: &mut Bits<'_>, leaf: usize, threshold: u32) -> Walk<bool> {
        let mut path = vec![leaf];
        let mut node = leaf;
        while self.parent[node] != usize::MAX {
            node = self.parent[node];
            path.push(node);
        }
        let mut low = 0;
        for &node in path.iter().rev() {
            if low > self.low[node] {
                self.low[node] = low;
            } else {
                low = self.low[node];
            }
            while low < threshold && low < self.value[node] {
                if bits.bit()? == 1 {
                    self.value[node] = low;
                } else {
                    low += 1;
                }
            }
            self.low[node] = low;
        }
        Ok(self.value[leaf] < threshold)
    }
}

/// The coding-pass segments of one code-block (OpenJPEG `opj_t2_init_seg`).
struct CodeBlock {
    included: bool,
    length_bits: u32,
    /// (passes so far, most passes) of each segment.
    segments: Vec<(u32, u32)>,
}

fn new_segment(block: &CodeBlock, style: u8, first: bool) -> (u32, u32) {
    let most = if style & 0x04 != 0 {
        1
    } else if style & 0x01 != 0 {
        if first {
            10
        } else {
            match block.segments.last() {
                Some(&(_, 1)) | Some(&(_, 10)) => 2,
                _ => 1,
            }
        }
    } else {
        109
    };
    (0, most)
}

struct BandPrecinct {
    inclusion: TagTree,
    zero_planes: TagTree,
    blocks: Vec<CodeBlock>,
}

/// A precinct of one resolution of one component.
struct Precinct {
    bands: Vec<BandPrecinct>,
}

struct Resolution {
    /// Precinct size exponents, and the precinct grid.
    precinct_width_exponent: u32,
    precinct_height_exponent: u32,
    precincts_wide: u64,
    precincts_high: u64,
    /// Resolution coordinates (x0, y0, x1, y1).
    bounds: (u64, u64, u64, u64),
}

/// The geometry of one tile-component; its precinct states are kept apart, so that the
/// packet order can read the geometry while the parser updates the states.
struct TileComponent {
    step_x: u64,
    step_y: u64,
    resolutions: Vec<Resolution>,
    style: u8,
}

/// The precinct states of one tile-component, by resolution.
type PrecinctStates = Vec<Vec<Precinct>>;

/// The number of precincts of one tile-component, by the arithmetic of
/// `tile_component`, without allocating anything.
fn precinct_count(style: &ComponentStyle, tile: (u64, u64, u64, u64), step: (u64, u64)) -> u64 {
    let (x0, y0, x1, y1) = (
        ceil_div(tile.0, step.0),
        ceil_div(tile.1, step.1),
        ceil_div(tile.2, step.0),
        ceil_div(tile.3, step.1),
    );
    (0..=style.levels)
        .map(|resolution| {
            let level = style.levels - resolution;
            let (rx0, ry0, rx1, ry1) = (
                ceil_div_pow2(x0, level),
                ceil_div_pow2(y0, level),
                ceil_div_pow2(x1, level),
                ceil_div_pow2(y1, level),
            );
            let (pdx, pdy) = style.precincts[resolution as usize];
            let wide = if rx0 == rx1 {
                0
            } else {
                ((ceil_div_pow2(rx1, pdx) << pdx) - (floor_div_pow2(rx0, pdx) << pdx)) >> pdx
            };
            let high = if ry0 == ry1 {
                0
            } else {
                ((ceil_div_pow2(ry1, pdy) << pdy) - (floor_div_pow2(ry0, pdy) << pdy)) >> pdy
            };
            wide.saturating_mul(high)
        })
        .fold(0_u64, u64::saturating_add)
}

/// Build the precinct and code-block structure of one tile-component (OpenJPEG
/// `opj_tcd_init_tile`).
fn tile_component(
    style: &ComponentStyle,
    tile: (u64, u64, u64, u64),
    step: (u64, u64),
    blocks: &mut usize,
) -> Walk<(TileComponent, PrecinctStates)> {
    let (x0, y0, x1, y1) = (
        ceil_div(tile.0, step.0),
        ceil_div(tile.1, step.1),
        ceil_div(tile.2, step.0),
        ceil_div(tile.3, step.1),
    );
    let mut resolutions = Vec::new();
    let mut states = Vec::new();
    for resolution in 0..=style.levels {
        let level = style.levels - resolution;
        let bounds = (
            ceil_div_pow2(x0, level),
            ceil_div_pow2(y0, level),
            ceil_div_pow2(x1, level),
            ceil_div_pow2(y1, level),
        );
        let (pdx, pdy) = style.precincts[resolution as usize];
        let start_x = floor_div_pow2(bounds.0, pdx) << pdx;
        let start_y = floor_div_pow2(bounds.1, pdy) << pdy;
        let end_x = ceil_div_pow2(bounds.2, pdx) << pdx;
        let end_y = ceil_div_pow2(bounds.3, pdy) << pdy;
        let wide = if bounds.0 == bounds.2 {
            0
        } else {
            (end_x - start_x) >> pdx
        };
        let high = if bounds.1 == bounds.3 {
            0
        } else {
            (end_y - start_y) >> pdy
        };
        let (group_x, group_y, group_width, group_height, bands) = if resolution == 0 {
            (start_x, start_y, pdx, pdy, 1)
        } else {
            (
                ceil_div_pow2(start_x, 1),
                ceil_div_pow2(start_y, 1),
                pdx.saturating_sub(1),
                pdy.saturating_sub(1),
                3,
            )
        };
        let block_width = style.block_width.min(group_width);
        let block_height = style.block_height.min(group_height);
        let count = wide
            .checked_mul(high)
            .ok_or("a JPEG 2000 tile has too many precincts")?;
        if count > MAX_CODE_BLOCKS as u64 {
            return Err("a JPEG 2000 tile has too many precincts");
        }
        let mut precincts = Vec::with_capacity(count as usize);
        for number in 0..count {
            let mut band_list = Vec::with_capacity(bands);
            for band in 0..bands {
                let band_bounds = if resolution == 0 {
                    bounds
                } else {
                    let band_number = band as u64 + 1;
                    let (offset_x, offset_y) = (band_number & 1, band_number >> 1);
                    let shift = |value: u64, offset: u64| -> u64 {
                        ceil_div_pow2(value.saturating_sub(offset << level), level + 1)
                    };
                    (
                        shift(x0, offset_x),
                        shift(y0, offset_y),
                        shift(x1, offset_x),
                        shift(y1, offset_y),
                    )
                };
                let group_start_x = group_x + (number % wide) * (1 << group_width);
                let group_start_y = group_y + (number / wide) * (1 << group_height);
                let px0 = group_start_x.max(band_bounds.0);
                let py0 = group_start_y.max(band_bounds.1);
                let px1 = (group_start_x + (1 << group_width)).min(band_bounds.2);
                let py1 = (group_start_y + (1 << group_height)).min(band_bounds.3);
                let (columns, rows) = if px1 <= px0 || py1 <= py0 {
                    (0, 0)
                } else {
                    (
                        ((ceil_div_pow2(px1, block_width) << block_width)
                            - (floor_div_pow2(px0, block_width) << block_width))
                            >> block_width,
                        ((ceil_div_pow2(py1, block_height) << block_height)
                            - (floor_div_pow2(py0, block_height) << block_height))
                            >> block_height,
                    )
                };
                let (columns, rows) = (columns as usize, rows as usize);
                *blocks += columns * rows;
                if *blocks > MAX_CODE_BLOCKS {
                    return Err("a JPEG 2000 tile has too many code-blocks");
                }
                // A band without code-blocks adds nothing to a packet header.
                if columns * rows == 0 {
                    continue;
                }
                band_list.push(BandPrecinct {
                    inclusion: TagTree::new(columns, rows),
                    zero_planes: TagTree::new(columns, rows),
                    blocks: (0..columns * rows)
                        .map(|_| CodeBlock {
                            included: false,
                            length_bits: 3,
                            segments: Vec::new(),
                        })
                        .collect(),
                });
            }
            precincts.push(Precinct { bands: band_list });
        }
        resolutions.push(Resolution {
            precinct_width_exponent: pdx,
            precinct_height_exponent: pdy,
            precincts_wide: wide,
            precincts_high: high,
            bounds,
        });
        states.push(precincts);
    }
    Ok((
        TileComponent {
            step_x: step.0,
            step_y: step.1,
            resolutions,
            style: style.block_style,
        },
        states,
    ))
}

/// Read the number of new coding passes (T.800 Table B.4).
fn passes(bits: &mut Bits<'_>) -> Walk<u32> {
    if bits.bit()? == 0 {
        return Ok(1);
    }
    if bits.bit()? == 0 {
        return Ok(2);
    }
    let value = bits.bits(2)?;
    if value != 3 {
        return Ok(3 + value);
    }
    let value = bits.bits(5)?;
    if value != 31 {
        return Ok(6 + value);
    }
    Ok(37 + bits.bits(7)?)
}

/// Read one packet header from `header` and return its header length and the length of
/// its body.
fn packet(
    header: &[u8],
    layer: u32,
    precinct: &mut Precinct,
    style: u8,
    eph: bool,
) -> Walk<(usize, u64)> {
    let mut bits = Bits::new(header);
    let mut body = 0_u64;
    if bits.bit()? == 1 {
        for band in &mut precinct.bands {
            for index in 0..band.blocks.len() {
                let block = &mut band.blocks[index];
                let included = if block.included {
                    bits.bit()? == 1
                } else {
                    band.inclusion.decode(&mut bits, index, layer + 1)?
                };
                if !included {
                    continue;
                }
                if !block.included {
                    // Zero bit-planes: the depth below a bound, for a crafted header.
                    let mut planes = 0;
                    while !band.zero_planes.decode(&mut bits, index, planes)? {
                        planes += 1;
                        if planes > 255 {
                            return Err("a packet header has too many zero bit-planes");
                        }
                    }
                    block.included = true;
                }
                let mut remaining = passes(&mut bits)?;
                while bits.bit()? == 1 {
                    block.length_bits += 1;
                    if block.length_bits > 32 {
                        return Err("a packet header length field is too long");
                    }
                }
                if block
                    .segments
                    .last()
                    .is_none_or(|&(done, most)| done == most)
                {
                    let first = block.segments.is_empty();
                    let segment = new_segment(block, style, first);
                    block.segments.push(segment);
                }
                while remaining > 0 {
                    let last = block.segments.len() - 1;
                    let (done, most) = block.segments[last];
                    let new = (most - done).min(remaining);
                    body += u64::from(bits.bits(block.length_bits + new.ilog2())?);
                    block.segments[last].0 += new;
                    remaining -= new;
                    if remaining > 0 {
                        let segment = new_segment(block, style, false);
                        block.segments.push(segment);
                    }
                }
            }
        }
    }
    let mut length = bits.align()?;
    if eph {
        if header.get(length..length + 2) != Some(&[0xFF, 0x92]) {
            return Err("a JPEG 2000 packet header lacks its EPH marker");
        }
        length += 2;
    }
    Ok((length, body))
}

/// A tile's data: its tile-part bodies, its packed headers, and its coding.
struct Tile {
    body: Vec<u8>,
    packed: Option<Vec<u8>>,
    coding: Coding,
    coding_read: bool,
}

/// The packet order of one tile, as (layer, resolution, component, precinct), with
/// progression order changes (OpenJPEG `pi.c`).
/// Visit the packets of one tile in order, as (layer, resolution, component, precinct),
/// with progression order changes (OpenJPEG `pi.c`). Each packet is visited once, and
/// `expected` is the number that the tile implies. Returns how many were visited.
fn packet_order(
    coding: &Coding,
    components: &[TileComponent],
    tile: (u64, u64, u64, u64),
    expected: u64,
    visit: &mut dyn FnMut(u32, u32, u32, u64) -> Walk<()>,
) -> Walk<u64> {
    let component_count = components.len() as u32;
    let most_resolutions = components
        .iter()
        .map(|component| component.resolutions.len() as u32)
        .max()
        .unwrap_or(0);
    let changes = if coding.changes.is_empty() {
        vec![Change {
            resolution_start: 0,
            component_start: 0,
            layer_end: coding.layers,
            resolution_end: most_resolutions,
            component_end: component_count,
            order: coding.order,
        }]
    } else {
        coding.changes.clone()
    };
    // One byte per packet: the index of a packet is its precinct's place among all
    // precincts of the tile, times the layer count, plus its layer.
    let mut offsets = Vec::with_capacity(components.len());
    let mut total = 0_u64;
    for component in components {
        let mut starts = Vec::with_capacity(component.resolutions.len());
        for resolution in &component.resolutions {
            starts.push(total);
            total += resolution.precincts_wide * resolution.precincts_high;
        }
        offsets.push(starts);
    }
    let layer_count = u64::from(coding.layers);
    if total.checked_mul(layer_count) != Some(expected) {
        return Err("a JPEG 2000 tile's precinct grid is inconsistent");
    }
    let mut seen = vec![false; expected as usize];
    // Shared with the loops below, which stop once a packet fails or every packet that
    // the tile implies has been visited.
    let visited = std::cell::Cell::new(0_u64);
    let failure: std::cell::Cell<Option<&'static str>> = std::cell::Cell::new(None);
    let stop = || failure.get().is_some() || visited.get() >= expected;
    let mut order = ();
    let mut emit = |layer: u32, resolution: u32, component: u32, precinct: u64, _: &mut ()| {
        if failure.get().is_some() {
            return;
        }
        let index = (offsets[component as usize][resolution as usize] + precinct) * layer_count
            + u64::from(layer);
        match seen.get_mut(index as usize) {
            Some(seen) if !*seen => {
                *seen = true;
                visited.set(visited.get() + 1);
                if let Err(error) = visit(layer, resolution, component, precinct) {
                    failure.set(Some(error));
                }
            }
            Some(_) => {}
            None => failure.set(Some("a JPEG 2000 packet names a missing precinct")),
        }
    };
    for change in changes {
        if stop() {
            break;
        }
        let layers = change.layer_end.min(coding.layers);
        let resolutions = change.resolution_start..change.resolution_end.min(most_resolutions);
        let comps = change.component_start..change.component_end.min(component_count);
        let precinct_count = |component: u32, resolution: u32| -> Option<u64> {
            let resolution = components[component as usize]
                .resolutions
                .get(resolution as usize)?;
            Some(resolution.precincts_wide * resolution.precincts_high)
        };
        match change.order {
            // LRCP and RLCP iterate precinct numbers directly.
            0 | 1 => {
                let mut walk_precincts = |layer: u32, resolution: u32, order: &mut ()| {
                    for component in comps.clone() {
                        for precinct in 0..precinct_count(component, resolution).unwrap_or(0) {
                            emit(layer, resolution, component, precinct, order);
                        }
                    }
                };
                if change.order == 0 {
                    for layer in 0..layers {
                        if stop() {
                            break;
                        }
                        for resolution in resolutions.clone() {
                            walk_precincts(layer, resolution, &mut order);
                        }
                    }
                } else {
                    for resolution in resolutions.clone() {
                        if stop() {
                            break;
                        }
                        for layer in 0..layers {
                            walk_precincts(layer, resolution, &mut order);
                        }
                    }
                }
            }
            // RPCL, PCRL and CPRL iterate positions on the reference grid.
            2..=4 => {
                let (tx0, ty0, tx1, ty1) = tile;
                // The step covers every component, as in OpenJPEG.
                let (mut step_x, mut step_y) = (u64::MAX, u64::MAX);
                for tile_component in components {
                    let levels = tile_component.resolutions.len() as u32 - 1;
                    for (index, resolution) in tile_component.resolutions.iter().enumerate() {
                        let shift_x = resolution.precinct_width_exponent + levels - index as u32;
                        let shift_y = resolution.precinct_height_exponent + levels - index as u32;
                        if shift_x < 63 {
                            step_x = step_x.min(tile_component.step_x.saturating_mul(1 << shift_x));
                        }
                        if shift_y < 63 {
                            step_y = step_y.min(tile_component.step_y.saturating_mul(1 << shift_y));
                        }
                    }
                }
                let positions = |component: u32, resolution: u32, x: u64, y: u64| -> Option<u64> {
                    let tile_component = &components[component as usize];
                    let data = tile_component.resolutions.get(resolution as usize)?;
                    let level = tile_component.resolutions.len() as u32 - 1 - resolution;
                    let (rx0, ry0, rx1, ry1) = data.bounds;
                    let (rpx, rpy) = (
                        data.precinct_width_exponent + level,
                        data.precinct_height_exponent + level,
                    );
                    let sx = tile_component.step_x.checked_shl(rpx).unwrap_or(0);
                    let sy = tile_component.step_y.checked_shl(rpy).unwrap_or(0);
                    let at_row = (sy != 0 && y.is_multiple_of(sy))
                        || (y == ty0
                            && !(ry0 << level)
                                .is_multiple_of(1_u64.checked_shl(rpy).unwrap_or(0).max(1)));
                    let at_column = (sx != 0 && x.is_multiple_of(sx))
                        || (x == tx0
                            && !(rx0 << level)
                                .is_multiple_of(1_u64.checked_shl(rpx).unwrap_or(0).max(1)));
                    if !at_row
                        || !at_column
                        || data.precincts_wide == 0
                        || data.precincts_high == 0
                        || rx0 == rx1
                        || ry0 == ry1
                    {
                        return None;
                    }
                    let column = floor_div_pow2(
                        ceil_div(x, tile_component.step_x << level),
                        data.precinct_width_exponent,
                    ) - floor_div_pow2(rx0, data.precinct_width_exponent);
                    let row = floor_div_pow2(
                        ceil_div(y, tile_component.step_y << level),
                        data.precinct_height_exponent,
                    ) - floor_div_pow2(ry0, data.precinct_height_exponent);
                    Some(column + row * data.precincts_wide)
                };
                if step_x == u64::MAX || step_y == u64::MAX || step_x == 0 || step_y == 0 {
                    continue;
                }
                let ys: Vec<u64> =
                    std::iter::successors(Some(ty0), |&y| Some(y + step_y - y % step_y))
                        .take_while(|&y| y < ty1)
                        .collect();
                let xs: Vec<u64> =
                    std::iter::successors(Some(tx0), |&x| Some(x + step_x - x % step_x))
                        .take_while(|&x| x < tx1)
                        .collect();
                if (ys.len() as u64).saturating_mul(xs.len() as u64) > MAX_CODE_BLOCKS as u64 {
                    return Err("a JPEG 2000 tile has too many precinct positions");
                }
                let mut walk_position =
                    |order: &mut (), component: u32, resolution: u32, x: u64, y: u64| {
                        if let Some(precinct) = positions(component, resolution, x, y) {
                            for layer in 0..layers {
                                emit(layer, resolution, component, precinct, order);
                            }
                        }
                    };
                match change.order {
                    2 => {
                        for resolution in resolutions.clone() {
                            for &y in &ys {
                                if stop() {
                                    break;
                                }
                                for &x in &xs {
                                    for component in comps.clone() {
                                        walk_position(&mut order, component, resolution, x, y);
                                    }
                                }
                            }
                        }
                    }
                    3 => {
                        for &y in &ys {
                            if stop() {
                                break;
                            }
                            for &x in &xs {
                                for component in comps.clone() {
                                    for resolution in resolutions.clone() {
                                        walk_position(&mut order, component, resolution, x, y);
                                    }
                                }
                            }
                        }
                    }
                    _ => {
                        for component in comps.clone() {
                            for &y in &ys {
                                if stop() {
                                    break;
                                }
                                for &x in &xs {
                                    for resolution in resolutions.clone() {
                                        walk_position(&mut order, component, resolution, x, y);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => return Err("a JPEG 2000 progression order is invalid"),
        }
    }
    match failure.get() {
        Some(error) => Err(error),
        None => Ok(visited.get()),
    }
}

/// Check that every tile of `codestream` holds all of its packets.
pub(crate) fn check_packets(codestream: &[u8]) -> Walk<()> {
    packet_usage(codestream).map(|_| ())
}

/// For each tile, the bytes of tile data that its packets use and the bytes it has.
/// Equal counts show that the walk read every packet as the encoder wrote it.
pub(crate) fn packet_usage(codestream: &[u8]) -> Walk<Vec<(usize, usize)>> {
    // SIZ.
    if !codestream.starts_with(&[0xFF, 0x4F, 0xFF, 0x51]) {
        return Err("a JPEG 2000 frame is not a raw codestream");
    }
    let siz_length = usize::from(read_u16(codestream, 4)?);
    let siz = codestream
        .get(6..4 + siz_length)
        .ok_or("a SIZ segment is truncated")?;
    let field = |index: usize| -> Walk<u64> { Ok(u64::from(read_u32(siz, 2 + 4 * index)?)) };
    let (width, height, x0, y0) = (field(0)?, field(1)?, field(2)?, field(3)?);
    let (tile_width, tile_height, tile_x0, tile_y0) = (field(4)?, field(5)?, field(6)?, field(7)?);
    let components = u32::from(read_u16(siz, 34)?);
    // T.800 A.5.1: the first tile starts at or before the image and covers its origin.
    if tile_width == 0
        || tile_height == 0
        || components == 0
        || x0 >= width
        || y0 >= height
        || tile_x0 > x0
        || tile_y0 > y0
        || tile_x0.saturating_add(tile_width) <= x0
        || tile_y0.saturating_add(tile_height) <= y0
    {
        return Err("a SIZ segment is invalid");
    }
    let steps: Vec<(u64, u64)> = (0..components as usize)
        .map(|component| {
            let at = 36 + 3 * component;
            match siz.get(at + 1..at + 3) {
                Some(&[dx, dy]) if dx > 0 && dy > 0 => Ok((u64::from(dx), u64::from(dy))),
                _ => Err("a SIZ segment is invalid"),
            }
        })
        .collect::<Walk<_>>()?;
    let tiles_wide = (width - tile_x0).div_ceil(tile_width);
    let tiles_high = (height - tile_y0).div_ceil(tile_height);
    let tile_count = usize::try_from(tiles_wide * tiles_high)
        .map_err(|_| "a JPEG 2000 frame has too many tiles")?;
    if tile_count > 1 << 16 {
        return Err("a JPEG 2000 frame has too many tiles");
    }

    // Segments of the main header.
    let segment_at = |position: usize| -> Walk<(u8, &[u8], usize)> {
        let marker = codestream
            .get(position..position + 2)
            .ok_or("a JPEG 2000 header is truncated")?;
        if marker[0] != 0xFF {
            return Err("a JPEG 2000 header is malformed");
        }
        if (0x30..=0x3F).contains(&marker[1]) {
            return Ok((marker[1], &[], position + 2));
        }
        let length = usize::from(read_u16(codestream, position + 2)?);
        if length < 2 {
            return Err("a JPEG 2000 marker segment has an invalid length");
        }
        let segment = codestream
            .get(position + 4..position + 2 + length)
            .ok_or("a JPEG 2000 header is truncated")?;
        Ok((marker[1], segment, position + 2 + length))
    };
    let mut main = Coding {
        order: 0,
        layers: 0,
        sop: false,
        eph: false,
        default: None,
        components: BTreeMap::new(),
        changes: Vec::new(),
        tile_changes: false,
    };
    let mut ppm: Vec<(u8, &[u8])> = Vec::new();
    let mut position = 4 + siz_length;
    loop {
        let (marker, segment, next) = segment_at(position)?;
        if marker == 0x90 {
            break;
        }
        if marker == 0x60 {
            let (&index, data) = segment.split_first().ok_or("a PPM segment is truncated")?;
            ppm.push((index, data));
        } else {
            main.apply(marker, segment, components, false)?;
        }
        position = next;
    }
    ppm.sort_by_key(|&(index, _)| index);
    let ppm: Vec<u8> = ppm
        .into_iter()
        .flat_map(|(_, data)| data.iter().copied())
        .collect();
    let mut ppm_position = 0;
    let mut use_ppm = !ppm.is_empty();

    // Tile-parts.
    let mut tiles: Vec<Option<Tile>> = (0..tile_count).map(|_| None).collect();
    let mut ppt: Vec<Vec<(u8, Vec<u8>)>> = vec![Vec::new(); tile_count];
    loop {
        let (marker, sot, mut header) = segment_at(position)?;
        if marker != 0x90 || sot.len() != 8 {
            return Err("a JPEG 2000 tile-part is malformed");
        }
        let index = usize::from(read_u16(sot, 0)?);
        let length = read_u32(sot, 2)? as usize;
        if index >= tile_count {
            return Err("a JPEG 2000 tile-part names a missing tile");
        }
        let tile = tiles[index].get_or_insert_with(|| Tile {
            body: Vec::new(),
            packed: None,
            coding: main.clone(),
            coding_read: false,
        });
        let first = !tile.coding_read;
        loop {
            if codestream.get(header..header + 2) == Some(&[0xFF, 0x93]) {
                break;
            }
            let (marker, segment, next) = segment_at(header)?;
            match marker {
                0x61 => {
                    let (&order, data) =
                        segment.split_first().ok_or("a PPT segment is truncated")?;
                    ppt[index].push((order, data.to_vec()));
                }
                // COD and COC only in the first tile-part of a tile; POC in any.
                0x52 | 0x53 if first => tile.coding.apply(marker, segment, components, true)?,
                0x5F => tile.coding.apply(marker, segment, components, true)?,
                _ => {}
            }
            header = next;
        }
        tile.coding_read = true;
        let end = if length == 0 {
            codestream.len().saturating_sub(2).max(header + 2)
        } else {
            position
                .checked_add(length)
                .ok_or("a JPEG 2000 tile-part is malformed")?
        };
        let body = codestream
            .get(header + 2..end)
            .ok_or("a JPEG 2000 tile-part runs past the codestream")?;
        tile.body.extend_from_slice(body);
        if use_ppm {
            let size =
                read_u32(&ppm, ppm_position).map_err(|_| "a PPM segment is truncated")? as usize;
            let data = ppm
                .get(ppm_position + 4..ppm_position + 4 + size)
                .ok_or("a PPM segment is truncated")?;
            tile.packed
                .get_or_insert_with(Vec::new)
                .extend_from_slice(data);
            ppm_position += 4 + size;
            use_ppm = ppm_position < ppm.len();
        }
        if length == 0 {
            break;
        }
        position = end;
        match codestream.get(position..position + 2) {
            Some([0xFF, 0x90]) => {}
            Some([0xFF, 0xD9]) | None => break,
            _ => return Err("a JPEG 2000 tile-part is malformed"),
        }
    }

    // The packets of each tile.
    let mut usage = Vec::with_capacity(tile_count);
    for (index, tile) in tiles.into_iter().enumerate() {
        let mut tile = tile.ok_or("a JPEG 2000 frame is missing a tile")?;
        if !ppt[index].is_empty() {
            let mut parts = std::mem::take(&mut ppt[index]);
            parts.sort_by_key(|&(order, _)| order);
            tile.packed = Some(parts.into_iter().flat_map(|(_, data)| data).collect());
        }
        let column = (index as u64) % tiles_wide;
        let row = (index as u64) / tiles_wide;
        let bounds = (
            (tile_x0 + column * tile_width).max(x0),
            (tile_y0 + row * tile_height).max(y0),
            (tile_x0 + (column + 1) * tile_width).min(width),
            (tile_y0 + (row + 1) * tile_height).min(height),
        );
        // Every packet has a header of at least one byte, so a tile that implies more
        // packets than it has header bytes is incomplete. This is checked from the
        // geometry alone, before any precinct state is allocated, and it bounds that
        // state and the packet order to the size of the codestream.
        let mut precincts = 0_u64;
        for component in 0..components {
            precincts = precincts.saturating_add(precinct_count(
                tile.coding.style(component)?,
                bounds,
                steps[component as usize],
            ));
        }
        if tile.coding.layers == 0 {
            return Err("a JPEG 2000 COD segment has no quality layers");
        }
        let expected = precincts
            .checked_mul(u64::from(tile.coding.layers))
            .ok_or("a JPEG 2000 tile implies too many packets")?;
        let header_bytes = tile.packed.as_ref().map_or(tile.body.len(), Vec::len) as u64;
        if precincts > MAX_PRECINCTS || expected > MAX_PACKETS {
            return Err("a JPEG 2000 tile has too many precincts or packets");
        }
        if expected > header_bytes {
            return Err("a JPEG 2000 tile is missing packets");
        }
        let mut blocks = 0;
        let (geometry, mut states): (Vec<TileComponent>, Vec<PrecinctStates>) = (0..components)
            .map(|component| {
                tile_component(
                    tile.coding.style(component)?,
                    bounds,
                    steps[component as usize],
                    &mut blocks,
                )
            })
            .collect::<Walk<Vec<_>>>()?
            .into_iter()
            .unzip();
        // The structure must hold exactly the precincts that were counted.
        let built: u64 = states
            .iter()
            .flatten()
            .map(|resolution| resolution.len() as u64)
            .sum();
        if built != precincts {
            return Err("a JPEG 2000 tile's precinct grid is inconsistent");
        }
        let (mut body, mut header) = (0_usize, 0_usize);
        let (sop, eph) = (tile.coding.sop, tile.coding.eph);
        let tile_body = &tile.body;
        let packed = tile.packed.as_deref();
        let mut parse = |layer: u32, resolution: u32, component: u32, precinct: u64| -> Walk<()> {
            if sop && tile_body.get(body..body + 2) == Some(&[0xFF, 0x91]) {
                body += 6;
            }
            let style = geometry[component as usize].style;
            let precinct = states[component as usize][resolution as usize]
                .get_mut(precinct as usize)
                .ok_or("a JPEG 2000 packet names a missing precinct")?;
            let source: &[u8] = match packed {
                Some(packed) => packed
                    .get(header..)
                    .ok_or("a JPEG 2000 tile is missing packets")?,
                None => tile_body
                    .get(body..)
                    .ok_or("a JPEG 2000 tile is missing packets")?,
            };
            let (header_length, body_length) = packet(source, layer, precinct, style, eph)?;
            if packed.is_some() {
                header += header_length;
            } else {
                body += header_length;
            }
            body = body
                .checked_add(
                    usize::try_from(body_length).map_err(|_| "a JPEG 2000 packet is too long")?,
                )
                .ok_or("a JPEG 2000 packet is too long")?;
            if body > tile_body.len() {
                return Err("a JPEG 2000 tile is missing packets");
            }
            Ok(())
        };
        let visited = packet_order(&tile.coding, &geometry, bounds, expected, &mut parse)?;
        // POC ranges must cover every packet that the tile implies.
        if visited != expected {
            return Err("a JPEG 2000 tile is missing packets");
        }
        usage.push((body, tile.body.len()));
    }
    Ok(usage)
}
