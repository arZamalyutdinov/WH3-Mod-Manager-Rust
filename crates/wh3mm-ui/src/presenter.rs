//! Pure functions that project core state into UI view models.

use std::cmp::Ordering;

use wh3mm_core::{
    AppState, DbPrimitiveValue, DbRows, DbTableMetadata, DbVersionSchema, PackContents,
    PackFileIndexEntry, PackFileKind, PackFileMetadata, PackIndex, WhmmFlowFileSummary,
    WhmmFlowOptionSummary, WhmmFlowPackSummary, WorkshopModData,
};

use crate::view_model::{
    AppViewModel, DbTableColumnViewModel, DbTablePreviewViewModel, DbTableRowViewModel,
    ModRowViewModel, ModSortColumn, ModSortSpec, PackFileRowViewModel, PackFlowErrorViewModel,
    PackFlowFileViewModel, PackFlowOptionViewModel, PackFlowSummaryViewModel, PackViewModel,
    SortDirection,
};

/// Builds the main-window view model from core state.
#[must_use]
pub fn build_app_view_model(state: &AppState) -> AppViewModel {
    AppViewModel {
        title: "WH3 Mod Manager".to_string(),
        mods: build_mod_archive_rows(state, &[], ModSortSpec::default()),
        busy: false,
        status_message: None,
        selected_pack: None,
    }
}

/// Builds enriched, sorted archive rows without mutating core mod order.
#[must_use]
pub fn build_mod_archive_rows(
    state: &AppState,
    workshop_metadata: &[WorkshopModData],
    sort: ModSortSpec,
) -> Vec<ModRowViewModel> {
    let mut rows = state
        .mods
        .iter()
        .enumerate()
        .map(|(index, mod_record)| {
            build_mod_row_view_model(mod_record, index + 1, workshop_metadata)
        })
        .collect::<Vec<_>>();
    sort_mod_archive_rows(&mut rows, sort);
    rows
}

/// Sorts archive rows as a presentation-only projection.
pub fn sort_mod_archive_rows(rows: &mut [ModRowViewModel], sort: ModSortSpec) {
    rows.sort_by(|left, right| {
        let primary = match sort.column {
            ModSortColumn::Order => {
                compare_values(&left.load_order, &right.load_order, sort.direction)
            }
            ModSortColumn::Status => compare_values(
                &usize::from(!left.enabled),
                &usize::from(!right.enabled),
                sort.direction,
            ),
            ModSortColumn::Name => {
                compare_text(&left.display_name, &right.display_name, sort.direction)
            }
            ModSortColumn::Author => compare_optional_text(
                left.author.as_deref(),
                right.author.as_deref(),
                sort.direction,
            ),
            ModSortColumn::Updated => compare_optional_values(
                left.updated_ms.as_ref(),
                right.updated_ms.as_ref(),
                sort.direction,
            ),
        };

        primary
            .then_with(|| left.load_order.cmp(&right.load_order))
            .then_with(|| compare_text(&left.pack_name, &right.pack_name, SortDirection::Ascending))
            .then_with(|| left.subtitle.cmp(&right.subtitle))
    });
}

/// Builds a selected-pack view model from core parser output.
#[must_use]
pub fn build_pack_view_model(
    pack_index: &PackIndex,
    metadata: &[PackFileMetadata],
) -> PackViewModel {
    PackViewModel {
        path: pack_index.path.display().to_string(),
        magic: String::from_utf8_lossy(&pack_index.magic).into_owned(),
        is_movie: pack_index.is_movie,
        dependency_packs: pack_index.dependency_packs.clone(),
        files: pack_index
            .files
            .iter()
            .map(|entry| build_pack_file_row_view_model(entry, metadata_for_entry(entry, metadata)))
            .collect(),
        table_preview: None,
        flow_summary: None,
    }
}

/// Builds a selected-pack view model from a core pack-content snapshot.
#[must_use]
pub fn build_pack_contents_view_model(contents: &PackContents) -> PackViewModel {
    build_pack_view_model(&contents.index, &contents.metadata)
}

/// Builds a toolkit-neutral DB table preview from decoded core rows.
#[must_use]
pub fn build_db_table_preview_view_model(
    metadata: &DbTableMetadata,
    schema: &DbVersionSchema,
    rows: &DbRows,
    max_rows: usize,
) -> DbTablePreviewViewModel {
    DbTablePreviewViewModel {
        title: format!("{} / {}", metadata.db_name, metadata.db_subname),
        source_name: metadata.name.clone(),
        version_label: metadata
            .version
            .map_or_else(|| "auto".to_string(), |version| version.to_string()),
        row_count_label: format!("{} rows", rows.rows.len()),
        columns: schema
            .fields
            .iter()
            .map(|field| DbTableColumnViewModel {
                name: field.name.clone(),
                is_key: field.is_key,
            })
            .collect(),
        rows: rows
            .rows
            .iter()
            .take(max_rows)
            .enumerate()
            .map(|(row_index, row)| DbTableRowViewModel {
                key: format!("row-{row_index}"),
                cells: row
                    .iter()
                    .map(|cell| format_db_value(&cell.value))
                    .collect(),
            })
            .collect(),
    }
}

/// Builds a WH3MM user-flow summary view model.
#[must_use]
pub fn build_pack_flow_summary_view_model(
    summary: &WhmmFlowPackSummary,
) -> Option<PackFlowSummaryViewModel> {
    if summary.files.is_empty() && summary.read_errors.is_empty() {
        return None;
    }

    Some(PackFlowSummaryViewModel {
        file_count_label: format!(
            "{} flow file{}",
            summary.files.len(),
            plural_suffix(summary.files.len())
        ),
        read_error_count_label: format!(
            "{} read error{}",
            summary.read_errors.len(),
            plural_suffix(summary.read_errors.len())
        ),
        files: summary
            .files
            .iter()
            .map(build_pack_flow_file_view_model)
            .collect(),
        read_errors: summary
            .read_errors
            .iter()
            .map(|error| PackFlowErrorViewModel {
                name: error.name.clone(),
                message: error.message.clone(),
            })
            .collect(),
    })
}

fn build_pack_flow_file_view_model(file: &WhmmFlowFileSummary) -> PackFlowFileViewModel {
    PackFlowFileViewModel {
        name: file.name.clone(),
        detail_label: format!(
            "{} node{} / {} connection{} / {} option{}",
            file.node_count,
            plural_suffix(file.node_count),
            file.connection_count,
            plural_suffix(file.connection_count),
            file.option_count,
            plural_suffix(file.option_count)
        ),
        graph_label: graph_toggle_label(file),
        options: file
            .options
            .iter()
            .map(build_pack_flow_option_view_model)
            .collect(),
    }
}

fn build_pack_flow_option_view_model(option: &WhmmFlowOptionSummary) -> PackFlowOptionViewModel {
    PackFlowOptionViewModel {
        id: option.id.clone(),
        label: format!("{} ({})", option.name, option.kind),
        default_value_label: option
            .default_value
            .as_ref()
            .map(|value| format!("default {value}")),
    }
}

fn graph_toggle_label(file: &WhmmFlowFileSummary) -> String {
    if !file.has_graph_enable_toggle {
        return "no user toggle".to_string();
    }

    if file.graph_starts_enabled {
        "user toggle, default on".to_string()
    } else {
        "user toggle, default off".to_string()
    }
}

fn build_mod_row_view_model(
    mod_record: &wh3mm_core::ModRecord,
    load_order: usize,
    workshop_metadata: &[WorkshopModData],
) -> ModRowViewModel {
    let metadata = mod_record
        .identity
        .workshop_id
        .as_deref()
        .and_then(|workshop_id| {
            workshop_metadata
                .iter()
                .find(|metadata| metadata.workshop_id == workshop_id)
        });
    let pack_name = mod_record
        .identity
        .path
        .rsplit(['\\', '/'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(&mod_record.identity.name)
        .to_string();
    let display_name = metadata
        .map(|metadata| metadata.title.trim())
        .filter(|title| !title.is_empty())
        .unwrap_or(&mod_record.display_name)
        .to_string();
    let author = metadata
        .map(|metadata| metadata.author.trim())
        .filter(|author| !author.is_empty())
        .map(str::to_string);
    let description = metadata
        .map(|metadata| metadata.description.trim())
        .filter(|description| !description.is_empty())
        .map(str::to_string);
    let mut tags = mod_record.tags.clone();
    if let Some(metadata) = metadata {
        for tag in &metadata.tags {
            if !tag.trim().is_empty() && !tags.iter().any(|existing| existing == tag) {
                tags.push(tag.clone());
            }
        }
    }

    ModRowViewModel {
        key: mod_record.identity.stable_key(),
        display_name,
        load_order,
        pack_name,
        source: mod_record.source,
        workshop_id: mod_record.identity.workshop_id.clone(),
        thumbnail_path: mod_record.thumbnail_path.clone(),
        author,
        updated_ms: metadata
            .and_then(|metadata| (metadata.last_changed_ms > 0).then_some(metadata.last_changed_ms))
            .or(mod_record.local_modified_ms),
        description,
        subtitle: mod_record.identity.path.clone(),
        enabled: mod_record.effectively_enabled(),
        locked: mod_record.always_enabled,
        hidden: mod_record.hidden,
        categories: mod_record.categories.clone(),
        tags,
    }
}

fn compare_values<T: Ord>(left: &T, right: &T, direction: SortDirection) -> Ordering {
    match direction {
        SortDirection::Ascending => left.cmp(right),
        SortDirection::Descending => right.cmp(left),
    }
}

fn compare_text(left: &str, right: &str, direction: SortDirection) -> Ordering {
    compare_values(&left.to_lowercase(), &right.to_lowercase(), direction)
}

fn compare_optional_text(
    left: Option<&str>,
    right: Option<&str>,
    direction: SortDirection,
) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => compare_text(left, right, direction),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn compare_optional_values<T: Ord>(
    left: Option<&T>,
    right: Option<&T>,
    direction: SortDirection,
) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => compare_values(left, right, direction),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn build_pack_file_row_view_model(
    entry: &PackFileIndexEntry,
    metadata: Option<&PackFileMetadata>,
) -> PackFileRowViewModel {
    PackFileRowViewModel {
        key: entry.name.clone(),
        name: entry.name.clone(),
        kind: kind_label(&entry.kind()).to_string(),
        size_label: format!("{} B", entry.file_size),
        offset_label: format!("@{}", entry.start_pos),
        compression_label: if entry.is_compressed {
            "zstd".to_string()
        } else {
            "plain".to_string()
        },
        metadata_label: metadata.and_then(metadata_label),
    }
}

fn metadata_for_entry<'a>(
    entry: &PackFileIndexEntry,
    metadata: &'a [PackFileMetadata],
) -> Option<&'a PackFileMetadata> {
    metadata.iter().find(|metadata| match metadata {
        PackFileMetadata::DbTable(metadata) => metadata.name == entry.name,
        PackFileMetadata::Loc(metadata) => metadata.name == entry.name,
        PackFileMetadata::Other { name, .. } | PackFileMetadata::Unsupported { name, .. } => {
            name == &entry.name
        }
    })
}

fn kind_label(kind: &PackFileKind) -> &'static str {
    match kind {
        PackFileKind::DbTable { .. } => "DB",
        PackFileKind::Loc => "LOC",
        PackFileKind::Script => "Lua",
        PackFileKind::XmlLike => "XML",
        PackFileKind::Other => "File",
    }
}

fn metadata_label(metadata: &PackFileMetadata) -> Option<String> {
    match metadata {
        PackFileMetadata::DbTable(metadata) => {
            let version = metadata
                .version
                .map_or_else(|| "auto".to_string(), |version| version.to_string());
            Some(format!(
                "{} / {}, v{}, {} rows",
                metadata.db_name, metadata.db_subname, version, metadata.entry_count
            ))
        }
        PackFileMetadata::Loc(metadata) => Some(format!(
            "loc v{}, {} rows",
            metadata.version, metadata.entry_count
        )),
        PackFileMetadata::Unsupported { reason, .. } => Some(reason.clone()),
        PackFileMetadata::Other { .. } => None,
    }
}

fn plural_suffix(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn format_db_value(value: &DbPrimitiveValue) -> String {
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
        DbPrimitiveValue::I16(value) => format!("{value}"),
        DbPrimitiveValue::I64(value) => format!("{value}"),
        DbPrimitiveValue::F32(value) => format!("{value}"),
        DbPrimitiveValue::F64(value) => format!("{value}"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use wh3mm_core::{
        AppState, DbCell, DbFieldSchema, DbFieldType, DbPrimitiveValue, DbRows, DbTableMetadata,
        DbVersionSchema, GameId, LocFileMetadata, ModIdentity, ModRecord, ModSource, PackContents,
        PackFileIndexEntry, PackFileMetadata, PackIndex, WhmmFlowFileReadError,
        WhmmFlowFileSummary, WhmmFlowOptionSummary, WhmmFlowPackSummary, WorkshopModData,
    };

    use super::{
        ModRowViewModel, ModSortColumn, ModSortSpec, SortDirection, build_app_view_model,
        build_db_table_preview_view_model, build_mod_archive_rows, build_pack_contents_view_model,
        build_pack_flow_summary_view_model, build_pack_view_model, sort_mod_archive_rows,
    };

    #[test]
    fn presenter_marks_always_enabled_rows_as_enabled_and_locked() {
        let state = AppState::with_mods(
            GameId::Warhammer3,
            vec![ModRecord {
                identity: ModIdentity::new("data/mod.pack", Some("42"), "mod"),
                display_name: "My Mod".to_string(),
                source: wh3mm_core::ModSource::GameData,
                thumbnail_path: None,
                local_modified_ms: None,
                enabled: false,
                always_enabled: true,
                hidden: false,
                categories: Vec::new(),
                tags: vec!["core".to_string()],
            }],
        );

        let view_model = build_app_view_model(&state);
        let row = &view_model.mods[0];

        assert_eq!(row.key, "path:data/mod.pack");
        assert_eq!(row.display_name, "My Mod");
        assert!(row.enabled);
        assert!(row.locked);
        assert!(!row.hidden);
        assert_eq!(row.tags, ["core"]);
    }

    #[test]
    fn archive_sort_clicks_use_column_defaults_then_toggle() {
        let sort = ModSortSpec::default();
        assert_eq!(
            sort.after_column_click(ModSortColumn::Updated),
            ModSortSpec {
                column: ModSortColumn::Updated,
                direction: SortDirection::Descending,
            }
        );
        assert_eq!(
            sort.after_column_click(ModSortColumn::Order),
            ModSortSpec {
                column: ModSortColumn::Order,
                direction: SortDirection::Descending,
            }
        );
        assert_eq!(
            sort.after_column_click(ModSortColumn::Author),
            ModSortSpec {
                column: ModSortColumn::Author,
                direction: SortDirection::Ascending,
            }
        );
    }

    #[test]
    fn archive_sorting_covers_all_columns_and_directions() {
        let original = vec![
            archive_row(2, "bravo.pack", "Bravo", false, Some("Zed"), Some(200)),
            archive_row(1, "amber.pack", "Amber", true, Some("Amy"), Some(100)),
            archive_row(3, "gamma.pack", "Gamma", false, None, None),
        ];

        let cases = [
            (
                ModSortColumn::Order,
                SortDirection::Ascending,
                vec![1, 2, 3],
            ),
            (
                ModSortColumn::Order,
                SortDirection::Descending,
                vec![3, 2, 1],
            ),
            (
                ModSortColumn::Status,
                SortDirection::Ascending,
                vec![1, 2, 3],
            ),
            (
                ModSortColumn::Status,
                SortDirection::Descending,
                vec![2, 3, 1],
            ),
            (ModSortColumn::Name, SortDirection::Ascending, vec![1, 2, 3]),
            (
                ModSortColumn::Name,
                SortDirection::Descending,
                vec![3, 2, 1],
            ),
            (
                ModSortColumn::Author,
                SortDirection::Ascending,
                vec![1, 2, 3],
            ),
            (
                ModSortColumn::Author,
                SortDirection::Descending,
                vec![2, 1, 3],
            ),
            (
                ModSortColumn::Updated,
                SortDirection::Ascending,
                vec![1, 2, 3],
            ),
            (
                ModSortColumn::Updated,
                SortDirection::Descending,
                vec![2, 1, 3],
            ),
        ];

        for (column, direction, expected) in cases {
            let mut rows = original.clone();
            sort_mod_archive_rows(&mut rows, ModSortSpec { column, direction });
            assert_eq!(
                rows.iter().map(|row| row.load_order).collect::<Vec<_>>(),
                expected,
                "failed for {column:?} {direction:?}"
            );
        }
    }

    #[test]
    fn archive_projection_enriches_rows_without_mutating_launch_order() {
        let mods = vec![
            ModRecord {
                identity: ModIdentity::new("data/zeta.pack", Some("22"), "zeta"),
                display_name: "zeta".to_string(),
                source: ModSource::GameData,
                thumbnail_path: Some("data/zeta.png".to_string()),
                local_modified_ms: Some(10),
                enabled: false,
                always_enabled: false,
                hidden: false,
                categories: Vec::new(),
                tags: vec!["local".to_string()],
            },
            ModRecord {
                identity: ModIdentity::new("content/11/amber.pack", Some("11"), "amber"),
                display_name: "amber".to_string(),
                source: ModSource::Workshop,
                thumbnail_path: None,
                local_modified_ms: Some(20),
                enabled: true,
                always_enabled: false,
                hidden: false,
                categories: Vec::new(),
                tags: Vec::new(),
            },
        ];
        let state = AppState::with_mods(GameId::Warhammer3, mods);
        let original_paths = state
            .mods
            .iter()
            .map(|record| record.identity.path.clone())
            .collect::<Vec<_>>();
        let metadata = vec![WorkshopModData {
            workshop_id: "22".to_string(),
            title: "Steam Zeta".to_string(),
            author: "Author".to_string(),
            description: "Description".to_string(),
            tags: vec!["Units".to_string()],
            dependency_ids: Vec::new(),
            dependency_id_to_name: Vec::new(),
            last_changed_ms: 30,
        }];

        let rows = build_mod_archive_rows(
            &state,
            &metadata,
            ModSortSpec {
                column: ModSortColumn::Name,
                direction: SortDirection::Ascending,
            },
        );

        assert_eq!(rows[0].pack_name, "amber.pack");
        assert_eq!(rows[1].display_name, "Steam Zeta");
        assert_eq!(rows[1].author.as_deref(), Some("Author"));
        assert_eq!(rows[1].updated_ms, Some(30));
        assert_eq!(rows[1].thumbnail_path.as_deref(), Some("data/zeta.png"));
        assert!(rows[1].tags.contains(&"Units".to_string()));
        assert_eq!(
            state
                .mods
                .iter()
                .map(|record| record.identity.path.clone())
                .collect::<Vec<_>>(),
            original_paths
        );
    }

    fn archive_row(
        load_order: usize,
        pack_name: &str,
        display_name: &str,
        enabled: bool,
        author: Option<&str>,
        updated_ms: Option<u64>,
    ) -> ModRowViewModel {
        ModRowViewModel {
            key: pack_name.to_string(),
            display_name: display_name.to_string(),
            load_order,
            pack_name: pack_name.to_string(),
            author: author.map(str::to_string),
            updated_ms,
            subtitle: format!("path/{pack_name}"),
            enabled,
            ..ModRowViewModel::default()
        }
    }

    #[test]
    fn presenter_builds_pack_file_rows_with_metadata_labels() {
        let pack_index = PackIndex {
            path: PathBuf::from("fixture.pack"),
            magic: *b"PFH5",
            byte_mask: 3,
            is_movie: false,
            reference_file_count: 0,
            dependency_index_size: 0,
            packed_file_index_size: 0,
            header_buffer: [0xff, 0xff, 0xff, 0x7f],
            dependency_packs: vec!["data.pack".to_string()],
            files: vec![
                PackFileIndexEntry {
                    name: "db\\main_units_tables\\fixture".to_string(),
                    file_size: 14,
                    start_pos: 42,
                    is_compressed: true,
                },
                PackFileIndexEntry {
                    name: "text\\db\\fixture.loc".to_string(),
                    file_size: 25,
                    start_pos: 56,
                    is_compressed: false,
                },
            ],
        };
        let metadata = vec![
            PackFileMetadata::DbTable(DbTableMetadata {
                name: "db\\main_units_tables\\fixture".to_string(),
                db_name: "main_units_tables".to_string(),
                db_subname: "fixture".to_string(),
                guid: None,
                version: Some(7),
                entry_count: 3,
            }),
            PackFileMetadata::Loc(LocFileMetadata {
                name: "text\\db\\fixture.loc".to_string(),
                version: 1,
                entry_count: 2,
            }),
        ];

        let view_model = build_pack_view_model(&pack_index, &metadata);

        assert_eq!(view_model.magic, "PFH5");
        assert_eq!(view_model.dependency_packs, ["data.pack"]);
        assert_eq!(view_model.files[0].kind, "DB");
        assert_eq!(view_model.files[0].compression_label, "zstd");
        assert_eq!(
            view_model.files[0].metadata_label.as_deref(),
            Some("main_units_tables / fixture, v7, 3 rows")
        );
        assert_eq!(view_model.files[1].kind, "LOC");
        assert_eq!(
            view_model.files[1].metadata_label.as_deref(),
            Some("loc v1, 2 rows")
        );
    }

    #[test]
    fn presenter_accepts_core_pack_contents_snapshot() {
        let contents = PackContents {
            index: PackIndex {
                path: PathBuf::from("fixture.pack"),
                magic: *b"PFH5",
                byte_mask: 3,
                is_movie: false,
                reference_file_count: 0,
                dependency_index_size: 0,
                packed_file_index_size: 0,
                header_buffer: [0xff, 0xff, 0xff, 0x7f],
                dependency_packs: Vec::new(),
                files: vec![PackFileIndexEntry {
                    name: "script\\campaign\\main.lua".to_string(),
                    file_size: 10,
                    start_pos: 32,
                    is_compressed: false,
                }],
            },
            metadata: vec![PackFileMetadata::Other {
                name: "script\\campaign\\main.lua".to_string(),
                kind: wh3mm_core::PackFileKind::Script,
            }],
        };

        let view_model = build_pack_contents_view_model(&contents);

        assert_eq!(view_model.files[0].kind, "Lua");
        assert_eq!(view_model.files[0].compression_label, "plain");
        assert_eq!(view_model.flow_summary, None);
    }

    #[test]
    fn presenter_builds_pack_flow_summary() {
        let summary = WhmmFlowPackSummary {
            files: vec![WhmmFlowFileSummary {
                name: "whmmflows\\campaign.json".to_string(),
                has_graph_enable_toggle: true,
                graph_starts_enabled: false,
                node_count: 2,
                connection_count: 1,
                option_count: 1,
                options: vec![WhmmFlowOptionSummary {
                    id: "radius".to_string(),
                    name: "Radius".to_string(),
                    kind: "range".to_string(),
                    description: None,
                    default_value: Some("3".to_string()),
                }],
            }],
            read_errors: vec![WhmmFlowFileReadError {
                name: "whmmflows\\bad.json".to_string(),
                message: "failed to parse flow JSON: expected value".to_string(),
            }],
        };

        let view_model = build_pack_flow_summary_view_model(&summary).unwrap();

        assert_eq!(view_model.file_count_label, "1 flow file");
        assert_eq!(view_model.read_error_count_label, "1 read error");
        assert_eq!(
            view_model.files[0].detail_label,
            "2 nodes / 1 connection / 1 option"
        );
        assert_eq!(view_model.files[0].graph_label, "user toggle, default off");
        assert_eq!(view_model.files[0].options[0].label, "Radius (range)");
        assert_eq!(
            view_model.files[0].options[0]
                .default_value_label
                .as_deref(),
            Some("default 3")
        );
        assert_eq!(view_model.read_errors[0].name, "whmmflows\\bad.json");
    }

    #[test]
    fn presenter_builds_db_table_preview() {
        let metadata = DbTableMetadata {
            name: "db\\example_tables\\local".to_string(),
            db_name: "example_tables".to_string(),
            db_subname: "local".to_string(),
            guid: None,
            version: Some(2),
            entry_count: 1,
        };
        let schema = DbVersionSchema {
            version: 2,
            fields: vec![
                DbFieldSchema {
                    name: "key".to_string(),
                    field_type: DbFieldType::StringU8,
                    is_key: true,
                    reference: None,
                },
                DbFieldSchema {
                    name: "score".to_string(),
                    field_type: DbFieldType::I32,
                    is_key: false,
                    reference: None,
                },
            ],
        };
        let rows = DbRows {
            guid: None,
            version: Some(2),
            rows: vec![vec![
                DbCell {
                    name: "key".to_string(),
                    is_key: true,
                    value: DbPrimitiveValue::String("unit_key".to_string()),
                },
                DbCell {
                    name: "score".to_string(),
                    is_key: false,
                    value: DbPrimitiveValue::I32(42),
                },
            ]],
        };

        let preview = build_db_table_preview_view_model(&metadata, &schema, &rows, 10);

        assert_eq!(preview.title, "example_tables / local");
        assert_eq!(preview.columns[0].name, "key");
        assert!(preview.columns[0].is_key);
        assert_eq!(preview.rows[0].cells, ["unit_key", "42"]);
    }
}
