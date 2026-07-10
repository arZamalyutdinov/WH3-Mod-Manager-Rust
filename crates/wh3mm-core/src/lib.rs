//! UI-agnostic domain core for WH3 Mod Manager.
//!
//! Keep this crate free of desktop toolkit, `WebView`, and platform-shell
//! dependencies. Dioxus, Slint, or any other UI should talk to this crate
//! through explicit commands and snapshots.

pub mod app;
pub mod compat;
pub mod db;
pub mod discovery;
pub mod domain;
pub mod flows;
pub mod launcher;
pub mod overwrites;
pub mod pack;
pub mod persistence;
pub mod ports;
pub mod schema;
pub mod start_game;
pub mod steam;
pub mod ts_config;

pub use app::{AppState, CoreCommand, CoreEvent};
pub use compat::{
    FileReferenceReadError, FileToFileReference, MissingDbReference, MissingDependencyPack,
    PackConflictReport, PackFileCollision, PackReadError, PackTableCollision,
    ScriptListenerCollision, ScriptListenerValue, ScriptReadError, TableReadError,
    UniqueIdCollision, UniqueIdValue, analyze_enabled_mod_conflicts,
    analyze_enabled_mod_conflicts_with_schema, analyze_pack_indexes,
};
pub use db::{
    DbCell, DbFieldReference, DbFieldSchema, DbFieldType, DbPrimitiveValue, DbRows,
    read_db_rows_from_pack, read_db_rows_from_payload, write_db_rows_to_payload,
};
pub use discovery::{ModDiscoveryOptions, discover_mods};
pub use domain::{GameId, ModIdentity, ModRecord, ModSource};
pub use flows::{
    WHMM_FLOW_FILE_PREFIX, WhmmFlowFileReadError, WhmmFlowFileSummary, WhmmFlowOptionSummary,
    WhmmFlowPackSummary, is_whmm_flow_file_name, read_whmm_flow_file_names,
    read_whmm_flow_pack_summary,
};
pub use launcher::{
    PreLaunchCopyOperation, PreLaunchPackWrite, WindowsLaunchOptions, WindowsLaunchPackGroup,
    WindowsLaunchPlan, plan_windows_launch,
};
pub use overwrites::{
    GeneratedOverwritePack, PackDataOverwrite, PackDataOverwriteOperation, PackDataOverwriteValue,
    build_pack_data_overwrite_pack,
};
pub use pack::{
    DbTableMetadata, LocFileMetadata, PackContents, PackFileIndexEntry, PackFileKind,
    PackFileMetadata, PackFileWrite, PackIndex, PackReadOptions, build_pfh5_pack_bytes,
    read_pack_contents, read_pack_contents_lossy, read_pack_index, read_packed_file_metadata,
    read_packed_file_payload,
};
pub use persistence::{
    GameFolderConfig, ModListConfig, ModUserConfig, PersistedModState, PersistedModUserState,
    PersistedPreset, PresetConfig, SteamHelperConfig, add_category_config, add_mod_category,
    apply_mod_list_config, apply_mod_list_pack_names, apply_mod_user_config, apply_preset_config,
    capture_game_folder_config, capture_mod_list_config, capture_mod_user_config, capture_preset,
    capture_preset_config, capture_steam_helper_config, capture_steam_helper_config_with_backend,
    delete_category_config, delete_preset_config, parse_mod_list_pack_names, preset_names,
    read_game_folder_config, read_mod_list_config, read_mod_user_config, read_preset_config,
    read_steam_helper_config, read_workshop_metadata_cache, remove_mod_category,
    rename_category_config, set_category_color_config, upsert_preset_config,
    write_game_folder_config_atomic, write_mod_list_config_atomic, write_mod_user_config_atomic,
    write_preset_config_atomic, write_steam_helper_config_atomic,
    write_workshop_metadata_cache_atomic,
};
pub use ports::{CoreError, CoreResult, ModRepository};
pub use schema::{
    DbSchema, DbVersionSchema, load_schema_file, resolve_table_schema, select_schema_version,
};
pub use start_game::{
    GeneratedStartGamePack, WH3_BATTLE_PERMISSIONS_DB_NAME, WH3_MAKE_UNITS_GENERALS_TABLE_GUID,
    WH3_MAKE_UNITS_GENERALS_TABLE_PATH, WH3_MAKE_UNITS_GENERALS_TABLE_VERSION,
    WH3_START_GAME_PACK_NAME, WH3_START_GAME_SOURCE_PACK_NAMES, Wh3BattlePermissionTables,
    Wh3StartGamePackOptions, build_wh3_make_units_generals_payload,
    build_wh3_make_units_generals_rows, build_wh3_start_game_pack,
    build_wh3_start_game_pack_with_battle_permissions,
    read_wh3_battle_permission_tables_from_packs,
};
pub use steam::{
    CachedWorkshopData, QueueWorkshopIdsResult, RetryDelayPlan, SteamWorkshopAdapterError,
    SteamWorkshopAdapterErrorKind, SteamWorkshopMetadataAdapter, SteamWorkshopRequestState,
    SteamWorkshopSafetyConfig, WorkshopBatchPlan, WorkshopMetadataCache,
    WorkshopMetadataCacheEntry, WorkshopMetadataFetchStep, WorkshopModData, normalize_workshop_id,
    parse_ts_steam_helper_mod_data_response, ts_steam_helper_dependency_ids_needing_titles,
};
pub use ts_config::{
    LegacyTsConfigSnapshot, LegacyTsLaunchOptions, export_legacy_ts_config_bytes,
    import_legacy_ts_config_bytes, read_legacy_ts_config, write_legacy_ts_config_atomic,
};
