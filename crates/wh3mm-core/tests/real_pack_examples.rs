//! Real Steam workshop pack coverage.
//!
//! These fixtures are intentionally small workshop packs copied from a real
//! Windows Steam install. They catch format assumptions that synthetic packs
//! cannot.

use std::path::{Path, PathBuf};

use wh3mm_core::{
    ModIdentity, ModRecord, PackFileMetadata, PackReadOptions,
    analyze_enabled_mod_conflicts_with_schema, load_schema_file, read_db_rows_from_pack,
    read_pack_contents_lossy, read_pack_index, read_packed_file_metadata, resolve_table_schema,
};

const REAL_PACK_EXAMPLES: &[&str] = &[
    "Tzeentch_Tech-Tree_Decky.pack",
    "Chasslo_Landmarks_IEE.pack",
    "SIEGE_OVERHAUL.pack",
];

#[test]
fn reads_real_steam_pack_indexes_and_lossy_metadata() {
    for pack_path in real_pack_paths() {
        let index = read_pack_index(&pack_path, &PackReadOptions::default())
            .unwrap_or_else(|error| panic!("failed to read index for {pack_path:?}: {error:?}"));
        assert!(
            !index.files.is_empty(),
            "expected real pack to contain files: {pack_path:?}"
        );

        let contents = read_pack_contents_lossy(&pack_path, &PackReadOptions::default())
            .unwrap_or_else(|error| {
                panic!("failed to read lossy contents for {pack_path:?}: {error:?}")
            });

        assert_eq!(
            contents.index.files.len(),
            contents.metadata.len(),
            "metadata should align with index rows for {pack_path:?}"
        );
        assert!(
            contents
                .metadata
                .iter()
                .any(|metadata| matches!(metadata, PackFileMetadata::DbTable(_))),
            "expected at least one DB table in {pack_path:?}"
        );
    }
}

#[test]
fn real_steam_db_metadata_is_decodable_without_lossy_fallback() {
    for pack_path in real_pack_paths() {
        let index = read_pack_index(&pack_path, &PackReadOptions::default())
            .unwrap_or_else(|error| panic!("failed to read index for {pack_path:?}: {error:?}"));
        let db_entries = index
            .files
            .iter()
            .filter(|entry| matches!(entry.kind(), wh3mm_core::PackFileKind::DbTable { .. }));

        for entry in db_entries {
            let metadata = read_packed_file_metadata(&pack_path, entry).unwrap_or_else(|error| {
                panic!(
                    "failed to read DB metadata for {} in {pack_path:?}: {error:?}",
                    entry.name
                )
            });
            assert!(
                matches!(metadata, PackFileMetadata::DbTable(_)),
                "expected DB metadata for {} in {pack_path:?}",
                entry.name
            );
        }
    }
}

#[test]
fn previews_first_schema_resolvable_db_table_in_real_steam_packs() {
    let schema = load_schema_file(real_schema_path()).expect("failed to load WH3 schema");

    for pack_path in real_pack_paths() {
        let contents = read_pack_contents_lossy(&pack_path, &PackReadOptions::default())
            .unwrap_or_else(|error| {
                panic!("failed to read lossy contents for {pack_path:?}: {error:?}")
            });

        let mut decoded_table = None;
        for (entry, metadata) in contents.index.files.iter().zip(contents.metadata.iter()) {
            let PackFileMetadata::DbTable(metadata) = metadata else {
                continue;
            };
            let Some(selected_schema) = resolve_table_schema(&schema, metadata) else {
                continue;
            };

            let rows = read_db_rows_from_pack(&pack_path, entry, &selected_schema.fields)
                .unwrap_or_else(|error| {
                    panic!(
                        "failed to decode rows for {} in {pack_path:?}: {error:?}",
                        metadata.name
                    )
                });
            assert_eq!(
                rows.rows.len(),
                usize::try_from(metadata.entry_count).expect("entry count should fit usize"),
                "decoded row count should match metadata for {} in {pack_path:?}",
                metadata.name
            );
            decoded_table = Some(metadata.name.clone());
            break;
        }

        assert!(
            decoded_table.is_some(),
            "expected at least one schema-resolvable DB table in {pack_path:?}"
        );
    }
}

#[test]
fn analyzes_real_steam_packs_with_schema_backed_compatibility() {
    let schema = load_schema_file(real_schema_path()).expect("failed to load WH3 schema");
    let mods = real_pack_paths()
        .into_iter()
        .map(|pack_path| {
            let file_name = pack_path
                .file_name()
                .and_then(|file_name| file_name.to_str())
                .expect("real pack fixture should have UTF-8 file name")
                .to_string();
            ModRecord {
                identity: ModIdentity::new(
                    pack_path.to_string_lossy().into_owned(),
                    Option::<String>::None,
                    file_name.clone(),
                ),
                display_name: file_name,
                enabled: true,
                always_enabled: false,
                hidden: false,
                categories: Vec::new(),
                tags: vec!["real-steam-fixture".to_string()],
            }
        })
        .collect::<Vec<_>>();

    let report =
        analyze_enabled_mod_conflicts_with_schema(&mods, &PackReadOptions::default(), &schema);

    assert!(
        report.pack_read_errors.is_empty(),
        "real Steam packs should all be readable: {:?}",
        report.pack_read_errors
    );
    assert!(
        report.decoded_db_table_count >= REAL_PACK_EXAMPLES.len(),
        "expected schema-backed compat to decode DB tables from the real packs, got report: {report:?}"
    );
}

fn real_pack_paths() -> Vec<PathBuf> {
    REAL_PACK_EXAMPLES
        .iter()
        .map(|file_name| real_pack_root().join(file_name))
        .inspect(|path| assert_real_pack_exists(path))
        .collect()
}

fn real_pack_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../pack_examples_from_steam")
}

fn real_schema_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/schema_wh3.json.zst")
}

fn assert_real_pack_exists(path: &Path) {
    assert!(
        path.exists(),
        "missing real pack fixture: {path:?}. Expected pack_examples_from_steam to be present at repo root"
    );
}
