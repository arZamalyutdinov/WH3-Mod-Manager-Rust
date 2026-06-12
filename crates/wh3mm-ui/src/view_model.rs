//! Toolkit-neutral view-model structs.

/// Full view model for the main window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppViewModel {
    /// Window or app title.
    pub title: String,
    /// Current mod rows.
    pub mods: Vec<ModRowViewModel>,
    /// Whether background work is in progress.
    pub busy: bool,
    /// Optional status text.
    pub status_message: Option<String>,
    /// Optional selected pack summary.
    pub selected_pack: Option<PackViewModel>,
}

/// View model for one mod-list row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModRowViewModel {
    /// Toolkit-neutral stable key.
    pub key: String,
    /// Primary row label.
    pub display_name: String,
    /// Secondary row label.
    pub subtitle: String,
    /// Effective enablement, including forced-on mods.
    pub enabled: bool,
    /// Whether enablement is locked by app/game rules.
    pub locked: bool,
    /// Whether the row is hidden from normal mod-list views.
    pub hidden: bool,
    /// User-defined categories.
    pub categories: Vec<String>,
    /// User-facing tags.
    pub tags: Vec<String>,
}

/// View model for a selected pack.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackViewModel {
    /// Source pack path.
    pub path: String,
    /// Pack magic such as `PFH5`.
    pub magic: String,
    /// Whether the pack is a movie pack.
    pub is_movie: bool,
    /// Dependency pack names.
    pub dependency_packs: Vec<String>,
    /// Packed file rows.
    pub files: Vec<PackFileRowViewModel>,
    /// Optional selected DB table preview.
    pub table_preview: Option<DbTablePreviewViewModel>,
    /// Optional WH3MM user-flow summary.
    pub flow_summary: Option<PackFlowSummaryViewModel>,
}

/// View model for WH3MM user-flow files inside a pack.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackFlowSummaryViewModel {
    /// Summary label for successfully parsed flow files.
    pub file_count_label: String,
    /// Summary label for per-flow read errors.
    pub read_error_count_label: String,
    /// Parsed flow files.
    pub files: Vec<PackFlowFileViewModel>,
    /// Flow files that could not be parsed/read.
    pub read_errors: Vec<PackFlowErrorViewModel>,
}

/// View model for one WH3MM flow file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackFlowFileViewModel {
    /// Packed flow file path.
    pub name: String,
    /// Node/connection/option counts.
    pub detail_label: String,
    /// Toggle/default-state summary.
    pub graph_label: String,
    /// User-facing option summaries.
    pub options: Vec<PackFlowOptionViewModel>,
}

/// View model for one WH3MM flow option.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackFlowOptionViewModel {
    /// Stable option ID.
    pub id: String,
    /// User-facing label and type.
    pub label: String,
    /// Optional default-value label.
    pub default_value_label: Option<String>,
}

/// View model for one malformed/unreadable flow file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackFlowErrorViewModel {
    /// Packed flow file path.
    pub name: String,
    /// Human-readable diagnostic.
    pub message: String,
}

/// View model for one packed file inside a pack.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackFileRowViewModel {
    /// Stable row key.
    pub key: String,
    /// Packed file path.
    pub name: String,
    /// Human-readable kind label.
    pub kind: String,
    /// File size label.
    pub size_label: String,
    /// Payload start offset label.
    pub offset_label: String,
    /// Compression label.
    pub compression_label: String,
    /// Optional metadata summary.
    pub metadata_label: Option<String>,
}

/// View model for a decoded DB table preview.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DbTablePreviewViewModel {
    /// Stable title for the selected table.
    pub title: String,
    /// Source packed file path.
    pub source_name: String,
    /// Schema version label.
    pub version_label: String,
    /// Total decoded row count label.
    pub row_count_label: String,
    /// Column headers.
    pub columns: Vec<DbTableColumnViewModel>,
    /// Preview rows.
    pub rows: Vec<DbTableRowViewModel>,
}

/// View model for one DB table column.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DbTableColumnViewModel {
    /// Field name.
    pub name: String,
    /// Whether this column is a key field.
    pub is_key: bool,
}

/// View model for one DB preview row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DbTableRowViewModel {
    /// Stable row key.
    pub key: String,
    /// Formatted cell values.
    pub cells: Vec<String>,
}
