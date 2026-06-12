//! Schema-driven DB table row decoding.
//!
//! This is the first small Rust equivalent of the TypeScript `parseTypeBuffer`
//! path. It intentionally accepts an explicit schema slice instead of loading
//! schema files directly.

use std::path::Path;
use std::str::FromStr;

use crate::pack::{PackFileIndexEntry, read_packed_file_payload};
use crate::ports::{CoreError, CoreResult};

const GUID_MARKER: [u8; 4] = [0xfd, 0xfe, 0xfc, 0xff];
const VERSION_MARKER: [u8; 4] = [0xfc, 0xfd, 0xfe, 0xff];

/// Supported CA DB field types.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DbFieldType {
    /// One-byte boolean-ish value.
    Boolean,
    /// Optional one-byte-present marker followed by a `StringU8`.
    OptionalStringU8,
    /// Optional one-byte-present marker followed by an `I32`.
    OptionalI32,
    /// Two-byte length followed by single-byte string data.
    StringU8,
    /// 32-bit float.
    F32,
    /// 16-bit signed integer.
    I16,
    /// 32-bit signed integer.
    I32,
    /// 64-bit signed integer.
    I64,
    /// 64-bit float.
    F64,
    /// Packed RGB integer.
    ColourRgb,
    /// Two-byte character count followed by UTF-16LE string data.
    StringU16,
}

impl FromStr for DbFieldType {
    type Err = CoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "Boolean" => Ok(Self::Boolean),
            "OptionalStringU8" => Ok(Self::OptionalStringU8),
            "OptionalI32" => Ok(Self::OptionalI32),
            "StringU8" => Ok(Self::StringU8),
            "F32" => Ok(Self::F32),
            "I16" => Ok(Self::I16),
            "I32" => Ok(Self::I32),
            "I64" => Ok(Self::I64),
            "F64" => Ok(Self::F64),
            "ColourRGB" => Ok(Self::ColourRgb),
            "StringU16" => Ok(Self::StringU16),
            _ => Err(CoreError::parse(format!(
                "unsupported DB field type: {value}"
            ))),
        }
    }
}

/// One schema field for selected table decoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DbFieldSchema {
    /// Field name from schema JSON.
    pub name: String,
    /// Field type.
    pub field_type: DbFieldType,
    /// Whether this field is part of the schema key.
    pub is_key: bool,
    /// Optional target table/field referenced by this field.
    pub reference: Option<DbFieldReference>,
}

/// One schema reference edge from a DB field to a target DB table field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DbFieldReference {
    /// Target DB table name.
    pub table_name: String,
    /// Target DB field name.
    pub field_name: String,
}

/// Primitive DB cell value.
#[derive(Clone, Debug, PartialEq)]
pub enum DbPrimitiveValue {
    /// One-byte boolean-ish value.
    Boolean(u8),
    /// Optional string.
    OptionalString(Option<String>),
    /// Optional 32-bit signed integer.
    OptionalI32(Option<i32>),
    /// String value.
    String(String),
    /// 32-bit float.
    F32(f32),
    /// 16-bit signed integer.
    I16(i16),
    /// 32-bit signed integer.
    I32(i32),
    /// 64-bit signed integer.
    I64(i64),
    /// 64-bit float.
    F64(f64),
    /// Packed RGB integer.
    ColourRgb(i32),
}

/// One decoded DB cell.
#[derive(Clone, Debug, PartialEq)]
pub struct DbCell {
    /// Field schema name.
    pub name: String,
    /// Whether this cell is part of the schema key.
    pub is_key: bool,
    /// Decoded primitive value.
    pub value: DbPrimitiveValue,
}

/// Decoded rows for one DB table payload.
#[derive(Clone, Debug, PartialEq)]
pub struct DbRows {
    /// Optional GUID marker payload.
    pub guid: Option<String>,
    /// Optional DB version marker value.
    pub version: Option<i32>,
    /// Decoded rows.
    pub rows: Vec<Vec<DbCell>>,
}

/// Reads DB rows from a packed file entry using an explicit field schema.
///
/// # Errors
///
/// Returns [`CoreError`] when the packed file cannot be read/decompressed or
/// row decoding fails.
pub fn read_db_rows_from_pack(
    pack_path: impl AsRef<Path>,
    entry: &PackFileIndexEntry,
    schema: &[DbFieldSchema],
) -> CoreResult<DbRows> {
    let payload = read_packed_file_payload(pack_path, entry)?;
    read_db_rows_from_payload(&payload, schema)
}

/// Reads DB rows from an already decompressed DB table payload.
///
/// # Errors
///
/// Returns [`CoreError`] when the payload is malformed for the supplied schema.
pub fn read_db_rows_from_payload(payload: &[u8], schema: &[DbFieldSchema]) -> CoreResult<DbRows> {
    let (guid, version, mut cursor) = read_table_prefix(payload)?;
    let entry_count = read_non_negative_i32(payload, cursor, "DB entry count")?;
    cursor += 4;

    let mut rows = Vec::with_capacity(entry_count);
    for row_index in 0..entry_count {
        let mut row = Vec::with_capacity(schema.len());
        for field in schema {
            let (value, next_cursor) = read_field_value(payload, cursor, field.field_type)
                .map_err(|error| {
                    CoreError::parse(format!(
                        "failed to decode row {row_index}, field {}: {}",
                        field.name, error.message
                    ))
                })?;
            cursor = next_cursor;
            row.push(DbCell {
                name: field.name.clone(),
                is_key: field.is_key,
                value,
            });
        }
        rows.push(row);
    }

    if cursor != payload.len() {
        return Err(CoreError::parse(format!(
            "DB payload has {} trailing bytes after decoded rows",
            payload.len() - cursor
        )));
    }

    Ok(DbRows {
        guid,
        version,
        rows,
    })
}

/// Writes decoded DB rows back to a CA DB table payload.
///
/// # Errors
///
/// Returns [`CoreError`] when row/cell counts do not match the supplied schema,
/// a cell value is incompatible with the schema field type, or a generated
/// length/count cannot fit the DB binary format.
pub fn write_db_rows_to_payload(rows: &DbRows, schema: &[DbFieldSchema]) -> CoreResult<Vec<u8>> {
    let entry_count = i32::try_from(rows.rows.len())
        .map_err(|_| CoreError::invalid_input("too many DB rows to serialize"))?;
    let mut payload = Vec::new();

    if let Some(guid) = &rows.guid {
        payload.extend_from_slice(&GUID_MARKER);
        write_ts_utf_string(&mut payload, guid)?;
    }
    if let Some(version) = rows.version {
        payload.extend_from_slice(&VERSION_MARKER);
        payload.extend_from_slice(&version.to_le_bytes());
    }

    payload.push(1);
    payload.extend_from_slice(&entry_count.to_le_bytes());

    for (row_index, row) in rows.rows.iter().enumerate() {
        if row.len() != schema.len() {
            return Err(CoreError::invalid_input(format!(
                "DB row {row_index} has {} cells but schema has {} fields",
                row.len(),
                schema.len()
            )));
        }

        for (field, cell) in schema.iter().zip(row) {
            if cell.name != field.name {
                return Err(CoreError::invalid_input(format!(
                    "DB row {row_index} expected field {} but found {}",
                    field.name, cell.name
                )));
            }
            write_field_value(&mut payload, field, &cell.value).map_err(|error| {
                CoreError::invalid_input(format!(
                    "failed to serialize row {row_index}, field {}: {}",
                    field.name, error.message
                ))
            })?;
        }
    }

    Ok(payload)
}

fn read_table_prefix(payload: &[u8]) -> CoreResult<(Option<String>, Option<i32>, usize)> {
    let mut cursor = 0;
    let mut guid = None;
    let mut version = None;

    loop {
        let marker = payload.get(cursor..cursor + 4).ok_or_else(|| {
            CoreError::parse("DB payload is missing table marker byte and entry count")
        })?;

        if marker == GUID_MARKER {
            cursor += 4;
            let (read_guid, next_cursor) = read_ts_utf_string(payload, cursor, "DB GUID")?;
            guid = Some(read_guid);
            cursor = next_cursor;
        } else if marker == VERSION_MARKER {
            version = Some(read_i32(payload, cursor + 4)?);
            cursor += 8;
        } else {
            cursor += 1;
            break;
        }
    }

    Ok((guid, version, cursor))
}

fn read_field_value(
    bytes: &[u8],
    cursor: usize,
    field_type: DbFieldType,
) -> CoreResult<(DbPrimitiveValue, usize)> {
    match field_type {
        DbFieldType::Boolean => {
            let value = *bytes
                .get(cursor)
                .ok_or_else(|| CoreError::parse("missing Boolean byte"))?;
            Ok((DbPrimitiveValue::Boolean(value), cursor + 1))
        }
        DbFieldType::OptionalStringU8 => {
            let does_exist = *bytes
                .get(cursor)
                .ok_or_else(|| CoreError::parse("missing OptionalStringU8 presence byte"))?;
            if does_exist == 1 {
                let (value, cursor) = read_string_u8(bytes, cursor + 1)?;
                Ok((DbPrimitiveValue::OptionalString(Some(value)), cursor))
            } else {
                Ok((DbPrimitiveValue::OptionalString(None), cursor + 1))
            }
        }
        DbFieldType::OptionalI32 => {
            let does_exist = *bytes
                .get(cursor)
                .ok_or_else(|| CoreError::parse("missing OptionalI32 presence byte"))?;
            if does_exist == 1 {
                Ok((
                    DbPrimitiveValue::OptionalI32(Some(read_i32(bytes, cursor + 1)?)),
                    cursor + 5,
                ))
            } else {
                Ok((DbPrimitiveValue::OptionalI32(None), cursor + 1))
            }
        }
        DbFieldType::StringU8 => {
            let (value, cursor) = read_string_u8(bytes, cursor)?;
            Ok((DbPrimitiveValue::String(value), cursor))
        }
        DbFieldType::F32 => {
            let field = read_array::<4>(bytes, cursor, "F32")?;
            Ok((DbPrimitiveValue::F32(f32::from_le_bytes(field)), cursor + 4))
        }
        DbFieldType::I16 => Ok((DbPrimitiveValue::I16(read_i16(bytes, cursor)?), cursor + 2)),
        DbFieldType::I32 => Ok((DbPrimitiveValue::I32(read_i32(bytes, cursor)?), cursor + 4)),
        DbFieldType::I64 => {
            let field = read_array::<8>(bytes, cursor, "I64")?;
            Ok((DbPrimitiveValue::I64(i64::from_le_bytes(field)), cursor + 8))
        }
        DbFieldType::F64 => {
            let field = read_array::<8>(bytes, cursor, "F64")?;
            Ok((DbPrimitiveValue::F64(f64::from_le_bytes(field)), cursor + 8))
        }
        DbFieldType::ColourRgb => Ok((
            DbPrimitiveValue::ColourRgb(read_i32(bytes, cursor)?),
            cursor + 4,
        )),
        DbFieldType::StringU16 => {
            let (value, cursor) = read_string_u16(bytes, cursor)?;
            Ok((DbPrimitiveValue::String(value), cursor))
        }
    }
}

fn read_string_u8(bytes: &[u8], cursor: usize) -> CoreResult<(String, usize)> {
    let length = usize::from(read_u16(bytes, cursor)?);
    let start = cursor + 2;
    let end = checked_end(start, length, "StringU8")?;
    let string_bytes = bytes
        .get(start..end)
        .ok_or_else(|| CoreError::parse("StringU8 extends past payload"))?;
    Ok((String::from_utf8_lossy(string_bytes).into_owned(), end))
}

fn read_string_u16(bytes: &[u8], cursor: usize) -> CoreResult<(String, usize)> {
    let length = usize::try_from(read_i16(bytes, cursor)?)
        .map_err(|_| CoreError::parse("StringU16 length cannot be negative"))?;
    let start = cursor + 2;
    let byte_len = length
        .checked_mul(2)
        .ok_or_else(|| CoreError::parse("StringU16 byte length overflow"))?;
    let end = checked_end(start, byte_len, "StringU16")?;
    let string_bytes = bytes
        .get(start..end)
        .ok_or_else(|| CoreError::parse("StringU16 extends past payload"))?;
    let utf16_units = string_bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]));
    let value = char::decode_utf16(utf16_units)
        .collect::<Result<String, _>>()
        .map_err(|error| CoreError::parse(format!("invalid UTF-16 string: {error}")))?;

    Ok((value, end))
}

fn read_ts_utf_string(
    bytes: &[u8],
    cursor: usize,
    field_name: &str,
) -> CoreResult<(String, usize)> {
    let length = usize::try_from(read_i16(bytes, cursor)?)
        .map_err(|_| CoreError::parse(format!("{field_name} length cannot be negative")))?;
    let start = cursor + 2;
    let byte_len = length
        .checked_mul(2)
        .ok_or_else(|| CoreError::parse(format!("{field_name} byte length overflow")))?;
    let end = checked_end(start, byte_len, field_name)?;
    let string_bytes = bytes
        .get(start..end)
        .ok_or_else(|| CoreError::parse(format!("{field_name} extends past payload")))?;
    let utf16_units = string_bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]));
    let value = char::decode_utf16(utf16_units)
        .collect::<Result<String, _>>()
        .map_err(|error| CoreError::parse(format!("invalid {field_name}: {error}")))?;
    Ok((value, end))
}

fn write_field_value(
    bytes: &mut Vec<u8>,
    field: &DbFieldSchema,
    value: &DbPrimitiveValue,
) -> CoreResult<()> {
    match (field.field_type, value) {
        (DbFieldType::Boolean, DbPrimitiveValue::Boolean(value)) => bytes.push(*value),
        (DbFieldType::OptionalStringU8, DbPrimitiveValue::OptionalString(value)) => {
            if let Some(value) = value {
                bytes.push(1);
                write_string_u8(bytes, value)?;
            } else {
                bytes.push(0);
            }
        }
        (DbFieldType::OptionalI32, DbPrimitiveValue::OptionalI32(value)) => {
            if let Some(value) = value {
                bytes.push(1);
                bytes.extend_from_slice(&value.to_le_bytes());
            } else {
                bytes.push(0);
            }
        }
        (DbFieldType::StringU8, DbPrimitiveValue::String(value)) => write_string_u8(bytes, value)?,
        (DbFieldType::F32, DbPrimitiveValue::F32(value)) => {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        (DbFieldType::I16, DbPrimitiveValue::I16(value)) => {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        (DbFieldType::I32, DbPrimitiveValue::I32(value))
        | (DbFieldType::ColourRgb, DbPrimitiveValue::ColourRgb(value)) => {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        (DbFieldType::I64, DbPrimitiveValue::I64(value)) => {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        (DbFieldType::F64, DbPrimitiveValue::F64(value)) => {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        (DbFieldType::StringU16, DbPrimitiveValue::String(value)) => {
            write_string_u16(bytes, value)?;
        }
        _ => {
            return Err(CoreError::invalid_input(format!(
                "value {:?} is incompatible with {:?}",
                value, field.field_type
            )));
        }
    }

    Ok(())
}

fn write_string_u8(bytes: &mut Vec<u8>, value: &str) -> CoreResult<()> {
    let value_bytes = value.as_bytes();
    let length = u16::try_from(value_bytes.len())
        .map_err(|_| CoreError::invalid_input("StringU8 value is too long"))?;
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(value_bytes);
    Ok(())
}

fn write_string_u16(bytes: &mut Vec<u8>, value: &str) -> CoreResult<()> {
    let utf16: Vec<u16> = value.encode_utf16().collect();
    let length = i16::try_from(utf16.len())
        .map_err(|_| CoreError::invalid_input("StringU16 value is too long"))?;
    bytes.extend_from_slice(&length.to_le_bytes());
    for unit in utf16 {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(())
}

fn write_ts_utf_string(bytes: &mut Vec<u8>, value: &str) -> CoreResult<()> {
    let utf16: Vec<u16> = value.encode_utf16().collect();
    let length = i16::try_from(utf16.len())
        .map_err(|_| CoreError::invalid_input("DB GUID value is too long"))?;
    bytes.extend_from_slice(&length.to_le_bytes());
    for unit in utf16 {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(())
}

fn read_non_negative_i32(bytes: &[u8], cursor: usize, field_name: &str) -> CoreResult<usize> {
    let value = read_i32(bytes, cursor)?;
    if value < 0 {
        return Err(CoreError::parse(format!(
            "{field_name} cannot be negative: {value}"
        )));
    }

    usize::try_from(value)
        .map_err(|_| CoreError::parse(format!("{field_name} does not fit this platform")))
}

fn read_i16(bytes: &[u8], cursor: usize) -> CoreResult<i16> {
    Ok(i16::from_le_bytes(read_array::<2>(bytes, cursor, "I16")?))
}

fn read_u16(bytes: &[u8], cursor: usize) -> CoreResult<u16> {
    Ok(u16::from_le_bytes(read_array::<2>(bytes, cursor, "U16")?))
}

fn read_i32(bytes: &[u8], cursor: usize) -> CoreResult<i32> {
    Ok(i32::from_le_bytes(read_array::<4>(bytes, cursor, "I32")?))
}

fn read_array<const N: usize>(
    bytes: &[u8],
    cursor: usize,
    field_name: &str,
) -> CoreResult<[u8; N]> {
    let end = checked_end(cursor, N, field_name)?;
    let field = bytes
        .get(cursor..end)
        .ok_or_else(|| CoreError::parse(format!("{field_name} extends past payload")))?;
    field
        .try_into()
        .map_err(|_| CoreError::parse(format!("{field_name} length mismatch")))
}

fn checked_end(start: usize, len: usize, field_name: &str) -> CoreResult<usize> {
    start
        .checked_add(len)
        .ok_or_else(|| CoreError::parse(format!("{field_name} offset overflow")))
}

#[cfg(test)]
mod tests {
    use super::{
        DbCell, DbFieldSchema, DbFieldType, DbPrimitiveValue, DbRows, read_db_rows_from_payload,
        write_db_rows_to_payload,
    };

    #[test]
    fn reads_rows_with_core_primitive_types() {
        let schema = vec![
            field("enabled", DbFieldType::Boolean),
            field("key", DbFieldType::StringU8),
            field("optional", DbFieldType::OptionalStringU8),
            field("optional_i32", DbFieldType::OptionalI32),
            field("count", DbFieldType::I32),
            field("short", DbFieldType::I16),
            field("big", DbFieldType::I64),
            field("ratio", DbFieldType::F32),
            field("weight", DbFieldType::F64),
            field("colour", DbFieldType::ColourRgb),
            field("text", DbFieldType::StringU16),
        ];
        let payload = build_payload(
            Some(7),
            1,
            &[
                &[1],
                string_u8("unit_key").as_slice(),
                optional_string_u8(Some("portrait")).as_slice(),
                optional_i32(Some(44)).as_slice(),
                &12_i32.to_le_bytes(),
                &3_i16.to_le_bytes(),
                &99_i64.to_le_bytes(),
                &1.5_f32.to_le_bytes(),
                &2.25_f64.to_le_bytes(),
                &0x00ff00_i32.to_le_bytes(),
                string_u16("hello").as_slice(),
            ],
        );

        let rows = read_db_rows_from_payload(&payload, &schema).unwrap();

        assert_eq!(rows.version, Some(7));
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(rows.rows[0][0].value, DbPrimitiveValue::Boolean(1));
        assert_eq!(
            rows.rows[0][1].value,
            DbPrimitiveValue::String("unit_key".to_string())
        );
        assert_eq!(
            rows.rows[0][2].value,
            DbPrimitiveValue::OptionalString(Some("portrait".to_string()))
        );
        assert_eq!(
            rows.rows[0][3].value,
            DbPrimitiveValue::OptionalI32(Some(44))
        );
        assert_eq!(rows.rows[0][4].value, DbPrimitiveValue::I32(12));
        assert_eq!(rows.rows[0][5].value, DbPrimitiveValue::I16(3));
        assert_eq!(rows.rows[0][6].value, DbPrimitiveValue::I64(99));
        assert_eq!(rows.rows[0][7].value, DbPrimitiveValue::F32(1.5));
        assert_eq!(rows.rows[0][8].value, DbPrimitiveValue::F64(2.25));
        assert_eq!(rows.rows[0][9].value, DbPrimitiveValue::ColourRgb(0x00ff00));
        assert_eq!(
            rows.rows[0][10].value,
            DbPrimitiveValue::String("hello".to_string())
        );
    }

    #[test]
    fn writes_rows_with_core_primitive_types_for_round_trip() {
        let schema = vec![
            field("enabled", DbFieldType::Boolean),
            field("key", DbFieldType::StringU8),
            field("optional", DbFieldType::OptionalStringU8),
            field("optional_i32", DbFieldType::OptionalI32),
            field("count", DbFieldType::I32),
            field("short", DbFieldType::I16),
            field("big", DbFieldType::I64),
            field("ratio", DbFieldType::F32),
            field("weight", DbFieldType::F64),
            field("colour", DbFieldType::ColourRgb),
            field("text", DbFieldType::StringU16),
        ];
        let rows = DbRows {
            guid: Some("129d32d8-3563-4d4f-8e19-a815e834e456".to_string()),
            version: Some(11),
            rows: vec![vec![
                cell("enabled", DbPrimitiveValue::Boolean(1)),
                cell("key", DbPrimitiveValue::String("unit_key".to_string())),
                cell(
                    "optional",
                    DbPrimitiveValue::OptionalString(Some("portrait".to_string())),
                ),
                cell("optional_i32", DbPrimitiveValue::OptionalI32(Some(44))),
                cell("count", DbPrimitiveValue::I32(12)),
                cell("short", DbPrimitiveValue::I16(3)),
                cell("big", DbPrimitiveValue::I64(99)),
                cell("ratio", DbPrimitiveValue::F32(1.5)),
                cell("weight", DbPrimitiveValue::F64(2.25)),
                cell("colour", DbPrimitiveValue::ColourRgb(0x00ff00)),
                cell("text", DbPrimitiveValue::String("hello".to_string())),
            ]],
        };

        let payload = write_db_rows_to_payload(&rows, &schema).unwrap();
        let decoded = read_db_rows_from_payload(&payload, &schema).unwrap();

        assert_eq!(decoded, rows);
    }

    #[test]
    fn rejects_serializing_rows_that_do_not_match_schema() {
        let schema = vec![field("key", DbFieldType::StringU8)];
        let rows = DbRows {
            guid: None,
            version: None,
            rows: vec![vec![cell(
                "wrong",
                DbPrimitiveValue::String("value".to_string()),
            )]],
        };

        let error = write_db_rows_to_payload(&rows, &schema).unwrap_err();

        assert!(error.message.contains("expected field key"));
    }

    #[test]
    fn rejects_serializing_incompatible_cell_values() {
        let schema = vec![field("enabled", DbFieldType::Boolean)];
        let rows = DbRows {
            guid: None,
            version: None,
            rows: vec![vec![cell(
                "enabled",
                DbPrimitiveValue::String("yes".to_string()),
            )]],
        };

        let error = write_db_rows_to_payload(&rows, &schema).unwrap_err();

        assert!(error.message.contains("incompatible"));
    }

    #[test]
    fn reads_optional_string_absent() {
        let schema = vec![field("optional", DbFieldType::OptionalStringU8)];
        let payload = build_payload(None, 1, &[&[0]]);

        let rows = read_db_rows_from_payload(&payload, &schema).unwrap();

        assert_eq!(
            rows.rows[0][0].value,
            DbPrimitiveValue::OptionalString(None)
        );
    }

    #[test]
    fn parses_schema_field_type_names() {
        assert_eq!("Boolean".parse(), Ok(DbFieldType::Boolean));
        assert_eq!(
            "OptionalStringU8".parse(),
            Ok(DbFieldType::OptionalStringU8)
        );
        assert_eq!("OptionalI32".parse(), Ok(DbFieldType::OptionalI32));
        assert_eq!("StringU8".parse(), Ok(DbFieldType::StringU8));
        assert_eq!("F32".parse(), Ok(DbFieldType::F32));
        assert_eq!("I16".parse(), Ok(DbFieldType::I16));
        assert_eq!("I32".parse(), Ok(DbFieldType::I32));
        assert_eq!("I64".parse(), Ok(DbFieldType::I64));
        assert_eq!("F64".parse(), Ok(DbFieldType::F64));
        assert_eq!("ColourRGB".parse(), Ok(DbFieldType::ColourRgb));
        assert_eq!("StringU16".parse(), Ok(DbFieldType::StringU16));
        assert!("Buffer".parse::<DbFieldType>().is_err());
    }

    #[test]
    fn rejects_truncated_rows_with_field_context() {
        let schema = vec![field("key", DbFieldType::StringU8)];
        let payload = build_payload(None, 1, &[&[5, 0, b'a']]);

        let error = read_db_rows_from_payload(&payload, &schema).unwrap_err();

        assert!(error.message.contains("row 0, field key"));
    }

    #[test]
    fn rejects_payload_when_schema_leaves_trailing_bytes() {
        let schema = vec![field("key", DbFieldType::StringU8)];
        let payload = build_payload(
            None,
            1,
            &[string_u8("unit_key").as_slice(), &42_i32.to_le_bytes()],
        );

        let error = read_db_rows_from_payload(&payload, &schema).unwrap_err();

        assert!(error.message.contains("trailing bytes"));
    }

    fn field(name: &str, field_type: DbFieldType) -> DbFieldSchema {
        DbFieldSchema {
            name: name.to_string(),
            field_type,
            is_key: false,
            reference: None,
        }
    }

    fn cell(name: &str, value: DbPrimitiveValue) -> DbCell {
        DbCell {
            name: name.to_string(),
            is_key: false,
            value,
        }
    }

    fn build_payload(version: Option<i32>, entry_count: i32, row_chunks: &[&[u8]]) -> Vec<u8> {
        let mut payload = Vec::new();
        if let Some(version) = version {
            payload.extend_from_slice(&super::VERSION_MARKER);
            payload.extend_from_slice(&version.to_le_bytes());
        }
        payload.push(1);
        payload.extend_from_slice(&entry_count.to_le_bytes());
        for chunk in row_chunks {
            payload.extend_from_slice(chunk);
        }
        payload
    }

    fn string_u8(value: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(value.len() as u16).to_le_bytes());
        bytes.extend_from_slice(value.as_bytes());
        bytes
    }

    fn optional_string_u8(value: Option<&str>) -> Vec<u8> {
        let mut bytes = Vec::new();
        if let Some(value) = value {
            bytes.push(1);
            bytes.extend_from_slice(&string_u8(value));
        } else {
            bytes.push(0);
        }
        bytes
    }

    fn optional_i32(value: Option<i32>) -> Vec<u8> {
        let mut bytes = Vec::new();
        if let Some(value) = value {
            bytes.push(1);
            bytes.extend_from_slice(&value.to_le_bytes());
        } else {
            bytes.push(0);
        }
        bytes
    }

    fn string_u16(value: &str) -> Vec<u8> {
        let utf16: Vec<u16> = value.encode_utf16().collect();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(utf16.len() as i16).to_le_bytes());
        for unit in utf16 {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }
}
