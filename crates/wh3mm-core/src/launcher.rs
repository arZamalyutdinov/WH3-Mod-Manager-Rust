//! Windows-first game launch planning.
//!
//! This module keeps process spawning and filesystem mutation out of core. It
//! produces the mod-list contents and pre-launch operations that platform
//! adapters can execute.

use crate::domain::ModRecord;
use crate::ports::{CoreError, CoreResult};

/// Options needed to build a Windows launch plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowsLaunchOptions {
    /// Game install directory.
    pub game_dir: String,
    /// Game data directory.
    pub data_dir: String,
    /// Game executable name, such as `Warhammer3.exe`.
    pub process_name: String,
    /// Mod-list file name. WH3 normally uses `used_mods.txt`.
    pub mod_list_file_name: String,
    /// Optional save name to pass to campaign-load startup mode.
    pub save_name: Option<String>,
    /// Generated or adapter-provided pack groups appended after normal mods.
    pub extra_pack_groups: Vec<WindowsLaunchPackGroup>,
    /// Generated pack files runtime adapters should write before launch.
    pub pre_launch_pack_writes: Vec<PreLaunchPackWrite>,
    /// Source mod paths replaced by generated launch packs.
    pub replaced_pack_paths: Vec<String>,
}

/// Extra launch packs that should be appended after the normal enabled list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowsLaunchPackGroup {
    /// Directory that contains the generated pack files.
    pub working_dir: String,
    /// Pack file names to load from `working_dir`, in launch order.
    pub pack_names: Vec<String>,
}

/// Generated pack file that should be written before launching the game.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreLaunchPackWrite {
    /// Destination pack path.
    pub path: String,
    /// Complete pack file bytes.
    pub bytes: Vec<u8>,
    /// Packed file names contained in the generated pack, for preview/status.
    pub packed_file_names: Vec<String>,
}

impl WindowsLaunchPackGroup {
    /// Creates a generated-pack launch group.
    pub fn new<I, S>(working_dir: impl Into<String>, pack_names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            working_dir: working_dir.into(),
            pack_names: pack_names.into_iter().map(Into::into).collect(),
        }
    }
}

impl WindowsLaunchOptions {
    /// Creates WH3 defaults for a Windows launch plan.
    #[must_use]
    pub fn warhammer3(game_dir: impl Into<String>, data_dir: impl Into<String>) -> Self {
        Self {
            game_dir: game_dir.into(),
            data_dir: data_dir.into(),
            process_name: "Warhammer3.exe".to_string(),
            mod_list_file_name: "used_mods.txt".to_string(),
            save_name: None,
            extra_pack_groups: Vec::new(),
            pre_launch_pack_writes: Vec::new(),
            replaced_pack_paths: Vec::new(),
        }
    }
}

/// A pre-launch copy operation required before writing the mod list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreLaunchCopyOperation {
    /// Source pack path.
    pub from_path: String,
    /// Destination pack path inside the game data directory.
    pub to_path: String,
}

/// Core-owned Windows launch plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowsLaunchPlan {
    /// Name of the mod-list file to write in `game_dir`.
    pub mod_list_file_name: String,
    /// Contents of the mod-list file.
    pub mod_list_contents: String,
    /// Pack files from `data/modding` that should be copied into `data`.
    pub pre_launch_copies: Vec<PreLaunchCopyOperation>,
    /// Generated pack files that should be written before launch.
    pub pre_launch_pack_writes: Vec<PreLaunchPackWrite>,
    /// Process current directory.
    pub working_dir: String,
    /// Executable to run.
    pub executable: String,
    /// Arguments to pass to the executable.
    pub args: Vec<String>,
    /// Windows `cmd` preview matching the legacy TS launch shape.
    pub command_line_preview: String,
}

/// Builds a Windows-first launch plan from the current ordered mod list.
///
/// # Errors
///
/// Returns [`CoreError`] when required launch paths are missing or an enabled
/// mod does not have a usable pack path/file name.
pub fn plan_windows_launch(
    options: &WindowsLaunchOptions,
    mods: &[ModRecord],
) -> CoreResult<WindowsLaunchPlan> {
    validate_launch_options(options)?;

    let enabled_mod_candidates = mods
        .iter()
        .filter(|mod_record| mod_record.effectively_enabled())
        .collect::<Vec<_>>();
    let merged_source_paths = enabled_mod_candidates
        .iter()
        .flat_map(|mod_record| mod_record.merged_source_paths().map(str::to_string))
        .collect::<Vec<_>>();
    let enabled_mods = enabled_mod_candidates
        .into_iter()
        .filter(|mod_record| !is_merged_source_mod(mod_record, &merged_source_paths))
        .filter(|mod_record| !is_replaced_mod(mod_record, &options.replaced_pack_paths))
        .collect::<Vec<_>>();

    let mut mod_list_lines = Vec::new();
    for mod_record in &enabled_mods {
        let pack_path = normalized_non_empty_path(&mod_record.identity.path)?;
        if mod_record.tags.iter().any(|tag| tag == "data-modding") {
            continue;
        }

        let mod_dir = parent_dir(pack_path).ok_or_else(|| {
            CoreError::invalid_input(format!("mod path has no parent: {pack_path}"))
        })?;
        if !windows_path_eq(mod_dir, &options.data_dir) {
            mod_list_lines.push(format!("add_working_directory \"{mod_dir}\";"));
        }
    }

    let mut pre_launch_copies = Vec::new();
    for mod_record in &enabled_mods {
        let pack_path = normalized_non_empty_path(&mod_record.identity.path)?;
        let pack_name = file_name(pack_path).ok_or_else(|| {
            CoreError::invalid_input(format!("mod path has no file name: {pack_path}"))
        })?;

        if mod_record.tags.iter().any(|tag| tag == "data-modding") {
            pre_launch_copies.push(PreLaunchCopyOperation {
                from_path: pack_path.to_string(),
                to_path: join_path(&options.data_dir, pack_name),
            });
        }

        mod_list_lines.push(format!("mod \"{pack_name}\";"));
    }
    append_extra_pack_groups(&mut mod_list_lines, &options.extra_pack_groups)?;

    let mut args = Vec::new();
    if let Some(save_name) = options
        .save_name
        .as_ref()
        .filter(|save_name| !save_name.is_empty())
    {
        args.extend([
            "game_startup_mode".to_string(),
            "campaign_load".to_string(),
            save_name.clone(),
            ";".to_string(),
        ]);
    }
    args.push(format!("{};", options.mod_list_file_name));

    Ok(WindowsLaunchPlan {
        mod_list_file_name: options.mod_list_file_name.clone(),
        mod_list_contents: mod_list_lines.join("\n"),
        pre_launch_copies,
        pre_launch_pack_writes: options.pre_launch_pack_writes.clone(),
        working_dir: options.game_dir.clone(),
        executable: options.process_name.clone(),
        command_line_preview: windows_command_line_preview(options, &args),
        args,
    })
}

fn validate_launch_options(options: &WindowsLaunchOptions) -> CoreResult<()> {
    if options.game_dir.trim().is_empty() {
        return Err(CoreError::invalid_input("game_dir is required"));
    }
    if options.data_dir.trim().is_empty() {
        return Err(CoreError::invalid_input("data_dir is required"));
    }
    if options.process_name.trim().is_empty() {
        return Err(CoreError::invalid_input("process_name is required"));
    }
    if options.mod_list_file_name.trim().is_empty() {
        return Err(CoreError::invalid_input("mod_list_file_name is required"));
    }
    Ok(())
}

fn append_extra_pack_groups(
    mod_list_lines: &mut Vec<String>,
    groups: &[WindowsLaunchPackGroup],
) -> CoreResult<()> {
    for group in groups {
        if group.pack_names.is_empty() {
            continue;
        }

        let working_dir = group.working_dir.trim();
        if working_dir.is_empty() {
            return Err(CoreError::invalid_input(
                "extra launch pack working directory is required",
            ));
        }

        mod_list_lines.push(format!("add_working_directory \"{working_dir}\";"));
        for pack_name in &group.pack_names {
            let pack_name = pack_name.trim();
            if pack_name.is_empty() {
                return Err(CoreError::invalid_input(
                    "extra launch pack file name is required",
                ));
            }
            mod_list_lines.push(format!("mod \"{pack_name}\";"));
        }
    }

    Ok(())
}

fn is_merged_source_mod(mod_record: &ModRecord, merged_source_paths: &[String]) -> bool {
    let path = mod_record.identity.path.trim();
    !path.is_empty()
        && merged_source_paths
            .iter()
            .any(|source_path| windows_path_eq(path, source_path))
}

fn is_replaced_mod(mod_record: &ModRecord, replaced_pack_paths: &[String]) -> bool {
    let path = mod_record.identity.path.trim();
    !path.is_empty()
        && replaced_pack_paths
            .iter()
            .any(|replaced_path| windows_path_eq(path, replaced_path))
}

fn normalized_non_empty_path(path: &str) -> CoreResult<&str> {
    let path = path.trim();
    if path.is_empty() {
        Err(CoreError::invalid_input("enabled mod path is required"))
    } else {
        Ok(path)
    }
}

fn file_name(path: &str) -> Option<&str> {
    path.rsplit(['\\', '/'])
        .next()
        .filter(|file_name| !file_name.is_empty())
}

fn parent_dir(path: &str) -> Option<&str> {
    path.rfind(['\\', '/'])
        .map(|index| path[..index].trim_end_matches(['\\', '/']))
        .filter(|parent| !parent.is_empty())
}

fn join_path(dir: &str, file_name: &str) -> String {
    let separator = if dir.contains('\\') { "\\" } else { "/" };
    format!(
        "{}{}{}",
        dir.trim_end_matches(['\\', '/']),
        separator,
        file_name
    )
}

fn windows_path_eq(left: &str, right: &str) -> bool {
    normalize_path_key(left) == normalize_path_key(right)
}

fn normalize_path_key(path: &str) -> String {
    path.trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_ascii_lowercase()
}

fn windows_command_line_preview(options: &WindowsLaunchOptions, args: &[String]) -> String {
    let args = args
        .iter()
        .map(|arg| {
            if arg.contains(' ') {
                format!("\"{arg}\"")
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "start /d \"{}\" {} {}",
        options.game_dir, options.process_name, args
    )
}

#[cfg(test)]
mod tests {
    use super::{
        PreLaunchCopyOperation, WindowsLaunchOptions, WindowsLaunchPackGroup, plan_windows_launch,
    };
    use crate::domain::{ModIdentity, ModRecord, merged_source_path_tag};
    use crate::ports::CoreErrorKind;

    #[test]
    fn builds_windows_mod_list_and_launch_args_from_enabled_mods() {
        let options = WindowsLaunchOptions {
            save_name: Some("Karl Franz autosave".to_string()),
            ..WindowsLaunchOptions::warhammer3(
                r"C:\Steam\steamapps\common\Total War WARHAMMER III",
                r"C:\Steam\steamapps\common\Total War WARHAMMER III\data",
            )
        };
        let mods = vec![
            mod_record(
                r"C:\Steam\steamapps\workshop\content\1142710\111\a.pack",
                Some("111"),
                "A",
                true,
                &["workshop", "steam"],
            ),
            mod_record(
                r"C:\Steam\steamapps\common\Total War WARHAMMER III\data\b.pack",
                None,
                "B",
                true,
                &["data"],
            ),
            mod_record(
                r"C:\Steam\steamapps\workshop\content\1142710\333\c.pack",
                Some("333"),
                "C",
                false,
                &["workshop", "steam"],
            ),
        ];

        let plan = plan_windows_launch(&options, &mods).unwrap();

        assert_eq!(
            plan.mod_list_contents,
            concat!(
                r#"add_working_directory "C:\Steam\steamapps\workshop\content\1142710\111";"#,
                "\n",
                r#"mod "a.pack";"#,
                "\n",
                r#"mod "b.pack";"#
            )
        );
        assert_eq!(plan.working_dir, options.game_dir);
        assert_eq!(plan.executable, "Warhammer3.exe");
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
        assert_eq!(
            plan.command_line_preview,
            concat!(
                r#"start /d "C:\Steam\steamapps\common\Total War WARHAMMER III" "#,
                r#"Warhammer3.exe game_startup_mode campaign_load "Karl Franz autosave" ; used_mods.txt;"#
            )
        );
    }

    #[test]
    fn repeats_working_directories_like_ts_and_preserves_mod_order() {
        let options = WindowsLaunchOptions::warhammer3(r"C:\game", r"C:\game\data");
        let mods = vec![
            mod_record(r"C:\mods\shared\a.pack", None, "A", true, &["local"]),
            mod_record(r"C:\mods\shared\b.pack", None, "B", true, &["local"]),
        ];

        let plan = plan_windows_launch(&options, &mods).unwrap();

        assert_eq!(
            plan.mod_list_contents,
            concat!(
                r#"add_working_directory "C:\mods\shared";"#,
                "\n",
                r#"add_working_directory "C:\mods\shared";"#,
                "\n",
                r#"mod "a.pack";"#,
                "\n",
                r#"mod "b.pack";"#
            )
        );
    }

    #[test]
    fn plans_data_modding_copy_without_extra_working_directory() {
        let options = WindowsLaunchOptions::warhammer3(r"C:\game", r"C:\game\data");
        let mods = vec![mod_record(
            r"C:\game\data\modding\local.pack",
            None,
            "Local",
            true,
            &["data-modding"],
        )];

        let plan = plan_windows_launch(&options, &mods).unwrap();

        assert_eq!(plan.mod_list_contents, r#"mod "local.pack";"#);
        assert_eq!(
            plan.pre_launch_copies,
            [PreLaunchCopyOperation {
                from_path: r"C:\game\data\modding\local.pack".to_string(),
                to_path: r"C:\game\data\local.pack".to_string(),
            }]
        );
    }

    #[test]
    fn enabled_merged_packs_skip_their_enabled_source_mods() {
        let options = WindowsLaunchOptions::warhammer3(r"C:\game", r"C:\game\data");
        let merged_source_tag = merged_source_path_tag(r"C:\game\data\a.pack").unwrap();
        let mods = vec![
            mod_record(r"C:\game\data\a.pack", None, "A", true, &["data"]),
            ModRecord {
                identity: ModIdentity::new(
                    r"C:\game\data\merged.pack",
                    Option::<String>::None,
                    "Merged",
                ),
                display_name: "Merged".to_string(),
                source: crate::domain::ModSource::GameData,
                thumbnail_path: None,
                local_modified_ms: None,
                enabled: true,
                always_enabled: false,
                hidden: false,
                categories: Vec::new(),
                tags: vec!["data".to_string(), merged_source_tag],
            },
            mod_record(r"C:\game\data\b.pack", None, "B", true, &["data"]),
        ];

        let plan = plan_windows_launch(&options, &mods).unwrap();

        assert_eq!(
            plan.mod_list_contents,
            concat!(r#"mod "merged.pack";"#, "\n", r#"mod "b.pack";"#)
        );
    }

    #[test]
    fn disabled_merged_packs_do_not_skip_source_mods() {
        let options = WindowsLaunchOptions::warhammer3(r"C:\game", r"C:\game\data");
        let merged_source_tag = merged_source_path_tag(r"C:\game\data\a.pack").unwrap();
        let mods = vec![
            mod_record(r"C:\game\data\a.pack", None, "A", true, &["data"]),
            ModRecord {
                identity: ModIdentity::new(
                    r"C:\game\data\merged.pack",
                    Option::<String>::None,
                    "Merged",
                ),
                display_name: "Merged".to_string(),
                source: crate::domain::ModSource::GameData,
                thumbnail_path: None,
                local_modified_ms: None,
                enabled: false,
                always_enabled: false,
                hidden: false,
                categories: Vec::new(),
                tags: vec!["data".to_string(), merged_source_tag],
            },
        ];

        let plan = plan_windows_launch(&options, &mods).unwrap();

        assert_eq!(plan.mod_list_contents, r#"mod "a.pack";"#);
    }

    #[test]
    fn appends_generated_pack_groups_after_normal_enabled_mods() {
        let mut options = WindowsLaunchOptions::warhammer3(r"C:\game", r"C:\game\data");
        options.extra_pack_groups = vec![
            WindowsLaunchPackGroup::new(
                r"C:\Users\player\AppData\Roaming\wh3mm\tempPacks",
                ["!!!!out.pack"],
            ),
            WindowsLaunchPackGroup::new(r"C:\game\whmm_flows", ["flow_a.pack", "flow_b.pack"]),
        ];
        let mods = vec![
            mod_record(r"C:\mods\a.pack", None, "A", true, &["local"]),
            mod_record(r"C:\game\data\b.pack", None, "B", true, &["data"]),
        ];

        let plan = plan_windows_launch(&options, &mods).unwrap();

        assert_eq!(
            plan.mod_list_contents,
            concat!(
                r#"add_working_directory "C:\mods";"#,
                "\n",
                r#"mod "a.pack";"#,
                "\n",
                r#"mod "b.pack";"#,
                "\n",
                r#"add_working_directory "C:\Users\player\AppData\Roaming\wh3mm\tempPacks";"#,
                "\n",
                r#"mod "!!!!out.pack";"#,
                "\n",
                r#"add_working_directory "C:\game\whmm_flows";"#,
                "\n",
                r#"mod "flow_a.pack";"#,
                "\n",
                r#"mod "flow_b.pack";"#
            )
        );
    }

    #[test]
    fn generated_replacement_pack_skips_original_source_mod() {
        let mut options = WindowsLaunchOptions::warhammer3(r"C:\game", r"C:\game\data");
        options.replaced_pack_paths = vec![r"C:\mods\a.pack".to_string()];
        options.extra_pack_groups = vec![WindowsLaunchPackGroup::new(
            r"C:\game\whmm_overwrites",
            ["a.pack"],
        )];
        let mods = vec![
            mod_record(r"C:\mods\a.pack", None, "A", true, &["local"]),
            mod_record(r"C:\game\data\b.pack", None, "B", true, &["data"]),
        ];

        let plan = plan_windows_launch(&options, &mods).unwrap();

        assert_eq!(
            plan.mod_list_contents,
            concat!(
                r#"mod "b.pack";"#,
                "\n",
                r#"add_working_directory "C:\game\whmm_overwrites";"#,
                "\n",
                r#"mod "a.pack";"#
            )
        );
    }

    #[test]
    fn carries_pre_launch_pack_writes_to_runtime_plan() {
        let mut options = WindowsLaunchOptions::warhammer3(r"C:\game", r"C:\game\data");
        options.pre_launch_pack_writes = vec![super::PreLaunchPackWrite {
            path: r"C:\generated\!!!!out.pack".to_string(),
            bytes: vec![1, 2, 3],
            packed_file_names: vec!["script\\enable_console_logging".to_string()],
        }];

        let plan = plan_windows_launch(&options, &[]).unwrap();

        assert_eq!(plan.pre_launch_pack_writes, options.pre_launch_pack_writes);
    }

    #[test]
    fn rejects_generated_pack_groups_without_pack_names() {
        let mut options = WindowsLaunchOptions::warhammer3(r"C:\game", r"C:\game\data");
        options.extra_pack_groups = vec![WindowsLaunchPackGroup::new(r"C:\generated", [""])];

        let error = plan_windows_launch(&options, &[]).unwrap_err();

        assert_eq!(error.kind, CoreErrorKind::InvalidInput);
    }

    #[test]
    fn rejects_enabled_mods_without_paths() {
        let options = WindowsLaunchOptions::warhammer3(r"C:\game", r"C:\game\data");
        let mods = vec![mod_record("", None, "Missing", true, &["local"])];

        let error = plan_windows_launch(&options, &mods).unwrap_err();

        assert_eq!(error.kind, CoreErrorKind::InvalidInput);
    }

    fn mod_record(
        path: &str,
        workshop_id: Option<&str>,
        name: &str,
        enabled: bool,
        tags: &[&str],
    ) -> ModRecord {
        ModRecord {
            identity: ModIdentity::new(path, workshop_id, name),
            display_name: name.to_string(),
            source: crate::domain::ModSource::Local,
            thumbnail_path: None,
            local_modified_ms: None,
            enabled,
            always_enabled: false,
            hidden: false,
            categories: Vec::new(),
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
        }
    }
}
