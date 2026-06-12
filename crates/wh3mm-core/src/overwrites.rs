//! TS-style pack data overwrite generation.
//!
//! The legacy app stores row-level overwrite rules keyed by source pack path.
//! At launch it writes replacement packs into `whmm_overwrites` and loads those
//! packs instead of the original source packs. This module owns the pure core
//! part: decode selected DB tables, apply supported row edits/removals, and
//! build a generated pack containing the rewritten tables.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::db::{
    DbCell, DbFieldSchema, DbFieldType, DbPrimitiveValue, DbRows, read_db_rows_from_pack,
    write_db_rows_to_payload,
};
use crate::pack::{
    PackFileKind, PackFileMetadata, PackFileWrite, PackReadOptions, build_pfh5_pack_bytes,
    read_pack_index, read_packed_file_metadata,
};
use crate::ports::{CoreError, CoreResult};
use crate::schema::{DbSchema, resolve_table_schema};

/// One TS `packDataOverwrites` rule.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackDataOverwrite {
    /// Packed DB table path inside the source pack.
    pub pack_file_path: String,
    /// Stable row key used by the TS UI.
    pub columns_id: String,
    /// Column indices that identify matching rows.
    pub column_indices: Vec<usize>,
    /// Values paired with `column_indices`.
    pub column_values: Vec<PackDataOverwriteValue>,
    /// Operation to apply to matching rows.
    pub operation: PackDataOverwriteOperation,
    /// Column index to replace for `Edit`.
    pub overwrite_index: Option<usize>,
    /// Replacement value for `Edit`.
    pub overwrite_data: Option<PackDataOverwriteValue>,
}

/// Supported TS `PackDataOverwriteOperation` values.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PackDataOverwriteOperation {
    /// Remove matching rows.
    Remove,
    /// Append is present in the TS type, but TS launch generation does not
    /// currently mutate rows for it.
    Append,
    /// Edit one column on matching rows.
    Edit,
}

/// Primitive value shape stored by the TS overwrite config.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PackDataOverwriteValue {
    /// String value.
    String(String),
    /// Boolean value.
    Boolean(bool),
}

/// Generated overwrite pack bytes and summary metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedOverwritePack {
    /// Complete generated `.pack` bytes.
    pub bytes: Vec<u8>,
    /// Packed file names written into the generated pack.
    pub packed_file_names: Vec<String>,
    /// Number of row edits/removals applied.
    pub applied_operation_count: usize,
}

/// Builds an overwrite pack for one source pack and its configured overwrite rules.
///
/// Returns `Ok(None)` when `overwrites` is empty.
///
/// # Errors
///
/// Returns [`CoreError`] when the source pack cannot be read, a target DB table
/// cannot be decoded with the supplied schema, or an overwrite rule references
/// unsupported data.
pub fn build_pack_data_overwrite_pack(
    source_pack_path: impl AsRef<Path>,
    overwrites: &[PackDataOverwrite],
    schema: &DbSchema,
) -> CoreResult<Option<GeneratedOverwritePack>> {
    if overwrites.is_empty() {
        return Ok(None);
    }

    let source_pack_path = source_pack_path.as_ref();
    let requested_tables = requested_table_paths(overwrites);
    if requested_tables.is_empty() {
        return Err(CoreError::invalid_input(
            "pack data overwrites require at least one packed DB table path",
        ));
    }

    let index = read_pack_index(source_pack_path, &PackReadOptions::default())?;
    let mut files = Vec::new();
    let mut applied_operation_count = 0;

    for table_path in requested_tables {
        let entry = index
            .files
            .iter()
            .find(|entry| entry.name == table_path)
            .ok_or_else(|| {
                CoreError::invalid_input(format!(
                    "overwrite table {table_path} was not found in {}",
                    source_pack_path.display()
                ))
            })?;
        let PackFileKind::DbTable { .. } = entry.kind() else {
            return Err(CoreError::invalid_input(format!(
                "overwrite target is not a DB table: {table_path}"
            )));
        };
        let metadata = read_packed_file_metadata(source_pack_path, entry)?;
        let PackFileMetadata::DbTable(metadata) = metadata else {
            return Err(CoreError::invalid_input(format!(
                "overwrite target metadata is not a DB table: {table_path}"
            )));
        };
        let table_schema = resolve_table_schema(schema, &metadata).ok_or_else(|| {
            CoreError::parse(format!(
                "could not resolve schema for overwrite table {} version {:?}",
                metadata.name, metadata.version
            ))
        })?;
        let table_overwrites = overwrites
            .iter()
            .filter(|overwrite| overwrite.pack_file_path == table_path)
            .collect::<Vec<_>>();
        let rows = read_db_rows_from_pack(source_pack_path, entry, &table_schema.fields)?;
        let (rewritten_rows, applied_count) =
            apply_table_overwrites(rows, &table_schema.fields, &table_overwrites)?;
        applied_operation_count += applied_count;
        files.push(PackFileWrite {
            name: table_path,
            payload: write_db_rows_to_payload(&rewritten_rows, &table_schema.fields)?,
        });
    }

    let packed_file_names = files.iter().map(|file| file.name.clone()).collect();
    Ok(Some(GeneratedOverwritePack {
        bytes: build_pfh5_pack_bytes(&files)?,
        packed_file_names,
        applied_operation_count,
    }))
}

fn requested_table_paths(overwrites: &[PackDataOverwrite]) -> Vec<String> {
    let mut paths = Vec::new();
    for overwrite in overwrites {
        let path = overwrite.pack_file_path.trim();
        if !path.is_empty() && !paths.iter().any(|saved| saved == path) {
            paths.push(path.to_string());
        }
    }
    paths
}

fn apply_table_overwrites(
    mut rows: DbRows,
    schema: &[DbFieldSchema],
    overwrites: &[&PackDataOverwrite],
) -> CoreResult<(DbRows, usize)> {
    let mut applied_count = 0;

    for overwrite in overwrites {
        validate_overwrite(overwrite, schema)?;
        match overwrite.operation {
            PackDataOverwriteOperation::Append => {}
            PackDataOverwriteOperation::Remove => {
                let mut next_rows = Vec::with_capacity(rows.rows.len());
                for row in rows.rows {
                    if row_matches_overwrite(&row, overwrite)? {
                        applied_count += 1;
                    } else {
                        next_rows.push(row);
                    }
                }
                rows.rows = next_rows;
            }
            PackDataOverwriteOperation::Edit => {
                let overwrite_index = overwrite.overwrite_index.ok_or_else(|| {
                    CoreError::invalid_input(format!(
                        "overwrite {} is missing overwrite_index",
                        overwrite.columns_id
                    ))
                })?;
                let overwrite_data = overwrite.overwrite_data.as_ref().ok_or_else(|| {
                    CoreError::invalid_input(format!(
                        "overwrite {} is missing overwrite_data",
                        overwrite.columns_id
                    ))
                })?;
                let replacement =
                    overwrite_value_to_cell(overwrite_data, &schema[overwrite_index])?;

                let mut next_rows = Vec::with_capacity(rows.rows.len());
                for mut row in rows.rows {
                    if row_matches_overwrite(&row, overwrite)? {
                        row[overwrite_index].value = replacement.clone();
                        applied_count += 1;
                    }
                    next_rows.push(row);
                }
                rows.rows = next_rows;
            }
        }
    }

    Ok((rows, applied_count))
}

fn validate_overwrite(overwrite: &PackDataOverwrite, schema: &[DbFieldSchema]) -> CoreResult<()> {
    if overwrite.column_indices.len() != overwrite.column_values.len() {
        return Err(CoreError::invalid_input(format!(
            "overwrite {} has {} column indices but {} values",
            overwrite.columns_id,
            overwrite.column_indices.len(),
            overwrite.column_values.len()
        )));
    }

    for column_index in &overwrite.column_indices {
        if *column_index >= schema.len() {
            return Err(CoreError::invalid_input(format!(
                "overwrite {} references column {} but table has {} columns",
                overwrite.columns_id,
                column_index,
                schema.len()
            )));
        }
    }

    if let PackDataOverwriteOperation::Edit = overwrite.operation {
        let overwrite_index = overwrite.overwrite_index.ok_or_else(|| {
            CoreError::invalid_input(format!(
                "overwrite {} is missing overwrite_index",
                overwrite.columns_id
            ))
        })?;
        if overwrite_index >= schema.len() {
            return Err(CoreError::invalid_input(format!(
                "overwrite {} references overwrite column {} but table has {} columns",
                overwrite.columns_id,
                overwrite_index,
                schema.len()
            )));
        }
    }

    Ok(())
}

fn row_matches_overwrite(row: &[DbCell], overwrite: &PackDataOverwrite) -> CoreResult<bool> {
    for (column_index, expected_value) in overwrite
        .column_indices
        .iter()
        .zip(&overwrite.column_values)
    {
        let cell = row.get(*column_index).ok_or_else(|| {
            CoreError::invalid_input(format!(
                "overwrite {} references missing row column {}",
                overwrite.columns_id, column_index
            ))
        })?;
        if !overwrite_value_matches_cell(expected_value, cell) {
            return Ok(false);
        }
    }

    Ok(true)
}

fn overwrite_value_matches_cell(value: &PackDataOverwriteValue, cell: &DbCell) -> bool {
    match (value, &cell.value) {
        (PackDataOverwriteValue::String(left), DbPrimitiveValue::String(right)) => left == right,
        (PackDataOverwriteValue::String(left), DbPrimitiveValue::OptionalString(Some(right))) => {
            left == right
        }
        (PackDataOverwriteValue::Boolean(left), DbPrimitiveValue::Boolean(right)) => {
            *left == (*right != 0)
        }
        _ => false,
    }
}

fn overwrite_value_to_cell(
    value: &PackDataOverwriteValue,
    field: &DbFieldSchema,
) -> CoreResult<DbPrimitiveValue> {
    match (value, field.field_type) {
        (PackDataOverwriteValue::String(value), DbFieldType::StringU8 | DbFieldType::StringU16) => {
            Ok(DbPrimitiveValue::String(value.clone()))
        }
        (PackDataOverwriteValue::String(value), DbFieldType::OptionalStringU8) => {
            Ok(DbPrimitiveValue::OptionalString(Some(value.clone())))
        }
        (PackDataOverwriteValue::Boolean(value), DbFieldType::Boolean) => {
            Ok(DbPrimitiveValue::Boolean(u8::from(*value)))
        }
        _ => Err(CoreError::invalid_input(format!(
            "overwrite value {:?} is incompatible with field {} {:?}",
            value, field.name, field.field_type
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::db::{
        DbCell, DbFieldSchema, DbFieldType, DbPrimitiveValue, DbRows, read_db_rows_from_pack,
        write_db_rows_to_payload,
    };
    use crate::pack::{
        PackFileWrite, PackReadOptions, build_pfh5_pack_bytes, read_pack_index,
        read_packed_file_metadata,
    };
    use crate::ports::CoreErrorKind;
    use crate::schema::DbVersionSchema;

    use super::{
        PackDataOverwrite, PackDataOverwriteOperation, PackDataOverwriteValue,
        build_pack_data_overwrite_pack,
    };

    static TEST_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn builds_overwrite_pack_with_removed_and_edited_rows() {
        let source_pack = write_source_pack("remove-edit");
        let schema = test_schema();
        let overwrites = vec![
            remove_rule("db\\unit_tables\\units", "unit_b"),
            edit_rule("db\\unit_tables\\units", "unit_a", 2, false),
        ];

        let generated = build_pack_data_overwrite_pack(&source_pack, &overwrites, &schema)
            .unwrap()
            .unwrap();

        let generated_path = temp_pack_path("generated-remove-edit");
        fs::write(&generated_path, &generated.bytes).unwrap();
        let index = read_pack_index(&generated_path, &PackReadOptions::default()).unwrap();
        let rows =
            read_db_rows_from_pack(&generated_path, &index.files[0], &test_fields()).unwrap();

        assert_eq!(generated.packed_file_names, ["db\\unit_tables\\units"]);
        assert_eq!(generated.applied_operation_count, 2);
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(cell_string(&rows.rows[0][0]), "unit_a");
        assert_eq!(cell_bool(&rows.rows[0][2]), false);

        fs::remove_file(source_pack).ok();
        fs::remove_file(generated_path).ok();
    }

    #[test]
    fn rejects_overwrite_column_outside_schema() {
        let source_pack = write_source_pack("bad-column");
        let schema = test_schema();
        let overwrites = vec![PackDataOverwrite {
            pack_file_path: "db\\unit_tables\\units".to_string(),
            columns_id: "bad".to_string(),
            column_indices: vec![99],
            column_values: vec![PackDataOverwriteValue::String("unit_a".to_string())],
            operation: PackDataOverwriteOperation::Remove,
            overwrite_index: None,
            overwrite_data: None,
        }];

        let error = build_pack_data_overwrite_pack(&source_pack, &overwrites, &schema).unwrap_err();

        assert_eq!(error.kind, CoreErrorKind::InvalidInput);

        fs::remove_file(source_pack).ok();
    }

    #[test]
    fn ignores_append_operation_like_ts_launch_generation() {
        let source_pack = write_source_pack("append-noop");
        let schema = test_schema();
        let overwrites = vec![PackDataOverwrite {
            pack_file_path: "db\\unit_tables\\units".to_string(),
            columns_id: "append".to_string(),
            column_indices: vec![0],
            column_values: vec![PackDataOverwriteValue::String("unit_a".to_string())],
            operation: PackDataOverwriteOperation::Append,
            overwrite_index: None,
            overwrite_data: None,
        }];

        let generated = build_pack_data_overwrite_pack(&source_pack, &overwrites, &schema)
            .unwrap()
            .unwrap();

        let generated_path = temp_pack_path("generated-append-noop");
        fs::write(&generated_path, &generated.bytes).unwrap();
        let index = read_pack_index(&generated_path, &PackReadOptions::default()).unwrap();
        let rows =
            read_db_rows_from_pack(&generated_path, &index.files[0], &test_fields()).unwrap();

        assert_eq!(generated.applied_operation_count, 0);
        assert_eq!(rows.rows.len(), 2);

        fs::remove_file(source_pack).ok();
        fs::remove_file(generated_path).ok();
    }

    fn write_source_pack(test_name: &str) -> PathBuf {
        let rows = DbRows {
            guid: None,
            version: Some(1),
            rows: vec![
                row("unit_a", "faction_a", true),
                row("unit_b", "faction_b", true),
            ],
        };
        let payload = write_db_rows_to_payload(&rows, &test_fields()).unwrap();
        let bytes = build_pfh5_pack_bytes(&[PackFileWrite {
            name: "db\\unit_tables\\units".to_string(),
            payload,
        }])
        .unwrap();
        let path = temp_pack_path(test_name);
        fs::write(&path, bytes).unwrap();

        let index = read_pack_index(&path, &PackReadOptions::default()).unwrap();
        let metadata = read_packed_file_metadata(&path, &index.files[0]).unwrap();
        assert!(matches!(
            metadata,
            crate::pack::PackFileMetadata::DbTable(_)
        ));

        path
    }

    fn test_schema() -> crate::schema::DbSchema {
        BTreeMap::from([(
            "unit_tables".to_string(),
            vec![DbVersionSchema {
                version: 1,
                fields: test_fields(),
            }],
        )])
    }

    fn test_fields() -> Vec<DbFieldSchema> {
        vec![
            field("unit", DbFieldType::StringU8),
            field("faction", DbFieldType::StringU8),
            field("enabled", DbFieldType::Boolean),
        ]
    }

    fn field(name: &str, field_type: DbFieldType) -> DbFieldSchema {
        DbFieldSchema {
            name: name.to_string(),
            field_type,
            is_key: false,
            reference: None,
        }
    }

    fn row(unit: &str, faction: &str, enabled: bool) -> Vec<DbCell> {
        vec![
            cell("unit", DbPrimitiveValue::String(unit.to_string())),
            cell("faction", DbPrimitiveValue::String(faction.to_string())),
            cell("enabled", DbPrimitiveValue::Boolean(u8::from(enabled))),
        ]
    }

    fn cell(name: &str, value: DbPrimitiveValue) -> DbCell {
        DbCell {
            name: name.to_string(),
            is_key: false,
            value,
        }
    }

    fn remove_rule(pack_file_path: &str, unit: &str) -> PackDataOverwrite {
        PackDataOverwrite {
            pack_file_path: pack_file_path.to_string(),
            columns_id: unit.to_string(),
            column_indices: vec![0],
            column_values: vec![PackDataOverwriteValue::String(unit.to_string())],
            operation: PackDataOverwriteOperation::Remove,
            overwrite_index: None,
            overwrite_data: None,
        }
    }

    fn edit_rule(
        pack_file_path: &str,
        unit: &str,
        overwrite_index: usize,
        overwrite_data: bool,
    ) -> PackDataOverwrite {
        PackDataOverwrite {
            pack_file_path: pack_file_path.to_string(),
            columns_id: unit.to_string(),
            column_indices: vec![0],
            column_values: vec![PackDataOverwriteValue::String(unit.to_string())],
            operation: PackDataOverwriteOperation::Edit,
            overwrite_index: Some(overwrite_index),
            overwrite_data: Some(PackDataOverwriteValue::Boolean(overwrite_data)),
        }
    }

    fn cell_string(cell: &DbCell) -> &str {
        let DbPrimitiveValue::String(value) = &cell.value else {
            panic!("expected string cell");
        };
        value
    }

    fn cell_bool(cell: &DbCell) -> bool {
        let DbPrimitiveValue::Boolean(value) = cell.value else {
            panic!("expected boolean cell");
        };
        value != 0
    }

    fn temp_pack_path(test_name: &str) -> PathBuf {
        let counter = TEST_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "wh3mm-core-overwrites-{test_name}-{}-{counter}.pack",
            std::process::id()
        ));
        path
    }
}
