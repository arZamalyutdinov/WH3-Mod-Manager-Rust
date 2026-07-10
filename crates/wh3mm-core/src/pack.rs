//! Pack-file index parsing.
//!
//! This module intentionally starts with metadata/index parsing only. DB table,
//! loc, compression, and writer behavior should be layered on top after fixture
//! parity tests exist.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::domain::GameId;
use crate::ports::{CoreError, CoreResult};

const FIXED_HEADER_LEN: usize = 28;
const FILE_SIZE_FIELD_LEN: usize = 4;
const COMPRESSION_FLAG_LEN: usize = 1;
const COMPRESSED_PREFIX_LEN: usize = 4;
const GUID_MARKER: [u8; 4] = [0xfd, 0xfe, 0xfc, 0xff];
const VERSION_MARKER: [u8; 4] = [0xfc, 0xfd, 0xfe, 0xff];
const LOC_BOM: [u8; 2] = [0xff, 0xfe];
const LOC_MAGIC: [u8; 3] = [0x4c, 0x4f, 0x43];

/// Options for reading a pack index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackReadOptions {
    /// Game format to parse.
    pub game: GameId,
}

impl Default for PackReadOptions {
    fn default() -> Self {
        Self {
            game: GameId::Warhammer3,
        }
    }
}

/// Parsed pack metadata and file index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackIndex {
    /// Source pack path.
    pub path: PathBuf,
    /// Four-byte pack magic such as `PFH5`.
    pub magic: [u8; 4],
    /// Raw byte mask from the pack header.
    pub byte_mask: i32,
    /// Whether this is a movie pack according to current WH3MM behavior.
    pub is_movie: bool,
    /// Number of reference files recorded by the header.
    pub reference_file_count: i32,
    /// Header dependency-name section size in bytes.
    pub dependency_index_size: u32,
    /// Packed-file index section size in bytes.
    pub packed_file_index_size: u32,
    /// Header buffer marker.
    pub header_buffer: [u8; 4],
    /// Dependency pack names.
    pub dependency_packs: Vec<String>,
    /// Indexed packed files in on-disk order.
    pub files: Vec<PackFileIndexEntry>,
}

/// Parsed pack index plus lightweight metadata for each indexed file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackContents {
    /// Parsed pack index.
    pub index: PackIndex,
    /// Per-file metadata in index order.
    pub metadata: Vec<PackFileMetadata>,
}

/// One uncompressed packed file to write into a generated PFH5 pack.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackFileWrite {
    /// Packed file path inside the generated pack.
    pub name: String,
    /// Complete uncompressed packed-file payload.
    pub payload: Vec<u8>,
}

/// One packed-file index entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackFileIndexEntry {
    /// Packed file path inside the pack.
    pub name: String,
    /// File payload size in bytes.
    pub file_size: u64,
    /// Absolute byte offset where file payload starts.
    pub start_pos: u64,
    /// Whether the packed file payload is compressed.
    pub is_compressed: bool,
}

impl PackFileIndexEntry {
    /// Classifies this packed file by its internal path.
    #[must_use]
    pub fn kind(&self) -> PackFileKind {
        classify_packed_file_name(&self.name)
    }
}

/// Toolkit- and parser-neutral packed-file classification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackFileKind {
    /// Creative Assembly DB table.
    DbTable {
        /// DB table folder name, matching current TS `getDBName` behavior.
        db_name: String,
        /// Table sub-file name, matching current TS `getDBSubname` behavior.
        db_subname: String,
    },
    /// Localisation file.
    Loc,
    /// Lua script file.
    Script,
    /// XML-like metadata file.
    XmlLike,
    /// Any other packed file.
    Other,
}

/// Lightweight metadata for a DB table payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DbTableMetadata {
    /// Packed file path inside the pack.
    pub name: String,
    /// DB table folder name.
    pub db_name: String,
    /// DB table sub-file name.
    pub db_subname: String,
    /// Optional GUID marker payload.
    pub guid: Option<String>,
    /// Optional DB version marker value.
    pub version: Option<i32>,
    /// Row count recorded after the table marker byte.
    pub entry_count: u32,
}

/// Lightweight metadata for a loc payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocFileMetadata {
    /// Packed file path inside the pack.
    pub name: String,
    /// Loc format version. WH3MM writes and expects `1`.
    pub version: i32,
    /// Number of loc rows.
    pub entry_count: u32,
}

/// Metadata read result for one packed file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackFileMetadata {
    /// DB table metadata.
    DbTable(DbTableMetadata),
    /// Loc file metadata.
    Loc(LocFileMetadata),
    /// File is not a metadata-bearing type yet.
    Other {
        /// Packed file path inside the pack.
        name: String,
        /// File kind.
        kind: PackFileKind,
    },
    /// Metadata exists in principle, but could not be decoded for display.
    Unsupported {
        /// Packed file path inside the pack.
        name: String,
        /// File kind.
        kind: PackFileKind,
        /// Human-readable reason.
        reason: String,
    },
}

/// Reads the fixed pack header, dependencies, and packed-file index.
///
/// # Errors
///
/// Returns [`CoreError`] when the file cannot be read, is too short, uses an
/// unexpected magic value, or has a malformed dependency/index section.
pub fn read_pack_index(path: impl AsRef<Path>, options: &PackReadOptions) -> CoreResult<PackIndex> {
    let path = path.as_ref();
    let bytes = fs::read(path)?;

    if bytes.len() < FIXED_HEADER_LEN {
        return Err(CoreError::parse(format!(
            "pack is too short for fixed header: {} bytes",
            bytes.len()
        )));
    }

    let magic = read_magic(&bytes);
    if &magic != options.game.pack_magic() {
        return Err(CoreError::parse(format!(
            "unexpected pack magic: {}",
            String::from_utf8_lossy(&magic)
        )));
    }

    let byte_mask = read_i32(&bytes, 4)?;
    let reference_file_count = read_i32(&bytes, 8)?;
    let dependency_index_size = read_non_negative_size(&bytes, 12, "dependency index size")?;
    let file_count = read_non_negative_size(&bytes, 16, "file count")?;
    let packed_file_index_size = read_non_negative_size(&bytes, 20, "packed file index size")?;
    let header_buffer = read_header_buffer(&bytes);

    let dependency_start = FIXED_HEADER_LEN;
    let dependency_index_len = usize::try_from(dependency_index_size)
        .map_err(|_| CoreError::parse("dependency index size does not fit this platform"))?;
    let packed_file_index_len = usize::try_from(packed_file_index_size)
        .map_err(|_| CoreError::parse("packed file index size does not fit this platform"))?;
    let file_count = usize::try_from(file_count)
        .map_err(|_| CoreError::parse("file count does not fit this platform"))?;

    let dependency_end = checked_section_end(
        dependency_start,
        dependency_index_len,
        bytes.len(),
        "dependency index",
    )?;
    let file_index_end = checked_section_end(
        dependency_end,
        packed_file_index_len,
        bytes.len(),
        "packed file index",
    )?;

    let dependency_packs = read_nul_terminated_strings(&bytes[dependency_start..dependency_end])?;
    let file_index = &bytes[dependency_end..file_index_end];
    let data_start = file_index_end as u64;
    let files = read_file_index(
        file_index,
        file_count,
        data_start,
        options.game.supports_pack_compression(),
    )?;
    validate_payload_bounds(&files, bytes.len())?;

    Ok(PackIndex {
        path: path.to_path_buf(),
        magic,
        byte_mask,
        is_movie: byte_mask == 4,
        reference_file_count,
        dependency_index_size,
        packed_file_index_size,
        header_buffer,
        dependency_packs,
        files,
    })
}

/// Reads a pack index and lightweight metadata for every indexed file.
///
/// # Errors
///
/// Returns [`CoreError`] when the index cannot be read or any metadata-bearing
/// file is malformed.
pub fn read_pack_contents(
    path: impl AsRef<Path>,
    options: &PackReadOptions,
) -> CoreResult<PackContents> {
    let path = path.as_ref();
    let index = read_pack_index(path, options)?;
    let metadata = index
        .files
        .iter()
        .map(|entry| read_packed_file_metadata(path, entry))
        .collect::<CoreResult<Vec<_>>>()?;

    Ok(PackContents { index, metadata })
}

/// Reads a pack index and best-effort lightweight metadata for display.
///
/// Unlike [`read_pack_contents`], this keeps the index visible when one packed
/// file's metadata is malformed. Per-file metadata failures are represented as
/// [`PackFileMetadata::Unsupported`] rows.
///
/// # Errors
///
/// Returns [`CoreError`] only when the pack index itself cannot be read.
pub fn read_pack_contents_lossy(
    path: impl AsRef<Path>,
    options: &PackReadOptions,
) -> CoreResult<PackContents> {
    let path = path.as_ref();
    let index = read_pack_index(path, options)?;
    let metadata = index
        .files
        .iter()
        .map(|entry| {
            read_packed_file_metadata(path, entry).unwrap_or_else(|error| {
                PackFileMetadata::Unsupported {
                    name: entry.name.clone(),
                    kind: entry.kind(),
                    reason: error.message,
                }
            })
        })
        .collect();

    Ok(PackContents { index, metadata })
}

/// Builds a minimal uncompressed WH3 PFH5 pack from supplied packed files.
///
/// The generated shape matches the lightweight generated packs used by this
/// Current app: byte mask `3`, no dependency index, no reference files, and an
/// uncompressed packed-file index.
///
/// # Errors
///
/// Returns [`CoreError`] when file sizes/counts cannot fit the WH3 pack index
/// integer fields.
pub fn build_pfh5_pack_bytes(files: &[PackFileWrite]) -> CoreResult<Vec<u8>> {
    let mut file_index = Vec::new();
    let mut payloads = Vec::new();

    for file in files {
        let file_size = i32::try_from(file.payload.len()).map_err(|_| {
            CoreError::invalid_input(format!("generated pack file is too large: {}", file.name))
        })?;
        file_index.extend_from_slice(&file_size.to_le_bytes());
        file_index.push(0);
        file_index.extend_from_slice(file.name.as_bytes());
        file_index.push(0);
        payloads.extend_from_slice(&file.payload);
    }

    let file_count = i32::try_from(files.len())
        .map_err(|_| CoreError::invalid_input("too many generated pack files"))?;
    let file_index_size = i32::try_from(file_index.len())
        .map_err(|_| CoreError::invalid_input("generated pack index is too large"))?;

    let mut bytes = Vec::with_capacity(FIXED_HEADER_LEN + file_index.len() + payloads.len());
    bytes.extend_from_slice(b"PFH5");
    bytes.extend_from_slice(&3_i32.to_le_bytes());
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    bytes.extend_from_slice(&file_count.to_le_bytes());
    bytes.extend_from_slice(&file_index_size.to_le_bytes());
    bytes.extend_from_slice(&0x7fff_ffff_i32.to_le_bytes());
    bytes.extend_from_slice(&file_index);
    bytes.extend_from_slice(&payloads);
    Ok(bytes)
}

/// Reads lightweight metadata for one packed file entry.
///
/// # Errors
///
/// Returns [`CoreError`] when the file cannot be read, decompression fails, or
/// metadata-bearing payload is malformed.
pub fn read_packed_file_metadata(
    pack_path: impl AsRef<Path>,
    entry: &PackFileIndexEntry,
) -> CoreResult<PackFileMetadata> {
    let kind = entry.kind();

    match kind {
        PackFileKind::DbTable {
            ref db_name,
            ref db_subname,
        } => {
            let payload = read_packed_file_payload(pack_path, entry)?;
            read_db_table_metadata(entry, db_name, db_subname, &payload)
                .map(PackFileMetadata::DbTable)
        }
        PackFileKind::Loc => {
            let payload = read_packed_file_payload(pack_path, entry)?;
            read_loc_file_metadata(entry, &payload).map(PackFileMetadata::Loc)
        }
        PackFileKind::Script | PackFileKind::XmlLike | PackFileKind::Other => {
            Ok(PackFileMetadata::Other {
                name: entry.name.clone(),
                kind,
            })
        }
    }
}

/// Reads and decompresses one packed-file payload when needed.
///
/// # Errors
///
/// Returns [`CoreError`] when the payload cannot be read, compressed payload is
/// missing the four-byte WH3 prefix, or zstd decompression fails.
pub fn read_packed_file_payload(
    pack_path: impl AsRef<Path>,
    entry: &PackFileIndexEntry,
) -> CoreResult<Vec<u8>> {
    let payload = read_packed_file_prefix(pack_path, entry, u64::MAX)?;
    if !entry.is_compressed {
        return Ok(payload);
    }

    if payload.len() < COMPRESSED_PREFIX_LEN {
        return Err(CoreError::parse(format!(
            "compressed payload is missing four-byte prefix: {}",
            entry.name
        )));
    }

    zstd::stream::decode_all(&payload[COMPRESSED_PREFIX_LEN..]).map_err(|error| {
        CoreError::parse(format!(
            "zstd decompression failed for {}: {error}",
            entry.name
        ))
    })
}

fn classify_packed_file_name(name: &str) -> PackFileKind {
    if let Some((db_name, db_subname)) = parse_db_name_parts(name) {
        return PackFileKind::DbTable {
            db_name,
            db_subname,
        };
    }

    if has_ascii_suffix(name, ".loc") {
        return PackFileKind::Loc;
    }

    if has_ascii_suffix(name, ".lua") {
        return PackFileKind::Script;
    }

    let xml_like_extensions = [
        ".xml",
        ".variantmeshdefinition",
        ".wsmodel",
        ".xml.material",
    ];
    if xml_like_extensions
        .iter()
        .any(|extension| has_ascii_suffix(name, extension))
    {
        return PackFileKind::XmlLike;
    }

    PackFileKind::Other
}

fn has_ascii_suffix(value: &str, suffix: &str) -> bool {
    value
        .get(value.len().saturating_sub(suffix.len())..)
        .is_some_and(|ending| ending.eq_ignore_ascii_case(suffix))
}

fn parse_db_name_parts(name: &str) -> Option<(String, String)> {
    let after_prefix = name.strip_prefix("db\\")?;
    let (db_name, db_subname) = after_prefix.split_once('\\')?;
    if db_name.is_empty() || db_subname.is_empty() {
        return None;
    }

    Some((db_name.to_string(), db_subname.to_string()))
}

fn read_packed_file_prefix(
    pack_path: impl AsRef<Path>,
    entry: &PackFileIndexEntry,
    max_len: u64,
) -> CoreResult<Vec<u8>> {
    let read_len = entry.file_size.min(max_len);
    let read_len = usize::try_from(read_len)
        .map_err(|_| CoreError::parse("metadata prefix length does not fit this platform"))?;

    let mut file = File::open(pack_path)?;
    file.seek(SeekFrom::Start(entry.start_pos))?;

    let mut bytes = vec![0; read_len];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn read_db_table_metadata(
    entry: &PackFileIndexEntry,
    db_name: &str,
    db_subname: &str,
    bytes: &[u8],
) -> CoreResult<DbTableMetadata> {
    let mut cursor = 0;
    let mut guid = None;
    let mut version = None;

    loop {
        let Some(marker) = bytes.get(cursor..cursor + 4) else {
            return Err(CoreError::parse(format!(
                "DB metadata is missing table marker byte and entry count: {}",
                entry.name
            )));
        };

        if marker == GUID_MARKER {
            cursor += 4;
            let (read_guid, next_cursor) = read_ts_utf_string(bytes, cursor, "DB GUID")?;
            guid = Some(read_guid);
            cursor = next_cursor;
        } else if marker == VERSION_MARKER {
            version = Some(read_i32(bytes, cursor + 4)?);
            cursor += 8;
        } else {
            cursor += 1;
            break;
        }
    }

    let entry_count = read_non_negative_size(bytes, cursor, "DB entry count")?;

    Ok(DbTableMetadata {
        name: entry.name.clone(),
        db_name: db_name.to_string(),
        db_subname: db_subname.to_string(),
        guid,
        version,
        entry_count,
    })
}

fn read_loc_file_metadata(entry: &PackFileIndexEntry, bytes: &[u8]) -> CoreResult<LocFileMetadata> {
    let bom = bytes
        .get(0..2)
        .ok_or_else(|| CoreError::parse(format!("LOC file is missing BOM: {}", entry.name)))?;
    if bom != LOC_BOM {
        return Err(CoreError::parse(format!(
            "LOC file has wrong BOM: {}",
            entry.name
        )));
    }

    let loc_magic = bytes
        .get(2..5)
        .ok_or_else(|| CoreError::parse(format!("LOC file is missing magic: {}", entry.name)))?;
    if loc_magic != LOC_MAGIC {
        return Err(CoreError::parse(format!(
            "LOC file has wrong magic: {}",
            entry.name
        )));
    }

    let version = read_i32(bytes, 6)?;
    let entry_count = read_non_negative_size(bytes, 10, "LOC entry count")?;

    Ok(LocFileMetadata {
        name: entry.name.clone(),
        version,
        entry_count,
    })
}

fn read_ts_utf_string(
    bytes: &[u8],
    cursor: usize,
    field_name: &str,
) -> CoreResult<(String, usize)> {
    let length = read_i16(bytes, cursor)?;
    if length < 0 {
        return Err(CoreError::parse(format!(
            "{field_name} length cannot be negative: {length}"
        )));
    }

    let length = usize::try_from(length)
        .map_err(|_| CoreError::parse(format!("{field_name} length does not fit this platform")))?;
    let byte_len = length
        .checked_mul(2)
        .ok_or_else(|| CoreError::parse(format!("{field_name} byte length overflow")))?;
    let start = cursor + 2;
    let end = start
        .checked_add(byte_len)
        .ok_or_else(|| CoreError::parse(format!("{field_name} offset overflow")))?;
    let string_bytes = bytes
        .get(start..end)
        .ok_or_else(|| CoreError::parse(format!("{field_name} extends past metadata prefix")))?;
    let utf16_units = string_bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]));
    let value = char::decode_utf16(utf16_units)
        .collect::<Result<String, _>>()
        .map_err(|error| CoreError::parse(format!("invalid {field_name}: {error}")))?;

    Ok((value, end))
}

fn read_magic(bytes: &[u8]) -> [u8; 4] {
    [bytes[0], bytes[1], bytes[2], bytes[3]]
}

fn read_header_buffer(bytes: &[u8]) -> [u8; 4] {
    [bytes[24], bytes[25], bytes[26], bytes[27]]
}

fn read_i32(bytes: &[u8], offset: usize) -> CoreResult<i32> {
    let field = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| CoreError::parse(format!("missing i32 at offset {offset}")))?;

    Ok(i32::from_le_bytes([field[0], field[1], field[2], field[3]]))
}

fn read_i16(bytes: &[u8], offset: usize) -> CoreResult<i16> {
    let field = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| CoreError::parse(format!("missing i16 at offset {offset}")))?;

    Ok(i16::from_le_bytes([field[0], field[1]]))
}

fn read_non_negative_size(bytes: &[u8], offset: usize, field_name: &str) -> CoreResult<u32> {
    let value = read_i32(bytes, offset)?;
    if value < 0 {
        return Err(CoreError::parse(format!(
            "{field_name} cannot be negative: {value}"
        )));
    }

    u32::try_from(value)
        .map_err(|_| CoreError::parse(format!("{field_name} does not fit u32: {value}")))
}

fn checked_section_end(
    start: usize,
    size: usize,
    total_len: usize,
    section_name: &str,
) -> CoreResult<usize> {
    let end = start.checked_add(size).ok_or_else(|| {
        CoreError::parse(format!("{section_name} offset overflow: {start} + {size}"))
    })?;

    if end > total_len {
        return Err(CoreError::parse(format!(
            "{section_name} extends past file end: end={end}, file_size={total_len}"
        )));
    }

    Ok(end)
}

fn read_nul_terminated_strings(bytes: &[u8]) -> CoreResult<Vec<String>> {
    let mut strings = Vec::new();
    let mut start = 0;

    for (index, byte) in bytes.iter().enumerate() {
        if *byte == 0 {
            if index > start {
                strings.push(read_utf8(&bytes[start..index], "dependency pack name")?);
            }
            start = index + 1;
        }
    }

    if start != bytes.len() {
        return Err(CoreError::parse(
            "dependency index is not nul-terminated at section end",
        ));
    }

    Ok(strings)
}

fn read_file_index(
    bytes: &[u8],
    file_count: usize,
    data_start: u64,
    supports_compression: bool,
) -> CoreResult<Vec<PackFileIndexEntry>> {
    let mut entries = Vec::with_capacity(file_count);
    let mut cursor = 0;
    let mut start_pos = data_start;

    for entry_index in 0..file_count {
        if cursor + FILE_SIZE_FIELD_LEN > bytes.len() {
            return Err(CoreError::parse(format!(
                "file index entry {entry_index} is missing file size"
            )));
        }

        let file_size = read_i32(bytes, cursor)?;
        if file_size < 0 {
            return Err(CoreError::parse(format!(
                "file index entry {entry_index} has negative file size: {file_size}"
            )));
        }
        cursor += FILE_SIZE_FIELD_LEN;

        let mut is_compressed = false;
        if supports_compression {
            let compression_flag = bytes.get(cursor).ok_or_else(|| {
                CoreError::parse(format!(
                    "file index entry {entry_index} is missing compression flag"
                ))
            })?;
            is_compressed = *compression_flag == 1;
            cursor += COMPRESSION_FLAG_LEN;
        }

        let name_end = bytes[cursor..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|relative_index| cursor + relative_index)
            .ok_or_else(|| {
                CoreError::parse(format!(
                    "file index entry {entry_index} name is not nul-terminated"
                ))
            })?;

        let name = read_utf8(&bytes[cursor..name_end], "packed file name")?;
        cursor = name_end + 1;

        let file_size = u64::try_from(file_size)
            .map_err(|_| CoreError::parse(format!("file size does not fit u64: {file_size}")))?;

        entries.push(PackFileIndexEntry {
            name,
            file_size,
            start_pos,
            is_compressed,
        });
        start_pos = start_pos
            .checked_add(file_size)
            .ok_or_else(|| CoreError::parse("packed file start position overflow"))?;
    }

    if cursor != bytes.len() {
        return Err(CoreError::parse(format!(
            "packed file index has {} trailing bytes after {file_count} entries",
            bytes.len() - cursor
        )));
    }

    Ok(entries)
}

fn validate_payload_bounds(entries: &[PackFileIndexEntry], total_len: usize) -> CoreResult<()> {
    let total_len =
        u64::try_from(total_len).map_err(|_| CoreError::parse("file size does not fit u64"))?;

    for entry in entries {
        let end = entry
            .start_pos
            .checked_add(entry.file_size)
            .ok_or_else(|| {
                CoreError::parse(format!("packed file payload overflows: {}", entry.name))
            })?;

        if end > total_len {
            return Err(CoreError::parse(format!(
                "packed file payload extends past file end: {} end={}, file_size={}",
                entry.name, end, total_len
            )));
        }
    }

    Ok(())
}

fn read_utf8(bytes: &[u8], field_name: &str) -> CoreResult<String> {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|error| CoreError::parse(format!("{field_name} is not valid UTF-8: {error}")))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        DbTableMetadata, LocFileMetadata, PackFileKind, PackFileMetadata, PackReadOptions,
        read_pack_contents, read_pack_contents_lossy, read_pack_index, read_packed_file_metadata,
    };
    use crate::ports::CoreErrorKind;

    static TEST_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn reads_dependencies_and_file_index() {
        let bytes = build_pack_bytes(
            3,
            &["data.pack", "audio.pack"],
            &[
                TestFileEntry {
                    name: "db\\units_tables\\main".to_string(),
                    size: 3,
                    compressed: false,
                    contents: vec![1, 2, 3],
                },
                TestFileEntry {
                    name: "text\\db\\example.loc".to_string(),
                    size: 4,
                    compressed: true,
                    contents: vec![4, 5, 6, 7],
                },
            ],
        );
        let path = write_temp_pack("reads_dependencies_and_file_index", &bytes);

        let index = read_pack_index(&path, &PackReadOptions::default()).unwrap();

        assert_eq!(index.magic, *b"PFH5");
        assert_eq!(index.byte_mask, 3);
        assert!(!index.is_movie);
        assert_eq!(index.dependency_packs, ["data.pack", "audio.pack"]);
        assert_eq!(index.files.len(), 2);
        assert_eq!(index.files[0].name, "db\\units_tables\\main");
        assert_eq!(index.files[0].file_size, 3);
        assert!(!index.files[0].is_compressed);
        assert_eq!(index.files[1].start_pos, index.files[0].start_pos + 3);
        assert!(index.files[1].is_compressed);

        fs::remove_file(path).ok();
    }

    #[test]
    fn classifies_db_loc_script_xml_and_other_files() {
        let db_entry = index_entry("db\\main_units_tables\\my_units", 0, 0, false);
        let loc_entry = index_entry("text\\db\\example.loc", 0, 0, false);
        let script_entry = index_entry("script\\campaign\\main.lua", 0, 0, false);
        let xml_entry = index_entry("variantmeshes\\foo.variantmeshdefinition", 0, 0, false);
        let other_entry = index_entry("ui\\skins\\icon.png", 0, 0, false);

        assert_eq!(
            db_entry.kind(),
            PackFileKind::DbTable {
                db_name: "main_units_tables".to_string(),
                db_subname: "my_units".to_string(),
            }
        );
        assert_eq!(loc_entry.kind(), PackFileKind::Loc);
        assert_eq!(script_entry.kind(), PackFileKind::Script);
        assert_eq!(xml_entry.kind(), PackFileKind::XmlLike);
        assert_eq!(other_entry.kind(), PackFileKind::Other);
    }

    #[test]
    fn reads_uncompressed_db_table_metadata() {
        let payload = build_db_payload(Some(7), 13);
        let bytes = build_pack_bytes(
            3,
            &[],
            &[TestFileEntry {
                name: "db\\main_units_tables\\my_units".to_string(),
                size: i32::try_from(payload.len()).unwrap(),
                compressed: false,
                contents: payload,
            }],
        );
        let path = write_temp_pack("reads_uncompressed_db_table_metadata", &bytes);
        let index = read_pack_index(&path, &PackReadOptions::default()).unwrap();

        let metadata = read_packed_file_metadata(&path, &index.files[0]).unwrap();

        assert_eq!(
            metadata,
            PackFileMetadata::DbTable(DbTableMetadata {
                name: "db\\main_units_tables\\my_units".to_string(),
                db_name: "main_units_tables".to_string(),
                db_subname: "my_units".to_string(),
                guid: None,
                version: Some(7),
                entry_count: 13,
            })
        );

        fs::remove_file(path).ok();
    }

    #[test]
    fn reads_db_table_guid_metadata_as_plain_string() {
        let guid = "129d32d8-3563-4d4f-8e19-a815e834e456";
        let payload = build_db_payload_with_guid(guid, Some(11), 2);
        let bytes = build_pack_bytes(
            3,
            &[],
            &[TestFileEntry {
                name: "db\\units_custom_battle_permissions_tables\\!!!!whmm_out".to_string(),
                size: i32::try_from(payload.len()).unwrap(),
                compressed: false,
                contents: payload,
            }],
        );
        let path = write_temp_pack("reads_db_table_guid_metadata_as_plain_string", &bytes);
        let index = read_pack_index(&path, &PackReadOptions::default()).unwrap();

        let metadata = read_packed_file_metadata(&path, &index.files[0]).unwrap();

        assert_eq!(
            metadata,
            PackFileMetadata::DbTable(DbTableMetadata {
                name: "db\\units_custom_battle_permissions_tables\\!!!!whmm_out".to_string(),
                db_name: "units_custom_battle_permissions_tables".to_string(),
                db_subname: "!!!!whmm_out".to_string(),
                guid: Some(guid.to_string()),
                version: Some(11),
                entry_count: 2,
            })
        );

        fs::remove_file(path).ok();
    }

    #[test]
    fn reads_unversioned_db_table_metadata() {
        let mut payload = Vec::new();
        payload.push(1);
        payload.extend_from_slice(&5_i32.to_le_bytes());
        let bytes = build_pack_bytes(
            3,
            &[],
            &[TestFileEntry {
                name: "db\\land_units_tables\\local".to_string(),
                size: i32::try_from(payload.len()).unwrap(),
                compressed: false,
                contents: payload,
            }],
        );
        let path = write_temp_pack("reads_unversioned_db_table_metadata", &bytes);
        let index = read_pack_index(&path, &PackReadOptions::default()).unwrap();

        let metadata = read_packed_file_metadata(&path, &index.files[0]).unwrap();

        assert_eq!(
            metadata,
            PackFileMetadata::DbTable(DbTableMetadata {
                name: "db\\land_units_tables\\local".to_string(),
                db_name: "land_units_tables".to_string(),
                db_subname: "local".to_string(),
                guid: None,
                version: None,
                entry_count: 5,
            })
        );

        fs::remove_file(path).ok();
    }

    #[test]
    fn reads_uncompressed_loc_metadata() {
        let payload = build_loc_payload(1, 2);
        let bytes = build_pack_bytes(
            3,
            &[],
            &[TestFileEntry {
                name: "text\\db\\example.loc".to_string(),
                size: i32::try_from(payload.len()).unwrap(),
                compressed: false,
                contents: payload,
            }],
        );
        let path = write_temp_pack("reads_uncompressed_loc_metadata", &bytes);
        let index = read_pack_index(&path, &PackReadOptions::default()).unwrap();

        let metadata = read_packed_file_metadata(&path, &index.files[0]).unwrap();

        assert_eq!(
            metadata,
            PackFileMetadata::Loc(LocFileMetadata {
                name: "text\\db\\example.loc".to_string(),
                version: 1,
                entry_count: 2,
            })
        );

        fs::remove_file(path).ok();
    }

    #[test]
    fn reads_compressed_db_table_metadata() {
        let compressed_payload = build_compressed_payload(&build_db_payload(Some(9), 21));
        let bytes = build_pack_bytes(
            3,
            &[],
            &[TestFileEntry {
                name: "db\\main_units_tables\\compressed".to_string(),
                size: i32::try_from(compressed_payload.len()).unwrap(),
                compressed: true,
                contents: compressed_payload,
            }],
        );
        let path = write_temp_pack("reads_compressed_db_table_metadata", &bytes);
        let index = read_pack_index(&path, &PackReadOptions::default()).unwrap();

        let metadata = read_packed_file_metadata(&path, &index.files[0]).unwrap();

        assert_eq!(
            metadata,
            PackFileMetadata::DbTable(DbTableMetadata {
                name: "db\\main_units_tables\\compressed".to_string(),
                db_name: "main_units_tables".to_string(),
                db_subname: "compressed".to_string(),
                guid: None,
                version: Some(9),
                entry_count: 21,
            })
        );

        fs::remove_file(path).ok();
    }

    #[test]
    fn reads_pack_contents_with_metadata_in_index_order() {
        let db_payload = build_db_payload(Some(9), 21);
        let loc_payload = build_loc_payload(1, 2);
        let bytes = build_pack_bytes(
            3,
            &["data.pack"],
            &[
                TestFileEntry {
                    name: "db\\main_units_tables\\core".to_string(),
                    size: i32::try_from(db_payload.len()).unwrap(),
                    compressed: false,
                    contents: db_payload,
                },
                TestFileEntry {
                    name: "text\\db\\core.loc".to_string(),
                    size: i32::try_from(loc_payload.len()).unwrap(),
                    compressed: false,
                    contents: loc_payload,
                },
                TestFileEntry {
                    name: "script\\campaign\\main.lua".to_string(),
                    size: 0,
                    compressed: false,
                    contents: Vec::new(),
                },
            ],
        );
        let path = write_temp_pack("reads_pack_contents_with_metadata_in_index_order", &bytes);

        let contents = read_pack_contents(&path, &PackReadOptions::default()).unwrap();

        assert_eq!(contents.index.dependency_packs, ["data.pack"]);
        assert_eq!(contents.metadata.len(), 3);
        assert!(matches!(contents.metadata[0], PackFileMetadata::DbTable(_)));
        assert!(matches!(contents.metadata[1], PackFileMetadata::Loc(_)));
        assert!(matches!(
            contents.metadata[2],
            PackFileMetadata::Other {
                kind: PackFileKind::Script,
                ..
            }
        ));

        fs::remove_file(path).ok();
    }

    #[test]
    fn strict_pack_contents_fails_on_malformed_metadata() {
        let bytes = build_pack_bytes(
            3,
            &[],
            &[TestFileEntry {
                name: "text\\db\\broken.loc".to_string(),
                size: 4,
                compressed: false,
                contents: vec![0, 1, 2, 3],
            }],
        );
        let path = write_temp_pack("strict_pack_contents_fails_on_malformed_metadata", &bytes);

        let error = read_pack_contents(&path, &PackReadOptions::default()).unwrap_err();

        assert_eq!(error.kind, CoreErrorKind::Parse);
        assert!(error.message.contains("wrong BOM"));

        fs::remove_file(path).ok();
    }

    #[test]
    fn lossy_pack_contents_keeps_rows_with_malformed_metadata() {
        let bytes = build_pack_bytes(
            3,
            &[],
            &[
                TestFileEntry {
                    name: "text\\db\\broken.loc".to_string(),
                    size: 4,
                    compressed: false,
                    contents: vec![0, 1, 2, 3],
                },
                TestFileEntry {
                    name: "ui\\skins\\icon.png".to_string(),
                    size: 0,
                    compressed: false,
                    contents: Vec::new(),
                },
            ],
        );
        let path = write_temp_pack(
            "lossy_pack_contents_keeps_rows_with_malformed_metadata",
            &bytes,
        );

        let contents = read_pack_contents_lossy(&path, &PackReadOptions::default()).unwrap();

        assert_eq!(contents.index.files.len(), 2);
        assert!(matches!(
            &contents.metadata[0],
            PackFileMetadata::Unsupported {
                kind: PackFileKind::Loc,
                reason,
                ..
            } if reason.contains("wrong BOM")
        ));
        assert!(matches!(
            contents.metadata[1],
            PackFileMetadata::Other {
                kind: PackFileKind::Other,
                ..
            }
        ));

        fs::remove_file(path).ok();
    }

    #[test]
    fn rejects_compressed_payload_without_prefix() {
        let bytes = build_pack_bytes(
            3,
            &[],
            &[TestFileEntry {
                name: "db\\main_units_tables\\compressed".to_string(),
                size: 3,
                compressed: true,
                contents: vec![1, 2, 3],
            }],
        );
        let path = write_temp_pack("rejects_compressed_payload_without_prefix", &bytes);
        let index = read_pack_index(&path, &PackReadOptions::default()).unwrap();

        let error = read_packed_file_metadata(&path, &index.files[0]).unwrap_err();

        assert_eq!(error.kind, CoreErrorKind::Parse);
        assert!(error.message.contains("four-byte prefix"));

        fs::remove_file(path).ok();
    }

    #[test]
    fn marks_byte_mask_four_as_movie() {
        let bytes = build_pack_bytes(4, &[], &[]);
        let path = write_temp_pack("marks_byte_mask_four_as_movie", &bytes);

        let index = read_pack_index(&path, &PackReadOptions::default()).unwrap();

        assert!(index.is_movie);

        fs::remove_file(path).ok();
    }

    #[test]
    fn rejects_wrong_magic() {
        let mut bytes = build_pack_bytes(3, &[], &[]);
        bytes[3] = b'4';
        let path = write_temp_pack("rejects_wrong_magic", &bytes);

        let error = read_pack_index(&path, &PackReadOptions::default()).unwrap_err();

        assert_eq!(error.kind, CoreErrorKind::Parse);
        assert!(error.message.contains("unexpected pack magic"));

        fs::remove_file(path).ok();
    }

    #[test]
    fn allows_payload_bytes_after_header_index() {
        let mut bytes = build_pack_bytes(3, &[], &[]);
        bytes.extend_from_slice(b"extra");
        let path = write_temp_pack("allows_payload_bytes_after_header_index", &bytes);

        let index = read_pack_index(&path, &PackReadOptions::default()).unwrap();

        assert!(index.files.is_empty());

        fs::remove_file(path).ok();
    }

    #[test]
    fn rejects_payload_that_extends_past_file_end() {
        let bytes = build_pack_bytes(
            3,
            &[],
            &[TestFileEntry {
                name: "db\\broken_tables\\main".to_string(),
                size: 99,
                compressed: false,
                contents: vec![1],
            }],
        );
        let path = write_temp_pack("rejects_payload_that_extends_past_file_end", &bytes);

        let error = read_pack_index(&path, &PackReadOptions::default()).unwrap_err();

        assert_eq!(error.kind, CoreErrorKind::Parse);
        assert!(error.message.contains("payload extends past file end"));

        fs::remove_file(path).ok();
    }

    #[test]
    fn rejects_unterminated_dependency_index() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PFH5");
        bytes.extend_from_slice(&3_i32.to_le_bytes());
        bytes.extend_from_slice(&0_i32.to_le_bytes());
        bytes.extend_from_slice(&3_i32.to_le_bytes());
        bytes.extend_from_slice(&0_i32.to_le_bytes());
        bytes.extend_from_slice(&0_i32.to_le_bytes());
        bytes.extend_from_slice(&0x7fff_ffff_i32.to_le_bytes());
        bytes.extend_from_slice(b"dep");
        let path = write_temp_pack("rejects_unterminated_dependency_index", &bytes);

        let error = read_pack_index(&path, &PackReadOptions::default()).unwrap_err();

        assert_eq!(error.kind, CoreErrorKind::Parse);
        assert!(
            error
                .message
                .contains("dependency index is not nul-terminated")
        );

        fs::remove_file(path).ok();
    }

    #[test]
    fn rejects_unterminated_file_name() {
        let mut index = Vec::new();
        index.extend_from_slice(&1_i32.to_le_bytes());
        index.push(0);
        index.extend_from_slice(b"db\\broken");

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PFH5");
        bytes.extend_from_slice(&3_i32.to_le_bytes());
        bytes.extend_from_slice(&0_i32.to_le_bytes());
        bytes.extend_from_slice(&0_i32.to_le_bytes());
        bytes.extend_from_slice(&1_i32.to_le_bytes());
        bytes.extend_from_slice(&(index.len() as i32).to_le_bytes());
        bytes.extend_from_slice(&0x7fff_ffff_i32.to_le_bytes());
        bytes.extend_from_slice(&index);
        bytes.push(1);
        let path = write_temp_pack("rejects_unterminated_file_name", &bytes);

        let error = read_pack_index(&path, &PackReadOptions::default()).unwrap_err();

        assert_eq!(error.kind, CoreErrorKind::Parse);
        assert!(error.message.contains("name is not nul-terminated"));

        fs::remove_file(path).ok();
    }

    struct TestFileEntry {
        name: String,
        size: i32,
        compressed: bool,
        contents: Vec<u8>,
    }

    fn index_entry(
        name: impl Into<String>,
        file_size: u64,
        start_pos: u64,
        is_compressed: bool,
    ) -> super::PackFileIndexEntry {
        super::PackFileIndexEntry {
            name: name.into(),
            file_size,
            start_pos,
            is_compressed,
        }
    }

    fn build_db_payload(version: Option<i32>, entry_count: i32) -> Vec<u8> {
        let mut payload = Vec::new();
        if let Some(version) = version {
            payload.extend_from_slice(&super::VERSION_MARKER);
            payload.extend_from_slice(&version.to_le_bytes());
        }
        payload.push(1);
        payload.extend_from_slice(&entry_count.to_le_bytes());
        payload
    }

    fn build_db_payload_with_guid(guid: &str, version: Option<i32>, entry_count: i32) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&super::GUID_MARKER);
        let utf16: Vec<u16> = guid.encode_utf16().collect();
        payload.extend_from_slice(&(utf16.len() as i16).to_le_bytes());
        for unit in utf16 {
            payload.extend_from_slice(&unit.to_le_bytes());
        }
        if let Some(version) = version {
            payload.extend_from_slice(&super::VERSION_MARKER);
            payload.extend_from_slice(&version.to_le_bytes());
        }
        payload.push(1);
        payload.extend_from_slice(&entry_count.to_le_bytes());
        payload
    }

    fn build_compressed_payload(uncompressed_payload: &[u8]) -> Vec<u8> {
        let mut payload = vec![0, 0, 0, 0];
        payload.extend(zstd::stream::encode_all(uncompressed_payload, 0).unwrap());
        payload
    }

    fn build_loc_payload(version: i32, entry_count: i32) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&super::LOC_BOM);
        payload.extend_from_slice(&super::LOC_MAGIC);
        payload.push(0);
        payload.extend_from_slice(&version.to_le_bytes());
        payload.extend_from_slice(&entry_count.to_le_bytes());
        payload
    }

    fn build_pack_bytes(byte_mask: i32, dependencies: &[&str], files: &[TestFileEntry]) -> Vec<u8> {
        let mut dependency_index = Vec::new();
        for dependency in dependencies {
            dependency_index.extend_from_slice(dependency.as_bytes());
            dependency_index.push(0);
        }

        let mut file_index = Vec::new();
        let mut contents = Vec::new();
        for file in files {
            file_index.extend_from_slice(&file.size.to_le_bytes());
            file_index.push(u8::from(file.compressed));
            file_index.extend_from_slice(file.name.as_bytes());
            file_index.push(0);
            contents.extend_from_slice(&file.contents);
        }

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PFH5");
        bytes.extend_from_slice(&byte_mask.to_le_bytes());
        bytes.extend_from_slice(&0_i32.to_le_bytes());
        bytes.extend_from_slice(&(dependency_index.len() as i32).to_le_bytes());
        bytes.extend_from_slice(&(files.len() as i32).to_le_bytes());
        bytes.extend_from_slice(&(file_index.len() as i32).to_le_bytes());
        bytes.extend_from_slice(&0x7fff_ffff_i32.to_le_bytes());
        bytes.extend_from_slice(&dependency_index);
        bytes.extend_from_slice(&file_index);
        bytes.extend_from_slice(&contents);
        bytes
    }

    fn write_temp_pack(test_name: &str, bytes: &[u8]) -> PathBuf {
        let counter = TEST_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "wh3mm-core-{test_name}-{}-{counter}.pack",
            std::process::id()
        ));
        fs::write(&path, bytes).unwrap();
        path
    }
}
