//! Pack/schema/row-decoder integration coverage using public core APIs.

use std::collections::BTreeMap;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

use wh3mm_core::{
    DbFieldSchema, DbFieldType, DbPrimitiveValue, DbSchema, DbTableMetadata, DbVersionSchema,
    PackFileMetadata, PackReadOptions, read_db_rows_from_pack, read_pack_index,
    read_packed_file_metadata, resolve_table_schema,
};

const VERSION_MARKER: [u8; 4] = [0xfc, 0xfd, 0xfe, 0xff];

static TEST_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn resolves_schema_and_decodes_rows_from_compressed_pack_entry() {
    let raw_db_payload = build_db_payload();
    let compressed_payload = build_compressed_payload(&raw_db_payload);
    let bytes = build_pack_bytes(&[TestFileEntry {
        name: "db\\example_tables\\local".to_string(),
        size: i32::try_from(compressed_payload.len()).unwrap(),
        compressed: true,
        contents: compressed_payload,
    }]);
    let path = write_temp_pack("compressed-db-schema-rows", &bytes);

    let index = read_pack_index(&path, &PackReadOptions::default()).unwrap();
    let metadata = read_packed_file_metadata(&path, &index.files[0]).unwrap();
    let schema = build_schema();
    let PackFileMetadata::DbTable(metadata) = metadata else {
        panic!("expected DB metadata");
    };

    let selected_schema = resolve_table_schema(&schema, &metadata).unwrap();
    let rows = read_db_rows_from_pack(&path, &index.files[0], &selected_schema.fields).unwrap();

    assert_eq!(metadata, expected_metadata());
    assert_eq!(selected_schema.version, 2);
    assert_eq!(rows.version, Some(2));
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(
        rows.rows[0][0].value,
        DbPrimitiveValue::String("unit_key".to_string())
    );
    assert_eq!(rows.rows[0][1].value, DbPrimitiveValue::I32(42));

    fs::remove_file(path).ok();
}

#[test]
fn refuses_older_zero_schema_when_metadata_requests_newer_version() {
    let metadata = DbTableMetadata {
        name: "db\\example_tables\\local".to_string(),
        db_name: "example_tables".to_string(),
        db_subname: "local".to_string(),
        guid: None,
        version: Some(2),
        entry_count: 1,
    };
    let mut schema = DbSchema::new();
    schema.insert(
        "example_tables".to_string(),
        vec![DbVersionSchema {
            version: 0,
            fields: vec![field("key", DbFieldType::StringU8, true)],
        }],
    );

    assert!(resolve_table_schema(&schema, &metadata).is_none());
}

fn expected_metadata() -> DbTableMetadata {
    DbTableMetadata {
        name: "db\\example_tables\\local".to_string(),
        db_name: "example_tables".to_string(),
        db_subname: "local".to_string(),
        guid: None,
        version: Some(2),
        entry_count: 1,
    }
}

fn build_schema() -> DbSchema {
    let mut schema = BTreeMap::new();
    schema.insert(
        "example_tables".to_string(),
        vec![
            DbVersionSchema {
                version: 2,
                fields: vec![
                    field("key", DbFieldType::StringU8, true),
                    field("value", DbFieldType::I32, false),
                ],
            },
            DbVersionSchema {
                version: 0,
                fields: vec![field("legacy_key", DbFieldType::StringU8, true)],
            },
        ],
    );
    schema
}

fn field(name: &str, field_type: DbFieldType, is_key: bool) -> DbFieldSchema {
    DbFieldSchema {
        name: name.to_string(),
        field_type,
        is_key,
        reference: None,
    }
}

fn build_db_payload() -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&VERSION_MARKER);
    payload.extend_from_slice(&2_i32.to_le_bytes());
    payload.push(1);
    payload.extend_from_slice(&1_i32.to_le_bytes());
    payload.extend_from_slice(&string_u8("unit_key"));
    payload.extend_from_slice(&42_i32.to_le_bytes());
    payload
}

fn string_u8(value: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(value.len() as u16).to_le_bytes());
    bytes.extend_from_slice(value.as_bytes());
    bytes
}

fn build_compressed_payload(uncompressed_payload: &[u8]) -> Vec<u8> {
    let mut payload = vec![0, 0, 0, 0];
    payload.extend(zstd::stream::encode_all(uncompressed_payload, 0).unwrap());
    payload
}

struct TestFileEntry {
    name: String,
    size: i32,
    compressed: bool,
    contents: Vec<u8>,
}

fn build_pack_bytes(files: &[TestFileEntry]) -> Vec<u8> {
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
    bytes.extend_from_slice(&3_i32.to_le_bytes());
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    bytes.extend_from_slice(&(files.len() as i32).to_le_bytes());
    bytes.extend_from_slice(&(file_index.len() as i32).to_le_bytes());
    bytes.extend_from_slice(&0x7fff_ffff_i32.to_le_bytes());
    bytes.extend_from_slice(&file_index);
    bytes.extend_from_slice(&contents);
    bytes
}

fn write_temp_pack(test_name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let counter = TEST_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "wh3mm-core-{test_name}-{}-{counter}.pack",
        std::process::id()
    ));
    fs::write(&path, bytes).unwrap();
    path
}
