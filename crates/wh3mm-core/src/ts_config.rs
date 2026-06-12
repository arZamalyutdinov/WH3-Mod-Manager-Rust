//! Legacy TypeScript `config.json` import/export bridge.
//!
//! The Rust prototype stores narrow, workflow-specific config files. The
//! legacy Electron app stores the same alpha-critical data inside one larger
//! `config.json`. This module maps only the shared mod-manager fields:
//! active order/enablement, named presets, categories, hidden/always-enabled
//! mod state, selected game folder, and currently ported launch options.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::domain::ModIdentity;
use crate::overwrites::PackDataOverwrite;
use crate::persistence::{
    GameFolderConfig, ModListConfig, ModUserConfig, PersistedModState, PersistedModUserState,
    PersistedPreset, PresetConfig,
};
use crate::ports::{CoreError, CoreResult};

const CONFIG_VERSION: u32 = 1;
const DEFAULT_TS_GAME_KEY: &str = "wh3";

/// Imported/exportable subset of the legacy TypeScript app config.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyTsConfigSnapshot {
    /// Active mod list state for the selected game/current preset.
    pub mod_list: ModListConfig,
    /// Named preset state for the selected game.
    pub presets: PresetConfig,
    /// Category, hidden, and always-enabled state.
    pub mod_user: ModUserConfig,
    /// Selected WH3 game install folder when present in the TS config.
    pub game_folder: Option<GameFolderConfig>,
    /// Currently ported start-game option flags.
    pub launch_options: LegacyTsLaunchOptions,
}

/// Start-game option flags shared with the legacy TS config.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct LegacyTsLaunchOptions {
    /// TS `isSkipIntroMoviesEnabled`.
    pub skip_intro_movies: bool,
    /// TS `isScriptLoggingEnabled`.
    pub script_logging: bool,
    /// TS `isAutoStartCustomBattleEnabled`.
    pub auto_start_custom_battle: bool,
    /// TS `isMakeUnitsGeneralsEnabled`.
    ///
    /// The Dioxus launch path implements this DB-backed option when enabled
    /// source battle-permission rows are available, and the bridge preserves it
    /// for TS import/export compatibility.
    pub make_units_generals: bool,
    /// TS `isClosedOnPlay`; tracked so import can warn when Rust launch cannot
    /// yet reproduce the TS app lifecycle behavior.
    pub close_on_play: bool,
    /// TS `isChangingGameProcessPriority`; tracked so import can warn when Rust
    /// launch cannot yet reproduce the TS priority behavior.
    pub changing_game_process_priority: bool,
    /// TS `packDataOverwrites` entries imported for launch-time generated
    /// overwrite packs.
    pub pack_data_overwrites: BTreeMap<String, Vec<PackDataOverwrite>>,
    /// Number of TS pack entries with configured `packDataOverwrites`.
    pub pack_data_overwrite_mod_count: usize,
    /// TS `userFlowOptions` entries preserved for round-trip compatibility and
    /// future flow execution.
    pub user_flow_options: BTreeMap<String, serde_json::Value>,
    /// Number of TS pack entries with configured `userFlowOptions`.
    pub user_flow_option_mod_count: usize,
    /// Number of enabled current-preset mods that carry TS `mergedModsData`.
    pub enabled_merged_mod_count: usize,
}

/// Reads a legacy TypeScript config file and converts its alpha-critical fields.
///
/// # Errors
///
/// Returns [`CoreError`] when the file cannot be read or decoded.
pub fn read_legacy_ts_config(
    path: impl AsRef<Path>,
    game_key: &str,
) -> CoreResult<LegacyTsConfigSnapshot> {
    let path = path.as_ref();
    let bytes = fs::read(path)?;
    import_legacy_ts_config_bytes(&bytes, game_key)
}

/// Writes a TypeScript-compatible config file through a temp-file rename.
///
/// # Errors
///
/// Returns [`CoreError`] when the config cannot be encoded or written.
pub fn write_legacy_ts_config_atomic(
    path: impl AsRef<Path>,
    snapshot: &LegacyTsConfigSnapshot,
    game_key: &str,
) -> CoreResult<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let bytes = export_legacy_ts_config_bytes(snapshot, game_key)?;
    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, bytes)?;
    fs::rename(&temp_path, path)?;
    Ok(())
}

/// Converts TypeScript `config.json` bytes into the Rust config subset.
///
/// # Errors
///
/// Returns [`CoreError`] when the JSON cannot be decoded.
pub fn import_legacy_ts_config_bytes(
    bytes: &[u8],
    game_key: &str,
) -> CoreResult<LegacyTsConfigSnapshot> {
    let config: TsAppConfig = serde_json::from_slice(bytes)
        .map_err(|error| CoreError::parse(format!("failed to parse legacy TS config: {error}")))?;
    Ok(import_legacy_ts_config(&config, game_key))
}

/// Converts the Rust config subset into TypeScript `config.json` bytes.
///
/// # Errors
///
/// Returns [`CoreError`] when the JSON cannot be encoded.
pub fn export_legacy_ts_config_bytes(
    snapshot: &LegacyTsConfigSnapshot,
    game_key: &str,
) -> CoreResult<Vec<u8>> {
    let game_key = normalize_game_key_for_export(game_key);
    let active_preset_name = snapshot
        .presets
        .active_preset
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("Rust Export");
    let current_preset = ts_preset_from_mod_states(
        active_preset_name,
        &snapshot.mod_list.mods,
        &snapshot.mod_user,
    );
    let presets = export_presets(snapshot, &current_preset);
    let game_folder_paths = snapshot
        .game_folder
        .as_ref()
        .map(|config| {
            (
                game_key.clone(),
                TsGameFolderPaths::from_game_dir(&config.game_dir),
            )
        })
        .into_iter()
        .collect();

    let mut game_to_current_preset = BTreeMap::new();
    game_to_current_preset.insert(game_key.clone(), Some(current_preset.clone()));

    let mut game_to_presets = BTreeMap::new();
    game_to_presets.insert(game_key.clone(), Some(presets.clone()));

    let config = TsAppConfig {
        always_enabled_mods: snapshot
            .mod_user
            .mods
            .iter()
            .filter(|mod_state| mod_state.always_enabled)
            .map(ts_mod_from_user_state)
            .collect(),
        hidden_mods: snapshot
            .mod_user
            .mods
            .iter()
            .filter(|mod_state| mod_state.hidden)
            .map(ts_mod_from_user_state)
            .collect(),
        was_onboarding_ever_run: true,
        is_author_enabled: true,
        are_thumbnails_enabled: true,
        is_make_units_generals_enabled: snapshot.launch_options.make_units_generals,
        is_script_logging_enabled: snapshot.launch_options.script_logging,
        is_skip_intro_movies_enabled: snapshot.launch_options.skip_intro_movies,
        is_auto_start_custom_battle_enabled: snapshot.launch_options.auto_start_custom_battle,
        is_changing_game_process_priority: snapshot.launch_options.changing_game_process_priority,
        is_features_for_modders_enabled: false,
        is_closed_on_play: snapshot.launch_options.close_on_play,
        is_compat_checking_vanilla_packs: false,
        categories: snapshot.mod_user.categories.clone(),
        category_colors: snapshot.mod_user.category_colors.clone(),
        current_language: Some("en".to_string()),
        current_game: Some(game_key.clone()),
        pack_data_overwrites: pack_data_overwrites_to_json_map(
            &snapshot.launch_options.pack_data_overwrites,
        ),
        user_flow_options: non_empty_json_map_entries(&snapshot.launch_options.user_flow_options),
        game_folder_paths,
        game_to_current_preset,
        game_to_presets,
        current_preset: Some(current_preset),
        presets,
        app_folder_paths: snapshot
            .game_folder
            .as_ref()
            .map(|config| TsGameFolderPaths::from_game_dir(&config.game_dir)),
    };

    serde_json::to_vec_pretty(&config)
        .map_err(|error| CoreError::parse(format!("failed to encode legacy TS config: {error}")))
}

fn import_legacy_ts_config(
    config: &TsAppConfig,
    requested_game_key: &str,
) -> LegacyTsConfigSnapshot {
    let game_key = select_game_key(config, requested_game_key);
    let game_presets = presets_for_game(config, &game_key);
    let current_preset = current_preset_for_game(config, &game_key, &game_presets);
    let active_name = current_preset
        .as_ref()
        .map(|preset| preset.name.trim())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            game_presets
                .first()
                .map(|preset| preset.name.trim())
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned)
        });
    let active_mods = current_preset
        .as_ref()
        .map(|preset| ts_mod_states(&preset.mods))
        .unwrap_or_default();
    let mut preset_config = PresetConfig {
        version: CONFIG_VERSION,
        active_preset: active_name.clone(),
        presets: game_presets
            .iter()
            .filter_map(ts_persisted_preset)
            .collect(),
    };

    if let (Some(active_name), Some(current_preset)) = (&active_name, &current_preset) {
        if !current_preset.mods.is_empty()
            && !preset_config
                .presets
                .iter()
                .any(|preset| preset.name == *active_name)
        {
            preset_config.presets.push(PersistedPreset {
                name: active_name.clone(),
                mods: ts_mod_states(&current_preset.mods),
            });
        }
    }

    let mod_user = import_mod_user_config(config, current_preset.as_ref(), &game_presets);
    let game_folder = game_folder_for_game(config, &game_key);
    let enabled_merged_mod_count = current_preset
        .as_ref()
        .map(|preset| count_enabled_merged_mods(&preset.mods))
        .unwrap_or_default();
    let pack_data_overwrites = parse_ts_pack_data_overwrites(&config.pack_data_overwrites);
    let launch_options = LegacyTsLaunchOptions {
        skip_intro_movies: config.is_skip_intro_movies_enabled,
        script_logging: config.is_script_logging_enabled,
        auto_start_custom_battle: config.is_auto_start_custom_battle_enabled,
        make_units_generals: config.is_make_units_generals_enabled,
        close_on_play: config.is_closed_on_play,
        changing_game_process_priority: config.is_changing_game_process_priority,
        pack_data_overwrite_mod_count: count_non_empty_pack_overwrite_entries(
            &pack_data_overwrites,
        ),
        pack_data_overwrites,
        user_flow_options: non_empty_json_map_entries(&config.user_flow_options),
        user_flow_option_mod_count: count_non_empty_json_map_entries(&config.user_flow_options),
        enabled_merged_mod_count,
    };

    LegacyTsConfigSnapshot {
        mod_list: ModListConfig {
            version: CONFIG_VERSION,
            mods: active_mods,
        },
        presets: preset_config,
        mod_user,
        game_folder,
        launch_options,
    }
}

fn select_game_key(config: &TsAppConfig, requested_game_key: &str) -> String {
    if !requested_game_key.trim().is_empty() {
        return requested_game_key.trim().to_string();
    }

    config
        .current_game
        .as_deref()
        .filter(|game| !game.trim().is_empty())
        .unwrap_or(DEFAULT_TS_GAME_KEY)
        .trim()
        .to_string()
}

fn normalize_game_key_for_export(game_key: &str) -> String {
    if game_key.trim().is_empty() {
        DEFAULT_TS_GAME_KEY.to_string()
    } else {
        game_key.trim().to_string()
    }
}

fn presets_for_game(config: &TsAppConfig, game_key: &str) -> Vec<TsPreset> {
    config
        .game_to_presets
        .get(game_key)
        .and_then(Clone::clone)
        .filter(|presets| !presets.is_empty())
        .or_else(|| {
            if game_key == DEFAULT_TS_GAME_KEY && !config.presets.is_empty() {
                Some(config.presets.clone())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

fn current_preset_for_game(
    config: &TsAppConfig,
    game_key: &str,
    game_presets: &[TsPreset],
) -> Option<TsPreset> {
    config
        .game_to_current_preset
        .get(game_key)
        .and_then(Clone::clone)
        .or_else(|| {
            if game_key == DEFAULT_TS_GAME_KEY {
                config.current_preset.clone()
            } else {
                None
            }
        })
        .or_else(|| config.current_preset.clone())
        .or_else(|| game_presets.first().cloned())
}

fn ts_persisted_preset(preset: &TsPreset) -> Option<PersistedPreset> {
    let name = preset.name.trim();
    if name.is_empty() {
        return None;
    }

    Some(PersistedPreset {
        name: name.to_string(),
        mods: ts_mod_states(&preset.mods),
    })
}

fn ts_mod_states(mods: &[TsMod]) -> Vec<PersistedModState> {
    ts_launch_order_indices(mods)
        .into_iter()
        .enumerate()
        .map(|(order, index)| {
            let mod_state = &mods[index];
            PersistedModState {
                path: mod_state.path.clone(),
                workshop_id: normalize_optional_id(&mod_state.workshop_id),
                name: ts_mod_identity_name(mod_state),
                enabled: mod_state.is_enabled,
                order,
                merged_source_paths: merged_source_paths_from_ts_mod(mod_state),
            }
        })
        .collect()
}

// Mirrors the legacy TS `sortByNameAndLoadOrder(...)` launch order: sort by
// TS name first, remove rows with explicit loadOrder, then splice them back in.
fn ts_launch_order_indices(mods: &[TsMod]) -> Vec<usize> {
    let mut indices = (0..mods.len()).collect::<Vec<_>>();
    indices.sort_by(|left, right| compare_ts_mod_names(&mods[*left].name, &mods[*right].name));

    let mut ordered_with_load_order = indices
        .iter()
        .copied()
        .filter(|index| mods[*index].load_order.is_some())
        .collect::<Vec<_>>();
    ordered_with_load_order.sort_by_key(|index| mods[*index].load_order);

    for index in &ordered_with_load_order {
        if let Some(position) = indices.iter().position(|candidate| candidate == index) {
            indices.remove(position);
        }
    }

    for index in ordered_with_load_order {
        let insertion_index = mods[index].load_order.unwrap_or(indices.len());
        indices.insert(insertion_index.min(indices.len()), index);
    }

    indices
}

fn compare_ts_mod_names(left: &str, right: &str) -> Ordering {
    let left = left.to_ascii_lowercase();
    let right = right.to_ascii_lowercase();
    let max_len = left.len().max(right.len());

    for index in 0..max_len {
        if index == left.len() {
            return Ordering::Greater;
        }
        if index == right.len() {
            return Ordering::Less;
        }

        match left.as_bytes()[index].cmp(&right.as_bytes()[index]) {
            Ordering::Equal => {}
            ordering => return ordering,
        }
    }

    Ordering::Equal
}

fn import_mod_user_config(
    config: &TsAppConfig,
    current_preset: Option<&TsPreset>,
    game_presets: &[TsPreset],
) -> ModUserConfig {
    let mut merged = BTreeMap::<String, PersistedModUserState>::new();

    if let Some(current_preset) = current_preset {
        for mod_state in &current_preset.mods {
            merge_ts_mod_user_state(&mut merged, mod_state, false, false);
        }
    }

    for preset in game_presets {
        for mod_state in &preset.mods {
            merge_ts_mod_user_state(&mut merged, mod_state, false, false);
        }
    }

    for mod_state in &config.always_enabled_mods {
        merge_ts_mod_user_state(&mut merged, mod_state, true, false);
    }
    for mod_state in &config.hidden_mods {
        merge_ts_mod_user_state(&mut merged, mod_state, false, true);
    }

    let mut categories = config.categories.clone();
    dedupe_strings(&mut categories);
    for mod_state in merged.values() {
        for category in &mod_state.categories {
            if !category.trim().is_empty() && !categories.iter().any(|saved| saved == category) {
                categories.push(category.clone());
            }
        }
    }

    ModUserConfig {
        version: CONFIG_VERSION,
        categories,
        category_colors: config.category_colors.clone(),
        mods: merged.into_values().collect(),
    }
}

fn merge_ts_mod_user_state(
    merged: &mut BTreeMap<String, PersistedModUserState>,
    mod_state: &TsMod,
    always_enabled: bool,
    hidden: bool,
) {
    let path = mod_state.path.clone();
    let workshop_id = normalize_optional_id(&mod_state.workshop_id);
    let name = ts_mod_identity_name(mod_state);
    let key = stable_key(&path, workshop_id.as_deref(), &name);
    let entry = merged.entry(key).or_insert_with(|| PersistedModUserState {
        path,
        workshop_id,
        name,
        categories: Vec::new(),
        hidden: false,
        always_enabled: false,
    });

    if always_enabled {
        entry.always_enabled = true;
    }
    if hidden {
        entry.hidden = true;
    }
    for category in &mod_state.categories {
        let category = category.trim();
        if !category.is_empty() && !entry.categories.iter().any(|saved| saved == category) {
            entry.categories.push(category.to_string());
        }
    }
}

fn game_folder_for_game(config: &TsAppConfig, game_key: &str) -> Option<GameFolderConfig> {
    config
        .game_folder_paths
        .get(game_key)
        .and_then(TsGameFolderPaths::game_path)
        .or_else(|| {
            config
                .app_folder_paths
                .as_ref()
                .and_then(TsGameFolderPaths::game_path)
        })
        .map(|game_dir| GameFolderConfig {
            version: CONFIG_VERSION,
            game_dir,
        })
}

fn export_presets(snapshot: &LegacyTsConfigSnapshot, current_preset: &TsPreset) -> Vec<TsPreset> {
    let mut presets = snapshot
        .presets
        .presets
        .iter()
        .map(|preset| ts_preset_from_mod_states(&preset.name, &preset.mods, &snapshot.mod_user))
        .collect::<Vec<_>>();

    if !presets
        .iter()
        .any(|preset| preset.name == current_preset.name)
    {
        presets.push(current_preset.clone());
    }

    presets
}

fn ts_preset_from_mod_states(
    name: &str,
    mods: &[PersistedModState],
    mod_user: &ModUserConfig,
) -> TsPreset {
    let mut mods = mods.to_vec();
    mods.sort_by_key(|mod_state| mod_state.order);
    TsPreset {
        name: if name.trim().is_empty() {
            "Rust Export".to_string()
        } else {
            name.trim().to_string()
        },
        version: Some(2),
        mods: mods
            .iter()
            .map(|mod_state| ts_mod_from_mod_state(mod_state, mod_user))
            .collect(),
    }
}

fn ts_mod_from_mod_state(mod_state: &PersistedModState, mod_user: &ModUserConfig) -> TsMod {
    let user_state = find_user_state(mod_user, mod_state);
    let mut ts_mod = TsMod::from_identity(
        &mod_state.path,
        mod_state.workshop_id.as_deref().unwrap_or_default(),
        &mod_state.name,
    );
    ts_mod.human_name.clone_from(&mod_state.name);
    ts_mod.is_enabled = mod_state.enabled;
    ts_mod.load_order = Some(mod_state.order);
    if let Some(user_state) = user_state {
        ts_mod.categories.clone_from(&user_state.categories);
        ts_mod.tags = Vec::new();
    }
    ts_mod.merged_mods_data = ts_merged_mods_data_from_paths(&mod_state.merged_source_paths);
    ts_mod
}

fn ts_mod_from_user_state(mod_state: &PersistedModUserState) -> TsMod {
    let mut ts_mod = TsMod::from_identity(
        &mod_state.path,
        mod_state.workshop_id.as_deref().unwrap_or_default(),
        &mod_state.name,
    );
    ts_mod.categories.clone_from(&mod_state.categories);
    ts_mod
}

fn find_user_state<'a>(
    config: &'a ModUserConfig,
    mod_state: &PersistedModState,
) -> Option<&'a PersistedModUserState> {
    let target = ModIdentity::new(
        mod_state.path.clone(),
        mod_state.workshop_id.clone(),
        mod_state.name.clone(),
    );
    config.mods.iter().find(|candidate| {
        ModIdentity::new(
            candidate.path.clone(),
            candidate.workshop_id.clone(),
            candidate.name.clone(),
        )
        .matches(&target)
    })
}

fn ts_mod_identity_name(mod_state: &TsMod) -> String {
    if !mod_state.name.trim().is_empty() {
        return mod_state.name.trim().to_string();
    }
    if !mod_state.human_name.trim().is_empty() {
        return mod_state.human_name.trim().to_string();
    }
    path_file_name(&mod_state.path).unwrap_or_else(|| "unknown.pack".to_string())
}

fn normalize_optional_id(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn stable_key(path: &str, workshop_id: Option<&str>, name: &str) -> String {
    if !path.is_empty() {
        return format!("path:{path}");
    }
    if let Some(workshop_id) = workshop_id {
        if !workshop_id.is_empty() {
            return format!("workshop:{workshop_id}");
        }
    }
    format!("name:{name}")
}

fn path_file_name(path: &str) -> Option<String> {
    path.rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .map(ToOwned::to_owned)
}

fn merged_source_paths_from_ts_mod(mod_state: &TsMod) -> Vec<String> {
    let Some(merged_mods_data) = &mod_state.merged_mods_data else {
        return Vec::new();
    };

    let mut paths = Vec::new();
    for entry in merged_mods_data {
        let Some(path) = entry.get("path").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let path = path.trim();
        if !path.is_empty() && !paths.iter().any(|saved| saved == path) {
            paths.push(path.to_string());
        }
    }

    paths
}

fn ts_merged_mods_data_from_paths(paths: &[String]) -> Option<Vec<serde_json::Value>> {
    let mut entries = Vec::new();
    for path in paths {
        let path = path.trim();
        if path.is_empty()
            || entries.iter().any(|entry: &serde_json::Value| {
                entry.get("path").and_then(serde_json::Value::as_str) == Some(path)
            })
        {
            continue;
        }

        entries.push(serde_json::json!({
            "name": path_file_name(path).unwrap_or_else(|| path.to_string()),
            "path": path,
        }));
    }

    (!entries.is_empty()).then_some(entries)
}

fn dedupe_strings(values: &mut Vec<String>) {
    let mut deduped = Vec::with_capacity(values.len());
    for value in values.iter() {
        let value = value.trim();
        if !value.is_empty() && !deduped.iter().any(|saved| saved == value) {
            deduped.push(value.to_string());
        }
    }
    *values = deduped;
}

fn parse_ts_pack_data_overwrites(
    map: &BTreeMap<String, serde_json::Value>,
) -> BTreeMap<String, Vec<PackDataOverwrite>> {
    map.iter()
        .filter_map(|(pack_path, value)| {
            let overwrites = serde_json::from_value::<Vec<PackDataOverwrite>>(value.clone())
                .ok()
                .unwrap_or_default()
                .into_iter()
                .filter(valid_pack_data_overwrite)
                .collect::<Vec<_>>();
            (!overwrites.is_empty()).then(|| (pack_path.clone(), overwrites))
        })
        .collect()
}

fn valid_pack_data_overwrite(overwrite: &PackDataOverwrite) -> bool {
    !overwrite.pack_file_path.trim().is_empty()
        && overwrite.column_indices.len() == overwrite.column_values.len()
}

fn pack_data_overwrites_to_json_map(
    map: &BTreeMap<String, Vec<PackDataOverwrite>>,
) -> BTreeMap<String, serde_json::Value> {
    map.iter()
        .filter_map(|(pack_path, overwrites)| {
            if overwrites.is_empty() {
                return None;
            }

            serde_json::to_value(overwrites)
                .ok()
                .map(|value| (pack_path.clone(), value))
        })
        .collect()
}

fn count_non_empty_pack_overwrite_entries(map: &BTreeMap<String, Vec<PackDataOverwrite>>) -> usize {
    map.values()
        .filter(|overwrites| !overwrites.is_empty())
        .count()
}

fn non_empty_json_map_entries(
    map: &BTreeMap<String, serde_json::Value>,
) -> BTreeMap<String, serde_json::Value> {
    map.iter()
        .filter(|(_, value)| !is_empty_json_launch_value(value))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn count_non_empty_json_map_entries(map: &BTreeMap<String, serde_json::Value>) -> usize {
    map.values()
        .filter(|value| !is_empty_json_launch_value(value))
        .count()
}

fn is_empty_json_launch_value(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Array(values) => values.is_empty(),
        serde_json::Value::Object(values) => values.is_empty(),
        _ => false,
    }
}

fn count_enabled_merged_mods(mods: &[TsMod]) -> usize {
    mods.iter()
        .filter(|mod_state| {
            mod_state.is_enabled
                && mod_state
                    .merged_mods_data
                    .as_ref()
                    .is_some_and(|merged_mods_data| !merged_mods_data.is_empty())
        })
        .count()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
#[serde(rename_all = "camelCase")]
struct TsAppConfig {
    #[serde(default)]
    always_enabled_mods: Vec<TsMod>,
    #[serde(default)]
    hidden_mods: Vec<TsMod>,
    #[serde(default)]
    was_onboarding_ever_run: bool,
    #[serde(default)]
    is_author_enabled: bool,
    #[serde(default)]
    are_thumbnails_enabled: bool,
    #[serde(default)]
    is_make_units_generals_enabled: bool,
    #[serde(default)]
    is_script_logging_enabled: bool,
    #[serde(default)]
    is_skip_intro_movies_enabled: bool,
    #[serde(default)]
    is_auto_start_custom_battle_enabled: bool,
    #[serde(default)]
    is_changing_game_process_priority: bool,
    #[serde(default)]
    is_features_for_modders_enabled: bool,
    #[serde(default)]
    is_closed_on_play: bool,
    #[serde(default)]
    is_compat_checking_vanilla_packs: bool,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    category_colors: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_game: Option<String>,
    #[serde(default)]
    pack_data_overwrites: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    user_flow_options: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    game_folder_paths: BTreeMap<String, TsGameFolderPaths>,
    #[serde(default)]
    game_to_current_preset: BTreeMap<String, Option<TsPreset>>,
    #[serde(default)]
    game_to_presets: BTreeMap<String, Option<Vec<TsPreset>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_preset: Option<TsPreset>,
    #[serde(default)]
    presets: Vec<TsPreset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    app_folder_paths: Option<TsGameFolderPaths>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TsPreset {
    #[serde(default)]
    mods: Vec<TsMod>,
    #[serde(default)]
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
#[serde(rename_all = "camelCase")]
struct TsMod {
    #[serde(default)]
    human_name: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    img_path: String,
    #[serde(default)]
    workshop_id: String,
    #[serde(default)]
    is_enabled: bool,
    #[serde(default)]
    mod_directory: String,
    #[serde(default)]
    is_in_data: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_changed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_changed_local: Option<u64>,
    #[serde(default)]
    load_order: Option<usize>,
    #[serde(default)]
    author: String,
    #[serde(default)]
    is_deleted: bool,
    #[serde(default)]
    is_movie: bool,
    #[serde(default)]
    dependency_packs: Vec<String>,
    #[serde(default)]
    req_mod_id_to_name: Vec<(String, String)>,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    subbed_time: Option<u64>,
    #[serde(default)]
    is_symbolic_link: bool,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    is_in_modding: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    merged_mods_data: Option<Vec<serde_json::Value>>,
}

impl TsMod {
    fn from_identity(path: &str, workshop_id: &str, name: &str) -> Self {
        Self {
            human_name: name.to_string(),
            name: name.to_string(),
            path: path.to_string(),
            workshop_id: workshop_id.to_string(),
            is_enabled: false,
            load_order: None,
            author: String::new(),
            categories: Vec::new(),
            tags: Vec::new(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TsGameFolderPaths {
    #[serde(default)]
    game_path: String,
    #[serde(default)]
    content_folder: String,
    #[serde(default)]
    data_folder: String,
}

impl TsGameFolderPaths {
    fn from_game_dir(game_dir: &str) -> Self {
        let game_dir = game_dir.trim();
        Self {
            game_path: game_dir.to_string(),
            content_folder: String::new(),
            data_folder: if game_dir.is_empty() {
                String::new()
            } else {
                format!("{game_dir}\\data")
            },
        }
    }

    fn game_path(&self) -> Option<String> {
        let game_path = self.game_path.trim();
        if game_path.is_empty() {
            None
        } else {
            Some(game_path.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LegacyTsConfigSnapshot, LegacyTsLaunchOptions, export_legacy_ts_config_bytes,
        import_legacy_ts_config_bytes,
    };
    use crate::persistence::{
        GameFolderConfig, ModListConfig, ModUserConfig, PersistedModState, PersistedModUserState,
        PersistedPreset, PresetConfig,
    };
    use crate::{PackDataOverwrite, PackDataOverwriteOperation, PackDataOverwriteValue};
    use std::collections::BTreeMap;

    #[test]
    fn imports_legacy_ts_config_for_active_wh3_game() {
        let config = br##"{
            "currentGame": "wh3",
            "categories": ["Core", "Core", "Utility"],
            "categoryColors": {"Core": "#ffffff"},
            "isSkipIntroMoviesEnabled": true,
            "isScriptLoggingEnabled": true,
            "isAutoStartCustomBattleEnabled": true,
            "isClosedOnPlay": true,
            "isChangingGameProcessPriority": true,
            "packDataOverwrites": {
                "C:\\mods\\a.pack": [{
                    "packFilePath": "db\\unit_tables\\units",
                    "columnsId": "unit_a",
                    "columnIndices": [0],
                    "columnValues": ["unit_a"],
                    "operation": "EDIT",
                    "overwriteIndex": 2,
                    "overwriteData": false
                }],
                "C:\\mods\\empty.pack": []
            },
            "userFlowOptions": {
                "a.pack": {
                    "whmmflows\\flow.json": {
                        "graphEnabled": true,
                        "optionValues": {"radius": 25}
                    }
                },
                "empty.pack": {}
            },
            "gameFolderPaths": {
                "wh3": {
                    "gamePath": "C:\\Steam\\steamapps\\common\\Total War WARHAMMER III",
                    "contentFolder": "C:\\Steam\\steamapps\\workshop\\content\\1142710",
                    "dataFolder": "C:\\Steam\\steamapps\\common\\Total War WARHAMMER III\\data"
                }
            },
            "alwaysEnabledMods": [
                {"name": "always.pack", "path": "C:\\mods\\always.pack", "workshopId": "333"}
            ],
            "hiddenMods": [
                {"name": "hidden.pack", "path": "C:\\mods\\hidden.pack", "workshopId": "444"}
            ],
            "gameToCurrentPreset": {
                "wh3": {
                    "name": "Campaign",
                    "version": 2,
                    "mods": [
                        {"name": "b.pack", "humanName": "B", "path": "C:\\mods\\b.pack", "workshopId": "222", "isEnabled": false, "loadOrder": 1, "categories": ["Utility"]},
                        {"name": "a.pack", "humanName": "A", "path": "C:\\mods\\a.pack", "workshopId": "111", "isEnabled": true, "loadOrder": 0, "categories": ["Core"], "mergedModsData": [{"name": "merged.pack", "path": "C:\\mods\\merged.pack"}]}
                    ]
                }
            },
            "gameToPresets": {
                "wh3": [
                    {
                        "name": "Campaign",
                        "version": 2,
                        "mods": [
                            {"name": "a.pack", "path": "C:\\mods\\a.pack", "workshopId": "111", "isEnabled": true, "loadOrder": 0, "categories": ["Core"]}
                        ]
                    }
                ]
            }
        }"##;

        let imported = import_legacy_ts_config_bytes(config, "wh3").unwrap();

        assert_eq!(imported.presets.active_preset.as_deref(), Some("Campaign"));
        assert_eq!(imported.mod_list.mods.len(), 2);
        assert_eq!(mod_names(&imported.mod_list.mods), ["a.pack", "b.pack"]);
        assert_eq!(imported.mod_list.mods[0].order, 0);
        assert_eq!(imported.mod_list.mods[1].order, 1);
        assert_eq!(
            imported.mod_list.mods[0].merged_source_paths,
            [r"C:\mods\merged.pack"]
        );
        assert_eq!(
            imported
                .game_folder
                .as_ref()
                .map(|config| config.game_dir.as_str()),
            Some("C:\\Steam\\steamapps\\common\\Total War WARHAMMER III")
        );
        assert_eq!(imported.mod_user.categories, ["Core", "Utility"]);
        assert_eq!(
            imported
                .mod_user
                .category_colors
                .get("Core")
                .map(String::as_str),
            Some("#ffffff")
        );
        assert!(
            imported
                .mod_user
                .mods
                .iter()
                .any(|mod_state| mod_state.name == "always.pack" && mod_state.always_enabled)
        );
        assert!(
            imported
                .mod_user
                .mods
                .iter()
                .any(|mod_state| mod_state.name == "hidden.pack" && mod_state.hidden)
        );
        assert!(imported.launch_options.skip_intro_movies);
        assert!(imported.launch_options.script_logging);
        assert!(imported.launch_options.auto_start_custom_battle);
        assert!(imported.launch_options.close_on_play);
        assert!(imported.launch_options.changing_game_process_priority);
        assert_eq!(imported.launch_options.pack_data_overwrite_mod_count, 1);
        assert_eq!(
            imported
                .launch_options
                .pack_data_overwrites
                .get(r"C:\mods\a.pack")
                .and_then(|overwrites| overwrites.first())
                .map(|overwrite| overwrite.pack_file_path.as_str()),
            Some("db\\unit_tables\\units")
        );
        assert_eq!(imported.launch_options.user_flow_option_mod_count, 1);
        assert_eq!(
            imported
                .launch_options
                .user_flow_options
                .get("a.pack")
                .and_then(|pack| pack.get("whmmflows\\flow.json"))
                .and_then(|flow| flow.get("graphEnabled"))
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert!(
            !imported
                .launch_options
                .user_flow_options
                .contains_key("empty.pack")
        );
        assert_eq!(imported.launch_options.enabled_merged_mod_count, 1);
    }

    #[test]
    fn imports_legacy_ts_mods_without_load_order_in_ts_name_sort_order() {
        let config = br#"{
            "currentGame": "wh3",
            "gameToCurrentPreset": {
                "wh3": {
                    "name": "Campaign",
                    "mods": [
                        {"name": "b.pack", "path": "C:\\mods\\b.pack", "isEnabled": true},
                        {"name": "a.pack", "path": "C:\\mods\\a.pack", "isEnabled": true}
                    ]
                }
            }
        }"#;

        let imported = import_legacy_ts_config_bytes(config, "wh3").unwrap();

        assert_eq!(mod_names(&imported.mod_list.mods), ["a.pack", "b.pack"]);
        assert_eq!(mod_orders(&imported.mod_list.mods), [0, 1]);
    }

    #[test]
    fn imports_legacy_ts_mods_with_mixed_load_order_like_ts_launch_sort() {
        let config = br#"{
            "currentGame": "wh3",
            "gameToCurrentPreset": {
                "wh3": {
                    "name": "Campaign",
                    "mods": [
                        {"name": "c.pack", "path": "C:\\mods\\c.pack", "isEnabled": true},
                        {"name": "b.pack", "path": "C:\\mods\\b.pack", "isEnabled": true, "loadOrder": 0},
                        {"name": "a.pack", "path": "C:\\mods\\a.pack", "isEnabled": true},
                        {"name": "d.pack", "path": "C:\\mods\\d.pack", "isEnabled": true, "loadOrder": 9}
                    ]
                }
            }
        }"#;

        let imported = import_legacy_ts_config_bytes(config, "wh3").unwrap();

        assert_eq!(
            mod_names(&imported.mod_list.mods),
            ["b.pack", "a.pack", "c.pack", "d.pack"]
        );
        assert_eq!(mod_orders(&imported.mod_list.mods), [0, 1, 2, 3]);
    }

    #[test]
    fn exports_legacy_ts_config_and_imports_it_back() {
        let mut merged_mod = persisted_mod("a.pack", Some("111"), "a.pack", true, 0);
        merged_mod.merged_source_paths = vec![r"C:\mods\source.pack".to_string()];
        let pack_data_overwrites = BTreeMap::from([(
            "a.pack".to_string(),
            vec![PackDataOverwrite {
                pack_file_path: "db\\unit_tables\\units".to_string(),
                columns_id: "unit_a".to_string(),
                column_indices: vec![0],
                column_values: vec![PackDataOverwriteValue::String("unit_a".to_string())],
                operation: PackDataOverwriteOperation::Edit,
                overwrite_index: Some(2),
                overwrite_data: Some(PackDataOverwriteValue::Boolean(false)),
            }],
        )]);
        let user_flow_options = BTreeMap::from([(
            "a.pack".to_string(),
            serde_json::json!({
                "whmmflows\\flow.json": {
                    "graphEnabled": true,
                    "optionValues": {
                        "radius": 25
                    }
                }
            }),
        )]);
        let snapshot = LegacyTsConfigSnapshot {
            mod_list: ModListConfig {
                version: 1,
                mods: vec![
                    persisted_mod("b.pack", Some("222"), "b.pack", false, 1),
                    merged_mod.clone(),
                ],
            },
            presets: PresetConfig {
                version: 1,
                active_preset: Some("Campaign".to_string()),
                presets: vec![PersistedPreset {
                    name: "Campaign".to_string(),
                    mods: vec![merged_mod],
                }],
            },
            mod_user: ModUserConfig {
                version: 1,
                categories: vec!["Core".to_string()],
                category_colors: BTreeMap::from([("Core".to_string(), "blue".to_string())]),
                mods: vec![
                    PersistedModUserState {
                        path: "a.pack".to_string(),
                        workshop_id: Some("111".to_string()),
                        name: "a.pack".to_string(),
                        categories: vec!["Core".to_string()],
                        hidden: false,
                        always_enabled: true,
                    },
                    PersistedModUserState {
                        path: "b.pack".to_string(),
                        workshop_id: Some("222".to_string()),
                        name: "b.pack".to_string(),
                        categories: Vec::new(),
                        hidden: true,
                        always_enabled: false,
                    },
                ],
            },
            game_folder: Some(GameFolderConfig {
                version: 1,
                game_dir: r"C:\Games\Total War WARHAMMER III".to_string(),
            }),
            launch_options: LegacyTsLaunchOptions {
                skip_intro_movies: true,
                script_logging: true,
                auto_start_custom_battle: true,
                make_units_generals: false,
                close_on_play: true,
                changing_game_process_priority: true,
                pack_data_overwrite_mod_count: 1,
                pack_data_overwrites,
                user_flow_option_mod_count: 1,
                user_flow_options,
                ..LegacyTsLaunchOptions::default()
            },
        };

        let bytes = export_legacy_ts_config_bytes(&snapshot, "wh3").unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(value["currentGame"], "wh3");
        assert_eq!(value["currentPreset"]["name"], "Campaign");
        assert_eq!(
            value["gameToCurrentPreset"]["wh3"]["mods"][0]["name"],
            "a.pack"
        );
        assert_eq!(
            value["gameToCurrentPreset"]["wh3"]["mods"][0]["loadOrder"],
            0
        );
        assert_eq!(
            value["gameToCurrentPreset"]["wh3"]["mods"][0]["mergedModsData"][0]["path"],
            r"C:\mods\source.pack"
        );
        assert_eq!(
            value["packDataOverwrites"]["a.pack"][0]["packFilePath"],
            "db\\unit_tables\\units"
        );
        assert_eq!(
            value["packDataOverwrites"]["a.pack"][0]["operation"],
            "EDIT"
        );
        assert_eq!(
            value["userFlowOptions"]["a.pack"]["whmmflows\\flow.json"]["optionValues"]["radius"],
            25
        );
        assert_eq!(value["alwaysEnabledMods"][0]["name"], "a.pack");
        assert_eq!(value["hiddenMods"][0]["name"], "b.pack");
        assert_eq!(value["isSkipIntroMoviesEnabled"], true);
        assert_eq!(value["isClosedOnPlay"], true);
        assert_eq!(value["isChangingGameProcessPriority"], true);

        let imported = import_legacy_ts_config_bytes(&bytes, "wh3").unwrap();

        assert_eq!(imported.presets.active_preset.as_deref(), Some("Campaign"));
        assert_eq!(imported.mod_list.mods[0].name, "a.pack");
        assert!(imported.mod_list.mods[0].enabled);
        assert_eq!(
            imported.mod_list.mods[0].merged_source_paths,
            [r"C:\mods\source.pack"]
        );
        assert_eq!(imported.launch_options.pack_data_overwrite_mod_count, 1);
        assert_eq!(
            imported
                .launch_options
                .pack_data_overwrites
                .get("a.pack")
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(imported.launch_options.user_flow_option_mod_count, 1);
        assert_eq!(
            imported
                .launch_options
                .user_flow_options
                .get("a.pack")
                .and_then(|pack| pack.get("whmmflows\\flow.json"))
                .and_then(|flow| flow.get("optionValues"))
                .and_then(|values| values.get("radius"))
                .and_then(serde_json::Value::as_i64),
            Some(25)
        );
        assert_eq!(imported.mod_user.categories, ["Core"]);
        assert!(
            imported
                .mod_user
                .mods
                .iter()
                .any(|mod_state| mod_state.name == "a.pack" && mod_state.always_enabled)
        );
        assert!(imported.launch_options.skip_intro_movies);
        assert!(imported.launch_options.script_logging);
    }

    fn persisted_mod(
        path: &str,
        workshop_id: Option<&str>,
        name: &str,
        enabled: bool,
        order: usize,
    ) -> PersistedModState {
        PersistedModState {
            path: path.to_string(),
            workshop_id: workshop_id.map(ToOwned::to_owned),
            name: name.to_string(),
            enabled,
            order,
            merged_source_paths: Vec::new(),
        }
    }

    fn mod_names(mods: &[PersistedModState]) -> Vec<&str> {
        mods.iter()
            .map(|mod_state| mod_state.name.as_str())
            .collect()
    }

    fn mod_orders(mods: &[PersistedModState]) -> Vec<usize> {
        mods.iter().map(|mod_state| mod_state.order).collect()
    }
}
