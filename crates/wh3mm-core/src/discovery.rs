//! Filesystem mod discovery.
//!
//! This module is intentionally about inventory only. It does not contact
//! Steam, mutate load order, parse full pack contents, or launch the game.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::domain::{ModIdentity, ModRecord, ModSource, merged_source_path_tag};
use crate::ports::{CoreError, CoreResult};

// Mirrors the WH3 fallback manifest in the TypeScript reference. The live
// `data/manifest.txt` remains authoritative; this list is used only when that
// file is absent, unreadable, or contains no pack entries.
const WH3_VANILLA_PACK_NAMES: &[&str] = &[
    "audio_base.pack",
    "audio_base_2.pack",
    "audio_base_bl.pack",
    "audio_base_m.pack",
    "audio_cn.pack",
    "audio_cn_2.pack",
    "audio_cn_bm.pack",
    "audio_cn_br.pack",
    "audio_cn_cst.pack",
    "audio_cn_lu.pack",
    "audio_cn_lu2.pack",
    "audio_cn_sc.pack",
    "audio_cn_tk.pack",
    "audio_cn_we.pack",
    "audio_en.pack",
    "audio_en_2.pack",
    "audio_en_bm.pack",
    "audio_en_br.pack",
    "audio_en_cst.pack",
    "audio_en_lu.pack",
    "audio_en_lu2.pack",
    "audio_en_pw.pack",
    "audio_en_sc.pack",
    "audio_en_tk.pack",
    "audio_en_we.pack",
    "audio_fr.pack",
    "audio_fr_2.pack",
    "audio_fr_bm.pack",
    "audio_fr_br.pack",
    "audio_fr_cst.pack",
    "audio_fr_lu.pack",
    "audio_fr_lu2.pack",
    "audio_fr_sc.pack",
    "audio_fr_tk.pack",
    "audio_fr_we.pack",
    "audio_ge.pack",
    "audio_ge_2.pack",
    "audio_ge_bm.pack",
    "audio_ge_br.pack",
    "audio_ge_cst.pack",
    "audio_ge_lu.pack",
    "audio_ge_lu2.pack",
    "audio_ge_sc.pack",
    "audio_ge_tk.pack",
    "audio_ge_we.pack",
    "audio_it.pack",
    "audio_it_2.pack",
    "audio_it_bm.pack",
    "audio_it_br.pack",
    "audio_it_cst.pack",
    "audio_it_lu.pack",
    "audio_it_lu2.pack",
    "audio_it_sc.pack",
    "audio_it_tk.pack",
    "audio_it_we.pack",
    "audio_pl.pack",
    "audio_pl_2.pack",
    "audio_pl_bm.pack",
    "audio_pl_br.pack",
    "audio_pl_cst.pack",
    "audio_pl_lu.pack",
    "audio_pl_lu2.pack",
    "audio_pl_sc.pack",
    "audio_pl_tk.pack",
    "audio_pl_we.pack",
    "audio_ru.pack",
    "audio_ru_2.pack",
    "audio_ru_bm.pack",
    "audio_ru_br.pack",
    "audio_ru_cst.pack",
    "audio_ru_lu.pack",
    "audio_ru_lu2.pack",
    "audio_ru_sc.pack",
    "audio_ru_tk.pack",
    "audio_ru_we.pack",
    "audio_sp.pack",
    "audio_sp_2.pack",
    "audio_sp_bm.pack",
    "audio_sp_br.pack",
    "audio_sp_cst.pack",
    "audio_sp_lu.pack",
    "audio_sp_lu2.pack",
    "audio_sp_sc.pack",
    "audio_sp_tk.pack",
    "audio_sp_we.pack",
    "boot.pack",
    "data.pack",
    "data_1.pack",
    "data_2.pack",
    "data_3.pack",
    "data_bl.pack",
    "data_bm.pack",
    "data_tk.pack",
    "data_we.pack",
    "data_wp_.pack",
    "data_script.pack",
    "data_script_3.pack",
    "db.pack",
    "local_br.pack",
    "local_br_3.pack",
    "local_cn.pack",
    "local_cn_3.pack",
    "local_cz.pack",
    "local_cz_3.pack",
    "local_en.pack",
    "local_en_3.pack",
    "local_fr.pack",
    "local_fr_3.pack",
    "local_ge.pack",
    "local_ge_3.pack",
    "local_it.pack",
    "local_it_3.pack",
    "local_kr.pack",
    "local_kr_3.pack",
    "local_pl.pack",
    "local_pl_3.pack",
    "local_ru.pack",
    "local_ru_3.pack",
    "local_sp.pack",
    "local_sp_3.pack",
    "local_tr.pack",
    "local_tr_3.pack",
    "local_zh.pack",
    "local_zh_3.pack",
    "models.pack",
    "models_2.pack",
    "models_3.pack",
    "models2.pack",
    "models2_2.pack",
    "models2_3.pack",
    "movies.pack",
    "movies_3.pack",
    "shaders.pack",
    "shaders_bl.pack",
    "terrain.pack",
    "terrain_2.pack",
    "terrain_3.pack",
    "terrain10.pack",
    "terrain10_2.pack",
    "terrain10_3.pack",
    "terrain11.pack",
    "terrain11_2.pack",
    "terrain13.pack",
    "terrain13_3.pack",
    "terrain2.pack",
    "terrain2_2.pack",
    "terrain2_3.pack",
    "terrain3.pack",
    "terrain3_2.pack",
    "terrain3_3.pack",
    "terrain4.pack",
    "terrain5.pack",
    "terrain5_3.pack",
    "terrain6.pack",
    "terrain6_3.pack",
    "terrain7.pack",
    "terrain7_3.pack",
    "terrain8.pack",
    "terrain9.pack",
    "terrain9_3.pack",
    "variants.pack",
    "variants_2.pack",
    "variants_3.pack",
    "variants_bl.pack",
    "variants_dds.pack",
    "variants_dds_3.pack",
    "variants_dds_bl.pack",
    "variants_dds2.pack",
    "variants_dds2_3.pack",
    "warmachines.pack",
    "warmachines_3.pack",
];

/// Options for discovering `.pack` mods from local folders.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModDiscoveryOptions {
    /// Game `data` directory. Direct `.pack` files are scanned here.
    pub data_dir: Option<PathBuf>,
    /// Steam workshop content directory, usually `workshop/content/1142710`.
    pub workshop_content_dir: Option<PathBuf>,
    /// Additional directories containing local `.pack` files.
    pub extra_mod_dirs: Vec<PathBuf>,
    /// Whether likely vanilla/base game packs should be included.
    pub include_vanilla_packs: bool,
}

/// Discovers mods from configured local folders.
///
/// # Errors
///
/// Returns [`CoreError`] when a configured directory cannot be read.
pub fn discover_mods(options: &ModDiscoveryOptions) -> CoreResult<Vec<ModRecord>> {
    let mut mods = Vec::new();

    if let Some(data_dir) = &options.data_dir {
        mods.extend(discover_data_mods(data_dir, options.include_vanilla_packs)?);
    }

    if let Some(workshop_content_dir) = &options.workshop_content_dir {
        mods.extend(discover_workshop_mods(workshop_content_dir)?);
    }

    for extra_dir in &options.extra_mod_dirs {
        mods.extend(discover_pack_files_in_dir(
            extra_dir,
            None,
            ModSource::Local,
        )?);
    }

    remove_data_packs_shadowed_by_modding(&mut mods);
    merge_data_and_content_duplicates(&mut mods);

    mods.sort_by(|left, right| {
        compare_ts_mod_names(
            pack_file_name_for_sort(left),
            pack_file_name_for_sort(right),
        )
        .then_with(|| left.identity.path.cmp(&right.identity.path))
    });
    Ok(mods)
}

fn pack_file_name_for_sort(mod_record: &ModRecord) -> &str {
    mod_record
        .identity
        .path
        .rsplit(['\\', '/'])
        .next()
        .filter(|file_name| !file_name.is_empty())
        .unwrap_or(&mod_record.identity.name)
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

fn discover_data_mods(data_dir: &Path, include_vanilla_packs: bool) -> CoreResult<Vec<ModRecord>> {
    let mut mods = discover_pack_files_in_dir(data_dir, None, ModSource::GameData)?;
    let modding_dir = data_dir.join("modding");
    if modding_dir.exists() {
        mods.extend(discover_pack_files_in_dir(
            &modding_dir,
            None,
            ModSource::GameDataModding,
        )?);
    }

    if !include_vanilla_packs {
        let vanilla_pack_names = vanilla_pack_names(data_dir);
        mods.retain(|mod_record| {
            pack_file_name_key(mod_record)
                .is_none_or(|pack_name| !vanilla_pack_names.contains(&pack_name))
        });
    }

    Ok(mods)
}

fn discover_workshop_mods(workshop_content_dir: &Path) -> CoreResult<Vec<ModRecord>> {
    let mut mods = Vec::new();
    for entry in sorted_dir_entries(workshop_content_dir)? {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let workshop_id = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| name.chars().all(|character| character.is_ascii_digit()))
            .map(str::to_owned);
        mods.extend(discover_pack_files_in_dir(
            &path,
            workshop_id.as_deref(),
            ModSource::Workshop,
        )?);
    }

    Ok(mods)
}

fn discover_pack_files_in_dir(
    dir: &Path,
    workshop_id: Option<&str>,
    source: ModSource,
) -> CoreResult<Vec<ModRecord>> {
    let mut mods = Vec::new();
    let entries = sorted_dir_entries(dir)?;
    let thumbnails_by_stem = thumbnail_paths_by_stem(&entries);
    let workshop_thumbnail = (source == ModSource::Workshop)
        .then(|| first_thumbnail_path(&entries))
        .flatten();
    for entry in &entries {
        let path = entry.path();
        if !path.is_file() || !has_pack_extension(&path) {
            continue;
        }

        let display_name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown pack")
            .replace('_', " ");
        let path_label = path.display().to_string();
        let mut tags = vec![source_tag(source).to_string()];
        if workshop_id.is_some() {
            tags.push("steam".to_string());
        }
        tags.extend(merged_source_path_tags_for_pack(&path));
        let thumbnail_path = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| thumbnails_by_stem.get(&stem.to_ascii_lowercase()).cloned())
            .or_else(|| workshop_thumbnail.clone())
            .map(|path| path.display().to_string());

        mods.push(ModRecord {
            identity: ModIdentity::new(path_label, workshop_id, display_name.clone()),
            display_name,
            source,
            thumbnail_path,
            local_modified_ms: local_modified_ms(&path),
            enabled: false,
            always_enabled: false,
            hidden: false,
            categories: Vec::new(),
            tags,
        });
    }

    Ok(mods)
}

fn source_tag(source: ModSource) -> &'static str {
    match source {
        ModSource::GameData => "data",
        ModSource::GameDataModding => "data-modding",
        ModSource::Workshop => "workshop",
        ModSource::Local => "local",
    }
}

fn local_modified_ms(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

fn thumbnail_paths_by_stem(entries: &[fs::DirEntry]) -> BTreeMap<String, PathBuf> {
    let mut thumbnails = BTreeMap::new();
    for wanted_extension in ["jpg", "png"] {
        for entry in entries {
            let path = entry.path();
            let matches = path.is_file()
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case(wanted_extension));
            if !matches {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let key = stem.to_ascii_lowercase();
            if wanted_extension == "png" {
                thumbnails.insert(key, path);
            } else {
                thumbnails.entry(key).or_insert(path);
            }
        }
    }
    thumbnails
}

fn first_thumbnail_path(entries: &[fs::DirEntry]) -> Option<PathBuf> {
    ["png", "jpg"].into_iter().find_map(|wanted_extension| {
        entries.iter().find_map(|entry| {
            let path = entry.path();
            let matches = path.is_file()
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case(wanted_extension));
            matches.then_some(path)
        })
    })
}

fn merged_source_path_tags_for_pack(path: &Path) -> Vec<String> {
    let metadata_path = path.with_extension("json");
    let Ok(bytes) = fs::read(metadata_path) else {
        return Vec::new();
    };
    let Ok(metadata) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Vec::new();
    };
    let Some(entries) = metadata.as_array() else {
        return Vec::new();
    };

    let mut tags = Vec::new();
    for entry in entries {
        let Some(path) = entry.get("path").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(tag) = merged_source_path_tag(path) else {
            continue;
        };
        if !tags.iter().any(|existing| existing == &tag) {
            tags.push(tag);
        }
    }

    tags
}

fn merge_data_and_content_duplicates(mods: &mut Vec<ModRecord>) {
    let data_pack_names = mods
        .iter()
        .filter(|mod_record| is_data_source(mod_record))
        .filter_map(pack_file_name_key)
        .collect::<BTreeSet<_>>();

    let workshop_by_pack_name = mods
        .iter()
        .filter(|mod_record| mod_record.source == ModSource::Workshop)
        .filter_map(|mod_record| {
            pack_file_name_key(mod_record).map(|pack_name| (pack_name, mod_record.clone()))
        })
        .collect::<BTreeMap<_, _>>();

    for mod_record in mods.iter_mut().filter(|record| is_data_source(record)) {
        let Some(pack_name) = pack_file_name_key(mod_record) else {
            continue;
        };
        let Some(workshop_record) = workshop_by_pack_name.get(&pack_name) else {
            continue;
        };
        if mod_record.identity.workshop_id.is_none() {
            mod_record
                .identity
                .workshop_id
                .clone_from(&workshop_record.identity.workshop_id);
        }
        if mod_record.thumbnail_path.is_none() {
            mod_record
                .thumbnail_path
                .clone_from(&workshop_record.thumbnail_path);
        }
        if !mod_record.tags.iter().any(|tag| tag == "steam") {
            mod_record.tags.push("steam".to_string());
        }
    }

    mods.retain(|mod_record| {
        is_data_source(mod_record)
            || pack_file_name_key(mod_record)
                .is_none_or(|pack_name| !data_pack_names.contains(&pack_name))
    });
}

fn remove_data_packs_shadowed_by_modding(mods: &mut Vec<ModRecord>) {
    let modding_pack_names = mods
        .iter()
        .filter(|mod_record| is_data_modding_source(mod_record))
        .filter_map(pack_file_name_key)
        .collect::<std::collections::BTreeSet<_>>();

    mods.retain(|mod_record| {
        !is_plain_data_source(mod_record)
            || pack_file_name_key(mod_record)
                .is_none_or(|pack_name| !modding_pack_names.contains(&pack_name))
    });
}

fn is_data_source(mod_record: &ModRecord) -> bool {
    matches!(
        mod_record.source,
        ModSource::GameData | ModSource::GameDataModding
    )
}

fn is_plain_data_source(mod_record: &ModRecord) -> bool {
    mod_record.source == ModSource::GameData
}

fn is_data_modding_source(mod_record: &ModRecord) -> bool {
    mod_record.source == ModSource::GameDataModding
}

fn pack_file_name_key(mod_record: &ModRecord) -> Option<String> {
    Path::new(&mod_record.identity.path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_ascii_lowercase)
}

fn sorted_dir_entries(dir: &Path) -> CoreResult<Vec<fs::DirEntry>> {
    let mut entries = fs::read_dir(dir)
        .map_err(|error| CoreError::io(format!("failed to read {}: {error}", dir.display())))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| CoreError::io(format!("failed to read {}: {error}", dir.display())))?;
    entries.sort_by_key(fs::DirEntry::path);
    Ok(entries)
}

fn has_pack_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pack"))
}

fn vanilla_pack_names(data_dir: &Path) -> BTreeSet<String> {
    fs::read_to_string(data_dir.join("manifest.txt"))
        .ok()
        .map(|manifest| parse_manifest_pack_names(&manifest))
        .filter(|names| !names.is_empty())
        .unwrap_or_else(|| {
            WH3_VANILLA_PACK_NAMES
                .iter()
                .map(|name| (*name).to_string())
                .collect()
        })
}

fn parse_manifest_pack_names(manifest: &str) -> BTreeSet<String> {
    manifest
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::trim)
        .filter(|entry| entry.to_ascii_lowercase().ends_with(".pack"))
        .map(str::to_ascii_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::domain::merged_source_path_tag;

    use super::{ModDiscoveryOptions, discover_mods};

    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn path_ends_with(path: &str, suffix: &str) -> bool {
        path.replace('\\', "/").ends_with(suffix)
    }

    #[test]
    fn discovers_data_and_extra_pack_files() {
        let root = temp_root("data-extra");
        let data_dir = root.join("data");
        let extra_dir = root.join("extra");
        fs::create_dir_all(&data_dir).unwrap();
        fs::create_dir_all(&extra_dir).unwrap();
        touch(data_dir.join("data.pack"));
        touch(data_dir.join("zzz_local_mod.pack"));
        touch(data_dir.join("ignored.txt"));
        touch(extra_dir.join("extra_mod.PACK"));

        let mods = discover_mods(&ModDiscoveryOptions {
            data_dir: Some(data_dir),
            extra_mod_dirs: vec![extra_dir],
            ..ModDiscoveryOptions::default()
        })
        .unwrap();

        assert_eq!(mods.len(), 2);
        assert!(mods.iter().any(|mod_record| {
            mod_record.display_name == "zzz local mod" && mod_record.tags == ["data"]
        }));
        assert!(mods.iter().any(|mod_record| {
            mod_record.display_name == "extra mod" && mod_record.tags == ["local"]
        }));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn discovers_mods_in_ts_pack_name_order() {
        let root = temp_root("ts-pack-name-order");
        let extra_dir = root.join("extra");
        fs::create_dir_all(&extra_dir).unwrap();
        touch(extra_dir.join("a_b.pack"));
        touch(extra_dir.join("a-a.pack"));
        touch(extra_dir.join("a.pack"));

        let mods = discover_mods(&ModDiscoveryOptions {
            extra_mod_dirs: vec![extra_dir],
            ..ModDiscoveryOptions::default()
        })
        .unwrap();

        assert_eq!(pack_file_names(&mods), ["a-a.pack", "a.pack", "a_b.pack"]);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn discovers_workshop_pack_files_with_workshop_id() {
        let root = temp_root("workshop");
        let workshop_dir = root.join("workshop/content/1142710");
        let mod_dir = workshop_dir.join("123456789");
        fs::create_dir_all(&mod_dir).unwrap();
        touch(mod_dir.join("community_balance.pack"));

        let mods = discover_mods(&ModDiscoveryOptions {
            workshop_content_dir: Some(workshop_dir),
            ..ModDiscoveryOptions::default()
        })
        .unwrap();

        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].identity.workshop_id.as_deref(), Some("123456789"));
        assert_eq!(mods[0].tags, ["workshop", "steam"]);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn discovers_merged_pack_source_paths_from_sibling_json() {
        let root = temp_root("merged-json");
        let extra_dir = root.join("extra");
        fs::create_dir_all(&extra_dir).unwrap();
        touch(extra_dir.join("merged.pack"));
        fs::write(
            extra_dir.join("merged.json"),
            r#"[{"path":"C:\\mods\\a.pack"},{"path":"C:\\mods\\a.pack"},{"name":"missing"},{"path":""}]"#,
        )
        .unwrap();

        let mods = discover_mods(&ModDiscoveryOptions {
            extra_mod_dirs: vec![extra_dir],
            ..ModDiscoveryOptions::default()
        })
        .unwrap();

        let source_tag = merged_source_path_tag(r"C:\mods\a.pack").unwrap();
        assert_eq!(mods.len(), 1);
        assert!(mods[0].tags.iter().any(|tag| tag == "local"));
        assert_eq!(
            mods[0]
                .tags
                .iter()
                .filter(|tag| *tag == &source_tag)
                .count(),
            1
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn ignores_malformed_merged_pack_json() {
        let root = temp_root("malformed-merged-json");
        let extra_dir = root.join("extra");
        fs::create_dir_all(&extra_dir).unwrap();
        touch(extra_dir.join("merged.pack"));
        fs::write(extra_dir.join("merged.json"), b"{not-json").unwrap();

        let mods = discover_mods(&ModDiscoveryOptions {
            extra_mod_dirs: vec![extra_dir],
            ..ModDiscoveryOptions::default()
        })
        .unwrap();

        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].tags, ["local"]);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn data_pack_suppresses_same_named_workshop_and_local_pack() {
        let root = temp_root("data-shadows-content");
        let data_dir = root.join("data");
        let workshop_mod_dir = root.join("workshop/content/1142710/123456789");
        let extra_dir = root.join("extra");
        fs::create_dir_all(&data_dir).unwrap();
        fs::create_dir_all(&workshop_mod_dir).unwrap();
        fs::create_dir_all(&extra_dir).unwrap();
        touch(data_dir.join("shared.pack"));
        touch(workshop_mod_dir.join("shared.pack"));
        touch(workshop_mod_dir.join("preview.jpg"));
        touch(extra_dir.join("shared.pack"));
        touch(workshop_mod_dir.join("workshop_only.pack"));

        let mods = discover_mods(&ModDiscoveryOptions {
            data_dir: Some(data_dir),
            workshop_content_dir: Some(root.join("workshop/content/1142710")),
            extra_mod_dirs: vec![extra_dir],
            ..ModDiscoveryOptions::default()
        })
        .unwrap();

        assert_eq!(mods.len(), 2);
        let shared = mods
            .iter()
            .find(|mod_record| path_ends_with(&mod_record.identity.path, "data/shared.pack"))
            .unwrap();
        assert_eq!(shared.identity.workshop_id.as_deref(), Some("123456789"));
        assert_eq!(shared.tags, ["data", "steam"]);
        assert!(
            shared
                .thumbnail_path
                .as_deref()
                .is_some_and(|path| path_ends_with(path, "123456789/preview.jpg"))
        );
        assert!(mods.iter().any(|mod_record| {
            path_ends_with(&mod_record.identity.path, "workshop_only.pack")
                && mod_record.tags == ["workshop", "steam"]
        }));
        assert!(!mods.iter().any(|mod_record| {
            path_ends_with(&mod_record.identity.path, "123456789/shared.pack")
                || path_ends_with(&mod_record.identity.path, "extra/shared.pack")
        }));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn data_modding_pack_suppresses_same_named_data_pack() {
        let root = temp_root("modding-shadows-data");
        let data_dir = root.join("data");
        let modding_dir = data_dir.join("modding");
        fs::create_dir_all(&modding_dir).unwrap();
        touch(data_dir.join("override_shared.pack"));
        touch(modding_dir.join("override_shared.pack"));
        touch(data_dir.join("data_only.pack"));

        let mods = discover_mods(&ModDiscoveryOptions {
            data_dir: Some(data_dir),
            ..ModDiscoveryOptions::default()
        })
        .unwrap();

        assert_eq!(mods.len(), 2);
        assert!(mods.iter().any(|mod_record| {
            path_ends_with(&mod_record.identity.path, "modding/override_shared.pack")
                && mod_record.tags == ["data-modding"]
        }));
        assert!(mods.iter().any(|mod_record| {
            path_ends_with(&mod_record.identity.path, "data/data_only.pack")
                && mod_record.tags == ["data"]
        }));
        assert!(!mods.iter().any(|mod_record| {
            path_ends_with(&mod_record.identity.path, "data/override_shared.pack")
        }));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn can_include_vanilla_data_packs() {
        let root = temp_root("vanilla");
        let data_dir = root.join("data");
        fs::create_dir_all(&data_dir).unwrap();
        touch(data_dir.join("data.pack"));

        let excluded = discover_mods(&ModDiscoveryOptions {
            data_dir: Some(data_dir.clone()),
            ..ModDiscoveryOptions::default()
        })
        .unwrap();
        let included = discover_mods(&ModDiscoveryOptions {
            data_dir: Some(data_dir),
            include_vanilla_packs: true,
            ..ModDiscoveryOptions::default()
        })
        .unwrap();

        assert!(excluded.is_empty());
        assert_eq!(included.len(), 1);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn manifest_filters_exact_pack_names_case_insensitively() {
        let root = temp_root("manifest");
        let data_dir = root.join("data");
        fs::create_dir_all(&data_dir).unwrap();
        fs::write(
            data_dir.join("manifest.txt"),
            "DATA_CUSTOM.PACK 123\nlocal_en.pack 456\nnot-a-pack.bin\n",
        )
        .unwrap();
        touch(data_dir.join("data_custom.pack"));
        touch(data_dir.join("LOCAL_EN.PACK"));
        touch(data_dir.join("audio_user_overhaul.pack"));
        touch(data_dir.join("my_mod.pack"));

        let mods = discover_mods(&ModDiscoveryOptions {
            data_dir: Some(data_dir),
            ..ModDiscoveryOptions::default()
        })
        .unwrap();

        assert_eq!(
            pack_file_names(&mods),
            ["audio_user_overhaul.pack", "my_mod.pack"]
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn fallback_manifest_filters_ts_wh3_official_packs() {
        let root = temp_root("fallback-manifest");
        let data_dir = root.join("data");
        fs::create_dir_all(&data_dir).unwrap();
        touch(data_dir.join("data_bl.pack"));
        touch(data_dir.join("community.pack"));

        let mods = discover_mods(&ModDiscoveryOptions {
            data_dir: Some(data_dir),
            ..ModDiscoveryOptions::default()
        })
        .unwrap();

        assert_eq!(pack_file_names(&mods), ["community.pack"]);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn discovers_same_stem_thumbnail_and_local_timestamp() {
        let root = temp_root("thumbnail");
        let extra_dir = root.join("extra");
        fs::create_dir_all(&extra_dir).unwrap();
        touch(extra_dir.join("custom.pack"));
        touch(extra_dir.join("custom.jpg"));
        touch(extra_dir.join("custom.png"));

        let mods = discover_mods(&ModDiscoveryOptions {
            extra_mod_dirs: vec![extra_dir],
            ..ModDiscoveryOptions::default()
        })
        .unwrap();

        assert_eq!(mods.len(), 1);
        assert!(
            mods[0]
                .thumbnail_path
                .as_deref()
                .is_some_and(|path| path_ends_with(path, "custom.png"))
        );
        assert!(mods[0].local_modified_ms.is_some());
        fs::remove_dir_all(root).ok();
    }

    fn temp_root(test_name: &str) -> std::path::PathBuf {
        let counter = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "wh3mm-core-discovery-{test_name}-{}-{counter}",
            std::process::id()
        ));
        path
    }

    fn touch(path: impl AsRef<std::path::Path>) {
        File::create(path).unwrap();
    }

    fn pack_file_names(mods: &[crate::domain::ModRecord]) -> Vec<String> {
        mods.iter()
            .map(|mod_record| {
                std::path::Path::new(&mod_record.identity.path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap()
                    .to_string()
            })
            .collect()
    }
}
