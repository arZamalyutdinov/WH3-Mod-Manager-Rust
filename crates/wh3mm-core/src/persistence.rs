//! Active mod-list persistence.
//!
//! The legacy TypeScript app stores this inside a much larger `config.json`
//! preset structure. The Rust prototype starts with a narrow equivalent for
//! restoring discovered mods' enablement and order.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::domain::{ModIdentity, ModRecord, merged_source_path_tag};
use crate::ports::{CoreError, CoreResult};

const CONFIG_VERSION: u32 = 1;

/// Persisted active mod-list state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModListConfig {
    /// Config format version.
    pub version: u32,
    /// Ordered mod entries.
    pub mods: Vec<PersistedModState>,
}

/// Persisted named preset collection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PresetConfig {
    /// Config format version.
    pub version: u32,
    /// Active/last selected preset name when known.
    pub active_preset: Option<String>,
    /// Saved named presets.
    pub presets: Vec<PersistedPreset>,
}

/// Persisted user-visible mod metadata and visibility config.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModUserConfig {
    /// Config format version.
    pub version: u32,
    /// User-defined category names in display order.
    pub categories: Vec<String>,
    /// User-defined category color keys.
    pub category_colors: BTreeMap<String, String>,
    /// Per-mod category/visibility/lock state.
    pub mods: Vec<PersistedModUserState>,
}

/// Persisted user-visible metadata for one mod.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersistedModUserState {
    /// Path identity, preferred when present.
    pub path: String,
    /// Optional Steam workshop ID fallback.
    pub workshop_id: Option<String>,
    /// Name fallback for legacy/local state.
    pub name: String,
    /// User-defined categories assigned to the mod.
    #[serde(default)]
    pub categories: Vec<String>,
    /// Whether the mod is hidden from normal mod-list views.
    #[serde(default)]
    pub hidden: bool,
    /// Whether the mod is forced enabled.
    #[serde(default)]
    pub always_enabled: bool,
}

impl PersistedModUserState {
    fn identity(&self) -> ModIdentity {
        ModIdentity::new(
            self.path.clone(),
            self.workshop_id.clone(),
            self.name.clone(),
        )
    }
}

/// Persisted named preset.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersistedPreset {
    /// User-facing preset name.
    pub name: String,
    /// Ordered mod entries captured for this preset.
    pub mods: Vec<PersistedModState>,
}

/// Persisted game-folder state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GameFolderConfig {
    /// Config format version.
    pub version: u32,
    /// WH3 game install directory.
    pub game_dir: String,
}

/// Persisted Steam helper executable state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SteamHelperConfig {
    /// Config format version.
    pub version: u32,
    /// External Steam helper executable path.
    pub helper_path: String,
    /// Optional helper backend selector, such as `native` or `fixture`.
    #[serde(default)]
    pub backend: Option<String>,
}

/// Persisted state for one mod row.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersistedModState {
    /// Path identity, preferred when present.
    pub path: String,
    /// Optional Steam workshop ID fallback.
    pub workshop_id: Option<String>,
    /// Name fallback for legacy/local state.
    pub name: String,
    /// Explicit user enablement.
    pub enabled: bool,
    /// Zero-based order in the active mod list.
    pub order: usize,
    /// Source pack paths declared by merged-pack metadata.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged_source_paths: Vec<String>,
}

impl PersistedModState {
    fn identity(&self) -> ModIdentity {
        ModIdentity::new(
            self.path.clone(),
            self.workshop_id.clone(),
            self.name.clone(),
        )
    }
}

/// Captures the current ordered mod list for persistence.
#[must_use]
pub fn capture_mod_list_config(mods: &[ModRecord]) -> ModListConfig {
    ModListConfig {
        version: CONFIG_VERSION,
        mods: mods
            .iter()
            .enumerate()
            .map(|(order, mod_record)| PersistedModState {
                path: mod_record.identity.path.clone(),
                workshop_id: mod_record.identity.workshop_id.clone(),
                name: mod_record.identity.name.clone(),
                enabled: mod_record.enabled,
                order,
                merged_source_paths: mod_record
                    .merged_source_paths()
                    .map(str::to_string)
                    .collect(),
            })
            .collect(),
    }
}

/// Captures user-visible mod metadata and visibility config.
#[must_use]
pub fn capture_mod_user_config(
    mods: &[ModRecord],
    categories: &[String],
    category_colors: &BTreeMap<String, String>,
) -> ModUserConfig {
    let mut merged_categories = categories.to_vec();
    for category in mods.iter().flat_map(|mod_record| &mod_record.categories) {
        if !category.trim().is_empty() && !merged_categories.iter().any(|saved| saved == category) {
            merged_categories.push(category.clone());
        }
    }

    ModUserConfig {
        version: CONFIG_VERSION,
        categories: merged_categories,
        category_colors: category_colors.clone(),
        mods: mods
            .iter()
            .map(|mod_record| PersistedModUserState {
                path: mod_record.identity.path.clone(),
                workshop_id: mod_record.identity.workshop_id.clone(),
                name: mod_record.identity.name.clone(),
                categories: mod_record.categories.clone(),
                hidden: mod_record.hidden,
                always_enabled: mod_record.always_enabled,
            })
            .collect(),
    }
}

/// Adds a user category to the global category list when missing.
///
/// # Errors
///
/// Returns [`CoreError`] when the category name is empty.
pub fn add_category_config(
    config: &mut ModUserConfig,
    category: impl Into<String>,
) -> CoreResult<()> {
    let category = normalize_category(category)?;
    ensure_category(&mut config.categories, &category);
    Ok(())
}

/// Renames a user category and all persisted mod assignments.
///
/// If the target category already exists, assignments are merged without
/// duplicate category labels.
///
/// # Errors
///
/// Returns [`CoreError`] when either category name is empty or the source
/// category is not present in the config.
pub fn rename_category_config(
    config: &mut ModUserConfig,
    old_category: &str,
    new_category: impl Into<String>,
) -> CoreResult<()> {
    let old_category = normalize_category(old_category)?;
    let new_category = normalize_category(new_category)?;
    let found = config
        .categories
        .iter()
        .any(|category| category == &old_category)
        || config.category_colors.contains_key(&old_category)
        || config.mods.iter().any(|mod_state| {
            mod_state
                .categories
                .iter()
                .any(|category| category == &old_category)
        });
    if !found {
        return Err(CoreError::invalid_input(format!(
            "category not found: {old_category}"
        )));
    }

    if old_category == new_category {
        ensure_category(&mut config.categories, &new_category);
        return Ok(());
    }

    replace_category_label(&mut config.categories, &old_category, &new_category);
    if let Some(color) = config.category_colors.remove(&old_category) {
        config
            .category_colors
            .entry(new_category.clone())
            .or_insert(color);
    }
    for mod_state in &mut config.mods {
        replace_category_label(&mut mod_state.categories, &old_category, &new_category);
    }

    Ok(())
}

/// Deletes a user category, its color, and all persisted mod assignments.
///
/// # Errors
///
/// Returns [`CoreError`] when the category name is empty.
pub fn delete_category_config(config: &mut ModUserConfig, category: &str) -> CoreResult<()> {
    let category = normalize_category(category)?;
    config.categories.retain(|saved| saved != &category);
    config.category_colors.remove(&category);
    for mod_state in &mut config.mods {
        mod_state
            .categories
            .retain(|assigned| assigned != &category);
    }
    Ok(())
}

/// Sets the color key for a user category.
///
/// # Errors
///
/// Returns [`CoreError`] when either category or color key is empty.
pub fn set_category_color_config(
    config: &mut ModUserConfig,
    category: impl Into<String>,
    color_key: impl Into<String>,
) -> CoreResult<()> {
    let category = normalize_category(category)?;
    let color_key = color_key.into().trim().to_string();
    if color_key.is_empty() {
        return Err(CoreError::invalid_input("category color is required"));
    }

    ensure_category(&mut config.categories, &category);
    config.category_colors.insert(category, color_key);
    Ok(())
}

/// Adds a user category assignment to a mod record.
///
/// # Errors
///
/// Returns [`CoreError`] when the category name is empty.
pub fn add_mod_category(mod_record: &mut ModRecord, category: impl Into<String>) -> CoreResult<()> {
    let category = normalize_category(category)?;
    ensure_category(&mut mod_record.categories, &category);
    Ok(())
}

/// Removes a user category assignment from a mod record.
///
/// # Errors
///
/// Returns [`CoreError`] when the category name is empty.
pub fn remove_mod_category(mod_record: &mut ModRecord, category: &str) -> CoreResult<bool> {
    let category = normalize_category(category)?;
    let before_len = mod_record.categories.len();
    mod_record
        .categories
        .retain(|assigned| assigned != &category);
    Ok(mod_record.categories.len() != before_len)
}

/// Captures a named preset from the current ordered mod list.
///
/// # Errors
///
/// Returns [`CoreError`] when the preset name is empty.
pub fn capture_preset(name: impl Into<String>, mods: &[ModRecord]) -> CoreResult<PersistedPreset> {
    let name = name.into().trim().to_string();
    if name.is_empty() {
        return Err(CoreError::invalid_input("preset name is required"));
    }

    Ok(PersistedPreset {
        name,
        mods: capture_mod_list_config(mods).mods,
    })
}

/// Captures a one-preset config from the current ordered mod list.
///
/// # Errors
///
/// Returns [`CoreError`] when the preset name is empty.
pub fn capture_preset_config(
    active_preset: impl Into<String>,
    mods: &[ModRecord],
) -> CoreResult<PresetConfig> {
    let preset = capture_preset(active_preset, mods)?;
    Ok(PresetConfig {
        version: CONFIG_VERSION,
        active_preset: Some(preset.name.clone()),
        presets: vec![preset],
    })
}

/// Inserts or replaces a preset by name and marks it active.
///
/// # Errors
///
/// Returns [`CoreError`] when the preset name is empty.
pub fn upsert_preset_config(
    config: &mut PresetConfig,
    preset_name: impl Into<String>,
    mods: &[ModRecord],
) -> CoreResult<()> {
    let preset = capture_preset(preset_name, mods)?;
    if let Some(existing) = config
        .presets
        .iter_mut()
        .find(|existing| existing.name == preset.name)
    {
        *existing = preset.clone();
    } else {
        config.presets.push(preset.clone());
    }
    config.active_preset = Some(preset.name);
    Ok(())
}

/// Returns preset names in storage order.
#[must_use]
pub fn preset_names(config: &PresetConfig) -> Vec<String> {
    config
        .presets
        .iter()
        .map(|preset| preset.name.clone())
        .collect()
}

/// Deletes a preset by exact name and clears it as active when needed.
///
/// # Errors
///
/// Returns [`CoreError`] when the named preset is not present.
pub fn delete_preset_config(config: &mut PresetConfig, preset_name: &str) -> CoreResult<()> {
    let before_len = config.presets.len();
    config.presets.retain(|preset| preset.name != preset_name);
    if config.presets.len() == before_len {
        return Err(CoreError::invalid_input(format!(
            "preset not found: {preset_name}"
        )));
    }

    if config.active_preset.as_deref() == Some(preset_name) {
        config.active_preset = config.presets.first().map(|preset| preset.name.clone());
    }

    Ok(())
}

/// Applies saved enablement/order to freshly discovered mods.
///
/// Saved rows that are not present in discovery are ignored. Newly discovered
/// rows are appended by display name/path after restored rows.
#[must_use]
pub fn apply_mod_list_config(
    mut discovered_mods: Vec<ModRecord>,
    config: &ModListConfig,
) -> Vec<ModRecord> {
    for mod_record in &mut discovered_mods {
        if let Some(saved) = find_saved_state(config, &mod_record.identity) {
            mod_record.enabled = saved.enabled;
            merge_merged_source_path_tags(mod_record, &saved.merged_source_paths);
        }
    }

    let mut remaining = discovered_mods;
    let mut ordered_mods = Vec::with_capacity(remaining.len());
    let mut saved_rows = config.mods.clone();
    saved_rows.sort_by_key(|saved| saved.order);

    for saved in saved_rows {
        let saved_identity = saved.identity();
        if let Some(index) = remaining
            .iter()
            .position(|mod_record| mod_record.identity.matches(&saved_identity))
        {
            ordered_mods.push(remaining.remove(index));
        }
    }

    remaining.sort_by(|left, right| {
        left.display_name
            .to_ascii_lowercase()
            .cmp(&right.display_name.to_ascii_lowercase())
            .then_with(|| left.identity.path.cmp(&right.identity.path))
    });
    ordered_mods.extend(remaining);
    ordered_mods
}

fn merge_merged_source_path_tags(mod_record: &mut ModRecord, paths: &[String]) {
    for path in paths {
        let Some(tag) = merged_source_path_tag(path) else {
            continue;
        };
        if !mod_record.tags.iter().any(|existing| existing == &tag) {
            mod_record.tags.push(tag);
        }
    }
}

/// Parses pack names from a WH3 launch mod-list file such as `used_mods.txt`.
///
/// The legacy TS app reads lines shaped like `mod "pack_name.pack";` when no
/// app config exists. This parser intentionally ignores working-directory
/// lines and malformed rows.
#[must_use]
pub fn parse_mod_list_pack_names(contents: &str) -> Vec<String> {
    contents
        .lines()
        .filter_map(parse_mod_list_pack_name_line)
        .map(str::to_string)
        .collect()
}

/// Applies pack names from a launch mod-list file to discovered mods.
///
/// Matching is case-insensitive by pack file name. Matched mods are enabled
/// and ordered as they appeared in the launch file; unmatched mods are disabled
/// and appended in their existing discovery order.
#[must_use]
pub fn apply_mod_list_pack_names(
    discovered_mods: Vec<ModRecord>,
    pack_names: &[String],
) -> Vec<ModRecord> {
    let mut requested_names = Vec::<String>::new();
    for pack_name in pack_names {
        let key = normalize_pack_name(pack_name);
        if !key.is_empty() && !requested_names.iter().any(|existing| existing == &key) {
            requested_names.push(key);
        }
    }

    let mut remaining = discovered_mods
        .into_iter()
        .map(|mut mod_record| {
            mod_record.enabled = mod_record_pack_name_key(&mod_record).is_some_and(|pack_name| {
                requested_names
                    .iter()
                    .any(|requested| requested == &pack_name)
            });
            mod_record
        })
        .collect::<Vec<_>>();
    let mut ordered_mods = Vec::with_capacity(remaining.len());

    for requested_name in requested_names {
        if let Some(index) = remaining.iter().position(|mod_record| {
            mod_record_pack_name_key(mod_record).as_ref() == Some(&requested_name)
        }) {
            ordered_mods.push(remaining.remove(index));
        }
    }

    ordered_mods.extend(remaining);
    ordered_mods
}

/// Applies a named preset to freshly discovered/current mods.
///
/// Saved rows that are not present in the current mod list are ignored. Newly
/// discovered rows are appended by display name/path after restored rows.
///
/// # Errors
///
/// Returns [`CoreError`] when the named preset is not present.
pub fn apply_preset_config(
    discovered_mods: Vec<ModRecord>,
    config: &PresetConfig,
    preset_name: &str,
) -> CoreResult<Vec<ModRecord>> {
    let preset = config
        .presets
        .iter()
        .find(|preset| preset.name == preset_name)
        .ok_or_else(|| CoreError::invalid_input(format!("preset not found: {preset_name}")))?;
    Ok(apply_mod_list_config(
        discovered_mods,
        &ModListConfig {
            version: config.version,
            mods: preset.mods.clone(),
        },
    ))
}

/// Applies user-visible mod metadata and visibility config.
#[must_use]
pub fn apply_mod_user_config(
    mut discovered_mods: Vec<ModRecord>,
    config: &ModUserConfig,
) -> Vec<ModRecord> {
    for mod_record in &mut discovered_mods {
        if let Some(saved) = config
            .mods
            .iter()
            .find(|saved| saved.identity().matches(&mod_record.identity))
        {
            mod_record.categories = saved.categories.clone();
            mod_record.hidden = saved.hidden;
            mod_record.always_enabled = saved.always_enabled;
        }
    }

    discovered_mods
}

/// Reads persisted active mod-list state from JSON.
///
/// # Errors
///
/// Returns [`CoreError`] when the file cannot be read or decoded.
pub fn read_mod_list_config(path: impl AsRef<Path>) -> CoreResult<ModListConfig> {
    let path = path.as_ref();
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|error| {
        CoreError::parse(format!(
            "failed to parse mod-list config {}: {error}",
            path.display()
        ))
    })
}

/// Reads persisted preset collection from JSON.
///
/// # Errors
///
/// Returns [`CoreError`] when the file cannot be read or decoded.
pub fn read_preset_config(path: impl AsRef<Path>) -> CoreResult<PresetConfig> {
    let path = path.as_ref();
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|error| {
        CoreError::parse(format!(
            "failed to parse preset config {}: {error}",
            path.display()
        ))
    })
}

/// Reads persisted user-visible mod metadata and visibility config from JSON.
///
/// # Errors
///
/// Returns [`CoreError`] when the file cannot be read or decoded.
pub fn read_mod_user_config(path: impl AsRef<Path>) -> CoreResult<ModUserConfig> {
    let path = path.as_ref();
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|error| {
        CoreError::parse(format!(
            "failed to parse mod user config {}: {error}",
            path.display()
        ))
    })
}

/// Writes persisted active mod-list state through a temp-file rename.
///
/// If the new config is empty but the existing config has mods, the write is
/// skipped to preserve the TypeScript guard against startup/discovery races.
///
/// # Errors
///
/// Returns [`CoreError`] when the config cannot be serialized or written.
pub fn write_mod_list_config_atomic(
    path: impl AsRef<Path>,
    config: &ModListConfig,
) -> CoreResult<bool> {
    let path = path.as_ref();
    if config.mods.is_empty()
        && read_mod_list_config(path).is_ok_and(|existing_config| !existing_config.mods.is_empty())
    {
        return Ok(false);
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let bytes = serde_json::to_vec_pretty(config)
        .map_err(|error| CoreError::parse(format!("failed to encode mod-list config: {error}")))?;
    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, bytes)?;
    fs::rename(&temp_path, path)?;
    Ok(true)
}

/// Writes persisted preset collection through a temp-file rename.
///
/// If the new config is empty but the existing config has presets, the write is
/// skipped to preserve the TypeScript guard against startup/discovery races.
///
/// # Errors
///
/// Returns [`CoreError`] when the config cannot be serialized or written.
pub fn write_preset_config_atomic(
    path: impl AsRef<Path>,
    config: &PresetConfig,
) -> CoreResult<bool> {
    let path = path.as_ref();
    if config.presets.is_empty()
        && read_preset_config(path).is_ok_and(|existing_config| !existing_config.presets.is_empty())
    {
        return Ok(false);
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let bytes = serde_json::to_vec_pretty(config)
        .map_err(|error| CoreError::parse(format!("failed to encode preset config: {error}")))?;
    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, bytes)?;
    fs::rename(&temp_path, path)?;
    Ok(true)
}

/// Writes persisted user-visible mod metadata and visibility config through a
/// temp-file rename.
///
/// If the new config is empty but the existing config has user metadata, the
/// write is skipped to preserve the TypeScript guard against startup/discovery
/// races.
///
/// # Errors
///
/// Returns [`CoreError`] when the config cannot be serialized or written.
pub fn write_mod_user_config_atomic(
    path: impl AsRef<Path>,
    config: &ModUserConfig,
) -> CoreResult<bool> {
    let path = path.as_ref();
    if config.categories.is_empty()
        && config.category_colors.is_empty()
        && config.mods.is_empty()
        && read_mod_user_config(path).is_ok_and(|existing_config| {
            !existing_config.categories.is_empty()
                || !existing_config.category_colors.is_empty()
                || !existing_config.mods.is_empty()
        })
    {
        return Ok(false);
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let bytes = serde_json::to_vec_pretty(config)
        .map_err(|error| CoreError::parse(format!("failed to encode mod user config: {error}")))?;
    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, bytes)?;
    fs::rename(&temp_path, path)?;
    Ok(true)
}

/// Captures the selected game folder for persistence.
#[must_use]
pub fn capture_game_folder_config(game_dir: impl Into<String>) -> GameFolderConfig {
    GameFolderConfig {
        version: CONFIG_VERSION,
        game_dir: game_dir.into(),
    }
}

/// Captures the selected Steam helper path for persistence.
#[must_use]
pub fn capture_steam_helper_config(helper_path: impl Into<String>) -> SteamHelperConfig {
    capture_steam_helper_config_with_backend(helper_path, None::<String>)
}

/// Captures the selected Steam helper path and backend for persistence.
#[must_use]
pub fn capture_steam_helper_config_with_backend(
    helper_path: impl Into<String>,
    backend: Option<impl Into<String>>,
) -> SteamHelperConfig {
    SteamHelperConfig {
        version: CONFIG_VERSION,
        helper_path: helper_path.into(),
        backend: backend
            .map(Into::into)
            .map(|backend| backend.trim().to_ascii_lowercase())
            .filter(|backend| !backend.is_empty()),
    }
}

/// Reads persisted game-folder state from JSON.
///
/// # Errors
///
/// Returns [`CoreError`] when the file cannot be read or decoded.
pub fn read_game_folder_config(path: impl AsRef<Path>) -> CoreResult<GameFolderConfig> {
    let path = path.as_ref();
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|error| {
        CoreError::parse(format!(
            "failed to parse game-folder config {}: {error}",
            path.display()
        ))
    })
}

/// Reads persisted Steam helper state from JSON.
///
/// # Errors
///
/// Returns [`CoreError`] when the file cannot be read or decoded.
pub fn read_steam_helper_config(path: impl AsRef<Path>) -> CoreResult<SteamHelperConfig> {
    let path = path.as_ref();
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|error| {
        CoreError::parse(format!(
            "failed to parse Steam helper config {}: {error}",
            path.display()
        ))
    })
}

/// Writes persisted game-folder state through a temp-file rename.
///
/// # Errors
///
/// Returns [`CoreError`] when the config cannot be serialized or written.
pub fn write_game_folder_config_atomic(
    path: impl AsRef<Path>,
    config: &GameFolderConfig,
) -> CoreResult<()> {
    let path = path.as_ref();
    if config.game_dir.trim().is_empty() {
        return Err(CoreError::invalid_input("game_dir is required"));
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let bytes = serde_json::to_vec_pretty(config).map_err(|error| {
        CoreError::parse(format!("failed to encode game-folder config: {error}"))
    })?;
    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, bytes)?;
    fs::rename(&temp_path, path)?;
    Ok(())
}

/// Writes persisted Steam helper state through a temp-file rename.
///
/// # Errors
///
/// Returns [`CoreError`] when the config cannot be serialized or written.
pub fn write_steam_helper_config_atomic(
    path: impl AsRef<Path>,
    config: &SteamHelperConfig,
) -> CoreResult<()> {
    let path = path.as_ref();
    if config.helper_path.trim().is_empty() {
        return Err(CoreError::invalid_input("helper_path is required"));
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let bytes = serde_json::to_vec_pretty(config).map_err(|error| {
        CoreError::parse(format!("failed to encode Steam helper config: {error}"))
    })?;
    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, bytes)?;
    fs::rename(&temp_path, path)?;
    Ok(())
}

fn find_saved_state<'a>(
    config: &'a ModListConfig,
    identity: &ModIdentity,
) -> Option<&'a PersistedModState> {
    config
        .mods
        .iter()
        .find(|saved| saved.identity().matches(identity))
}

fn parse_mod_list_pack_name_line(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let after_mod = line.strip_prefix("mod")?;
    let mut chars = after_mod.chars();
    if !chars.next().is_some_and(char::is_whitespace) {
        return None;
    }

    let quoted = after_mod.trim_start().strip_prefix('"')?;
    let end_quote = quoted.find('"')?;
    let after_quote = quoted[end_quote + 1..].trim_start();
    if !after_quote.starts_with(';') {
        return None;
    }

    let pack_name = quoted[..end_quote].trim();
    (!pack_name.is_empty()).then_some(pack_name)
}

fn mod_record_pack_name_key(mod_record: &ModRecord) -> Option<String> {
    let path_pack_name = mod_record
        .identity
        .path
        .rsplit(['\\', '/'])
        .next()
        .filter(|file_name| !file_name.is_empty());
    path_pack_name
        .or_else(|| {
            (!mod_record.identity.name.trim().is_empty())
                .then_some(mod_record.identity.name.as_str())
        })
        .map(normalize_pack_name)
}

fn normalize_pack_name(pack_name: &str) -> String {
    pack_name.trim().to_ascii_lowercase()
}

fn normalize_category(category: impl Into<String>) -> CoreResult<String> {
    let category = category.into().trim().to_string();
    if category.is_empty() {
        return Err(CoreError::invalid_input("category name is required"));
    }
    Ok(category)
}

fn ensure_category(categories: &mut Vec<String>, category: &str) {
    if !categories.iter().any(|saved| saved == category) {
        categories.push(category.to_string());
    }
}

fn replace_category_label(categories: &mut Vec<String>, old_category: &str, new_category: &str) {
    let mut replaced = false;
    for category in categories.iter_mut() {
        if category == old_category {
            *category = new_category.to_string();
            replaced = true;
        }
    }
    if replaced {
        dedupe_categories(categories);
    }
}

fn dedupe_categories(categories: &mut Vec<String>) {
    let mut deduped = Vec::with_capacity(categories.len());
    for category in categories.iter() {
        if !category.trim().is_empty() && !deduped.iter().any(|saved| saved == category) {
            deduped.push(category.clone());
        }
    }
    *categories = deduped;
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::domain::{ModIdentity, ModRecord, merged_source_path_tag};

    use super::{
        ModListConfig, ModUserConfig, PersistedModState, PersistedModUserState, PresetConfig,
        add_category_config, add_mod_category, apply_mod_list_config, apply_mod_list_pack_names,
        apply_mod_user_config, apply_preset_config, capture_game_folder_config,
        capture_mod_list_config, capture_mod_user_config, capture_preset_config,
        capture_steam_helper_config, capture_steam_helper_config_with_backend,
        delete_category_config, delete_preset_config, parse_mod_list_pack_names, preset_names,
        read_game_folder_config, read_mod_list_config, read_mod_user_config, read_preset_config,
        read_steam_helper_config, remove_mod_category, rename_category_config,
        set_category_color_config, upsert_preset_config, write_game_folder_config_atomic,
        write_mod_list_config_atomic, write_mod_user_config_atomic, write_preset_config_atomic,
        write_steam_helper_config_atomic,
    };

    static TEST_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn captures_enablement_and_order() {
        let mut a = mod_record("a.pack", None, "A", true);
        a.tags
            .push(merged_source_path_tag(r"C:\mods\source.pack").unwrap());
        let mods = vec![a, mod_record("b.pack", Some("22"), "B", false)];

        let config = capture_mod_list_config(&mods);

        assert_eq!(config.version, 1);
        assert_eq!(config.mods[0].path, "a.pack");
        assert!(config.mods[0].enabled);
        assert_eq!(config.mods[0].merged_source_paths, [r"C:\mods\source.pack"]);
        assert_eq!(config.mods[1].workshop_id.as_deref(), Some("22"));
        assert_eq!(config.mods[1].order, 1);
    }

    #[test]
    fn applies_enablement_and_saved_order_to_discovered_mods() {
        let discovered = vec![
            mod_record("a.pack", None, "A", false),
            mod_record("b.pack", Some("22"), "B", false),
            mod_record("c.pack", None, "C", false),
        ];
        let config = ModListConfig {
            version: 1,
            mods: vec![
                saved("b.pack", Some("22"), "B", true, 0),
                saved("a.pack", None, "A", false, 1),
            ],
        };

        let merged = apply_mod_list_config(discovered, &config);

        assert_eq!(merged[0].identity.path, "b.pack");
        assert!(merged[0].enabled);
        assert_eq!(merged[1].identity.path, "a.pack");
        assert!(!merged[1].enabled);
        assert_eq!(merged[2].identity.path, "c.pack");
    }

    #[test]
    fn applies_saved_merged_source_paths_as_launch_tags() {
        let discovered = vec![mod_record("merged.pack", None, "Merged", false)];
        let mut saved_mod = saved("merged.pack", None, "Merged", true, 0);
        saved_mod.merged_source_paths = vec![r"C:\mods\a.pack".to_string()];
        let config = ModListConfig {
            version: 1,
            mods: vec![saved_mod],
        };

        let merged = apply_mod_list_config(discovered, &config);

        assert!(merged[0].enabled);
        assert_eq!(
            merged[0].merged_source_paths().collect::<Vec<_>>(),
            [r"C:\mods\a.pack"]
        );
    }

    #[test]
    fn matches_saved_workshop_id_when_path_is_missing() {
        let discovered = vec![mod_record("", Some("22"), "B", false)];
        let config = ModListConfig {
            version: 1,
            mods: vec![saved("", Some("22"), "old name", true, 0)],
        };

        let merged = apply_mod_list_config(discovered, &config);

        assert!(merged[0].enabled);
    }

    #[test]
    fn parses_pack_names_from_launch_mod_list_contents() {
        let contents = concat!(
            "add_working_directory \"C:\\mods\";\n",
            "mod \"z.pack\";\r\n",
            "  mod \"A.PACK\";\n",
            "mod \"missing-semicolon.pack\"\n",
            "mod \"\";\n"
        );

        assert_eq!(
            parse_mod_list_pack_names(contents),
            ["z.pack".to_string(), "A.PACK".to_string()]
        );
    }

    #[test]
    fn applies_launch_mod_list_pack_names_to_enablement_and_order() {
        let discovered = vec![
            mod_record(r"C:\game\data\a.pack", None, "A", false),
            mod_record(r"C:\game\data\b.pack", None, "B", false),
            mod_record(r"C:\game\data\c.pack", None, "C", true),
        ];
        let pack_names = vec!["B.PACK".to_string(), "a.pack".to_string()];

        let merged = apply_mod_list_pack_names(discovered, &pack_names);

        assert_eq!(merged[0].identity.path, r"C:\game\data\b.pack");
        assert!(merged[0].enabled);
        assert_eq!(merged[1].identity.path, r"C:\game\data\a.pack");
        assert!(merged[1].enabled);
        assert_eq!(merged[2].identity.path, r"C:\game\data\c.pack");
        assert!(!merged[2].enabled);
    }

    #[test]
    fn writes_reads_and_refuses_empty_overwrite() {
        let path = temp_config_path("write-read");
        let config = ModListConfig {
            version: 1,
            mods: vec![saved("a.pack", None, "A", true, 0)],
        };

        assert!(write_mod_list_config_atomic(&path, &config).unwrap());
        assert_eq!(read_mod_list_config(&path).unwrap(), config);

        let empty = ModListConfig {
            version: 1,
            mods: Vec::new(),
        };
        assert!(!write_mod_list_config_atomic(&path, &empty).unwrap());
        assert_eq!(read_mod_list_config(&path).unwrap(), config);

        fs::remove_file(path).ok();
    }

    #[test]
    fn captures_upserts_and_applies_named_presets() {
        let mods = vec![
            mod_record("a.pack", None, "A", true),
            mod_record("b.pack", Some("22"), "B", false),
        ];
        let mut config = capture_preset_config("Campaign", &mods).unwrap();
        let replacement = vec![
            mod_record("b.pack", Some("22"), "B", true),
            mod_record("a.pack", None, "A", false),
        ];

        upsert_preset_config(&mut config, "Campaign", &replacement).unwrap();

        assert_eq!(config.active_preset.as_deref(), Some("Campaign"));
        assert_eq!(config.presets.len(), 1);
        assert_eq!(config.presets[0].mods[0].path, "b.pack");
        let discovered = vec![
            mod_record("a.pack", None, "A", true),
            mod_record("b.pack", Some("22"), "B", false),
            mod_record("c.pack", None, "C", false),
        ];
        let applied = apply_preset_config(discovered, &config, "Campaign").unwrap();

        assert_eq!(applied[0].identity.path, "b.pack");
        assert!(applied[0].enabled);
        assert_eq!(applied[1].identity.path, "a.pack");
        assert!(!applied[1].enabled);
        assert_eq!(applied[2].identity.path, "c.pack");
    }

    #[test]
    fn lists_and_deletes_named_presets() {
        let mut config =
            capture_preset_config("Campaign", &[mod_record("a.pack", None, "A", true)]).unwrap();
        upsert_preset_config(
            &mut config,
            "Battle",
            &[mod_record("b.pack", None, "B", false)],
        )
        .unwrap();

        assert_eq!(preset_names(&config), ["Campaign", "Battle"]);

        delete_preset_config(&mut config, "Battle").unwrap();

        assert_eq!(preset_names(&config), ["Campaign"]);
        assert_eq!(
            delete_preset_config(&mut config, "Missing")
                .unwrap_err()
                .kind,
            crate::ports::CoreErrorKind::InvalidInput
        );
    }

    #[test]
    fn captures_and_applies_mod_user_config() {
        let mut category_colors = BTreeMap::new();
        category_colors.insert("Core".to_string(), "blue".to_string());
        let mut mods = vec![
            mod_record("a.pack", None, "A", true),
            mod_record("b.pack", Some("22"), "B", false),
        ];
        mods[0].categories = vec!["Core".to_string()];
        mods[0].always_enabled = true;
        mods[1].categories = vec!["Optional".to_string()];
        mods[1].hidden = true;

        let config = capture_mod_user_config(&mods, &["Core".to_string()], &category_colors);

        assert_eq!(config.categories, ["Core", "Optional"]);
        assert_eq!(
            config.category_colors.get("Core").map(String::as_str),
            Some("blue")
        );
        let discovered = vec![
            mod_record("a.pack", None, "A", false),
            mod_record("", Some("22"), "B renamed", false),
        ];
        let applied = apply_mod_user_config(discovered, &config);

        assert_eq!(applied[0].categories, ["Core"]);
        assert!(applied[0].always_enabled);
        assert_eq!(applied[1].categories, ["Optional"]);
        assert!(applied[1].hidden);
    }

    #[test]
    fn writes_reads_and_refuses_empty_mod_user_overwrite() {
        let path = temp_config_path("mod-user-write-read");
        let mut mods = vec![mod_record("a.pack", None, "A", true)];
        mods[0].categories = vec!["Core".to_string()];
        let config = capture_mod_user_config(&mods, &["Core".to_string()], &BTreeMap::new());

        assert!(write_mod_user_config_atomic(&path, &config).unwrap());
        assert_eq!(read_mod_user_config(&path).unwrap(), config);

        let empty = capture_mod_user_config(&[], &[], &BTreeMap::new());
        assert!(!write_mod_user_config_atomic(&path, &empty).unwrap());
        assert_eq!(read_mod_user_config(&path).unwrap(), config);

        fs::remove_file(path).ok();
    }

    #[test]
    fn edits_categories_without_duplicate_assignments() {
        let mut config = ModUserConfig {
            version: 1,
            categories: vec!["Core".to_string(), "Gameplay".to_string()],
            category_colors: BTreeMap::from([
                ("Core".to_string(), "blue".to_string()),
                ("Gameplay".to_string(), "green".to_string()),
            ]),
            mods: vec![PersistedModUserState {
                path: "a.pack".to_string(),
                workshop_id: None,
                name: "A".to_string(),
                categories: vec!["Core".to_string(), "Gameplay".to_string()],
                hidden: false,
                always_enabled: false,
            }],
        };

        add_category_config(&mut config, "Core").unwrap();
        rename_category_config(&mut config, "Core", "Gameplay").unwrap();

        assert_eq!(config.categories, ["Gameplay"]);
        assert_eq!(config.mods[0].categories, ["Gameplay"]);
        assert_eq!(
            config.category_colors.get("Gameplay").map(String::as_str),
            Some("green")
        );

        delete_category_config(&mut config, "Gameplay").unwrap();

        assert!(config.categories.is_empty());
        assert!(config.mods[0].categories.is_empty());
        assert!(config.category_colors.is_empty());
    }

    #[test]
    fn sets_category_color_and_rejects_empty_category_inputs() {
        let mut config = ModUserConfig {
            version: 1,
            categories: Vec::new(),
            category_colors: BTreeMap::new(),
            mods: Vec::new(),
        };

        set_category_color_config(&mut config, " Core ", " blue ").unwrap();

        assert_eq!(config.categories, ["Core"]);
        assert_eq!(
            config.category_colors.get("Core").map(String::as_str),
            Some("blue")
        );
        assert_eq!(
            add_category_config(&mut config, "  ").unwrap_err().kind,
            crate::ports::CoreErrorKind::InvalidInput
        );
        assert_eq!(
            set_category_color_config(&mut config, "Core", "  ")
                .unwrap_err()
                .kind,
            crate::ports::CoreErrorKind::InvalidInput
        );
    }

    #[test]
    fn adds_and_removes_mod_category_assignments() {
        let mut mod_record = mod_record("a.pack", None, "A", true);

        add_mod_category(&mut mod_record, " Core ").unwrap();
        add_mod_category(&mut mod_record, "Core").unwrap();

        assert_eq!(mod_record.categories, ["Core"]);
        assert!(remove_mod_category(&mut mod_record, "Core").unwrap());
        assert!(!remove_mod_category(&mut mod_record, "Core").unwrap());
        assert_eq!(
            remove_mod_category(&mut mod_record, "  ").unwrap_err().kind,
            crate::ports::CoreErrorKind::InvalidInput
        );
    }

    #[test]
    fn writes_reads_and_refuses_empty_preset_overwrite() {
        let path = temp_config_path("preset-write-read");
        let config =
            capture_preset_config("Campaign", &[mod_record("a.pack", None, "A", true)]).unwrap();

        assert!(write_preset_config_atomic(&path, &config).unwrap());
        assert_eq!(read_preset_config(&path).unwrap(), config);

        let empty = PresetConfig {
            version: 1,
            active_preset: None,
            presets: Vec::new(),
        };
        assert!(!write_preset_config_atomic(&path, &empty).unwrap());
        assert_eq!(read_preset_config(&path).unwrap(), config);

        fs::remove_file(path).ok();
    }

    #[test]
    fn writes_reads_and_rejects_empty_game_folder_config() {
        let path = temp_config_path("game-folder");
        let config = capture_game_folder_config("C:\\Games\\Total War WARHAMMER III");

        write_game_folder_config_atomic(&path, &config).unwrap();

        assert_eq!(read_game_folder_config(&path).unwrap(), config);
        assert_eq!(
            write_game_folder_config_atomic(&path, &capture_game_folder_config(""))
                .unwrap_err()
                .kind,
            crate::ports::CoreErrorKind::InvalidInput
        );

        fs::remove_file(path).ok();
    }

    #[test]
    fn writes_reads_and_rejects_empty_steam_helper_config() {
        let path = temp_config_path("steam-helper");
        let config = capture_steam_helper_config_with_backend(
            "C:\\Tools\\wh3mm-steam-helper.exe",
            Some(" Native "),
        );

        write_steam_helper_config_atomic(&path, &config).unwrap();

        assert_eq!(read_steam_helper_config(&path).unwrap(), config);
        assert_eq!(config.backend.as_deref(), Some("native"));
        assert_eq!(
            write_steam_helper_config_atomic(&path, &capture_steam_helper_config("  "))
                .unwrap_err()
                .kind,
            crate::ports::CoreErrorKind::InvalidInput
        );

        fs::remove_file(path).ok();
    }

    fn mod_record(path: &str, workshop_id: Option<&str>, name: &str, enabled: bool) -> ModRecord {
        ModRecord {
            identity: ModIdentity::new(path, workshop_id, name),
            display_name: name.to_string(),
            enabled,
            always_enabled: false,
            hidden: false,
            categories: Vec::new(),
            tags: Vec::new(),
        }
    }

    fn saved(
        path: &str,
        workshop_id: Option<&str>,
        name: &str,
        enabled: bool,
        order: usize,
    ) -> PersistedModState {
        PersistedModState {
            path: path.to_string(),
            workshop_id: workshop_id.map(str::to_string),
            name: name.to_string(),
            enabled,
            order,
            merged_source_paths: Vec::new(),
        }
    }

    fn temp_config_path(test_name: &str) -> std::path::PathBuf {
        let counter = TEST_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "wh3mm-core-persistence-{test_name}-{}-{counter}.json",
            std::process::id()
        ));
        path
    }
}
