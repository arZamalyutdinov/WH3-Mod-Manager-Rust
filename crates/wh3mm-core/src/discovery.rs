//! Filesystem mod discovery.
//!
//! This module is intentionally about inventory only. It does not contact
//! Steam, mutate load order, parse full pack contents, or launch the game.

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

use crate::domain::{ModIdentity, ModRecord, merged_source_path_tag};
use crate::ports::{CoreError, CoreResult};

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
        mods.extend(discover_pack_files_in_dir(extra_dir, None, "local")?);
    }

    remove_data_and_content_duplicates(&mut mods);
    remove_data_packs_shadowed_by_modding(&mut mods);

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
    let mut mods = discover_pack_files_in_dir(data_dir, None, "data")?;
    let modding_dir = data_dir.join("modding");
    if modding_dir.exists() {
        mods.extend(discover_pack_files_in_dir(
            &modding_dir,
            None,
            "data-modding",
        )?);
    }

    if !include_vanilla_packs {
        mods.retain(|mod_record| !is_likely_vanilla_pack(&mod_record.identity.path));
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
            "workshop",
        )?);
    }

    Ok(mods)
}

fn discover_pack_files_in_dir(
    dir: &Path,
    workshop_id: Option<&str>,
    source_tag: &str,
) -> CoreResult<Vec<ModRecord>> {
    let mut mods = Vec::new();
    for entry in sorted_dir_entries(dir)? {
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
        let mut tags = vec![source_tag.to_string()];
        if workshop_id.is_some() {
            tags.push("steam".to_string());
        }
        tags.extend(merged_source_path_tags_for_pack(&path));

        mods.push(ModRecord {
            identity: ModIdentity::new(path_label, workshop_id, display_name.clone()),
            display_name,
            enabled: false,
            always_enabled: false,
            hidden: false,
            categories: Vec::new(),
            tags,
        });
    }

    Ok(mods)
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

fn remove_data_and_content_duplicates(mods: &mut Vec<ModRecord>) {
    let data_pack_names = mods
        .iter()
        .filter(|mod_record| is_data_source(mod_record))
        .filter_map(pack_file_name_key)
        .collect::<std::collections::BTreeSet<_>>();

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
    is_plain_data_source(mod_record) || is_data_modding_source(mod_record)
}

fn is_plain_data_source(mod_record: &ModRecord) -> bool {
    mod_record.tags.iter().any(|tag| tag == "data")
}

fn is_data_modding_source(mod_record: &ModRecord) -> bool {
    mod_record.tags.iter().any(|tag| tag == "data-modding")
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

fn is_likely_vanilla_pack(path: &str) -> bool {
    let Some(file_name) = Path::new(path).file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let file_name = file_name.to_ascii_lowercase();
    matches!(
        file_name.as_str(),
        "boot.pack"
            | "data.pack"
            | "local_en.pack"
            | "models.pack"
            | "movies.pack"
            | "shaders.pack"
            | "terrain.pack"
            | "tiles.pack"
            | "variants.pack"
    ) || file_name.starts_with("audio_")
        || file_name.starts_with("local_")
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::domain::merged_source_path_tag;

    use super::{ModDiscoveryOptions, discover_mods};

    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

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
        assert!(mods.iter().any(|mod_record| {
            mod_record.identity.path.ends_with("data/shared.pack") && mod_record.tags == ["data"]
        }));
        assert!(mods.iter().any(|mod_record| {
            mod_record.identity.path.ends_with("workshop_only.pack")
                && mod_record.tags == ["workshop", "steam"]
        }));
        assert!(!mods.iter().any(|mod_record| {
            mod_record.identity.path.ends_with("123456789/shared.pack")
                || mod_record.identity.path.ends_with("extra/shared.pack")
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
            mod_record
                .identity
                .path
                .ends_with("modding/override_shared.pack")
                && mod_record.tags == ["data-modding"]
        }));
        assert!(mods.iter().any(|mod_record| {
            mod_record.identity.path.ends_with("data/data_only.pack") && mod_record.tags == ["data"]
        }));
        assert!(!mods.iter().any(|mod_record| {
            mod_record
                .identity
                .path
                .ends_with("data/override_shared.pack")
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
