//! Pack compatibility and reference analysis.
//!
//! This is the first Rust layer corresponding to the TypeScript
//! `modCompat/packFileCompatManager.ts` path. It starts with checks that only
//! require pack indexes: whole-file overwrite collisions and declared
//! dependency-pack presence.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::db::{DbPrimitiveValue, DbRows, read_db_rows_from_pack};
use crate::domain::ModRecord;
use crate::pack::{
    DbTableMetadata, PackFileIndexEntry, PackFileKind, PackFileMetadata, PackIndex,
    PackReadOptions, read_pack_contents_lossy, read_pack_index, read_packed_file_payload,
};
use crate::schema::{DbSchema, DbVersionSchema, resolve_table_schema};

/// Compatibility report for an ordered enabled mod set.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PackConflictReport {
    /// Packed files with the same internal path in multiple packs.
    pub pack_file_collisions: Vec<PackFileCollision>,
    /// DB table rows sharing the same key value in multiple packs.
    pub pack_table_collisions: Vec<PackTableCollision>,
    /// Pack headers declaring dependency packs that are not enabled/present.
    pub missing_dependency_packs: Vec<MissingDependencyPack>,
    /// DB references whose target value is absent from the enabled pack set.
    pub missing_db_references: Vec<MissingDbReference>,
    /// Numeric DB ID values duplicated within or across packs.
    pub unique_id_collisions: Vec<UniqueIdCollision>,
    /// Lua `core:add_listener(...)` names duplicated within or across packs.
    pub script_listener_collisions: Vec<ScriptListenerCollision>,
    /// XML-like packed files that reference files absent from enabled packs.
    pub missing_file_references: Vec<FileToFileReference>,
    /// Number of DB tables decoded during schema-backed analysis.
    pub decoded_db_table_count: usize,
    /// Packs that could not be indexed.
    pub pack_read_errors: Vec<PackReadError>,
    /// DB tables that could not be decoded during schema-backed analysis.
    pub table_read_errors: Vec<TableReadError>,
    /// Lua script files that could not be read/decoded during analysis.
    pub script_read_errors: Vec<ScriptReadError>,
    /// XML-like files that could not be read/decoded during file-reference analysis.
    pub file_reference_read_errors: Vec<FileReferenceReadError>,
}

/// One directional packed-file collision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackFileCollision {
    /// First pack name.
    pub first_pack_name: String,
    /// Second pack name.
    pub second_pack_name: String,
    /// Internal packed-file path that collides.
    pub file_name: String,
    /// Whether both packed-file payload sizes match.
    pub are_same_size: bool,
}

/// One directional DB table key collision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackTableCollision {
    /// First pack name.
    pub first_pack_name: String,
    /// Second pack name.
    pub second_pack_name: String,
    /// First packed DB file path.
    pub file_name: String,
    /// Second packed DB file path.
    pub second_file_name: String,
    /// Schema key field name.
    pub key: String,
    /// Colliding key value.
    pub value: String,
}

/// Missing declared dependency pack.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissingDependencyPack {
    /// Pack with the missing dependency.
    pub pack_name: String,
    /// Dependency pack name declared in the header.
    pub dependency_pack_name: String,
}

/// Missing DB reference discovered from schema `is_reference` metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissingDbReference {
    /// Pack containing the unresolved reference.
    pub pack_name: String,
    /// Packed DB file containing the unresolved reference.
    pub origin_file_name: String,
    /// Source DB table name.
    pub origin_db_name: String,
    /// Source DB field name.
    pub origin_field_name: String,
    /// Target DB table name.
    pub target_db_name: String,
    /// Target DB field name.
    pub target_field_name: String,
    /// Referenced value missing from the enabled pack set.
    pub value: String,
}

/// Unique numeric DB ID collision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UniqueIdCollision {
    /// DB table name containing the unique ID field.
    pub table_name: String,
    /// Field expected to be unique.
    pub field_name: String,
    /// First colliding value occurrence.
    pub value: UniqueIdValue,
    /// Second colliding value occurrence.
    pub value_two: UniqueIdValue,
    /// First pack name.
    pub first_pack_name: String,
    /// Second pack name, absent for duplicate values inside one pack.
    pub second_pack_name: Option<String>,
}

/// One unique numeric DB ID occurrence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UniqueIdValue {
    /// Unique ID value.
    pub value: String,
    /// DB packed-file subname.
    pub pack_file_name: String,
    /// Full decoded DB row values.
    pub table_row: Vec<String>,
    /// Pack containing the row.
    pub pack_name: String,
}

/// Collision between two Lua `core:add_listener(...)` declarations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptListenerCollision {
    /// Script packed-file path.
    pub pack_file_name: String,
    /// First listener declaration.
    pub value: ScriptListenerValue,
    /// Second listener declaration.
    pub value_two: ScriptListenerValue,
    /// First pack name.
    pub first_pack_name: String,
    /// Second pack name, absent for duplicate listener names inside one pack.
    pub second_pack_name: Option<String>,
}

/// One Lua `core:add_listener(...)` declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptListenerValue {
    /// Listener name.
    pub value: String,
    /// Script packed-file path.
    pub pack_file_name: String,
    /// Pack containing the script.
    pub pack_name: String,
    /// Byte position of the matched `core:add_listener` call.
    pub position: usize,
}

/// Missing packed-file reference discovered from XML-like files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileToFileReference {
    /// Referenced packed-file path, normalized to lowercase backslash form.
    pub reference: String,
    /// Pack containing the reference.
    pub pack_name: String,
    /// Packed file containing the reference.
    pub pack_file_name: String,
}

/// Error produced while trying to index one pack.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackReadError {
    /// Source pack path.
    pub pack_path: String,
    /// Human-readable parser/IO error.
    pub message: String,
}

/// Error produced while trying to decode one DB table for analysis.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableReadError {
    /// Source pack name.
    pub pack_name: String,
    /// Source packed DB file path.
    pub table_name: String,
    /// Human-readable parser/schema error.
    pub message: String,
}

/// Error produced while trying to read/decode one Lua script for analysis.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptReadError {
    /// Source pack name.
    pub pack_name: String,
    /// Source script packed-file path.
    pub script_name: String,
    /// Human-readable parser/IO error.
    pub message: String,
}

/// Error produced while trying to read/decode one XML-like file for file-reference analysis.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileReferenceReadError {
    /// Source pack name.
    pub pack_name: String,
    /// Source XML-like packed-file path.
    pub file_name: String,
    /// Human-readable parser/IO error.
    pub message: String,
}

/// Reads enabled mods' pack indexes and analyzes first-pass compatibility.
#[must_use]
pub fn analyze_enabled_mod_conflicts(
    mods: &[ModRecord],
    options: &PackReadOptions,
) -> PackConflictReport {
    let mut indexes = Vec::new();
    let mut pack_read_errors = Vec::new();

    for mod_record in mods
        .iter()
        .filter(|mod_record| mod_record.effectively_enabled())
    {
        match read_pack_index(&mod_record.identity.path, options) {
            Ok(index) => indexes.push(index),
            Err(error) => pack_read_errors.push(PackReadError {
                pack_path: mod_record.identity.path.clone(),
                message: error.message,
            }),
        }
    }

    let mut report = analyze_pack_indexes(&indexes);
    report.pack_read_errors = pack_read_errors;
    report
}

/// Reads enabled mods and analyzes index-level and schema-backed DB table compatibility.
#[must_use]
pub fn analyze_enabled_mod_conflicts_with_schema(
    mods: &[ModRecord],
    options: &PackReadOptions,
    schema: &DbSchema,
) -> PackConflictReport {
    let mut indexes = Vec::new();
    let mut tables = Vec::new();
    let mut script_listeners = Vec::new();
    let mut file_references = Vec::new();
    let mut pack_read_errors = Vec::new();
    let mut table_read_errors = Vec::new();
    let mut script_read_errors = Vec::new();
    let mut file_reference_read_errors = Vec::new();

    for mod_record in mods
        .iter()
        .filter(|mod_record| mod_record.effectively_enabled())
    {
        match read_pack_contents_lossy(&mod_record.identity.path, options) {
            Ok(contents) => {
                let pack_name = pack_display_name(&contents.index);
                for (entry, metadata) in contents.index.files.iter().zip(contents.metadata.iter()) {
                    if entry.kind() == PackFileKind::Script {
                        match read_script_listeners(
                            &contents.index,
                            entry,
                            &mod_record.identity.path,
                        ) {
                            Ok(listeners) => script_listeners.extend(listeners),
                            Err(message) => script_read_errors.push(ScriptReadError {
                                pack_name: pack_name.clone(),
                                script_name: entry.name.clone(),
                                message,
                            }),
                        }
                    }
                    if entry.kind() == PackFileKind::XmlLike {
                        match read_file_references(
                            &contents.index,
                            entry,
                            &mod_record.identity.path,
                        ) {
                            Ok(references) => file_references.extend(references),
                            Err(message) => {
                                file_reference_read_errors.push(FileReferenceReadError {
                                    pack_name: pack_name.clone(),
                                    file_name: entry.name.clone(),
                                    message,
                                });
                            }
                        }
                    }

                    let PackFileMetadata::DbTable(metadata) = metadata else {
                        continue;
                    };
                    let Some(selected_schema) = resolve_table_schema(schema, metadata) else {
                        continue;
                    };
                    let decoded_table = decode_analysis_table(
                        &contents.index,
                        entry,
                        metadata,
                        selected_schema,
                        &mod_record.identity.path,
                    );
                    match decoded_table {
                        Ok(table) => tables.push(table),
                        Err(message) => table_read_errors.push(TableReadError {
                            pack_name: pack_name.clone(),
                            table_name: metadata.name.clone(),
                            message,
                        }),
                    }
                }
                indexes.push(contents.index);
            }
            Err(error) => pack_read_errors.push(PackReadError {
                pack_path: mod_record.identity.path.clone(),
                message: error.message,
            }),
        }
    }

    let mut report = analyze_pack_indexes(&indexes);
    report.decoded_db_table_count = tables.len();
    report.pack_table_collisions = find_pack_table_collisions(&tables);
    report.missing_db_references = find_missing_db_references(&tables);
    report.unique_id_collisions = find_unique_id_collisions(&tables);
    report.script_listener_collisions = find_script_listener_collisions(&script_listeners);
    report.missing_file_references = find_missing_file_references(&indexes, &file_references);
    report.pack_read_errors = pack_read_errors;
    report.table_read_errors = table_read_errors;
    report.script_read_errors = script_read_errors;
    report.file_reference_read_errors = file_reference_read_errors;
    report
}

/// Analyzes already-read pack indexes.
#[must_use]
pub fn analyze_pack_indexes(indexes: &[PackIndex]) -> PackConflictReport {
    PackConflictReport {
        pack_file_collisions: find_pack_file_collisions(indexes),
        pack_table_collisions: Vec::new(),
        missing_dependency_packs: find_missing_dependency_packs(indexes),
        missing_db_references: Vec::new(),
        unique_id_collisions: Vec::new(),
        script_listener_collisions: Vec::new(),
        missing_file_references: Vec::new(),
        decoded_db_table_count: 0,
        pack_read_errors: Vec::new(),
        table_read_errors: Vec::new(),
        script_read_errors: Vec::new(),
        file_reference_read_errors: Vec::new(),
    }
}

fn decode_analysis_table(
    index: &PackIndex,
    entry: &PackFileIndexEntry,
    metadata: &DbTableMetadata,
    schema: &DbVersionSchema,
    pack_path: &str,
) -> Result<DecodedDbTable, String> {
    let key_fields = schema
        .fields
        .iter()
        .filter(|field| field.is_key)
        .collect::<Vec<_>>();

    let rows = match read_db_rows_from_pack(pack_path, entry, &schema.fields) {
        Ok(rows) => rows,
        Err(error) => return Err(error.message),
    };
    let field_values = field_values_from_rows(&rows);
    let field_names = schema
        .fields
        .iter()
        .map(|field| field.name.clone())
        .collect::<Vec<_>>();
    let row_values = row_values_from_rows(&rows);
    let (key_name, key_values) = if key_fields.len() == 1 {
        let key_field = key_fields[0];
        (
            Some(key_field.name.clone()),
            field_values
                .get(&key_field.name)
                .cloned()
                .unwrap_or_default(),
        )
    } else {
        (None, Vec::new())
    };
    let references = schema
        .fields
        .iter()
        .filter_map(|field| {
            let reference = field.reference.as_ref()?;
            Some(DecodedDbFieldReference {
                origin_field_name: field.name.clone(),
                target_db_name: reference.table_name.clone(),
                target_field_name: reference.field_name.clone(),
                values: field_values.get(&field.name).cloned().unwrap_or_default(),
            })
        })
        .collect();

    Ok(DecodedDbTable {
        pack_name: pack_display_name(index),
        file_name: metadata.name.clone(),
        db_subname: metadata.db_subname.clone(),
        db_name: metadata.db_name.clone(),
        key_name,
        key_values,
        field_names,
        field_values,
        row_values,
        references,
    })
}

fn field_values_from_rows(rows: &DbRows) -> BTreeMap<String, Vec<String>> {
    let mut field_values: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in &rows.rows {
        for cell in row {
            field_values
                .entry(cell.name.clone())
                .or_default()
                .push(db_value_to_string(&cell.value));
        }
    }
    field_values
}

fn row_values_from_rows(rows: &DbRows) -> Vec<Vec<String>> {
    rows.rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| db_value_to_string(&cell.value))
                .collect()
        })
        .collect()
}

fn db_value_to_string(value: &DbPrimitiveValue) -> String {
    match value {
        DbPrimitiveValue::Boolean(value) => value.to_string(),
        DbPrimitiveValue::OptionalString(Some(value)) | DbPrimitiveValue::String(value) => {
            value.clone()
        }
        DbPrimitiveValue::OptionalString(None) | DbPrimitiveValue::OptionalI32(None) => {
            String::new()
        }
        DbPrimitiveValue::OptionalI32(Some(value))
        | DbPrimitiveValue::I32(value)
        | DbPrimitiveValue::ColourRgb(value) => value.to_string(),
        DbPrimitiveValue::I16(value) => value.to_string(),
        DbPrimitiveValue::I64(value) => value.to_string(),
        DbPrimitiveValue::F32(value) => value.to_string(),
        DbPrimitiveValue::F64(value) => value.to_string(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DecodedDbTable {
    pack_name: String,
    file_name: String,
    db_subname: String,
    db_name: String,
    key_name: Option<String>,
    key_values: Vec<String>,
    field_names: Vec<String>,
    field_values: BTreeMap<String, Vec<String>>,
    row_values: Vec<Vec<String>>,
    references: Vec<DecodedDbFieldReference>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DecodedDbFieldReference {
    origin_field_name: String,
    target_db_name: String,
    target_field_name: String,
    values: Vec<String>,
}

fn read_script_listeners(
    index: &PackIndex,
    entry: &PackFileIndexEntry,
    pack_path: &str,
) -> Result<Vec<ScriptListenerValue>, String> {
    let payload = read_packed_file_payload(pack_path, entry).map_err(|error| error.message)?;
    let script_text = std::str::from_utf8(&payload)
        .map_err(|error| format!("script is not valid UTF-8: {error}"))?;
    let pack_name = pack_display_name(index);

    Ok(find_add_listener_names(script_text)
        .into_iter()
        .map(|(value, position)| ScriptListenerValue {
            value,
            pack_file_name: entry.name.clone(),
            pack_name: pack_name.clone(),
            position,
        })
        .collect())
}

fn find_add_listener_names(script_text: &str) -> Vec<(String, usize)> {
    const NEEDLE: &str = "core:add_listener";

    let mut listeners = Vec::new();
    let mut search_from = 0;
    while let Some(relative_start) = script_text[search_from..].find(NEEDLE) {
        let start = search_from + relative_start;
        search_from = start + NEEDLE.len();

        let Some((listener_name, next_cursor)) =
            parse_add_listener_name_after_needle(script_text, search_from)
        else {
            continue;
        };
        search_from = next_cursor;
        listeners.push((listener_name, start));
    }

    listeners
}

fn parse_add_listener_name_after_needle(
    script_text: &str,
    cursor: usize,
) -> Option<(String, usize)> {
    let mut cursor = skip_ascii_whitespace(script_text, cursor);
    cursor = consume_byte(script_text, cursor, b'(')?;
    cursor = skip_ascii_whitespace(script_text, cursor);
    let quote = *script_text.as_bytes().get(cursor)?;
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    cursor += 1;
    cursor = skip_ascii_whitespace(script_text, cursor);
    let value_start = cursor;
    while let Some(byte) = script_text.as_bytes().get(cursor) {
        if *byte == quote {
            let value = script_text[value_start..cursor].trim_end().to_string();
            cursor += 1;
            cursor = skip_ascii_whitespace(script_text, cursor);
            cursor = consume_byte(script_text, cursor, b',')?;
            return (!value.is_empty()).then_some((value, cursor));
        }
        cursor += 1;
    }

    None
}

fn skip_ascii_whitespace(script_text: &str, mut cursor: usize) -> usize {
    while script_text
        .as_bytes()
        .get(cursor)
        .is_some_and(u8::is_ascii_whitespace)
    {
        cursor += 1;
    }
    cursor
}

fn consume_byte(script_text: &str, cursor: usize, expected: u8) -> Option<usize> {
    script_text
        .as_bytes()
        .get(cursor)
        .is_some_and(|byte| *byte == expected)
        .then_some(cursor + 1)
}

fn find_pack_table_collisions(tables: &[DecodedDbTable]) -> Vec<PackTableCollision> {
    let mut collisions = Vec::new();
    for first_index in 0..tables.len() {
        for second_index in first_index + 1..tables.len() {
            let first = &tables[first_index];
            let second = &tables[second_index];
            if first.pack_name == second.pack_name
                || first.db_name != second.db_name
                || first.key_name != second.key_name
                || first.key_name.is_none()
            {
                continue;
            }
            let key_name = first.key_name.as_ref().expect("checked above");

            let second_values = second.key_values.iter().collect::<BTreeSet<_>>();
            for first_value in &first.key_values {
                if !second_values.contains(first_value) {
                    continue;
                }

                collisions.push(PackTableCollision {
                    first_pack_name: first.pack_name.clone(),
                    second_pack_name: second.pack_name.clone(),
                    file_name: first.file_name.clone(),
                    second_file_name: second.file_name.clone(),
                    key: key_name.clone(),
                    value: first_value.clone(),
                });
                collisions.push(PackTableCollision {
                    first_pack_name: second.pack_name.clone(),
                    second_pack_name: first.pack_name.clone(),
                    file_name: second.file_name.clone(),
                    second_file_name: first.file_name.clone(),
                    key: key_name.clone(),
                    value: first_value.clone(),
                });
            }
        }
    }

    collisions
}

fn find_script_listener_collisions(
    listeners: &[ScriptListenerValue],
) -> Vec<ScriptListenerCollision> {
    let mut pack_to_scripts: BTreeMap<String, BTreeMap<String, Vec<ScriptListenerValue>>> =
        BTreeMap::new();

    for listener in listeners {
        pack_to_scripts
            .entry(listener.pack_name.clone())
            .or_default()
            .entry(listener.pack_file_name.clone())
            .or_default()
            .push(listener.clone());
    }

    for scripts in pack_to_scripts.values_mut() {
        for script_listeners in scripts.values_mut() {
            script_listeners.sort_by(|left, right| {
                left.value
                    .cmp(&right.value)
                    .then_with(|| left.position.cmp(&right.position))
            });
        }
    }

    let pack_names = pack_to_scripts.keys().cloned().collect::<Vec<_>>();
    let mut reported = BTreeSet::new();
    let mut collisions = Vec::new();

    for (pack_index, pack_name) in pack_names.iter().enumerate() {
        let Some(scripts_in_pack) = pack_to_scripts.get(pack_name) else {
            continue;
        };
        for (script_name, script_listeners) in scripts_in_pack {
            for collision in
                same_pack_script_listener_collisions(script_name, script_listeners, pack_name)
            {
                if reported.insert(script_listener_collision_key(&collision)) {
                    collisions.push(collision);
                }
            }

            for second_pack_name in pack_names.iter().skip(pack_index + 1) {
                let Some(script_listeners_in_second_pack) = pack_to_scripts
                    .get(second_pack_name)
                    .and_then(|scripts| scripts.get(script_name))
                else {
                    continue;
                };

                for collision in cross_pack_script_listener_collisions(
                    script_name,
                    script_listeners,
                    pack_name,
                    script_listeners_in_second_pack,
                    second_pack_name,
                ) {
                    if reported.insert(script_listener_collision_key(&collision)) {
                        collisions.push(collision);
                    }
                }
            }
        }
    }

    collisions
}

fn same_pack_script_listener_collisions(
    script_name: &str,
    listeners: &[ScriptListenerValue],
    pack_name: &str,
) -> Vec<ScriptListenerCollision> {
    listeners
        .windows(2)
        .filter_map(|pair| {
            let [first, second] = pair else {
                return None;
            };
            (first.value == second.value).then(|| ScriptListenerCollision {
                pack_file_name: script_name.to_string(),
                value: first.clone(),
                value_two: second.clone(),
                first_pack_name: pack_name.to_string(),
                second_pack_name: None,
            })
        })
        .collect()
}

fn cross_pack_script_listener_collisions(
    script_name: &str,
    first_listeners: &[ScriptListenerValue],
    first_pack_name: &str,
    second_listeners: &[ScriptListenerValue],
    second_pack_name: &str,
) -> Vec<ScriptListenerCollision> {
    let first_by_value = first_listeners
        .iter()
        .map(|listener| (listener.value.clone(), listener))
        .collect::<BTreeMap<_, _>>();
    let second_by_value = second_listeners
        .iter()
        .map(|listener| (listener.value.clone(), listener))
        .collect::<BTreeMap<_, _>>();

    first_by_value
        .into_iter()
        .filter_map(|(value, first)| {
            let second = second_by_value.get(&value)?;
            Some(ScriptListenerCollision {
                pack_file_name: script_name.to_string(),
                value: first.clone(),
                value_two: (*second).clone(),
                first_pack_name: first_pack_name.to_string(),
                second_pack_name: Some(second_pack_name.to_string()),
            })
        })
        .collect()
}

fn script_listener_collision_key(
    collision: &ScriptListenerCollision,
) -> (String, String, String, String, Option<String>, usize, usize) {
    (
        collision.pack_file_name.clone(),
        collision.value.value.clone(),
        collision.value.pack_name.clone(),
        collision.first_pack_name.clone(),
        collision.second_pack_name.clone(),
        collision.value.position,
        collision.value_two.position,
    )
}

fn read_file_references(
    index: &PackIndex,
    entry: &PackFileIndexEntry,
    pack_path: &str,
) -> Result<Vec<FileToFileReference>, String> {
    let payload = read_packed_file_payload(pack_path, entry).map_err(|error| error.message)?;
    let text = std::str::from_utf8(&payload)
        .map_err(|error| format!("XML-like file is not valid UTF-8: {error}"))?;
    let referenced_files = extract_referenced_files(entry, text);
    if referenced_files.is_empty() {
        return Ok(Vec::new());
    }

    let pack_file_names = index
        .files
        .iter()
        .map(|file| normalize_packed_file_reference(&file.name))
        .collect::<BTreeSet<_>>();
    let pack_name = pack_display_name(index);
    let mut reported = BTreeSet::new();
    let mut references = Vec::new();

    for reference in referenced_files {
        let reference = normalize_packed_file_reference(&reference);
        if reference.is_empty() || pack_file_names.contains(&reference) {
            continue;
        }

        let report_key = (pack_name.clone(), entry.name.clone(), reference.clone());
        if !reported.insert(report_key.clone()) {
            continue;
        }

        references.push(FileToFileReference {
            reference: report_key.2,
            pack_name: report_key.0,
            pack_file_name: report_key.1,
        });
    }

    Ok(references)
}

fn extract_referenced_files(entry: &PackFileIndexEntry, text: &str) -> Vec<String> {
    let lower_name = entry.name.to_ascii_lowercase();
    if lower_name.ends_with(".variantmeshdefinition") {
        return extract_xml_attribute_values(text, &["model", "definition"]);
    }
    if lower_name.ends_with(".wsmodel") {
        return extract_xml_tag_values(text, &["material", "geometry"]);
    }
    if lower_name.ends_with(".xml.material") {
        return extract_xml_tag_values(text, &["shader", "source"])
            .into_iter()
            .filter(|value| !should_ignore_material_reference(value))
            .collect();
    }

    Vec::new()
}

fn extract_xml_attribute_values(text: &str, names: &[&str]) -> Vec<String> {
    let mut values = Vec::new();
    for name in names {
        values.extend(extract_one_xml_attribute_values(text, name));
    }
    values
}

fn extract_one_xml_attribute_values(text: &str, name: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut search_from = 0;
    while let Some(relative_index) = text[search_from..].find(name) {
        let name_start = search_from + relative_index;
        let name_end = name_start + name.len();
        search_from = name_end;

        if !is_xml_name_boundary(text, name_start.saturating_sub(1))
            || !is_xml_name_boundary(text, name_end)
        {
            continue;
        }

        let mut cursor = skip_ascii_whitespace(text, name_end);
        let Some(next_cursor) = consume_byte(text, cursor, b'=') else {
            continue;
        };
        cursor = skip_ascii_whitespace(text, next_cursor);
        let Some((value, next_cursor)) = read_quoted_xml_value(text, cursor) else {
            continue;
        };
        search_from = next_cursor;
        values.push(value);
    }

    values
}

fn extract_xml_tag_values(text: &str, names: &[&str]) -> Vec<String> {
    let mut values = Vec::new();
    for name in names {
        values.extend(extract_one_xml_tag_values(text, name));
    }
    values
}

fn extract_one_xml_tag_values(text: &str, name: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut search_from = 0;
    let open_prefix = format!("<{name}");
    let close_tag = format!("</{name}>");

    while let Some(relative_open) = text[search_from..].find(&open_prefix) {
        let open_start = search_from + relative_open;
        let Some(open_end_relative) = text[open_start..].find('>') else {
            break;
        };
        let value_start = open_start + open_end_relative + 1;
        let Some(close_relative) = text[value_start..].find(&close_tag) else {
            search_from = value_start;
            continue;
        };
        let value_end = value_start + close_relative;
        let value = text[value_start..value_end].trim();
        if !value.is_empty() {
            values.push(value.to_string());
        }
        search_from = value_end + close_tag.len();
    }

    values
}

fn read_quoted_xml_value(text: &str, cursor: usize) -> Option<(String, usize)> {
    let quote = *text.as_bytes().get(cursor)?;
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    let value_start = cursor + 1;
    let mut cursor = value_start;
    while let Some(byte) = text.as_bytes().get(cursor) {
        if *byte == quote {
            return Some((text[value_start..cursor].to_string(), cursor + 1));
        }
        cursor += 1;
    }

    None
}

fn is_xml_name_boundary(text: &str, cursor: usize) -> bool {
    text.as_bytes()
        .get(cursor)
        .is_none_or(|byte| !byte.is_ascii_alphanumeric() && !matches!(*byte, b'_' | b'-' | b':'))
}

fn should_ignore_material_reference(value: &str) -> bool {
    let normalized = normalize_packed_file_reference(value);
    matches!(
        normalized.as_str(),
        "commontextures\\default_black.dds" | "mask_path"
    ) || normalized.ends_with("test_mask.dds")
}

fn normalize_packed_file_reference(value: &str) -> String {
    value.trim().replace('/', "\\").to_ascii_lowercase()
}

fn find_missing_file_references(
    indexes: &[PackIndex],
    references: &[FileToFileReference],
) -> Vec<FileToFileReference> {
    let pack_to_files = indexes
        .iter()
        .map(|index| {
            (
                pack_display_name(index),
                index
                    .files
                    .iter()
                    .map(|file| normalize_packed_file_reference(&file.name))
                    .collect::<BTreeSet<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();

    let mut reported = BTreeSet::new();
    let mut missing = Vec::new();
    for reference in references {
        let found_in_other_pack = pack_to_files
            .iter()
            .filter(|(pack_name, _)| *pack_name != &reference.pack_name)
            .any(|(_, files)| files.contains(&reference.reference));
        if found_in_other_pack {
            continue;
        }

        let report_key = (
            reference.pack_name.clone(),
            reference.pack_file_name.clone(),
            reference.reference.clone(),
        );
        if !reported.insert(report_key.clone()) {
            continue;
        }

        missing.push(FileToFileReference {
            pack_name: report_key.0,
            pack_file_name: report_key.1,
            reference: report_key.2,
        });
    }

    missing
}

fn find_missing_db_references(tables: &[DecodedDbTable]) -> Vec<MissingDbReference> {
    let optional_nontext_fields = optional_nontext_fields();
    let additional_field_keys = additional_field_keys();
    let mut provided_values: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();

    for table in tables {
        for (field_name, values) in &table.field_values {
            provided_values
                .entry((table.db_name.clone(), field_name.clone()))
                .or_default()
                .extend(values.iter().filter(|value| !value.is_empty()).cloned());
        }
    }

    let mut reported = BTreeSet::new();
    let mut missing = Vec::new();
    for table in tables {
        for reference in &table.references {
            let target_key = (
                reference.target_db_name.clone(),
                reference.target_field_name.clone(),
            );
            let target_values = provided_values.get(&target_key);

            for value in reference.values.iter().filter(|value| !value.is_empty()) {
                if target_values.is_some_and(|values| values.contains(value)) {
                    continue;
                }
                if should_skip_missing_db_reference(
                    &optional_nontext_fields,
                    &additional_field_keys,
                    &table.db_name,
                    &reference.origin_field_name,
                    &reference.target_db_name,
                    &reference.target_field_name,
                    value,
                ) {
                    continue;
                }

                let report_key = (
                    table.pack_name.clone(),
                    table.file_name.clone(),
                    table.db_name.clone(),
                    reference.origin_field_name.clone(),
                    reference.target_db_name.clone(),
                    reference.target_field_name.clone(),
                    value.clone(),
                );
                if !reported.insert(report_key.clone()) {
                    continue;
                }

                missing.push(MissingDbReference {
                    pack_name: report_key.0,
                    origin_file_name: report_key.1,
                    origin_db_name: report_key.2,
                    origin_field_name: report_key.3,
                    target_db_name: report_key.4,
                    target_field_name: report_key.5,
                    value: report_key.6,
                });
            }
        }
    }

    missing
}

fn find_unique_id_collisions(tables: &[DecodedDbTable]) -> Vec<UniqueIdCollision> {
    let mut pack_to_tables: BTreeMap<String, BTreeMap<String, Vec<UniqueIdValue>>> =
        BTreeMap::new();

    for table in tables {
        let Some(field_name) = numeric_id_field_for_table(&table.db_name) else {
            continue;
        };
        let Some(field_index) = table.field_names.iter().position(|name| name == field_name) else {
            continue;
        };

        for row in &table.row_values {
            let Some(value) = row.get(field_index) else {
                continue;
            };

            pack_to_tables
                .entry(table.pack_name.clone())
                .or_default()
                .entry(table.db_name.clone())
                .or_default()
                .push(UniqueIdValue {
                    value: value.clone(),
                    pack_file_name: table.db_subname.clone(),
                    table_row: row.clone(),
                    pack_name: table.pack_name.clone(),
                });
        }
    }

    for tables in pack_to_tables.values_mut() {
        for values in tables.values_mut() {
            values.sort_by(|left, right| {
                left.value
                    .cmp(&right.value)
                    .then_with(|| left.pack_file_name.cmp(&right.pack_file_name))
                    .then_with(|| left.table_row.cmp(&right.table_row))
            });
        }
    }

    let pack_names = pack_to_tables.keys().cloned().collect::<Vec<_>>();
    let mut reported = BTreeSet::new();
    let mut collisions = Vec::new();

    for (pack_index, pack_name) in pack_names.iter().enumerate() {
        let Some(tables_in_pack) = pack_to_tables.get(pack_name) else {
            continue;
        };

        for (table_name, values) in tables_in_pack {
            if pack_name != "db.pack" || table_name != "technologies_tables" {
                for duplicate in same_pack_duplicate_unique_ids(table_name, values, pack_name) {
                    if reported.insert(unique_id_collision_key(&duplicate)) {
                        collisions.push(duplicate);
                    }
                }
            }

            for second_pack_name in pack_names.iter().skip(pack_index + 1) {
                let Some(values_in_second_pack) = pack_to_tables
                    .get(second_pack_name)
                    .and_then(|tables| tables.get(table_name))
                else {
                    continue;
                };

                for collision in cross_pack_unique_id_collisions(
                    table_name,
                    values,
                    pack_name,
                    values_in_second_pack,
                    second_pack_name,
                ) {
                    if reported.insert(unique_id_collision_key(&collision)) {
                        collisions.push(collision);
                    }
                }
            }
        }
    }

    collisions
}

fn same_pack_duplicate_unique_ids(
    table_name: &str,
    values: &[UniqueIdValue],
    pack_name: &str,
) -> Vec<UniqueIdCollision> {
    let Some(field_name) = numeric_id_field_for_table(table_name) else {
        return Vec::new();
    };

    values
        .windows(2)
        .filter_map(|pair| {
            let [first, second] = pair else {
                return None;
            };
            (first.value == second.value).then(|| UniqueIdCollision {
                table_name: table_name.to_string(),
                field_name: field_name.to_string(),
                value: first.clone(),
                value_two: second.clone(),
                first_pack_name: pack_name.to_string(),
                second_pack_name: None,
            })
        })
        .collect()
}

fn cross_pack_unique_id_collisions(
    table_name: &str,
    first_values: &[UniqueIdValue],
    first_pack_name: &str,
    second_values: &[UniqueIdValue],
    second_pack_name: &str,
) -> Vec<UniqueIdCollision> {
    let Some(field_name) = numeric_id_field_for_table(table_name) else {
        return Vec::new();
    };
    let first_by_value = first_values
        .iter()
        .map(|value| (value.value.clone(), value))
        .collect::<BTreeMap<_, _>>();
    let second_by_value = second_values
        .iter()
        .map(|value| (value.value.clone(), value))
        .collect::<BTreeMap<_, _>>();

    first_by_value
        .into_iter()
        .filter_map(|(value, first)| {
            let second = second_by_value.get(&value)?;
            Some(UniqueIdCollision {
                table_name: table_name.to_string(),
                field_name: field_name.to_string(),
                value: first.clone(),
                value_two: (*second).clone(),
                first_pack_name: first_pack_name.to_string(),
                second_pack_name: Some(second_pack_name.to_string()),
            })
        })
        .collect()
}

fn unique_id_collision_key(
    collision: &UniqueIdCollision,
) -> (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
) {
    (
        collision.table_name.clone(),
        collision.field_name.clone(),
        collision.value.value.clone(),
        collision.value.pack_file_name.clone(),
        collision.first_pack_name.clone(),
        collision.second_pack_name.clone(),
        collision.value_two.pack_file_name.clone(),
    )
}

fn numeric_id_field_for_table(table_name: &str) -> Option<&'static str> {
    match table_name {
        "warscape_animated_lod_tables"
        | "building_units_allowed_tables"
        | "mercenary_pool_to_groups_junctions_tables"
        | "campaign_building_level_factorial_effect_junctions_tables"
        | "campaign_agent_subtype_factorial_effect_junctions_tables" => Some("key"),
        "culture_settlement_occupation_options_tables"
        | "campaign_post_battle_captive_options_tables"
        | "faction_set_items_tables"
        | "campaign_character_arts_tables"
        | "cdir_events_mission_option_junctions_tables"
        | "cdir_events_mission_payloads_tables"
        | "cdir_events_incident_option_junctions_tables"
        | "cdir_events_incident_payloads_tables"
        | "cdir_events_dilemma_option_junctions_tables"
        | "cdir_events_dilemma_payloads_tables"
        | "armed_citizenry_units_to_unit_groups_junctions_tables"
        | "building_level_armed_citizenry_junctions_tables"
        | "slot_set_items_tables"
        | "names_tables"
        | "ritual_payload_spawn_mercenaries_tables"
        | "units_custom_battle_types_tables"
        | "building_chain_availabilities_tables"
        | "campaign_group_post_battle_casualty_resources_tables" => Some("id"),
        "technologies_tables" => Some("unique_index"),
        "army_special_abilities_tables" | "unit_special_abilities_tables" => Some("unique_id"),
        _ => None,
    }
}

fn should_skip_missing_db_reference(
    optional_nontext_fields: &BTreeMap<String, BTreeSet<String>>,
    additional_field_keys: &BTreeMap<String, BTreeMap<String, BTreeSet<String>>>,
    origin_db_name: &str,
    origin_field_name: &str,
    target_db_name: &str,
    target_field_name: &str,
    value: &str,
) -> bool {
    optional_nontext_fields
        .get(origin_db_name)
        .is_some_and(|fields| fields.contains(origin_field_name) && value == "0")
        || additional_field_keys
            .get(target_db_name)
            .and_then(|fields| fields.get(target_field_name))
            .is_some_and(|values| values.contains(value))
}

fn optional_nontext_fields() -> BTreeMap<String, BTreeSet<String>> {
    serde_json::from_str::<BTreeMap<String, Vec<String>>>(include_str!(
        "../../../schema/optional_nontext_fields.json"
    ))
    .unwrap_or_default()
    .into_iter()
    .map(|(table, fields)| (table, fields.into_iter().collect()))
    .collect()
}

fn additional_field_keys() -> BTreeMap<String, BTreeMap<String, BTreeSet<String>>> {
    serde_json::from_str::<BTreeMap<String, BTreeMap<String, Vec<String>>>>(include_str!(
        "../../../schema/additional_field_keys.json"
    ))
    .unwrap_or_default()
    .into_iter()
    .map(|(table, fields)| {
        (
            table,
            fields
                .into_iter()
                .map(|(field, values)| (field, values.into_iter().collect()))
                .collect(),
        )
    })
    .collect()
}

fn find_pack_file_collisions(indexes: &[PackIndex]) -> Vec<PackFileCollision> {
    let mut file_to_entries: BTreeMap<String, Vec<(&PackIndex, u64)>> = BTreeMap::new();

    for index in indexes {
        for file in &index.files {
            if file.name.ends_with(".rpfm_reserved") {
                continue;
            }
            file_to_entries
                .entry(file.name.clone())
                .or_default()
                .push((index, file.file_size));
        }
    }

    let mut collisions = Vec::new();
    for (file_name, entries) in file_to_entries {
        for first_index in 0..entries.len() {
            for second_index in first_index + 1..entries.len() {
                let (first_pack, first_size) = entries[first_index];
                let (second_pack, second_size) = entries[second_index];
                let first_pack_name = pack_display_name(first_pack);
                let second_pack_name = pack_display_name(second_pack);
                if first_pack_name == second_pack_name {
                    continue;
                }

                let are_same_size = first_size == second_size;
                collisions.push(PackFileCollision {
                    first_pack_name: first_pack_name.clone(),
                    second_pack_name: second_pack_name.clone(),
                    file_name: file_name.clone(),
                    are_same_size,
                });
                collisions.push(PackFileCollision {
                    first_pack_name: second_pack_name,
                    second_pack_name: first_pack_name,
                    file_name: file_name.clone(),
                    are_same_size,
                });
            }
        }
    }

    collisions
}

fn find_missing_dependency_packs(indexes: &[PackIndex]) -> Vec<MissingDependencyPack> {
    let enabled_pack_names = indexes
        .iter()
        .map(|index| pack_display_name(index).to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let mut missing = Vec::new();

    for index in indexes {
        let pack_name = pack_display_name(index);
        for dependency in &index.dependency_packs {
            if !enabled_pack_names.contains(&dependency.to_ascii_lowercase()) {
                missing.push(MissingDependencyPack {
                    pack_name: pack_name.clone(),
                    dependency_pack_name: dependency.clone(),
                });
            }
        }
    }

    missing
}

fn pack_display_name(index: &PackIndex) -> String {
    Path::new(&index.path)
        .file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| index.path.display().to_string(), str::to_string)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use crate::domain::{ModIdentity, ModRecord};
    use crate::pack::{PackFileIndexEntry, PackIndex, PackReadOptions};

    use super::{DecodedDbTable, analyze_enabled_mod_conflicts, analyze_pack_indexes};

    #[test]
    fn detects_directional_pack_file_collisions() {
        let first = pack_index(
            "first.pack",
            &[],
            &[
                file("db\\units_tables\\main", 10),
                file("ignored.rpfm_reserved", 1),
            ],
        );
        let second = pack_index(
            "second.pack",
            &[],
            &[
                file("db\\units_tables\\main", 12),
                file("script\\main.lua", 1),
            ],
        );

        let report = analyze_pack_indexes(&[first, second]);

        assert_eq!(report.pack_file_collisions.len(), 2);
        assert_eq!(
            report.pack_file_collisions[0].file_name,
            "db\\units_tables\\main"
        );
        assert!(!report.pack_file_collisions[0].are_same_size);
        assert_eq!(report.pack_file_collisions[0].first_pack_name, "first.pack");
        assert_eq!(
            report.pack_file_collisions[1].first_pack_name,
            "second.pack"
        );
    }

    #[test]
    fn detects_missing_dependency_packs_case_insensitively() {
        let first = pack_index("first.pack", &["Second.PACK", "missing.pack"], &[]);
        let second = pack_index("second.pack", &[], &[]);

        let report = analyze_pack_indexes(&[first, second]);

        assert_eq!(report.missing_dependency_packs.len(), 1);
        assert_eq!(
            report.missing_dependency_packs[0].dependency_pack_name,
            "missing.pack"
        );
    }

    #[test]
    fn records_read_errors_for_enabled_invalid_pack_paths() {
        let mods = vec![ModRecord {
            identity: ModIdentity::new("does-not-exist.pack", Option::<String>::None, "missing"),
            display_name: "missing".to_string(),
            enabled: true,
            always_enabled: false,
            hidden: false,
            categories: Vec::new(),
            tags: Vec::new(),
        }];

        let report = analyze_enabled_mod_conflicts(&mods, &PackReadOptions::default());

        assert_eq!(report.pack_read_errors.len(), 1);
        assert!(report.pack_file_collisions.is_empty());
    }

    #[test]
    fn detects_directional_db_table_key_collisions() {
        let first = DecodedDbTable {
            pack_name: "first.pack".to_string(),
            file_name: "db\\units_tables\\first".to_string(),
            db_subname: "first".to_string(),
            db_name: "units_tables".to_string(),
            key_name: Some("key".to_string()),
            key_values: vec!["shared".to_string(), "first-only".to_string()],
            field_names: vec!["key".to_string()],
            field_values: BTreeMap::from([(
                "key".to_string(),
                vec!["shared".to_string(), "first-only".to_string()],
            )]),
            row_values: vec![vec!["shared".to_string()], vec!["first-only".to_string()]],
            references: Vec::new(),
        };
        let second = DecodedDbTable {
            pack_name: "second.pack".to_string(),
            file_name: "db\\units_tables\\second".to_string(),
            db_subname: "second".to_string(),
            db_name: "units_tables".to_string(),
            key_name: Some("key".to_string()),
            key_values: vec!["shared".to_string(), "second-only".to_string()],
            field_names: vec!["key".to_string()],
            field_values: BTreeMap::from([(
                "key".to_string(),
                vec!["shared".to_string(), "second-only".to_string()],
            )]),
            row_values: vec![vec!["shared".to_string()], vec!["second-only".to_string()]],
            references: Vec::new(),
        };

        let collisions = super::find_pack_table_collisions(&[first, second]);

        assert_eq!(collisions.len(), 2);
        assert_eq!(collisions[0].first_pack_name, "first.pack");
        assert_eq!(collisions[0].second_pack_name, "second.pack");
        assert_eq!(collisions[0].key, "key");
        assert_eq!(collisions[0].value, "shared");
        assert_eq!(collisions[1].first_pack_name, "second.pack");
    }

    #[test]
    fn detects_missing_db_references() {
        let table = table_with_reference(
            "source.pack",
            "origin_tables",
            "target_id",
            "target_tables",
            "key",
            &["missing"],
        );

        let missing = super::find_missing_db_references(&[table]);

        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].pack_name, "source.pack");
        assert_eq!(missing[0].origin_db_name, "origin_tables");
        assert_eq!(missing[0].origin_field_name, "target_id");
        assert_eq!(missing[0].target_db_name, "target_tables");
        assert_eq!(missing[0].value, "missing");
    }

    #[test]
    fn does_not_report_db_reference_provided_by_another_table() {
        let referencing_table = table_with_reference(
            "source.pack",
            "origin_tables",
            "target_id",
            "target_tables",
            "key",
            &["provided"],
        );
        let provider_table =
            table_with_values("provider.pack", "target_tables", "key", &["provided"]);

        let missing = super::find_missing_db_references(&[referencing_table, provider_table]);

        assert!(missing.is_empty());
    }

    #[test]
    fn detects_same_pack_unique_id_collisions() {
        let table = table_with_values(
            "source.pack",
            "technologies_tables",
            "unique_index",
            &["7", "7"],
        );

        let collisions = super::find_unique_id_collisions(&[table]);

        assert_eq!(collisions.len(), 1);
        assert_eq!(collisions[0].first_pack_name, "source.pack");
        assert_eq!(collisions[0].second_pack_name, None);
        assert_eq!(collisions[0].table_name, "technologies_tables");
        assert_eq!(collisions[0].field_name, "unique_index");
        assert_eq!(collisions[0].value.value, "7");
    }

    #[test]
    fn detects_cross_pack_unique_id_collisions() {
        let first = table_with_values("first.pack", "names_tables", "id", &["10", "11"]);
        let second = table_with_values("second.pack", "names_tables", "id", &["10", "12"]);

        let collisions = super::find_unique_id_collisions(&[first, second]);

        assert_eq!(collisions.len(), 1);
        assert_eq!(collisions[0].first_pack_name, "first.pack");
        assert_eq!(
            collisions[0].second_pack_name.as_deref(),
            Some("second.pack")
        );
        assert_eq!(collisions[0].table_name, "names_tables");
        assert_eq!(collisions[0].value.value, "10");
        assert_eq!(collisions[0].value_two.value, "10");
    }

    #[test]
    fn parses_lua_add_listener_names_like_ts_regex() {
        let script = r#"
            core:add_listener( " FirstListener " , "Event", true, function() end, false )
            core:add_listener('SecondListener', "Event", true, function() end, false)
            core:add_listener("missing comma" "Event")
        "#;

        let listeners = super::find_add_listener_names(script);

        assert_eq!(listeners.len(), 2);
        assert_eq!(listeners[0].0, "FirstListener");
        assert_eq!(listeners[1].0, "SecondListener");
        assert_eq!(
            listeners[0].1,
            script.find("core:add_listener").expect("listener exists")
        );
    }

    #[test]
    fn detects_same_pack_script_listener_collisions() {
        let listeners = vec![
            script_listener("source.pack", "script\\main.lua", "SharedListener", 10),
            script_listener("source.pack", "script\\main.lua", "SharedListener", 40),
        ];

        let collisions = super::find_script_listener_collisions(&listeners);

        assert_eq!(collisions.len(), 1);
        assert_eq!(collisions[0].first_pack_name, "source.pack");
        assert_eq!(collisions[0].second_pack_name, None);
        assert_eq!(collisions[0].pack_file_name, "script\\main.lua");
        assert_eq!(collisions[0].value.value, "SharedListener");
    }

    #[test]
    fn detects_cross_pack_script_listener_collisions() {
        let listeners = vec![
            script_listener("first.pack", "script\\main.lua", "SharedListener", 10),
            script_listener("second.pack", "script\\main.lua", "SharedListener", 20),
            script_listener("second.pack", "script\\other.lua", "SharedListener", 30),
        ];

        let collisions = super::find_script_listener_collisions(&listeners);

        assert_eq!(collisions.len(), 1);
        assert_eq!(collisions[0].first_pack_name, "first.pack");
        assert_eq!(
            collisions[0].second_pack_name.as_deref(),
            Some("second.pack")
        );
        assert_eq!(collisions[0].pack_file_name, "script\\main.lua");
        assert_eq!(collisions[0].value_two.value, "SharedListener");
    }

    #[test]
    fn extracts_xml_like_file_references() {
        let vmd = file("variant.variantmeshdefinition", 1);
        let wsmodel = file("model.wsmodel", 1);
        let material = file("material.xml.material", 1);

        let vmd_refs = super::extract_referenced_files(
            &vmd,
            r#"<root model="models/a.mesh" definition='defs/b.xml' />"#,
        );
        let wsmodel_refs = super::extract_referenced_files(
            &wsmodel,
            "<root><material>materials/a.xml.material</material><geometry>geometry/a.wsmodel</geometry></root>",
        );
        let material_refs = super::extract_referenced_files(
            &material,
            "<root><shader>shaders/a.fx</shader><source>commontextures/default_black.dds</source><source>textures/test_mask.dds</source></root>",
        );

        assert_eq!(vmd_refs, vec!["models/a.mesh", "defs/b.xml"]);
        assert_eq!(
            wsmodel_refs,
            vec!["materials/a.xml.material", "geometry/a.wsmodel"]
        );
        assert_eq!(material_refs, vec!["shaders/a.fx"]);
    }

    #[test]
    fn reports_file_references_missing_from_enabled_pack_set() {
        let source = pack_index("source.pack", &[], &[file("source.wsmodel", 1)]);
        let provider = pack_index(
            "provider.pack",
            &[],
            &[file("materials\\provided.xml.material", 1)],
        );
        let references = vec![
            file_reference(
                "source.pack",
                "source.wsmodel",
                "materials/provided.xml.material",
            ),
            file_reference(
                "source.pack",
                "source.wsmodel",
                "materials/missing.xml.material",
            ),
        ];

        let missing = super::find_missing_file_references(&[source, provider], &references);

        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].reference, "materials\\missing.xml.material");
        assert_eq!(missing[0].pack_name, "source.pack");
    }

    #[test]
    fn skips_missing_references_from_ts_exception_lists() {
        let optional_zero = table_with_reference(
            "optional.pack",
            "factions_tables",
            "movie_death_event",
            "target_tables",
            "key",
            &["0"],
        );
        let additional_key = table_with_reference(
            "additional.pack",
            "origin_tables",
            "campaign_name",
            "campaigns_tables",
            "campaign_name",
            &["cr_oldworld"],
        );

        let missing = super::find_missing_db_references(&[optional_zero, additional_key]);

        assert!(missing.is_empty());
    }

    fn pack_index(
        path: &str,
        dependency_packs: &[&str],
        files: &[PackFileIndexEntry],
    ) -> PackIndex {
        PackIndex {
            path: PathBuf::from(path),
            magic: *b"PFH5",
            byte_mask: 3,
            is_movie: false,
            reference_file_count: 0,
            dependency_index_size: 0,
            packed_file_index_size: 0,
            header_buffer: [0xff, 0xff, 0xff, 0x7f],
            dependency_packs: dependency_packs
                .iter()
                .map(|dependency| (*dependency).to_string())
                .collect(),
            files: files.to_vec(),
        }
    }

    fn file(name: &str, file_size: u64) -> PackFileIndexEntry {
        PackFileIndexEntry {
            name: name.to_string(),
            file_size,
            start_pos: 0,
            is_compressed: false,
        }
    }

    fn table_with_values(
        pack_name: &str,
        db_name: &str,
        field_name: &str,
        values: &[&str],
    ) -> DecodedDbTable {
        DecodedDbTable {
            pack_name: pack_name.to_string(),
            file_name: format!("db\\{db_name}\\local"),
            db_subname: "local".to_string(),
            db_name: db_name.to_string(),
            key_name: Some(field_name.to_string()),
            key_values: values.iter().map(|value| (*value).to_string()).collect(),
            field_names: vec![field_name.to_string()],
            field_values: BTreeMap::from([(
                field_name.to_string(),
                values.iter().map(|value| (*value).to_string()).collect(),
            )]),
            row_values: values
                .iter()
                .map(|value| vec![(*value).to_string()])
                .collect(),
            references: Vec::new(),
        }
    }

    fn table_with_reference(
        pack_name: &str,
        db_name: &str,
        field_name: &str,
        target_db_name: &str,
        target_field_name: &str,
        values: &[&str],
    ) -> DecodedDbTable {
        DecodedDbTable {
            pack_name: pack_name.to_string(),
            file_name: format!("db\\{db_name}\\local"),
            db_subname: "local".to_string(),
            db_name: db_name.to_string(),
            key_name: None,
            key_values: Vec::new(),
            field_names: vec![field_name.to_string()],
            field_values: BTreeMap::from([(
                field_name.to_string(),
                values.iter().map(|value| (*value).to_string()).collect(),
            )]),
            row_values: values
                .iter()
                .map(|value| vec![(*value).to_string()])
                .collect(),
            references: vec![super::DecodedDbFieldReference {
                origin_field_name: field_name.to_string(),
                target_db_name: target_db_name.to_string(),
                target_field_name: target_field_name.to_string(),
                values: values.iter().map(|value| (*value).to_string()).collect(),
            }],
        }
    }

    fn script_listener(
        pack_name: &str,
        pack_file_name: &str,
        value: &str,
        position: usize,
    ) -> super::ScriptListenerValue {
        super::ScriptListenerValue {
            value: value.to_string(),
            pack_file_name: pack_file_name.to_string(),
            pack_name: pack_name.to_string(),
            position,
        }
    }

    fn file_reference(
        pack_name: &str,
        pack_file_name: &str,
        reference: &str,
    ) -> super::FileToFileReference {
        super::FileToFileReference {
            reference: super::normalize_packed_file_reference(reference),
            pack_name: pack_name.to_string(),
            pack_file_name: pack_file_name.to_string(),
        }
    }
}
