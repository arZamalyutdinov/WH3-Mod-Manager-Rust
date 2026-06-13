//! First Dioxus desktop shell.
//!
//! This app intentionally depends on `wh3mm-core` and `wh3mm-ui` only through
//! toolkit-neutral models. A future Slint app should be able to render the same
//! `AppViewModel` without reimplementing domain behavior.

use dioxus::prelude::*;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use wh3mm_core::{
    AppState, CoreCommand, GameId, LegacyTsConfigSnapshot, LegacyTsLaunchOptions,
    ModDiscoveryOptions, ModIdentity, ModRecord, ModUserConfig, PackConflictReport, PackContents,
    PackDataOverwrite, PackFileMetadata, PackReadOptions, PreLaunchPackWrite, PresetConfig,
    SteamWorkshopMetadataAdapter, SteamWorkshopRequestState, SteamWorkshopSafetyConfig,
    WH3_START_GAME_PACK_NAME, WH3_START_GAME_SOURCE_PACK_NAMES, Wh3StartGamePackOptions,
    WindowsLaunchOptions, WindowsLaunchPackGroup, WorkshopMetadataFetchStep, WorkshopModData,
    add_mod_category, analyze_enabled_mod_conflicts, analyze_enabled_mod_conflicts_with_schema,
    apply_mod_list_config, apply_mod_list_pack_names, apply_mod_user_config, apply_preset_config,
    build_pack_data_overwrite_pack, build_wh3_start_game_pack_with_battle_permissions,
    capture_game_folder_config, capture_mod_list_config, capture_mod_user_config,
    capture_steam_helper_config_with_backend, delete_category_config, delete_preset_config,
    discover_mods, normalize_workshop_id, parse_mod_list_pack_names, plan_windows_launch,
    preset_names, read_db_rows_from_pack, read_game_folder_config, read_legacy_ts_config,
    read_mod_list_config, read_mod_user_config, read_pack_contents_lossy, read_preset_config,
    read_steam_helper_config, read_wh3_battle_permission_tables_from_packs,
    read_whmm_flow_pack_summary, remove_mod_category, rename_category_config, resolve_table_schema,
    set_category_color_config, upsert_preset_config, write_game_folder_config_atomic,
    write_legacy_ts_config_atomic, write_mod_list_config_atomic, write_mod_user_config_atomic,
    write_preset_config_atomic, write_steam_helper_config_atomic,
};
use wh3mm_runtime::{
    LaunchPreparationOptions, SteamResubscribeResult, SteamResubscribeSafetyConfig,
    SteamWorkshopCheckStateResult, SteamWorkshopCommandAdapter, SteamWorkshopCommandResult,
    SteamWorkshopCommandRunner, SteamWorkshopHelperProcessConfig, SteamWorkshopHelperProcessRunner,
    TsSteamHelperMetadataAdapter, WH3_STEAM_APP_ID, WindowsLaunchSpawnOptions,
    WindowsProcessPriorityClass, WindowsProcessPriorityUpdate,
    discover_wh3_steam_install_from_windows_registry, discover_wh3_workshop_folder,
    prepare_windows_launch_files, resubscribe_with_cleanup_and_verification,
    spawn_prepared_windows_launch_with_options, validate_wh3_game_folder,
};
use wh3mm_ui::{
    ModRowViewModel, PackViewModel, build_app_view_model, build_db_table_preview_view_model,
    build_pack_contents_view_model, build_pack_flow_summary_view_model,
};

const MAX_TABLE_PREVIEW_ROWS: usize = 25;
const APP_CONFIG_DIR_NAME: &str = "WH3 Mod Manager Rust";
const APP_CONFIG_DIR_ENV: &str = "WH3MM_CONFIG_DIR";
const GAME_FOLDER_CONFIG_FILE: &str = "wh3mm_game_folder.json";
const MOD_STATE_CONFIG_FILE: &str = "wh3mm_mod_state.json";
const MOD_USER_CONFIG_FILE: &str = "wh3mm_mod_user_config.json";
const PRESET_CONFIG_FILE: &str = "wh3mm_presets.json";
const STEAM_HELPER_CONFIG_FILE: &str = "wh3mm_steam_helper.json";
const DIAGNOSTICS_DIR_NAME: &str = "diagnostics";
const APP_DIAGNOSTIC_LOG_FILE: &str = "wh3mm-dioxus.log";
const STEAM_HELPER_COMMAND_LOG_FILE: &str = "wh3mm-steam-helper-commands.jsonl";
const LEGACY_TS_GAME_KEY: &str = "wh3";
const ON_LAST_GAME_LAUNCH_PRESET_NAME: &str = "On Last Game Launch";
const STEAM_HELPER_BACKEND_ENV: &str = "WH3MM_STEAM_HELPER_BACKEND";
const STEAM_HELPER_COMMAND_LOG_ENV: &str = "WH3MM_STEAM_HELPER_COMMAND_LOG";
const STEAM_HELPER_BACKEND_NATIVE: &str = "native";
const STEAM_HELPER_BACKEND_FIXTURE: &str = "fixture";
const CLOSE_ON_PLAY_DELAY_SECS: u64 = 5;

#[derive(Clone, Debug, Eq, PartialEq)]
struct LaunchPreview {
    game_dir: String,
    data_dir: String,
    mod_list_file_name: String,
    mod_list_contents: String,
    command_line_preview: String,
    enabled_count: usize,
    pre_launch_copies: Vec<LaunchCopyPreview>,
    generated_packs: Vec<GeneratedPackPreview>,
    fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LaunchCopyPreview {
    from_path: String,
    to_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GeneratedPackPreview {
    path: String,
    byte_len: usize,
    packed_file_names: Vec<String>,
    packed_file_summary: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct LaunchOptionState {
    skip_intro_movies: bool,
    script_logging: bool,
    auto_start_custom_battle: bool,
    make_units_generals: bool,
    close_on_play: bool,
    high_process_priority: bool,
    pack_data_overwrites: BTreeMap<String, Vec<PackDataOverwrite>>,
    user_flow_options: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SteamRefreshResult {
    subscribed_ids: Vec<String>,
    metadata: Vec<WorkshopModData>,
    requested_metadata_count: usize,
    missing_metadata_count: usize,
    filtered_unsubscribed_count: usize,
    renamed_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LegacyTsConfigImportResult {
    mods: Vec<ModRecord>,
    game_folder: Option<PathBuf>,
    launch_options: LaunchOptionState,
    status: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SteamMetadataBatchResult {
    metadata: Vec<WorkshopModData>,
    requested_count: usize,
    missing_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SteamCommandPanelState {
    title: String,
    summary: String,
    rows: Vec<SteamCommandPanelRow>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SteamCommandPanelRow {
    label: String,
    value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SteamCommandUiResult {
    status: String,
    panel: SteamCommandPanelState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
struct SteamHelperProbeReport {
    app_id: String,
    selected_backend: String,
    #[serde(default)]
    fixture_configured: bool,
    #[serde(default)]
    fixture_available: bool,
    #[serde(default)]
    command_log_configured: bool,
    #[serde(default)]
    native_implemented: bool,
    #[serde(default)]
    native_available: bool,
    #[serde(default)]
    native_status: String,
    #[serde(default)]
    windows_runtime_redistributables: Vec<String>,
    #[serde(default)]
    windows_runtime_redistributable_statuses: Vec<SteamRuntimeRedistributableStatus>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
struct SteamRuntimeRedistributableStatus {
    file_name: String,
    expected_path: String,
    #[serde(default)]
    present: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AlphaReadinessReport {
    summary: String,
    rows: Vec<AlphaReadinessRow>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AlphaReadinessRow {
    label: String,
    status: AlphaReadinessStatus,
    detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum AlphaReadinessStatus {
    Ready,
    Warning,
    Error,
}

struct DiagnosticSnapshotInput<'a> {
    app_state: &'a AppState,
    game_folder: Option<&'a Path>,
    helper_path: &'a str,
    helper_backend: &'a str,
    launch_options: &'a LaunchOptionState,
    launch_save_name: &'a str,
    status_message: Option<&'a str>,
    readiness: &'a AlphaReadinessReport,
    launch_preview: Option<&'a LaunchPreview>,
    last_steam_command: Option<&'a SteamCommandPanelState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModListFilter {
    All,
    Enabled,
    Disabled,
    Locked,
    Hidden,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LibraryToolTab {
    None,
    Presets,
    Categories,
    Config,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkspacePage {
    Mods,
    ModDetail,
    Categories,
    Collections,
    Compatibility,
    Checks,
    Steam,
    Workshop,
    Settings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LibraryNavTarget {
    AllMods,
    Enabled,
    Categories,
    Collections,
    Settings,
}

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    let mut app_state = use_signal(initial_app_state);
    let mut pack_selection = use_signal(selected_pack_from_args);
    let mut mod_status = use_signal(|| None::<String>);
    let mut game_folder = use_signal(load_saved_game_folder);
    let mut preset_name = use_signal(|| "Prototype preset".to_string());
    let mut saved_presets = use_signal(load_preset_names);
    let mut category_name = use_signal(|| "Core".to_string());
    let mut selected_category_name = use_signal(|| "Core".to_string());
    let mut category_color = use_signal(|| "blue".to_string());
    let mut saved_categories = use_signal(load_category_names);
    let mut show_hidden = use_signal(|| false);
    let mut mod_search = use_signal(String::new);
    let mut mod_list_filter = use_signal(|| ModListFilter::All);
    let mut library_tool_tab = use_signal(|| LibraryToolTab::None);
    let mut workspace_page = use_signal(|| WorkspacePage::Mods);
    let mut selected_mod_key = use_signal(|| None::<String>);
    let mut conflict_report = use_signal(|| None::<PackConflictReport>);
    let mut launch_preview = use_signal(|| None::<LaunchPreview>);
    let mut launch_options = use_signal(LaunchOptionState::default);
    let mut launch_save_name = use_signal(String::new);
    let mut steam_helper_path = use_signal(load_saved_steam_helper_path);
    let mut steam_helper_backend = use_signal(load_saved_steam_helper_backend);
    let mut steam_command_ids = use_signal(String::new);
    let mut steam_metadata = use_signal(Vec::<WorkshopModData>::new);
    let mut subscribed_workshop_ids = use_signal(Vec::<String>::new);
    let mut last_steam_command = use_signal(|| None::<SteamCommandPanelState>);
    let mut last_logged_mod_status = use_signal(|| None::<String>);

    use_effect(move || {
        let status = mod_status.read().clone();
        if let Some(status) = status {
            let should_log = {
                let logged_status = last_logged_mod_status.read();
                logged_status.as_deref() != Some(status.as_str())
            };
            if should_log {
                last_logged_mod_status.set(Some(status.clone()));
                let _ = append_app_diagnostic_log_event(&format!("status: {status}"));
            }
        }
    });

    let mut view_model = build_app_view_model(&app_state.read());
    let (selected_pack, status_message) = pack_selection.read().clone();
    view_model.selected_pack = selected_pack;
    view_model.status_message =
        combined_status(mod_status.read().as_deref(), status_message.as_deref());
    let all_mod_rows = view_model.mods.clone();
    let current_mod_filter = *mod_list_filter.read();
    let current_mod_filter_label = mod_list_filter_label(current_mod_filter);
    let current_library_tool = *library_tool_tab.read();
    let current_workspace_page = *workspace_page.read();
    let visible_rows = all_mod_rows
        .iter()
        .filter(|mod_row| {
            *show_hidden.read() || current_mod_filter == ModListFilter::Hidden || !mod_row.hidden
        })
        .filter(|mod_row| mod_row_matches_filter(mod_row, current_mod_filter))
        .cloned()
        .collect::<Vec<_>>();
    let mod_search_query = mod_search.read().trim().to_ascii_lowercase();
    let filtered_mods = if mod_search_query.is_empty() {
        visible_rows
    } else {
        visible_rows
            .iter()
            .filter(|mod_row| mod_row_matches_query(mod_row, &mod_search_query))
            .cloned()
            .collect::<Vec<_>>()
    };
    let filtered_mod_count = filtered_mods.len();
    let selected_mod =
        selected_or_first_mod_row(&filtered_mods, selected_mod_key.read().as_deref());
    let active_mod_key = selected_mod.as_ref().map(|mod_row| mod_row.key.clone());
    let current_launch_options = launch_options.read().clone();
    let current_launch_save_name = launch_save_name.read().clone();
    let current_launch_fingerprint = launch_state_fingerprint(
        &app_state.read().mods,
        &current_launch_options,
        &current_launch_save_name,
    );
    let visible_mod_count = all_mod_rows
        .iter()
        .filter(|mod_row| !mod_row.hidden)
        .count();
    let enabled_mod_count = all_mod_rows
        .iter()
        .filter(|mod_row| mod_row.enabled)
        .count();
    let disabled_mod_count = all_mod_rows
        .iter()
        .filter(|mod_row| !mod_row.enabled && !mod_row.locked)
        .count();
    let locked_mod_count = all_mod_rows.iter().filter(|mod_row| mod_row.locked).count();
    let hidden_mod_count = all_mod_rows.iter().filter(|mod_row| mod_row.hidden).count();
    let total_mod_count = all_mod_rows.len();
    let launch_enabled_mod_count = app_state
        .read()
        .mods
        .iter()
        .filter(|mod_record| mod_record.effectively_enabled())
        .count();
    let current_game_folder_label = game_folder
        .read()
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "No WH3 folder selected".to_string());
    let readiness_game_folder = game_folder.read().clone();
    let readiness_helper_path = steam_helper_path.read().clone();
    let alpha_readiness = build_alpha_readiness_report(
        readiness_game_folder.as_deref(),
        readiness_helper_path.trim(),
    );
    let alpha_readiness_summary = alpha_readiness.summary.clone();
    let saved_category_count = saved_categories.read().len();
    let saved_preset_count = saved_presets.read().len();
    let category_summaries = saved_categories
        .read()
        .iter()
        .map(|category| {
            let assigned_count = all_mod_rows
                .iter()
                .filter(|mod_row| mod_row.categories.iter().any(|name| name == category))
                .count();
            (category.clone(), assigned_count)
        })
        .collect::<Vec<_>>();
    let preset_summaries = saved_presets.read().clone();
    let current_steam_metadata = steam_metadata.read().clone();
    let current_time_ms = current_unix_ms();
    let diagnostics_log_path_label = app_diagnostic_log_path().display().to_string();
    let steam_command_log_path_label = steam_helper_command_log_path().display().to_string();

    rsx! {
        main {
            style: "height: 100vh; min-height: 0; background: #0f1218; color: #f2f5f2; font-family: Inter, ui-sans-serif, system-ui, sans-serif; display: flex; flex-direction: column; overflow: hidden;",
            header {
                style: "height: 66px; border-bottom: 1px solid #263041; background: #121620; display: grid; grid-template-columns: minmax(220px, auto) minmax(300px, 520px) minmax(250px, 1fr); align-items: center; gap: 16px; padding: 8px 18px; flex-shrink: 0;",
                div {
                    style: "min-width: 0; display: grid; gap: 2px;",
                    h1 {
                        style: "font-size: 21px; line-height: 27px; margin: 0; color: #f8fafc; letter-spacing: 0; white-space: nowrap; text-transform: uppercase;",
                        "{app_brand_title()}"
                    }
                    div {
                        style: "font-size: 11px; line-height: 14px; color: #94a89b; text-transform: uppercase;",
                        "{app_brand_subtitle(&view_model.title)}"
                    }
                }
                div {
                    style: "min-width: 0; display: flex; align-items: center; gap: 8px; border: 1px solid #35374a; background: #242531; border-radius: 5px; padding: 0 10px;",
                    span {
                        style: "font-size: 13px; color: #7d8ea3;",
                        "Search"
                    }
                    input {
                        style: "width: 100%; min-width: 0; height: 38px; border: 0; outline: 0; background: transparent; color: #edf2f7; font-size: 14px;",
                        value: "{mod_search}",
                        placeholder: "Name, pack path, tag, category",
                        oninput: move |event| {
                            mod_search.set(event.value());
                        },
                    }
                }
                div {
                    style: "min-width: 0; display: flex; align-items: center; justify-content: flex-end; gap: 8px; color: #9fb0c0; font-size: 12px; white-space: nowrap;",
                    span { style: header_metric_style(), "{filtered_mod_count} shown" }
                    span { style: header_metric_style(), "{launch_enabled_mod_count} enabled" }
                    span { style: header_metric_style(), "{hidden_mod_count} hidden" }
                    button {
                        title: "Alpha readiness: {alpha_readiness_summary}",
                        style: top_icon_button_style(current_workspace_page == WorkspacePage::Checks),
                        onclick: move |_| {
                            workspace_page.set(WorkspacePage::Checks);
                        },
                        "Checks"
                    }
                    button {
                        title: "Workshop commands",
                        style: top_icon_button_style(current_workspace_page == WorkspacePage::Workshop),
                        onclick: move |_| {
                            workspace_page.set(WorkspacePage::Workshop);
                        },
                        "Workshop"
                    }
                    button {
                        title: "Settings",
                        style: top_icon_button_style(current_workspace_page == WorkspacePage::Settings),
                        onclick: move |_| {
                            workspace_page.set(WorkspacePage::Settings);
                            library_tool_tab.set(LibraryToolTab::None);
                        },
                        "Settings"
                    }
                }
            }
            div {
                style: "flex: 1; min-height: 0; display: grid; grid-template-columns: minmax(220px, 252px) minmax(0, 1fr) minmax(288px, 340px); grid-template-areas: \"library content tools\"; overflow: hidden;",
                aside {
                    style: "grid-area: library; min-width: 0; border-right: 1px solid #263041; background: #171b24; padding: 20px 14px; overflow-y: auto; display: flex; flex-direction: column;",
                    div {
                        style: "display: grid; gap: 4px; margin-bottom: 18px;",
                        h2 {
                            style: "font-size: 18px; line-height: 24px; margin: 0; color: #edf2f7;",
                            "Library"
                        }
                        div {
                            style: "font-size: 11px; line-height: 15px; color: #9fb0a3; text-transform: uppercase;",
                            "Windows alpha"
                        }
                    }
                    nav {
                        style: "display: grid; gap: 8px; margin-bottom: 22px;",
                        button {
                            style: nav_button_style(library_nav_active(
                                LibraryNavTarget::AllMods,
                                current_workspace_page,
                                current_mod_filter,
                                current_library_tool,
                            )),
                            onclick: move |_| {
                                workspace_page.set(WorkspacePage::Mods);
                                mod_list_filter.set(ModListFilter::All);
                                library_tool_tab.set(LibraryToolTab::None);
                            },
                            span { style: nav_badge_style(), "ALL" }
                            span { "All mods" }
                            strong { "{total_mod_count}" }
                        }
                        button {
                            style: nav_button_style(library_nav_active(
                                LibraryNavTarget::Enabled,
                                current_workspace_page,
                                current_mod_filter,
                                current_library_tool,
                            )),
                            onclick: move |_| {
                                workspace_page.set(WorkspacePage::Mods);
                                mod_list_filter.set(ModListFilter::Enabled);
                                library_tool_tab.set(LibraryToolTab::None);
                            },
                            span { style: nav_badge_style(), "ON" }
                            span { "Enabled" }
                            strong { "{enabled_mod_count}" }
                        }
                        button {
                            style: nav_button_style(library_nav_active(
                                LibraryNavTarget::Categories,
                                current_workspace_page,
                                current_mod_filter,
                                current_library_tool,
                            )),
                            onclick: move |_| {
                                workspace_page.set(WorkspacePage::Categories);
                                mod_list_filter.set(ModListFilter::All);
                                library_tool_tab.set(LibraryToolTab::None);
                            },
                            span { style: nav_badge_style(), "CAT" }
                            span { "Categories" }
                            strong { "{saved_category_count}" }
                        }
                        button {
                            style: nav_button_style(library_nav_active(
                                LibraryNavTarget::Collections,
                                current_workspace_page,
                                current_mod_filter,
                                current_library_tool,
                            )),
                            onclick: move |_| {
                                workspace_page.set(WorkspacePage::Collections);
                                mod_list_filter.set(ModListFilter::All);
                                library_tool_tab.set(LibraryToolTab::None);
                            },
                            span { style: nav_badge_style(), "COL" }
                            span { "Collections" }
                            strong { "{saved_preset_count}" }
                        }
                        button {
                            style: nav_button_style(library_nav_active(
                                LibraryNavTarget::Settings,
                                current_workspace_page,
                                current_mod_filter,
                                current_library_tool,
                            )),
                            onclick: move |_| {
                                workspace_page.set(WorkspacePage::Settings);
                                library_tool_tab.set(LibraryToolTab::None);
                            },
                            span { style: nav_badge_style(), "SET" }
                            span { "Settings" }
                            strong { "" }
                        }
                    }
                    if current_workspace_page == WorkspacePage::Mods {
                    div {
                        style: "display: grid; gap: 6px; margin-bottom: 18px;",
                        h3 {
                            style: "font-size: 12px; line-height: 16px; color: #9fb0a3; text-transform: uppercase; margin: 0;",
                            "Game folder"
                        }
                        div {
                            style: "font-size: 12px; line-height: 17px; color: #cbd8cc; overflow-wrap: anywhere;",
                            "{current_game_folder_label}"
                        }
                    }
                    button {
                        style: "width: 100%; border: 1px solid #3a4756; background: #202832; color: #edf2f7; border-radius: 6px; padding: 8px 10px; margin: -8px 0 18px;",
                        onclick: move |_| {
                            if let Some(selected_game_folder) = pick_game_folder() {
                                match save_game_folder(&selected_game_folder) {
                                    Ok(status) => {
                                        game_folder.set(Some(selected_game_folder));
                                        mod_status.set(Some(status));
                                    }
                                    Err(error) => mod_status.set(Some(format!("Could not save game folder: {}", error.message))),
                                }
                            }
                        },
                        "Set game folder"
                    }
                    if current_library_tool == LibraryToolTab::Presets {
                    section {
                        style: "display: grid; gap: 8px; margin-bottom: 18px; border: 1px solid #2b352d; background: #111710; border-radius: 6px; padding: 10px;",
                        h3 {
                            style: "font-size: 12px; line-height: 16px; color: #9fb0a3; text-transform: uppercase; margin: 0;",
                            "Preset"
                        }
                        input {
                            style: "min-width: 0; border: 1px solid #3a4756; background: #111820; color: #edf2f7; border-radius: 6px; padding: 8px 10px;",
                            value: "{preset_name}",
                            placeholder: "Preset name",
                            oninput: move |event| {
                                preset_name.set(event.value());
                            },
                        }
                        select {
                            style: "min-width: 0; border: 1px solid #3a4756; background: #111820; color: #edf2f7; border-radius: 6px; padding: 8px 10px;",
                            value: "{preset_name}",
                            onchange: move |event| {
                                let value = event.value();
                                if !value.trim().is_empty() {
                                    preset_name.set(value);
                                }
                            },
                            option {
                                value: "",
                                "Presets"
                            }
                            for saved_preset in saved_presets.read().iter() {
                                option {
                                    value: "{saved_preset}",
                                    "{saved_preset}"
                                }
                            }
                        }
                        div {
                            style: "display: grid; grid-template-columns: 1fr 1fr; gap: 6px;",
                            button {
                                style: "border: 1px solid #3a4756; background: #202832; color: #edf2f7; border-radius: 6px; padding: 8px 10px;",
                                onclick: move |_| {
                                    let name = preset_name.read().trim().to_string();
                                    let state = app_state.read().clone();
                                    match save_named_preset(&name, &state) {
                                        Ok(status) => {
                                            saved_presets.set(load_preset_names());
                                            mod_status.set(Some(status));
                                        }
                                        Err(error) => mod_status.set(Some(format!("Could not save preset: {}", error.message))),
                                    }
                                },
                                "Save"
                            }
                            button {
                                style: "border: 1px solid #3a4756; background: #202832; color: #edf2f7; border-radius: 6px; padding: 8px 10px;",
                                onclick: move |_| {
                                    let name = preset_name.read().trim().to_string();
                                    let state = app_state.read().clone();
                                    match load_named_preset(&name, &state) {
                                        Ok((mods, status)) => {
                                            let mut next_state = state.clone();
                                            let _ = next_state.apply(CoreCommand::ReplaceMods { mods });
                                            match save_mod_state(&next_state) {
                                                Ok(save_status) => mod_status.set(Some(format!("{status} {save_status}"))),
                                                Err(error) => mod_status.set(Some(format!("{status} Could not save mod state: {}", error.message))),
                                            }
                                            app_state.set(next_state);
                                        }
                                        Err(error) => mod_status.set(Some(format!("Could not load preset: {}", error.message))),
                                    }
                                },
                                "Load"
                            }
                        }
                        button {
                            style: "border: 1px solid #7f1d1d; background: #451a1a; color: #fecaca; border-radius: 6px; padding: 8px 10px;",
                            onclick: move |_| {
                                let name = preset_name.read().trim().to_string();
                                match delete_named_preset(&name) {
                                    Ok(status) => {
                                        let names = load_preset_names();
                                        let next_name = if names.iter().any(|saved_name| saved_name == &name) {
                                            name
                                        } else {
                                            names.first().cloned().unwrap_or_else(|| "Prototype preset".to_string())
                                        };
                                        preset_name.set(next_name);
                                        saved_presets.set(names);
                                        mod_status.set(Some(status));
                                    }
                                    Err(error) => mod_status.set(Some(format!("Could not delete preset: {}", error.message))),
                                }
                            },
                            "Delete preset"
                        }
                    }
                    }
                    if current_library_tool == LibraryToolTab::Categories {
                    section {
                        style: "display: grid; gap: 8px; margin-bottom: 18px; border: 1px solid #2b352d; background: #111710; border-radius: 6px; padding: 10px;",
                        h3 {
                            style: "font-size: 12px; line-height: 16px; color: #9fb0a3; text-transform: uppercase; margin: 0;",
                            "Category"
                        }
                        input {
                            style: "min-width: 0; border: 1px solid #3a4756; background: #111820; color: #edf2f7; border-radius: 6px; padding: 8px 10px;",
                            value: "{category_name}",
                            placeholder: "Category",
                            oninput: move |event| {
                                category_name.set(event.value());
                            },
                        }
                        select {
                            style: "min-width: 0; border: 1px solid #3a4756; background: #111820; color: #edf2f7; border-radius: 6px; padding: 8px 10px;",
                            value: "{selected_category_name}",
                            onchange: move |event| {
                                let value = event.value();
                                if !value.trim().is_empty() {
                                    selected_category_name.set(value.clone());
                                    category_name.set(value);
                                }
                            },
                            option {
                                value: "",
                                "Categories"
                            }
                            for saved_category in saved_categories.read().iter() {
                                option {
                                    value: "{saved_category}",
                                    "{saved_category}"
                                }
                            }
                        }
                        select {
                            style: "min-width: 0; border: 1px solid #3a4756; background: #111820; color: #edf2f7; border-radius: 6px; padding: 8px 10px;",
                            value: "{category_color}",
                            onchange: move |event| {
                                category_color.set(event.value());
                            },
                            option { value: "blue", "Blue" }
                            option { value: "green", "Green" }
                            option { value: "yellow", "Yellow" }
                            option { value: "red", "Red" }
                            option { value: "purple", "Purple" }
                            option { value: "gray", "Gray" }
                        }
                        div {
                            style: "display: grid; grid-template-columns: 1fr 1fr; gap: 6px;",
                            button {
                                style: "border: 1px solid #3a4756; background: #202832; color: #edf2f7; border-radius: 6px; padding: 8px 10px;",
                                onclick: move |_| {
                                    let category = category_name.read().trim().to_string();
                                    let color = category_color.read().trim().to_string();
                                    match save_category_definition(&category, &color) {
                                        Ok(status) => {
                                            saved_categories.set(load_category_names());
                                            mod_status.set(Some(status));
                                        }
                                        Err(error) => mod_status.set(Some(format!("Could not save category: {}", error.message))),
                                    }
                                },
                                "Save"
                            }
                            button {
                                style: "border: 1px solid #3a4756; background: #202832; color: #edf2f7; border-radius: 6px; padding: 8px 10px;",
                                onclick: move |_| {
                                    let old_category = selected_category_name.read().trim().to_string();
                                    let new_category = category_name.read().trim().to_string();
                                    let state = app_state.read().clone();
                                    match rename_category_definition(&old_category, &new_category, &state) {
                                        Ok((mods, status)) => {
                                            let mut next_state = app_state.read().clone();
                                            let _ = next_state.apply(CoreCommand::ReplaceMods { mods });
                                            app_state.set(next_state);
                                            selected_category_name.set(new_category);
                                            saved_categories.set(load_category_names());
                                            mod_status.set(Some(status));
                                        }
                                        Err(error) => mod_status.set(Some(format!("Could not rename category: {}", error.message))),
                                    }
                                },
                                "Rename"
                            }
                        }
                        button {
                            style: "border: 1px solid #7f1d1d; background: #451a1a; color: #fecaca; border-radius: 6px; padding: 8px 10px;",
                            onclick: move |_| {
                                let category = selected_category_name.read().trim().to_string();
                                let state = app_state.read().clone();
                                match delete_category_definition(&category, &state) {
                                    Ok((mods, status)) => {
                                        let mut next_state = app_state.read().clone();
                                        let _ = next_state.apply(CoreCommand::ReplaceMods { mods });
                                        app_state.set(next_state);
                                        let names = load_category_names();
                                        let next_category = names.first().cloned().unwrap_or_else(|| "Core".to_string());
                                        category_name.set(next_category.clone());
                                        selected_category_name.set(next_category);
                                        saved_categories.set(names);
                                        mod_status.set(Some(status));
                                    }
                                    Err(error) => mod_status.set(Some(format!("Could not delete category: {}", error.message))),
                                }
                            },
                            "Delete category"
                        }
                    }
                    }
                    if current_library_tool == LibraryToolTab::Config {
                    section {
                        style: "display: grid; gap: 8px; margin-bottom: 18px; border: 1px solid #2b352d; background: #111710; border-radius: 6px; padding: 10px;",
                        h3 {
                            style: "font-size: 12px; line-height: 16px; color: #9fb0a3; text-transform: uppercase; margin: 0;",
                            "Config"
                        }
                        label {
                            style: "display: inline-flex; align-items: center; gap: 6px; color: #cbd5e1; font-size: 13px;",
                            input {
                                r#type: "checkbox",
                                checked: *show_hidden.read(),
                                onchange: move |event| {
                                    show_hidden.set(event.checked());
                                },
                            }
                            "Show hidden mods"
                        }
                        button {
                            style: "border: 1px solid #3a4756; background: #202832; color: #edf2f7; border-radius: 6px; padding: 8px 10px;",
                            onclick: move |_| {
                                if let Some(config_path) = pick_legacy_ts_config_file() {
                                    let state = app_state.read().clone();
                                    match import_legacy_ts_config_into_app(&state, &config_path) {
                                        Ok(imported) => {
                                            let mut next_state = state.clone();
                                            let _ = next_state.apply(CoreCommand::ReplaceMods { mods: imported.mods });
                                            app_state.set(next_state);
                                            if let Some(imported_game_folder) = imported.game_folder {
                                                game_folder.set(Some(imported_game_folder));
                                            }
                                            launch_options.set(imported.launch_options);
                                            saved_presets.set(load_preset_names());
                                            let categories = load_category_names();
                                            let next_category = categories.first().cloned().unwrap_or_else(|| "Core".to_string());
                                            category_name.set(next_category.clone());
                                            selected_category_name.set(next_category);
                                            saved_categories.set(categories);
                                            mod_status.set(Some(imported.status));
                                        }
                                        Err(error) => mod_status.set(Some(format!("Could not import TS config: {}", error.message))),
                                    }
                                }
                            },
                            "Import TS config"
                        }
                        button {
                            style: "border: 1px solid #3a4756; background: #202832; color: #edf2f7; border-radius: 6px; padding: 8px 10px;",
                            onclick: move |_| {
                                if let Some(config_path) = pick_legacy_ts_config_save_file() {
                                    let state = app_state.read().clone();
                                    let options = launch_options.read().clone();
                                    match export_legacy_ts_config_from_app(&state, &options, &config_path) {
                                        Ok(status) => mod_status.set(Some(status)),
                                        Err(error) => mod_status.set(Some(format!("Could not export TS config: {}", error.message))),
                                    }
                                }
                            },
                            "Export TS config"
                        }
                    }
                    }
                    } else if false {
                        header {
                            style: "display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: end; gap: 16px; padding: 32px 40px 20px; border-bottom: 1px solid #262838;",
                            div {
                                style: "min-width: 0;",
                                h2 {
                                    style: "font-size: 25px; line-height: 32px; margin: 0; color: #f2f5f2;",
                                    "Mod Detail"
                                }
                                div {
                                    style: "font-size: 14px; line-height: 20px; color: #cbd5c9; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                    if let Some(selected_mod) = selected_mod.as_ref() {
                                        "{selected_mod.display_name}"
                                    } else {
                                        "Select a mod from the library to inspect it here."
                                    }
                                }
                            }
                            button {
                                style: "border: 1px solid #3b3d4d; background: #343541; color: #f2f5f2; border-radius: 5px; padding: 10px 14px; font-size: 13px;",
                                onclick: move |_| {
                                    workspace_page.set(WorkspacePage::Mods);
                                },
                                "Back to library"
                            }
                        }
                        if let Some(status_message) = view_model.status_message.clone() {
                            div {
                                style: "border: 1px solid #303346; background: #1a1b25; border-radius: 5px; padding: 8px 10px; margin: 16px 40px; color: #aeb8c8; font-size: 13px;",
                                "{status_message}"
                            }
                        }
                        div {
                            style: "display: grid; gap: 22px; max-width: 980px; padding: 8px 40px 40px;",
                            section {
                                style: "border: 1px solid #303241; border-radius: 8px; background: #1f202b; overflow: hidden;",
                                header {
                                    style: "display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 18px 20px; background: #2a2b37; border-bottom: 1px solid #303241;",
                                    div {
                                        style: "display: grid; gap: 4px; min-width: 0;",
                                        h3 {
                                            style: "font-size: 19px; line-height: 24px; margin: 0; color: #f2f5f2;",
                                            "Selected Archive"
                                        }
                                        div {
                                            style: "font-size: 12px; color: #aeb8c8;",
                                            "Source, metadata, categories, and row operations"
                                        }
                                    }
                                }
                                if let Some(selected_mod) = selected_mod.clone() {
                                    div {
                                        style: "display: grid; grid-template-columns: minmax(160px, 220px) minmax(0, 1fr); gap: 20px; padding: 20px;",
                                        div {
                                            style: "display: grid; gap: 12px; align-content: start; min-width: 0;",
                                            div {
                                                style: detail_source_tile_style(&selected_mod),
                                                div {
                                                    style: "font-size: 28px; line-height: 32px; font-weight: 800; letter-spacing: 0;",
                                                    "{mod_source_label(&selected_mod)}"
                                                }
                                                div {
                                                    style: "font-size: 12px; line-height: 16px; color: #d7ded9;",
                                                    "{mod_state_label(&selected_mod)}"
                                                }
                                            }
                                            div {
                                                style: "display: grid; gap: 8px; font-size: 13px;",
                                                div {
                                                    style: "display: flex; justify-content: space-between; gap: 12px; color: #aeb8c8;",
                                                    span { "Author" }
                                                    strong {
                                                        style: "color: #f2f5f2; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                                        "{mod_author_label(&selected_mod, &current_steam_metadata)}"
                                                    }
                                                }
                                                div {
                                                    style: "display: flex; justify-content: space-between; gap: 12px; color: #aeb8c8;",
                                                    span { "Updated" }
                                                    strong {
                                                        style: "color: #f2f5f2; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                                        "{mod_updated_label(&selected_mod, &current_steam_metadata, current_time_ms)}"
                                                    }
                                                }
                                                div {
                                                    style: "display: flex; justify-content: space-between; gap: 12px; color: #aeb8c8;",
                                                    span { "Source" }
                                                    strong {
                                                        style: "color: #f2f5f2;",
                                                        "{mod_source_label(&selected_mod)}"
                                                    }
                                                }
                                            }
                                        }
                                        div {
                                            style: "display: grid; gap: 16px; min-width: 0;",
                                            div {
                                                style: "display: grid; gap: 6px; min-width: 0;",
                                                h3 {
                                                    style: "font-size: 26px; line-height: 32px; margin: 0; color: #f2f5f2; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                                    "{selected_mod.display_name}"
                                                }
                                                div {
                                                    style: "font-size: 13px; color: #aeb8c8; overflow-wrap: anywhere; font-family: ui-monospace, SFMono-Regular, Menlo, monospace;",
                                                    "{selected_mod.subtitle}"
                                                }
                                            }
                                            div {
                                                style: "display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 10px;",
                                                div {
                                                    style: detail_metric_style(),
                                                    span { "State" }
                                                    strong { "{mod_state_label(&selected_mod)}" }
                                                }
                                                div {
                                                    style: detail_metric_style(),
                                                    span { "Categories" }
                                                    strong { "{mod_categories_label(&selected_mod)}" }
                                                }
                                                div {
                                                    style: detail_metric_style(),
                                                    span { "Tags" }
                                                    strong { "{selected_mod.tags.len()}" }
                                                }
                                            }
                                            div {
                                                style: "display: grid; gap: 8px;",
                                                div {
                                                    style: "font-size: 11px; line-height: 14px; color: #aeb8c8; text-transform: uppercase; letter-spacing: 1px;",
                                                    "Tags"
                                                }
                                                if selected_mod.tags.is_empty() {
                                                    div {
                                                        style: "font-size: 13px; color: #778194;",
                                                        "No tags recorded"
                                                    }
                                                } else {
                                                    div {
                                                        style: "display: flex; flex-wrap: wrap; gap: 6px;",
                                                        for tag in selected_mod.tags.iter() {
                                                            span {
                                                                key: "{tag}",
                                                                style: "border: 1px solid #3b3d4d; border-radius: 4px; padding: 4px 7px; font-size: 12px; color: #d8ded8; background: #292a35;",
                                                                "{tag}"
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            div {
                                                style: "display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 8px;",
                                                button {
                                                    style: detail_action_button_style(false),
                                                    disabled: selected_mod.locked,
                                                    onclick: {
                                                        let mod_key = selected_mod.key.clone();
                                                        move |_| {
                                                            let mut next_state = app_state.read().clone();
                                                            if let Some(identity) = identity_for_mod_key(&next_state, &mod_key) {
                                                                match next_state.apply(CoreCommand::ToggleMod { identity }) {
                                                                    Ok(_) => {
                                                                        match save_mod_state(&next_state) {
                                                                            Ok(status) => mod_status.set(Some(status)),
                                                                            Err(error) => mod_status.set(Some(format!("Could not save mod state: {}", error.message))),
                                                                        }
                                                                        app_state.set(next_state);
                                                                    }
                                                                    Err(error) => mod_status.set(Some(format!("Could not toggle mod: {}", error.message))),
                                                                }
                                                            }
                                                        }
                                                    },
                                                    if selected_mod.enabled {
                                                        "Disable"
                                                    } else {
                                                        "Enable"
                                                    }
                                                }
                                                button {
                                                    style: detail_action_button_style(false),
                                                    onclick: {
                                                        let mod_key = selected_mod.key.clone();
                                                        move |_| toggle_mod_hidden_by_key(&mut app_state, &mut mod_status, &mod_key)
                                                    },
                                                    if selected_mod.hidden {
                                                        "Show"
                                                    } else {
                                                        "Hide"
                                                    }
                                                }
                                                button {
                                                    style: detail_action_button_style(false),
                                                    disabled: selected_mod.locked,
                                                    onclick: {
                                                        let mod_key = selected_mod.key.clone();
                                                        move |_| move_mod_by_delta(&mut app_state, &mut mod_status, &mod_key, -1)
                                                    },
                                                    "Move up"
                                                }
                                                button {
                                                    style: detail_action_button_style(false),
                                                    disabled: selected_mod.locked,
                                                    onclick: {
                                                        let mod_key = selected_mod.key.clone();
                                                        move |_| move_mod_by_delta(&mut app_state, &mut mod_status, &mod_key, 1)
                                                    },
                                                    "Move down"
                                                }
                                                button {
                                                    style: detail_action_button_style(false),
                                                    onclick: {
                                                        let mod_key = selected_mod.key.clone();
                                                        move |_| toggle_mod_lock_by_key(&mut app_state, &mut mod_status, &mod_key)
                                                    },
                                                    if selected_mod.locked {
                                                        "Unlock"
                                                    } else {
                                                        "Lock"
                                                    }
                                                }
                                                button {
                                                    style: detail_action_button_style(false),
                                                    onclick: {
                                                        let mod_key = selected_mod.key.clone();
                                                        move |_| {
                                                            let category = category_name.read().trim().to_string();
                                                            let color = category_color.read().trim().to_string();
                                                            add_mod_category_by_key(
                                                                &mut app_state,
                                                                &mut mod_status,
                                                                &mut saved_categories,
                                                                &mod_key,
                                                                &category,
                                                                &color,
                                                            )
                                                        }
                                                    },
                                                    "Add category"
                                                }
                                                button {
                                                    style: detail_action_button_style(false),
                                                    onclick: {
                                                        let mod_key = selected_mod.key.clone();
                                                        move |_| {
                                                            let category = category_name.read().trim().to_string();
                                                            remove_mod_category_by_key(
                                                                &mut app_state,
                                                                &mut mod_status,
                                                                &mut saved_categories,
                                                                &mod_key,
                                                                &category,
                                                            )
                                                        }
                                                    },
                                                    "Remove category"
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    div {
                                        style: "display: grid; gap: 12px; padding: 22px 20px; color: #aeb8c8;",
                                        h3 {
                                            style: "font-size: 17px; line-height: 22px; margin: 0; color: #edf2f7;",
                                            "No mod selected"
                                        }
                                        div {
                                            style: "font-size: 13px; line-height: 18px;",
                                            "Load game mods or open a mod folder, then select a row from the library."
                                        }
                                    }
                                }
                            }
                        }
                    } else if current_workspace_page == WorkspacePage::ModDetail {
                        header {
                            style: "display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: center; gap: 16px; padding: 32px 40px 20px; border-bottom: 1px solid #263041;",
                            div {
                                style: "min-width: 0; display: grid; gap: 7px;",
                                h2 {
                                    style: "font-size: 25px; line-height: 32px; margin: 0; color: #f8fafc;",
                                    "Mod Detail"
                                }
                                div {
                                    style: "font-size: 14px; line-height: 20px; color: #bccabb; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                    if let Some(selected_mod) = selected_mod.as_ref() {
                                        "{selected_mod.display_name}"
                                    } else {
                                        "Select a mod from the archive to inspect it here."
                                    }
                                }
                            }
                            button {
                                style: settings_secondary_button_style(),
                                onclick: move |_| {
                                    workspace_page.set(WorkspacePage::Mods);
                                },
                                "Back to mods"
                            }
                        }
                        if let Some(status_message) = view_model.status_message.clone() {
                            div {
                                style: "border: 1px solid #303746; background: #171b24; border-radius: 4px; padding: 8px 10px; margin: 16px 40px; color: #aeb8c8; font-size: 13px;",
                                "{status_message}"
                            }
                        }
                        div {
                            style: "display: grid; gap: 22px; max-width: 980px; padding: 18px 40px 40px;",
                            if let Some(selected_mod) = selected_mod.clone() {
                                section {
                                    style: "display: grid; grid-template-columns: minmax(180px, 240px) minmax(0, 1fr); gap: 22px; align-items: start;",
                                    div {
                                        style: "display: grid; gap: 14px; min-width: 0;",
                                        div {
                                            style: detail_source_tile_style(&selected_mod),
                                            div {
                                                style: "font-size: 30px; line-height: 34px; font-weight: 800; letter-spacing: 0;",
                                                "{mod_source_label(&selected_mod)}"
                                            }
                                            div {
                                                style: "font-size: 12px; line-height: 16px; color: #d7ded9;",
                                                "{mod_state_label(&selected_mod)}"
                                            }
                                        }
                                        div {
                                            style: settings_card_style(),
                                            div {
                                                style: "display: grid; gap: 10px; padding: 14px; font-size: 13px;",
                                                div {
                                                    style: "display: flex; justify-content: space-between; gap: 12px; color: #aeb8c8;",
                                                    span { "Author" }
                                                    strong {
                                                        style: "color: #f8fafc; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                                        "{mod_author_label(&selected_mod, &current_steam_metadata)}"
                                                    }
                                                }
                                                div {
                                                    style: "display: flex; justify-content: space-between; gap: 12px; color: #aeb8c8;",
                                                    span { "Updated" }
                                                    strong {
                                                        style: "color: #f8fafc; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                                        "{mod_updated_label(&selected_mod, &current_steam_metadata, current_time_ms)}"
                                                    }
                                                }
                                                div {
                                                    style: "display: flex; justify-content: space-between; gap: 12px; color: #aeb8c8;",
                                                    span { "Source" }
                                                    strong {
                                                        style: "color: #f8fafc;",
                                                        "{mod_source_label(&selected_mod)}"
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    div {
                                        style: "display: grid; gap: 18px; min-width: 0;",
                                        div {
                                            style: "display: grid; gap: 8px; min-width: 0;",
                                            h3 {
                                                style: "font-size: 28px; line-height: 34px; margin: 0; color: #f8fafc; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                                "{selected_mod.display_name}"
                                            }
                                            div {
                                                style: "font-size: 13px; color: #aeb8c8; overflow-wrap: anywhere; font-family: ui-monospace, SFMono-Regular, Menlo, monospace;",
                                                "{selected_mod.subtitle}"
                                            }
                                        }
                                        div {
                                            style: "display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 10px;",
                                            div {
                                                style: detail_metric_style(),
                                                span { "State" }
                                                strong { "{mod_state_label(&selected_mod)}" }
                                            }
                                            div {
                                                style: detail_metric_style(),
                                                span { "Categories" }
                                                strong { "{mod_categories_label(&selected_mod)}" }
                                            }
                                            div {
                                                style: detail_metric_style(),
                                                span { "Tags" }
                                                strong { "{selected_mod.tags.len()}" }
                                            }
                                        }
                                        div {
                                            style: "display: grid; gap: 8px;",
                                            div {
                                                style: "font-size: 11px; line-height: 14px; color: #aeb8c8; text-transform: uppercase; letter-spacing: 0;",
                                                "Tags"
                                            }
                                            if selected_mod.tags.is_empty() {
                                                div {
                                                    style: "font-size: 13px; color: #778194;",
                                                    "No tags recorded"
                                                }
                                            } else {
                                                div {
                                                    style: "display: flex; flex-wrap: wrap; gap: 6px;",
                                                    for tag in selected_mod.tags.iter() {
                                                        span {
                                                            key: "{tag}",
                                                            style: "border: 1px solid #303746; border-radius: 4px; padding: 4px 7px; font-size: 12px; color: #d8ded8; background: #1f2430;",
                                                            "{tag}"
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        div {
                                            style: "display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 8px;",
                                            button {
                                                style: detail_action_button_style(false),
                                                disabled: selected_mod.locked,
                                                onclick: {
                                                    let mod_key = selected_mod.key.clone();
                                                    move |_| {
                                                        let mut next_state = app_state.read().clone();
                                                        if let Some(identity) = identity_for_mod_key(&next_state, &mod_key) {
                                                            match next_state.apply(CoreCommand::ToggleMod { identity }) {
                                                                Ok(_) => {
                                                                    match save_mod_state(&next_state) {
                                                                        Ok(status) => mod_status.set(Some(status)),
                                                                        Err(error) => mod_status.set(Some(format!("Could not save mod state: {}", error.message))),
                                                                    }
                                                                    app_state.set(next_state);
                                                                }
                                                                Err(error) => mod_status.set(Some(format!("Could not toggle mod: {}", error.message))),
                                                            }
                                                        }
                                                    }
                                                },
                                                if selected_mod.enabled {
                                                    "Disable"
                                                } else {
                                                    "Enable"
                                                }
                                            }
                                            button {
                                                style: detail_action_button_style(false),
                                                onclick: {
                                                    let mod_key = selected_mod.key.clone();
                                                    move |_| toggle_mod_hidden_by_key(&mut app_state, &mut mod_status, &mod_key)
                                                },
                                                if selected_mod.hidden {
                                                    "Show"
                                                } else {
                                                    "Hide"
                                                }
                                            }
                                            button {
                                                style: detail_action_button_style(false),
                                                disabled: selected_mod.locked,
                                                onclick: {
                                                    let mod_key = selected_mod.key.clone();
                                                    move |_| move_mod_by_delta(&mut app_state, &mut mod_status, &mod_key, -1)
                                                },
                                                "Move up"
                                            }
                                            button {
                                                style: detail_action_button_style(false),
                                                disabled: selected_mod.locked,
                                                onclick: {
                                                    let mod_key = selected_mod.key.clone();
                                                    move |_| move_mod_by_delta(&mut app_state, &mut mod_status, &mod_key, 1)
                                                },
                                                "Move down"
                                            }
                                            button {
                                                style: detail_action_button_style(false),
                                                onclick: {
                                                    let mod_key = selected_mod.key.clone();
                                                    move |_| toggle_mod_lock_by_key(&mut app_state, &mut mod_status, &mod_key)
                                                },
                                                if selected_mod.locked {
                                                    "Unlock"
                                                } else {
                                                    "Lock"
                                                }
                                            }
                                            button {
                                                style: detail_action_button_style(false),
                                                onclick: {
                                                    let mod_key = selected_mod.key.clone();
                                                    move |_| {
                                                        let category = category_name.read().trim().to_string();
                                                        let color = category_color.read().trim().to_string();
                                                        add_mod_category_by_key(
                                                            &mut app_state,
                                                            &mut mod_status,
                                                            &mut saved_categories,
                                                            &mod_key,
                                                            &category,
                                                            &color,
                                                        )
                                                    }
                                                },
                                                "Add category"
                                            }
                                            button {
                                                style: detail_action_button_style(false),
                                                onclick: {
                                                    let mod_key = selected_mod.key.clone();
                                                    move |_| {
                                                        let category = category_name.read().trim().to_string();
                                                        remove_mod_category_by_key(
                                                            &mut app_state,
                                                            &mut mod_status,
                                                            &mut saved_categories,
                                                            &mod_key,
                                                            &category,
                                                        )
                                                    }
                                                },
                                                "Remove category"
                                            }
                                        }
                                    }
                                }
                            } else {
                                section {
                                    style: "display: grid; gap: 12px; border: 1px solid #293142; border-radius: 4px; background: #171b24; padding: 22px 20px; color: #aeb8c8;",
                                    h3 {
                                        style: "font-size: 17px; line-height: 22px; margin: 0; color: #edf2f7;",
                                        "No mod selected"
                                    }
                                    div {
                                        style: "font-size: 13px; line-height: 18px;",
                                        "Load game mods or open a mod folder, then select a row from the archive."
                                    }
                                }
                            }
                        }
                    } else {
                        div {
                            style: "display: grid; gap: 8px; margin-top: 28px; padding-top: 18px; border-top: 1px solid #2b2d3a;",
                            div {
                                style: "font-size: 12px; line-height: 17px; color: #aeb8c8; overflow-wrap: anywhere;",
                                "{current_game_folder_label}"
                            }
                            button {
                                style: "width: 100%; border: 1px solid #3b3d4d; background: #343541; color: #f2f5f2; border-radius: 5px; padding: 10px 12px; font-size: 13px;",
                                onclick: move |_| {
                                    workspace_page.set(WorkspacePage::Mods);
                                    mod_list_filter.set(ModListFilter::All);
                                },
                                "Back to mods"
                            }
                        }
                    }
                    div {
                        style: "margin-top: auto; padding-top: 18px; border-top: 1px solid #2b2d3a; display: grid; gap: 8px;",
                        button {
                            style: library_utility_button_style(current_workspace_page == WorkspacePage::Checks),
                            onclick: move |_| {
                                workspace_page.set(WorkspacePage::Checks);
                            },
                            span { style: nav_badge_style(), "CHK" }
                            span { "Alpha checks" }
                        }
                        button {
                            style: library_utility_button_style(current_workspace_page == WorkspacePage::Settings),
                            onclick: move |_| {
                                workspace_page.set(WorkspacePage::Settings);
                                library_tool_tab.set(LibraryToolTab::None);
                            },
                            span { style: nav_badge_style(), "CFG" }
                            span { "Settings" }
                        }
                    }
                }
                aside {
                    style: "grid-area: tools; min-width: 0; border-left: 1px solid #263041; background: #171b24; padding: 20px 18px; overflow-y: auto;",
                    div {
                        style: "display: grid; gap: 4px; margin-bottom: 12px;",
                        h2 {
                            style: "font-size: 18px; line-height: 24px; margin: 0; color: #edf2f7;",
                            "Play & Tools"
                        }
                        div {
                            style: "font-size: 11px; line-height: 15px; color: #9fb0a3; text-transform: uppercase;",
                            "Instance: Default"
                        }
                    }
                    button {
                        style: "width: 100%; border: 1px solid #4ade80; background: #65f58b; color: #051d0c; border-radius: 6px; padding: 16px 14px; font-weight: 800; font-size: 18px; line-height: 24px; margin-bottom: 14px; letter-spacing: 0;",
                        onclick: move |_| {
                            let selected_game_folder = select_game_folder(game_folder.read().clone());
                            if let Some(selected_game_folder) = selected_game_folder {
                                let state = app_state.read().clone();
                                let launch_options = launch_options.read().clone();
                                let save_name = launch_save_name.read().clone();
                                let close_on_play = launch_options.close_on_play;
                                match save_game_folder(&selected_game_folder)
                                    .and_then(|_| launch_game_for_game_folder(&state, selected_game_folder.clone(), &launch_options, &save_name))
                                {
                                    Ok(status) => {
                                        game_folder.set(Some(selected_game_folder));
                                        saved_presets.set(load_preset_names());
                                        schedule_close_on_play_if_requested(close_on_play);
                                        mod_status.set(Some(launch_status_with_close_on_play(
                                            status,
                                            close_on_play,
                                        )));
                                    }
                                    Err(error) => mod_status.set(Some(format!("Could not launch game: {}", error.message))),
                                }
                            }
                        },
                        "PLAY GAME"
                    }
                    button {
                        style: continue_save_button_style(current_launch_save_name.trim().is_empty()),
                        onclick: move |_| {
                            let save_name = launch_save_name.read().trim().to_string();
                            if save_name.is_empty() {
                                mod_status.set(Some("Set a campaign save name in Settings before continuing a save.".to_string()));
                                workspace_page.set(WorkspacePage::Settings);
                                library_tool_tab.set(LibraryToolTab::None);
                                return;
                            }
                            let selected_game_folder = select_game_folder(game_folder.read().clone());
                            if let Some(selected_game_folder) = selected_game_folder {
                                let state = app_state.read().clone();
                                let launch_options = launch_options.read().clone();
                                let close_on_play = launch_options.close_on_play;
                                match save_game_folder(&selected_game_folder)
                                    .and_then(|_| launch_game_for_game_folder(&state, selected_game_folder.clone(), &launch_options, &save_name))
                                {
                                    Ok(status) => {
                                        game_folder.set(Some(selected_game_folder));
                                        saved_presets.set(load_preset_names());
                                        schedule_close_on_play_if_requested(close_on_play);
                                        mod_status.set(Some(launch_status_with_close_on_play(
                                            status,
                                            close_on_play,
                                        )));
                                    }
                                    Err(error) => mod_status.set(Some(format!("Could not continue save: {}", error.message))),
                                }
                            }
                        },
                        span { "Continue Last Save" }
                        span { ">" }
                    }
                    div {
                        style: "display: grid; gap: 10px; margin-bottom: 18px;",
                        button {
                            style: tool_action_button_style(current_workspace_page == WorkspacePage::Compatibility),
                            onclick: move |_| {
                                let report = analyze_enabled_with_optional_schema(&app_state.read().mods);
                                mod_status.set(Some(conflict_status(&report)));
                                conflict_report.set(Some(report));
                                workspace_page.set(WorkspacePage::Compatibility);
                                library_tool_tab.set(LibraryToolTab::None);
                            },
                            span { "Check Compatibility" }
                            span { ">" }
                        }
                        button {
                            style: tool_action_button_style(current_workspace_page == WorkspacePage::Settings),
                            onclick: move |_| {
                                workspace_page.set(WorkspacePage::Settings);
                                library_tool_tab.set(LibraryToolTab::None);
                            },
                            span { "Launch Options" }
                            span { ">" }
                        }
                        button {
                            style: tool_action_button_style(current_workspace_page == WorkspacePage::ModDetail),
                            onclick: move |_| {
                                workspace_page.set(WorkspacePage::ModDetail);
                            },
                            span { "Mod Detail" }
                            span { ">" }
                        }
                        button {
                            style: tool_action_button_style(current_workspace_page == WorkspacePage::Steam),
                            onclick: move |_| {
                                workspace_page.set(WorkspacePage::Steam);
                            },
                            span { "Steam Helper" }
                            span { ">" }
                        }
                        button {
                            style: tool_action_button_style(current_workspace_page == WorkspacePage::Workshop),
                            onclick: move |_| {
                                workspace_page.set(WorkspacePage::Workshop);
                            },
                            span { "Workshop Commands" }
                            span { ">" }
                        }
                        button {
                            style: tool_action_button_style(current_workspace_page == WorkspacePage::Checks),
                            onclick: move |_| {
                                workspace_page.set(WorkspacePage::Checks);
                            },
                            span { "Alpha Checks" }
                            span { ">" }
                        }
                    }
                }
                section {
                    style: "grid-area: content; min-width: 0; min-height: 0; overflow: auto; padding: 0; background: #11111a;",
                    if current_workspace_page == WorkspacePage::Checks {
                        header {
                            style: "display: grid; gap: 8px; padding: 32px 40px 20px; border-bottom: 1px solid #262838;",
                            h2 {
                                style: "font-size: 25px; line-height: 32px; margin: 0; color: #f2f5f2;",
                                "Alpha Checks"
                            }
                            div {
                                style: "font-size: 14px; line-height: 20px; color: #cbd5c9;",
                                "Readiness checks for the local app, schema, helper, Steam paths, and WH3 install."
                            }
                        }
                        if let Some(status_message) = view_model.status_message.clone() {
                            div {
                                style: "border: 1px solid #303346; background: #1a1b25; border-radius: 5px; padding: 8px 10px; margin: 16px 40px; color: #aeb8c8; font-size: 13px;",
                                "{status_message}"
                            }
                        }
                        div {
                            style: "display: grid; gap: 24px; max-width: 900px; padding: 8px 40px 40px;",
                            AlphaReadinessPanel {
                                report: alpha_readiness.clone()
                            }
                        }
                    } else if current_workspace_page == WorkspacePage::Compatibility {
                        header {
                            style: "display: grid; gap: 8px; padding: 32px 40px 20px; border-bottom: 1px solid #262838;",
                            h2 {
                                style: "font-size: 25px; line-height: 32px; margin: 0; color: #f2f5f2;",
                                "Compatibility"
                            }
                            div {
                                style: "font-size: 14px; line-height: 20px; color: #cbd5c9;",
                                "Analyze enabled packs for file collisions, dependency misses, DB references, unique IDs, script listeners, and read errors."
                            }
                        }
                        if let Some(status_message) = view_model.status_message.clone() {
                            div {
                                style: "border: 1px solid #303346; background: #1a1b25; border-radius: 5px; padding: 8px 10px; margin: 16px 40px; color: #aeb8c8; font-size: 13px;",
                                "{status_message}"
                            }
                        }
                        div {
                            style: "display: grid; gap: 24px; padding: 8px 40px 40px;",
                            section {
                                style: settings_card_style(),
                                header {
                                    style: settings_card_header_style(),
                                    h3 {
                                        style: "font-size: 18px; line-height: 24px; margin: 0; color: #f2f5f2;",
                                        "Enabled Pack Analysis"
                                    }
                                    button {
                                        style: settings_primary_button_style(),
                                        onclick: move |_| {
                                            let report = analyze_enabled_with_optional_schema(&app_state.read().mods);
                                            mod_status.set(Some(conflict_status(&report)));
                                            conflict_report.set(Some(report));
                                        },
                                        "Run analysis"
                                    }
                                }
                                div {
                                    style: settings_card_body_style(),
                                    div {
                                        style: "font-size: 13px; line-height: 19px; color: #aeb8c8;",
                                        "{launch_enabled_mod_count} enabled mods will be analyzed. Results stay on this screen so the archive can remain focused on mod ordering."
                                    }
                                }
                            }
                            if let Some(report) = conflict_report.read().as_ref() {
                                ConflictPanel { report: report.clone() }
                            } else {
                                section {
                                    style: settings_card_style(),
                                    header {
                                        style: settings_card_header_style(),
                                        h3 {
                                            style: "font-size: 18px; line-height: 24px; margin: 0; color: #f2f5f2;",
                                            "No analysis yet"
                                        }
                                    }
                                    div {
                                        style: settings_card_body_style(),
                                        div {
                                            style: "font-size: 13px; line-height: 19px; color: #aeb8c8;",
                                            "Use Run analysis or the Analyze button in the archive toolbar."
                                        }
                                    }
                                }
                            }
                        }
                    } else if current_workspace_page == WorkspacePage::Steam {
                        header {
                            style: "display: grid; gap: 8px; padding: 32px 40px 20px; border-bottom: 1px solid #262838;",
                            h2 {
                                style: "font-size: 25px; line-height: 32px; margin: 0; color: #f2f5f2;",
                                "Steam Helper"
                            }
                            div {
                                style: "font-size: 14px; line-height: 20px; color: #cbd5c9;",
                                "Configure the helper executable and refresh workshop metadata from a dedicated screen."
                            }
                        }
                        if let Some(status_message) = view_model.status_message.clone() {
                            div {
                                style: "border: 1px solid #303346; background: #1a1b25; border-radius: 5px; padding: 8px 10px; margin: 16px 40px; color: #aeb8c8; font-size: 13px;",
                                "{status_message}"
                            }
                        }
                        div {
                            style: "display: grid; gap: 24px; max-width: 900px; padding: 8px 40px 40px;",
                            section {
                                style: settings_card_style(),
                                header {
                                    style: settings_card_header_style(),
                                    h3 {
                                        style: "font-size: 18px; line-height: 24px; margin: 0; color: #f2f5f2;",
                                        "Helper Configuration"
                                    }
                                }
                                div {
                                    style: settings_card_body_style(),
                                    input {
                                        style: settings_input_style(),
                                        value: "{steam_helper_path}",
                                        placeholder: "Steam helper executable path",
                                        oninput: move |event| {
                                            steam_helper_path.set(event.value());
                                        },
                                    }
                                    select {
                                        style: settings_input_style(),
                                        value: "{steam_helper_backend}",
                                        onchange: move |event| {
                                            steam_helper_backend.set(event.value());
                                        },
                                        option { value: STEAM_HELPER_BACKEND_NATIVE, "Native backend" }
                                        option { value: STEAM_HELPER_BACKEND_FIXTURE, "Fixture backend" }
                                    }
                                    div {
                                        style: "display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 10px;",
                                        button {
                                            style: settings_secondary_button_style(),
                                            onclick: move |_| {
                                                if let Some(helper_path) = pick_steam_helper_file() {
                                                    let helper_path = helper_path.display().to_string();
                                                    let backend = steam_helper_backend.read().clone();
                                                    match save_steam_helper_settings(&helper_path, &backend) {
                                                        Ok(status) => {
                                                            steam_helper_path.set(helper_path);
                                                            mod_status.set(Some(status));
                                                        }
                                                        Err(error) => mod_status.set(Some(format!("Could not save Steam helper: {}", error.message))),
                                                    }
                                                }
                                            },
                                            "Choose"
                                        }
                                        button {
                                            style: settings_secondary_button_style(),
                                            onclick: move |_| {
                                                let helper_path = steam_helper_path.read().trim().to_string();
                                                let backend = steam_helper_backend.read().clone();
                                                match save_steam_helper_settings(&helper_path, &backend) {
                                                    Ok(status) => mod_status.set(Some(status)),
                                                    Err(error) => mod_status.set(Some(format!("Could not save Steam helper: {}", error.message))),
                                                }
                                            },
                                            "Save"
                                        }
                                        button {
                                            style: settings_secondary_button_style(),
                                            disabled: steam_helper_path.read().trim().is_empty(),
                                            onclick: move |_| {
                                                let helper_path = PathBuf::from(steam_helper_path.read().trim().to_string());
                                                let backend = steam_helper_backend.read().clone();
                                                match probe_steam_helper(&helper_path, &backend) {
                                                    Ok(status) => mod_status.set(Some(status)),
                                                    Err(error) => mod_status.set(Some(format!("Could not probe Steam helper: {error}"))),
                                                }
                                            },
                                            "Probe"
                                        }
                                        button {
                                            style: settings_primary_button_style(),
                                            disabled: steam_helper_path.read().trim().is_empty(),
                                            onclick: move |_| {
                                                let helper_path = PathBuf::from(steam_helper_path.read().trim().to_string());
                                                let backend = steam_helper_backend.read().clone();
                                                let mut next_state = app_state.read().clone();
                                                match refresh_steam_from_helper(&mut next_state, &helper_path, &backend) {
                                                    Ok(result) => {
                                                        let command_panel = steam_refresh_panel_state(&result);
                                                        let status = format!(
                                                            "Steam refreshed: {} subscribed IDs, {} metadata rows ({} requested, {} missing), {} filtered, {} renamed.",
                                                            result.subscribed_ids.len(),
                                                            result.metadata.len(),
                                                            result.requested_metadata_count,
                                                            result.missing_metadata_count,
                                                            result.filtered_unsubscribed_count,
                                                            result.renamed_count
                                                        );
                                                        subscribed_workshop_ids.set(result.subscribed_ids);
                                                        steam_metadata.set(result.metadata);
                                                        last_steam_command.set(Some(command_panel));
                                                        app_state.set(next_state);
                                                        mod_status.set(Some(status));
                                                    }
                                                    Err(error) => mod_status.set(Some(format!("Could not refresh Steam: {error}"))),
                                                }
                                            },
                                            "Refresh"
                                        }
                                    }
                                    button {
                                        style: settings_secondary_button_style(),
                                        disabled: steam_helper_path.read().trim().is_empty(),
                                        onclick: move |_| {
                                            let helper_path = PathBuf::from(steam_helper_path.read().trim().to_string());
                                            let backend = steam_helper_backend.read().clone();
                                            match check_steam_updates_with_helper(&app_state.read(), &helper_path, &backend) {
                                                Ok(result) => {
                                                    mod_status.set(Some(steam_check_update_status(&result)));
                                                    last_steam_command.set(Some(steam_check_update_panel_state(&result)));
                                                }
                                                Err(error) => mod_status.set(Some(format!("Could not check Steam updates: {error}"))),
                                            }
                                        },
                                        "Check updates"
                                    }
                                }
                            }
                            SteamMetadataPanel {
                                helper_path: steam_helper_path.read().clone(),
                                subscribed_ids: subscribed_workshop_ids.read().clone(),
                                metadata: steam_metadata.read().clone(),
                            }
                            if let Some(command_panel) = last_steam_command.read().clone() {
                                SteamCommandPanel { state: command_panel }
                            }
                        }
                    } else if current_workspace_page == WorkspacePage::Workshop {
                        header {
                            style: "display: grid; gap: 8px; padding: 32px 40px 20px; border-bottom: 1px solid #262838;",
                            h2 {
                                style: "font-size: 25px; line-height: 32px; margin: 0; color: #f2f5f2;",
                                "Workshop Commands"
                            }
                            div {
                                style: "font-size: 14px; line-height: 20px; color: #cbd5c9;",
                                "Run bounded subscribe, download, unsubscribe, and resubscribe actions for workshop IDs."
                            }
                        }
                        if let Some(status_message) = view_model.status_message.clone() {
                            div {
                                style: "border: 1px solid #303346; background: #1a1b25; border-radius: 5px; padding: 8px 10px; margin: 16px 40px; color: #aeb8c8; font-size: 13px;",
                                "{status_message}"
                            }
                        }
                        div {
                            style: "display: grid; gap: 24px; max-width: 900px; padding: 8px 40px 40px;",
                            section {
                                style: settings_card_style(),
                                header {
                                    style: settings_card_header_style(),
                                    h3 {
                                        style: "font-size: 18px; line-height: 24px; margin: 0; color: #f2f5f2;",
                                        "Command Queue"
                                    }
                                }
                                div {
                                    style: settings_card_body_style(),
                                    input {
                                        style: settings_input_style(),
                                        value: "{steam_command_ids}",
                                        placeholder: "Workshop IDs separated by spaces, commas, or new lines",
                                        oninput: move |event| {
                                            steam_command_ids.set(event.value());
                                        },
                                    }
                                    div {
                                        style: "display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 10px;",
                                        button {
                                            style: settings_primary_button_style(),
                                            disabled: steam_helper_path.read().trim().is_empty(),
                                            onclick: move |_| {
                                                let helper_path = PathBuf::from(steam_helper_path.read().trim().to_string());
                                                let backend = steam_helper_backend.read().clone();
                                                let ids = steam_command_ids.read().clone();
                                                let mods = app_state.read().mods.clone();
                                                match run_steam_command_with_helper(SteamCommandAction::Subscribe, &helper_path, &backend, &ids, &mods) {
                                                    Ok(result) => {
                                                        mod_status.set(Some(result.status));
                                                        last_steam_command.set(Some(result.panel));
                                                    }
                                                    Err(error) => mod_status.set(Some(format!("Could not subscribe: {error}"))),
                                                }
                                            },
                                            "Subscribe"
                                        }
                                        button {
                                            style: settings_secondary_button_style(),
                                            disabled: steam_helper_path.read().trim().is_empty(),
                                            onclick: move |_| {
                                                let helper_path = PathBuf::from(steam_helper_path.read().trim().to_string());
                                                let backend = steam_helper_backend.read().clone();
                                                let ids = steam_command_ids.read().clone();
                                                let mods = app_state.read().mods.clone();
                                                match run_steam_command_with_helper(SteamCommandAction::Download, &helper_path, &backend, &ids, &mods) {
                                                    Ok(result) => {
                                                        mod_status.set(Some(result.status));
                                                        last_steam_command.set(Some(result.panel));
                                                    }
                                                    Err(error) => mod_status.set(Some(format!("Could not download: {error}"))),
                                                }
                                            },
                                            "Download"
                                        }
                                        button {
                                            style: settings_danger_button_style(),
                                            disabled: steam_helper_path.read().trim().is_empty(),
                                            onclick: move |_| {
                                                let helper_path = PathBuf::from(steam_helper_path.read().trim().to_string());
                                                let backend = steam_helper_backend.read().clone();
                                                let ids = steam_command_ids.read().clone();
                                                let mods = app_state.read().mods.clone();
                                                match run_steam_command_with_helper(SteamCommandAction::Unsubscribe, &helper_path, &backend, &ids, &mods) {
                                                    Ok(result) => {
                                                        mod_status.set(Some(result.status));
                                                        last_steam_command.set(Some(result.panel));
                                                    }
                                                    Err(error) => mod_status.set(Some(format!("Could not unsubscribe: {error}"))),
                                                }
                                            },
                                            "Unsubscribe"
                                        }
                                        button {
                                            style: settings_secondary_button_style(),
                                            disabled: steam_helper_path.read().trim().is_empty(),
                                            onclick: move |_| {
                                                let helper_path = PathBuf::from(steam_helper_path.read().trim().to_string());
                                                let backend = steam_helper_backend.read().clone();
                                                let ids = steam_command_ids.read().clone();
                                                let mods = app_state.read().mods.clone();
                                                match run_steam_command_with_helper(SteamCommandAction::Resubscribe, &helper_path, &backend, &ids, &mods) {
                                                    Ok(result) => {
                                                        mod_status.set(Some(result.status));
                                                        last_steam_command.set(Some(result.panel));
                                                    }
                                                    Err(error) => mod_status.set(Some(format!("Could not resubscribe: {error}"))),
                                                }
                                            },
                                            "Resubscribe"
                                        }
                                    }
                                }
                            }
                            if let Some(command_panel) = last_steam_command.read().clone() {
                                SteamCommandPanel { state: command_panel }
                            }
                        }
                    } else if current_workspace_page == WorkspacePage::Categories {
                        header {
                            style: "display: grid; gap: 8px; padding: 32px 40px 20px; border-bottom: 1px solid #262838;",
                            h2 {
                                style: "font-size: 25px; line-height: 32px; margin: 0; color: #f2f5f2;",
                                "Categories"
                            }
                            div {
                                style: "font-size: 14px; line-height: 20px; color: #cbd5c9;",
                                "Manage category labels used by the archive and mod detail screens."
                            }
                        }
                        if let Some(status_message) = view_model.status_message.clone() {
                            div {
                                style: "border: 1px solid #303346; background: #1a1b25; border-radius: 5px; padding: 8px 10px; margin: 16px 40px; color: #aeb8c8; font-size: 13px;",
                                "{status_message}"
                            }
                        }
                        div {
                            style: "display: grid; gap: 24px; max-width: 900px; padding: 8px 40px 40px;",
                            section {
                                style: settings_card_style(),
                                header {
                                    style: settings_card_header_style(),
                                    h3 {
                                        style: "font-size: 18px; line-height: 24px; margin: 0; color: #f2f5f2;",
                                        "Category Editor"
                                    }
                                }
                                div {
                                    style: settings_card_body_style(),
                                    div {
                                        style: "display: grid; grid-template-columns: minmax(0, 1.2fr) minmax(160px, 0.8fr); gap: 12px;",
                                        input {
                                            style: settings_input_style(),
                                            value: "{category_name}",
                                            placeholder: "Category name",
                                            oninput: move |event| {
                                                category_name.set(event.value());
                                            },
                                        }
                                        select {
                                            style: settings_input_style(),
                                            value: "{category_color}",
                                            onchange: move |event| {
                                                category_color.set(event.value());
                                            },
                                            option { value: "blue", "Blue" }
                                            option { value: "green", "Green" }
                                            option { value: "yellow", "Yellow" }
                                            option { value: "red", "Red" }
                                            option { value: "purple", "Purple" }
                                            option { value: "gray", "Gray" }
                                        }
                                    }
                                    div {
                                        style: "display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 12px;",
                                        select {
                                            style: settings_input_style(),
                                            value: "{selected_category_name}",
                                            onchange: move |event| {
                                                let value = event.value();
                                                if !value.trim().is_empty() {
                                                    selected_category_name.set(value.clone());
                                                    category_name.set(value);
                                                }
                                            },
                                            option {
                                                value: "",
                                                "Saved categories"
                                            }
                                            for saved_category in saved_categories.read().iter() {
                                                option {
                                                    value: "{saved_category}",
                                                    "{saved_category}"
                                                }
                                            }
                                        }
                                        button {
                                            style: settings_secondary_button_style(),
                                            onclick: move |_| {
                                                selected_category_name.set("Core".to_string());
                                                category_name.set("Core".to_string());
                                                category_color.set("blue".to_string());
                                            },
                                            "Reset"
                                        }
                                    }
                                    div {
                                        style: "display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 10px;",
                                        button {
                                            style: settings_primary_button_style(),
                                            onclick: move |_| {
                                                let category = category_name.read().trim().to_string();
                                                let color = category_color.read().trim().to_string();
                                                match save_category_definition(&category, &color) {
                                                    Ok(status) => {
                                                        saved_categories.set(load_category_names());
                                                        mod_status.set(Some(status));
                                                    }
                                                    Err(error) => mod_status.set(Some(format!("Could not save category: {}", error.message))),
                                                }
                                            },
                                            "Save"
                                        }
                                        button {
                                            style: settings_secondary_button_style(),
                                            onclick: move |_| {
                                                let old_category = selected_category_name.read().trim().to_string();
                                                let new_category = category_name.read().trim().to_string();
                                                let state = app_state.read().clone();
                                                match rename_category_definition(&old_category, &new_category, &state) {
                                                    Ok((mods, status)) => {
                                                        let mut next_state = app_state.read().clone();
                                                        let _ = next_state.apply(CoreCommand::ReplaceMods { mods });
                                                        app_state.set(next_state);
                                                        selected_category_name.set(new_category);
                                                        saved_categories.set(load_category_names());
                                                        mod_status.set(Some(status));
                                                    }
                                                    Err(error) => mod_status.set(Some(format!("Could not rename category: {}", error.message))),
                                                }
                                            },
                                            "Rename"
                                        }
                                        button {
                                            style: settings_danger_button_style(),
                                            onclick: move |_| {
                                                let category = selected_category_name.read().trim().to_string();
                                                let state = app_state.read().clone();
                                                match delete_category_definition(&category, &state) {
                                                    Ok((mods, status)) => {
                                                        let mut next_state = app_state.read().clone();
                                                        let _ = next_state.apply(CoreCommand::ReplaceMods { mods });
                                                        app_state.set(next_state);
                                                        let names = load_category_names();
                                                        let next_category = names.first().cloned().unwrap_or_else(|| "Core".to_string());
                                                        category_name.set(next_category.clone());
                                                        selected_category_name.set(next_category);
                                                        saved_categories.set(names);
                                                        mod_status.set(Some(status));
                                                    }
                                                    Err(error) => mod_status.set(Some(format!("Could not delete category: {}", error.message))),
                                                }
                                            },
                                            "Delete"
                                        }
                                    }
                                }
                            }
                            section {
                                style: settings_card_style(),
                                header {
                                    style: settings_card_header_style(),
                                    h3 {
                                        style: "font-size: 18px; line-height: 24px; margin: 0; color: #f2f5f2;",
                                        "Saved Categories"
                                    }
                                    span {
                                        style: "font-size: 12px; color: #aeb8c8;",
                                        "{saved_category_count} total"
                                    }
                                }
                                div {
                                    style: settings_card_body_style(),
                                    if category_summaries.is_empty() {
                                        div {
                                            style: "font-size: 13px; color: #9fb0c0;",
                                            "No category definitions saved yet."
                                        }
                                    } else {
                                        div {
                                            style: "display: grid; gap: 8px;",
                                            for (category, assigned_count) in category_summaries.iter() {
                                                div {
                                                    key: "{category}",
                                                    style: collection_row_style(),
                                                    strong { "{category}" }
                                                    span { "{assigned_count} assigned mods" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else if current_workspace_page == WorkspacePage::Collections {
                        header {
                            style: "display: grid; gap: 8px; padding: 32px 40px 20px; border-bottom: 1px solid #262838;",
                            h2 {
                                style: "font-size: 25px; line-height: 32px; margin: 0; color: #f2f5f2;",
                                "Collections"
                            }
                            div {
                                style: "font-size: 14px; line-height: 20px; color: #cbd5c9;",
                                "Save, load, and delete named mod presets without leaving the central workspace."
                            }
                        }
                        if let Some(status_message) = view_model.status_message.clone() {
                            div {
                                style: "border: 1px solid #303346; background: #1a1b25; border-radius: 5px; padding: 8px 10px; margin: 16px 40px; color: #aeb8c8; font-size: 13px;",
                                "{status_message}"
                            }
                        }
                        div {
                            style: "display: grid; gap: 24px; max-width: 900px; padding: 8px 40px 40px;",
                            section {
                                style: settings_card_style(),
                                header {
                                    style: settings_card_header_style(),
                                    h3 {
                                        style: "font-size: 18px; line-height: 24px; margin: 0; color: #f2f5f2;",
                                        "Preset Editor"
                                    }
                                }
                                div {
                                    style: settings_card_body_style(),
                                    input {
                                        style: settings_input_style(),
                                        value: "{preset_name}",
                                        placeholder: "Collection / preset name",
                                        oninput: move |event| {
                                            preset_name.set(event.value());
                                        },
                                    }
                                    select {
                                        style: settings_input_style(),
                                        value: "{preset_name}",
                                        onchange: move |event| {
                                            let value = event.value();
                                            if !value.trim().is_empty() {
                                                preset_name.set(value);
                                            }
                                        },
                                        option {
                                            value: "",
                                            "Saved collections"
                                        }
                                        for saved_preset in saved_presets.read().iter() {
                                            option {
                                                value: "{saved_preset}",
                                                "{saved_preset}"
                                            }
                                        }
                                    }
                                    div {
                                        style: "display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 10px;",
                                        button {
                                            style: settings_primary_button_style(),
                                            onclick: move |_| {
                                                let name = preset_name.read().trim().to_string();
                                                let state = app_state.read().clone();
                                                match save_named_preset(&name, &state) {
                                                    Ok(status) => {
                                                        saved_presets.set(load_preset_names());
                                                        mod_status.set(Some(status));
                                                    }
                                                    Err(error) => mod_status.set(Some(format!("Could not save preset: {}", error.message))),
                                                }
                                            },
                                            "Save"
                                        }
                                        button {
                                            style: settings_secondary_button_style(),
                                            onclick: move |_| {
                                                let name = preset_name.read().trim().to_string();
                                                let state = app_state.read().clone();
                                                match load_named_preset(&name, &state) {
                                                    Ok((mods, status)) => {
                                                        let mut next_state = state.clone();
                                                        let _ = next_state.apply(CoreCommand::ReplaceMods { mods });
                                                        match save_mod_state(&next_state) {
                                                            Ok(save_status) => mod_status.set(Some(format!("{status} {save_status}"))),
                                                            Err(error) => mod_status.set(Some(format!("{status} Could not save mod state: {}", error.message))),
                                                        }
                                                        app_state.set(next_state);
                                                    }
                                                    Err(error) => mod_status.set(Some(format!("Could not load preset: {}", error.message))),
                                                }
                                            },
                                            "Load"
                                        }
                                        button {
                                            style: settings_danger_button_style(),
                                            onclick: move |_| {
                                                let name = preset_name.read().trim().to_string();
                                                match delete_named_preset(&name) {
                                                    Ok(status) => {
                                                        let names = load_preset_names();
                                                        let next_name = if names.iter().any(|saved_name| saved_name == &name) {
                                                            name
                                                        } else {
                                                            names.first().cloned().unwrap_or_else(|| "Prototype preset".to_string())
                                                        };
                                                        preset_name.set(next_name);
                                                        saved_presets.set(names);
                                                        mod_status.set(Some(status));
                                                    }
                                                    Err(error) => mod_status.set(Some(format!("Could not delete preset: {}", error.message))),
                                                }
                                            },
                                            "Delete"
                                        }
                                    }
                                }
                            }
                            section {
                                style: settings_card_style(),
                                header {
                                    style: settings_card_header_style(),
                                    h3 {
                                        style: "font-size: 18px; line-height: 24px; margin: 0; color: #f2f5f2;",
                                        "Saved Collections"
                                    }
                                    span {
                                        style: "font-size: 12px; color: #aeb8c8;",
                                        "{saved_preset_count} total"
                                    }
                                }
                                div {
                                    style: settings_card_body_style(),
                                    if preset_summaries.is_empty() {
                                        div {
                                            style: "font-size: 13px; color: #9fb0c0;",
                                            "No collections saved yet."
                                        }
                                    } else {
                                        div {
                                            style: "display: grid; gap: 8px;",
                                            for preset in preset_summaries.iter() {
                                                button {
                                                    key: "{preset}",
                                                    style: collection_row_button_style(preset_name.read().as_str() == preset.as_str()),
                                                    onclick: {
                                                        let preset = preset.clone();
                                                        move |_| {
                                                            preset_name.set(preset.clone());
                                                        }
                                                    },
                                                    strong { "{preset}" }
                                                    span { "Saved load order" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else if current_workspace_page == WorkspacePage::Settings {
                        header {
                            style: "display: grid; gap: 8px; padding: 32px 40px 20px; border-bottom: 1px solid #262838;",
                            h2 {
                                style: "font-size: 25px; line-height: 32px; margin: 0; color: #f2f5f2;",
                                "Settings and Launch Options"
                            }
                            div {
                                style: "font-size: 14px; line-height: 20px; color: #cbd5c9;",
                                "Configure application behavior and game integration parameters."
                            }
                        }
                        if let Some(status_message) = view_model.status_message.clone() {
                            div {
                                style: "border: 1px solid #303346; background: #1a1b25; border-radius: 5px; padding: 8px 10px; margin: 16px 40px; color: #aeb8c8; font-size: 13px;",
                                "{status_message}"
                            }
                        }
                        div {
                            style: "display: grid; gap: 26px; max-width: 860px; padding: 8px 40px 40px;",
                            section {
                                style: settings_card_style(),
                                header {
                                    style: settings_card_header_style(),
                                    h3 {
                                        style: "font-size: 18px; line-height: 24px; margin: 0; color: #f2f5f2;",
                                        "General"
                                    }
                                }
                                div {
                                    style: settings_card_body_style(),
                                    div {
                                        style: settings_row_style(),
                                        div {
                                            style: "display: grid; gap: 4px;",
                                            strong { "Show hidden mods" }
                                            span {
                                                style: "font-size: 13px; color: #aeb8c8;",
                                                "Include hidden rows in the mod archive."
                                            }
                                        }
                                        label {
                                            style: toggle_label_style(*show_hidden.read()),
                                            input {
                                                style: "display: none;",
                                                r#type: "checkbox",
                                                checked: *show_hidden.read(),
                                                onchange: move |event| {
                                                    show_hidden.set(event.checked());
                                                },
                                            }
                                            span { if *show_hidden.read() { "ON" } else { "OFF" } }
                                        }
                                    }
                                    div {
                                        style: "display: grid; grid-template-columns: 1fr 1fr; gap: 10px;",
                                        button {
                                            style: settings_secondary_button_style(),
                                            onclick: move |_| {
                                                if let Some(config_path) = pick_legacy_ts_config_file() {
                                                    let state = app_state.read().clone();
                                                    match import_legacy_ts_config_into_app(&state, &config_path) {
                                                        Ok(imported) => {
                                                            let mut next_state = state.clone();
                                                            let _ = next_state.apply(CoreCommand::ReplaceMods { mods: imported.mods });
                                                            app_state.set(next_state);
                                                            if let Some(imported_game_folder) = imported.game_folder {
                                                                game_folder.set(Some(imported_game_folder));
                                                            }
                                                            launch_options.set(imported.launch_options);
                                                            saved_presets.set(load_preset_names());
                                                            let categories = load_category_names();
                                                            let next_category = categories.first().cloned().unwrap_or_else(|| "Core".to_string());
                                                            category_name.set(next_category.clone());
                                                            selected_category_name.set(next_category);
                                                            saved_categories.set(categories);
                                                            mod_status.set(Some(imported.status));
                                                        }
                                                        Err(error) => mod_status.set(Some(format!("Could not import TS config: {}", error.message))),
                                                    }
                                                }
                                            },
                                            "Import TS config"
                                        }
                                        button {
                                            style: settings_secondary_button_style(),
                                            onclick: move |_| {
                                                if let Some(config_path) = pick_legacy_ts_config_save_file() {
                                                    let state = app_state.read().clone();
                                                    let options = launch_options.read().clone();
                                                    match export_legacy_ts_config_from_app(&state, &options, &config_path) {
                                                        Ok(status) => mod_status.set(Some(status)),
                                                        Err(error) => mod_status.set(Some(format!("Could not export TS config: {}", error.message))),
                                                    }
                                                }
                                            },
                                            "Export TS config"
                                        }
                                    }
                                }
                            }
                            section {
                                style: settings_card_style(),
                                header {
                                    style: settings_card_header_style(),
                                    h3 {
                                        style: "font-size: 18px; line-height: 24px; margin: 0; color: #f2f5f2;",
                                        "Diagnostics"
                                    }
                                    button {
                                        style: settings_secondary_button_style(),
                                        onclick: move |_| {
                                            let state = app_state.read().clone();
                                            let game_folder_snapshot = game_folder.read().clone();
                                            let helper_path_snapshot = steam_helper_path.read().clone();
                                            let helper_backend_snapshot = steam_helper_backend.read().clone();
                                            let launch_options_snapshot = launch_options.read().clone();
                                            let launch_save_name_snapshot = launch_save_name.read().clone();
                                            let launch_preview_snapshot = launch_preview.read().clone();
                                            let last_steam_command_snapshot = last_steam_command.read().clone();
                                            let mod_status_snapshot = mod_status.read().clone();
                                            let (_, pack_status_snapshot) = pack_selection.read().clone();
                                            let status_snapshot = combined_status(
                                                mod_status_snapshot.as_deref(),
                                                pack_status_snapshot.as_deref(),
                                            );
                                            let readiness_snapshot = build_alpha_readiness_report(
                                                game_folder_snapshot.as_deref(),
                                                helper_path_snapshot.trim(),
                                            );

                                            match write_diagnostic_snapshot(DiagnosticSnapshotInput {
                                                app_state: &state,
                                                game_folder: game_folder_snapshot.as_deref(),
                                                helper_path: &helper_path_snapshot,
                                                helper_backend: &helper_backend_snapshot,
                                                launch_options: &launch_options_snapshot,
                                                launch_save_name: &launch_save_name_snapshot,
                                                status_message: status_snapshot.as_deref(),
                                                readiness: &readiness_snapshot,
                                                launch_preview: launch_preview_snapshot.as_ref(),
                                                last_steam_command: last_steam_command_snapshot.as_ref(),
                                            }) {
                                                Ok(path) => mod_status.set(Some(format!("Wrote diagnostic snapshot {}.", path.display()))),
                                                Err(error) => mod_status.set(Some(format!("Could not write diagnostic snapshot: {}", error.message))),
                                            }
                                        },
                                        "Write snapshot"
                                    }
                                }
                                div {
                                    style: settings_card_body_style(),
                                    div {
                                        style: "display: grid; gap: 8px;",
                                        label {
                                            style: "font-size: 14px; color: #e5e7eb;",
                                            "App log"
                                        }
                                        div {
                                            style: settings_path_box_style(),
                                            "{diagnostics_log_path_label}"
                                        }
                                    }
                                    div {
                                        style: "display: grid; gap: 8px;",
                                        label {
                                            style: "font-size: 14px; color: #e5e7eb;",
                                            "Steam helper command log"
                                        }
                                        div {
                                            style: settings_path_box_style(),
                                            "{steam_command_log_path_label}"
                                        }
                                    }
                                }
                            }
                            section {
                                style: settings_card_style(),
                                header {
                                    style: settings_card_header_style(),
                                    h3 {
                                        style: "font-size: 18px; line-height: 24px; margin: 0; color: #f2f5f2;",
                                        "Game Paths"
                                    }
                                    button {
                                        style: "border: 0; background: transparent; color: #bfdbfe; padding: 0; font-size: 13px;",
                                        onclick: move |_| {
                                            if let Some(discovered_game_folder) = auto_discover_game_folder() {
                                                match save_game_folder(&discovered_game_folder) {
                                                    Ok(status) => {
                                                        game_folder.set(Some(discovered_game_folder));
                                                        mod_status.set(Some(status));
                                                    }
                                                    Err(error) => mod_status.set(Some(format!("Could not save game folder: {}", error.message))),
                                                }
                                            } else {
                                                mod_status.set(Some("Could not auto-detect a WH3 Steam install on this machine.".to_string()));
                                            }
                                        },
                                        "Auto-detect"
                                    }
                                }
                                div {
                                    style: settings_card_body_style(),
                                    div {
                                        style: "display: grid; gap: 8px;",
                                        label {
                                            style: "font-size: 14px; color: #e5e7eb;",
                                            "Game folder"
                                        }
                                        div {
                                            style: "display: grid; grid-template-columns: minmax(0, 1fr) 96px; gap: 10px;",
                                            div {
                                                style: settings_path_box_style(),
                                                "{current_game_folder_label}"
                                            }
                                            button {
                                                style: settings_secondary_button_style(),
                                                onclick: move |_| {
                                                    if let Some(selected_game_folder) = pick_game_folder() {
                                                        match save_game_folder(&selected_game_folder) {
                                                            Ok(status) => {
                                                                game_folder.set(Some(selected_game_folder));
                                                                mod_status.set(Some(status));
                                                            }
                                                            Err(error) => mod_status.set(Some(format!("Could not save game folder: {}", error.message))),
                                                        }
                                                    }
                                                },
                                                "Browse"
                                            }
                                        }
                                    }
                                    div {
                                        style: "display: grid; gap: 8px;",
                                        label {
                                            style: "font-size: 14px; color: #e5e7eb;",
                                            "Steam helper executable"
                                        }
                                        input {
                                            style: settings_input_style(),
                                            value: "{steam_helper_path}",
                                            placeholder: "Steam helper executable path",
                                            oninput: move |event| {
                                                steam_helper_path.set(event.value());
                                            },
                                        }
                                    }
                                    div {
                                        style: "display: grid; grid-template-columns: minmax(0, 1fr) 96px 96px; gap: 10px;",
                                        select {
                                            style: settings_input_style(),
                                            value: "{steam_helper_backend}",
                                            onchange: move |event| {
                                                steam_helper_backend.set(event.value());
                                            },
                                            option { value: STEAM_HELPER_BACKEND_NATIVE, "Native backend" }
                                            option { value: STEAM_HELPER_BACKEND_FIXTURE, "Fixture backend" }
                                        }
                                        button {
                                            style: settings_secondary_button_style(),
                                            onclick: move |_| {
                                                if let Some(helper_path) = pick_steam_helper_file() {
                                                    let helper_path = helper_path.display().to_string();
                                                    let backend = steam_helper_backend.read().clone();
                                                    match save_steam_helper_settings(&helper_path, &backend) {
                                                        Ok(status) => {
                                                            steam_helper_path.set(helper_path);
                                                            mod_status.set(Some(status));
                                                        }
                                                        Err(error) => mod_status.set(Some(format!("Could not save Steam helper: {}", error.message))),
                                                    }
                                                }
                                            },
                                            "Choose"
                                        }
                                        button {
                                            style: settings_secondary_button_style(),
                                            onclick: move |_| {
                                                let helper_path = steam_helper_path.read().trim().to_string();
                                                let backend = steam_helper_backend.read().clone();
                                                match save_steam_helper_settings(&helper_path, &backend) {
                                                    Ok(status) => mod_status.set(Some(status)),
                                                    Err(error) => mod_status.set(Some(format!("Could not save Steam helper: {}", error.message))),
                                                }
                                            },
                                            "Save"
                                        }
                                    }
                                }
                            }
                            section {
                                style: settings_card_style(),
                                header {
                                    style: settings_card_header_style(),
                                    h3 {
                                        style: "font-size: 18px; line-height: 24px; margin: 0; color: #f2f5f2;",
                                        "Launch Options"
                                    }
                                }
                                div {
                                    style: settings_card_body_style(),
                                    div {
                                        style: settings_row_style(),
                                        div {
                                            style: "display: grid; gap: 4px;",
                                            strong { "Skip intro movies" }
                                            span { style: "font-size: 13px; color: #aeb8c8;", "Bypass startup cinematic sequences." }
                                        }
                                        label {
                                            style: toggle_label_style(current_launch_options.skip_intro_movies),
                                            input {
                                                style: "display: none;",
                                                r#type: "checkbox",
                                                checked: current_launch_options.skip_intro_movies,
                                                onchange: move |event| {
                                                    let mut next = launch_options.read().clone();
                                                    next.skip_intro_movies = event.checked();
                                                    launch_options.set(next);
                                                },
                                            }
                                            span { if current_launch_options.skip_intro_movies { "ON" } else { "OFF" } }
                                        }
                                    }
                                    div {
                                        style: settings_row_style(),
                                        div {
                                            style: "display: grid; gap: 4px;",
                                            strong { "Script logging" }
                                            span { style: "font-size: 13px; color: #aeb8c8;", "Enable WH3 script log output for debugging." }
                                        }
                                        label {
                                            style: toggle_label_style(current_launch_options.script_logging),
                                            input {
                                                style: "display: none;",
                                                r#type: "checkbox",
                                                checked: current_launch_options.script_logging,
                                                onchange: move |event| {
                                                    let mut next = launch_options.read().clone();
                                                    next.script_logging = event.checked();
                                                    launch_options.set(next);
                                                },
                                            }
                                            span { if current_launch_options.script_logging { "ON" } else { "OFF" } }
                                        }
                                    }
                                    div {
                                        style: settings_row_style(),
                                        div {
                                            style: "display: grid; gap: 4px;",
                                            strong { "Auto battle" }
                                            span { style: "font-size: 13px; color: #aeb8c8;", "Start a configured custom battle when launching." }
                                        }
                                        label {
                                            style: toggle_label_style(current_launch_options.auto_start_custom_battle),
                                            input {
                                                style: "display: none;",
                                                r#type: "checkbox",
                                                checked: current_launch_options.auto_start_custom_battle,
                                                onchange: move |event| {
                                                    let mut next = launch_options.read().clone();
                                                    next.auto_start_custom_battle = event.checked();
                                                    launch_options.set(next);
                                                },
                                            }
                                            span { if current_launch_options.auto_start_custom_battle { "ON" } else { "OFF" } }
                                        }
                                    }
                                    div {
                                        style: settings_row_style(),
                                        div {
                                            style: "display: grid; gap: 4px;",
                                            strong { "Make units generals" }
                                            span { style: "font-size: 13px; color: #aeb8c8;", "Generate the start-game pack from supported battle permission tables." }
                                        }
                                        label {
                                            style: toggle_label_style(current_launch_options.make_units_generals),
                                            input {
                                                style: "display: none;",
                                                r#type: "checkbox",
                                                checked: current_launch_options.make_units_generals,
                                                onchange: move |event| {
                                                    let mut next = launch_options.read().clone();
                                                    next.make_units_generals = event.checked();
                                                    launch_options.set(next);
                                                },
                                            }
                                            span { if current_launch_options.make_units_generals { "ON" } else { "OFF" } }
                                        }
                                    }
                                    div {
                                        style: settings_row_style(),
                                        div {
                                            style: "display: grid; gap: 4px;",
                                            strong { "High priority process" }
                                            span { style: "font-size: 13px; color: #aeb8c8;", "Request elevated process priority after launch on Windows." }
                                        }
                                        label {
                                            style: toggle_label_style(current_launch_options.high_process_priority),
                                            input {
                                                style: "display: none;",
                                                r#type: "checkbox",
                                                checked: current_launch_options.high_process_priority,
                                                onchange: move |event| {
                                                    let mut next = launch_options.read().clone();
                                                    next.high_process_priority = event.checked();
                                                    launch_options.set(next);
                                                },
                                            }
                                            span { if current_launch_options.high_process_priority { "ON" } else { "OFF" } }
                                        }
                                    }
                                    div {
                                        style: settings_row_style(),
                                        div {
                                            style: "display: grid; gap: 4px;",
                                            strong { "Close on play" }
                                            span { style: "font-size: 13px; color: #aeb8c8;", "Close this manager shortly after a successful launch." }
                                        }
                                        label {
                                            style: toggle_label_style(current_launch_options.close_on_play),
                                            input {
                                                style: "display: none;",
                                                r#type: "checkbox",
                                                checked: current_launch_options.close_on_play,
                                                onchange: move |event| {
                                                    let mut next = launch_options.read().clone();
                                                    next.close_on_play = event.checked();
                                                    launch_options.set(next);
                                                },
                                            }
                                            span { if current_launch_options.close_on_play { "ON" } else { "OFF" } }
                                        }
                                    }
                                    div {
                                        style: "display: grid; gap: 8px;",
                                        label {
                                            style: "font-size: 14px; color: #e5e7eb;",
                                            "Campaign save name"
                                        }
                                        input {
                                            style: settings_input_style(),
                                            value: "{current_launch_save_name}",
                                            placeholder: "Campaign save name",
                                            oninput: move |event| {
                                                launch_save_name.set(event.value());
                                            },
                                        }
                                    }
                                    div {
                                        style: "display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px;",
                                        button {
                                            style: launch_quick_button_style(),
                                            onclick: move |_| {
                                                let selected_game_folder = select_game_folder(game_folder.read().clone());
                                                if let Some(selected_game_folder) = selected_game_folder {
                                                    let state = app_state.read().clone();
                                                    let launch_options = launch_options.read().clone();
                                                    let save_name = launch_save_name.read().clone();
                                                    match save_game_folder(&selected_game_folder)
                                                        .and_then(|_| build_launch_preview(&state, selected_game_folder.clone(), &launch_options, &save_name))
                                                    {
                                                        Ok(preview) => {
                                                            game_folder.set(Some(selected_game_folder));
                                                            mod_status.set(Some(format!(
                                                                "Previewed {} enabled mods and {} generated packs for {}.",
                                                                preview.enabled_count,
                                                                preview.generated_packs.len(),
                                                                preview.mod_list_file_name
                                                            )));
                                                            launch_preview.set(Some(preview));
                                                        }
                                                        Err(error) => mod_status.set(Some(format!("Could not preview launch: {}", error.message))),
                                                    }
                                                }
                                            },
                                            "Preview launch"
                                        }
                                        button {
                                            style: launch_quick_button_style(),
                                            onclick: move |_| {
                                                let selected_game_folder = select_game_folder(game_folder.read().clone());
                                                if let Some(selected_game_folder) = selected_game_folder {
                                                    let state = app_state.read().clone();
                                                    let launch_options = launch_options.read().clone();
                                                    let save_name = launch_save_name.read().clone();
                                                    match save_game_folder(&selected_game_folder)
                                                        .and_then(|_| prepare_launch_for_game_folder(&state, selected_game_folder.clone(), &launch_options, &save_name))
                                                    {
                                                        Ok(status) => {
                                                            game_folder.set(Some(selected_game_folder));
                                                            mod_status.set(Some(status));
                                                        }
                                                        Err(error) => mod_status.set(Some(format!("Could not prepare launch files: {}", error.message))),
                                                    }
                                                }
                                            },
                                            "Prepare files"
                                        }
                                    }
                                    if let Some(preview) = launch_preview.read().as_ref() {
                                        LaunchPreviewPanel {
                                            preview: preview.clone(),
                                            is_stale: preview.fingerprint != current_launch_fingerprint,
                                        }
                                    }
                                }
                            }
                        }
                    } else {
            header {
                style: "min-height: 48px; display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: center; gap: 14px; padding: 8px 26px; border-bottom: 1px solid #262838; background: #11111a;",
                div {
                    style: "min-width: 0; display: flex; align-items: center; gap: 10px;",
                    strong {
                        style: "font-size: 11px; line-height: 15px; color: #d5d9df; text-transform: uppercase; letter-spacing: 1.5px; white-space: nowrap;",
                        "Archive"
                    }
                    div {
                        style: "font-size: 12px; line-height: 16px; color: #94a89b; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                        "{current_mod_filter_label}: {filtered_mod_count} shown / {visible_mod_count} visible / {launch_enabled_mod_count} launch enabled"
                    }
                }
                div {
                    style: "display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 6px;",
                    button {
                        style: archive_toolbar_button_style(false),
                        onclick: move |_| {
                            if let Some(pack_path) = pick_pack_file() {
                                pack_selection.set(load_pack_from_path(pack_path));
                            }
                        },
                        "Open pack"
                    }
                    button {
                        style: archive_toolbar_button_style(false),
                        onclick: move |_| {
                            if let Some(mod_folder) = pick_mod_folder() {
                                match load_mods_from_folder(mod_folder) {
                                    Ok((mods, status)) => {
                                        let mut next_state = app_state.read().clone();
                                        let _ = next_state.apply(CoreCommand::ReplaceMods { mods });
                                        app_state.set(next_state);
                                        mod_status.set(Some(status));
                                    }
                                    Err(error) => {
                                        mod_status.set(Some(format!("Could not discover mods: {}", error.message)));
                                    }
                                }
                            }
                        },
                        "Open mod folder"
                    }
                    button {
                        style: archive_toolbar_button_style(true),
                        onclick: move |_| {
                            let selected_game_folder = select_game_folder(game_folder.read().clone());
                            if let Some(selected_game_folder) = selected_game_folder {
                                match save_game_folder(&selected_game_folder)
                                    .and_then(|_| load_mods_from_game_folder(selected_game_folder.clone()))
                                {
                                    Ok((mods, status)) => {
                                        let mut next_state = app_state.read().clone();
                                        let _ = next_state.apply(CoreCommand::ReplaceMods { mods });
                                        app_state.set(next_state);
                                        game_folder.set(Some(selected_game_folder));
                                        mod_status.set(Some(status));
                                    }
                                    Err(error) => mod_status.set(Some(format!("Could not load game mods: {}", error.message))),
                                }
                            }
                        },
                        "Load game mods"
                    }
                    button {
                        style: archive_toolbar_button_style(false),
                        onclick: move |_| {
                            let report = analyze_enabled_with_optional_schema(&app_state.read().mods);
                            mod_status.set(Some(conflict_status(&report)));
                            conflict_report.set(Some(report));
                            workspace_page.set(WorkspacePage::Compatibility);
                        },
                        "Analyze"
                    }
                    if !mod_search_query.is_empty() {
                        button {
                            style: archive_toolbar_button_style(false),
                        onclick: move |_| {
                            mod_search.set(String::new());
                        },
                        "Clear search"
                    }
                }
                }
            }
            if let Some(status_message) = view_model.status_message {
                div {
                    style: "border: 1px solid #303346; background: #1a1b25; border-radius: 5px; padding: 8px 10px; margin: 14px 26px; color: #aeb8c8; font-size: 13px;",
                    "{status_message}"
                }
            }
            div {
                style: "display: grid; grid-template-columns: minmax(0, 1fr); gap: 16px; align-items: start; padding: 0 26px 26px;",
                div {
                    style: archive_filter_bar_style(),
                    span {
                        style: "font-size: 11px; color: #aeb8c8; text-transform: uppercase; letter-spacing: 0;",
                        "View"
                    }
                    button {
                        style: archive_filter_button_style(current_mod_filter == ModListFilter::All),
                        onclick: move |_| {
                            mod_list_filter.set(ModListFilter::All);
                            library_tool_tab.set(LibraryToolTab::None);
                        },
                        "All {total_mod_count}"
                    }
                    button {
                        style: archive_filter_button_style(current_mod_filter == ModListFilter::Enabled),
                        onclick: move |_| {
                            mod_list_filter.set(ModListFilter::Enabled);
                            library_tool_tab.set(LibraryToolTab::None);
                        },
                        "Enabled {enabled_mod_count}"
                    }
                    button {
                        style: archive_filter_button_style(current_mod_filter == ModListFilter::Disabled),
                        onclick: move |_| {
                            mod_list_filter.set(ModListFilter::Disabled);
                            library_tool_tab.set(LibraryToolTab::None);
                        },
                        "Disabled {disabled_mod_count}"
                    }
                    button {
                        style: archive_filter_button_style(current_mod_filter == ModListFilter::Locked),
                        onclick: move |_| {
                            mod_list_filter.set(ModListFilter::Locked);
                            library_tool_tab.set(LibraryToolTab::None);
                        },
                        "Locked {locked_mod_count}"
                    }
                    button {
                        style: archive_filter_button_style(current_mod_filter == ModListFilter::Hidden),
                        onclick: move |_| {
                            mod_list_filter.set(ModListFilter::Hidden);
                            library_tool_tab.set(LibraryToolTab::None);
                        },
                        "Hidden {hidden_mod_count}"
                    }
                }
                section {
                    style: "display: grid; gap: 6px; min-width: 0;",
                    div {
                        style: archive_table_header_style(),
                        div { "Ord" }
                        div { "Status" }
                        div { "Type" }
                        div { "Pack / Mod Name" }
                        div { "Author" }
                        div { "Updated" }
                    }
                    if all_mod_rows.is_empty() {
                        div {
                            style: "border: 1px solid #28323d; border-radius: 6px; background: #151b18; padding: 22px 18px; color: #cbd8cc; display: grid; gap: 8px;",
                            h3 {
                                style: "font-size: 17px; line-height: 22px; margin: 0; color: #edf2f7;",
                                "No mods loaded"
                            }
                            div {
                                style: "font-size: 13px; line-height: 18px; color: #9fb0c0;",
                                "No source selected."
                            }
                            div {
                                style: "display: flex; flex-wrap: wrap; gap: 8px;",
                                button {
                                    style: "border: 1px solid #2f80ed; background: #1f6feb; color: white; border-radius: 6px; padding: 8px 12px;",
                                    onclick: move |_| {
                                        let selected_game_folder = select_game_folder(game_folder.read().clone());
                                        if let Some(selected_game_folder) = selected_game_folder {
                                            match save_game_folder(&selected_game_folder)
                                                .and_then(|_| load_mods_from_game_folder(selected_game_folder.clone()))
                                            {
                                                Ok((mods, status)) => {
                                                    let mut next_state = app_state.read().clone();
                                                    let _ = next_state.apply(CoreCommand::ReplaceMods { mods });
                                                    app_state.set(next_state);
                                                    game_folder.set(Some(selected_game_folder));
                                                    mod_status.set(Some(status));
                                                }
                                                Err(error) => mod_status.set(Some(format!("Could not load game mods: {}", error.message))),
                                            }
                                        }
                                    },
                                    "Load game mods"
                                }
                                button {
                                    style: "border: 1px solid #3a4756; background: #202832; color: #edf2f7; border-radius: 6px; padding: 8px 12px;",
                                    onclick: move |_| {
                                        if let Some(mod_folder) = pick_mod_folder() {
                                            match load_mods_from_folder(mod_folder) {
                                                Ok((mods, status)) => {
                                                    let mut next_state = app_state.read().clone();
                                                    let _ = next_state.apply(CoreCommand::ReplaceMods { mods });
                                                    app_state.set(next_state);
                                                    mod_status.set(Some(status));
                                                }
                                                Err(error) => {
                                                    mod_status.set(Some(format!("Could not discover mods: {}", error.message)));
                                                }
                                            }
                                        }
                                    },
                                    "Open mod folder"
                                }
                            }
                        }
                    } else if filtered_mods.is_empty() {
                        div {
                            style: "border: 1px solid #28323d; border-radius: 6px; background: #151b18; padding: 18px 16px; color: #9fb0c0; font-size: 13px;",
                            "No mods match the current search."
                        }
                    }
                    for (mod_order, mod_row) in filtered_mods.iter().enumerate().map(|(index, row)| (index + 1, row)) {
                        article {
                            key: "{mod_row.key}",
                            style: mod_row_style(active_mod_key.as_deref() == Some(mod_row.key.as_str())),
                            onclick: {
                                let mod_key = mod_row.key.clone();
                                move |_| {
                                    selected_mod_key.set(Some(mod_key.clone()));
                                    workspace_page.set(WorkspacePage::ModDetail);
                                }
                            },
                            div {
                                style: "font-size: 13px; color: #d5d9df; text-align: center; font-family: ui-monospace, SFMono-Regular, Menlo, monospace;",
                                "{mod_order}"
                            }
                            button {
                                title: "Enable or disable mod",
                                style: mod_enable_button_style(mod_row.enabled, mod_row.locked),
                                disabled: mod_row.locked,
                                onclick: {
                                    let mod_key = mod_row.key.clone();
                                    move |event| {
                                        event.stop_propagation();
                                        let mut next_state = app_state.read().clone();
                                        if let Some(identity) = identity_for_mod_key(&next_state, &mod_key) {
                                            match next_state.apply(CoreCommand::ToggleMod { identity }) {
                                                Ok(_) => {
                                                    match save_mod_state(&next_state) {
                                                        Ok(status) => mod_status.set(Some(status)),
                                                        Err(error) => mod_status.set(Some(format!("Could not save mod state: {}", error.message))),
                                                    }
                                                    app_state.set(next_state);
                                                }
                                                Err(error) => mod_status.set(Some(format!("Could not toggle mod: {}", error.message))),
                                            }
                                        }
                                    }
                                },
                                if mod_row.enabled {
                                    "ON"
                                } else {
                                    "OFF"
                                }
                            }
                            div {
                                style: source_tile_style(mod_row),
                                "{mod_source_label(mod_row)}"
                            }
                            div {
                                style: "min-width: 0;",
                                div {
                                    style: "font-size: 15px; font-weight: 700; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                    "{mod_row.display_name}"
                                }
                                div {
                                    style: "font-size: 12px; color: #9aa4b7; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: ui-monospace, SFMono-Regular, Menlo, monospace;",
                                    "{mod_row.subtitle}"
                                }
                                if !mod_row.categories.is_empty() {
                                    div {
                                        style: "font-size: 12px; color: #86efac; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                        "{mod_row.categories.join(\", \")}"
                                    }
                                }
                            }
                            div {
                                style: "font-size: 13px; color: #d5d9df; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                "{mod_author_label(mod_row, &current_steam_metadata)}"
                            }
                            div {
                                style: "font-size: 12px; color: #aeb8c8; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: ui-monospace, SFMono-Regular, Menlo, monospace;",
                                "{mod_updated_label(mod_row, &current_steam_metadata, current_time_ms)}"
                            }
                        }
                    }
                }
                if let Some(pack) = view_model.selected_pack {
                    PackPanel { pack }
                }
            }
            div {
                style: "display: grid; gap: 12px; margin-top: 16px;",
                if let Some(preview) = launch_preview.read().as_ref() {
                    LaunchPreviewPanel {
                        preview: preview.clone(),
                        is_stale: preview.fingerprint != current_launch_fingerprint,
                    }
                }
                SteamMetadataPanel {
                    helper_path: steam_helper_path.read().clone(),
                    subscribed_ids: subscribed_workshop_ids.read().clone(),
                    metadata: steam_metadata.read().clone(),
                }
                if let Some(command_panel) = last_steam_command.read().as_ref() {
                    SteamCommandPanel { state: command_panel.clone() }
                }
            }
                    }
                }
            }
        }
    }
}

fn combined_status(mod_status: Option<&str>, pack_status: Option<&str>) -> Option<String> {
    match (mod_status, pack_status) {
        (Some(mod_status), Some(pack_status)) => Some(format!("{mod_status} {pack_status}")),
        (Some(mod_status), None) => Some(mod_status.to_string()),
        (None, Some(pack_status)) => Some(pack_status.to_string()),
        (None, None) => None,
    }
}

fn diagnostics_dir() -> PathBuf {
    app_config_dir().join(DIAGNOSTICS_DIR_NAME)
}

fn app_diagnostic_log_path() -> PathBuf {
    diagnostics_dir().join(APP_DIAGNOSTIC_LOG_FILE)
}

fn steam_helper_command_log_path() -> PathBuf {
    diagnostics_dir().join(STEAM_HELPER_COMMAND_LOG_FILE)
}

fn diagnostic_snapshot_path() -> PathBuf {
    diagnostics_dir().join(format!("wh3mm-diagnostic-{}.txt", current_unix_ms()))
}

fn append_app_diagnostic_log_event(event: &str) -> wh3mm_core::CoreResult<PathBuf> {
    let path = app_diagnostic_log_path();
    append_app_diagnostic_log_event_to_path(&path, event)?;
    Ok(path)
}

fn append_app_diagnostic_log_event_to_path(path: &Path, event: &str) -> wh3mm_core::CoreResult<()> {
    ensure_parent_dir(path)?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| {
            wh3mm_core::CoreError::io(format!(
                "failed to open app diagnostic log {}: {error}",
                path.display()
            ))
        })?;
    let one_line_event = event.replace('\n', "\\n");
    writeln!(file, "{}\t{}", current_unix_ms(), one_line_event).map_err(|error| {
        wh3mm_core::CoreError::io(format!(
            "failed to append app diagnostic log {}: {error}",
            path.display()
        ))
    })
}

fn write_diagnostic_snapshot(
    input: DiagnosticSnapshotInput<'_>,
) -> wh3mm_core::CoreResult<PathBuf> {
    let path = diagnostic_snapshot_path();
    ensure_parent_dir(&path)?;
    fs::write(&path, diagnostic_snapshot_text(&input)).map_err(|error| {
        wh3mm_core::CoreError::io(format!(
            "failed to write diagnostic snapshot {}: {error}",
            path.display()
        ))
    })?;
    let _ = append_app_diagnostic_log_event(&format!(
        "diagnostic snapshot written: {}",
        path.display()
    ));
    Ok(path)
}

fn diagnostic_snapshot_text(input: &DiagnosticSnapshotInput<'_>) -> String {
    let total_mod_count = input.app_state.mods.len();
    let enabled_mod_count = input
        .app_state
        .mods
        .iter()
        .filter(|mod_record| mod_record.effectively_enabled())
        .count();
    let hidden_mod_count = input
        .app_state
        .mods
        .iter()
        .filter(|mod_record| mod_record.hidden)
        .count();
    let locked_mod_count = input
        .app_state
        .mods
        .iter()
        .filter(|mod_record| mod_record.always_enabled)
        .count();

    let mut lines = vec![
        "WH3 Mod Manager Rust diagnostic snapshot".to_string(),
        format!("timestamp_unix_ms={}", current_unix_ms()),
        format!("config_dir={}", app_config_dir().display()),
        format!("app_log={}", app_diagnostic_log_path().display()),
        format!(
            "steam_helper_command_log={}",
            steam_helper_command_log_path().display()
        ),
        format!("status={}", input.status_message.unwrap_or("")),
        format!(
            "game_folder={}",
            input
                .game_folder
                .map(|path| path.display().to_string())
                .unwrap_or_default()
        ),
        format!("helper.path={}", input.helper_path.trim()),
        format!("helper.backend={}", input.helper_backend.trim()),
        format!("mods.total={total_mod_count}"),
        format!("mods.enabled={enabled_mod_count}"),
        format!("mods.hidden={hidden_mod_count}"),
        format!("mods.locked={locked_mod_count}"),
        format!(
            "launch.skip_intro_movies={}",
            input.launch_options.skip_intro_movies
        ),
        format!(
            "launch.script_logging={}",
            input.launch_options.script_logging
        ),
        format!(
            "launch.auto_start_custom_battle={}",
            input.launch_options.auto_start_custom_battle
        ),
        format!(
            "launch.make_units_generals={}",
            input.launch_options.make_units_generals
        ),
        format!(
            "launch.high_process_priority={}",
            input.launch_options.high_process_priority
        ),
        format!(
            "launch.close_on_play={}",
            input.launch_options.close_on_play
        ),
        format!("launch.save_name={}", input.launch_save_name.trim()),
        format!("readiness.summary={}", input.readiness.summary),
    ];

    for row in &input.readiness.rows {
        lines.push(format!(
            "readiness.{}={}: {}",
            row.label,
            alpha_readiness_status_label(&row.status),
            row.detail
        ));
    }

    match input.launch_preview {
        Some(preview) => {
            lines.push("launch_preview=present".to_string());
            lines.push(format!("launch_preview.game_dir={}", preview.game_dir));
            lines.push(format!("launch_preview.data_dir={}", preview.data_dir));
            lines.push(format!(
                "launch_preview.mod_list_file={}",
                preview.mod_list_file_name
            ));
            lines.push(format!(
                "launch_preview.enabled_count={}",
                preview.enabled_count
            ));
            lines.push(format!(
                "launch_preview.pre_launch_copies={}",
                preview.pre_launch_copies.len()
            ));
            lines.push(format!(
                "launch_preview.generated_packs={}",
                preview.generated_packs.len()
            ));
            lines.push(format!(
                "launch_preview.fingerprint={}",
                preview.fingerprint
            ));
            lines.push("launch_preview.mod_list_contents:".to_string());
            lines.push(preview.mod_list_contents.clone());
        }
        None => lines.push("launch_preview=none".to_string()),
    }

    match input.last_steam_command {
        Some(panel) => {
            lines.push(format!("steam_command.title={}", panel.title));
            lines.push(format!("steam_command.summary={}", panel.summary));
            for row in &panel.rows {
                lines.push(format!("steam_command.{}={}", row.label, row.value));
            }
        }
        None => lines.push("steam_command=none".to_string()),
    }

    lines.push(String::new());
    lines.join("\n")
}

fn alpha_readiness_status_label(status: &AlphaReadinessStatus) -> &'static str {
    match status {
        AlphaReadinessStatus::Ready => "READY",
        AlphaReadinessStatus::Warning => "CHECK",
        AlphaReadinessStatus::Error => "ERROR",
    }
}

fn ensure_parent_dir(path: &Path) -> wh3mm_core::CoreResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            wh3mm_core::CoreError::io(format!(
                "failed to create diagnostics directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    Ok(())
}

fn app_brand_title() -> &'static str {
    "Mod Archive"
}

fn app_brand_subtitle(app_title: &str) -> String {
    format!("{app_title} / Windows alpha")
}

fn mod_row_matches_query(mod_row: &ModRowViewModel, normalized_query: &str) -> bool {
    if normalized_query.is_empty() {
        return true;
    }

    mod_row
        .display_name
        .to_ascii_lowercase()
        .contains(normalized_query)
        || mod_row
            .subtitle
            .to_ascii_lowercase()
            .contains(normalized_query)
        || mod_row
            .categories
            .iter()
            .any(|category| category.to_ascii_lowercase().contains(normalized_query))
        || mod_row
            .tags
            .iter()
            .any(|tag| tag.to_ascii_lowercase().contains(normalized_query))
}

fn selected_or_first_mod_row(
    rows: &[ModRowViewModel],
    selected_key: Option<&str>,
) -> Option<ModRowViewModel> {
    selected_key
        .and_then(|key| rows.iter().find(|row| row.key == key).cloned())
        .or_else(|| rows.first().cloned())
}

fn mod_row_matches_filter(mod_row: &ModRowViewModel, filter: ModListFilter) -> bool {
    match filter {
        ModListFilter::All => true,
        ModListFilter::Enabled => mod_row.enabled,
        ModListFilter::Disabled => !mod_row.enabled && !mod_row.locked,
        ModListFilter::Locked => mod_row.locked,
        ModListFilter::Hidden => mod_row.hidden,
    }
}

fn mod_state_label(mod_row: &ModRowViewModel) -> &'static str {
    if mod_row.hidden {
        "hidden"
    } else if mod_row.locked {
        "locked"
    } else if mod_row.enabled {
        "enabled"
    } else {
        "disabled"
    }
}

fn mod_categories_label(mod_row: &ModRowViewModel) -> String {
    if mod_row.categories.is_empty() {
        "None".to_string()
    } else {
        mod_row.categories.join(", ")
    }
}

fn mod_list_filter_label(filter: ModListFilter) -> &'static str {
    match filter {
        ModListFilter::All => "All mods",
        ModListFilter::Enabled => "Enabled",
        ModListFilter::Disabled => "Disabled",
        ModListFilter::Locked => "Locked",
        ModListFilter::Hidden => "Hidden",
    }
}

fn mod_row_style(active: bool) -> &'static str {
    if active {
        "min-height: 54px; display: grid; grid-template-columns: 42px 72px 52px minmax(0, 1.9fr) minmax(120px, 0.55fr) minmax(96px, 0.45fr); align-items: center; gap: 10px; border: 1px solid #3b82f6; border-left: 3px solid #60a5fa; border-radius: 4px; padding: 7px 14px; background: #1d2631; cursor: pointer;"
    } else {
        "min-height: 54px; display: grid; grid-template-columns: 42px 72px 52px minmax(0, 1.9fr) minmax(120px, 0.55fr) minmax(96px, 0.45fr); align-items: center; gap: 10px; border: 1px solid #252b38; border-left: 3px solid transparent; border-radius: 4px; padding: 7px 14px; background: #171922; cursor: pointer;"
    }
}

fn archive_table_header_style() -> &'static str {
    "position: sticky; top: 0; z-index: 1; display: grid; grid-template-columns: 42px 72px 52px minmax(0, 1.9fr) minmax(120px, 0.55fr) minmax(96px, 0.45fr); align-items: center; gap: 10px; padding: 10px 14px; border: 1px solid #303746; background: #20242f; color: #cbd5e1; font-size: 11px; line-height: 15px; text-transform: uppercase; letter-spacing: 0;"
}

fn archive_filter_bar_style() -> &'static str {
    "display: flex; align-items: center; flex-wrap: wrap; gap: 7px; width: 100%; min-height: 42px; box-sizing: border-box; border: 1px solid #293142; background: #151821; border-radius: 4px; padding: 6px 8px;"
}

fn archive_filter_button_style(active: bool) -> &'static str {
    if active {
        "min-width: 78px; height: 30px; border: 1px solid #60a5fa; background: #172033; color: #bfdbfe; border-radius: 4px; padding: 0 10px; font-size: 11px; font-weight: 800; letter-spacing: 0;"
    } else {
        "min-width: 78px; height: 30px; border: 1px solid #303746; background: #1f2430; color: #d5d9df; border-radius: 4px; padding: 0 10px; font-size: 11px; font-weight: 700; letter-spacing: 0;"
    }
}

fn library_nav_active(
    target: LibraryNavTarget,
    workspace_page: WorkspacePage,
    mod_filter: ModListFilter,
    library_tool: LibraryToolTab,
) -> bool {
    match target {
        LibraryNavTarget::AllMods => {
            workspace_page == WorkspacePage::Mods
                && mod_filter == ModListFilter::All
                && library_tool == LibraryToolTab::None
        }
        LibraryNavTarget::Enabled => {
            workspace_page == WorkspacePage::Mods
                && mod_filter == ModListFilter::Enabled
                && library_tool == LibraryToolTab::None
        }
        LibraryNavTarget::Categories => workspace_page == WorkspacePage::Categories,
        LibraryNavTarget::Collections => workspace_page == WorkspacePage::Collections,
        LibraryNavTarget::Settings => workspace_page == WorkspacePage::Settings,
    }
}

fn nav_button_style(active: bool) -> &'static str {
    if active {
        "display: grid; grid-template-columns: 26px minmax(0, 1fr) auto; align-items: center; gap: 10px; width: 100%; border: 0; border-left: 2px solid #65f58b; background: #202c2a; color: #65f58b; border-radius: 4px; padding: 12px 10px; text-align: left; font-size: 12px; font-weight: 750; text-transform: uppercase; letter-spacing: 1.6px;"
    } else {
        "display: grid; grid-template-columns: 26px minmax(0, 1fr) auto; align-items: center; gap: 10px; width: 100%; border: 0; border-left: 2px solid transparent; background: transparent; color: #d7ded9; border-radius: 4px; padding: 12px 10px; text-align: left; font-size: 12px; font-weight: 650; text-transform: uppercase; letter-spacing: 1.6px;"
    }
}

fn nav_badge_style() -> &'static str {
    "width: 26px; height: 22px; display: inline-flex; align-items: center; justify-content: center; border: 1px solid currentColor; border-radius: 4px; font-size: 9px; font-weight: 800; letter-spacing: 0;"
}

fn header_metric_style() -> &'static str {
    "min-height: 30px; display: inline-flex; align-items: center; border: 1px solid #293142; background: #171b24; color: #cbd5e1; border-radius: 4px; padding: 0 9px; font-size: 11px; font-weight: 650; letter-spacing: 0;"
}

fn top_icon_button_style(active: bool) -> &'static str {
    if active {
        "height: 34px; min-width: 68px; display: inline-grid; place-items: center; flex: 0 0 auto; border: 1px solid #60a5fa; background: #172033; color: #bfdbfe; border-radius: 6px; padding: 0 10px; font-size: 11px; font-weight: 800; letter-spacing: 0;"
    } else {
        "height: 34px; min-width: 68px; display: inline-grid; place-items: center; flex: 0 0 auto; border: 1px solid #303746; background: #1f2430; color: #cbd8cc; border-radius: 6px; padding: 0 10px; font-size: 11px; font-weight: 750; letter-spacing: 0;"
    }
}

fn library_utility_button_style(active: bool) -> &'static str {
    if active {
        "display: grid; grid-template-columns: 26px minmax(0, 1fr); align-items: center; gap: 10px; width: 100%; min-height: 38px; border: 0; border-left: 2px solid #65f58b; background: #202c2a; color: #65f58b; border-radius: 4px; padding: 8px 10px; text-align: left; font-size: 11px; font-weight: 750; text-transform: uppercase; letter-spacing: 1.4px;"
    } else {
        "display: grid; grid-template-columns: 26px minmax(0, 1fr); align-items: center; gap: 10px; width: 100%; min-height: 38px; border: 0; border-left: 2px solid transparent; background: transparent; color: #cbd8cc; border-radius: 4px; padding: 8px 10px; text-align: left; font-size: 11px; font-weight: 650; text-transform: uppercase; letter-spacing: 1.4px;"
    }
}

fn settings_card_style() -> &'static str {
    "border: 1px solid #303241; background: #1f202b; border-radius: 8px; overflow: hidden;"
}

fn settings_card_header_style() -> &'static str {
    "display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 18px 20px; background: #2a2b37; border-bottom: 1px solid #303241;"
}

fn settings_card_body_style() -> &'static str {
    "display: grid; gap: 18px; padding: 20px;"
}

fn settings_row_style() -> &'static str {
    "display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 18px; align-items: center; padding-bottom: 18px; border-bottom: 1px solid #2b2d39;"
}

fn settings_input_style() -> &'static str {
    "width: 100%; min-width: 0; box-sizing: border-box; border: 1px solid #3b3d4d; background: #101018; color: #f2f5f2; border-radius: 5px; padding: 10px 12px; font-size: 14px;"
}

fn settings_path_box_style() -> &'static str {
    "min-width: 0; border: 1px solid #3b3d4d; background: #101018; color: #dbe4d8; border-radius: 5px; padding: 10px 12px; font-size: 14px; overflow-wrap: anywhere; font-family: ui-monospace, SFMono-Regular, Menlo, monospace;"
}

fn settings_secondary_button_style() -> &'static str {
    "border: 1px solid #3b3d4d; background: #343541; color: #f2f5f2; border-radius: 5px; padding: 10px 12px; font-size: 14px;"
}

fn settings_primary_button_style() -> &'static str {
    "border: 1px solid #4ade80; background: #65f58b; color: #06210d; border-radius: 5px; padding: 10px 12px; font-size: 14px; font-weight: 800;"
}

fn settings_danger_button_style() -> &'static str {
    "border: 1px solid #7f1d1d; background: #451a1a; color: #fecaca; border-radius: 5px; padding: 10px 12px; font-size: 14px;"
}

fn collection_row_style() -> &'static str {
    "display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: center; gap: 12px; border: 1px solid #303746; background: #171b24; border-radius: 5px; padding: 12px 14px; color: #e5e7eb; font-size: 14px;"
}

fn collection_row_button_style(active: bool) -> &'static str {
    if active {
        "display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: center; gap: 12px; width: 100%; border: 1px solid #60a5fa; background: #172033; color: #bfdbfe; border-radius: 5px; padding: 12px 14px; text-align: left; font-size: 14px;"
    } else {
        "display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: center; gap: 12px; width: 100%; border: 1px solid #303746; background: #171b24; color: #e5e7eb; border-radius: 5px; padding: 12px 14px; text-align: left; font-size: 14px;"
    }
}

fn archive_toolbar_button_style(primary: bool) -> &'static str {
    if primary {
        "min-height: 30px; border: 1px solid #2f80ed; background: #1f6feb; color: white; border-radius: 5px; padding: 6px 10px; font-size: 12px; font-weight: 650;"
    } else {
        "min-height: 30px; border: 1px solid #3b3d4d; background: #202832; color: #edf2f7; border-radius: 5px; padding: 6px 10px; font-size: 12px;"
    }
}

fn toggle_label_style(enabled: bool) -> &'static str {
    if enabled {
        "min-width: 58px; height: 30px; display: inline-flex; align-items: center; justify-content: center; border: 1px solid #4ade80; background: #65f58b; color: #06210d; border-radius: 999px; font-size: 11px; font-weight: 800; letter-spacing: 0.8px;"
    } else {
        "min-width: 58px; height: 30px; display: inline-flex; align-items: center; justify-content: center; border: 1px solid #475569; background: #353a43; color: #cbd5e1; border-radius: 999px; font-size: 11px; font-weight: 800; letter-spacing: 0.8px;"
    }
}

fn mod_enable_button_style(enabled: bool, locked: bool) -> &'static str {
    if locked {
        "width: 54px; height: 26px; border: 1px solid #4b5563; background: #343946; color: #9aa4b7; border-radius: 999px; padding: 0; font-size: 11px; font-weight: 800; letter-spacing: 0.8px;"
    } else if enabled {
        "width: 54px; height: 26px; border: 1px solid #4ade80; background: #65f58b; color: #06210d; border-radius: 999px; padding: 0; font-size: 11px; font-weight: 800; letter-spacing: 0.8px;"
    } else {
        "width: 54px; height: 26px; border: 1px solid #475569; background: #353a43; color: #cbd5e1; border-radius: 999px; padding: 0; font-size: 11px; font-weight: 800; letter-spacing: 0.8px;"
    }
}

fn mod_source_label(mod_row: &ModRowViewModel) -> &'static str {
    if mod_row
        .tags
        .iter()
        .any(|tag| tag.eq_ignore_ascii_case("workshop"))
        || mod_row.subtitle.to_ascii_lowercase().contains("workshop")
    {
        "WS"
    } else if mod_row.locked {
        "CORE"
    } else {
        "MOD"
    }
}

fn mod_author_label(mod_row: &ModRowViewModel, metadata: &[WorkshopModData]) -> String {
    if let Some(metadata) = workshop_metadata_for_row(mod_row, metadata)
        && !metadata.author.trim().is_empty()
    {
        return metadata.author.trim().to_string();
    }

    match mod_source_label(mod_row) {
        "WS" => "Steam Workshop".to_string(),
        "CORE" => "Core".to_string(),
        _ => "Local".to_string(),
    }
}

fn mod_updated_label(
    mod_row: &ModRowViewModel,
    metadata: &[WorkshopModData],
    now_ms: u64,
) -> String {
    let Some(metadata) = workshop_metadata_for_row(mod_row, metadata) else {
        return "Local".to_string();
    };
    relative_time_label(metadata.last_changed_ms, now_ms)
}

fn workshop_metadata_for_row<'a>(
    mod_row: &ModRowViewModel,
    metadata: &'a [WorkshopModData],
) -> Option<&'a WorkshopModData> {
    let workshop_id = mod_workshop_id_from_row(mod_row)?;
    metadata
        .iter()
        .find(|metadata| metadata.workshop_id == workshop_id)
}

fn mod_workshop_id_from_row(mod_row: &ModRowViewModel) -> Option<String> {
    if let Some(workshop_id) = mod_row
        .key
        .strip_prefix("workshop:")
        .and_then(normalize_workshop_id)
    {
        return Some(workshop_id);
    }

    let normalized_path = mod_row.subtitle.replace('\\', "/").to_ascii_lowercase();
    let workshop_prefix = "/workshop/content/1142710/";
    normalized_path
        .find(workshop_prefix)
        .and_then(|index| {
            normalized_path[index + workshop_prefix.len()..]
                .split('/')
                .next()
        })
        .and_then(normalize_workshop_id)
}

fn relative_time_label(changed_ms: u64, now_ms: u64) -> String {
    if changed_ms == 0 {
        return "Unknown".to_string();
    }
    if changed_ms > now_ms {
        return "Pending".to_string();
    }

    let days = (now_ms - changed_ms) / 86_400_000;
    match days {
        0 => "Today".to_string(),
        1 => "1 day ago".to_string(),
        2..=59 => format!("{days} days ago"),
        60..=729 => {
            let months = (days / 30).max(2);
            format!("{months} months ago")
        }
        _ => {
            let years = (days / 365).max(2);
            format!("{years} years ago")
        }
    }
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn source_tile_style(mod_row: &ModRowViewModel) -> &'static str {
    match mod_source_label(mod_row) {
        "WS" => {
            "width: 42px; height: 42px; display: grid; place-items: center; justify-self: start; border: 1px solid #2563eb; background: #172554; color: #bfdbfe; border-radius: 5px; padding: 0; font-size: 11px; font-weight: 800; letter-spacing: 0;"
        }
        "CORE" => {
            "width: 42px; height: 42px; display: grid; place-items: center; justify-self: start; border: 1px solid #4b5563; background: #272b35; color: #d1d5db; border-radius: 5px; padding: 0; font-size: 10px; font-weight: 800; letter-spacing: 0;"
        }
        _ => {
            "width: 42px; height: 42px; display: grid; place-items: center; justify-self: start; border: 1px solid #166534; background: #10281a; color: #86efac; border-radius: 5px; padding: 0; font-size: 10px; font-weight: 800; letter-spacing: 0;"
        }
    }
}

fn detail_source_tile_style(mod_row: &ModRowViewModel) -> &'static str {
    match mod_source_label(mod_row) {
        "WS" => {
            "aspect-ratio: 1 / 1; min-height: 150px; display: grid; place-items: center; align-content: center; gap: 8px; border: 1px solid #2563eb; background: #172554; color: #bfdbfe; border-radius: 8px; text-align: center;"
        }
        "CORE" => {
            "aspect-ratio: 1 / 1; min-height: 150px; display: grid; place-items: center; align-content: center; gap: 8px; border: 1px solid #4b5563; background: #272b35; color: #d1d5db; border-radius: 8px; text-align: center;"
        }
        _ => {
            "aspect-ratio: 1 / 1; min-height: 150px; display: grid; place-items: center; align-content: center; gap: 8px; border: 1px solid #166534; background: #10281a; color: #86efac; border-radius: 8px; text-align: center;"
        }
    }
}

fn detail_metric_style() -> &'static str {
    "display: grid; gap: 4px; min-width: 0; border-top: 1px solid #3b3d4d; padding-top: 10px; font-size: 12px; color: #aeb8c8;"
}

fn detail_action_button_style(danger: bool) -> &'static str {
    if danger {
        "border: 1px solid #7f1d1d; background: #451a1a; color: #fecaca; border-radius: 5px; padding: 9px 10px; font-size: 13px;"
    } else {
        "border: 1px solid #3b3d4d; background: #343541; color: #f2f5f2; border-radius: 5px; padding: 9px 10px; font-size: 13px;"
    }
}

#[cfg(test)]
fn library_tool_tab_label(tab: LibraryToolTab) -> &'static str {
    match tab {
        LibraryToolTab::None => "None",
        LibraryToolTab::Presets => "Presets",
        LibraryToolTab::Categories => "Cats",
        LibraryToolTab::Config => "Config",
    }
}

fn launch_quick_button_style() -> &'static str {
    "min-height: 42px; border: 1px solid #3b3d4d; background: #343541; color: #f2f5f2; border-radius: 5px; padding: 10px 12px; font-size: 14px; font-weight: 650;"
}

fn continue_save_button_style(needs_save_name: bool) -> &'static str {
    if needs_save_name {
        "display: flex; align-items: center; justify-content: space-between; gap: 12px; width: 100%; min-height: 50px; border: 1px solid #333541; background: #292a35; color: #d8ded8; border-radius: 5px; padding: 12px 16px; margin: -4px 0 18px; font-size: 15px; text-align: left;"
    } else {
        "display: flex; align-items: center; justify-content: space-between; gap: 12px; width: 100%; min-height: 50px; border: 1px solid #3b3d4d; background: #3a3b48; color: #f2f5f2; border-radius: 5px; padding: 12px 16px; margin: -4px 0 18px; font-size: 15px; text-align: left;"
    }
}

fn tool_action_button_style(active: bool) -> &'static str {
    if active {
        "display: flex; align-items: center; justify-content: space-between; gap: 12px; width: 100%; border: 1px solid #3b3d4d; background: #3a3b48; color: #f2f5f2; border-radius: 5px; padding: 14px 16px; font-size: 15px; text-align: left;"
    } else {
        "display: flex; align-items: center; justify-content: space-between; gap: 12px; width: 100%; border: 1px solid #333541; background: #292a35; color: #d8ded8; border-radius: 5px; padding: 14px 16px; font-size: 15px; text-align: left;"
    }
}

#[component]
fn AlphaReadinessPanel(report: AlphaReadinessReport) -> Element {
    rsx! {
        section {
            style: "display: grid; gap: 8px; margin-bottom: 18px; border: 1px solid #2b352d; background: #111710; border-radius: 6px; padding: 10px;",
            header {
                style: "display: grid; gap: 3px;",
                h3 {
                    style: "font-size: 12px; line-height: 16px; color: #9fb0a3; text-transform: uppercase; margin: 0;",
                    "Alpha readiness"
                }
                div {
                    style: "font-size: 12px; color: #cbd8cc; overflow-wrap: anywhere;",
                    "{report.summary}"
                }
            }
            div {
                style: "display: grid; gap: 6px;",
                for row in report.rows.iter() {
                    article {
                        key: "{row.label}",
                        style: "display: grid; gap: 3px; padding: 7px 8px; border: 1px solid #263128; border-radius: 4px; background: #151b18;",
                        div {
                            style: "display: flex; align-items: center; justify-content: space-between; gap: 8px;",
                            span {
                                style: "font-size: 12px; color: #edf2f7; font-weight: 650;",
                                "{row.label}"
                            }
                            if row.status == AlphaReadinessStatus::Ready {
                                span {
                                    style: "font-size: 11px; color: #86efac; text-transform: uppercase;",
                                    "Ready"
                                }
                            } else if row.status == AlphaReadinessStatus::Warning {
                                span {
                                    style: "font-size: 11px; color: #fde68a; text-transform: uppercase;",
                                    "Check"
                                }
                            } else {
                                span {
                                    style: "font-size: 11px; color: #fca5a5; text-transform: uppercase;",
                                    "Error"
                                }
                            }
                        }
                        div {
                            style: "font-size: 12px; line-height: 16px; color: #9fb0c0; overflow-wrap: anywhere;",
                            "{row.detail}"
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn SteamMetadataPanel(
    helper_path: String,
    subscribed_ids: Vec<String>,
    metadata: Vec<WorkshopModData>,
) -> Element {
    let has_steam_data = !subscribed_ids.is_empty() || !metadata.is_empty();
    if helper_path.trim().is_empty() && !has_steam_data {
        return rsx! {};
    }

    rsx! {
        section {
            style: "margin-bottom: 16px; border: 1px solid #28323d; border-radius: 6px; background: #151b21; overflow: hidden;",
            header {
                style: "display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 14px; border-bottom: 1px solid #28323d;",
                div {
                    style: "min-width: 0;",
                    h2 {
                        style: "font-size: 16px; margin: 0;",
                        "Steam"
                    }
                    div {
                        style: "font-size: 12px; color: #9fb0c0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                        if helper_path.trim().is_empty() {
                            "No helper configured"
                        } else {
                            "{helper_path}"
                        }
                    }
                }
                div {
                    style: "font-size: 12px; color: #9fb0c0; text-align: right;",
                    "{subscribed_ids.len()} subscribed / {metadata.len()} metadata"
                }
            }
            if has_steam_data {
                div {
                    style: "display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 10px; padding: 12px 14px;",
                    for item in metadata.iter().take(8) {
                        article {
                            key: "{item.workshop_id}",
                            style: "min-width: 0; display: grid; gap: 4px; border: 1px solid #222b34; border-radius: 6px; padding: 9px 10px; background: #111820;",
                            div {
                                style: "font-size: 13px; font-weight: 650; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                "{item.title}"
                            }
                            div {
                                style: "font-size: 12px; color: #9fb0c0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                "{item.workshop_id} / {item.author}"
                            }
                            div {
                                style: "font-size: 12px; color: #708090;",
                                "{item.dependency_ids.len()} dependencies"
                            }
                            if let Some(summary) = dependency_names_summary(item, 3) {
                                div {
                                    style: "font-size: 12px; color: #9fb0c0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                    "{summary}"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn SteamCommandPanel(state: SteamCommandPanelState) -> Element {
    rsx! {
        section {
            style: "margin-bottom: 16px; border: 1px solid #28323d; border-radius: 6px; background: #151b21; overflow: hidden;",
            header {
                style: "display: grid; gap: 4px; padding: 10px 14px; border-bottom: 1px solid #28323d;",
                h2 {
                    style: "font-size: 16px; margin: 0;",
                    "{state.title}"
                }
                div {
                    style: "font-size: 12px; color: #9fb0c0; overflow-wrap: anywhere;",
                    "{state.summary}"
                }
            }
            div {
                style: "display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 10px; padding: 12px 14px;",
                for row in state.rows.iter() {
                    article {
                        key: "{row.label}",
                        style: "display: grid; gap: 4px; border: 1px solid #222b34; border-radius: 6px; padding: 9px 10px; background: #111820;",
                        div {
                            style: "font-size: 11px; color: #9fb0a3; text-transform: uppercase;",
                            "{row.label}"
                        }
                        div {
                            style: "font-size: 13px; color: #edf2f7; overflow-wrap: anywhere;",
                            "{row.value}"
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn LaunchPreviewPanel(preview: LaunchPreview, is_stale: bool) -> Element {
    rsx! {
        section {
            style: "margin-bottom: 16px; border: 1px solid #28323d; border-radius: 6px; background: #151b21; overflow: hidden;",
            header {
                style: "display: grid; gap: 4px; padding: 12px 14px; border-bottom: 1px solid #28323d;",
                div {
                    style: "display: flex; align-items: center; justify-content: space-between; gap: 12px;",
                    h2 {
                        style: "font-size: 16px; margin: 0;",
                        "Launch preview"
                    }
                    if is_stale {
                        div {
                            style: "font-size: 12px; color: #fbbf24;",
                            "Stale"
                        }
                    }
                }
                div {
                    style: "font-size: 12px; color: #9fb0c0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                    "{preview.enabled_count} enabled / {preview.mod_list_file_name} / {preview.game_dir}"
                }
            }
            div {
                style: "display: grid; grid-template-columns: minmax(0, 1fr) minmax(240px, 360px); gap: 14px; padding: 12px 14px;",
                section {
                    style: "min-width: 0; display: grid; gap: 8px;",
                    h3 {
                        style: "font-size: 13px; margin: 0; color: #cbd8e4;",
                        "Mod list"
                    }
                    pre {
                        style: "max-height: 260px; overflow: auto; margin: 0; padding: 10px; background: #0d1117; border: 1px solid #222b34; color: #d8dee9; font-size: 12px; line-height: 1.45; white-space: pre;",
                        "{preview.mod_list_contents}"
                    }
                }
                section {
                    style: "min-width: 0; display: grid; gap: 10px; align-content: start;",
                    div {
                        style: "display: grid; gap: 4px;",
                        h3 {
                            style: "font-size: 13px; margin: 0; color: #cbd8e4;",
                            "Command"
                        }
                        div {
                            style: "font-size: 12px; color: #9fb0c0; overflow-wrap: anywhere;",
                            "{preview.command_line_preview}"
                        }
                    }
                    div {
                        style: "display: grid; gap: 4px;",
                        h3 {
                            style: "font-size: 13px; margin: 0; color: #cbd8e4;",
                            "Copies"
                        }
                        if preview.pre_launch_copies.is_empty() {
                            div {
                                style: "font-size: 12px; color: #708090;",
                                "None"
                            }
                        } else {
                            for copy in preview.pre_launch_copies.iter() {
                                div {
                                    key: "{copy.from_path}->{copy.to_path}",
                                    style: "font-size: 12px; color: #9fb0c0; overflow-wrap: anywhere;",
                                    "{copy.from_path} -> {copy.to_path}"
                                }
                            }
                        }
                    }
                    div {
                        style: "display: grid; gap: 4px;",
                        h3 {
                            style: "font-size: 13px; margin: 0; color: #cbd8e4;",
                            "Generated"
                        }
                        if preview.generated_packs.is_empty() {
                            div {
                                style: "font-size: 12px; color: #708090;",
                                "None"
                            }
                        } else {
                            for pack in preview.generated_packs.iter() {
                                div {
                                    key: "{pack.path}",
                                    style: "display: grid; gap: 2px; font-size: 12px; color: #9fb0c0; overflow-wrap: anywhere;",
                                    div {
                                        "{pack.path} ({pack.byte_len} bytes)"
                                    }
                                    if !pack.packed_file_summary.is_empty() {
                                        div {
                                            style: "color: #708090;",
                                            "{pack.packed_file_summary}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                    div {
                        style: "display: grid; gap: 4px;",
                        h3 {
                            style: "font-size: 13px; margin: 0; color: #cbd8e4;",
                            "Data"
                        }
                        div {
                            style: "font-size: 12px; color: #9fb0c0; overflow-wrap: anywhere;",
                            "{preview.data_dir}"
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ConflictPanel(report: PackConflictReport) -> Element {
    rsx! {
        section {
            style: "margin-top: 16px; border: 1px solid #28323d; border-radius: 6px; background: #151b21; overflow: hidden;",
            header {
                style: "display: grid; gap: 4px; padding: 12px 14px; border-bottom: 1px solid #28323d;",
                h2 {
                    style: "font-size: 16px; margin: 0;",
                    "Compatibility"
                }
                div {
                    style: "font-size: 12px; color: #9fb0c0;",
                    "{conflict_status(&report)}"
                }
            }
            div {
                style: "display: grid; grid-template-columns: repeat(11, minmax(0, 1fr)); gap: 12px; padding: 12px 14px; align-items: start;",
                ConflictList {
                    title: "File collisions",
                    rows: report.pack_file_collisions.iter().take(8).map(|collision| {
                        format!("{} -> {} / {}", collision.first_pack_name, collision.second_pack_name, collision.file_name)
                    }).collect::<Vec<_>>(),
                }
                ConflictList {
                    title: "Table collisions",
                    rows: report.pack_table_collisions.iter().take(8).map(|collision| {
                        format!("{} -> {} / {}={}", collision.first_pack_name, collision.second_pack_name, collision.key, collision.value)
                    }).collect::<Vec<_>>(),
                }
                ConflictList {
                    title: "Missing dependencies",
                    rows: report.missing_dependency_packs.iter().take(8).map(|dependency| {
                        format!("{} needs {}", dependency.pack_name, dependency.dependency_pack_name)
                    }).collect::<Vec<_>>(),
                }
                ConflictList {
                    title: "Missing DB refs",
                    rows: report.missing_db_references.iter().take(8).map(|reference| {
                        format!(
                            "{} / {}.{} -> {}.{}={}",
                            reference.pack_name,
                            reference.origin_db_name,
                            reference.origin_field_name,
                            reference.target_db_name,
                            reference.target_field_name,
                            reference.value,
                        )
                    }).collect::<Vec<_>>(),
                }
                ConflictList {
                    title: "Unique IDs",
                    rows: report.unique_id_collisions.iter().take(8).map(|collision| {
                        match &collision.second_pack_name {
                            Some(second_pack_name) => format!(
                                "{} -> {} / {}.{}={}",
                                collision.first_pack_name,
                                second_pack_name,
                                collision.table_name,
                                collision.field_name,
                                collision.value.value,
                            ),
                            None => format!(
                                "{} / {}.{}={}",
                                collision.first_pack_name,
                                collision.table_name,
                                collision.field_name,
                                collision.value.value,
                            ),
                        }
                    }).collect::<Vec<_>>(),
                }
                ConflictList {
                    title: "Script listeners",
                    rows: report.script_listener_collisions.iter().take(8).map(|collision| {
                        match &collision.second_pack_name {
                            Some(second_pack_name) => format!(
                                "{} -> {} / {}={}",
                                collision.first_pack_name,
                                second_pack_name,
                                collision.pack_file_name,
                                collision.value.value,
                            ),
                            None => format!(
                                "{} / {}={}",
                                collision.first_pack_name,
                                collision.pack_file_name,
                                collision.value.value,
                            ),
                        }
                    }).collect::<Vec<_>>(),
                }
                ConflictList {
                    title: "Missing file refs",
                    rows: report.missing_file_references.iter().take(8).map(|reference| {
                        format!(
                            "{} / {} -> {}",
                            reference.pack_name,
                            reference.pack_file_name,
                            reference.reference,
                        )
                    }).collect::<Vec<_>>(),
                }
                ConflictList {
                    title: "Read errors",
                    rows: report.pack_read_errors.iter().take(8).map(|error| {
                        format!("{} / {}", error.pack_path, error.message)
                    }).collect::<Vec<_>>(),
                }
                ConflictList {
                    title: "Table read errors",
                    rows: report.table_read_errors.iter().take(8).map(|error| {
                        format!("{} / {} / {}", error.pack_name, error.table_name, error.message)
                    }).collect::<Vec<_>>(),
                }
                ConflictList {
                    title: "Script read errors",
                    rows: report.script_read_errors.iter().take(8).map(|error| {
                        format!("{} / {} / {}", error.pack_name, error.script_name, error.message)
                    }).collect::<Vec<_>>(),
                }
                ConflictList {
                    title: "File ref errors",
                    rows: report.file_reference_read_errors.iter().take(8).map(|error| {
                        format!("{} / {} / {}", error.pack_name, error.file_name, error.message)
                    }).collect::<Vec<_>>(),
                }
            }
        }
    }
}

#[component]
fn ConflictList(title: &'static str, rows: Vec<String>) -> Element {
    rsx! {
        section {
            style: "min-width: 0; display: grid; gap: 6px;",
            h3 {
                style: "font-size: 13px; margin: 0; color: #cbd8e4;",
                "{title}"
            }
            if rows.is_empty() {
                div {
                    style: "font-size: 12px; color: #708090;",
                    "None"
                }
            } else {
                for row in rows {
                    div {
                        key: "{row}",
                        style: "font-size: 12px; color: #9fb0c0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                        "{row}"
                    }
                }
            }
        }
    }
}

fn conflict_status(report: &PackConflictReport) -> String {
    format!(
        "{} file collisions, {} table collisions, {} missing dependency packs, {} missing DB refs, {} unique ID collisions, {} script listener collisions, {} missing file refs, {} decoded DB tables, {} read errors, {} table read errors, {} script read errors, {} file ref errors.",
        report.pack_file_collisions.len(),
        report.pack_table_collisions.len(),
        report.missing_dependency_packs.len(),
        report.missing_db_references.len(),
        report.unique_id_collisions.len(),
        report.script_listener_collisions.len(),
        report.missing_file_references.len(),
        report.decoded_db_table_count,
        report.pack_read_errors.len(),
        report.table_read_errors.len(),
        report.script_read_errors.len(),
        report.file_reference_read_errors.len()
    )
}

fn analyze_enabled_with_optional_schema(mods: &[ModRecord]) -> PackConflictReport {
    let schema_path = schema_path();
    match wh3mm_core::load_schema_file(&schema_path) {
        Ok(schema) => {
            analyze_enabled_mod_conflicts_with_schema(mods, &PackReadOptions::default(), &schema)
        }
        Err(_) => analyze_enabled_mod_conflicts(mods, &PackReadOptions::default()),
    }
}

#[component]
fn PackPanel(pack: PackViewModel) -> Element {
    rsx! {
        section {
            style: "border: 1px solid #28323d; border-radius: 6px; background: #151b21; overflow: hidden;",
            header {
                style: "display: grid; gap: 4px; padding: 12px 14px; border-bottom: 1px solid #28323d;",
                h2 {
                    style: "font-size: 16px; margin: 0;",
                    "Pack index"
                }
                div {
                    style: "font-size: 12px; color: #9fb0c0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                    "{pack.path} / {pack.magic}"
                }
            }
            if let Some(flow_summary) = &pack.flow_summary {
                section {
                    style: "display: grid; gap: 10px; padding: 12px 14px; border-bottom: 1px solid #28323d;",
                    header {
                        style: "display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 12px; align-items: end;",
                        div {
                            style: "min-width: 0;",
                            h3 {
                                style: "font-size: 14px; margin: 0 0 3px;",
                                "User flows"
                            }
                            div {
                                style: "font-size: 12px; color: #9fb0c0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                "{flow_summary.file_count_label} / {flow_summary.read_error_count_label}"
                            }
                        }
                    }
                    for flow in flow_summary.files.iter() {
                        article {
                            key: "{flow.name}",
                            style: "display: grid; gap: 5px; border: 1px solid #222b34; border-radius: 6px; padding: 8px 10px; background: #11171d;",
                            div {
                                style: "display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 10px; align-items: center;",
                                div {
                                    style: "font-size: 13px; color: #edf2f7; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                    "{flow.name}"
                                }
                                div {
                                    style: "font-size: 12px; color: #9fb0c0; white-space: nowrap;",
                                    "{flow.graph_label}"
                                }
                            }
                            div {
                                style: "font-size: 12px; color: #9fb0c0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                "{flow.detail_label}"
                            }
                            if !flow.options.is_empty() {
                                div {
                                    style: "display: flex; flex-wrap: wrap; gap: 6px;",
                                    for option in flow.options.iter() {
                                        span {
                                            key: "{option.id}",
                                            style: "border: 1px solid #334150; border-radius: 4px; padding: 3px 6px; font-size: 12px; color: #cbd8e4; background: #18212a;",
                                            "{option.label}"
                                            if let Some(default_value) = &option.default_value_label {
                                                span {
                                                    style: "color: #9fb0c0;",
                                                    " / {default_value}"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    for error in flow_summary.read_errors.iter() {
                        article {
                            key: "{error.name}",
                            style: "display: grid; gap: 3px; border: 1px solid #5a3440; border-radius: 6px; padding: 8px 10px; background: #201418;",
                            div {
                                style: "font-size: 13px; color: #fecdd3; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                "{error.name}"
                            }
                            div {
                                style: "font-size: 12px; color: #fca5a5; overflow-wrap: anywhere;",
                                "{error.message}"
                            }
                        }
                    }
                }
            }
            if let Some(preview) = &pack.table_preview {
                section {
                    style: "display: grid; gap: 10px; padding: 12px 14px; border-bottom: 1px solid #28323d;",
                    header {
                        style: "display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 12px; align-items: end;",
                        div {
                            style: "min-width: 0;",
                            h3 {
                                style: "font-size: 14px; margin: 0 0 3px;",
                                "{preview.title}"
                            }
                            div {
                                style: "font-size: 12px; color: #9fb0c0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                "{preview.source_name}"
                            }
                        }
                        div {
                            style: "font-size: 12px; color: #9fb0c0; text-align: right;",
                            "v{preview.version_label} / {preview.row_count_label}"
                        }
                    }
                    div {
                        style: "overflow: auto; max-height: 280px; border: 1px solid #222b34;",
                        table {
                            style: "width: 100%; min-width: 640px; border-collapse: collapse; font-size: 12px;",
                            thead {
                                tr {
                                    for column in preview.columns.iter() {
                                        th {
                                            key: "{column.name}",
                                            style: "position: sticky; top: 0; background: #1c242c; color: #cbd8e4; text-align: left; font-weight: 650; padding: 7px 8px; border-bottom: 1px solid #28323d; white-space: nowrap;",
                                            if column.is_key {
                                                "{column.name} *"
                                            } else {
                                                "{column.name}"
                                            }
                                        }
                                    }
                                }
                            }
                            tbody {
                                for row in preview.rows.iter() {
                                    tr {
                                        key: "{row.key}",
                                        for cell in row.cells.iter() {
                                            td {
                                                style: "padding: 6px 8px; border-bottom: 1px solid #222b34; color: #edf2f7; max-width: 260px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                                "{cell}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            div {
                style: "display: grid;",
                for file in pack.files.iter() {
                    article {
                        key: "{file.key}",
                        style: "display: grid; grid-template-columns: 64px minmax(0, 1fr) 72px 56px; gap: 10px; align-items: center; padding: 9px 14px; border-bottom: 1px solid #222b34;",
                        div { style: "font-size: 12px; color: #8fc7ff;", "{file.kind}" }
                        div {
                            style: "min-width: 0;",
                            div {
                                style: "font-size: 13px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                "{file.name}"
                            }
                            if let Some(metadata) = &file.metadata_label {
                                div {
                                    style: "font-size: 12px; color: #9fb0c0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                    "{metadata}"
                                }
                            }
                        }
                        div { style: "font-size: 12px; color: #9fb0c0; text-align: right;", "{file.size_label}" }
                        div { style: "font-size: 12px; color: #9fb0c0; text-align: right;", "{file.compression_label}" }
                    }
                }
            }
        }
    }
}

fn initial_app_state() -> AppState {
    AppState::with_mods(GameId::Warhammer3, Vec::new())
}

fn selected_pack_from_args() -> (Option<PackViewModel>, Option<String>) {
    selected_pack_from_optional_arg(env::args().nth(1))
}

fn selected_pack_from_optional_arg(
    pack_path: Option<String>,
) -> (Option<PackViewModel>, Option<String>) {
    pack_path
        .map(PathBuf::from)
        .map(load_pack_from_path)
        .unwrap_or((None, None))
}

fn pick_pack_file() -> Option<PathBuf> {
    let dialog = rfd::FileDialog::new()
        .set_title("Open WH3 pack")
        .add_filter("WH3 pack", &["pack"]);
    let fixture_dir = PathBuf::from("pack_examples_from_steam");
    let dialog = if fixture_dir.exists() {
        dialog.set_directory(fixture_dir)
    } else {
        dialog
    };

    dialog.pick_file()
}

fn pick_mod_folder() -> Option<PathBuf> {
    let dialog = rfd::FileDialog::new().set_title("Open WH3 mod folder");
    let fixture_dir = PathBuf::from("pack_examples_from_steam");
    let dialog = if fixture_dir.exists() {
        dialog.set_directory(fixture_dir)
    } else {
        dialog
    };

    dialog.pick_folder()
}

fn pick_game_folder() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Open WH3 game folder")
        .pick_folder()
}

fn pick_steam_helper_file() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Open Steam helper executable")
        .pick_file()
}

fn pick_legacy_ts_config_file() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Import WH3MM TypeScript config")
        .add_filter("WH3MM config", &["json"])
        .pick_file()
}

fn pick_legacy_ts_config_save_file() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Export WH3MM TypeScript config")
        .set_file_name("config.json")
        .add_filter("WH3MM config", &["json"])
        .save_file()
}

fn select_game_folder(saved_game_folder: Option<PathBuf>) -> Option<PathBuf> {
    saved_game_folder
        .or_else(auto_discover_game_folder)
        .or_else(pick_game_folder)
}

fn auto_discover_game_folder() -> Option<PathBuf> {
    discover_wh3_steam_install_from_windows_registry()
        .ok()
        .map(|install| install.game_dir)
}

fn build_launch_preview(
    state: &AppState,
    game_folder: PathBuf,
    launch_options: &LaunchOptionState,
    save_name: &str,
) -> wh3mm_core::CoreResult<LaunchPreview> {
    let validated = validate_wh3_game_folder(&game_folder)?;
    let options = build_windows_launch_options(
        validated.game_dir.display().to_string(),
        validated.data_dir.display().to_string(),
        &state.mods,
        launch_options,
        save_name,
    )?;
    let plan = plan_windows_launch(&options, &state.mods)?;
    Ok(LaunchPreview {
        game_dir: options.game_dir,
        data_dir: options.data_dir,
        mod_list_file_name: plan.mod_list_file_name,
        mod_list_contents: plan.mod_list_contents,
        command_line_preview: plan.command_line_preview,
        enabled_count: state
            .mods
            .iter()
            .filter(|mod_record| mod_record.effectively_enabled())
            .count(),
        pre_launch_copies: plan
            .pre_launch_copies
            .into_iter()
            .map(|copy| LaunchCopyPreview {
                from_path: copy.from_path,
                to_path: copy.to_path,
            })
            .collect(),
        generated_packs: plan
            .pre_launch_pack_writes
            .into_iter()
            .map(|write| GeneratedPackPreview {
                packed_file_summary: write.packed_file_names.join(", "),
                path: write.path,
                byte_len: write.bytes.len(),
                packed_file_names: write.packed_file_names,
            })
            .collect(),
        fingerprint: launch_state_fingerprint(&state.mods, launch_options, save_name),
    })
}

fn launch_state_fingerprint(
    mods: &[ModRecord],
    launch_options: &LaunchOptionState,
    save_name: &str,
) -> String {
    let mut parts = vec![format!(
        "launch-options|{}|{}|{}|{}|{}|{}|{}|{:?}|{:?}",
        launch_options.make_units_generals,
        launch_options.skip_intro_movies,
        launch_options.script_logging,
        launch_options.auto_start_custom_battle,
        launch_options.high_process_priority,
        launch_options.close_on_play,
        save_name.trim(),
        launch_options.pack_data_overwrites,
        launch_options.user_flow_options
    )];
    parts.extend(
        mods.iter()
            .filter(|mod_record| mod_record.effectively_enabled())
            .map(|mod_record| {
                format!(
                    "{}|{}|{}",
                    mod_record.identity.stable_key(),
                    mod_record.identity.path,
                    mod_record.display_name
                )
            }),
    );
    parts.join("\n")
}

fn prepare_launch_for_game_folder(
    state: &AppState,
    game_folder: PathBuf,
    launch_options: &LaunchOptionState,
    save_name: &str,
) -> wh3mm_core::CoreResult<String> {
    let validated = validate_wh3_game_folder(&game_folder)?;
    let options = build_windows_launch_options(
        validated.game_dir.display().to_string(),
        validated.data_dir.display().to_string(),
        &state.mods,
        launch_options,
        save_name,
    )?;
    let plan = plan_windows_launch(&options, &state.mods)?;
    let prepared = prepare_windows_launch_files(&plan, &LaunchPreparationOptions::default())?;
    let generated_details = generated_pack_details(&plan.pre_launch_pack_writes);
    Ok(format!(
        "Prepared {} with {} enabled mods, {} generated packs [{}], and {} pre-launch copies. Launch args: {}",
        prepared.mod_list_path.display(),
        state
            .mods
            .iter()
            .filter(|mod_record| mod_record.effectively_enabled())
            .count(),
        prepared.written_pack_files.len(),
        generated_details,
        prepared.copied_files.len(),
        prepared.args.join(" ")
    ))
}

fn launch_game_for_game_folder(
    state: &AppState,
    game_folder: PathBuf,
    launch_options: &LaunchOptionState,
    save_name: &str,
) -> wh3mm_core::CoreResult<String> {
    let validated = validate_wh3_game_folder(&game_folder)?;
    let options = build_windows_launch_options(
        validated.game_dir.display().to_string(),
        validated.data_dir.display().to_string(),
        &state.mods,
        launch_options,
        save_name,
    )?;
    let plan = plan_windows_launch(&options, &state.mods)?;
    let prepared = prepare_windows_launch_files(&plan, &LaunchPreparationOptions::default())?;
    let spawn_options = WindowsLaunchSpawnOptions {
        priority_class: launch_options
            .high_process_priority
            .then_some(WindowsProcessPriorityClass::High),
    };
    let (child, priority_update) =
        spawn_prepared_windows_launch_with_options(&prepared, &spawn_options)?;
    let generated_details = generated_pack_details(&plan.pre_launch_pack_writes);
    let priority_status = launch_priority_status(priority_update.as_ref());
    let preset_status = save_on_last_game_launch_preset(state).unwrap_or_else(|error| {
        format!(
            "Could not save \"{ON_LAST_GAME_LAUNCH_PRESET_NAME}\" preset: {}.",
            error.message
        )
    });
    Ok(format!(
        "Launched {} with process id {} after writing {}. Enabled mods: {}. Generated packs: {} [{}]. Pre-launch copies: {}. {} {}",
        prepared.executable,
        child.id(),
        prepared.mod_list_path.display(),
        state
            .mods
            .iter()
            .filter(|mod_record| mod_record.effectively_enabled())
            .count(),
        prepared.written_pack_files.len(),
        generated_details,
        prepared.copied_files.len(),
        priority_status,
        preset_status
    ))
}

fn launch_priority_status(priority_update: Option<&WindowsProcessPriorityUpdate>) -> String {
    match priority_update {
        Some(update) if update.applied => {
            format!("Process priority: {}.", update.message)
        }
        Some(update) if update.attempted => {
            format!("Process priority was not changed: {}.", update.message)
        }
        Some(update) => {
            format!("Process priority skipped: {}.", update.message)
        }
        None => "Process priority unchanged.".to_string(),
    }
}

fn schedule_close_on_play_if_requested(close_on_play: bool) {
    if close_on_play {
        thread::spawn(|| {
            thread::sleep(Duration::from_secs(CLOSE_ON_PLAY_DELAY_SECS));
            std::process::exit(0);
        });
    }
}

fn launch_status_with_close_on_play(status: String, close_on_play: bool) -> String {
    if close_on_play {
        format!("{status} Close-on-play requested; app will exit in {CLOSE_ON_PLAY_DELAY_SECS}s.")
    } else {
        status
    }
}

fn build_windows_launch_options(
    game_dir: String,
    data_dir: String,
    mods: &[ModRecord],
    launch_options: &LaunchOptionState,
    save_name: &str,
) -> wh3mm_core::CoreResult<WindowsLaunchOptions> {
    let mut options = WindowsLaunchOptions::warhammer3(game_dir, data_dir);
    options.save_name = normalized_launch_save_name(save_name);
    let start_game_options = Wh3StartGamePackOptions {
        make_units_generals: launch_options.make_units_generals,
        skip_intro_movies: launch_options.skip_intro_movies,
        script_logging: launch_options.script_logging,
        auto_start_custom_battle: launch_options.auto_start_custom_battle,
    };
    append_pack_data_overwrite_packs_from_schema_file(&mut options, mods, launch_options)?;
    let (battle_permission_tables, battle_permission_schema) =
        battle_permission_tables_for_start_game(mods, &options.data_dir, launch_options)?;
    if let Some(generated) = build_wh3_start_game_pack_with_battle_permissions(
        &start_game_options,
        &battle_permission_tables,
        &battle_permission_schema,
    )? {
        debug_assert_eq!(generated.pack_name, WH3_START_GAME_PACK_NAME);
        let temp_dir = start_game_temp_packs_dir();
        let pack_path = temp_dir.join(&generated.pack_name);
        options.extra_pack_groups.push(WindowsLaunchPackGroup::new(
            temp_dir.display().to_string(),
            [generated.pack_name],
        ));
        options.pre_launch_pack_writes.push(PreLaunchPackWrite {
            path: pack_path.display().to_string(),
            bytes: generated.bytes,
            packed_file_names: generated.packed_file_names,
        });
    }
    Ok(options)
}

fn append_pack_data_overwrite_packs_from_schema_file(
    options: &mut WindowsLaunchOptions,
    mods: &[ModRecord],
    launch_options: &LaunchOptionState,
) -> wh3mm_core::CoreResult<()> {
    if launch_options.pack_data_overwrites.is_empty() {
        return Ok(());
    }

    let schema_path = schema_path();
    let schema = wh3mm_core::load_schema_file(&schema_path).map_err(|error| {
        wh3mm_core::CoreError::parse(format!(
            "could not load WH3 schema at {} for pack overwrites: {}",
            schema_path.display(),
            error.message
        ))
    })?;
    append_pack_data_overwrite_packs(options, mods, launch_options, &schema)
}

fn append_pack_data_overwrite_packs(
    options: &mut WindowsLaunchOptions,
    mods: &[ModRecord],
    launch_options: &LaunchOptionState,
    schema: &wh3mm_core::DbSchema,
) -> wh3mm_core::CoreResult<()> {
    let overwrite_mods =
        enabled_mods_with_pack_data_overwrites(mods, &launch_options.pack_data_overwrites);
    if overwrite_mods.is_empty() {
        return Ok(());
    }

    let overwrites_dir = PathBuf::from(&options.game_dir).join("whmm_overwrites");
    let mut overwrite_pack_names = Vec::new();
    for (mod_record, overwrites) in overwrite_mods {
        let pack_path = mod_record.identity.path.trim();
        let pack_name = pack_file_name(pack_path).ok_or_else(|| {
            wh3mm_core::CoreError::invalid_input(format!(
                "pack overwrite source path has no file name: {pack_path}"
            ))
        })?;
        let Some(generated) = build_pack_data_overwrite_pack(pack_path, overwrites, schema)? else {
            continue;
        };

        overwrite_pack_names.push(pack_name.to_string());
        options.replaced_pack_paths.push(pack_path.to_string());
        options.pre_launch_pack_writes.push(PreLaunchPackWrite {
            path: overwrites_dir.join(pack_name).display().to_string(),
            bytes: generated.bytes,
            packed_file_names: generated.packed_file_names,
        });
    }

    if !overwrite_pack_names.is_empty() {
        options.extra_pack_groups.push(WindowsLaunchPackGroup::new(
            overwrites_dir.display().to_string(),
            overwrite_pack_names,
        ));
    }

    Ok(())
}

fn enabled_mods_with_pack_data_overwrites<'a>(
    mods: &'a [ModRecord],
    pack_data_overwrites: &'a BTreeMap<String, Vec<PackDataOverwrite>>,
) -> Vec<(&'a ModRecord, &'a [PackDataOverwrite])> {
    let merged_source_paths = mods
        .iter()
        .filter(|mod_record| mod_record.effectively_enabled())
        .flat_map(|mod_record| mod_record.merged_source_paths().map(str::to_string))
        .collect::<Vec<_>>();

    mods.iter()
        .filter(|mod_record| mod_record.effectively_enabled())
        .filter(|mod_record| !is_merged_source_mod_for_launch(mod_record, &merged_source_paths))
        .filter_map(|mod_record| {
            let overwrites = pack_data_overwrites_for_mod(pack_data_overwrites, mod_record)?;
            (!overwrites.is_empty()).then_some((mod_record, overwrites.as_slice()))
        })
        .collect()
}

fn pack_data_overwrites_for_mod<'a>(
    pack_data_overwrites: &'a BTreeMap<String, Vec<PackDataOverwrite>>,
    mod_record: &ModRecord,
) -> Option<&'a Vec<PackDataOverwrite>> {
    pack_data_overwrites
        .get(&mod_record.identity.path)
        .or_else(|| pack_data_overwrites.get(&mod_record.identity.name))
        .or_else(|| pack_data_overwrites.get(pack_file_name(&mod_record.identity.path)?))
}

fn is_merged_source_mod_for_launch(mod_record: &ModRecord, merged_source_paths: &[String]) -> bool {
    let path = mod_record.identity.path.trim();
    !path.is_empty()
        && merged_source_paths
            .iter()
            .any(|source_path| windows_path_eq(path, source_path))
}

fn windows_path_eq(left: &str, right: &str) -> bool {
    normalize_windows_path_key(left) == normalize_windows_path_key(right)
}

fn normalize_windows_path_key(path: &str) -> String {
    path.trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_ascii_lowercase()
}

fn pack_file_name(path: &str) -> Option<&str> {
    path.rsplit(['\\', '/'])
        .next()
        .filter(|file_name| !file_name.is_empty())
}

fn normalized_launch_save_name(save_name: &str) -> Option<String> {
    let save_name = save_name.trim();
    if save_name.is_empty() {
        None
    } else {
        Some(save_name.to_string())
    }
}

fn generated_pack_details(writes: &[PreLaunchPackWrite]) -> String {
    if writes.is_empty() {
        return "none".to_string();
    }

    writes
        .iter()
        .map(|write| {
            let pack_name = write
                .path
                .rsplit(['\\', '/'])
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or(&write.path);
            if write.packed_file_names.is_empty() {
                pack_name.to_string()
            } else {
                format!("{pack_name}: {}", write.packed_file_names.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn battle_permission_tables_for_start_game(
    mods: &[ModRecord],
    data_dir: &str,
    launch_options: &LaunchOptionState,
) -> wh3mm_core::CoreResult<(Vec<wh3mm_core::DbRows>, Vec<wh3mm_core::DbFieldSchema>)> {
    if !launch_options.make_units_generals {
        return Ok((Vec::new(), Vec::new()));
    }

    let pack_paths = start_game_source_pack_paths(mods, data_dir);
    if pack_paths.is_empty() {
        return Err(wh3mm_core::CoreError::invalid_input(
            "MakeUnitsGenerals found no enabled or vanilla pack paths to scan for battle-permission DB rows",
        ));
    }

    let schema_path = schema_path();
    let schema = wh3mm_core::load_schema_file(&schema_path).map_err(|error| {
        wh3mm_core::CoreError::parse(format!(
            "could not load WH3 schema at {} for MakeUnitsGenerals: {}",
            schema_path.display(),
            error.message
        ))
    })?;
    let collected = read_wh3_battle_permission_tables_from_packs(&pack_paths, &schema)?;
    let selected_schema = collected.schema.ok_or_else(|| {
        wh3mm_core::CoreError::invalid_input(
            "MakeUnitsGenerals found no enabled battle-permission DB rows",
        )
    })?;

    Ok((collected.tables, selected_schema.fields))
}

fn start_game_source_pack_paths(mods: &[ModRecord], data_dir: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for path in enabled_pack_paths_for_start_game(mods)
        .into_iter()
        .chain(wh3_vanilla_start_game_pack_paths(data_dir))
    {
        push_unique_path(&mut paths, path);
    }
    paths
}

fn enabled_pack_paths_for_start_game(mods: &[ModRecord]) -> Vec<PathBuf> {
    mods.iter()
        .filter(|mod_record| mod_record.effectively_enabled())
        .filter_map(|mod_record| {
            if mod_record.identity.path.is_empty() {
                None
            } else {
                Some(PathBuf::from(&mod_record.identity.path))
            }
        })
        .collect()
}

fn wh3_vanilla_start_game_pack_paths(data_dir: &str) -> Vec<PathBuf> {
    let data_dir = data_dir.trim();
    if data_dir.is_empty() {
        return Vec::new();
    }

    WH3_START_GAME_SOURCE_PACK_NAMES
        .iter()
        .map(|pack_name| PathBuf::from(data_dir).join(pack_name))
        .filter(|path| path.is_file())
        .collect()
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    let key = path_key(&path);
    if !paths.iter().any(|existing| path_key(existing) == key) {
        paths.push(path);
    }
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase()
}

fn start_game_temp_packs_dir() -> PathBuf {
    start_game_temp_packs_dir_for_config_dir(&app_config_dir())
}

fn start_game_temp_packs_dir_for_config_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("tempPacks")
}

fn load_saved_game_folder() -> Option<PathBuf> {
    read_game_folder_config(game_folder_config_read_path())
        .ok()
        .and_then(|config| {
            (!config.game_dir.trim().is_empty()).then(|| PathBuf::from(config.game_dir))
        })
}

fn save_game_folder(game_folder: &Path) -> wh3mm_core::CoreResult<String> {
    let validated = validate_wh3_game_folder(game_folder)?;
    let config = capture_game_folder_config(validated.game_dir.display().to_string());
    let path = game_folder_config_path();
    write_game_folder_config_atomic(&path, &config)?;
    Ok(format!(
        "Saved WH3 game folder {} to {}.",
        validated.game_dir.display(),
        path.display()
    ))
}

fn game_folder_config_path() -> PathBuf {
    config_file_write_path(GAME_FOLDER_CONFIG_FILE)
}

fn game_folder_config_read_path() -> PathBuf {
    config_file_read_path(GAME_FOLDER_CONFIG_FILE)
}

fn load_saved_steam_helper_path() -> String {
    if let Ok(path) = env::var("WH3MM_STEAM_HELPER") {
        if !path.trim().is_empty() {
            return path;
        }
    }

    if let Ok(config) = read_steam_helper_config(steam_helper_config_read_path())
        && !config.helper_path.trim().is_empty()
    {
        return config.helper_path;
    }

    discover_default_steam_helper_path()
        .map(|path| path.display().to_string())
        .unwrap_or_default()
}

fn load_saved_steam_helper_backend() -> String {
    if let Ok(backend) = env::var(STEAM_HELPER_BACKEND_ENV)
        && let Ok(backend) = normalize_steam_helper_backend(&backend)
    {
        return backend;
    }

    if let Ok(config) = read_steam_helper_config(steam_helper_config_read_path())
        && let Some(backend) = config.backend
        && let Ok(backend) = normalize_steam_helper_backend(&backend)
    {
        return backend;
    }

    STEAM_HELPER_BACKEND_NATIVE.to_string()
}

fn build_alpha_readiness_report(
    game_folder: Option<&Path>,
    helper_path: &str,
) -> AlphaReadinessReport {
    build_alpha_readiness_report_with_paths(
        game_folder,
        helper_path,
        &schema_path(),
        &app_config_dir(),
    )
}

fn build_alpha_readiness_report_with_paths(
    game_folder: Option<&Path>,
    helper_path: &str,
    schema_path: &Path,
    config_dir: &Path,
) -> AlphaReadinessReport {
    let mut rows = Vec::new();
    rows.push(readiness_row(
        "Config",
        if config_dir.is_dir() {
            AlphaReadinessStatus::Ready
        } else {
            AlphaReadinessStatus::Warning
        },
        if config_dir.is_dir() {
            format!("Using {}", config_dir.display())
        } else {
            format!("Will create {}", config_dir.display())
        },
    ));
    rows.push(readiness_row(
        "Schema",
        if schema_path.is_file() {
            AlphaReadinessStatus::Ready
        } else {
            AlphaReadinessStatus::Error
        },
        if schema_path.is_file() {
            format!("Found {}", schema_path.display())
        } else {
            format!("Missing {}", schema_path.display())
        },
    ));

    let helper_path = helper_path.trim();
    let helper = (!helper_path.is_empty()).then(|| PathBuf::from(helper_path));
    match helper.as_deref() {
        Some(path) if path.is_file() => {
            rows.push(readiness_row(
                "Steam helper",
                AlphaReadinessStatus::Ready,
                format!("Found {}", path.display()),
            ));
            rows.push(steam_runtime_readiness_row(path));
        }
        Some(path) => {
            rows.push(readiness_row(
                "Steam helper",
                AlphaReadinessStatus::Error,
                format!("Missing {}", path.display()),
            ));
            rows.push(readiness_row(
                "Steam DLL",
                AlphaReadinessStatus::Warning,
                "Helper path must be valid before checking steam_api64.dll.".to_string(),
            ));
        }
        None => {
            rows.push(readiness_row(
                "Steam helper",
                AlphaReadinessStatus::Warning,
                "No helper selected or auto-discovered.".to_string(),
            ));
            rows.push(readiness_row(
                "Steam DLL",
                AlphaReadinessStatus::Warning,
                "No helper selected; steam_api64.dll should sit beside the packaged helper."
                    .to_string(),
            ));
        }
    }

    let game_status = game_folder.map(validate_wh3_game_folder);
    match (&game_folder, &game_status) {
        (Some(_), Some(Ok(validated))) => {
            rows.push(readiness_row(
                "WH3 folder",
                AlphaReadinessStatus::Ready,
                format!("Found {}", validated.game_dir.display()),
            ));
            rows.push(match discover_wh3_workshop_folder(&validated.game_dir) {
                Ok(workshop) => readiness_row(
                    "Workshop",
                    AlphaReadinessStatus::Ready,
                    format!("Found {}", workshop.workshop_content_dir.display()),
                ),
                Err(error) => {
                    readiness_row("Workshop", AlphaReadinessStatus::Warning, error.message)
                }
            });
        }
        (Some(path), Some(Err(error))) => {
            rows.push(readiness_row(
                "WH3 folder",
                AlphaReadinessStatus::Error,
                format!("{} ({})", path.display(), error.message),
            ));
            rows.push(readiness_row(
                "Workshop",
                AlphaReadinessStatus::Warning,
                "WH3 folder must validate before checking workshop content.".to_string(),
            ));
        }
        _ => {
            rows.push(readiness_row(
                "WH3 folder",
                AlphaReadinessStatus::Warning,
                "No WH3 folder selected.".to_string(),
            ));
            rows.push(readiness_row(
                "Workshop",
                AlphaReadinessStatus::Warning,
                "No WH3 folder selected; workshop content is unknown.".to_string(),
            ));
        }
    }

    let ready_count = rows
        .iter()
        .filter(|row| row.status == AlphaReadinessStatus::Ready)
        .count();
    let warning_count = rows
        .iter()
        .filter(|row| row.status == AlphaReadinessStatus::Warning)
        .count();
    let error_count = rows
        .iter()
        .filter(|row| row.status == AlphaReadinessStatus::Error)
        .count();

    AlphaReadinessReport {
        summary: format!("{ready_count} ready / {warning_count} checks / {error_count} errors"),
        rows,
    }
}

fn readiness_row(label: &str, status: AlphaReadinessStatus, detail: String) -> AlphaReadinessRow {
    AlphaReadinessRow {
        label: label.to_string(),
        status,
        detail,
    }
}

fn steam_runtime_readiness_row(helper_path: &Path) -> AlphaReadinessRow {
    let Some(helper_dir) = helper_path.parent() else {
        return readiness_row(
            "Steam DLL",
            AlphaReadinessStatus::Error,
            format!(
                "Could not resolve helper directory for {}",
                helper_path.display()
            ),
        );
    };
    let steam_dll = helper_dir.join("steam_api64.dll");
    if steam_dll.is_file() {
        readiness_row(
            "Steam DLL",
            AlphaReadinessStatus::Ready,
            format!("Found {}", steam_dll.display()),
        )
    } else {
        readiness_row(
            "Steam DLL",
            AlphaReadinessStatus::Error,
            format!("Missing {}", steam_dll.display()),
        )
    }
}

fn discover_default_steam_helper_path() -> Option<PathBuf> {
    first_existing_steam_helper_path(&steam_helper_candidate_paths())
}

fn first_existing_steam_helper_path(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|path| path.is_file()).cloned()
}

fn steam_helper_candidate_paths() -> Vec<PathBuf> {
    let helper_name = steam_helper_executable_name();
    let mut candidates = Vec::new();

    if let Some(app_dir) = env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    {
        candidates.push(app_dir.join(helper_name));
        candidates.push(app_dir.join("helpers").join(helper_name));
    }

    let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    candidates.push(current_dir.join(helper_name));
    candidates.push(current_dir.join("target").join("debug").join(helper_name));
    candidates.push(current_dir.join("target").join("release").join(helper_name));
    candidates
}

fn steam_helper_executable_name() -> &'static str {
    if cfg!(windows) {
        "wh3mm-steam-helper.exe"
    } else {
        "wh3mm-steam-helper"
    }
}

fn save_steam_helper_settings(helper_path: &str, backend: &str) -> wh3mm_core::CoreResult<String> {
    let helper_path = helper_path.trim();
    if helper_path.is_empty() {
        return Err(wh3mm_core::CoreError::invalid_input(
            "Steam helper path is required",
        ));
    }
    let backend = normalize_steam_helper_backend(backend)?;
    let path = PathBuf::from(helper_path);
    if !path.is_file() {
        return Err(wh3mm_core::CoreError::invalid_input(format!(
            "Steam helper does not exist: {}",
            path.display()
        )));
    }

    let config =
        capture_steam_helper_config_with_backend(path.display().to_string(), Some(backend));
    let config_path = steam_helper_config_path();
    write_steam_helper_config_atomic(&config_path, &config)?;
    Ok(format!(
        "Saved Steam helper {} ({}) to {}.",
        path.display(),
        config
            .backend
            .as_deref()
            .unwrap_or(STEAM_HELPER_BACKEND_NATIVE),
        config_path.display()
    ))
}

fn normalize_steam_helper_backend(backend: &str) -> wh3mm_core::CoreResult<String> {
    match backend.trim().to_ascii_lowercase().as_str() {
        "" | STEAM_HELPER_BACKEND_NATIVE => Ok(STEAM_HELPER_BACKEND_NATIVE.to_string()),
        STEAM_HELPER_BACKEND_FIXTURE => Ok(STEAM_HELPER_BACKEND_FIXTURE.to_string()),
        other => Err(wh3mm_core::CoreError::invalid_input(format!(
            "unsupported Steam helper backend {other:?}; expected native or fixture"
        ))),
    }
}

fn steam_helper_process_config(
    backend: &str,
) -> wh3mm_core::CoreResult<SteamWorkshopHelperProcessConfig> {
    Ok(SteamWorkshopHelperProcessConfig {
        timeout: Duration::from_secs(30),
        env_overrides: vec![
            (
                STEAM_HELPER_BACKEND_ENV.to_string(),
                normalize_steam_helper_backend(backend)?,
            ),
            (
                STEAM_HELPER_COMMAND_LOG_ENV.to_string(),
                steam_helper_command_log_path().display().to_string(),
            ),
        ],
    })
}

fn steam_helper_process_runner(
    helper_path: &Path,
    backend: &str,
) -> wh3mm_core::CoreResult<SteamWorkshopHelperProcessRunner> {
    Ok(SteamWorkshopHelperProcessRunner::with_config(
        helper_path,
        steam_helper_process_config(backend)?,
    ))
}

fn steam_helper_config_path() -> PathBuf {
    config_file_write_path(STEAM_HELPER_CONFIG_FILE)
}

fn steam_helper_config_read_path() -> PathBuf {
    config_file_read_path(STEAM_HELPER_CONFIG_FILE)
}

fn load_mods_from_folder(folder: PathBuf) -> wh3mm_core::CoreResult<(Vec<ModRecord>, String)> {
    let options = discovery_options_for_folder(folder.clone());
    let mut mods = discover_mods(&options)?;
    let mut status = format!("Discovered {} mods from {}.", mods.len(), folder.display());
    mods = apply_saved_or_existing_game_mod_list(
        mods,
        &mod_state_config_read_path(),
        None,
        &mut status,
    );
    if let Ok(config) = read_mod_user_config(mod_user_config_read_path()) {
        mods = apply_mod_user_config(mods, &config);
        status.push_str(" Restored categories/visibility.");
    }
    Ok((mods, status))
}

fn load_mods_from_game_folder(
    game_folder: PathBuf,
) -> wh3mm_core::CoreResult<(Vec<ModRecord>, String)> {
    let validated = validate_wh3_game_folder(&game_folder)?;
    let workshop_content_dir = discover_wh3_workshop_folder(&validated.game_dir)
        .ok()
        .map(|workshop| workshop.workshop_content_dir);
    let options = ModDiscoveryOptions {
        data_dir: Some(validated.data_dir.clone()),
        workshop_content_dir: workshop_content_dir.clone(),
        ..ModDiscoveryOptions::default()
    };
    let mut mods = discover_mods(&options)?;
    let mut status = format!(
        "Discovered {} mods from {}",
        mods.len(),
        validated.data_dir.display()
    );
    if let Some(workshop_content_dir) = workshop_content_dir {
        status.push_str(&format!(" and {}.", workshop_content_dir.display()));
    } else {
        status.push_str(". Workshop folder was not found.");
    }

    mods = apply_saved_or_existing_game_mod_list(
        mods,
        &mod_state_config_read_path(),
        Some(&validated.game_dir),
        &mut status,
    );
    if let Ok(config) = read_mod_user_config(mod_user_config_read_path()) {
        mods = apply_mod_user_config(mods, &config);
        status.push_str(" Restored categories/visibility.");
    }

    Ok((mods, status))
}

fn apply_saved_or_existing_game_mod_list(
    mods: Vec<ModRecord>,
    mod_state_path: &Path,
    game_dir: Option<&Path>,
    status: &mut String,
) -> Vec<ModRecord> {
    if let Ok(config) = read_mod_list_config(mod_state_path) {
        status.push_str(" Restored saved enablement/order.");
        return apply_mod_list_config(mods, &config);
    }

    if let Some(game_dir) = game_dir
        && let Some((file_name, pack_names)) = read_existing_launch_mod_list_pack_names(game_dir)
    {
        status.push_str(&format!(" Restored enablement/order from {file_name}."));
        return apply_mod_list_pack_names(mods, &pack_names);
    }

    mods
}

fn read_existing_launch_mod_list_pack_names(game_dir: &Path) -> Option<(String, Vec<String>)> {
    for file_name in ["used_mods.txt", "my_mods.txt"] {
        let path = game_dir.join(file_name);
        let Ok(contents) = fs::read_to_string(path) else {
            continue;
        };
        let pack_names = parse_mod_list_pack_names(&contents);
        if !pack_names.is_empty() {
            return Some((file_name.to_string(), pack_names));
        }
    }
    None
}

fn probe_steam_helper(helper_path: &Path, backend: &str) -> Result<String, String> {
    validate_steam_helper_path(helper_path)?;
    let mut runner =
        steam_helper_process_runner(helper_path, backend).map_err(|error| error.message)?;
    let json = runner
        .probe_json(WH3_STEAM_APP_ID)
        .map_err(|error| error.message)?;
    Ok(steam_probe_status(&json))
}

fn steam_probe_status(json: &str) -> String {
    match serde_json::from_str::<SteamHelperProbeReport>(json) {
        Ok(report) => steam_probe_status_from_report(&report),
        Err(error) => format!("Steam helper probe returned malformed JSON: {error}. Raw: {json}"),
    }
}

fn steam_probe_status_from_report(report: &SteamHelperProbeReport) -> String {
    let mut parts = vec![format!(
        "Steam helper probe: backend {} for app {}.",
        report.selected_backend, report.app_id
    )];

    if report.selected_backend == "fixture" {
        if report.fixture_available {
            parts.push("Fixture backend ready.".to_string());
        } else if report.fixture_configured {
            parts.push("Fixture backend selected but fixture could not be loaded.".to_string());
        } else {
            parts.push("Fixture backend selected without a fixture file.".to_string());
        }
    }

    let native_state = if report.native_implemented {
        if report.native_available {
            "native Steamworks available"
        } else {
            "native Steamworks compiled; live commands initialize Steam"
        }
    } else {
        "native Steamworks unavailable in this build"
    };
    if report.native_status.trim().is_empty() {
        parts.push(format!("Native: {native_state}."));
    } else {
        parts.push(format!(
            "Native: {native_state} ({}).",
            report.native_status
        ));
    }

    if report.command_log_configured {
        parts.push("Command log enabled.".to_string());
    }

    parts.push(steam_runtime_redistributable_status(report));
    parts.join(" ")
}

fn steam_runtime_redistributable_status(report: &SteamHelperProbeReport) -> String {
    if report.windows_runtime_redistributable_statuses.is_empty() {
        if report.windows_runtime_redistributables.is_empty() {
            return "No Windows runtime redistributables reported.".to_string();
        }

        return format!(
            "Windows runtime redistributables expected: {}.",
            report.windows_runtime_redistributables.join(", ")
        );
    }

    let missing = report
        .windows_runtime_redistributable_statuses
        .iter()
        .filter(|status| !status.present)
        .map(|status| format!("{} at {}", status.file_name, status.expected_path))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        let present = report
            .windows_runtime_redistributable_statuses
            .iter()
            .map(|status| status.file_name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return format!("Windows runtime redistributables present: {present}.");
    }

    format!(
        "Missing Windows runtime redistributables: {}.",
        missing.join("; ")
    )
}

fn refresh_steam_from_helper(
    state: &mut AppState,
    helper_path: &Path,
    backend: &str,
) -> Result<SteamRefreshResult, String> {
    validate_steam_helper_path(helper_path)?;

    let mut command_adapter = SteamWorkshopCommandAdapter::new(
        WH3_STEAM_APP_ID,
        steam_helper_process_runner(helper_path, backend).map_err(|error| error.message)?,
    );
    let subscribed_ids = command_adapter
        .subscribed_workshop_ids()
        .map_err(|error| error.message)?;

    let before_filter_len = state.mods.len();
    if !subscribed_ids.is_empty() {
        let subscribed_set = subscribed_ids.iter().cloned().collect::<BTreeSet<_>>();
        state.mods.retain(|mod_record| {
            mod_record
                .identity
                .workshop_id
                .as_ref()
                .is_none_or(|workshop_id| subscribed_set.contains(workshop_id))
        });
    }
    let filtered_unsubscribed_count = before_filter_len.saturating_sub(state.mods.len());

    let workshop_ids = workshop_ids_from_mods(&state.mods);
    let mut metadata_adapter = TsSteamHelperMetadataAdapter::new(
        WH3_STEAM_APP_ID,
        steam_helper_process_runner(helper_path, backend).map_err(|error| error.message)?,
    );
    let metadata_result = fetch_steam_metadata_safely(&mut metadata_adapter, &workshop_ids)?;
    let renamed_count = apply_steam_metadata_to_mods(&mut state.mods, &metadata_result.metadata);

    Ok(SteamRefreshResult {
        subscribed_ids,
        metadata: metadata_result.metadata,
        requested_metadata_count: metadata_result.requested_count,
        missing_metadata_count: metadata_result.missing_count,
        filtered_unsubscribed_count,
        renamed_count,
    })
}

fn check_steam_updates_with_helper(
    state: &AppState,
    helper_path: &Path,
    backend: &str,
) -> Result<SteamWorkshopCheckStateResult, String> {
    validate_steam_helper_path(helper_path)?;
    let workshop_ids = workshop_ids_from_mods(&state.mods);
    if workshop_ids.is_empty() {
        return Ok(SteamWorkshopCheckStateResult::default());
    }

    let mut command_adapter = SteamWorkshopCommandAdapter::new(
        WH3_STEAM_APP_ID,
        steam_helper_process_runner(helper_path, backend).map_err(|error| error.message)?,
    );
    let result = command_adapter
        .check_state_and_download_updates(&workshop_ids)
        .map_err(|error| error.message)?;

    Ok(result)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SteamCommandAction {
    Subscribe,
    Download,
    Unsubscribe,
    Resubscribe,
}

impl SteamCommandAction {
    fn status_prefix(self) -> &'static str {
        match self {
            Self::Subscribe => "Subscribed to",
            Self::Download => "Requested download for",
            Self::Unsubscribe => "Unsubscribed from",
            Self::Resubscribe => "Resubscribed",
        }
    }

    fn panel_title(self) -> &'static str {
        match self {
            Self::Subscribe => "Steam subscribe",
            Self::Download => "Steam download",
            Self::Unsubscribe => "Steam unsubscribe",
            Self::Resubscribe => "Steam resubscribe",
        }
    }
}

fn run_steam_command_with_helper(
    action: SteamCommandAction,
    helper_path: &Path,
    backend: &str,
    raw_ids: &str,
    mods: &[ModRecord],
) -> Result<SteamCommandUiResult, String> {
    validate_steam_helper_path(helper_path)?;
    let ids = workshop_ids_from_input(raw_ids);
    if ids.is_empty() {
        return Err("at least one numeric workshop ID is required".to_string());
    }

    let mut command_adapter = SteamWorkshopCommandAdapter::new(
        WH3_STEAM_APP_ID,
        steam_helper_process_runner(helper_path, backend).map_err(|error| error.message)?,
    );
    if action == SteamCommandAction::Resubscribe {
        let result = resubscribe_with_cleanup_and_verification(
            mods,
            &ids,
            &mut command_adapter,
            &SteamResubscribeSafetyConfig::default(),
        )
        .map_err(|error| error.message)?;
        return Ok(SteamCommandUiResult {
            status: steam_resubscribe_status(&result),
            panel: steam_resubscribe_panel_state(&result),
        });
    }

    let result = run_steam_command_action(action, &mut command_adapter, &ids)
        .map_err(|error| error.message)?;
    Ok(SteamCommandUiResult {
        status: steam_command_status(action, &result),
        panel: steam_command_panel_state(action, &result),
    })
}

fn run_steam_command_action<R>(
    action: SteamCommandAction,
    command_adapter: &mut SteamWorkshopCommandAdapter<R>,
    ids: &[String],
) -> Result<SteamWorkshopCommandResult, wh3mm_core::SteamWorkshopAdapterError>
where
    R: SteamWorkshopCommandRunner,
{
    match action {
        SteamCommandAction::Subscribe => command_adapter.subscribe(ids),
        SteamCommandAction::Download => command_adapter.download(ids),
        SteamCommandAction::Unsubscribe => command_adapter.unsubscribe(ids),
        SteamCommandAction::Resubscribe => {
            let unsubscribe_result = command_adapter.unsubscribe(ids)?;
            let requested_ids = unsubscribe_result.requested_ids;
            command_adapter.subscribe(&requested_ids)?;
            command_adapter.download(&requested_ids)?;
            Ok(SteamWorkshopCommandResult::requested(
                "resubscribe",
                requested_ids,
            ))
        }
    }
}

fn steam_command_status(action: SteamCommandAction, result: &SteamWorkshopCommandResult) -> String {
    let mut status = steam_command_status_for_count(action, result.requested_ids.len());
    if let Some(summary) = workshop_id_summary(&result.confirmed_ids, 5) {
        status.push_str(&format!(
            " Helper confirmed {} {}: {summary}.",
            result.confirmed_ids.len(),
            if result.confirmed_ids.len() == 1 {
                "ID"
            } else {
                "IDs"
            }
        ));
    }
    if let Some(delay_ms) = result.delay_ms {
        status.push_str(&format!(" Helper delay: {delay_ms}ms."));
    }
    status
}

fn steam_refresh_panel_state(result: &SteamRefreshResult) -> SteamCommandPanelState {
    SteamCommandPanelState {
        title: "Steam refresh".to_string(),
        summary: format!(
            "{} subscribed IDs, {} metadata rows, {} missing.",
            result.subscribed_ids.len(),
            result.metadata.len(),
            result.missing_metadata_count
        ),
        rows: vec![
            steam_panel_row("Subscribed IDs", ids_panel_value(&result.subscribed_ids)),
            steam_panel_row(
                "Metadata",
                format!(
                    "{} requested / {} loaded / {} missing",
                    result.requested_metadata_count,
                    result.metadata.len(),
                    result.missing_metadata_count
                ),
            ),
            steam_panel_row(
                "Loaded mods",
                format!(
                    "{} filtered as unsubscribed / {} renamed from metadata",
                    result.filtered_unsubscribed_count, result.renamed_count
                ),
            ),
        ],
    }
}

fn steam_command_panel_state(
    action: SteamCommandAction,
    result: &SteamWorkshopCommandResult,
) -> SteamCommandPanelState {
    let mut rows = vec![
        steam_panel_row("Helper command", result.command.clone()),
        steam_panel_row("Requested IDs", ids_panel_value(&result.requested_ids)),
        steam_panel_row("Confirmed IDs", ids_panel_value(&result.confirmed_ids)),
    ];
    if !result.update_requested_ids.is_empty() {
        rows.push(steam_panel_row(
            "Update downloads",
            ids_panel_value(&result.update_requested_ids),
        ));
    }
    rows.push(steam_panel_row(
        "Helper delay",
        result
            .delay_ms
            .map_or_else(|| "not reported".to_string(), |delay| format!("{delay}ms")),
    ));

    SteamCommandPanelState {
        title: action.panel_title().to_string(),
        summary: steam_command_status(action, result),
        rows,
    }
}

fn steam_check_update_panel_state(
    result: &SteamWorkshopCheckStateResult,
) -> SteamCommandPanelState {
    SteamCommandPanelState {
        title: "Steam update check".to_string(),
        summary: steam_check_update_status(result),
        rows: vec![
            steam_panel_row("Checked IDs", ids_panel_value(&result.checked_ids)),
            steam_panel_row(
                "Update downloads",
                ids_panel_value(&result.update_requested_ids),
            ),
        ],
    }
}

fn steam_resubscribe_panel_state(result: &SteamResubscribeResult) -> SteamCommandPanelState {
    SteamCommandPanelState {
        title: "Steam resubscribe".to_string(),
        summary: steam_resubscribe_status(result),
        rows: vec![
            steam_panel_row("Requested IDs", ids_panel_value(&result.requested_ids)),
            steam_panel_row(
                "Observed subscribed",
                ids_panel_value(&result.observed_subscribed_ids),
            ),
            steam_panel_row("Still missing", ids_panel_value(&result.failed_ids)),
            steam_panel_row(
                "Removed folders",
                removed_dirs_panel_value(&result.removed_dirs),
            ),
            steam_panel_row("Attempts", result.attempts.to_string()),
        ],
    }
}

fn steam_panel_row(label: &str, value: String) -> SteamCommandPanelRow {
    SteamCommandPanelRow {
        label: label.to_string(),
        value,
    }
}

fn ids_panel_value(ids: &[String]) -> String {
    workshop_id_summary(ids, 8).unwrap_or_else(|| "none".to_string())
}

fn removed_dirs_panel_value(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "none".to_string();
    }

    let visible = paths
        .iter()
        .take(3)
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    let mut value = visible.join("; ");
    let hidden_count = paths.len().saturating_sub(visible.len());
    if hidden_count > 0 {
        value.push_str(&format!(" +{hidden_count}"));
    }
    value
}

fn steam_command_status_for_count(action: SteamCommandAction, id_count: usize) -> String {
    format!(
        "{} {} workshop {}.",
        action.status_prefix(),
        id_count,
        if id_count == 1 { "mod" } else { "mods" }
    )
}

fn steam_check_update_status(result: &SteamWorkshopCheckStateResult) -> String {
    if result.checked_ids.is_empty() {
        return "No workshop mods are loaded.".to_string();
    }

    format!(
        "Checked Steam state for {} workshop {}. Requested update downloads for {} {}.",
        result.checked_ids.len(),
        if result.checked_ids.len() == 1 {
            "mod"
        } else {
            "mods"
        },
        result.update_requested_ids.len(),
        if result.update_requested_ids.len() == 1 {
            "mod"
        } else {
            "mods"
        }
    )
}

fn steam_resubscribe_status(result: &SteamResubscribeResult) -> String {
    let removed_dir_count = result.removed_dirs.len();
    let mut status = format!(
        "{} Removed {} local workshop {}.",
        steam_command_status_for_count(SteamCommandAction::Resubscribe, result.requested_ids.len()),
        removed_dir_count,
        if removed_dir_count == 1 {
            "directory"
        } else {
            "directories"
        }
    );
    if result.failed_ids.is_empty() {
        status.push_str(&format!(
            " Verified in {} {}.",
            result.attempts,
            if result.attempts == 1 {
                "attempt"
            } else {
                "attempts"
            }
        ));
    } else {
        status.push_str(&format!(
            " {} still missing after {} {}.",
            result.failed_ids.len(),
            result.attempts,
            if result.attempts == 1 {
                "attempt"
            } else {
                "attempts"
            }
        ));
    }
    status
}

fn workshop_ids_from_input(raw_ids: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut ids = Vec::new();
    for candidate in raw_ids.split(|character: char| {
        character == ',' || character == ';' || character.is_ascii_whitespace()
    }) {
        if let Some(id) = normalize_workshop_id(candidate)
            && seen.insert(id.clone())
        {
            ids.push(id);
        }
    }
    ids
}

fn workshop_id_summary(ids: &[String], max_ids: usize) -> Option<String> {
    if max_ids == 0 || ids.is_empty() {
        return None;
    }
    let visible_ids = ids
        .iter()
        .take(max_ids)
        .map(String::as_str)
        .collect::<Vec<_>>();
    let mut summary = visible_ids.join(", ");
    let hidden_count = ids.len().saturating_sub(visible_ids.len());
    if hidden_count > 0 {
        summary.push_str(&format!(" +{hidden_count}"));
    }
    Some(summary)
}

fn dependency_names_summary(item: &WorkshopModData, max_names: usize) -> Option<String> {
    if max_names == 0 || item.dependency_id_to_name.is_empty() {
        return None;
    }

    let non_empty_names = item
        .dependency_id_to_name
        .iter()
        .map(|(_, name)| name.trim())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    let names = non_empty_names
        .iter()
        .take(max_names)
        .copied()
        .collect::<Vec<_>>();
    if names.is_empty() {
        return None;
    }

    let remaining_count = non_empty_names.len().saturating_sub(names.len());
    let mut summary = names.join(", ");
    if remaining_count > 0 {
        summary.push_str(&format!(" +{remaining_count}"));
    }
    Some(summary)
}

fn fetch_steam_metadata_safely<A>(
    adapter: &mut A,
    workshop_ids: &[String],
) -> Result<SteamMetadataBatchResult, String>
where
    A: SteamWorkshopMetadataAdapter,
{
    let started_at = Instant::now();
    let mut request_state =
        SteamWorkshopRequestState::<WorkshopModData>::new(SteamWorkshopSafetyConfig::default());
    let queued = request_state.queue_workshop_ids(workshop_ids, elapsed_ms(started_at));
    let mut metadata = queued
        .cached
        .into_iter()
        .map(|cached| cached.data)
        .collect::<Vec<_>>();
    let mut requested_count = 0;
    let mut missing_count = 0;

    while request_state.queued_len() > 0 || request_state.in_flight_len() > 0 {
        match request_state.fetch_ready_workshop_batch(adapter, elapsed_ms(started_at)) {
            WorkshopMetadataFetchStep::Idle => break,
            WorkshopMetadataFetchStep::Waiting {
                wait_before_request,
            } => {
                thread::sleep(wait_before_request.min(Duration::from_secs(5)));
            }
            WorkshopMetadataFetchStep::Fetched {
                requested_ids,
                data,
                missing_ids,
            } => {
                requested_count += requested_ids.len();
                missing_count += missing_ids.len();
                metadata.extend(data);
            }
            WorkshopMetadataFetchStep::Failed {
                requested_ids,
                error,
                ..
            } => {
                return Err(format!(
                    "metadata batch for {} ids failed: {}",
                    requested_ids.len(),
                    error.message
                ));
            }
        }
    }

    Ok(SteamMetadataBatchResult {
        metadata,
        requested_count,
        missing_count,
    })
}

fn apply_steam_metadata_to_mods(mods: &mut [ModRecord], metadata: &[WorkshopModData]) -> usize {
    let metadata_by_id = metadata
        .iter()
        .map(|metadata| (metadata.workshop_id.clone(), metadata))
        .collect::<BTreeMap<_, _>>();
    let mut renamed_count = 0;

    for mod_record in mods {
        let Some(workshop_id) = &mod_record.identity.workshop_id else {
            continue;
        };
        let Some(metadata) = metadata_by_id.get(workshop_id) else {
            continue;
        };
        if metadata.title.trim().is_empty() || mod_record.display_name == metadata.title {
            continue;
        }

        mod_record.display_name.clone_from(&metadata.title);
        mod_record.identity.name.clone_from(&metadata.title);
        renamed_count += 1;
    }

    renamed_count
}

fn workshop_ids_from_mods(mods: &[ModRecord]) -> Vec<String> {
    let mut ids = Vec::new();
    for mod_record in mods {
        let Some(workshop_id) = &mod_record.identity.workshop_id else {
            continue;
        };
        if let Some(workshop_id) = normalize_workshop_id(workshop_id)
            && !ids.iter().any(|existing| existing == &workshop_id)
        {
            ids.push(workshop_id);
        }
    }
    ids
}

fn validate_steam_helper_path(helper_path: &Path) -> Result<(), String> {
    if helper_path.as_os_str().is_empty() {
        return Err("Steam helper path is required.".to_string());
    }
    if !helper_path.is_file() {
        return Err(format!(
            "Steam helper does not exist: {}",
            helper_path.display()
        ));
    }
    Ok(())
}

fn elapsed_ms(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn discovery_options_for_folder(folder: PathBuf) -> ModDiscoveryOptions {
    if folder
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("data"))
    {
        return ModDiscoveryOptions {
            data_dir: Some(folder),
            ..ModDiscoveryOptions::default()
        };
    }

    if folder
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "1142710")
        || has_numeric_child_dir(&folder)
    {
        return ModDiscoveryOptions {
            workshop_content_dir: Some(folder),
            ..ModDiscoveryOptions::default()
        };
    }

    ModDiscoveryOptions {
        extra_mod_dirs: vec![folder],
        ..ModDiscoveryOptions::default()
    }
}

fn has_numeric_child_dir(folder: &Path) -> bool {
    std::fs::read_dir(folder).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            entry.path().is_dir()
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.chars().all(|character| character.is_ascii_digit()))
        })
    })
}

fn identity_for_mod_key(state: &AppState, mod_key: &str) -> Option<ModIdentity> {
    state
        .mods
        .iter()
        .find(|mod_record| mod_record.identity.stable_key() == mod_key)
        .map(|mod_record| mod_record.identity.clone())
}

fn move_mod_by_delta(
    app_state: &mut Signal<AppState>,
    mod_status: &mut Signal<Option<String>>,
    mod_key: &str,
    delta: isize,
) {
    let mut next_state = app_state.read().clone();
    let Some(from_index) = next_state
        .mods
        .iter()
        .position(|mod_record| mod_record.identity.stable_key() == mod_key)
    else {
        mod_status.set(Some(
            "Could not move mod: mod no longer exists.".to_string(),
        ));
        return;
    };
    let target_index = from_index
        .saturating_add_signed(delta)
        .min(next_state.mods.len().saturating_sub(1));
    let identity = next_state.mods[from_index].identity.clone();

    match next_state.apply(CoreCommand::MoveMod {
        identity,
        target_index,
    }) {
        Ok(_) => {
            match save_mod_state(&next_state) {
                Ok(status) => mod_status.set(Some(status)),
                Err(error) => {
                    mod_status.set(Some(format!("Could not save mod state: {}", error.message)))
                }
            }
            app_state.set(next_state);
        }
        Err(error) => mod_status.set(Some(format!("Could not move mod: {}", error.message))),
    }
}

fn toggle_mod_hidden_by_key(
    app_state: &mut Signal<AppState>,
    mod_status: &mut Signal<Option<String>>,
    mod_key: &str,
) {
    let mut next_state = app_state.read().clone();
    let Some(mod_record) = next_state
        .mods
        .iter_mut()
        .find(|mod_record| mod_record.identity.stable_key() == mod_key)
    else {
        mod_status.set(Some(
            "Could not change visibility: mod no longer exists.".to_string(),
        ));
        return;
    };
    mod_record.hidden = !mod_record.hidden;

    match save_mod_user_state(&next_state) {
        Ok(status) => {
            mod_status.set(Some(status));
            app_state.set(next_state);
        }
        Err(error) => mod_status.set(Some(format!(
            "Could not save mod visibility: {}",
            error.message
        ))),
    }
}

fn toggle_mod_lock_by_key(
    app_state: &mut Signal<AppState>,
    mod_status: &mut Signal<Option<String>>,
    mod_key: &str,
) {
    let mut next_state = app_state.read().clone();
    let Some(mod_record) = next_state
        .mods
        .iter_mut()
        .find(|mod_record| mod_record.identity.stable_key() == mod_key)
    else {
        mod_status.set(Some(
            "Could not change lock state: mod no longer exists.".to_string(),
        ));
        return;
    };
    mod_record.always_enabled = !mod_record.always_enabled;

    match save_mod_user_state(&next_state) {
        Ok(status) => {
            mod_status.set(Some(status));
            app_state.set(next_state);
        }
        Err(error) => mod_status.set(Some(format!(
            "Could not save mod lock state: {}",
            error.message
        ))),
    }
}

fn add_mod_category_by_key(
    app_state: &mut Signal<AppState>,
    mod_status: &mut Signal<Option<String>>,
    saved_categories: &mut Signal<Vec<String>>,
    mod_key: &str,
    category: &str,
    color_key: &str,
) {
    let mut next_state = app_state.read().clone();
    let Some(mod_record) = next_state
        .mods
        .iter_mut()
        .find(|mod_record| mod_record.identity.stable_key() == mod_key)
    else {
        mod_status.set(Some(
            "Could not add category: mod no longer exists.".to_string(),
        ));
        return;
    };

    if let Err(error) = add_mod_category(mod_record, category) {
        mod_status.set(Some(format!("Could not add category: {}", error.message)));
        return;
    }

    match save_mod_user_state_with_category(&next_state, category, color_key) {
        Ok(status) => {
            saved_categories.set(load_category_names());
            mod_status.set(Some(status));
            app_state.set(next_state);
        }
        Err(error) => mod_status.set(Some(format!(
            "Could not save mod category: {}",
            error.message
        ))),
    }
}

fn remove_mod_category_by_key(
    app_state: &mut Signal<AppState>,
    mod_status: &mut Signal<Option<String>>,
    saved_categories: &mut Signal<Vec<String>>,
    mod_key: &str,
    category: &str,
) {
    let mut next_state = app_state.read().clone();
    let Some(mod_record) = next_state
        .mods
        .iter_mut()
        .find(|mod_record| mod_record.identity.stable_key() == mod_key)
    else {
        mod_status.set(Some(
            "Could not remove category: mod no longer exists.".to_string(),
        ));
        return;
    };

    match remove_mod_category(mod_record, category) {
        Ok(true) => {}
        Ok(false) => {
            mod_status.set(Some(format!(
                "Category \"{}\" was not assigned to this mod.",
                category.trim()
            )));
            return;
        }
        Err(error) => {
            mod_status.set(Some(format!(
                "Could not remove category: {}",
                error.message
            )));
            return;
        }
    }

    match save_mod_user_state(&next_state) {
        Ok(status) => {
            saved_categories.set(load_category_names());
            mod_status.set(Some(status));
            app_state.set(next_state);
        }
        Err(error) => mod_status.set(Some(format!(
            "Could not save mod category: {}",
            error.message
        ))),
    }
}

fn save_mod_state(state: &AppState) -> wh3mm_core::CoreResult<String> {
    let config = capture_mod_list_config(&state.mods);
    let path = mod_state_config_path();
    let did_write = write_mod_list_config_atomic(&path, &config)?;
    if did_write {
        Ok(format!("Saved mod state to {}.", path.display()))
    } else {
        Ok(format!(
            "Skipped empty mod-state save for {}.",
            path.display()
        ))
    }
}

fn import_legacy_ts_config_into_app(
    state: &AppState,
    config_path: &Path,
) -> wh3mm_core::CoreResult<LegacyTsConfigImportResult> {
    let imported = read_legacy_ts_config(config_path, LEGACY_TS_GAME_KEY)?;
    let did_write_mod_state =
        write_mod_list_config_atomic(mod_state_config_path(), &imported.mod_list)?;
    let did_write_presets = write_preset_config_atomic(preset_config_path(), &imported.presets)?;
    let did_write_mod_user =
        write_mod_user_config_atomic(mod_user_config_path(), &imported.mod_user)?;
    let game_folder = if let Some(game_folder) = &imported.game_folder {
        write_game_folder_config_atomic(game_folder_config_path(), game_folder)?;
        Some(PathBuf::from(&game_folder.game_dir))
    } else {
        None
    };
    let mods = apply_mod_user_config(
        apply_mod_list_config(state.mods.clone(), &imported.mod_list),
        &imported.mod_user,
    );
    let launch_options = launch_options_from_legacy_ts(&imported.launch_options);
    let skipped_writes = [
        (!did_write_mod_state).then_some("mod state"),
        (!did_write_presets).then_some("presets"),
        (!did_write_mod_user).then_some("mod user config"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let skipped_summary = if skipped_writes.is_empty() {
        String::new()
    } else {
        format!(
            " Skipped empty overwrite for {}.",
            skipped_writes.join(", ")
        )
    };
    let launch_option_summary = legacy_ts_launch_option_import_summary(&imported.launch_options);

    Ok(LegacyTsConfigImportResult {
        mods,
        game_folder,
        launch_options,
        status: format!(
            "Imported TS config from {}: {} current mods, {} presets, {} categories.{}{}",
            config_path.display(),
            imported.mod_list.mods.len(),
            imported.presets.presets.len(),
            imported.mod_user.categories.len(),
            skipped_summary,
            launch_option_summary
        ),
    })
}

fn legacy_ts_launch_option_import_summary(launch_options: &LegacyTsLaunchOptions) -> String {
    let mut summaries = Vec::new();
    if launch_options.make_units_generals {
        summaries.push("MakeUnitsGenerals was imported as an enabled launch option; launch generation will use enabled battle-permission DB rows when available.".to_string());
    }
    if launch_options.enabled_merged_mod_count > 0 {
        summaries.push(format!(
            "{} enabled merged mod{} imported; launch generation will skip source packs represented by enabled merged packs.",
            launch_options.enabled_merged_mod_count,
            plural_suffix(launch_options.enabled_merged_mod_count)
        ));
    }
    if launch_options.pack_data_overwrite_mod_count > 0 {
        summaries.push(format!(
            "{} pack overwrite group{} imported; launch generation will write replacement packs under whmm_overwrites.",
            launch_options.pack_data_overwrite_mod_count,
            plural_suffix(launch_options.pack_data_overwrite_mod_count)
        ));
    }

    if let Some(summary) = legacy_ts_unported_launch_summary(launch_options) {
        summaries.push(summary);
    }

    if summaries.is_empty() {
        String::new()
    } else {
        format!(" {}", summaries.join(" "))
    }
}

fn legacy_ts_unported_launch_summary(launch_options: &LegacyTsLaunchOptions) -> Option<String> {
    let mut items = Vec::new();
    if launch_options.user_flow_option_mod_count > 0 {
        items.push(format!(
            "{} user-flow option{}",
            launch_options.user_flow_option_mod_count,
            plural_suffix(launch_options.user_flow_option_mod_count)
        ));
    }
    (!items.is_empty()).then(|| {
        format!(
            "Unported TS launch settings detected ({}); Rust launch will not reproduce those yet.",
            items.join(", ")
        )
    })
}

fn plural_suffix(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn export_legacy_ts_config_from_app(
    state: &AppState,
    launch_options: &LaunchOptionState,
    config_path: &Path,
) -> wh3mm_core::CoreResult<String> {
    let snapshot = capture_legacy_ts_config_snapshot(state, launch_options);
    write_legacy_ts_config_atomic(config_path, &snapshot, LEGACY_TS_GAME_KEY)?;
    Ok(format!(
        "Exported TS config to {} with {} current mods, {} presets, and {} categories.",
        config_path.display(),
        snapshot.mod_list.mods.len(),
        snapshot.presets.presets.len(),
        snapshot.mod_user.categories.len()
    ))
}

fn capture_legacy_ts_config_snapshot(
    state: &AppState,
    launch_options: &LaunchOptionState,
) -> LegacyTsConfigSnapshot {
    LegacyTsConfigSnapshot {
        mod_list: capture_mod_list_config(&state.mods),
        presets: read_preset_config(preset_config_read_path()).unwrap_or(PresetConfig {
            version: 1,
            active_preset: None,
            presets: Vec::new(),
        }),
        mod_user: capture_current_mod_user_config(state),
        game_folder: read_game_folder_config(game_folder_config_read_path()).ok(),
        launch_options: legacy_ts_launch_options_from_state(launch_options),
    }
}

fn legacy_ts_launch_options_from_state(
    launch_options: &LaunchOptionState,
) -> LegacyTsLaunchOptions {
    LegacyTsLaunchOptions {
        skip_intro_movies: launch_options.skip_intro_movies,
        script_logging: launch_options.script_logging,
        auto_start_custom_battle: launch_options.auto_start_custom_battle,
        make_units_generals: launch_options.make_units_generals,
        close_on_play: launch_options.close_on_play,
        changing_game_process_priority: launch_options.high_process_priority,
        pack_data_overwrite_mod_count: launch_options.pack_data_overwrites.len(),
        pack_data_overwrites: launch_options.pack_data_overwrites.clone(),
        user_flow_option_mod_count: launch_options.user_flow_options.len(),
        user_flow_options: launch_options.user_flow_options.clone(),
        ..LegacyTsLaunchOptions::default()
    }
}

fn launch_options_from_legacy_ts(launch_options: &LegacyTsLaunchOptions) -> LaunchOptionState {
    LaunchOptionState {
        skip_intro_movies: launch_options.skip_intro_movies,
        script_logging: launch_options.script_logging,
        auto_start_custom_battle: launch_options.auto_start_custom_battle,
        make_units_generals: launch_options.make_units_generals,
        close_on_play: launch_options.close_on_play,
        high_process_priority: launch_options.changing_game_process_priority,
        pack_data_overwrites: launch_options.pack_data_overwrites.clone(),
        user_flow_options: launch_options.user_flow_options.clone(),
    }
}

fn save_mod_user_state(state: &AppState) -> wh3mm_core::CoreResult<String> {
    let path = mod_user_config_path();
    let config = capture_current_mod_user_config(state);
    let did_write = write_mod_user_config_atomic(&path, &config)?;
    if did_write {
        Ok(format!("Saved mod user config to {}.", path.display()))
    } else {
        Ok(format!(
            "Skipped empty mod-user config save for {}.",
            path.display()
        ))
    }
}

fn save_mod_user_state_with_category(
    state: &AppState,
    category: &str,
    color_key: &str,
) -> wh3mm_core::CoreResult<String> {
    let path = mod_user_config_path();
    let mut config = capture_current_mod_user_config(state);
    set_category_color_config(&mut config, category, color_key)?;
    let did_write = write_mod_user_config_atomic(&path, &config)?;
    if did_write {
        Ok(format!("Saved mod user config to {}.", path.display()))
    } else {
        Ok(format!(
            "Skipped empty mod-user config save for {}.",
            path.display()
        ))
    }
}

fn rename_category_definition(
    old_category: &str,
    new_category: &str,
    state: &AppState,
) -> wh3mm_core::CoreResult<(Vec<ModRecord>, String)> {
    let path = mod_user_config_path();
    let (config, mods) = rename_category_config_for_state(
        capture_current_mod_user_config(state),
        old_category,
        new_category,
        state,
    )?;
    let did_write = write_mod_user_config_atomic(&path, &config)?;
    let status = if did_write {
        format!(
            "Renamed category \"{}\" to \"{}\" in {}.",
            old_category.trim(),
            new_category.trim(),
            path.display()
        )
    } else {
        format!("Skipped empty category rename for {}.", path.display())
    };
    Ok((mods, status))
}

fn delete_category_definition(
    category: &str,
    state: &AppState,
) -> wh3mm_core::CoreResult<(Vec<ModRecord>, String)> {
    let path = mod_user_config_path();
    let (config, mods) =
        delete_category_config_for_state(capture_current_mod_user_config(state), category, state)?;
    let did_write = write_mod_user_config_atomic(&path, &config)?;
    let status = if did_write {
        format!(
            "Deleted category \"{}\" from {}.",
            category.trim(),
            path.display()
        )
    } else {
        format!("Skipped empty category delete for {}.", path.display())
    };
    Ok((mods, status))
}

fn rename_category_config_for_state(
    mut config: ModUserConfig,
    old_category: &str,
    new_category: &str,
    state: &AppState,
) -> wh3mm_core::CoreResult<(ModUserConfig, Vec<ModRecord>)> {
    rename_category_config(&mut config, old_category, new_category)?;
    let mods = apply_mod_user_config(state.mods.clone(), &config);
    Ok((config, mods))
}

fn delete_category_config_for_state(
    mut config: ModUserConfig,
    category: &str,
    state: &AppState,
) -> wh3mm_core::CoreResult<(ModUserConfig, Vec<ModRecord>)> {
    delete_category_config(&mut config, category)?;
    let mods = apply_mod_user_config(state.mods.clone(), &config);
    Ok((config, mods))
}

fn capture_current_mod_user_config(state: &AppState) -> ModUserConfig {
    let existing = read_mod_user_config(mod_user_config_read_path()).ok();
    let categories = existing
        .as_ref()
        .map(|config| config.categories.clone())
        .unwrap_or_default();
    let category_colors = existing
        .as_ref()
        .map(|config| config.category_colors.clone())
        .unwrap_or_default();
    capture_mod_user_config(&state.mods, &categories, &category_colors)
}

fn save_category_definition(category: &str, color_key: &str) -> wh3mm_core::CoreResult<String> {
    let path = mod_user_config_path();
    let mut config = read_mod_user_config(mod_user_config_read_path()).unwrap_or(ModUserConfig {
        version: 1,
        categories: Vec::new(),
        category_colors: Default::default(),
        mods: Vec::new(),
    });
    set_category_color_config(&mut config, category, color_key)?;
    let did_write = write_mod_user_config_atomic(&path, &config)?;
    if did_write {
        Ok(format!(
            "Saved category \"{}\" to {}.",
            category.trim(),
            path.display()
        ))
    } else {
        Ok(format!(
            "Skipped empty category save for {}.",
            path.display()
        ))
    }
}

fn save_named_preset(name: &str, state: &AppState) -> wh3mm_core::CoreResult<String> {
    let path = preset_config_path();
    save_named_preset_to_path(&path, &preset_config_read_path(), name, state)
}

fn save_named_preset_to_path(
    path: &Path,
    read_path: &Path,
    name: &str,
    state: &AppState,
) -> wh3mm_core::CoreResult<String> {
    let mut config = read_preset_config(read_path).unwrap_or(PresetConfig {
        version: 1,
        active_preset: None,
        presets: Vec::new(),
    });
    upsert_preset_config(&mut config, name, &state.mods)?;
    let did_write = write_preset_config_atomic(path, &config)?;
    if did_write {
        Ok(format!("Saved preset \"{}\" to {}.", name, path.display()))
    } else {
        Ok(format!("Skipped empty preset save for {}.", path.display()))
    }
}

fn save_on_last_game_launch_preset(state: &AppState) -> wh3mm_core::CoreResult<String> {
    let path = preset_config_path();
    save_on_last_game_launch_preset_to_path(&path, &preset_config_read_path(), state)
}

fn save_on_last_game_launch_preset_to_path(
    path: &Path,
    read_path: &Path,
    state: &AppState,
) -> wh3mm_core::CoreResult<String> {
    let mut config = read_preset_config(read_path).unwrap_or(PresetConfig {
        version: 1,
        active_preset: None,
        presets: Vec::new(),
    });
    let active_preset = config.active_preset.clone();
    upsert_preset_config(&mut config, ON_LAST_GAME_LAUNCH_PRESET_NAME, &state.mods)?;
    config.active_preset = active_preset;
    let did_write = write_preset_config_atomic(path, &config)?;
    if did_write {
        Ok(format!(
            "Saved \"{ON_LAST_GAME_LAUNCH_PRESET_NAME}\" preset to {}.",
            path.display()
        ))
    } else {
        Ok(format!(
            "Skipped empty \"{ON_LAST_GAME_LAUNCH_PRESET_NAME}\" preset save for {}.",
            path.display()
        ))
    }
}

fn load_named_preset(
    name: &str,
    state: &AppState,
) -> wh3mm_core::CoreResult<(Vec<ModRecord>, String)> {
    let path = preset_config_read_path();
    let config = read_preset_config(&path)?;
    let mods = apply_preset_config(state.mods.clone(), &config, name)?;
    Ok((
        mods,
        format!("Loaded preset \"{}\" from {}.", name, path.display()),
    ))
}

fn delete_named_preset(name: &str) -> wh3mm_core::CoreResult<String> {
    let path = preset_config_path();
    let mut config = read_preset_config(preset_config_read_path())?;
    delete_preset_config(&mut config, name)?;
    let did_write = write_preset_config_atomic(&path, &config)?;
    if did_write {
        Ok(format!(
            "Deleted preset \"{}\" from {}.",
            name,
            path.display()
        ))
    } else {
        Ok(format!(
            "Skipped empty preset delete for {}.",
            path.display()
        ))
    }
}

fn load_preset_names() -> Vec<String> {
    read_preset_config(preset_config_read_path())
        .map(|config| preset_names(&config))
        .unwrap_or_default()
}

fn load_category_names() -> Vec<String> {
    read_mod_user_config(mod_user_config_read_path())
        .map(|config| config.categories)
        .unwrap_or_default()
}

fn mod_state_config_path() -> PathBuf {
    config_file_write_path(MOD_STATE_CONFIG_FILE)
}

fn mod_state_config_read_path() -> PathBuf {
    config_file_read_path(MOD_STATE_CONFIG_FILE)
}

fn mod_user_config_path() -> PathBuf {
    config_file_write_path(MOD_USER_CONFIG_FILE)
}

fn mod_user_config_read_path() -> PathBuf {
    config_file_read_path(MOD_USER_CONFIG_FILE)
}

fn preset_config_path() -> PathBuf {
    config_file_write_path(PRESET_CONFIG_FILE)
}

fn preset_config_read_path() -> PathBuf {
    config_file_read_path(PRESET_CONFIG_FILE)
}

fn config_file_write_path(file_name: &str) -> PathBuf {
    app_config_dir().join(file_name)
}

fn config_file_read_path(file_name: &str) -> PathBuf {
    resolve_config_file_read_path(
        &config_file_write_path(file_name),
        &legacy_config_file_path(file_name),
    )
}

fn resolve_config_file_read_path(preferred_path: &Path, legacy_path: &Path) -> PathBuf {
    if preferred_path.exists() || !legacy_path.exists() {
        preferred_path.to_path_buf()
    } else {
        legacy_path.to_path_buf()
    }
}

fn legacy_config_file_path(file_name: &str) -> PathBuf {
    env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(file_name)
}

fn app_config_dir() -> PathBuf {
    if let Ok(path) = env::var(APP_CONFIG_DIR_ENV)
        && !path.trim().is_empty()
    {
        return PathBuf::from(path);
    }

    platform_app_config_dir().unwrap_or_else(|| {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(APP_CONFIG_DIR_NAME)
    })
}

fn platform_app_config_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        env::var_os("APPDATA")
            .or_else(|| env::var_os("LOCALAPPDATA"))
            .map(PathBuf::from)
            .map(|path| path.join(APP_CONFIG_DIR_NAME))
    } else if cfg!(target_os = "macos") {
        env::var_os("HOME").map(PathBuf::from).map(|path| {
            path.join("Library/Application Support")
                .join(APP_CONFIG_DIR_NAME)
        })
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|path| PathBuf::from(path).join(".config")))
            .map(|path| path.join("wh3mm-rust"))
    }
}

fn load_pack_from_path(pack_path: PathBuf) -> (Option<PackViewModel>, Option<String>) {
    match read_pack_contents_lossy(&pack_path, &PackReadOptions::default()) {
        Ok(contents) => {
            let mut pack = build_pack_contents_view_model(&contents);
            let preview_status = attach_first_table_preview(&pack_path, &contents, &mut pack);
            let flow_status = attach_flow_summary(&pack_path, &mut pack);
            (
                Some(pack),
                Some(format!(
                    "Loaded pack summary from {}. {preview_status} {flow_status}",
                    pack_path.display()
                )),
            )
        }
        Err(error) => (
            None,
            Some(format!(
                "Could not load {}: {}.",
                pack_path.display(),
                error.message,
            )),
        ),
    }
}

fn attach_flow_summary(pack_path: &Path, pack: &mut PackViewModel) -> String {
    match read_whmm_flow_pack_summary(pack_path) {
        Ok(summary) => {
            let file_count = summary.files.len();
            let error_count = summary.read_errors.len();
            pack.flow_summary = build_pack_flow_summary_view_model(&summary);
            match (file_count, error_count) {
                (0, 0) => "No WH3MM user-flow files found.".to_string(),
                (_, 0) => format!(
                    "Detected {file_count} WH3MM user-flow file{}.",
                    plural_suffix(file_count)
                ),
                _ => format!(
                    "Detected {file_count} WH3MM user-flow file{} with {error_count} read error{}.",
                    plural_suffix(file_count),
                    plural_suffix(error_count)
                ),
            }
        }
        Err(error) => format!("Flow summary unavailable: {}.", error.message),
    }
}

fn attach_first_table_preview(
    pack_path: &Path,
    contents: &PackContents,
    pack: &mut PackViewModel,
) -> String {
    let schema_path = schema_path();
    let schema = match wh3mm_core::load_schema_file(&schema_path) {
        Ok(schema) => schema,
        Err(error) => {
            return format!(
                "Schema unavailable at {}: {}",
                schema_path.display(),
                error.message
            );
        }
    };

    for (entry, metadata) in contents.index.files.iter().zip(contents.metadata.iter()) {
        let PackFileMetadata::DbTable(metadata) = metadata else {
            continue;
        };
        let Some(selected_schema) = resolve_table_schema(&schema, metadata) else {
            continue;
        };

        let rows = match read_db_rows_from_pack(pack_path, entry, &selected_schema.fields) {
            Ok(rows) => rows,
            Err(error) => return format!("Could not preview {}: {}", metadata.name, error.message),
        };

        pack.table_preview = Some(build_db_table_preview_view_model(
            metadata,
            selected_schema,
            &rows,
            MAX_TABLE_PREVIEW_ROWS,
        ));
        return format!("Previewing {}", metadata.name);
    }

    "No schema-resolvable DB table preview found.".to_string()
}

fn schema_path() -> PathBuf {
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    [
        cwd.join("schema/schema_wh3.json.zst"),
        cwd.join("../../schema/schema_wh3.json.zst"),
        PathBuf::from("schema/schema_wh3.json.zst"),
    ]
    .into_iter()
    .find(|path| path.exists())
    .unwrap_or_else(|| PathBuf::from("schema/schema_wh3.json.zst"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    use wh3mm_core::{
        AppState, DbCell, DbFieldSchema, DbFieldType, DbPrimitiveValue, DbRows, DbSchema,
        DbVersionSchema, GameId, LegacyTsLaunchOptions, ModIdentity, ModListConfig, ModRecord,
        PackDataOverwrite, PackDataOverwriteOperation, PackDataOverwriteValue, PackFileWrite,
        PersistedModState, PersistedPreset, PreLaunchPackWrite, PresetConfig,
        SteamWorkshopAdapterError, SteamWorkshopMetadataAdapter,
        WH3_MAKE_UNITS_GENERALS_TABLE_PATH, WH3_START_GAME_PACK_NAME, Wh3StartGamePackOptions,
        WorkshopModData, build_pfh5_pack_bytes, build_wh3_start_game_pack_with_battle_permissions,
        capture_mod_user_config, plan_windows_launch, read_preset_config, write_db_rows_to_payload,
        write_mod_list_config_atomic, write_preset_config_atomic,
    };
    use wh3mm_runtime::{
        SteamResubscribeResult, SteamWorkshopCheckStateResult, SteamWorkshopCommandAdapter,
        SteamWorkshopCommandResult, SteamWorkshopCommandRunner, WH3_STEAM_APP_ID,
        WindowsProcessPriorityClass, WindowsProcessPriorityUpdate,
    };

    use super::{
        APP_DIAGNOSTIC_LOG_FILE, AlphaReadinessReport, AlphaReadinessRow, AlphaReadinessStatus,
        DIAGNOSTICS_DIR_NAME, DiagnosticSnapshotInput, LaunchOptionState, LibraryNavTarget,
        LibraryToolTab, ModListFilter, ON_LAST_GAME_LAUNCH_PRESET_NAME,
        STEAM_HELPER_COMMAND_LOG_ENV, STEAM_HELPER_COMMAND_LOG_FILE, SteamCommandAction,
        SteamCommandPanelRow, SteamCommandPanelState, SteamRefreshResult, WorkspacePage,
        app_brand_subtitle, app_brand_title, append_app_diagnostic_log_event_to_path,
        append_pack_data_overwrite_packs, apply_saved_or_existing_game_mod_list,
        apply_steam_metadata_to_mods, archive_filter_bar_style, archive_filter_button_style,
        archive_table_header_style, archive_toolbar_button_style,
        build_alpha_readiness_report_with_paths, build_windows_launch_options,
        collection_row_button_style, collection_row_style, continue_save_button_style,
        delete_category_config_for_state, dependency_names_summary, detail_action_button_style,
        detail_metric_style, detail_source_tile_style, diagnostic_snapshot_text,
        enabled_pack_paths_for_start_game, fetch_steam_metadata_safely,
        first_existing_steam_helper_path, generated_pack_details, header_metric_style,
        initial_app_state, launch_options_from_legacy_ts, launch_priority_status,
        launch_quick_button_style, launch_state_fingerprint, launch_status_with_close_on_play,
        legacy_ts_launch_option_import_summary, legacy_ts_launch_options_from_state,
        library_nav_active, library_tool_tab_label, library_utility_button_style, mod_author_label,
        mod_categories_label, mod_enable_button_style, mod_list_filter_label,
        mod_row_matches_filter, mod_row_matches_query, mod_row_style, mod_source_label,
        mod_state_label, mod_updated_label, mod_workshop_id_from_row, nav_badge_style,
        nav_button_style, normalize_steam_helper_backend, read_existing_launch_mod_list_pack_names,
        relative_time_label, rename_category_config_for_state, resolve_config_file_read_path,
        run_steam_command_action, save_on_last_game_launch_preset_to_path,
        selected_or_first_mod_row, selected_pack_from_optional_arg, settings_card_style,
        settings_danger_button_style, settings_input_style, settings_primary_button_style,
        source_tile_style, start_game_source_pack_paths, start_game_temp_packs_dir_for_config_dir,
        steam_check_update_panel_state, steam_check_update_status, steam_command_panel_state,
        steam_command_status, steam_helper_process_config, steam_probe_status,
        steam_refresh_panel_state, steam_resubscribe_panel_state, steam_resubscribe_status,
        toggle_label_style, tool_action_button_style, top_icon_button_style, workshop_id_summary,
        workshop_ids_from_input, workshop_ids_from_mods,
    };
    use wh3mm_ui::ModRowViewModel;

    #[test]
    fn mod_row_search_matches_name_path_category_and_tag() {
        let row = ModRowViewModel {
            key: "mod-a".to_string(),
            display_name: "Community Balance".to_string(),
            subtitle: "workshop/content/1142710/123456789/mod.pack".to_string(),
            enabled: true,
            locked: false,
            hidden: false,
            categories: vec!["Gameplay".to_string()],
            tags: vec!["workshop".to_string()],
        };

        assert!(mod_row_matches_query(&row, "balance"));
        assert!(mod_row_matches_query(&row, "123456789"));
        assert!(mod_row_matches_query(&row, "gameplay"));
        assert!(mod_row_matches_query(&row, "workshop"));
        assert!(!mod_row_matches_query(&row, "graphics"));
    }

    #[test]
    fn mod_list_filter_matches_expected_row_states() {
        let enabled = ModRowViewModel {
            key: "enabled".to_string(),
            display_name: "Enabled".to_string(),
            subtitle: "enabled.pack".to_string(),
            enabled: true,
            locked: false,
            hidden: false,
            categories: Vec::new(),
            tags: Vec::new(),
        };
        let disabled = ModRowViewModel {
            key: "disabled".to_string(),
            display_name: "Disabled".to_string(),
            subtitle: "disabled.pack".to_string(),
            enabled: false,
            locked: false,
            hidden: false,
            categories: Vec::new(),
            tags: Vec::new(),
        };
        let locked = ModRowViewModel {
            key: "locked".to_string(),
            display_name: "Locked".to_string(),
            subtitle: "locked.pack".to_string(),
            enabled: true,
            locked: true,
            hidden: false,
            categories: Vec::new(),
            tags: Vec::new(),
        };
        let hidden = ModRowViewModel {
            key: "hidden".to_string(),
            display_name: "Hidden".to_string(),
            subtitle: "hidden.pack".to_string(),
            enabled: false,
            locked: false,
            hidden: true,
            categories: Vec::new(),
            tags: Vec::new(),
        };

        assert!(mod_row_matches_filter(&enabled, ModListFilter::All));
        assert!(mod_row_matches_filter(&enabled, ModListFilter::Enabled));
        assert!(mod_row_matches_filter(&disabled, ModListFilter::Disabled));
        assert!(mod_row_matches_filter(&locked, ModListFilter::Locked));
        assert!(mod_row_matches_filter(&hidden, ModListFilter::Hidden));
        assert!(!mod_row_matches_filter(&locked, ModListFilter::Disabled));
        assert_eq!(mod_list_filter_label(ModListFilter::Hidden), "Hidden");
    }

    #[test]
    fn selected_mod_detail_helpers_prefer_selected_then_first_row() {
        let first = ModRowViewModel {
            key: "first".to_string(),
            display_name: "First".to_string(),
            subtitle: "first.pack".to_string(),
            enabled: false,
            locked: false,
            hidden: false,
            categories: Vec::new(),
            tags: Vec::new(),
        };
        let second = ModRowViewModel {
            key: "second".to_string(),
            display_name: "Second".to_string(),
            subtitle: "second.pack".to_string(),
            enabled: true,
            locked: false,
            hidden: false,
            categories: vec!["Gameplay".to_string(), "Balance".to_string()],
            tags: Vec::new(),
        };
        let rows = vec![first.clone(), second.clone()];

        assert_eq!(
            selected_or_first_mod_row(&rows, Some("second")),
            Some(second.clone())
        );
        assert_eq!(
            selected_or_first_mod_row(&rows, Some("missing")),
            Some(first)
        );
        assert_eq!(mod_state_label(&second), "enabled");
        assert_eq!(mod_categories_label(&second), "Gameplay, Balance");
    }

    #[test]
    fn tool_actions_are_styled_for_screen_navigation() {
        assert_eq!(library_tool_tab_label(LibraryToolTab::None), "None");
        assert_eq!(library_tool_tab_label(LibraryToolTab::Presets), "Presets");
        assert_eq!(library_tool_tab_label(LibraryToolTab::Categories), "Cats");
        assert_eq!(library_tool_tab_label(LibraryToolTab::Config), "Config");
        assert!(tool_action_button_style(true).contains("background: #3a3b48"));
        assert!(tool_action_button_style(false).contains("justify-content: space-between"));
        assert!(launch_quick_button_style().contains("min-height: 42px"));
        assert!(continue_save_button_style(false).contains("min-height: 50px"));
        assert!(continue_save_button_style(true).contains("background: #292a35"));
        assert!(header_metric_style().contains("min-height: 30px"));
        assert!(top_icon_button_style(true).contains("min-width: 68px"));
        assert!(top_icon_button_style(false).contains("flex: 0 0 auto"));
        assert!(archive_toolbar_button_style(true).contains("#1f6feb"));
        assert!(archive_toolbar_button_style(false).contains("min-height: 30px"));
    }

    #[test]
    fn library_primary_nav_has_single_clear_active_destination() {
        assert!(library_nav_active(
            LibraryNavTarget::AllMods,
            WorkspacePage::Mods,
            ModListFilter::All,
            LibraryToolTab::None,
        ));
        assert!(!library_nav_active(
            LibraryNavTarget::AllMods,
            WorkspacePage::Mods,
            ModListFilter::All,
            LibraryToolTab::Presets,
        ));
        assert!(library_nav_active(
            LibraryNavTarget::Collections,
            WorkspacePage::Collections,
            ModListFilter::All,
            LibraryToolTab::None,
        ));
        assert!(!library_nav_active(
            LibraryNavTarget::AllMods,
            WorkspacePage::Mods,
            ModListFilter::All,
            LibraryToolTab::Categories,
        ));
        assert!(library_nav_active(
            LibraryNavTarget::Categories,
            WorkspacePage::Categories,
            ModListFilter::All,
            LibraryToolTab::None,
        ));
        assert!(!library_nav_active(
            LibraryNavTarget::Collections,
            WorkspacePage::Categories,
            ModListFilter::All,
            LibraryToolTab::None,
        ));
        assert!(library_nav_active(
            LibraryNavTarget::Settings,
            WorkspacePage::Settings,
            ModListFilter::Enabled,
            LibraryToolTab::None,
        ));
        assert_eq!(app_brand_title(), "Mod Archive");
        assert_eq!(
            app_brand_subtitle("WH3 Mod Manager"),
            "WH3 Mod Manager / Windows alpha"
        );
        assert!(nav_badge_style().contains("width: 26px"));
        assert!(library_utility_button_style(true).contains("min-height: 38px"));
        assert!(library_utility_button_style(false).contains("background: transparent"));
    }

    #[test]
    fn archive_row_helpers_distinguish_source_and_status_styles() {
        let workshop = ModRowViewModel {
            key: "workshop".to_string(),
            display_name: "Workshop".to_string(),
            subtitle: "workshop/content/1142710/123/mod.pack".to_string(),
            enabled: true,
            locked: false,
            hidden: false,
            categories: Vec::new(),
            tags: vec!["workshop".to_string()],
        };
        let local = ModRowViewModel {
            key: "local".to_string(),
            display_name: "Local".to_string(),
            subtitle: "data/modding/local.pack".to_string(),
            enabled: false,
            locked: false,
            hidden: false,
            categories: Vec::new(),
            tags: Vec::new(),
        };
        let locked = ModRowViewModel {
            key: "core".to_string(),
            display_name: "Core".to_string(),
            subtitle: "data/core.pack".to_string(),
            enabled: true,
            locked: true,
            hidden: false,
            categories: Vec::new(),
            tags: Vec::new(),
        };

        assert_eq!(mod_source_label(&workshop), "WS");
        assert_eq!(mod_source_label(&local), "MOD");
        assert_eq!(mod_source_label(&locked), "CORE");
        assert!(source_tile_style(&workshop).contains("#2563eb"));
        assert!(source_tile_style(&workshop).contains("width: 42px"));
        assert!(mod_row_style(false).contains("minmax(0, 1.9fr)"));
        assert!(!mod_row_style(false).contains("154px"));
        assert!(archive_table_header_style().contains("minmax(0, 1.9fr)"));
        assert!(!archive_table_header_style().contains("Actions"));
        assert!(archive_filter_bar_style().contains("width: 100%"));
        assert!(archive_filter_button_style(true).contains("#60a5fa"));
        assert!(archive_filter_button_style(false).contains("min-width: 78px"));
        assert!(mod_row_style(false).contains("cursor: pointer"));
        assert!(mod_enable_button_style(true, false).contains("#65f58b"));
        assert!(mod_enable_button_style(false, false).contains("#353a43"));
        assert!(mod_enable_button_style(true, true).contains("#343946"));
        assert!(nav_button_style(true).contains("border-left: 2px solid #65f58b"));
    }

    #[test]
    fn archive_row_metadata_uses_real_workshop_author_and_update_age() {
        let workshop = ModRowViewModel {
            key: "workshop:111".to_string(),
            display_name: "Workshop".to_string(),
            subtitle: "workshop/content/1142710/111/mod.pack".to_string(),
            enabled: true,
            locked: false,
            hidden: false,
            categories: Vec::new(),
            tags: vec!["workshop".to_string()],
        };
        let path_only = ModRowViewModel {
            key: "path".to_string(),
            display_name: "Workshop Path".to_string(),
            subtitle: "C:\\Steam\\steamapps\\workshop\\content\\1142710\\222\\mod.pack".to_string(),
            enabled: true,
            locked: false,
            hidden: false,
            categories: Vec::new(),
            tags: vec!["workshop".to_string()],
        };
        let local = ModRowViewModel {
            key: "local".to_string(),
            display_name: "Local".to_string(),
            subtitle: "data/modding/local.pack".to_string(),
            enabled: false,
            locked: false,
            hidden: false,
            categories: Vec::new(),
            tags: Vec::new(),
        };
        let metadata = vec![WorkshopModData {
            workshop_id: "111".to_string(),
            title: "Workshop".to_string(),
            author: "Groove Wizard".to_string(),
            dependency_ids: Vec::new(),
            dependency_id_to_name: Vec::new(),
            last_changed_ms: 86_400_000,
        }];

        assert_eq!(mod_workshop_id_from_row(&workshop).as_deref(), Some("111"));
        assert_eq!(mod_workshop_id_from_row(&path_only).as_deref(), Some("222"));
        assert_eq!(mod_author_label(&workshop, &metadata), "Groove Wizard");
        assert_eq!(
            mod_updated_label(&workshop, &metadata, 3 * 86_400_000),
            "2 days ago"
        );
        assert_eq!(mod_author_label(&local, &metadata), "Local");
        assert_eq!(
            mod_updated_label(&local, &metadata, 3 * 86_400_000),
            "Local"
        );
        assert_eq!(relative_time_label(0, 3 * 86_400_000), "Unknown");
        assert_eq!(relative_time_label(3 * 86_400_000, 3 * 86_400_000), "Today");
    }

    #[test]
    fn settings_workspace_styles_match_card_and_toggle_language() {
        assert!(settings_card_style().contains("background: #1f202b"));
        assert!(settings_input_style().contains("box-sizing: border-box"));
        assert!(settings_primary_button_style().contains("#65f58b"));
        assert!(settings_danger_button_style().contains("#451a1a"));
        assert!(collection_row_style().contains("grid-template-columns"));
        assert!(collection_row_button_style(true).contains("#60a5fa"));
        assert!(collection_row_button_style(false).contains("#171b24"));
        assert!(toggle_label_style(true).contains("#65f58b"));
        assert!(toggle_label_style(false).contains("#353a43"));
        let workshop = ModRowViewModel {
            key: "workshop".to_string(),
            display_name: "Workshop".to_string(),
            subtitle: "workshop/content/1142710/123/mod.pack".to_string(),
            enabled: true,
            locked: false,
            hidden: false,
            categories: Vec::new(),
            tags: vec!["workshop".to_string()],
        };
        assert!(detail_source_tile_style(&workshop).contains("aspect-ratio: 1 / 1"));
        assert!(detail_metric_style().contains("border-top"));
        assert!(detail_action_button_style(false).contains("background: #343541"));
        assert!(detail_action_button_style(true).contains("background: #451a1a"));
    }

    #[test]
    fn first_run_does_not_show_demo_pack_by_default() {
        assert_eq!(selected_pack_from_optional_arg(None), (None, None));
    }

    #[test]
    fn first_run_starts_with_empty_mod_library() {
        let state = initial_app_state();

        assert!(state.mods.is_empty());
        assert_eq!(state.active_game, GameId::Warhammer3);
    }

    #[test]
    fn workshop_ids_from_mods_normalizes_and_dedupes() {
        let mods = vec![
            mod_record("a.pack", Some(" 111 "), "A"),
            mod_record("b.pack", Some("bad"), "B"),
            mod_record("c.pack", Some("111"), "C"),
            mod_record("d.pack", Some("222"), "D"),
            mod_record("e.pack", None, "E"),
        ];

        assert_eq!(workshop_ids_from_mods(&mods), ["111", "222"]);
    }

    #[test]
    fn workshop_ids_from_input_accepts_common_separators() {
        assert_eq!(
            workshop_ids_from_input("111, bad;222\n111 333\t"),
            ["111", "222", "333"]
        );
    }

    #[test]
    fn steam_probe_status_summarizes_missing_runtime_files() {
        assert_eq!(
            steam_probe_status(
                r#"{"appId":"1142710","selectedBackend":"fixture","fixtureConfigured":true,"fixtureAvailable":true,"commandLogConfigured":false,"nativeImplemented":false,"nativeAvailable":false,"nativeStatus":"native Steamworks backend is not selected","windowsRuntimeRedistributables":["steam_api64.dll"],"windowsRuntimeRedistributableStatuses":[{"fileName":"steam_api64.dll","expectedPath":"C:\\helper\\steam_api64.dll","present":false}],"windowsLinkLibraries":["steam_api64.lib"]}"#
            ),
            r#"Steam helper probe: backend fixture for app 1142710. Fixture backend ready. Native: native Steamworks unavailable in this build (native Steamworks backend is not selected). Missing Windows runtime redistributables: steam_api64.dll at C:\helper\steam_api64.dll."#
        );
    }

    #[test]
    fn steam_probe_status_reports_malformed_json() {
        let status = steam_probe_status("{bad");

        assert!(status.starts_with("Steam helper probe returned malformed JSON:"));
        assert!(status.ends_with("Raw: {bad"));
    }

    #[test]
    fn first_existing_steam_helper_path_selects_existing_candidate() {
        let root =
            std::env::temp_dir().join(format!("wh3mm-dioxus-helper-path-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let missing = root.join("missing-helper");
        let helper = root.join("wh3mm-steam-helper");
        fs::write(&helper, b"helper").unwrap();

        assert_eq!(
            first_existing_steam_helper_path(&[missing, helper.clone()]),
            Some(helper)
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn alpha_readiness_report_accepts_packaged_windows_shape() {
        let root = std::env::temp_dir().join(format!("wh3mm-dioxus-ready-{}", std::process::id()));
        let payload = root.join("payload");
        let helper_dir = payload.join("helpers");
        let helper = helper_dir.join("wh3mm-steam-helper.exe");
        let schema = payload.join("schema/schema_wh3.json.zst");
        let config_dir = root.join("config");
        let game_dir = root
            .join("SteamLibrary")
            .join("steamapps")
            .join("common")
            .join("Total War WARHAMMER III");
        let workshop_dir = root
            .join("SteamLibrary")
            .join("steamapps")
            .join("workshop")
            .join("content")
            .join(WH3_STEAM_APP_ID);

        fs::create_dir_all(&helper_dir).unwrap();
        fs::create_dir_all(schema.parent().unwrap()).unwrap();
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(game_dir.join("data")).unwrap();
        fs::create_dir_all(&workshop_dir).unwrap();
        fs::write(&helper, b"helper").unwrap();
        fs::write(helper_dir.join("steam_api64.dll"), b"dll").unwrap();
        fs::write(&schema, b"schema").unwrap();
        fs::write(game_dir.join("Warhammer3.exe"), b"exe").unwrap();

        let report = build_alpha_readiness_report_with_paths(
            Some(&game_dir),
            helper.to_str().unwrap(),
            &schema,
            &config_dir,
        );

        assert_eq!(report.summary, "6 ready / 0 checks / 0 errors");
        assert!(
            report
                .rows
                .iter()
                .all(|row| row.status == super::AlphaReadinessStatus::Ready)
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn alpha_readiness_report_surfaces_missing_alpha_files() {
        let root =
            std::env::temp_dir().join(format!("wh3mm-dioxus-not-ready-{}", std::process::id()));
        let missing_helper = root.join("helpers/wh3mm-steam-helper.exe");
        let missing_schema = root.join("schema/schema_wh3.json.zst");
        let config_dir = root.join("config");

        let report = build_alpha_readiness_report_with_paths(
            None,
            missing_helper.to_str().unwrap(),
            &missing_schema,
            &config_dir,
        );

        assert_eq!(report.summary, "0 ready / 4 checks / 2 errors");
        assert_eq!(report.rows[1].label, "Schema");
        assert_eq!(report.rows[1].status, super::AlphaReadinessStatus::Error);
        assert_eq!(report.rows[2].label, "Steam helper");
        assert_eq!(report.rows[2].status, super::AlphaReadinessStatus::Error);
        assert_eq!(report.rows[4].label, "WH3 folder");
        assert_eq!(report.rows[4].status, super::AlphaReadinessStatus::Warning);
    }

    #[test]
    fn app_diagnostic_log_appends_one_line_events() {
        let root = std::env::temp_dir().join(format!(
            "wh3mm-dioxus-diagnostic-log-{}",
            std::process::id()
        ));
        let log_path = root
            .join(DIAGNOSTICS_DIR_NAME)
            .join(APP_DIAGNOSTIC_LOG_FILE);

        append_app_diagnostic_log_event_to_path(&log_path, "first\nsecond").unwrap();

        let log = fs::read_to_string(&log_path).unwrap();
        assert_eq!(log.lines().count(), 1);
        assert!(log.contains("first\\nsecond"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn diagnostic_snapshot_text_captures_alpha_context() {
        let state = AppState::with_mods(
            GameId::Warhammer3,
            vec![
                ModRecord {
                    identity: ModIdentity::new("data/core.pack", None::<String>, "core.pack"),
                    display_name: "Core".to_string(),
                    enabled: true,
                    always_enabled: true,
                    hidden: false,
                    categories: Vec::new(),
                    tags: Vec::new(),
                },
                ModRecord {
                    identity: ModIdentity::new(
                        "workshop/content/1142710/111/mod.pack",
                        Some("111"),
                        "mod.pack",
                    ),
                    display_name: "Workshop Mod".to_string(),
                    enabled: true,
                    always_enabled: false,
                    hidden: true,
                    categories: vec!["Gameplay".to_string()],
                    tags: vec!["workshop".to_string()],
                },
            ],
        );
        let readiness = AlphaReadinessReport {
            summary: "1 ready / 0 checks / 0 errors".to_string(),
            rows: vec![AlphaReadinessRow {
                label: "Config".to_string(),
                status: AlphaReadinessStatus::Ready,
                detail: "Using test config".to_string(),
            }],
        };
        let command = SteamCommandPanelState {
            title: "Steam check".to_string(),
            summary: "1 requested".to_string(),
            rows: vec![SteamCommandPanelRow {
                label: "Requested IDs".to_string(),
                value: "111".to_string(),
            }],
        };

        let game_folder = PathBuf::from("C:/Steam/steamapps/common/Total War WARHAMMER III");
        let launch_options = LaunchOptionState {
            script_logging: true,
            close_on_play: true,
            ..LaunchOptionState::default()
        };
        let snapshot = diagnostic_snapshot_text(&DiagnosticSnapshotInput {
            app_state: &state,
            game_folder: Some(&game_folder),
            helper_path: "C:/WH3MM/helpers/wh3mm-steam-helper.exe",
            helper_backend: "native",
            launch_options: &launch_options,
            launch_save_name: "test_save",
            status_message: Some("Loaded 2 mods."),
            readiness: &readiness,
            launch_preview: None,
            last_steam_command: Some(&command),
        });

        assert!(snapshot.contains("status=Loaded 2 mods."));
        assert!(snapshot.contains("helper.backend=native"));
        assert!(snapshot.contains("mods.total=2"));
        assert!(snapshot.contains("mods.enabled=2"));
        assert!(snapshot.contains("mods.hidden=1"));
        assert!(snapshot.contains("mods.locked=1"));
        assert!(snapshot.contains("launch.script_logging=true"));
        assert!(snapshot.contains("launch.close_on_play=true"));
        assert!(snapshot.contains("readiness.Config=READY: Using test config"));
        assert!(snapshot.contains("steam_command.Requested IDs=111"));
    }

    #[test]
    fn steam_helper_backend_normalization_accepts_native_and_fixture() {
        assert_eq!(
            normalize_steam_helper_backend(" Native ").unwrap().as_str(),
            "native"
        );
        assert_eq!(
            normalize_steam_helper_backend("fixture").unwrap().as_str(),
            "fixture"
        );
        assert!(normalize_steam_helper_backend("other").is_err());
    }

    #[test]
    fn steam_helper_process_config_sets_backend_env() {
        let config = steam_helper_process_config("fixture").unwrap();

        assert_eq!(config.timeout, Duration::from_secs(30));
        assert_eq!(
            config.env_overrides[0],
            (
                "WH3MM_STEAM_HELPER_BACKEND".to_string(),
                "fixture".to_string()
            )
        );
        assert_eq!(config.env_overrides[1].0, STEAM_HELPER_COMMAND_LOG_ENV);
        let command_log_path = PathBuf::from(&config.env_overrides[1].1);
        assert_eq!(
            command_log_path.file_name().and_then(|name| name.to_str()),
            Some(STEAM_HELPER_COMMAND_LOG_FILE)
        );
        assert_eq!(
            command_log_path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str()),
            Some(DIAGNOSTICS_DIR_NAME)
        );
    }

    #[test]
    fn config_read_path_prefers_app_config_but_falls_back_to_legacy() {
        let root =
            std::env::temp_dir().join(format!("wh3mm-dioxus-config-path-{}", std::process::id()));
        let preferred = root.join("app").join("wh3mm_mod_state.json");
        let legacy = root.join("legacy").join("wh3mm_mod_state.json");
        fs::create_dir_all(preferred.parent().unwrap()).unwrap();
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();

        assert_eq!(
            resolve_config_file_read_path(&preferred, &legacy),
            preferred
        );

        fs::write(&legacy, b"legacy").unwrap();
        assert_eq!(resolve_config_file_read_path(&preferred, &legacy), legacy);

        fs::write(&preferred, b"preferred").unwrap();
        assert_eq!(
            resolve_config_file_read_path(&preferred, &legacy),
            preferred
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn reads_existing_launch_mod_list_pack_names() {
        let root = unique_temp_root("read-launch-list");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("my_mods.txt"), "mod \"fallback.pack\";").unwrap();
        fs::write(
            root.join("used_mods.txt"),
            "add_working_directory \"C:\\mods\";\nmod \"b.pack\";\nmod \"a.pack\";",
        )
        .unwrap();

        assert_eq!(
            read_existing_launch_mod_list_pack_names(&root),
            Some((
                "used_mods.txt".to_string(),
                vec!["b.pack".to_string(), "a.pack".to_string()]
            ))
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn game_mod_list_fallback_restores_enablement_and_order_without_saved_config() {
        let root = unique_temp_root("used-mods-fallback");
        let _ = fs::remove_dir_all(&root);
        let game_dir = root.join("game");
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(
            game_dir.join("used_mods.txt"),
            "mod \"b.pack\";\nmod \"a.pack\";",
        )
        .unwrap();
        let mods = vec![
            mod_record(
                &game_dir.join("data").join("a.pack").display().to_string(),
                None,
                "A",
            ),
            mod_record(
                &game_dir.join("data").join("b.pack").display().to_string(),
                None,
                "B",
            ),
            mod_record(
                &game_dir.join("data").join("c.pack").display().to_string(),
                None,
                "C",
            ),
        ];
        let mut status = "Discovered 3 mods.".to_string();

        let applied = apply_saved_or_existing_game_mod_list(
            mods,
            &root.join("missing-mod-state.json"),
            Some(&game_dir),
            &mut status,
        );

        assert_eq!(
            applied[0].identity.path,
            game_dir.join("data").join("b.pack").display().to_string()
        );
        assert!(applied[0].enabled);
        assert_eq!(
            applied[1].identity.path,
            game_dir.join("data").join("a.pack").display().to_string()
        );
        assert!(applied[1].enabled);
        assert_eq!(
            applied[2].identity.path,
            game_dir.join("data").join("c.pack").display().to_string()
        );
        assert!(!applied[2].enabled);
        assert!(status.contains("Restored enablement/order from used_mods.txt"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn saved_mod_state_takes_precedence_over_existing_launch_mod_list() {
        let root = unique_temp_root("saved-state-before-used-mods");
        let _ = fs::remove_dir_all(&root);
        let game_dir = root.join("game");
        let config_dir = root.join("config");
        fs::create_dir_all(&game_dir).unwrap();
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(game_dir.join("used_mods.txt"), "mod \"b.pack\";").unwrap();
        let a_path = game_dir.join("data").join("a.pack").display().to_string();
        let b_path = game_dir.join("data").join("b.pack").display().to_string();
        let c_path = game_dir.join("data").join("c.pack").display().to_string();
        let mod_state_path = config_dir.join("wh3mm_mod_state.json");
        write_mod_list_config_atomic(
            &mod_state_path,
            &ModListConfig {
                version: 1,
                mods: vec![
                    PersistedModState {
                        path: c_path.clone(),
                        workshop_id: None,
                        name: "C".to_string(),
                        enabled: true,
                        order: 0,
                        merged_source_paths: Vec::new(),
                    },
                    PersistedModState {
                        path: a_path.clone(),
                        workshop_id: None,
                        name: "A".to_string(),
                        enabled: false,
                        order: 1,
                        merged_source_paths: Vec::new(),
                    },
                ],
            },
        )
        .unwrap();
        let mods = vec![
            mod_record(&a_path, None, "A"),
            mod_record(&b_path, None, "B"),
            mod_record(&c_path, None, "C"),
        ];
        let mut status = "Discovered 3 mods.".to_string();

        let applied = apply_saved_or_existing_game_mod_list(
            mods,
            &mod_state_path,
            Some(&game_dir),
            &mut status,
        );

        assert_eq!(applied[0].identity.path, c_path);
        assert!(applied[0].enabled);
        assert_eq!(applied[1].identity.path, a_path);
        assert!(!applied[1].enabled);
        assert_eq!(applied[2].identity.path, b_path);
        assert!(!applied[2].enabled);
        assert!(status.contains("Restored saved enablement/order"));
        assert!(!status.contains("used_mods.txt"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn on_last_game_launch_preset_preserves_active_preset() {
        let root = unique_temp_root("last-launch-preset-active");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let preset_path = root.join("wh3mm_presets.json");
        write_preset_config_atomic(
            &preset_path,
            &PresetConfig {
                version: 1,
                active_preset: Some("Campaign".to_string()),
                presets: vec![PersistedPreset {
                    name: "Campaign".to_string(),
                    mods: vec![PersistedModState {
                        path: "campaign.pack".to_string(),
                        workshop_id: None,
                        name: "Campaign".to_string(),
                        enabled: true,
                        order: 0,
                        merged_source_paths: Vec::new(),
                    }],
                }],
            },
        )
        .unwrap();
        let mut first = mod_record("first.pack", None, "First");
        first.enabled = true;
        let second = mod_record("second.pack", Some("222"), "Second");
        let state = AppState::with_mods(GameId::Warhammer3, vec![first, second]);

        save_on_last_game_launch_preset_to_path(&preset_path, &preset_path, &state).unwrap();

        let config = read_preset_config(&preset_path).unwrap();
        assert_eq!(config.active_preset.as_deref(), Some("Campaign"));
        assert_eq!(
            config
                .presets
                .iter()
                .map(|preset| preset.name.as_str())
                .collect::<Vec<_>>(),
            ["Campaign", ON_LAST_GAME_LAUNCH_PRESET_NAME]
        );
        let snapshot = config
            .presets
            .iter()
            .find(|preset| preset.name == ON_LAST_GAME_LAUNCH_PRESET_NAME)
            .unwrap();
        assert_eq!(snapshot.mods.len(), 2);
        assert_eq!(snapshot.mods[0].path, "first.pack");
        assert!(snapshot.mods[0].enabled);
        assert_eq!(snapshot.mods[0].order, 0);
        assert_eq!(snapshot.mods[1].path, "second.pack");
        assert_eq!(snapshot.mods[1].workshop_id.as_deref(), Some("222"));
        assert!(!snapshot.mods[1].enabled);
        assert_eq!(snapshot.mods[1].order, 1);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn on_last_game_launch_preset_replaces_existing_snapshot() {
        let root = unique_temp_root("last-launch-preset-replace");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let preset_path = root.join("wh3mm_presets.json");
        write_preset_config_atomic(
            &preset_path,
            &PresetConfig {
                version: 1,
                active_preset: None,
                presets: vec![PersistedPreset {
                    name: ON_LAST_GAME_LAUNCH_PRESET_NAME.to_string(),
                    mods: vec![PersistedModState {
                        path: "old.pack".to_string(),
                        workshop_id: None,
                        name: "Old".to_string(),
                        enabled: true,
                        order: 0,
                        merged_source_paths: Vec::new(),
                    }],
                }],
            },
        )
        .unwrap();
        let mut fresh = mod_record("fresh.pack", None, "Fresh");
        fresh.enabled = true;
        let state = AppState::with_mods(GameId::Warhammer3, vec![fresh]);

        save_on_last_game_launch_preset_to_path(&preset_path, &preset_path, &state).unwrap();

        let config = read_preset_config(&preset_path).unwrap();
        let snapshots = config
            .presets
            .iter()
            .filter(|preset| preset.name == ON_LAST_GAME_LAUNCH_PRESET_NAME)
            .collect::<Vec<_>>();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].mods.len(), 1);
        assert_eq!(snapshots[0].mods[0].path, "fresh.pack");
        assert!(snapshots[0].mods[0].enabled);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn start_game_temp_packs_dir_lives_under_config_dir() {
        assert_eq!(
            start_game_temp_packs_dir_for_config_dir(&PathBuf::from("config-root")),
            PathBuf::from("config-root").join("tempPacks")
        );
    }

    #[test]
    fn legacy_ts_launch_option_conversion_preserves_make_units_generals() {
        let user_flow_options = BTreeMap::from([(
            "a.pack".to_string(),
            serde_json::json!({
                "whmmflows\\flow.json": {
                    "graphEnabled": true,
                    "optionValues": {"radius": 25}
                }
            }),
        )]);
        let launch_options = launch_options_from_legacy_ts(&LegacyTsLaunchOptions {
            skip_intro_movies: true,
            script_logging: true,
            auto_start_custom_battle: true,
            make_units_generals: true,
            close_on_play: true,
            changing_game_process_priority: true,
            user_flow_option_mod_count: 1,
            user_flow_options: user_flow_options.clone(),
            ..LegacyTsLaunchOptions::default()
        });

        assert!(launch_options.skip_intro_movies);
        assert!(launch_options.script_logging);
        assert!(launch_options.auto_start_custom_battle);
        assert!(launch_options.make_units_generals);
        assert!(launch_options.close_on_play);
        assert!(launch_options.high_process_priority);
        assert_eq!(launch_options.user_flow_options, user_flow_options);
        assert_eq!(
            legacy_ts_launch_options_from_state(&launch_options),
            LegacyTsLaunchOptions {
                skip_intro_movies: true,
                script_logging: true,
                auto_start_custom_battle: true,
                make_units_generals: true,
                close_on_play: true,
                changing_game_process_priority: true,
                user_flow_option_mod_count: 1,
                user_flow_options,
                ..LegacyTsLaunchOptions::default()
            }
        );
    }

    #[test]
    fn legacy_ts_import_summary_reports_make_units_generals_as_launchable() {
        assert_eq!(
            legacy_ts_launch_option_import_summary(&LegacyTsLaunchOptions {
                make_units_generals: true,
                ..LegacyTsLaunchOptions::default()
            }),
            " MakeUnitsGenerals was imported as an enabled launch option; launch generation will use enabled battle-permission DB rows when available."
        );
        assert_eq!(
            legacy_ts_launch_option_import_summary(&LegacyTsLaunchOptions::default()),
            ""
        );
    }

    #[test]
    fn legacy_ts_import_summary_reports_unported_launch_settings() {
        assert_eq!(
            legacy_ts_launch_option_import_summary(&LegacyTsLaunchOptions {
                close_on_play: true,
                changing_game_process_priority: true,
                pack_data_overwrite_mod_count: 1,
                user_flow_option_mod_count: 2,
                enabled_merged_mod_count: 1,
                ..LegacyTsLaunchOptions::default()
            }),
            " 1 enabled merged mod imported; launch generation will skip source packs represented by enabled merged packs. 1 pack overwrite group imported; launch generation will write replacement packs under whmm_overwrites. Unported TS launch settings detected (2 user-flow options); Rust launch will not reproduce those yet."
        );
        assert_eq!(
            legacy_ts_launch_option_import_summary(&LegacyTsLaunchOptions {
                close_on_play: true,
                changing_game_process_priority: true,
                ..LegacyTsLaunchOptions::default()
            }),
            ""
        );
    }

    #[test]
    fn rename_category_config_for_state_updates_current_mod_rows() {
        let mut mod_record = mod_record("a.pack", None, "A");
        mod_record.categories = vec!["Old".to_string()];
        let state = AppState::with_mods(GameId::Warhammer3, vec![mod_record]);
        let category_colors = BTreeMap::from([("Old".to_string(), "green".to_string())]);
        let config = capture_mod_user_config(
            &state.mods,
            &["Old".to_string(), "Other".to_string()],
            &category_colors,
        );

        let (config, mods) =
            rename_category_config_for_state(config, "Old", "New", &state).unwrap();

        assert_eq!(
            config.categories,
            vec!["New".to_string(), "Other".to_string()]
        );
        assert_eq!(
            config.category_colors.get("New").map(String::as_str),
            Some("green")
        );
        assert!(!config.category_colors.contains_key("Old"));
        assert_eq!(config.mods[0].categories, vec!["New".to_string()]);
        assert_eq!(mods[0].categories, vec!["New".to_string()]);
    }

    #[test]
    fn delete_category_config_for_state_updates_current_mod_rows() {
        let mut mod_record = mod_record("a.pack", None, "A");
        mod_record.categories = vec!["Old".to_string(), "Other".to_string()];
        let state = AppState::with_mods(GameId::Warhammer3, vec![mod_record]);
        let category_colors = BTreeMap::from([
            ("Old".to_string(), "red".to_string()),
            ("Other".to_string(), "green".to_string()),
        ]);
        let config = capture_mod_user_config(
            &state.mods,
            &["Old".to_string(), "Other".to_string()],
            &category_colors,
        );

        let (config, mods) = delete_category_config_for_state(config, "Old", &state).unwrap();

        assert_eq!(config.categories, vec!["Other".to_string()]);
        assert!(!config.category_colors.contains_key("Old"));
        assert_eq!(
            config.category_colors.get("Other").map(String::as_str),
            Some("green")
        );
        assert_eq!(config.mods[0].categories, vec!["Other".to_string()]);
        assert_eq!(mods[0].categories, vec!["Other".to_string()]);
    }

    #[test]
    fn launch_state_fingerprint_changes_when_start_options_change() {
        let mut mods = vec![mod_record("a.pack", None, "A")];
        mods[0].enabled = true;
        let default_options = LaunchOptionState::default();
        let logging_options = LaunchOptionState {
            script_logging: true,
            ..LaunchOptionState::default()
        };
        let make_generals_options = LaunchOptionState {
            make_units_generals: true,
            ..LaunchOptionState::default()
        };
        let high_priority_options = LaunchOptionState {
            high_process_priority: true,
            ..LaunchOptionState::default()
        };
        let close_on_play_options = LaunchOptionState {
            close_on_play: true,
            ..LaunchOptionState::default()
        };

        assert_ne!(
            launch_state_fingerprint(&mods, &default_options, ""),
            launch_state_fingerprint(&mods, &logging_options, "")
        );
        assert_ne!(
            launch_state_fingerprint(&mods, &default_options, ""),
            launch_state_fingerprint(&mods, &make_generals_options, "")
        );
        assert_ne!(
            launch_state_fingerprint(&mods, &default_options, ""),
            launch_state_fingerprint(&mods, &high_priority_options, "")
        );
        assert_ne!(
            launch_state_fingerprint(&mods, &default_options, ""),
            launch_state_fingerprint(&mods, &close_on_play_options, "")
        );
        assert_ne!(
            launch_state_fingerprint(&mods, &default_options, ""),
            launch_state_fingerprint(&mods, &default_options, "Karl Franz autosave")
        );
    }

    #[test]
    fn launch_status_reports_priority_and_close_on_play() {
        let applied = WindowsProcessPriorityUpdate {
            process_id: 123,
            requested_class: WindowsProcessPriorityClass::High,
            attempted: true,
            applied: true,
            message: "set process 123 priority to High".to_string(),
        };
        let failed = WindowsProcessPriorityUpdate {
            process_id: 123,
            requested_class: WindowsProcessPriorityClass::High,
            attempted: true,
            applied: false,
            message: "access denied".to_string(),
        };
        let skipped = WindowsProcessPriorityUpdate {
            process_id: 123,
            requested_class: WindowsProcessPriorityClass::High,
            attempted: false,
            applied: false,
            message: "process priority changes are only supported on Windows".to_string(),
        };

        assert_eq!(
            launch_priority_status(Some(&applied)),
            "Process priority: set process 123 priority to High."
        );
        assert_eq!(
            launch_priority_status(Some(&failed)),
            "Process priority was not changed: access denied."
        );
        assert_eq!(
            launch_priority_status(Some(&skipped)),
            "Process priority skipped: process priority changes are only supported on Windows."
        );
        assert_eq!(launch_priority_status(None), "Process priority unchanged.");
        assert_eq!(
            launch_status_with_close_on_play("Launched.".to_string(), true),
            "Launched. Close-on-play requested; app will exit in 5s."
        );
        assert_eq!(
            launch_status_with_close_on_play("Launched.".to_string(), false),
            "Launched."
        );
    }

    #[test]
    fn windows_launch_options_adds_generated_start_game_pack() {
        let options = build_windows_launch_options(
            r"C:\game".to_string(),
            r"C:\game\data".to_string(),
            &[],
            &LaunchOptionState {
                script_logging: true,
                ..LaunchOptionState::default()
            },
            "",
        )
        .unwrap();

        assert_eq!(options.extra_pack_groups.len(), 1);
        assert_eq!(
            options.extra_pack_groups[0].pack_names,
            [WH3_START_GAME_PACK_NAME.to_string()]
        );
        assert!(
            options.extra_pack_groups[0]
                .working_dir
                .ends_with("tempPacks")
        );
        assert_eq!(options.pre_launch_pack_writes.len(), 1);
        assert_eq!(
            PathBuf::from(&options.pre_launch_pack_writes[0].path)
                .file_name()
                .and_then(|name| name.to_str()),
            Some(WH3_START_GAME_PACK_NAME)
        );
        assert!(!options.pre_launch_pack_writes[0].bytes.is_empty());
        assert_eq!(
            options.pre_launch_pack_writes[0].packed_file_names,
            ["script\\enable_console_logging".to_string()]
        );

        let plan = plan_windows_launch(&options, &[]).unwrap();
        assert!(plan.mod_list_contents.contains("add_working_directory"));
        assert!(
            plan.mod_list_contents
                .contains(&format!("mod \"{WH3_START_GAME_PACK_NAME}\";"))
        );
    }

    #[test]
    fn pack_data_overwrites_generate_replacement_pack_group() {
        let root = unique_temp_root("pack-overwrite-launch");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let source_pack = root.join("a.pack");
        fs::write(
            &source_pack,
            build_pfh5_pack_bytes(&[PackFileWrite {
                name: "db\\unit_tables\\units".to_string(),
                payload: write_db_rows_to_payload(
                    &DbRows {
                        guid: None,
                        version: Some(1),
                        rows: vec![
                            vec![
                                cell("unit", DbPrimitiveValue::String("unit_a".to_string()), true),
                                cell("enabled", DbPrimitiveValue::Boolean(1), false),
                            ],
                            vec![
                                cell("unit", DbPrimitiveValue::String("unit_b".to_string()), true),
                                cell("enabled", DbPrimitiveValue::Boolean(1), false),
                            ],
                        ],
                    },
                    &overwrite_test_fields(),
                )
                .unwrap(),
            }])
            .unwrap(),
        )
        .unwrap();
        let mut mod_record = mod_record(&source_pack.display().to_string(), None, "a.pack");
        mod_record.enabled = true;
        let mut options =
            wh3mm_core::WindowsLaunchOptions::warhammer3(root.display().to_string(), "C:\\data");
        let launch_options = LaunchOptionState {
            pack_data_overwrites: BTreeMap::from([(
                source_pack.display().to_string(),
                vec![PackDataOverwrite {
                    pack_file_path: "db\\unit_tables\\units".to_string(),
                    columns_id: "unit_a".to_string(),
                    column_indices: vec![0],
                    column_values: vec![PackDataOverwriteValue::String("unit_a".to_string())],
                    operation: PackDataOverwriteOperation::Edit,
                    overwrite_index: Some(1),
                    overwrite_data: Some(PackDataOverwriteValue::Boolean(false)),
                }],
            )]),
            ..LaunchOptionState::default()
        };
        let schema = DbSchema::from([(
            "unit_tables".to_string(),
            vec![DbVersionSchema {
                version: 1,
                fields: overwrite_test_fields(),
            }],
        )]);

        append_pack_data_overwrite_packs(
            &mut options,
            &[mod_record.clone()],
            &launch_options,
            &schema,
        )
        .unwrap();

        let overwrites_dir = root.join("whmm_overwrites");
        assert_eq!(
            options.replaced_pack_paths,
            [source_pack.display().to_string()]
        );
        assert_eq!(options.extra_pack_groups.len(), 1);
        assert_eq!(
            options.extra_pack_groups[0].working_dir,
            overwrites_dir.display().to_string()
        );
        assert_eq!(options.extra_pack_groups[0].pack_names, ["a.pack"]);
        assert_eq!(options.pre_launch_pack_writes.len(), 1);
        assert_eq!(
            PathBuf::from(&options.pre_launch_pack_writes[0].path)
                .file_name()
                .and_then(|name| name.to_str()),
            Some("a.pack")
        );
        assert_eq!(
            options.pre_launch_pack_writes[0].packed_file_names,
            ["db\\unit_tables\\units".to_string()]
        );

        let plan = plan_windows_launch(&options, &[mod_record]).unwrap();
        assert_eq!(
            plan.mod_list_contents,
            format!(
                "add_working_directory \"{}\";\nmod \"a.pack\";",
                overwrites_dir.display()
            )
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn windows_launch_options_preserves_campaign_load_save_name() {
        let options = build_windows_launch_options(
            r"C:\game".to_string(),
            r"C:\game\data".to_string(),
            &[],
            &LaunchOptionState::default(),
            "  Karl Franz autosave  ",
        )
        .unwrap();

        assert_eq!(options.save_name.as_deref(), Some("Karl Franz autosave"));

        let plan = plan_windows_launch(&options, &[]).unwrap();
        assert_eq!(
            plan.args,
            [
                "game_startup_mode",
                "campaign_load",
                "Karl Franz autosave",
                ";",
                "used_mods.txt;"
            ]
        );
        assert!(plan.command_line_preview.contains(
            r#"Warhammer3.exe game_startup_mode campaign_load "Karl Franz autosave" ; used_mods.txt;"#
        ));
    }

    #[test]
    fn windows_launch_options_rejects_make_units_generals_without_source_tables() {
        let error = build_windows_launch_options(
            r"C:\game".to_string(),
            r"C:\game\data".to_string(),
            &[],
            &LaunchOptionState {
                make_units_generals: true,
                script_logging: true,
                ..LaunchOptionState::default()
            },
            "",
        )
        .unwrap_err();

        assert!(error.message.contains("MakeUnitsGenerals"));
        assert!(error.message.contains("no enabled or vanilla pack paths"));
    }

    #[test]
    fn generated_pack_details_lists_packed_files() {
        let details = generated_pack_details(&[PreLaunchPackWrite {
            path: r"C:\tempPacks\!!!!out.pack".to_string(),
            bytes: vec![1, 2, 3],
            packed_file_names: vec![
                "script\\enable_console_logging".to_string(),
                "script\\frontend\\mod\\pj_auto_custom_battles.lua".to_string(),
            ],
        }]);

        assert_eq!(
            details,
            "!!!!out.pack: script\\enable_console_logging, script\\frontend\\mod\\pj_auto_custom_battles.lua"
        );
    }

    #[test]
    fn enabled_pack_paths_for_start_game_uses_enabled_rows_with_paths() {
        let mut enabled = mod_record("enabled.pack", None, "Enabled");
        enabled.enabled = true;
        let mut always_enabled = mod_record("always.pack", None, "Always");
        always_enabled.always_enabled = true;
        let disabled = mod_record("disabled.pack", None, "Disabled");
        let mut missing_path = mod_record("", Some("123"), "Missing path");
        missing_path.enabled = true;

        assert_eq!(
            enabled_pack_paths_for_start_game(&[enabled, always_enabled, disabled, missing_path]),
            [PathBuf::from("enabled.pack"), PathBuf::from("always.pack")]
        );
    }

    #[test]
    fn start_game_source_pack_paths_appends_existing_vanilla_packs_after_enabled_mods() {
        let root = unique_temp_root("source-pack-paths");
        let _ = fs::remove_dir_all(&root);
        let data_dir = root.join("data");
        fs::create_dir_all(&data_dir).unwrap();
        fs::write(data_dir.join("data.pack"), b"").unwrap();
        fs::write(data_dir.join("db.pack"), b"").unwrap();

        let mut enabled = mod_record("enabled.pack", None, "Enabled");
        enabled.enabled = true;
        let mut duplicate_vanilla = mod_record(
            &data_dir.join("data.pack").display().to_string(),
            None,
            "Duplicate vanilla",
        );
        duplicate_vanilla.enabled = true;

        assert_eq!(
            start_game_source_pack_paths(
                &[enabled, duplicate_vanilla],
                &data_dir.display().to_string()
            ),
            [
                PathBuf::from("enabled.pack"),
                data_dir.join("data.pack"),
                data_dir.join("db.pack")
            ]
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn windows_launch_options_uses_vanilla_battle_permissions_for_make_generals() {
        let root = unique_temp_root("vanilla-make-generals");
        let _ = fs::remove_dir_all(&root);
        let game_dir = root.join("game");
        let data_dir = game_dir.join("data");
        fs::create_dir_all(&data_dir).unwrap();

        let source = DbRows {
            guid: None,
            version: Some(11),
            rows: vec![battle_permission_row(
                "wh3_main_cth_cathay",
                0,
                "wh3_main_cth_inf_peasant_spearmen_0",
            )],
        };
        let generated_source_pack = build_wh3_start_game_pack_with_battle_permissions(
            &Wh3StartGamePackOptions {
                make_units_generals: true,
                ..Wh3StartGamePackOptions::default()
            },
            &[source],
            &battle_permission_schema(),
        )
        .unwrap()
        .unwrap();
        fs::write(data_dir.join("data.pack"), generated_source_pack.bytes).unwrap();

        let options = build_windows_launch_options(
            game_dir.display().to_string(),
            data_dir.display().to_string(),
            &[],
            &LaunchOptionState {
                make_units_generals: true,
                ..LaunchOptionState::default()
            },
            "",
        )
        .unwrap();

        assert_eq!(options.pre_launch_pack_writes.len(), 1);
        assert!(
            options.pre_launch_pack_writes[0]
                .packed_file_names
                .contains(&WH3_MAKE_UNITS_GENERALS_TABLE_PATH.to_string())
        );

        let plan = plan_windows_launch(&options, &[]).unwrap();
        assert!(!plan.mod_list_contents.contains("data.pack"));
        assert!(
            plan.mod_list_contents
                .contains(&format!("mod \"{WH3_START_GAME_PACK_NAME}\";"))
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn steam_command_actions_use_matching_adapter_methods() {
        for (action, expected_command) in [
            (SteamCommandAction::Subscribe, "sub"),
            (SteamCommandAction::Download, "download"),
            (SteamCommandAction::Unsubscribe, "unsubscribe"),
        ] {
            let mut adapter = SteamWorkshopCommandAdapter::new(
                WH3_STEAM_APP_ID,
                RecordingCommandRunner::default(),
            );

            let result = run_steam_command_action(
                action,
                &mut adapter,
                &["111".to_string(), "bad".to_string(), "111".to_string()],
            )
            .unwrap();
            let runner = adapter.into_runner();

            assert_eq!(result.requested_ids, ["111"]);
            assert_eq!(
                runner.calls,
                vec![(expected_command.to_string(), vec!["111".to_string()])]
            );
        }
    }

    #[test]
    fn resubscribe_action_unsubscribes_subscribes_and_downloads() {
        let mut adapter =
            SteamWorkshopCommandAdapter::new(WH3_STEAM_APP_ID, RecordingCommandRunner::default());

        let result = run_steam_command_action(
            SteamCommandAction::Resubscribe,
            &mut adapter,
            &["111".to_string(), "111".to_string(), "bad".to_string()],
        )
        .unwrap();
        let runner = adapter.into_runner();

        assert_eq!(
            result,
            SteamWorkshopCommandResult::requested("resubscribe", vec!["111".to_string()])
        );
        assert_eq!(
            runner.calls,
            vec![
                ("unsubscribe".to_string(), vec!["111".to_string()]),
                ("sub".to_string(), vec!["111".to_string()]),
                ("download".to_string(), vec!["111".to_string()]),
            ]
        );
    }

    #[test]
    fn steam_command_status_pluralizes_counts() {
        assert_eq!(
            steam_command_status(
                SteamCommandAction::Download,
                &SteamWorkshopCommandResult::requested("download", vec!["111".to_string()])
            ),
            "Requested download for 1 workshop mod."
        );
        assert_eq!(
            steam_command_status(
                SteamCommandAction::Download,
                &SteamWorkshopCommandResult::requested(
                    "download",
                    vec!["111".to_string(), "222".to_string()]
                )
            ),
            "Requested download for 2 workshop mods."
        );
        assert_eq!(
            steam_command_status(
                SteamCommandAction::Resubscribe,
                &SteamWorkshopCommandResult::requested(
                    "resubscribe",
                    vec!["111".to_string(), "222".to_string()]
                )
            ),
            "Resubscribed 2 workshop mods."
        );
        assert_eq!(
            steam_command_status(
                SteamCommandAction::Download,
                &SteamWorkshopCommandResult {
                    command: "download".to_string(),
                    requested_ids: vec!["111".to_string(), "222".to_string()],
                    confirmed_ids: vec!["111".to_string(), "222".to_string()],
                    update_requested_ids: Vec::new(),
                    delay_ms: Some(250),
                }
            ),
            "Requested download for 2 workshop mods. Helper confirmed 2 IDs: 111, 222. Helper delay: 250ms."
        );
        assert_eq!(
            workshop_id_summary(
                &[
                    "111".to_string(),
                    "222".to_string(),
                    "333".to_string(),
                    "444".to_string(),
                    "555".to_string(),
                    "666".to_string(),
                ],
                5
            )
            .as_deref(),
            Some("111, 222, 333, 444, 555 +1")
        );
    }

    #[test]
    fn steam_command_panel_state_keeps_helper_details_visible() {
        let panel = steam_command_panel_state(
            SteamCommandAction::Download,
            &SteamWorkshopCommandResult {
                command: "download".to_string(),
                requested_ids: vec!["111".to_string(), "222".to_string()],
                confirmed_ids: vec!["111".to_string()],
                update_requested_ids: vec!["222".to_string()],
                delay_ms: Some(250),
            },
        );

        assert_eq!(panel.title, "Steam download");
        assert!(
            panel
                .summary
                .contains("Requested download for 2 workshop mods.")
        );
        assert_eq!(panel.rows[0].value, "download");
        assert_eq!(panel.rows[1].value, "111, 222");
        assert_eq!(panel.rows[2].value, "111");
        assert_eq!(panel.rows[3].label, "Update downloads");
        assert_eq!(panel.rows[3].value, "222");
        assert_eq!(panel.rows[4].value, "250ms");
    }

    #[test]
    fn steam_refresh_panel_state_reports_metadata_and_filter_counts() {
        let panel = steam_refresh_panel_state(&SteamRefreshResult {
            subscribed_ids: vec!["111".to_string(), "222".to_string()],
            metadata: vec![WorkshopModData {
                workshop_id: "111".to_string(),
                title: "A".to_string(),
                author: "Author".to_string(),
                dependency_ids: Vec::new(),
                dependency_id_to_name: Vec::new(),
                last_changed_ms: 0,
            }],
            requested_metadata_count: 2,
            missing_metadata_count: 1,
            filtered_unsubscribed_count: 3,
            renamed_count: 1,
        });

        assert_eq!(panel.title, "Steam refresh");
        assert_eq!(panel.rows[0].value, "111, 222");
        assert_eq!(panel.rows[1].value, "2 requested / 1 loaded / 1 missing");
        assert_eq!(
            panel.rows[2].value,
            "3 filtered as unsubscribed / 1 renamed from metadata"
        );
    }

    #[test]
    fn steam_resubscribe_panel_state_reports_verification_details() {
        let verified_result = SteamResubscribeResult {
            requested_ids: vec!["111".to_string(), "222".to_string()],
            removed_dirs: vec!["workshop/content/1142710/111".into()],
            observed_subscribed_ids: vec!["111".to_string(), "222".to_string()],
            failed_ids: Vec::new(),
            attempts: 1,
        };
        assert_eq!(
            steam_resubscribe_status(&verified_result),
            "Resubscribed 2 workshop mods. Removed 1 local workshop directory. Verified in 1 attempt."
        );
        let failed_result = SteamResubscribeResult {
            requested_ids: vec!["111".to_string(), "222".to_string()],
            removed_dirs: Vec::new(),
            observed_subscribed_ids: vec!["111".to_string()],
            failed_ids: vec!["222".to_string()],
            attempts: 2,
        };
        assert_eq!(
            steam_resubscribe_status(&failed_result),
            "Resubscribed 2 workshop mods. Removed 0 local workshop directories. 1 still missing after 2 attempts."
        );

        let panel = steam_resubscribe_panel_state(&failed_result);
        assert_eq!(panel.title, "Steam resubscribe");
        assert_eq!(panel.rows[0].value, "111, 222");
        assert_eq!(panel.rows[1].value, "111");
        assert_eq!(panel.rows[2].value, "222");
        assert_eq!(panel.rows[3].value, "none");
        assert_eq!(panel.rows[4].value, "2");
    }

    #[test]
    fn steam_check_update_status_reports_requested_downloads() {
        assert_eq!(
            steam_check_update_status(&SteamWorkshopCheckStateResult {
                checked_ids: vec!["111".to_string(), "222".to_string()],
                update_requested_ids: vec!["222".to_string()],
            }),
            "Checked Steam state for 2 workshop mods. Requested update downloads for 1 mod."
        );
        let panel = steam_check_update_panel_state(&SteamWorkshopCheckStateResult {
            checked_ids: vec!["111".to_string(), "222".to_string()],
            update_requested_ids: vec!["222".to_string()],
        });
        assert_eq!(panel.title, "Steam update check");
        assert_eq!(panel.rows[0].value, "111, 222");
        assert_eq!(panel.rows[1].value, "222");
        let empty_panel = steam_check_update_panel_state(&SteamWorkshopCheckStateResult::default());
        assert_eq!(empty_panel.summary, "No workshop mods are loaded.");
        assert_eq!(empty_panel.rows[0].value, "none");
    }

    #[test]
    fn apply_steam_metadata_updates_workshop_titles_only() {
        let mut mods = vec![
            mod_record("a.pack", Some("111"), "Old A"),
            mod_record("b.pack", Some("222"), "Already Fresh"),
            mod_record("c.pack", None, "Local"),
        ];
        let metadata = vec![
            workshop_mod_data("111", "Fresh A"),
            workshop_mod_data("222", "Already Fresh"),
        ];

        let renamed_count = apply_steam_metadata_to_mods(&mut mods, &metadata);

        assert_eq!(renamed_count, 1);
        assert_eq!(mods[0].display_name, "Fresh A");
        assert_eq!(mods[0].identity.name, "Fresh A");
        assert_eq!(mods[1].display_name, "Already Fresh");
        assert_eq!(mods[2].display_name, "Local");
    }

    #[test]
    fn fetch_steam_metadata_safely_batches_requests() {
        let mut adapter = RecordingMetadataAdapter {
            requested_batches: Vec::new(),
        };
        let ids = (1..=41).map(|id| id.to_string()).collect::<Vec<_>>();

        let result = fetch_steam_metadata_safely(&mut adapter, &ids).unwrap();

        assert_eq!(adapter.requested_batches.len(), 2);
        assert_eq!(adapter.requested_batches[0].len(), 40);
        assert_eq!(adapter.requested_batches[1], ["41"]);
        assert_eq!(result.requested_count, 41);
        assert_eq!(result.metadata.len(), 41);
        assert_eq!(result.missing_count, 0);
    }

    #[test]
    fn dependency_names_summary_limits_visible_names() {
        let mut metadata = workshop_mod_data("111", "Mod 111");
        metadata.dependency_id_to_name = vec![
            ("222".to_string(), "Dependency A".to_string()),
            ("333".to_string(), "Dependency B".to_string()),
            ("444".to_string(), "Dependency C".to_string()),
            ("555".to_string(), "Dependency D".to_string()),
        ];

        assert_eq!(
            dependency_names_summary(&metadata, 3).as_deref(),
            Some("Dependency A, Dependency B, Dependency C +1")
        );
    }

    #[test]
    fn dependency_names_summary_skips_empty_names() {
        let mut metadata = workshop_mod_data("111", "Mod 111");
        metadata.dependency_id_to_name = vec![("222".to_string(), " ".to_string())];

        assert_eq!(dependency_names_summary(&metadata, 3), None);
        assert_eq!(dependency_names_summary(&metadata, 0), None);
    }

    struct RecordingMetadataAdapter {
        requested_batches: Vec<Vec<String>>,
    }

    #[derive(Default)]
    struct RecordingCommandRunner {
        calls: Vec<(String, Vec<String>)>,
    }

    impl SteamWorkshopCommandRunner for RecordingCommandRunner {
        fn get_subscribed_ids(
            &mut self,
            _app_id: &str,
        ) -> Result<Vec<String>, SteamWorkshopAdapterError> {
            Ok(Vec::new())
        }

        fn subscribe_ids(
            &mut self,
            _app_id: &str,
            workshop_ids: &[String],
            _command_delay: Duration,
        ) -> Result<SteamWorkshopCommandResult, SteamWorkshopAdapterError> {
            self.calls.push(("sub".to_string(), workshop_ids.to_vec()));
            Ok(SteamWorkshopCommandResult::requested(
                "sub",
                workshop_ids.to_vec(),
            ))
        }

        fn download_ids(
            &mut self,
            _app_id: &str,
            workshop_ids: &[String],
            _command_delay: Duration,
        ) -> Result<SteamWorkshopCommandResult, SteamWorkshopAdapterError> {
            self.calls
                .push(("download".to_string(), workshop_ids.to_vec()));
            Ok(SteamWorkshopCommandResult::requested(
                "download",
                workshop_ids.to_vec(),
            ))
        }

        fn unsubscribe_ids(
            &mut self,
            _app_id: &str,
            workshop_ids: &[String],
        ) -> Result<SteamWorkshopCommandResult, SteamWorkshopAdapterError> {
            self.calls
                .push(("unsubscribe".to_string(), workshop_ids.to_vec()));
            Ok(SteamWorkshopCommandResult::requested(
                "unsubscribe",
                workshop_ids.to_vec(),
            ))
        }

        fn check_state_and_download_updates(
            &mut self,
            _app_id: &str,
            workshop_ids: &[String],
            _command_delay: Duration,
        ) -> Result<SteamWorkshopCheckStateResult, SteamWorkshopAdapterError> {
            Ok(SteamWorkshopCheckStateResult::checked(
                workshop_ids.to_vec(),
            ))
        }
    }

    impl SteamWorkshopMetadataAdapter for RecordingMetadataAdapter {
        fn fetch_mod_data_batch(
            &mut self,
            workshop_ids: &[String],
        ) -> Result<Vec<WorkshopModData>, SteamWorkshopAdapterError> {
            self.requested_batches.push(workshop_ids.to_vec());
            Ok(workshop_ids
                .iter()
                .map(|workshop_id| workshop_mod_data(workshop_id, &format!("Mod {workshop_id}")))
                .collect())
        }
    }

    fn unique_temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("wh3mm-dioxus-{name}-{}", std::process::id()))
    }

    fn battle_permission_schema() -> Vec<DbFieldSchema> {
        vec![
            field("faction", DbFieldType::StringU8, true),
            field("general_unit", DbFieldType::Boolean, true),
            field("unit", DbFieldType::StringU8, true),
            field("siege_unit_attacker", DbFieldType::Boolean, false),
            field("siege_unit_defender", DbFieldType::Boolean, false),
            field("general_portrait", DbFieldType::OptionalStringU8, false),
            field("general_uniform", DbFieldType::OptionalStringU8, false),
            field("set_piece_character", DbFieldType::OptionalStringU8, false),
            field("campaign_exclusive", DbFieldType::Boolean, false),
            field("armory_item_set", DbFieldType::OptionalStringU8, false),
            field("supports_upgrades", DbFieldType::Boolean, false),
        ]
    }

    fn overwrite_test_fields() -> Vec<DbFieldSchema> {
        vec![
            field("unit", DbFieldType::StringU8, true),
            field("enabled", DbFieldType::Boolean, false),
        ]
    }

    fn battle_permission_row(faction: &str, general_unit: u8, unit: &str) -> Vec<DbCell> {
        vec![
            cell(
                "faction",
                DbPrimitiveValue::String(faction.to_string()),
                true,
            ),
            cell(
                "general_unit",
                DbPrimitiveValue::Boolean(general_unit),
                true,
            ),
            cell("unit", DbPrimitiveValue::String(unit.to_string()), true),
            cell("siege_unit_attacker", DbPrimitiveValue::Boolean(1), false),
            cell("siege_unit_defender", DbPrimitiveValue::Boolean(1), false),
            cell(
                "general_portrait",
                DbPrimitiveValue::OptionalString(None),
                false,
            ),
            cell(
                "general_uniform",
                DbPrimitiveValue::OptionalString(None),
                false,
            ),
            cell(
                "set_piece_character",
                DbPrimitiveValue::OptionalString(None),
                false,
            ),
            cell("campaign_exclusive", DbPrimitiveValue::Boolean(0), false),
            cell(
                "armory_item_set",
                DbPrimitiveValue::OptionalString(None),
                false,
            ),
            cell("supports_upgrades", DbPrimitiveValue::Boolean(0), false),
        ]
    }

    fn field(name: &str, field_type: DbFieldType, is_key: bool) -> DbFieldSchema {
        DbFieldSchema {
            name: name.to_string(),
            field_type,
            is_key,
            reference: None,
        }
    }

    fn cell(name: &str, value: DbPrimitiveValue, is_key: bool) -> DbCell {
        DbCell {
            name: name.to_string(),
            is_key,
            value,
        }
    }

    fn mod_record(path: &str, workshop_id: Option<&str>, name: &str) -> ModRecord {
        ModRecord {
            identity: ModIdentity::new(path, workshop_id, name),
            display_name: name.to_string(),
            enabled: false,
            always_enabled: false,
            hidden: false,
            categories: Vec::new(),
            tags: Vec::new(),
        }
    }

    fn workshop_mod_data(workshop_id: &str, title: &str) -> WorkshopModData {
        WorkshopModData {
            workshop_id: workshop_id.to_string(),
            title: title.to_string(),
            author: String::new(),
            dependency_ids: Vec::new(),
            dependency_id_to_name: Vec::new(),
            last_changed_ms: 0,
        }
    }
}
