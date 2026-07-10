//! Runtime adapters for filesystem and process boundaries.
//!
//! Domain crates produce plans; this crate performs OS-facing work.

use std::fs;
use std::fs::FileTimes;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use wh3mm_core::{
    CoreError, CoreResult, ModRecord, PreLaunchCopyOperation, PreLaunchPackWrite,
    SteamWorkshopAdapterError, SteamWorkshopAdapterErrorKind, SteamWorkshopMetadataAdapter,
    WindowsLaunchPlan, WorkshopModData, normalize_workshop_id,
    parse_ts_steam_helper_mod_data_response, ts_steam_helper_dependency_ids_needing_titles,
};

/// Steam app ID for Total War: Warhammer III.
pub const WH3_STEAM_APP_ID: &str = "1142710";

/// Validated Windows game folder paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedWindowsGameFolder {
    /// Game install directory.
    pub game_dir: PathBuf,
    /// Game data directory.
    pub data_dir: PathBuf,
    /// Game executable path.
    pub executable_path: PathBuf,
}

/// Derived Steam workshop paths for a game install.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamWorkshopFolder {
    /// Steam `steamapps` directory containing the game manifest.
    pub steam_apps_dir: PathBuf,
    /// Workshop content directory for the game app ID.
    pub workshop_content_dir: PathBuf,
}

/// Discovered Steam library paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamLibraryFolder {
    /// Steam library root such as `C:\Program Files (x86)\Steam` or
    /// `D:\SteamLibrary`.
    pub library_dir: PathBuf,
    /// Steam `steamapps` directory inside the library.
    pub steam_apps_dir: PathBuf,
}

/// Discovered WH3 Steam install paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Wh3SteamInstall {
    /// Steam `steamapps` directory containing `appmanifest_1142710.acf`.
    pub steam_apps_dir: PathBuf,
    /// Manifest that proved this library owns the WH3 install.
    pub appmanifest_path: PathBuf,
    /// Validated game install directory.
    pub game_dir: PathBuf,
    /// Validated game data directory.
    pub data_dir: PathBuf,
    /// Validated game executable path.
    pub executable_path: PathBuf,
    /// Workshop content directory when it already exists.
    pub workshop_content_dir: Option<PathBuf>,
}

/// Directories removed during a Steam workshop resubscribe cleanup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkshopModDirCleanup {
    /// Removed Steam workshop mod directories.
    pub removed_dirs: Vec<PathBuf>,
    /// Normalized workshop IDs requested by the caller.
    pub requested_ids: Vec<String>,
}

/// Result of a bounded Steam workshop resubscribe workflow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamResubscribeResult {
    /// Normalized workshop IDs requested by the caller.
    pub requested_ids: Vec<String>,
    /// Removed Steam workshop mod directories.
    pub removed_dirs: Vec<PathBuf>,
    /// Normalized subscribed IDs observed after the last verification.
    pub observed_subscribed_ids: Vec<String>,
    /// Requested IDs still missing after all attempts.
    pub failed_ids: Vec<String>,
    /// Number of subscribe attempts performed.
    pub attempts: usize,
}

/// Result of preparing launch files before spawning the game process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedWindowsLaunch {
    /// Directory used as the process current directory.
    pub working_dir: PathBuf,
    /// Executable to start.
    pub executable: String,
    /// Arguments updated to reference the actual written mod-list file.
    pub args: Vec<String>,
    /// Path to the mod-list file that was written.
    pub mod_list_path: PathBuf,
    /// Generated pack files written before launch.
    pub written_pack_files: Vec<WrittenPackFile>,
    /// Copy operations completed before launch.
    pub copied_files: Vec<CompletedCopyOperation>,
}

/// Optional behaviors to apply while spawning a prepared Windows launch.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WindowsLaunchSpawnOptions {
    /// Process priority to request immediately after spawning the game.
    pub priority_class: Option<WindowsProcessPriorityClass>,
}

/// Supported Windows process priority classes for game launch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowsProcessPriorityClass {
    /// Windows `High` priority class.
    High,
}

impl WindowsProcessPriorityClass {
    #[cfg(any(windows, test))]
    fn powershell_value(self) -> &'static str {
        match self {
            Self::High => "High",
        }
    }
}

/// Best-effort process priority update status.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowsProcessPriorityUpdate {
    /// Spawned process ID targeted by the update.
    pub process_id: u32,
    /// Requested priority class.
    pub requested_class: WindowsProcessPriorityClass,
    /// Whether the runtime attempted an OS priority update.
    pub attempted: bool,
    /// Whether the OS command reported success.
    pub applied: bool,
    /// Human-readable status or error details.
    pub message: String,
}

/// Completed generated pack write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WrittenPackFile {
    /// Destination pack path.
    pub path: PathBuf,
    /// Number of bytes written.
    pub byte_len: usize,
}

/// Completed pre-launch copy operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletedCopyOperation {
    /// Source pack path.
    pub from_path: PathBuf,
    /// Destination pack path.
    pub to_path: PathBuf,
}

/// Safety settings for Steam Workshop command adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamWorkshopCommandSafetyConfig {
    /// Delay a live runner should wait between per-ID Steam commands.
    pub command_delay: Duration,
}

/// Result of checking workshop item state and requesting needed updates.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SteamWorkshopCheckStateResult {
    /// Normalized workshop IDs checked by the command adapter.
    pub checked_ids: Vec<String>,
    /// Normalized checked IDs for which an update download was requested.
    pub update_requested_ids: Vec<String>,
}

impl SteamWorkshopCheckStateResult {
    /// Builds a result with checked IDs and no known update requests.
    #[must_use]
    pub fn checked(checked_ids: Vec<String>) -> Self {
        Self {
            checked_ids,
            update_requested_ids: Vec::new(),
        }
    }
}

/// Result of a Steam Workshop command action.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SteamWorkshopCommandResult {
    /// Helper command name such as `sub`, `download`, or `unsubscribe`.
    pub command: String,
    /// Normalized workshop IDs requested by the safe command adapter.
    pub requested_ids: Vec<String>,
    /// Normalized requested IDs reported by the helper command response.
    pub confirmed_ids: Vec<String>,
    /// Normalized requested IDs for which a check-state command requested
    /// downloads.
    pub update_requested_ids: Vec<String>,
    /// Per-ID command delay reported by the helper, in milliseconds.
    pub delay_ms: Option<u64>,
}

impl SteamWorkshopCommandResult {
    /// Builds a command result for a request without helper-provided details.
    #[must_use]
    pub fn requested(command: impl Into<String>, requested_ids: Vec<String>) -> Self {
        Self {
            command: command.into(),
            requested_ids,
            confirmed_ids: Vec::new(),
            update_requested_ids: Vec::new(),
            delay_ms: None,
        }
    }
}

impl Default for SteamWorkshopCommandSafetyConfig {
    fn default() -> Self {
        Self {
            command_delay: Duration::from_millis(250),
        }
    }
}

/// Safety settings for TS-style Steam workshop resubscribe verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamResubscribeSafetyConfig {
    /// Maximum subscribe/verify attempts.
    pub max_attempts: usize,
    /// Delay after each subscribe attempt before querying subscribed IDs.
    pub verification_delay: Duration,
    /// Delay before retrying IDs that are still not subscribed.
    pub retry_delay: Duration,
}

impl Default for SteamResubscribeSafetyConfig {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            verification_delay: Duration::from_secs(3),
            retry_delay: Duration::from_secs(5),
        }
    }
}

/// Settings for an external Steam Workshop helper executable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamWorkshopHelperProcessConfig {
    /// Maximum time to wait for one helper command to exit.
    pub timeout: Duration,
    /// Environment variables to pass to the helper process.
    pub env_overrides: Vec<(String, String)>,
}

impl Default for SteamWorkshopHelperProcessConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            env_overrides: Vec::new(),
        }
    }
}

/// Command boundary for TypeScript-helper-shaped Steam metadata responses.
pub trait TsSteamHelperRunner {
    /// Returns JSON matching the legacy helper `getModsData` message payload.
    ///
    /// # Errors
    ///
    /// Returns [`SteamWorkshopAdapterError`] when the helper cannot produce a
    /// metadata response.
    fn get_mods_data_json(
        &mut self,
        app_id: &str,
        workshop_ids: &[String],
    ) -> Result<String, SteamWorkshopAdapterError>;

    /// Returns JSON matching the legacy helper `getItems` message payload.
    ///
    /// The default implementation skips dependency-title resolution.
    ///
    /// # Errors
    ///
    /// Returns [`SteamWorkshopAdapterError`] when the helper cannot produce a
    /// dependency item response.
    fn get_items_json(
        &mut self,
        _app_id: &str,
        _workshop_ids: &[String],
    ) -> Result<Option<String>, SteamWorkshopAdapterError> {
        Ok(None)
    }

    /// Returns JSON matching the legacy helper `getDependencies` message
    /// payload.
    ///
    /// The default implementation skips standalone dependency lookup.
    ///
    /// # Errors
    ///
    /// Returns [`SteamWorkshopAdapterError`] when the helper cannot produce a
    /// dependency map response.
    fn get_dependencies_json(
        &mut self,
        _app_id: &str,
        _workshop_ids: &[String],
    ) -> Result<Option<String>, SteamWorkshopAdapterError> {
        Ok(None)
    }

    /// Returns JSON matching the legacy helper `getAuthors` message payload.
    ///
    /// The default implementation skips standalone author lookup.
    ///
    /// # Errors
    ///
    /// Returns [`SteamWorkshopAdapterError`] when the helper cannot produce an
    /// author map response.
    fn get_authors_json(
        &mut self,
        _app_id: &str,
        _author_ids: &[String],
    ) -> Result<Option<String>, SteamWorkshopAdapterError> {
        Ok(None)
    }
}

/// Steam metadata adapter that normalizes TypeScript-helper-shaped responses.
pub struct TsSteamHelperMetadataAdapter<R> {
    app_id: String,
    runner: R,
}

impl<R> TsSteamHelperMetadataAdapter<R> {
    /// Creates a metadata adapter for a Steam app ID.
    #[must_use]
    pub fn new(app_id: impl Into<String>, runner: R) -> Self {
        Self {
            app_id: app_id.into(),
            runner,
        }
    }

    /// Returns the wrapped runner.
    pub fn into_runner(self) -> R {
        self.runner
    }
}

impl<R> SteamWorkshopMetadataAdapter for TsSteamHelperMetadataAdapter<R>
where
    R: TsSteamHelperRunner,
{
    fn fetch_mod_data_batch(
        &mut self,
        workshop_ids: &[String],
    ) -> Result<Vec<WorkshopModData>, SteamWorkshopAdapterError> {
        let mods_data_json = self.runner.get_mods_data_json(&self.app_id, workshop_ids)?;
        let dependency_ids = ts_steam_helper_dependency_ids_needing_titles(&mods_data_json)?;
        let dependency_items_json = if dependency_ids.is_empty() {
            None
        } else {
            self.runner.get_items_json(&self.app_id, &dependency_ids)?
        };

        parse_ts_steam_helper_mod_data_response(&mods_data_json, dependency_items_json.as_deref())
    }
}

/// Command boundary for TypeScript-helper-shaped Steam Workshop actions.
pub trait SteamWorkshopCommandRunner {
    /// Returns subscribed workshop IDs.
    ///
    /// # Errors
    ///
    /// Returns [`SteamWorkshopAdapterError`] when the runner cannot query
    /// subscribed items.
    fn get_subscribed_ids(
        &mut self,
        app_id: &str,
    ) -> Result<Vec<String>, SteamWorkshopAdapterError>;

    /// Subscribes to workshop IDs.
    ///
    /// # Errors
    ///
    /// Returns [`SteamWorkshopAdapterError`] when the runner cannot subscribe.
    fn subscribe_ids(
        &mut self,
        app_id: &str,
        workshop_ids: &[String],
        command_delay: Duration,
    ) -> Result<SteamWorkshopCommandResult, SteamWorkshopAdapterError>;

    /// Downloads or updates workshop IDs.
    ///
    /// # Errors
    ///
    /// Returns [`SteamWorkshopAdapterError`] when the runner cannot request
    /// downloads.
    fn download_ids(
        &mut self,
        app_id: &str,
        workshop_ids: &[String],
        command_delay: Duration,
    ) -> Result<SteamWorkshopCommandResult, SteamWorkshopAdapterError>;

    /// Unsubscribes from workshop IDs.
    ///
    /// # Errors
    ///
    /// Returns [`SteamWorkshopAdapterError`] when the runner cannot unsubscribe.
    fn unsubscribe_ids(
        &mut self,
        app_id: &str,
        workshop_ids: &[String],
    ) -> Result<SteamWorkshopCommandResult, SteamWorkshopAdapterError>;

    /// Checks workshop state and downloads items that need updates.
    ///
    /// # Errors
    ///
    /// Returns [`SteamWorkshopAdapterError`] when the runner cannot check state.
    fn check_state_and_download_updates(
        &mut self,
        app_id: &str,
        workshop_ids: &[String],
        command_delay: Duration,
    ) -> Result<SteamWorkshopCheckStateResult, SteamWorkshopAdapterError>;
}

/// Steam Workshop command adapter that normalizes TypeScript-helper-style calls.
pub struct SteamWorkshopCommandAdapter<R> {
    app_id: String,
    config: SteamWorkshopCommandSafetyConfig,
    runner: R,
}

impl<R> SteamWorkshopCommandAdapter<R> {
    /// Creates a command adapter for a Steam app ID.
    #[must_use]
    pub fn new(app_id: impl Into<String>, runner: R) -> Self {
        Self::with_config(app_id, SteamWorkshopCommandSafetyConfig::default(), runner)
    }

    /// Creates a command adapter for a Steam app ID with explicit safety
    /// settings.
    #[must_use]
    pub fn with_config(
        app_id: impl Into<String>,
        config: SteamWorkshopCommandSafetyConfig,
        runner: R,
    ) -> Self {
        Self {
            app_id: app_id.into(),
            config,
            runner,
        }
    }

    /// Returns the active command safety settings.
    #[must_use]
    pub fn config(&self) -> &SteamWorkshopCommandSafetyConfig {
        &self.config
    }

    /// Returns the wrapped runner.
    pub fn into_runner(self) -> R {
        self.runner
    }
}

impl<R> SteamWorkshopCommandAdapter<R>
where
    R: SteamWorkshopCommandRunner,
{
    /// Returns normalized subscribed workshop IDs.
    ///
    /// # Errors
    ///
    /// Returns [`SteamWorkshopAdapterError`] when the runner cannot query
    /// subscribed items.
    pub fn subscribed_workshop_ids(&mut self) -> Result<Vec<String>, SteamWorkshopAdapterError> {
        let subscribed_ids = self.runner.get_subscribed_ids(&self.app_id)?;
        Ok(normalize_unique_workshop_ids(&subscribed_ids))
    }

    /// Subscribes to normalized, deduped workshop IDs and returns the command
    /// result.
    ///
    /// # Errors
    ///
    /// Returns [`SteamWorkshopAdapterError`] when the runner cannot subscribe.
    pub fn subscribe(
        &mut self,
        workshop_ids: &[String],
    ) -> Result<SteamWorkshopCommandResult, SteamWorkshopAdapterError> {
        let ids = normalize_unique_workshop_ids(workshop_ids);
        if ids.is_empty() {
            return Ok(SteamWorkshopCommandResult::requested("sub", ids));
        }
        let result = self
            .runner
            .subscribe_ids(&self.app_id, &ids, self.config.command_delay)?;
        Ok(normalize_command_result_for_request("sub", ids, result))
    }

    /// Downloads normalized, deduped workshop IDs and returns the command
    /// result.
    ///
    /// # Errors
    ///
    /// Returns [`SteamWorkshopAdapterError`] when the runner cannot download.
    pub fn download(
        &mut self,
        workshop_ids: &[String],
    ) -> Result<SteamWorkshopCommandResult, SteamWorkshopAdapterError> {
        let ids = normalize_unique_workshop_ids(workshop_ids);
        if ids.is_empty() {
            return Ok(SteamWorkshopCommandResult::requested("download", ids));
        }
        let result = self
            .runner
            .download_ids(&self.app_id, &ids, self.config.command_delay)?;
        Ok(normalize_command_result_for_request(
            "download", ids, result,
        ))
    }

    /// Unsubscribes from normalized, deduped workshop IDs and returns the
    /// command result.
    ///
    /// # Errors
    ///
    /// Returns [`SteamWorkshopAdapterError`] when the runner cannot unsubscribe.
    pub fn unsubscribe(
        &mut self,
        workshop_ids: &[String],
    ) -> Result<SteamWorkshopCommandResult, SteamWorkshopAdapterError> {
        let ids = normalize_unique_workshop_ids(workshop_ids);
        if ids.is_empty() {
            return Ok(SteamWorkshopCommandResult::requested("unsubscribe", ids));
        }
        let result = self.runner.unsubscribe_ids(&self.app_id, &ids)?;
        Ok(normalize_command_result_for_request(
            "unsubscribe",
            ids,
            result,
        ))
    }

    /// Checks workshop state for normalized, deduped IDs, triggers downloads
    /// for items that need updates, and returns the IDs sent to the runner.
    ///
    /// # Errors
    ///
    /// Returns [`SteamWorkshopAdapterError`] when the runner cannot check state.
    pub fn check_state_and_download_updates(
        &mut self,
        workshop_ids: &[String],
    ) -> Result<SteamWorkshopCheckStateResult, SteamWorkshopAdapterError> {
        let ids = normalize_unique_workshop_ids(workshop_ids);
        if ids.is_empty() {
            return Ok(SteamWorkshopCheckStateResult::default());
        }

        let result = self.runner.check_state_and_download_updates(
            &self.app_id,
            &ids,
            self.config.command_delay,
        )?;
        let requested_ids = ids
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let update_requested_ids = normalize_unique_workshop_ids(&result.update_requested_ids)
            .into_iter()
            .filter(|id| requested_ids.contains(id))
            .collect();
        Ok(SteamWorkshopCheckStateResult {
            checked_ids: ids,
            update_requested_ids,
        })
    }
}

fn normalize_command_result_for_request(
    command_name: &str,
    requested_ids: Vec<String>,
    result: SteamWorkshopCommandResult,
) -> SteamWorkshopCommandResult {
    let requested_id_set = requested_ids
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let confirmed_ids = normalize_unique_workshop_ids(&result.confirmed_ids)
        .into_iter()
        .filter(|id| requested_id_set.contains(id))
        .collect();
    let update_requested_ids = normalize_unique_workshop_ids(&result.update_requested_ids)
        .into_iter()
        .filter(|id| requested_id_set.contains(id))
        .collect();

    SteamWorkshopCommandResult {
        command: if result.command.trim().is_empty() {
            command_name.to_string()
        } else {
            result.command
        },
        requested_ids,
        confirmed_ids,
        update_requested_ids,
        delay_ms: result.delay_ms,
    }
}

struct HelperCommandResult {
    ok: bool,
    command: String,
    ids: Vec<String>,
    update_requested_ids: Vec<String>,
    delay_ms: Option<u64>,
}

/// External process runner for Steam Workshop metadata and command helpers.
///
/// The helper is called with TypeScript-helper-compatible command names:
/// `getSubscribedIds`, `getModsData`, `getItems`, `getDependencies`,
/// `getAuthors`, `sub`, `download`, `unsubscribe`, and `checkState`. Metadata
/// and subscribed-ID commands must print a compact JSON value on their last
/// non-empty stdout line.
pub struct SteamWorkshopHelperProcessRunner {
    executable_path: PathBuf,
    config: SteamWorkshopHelperProcessConfig,
}

impl SteamWorkshopHelperProcessRunner {
    /// Creates a helper runner with the default process timeout.
    #[must_use]
    pub fn new(executable_path: impl Into<PathBuf>) -> Self {
        Self::with_config(executable_path, SteamWorkshopHelperProcessConfig::default())
    }

    /// Creates a helper runner with explicit process settings.
    #[must_use]
    pub fn with_config(
        executable_path: impl Into<PathBuf>,
        config: SteamWorkshopHelperProcessConfig,
    ) -> Self {
        Self {
            executable_path: executable_path.into(),
            config,
        }
    }

    /// Returns the configured helper executable path.
    #[must_use]
    pub fn executable_path(&self) -> &Path {
        &self.executable_path
    }

    /// Returns the configured process settings.
    #[must_use]
    pub fn config(&self) -> &SteamWorkshopHelperProcessConfig {
        &self.config
    }

    /// Runs the helper's read-only diagnostic probe command.
    ///
    /// The helper returns compact JSON describing its selected backend and
    /// packaging/runtime readiness. This method intentionally returns the raw
    /// JSON so UI shells can surface backend-specific fields without expanding
    /// the core adapter contract for every helper implementation detail.
    ///
    /// # Errors
    ///
    /// Returns [`SteamWorkshopAdapterError`] when the helper process cannot be
    /// started, exits unsuccessfully, times out, or does not print JSON.
    pub fn probe_json(&mut self, app_id: &str) -> Result<String, SteamWorkshopAdapterError> {
        self.run_json_command(app_id, "probe", None, None)
    }

    fn run_json_command(
        &mut self,
        app_id: &str,
        command_name: &str,
        payload: Option<&str>,
        command_delay: Option<Duration>,
    ) -> Result<String, SteamWorkshopAdapterError> {
        let stdout = self.run_command(app_id, command_name, payload, command_delay)?;
        last_stdout_json_line(&stdout).ok_or_else(|| {
            SteamWorkshopAdapterError::new(
                SteamWorkshopAdapterErrorKind::MalformedResponse,
                format!("Steam helper {command_name} did not print a JSON response"),
            )
        })
    }

    fn run_command(
        &mut self,
        app_id: &str,
        command_name: &str,
        payload: Option<&str>,
        command_delay: Option<Duration>,
    ) -> Result<String, SteamWorkshopAdapterError> {
        let mut args = vec![app_id.to_string(), command_name.to_string()];
        if let Some(payload) = payload {
            args.push(payload.to_string());
        }
        if let Some(command_delay) = command_delay {
            args.push(command_delay.as_millis().to_string());
        }

        let output = run_helper_process_with_timeout(
            &self.executable_path,
            &args,
            self.config.timeout,
            &self.config.env_overrides,
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        if output.status.success() {
            return Ok(stdout);
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(SteamWorkshopAdapterError::new(
            SteamWorkshopAdapterErrorKind::Unavailable,
            format!(
                "Steam helper {command_name} exited with status {}: {}",
                output.status,
                stderr.trim()
            ),
        ))
    }
}

impl TsSteamHelperRunner for SteamWorkshopHelperProcessRunner {
    fn get_mods_data_json(
        &mut self,
        app_id: &str,
        workshop_ids: &[String],
    ) -> Result<String, SteamWorkshopAdapterError> {
        self.run_json_command(app_id, "getModsData", Some(&workshop_ids.join(",")), None)
    }

    fn get_items_json(
        &mut self,
        app_id: &str,
        workshop_ids: &[String],
    ) -> Result<Option<String>, SteamWorkshopAdapterError> {
        self.run_json_command(app_id, "getItems", Some(&workshop_ids.join(",")), None)
            .map(Some)
    }

    fn get_dependencies_json(
        &mut self,
        app_id: &str,
        workshop_ids: &[String],
    ) -> Result<Option<String>, SteamWorkshopAdapterError> {
        self.run_json_command(
            app_id,
            "getDependencies",
            Some(&workshop_ids.join(",")),
            None,
        )
        .map(Some)
    }

    fn get_authors_json(
        &mut self,
        app_id: &str,
        author_ids: &[String],
    ) -> Result<Option<String>, SteamWorkshopAdapterError> {
        self.run_json_command(app_id, "getAuthors", Some(&author_ids.join(",")), None)
            .map(Some)
    }
}

impl SteamWorkshopCommandRunner for SteamWorkshopHelperProcessRunner {
    fn get_subscribed_ids(
        &mut self,
        app_id: &str,
    ) -> Result<Vec<String>, SteamWorkshopAdapterError> {
        let json = self.run_json_command(app_id, "getSubscribedIds", None, None)?;
        serde_json::from_str::<Vec<String>>(&json).map_err(|error| {
            SteamWorkshopAdapterError::new(
                SteamWorkshopAdapterErrorKind::MalformedResponse,
                format!("failed to parse Steam helper getSubscribedIds response: {error}"),
            )
        })
    }

    fn subscribe_ids(
        &mut self,
        app_id: &str,
        workshop_ids: &[String],
        command_delay: Duration,
    ) -> Result<SteamWorkshopCommandResult, SteamWorkshopAdapterError> {
        let stdout = self.run_command(
            app_id,
            "sub",
            Some(&workshop_ids.join(";")),
            Some(command_delay),
        )?;
        command_result_from_stdout_or_request(&stdout, "sub", workshop_ids)
    }

    fn download_ids(
        &mut self,
        app_id: &str,
        workshop_ids: &[String],
        command_delay: Duration,
    ) -> Result<SteamWorkshopCommandResult, SteamWorkshopAdapterError> {
        let stdout = self.run_command(
            app_id,
            "download",
            Some(&workshop_ids.join(";")),
            Some(command_delay),
        )?;
        command_result_from_stdout_or_request(&stdout, "download", workshop_ids)
    }

    fn unsubscribe_ids(
        &mut self,
        app_id: &str,
        workshop_ids: &[String],
    ) -> Result<SteamWorkshopCommandResult, SteamWorkshopAdapterError> {
        let stdout =
            self.run_command(app_id, "unsubscribe", Some(&workshop_ids.join(";")), None)?;
        command_result_from_stdout_or_request(&stdout, "unsubscribe", workshop_ids)
    }

    fn check_state_and_download_updates(
        &mut self,
        app_id: &str,
        workshop_ids: &[String],
        command_delay: Duration,
    ) -> Result<SteamWorkshopCheckStateResult, SteamWorkshopAdapterError> {
        let stdout = self.run_command(
            app_id,
            "checkState",
            Some(&workshop_ids.join(";")),
            Some(command_delay),
        )?;
        let Some(json) = last_stdout_json_object_line(&stdout) else {
            return Ok(SteamWorkshopCheckStateResult::checked(
                workshop_ids.to_vec(),
            ));
        };
        let command_result = parse_helper_command_result(&json, "checkState")?;
        if !command_result.ok {
            return Err(SteamWorkshopAdapterError::new(
                SteamWorkshopAdapterErrorKind::Unavailable,
                "Steam helper checkState reported a failed update request",
            ));
        }
        Ok(SteamWorkshopCheckStateResult {
            checked_ids: workshop_ids.to_vec(),
            update_requested_ids: command_result.update_requested_ids,
        })
    }
}

fn run_helper_process_with_timeout(
    executable_path: &Path,
    args: &[String],
    timeout: Duration,
    env_overrides: &[(String, String)],
) -> Result<Output, SteamWorkshopAdapterError> {
    let mut child = Command::new(executable_path)
        .args(args)
        .envs(env_overrides.iter().map(|(key, value)| (key, value)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            SteamWorkshopAdapterError::new(
                SteamWorkshopAdapterErrorKind::Unavailable,
                format!(
                    "failed to start Steam helper {}: {error}",
                    executable_path.display()
                ),
            )
        })?;

    let started_at = Instant::now();
    loop {
        if child
            .try_wait()
            .map_err(|error| steam_helper_wait_error(&error))?
            .is_some()
        {
            return child
                .wait_with_output()
                .map_err(|error| steam_helper_wait_error(&error));
        }
        if started_at.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(SteamWorkshopAdapterError::new(
                SteamWorkshopAdapterErrorKind::Unavailable,
                format!(
                    "Steam helper {} timed out after {}ms",
                    executable_path.display(),
                    timeout.as_millis()
                ),
            ));
        }

        let remaining = timeout.saturating_sub(started_at.elapsed());
        thread::sleep(remaining.min(Duration::from_millis(10)));
    }
}

fn steam_helper_wait_error(error: &std::io::Error) -> SteamWorkshopAdapterError {
    SteamWorkshopAdapterError::new(
        SteamWorkshopAdapterErrorKind::Unavailable,
        format!("failed while waiting for Steam helper: {error}"),
    )
}

fn last_stdout_json_line(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToString::to_string)
}

fn last_stdout_json_object_line(stdout: &str) -> Option<String> {
    last_stdout_json_line(stdout).filter(|line| line.trim_start().starts_with('{'))
}

fn command_result_from_stdout_or_request(
    stdout: &str,
    command_name: &str,
    workshop_ids: &[String],
) -> Result<SteamWorkshopCommandResult, SteamWorkshopAdapterError> {
    let Some(json) = last_stdout_json_object_line(stdout) else {
        return Ok(SteamWorkshopCommandResult::requested(
            command_name,
            workshop_ids.to_vec(),
        ));
    };
    let command_result = parse_helper_command_result(&json, command_name)?;
    if !command_result.ok {
        return Err(SteamWorkshopAdapterError::new(
            SteamWorkshopAdapterErrorKind::Unavailable,
            format!("Steam helper {command_name} reported a failed command"),
        ));
    }
    Ok(SteamWorkshopCommandResult {
        command: command_result.command,
        requested_ids: workshop_ids.to_vec(),
        confirmed_ids: command_result.ids,
        update_requested_ids: command_result.update_requested_ids,
        delay_ms: command_result.delay_ms,
    })
}

fn parse_helper_command_result(
    json: &str,
    command_name: &str,
) -> Result<HelperCommandResult, SteamWorkshopAdapterError> {
    let value = serde_json::from_str::<serde_json::Value>(json).map_err(|error| {
        SteamWorkshopAdapterError::new(
            SteamWorkshopAdapterErrorKind::MalformedResponse,
            format!("failed to parse Steam helper {command_name} command response: {error}"),
        )
    })?;
    let ok = value
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            SteamWorkshopAdapterError::new(
                SteamWorkshopAdapterErrorKind::MalformedResponse,
                format!("Steam helper {command_name} command response is missing boolean ok"),
            )
        })?;
    let command = optional_string_field(&value, "command", command_name)?.unwrap_or_default();
    if !command.is_empty() && command != command_name {
        return Err(SteamWorkshopAdapterError::new(
            SteamWorkshopAdapterErrorKind::MalformedResponse,
            format!(
                "Steam helper {command_name} command response reported unexpected command {command:?}"
            ),
        ));
    }
    let ids = optional_string_array_field(&value, "ids", command_name)?;
    let update_requested_ids =
        optional_string_array_field(&value, "updateRequestedIds", command_name)?;
    let delay_ms = optional_u64_field(&value, "delayMs", command_name)?;

    Ok(HelperCommandResult {
        ok,
        command: if command.is_empty() {
            command_name.to_string()
        } else {
            command
        },
        ids: normalize_unique_workshop_ids(&ids),
        update_requested_ids: normalize_unique_workshop_ids(&update_requested_ids),
        delay_ms,
    })
}

fn optional_string_field(
    value: &serde_json::Value,
    field_name: &str,
    command_name: &str,
) -> Result<Option<String>, SteamWorkshopAdapterError> {
    let Some(value) = value.get(field_name) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(value) = value.as_str() else {
        return Err(SteamWorkshopAdapterError::new(
            SteamWorkshopAdapterErrorKind::MalformedResponse,
            format!(
                "Steam helper {command_name} command response field {field_name} is not a string"
            ),
        ));
    };
    Ok(Some(value.trim().to_string()))
}

fn optional_string_array_field(
    value: &serde_json::Value,
    field_name: &str,
    command_name: &str,
) -> Result<Vec<String>, SteamWorkshopAdapterError> {
    let Some(value) = value.get(field_name) else {
        return Ok(Vec::new());
    };
    let Some(values) = value.as_array() else {
        return Err(SteamWorkshopAdapterError::new(
            SteamWorkshopAdapterErrorKind::MalformedResponse,
            format!(
                "Steam helper {command_name} command response field {field_name} is not an array"
            ),
        ));
    };

    values
        .iter()
        .map(|value| {
            value.as_str().map(ToString::to_string).ok_or_else(|| {
                SteamWorkshopAdapterError::new(
                    SteamWorkshopAdapterErrorKind::MalformedResponse,
                    format!(
                        "Steam helper {command_name} command response field {field_name} contains a non-string value"
                    ),
                )
            })
        })
        .collect()
}

fn optional_u64_field(
    value: &serde_json::Value,
    field_name: &str,
    command_name: &str,
) -> Result<Option<u64>, SteamWorkshopAdapterError> {
    let Some(value) = value.get(field_name) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    if let Some(value) = value.as_u64() {
        return Ok(Some(value));
    }
    if let Some(value) = value.as_str() {
        return value.trim().parse::<u64>().map(Some).map_err(|error| {
            SteamWorkshopAdapterError::new(
                SteamWorkshopAdapterErrorKind::MalformedResponse,
                format!(
                    "Steam helper {command_name} command response field {field_name} is not a valid u64: {error}"
                ),
            )
        });
    }

    Err(SteamWorkshopAdapterError::new(
        SteamWorkshopAdapterErrorKind::MalformedResponse,
        format!("Steam helper {command_name} command response field {field_name} is not a u64"),
    ))
}

fn normalize_unique_workshop_ids(ids: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    for id in ids {
        if let Some(id) = normalize_workshop_id(id)
            && !normalized.iter().any(|existing| existing == &id)
        {
            normalized.push(id);
        }
    }
    normalized
}

/// Options for executing filesystem launch preparation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchPreparationOptions {
    /// Fallback mod-list file name when `used_mods.txt` cannot be written.
    pub fallback_mod_list_file_name: String,
    /// Attempts to write each generated launch pack before giving up.
    pub generated_pack_write_attempts: usize,
    /// Delay between generated launch pack write attempts.
    pub generated_pack_write_retry_delay: Duration,
}

impl Default for LaunchPreparationOptions {
    fn default() -> Self {
        Self {
            fallback_mod_list_file_name: "my_mods.txt".to_string(),
            generated_pack_write_attempts: 10,
            generated_pack_write_retry_delay: Duration::from_millis(500),
        }
    }
}

/// Validates that a selected folder looks like a Windows game install.
///
/// # Errors
///
/// Returns [`CoreError`] when the game directory, data directory, or executable
/// is missing.
pub fn validate_windows_game_folder(
    game_dir: impl AsRef<Path>,
    process_name: &str,
) -> CoreResult<ValidatedWindowsGameFolder> {
    let game_dir = game_dir.as_ref();
    if !game_dir.is_dir() {
        return Err(CoreError::invalid_input(format!(
            "game folder does not exist: {}",
            game_dir.display()
        )));
    }

    let data_dir = game_dir.join("data");
    if !data_dir.is_dir() {
        return Err(CoreError::invalid_input(format!(
            "game data folder does not exist: {}",
            data_dir.display()
        )));
    }

    let executable_path = game_dir.join(process_name);
    if !executable_path.is_file() {
        return Err(CoreError::invalid_input(format!(
            "game executable does not exist: {}",
            executable_path.display()
        )));
    }

    Ok(ValidatedWindowsGameFolder {
        game_dir: game_dir.to_path_buf(),
        data_dir,
        executable_path,
    })
}

/// Validates that a selected folder looks like a WH3 Windows install.
///
/// # Errors
///
/// Returns [`CoreError`] when the game directory, data directory, or
/// `Warhammer3.exe` is missing.
pub fn validate_wh3_game_folder(
    game_dir: impl AsRef<Path>,
) -> CoreResult<ValidatedWindowsGameFolder> {
    validate_windows_game_folder(game_dir, "Warhammer3.exe")
}

/// Removes loaded Steam workshop mod directories for the requested IDs.
///
/// This mirrors the local cleanup part of the TypeScript force-resubscribe
/// flow while keeping a strict path guard: only loaded mods with a matching
/// workshop ID and a path shaped like `workshop/content/1142710/<id>/*.pack`
/// are removed.
///
/// # Errors
///
/// Returns [`CoreError`] when a guarded workshop directory cannot be removed.
pub fn remove_loaded_workshop_mod_dirs(
    mods: &[ModRecord],
    workshop_ids: &[String],
) -> CoreResult<WorkshopModDirCleanup> {
    let requested_ids = normalize_unique_workshop_ids(workshop_ids);
    let mut seen_dirs = std::collections::BTreeSet::new();
    let mut removed_dirs = Vec::new();

    for workshop_id in &requested_ids {
        for mod_record in mods {
            if mod_record
                .identity
                .workshop_id
                .as_deref()
                .and_then(normalize_workshop_id)
                .as_ref()
                != Some(workshop_id)
            {
                continue;
            }

            let Some(dir) = guarded_workshop_pack_dir(&mod_record.identity.path, workshop_id)
            else {
                continue;
            };
            if !seen_dirs.insert(dir.clone()) {
                continue;
            }
            if !dir.exists() {
                continue;
            }
            fs::remove_dir_all(&dir).map_err(|error| {
                CoreError::io(format!(
                    "failed to remove Steam workshop mod directory {}: {error}",
                    dir.display()
                ))
            })?;
            removed_dirs.push(dir);
        }
    }

    Ok(WorkshopModDirCleanup {
        removed_dirs,
        requested_ids,
    })
}

/// Runs a guarded TS-style resubscribe workflow and verifies the result.
///
/// The workflow is:
/// 1. unsubscribe requested IDs,
/// 2. remove matching loaded workshop directories,
/// 3. subscribe/download pending IDs,
/// 4. query subscribed IDs and retry only still-missing IDs up to the configured
///    attempt limit.
///
/// # Errors
///
/// Returns [`CoreError`] when filesystem cleanup fails or a Steam helper command
/// fails.
pub fn resubscribe_with_cleanup_and_verification<R>(
    mods: &[ModRecord],
    workshop_ids: &[String],
    command_adapter: &mut SteamWorkshopCommandAdapter<R>,
    config: &SteamResubscribeSafetyConfig,
) -> CoreResult<SteamResubscribeResult>
where
    R: SteamWorkshopCommandRunner,
{
    let requested_ids = normalize_unique_workshop_ids(workshop_ids);
    if requested_ids.is_empty() {
        return Ok(SteamResubscribeResult {
            requested_ids,
            removed_dirs: Vec::new(),
            observed_subscribed_ids: Vec::new(),
            failed_ids: Vec::new(),
            attempts: 0,
        });
    }

    let unsubscribe_result = command_adapter
        .unsubscribe(&requested_ids)
        .map_err(steam_adapter_core_error)?;
    let sent_ids = unsubscribe_result.requested_ids;
    let cleanup = remove_loaded_workshop_mod_dirs(mods, &sent_ids)?;

    let max_attempts = config.max_attempts.max(1);
    let mut pending_ids = sent_ids.clone();
    let mut observed_subscribed_ids = Vec::new();
    let mut attempts = 0;

    for attempt_index in 0..max_attempts {
        attempts = attempt_index + 1;
        command_adapter
            .subscribe(&pending_ids)
            .map_err(steam_adapter_core_error)?;
        command_adapter
            .download(&pending_ids)
            .map_err(steam_adapter_core_error)?;

        sleep_if_nonzero(config.verification_delay);
        observed_subscribed_ids = command_adapter
            .subscribed_workshop_ids()
            .map_err(steam_adapter_core_error)?;
        let subscribed_set = observed_subscribed_ids
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        pending_ids = sent_ids
            .iter()
            .filter(|id| !subscribed_set.contains(*id))
            .cloned()
            .collect();
        if pending_ids.is_empty() {
            break;
        }
        if attempt_index + 1 < max_attempts {
            sleep_if_nonzero(config.retry_delay);
        }
    }

    Ok(SteamResubscribeResult {
        requested_ids: sent_ids,
        removed_dirs: cleanup.removed_dirs,
        observed_subscribed_ids,
        failed_ids: pending_ids,
        attempts,
    })
}

fn steam_adapter_core_error(error: SteamWorkshopAdapterError) -> CoreError {
    CoreError::adapter(error.message)
}

fn sleep_if_nonzero(duration: Duration) {
    if !duration.is_zero() {
        thread::sleep(duration);
    }
}

fn guarded_workshop_pack_dir(pack_path: &str, workshop_id: &str) -> Option<PathBuf> {
    let components = normalized_path_components(pack_path);
    if components.len() < 5 {
        return None;
    }

    for index in 0..components.len().saturating_sub(4) {
        if components[index].eq_ignore_ascii_case("workshop")
            && components[index + 1].eq_ignore_ascii_case("content")
            && components[index + 2] == WH3_STEAM_APP_ID
            && components[index + 3] == workshop_id
            && index + 4 == components.len() - 1
            && components[index + 4]
                .to_ascii_lowercase()
                .ends_with(".pack")
        {
            return Path::new(pack_path).parent().map(Path::to_path_buf);
        }
    }

    None
}

fn normalized_path_components(path: &str) -> Vec<String> {
    path.replace('\\', "/")
        .split('/')
        .filter(|component| !component.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Derives the WH3 Steam workshop content directory from a selected game folder.
///
/// # Errors
///
/// Returns [`CoreError`] when the folder is not a valid WH3 install, is not
/// under a recognizable Steam `steamapps/common` layout, or the workshop
/// content directory does not exist.
pub fn discover_wh3_workshop_folder(game_dir: impl AsRef<Path>) -> CoreResult<SteamWorkshopFolder> {
    let validated = validate_wh3_game_folder(game_dir)?;
    let steam_apps_dir = steam_apps_dir_from_game_dir(&validated.game_dir)?;
    let workshop_content_dir = steam_apps_dir
        .join("workshop")
        .join("content")
        .join(WH3_STEAM_APP_ID);

    if !workshop_content_dir.is_dir() {
        return Err(CoreError::invalid_input(format!(
            "WH3 workshop content folder does not exist: {}",
            workshop_content_dir.display()
        )));
    }

    Ok(SteamWorkshopFolder {
        steam_apps_dir,
        workshop_content_dir,
    })
}

/// Discovers the WH3 install from a Steam installation root.
///
/// This mirrors the TS path that starts at Steam's install directory, reads
/// `steamapps/libraryfolders.vdf`, then finds the library with
/// `appmanifest_1142710.acf`.
///
/// # Errors
///
/// Returns [`CoreError`] when `libraryfolders.vdf` cannot be read, cannot be
/// parsed, or no validated WH3 Steam install is found.
pub fn discover_wh3_steam_install_from_steam_root(
    steam_root: impl AsRef<Path>,
) -> CoreResult<Wh3SteamInstall> {
    discover_wh3_steam_install_from_libraryfolders_vdf(
        steam_root
            .as_ref()
            .join("steamapps")
            .join("libraryfolders.vdf"),
    )
}

/// Discovers the WH3 install from a Steam `libraryfolders.vdf` file.
///
/// # Errors
///
/// Returns [`CoreError`] when the VDF cannot be read, cannot be parsed, or no
/// validated WH3 Steam install is found.
pub fn discover_wh3_steam_install_from_libraryfolders_vdf(
    libraryfolders_vdf_path: impl AsRef<Path>,
) -> CoreResult<Wh3SteamInstall> {
    let libraryfolders_vdf_path = libraryfolders_vdf_path.as_ref();
    let text = fs::read_to_string(libraryfolders_vdf_path).map_err(|error| {
        CoreError::io(format!(
            "failed to read Steam libraryfolders file {}: {error}",
            libraryfolders_vdf_path.display()
        ))
    })?;
    let libraries = parse_steam_libraries_from_libraryfolders_vdf(&text)?;
    discover_wh3_steam_install_from_libraries(&libraries)
}

/// Discovers the WH3 Steam install from Steam's machine- or user-level Windows
/// registry values.
///
/// # Errors
///
/// Returns [`CoreError`] when the registry cannot be queried, Steam's
/// install path cannot be parsed, or no validated WH3 Steam install is found.
pub fn discover_wh3_steam_install_from_windows_registry() -> CoreResult<Wh3SteamInstall> {
    let steam_root = discover_steam_root_from_windows_registry()?;
    discover_wh3_steam_install_from_steam_root(steam_root)
}

/// Reads Steam's Windows registry install path.
///
/// # Errors
///
/// Returns [`CoreError`] when called outside Windows, when `reg.exe` fails, or
/// when the output does not contain an `InstallPath` or `SteamPath` value.
pub fn discover_steam_root_from_windows_registry() -> CoreResult<PathBuf> {
    discover_steam_root_from_windows_registry_impl()
}

/// Parses the output of `reg query` for Steam's `InstallPath` or `SteamPath`.
///
/// # Errors
///
/// Returns [`CoreError`] when no supported non-empty install path is present.
pub fn parse_steam_install_path_from_reg_query_output(output: &str) -> CoreResult<PathBuf> {
    for line in output.lines() {
        let trimmed = line.trim_start();
        let Some(value_name) = trimmed.split_whitespace().next() else {
            continue;
        };
        if !value_name.eq_ignore_ascii_case("InstallPath")
            && !value_name.eq_ignore_ascii_case("SteamPath")
        {
            continue;
        }

        let mut parts = trimmed.split_whitespace();
        let _name = parts.next();
        let Some(value_type) = parts.next() else {
            continue;
        };
        if !value_type.starts_with("REG_") {
            continue;
        }

        let value = parts.collect::<Vec<_>>().join(" ");
        if !value.trim().is_empty() {
            return Ok(PathBuf::from(value));
        }
    }

    Err(CoreError::parse(
        "Windows registry query output did not contain Steam InstallPath or SteamPath",
    ))
}

/// Parses Steam libraries from `libraryfolders.vdf` text.
///
/// Supports both the newer nested format with `"path"` fields and the older
/// flat index-to-path format.
///
/// # Errors
///
/// Returns [`CoreError`] when the VDF is malformed or contains no library
/// paths.
pub fn parse_steam_libraries_from_libraryfolders_vdf(
    text: &str,
) -> CoreResult<Vec<SteamLibraryFolder>> {
    let parsed = parse_vdf(text)?;
    let libraryfolders = parsed
        .get_object("libraryfolders")
        .ok_or_else(|| CoreError::parse("Steam libraryfolders.vdf is missing libraryfolders"))?;

    let mut libraries = Vec::new();
    for (_, value) in libraryfolders {
        let Some(path) = steam_library_path_from_vdf_value(value) else {
            continue;
        };
        push_unique_library(&mut libraries, PathBuf::from(path));
    }

    if libraries.is_empty() {
        return Err(CoreError::parse(
            "Steam libraryfolders.vdf did not contain any library paths",
        ));
    }

    Ok(libraries)
}

#[cfg(windows)]
fn discover_steam_root_from_windows_registry_impl() -> CoreResult<PathBuf> {
    let candidates = [
        (r"HKCU\SOFTWARE\Valve\Steam", "SteamPath"),
        (r"HKLM\SOFTWARE\Wow6432Node\Valve\Steam", "InstallPath"),
        (r"HKLM\SOFTWARE\Valve\Steam", "InstallPath"),
    ];
    let mut failures = Vec::new();

    for (key, value_name) in candidates {
        let output = match Command::new("reg")
            .args(["query", key, "/v", value_name])
            .output()
        {
            Ok(output) => output,
            Err(error) => {
                failures.push(format!("{key} {value_name}: {error}"));
                continue;
            }
        };
        if !output.status.success() {
            failures.push(format!(
                "{key} {value_name}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
            continue;
        }
        match parse_steam_install_path_from_reg_query_output(&String::from_utf8_lossy(
            &output.stdout,
        )) {
            Ok(path) => return Ok(path),
            Err(error) => failures.push(format!("{key} {value_name}: {}", error.message)),
        }
    }

    Err(CoreError::io(format!(
        "Steam registry discovery failed: {}",
        failures.join("; ")
    )))
}

#[cfg(not(windows))]
fn discover_steam_root_from_windows_registry_impl() -> CoreResult<PathBuf> {
    Err(CoreError::invalid_input(
        "Windows Steam registry discovery is only available on Windows",
    ))
}

/// Writes launch files and executes pre-launch copies for a Windows launch plan.
///
/// # Errors
///
/// Returns [`CoreError`] when any planned filesystem operation fails. If the
/// primary mod-list write fails, the fallback file is attempted before an error
/// is returned.
pub fn prepare_windows_launch_files(
    plan: &WindowsLaunchPlan,
    options: &LaunchPreparationOptions,
) -> CoreResult<PreparedWindowsLaunch> {
    let working_dir = PathBuf::from(&plan.working_dir);
    let written_pack_files = write_pre_launch_pack_files(&plan.pre_launch_pack_writes, options)?;
    let copied_files = copy_pre_launch_files(&plan.pre_launch_copies)?;
    let (mod_list_file_name, mod_list_path) = write_mod_list_with_fallback(
        &working_dir,
        &plan.mod_list_file_name,
        &options.fallback_mod_list_file_name,
        &plan.mod_list_contents,
    )?;

    Ok(PreparedWindowsLaunch {
        working_dir,
        executable: plan.executable.clone(),
        args: args_for_mod_list_file(plan, &mod_list_file_name),
        mod_list_path,
        written_pack_files,
        copied_files,
    })
}

/// Spawns a prepared launch process without going through a shell.
///
/// # Errors
///
/// Returns [`CoreError`] when the OS fails to spawn the process.
pub fn spawn_prepared_windows_launch(prepared: &PreparedWindowsLaunch) -> CoreResult<Child> {
    Command::new(executable_path_for_prepared_launch(prepared))
        .current_dir(&prepared.working_dir)
        .args(&prepared.args)
        .spawn()
        .map_err(|error| CoreError::io(format!("failed to spawn game process: {error}")))
}

/// Spawns the prepared game process and applies optional launch behaviors.
///
/// The process priority update is best-effort: failure to set priority is
/// returned as status but does not fail the already-spawned launch.
///
/// # Errors
///
/// Returns [`CoreError`] when the OS fails to spawn the process.
pub fn spawn_prepared_windows_launch_with_options(
    prepared: &PreparedWindowsLaunch,
    options: &WindowsLaunchSpawnOptions,
) -> CoreResult<(Child, Option<WindowsProcessPriorityUpdate>)> {
    let child = spawn_prepared_windows_launch(prepared)?;
    let priority_update = options
        .priority_class
        .map(|priority_class| set_windows_process_priority(child.id(), priority_class));
    Ok((child, priority_update))
}

/// Requests a Windows process priority update for a spawned process.
///
/// On non-Windows platforms this returns a skipped status so tests and
/// off-Windows development can exercise the launch path without invoking
/// platform tools.
#[must_use]
pub fn set_windows_process_priority(
    process_id: u32,
    priority_class: WindowsProcessPriorityClass,
) -> WindowsProcessPriorityUpdate {
    #[cfg(windows)]
    {
        set_windows_process_priority_impl(process_id, priority_class)
    }
    #[cfg(not(windows))]
    {
        WindowsProcessPriorityUpdate {
            process_id,
            requested_class: priority_class,
            attempted: false,
            applied: false,
            message: "process priority changes are only supported on Windows".to_string(),
        }
    }
}

#[cfg(windows)]
fn set_windows_process_priority_impl(
    process_id: u32,
    priority_class: WindowsProcessPriorityClass,
) -> WindowsProcessPriorityUpdate {
    let command = powershell_set_priority_command(process_id, priority_class);
    match Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &command])
        .output()
    {
        Ok(output) if output.status.success() => WindowsProcessPriorityUpdate {
            process_id,
            requested_class: priority_class,
            attempted: true,
            applied: true,
            message: format!(
                "set process {process_id} priority to {}",
                priority_class.powershell_value()
            ),
        },
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let detail = if stderr.is_empty() { stdout } else { stderr };
            WindowsProcessPriorityUpdate {
                process_id,
                requested_class: priority_class,
                attempted: true,
                applied: false,
                message: format!(
                    "failed to set process {process_id} priority to {}: {detail}",
                    priority_class.powershell_value()
                ),
            }
        }
        Err(error) => WindowsProcessPriorityUpdate {
            process_id,
            requested_class: priority_class,
            attempted: true,
            applied: false,
            message: format!(
                "failed to start PowerShell to set process {process_id} priority to {}: {error}",
                priority_class.powershell_value()
            ),
        },
    }
}

#[cfg(any(windows, test))]
fn powershell_set_priority_command(
    process_id: u32,
    priority_class: WindowsProcessPriorityClass,
) -> String {
    format!(
        "$ErrorActionPreference = 'Stop'; (Get-Process -Id {process_id}).PriorityClass = '{}'",
        priority_class.powershell_value()
    )
}

fn executable_path_for_prepared_launch(prepared: &PreparedWindowsLaunch) -> PathBuf {
    let executable_path = PathBuf::from(&prepared.executable);
    if executable_path.is_absolute() {
        executable_path
    } else {
        prepared.working_dir.join(executable_path)
    }
}

fn copy_pre_launch_files(
    operations: &[PreLaunchCopyOperation],
) -> CoreResult<Vec<CompletedCopyOperation>> {
    let mut completed = Vec::with_capacity(operations.len());
    for operation in operations {
        let from_path = PathBuf::from(&operation.from_path);
        let to_path = PathBuf::from(&operation.to_path);
        let source_metadata = fs::metadata(&from_path).map_err(|error| {
            CoreError::io(format!(
                "failed to read source launch pack metadata {}: {error}",
                from_path.display()
            ))
        })?;

        if let Some(parent) = to_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                CoreError::io(format!(
                    "failed to create launch copy directory {}: {error}",
                    parent.display()
                ))
            })?;
        }

        if let Ok(target_metadata) = fs::metadata(&to_path)
            && target_metadata.modified().ok() > source_metadata.modified().ok()
        {
            return Err(CoreError::invalid_input(format!(
                "launch copy target {} is newer than source {}",
                to_path.display(),
                from_path.display()
            )));
        }

        fs::copy(&from_path, &to_path).map_err(|error| {
            CoreError::io(format!(
                "failed to copy launch pack {} to {}: {error}",
                from_path.display(),
                to_path.display()
            ))
        })?;
        preserve_copied_file_times(&from_path, &to_path, &source_metadata)?;
        completed.push(CompletedCopyOperation { from_path, to_path });
    }

    Ok(completed)
}

fn write_pre_launch_pack_files(
    writes: &[PreLaunchPackWrite],
    options: &LaunchPreparationOptions,
) -> CoreResult<Vec<WrittenPackFile>> {
    let mut completed = Vec::with_capacity(writes.len());
    for write in writes {
        let path = PathBuf::from(write.path.trim());
        if path.as_os_str().is_empty() {
            return Err(CoreError::invalid_input(
                "generated launch pack path is required",
            ));
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                CoreError::io(format!(
                    "failed to create generated launch pack directory {}: {error}",
                    parent.display()
                ))
            })?;
        }

        write_generated_launch_pack_with_retries(&path, &write.bytes, options)?;
        completed.push(WrittenPackFile {
            path,
            byte_len: write.bytes.len(),
        });
    }

    Ok(completed)
}

fn write_generated_launch_pack_with_retries(
    path: &Path,
    bytes: &[u8],
    options: &LaunchPreparationOptions,
) -> CoreResult<()> {
    let attempts = options.generated_pack_write_attempts.max(1);
    let mut last_error = None;

    for attempt in 1..=attempts {
        match fs::write(path, bytes) {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                if attempt < attempts {
                    thread::sleep(options.generated_pack_write_retry_delay);
                }
            }
        }
    }

    let error = last_error.map_or_else(
        || "unknown write error".to_string(),
        |error| error.to_string(),
    );
    Err(CoreError::io(format!(
        "failed to write generated launch pack {} after {attempts} attempts: {error}",
        path.display()
    )))
}

fn preserve_copied_file_times(
    from_path: &Path,
    to_path: &Path,
    source_metadata: &fs::Metadata,
) -> CoreResult<()> {
    let accessed = source_metadata.accessed().map_err(|error| {
        CoreError::io(format!(
            "failed to read source launch pack access time {}: {error}",
            from_path.display()
        ))
    })?;
    let modified = source_metadata.modified().map_err(|error| {
        CoreError::io(format!(
            "failed to read source launch pack modified time {}: {error}",
            from_path.display()
        ))
    })?;
    let copied_file = fs::OpenOptions::new()
        .write(true)
        .open(to_path)
        .map_err(|error| {
            CoreError::io(format!(
                "failed to open copied launch pack {} for timestamp preservation: {error}",
                to_path.display()
            ))
        })?;
    copied_file
        .set_times(
            FileTimes::new()
                .set_accessed(accessed)
                .set_modified(modified),
        )
        .map_err(|error| {
            CoreError::io(format!(
                "failed to preserve copied launch pack timestamps {} from {}: {error}",
                to_path.display(),
                from_path.display()
            ))
        })
}

fn write_mod_list_with_fallback(
    working_dir: &Path,
    primary_file_name: &str,
    fallback_file_name: &str,
    contents: &str,
) -> CoreResult<(String, PathBuf)> {
    fs::create_dir_all(working_dir).map_err(|error| {
        CoreError::io(format!(
            "failed to create launch working directory {}: {error}",
            working_dir.display()
        ))
    })?;

    let primary_path = working_dir.join(primary_file_name);
    match fs::write(&primary_path, contents) {
        Ok(()) => Ok((primary_file_name.to_string(), primary_path)),
        Err(primary_error) => {
            let fallback_path = working_dir.join(fallback_file_name);
            fs::write(&fallback_path, contents).map_err(|fallback_error| {
                CoreError::io(format!(
                    "failed to write launch mod list {} ({primary_error}); fallback {} also failed: {fallback_error}",
                    primary_path.display(),
                    fallback_path.display()
                ))
            })?;
            Ok((fallback_file_name.to_string(), fallback_path))
        }
    }
}

fn args_for_mod_list_file(plan: &WindowsLaunchPlan, mod_list_file_name: &str) -> Vec<String> {
    let old_arg = format!("{};", plan.mod_list_file_name);
    let new_arg = format!("{mod_list_file_name};");
    plan.args
        .iter()
        .map(|arg| {
            if arg == &old_arg {
                new_arg.clone()
            } else {
                arg.clone()
            }
        })
        .collect()
}

fn steam_apps_dir_from_game_dir(game_dir: &Path) -> CoreResult<PathBuf> {
    let common_dir = game_dir.parent().ok_or_else(|| {
        CoreError::invalid_input(format!("game folder has no parent: {}", game_dir.display()))
    })?;
    if !common_dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("common"))
    {
        return Err(CoreError::invalid_input(format!(
            "game folder is not under a Steam common directory: {}",
            game_dir.display()
        )));
    }

    let steam_apps_dir = common_dir.parent().ok_or_else(|| {
        CoreError::invalid_input(format!(
            "Steam common directory has no parent: {}",
            common_dir.display()
        ))
    })?;
    if !steam_apps_dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("steamapps"))
    {
        return Err(CoreError::invalid_input(format!(
            "Steam common directory is not under steamapps: {}",
            common_dir.display()
        )));
    }

    Ok(steam_apps_dir.to_path_buf())
}

fn discover_wh3_steam_install_from_libraries(
    libraries: &[SteamLibraryFolder],
) -> CoreResult<Wh3SteamInstall> {
    let mut candidates = Vec::new();
    for library in libraries {
        let appmanifest_path = library
            .steam_apps_dir
            .join(format!("appmanifest_{WH3_STEAM_APP_ID}.acf"));
        if !appmanifest_path.is_file() {
            continue;
        }
        candidates.push(appmanifest_path.display().to_string());

        let install_dir_name = wh3_install_dir_from_appmanifest(&appmanifest_path)?;
        let game_dir = library.steam_apps_dir.join("common").join(install_dir_name);
        let validated = validate_wh3_game_folder(&game_dir)?;
        let workshop_content_dir = library
            .steam_apps_dir
            .join("workshop")
            .join("content")
            .join(WH3_STEAM_APP_ID);

        return Ok(Wh3SteamInstall {
            steam_apps_dir: library.steam_apps_dir.clone(),
            appmanifest_path,
            game_dir: validated.game_dir,
            data_dir: validated.data_dir,
            executable_path: validated.executable_path,
            workshop_content_dir: workshop_content_dir
                .is_dir()
                .then_some(workshop_content_dir),
        });
    }

    if candidates.is_empty() {
        return Err(CoreError::invalid_input(format!(
            "no appmanifest_{WH3_STEAM_APP_ID}.acf found in Steam libraries"
        )));
    }

    Err(CoreError::invalid_input(format!(
        "no valid WH3 install found for manifests: {}",
        candidates.join(", ")
    )))
}

fn wh3_install_dir_from_appmanifest(appmanifest_path: &Path) -> CoreResult<String> {
    let text = fs::read_to_string(appmanifest_path).map_err(|error| {
        CoreError::io(format!(
            "failed to read WH3 appmanifest {}: {error}",
            appmanifest_path.display()
        ))
    })?;
    let parsed = parse_vdf(&text)?;
    let app_state = parsed.get_object("AppState").ok_or_else(|| {
        CoreError::parse(format!(
            "WH3 appmanifest {} is missing AppState",
            appmanifest_path.display()
        ))
    })?;

    if let Some(appid) = vdf_object_get_str(app_state, "appid")
        && appid != WH3_STEAM_APP_ID
    {
        return Err(CoreError::invalid_input(format!(
            "WH3 appmanifest {} has unexpected appid {appid}",
            appmanifest_path.display()
        )));
    }

    Ok(vdf_object_get_str(app_state, "installdir")
        .filter(|install_dir| !install_dir.trim().is_empty())
        .unwrap_or("Total War WARHAMMER III")
        .to_string())
}

fn steam_library_path_from_vdf_value(value: &VdfValue) -> Option<String> {
    match value {
        VdfValue::String(path) => Some(path.clone()),
        VdfValue::Object(fields) => vdf_object_get_str(fields, "path").map(ToString::to_string),
    }
}

fn push_unique_library(libraries: &mut Vec<SteamLibraryFolder>, library_dir: PathBuf) {
    let steam_apps_dir = library_dir.join("steamapps");
    if libraries
        .iter()
        .any(|library| library.steam_apps_dir == steam_apps_dir)
    {
        return;
    }

    libraries.push(SteamLibraryFolder {
        library_dir,
        steam_apps_dir,
    });
}

fn vdf_object_get_str<'a>(fields: &'a [(String, VdfValue)], key: &str) -> Option<&'a str> {
    fields.iter().find_map(|(field_key, value)| {
        if field_key.eq_ignore_ascii_case(key) {
            match value {
                VdfValue::String(value) => Some(value.as_str()),
                VdfValue::Object(_) => None,
            }
        } else {
            None
        }
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum VdfValue {
    String(String),
    Object(Vec<(String, VdfValue)>),
}

impl VdfValue {
    fn get_object(&self, key: &str) -> Option<&[(String, VdfValue)]> {
        match self {
            Self::Object(fields) => fields.iter().find_map(|(field_key, value)| {
                if field_key.eq_ignore_ascii_case(key) {
                    match value {
                        Self::Object(fields) => Some(fields.as_slice()),
                        Self::String(_) => None,
                    }
                } else {
                    None
                }
            }),
            Self::String(_) => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum VdfToken {
    String(String),
    OpenBrace,
    CloseBrace,
}

fn parse_vdf(text: &str) -> CoreResult<VdfValue> {
    let tokens = tokenize_vdf(text)?;
    let mut index = 0;
    let object = parse_vdf_object(&tokens, &mut index, false)?;
    if index != tokens.len() {
        return Err(CoreError::parse("unexpected trailing tokens in VDF"));
    }
    Ok(VdfValue::Object(object))
}

fn parse_vdf_object(
    tokens: &[VdfToken],
    index: &mut usize,
    stop_at_close_brace: bool,
) -> CoreResult<Vec<(String, VdfValue)>> {
    let mut fields = Vec::new();
    while *index < tokens.len() {
        match &tokens[*index] {
            VdfToken::CloseBrace if stop_at_close_brace => {
                *index += 1;
                return Ok(fields);
            }
            VdfToken::CloseBrace => {
                return Err(CoreError::parse("unexpected closing brace in VDF"));
            }
            VdfToken::OpenBrace => return Err(CoreError::parse("unexpected opening brace in VDF")),
            VdfToken::String(key) => {
                let key = key.clone();
                *index += 1;
                let value = match tokens.get(*index) {
                    Some(VdfToken::String(value)) => {
                        *index += 1;
                        VdfValue::String(value.clone())
                    }
                    Some(VdfToken::OpenBrace) => {
                        *index += 1;
                        VdfValue::Object(parse_vdf_object(tokens, index, true)?)
                    }
                    Some(VdfToken::CloseBrace) => {
                        return Err(CoreError::parse(format!(
                            "VDF key {key} is missing a value before closing brace"
                        )));
                    }
                    None => {
                        return Err(CoreError::parse(format!(
                            "VDF key {key} is missing a value"
                        )));
                    }
                };
                fields.push((key, value));
            }
        }
    }

    if stop_at_close_brace {
        Err(CoreError::parse("unterminated VDF object"))
    } else {
        Ok(fields)
    }
}

fn tokenize_vdf(text: &str) -> CoreResult<Vec<VdfToken>> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        match ch {
            ch if ch.is_whitespace() => {}
            '/' if chars.peek().is_some_and(|(_, next)| *next == '/') => {
                chars.next();
                for (_, comment_ch) in chars.by_ref() {
                    if comment_ch == '\n' {
                        break;
                    }
                }
            }
            '{' => tokens.push(VdfToken::OpenBrace),
            '}' => tokens.push(VdfToken::CloseBrace),
            '"' => tokens.push(VdfToken::String(read_vdf_quoted_string(&mut chars)?)),
            other => {
                return Err(CoreError::parse(format!(
                    "unexpected character {other:?} in VDF"
                )));
            }
        }
    }
    Ok(tokens)
}

fn read_vdf_quoted_string<I>(chars: &mut std::iter::Peekable<I>) -> CoreResult<String>
where
    I: Iterator<Item = (usize, char)>,
{
    let mut value = String::new();
    while let Some((_, ch)) = chars.next() {
        match ch {
            '"' => return Ok(value),
            '\\' => {
                if let Some((_, escaped)) = chars.next() {
                    value.push(escaped);
                } else {
                    value.push('\\');
                }
            }
            _ => value.push(ch),
        }
    }

    Err(CoreError::parse("unterminated quoted string in VDF"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::fs::FileTimes;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime};

    use wh3mm_core::{
        ModIdentity, ModRecord, PreLaunchCopyOperation, PreLaunchPackWrite,
        SteamWorkshopAdapterError, SteamWorkshopAdapterErrorKind, SteamWorkshopMetadataAdapter,
        WindowsLaunchPlan, WorkshopModData,
    };

    #[cfg(not(windows))]
    use super::discover_steam_root_from_windows_registry;
    use super::{
        LaunchPreparationOptions, SteamResubscribeSafetyConfig, SteamWorkshopCheckStateResult,
        SteamWorkshopCommandAdapter, SteamWorkshopCommandResult, SteamWorkshopCommandRunner,
        SteamWorkshopCommandSafetyConfig, TsSteamHelperMetadataAdapter, TsSteamHelperRunner,
        WH3_STEAM_APP_ID, WrittenPackFile, discover_wh3_steam_install_from_steam_root,
        discover_wh3_workshop_folder, executable_path_for_prepared_launch,
        parse_steam_install_path_from_reg_query_output,
        parse_steam_libraries_from_libraryfolders_vdf, prepare_windows_launch_files,
        remove_loaded_workshop_mod_dirs, resubscribe_with_cleanup_and_verification,
        validate_wh3_game_folder, validate_windows_game_folder,
    };
    #[cfg(unix)]
    use super::{SteamWorkshopHelperProcessConfig, SteamWorkshopHelperProcessRunner};

    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn ts_steam_helper_adapter_resolves_dependency_titles() {
        let runner = RecordingTsSteamHelperRunner {
            mods_data_json: r#"
                {
                  "mods": [
                    {
                      "publishedFileId": "111",
                      "title": "Main Mod",
                      "owner": { "steamId64": "76561198000000001" },
                      "timeUpdated": 1234
                    }
                  ],
                  "dependencies": {
                    "111": ["222"]
                  },
                  "authors": {
                    "76561198000000001": "Mod Author"
                  }
                }
            "#
            .to_string(),
            items_json: Some(
                r#"
                    [
                      {
                        "publishedFileId": "222",
                        "title": "Dependency Mod",
                        "owner": { "steamId64": "76561198000000002" },
                        "timeUpdated": 999
                      }
                    ]
                "#
                .to_string(),
            ),
            requested_mod_batches: Vec::new(),
            requested_item_batches: Vec::new(),
        };
        let mut adapter = TsSteamHelperMetadataAdapter::new(WH3_STEAM_APP_ID, runner);

        let data = adapter.fetch_mod_data_batch(&["111".to_string()]).unwrap();
        let runner = adapter.into_runner();

        assert_eq!(runner.requested_mod_batches, vec![vec!["111"]]);
        assert_eq!(runner.requested_item_batches, vec![vec!["222"]]);
        assert_eq!(
            data,
            vec![WorkshopModData {
                workshop_id: "111".to_string(),
                title: "Main Mod".to_string(),
                author: "Mod Author".to_string(),
                description: String::new(),
                tags: Vec::new(),
                dependency_ids: vec!["222".to_string()],
                dependency_id_to_name: vec![("222".to_string(), "Dependency Mod".to_string())],
                last_changed_ms: 1_234_000,
            }]
        );
    }

    #[test]
    fn ts_steam_helper_adapter_skips_dependency_lookup_when_not_needed() {
        let runner = RecordingTsSteamHelperRunner {
            mods_data_json: r#"
                {
                  "mods": [
                    {
                      "publishedFileId": "111",
                      "title": "Main Mod",
                      "owner": { "steamId64": "76561198000000001" },
                      "timeUpdated": 1234
                    }
                  ],
                  "dependencies": {
                    "111": ["2845454582"]
                  },
                  "authors": {}
                }
            "#
            .to_string(),
            items_json: None,
            requested_mod_batches: Vec::new(),
            requested_item_batches: Vec::new(),
        };
        let mut adapter = TsSteamHelperMetadataAdapter::new(WH3_STEAM_APP_ID, runner);

        let data = adapter.fetch_mod_data_batch(&["111".to_string()]).unwrap();
        let runner = adapter.into_runner();

        assert!(runner.requested_item_batches.is_empty());
        assert!(data[0].dependency_ids.is_empty());
    }

    #[test]
    fn steam_command_adapter_normalizes_subscribed_ids() {
        let runner = RecordingWorkshopCommandRunner {
            subscribed_ids: vec![
                " 111 ".to_string(),
                "abc".to_string(),
                "111".to_string(),
                "222".to_string(),
            ],
            ..RecordingWorkshopCommandRunner::default()
        };
        let mut adapter = SteamWorkshopCommandAdapter::new(WH3_STEAM_APP_ID, runner);

        let subscribed_ids = adapter.subscribed_workshop_ids().unwrap();
        let runner = adapter.into_runner();

        assert_eq!(subscribed_ids, ["111", "222"]);
        assert_eq!(
            runner.get_subscribed_calls,
            vec![WH3_STEAM_APP_ID.to_string()]
        );
    }

    #[test]
    fn steam_command_adapter_dedupes_command_ids_and_skips_empty_batches() {
        let runner = RecordingWorkshopCommandRunner::default();
        let mut adapter = SteamWorkshopCommandAdapter::with_config(
            WH3_STEAM_APP_ID,
            SteamWorkshopCommandSafetyConfig {
                command_delay: Duration::from_millis(123),
            },
            runner,
        );

        let subscribed = adapter
            .subscribe(&[
                "111".to_string(),
                "bad".to_string(),
                "111".to_string(),
                "222".to_string(),
            ])
            .unwrap();
        let downloaded = adapter
            .download(&["  ".to_string(), "not-a-number".to_string()])
            .unwrap();
        let unsubscribed = adapter.unsubscribe(&["333".to_string()]).unwrap();
        let checked = adapter
            .check_state_and_download_updates(&["444".to_string(), "444".to_string()])
            .unwrap();
        let runner = adapter.into_runner();

        assert_eq!(
            subscribed,
            SteamWorkshopCommandResult::requested(
                "sub",
                vec!["111".to_string(), "222".to_string()]
            )
        );
        assert_eq!(
            downloaded,
            SteamWorkshopCommandResult::requested("download", Vec::new())
        );
        assert_eq!(
            unsubscribed,
            SteamWorkshopCommandResult::requested("unsubscribe", vec!["333".to_string()])
        );
        assert_eq!(
            checked,
            SteamWorkshopCheckStateResult {
                checked_ids: vec!["444".to_string()],
                update_requested_ids: Vec::new(),
            }
        );
        assert_eq!(
            runner.subscribe_calls,
            vec![(
                WH3_STEAM_APP_ID.to_string(),
                vec!["111".to_string(), "222".to_string()],
                Duration::from_millis(123)
            )]
        );
        assert!(runner.download_calls.is_empty());
        assert_eq!(
            runner.unsubscribe_calls,
            vec![(WH3_STEAM_APP_ID.to_string(), vec!["333".to_string()])]
        );
        assert_eq!(
            runner.check_state_calls,
            vec![(
                WH3_STEAM_APP_ID.to_string(),
                vec!["444".to_string()],
                Duration::from_millis(123)
            )]
        );
    }

    #[test]
    fn steam_command_adapter_defaults_to_ts_command_delay() {
        let runner = RecordingWorkshopCommandRunner::default();
        let adapter = SteamWorkshopCommandAdapter::new(WH3_STEAM_APP_ID, runner);

        assert_eq!(adapter.config().command_delay, Duration::from_millis(250));
    }

    #[cfg(unix)]
    #[test]
    fn steam_helper_process_runner_reads_json_and_records_commands() {
        let root = temp_root("steam-helper-process");
        fs::create_dir_all(&root).unwrap();
        let log_path = root.join("helper.log");
        let helper_path = root.join("fake-steam-helper.sh");
        write_unix_helper_script(
            &helper_path,
            &format!(
                r#"#!/bin/sh
cmd="$2"
payload="${{3:-}}"
delay="${{4:-}}"
case "$cmd" in
  probe)
    echo '{{"selectedBackend":"fixture","nativeAvailable":false}}'
    ;;
  getSubscribedIds)
    echo "helper log"
    echo '["111","bad","222"]'
    ;;
  getModsData)
    echo '{{"mods":[{{"publishedFileId":"111","title":"Main Mod","owner":{{"steamId64":"76561198000000001"}},"timeUpdated":1234}}],"dependencies":{{"111":[]}},"authors":{{"76561198000000001":"Mod Author"}}}}'
    ;;
  getItems)
    echo '[]'
    ;;
  getDependencies)
    echo '{{"111":["222"]}}'
    ;;
  getAuthors)
    echo '{{"76561198000000001":"Mod Author"}}'
    ;;
  sub|download|unsubscribe|checkState)
    echo "$cmd|$payload|$delay" >> '{}'
    if [ "$cmd" = "checkState" ]; then
      echo '{{"ok":true,"command":"checkState","ids":["444"],"updateRequestedIds":["444"],"delayMs":123}}'
    elif [ "$cmd" = "sub" ]; then
      echo '{{"ok":true,"command":"sub","ids":["111"],"delayMs":123}}'
    elif [ "$cmd" = "download" ]; then
      echo '{{"ok":true,"command":"download","ids":["222"],"delayMs":123}}'
    else
      echo '{{"ok":true,"command":"unsubscribe","ids":["333"],"delayMs":null}}'
    fi
    ;;
  *)
    exit 7
    ;;
esac
"#,
                log_path.display()
            ),
        );

        let mut subscribed_adapter = SteamWorkshopCommandAdapter::new(
            WH3_STEAM_APP_ID,
            SteamWorkshopHelperProcessRunner::new(&helper_path),
        );
        let subscribed_ids = subscribed_adapter.subscribed_workshop_ids().unwrap();

        let mut metadata_adapter = TsSteamHelperMetadataAdapter::new(
            WH3_STEAM_APP_ID,
            SteamWorkshopHelperProcessRunner::new(&helper_path),
        );
        let metadata = metadata_adapter
            .fetch_mod_data_batch(&["111".to_string()])
            .unwrap();
        let mut protocol_runner = SteamWorkshopHelperProcessRunner::new(&helper_path);
        let dependencies_json = protocol_runner
            .get_dependencies_json(WH3_STEAM_APP_ID, &["111".to_string()])
            .unwrap();
        let authors_json = protocol_runner
            .get_authors_json(WH3_STEAM_APP_ID, &["76561198000000001".to_string()])
            .unwrap();
        let probe_json = protocol_runner.probe_json(WH3_STEAM_APP_ID).unwrap();

        let mut command_adapter = SteamWorkshopCommandAdapter::with_config(
            WH3_STEAM_APP_ID,
            SteamWorkshopCommandSafetyConfig {
                command_delay: Duration::from_millis(123),
            },
            SteamWorkshopHelperProcessRunner::new(&helper_path),
        );
        let subscribe_result = command_adapter.subscribe(&["111".to_string()]).unwrap();
        let download_result = command_adapter.download(&["222".to_string()]).unwrap();
        let unsubscribe_result = command_adapter.unsubscribe(&["333".to_string()]).unwrap();
        let check_state_result = command_adapter
            .check_state_and_download_updates(&["444".to_string()])
            .unwrap();

        assert_eq!(subscribed_ids, ["111", "222"]);
        assert_eq!(
            metadata,
            vec![WorkshopModData {
                workshop_id: "111".to_string(),
                title: "Main Mod".to_string(),
                author: "Mod Author".to_string(),
                description: String::new(),
                tags: Vec::new(),
                dependency_ids: Vec::new(),
                dependency_id_to_name: Vec::new(),
                last_changed_ms: 1_234_000,
            }]
        );
        assert_eq!(dependencies_json.as_deref(), Some(r#"{"111":["222"]}"#));
        assert_eq!(
            authors_json.as_deref(),
            Some(r#"{"76561198000000001":"Mod Author"}"#)
        );
        assert_eq!(
            probe_json,
            r#"{"selectedBackend":"fixture","nativeAvailable":false}"#
        );
        assert_eq!(
            subscribe_result,
            SteamWorkshopCommandResult {
                command: "sub".to_string(),
                requested_ids: vec!["111".to_string()],
                confirmed_ids: vec!["111".to_string()],
                update_requested_ids: Vec::new(),
                delay_ms: Some(123),
            }
        );
        assert_eq!(
            download_result,
            SteamWorkshopCommandResult {
                command: "download".to_string(),
                requested_ids: vec!["222".to_string()],
                confirmed_ids: vec!["222".to_string()],
                update_requested_ids: Vec::new(),
                delay_ms: Some(123),
            }
        );
        assert_eq!(
            unsubscribe_result,
            SteamWorkshopCommandResult {
                command: "unsubscribe".to_string(),
                requested_ids: vec!["333".to_string()],
                confirmed_ids: vec!["333".to_string()],
                update_requested_ids: Vec::new(),
                delay_ms: None,
            }
        );
        assert_eq!(
            check_state_result,
            SteamWorkshopCheckStateResult {
                checked_ids: vec!["444".to_string()],
                update_requested_ids: vec!["444".to_string()],
            }
        );
        assert_eq!(
            fs::read_to_string(&log_path).unwrap(),
            "sub|111|123\ndownload|222|123\nunsubscribe|333|\ncheckState|444|123\n"
        );

        fs::remove_dir_all(root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn steam_helper_process_runner_tolerates_command_without_json_detail() {
        let root = temp_root("steam-helper-command-no-json");
        fs::create_dir_all(&root).unwrap();
        let helper_path = root.join("fake-steam-helper.sh");
        write_unix_helper_script(
            &helper_path,
            r#"#!/bin/sh
if [ "$2" = "download" ]; then
  echo "download"
  exit 0
fi
exit 7
"#,
        );
        let mut command_adapter = SteamWorkshopCommandAdapter::new(
            WH3_STEAM_APP_ID,
            SteamWorkshopHelperProcessRunner::new(&helper_path),
        );

        let result = command_adapter
            .download(&["111".to_string(), "111".to_string()])
            .unwrap();

        assert_eq!(
            result,
            SteamWorkshopCommandResult::requested("download", vec!["111".to_string()])
        );

        fs::remove_dir_all(root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn steam_helper_process_runner_tolerates_check_state_without_json_detail() {
        let root = temp_root("steam-helper-check-state-no-json");
        fs::create_dir_all(&root).unwrap();
        let helper_path = root.join("fake-steam-helper.sh");
        write_unix_helper_script(
            &helper_path,
            r#"#!/bin/sh
if [ "$2" = "checkState" ]; then
  echo "checkState"
  exit 0
fi
exit 7
"#,
        );
        let mut command_adapter = SteamWorkshopCommandAdapter::new(
            WH3_STEAM_APP_ID,
            SteamWorkshopHelperProcessRunner::new(&helper_path),
        );

        let result = command_adapter
            .check_state_and_download_updates(&["111".to_string(), "111".to_string()])
            .unwrap();

        assert_eq!(
            result,
            SteamWorkshopCheckStateResult {
                checked_ids: vec!["111".to_string()],
                update_requested_ids: Vec::new(),
            }
        );

        fs::remove_dir_all(root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn steam_helper_process_runner_passes_environment_overrides() {
        let root = temp_root("steam-helper-env");
        fs::create_dir_all(&root).unwrap();
        let helper_path = root.join("env-steam-helper.sh");
        write_unix_helper_script(
            &helper_path,
            r#"#!/bin/sh
echo "{\"selectedBackend\":\"${WH3MM_STEAM_HELPER_BACKEND}\",\"fixture\":\"${WH3MM_STEAM_HELPER_FIXTURE}\"}"
"#,
        );
        let mut runner = SteamWorkshopHelperProcessRunner::with_config(
            &helper_path,
            SteamWorkshopHelperProcessConfig {
                timeout: Duration::from_secs(5),
                env_overrides: vec![
                    (
                        "WH3MM_STEAM_HELPER_BACKEND".to_string(),
                        "fixture".to_string(),
                    ),
                    (
                        "WH3MM_STEAM_HELPER_FIXTURE".to_string(),
                        "fixture.json".to_string(),
                    ),
                ],
            },
        );

        let probe_json = runner.probe_json(WH3_STEAM_APP_ID).unwrap();

        assert_eq!(
            probe_json,
            r#"{"selectedBackend":"fixture","fixture":"fixture.json"}"#
        );

        fs::remove_dir_all(root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn steam_helper_process_runner_times_out() {
        let root = temp_root("steam-helper-timeout");
        fs::create_dir_all(&root).unwrap();
        let helper_path = root.join("slow-steam-helper.sh");
        write_unix_helper_script(
            &helper_path,
            r#"#!/bin/sh
sleep 1
echo '[]'
"#,
        );
        let mut runner = SteamWorkshopHelperProcessRunner::with_config(
            &helper_path,
            SteamWorkshopHelperProcessConfig {
                timeout: Duration::from_millis(20),
                env_overrides: Vec::new(),
            },
        );

        let error = runner.get_subscribed_ids(WH3_STEAM_APP_ID).unwrap_err();

        assert_eq!(error.kind, SteamWorkshopAdapterErrorKind::Unavailable);
        assert!(error.message.contains("timed out"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn removes_loaded_workshop_mod_dirs_for_requested_ids() {
        let root = temp_root("workshop-cleanup");
        let workshop_dir = root.join("Steam/steamapps/workshop/content/1142710/111");
        fs::create_dir_all(&workshop_dir).unwrap();
        let pack_path = workshop_dir.join("main.pack");
        fs::write(&pack_path, b"pack").unwrap();
        let mods = vec![mod_record_with_path(
            &pack_path,
            Some("111"),
            &["workshop", "steam"],
        )];

        let cleanup =
            remove_loaded_workshop_mod_dirs(&mods, &["111".to_string(), "111".to_string()])
                .unwrap();

        assert_eq!(cleanup.requested_ids, ["111"]);
        assert_eq!(cleanup.removed_dirs, [workshop_dir.clone()]);
        assert!(!workshop_dir.exists());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn workshop_cleanup_skips_paths_outside_steam_workshop_layout() {
        let root = temp_root("workshop-cleanup-skip");
        let local_dir = root.join("mods/111");
        fs::create_dir_all(&local_dir).unwrap();
        let pack_path = local_dir.join("main.pack");
        fs::write(&pack_path, b"pack").unwrap();
        let mods = vec![mod_record_with_path(&pack_path, Some("111"), &["local"])];

        let cleanup = remove_loaded_workshop_mod_dirs(&mods, &["111".to_string()]).unwrap();

        assert_eq!(cleanup.requested_ids, ["111"]);
        assert!(cleanup.removed_dirs.is_empty());
        assert!(local_dir.exists());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn resubscribe_retries_only_missing_ids_until_verified() {
        let root = temp_root("resubscribe-retry");
        let workshop_dir_111 = root.join("Steam/steamapps/workshop/content/1142710/111");
        let workshop_dir_222 = root.join("Steam/steamapps/workshop/content/1142710/222");
        fs::create_dir_all(&workshop_dir_111).unwrap();
        fs::create_dir_all(&workshop_dir_222).unwrap();
        let pack_path_111 = workshop_dir_111.join("main.pack");
        let pack_path_222 = workshop_dir_222.join("main.pack");
        fs::write(&pack_path_111, b"pack").unwrap();
        fs::write(&pack_path_222, b"pack").unwrap();
        let mods = vec![
            mod_record_with_path(&pack_path_111, Some("111"), &["workshop", "steam"]),
            mod_record_with_path(&pack_path_222, Some("222"), &["workshop", "steam"]),
        ];
        let runner = SequencedWorkshopCommandRunner::new(vec![
            vec!["111".to_string()],
            vec!["111".to_string(), "222".to_string()],
        ]);
        let mut adapter = SteamWorkshopCommandAdapter::with_config(
            WH3_STEAM_APP_ID,
            SteamWorkshopCommandSafetyConfig {
                command_delay: Duration::from_millis(7),
            },
            runner,
        );

        let result = resubscribe_with_cleanup_and_verification(
            &mods,
            &["111".to_string(), "222".to_string(), "222".to_string()],
            &mut adapter,
            &zero_delay_resubscribe_config(3),
        )
        .unwrap();
        let runner = adapter.into_runner();

        assert_eq!(result.requested_ids, ["111", "222"]);
        assert_eq!(
            result.removed_dirs,
            [workshop_dir_111.clone(), workshop_dir_222.clone()]
        );
        assert_eq!(result.observed_subscribed_ids, ["111", "222"]);
        assert!(result.failed_ids.is_empty());
        assert_eq!(result.attempts, 2);
        assert_eq!(
            runner.unsubscribe_calls,
            vec![(
                WH3_STEAM_APP_ID.to_string(),
                vec!["111".to_string(), "222".to_string()]
            )]
        );
        assert_eq!(
            runner.subscribe_calls,
            vec![
                (
                    WH3_STEAM_APP_ID.to_string(),
                    vec!["111".to_string(), "222".to_string()],
                    Duration::from_millis(7),
                ),
                (
                    WH3_STEAM_APP_ID.to_string(),
                    vec!["222".to_string()],
                    Duration::from_millis(7),
                ),
            ]
        );
        assert_eq!(
            runner.download_calls,
            vec![
                (
                    WH3_STEAM_APP_ID.to_string(),
                    vec!["111".to_string(), "222".to_string()],
                    Duration::from_millis(7),
                ),
                (
                    WH3_STEAM_APP_ID.to_string(),
                    vec!["222".to_string()],
                    Duration::from_millis(7),
                ),
            ]
        );
        assert_eq!(
            runner.get_subscribed_calls,
            vec![WH3_STEAM_APP_ID.to_string(), WH3_STEAM_APP_ID.to_string()]
        );
        assert!(!workshop_dir_111.exists());
        assert!(!workshop_dir_222.exists());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn resubscribe_reports_failed_ids_after_attempt_limit() {
        let runner = SequencedWorkshopCommandRunner::new(vec![
            vec!["111".to_string()],
            vec!["111".to_string()],
        ]);
        let mut adapter = SteamWorkshopCommandAdapter::new(WH3_STEAM_APP_ID, runner);

        let result = resubscribe_with_cleanup_and_verification(
            &[],
            &["111".to_string(), "222".to_string()],
            &mut adapter,
            &zero_delay_resubscribe_config(2),
        )
        .unwrap();
        let runner = adapter.into_runner();

        assert_eq!(result.requested_ids, ["111", "222"]);
        assert_eq!(result.observed_subscribed_ids, ["111"]);
        assert_eq!(result.failed_ids, ["222"]);
        assert_eq!(result.attempts, 2);
        assert_eq!(
            runner.subscribe_calls[1].1,
            vec!["222".to_string()],
            "second attempt should retry only the missing ID"
        );
    }

    #[test]
    fn writes_primary_mod_list_and_copies_prelaunch_files() {
        let root = temp_root("primary");
        let source_dir = root.join("source");
        let game_dir = root.join("game");
        let data_dir = game_dir.join("data");
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        let source_pack = source_dir.join("local.pack");
        let target_pack = data_dir.join("local.pack");
        fs::write(&source_pack, b"pack bytes").unwrap();
        let plan = launch_plan(
            &game_dir,
            "used_mods.txt",
            r#"mod "local.pack";"#,
            vec![PreLaunchCopyOperation {
                from_path: source_pack.display().to_string(),
                to_path: target_pack.display().to_string(),
            }],
        );

        let prepared =
            prepare_windows_launch_files(&plan, &LaunchPreparationOptions::default()).unwrap();

        assert_eq!(prepared.mod_list_path, game_dir.join("used_mods.txt"));
        assert_eq!(
            fs::read_to_string(&prepared.mod_list_path).unwrap(),
            r#"mod "local.pack";"#
        );
        assert_eq!(fs::read(&target_pack).unwrap(), b"pack bytes");
        assert_eq!(prepared.args, ["used_mods.txt;"]);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn writes_generated_prelaunch_pack_files() {
        let root = temp_root("generated-pack");
        let game_dir = root.join("game");
        let generated_pack = root.join("tempPacks/!!!!out.pack");
        let mut plan = launch_plan(
            &game_dir,
            "used_mods.txt",
            r#"mod "!!!!out.pack";"#,
            Vec::new(),
        );
        plan.pre_launch_pack_writes = vec![PreLaunchPackWrite {
            path: generated_pack.display().to_string(),
            bytes: vec![1, 2, 3, 4],
            packed_file_names: vec!["script\\enable_console_logging".to_string()],
        }];

        let prepared =
            prepare_windows_launch_files(&plan, &LaunchPreparationOptions::default()).unwrap();

        assert_eq!(fs::read(&generated_pack).unwrap(), [1, 2, 3, 4]);
        assert_eq!(
            prepared.written_pack_files,
            [WrittenPackFile {
                path: generated_pack,
                byte_len: 4,
            }]
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn retries_generated_prelaunch_pack_write_after_transient_failure() {
        let root = temp_root("generated-pack-retry");
        let game_dir = root.join("game");
        let generated_pack = root.join("tempPacks/!!!!out.pack");
        fs::create_dir_all(&generated_pack).unwrap();
        let mut plan = launch_plan(
            &game_dir,
            "used_mods.txt",
            r#"mod "!!!!out.pack";"#,
            Vec::new(),
        );
        plan.pre_launch_pack_writes = vec![PreLaunchPackWrite {
            path: generated_pack.display().to_string(),
            bytes: vec![5, 6, 7, 8],
            packed_file_names: vec!["script\\enable_console_logging".to_string()],
        }];
        let unblock_path = generated_pack.clone();
        let unblock = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            fs::remove_dir_all(unblock_path).unwrap();
        });
        let options = LaunchPreparationOptions {
            generated_pack_write_attempts: 20,
            generated_pack_write_retry_delay: Duration::from_millis(5),
            ..LaunchPreparationOptions::default()
        };

        let result = prepare_windows_launch_files(&plan, &options);
        unblock.join().unwrap();
        let prepared = result.unwrap();

        assert_eq!(fs::read(&generated_pack).unwrap(), [5, 6, 7, 8]);
        assert_eq!(
            prepared.written_pack_files,
            [WrittenPackFile {
                path: generated_pack,
                byte_len: 4,
            }]
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn reports_generated_prelaunch_pack_write_attempt_count() {
        let root = temp_root("generated-pack-retry-failed");
        let game_dir = root.join("game");
        let generated_pack = root.join("tempPacks/!!!!out.pack");
        fs::create_dir_all(&generated_pack).unwrap();
        let mut plan = launch_plan(
            &game_dir,
            "used_mods.txt",
            r#"mod "!!!!out.pack";"#,
            Vec::new(),
        );
        plan.pre_launch_pack_writes = vec![PreLaunchPackWrite {
            path: generated_pack.display().to_string(),
            bytes: vec![5, 6, 7, 8],
            packed_file_names: vec!["script\\enable_console_logging".to_string()],
        }];
        let options = LaunchPreparationOptions {
            generated_pack_write_attempts: 2,
            generated_pack_write_retry_delay: Duration::ZERO,
            ..LaunchPreparationOptions::default()
        };

        let error = prepare_windows_launch_files(&plan, &options).unwrap_err();

        assert!(error.message.contains("generated launch pack"));
        assert!(error.message.contains("after 2 attempts"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn preserves_prelaunch_copy_source_timestamps() {
        let root = temp_root("copy-times");
        let source_dir = root.join("source");
        let game_dir = root.join("game");
        let data_dir = game_dir.join("data");
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        let source_pack = source_dir.join("local.pack");
        let target_pack = data_dir.join("local.pack");
        fs::write(&source_pack, b"pack bytes").unwrap();
        let source_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        fs::OpenOptions::new()
            .write(true)
            .open(&source_pack)
            .unwrap()
            .set_times(
                FileTimes::new()
                    .set_accessed(source_time)
                    .set_modified(source_time),
            )
            .unwrap();
        let plan = launch_plan(
            &game_dir,
            "used_mods.txt",
            r#"mod "local.pack";"#,
            vec![PreLaunchCopyOperation {
                from_path: source_pack.display().to_string(),
                to_path: target_pack.display().to_string(),
            }],
        );

        prepare_windows_launch_files(&plan, &LaunchPreparationOptions::default()).unwrap();

        let copied_metadata = fs::metadata(&target_pack).unwrap();
        assert_eq!(copied_metadata.modified().unwrap(), source_time);
        assert_eq!(copied_metadata.accessed().unwrap(), source_time);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rejects_prelaunch_copy_when_target_is_newer_than_source() {
        let root = temp_root("copy-newer-target");
        let source_dir = root.join("source");
        let game_dir = root.join("game");
        let data_dir = game_dir.join("data");
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        let source_pack = source_dir.join("local.pack");
        let target_pack = data_dir.join("local.pack");
        fs::write(&source_pack, b"older pack bytes").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&target_pack, b"newer pack bytes").unwrap();
        let plan = launch_plan(
            &game_dir,
            "used_mods.txt",
            r#"mod "local.pack";"#,
            vec![PreLaunchCopyOperation {
                from_path: source_pack.display().to_string(),
                to_path: target_pack.display().to_string(),
            }],
        );

        let error =
            prepare_windows_launch_files(&plan, &LaunchPreparationOptions::default()).unwrap_err();

        assert!(error.message.contains("is newer than source"));
        assert_eq!(fs::read(&target_pack).unwrap(), b"newer pack bytes");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn validates_wh3_game_folder_shape() {
        let root = temp_root("validate");
        let game_dir = root.join("game");
        fs::create_dir_all(game_dir.join("data")).unwrap();
        fs::write(game_dir.join("Warhammer3.exe"), b"exe").unwrap();

        let validated = validate_wh3_game_folder(&game_dir).unwrap();

        assert_eq!(validated.game_dir, game_dir);
        assert_eq!(validated.data_dir, root.join("game/data"));
        assert_eq!(validated.executable_path, root.join("game/Warhammer3.exe"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rejects_game_folder_without_data_dir() {
        let root = temp_root("missing-data");
        let game_dir = root.join("game");
        fs::create_dir_all(&game_dir).unwrap();
        fs::write(game_dir.join("Warhammer3.exe"), b"exe").unwrap();

        let error = validate_wh3_game_folder(&game_dir).unwrap_err();

        assert!(error.message.contains("game data folder does not exist"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rejects_game_folder_without_expected_executable() {
        let root = temp_root("missing-exe");
        let game_dir = root.join("game");
        fs::create_dir_all(game_dir.join("data")).unwrap();

        let error = validate_windows_game_folder(&game_dir, "Warhammer3.exe").unwrap_err();

        assert!(error.message.contains("game executable does not exist"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn discovers_wh3_workshop_folder_from_steam_game_folder() {
        let root = temp_root("workshop");
        let steam_apps_dir = root.join("SteamLibrary/steamapps");
        let game_dir = steam_apps_dir.join("common/Total War WARHAMMER III");
        let workshop_content_dir = steam_apps_dir
            .join("workshop")
            .join("content")
            .join(WH3_STEAM_APP_ID);
        fs::create_dir_all(game_dir.join("data")).unwrap();
        fs::write(game_dir.join("Warhammer3.exe"), b"exe").unwrap();
        fs::create_dir_all(&workshop_content_dir).unwrap();

        let discovered = discover_wh3_workshop_folder(&game_dir).unwrap();

        assert_eq!(discovered.steam_apps_dir, steam_apps_dir);
        assert_eq!(discovered.workshop_content_dir, workshop_content_dir);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rejects_workshop_discovery_for_non_steam_game_folder() {
        let root = temp_root("non-steam");
        let game_dir = root.join("Total War WARHAMMER III");
        fs::create_dir_all(game_dir.join("data")).unwrap();
        fs::write(game_dir.join("Warhammer3.exe"), b"exe").unwrap();

        let error = discover_wh3_workshop_folder(&game_dir).unwrap_err();

        assert!(
            error
                .message
                .contains("game folder is not under a Steam common directory")
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rejects_workshop_discovery_when_content_folder_is_missing() {
        let root = temp_root("missing-workshop");
        let steam_apps_dir = root.join("SteamLibrary/steamapps");
        let game_dir = steam_apps_dir.join("common/Total War WARHAMMER III");
        fs::create_dir_all(game_dir.join("data")).unwrap();
        fs::write(game_dir.join("Warhammer3.exe"), b"exe").unwrap();

        let error = discover_wh3_workshop_folder(&game_dir).unwrap_err();

        assert!(
            error
                .message
                .contains("WH3 workshop content folder does not exist")
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn parses_nested_and_flat_steam_libraryfolders_vdf() {
        let nested = r#"
            "libraryfolders"
            {
                "0"
                {
                    "path" "C:\\Program Files (x86)\\Steam"
                    "apps"
                    {
                        "1142710" "1"
                    }
                }
                "1"
                {
                    "path" "D:\\SteamLibrary"
                }
            }
        "#;
        let nested_libraries = parse_steam_libraries_from_libraryfolders_vdf(nested).unwrap();

        assert_eq!(nested_libraries.len(), 2);
        assert_eq!(
            nested_libraries[0].library_dir,
            std::path::PathBuf::from(r"C:\Program Files (x86)\Steam")
        );
        assert_eq!(
            nested_libraries[1].steam_apps_dir,
            std::path::PathBuf::from(r"D:\SteamLibrary").join("steamapps")
        );

        let flat = r#"
            "libraryfolders"
            {
                "0" "E:\\Games\\SteamLibrary"
            }
        "#;
        let flat_libraries = parse_steam_libraries_from_libraryfolders_vdf(flat).unwrap();

        assert_eq!(flat_libraries.len(), 1);
        assert_eq!(
            flat_libraries[0].library_dir,
            std::path::PathBuf::from(r"E:\Games\SteamLibrary")
        );
    }

    #[test]
    fn parses_steam_install_path_from_windows_reg_query_output() {
        let output = r#"
HKEY_LOCAL_MACHINE\SOFTWARE\Wow6432Node\Valve\Steam
    InstallPath    REG_SZ    C:\Program Files (x86)\Steam
"#;

        let steam_root = parse_steam_install_path_from_reg_query_output(output).unwrap();

        assert_eq!(
            steam_root,
            std::path::PathBuf::from(r"C:\Program Files (x86)\Steam")
        );
    }

    #[test]
    fn parses_steam_user_path_from_windows_reg_query_output() {
        let output = r#"
HKEY_CURRENT_USER\SOFTWARE\Valve\Steam
    SteamPath    REG_SZ    D:\Games\Steam
"#;

        let steam_root = parse_steam_install_path_from_reg_query_output(output).unwrap();

        assert_eq!(steam_root, std::path::PathBuf::from(r"D:\Games\Steam"));
    }

    #[test]
    fn rejects_windows_reg_query_output_without_install_path() {
        let output = r#"
HKEY_LOCAL_MACHINE\SOFTWARE\Wow6432Node\Valve\Steam
    SteamExe    REG_SZ    C:\Program Files (x86)\Steam\steam.exe
"#;

        let error = parse_steam_install_path_from_reg_query_output(output).unwrap_err();

        assert!(error.message.contains("Steam InstallPath or SteamPath"));
    }

    #[cfg(not(windows))]
    #[test]
    fn windows_registry_discovery_is_unavailable_off_windows() {
        let error = discover_steam_root_from_windows_registry().unwrap_err();

        assert!(error.message.contains("only available on Windows"));
    }

    #[test]
    fn discovers_wh3_steam_install_from_libraryfolders_and_appmanifest() {
        let root = temp_root("steam-root");
        let steam_root = root.join("Steam");
        let library_dir = root.join("Games/SteamLibrary");
        let libraryfolders_vdf = steam_root.join("steamapps/libraryfolders.vdf");
        let steam_apps_dir = library_dir.join("steamapps");
        let appmanifest_path = steam_apps_dir.join(format!("appmanifest_{WH3_STEAM_APP_ID}.acf"));
        let game_dir = steam_apps_dir.join("common/Custom WH3 Folder");
        let workshop_content_dir = steam_apps_dir
            .join("workshop")
            .join("content")
            .join(WH3_STEAM_APP_ID);
        fs::create_dir_all(libraryfolders_vdf.parent().unwrap()).unwrap();
        fs::create_dir_all(game_dir.join("data")).unwrap();
        fs::create_dir_all(&workshop_content_dir).unwrap();
        fs::write(game_dir.join("Warhammer3.exe"), b"exe").unwrap();
        fs::write(
            &libraryfolders_vdf,
            format!(
                r#"
                "libraryfolders"
                {{
                    "0"
                    {{
                        "path" "{}"
                    }}
                }}
                "#,
                vdf_path_literal(&library_dir)
            ),
        )
        .unwrap();
        fs::write(
            &appmanifest_path,
            r#"
            "AppState"
            {
                "appid" "1142710"
                "installdir" "Custom WH3 Folder"
                "LastUpdated" "1710000000"
            }
            "#,
        )
        .unwrap();

        let install = discover_wh3_steam_install_from_steam_root(&steam_root).unwrap();

        assert_eq!(install.steam_apps_dir, steam_apps_dir);
        assert_eq!(install.appmanifest_path, appmanifest_path);
        assert_eq!(install.game_dir, game_dir);
        assert_eq!(install.data_dir, install.game_dir.join("data"));
        assert_eq!(
            install.executable_path,
            install.game_dir.join("Warhammer3.exe")
        );
        assert_eq!(install.workshop_content_dir, Some(workshop_content_dir));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn steam_install_discovery_does_not_require_existing_workshop_folder() {
        let root = temp_root("steam-root-no-workshop");
        let steam_root = root.join("Steam");
        let libraryfolders_vdf = steam_root.join("steamapps/libraryfolders.vdf");
        let steam_apps_dir = steam_root.join("steamapps");
        let appmanifest_path = steam_apps_dir.join(format!("appmanifest_{WH3_STEAM_APP_ID}.acf"));
        let game_dir = steam_apps_dir.join("common/Total War WARHAMMER III");
        fs::create_dir_all(game_dir.join("data")).unwrap();
        fs::write(game_dir.join("Warhammer3.exe"), b"exe").unwrap();
        fs::write(
            &libraryfolders_vdf,
            format!(
                r#"
                "libraryfolders"
                {{
                    "0" "{}"
                }}
                "#,
                vdf_path_literal(&steam_root)
            ),
        )
        .unwrap();
        fs::write(
            &appmanifest_path,
            r#"
            "AppState"
            {
                "appid" "1142710"
            }
            "#,
        )
        .unwrap();

        let install = discover_wh3_steam_install_from_steam_root(&steam_root).unwrap();

        assert_eq!(install.game_dir, game_dir);
        assert_eq!(install.workshop_content_dir, None);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn falls_back_to_my_mods_when_primary_write_fails() {
        let root = temp_root("fallback");
        let game_dir = root.join("game");
        fs::create_dir_all(game_dir.join("used_mods.txt")).unwrap();
        let plan = launch_plan(&game_dir, "used_mods.txt", r#"mod "a.pack";"#, Vec::new());

        let prepared =
            prepare_windows_launch_files(&plan, &LaunchPreparationOptions::default()).unwrap();

        assert_eq!(prepared.mod_list_path, game_dir.join("my_mods.txt"));
        assert_eq!(
            fs::read_to_string(&prepared.mod_list_path).unwrap(),
            r#"mod "a.pack";"#
        );
        assert_eq!(prepared.args, ["my_mods.txt;"]);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn preserves_campaign_load_args_when_replacing_mod_list_arg() {
        let root = temp_root("args");
        let game_dir = root.join("game");
        fs::create_dir_all(game_dir.join("used_mods.txt")).unwrap();
        let mut plan = launch_plan(&game_dir, "used_mods.txt", "", Vec::new());
        plan.args = vec![
            "game_startup_mode".to_string(),
            "campaign_load".to_string(),
            "My Save".to_string(),
            ";".to_string(),
            "used_mods.txt;".to_string(),
        ];

        let prepared =
            prepare_windows_launch_files(&plan, &LaunchPreparationOptions::default()).unwrap();

        assert_eq!(
            prepared.args,
            [
                "game_startup_mode",
                "campaign_load",
                "My Save",
                ";",
                "my_mods.txt;"
            ]
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn resolves_relative_prepared_executable_inside_working_dir() {
        let prepared = super::PreparedWindowsLaunch {
            working_dir: std::path::PathBuf::from(
                r"C:\Steam\steamapps\common\Total War WARHAMMER III",
            ),
            executable: "Warhammer3.exe".to_string(),
            args: Vec::new(),
            mod_list_path: std::path::PathBuf::from("used_mods.txt"),
            written_pack_files: Vec::new(),
            copied_files: Vec::new(),
        };

        assert_eq!(
            executable_path_for_prepared_launch(&prepared),
            prepared.working_dir.join("Warhammer3.exe")
        );
    }

    #[test]
    fn builds_powershell_command_for_high_process_priority() {
        assert_eq!(
            super::powershell_set_priority_command(1234, super::WindowsProcessPriorityClass::High),
            "$ErrorActionPreference = 'Stop'; (Get-Process -Id 1234).PriorityClass = 'High'"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn process_priority_update_is_skipped_off_windows() {
        let update =
            super::set_windows_process_priority(1234, super::WindowsProcessPriorityClass::High);

        assert_eq!(update.process_id, 1234);
        assert_eq!(
            update.requested_class,
            super::WindowsProcessPriorityClass::High
        );
        assert!(!update.attempted);
        assert!(!update.applied);
        assert!(update.message.contains("only supported on Windows"));
    }

    #[cfg(unix)]
    #[test]
    fn spawns_prepared_launch_in_working_dir_with_mod_list_arg() {
        let root = temp_root("spawn-prepared");
        let game_dir = root.join("game");
        fs::create_dir_all(&game_dir).unwrap();
        write_unix_helper_script(
            &game_dir.join("Warhammer3.exe"),
            r#"#!/bin/sh
printf '%s\n' "$PWD" > launch-cwd.txt
printf '%s\n' "$@" > launch-args.txt
modfile="${1%;}"
cat "$modfile" > launch-mod-list.txt
"#,
        );
        let plan = launch_plan(&game_dir, "used_mods.txt", "mod \"a.pack\";", Vec::new());
        let prepared =
            prepare_windows_launch_files(&plan, &LaunchPreparationOptions::default()).unwrap();

        let mut child = super::spawn_prepared_windows_launch(&prepared).unwrap();
        let status = child.wait().unwrap();

        assert!(status.success());
        let launched_cwd = std::path::PathBuf::from(
            fs::read_to_string(game_dir.join("launch-cwd.txt"))
                .unwrap()
                .trim(),
        );
        assert_eq!(
            launched_cwd.canonicalize().unwrap(),
            game_dir.canonicalize().unwrap()
        );
        assert_eq!(
            fs::read_to_string(game_dir.join("launch-args.txt")).unwrap(),
            "used_mods.txt;\n"
        );
        assert_eq!(
            fs::read_to_string(game_dir.join("launch-mod-list.txt")).unwrap(),
            "mod \"a.pack\";"
        );

        fs::remove_dir_all(root).ok();
    }

    fn launch_plan(
        game_dir: &std::path::Path,
        mod_list_file_name: &str,
        mod_list_contents: &str,
        pre_launch_copies: Vec<PreLaunchCopyOperation>,
    ) -> WindowsLaunchPlan {
        WindowsLaunchPlan {
            mod_list_file_name: mod_list_file_name.to_string(),
            mod_list_contents: mod_list_contents.to_string(),
            pre_launch_copies,
            pre_launch_pack_writes: Vec::new(),
            working_dir: game_dir.display().to_string(),
            executable: "Warhammer3.exe".to_string(),
            args: vec![format!("{mod_list_file_name};")],
            command_line_preview: String::new(),
        }
    }

    struct RecordingTsSteamHelperRunner {
        mods_data_json: String,
        items_json: Option<String>,
        requested_mod_batches: Vec<Vec<String>>,
        requested_item_batches: Vec<Vec<String>>,
    }

    impl TsSteamHelperRunner for RecordingTsSteamHelperRunner {
        fn get_mods_data_json(
            &mut self,
            _app_id: &str,
            workshop_ids: &[String],
        ) -> Result<String, SteamWorkshopAdapterError> {
            self.requested_mod_batches.push(workshop_ids.to_vec());
            Ok(self.mods_data_json.clone())
        }

        fn get_items_json(
            &mut self,
            _app_id: &str,
            workshop_ids: &[String],
        ) -> Result<Option<String>, SteamWorkshopAdapterError> {
            self.requested_item_batches.push(workshop_ids.to_vec());
            self.items_json.clone().map(Some).ok_or_else(|| {
                SteamWorkshopAdapterError::new(
                    SteamWorkshopAdapterErrorKind::Unavailable,
                    "test helper has no dependency item response",
                )
            })
        }
    }

    #[derive(Default)]
    struct RecordingWorkshopCommandRunner {
        subscribed_ids: Vec<String>,
        get_subscribed_calls: Vec<String>,
        subscribe_calls: Vec<(String, Vec<String>, Duration)>,
        download_calls: Vec<(String, Vec<String>, Duration)>,
        unsubscribe_calls: Vec<(String, Vec<String>)>,
        check_state_calls: Vec<(String, Vec<String>, Duration)>,
    }

    impl SteamWorkshopCommandRunner for RecordingWorkshopCommandRunner {
        fn get_subscribed_ids(
            &mut self,
            app_id: &str,
        ) -> Result<Vec<String>, SteamWorkshopAdapterError> {
            self.get_subscribed_calls.push(app_id.to_string());
            Ok(self.subscribed_ids.clone())
        }

        fn subscribe_ids(
            &mut self,
            app_id: &str,
            workshop_ids: &[String],
            command_delay: Duration,
        ) -> Result<SteamWorkshopCommandResult, SteamWorkshopAdapterError> {
            self.subscribe_calls
                .push((app_id.to_string(), workshop_ids.to_vec(), command_delay));
            Ok(SteamWorkshopCommandResult::requested(
                "sub",
                workshop_ids.to_vec(),
            ))
        }

        fn download_ids(
            &mut self,
            app_id: &str,
            workshop_ids: &[String],
            command_delay: Duration,
        ) -> Result<SteamWorkshopCommandResult, SteamWorkshopAdapterError> {
            self.download_calls
                .push((app_id.to_string(), workshop_ids.to_vec(), command_delay));
            Ok(SteamWorkshopCommandResult::requested(
                "download",
                workshop_ids.to_vec(),
            ))
        }

        fn unsubscribe_ids(
            &mut self,
            app_id: &str,
            workshop_ids: &[String],
        ) -> Result<SteamWorkshopCommandResult, SteamWorkshopAdapterError> {
            self.unsubscribe_calls
                .push((app_id.to_string(), workshop_ids.to_vec()));
            Ok(SteamWorkshopCommandResult::requested(
                "unsubscribe",
                workshop_ids.to_vec(),
            ))
        }

        fn check_state_and_download_updates(
            &mut self,
            app_id: &str,
            workshop_ids: &[String],
            command_delay: Duration,
        ) -> Result<SteamWorkshopCheckStateResult, SteamWorkshopAdapterError> {
            self.check_state_calls
                .push((app_id.to_string(), workshop_ids.to_vec(), command_delay));
            Ok(SteamWorkshopCheckStateResult::checked(
                workshop_ids.to_vec(),
            ))
        }
    }

    struct SequencedWorkshopCommandRunner {
        subscribed_sequences: std::collections::VecDeque<Vec<String>>,
        get_subscribed_calls: Vec<String>,
        subscribe_calls: Vec<(String, Vec<String>, Duration)>,
        download_calls: Vec<(String, Vec<String>, Duration)>,
        unsubscribe_calls: Vec<(String, Vec<String>)>,
        check_state_calls: Vec<(String, Vec<String>, Duration)>,
    }

    impl SequencedWorkshopCommandRunner {
        fn new(subscribed_sequences: Vec<Vec<String>>) -> Self {
            Self {
                subscribed_sequences: subscribed_sequences.into(),
                get_subscribed_calls: Vec::new(),
                subscribe_calls: Vec::new(),
                download_calls: Vec::new(),
                unsubscribe_calls: Vec::new(),
                check_state_calls: Vec::new(),
            }
        }
    }

    impl SteamWorkshopCommandRunner for SequencedWorkshopCommandRunner {
        fn get_subscribed_ids(
            &mut self,
            app_id: &str,
        ) -> Result<Vec<String>, SteamWorkshopAdapterError> {
            self.get_subscribed_calls.push(app_id.to_string());
            Ok(self.subscribed_sequences.pop_front().unwrap_or_default())
        }

        fn subscribe_ids(
            &mut self,
            app_id: &str,
            workshop_ids: &[String],
            command_delay: Duration,
        ) -> Result<SteamWorkshopCommandResult, SteamWorkshopAdapterError> {
            self.subscribe_calls
                .push((app_id.to_string(), workshop_ids.to_vec(), command_delay));
            Ok(SteamWorkshopCommandResult::requested(
                "sub",
                workshop_ids.to_vec(),
            ))
        }

        fn download_ids(
            &mut self,
            app_id: &str,
            workshop_ids: &[String],
            command_delay: Duration,
        ) -> Result<SteamWorkshopCommandResult, SteamWorkshopAdapterError> {
            self.download_calls
                .push((app_id.to_string(), workshop_ids.to_vec(), command_delay));
            Ok(SteamWorkshopCommandResult::requested(
                "download",
                workshop_ids.to_vec(),
            ))
        }

        fn unsubscribe_ids(
            &mut self,
            app_id: &str,
            workshop_ids: &[String],
        ) -> Result<SteamWorkshopCommandResult, SteamWorkshopAdapterError> {
            self.unsubscribe_calls
                .push((app_id.to_string(), workshop_ids.to_vec()));
            Ok(SteamWorkshopCommandResult::requested(
                "unsubscribe",
                workshop_ids.to_vec(),
            ))
        }

        fn check_state_and_download_updates(
            &mut self,
            app_id: &str,
            workshop_ids: &[String],
            command_delay: Duration,
        ) -> Result<SteamWorkshopCheckStateResult, SteamWorkshopAdapterError> {
            self.check_state_calls
                .push((app_id.to_string(), workshop_ids.to_vec(), command_delay));
            Ok(SteamWorkshopCheckStateResult::checked(
                workshop_ids.to_vec(),
            ))
        }
    }

    fn zero_delay_resubscribe_config(max_attempts: usize) -> SteamResubscribeSafetyConfig {
        SteamResubscribeSafetyConfig {
            max_attempts,
            verification_delay: Duration::ZERO,
            retry_delay: Duration::ZERO,
        }
    }

    fn temp_root(test_name: &str) -> std::path::PathBuf {
        let counter = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "wh3mm-runtime-launch-{test_name}-{}-{counter}",
            std::process::id()
        ));
        path
    }

    fn vdf_path_literal(path: &std::path::Path) -> String {
        path.display().to_string().replace('\\', "\\\\")
    }

    fn mod_record_with_path(
        path: &std::path::Path,
        workshop_id: Option<&str>,
        tags: &[&str],
    ) -> ModRecord {
        ModRecord {
            identity: ModIdentity::new(
                path.display().to_string(),
                workshop_id.map(ToOwned::to_owned),
                path.file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("test pack"),
            ),
            display_name: "Test Pack".to_string(),
            source: wh3mm_core::ModSource::Local,
            thumbnail_path: None,
            local_modified_ms: None,
            enabled: false,
            always_enabled: false,
            hidden: false,
            categories: Vec::new(),
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
        }
    }

    #[cfg(unix)]
    fn write_unix_helper_script(path: &std::path::Path, contents: &str) {
        use std::os::unix::fs::PermissionsExt as _;

        fs::write(path, contents).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
}
