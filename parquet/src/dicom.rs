use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use dicom_core::dictionary::{DataDictionary, DataDictionaryEntry};
use dicom_core::header::{HasLength, Header, Length};
use dicom_core::value::DataSetSequence;
use dicom_core::{DataElement, DicomValue, Tag, VR};
use dicom_dictionary_std::{tags, StandardDataDictionary};
use dicom_json::DicomJson;
use dicom_object::mem::InMemElement;
use dicom_object::{FileMetaTable, InMemDicomObject};
use dicom_parser::dataset::lazy_read::LazyDataSetReader;
use dicom_parser::dataset::LazyDataToken;
use dicom_parser::stateful::decode::StatefulDecode;
use dicom_transfer_syntax_registry::{TransferSyntaxIndex, TransferSyntaxRegistry};
use serde::Serialize;
use xxhash_rust::xxh3::{xxh3_64_with_seed, Xxh3};

use crate::error::{Error, IoContext, Result};
use crate::naming::ColumnNaming;

const HASH_SEED: u64 = 0;
const PART10_PREAMBLE_BYTES: usize = 128;
const PART10_MAGIC: &[u8; 4] = b"DICM";
const WORD_SWAP_BUFFER_BYTES: usize = 8 * 1024;
const EXPLICIT_VR_BIG_ENDIAN_UID: &str = "1.2.840.10008.1.2.2";

type ParseResult<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Options controlling extraction of one metadata row.
#[derive(Clone, Debug, Default)]
pub struct ExtractionOptions {
    pub hash_pixel_data: bool,
    pub naming: ColumnNaming,
    pub overrides: BTreeMap<Tag, String>,
}

/// I/O counters for pixel handling.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ExtractionStats {
    /// Pixel bytes streamed through XXH3. This remains zero when hashing is off.
    pub hashed_pixel_bytes: u64,
}

/// One metadata-only DICOM row.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MetadataRow {
    pub values: BTreeMap<String, String>,
    pub stats: ExtractionStats,
}

#[derive(Default)]
struct PixelHash {
    value: Option<u64>,
    bytes: u64,
}

/// Read one DICOM exactly once and return metadata suitable for a Parquet row.
///
/// Pixel Data is never retained. Native Pixel Data is skipped by seeking when
/// hashing is disabled and streamed into XXH3 when hashing is enabled.
pub fn extract_metadata(path: &Path, options: &ExtractionOptions) -> Result<MetadataRow> {
    if options.overrides.contains_key(&tags::PIXEL_DATA) {
        return Err(Error::PixelDataOverride);
    }
    extract_metadata_inner(path, options).map_err(|source| Error::Dicom {
        path: path.to_path_buf(),
        source,
    })
}

fn extract_metadata_inner(path: &Path, options: &ExtractionOptions) -> ParseResult<MetadataRow> {
    let file = File::open(path).at(path)?;
    let mut source = BufReader::new(file);
    position_at_magic(&mut source)?;
    let meta = FileMetaTable::from_reader(&mut source)?;
    let transfer_syntax = TransferSyntaxRegistry
        .get(meta.transfer_syntax())
        .ok_or_else(|| {
            invalid_data(format!(
                "unknown transfer syntax {}",
                meta.transfer_syntax()
            ))
        })?;
    let swap_native_words = native_word_bytes_need_swapping(meta.transfer_syntax());
    let mut reader = LazyDataSetReader::new_with_ts(source, transfer_syntax)?;
    let mut pixel_hash = PixelHash::default();
    let object = collect_data_set(
        &mut reader,
        false,
        true,
        options.hash_pixel_data,
        swap_native_words,
        &mut pixel_hash,
    )?;

    let mut raw_values = BTreeMap::<Tag, String>::new();
    for element in object.iter() {
        if element.tag() == tags::PIXEL_DATA || element.tag().group() % 2 == 1 {
            continue;
        }
        if StandardDataDictionary.by_tag(element.tag()).is_none() {
            continue;
        }
        if let Some(value) = element_to_string(element) {
            raw_values.insert(element.tag(), value);
        }
    }
    for element in meta.to_element_iter() {
        let tag = element.tag();
        if StandardDataDictionary.by_tag(tag).is_none() {
            continue;
        }
        if let Some(value) = element.into_value().into_primitive() {
            let value = value.to_string();
            if !value.is_empty() {
                raw_values.insert(tag, value);
            }
        }
    }
    raw_values.extend(options.overrides.clone());

    require_non_empty(path, &raw_values, tags::SOP_INSTANCE_UID, "SOPInstanceUID")?;
    require_non_empty(
        path,
        &raw_values,
        tags::STUDY_INSTANCE_UID,
        "StudyInstanceUID",
    )?;

    let mut values = apply_column_names(&raw_values, options.naming)?;
    if let Some(hash) = pixel_hash.value {
        values.insert(options.naming.apply("PixelDataHash"), hash.to_string());
    }

    Ok(MetadataRow {
        values,
        stats: ExtractionStats {
            hashed_pixel_bytes: pixel_hash.bytes,
        },
    })
}

fn position_at_magic(reader: &mut BufReader<File>) -> ParseResult<()> {
    let buffer = reader.fill_buf()?;
    let preamble = if buffer.len() >= PART10_PREAMBLE_BYTES + PART10_MAGIC.len()
        && &buffer[PART10_PREAMBLE_BYTES..PART10_PREAMBLE_BYTES + PART10_MAGIC.len()]
            == PART10_MAGIC
    {
        PART10_PREAMBLE_BYTES
    } else if buffer.len() >= PART10_MAGIC.len() && &buffer[..PART10_MAGIC.len()] == PART10_MAGIC {
        0
    } else {
        return Err(invalid_data("DICOM Part 10 magic code is missing"));
    };
    reader.consume(preamble);
    Ok(())
}

fn require_non_empty(
    path: &Path,
    values: &BTreeMap<Tag, String>,
    tag: Tag,
    field: &'static str,
) -> ParseResult<()> {
    if values
        .get(&tag)
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(());
    }
    Err(Box::new(Error::MissingRequiredField {
        path: path.to_path_buf(),
        field,
    }))
}

fn apply_column_names(
    raw_values: &BTreeMap<Tag, String>,
    naming: ColumnNaming,
) -> ParseResult<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    let mut assigned = HashMap::<String, Tag>::new();
    for (&tag, value) in raw_values {
        let Some(entry) = StandardDataDictionary.by_tag(tag) else {
            continue;
        };
        let name = naming.apply(entry.alias());
        if let Some(previous) = assigned.insert(name.clone(), tag) {
            if previous != tag {
                return Err(Box::new(Error::ColumnCollision {
                    name,
                    first: previous.to_string(),
                    second: tag.to_string(),
                }));
            }
        }
        values.insert(name, value.clone());
    }
    Ok(values)
}

fn element_to_string(element: &InMemElement<StandardDataDictionary>) -> Option<String> {
    match element.value() {
        DicomValue::Primitive(value) if !value.is_empty() => Some(value.to_string()),
        DicomValue::Sequence(sequence) if !sequence.is_empty() => {
            // Preserve dicom-structs compatibility: each DICOM JSON item is a
            // string inside the sequence's outer JSON array.
            let item_json = sequence
                .items()
                .iter()
                .filter_map(|item| serde_json::to_value(DicomJson::from(item)).ok())
                .map(|value| value.to_string())
                .collect::<Vec<_>>();
            serde_json::to_string(&item_json).ok()
        }
        DicomValue::PixelSequence(_) | DicomValue::Primitive(_) | DicomValue::Sequence(_) => None,
    }
}

fn collect_data_set<S>(
    reader: &mut LazyDataSetReader<S>,
    stop_at_item_end: bool,
    root: bool,
    hash_pixel_data: bool,
    swap_native_words: bool,
    pixel_hash: &mut PixelHash,
) -> ParseResult<InMemDicomObject>
where
    S: StatefulDecode,
{
    let mut elements = Vec::new();
    while let Some(token) = reader.advance() {
        match token? {
            LazyDataToken::ElementHeader(header) => {
                let value_token = reader
                    .advance()
                    .ok_or_else(|| invalid_data(format!("missing value for {}", header.tag)))??;
                if header.tag == tags::PIXEL_DATA {
                    handle_native_pixel_data(
                        value_token,
                        root && hash_pixel_data,
                        swap_native_words && header.vr == VR::OW,
                        pixel_hash,
                    )?;
                    continue;
                }
                if !is_output_metadata(header.tag) {
                    value_token.skip()?;
                    continue;
                }
                let value = value_token.into_value()?;
                elements.push(InMemElement::new_with_len(
                    header.tag, header.vr, header.len, value,
                ));
            }
            LazyDataToken::SequenceStart { tag, len } => {
                let items =
                    collect_sequence(reader, hash_pixel_data, swap_native_words, pixel_hash)?;
                if is_output_metadata(tag) {
                    elements.push(DataElement::new_with_len(
                        tag,
                        VR::SQ,
                        len,
                        DicomValue::from(DataSetSequence::new(items, len)),
                    ));
                }
            }
            LazyDataToken::PixelSequenceStart => {
                handle_encapsulated_pixel_data(reader, root && hash_pixel_data, pixel_hash)?;
            }
            LazyDataToken::ItemEnd if stop_at_item_end => break,
            LazyDataToken::SequenceEnd => {
                return Err(invalid_data("unexpected sequence delimiter in data set"))
            }
            LazyDataToken::ItemStart { .. } => {
                return Err(invalid_data("unexpected item start in data set"))
            }
            LazyDataToken::LazyValue { .. } | LazyDataToken::LazyItemValue { .. } => {
                return Err(invalid_data("unexpected unconsumed DICOM value"))
            }
            LazyDataToken::ItemEnd => return Err(invalid_data("unexpected item delimiter")),
            _ => return Err(invalid_data("unsupported DICOM parser token")),
        }
    }

    let mut object = InMemDicomObject::new_empty();
    object.extend(elements);
    Ok(object)
}

fn is_output_metadata(tag: Tag) -> bool {
    tag != tags::PIXEL_DATA
        && tag.group().is_multiple_of(2)
        && StandardDataDictionary.by_tag(tag).is_some()
}

fn collect_sequence<S>(
    reader: &mut LazyDataSetReader<S>,
    hash_pixel_data: bool,
    swap_native_words: bool,
    pixel_hash: &mut PixelHash,
) -> ParseResult<Vec<InMemDicomObject>>
where
    S: StatefulDecode,
{
    let mut items = Vec::new();
    while let Some(token) = reader.advance() {
        match token? {
            LazyDataToken::ItemStart { .. } => items.push(collect_data_set(
                reader,
                true,
                false,
                hash_pixel_data,
                swap_native_words,
                pixel_hash,
            )?),
            LazyDataToken::SequenceEnd => return Ok(items),
            token => {
                token.skip()?;
                return Err(invalid_data("unexpected token in DICOM sequence"));
            }
        }
    }
    Err(invalid_data("DICOM sequence ended without a delimiter"))
}

fn handle_native_pixel_data<S>(
    token: LazyDataToken<&mut S>,
    hash_pixel_data: bool,
    swap_words: bool,
    pixel_hash: &mut PixelHash,
) -> ParseResult<()>
where
    S: StatefulDecode,
{
    let LazyDataToken::LazyValue { header, .. } = &token else {
        return Err(invalid_data("native Pixel Data has no primitive value"));
    };
    let length = u64::from(
        header
            .length()
            .get()
            .ok_or_else(|| invalid_data("native Pixel Data has undefined length"))?,
    );
    if !hash_pixel_data {
        token.skip()?;
        return Ok(());
    }

    let mut hasher = Xxh3::with_seed(HASH_SEED);
    let mut writer = HashWriter::new(&mut hasher, swap_words);
    token.read_value_into(&mut writer)?;
    writer.finish()?;
    pixel_hash.value = Some(hasher.digest());
    pixel_hash.bytes = pixel_hash.bytes.saturating_add(length);
    Ok(())
}

fn handle_encapsulated_pixel_data<S>(
    reader: &mut LazyDataSetReader<S>,
    hash_pixel_data: bool,
    pixel_hash: &mut PixelHash,
) -> ParseResult<()>
where
    S: StatefulDecode,
{
    let mut item_index = 0_usize;
    let mut active_item = None;
    let mut fragments = 0_usize;
    let mut hash_sum = 0_u64;

    while let Some(token) = reader.advance() {
        match token? {
            LazyDataToken::ItemStart { len } => {
                active_item = Some(item_index);
                if item_index > 0 && len == Length(0) {
                    fragments += 1;
                    if hash_pixel_data {
                        hash_sum = hash_sum.wrapping_add(xxh3_64_with_seed(&[], HASH_SEED));
                    }
                }
                item_index += 1;
            }
            token @ LazyDataToken::LazyItemValue { len, .. } => {
                let current_item =
                    active_item.ok_or_else(|| invalid_data("pixel fragment has no item header"))?;
                if current_item == 0 || !hash_pixel_data {
                    token.skip()?;
                    continue;
                }
                let mut hasher = Xxh3::with_seed(HASH_SEED);
                let mut writer = HashWriter::new(&mut hasher, false);
                token.read_value_into(&mut writer)?;
                writer.finish()?;
                hash_sum = hash_sum.wrapping_add(hasher.digest());
                pixel_hash.bytes = pixel_hash.bytes.saturating_add(u64::from(len));
                fragments += 1;
            }
            LazyDataToken::ItemEnd => active_item = None,
            LazyDataToken::SequenceEnd => {
                if hash_pixel_data && fragments > 0 {
                    pixel_hash.value = Some(hash_sum);
                }
                return Ok(());
            }
            token => {
                token.skip()?;
                return Err(invalid_data("unexpected token in encapsulated Pixel Data"));
            }
        }
    }
    Err(invalid_data(
        "encapsulated Pixel Data ended without a delimiter",
    ))
}

struct HashWriter<'a> {
    hasher: &'a mut Xxh3,
    swap_words: bool,
    pending_byte: Option<u8>,
    swap_buffer: [u8; WORD_SWAP_BUFFER_BYTES],
}

impl<'a> HashWriter<'a> {
    fn new(hasher: &'a mut Xxh3, swap_words: bool) -> Self {
        Self {
            hasher,
            swap_words,
            pending_byte: None,
            swap_buffer: [0_u8; WORD_SWAP_BUFFER_BYTES],
        }
    }

    fn finish(&self) -> std::io::Result<()> {
        if self.pending_byte.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "word-sized Pixel Data has an odd byte length",
            ));
        }
        Ok(())
    }

    fn update_swapped(&mut self, mut bytes: &[u8]) {
        if let Some(first) = self.pending_byte.take() {
            if let Some((&second, remaining)) = bytes.split_first() {
                self.hasher.update(&[second, first]);
                bytes = remaining;
            } else {
                self.pending_byte = Some(first);
                return;
            }
        }

        let even_length = bytes.len() & !1;
        for source in bytes[..even_length].chunks(WORD_SWAP_BUFFER_BYTES) {
            let output = &mut self.swap_buffer[..source.len()];
            for (input_pair, output_pair) in source.chunks_exact(2).zip(output.chunks_exact_mut(2))
            {
                output_pair[0] = input_pair[1];
                output_pair[1] = input_pair[0];
            }
            self.hasher.update(output);
        }
        self.pending_byte = bytes.get(even_length).copied();
    }
}

impl Write for HashWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if self.swap_words {
            self.update_swapped(buffer);
        } else {
            self.hasher.update(buffer);
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn native_word_bytes_need_swapping(transfer_syntax_uid: &str) -> bool {
    let source_is_big_endian =
        transfer_syntax_uid.trim_end_matches(['\0', ' ']) == EXPLICIT_VR_BIG_ENDIAN_UID;
    source_is_big_endian != cfg!(target_endian = "big")
}

fn invalid_data(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message.into(),
    ))
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Seek, SeekFrom, Write};

    use dicom_core::Tag;
    use xxhash_rust::xxh3::{xxh3_64_with_seed, Xxh3};

    use super::{
        apply_column_names, native_word_bytes_need_swapping, position_at_magic, require_non_empty,
        HashWriter, EXPLICIT_VR_BIG_ENDIAN_UID, HASH_SEED,
    };
    use crate::naming::ColumnNaming;

    #[test]
    fn hash_writer_handles_unswapped_and_chunked_word_swapped_bytes() {
        let bytes = [1_u8, 2, 3, 4, 5, 6];
        let mut plain_hasher = Xxh3::with_seed(HASH_SEED);
        let mut plain = HashWriter::new(&mut plain_hasher, false);
        plain.write_all(&bytes).unwrap();
        plain.flush().unwrap();
        plain.finish().unwrap();
        assert_eq!(plain_hasher.digest(), xxh3_64_with_seed(&bytes, HASH_SEED));

        let mut swapped_hasher = Xxh3::with_seed(HASH_SEED);
        let mut swapped = HashWriter::new(&mut swapped_hasher, true);
        swapped.write_all(&bytes[..3]).unwrap();
        swapped.write_all(&[]).unwrap();
        swapped.write_all(&bytes[3..]).unwrap();
        swapped.finish().unwrap();
        assert_eq!(
            swapped_hasher.digest(),
            xxh3_64_with_seed(&[2, 1, 4, 3, 6, 5], HASH_SEED)
        );
    }

    #[test]
    fn hash_writer_rejects_an_unpaired_word_byte() {
        let mut hasher = Xxh3::with_seed(HASH_SEED);
        let mut writer = HashWriter::new(&mut hasher, true);
        writer.write_all(&[1]).unwrap();
        writer.write_all(&[]).unwrap();
        assert!(writer.finish().is_err());
    }

    #[test]
    fn magic_detection_accepts_zero_offset_and_rejects_missing_magic() {
        let mut valid = tempfile::tempfile().unwrap();
        valid.write_all(b"DICM").unwrap();
        valid.seek(SeekFrom::Start(0)).unwrap();
        let mut valid = BufReader::new(valid);
        position_at_magic(&mut valid).unwrap();

        let mut invalid = tempfile::tempfile().unwrap();
        invalid.write_all(b"NOPE").unwrap();
        invalid.seek(SeekFrom::Start(0)).unwrap();
        let mut invalid = BufReader::new(invalid);
        assert!(position_at_magic(&mut invalid).is_err());
    }

    #[test]
    fn required_fields_and_column_collisions_are_validated() {
        let path = std::path::Path::new("input.dcm");
        let tag = Tag(0x0008, 0x0018);
        let mut values = std::collections::BTreeMap::new();
        assert!(require_non_empty(path, &values, tag, "SOPInstanceUID").is_err());
        values.insert(tag, "1.2.3".to_owned());
        require_non_empty(path, &values, tag, "SOPInstanceUID").unwrap();

        let mut private_creators = std::collections::BTreeMap::new();
        private_creators.insert(Tag(0x0009, 0x0010), "A".to_owned());
        private_creators.insert(Tag(0x0011, 0x0010), "B".to_owned());
        assert!(apply_column_names(&private_creators, ColumnNaming::Dicom).is_err());

        let mut unknown = std::collections::BTreeMap::new();
        unknown.insert(Tag(0x7776, 0x1234), "ignored".to_owned());
        assert!(apply_column_names(&unknown, ColumnNaming::Dicom)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn native_word_swap_follows_transfer_syntax_and_host_endianness() {
        assert_eq!(
            native_word_bytes_need_swapping(EXPLICIT_VR_BIG_ENDIAN_UID),
            cfg!(target_endian = "little")
        );
        assert_eq!(
            native_word_bytes_need_swapping("1.2.840.10008.1.2.1\0"),
            cfg!(target_endian = "big")
        );
    }
}
