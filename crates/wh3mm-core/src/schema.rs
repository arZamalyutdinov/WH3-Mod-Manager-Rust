//! Schema JSON loading for CA DB tables.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::db::{DbFieldReference, DbFieldSchema, DbFieldType};
use crate::pack::DbTableMetadata;
use crate::ports::{CoreError, CoreResult};

/// Schema map keyed by DB table name.
pub type DbSchema = BTreeMap<String, Vec<DbVersionSchema>>;

/// One DB table schema version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DbVersionSchema {
    /// DB schema version number.
    pub version: i32,
    /// Ordered field schema.
    pub fields: Vec<DbFieldSchema>,
}

/// Loads a schema JSON or `.json.zst` file.
///
/// # Errors
///
/// Returns [`CoreError`] when the file cannot be read/decompressed, JSON cannot
/// be parsed, or a field type is unsupported.
pub fn load_schema_file(path: impl AsRef<Path>) -> CoreResult<DbSchema> {
    let path = path.as_ref();
    let bytes = fs::read(path)?;
    let json_bytes = if path.extension().is_some_and(|extension| extension == "zst") {
        zstd::stream::decode_all(bytes.as_slice()).map_err(|error| {
            CoreError::parse(format!("schema zstd decompression failed: {error}"))
        })?
    } else {
        bytes
    };

    let raw_schema: RawSchema = serde_json::from_slice(&json_bytes)
        .map_err(|error| CoreError::parse(format!("schema JSON parse failed: {error}")))?;
    raw_schema.into_schema()
}

/// Selects the schema version using current WH3MM fallback behavior.
///
/// Exact version wins. Version `0` is used as fallback. If an explicit table
/// version was requested and only an older fallback exists, no schema is
/// selected.
#[must_use]
pub fn select_schema_version(
    versions: &[DbVersionSchema],
    table_version: Option<i32>,
) -> Option<&DbVersionSchema> {
    let selected = versions
        .iter()
        .find(|schema| Some(schema.version) == table_version)
        .or_else(|| versions.iter().find(|schema| schema.version == 0))?;

    if table_version.is_some_and(|version| selected.version < version) {
        None
    } else {
        Some(selected)
    }
}

/// Resolves a DB table metadata record to the best matching loaded schema.
#[must_use]
pub fn resolve_table_schema<'a>(
    schema: &'a DbSchema,
    metadata: &DbTableMetadata,
) -> Option<&'a DbVersionSchema> {
    let versions = schema.get(&metadata.db_name)?;
    select_schema_version(versions, metadata.version)
}

#[derive(Debug, Deserialize)]
struct RawSchema {
    definitions: BTreeMap<String, Vec<RawDbVersion>>,
}

impl RawSchema {
    fn into_schema(self) -> CoreResult<DbSchema> {
        self.definitions
            .into_iter()
            .map(|(name, versions)| {
                let mut versions = versions
                    .into_iter()
                    .map(RawDbVersion::into_schema)
                    .collect::<CoreResult<Vec<_>>>()?;
                versions.sort_by(|left, right| right.version.cmp(&left.version));
                Ok((name, versions))
            })
            .collect()
    }
}

#[derive(Debug, Deserialize)]
struct RawDbVersion {
    version: i32,
    fields: Vec<RawDbField>,
}

impl RawDbVersion {
    fn into_schema(self) -> CoreResult<DbVersionSchema> {
        let fields = self
            .fields
            .into_iter()
            .map(RawDbField::into_schema)
            .collect::<CoreResult<Vec<_>>>()?;

        Ok(DbVersionSchema {
            version: self.version,
            fields,
        })
    }
}

#[derive(Debug, Deserialize)]
struct RawDbField {
    name: String,
    field_type: String,
    #[serde(default)]
    is_key: bool,
    #[serde(default)]
    is_reference: Option<[String; 2]>,
}

impl RawDbField {
    fn into_schema(self) -> CoreResult<DbFieldSchema> {
        let reference = self
            .is_reference
            .map(|[table_name, field_name]| DbFieldReference {
                table_name: normalize_reference_table_name(table_name),
                field_name,
            });

        Ok(DbFieldSchema {
            name: self.name,
            field_type: self.field_type.parse::<DbFieldType>()?,
            is_key: self.is_key,
            reference,
        })
    }
}

fn normalize_reference_table_name(table_name: String) -> String {
    if table_name.ends_with("_tables") {
        table_name
    } else {
        format!("{table_name}_tables")
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::pack::DbTableMetadata;

    use super::{load_schema_file, resolve_table_schema, select_schema_version};

    #[test]
    fn loads_wh3_zstd_schema_file() {
        let schema_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/schema_wh3.json.zst");

        let schema = load_schema_file(schema_path).unwrap();

        let versions = schema.get("main_units_tables").unwrap();
        assert!(!versions.is_empty());
        assert!(versions.iter().any(|version| {
            version
                .fields
                .iter()
                .any(|field| field.name == "unit" || field.name == "key")
        }));
    }

    #[test]
    fn selects_exact_version_before_zero_fallback() {
        let schema_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/schema_wh3.json.zst");
        let schema = load_schema_file(schema_path).unwrap();
        let versions = schema.get("_kv_fatigue_tables").unwrap();

        let selected = select_schema_version(versions, Some(0)).unwrap();

        assert_eq!(selected.version, 0);
    }

    #[test]
    fn returns_none_when_only_zero_fallback_is_older_than_requested() {
        let schema_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/schema_wh3.json.zst");
        let schema = load_schema_file(schema_path).unwrap();
        let versions = schema.get("_kv_fatigue_tables").unwrap();

        assert!(select_schema_version(versions, Some(999)).is_none());
    }

    #[test]
    fn resolves_schema_from_db_table_metadata() {
        let schema_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/schema_wh3.json.zst");
        let schema = load_schema_file(schema_path).unwrap();
        let metadata = DbTableMetadata {
            name: "db\\_kv_fatigue_tables\\local".to_string(),
            db_name: "_kv_fatigue_tables".to_string(),
            db_subname: "local".to_string(),
            guid: None,
            version: Some(0),
            entry_count: 1,
        };

        let selected = resolve_table_schema(&schema, &metadata).unwrap();

        assert_eq!(selected.version, 0);
    }

    #[test]
    fn loads_reference_metadata_with_table_suffix() {
        let schema_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/schema_wh3.json.zst");
        let schema = load_schema_file(schema_path).unwrap();
        let versions = schema.get("abilities_tables").unwrap();
        let category = versions
            .iter()
            .flat_map(|version| version.fields.iter())
            .find(|field| field.name == "category" && field.reference.is_some())
            .unwrap();

        let reference = category.reference.as_ref().unwrap();

        assert_eq!(reference.table_name, "agent_ability_categories_tables");
        assert_eq!(reference.field_name, "category");
    }
}
