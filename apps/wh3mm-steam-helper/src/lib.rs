//! Steam helper executable protocol support.
//!
//! This crate currently provides a fixture-backed implementation of the
//! process contract consumed by `SteamWorkshopHelperProcessRunner`. It also
//! owns the backend boundary that a native Windows Steamworks implementation
//! will fill without changing the Dioxus/runtime helper protocol.

#[cfg(windows)]
use std::sync::mpsc;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fmt, fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const FIXTURE_ENV: &str = "WH3MM_STEAM_HELPER_FIXTURE";
const COMMAND_LOG_ENV: &str = "WH3MM_STEAM_HELPER_COMMAND_LOG";
const BACKEND_ENV: &str = "WH3MM_STEAM_HELPER_BACKEND";
const WINDOWS_RUNTIME_REDISTRIBUTABLES: &[&str] = &["steam_api64.dll"];
const WINDOWS_LINK_LIBRARIES: &[&str] = &["steam_api64.lib"];
#[cfg(windows)]
const STEAM_CALLBACK_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(windows)]
const STEAM_CALLBACK_POLL_INTERVAL: Duration = Duration::from_millis(10);
#[cfg(windows)]
const STEAM_DEPENDENCY_DELAY: Duration = Duration::from_millis(100);
#[cfg(windows)]
const WORKSHOP_QUERY_CACHE_SECONDS: u32 = 60;
const DEFAULT_MONITOR_INTERVAL_MS: u64 = 1_000;
const DEFAULT_MONITOR_TIMEOUT_MS: u64 = 10 * 60 * 1_000;
const MAX_MONITOR_IDS: usize = 200;
const WORKSHOP_CATALOG_PAGE_SIZE: usize = 50;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum WorkshopCatalogScope {
    #[default]
    Discover,
    Subscribed,
    Favorites,
    Published,
    VotedUp,
    VotedDown,
    Followed,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum WorkshopCatalogSort {
    Relevance,
    #[default]
    Popular,
    Newest,
    Trending,
    MostSubscribed,
    Updated,
    Oldest,
    Title,
    SubscriptionDate,
    Score,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkshopCatalogQuery {
    scope: WorkshopCatalogScope,
    sort: WorkshopCatalogSort,
    #[serde(default)]
    search_text: String,
    #[serde(default)]
    required_tags: Vec<String>,
    #[serde(default)]
    match_any_tag: bool,
    page: u32,
}

impl Default for WorkshopCatalogQuery {
    fn default() -> Self {
        Self {
            scope: WorkshopCatalogScope::Discover,
            sort: WorkshopCatalogSort::Popular,
            search_text: String::new(),
            required_tags: Vec::new(),
            match_any_tag: false,
            page: 1,
        }
    }
}

impl WorkshopCatalogQuery {
    fn normalized(&self) -> Result<Self, String> {
        if self.page == 0 {
            return Err("Workshop catalog pages start at 1".to_string());
        }
        let supported = if self.scope == WorkshopCatalogScope::Discover {
            matches!(
                self.sort,
                WorkshopCatalogSort::Relevance
                    | WorkshopCatalogSort::Popular
                    | WorkshopCatalogSort::Newest
                    | WorkshopCatalogSort::Trending
                    | WorkshopCatalogSort::MostSubscribed
                    | WorkshopCatalogSort::Updated
            )
        } else {
            matches!(
                self.sort,
                WorkshopCatalogSort::Newest
                    | WorkshopCatalogSort::Oldest
                    | WorkshopCatalogSort::Title
                    | WorkshopCatalogSort::Updated
                    | WorkshopCatalogSort::SubscriptionDate
                    | WorkshopCatalogSort::Score
            )
        };
        if !supported {
            return Err("Workshop sort is incompatible with the selected scope".to_string());
        }
        if self.scope != WorkshopCatalogScope::Discover
            && (!self.search_text.trim().is_empty() || !self.required_tags.is_empty())
        {
            return Err("Search text and tags only apply to Discover".to_string());
        }
        let mut seen = BTreeSet::new();
        let required_tags = self
            .required_tags
            .iter()
            .map(|tag| tag.trim())
            .filter(|tag| !tag.is_empty())
            .filter(|tag| seen.insert(tag.to_ascii_lowercase()))
            .map(ToString::to_string)
            .take(20)
            .collect();
        let search_text = self
            .search_text
            .trim()
            .chars()
            .take(200)
            .collect::<String>();
        let sort = if self.sort == WorkshopCatalogSort::Relevance && search_text.is_empty() {
            WorkshopCatalogSort::Popular
        } else {
            self.sort
        };
        Ok(Self {
            scope: self.scope,
            sort,
            search_text,
            required_tags,
            match_any_tag: self.match_any_tag,
            page: self.page,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum WorkshopCatalogItemKind {
    #[default]
    Item,
    Collection,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
struct WorkshopItemStatistics {
    subscriptions: u64,
    favorites: u64,
    followers: u64,
    views: u64,
    comments: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)]
struct WorkshopItemState {
    workshop_id: String,
    subscribed: bool,
    installed: bool,
    needs_update: bool,
    downloading: bool,
    download_pending: bool,
    bytes_downloaded: Option<u64>,
    bytes_total: Option<u64>,
}

impl WorkshopItemState {
    fn download_complete(&self) -> bool {
        self.installed && !self.needs_update && !self.downloading && !self.download_pending
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
struct WorkshopCatalogItem {
    workshop_id: String,
    kind: WorkshopCatalogItemKind,
    title: String,
    description: String,
    owner_steam_id: String,
    author: String,
    created_at_ms: u64,
    updated_at_ms: u64,
    tags: Vec<String>,
    preview_url: Option<String>,
    file_size: u64,
    upvotes: u32,
    downvotes: u32,
    score: f32,
    child_ids: Vec<String>,
    statistics: WorkshopItemStatistics,
    state: WorkshopItemState,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
struct WorkshopCatalogPage {
    page: u32,
    total_results: u32,
    was_cached: bool,
    items: Vec<WorkshopCatalogItem>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
struct WorkshopMonitorSnapshot {
    elapsed_ms: u64,
    items: Vec<WorkshopItemState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum WorkshopMonitorCompletionReason {
    Complete,
    Timeout,
    Cancelled,
}

/// Files used by the fixture-backed helper.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HelperPaths {
    /// Backend selector, such as `fixture` or `native`.
    pub backend: Option<String>,
    /// JSON fixture path.
    pub fixture_path: Option<PathBuf>,
    /// Optional JSON-lines command log path.
    pub command_log_path: Option<PathBuf>,
}

impl HelperPaths {
    /// Reads helper paths from process environment variables.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            backend: env::var(BACKEND_ENV).ok(),
            fixture_path: env::var_os(FIXTURE_ENV).map(PathBuf::from),
            command_log_path: env::var_os(COMMAND_LOG_ENV).map(PathBuf::from),
        }
    }

    /// Creates helper paths from explicit optional paths.
    #[must_use]
    pub fn new(fixture_path: Option<PathBuf>, command_log_path: Option<PathBuf>) -> Self {
        Self {
            backend: None,
            fixture_path,
            command_log_path,
        }
    }

    /// Creates helper paths and an explicit backend selector from optional
    /// paths.
    #[must_use]
    pub fn with_backend(
        backend: impl Into<String>,
        fixture_path: Option<PathBuf>,
        command_log_path: Option<PathBuf>,
    ) -> Self {
        Self {
            backend: Some(backend.into()),
            fixture_path,
            command_log_path,
        }
    }
}

/// Error returned by the helper protocol implementation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HelperError {
    code: i32,
    message: String,
}

impl HelperError {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            code: 64,
            message: message.into(),
        }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            code: 2,
            message: message.into(),
        }
    }

    fn data(message: impl Into<String>) -> Self {
        Self {
            code: 65,
            message: message.into(),
        }
    }

    /// Builds an output/pipe failure for the streaming CLI entry point.
    pub fn output(message: impl Into<String>) -> Self {
        Self::unavailable(message)
    }

    /// Process exit code.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        self.code
    }
}

impl fmt::Display for HelperError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HelperError {}

/// Executes a helper command from CLI-style arguments.
///
/// Arguments follow the runtime process runner contract:
/// `app_id command [payload] [delay_ms]`.
///
/// # Errors
///
/// Returns [`HelperError`] when arguments are invalid, the fixture backend is
/// unavailable, fixture JSON is malformed, the native backend is unavailable,
/// or the response/log cannot be written.
pub fn run_with_args<I, S>(args: I, paths: &HelperPaths) -> Result<String, HelperError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
    if args.len() < 2 {
        return Err(HelperError::usage(
            "usage: wh3mm-steam-helper <app_id> <command> [payload] [delay_ms]",
        ));
    }

    let request = HelperRequest::from_args(&args)?;
    if request.command == HelperCommand::Probe {
        let response = probe_helper(paths, &request)?;
        append_command_log(paths.command_log_path.as_deref(), &request, Some(&response))?;
        return Ok(response);
    }

    let mut backend = load_backend(paths, &request.app_id)?;
    let response = execute_backend_command(backend.as_mut(), &request)?;
    append_command_log(paths.command_log_path.as_deref(), &request, Some(&response))?;
    Ok(response)
}

/// Executes a helper command and emits one or more JSON lines.
///
/// Normal commands emit one line. `monitorWorkshopItems` keeps one Steamworks
/// client alive and emits bounded local item-state snapshots until completion
/// or timeout.
///
/// # Errors
///
/// Returns an error for malformed command arguments or payloads, unavailable
/// fixture/native backends, command failures, or a failed output emission.
pub fn run_streaming_with_args<I, S, F>(
    args: I,
    paths: &HelperPaths,
    mut emit: F,
) -> Result<(), HelperError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
    F: FnMut(&str) -> Result<(), HelperError>,
{
    let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
    if args.len() < 2 {
        return Err(HelperError::usage(
            "usage: wh3mm-steam-helper <app_id> <command> [payload] [delay_ms]",
        ));
    }
    let request = HelperRequest::from_args(&args)?;
    if request.command != HelperCommand::MonitorWorkshopItems {
        let output = run_with_args(args, paths)?;
        return emit(&output);
    }

    let monitor: MonitorRequest = request.json_payload()?;
    let ids = normalize_id_list(monitor.ids.iter().map(String::as_str));
    if ids.is_empty() {
        return Err(HelperError::usage(
            "monitorWorkshopItems requires at least one numeric Workshop ID",
        ));
    }
    if ids.len() > MAX_MONITOR_IDS {
        return Err(HelperError::usage(format!(
            "monitorWorkshopItems accepts at most {MAX_MONITOR_IDS} IDs"
        )));
    }
    let interval = Duration::from_millis(monitor.interval_ms.clamp(250, 5_000));
    let timeout =
        Duration::from_millis(monitor.timeout_ms.clamp(1_000, DEFAULT_MONITOR_TIMEOUT_MS));
    let mut backend = load_backend(paths, &request.app_id)?;
    let started = Instant::now();
    let final_snapshot = loop {
        let states = backend.item_states(&ids)?;
        let snapshot = WorkshopMonitorSnapshot {
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            items: states,
        };
        emit(&to_json(&MonitorEvent::Snapshot {
            snapshot: snapshot.clone(),
        })?)?;
        if snapshot.items.len() == ids.len()
            && snapshot
                .items
                .iter()
                .all(WorkshopItemState::download_complete)
        {
            emit(&to_json(&MonitorEvent::Complete {
                reason: WorkshopMonitorCompletionReason::Complete,
                snapshot: snapshot.clone(),
            })?)?;
            break snapshot;
        }
        if started.elapsed() >= timeout {
            emit(&to_json(&MonitorEvent::Complete {
                reason: WorkshopMonitorCompletionReason::Timeout,
                snapshot: snapshot.clone(),
            })?)?;
            break snapshot;
        }
        thread::sleep(interval);
    };
    let _ = final_snapshot;
    append_command_log(paths.command_log_path.as_deref(), &request, None)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HelperBackendKind {
    Fixture,
    Native,
}

impl HelperBackendKind {
    fn select(paths: &HelperPaths) -> Result<Self, HelperError> {
        match paths.backend.as_deref().map(str::trim) {
            Some("") | None if paths.fixture_path.is_some() => Ok(Self::Fixture),
            Some("" | "native") | None => Ok(Self::Native),
            Some("fixture") => Ok(Self::Fixture),
            Some(value) => Err(HelperError::usage(format!(
                "unsupported Steam helper backend {value:?}; expected fixture or native"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Fixture => "fixture",
            Self::Native => "native",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HelperRequest {
    app_id: String,
    command: HelperCommand,
    payload: Option<String>,
    delay_ms: Option<u64>,
}

impl HelperRequest {
    fn from_args(args: &[String]) -> Result<Self, HelperError> {
        let command = HelperCommand::parse(&args[1])?;
        let (payload, delay_ms) = match command.payload_kind() {
            PayloadKind::None => (None, parse_optional_delay(args.get(2))?),
            PayloadKind::CommaIds | PayloadKind::SemicolonIds | PayloadKind::Json => {
                let payload = args.get(2).cloned().unwrap_or_default();
                let delay_ms = parse_optional_delay(args.get(3))?;
                (Some(payload), delay_ms)
            }
        };

        Ok(Self {
            app_id: args[0].trim().to_string(),
            command,
            payload,
            delay_ms,
        })
    }

    fn ids(&self) -> Vec<String> {
        match self.command.payload_kind() {
            PayloadKind::None | PayloadKind::Json => Vec::new(),
            PayloadKind::CommaIds => normalize_id_list(
                self.payload
                    .as_deref()
                    .unwrap_or_default()
                    .split(',')
                    .map(str::trim),
            ),
            PayloadKind::SemicolonIds => normalize_id_list(
                self.payload
                    .as_deref()
                    .unwrap_or_default()
                    .split(';')
                    .map(str::trim),
            ),
        }
    }

    fn json_payload<T: for<'de> Deserialize<'de>>(&self) -> Result<T, HelperError> {
        serde_json::from_str(self.payload.as_deref().unwrap_or_default()).map_err(|error| {
            HelperError::data(format!(
                "invalid {} JSON payload: {error}",
                self.command.as_str()
            ))
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HelperCommand {
    Probe,
    GetSubscribedIds,
    GetModsData,
    GetItems,
    GetDependencies,
    GetAuthors,
    QueryWorkshop,
    MonitorWorkshopItems,
    Subscribe,
    Download,
    Unsubscribe,
    CheckState,
}

impl HelperCommand {
    fn parse(value: &str) -> Result<Self, HelperError> {
        match value {
            "probe" => Ok(Self::Probe),
            "getSubscribedIds" => Ok(Self::GetSubscribedIds),
            "getModsData" => Ok(Self::GetModsData),
            "getItems" => Ok(Self::GetItems),
            "getDependencies" => Ok(Self::GetDependencies),
            "getAuthors" => Ok(Self::GetAuthors),
            "queryWorkshop" => Ok(Self::QueryWorkshop),
            "monitorWorkshopItems" => Ok(Self::MonitorWorkshopItems),
            "sub" => Ok(Self::Subscribe),
            "download" => Ok(Self::Download),
            "unsubscribe" => Ok(Self::Unsubscribe),
            "checkState" => Ok(Self::CheckState),
            _ => Err(HelperError::usage(format!(
                "unsupported Steam helper command: {value}"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Probe => "probe",
            Self::GetSubscribedIds => "getSubscribedIds",
            Self::GetModsData => "getModsData",
            Self::GetItems => "getItems",
            Self::GetDependencies => "getDependencies",
            Self::GetAuthors => "getAuthors",
            Self::QueryWorkshop => "queryWorkshop",
            Self::MonitorWorkshopItems => "monitorWorkshopItems",
            Self::Subscribe => "sub",
            Self::Download => "download",
            Self::Unsubscribe => "unsubscribe",
            Self::CheckState => "checkState",
        }
    }

    fn payload_kind(self) -> PayloadKind {
        match self {
            Self::Probe | Self::GetSubscribedIds => PayloadKind::None,
            Self::QueryWorkshop | Self::MonitorWorkshopItems => PayloadKind::Json,
            Self::GetModsData | Self::GetItems | Self::GetDependencies | Self::GetAuthors => {
                PayloadKind::CommaIds
            }
            Self::Subscribe | Self::Download | Self::Unsubscribe | Self::CheckState => {
                PayloadKind::SemicolonIds
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PayloadKind {
    None,
    CommaIds,
    SemicolonIds,
    Json,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MonitorRequest {
    ids: Vec<String>,
    #[serde(default = "default_monitor_interval_ms")]
    interval_ms: u64,
    #[serde(default = "default_monitor_timeout_ms")]
    timeout_ms: u64,
}

fn default_monitor_interval_ms() -> u64 {
    DEFAULT_MONITOR_INTERVAL_MS
}

fn default_monitor_timeout_ms() -> u64 {
    DEFAULT_MONITOR_TIMEOUT_MS
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
enum MonitorEvent {
    Snapshot {
        snapshot: WorkshopMonitorSnapshot,
    },
    Complete {
        reason: WorkshopMonitorCompletionReason,
        snapshot: WorkshopMonitorSnapshot,
    },
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct HelperFixture {
    #[serde(default, deserialize_with = "deserialize_string_values")]
    subscribed_ids: Vec<String>,
    #[serde(default)]
    mods: Vec<WorkshopItem>,
    #[serde(default)]
    items: Vec<WorkshopItem>,
    #[serde(default, deserialize_with = "deserialize_dependency_map")]
    dependencies: BTreeMap<String, Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_string_map")]
    authors: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    catalog: Vec<WorkshopCatalogItem>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_string_values"
    )]
    favorite_ids: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_string_values"
    )]
    published_ids: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_string_values"
    )]
    voted_up_ids: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_string_values"
    )]
    voted_down_ids: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_string_values"
    )]
    followed_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    monitor_snapshots: Vec<Vec<WorkshopItemState>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkshopItem {
    #[serde(default, deserialize_with = "deserialize_string_value")]
    published_file_id: String,
    #[serde(default)]
    title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[serde(default)]
    owner: WorkshopOwner,
    #[serde(default, deserialize_with = "deserialize_u64_value")]
    time_updated: u64,
    #[serde(
        default,
        deserialize_with = "deserialize_u64_value",
        skip_serializing_if = "is_zero"
    )]
    time_created: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    author: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preview_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    child_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    statistics: WorkshopItemStatistics,
    #[serde(default, skip_serializing_if = "is_default")]
    state: WorkshopItemState,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkshopOwner {
    #[serde(default, deserialize_with = "deserialize_string_value")]
    steam_id64: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandResult {
    ok: bool,
    app_id: String,
    command: String,
    ids: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    update_requested_ids: Vec<String>,
    delay_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query_scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query_sort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query_page: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_count: Option<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)]
struct HelperProbe {
    app_id: String,
    selected_backend: String,
    fixture_configured: bool,
    fixture_available: bool,
    command_log_configured: bool,
    native_implemented: bool,
    native_available: bool,
    native_status: String,
    windows_runtime_redistributables: Vec<String>,
    windows_runtime_redistributable_statuses: Vec<RuntimeRedistributableStatus>,
    windows_link_libraries: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeRedistributableStatus {
    file_name: String,
    expected_path: String,
    present: bool,
}

trait SteamHelperBackend {
    fn subscribed_ids(&mut self) -> Result<Vec<String>, HelperError>;

    fn mods_data(&mut self, ids: &[String]) -> Result<HelperFixture, HelperError>;

    fn items(&mut self, ids: &[String]) -> Result<Vec<WorkshopItem>, HelperError>;

    fn dependencies(
        &mut self,
        ids: &[String],
    ) -> Result<BTreeMap<String, Vec<String>>, HelperError>;

    fn authors(&mut self, ids: &[String]) -> Result<BTreeMap<String, String>, HelperError>;

    fn catalog(&mut self, query: &WorkshopCatalogQuery)
    -> Result<WorkshopCatalogPage, HelperError>;

    fn item_states(&mut self, ids: &[String]) -> Result<Vec<WorkshopItemState>, HelperError>;

    fn command_action(&mut self, request: &HelperRequest) -> Result<CommandResult, HelperError>;
}

struct FixtureSteamBackend {
    fixture: HelperFixture,
    monitor_index: usize,
}

impl FixtureSteamBackend {
    fn from_path(path: Option<&Path>) -> Result<Self, HelperError> {
        Ok(Self {
            fixture: load_fixture(path)?,
            monitor_index: 0,
        })
    }
}

impl SteamHelperBackend for FixtureSteamBackend {
    fn subscribed_ids(&mut self) -> Result<Vec<String>, HelperError> {
        Ok(normalize_id_list(
            self.fixture.subscribed_ids.iter().map(String::as_str),
        ))
    }

    fn mods_data(&mut self, ids: &[String]) -> Result<HelperFixture, HelperError> {
        let requested = ids.iter().cloned().collect::<BTreeSet<_>>();
        let mods = self
            .fixture
            .mods
            .iter()
            .filter(|item| normalized_item_id(item).is_some_and(|id| requested.contains(&id)))
            .cloned()
            .collect::<Vec<_>>();
        let dependencies = filter_dependencies(&self.fixture.dependencies, &requested);
        Ok(HelperFixture {
            subscribed_ids: Vec::new(),
            mods,
            items: Vec::new(),
            dependencies,
            authors: self.fixture.authors.clone(),
            ..HelperFixture::default()
        })
    }

    fn items(&mut self, ids: &[String]) -> Result<Vec<WorkshopItem>, HelperError> {
        let requested = ids.iter().cloned().collect::<BTreeSet<_>>();
        let mut items = lookup_items(&self.fixture, &requested);
        for item in &mut items {
            item.author = self
                .fixture
                .authors
                .get(&item.owner.steam_id64)
                .filter(|name| !name.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| item.owner.steam_id64.clone());
            item.state.workshop_id.clone_from(&item.published_file_id);
        }
        Ok(items)
    }

    fn dependencies(
        &mut self,
        ids: &[String],
    ) -> Result<BTreeMap<String, Vec<String>>, HelperError> {
        let requested = ids.iter().cloned().collect::<BTreeSet<_>>();
        Ok(filter_dependencies(&self.fixture.dependencies, &requested))
    }

    fn authors(&mut self, ids: &[String]) -> Result<BTreeMap<String, String>, HelperError> {
        let requested = ids.iter().cloned().collect::<BTreeSet<_>>();
        Ok(filter_authors(&self.fixture.authors, &requested))
    }

    fn catalog(
        &mut self,
        query: &WorkshopCatalogQuery,
    ) -> Result<WorkshopCatalogPage, HelperError> {
        let query = query.normalized().map_err(HelperError::usage)?;
        let allowed_ids = fixture_scope_ids(&self.fixture, query.scope);
        let mut items = self
            .fixture
            .catalog
            .iter()
            .filter(|item| {
                allowed_ids
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&item.workshop_id))
            })
            .filter(|item| fixture_item_matches_query(item, &query))
            .cloned()
            .collect::<Vec<_>>();
        sort_fixture_catalog(&mut items, query.sort);
        let total_results = u32::try_from(items.len()).unwrap_or(u32::MAX);
        let start = (query.page.saturating_sub(1) as usize) * WORKSHOP_CATALOG_PAGE_SIZE;
        let items = items
            .into_iter()
            .skip(start)
            .take(WORKSHOP_CATALOG_PAGE_SIZE)
            .collect();
        Ok(WorkshopCatalogPage {
            page: query.page,
            total_results,
            was_cached: false,
            items,
        })
    }

    fn item_states(&mut self, ids: &[String]) -> Result<Vec<WorkshopItemState>, HelperError> {
        let requested = normalize_id_list(ids.iter().map(String::as_str));
        if let Some(snapshot) = self
            .fixture
            .monitor_snapshots
            .get(self.monitor_index)
            .cloned()
        {
            self.monitor_index = self
                .monitor_index
                .saturating_add(1)
                .min(self.fixture.monitor_snapshots.len());
            return Ok(snapshot
                .into_iter()
                .filter(|state| requested.contains(&state.workshop_id))
                .collect());
        }
        Ok(self
            .fixture
            .catalog
            .iter()
            .filter(|item| requested.contains(&item.workshop_id))
            .map(|item| item.state.clone())
            .collect())
    }

    fn command_action(&mut self, request: &HelperRequest) -> Result<CommandResult, HelperError> {
        Ok(CommandResult {
            ok: true,
            app_id: request.app_id.clone(),
            command: request.command.as_str().to_string(),
            ids: request.ids(),
            update_requested_ids: Vec::new(),
            delay_ms: request.delay_ms,
            query_scope: None,
            query_sort: None,
            query_page: None,
            result_count: None,
        })
    }
}

#[cfg(windows)]
struct NativeSteamBackend {
    client: steamworks::Client,
    app_id: u32,
}

#[cfg(windows)]
impl NativeSteamBackend {
    fn new(app_id: &str) -> Result<Self, HelperError> {
        let app_id = parse_app_id(app_id)?;
        let client = steamworks::Client::init_app(steamworks::AppId(app_id)).map_err(|error| {
            HelperError::unavailable(format!(
                "failed to initialize native Steamworks for app {app_id}: {error:?}; \
                     make sure Steam is running and {WINDOWS_RUNTIME_REDISTRIBUTABLES:?} \
                     are discoverable beside the helper executable"
            ))
        })?;
        Ok(Self { client, app_id })
    }

    fn published_file_ids(ids: &[String]) -> Result<Vec<steamworks::PublishedFileId>, HelperError> {
        ids.iter()
            .map(|id| Ok(steamworks::PublishedFileId(parse_u64_id(id)?)))
            .collect()
    }

    fn wait_for_callback<T>(
        &self,
        receiver: &mpsc::Receiver<Result<T, String>>,
        context: &str,
    ) -> Result<T, HelperError> {
        let started_at = Instant::now();
        loop {
            match receiver.try_recv() {
                Ok(Ok(value)) => return Ok(value),
                Ok(Err(error)) => {
                    return Err(HelperError::unavailable(format!(
                        "native Steamworks {context} failed: {error}"
                    )));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(HelperError::unavailable(format!(
                        "native Steamworks {context} callback disconnected"
                    )));
                }
                Err(mpsc::TryRecvError::Empty)
                    if started_at.elapsed() >= STEAM_CALLBACK_TIMEOUT =>
                {
                    return Err(HelperError::unavailable(format!(
                        "native Steamworks {context} timed out after {}ms",
                        STEAM_CALLBACK_TIMEOUT.as_millis()
                    )));
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.client.run_callbacks();
                    thread::sleep(STEAM_CALLBACK_POLL_INTERVAL);
                }
            }
        }
    }

    fn wait_for_unit_callback(
        &self,
        receiver: &mpsc::Receiver<Result<(), String>>,
        context: &str,
    ) -> Result<(), HelperError> {
        self.wait_for_callback(receiver, context)
    }

    fn subscribe_item(&self, item: steamworks::PublishedFileId) -> Result<(), HelperError> {
        let (sender, receiver) = mpsc::channel();
        self.client.ugc().subscribe_item(item, move |result| {
            let _ = sender.send(result.map_err(|error| format!("{error:?}")));
        });
        self.wait_for_unit_callback(&receiver, &format!("subscribe {}", item.0))
    }

    fn unsubscribe_item(&self, item: steamworks::PublishedFileId) -> Result<(), HelperError> {
        let (sender, receiver) = mpsc::channel();
        self.client.ugc().unsubscribe_item(item, move |result| {
            let _ = sender.send(result.map_err(|error| format!("{error:?}")));
        });
        self.wait_for_unit_callback(&receiver, &format!("unsubscribe {}", item.0))
    }

    fn sleep_command_delay(request: &HelperRequest) {
        if let Some(delay_ms) = request.delay_ms {
            thread::sleep(Duration::from_millis(delay_ms));
        }
    }

    fn workshop_items_for_ids(&self, ids: &[String]) -> Result<Vec<WorkshopItem>, HelperError> {
        let published_file_ids = Self::published_file_ids(ids)?;
        if published_file_ids.is_empty() {
            return Ok(Vec::new());
        }

        let (sender, receiver) = mpsc::channel();
        self.client
            .ugc()
            .query_items(published_file_ids)
            .map_err(|error| {
                HelperError::unavailable(format!(
                    "failed to create native Steamworks getItems query: {error}"
                ))
            })?
            .include_long_desc(true)
            .include_children(true)
            .fetch(move |result| {
                let response = result
                    .map(|results| {
                        (0..results.returned_results())
                            .filter_map(|index| workshop_item_from_query_results(&results, index))
                            .collect::<Vec<_>>()
                    })
                    .map_err(|error| format!("{error:?}"));
                let _ = sender.send(response);
            });
        self.wait_for_callback(&receiver, "getItems query")
    }

    fn dependency_map_for_ids(
        &self,
        ids: &[String],
    ) -> Result<BTreeMap<String, Vec<String>>, HelperError> {
        let published_file_ids = Self::published_file_ids(ids)?;
        if published_file_ids.is_empty() {
            return Ok(BTreeMap::new());
        }

        let (sender, receiver) = mpsc::channel();
        self.client
            .ugc()
            .query_items(published_file_ids)
            .map_err(|error| {
                HelperError::unavailable(format!(
                    "failed to create native Steamworks getDependencies query: {error}"
                ))
            })?
            .include_children(true)
            .fetch(move |result| {
                let response = result
                    .map(|results| {
                        let mut dependencies = BTreeMap::new();
                        for index in 0..results.returned_results() {
                            let Some(item) = results.get(index) else {
                                continue;
                            };
                            let children = results
                                .get_children(index)
                                .unwrap_or_default()
                                .into_iter()
                                .map(|child| child.0.to_string())
                                .collect::<Vec<_>>();
                            dependencies.insert(item.published_file_id.0.to_string(), children);
                        }
                        dependencies
                    })
                    .map_err(|error| format!("{error:?}"));
                let _ = sender.send(response);
            });
        self.wait_for_callback(&receiver, "getDependencies query")
    }

    fn pump_callbacks_for(&self, duration: Duration) {
        let started_at = Instant::now();
        while started_at.elapsed() < duration {
            self.client.run_callbacks();
            thread::sleep(STEAM_CALLBACK_POLL_INTERVAL);
        }
    }

    fn states_for_ids(&self, ids: &[String]) -> Result<Vec<WorkshopItemState>, HelperError> {
        let ugc = self.client.ugc();
        Self::published_file_ids(ids)?
            .into_iter()
            .zip(ids.iter())
            .map(|(item, id)| {
                let state = ugc.item_state(item);
                let (bytes_downloaded, bytes_total) = ugc
                    .item_download_info(item)
                    .map_or((None, None), |(downloaded, total)| {
                        (Some(downloaded), Some(total))
                    });
                Ok(WorkshopItemState {
                    workshop_id: id.clone(),
                    subscribed: state.contains(steamworks::ItemState::SUBSCRIBED),
                    installed: state.contains(steamworks::ItemState::INSTALLED),
                    needs_update: state.contains(steamworks::ItemState::NEEDS_UPDATE),
                    downloading: state.contains(steamworks::ItemState::DOWNLOADING),
                    download_pending: state.contains(steamworks::ItemState::DOWNLOAD_PENDING),
                    bytes_downloaded,
                    bytes_total,
                })
            })
            .collect()
    }

    fn resolve_catalog_authors(
        &mut self,
        items: &mut [WorkshopCatalogItem],
    ) -> Result<(), HelperError> {
        let owner_ids = items
            .iter()
            .filter_map(|item| normalize_workshop_id(&item.owner_steam_id))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let authors = self.authors(&owner_ids)?;
        for item in items {
            item.author = authors
                .get(&item.owner_steam_id)
                .filter(|name| name.as_str() != "[unknown]" && !name.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| item.owner_steam_id.clone());
        }
        Ok(())
    }
}

#[cfg(windows)]
impl SteamHelperBackend for NativeSteamBackend {
    fn subscribed_ids(&mut self) -> Result<Vec<String>, HelperError> {
        let ids = self
            .client
            .ugc()
            .subscribed_items(false)
            .into_iter()
            .map(|item| item.0.to_string())
            .collect::<Vec<_>>();
        Ok(normalize_id_list(ids.iter().map(String::as_str)))
    }

    fn mods_data(&mut self, ids: &[String]) -> Result<HelperFixture, HelperError> {
        let mods = self.workshop_items_for_ids(ids)?;
        let dependencies = self.dependency_map_for_ids(ids)?;
        let author_ids = mods
            .iter()
            .filter_map(|item| normalize_workshop_id(&item.owner.steam_id64))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let authors = self.authors(&author_ids)?;
        Ok(HelperFixture {
            subscribed_ids: Vec::new(),
            mods,
            items: Vec::new(),
            dependencies,
            authors,
            ..HelperFixture::default()
        })
    }

    fn items(&mut self, ids: &[String]) -> Result<Vec<WorkshopItem>, HelperError> {
        let mut items = self.workshop_items_for_ids(ids)?;
        let states = self.states_for_ids(ids)?;
        let author_ids = items
            .iter()
            .filter_map(|item| normalize_workshop_id(&item.owner.steam_id64))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let authors = self.authors(&author_ids)?;
        for item in &mut items {
            item.author = authors
                .get(&item.owner.steam_id64)
                .filter(|name| name.as_str() != "[unknown]" && !name.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| item.owner.steam_id64.clone());
            if let Some(state) = states
                .iter()
                .find(|state| state.workshop_id == item.published_file_id)
            {
                item.state = state.clone();
            }
        }
        Ok(items)
    }

    fn dependencies(
        &mut self,
        ids: &[String],
    ) -> Result<BTreeMap<String, Vec<String>>, HelperError> {
        let dependencies = self.dependency_map_for_ids(ids)?;
        thread::sleep(STEAM_DEPENDENCY_DELAY);
        Ok(dependencies)
    }

    fn authors(&mut self, ids: &[String]) -> Result<BTreeMap<String, String>, HelperError> {
        let friends = self.client.friends();
        let mut authors = BTreeMap::new();
        let mut pending = Vec::new();

        for id in ids {
            let steam_id = steamworks::SteamId::from_raw(parse_u64_id(id)?);
            let friend = friends.get_friend(steam_id);
            let name = friend.name();
            if name == "[unknown]" || name.trim().is_empty() {
                let _ = friends.request_user_information(steam_id, true);
                pending.push((id.clone(), steam_id));
            } else {
                authors.insert(id.clone(), name);
            }
        }

        if !pending.is_empty() {
            self.pump_callbacks_for(Duration::from_millis(1_500));
            for (id, steam_id) in pending {
                let name = friends.get_friend(steam_id).name();
                authors.insert(id, name);
            }
        }

        Ok(authors)
    }

    fn catalog(
        &mut self,
        query: &WorkshopCatalogQuery,
    ) -> Result<WorkshopCatalogPage, HelperError> {
        let query = query.normalized().map_err(HelperError::usage)?;
        let app_ids = steamworks::AppIDs::ConsumerAppId(steamworks::AppId(self.app_id));
        let ugc = self.client.ugc();
        let mut handle = if query.scope == WorkshopCatalogScope::Discover {
            ugc.query_all(
                native_discovery_sort(query.sort),
                steamworks::UGCType::All,
                app_ids,
                query.page,
            )
        } else {
            ugc.query_user(
                self.client.user().steam_id().account_id(),
                native_user_list(query.scope),
                steamworks::UGCType::All,
                native_user_sort(query.sort),
                app_ids,
                query.page,
            )
        }
        .map_err(|error| {
            HelperError::unavailable(format!("failed to create Workshop catalog query: {error}"))
        })?;

        handle = handle
            .include_long_desc(true)
            .include_children(true)
            .allow_cached_response(WORKSHOP_QUERY_CACHE_SECONDS);
        if !query.search_text.is_empty() {
            handle = handle.set_search_text(&query.search_text);
        }
        for tag in &query.required_tags {
            handle = handle.add_required_tag(tag);
        }
        if !query.required_tags.is_empty() {
            handle = handle.set_match_any_tag(query.match_any_tag);
        }
        if query.sort == WorkshopCatalogSort::Trending {
            handle = handle.set_ranked_by_trend_days(7);
        }

        let (sender, receiver) = mpsc::channel();
        handle.fetch(move |result| {
            let response = result
                .map(|results| {
                    let items = (0..results.returned_results())
                        .filter_map(|index| {
                            workshop_catalog_item_from_query_results(&results, index)
                        })
                        .collect::<Vec<_>>();
                    WorkshopCatalogPage {
                        page: query.page,
                        total_results: results.total_results(),
                        was_cached: results.was_cached(),
                        items,
                    }
                })
                .map_err(|error| format!("{error:?}"));
            let _ = sender.send(response);
        });
        let mut page = self.wait_for_callback(&receiver, "Workshop catalog query")?;
        let ids = page
            .items
            .iter()
            .map(|item| item.workshop_id.clone())
            .collect::<Vec<_>>();
        let states = self.states_for_ids(&ids)?;
        for item in &mut page.items {
            if let Some(state) = states
                .iter()
                .find(|state| state.workshop_id == item.workshop_id)
            {
                item.state = state.clone();
            }
        }
        self.resolve_catalog_authors(&mut page.items)?;
        Ok(page)
    }

    fn item_states(&mut self, ids: &[String]) -> Result<Vec<WorkshopItemState>, HelperError> {
        self.states_for_ids(ids)
    }

    fn command_action(&mut self, request: &HelperRequest) -> Result<CommandResult, HelperError> {
        let ids = request.ids();
        let published_file_ids = Self::published_file_ids(&ids)?;
        let mut ok = true;
        let mut update_requested_ids = Vec::new();

        match request.command {
            HelperCommand::Subscribe => {
                for item in published_file_ids {
                    self.subscribe_item(item)?;
                    Self::sleep_command_delay(request);
                }
            }
            HelperCommand::Download => {
                for item in published_file_ids {
                    ok &= self.client.ugc().download_item(item, false);
                    Self::sleep_command_delay(request);
                }
            }
            HelperCommand::Unsubscribe => {
                for item in published_file_ids {
                    self.unsubscribe_item(item)?;
                }
            }
            HelperCommand::CheckState => {
                for (id, item) in ids.iter().zip(published_file_ids) {
                    if self
                        .client
                        .ugc()
                        .item_state(item)
                        .contains(steamworks::ItemState::NEEDS_UPDATE)
                    {
                        ok &= self.client.ugc().download_item(item, false);
                        update_requested_ids.push(id.clone());
                        Self::sleep_command_delay(request);
                    }
                }
            }
            HelperCommand::Probe
            | HelperCommand::GetSubscribedIds
            | HelperCommand::GetModsData
            | HelperCommand::GetItems
            | HelperCommand::GetDependencies
            | HelperCommand::GetAuthors
            | HelperCommand::QueryWorkshop
            | HelperCommand::MonitorWorkshopItems => {
                return Err(HelperError::usage(format!(
                    "{} is not a Steam command action",
                    request.command.as_str()
                )));
            }
        }

        Ok(CommandResult {
            ok,
            app_id: request.app_id.clone(),
            command: request.command.as_str().to_string(),
            ids,
            update_requested_ids,
            delay_ms: request.delay_ms,
            query_scope: None,
            query_sort: None,
            query_page: None,
            result_count: None,
        })
    }
}

#[cfg(windows)]
fn native_discovery_sort(sort: WorkshopCatalogSort) -> steamworks::UGCQueryType {
    match sort {
        WorkshopCatalogSort::Relevance => steamworks::UGCQueryType::RankedByTextSearch,
        WorkshopCatalogSort::Popular => steamworks::UGCQueryType::RankedByVote,
        WorkshopCatalogSort::Newest => steamworks::UGCQueryType::RankedByPublicationDate,
        WorkshopCatalogSort::Trending => steamworks::UGCQueryType::RankedByTrend,
        WorkshopCatalogSort::MostSubscribed => {
            steamworks::UGCQueryType::RankedByTotalUniqueSubscriptions
        }
        WorkshopCatalogSort::Updated => steamworks::UGCQueryType::RankedByLastUpdatedDate,
        WorkshopCatalogSort::Oldest
        | WorkshopCatalogSort::Title
        | WorkshopCatalogSort::SubscriptionDate
        | WorkshopCatalogSort::Score => steamworks::UGCQueryType::RankedByVote,
    }
}

#[cfg(windows)]
fn native_user_list(scope: WorkshopCatalogScope) -> steamworks::UserList {
    match scope {
        WorkshopCatalogScope::Subscribed => steamworks::UserList::Subscribed,
        WorkshopCatalogScope::Favorites => steamworks::UserList::Favorited,
        WorkshopCatalogScope::Published => steamworks::UserList::Published,
        WorkshopCatalogScope::VotedUp => steamworks::UserList::VotedUp,
        WorkshopCatalogScope::VotedDown => steamworks::UserList::VotedDown,
        WorkshopCatalogScope::Followed => steamworks::UserList::Followed,
        WorkshopCatalogScope::Discover => steamworks::UserList::Subscribed,
    }
}

#[cfg(windows)]
fn native_user_sort(sort: WorkshopCatalogSort) -> steamworks::UserListOrder {
    match sort {
        WorkshopCatalogSort::Oldest => steamworks::UserListOrder::CreationOrderAsc,
        WorkshopCatalogSort::Title => steamworks::UserListOrder::TitleAsc,
        WorkshopCatalogSort::Updated => steamworks::UserListOrder::LastUpdatedDesc,
        WorkshopCatalogSort::SubscriptionDate => steamworks::UserListOrder::SubscriptionDateDesc,
        WorkshopCatalogSort::Score => steamworks::UserListOrder::VoteScoreDesc,
        WorkshopCatalogSort::Newest
        | WorkshopCatalogSort::Relevance
        | WorkshopCatalogSort::Popular
        | WorkshopCatalogSort::Trending
        | WorkshopCatalogSort::MostSubscribed => steamworks::UserListOrder::CreationOrderDesc,
    }
}

#[cfg(windows)]
fn workshop_catalog_item_from_query_results(
    results: &steamworks::QueryResults<'_>,
    index: u32,
) -> Option<WorkshopCatalogItem> {
    let item = results.get(index)?;
    let workshop_id = item.published_file_id.0.to_string();
    let child_ids = results
        .get_children(index)
        .unwrap_or_default()
        .into_iter()
        .map(|child| child.0.to_string())
        .collect();
    let statistic = |kind| results.statistic(index, kind).unwrap_or(0);
    Some(WorkshopCatalogItem {
        workshop_id: workshop_id.clone(),
        kind: if item.file_type == steamworks::FileType::Collection {
            WorkshopCatalogItemKind::Collection
        } else {
            WorkshopCatalogItemKind::Item
        },
        title: item.title,
        description: item.description,
        owner_steam_id: item.owner.raw().to_string(),
        author: String::new(),
        created_at_ms: u64::from(item.time_created).saturating_mul(1_000),
        updated_at_ms: u64::from(item.time_updated).saturating_mul(1_000),
        tags: item.tags,
        preview_url: results
            .preview_url(index)
            .filter(|url| valid_preview_url(url)),
        file_size: u64::from(item.file_size),
        upvotes: item.num_upvotes,
        downvotes: item.num_downvotes,
        score: item.score,
        child_ids,
        statistics: WorkshopItemStatistics {
            subscriptions: statistic(steamworks::UGCStatisticType::Subscriptions),
            favorites: statistic(steamworks::UGCStatisticType::Favorites),
            followers: statistic(steamworks::UGCStatisticType::Followers),
            views: statistic(steamworks::UGCStatisticType::UniqueWebsiteViews),
            comments: statistic(steamworks::UGCStatisticType::Comments),
        },
        state: WorkshopItemState {
            workshop_id,
            ..WorkshopItemState::default()
        },
    })
}

#[cfg(not(windows))]
struct NativeSteamBackend;

#[cfg(not(windows))]
impl NativeSteamBackend {
    fn new(_app_id: &str) -> Self {
        Self
    }

    fn unavailable(command: &str) -> HelperError {
        HelperError::unavailable(format!(
            "native Steamworks backend is only available in Windows builds for {command}; \
             set {BACKEND_ENV}=fixture and {FIXTURE_ENV}=<path> for fixture mode"
        ))
    }
}

#[cfg(not(windows))]
impl SteamHelperBackend for NativeSteamBackend {
    fn subscribed_ids(&mut self) -> Result<Vec<String>, HelperError> {
        Err(Self::unavailable("getSubscribedIds"))
    }

    fn mods_data(&mut self, _ids: &[String]) -> Result<HelperFixture, HelperError> {
        Err(Self::unavailable("getModsData"))
    }

    fn items(&mut self, _ids: &[String]) -> Result<Vec<WorkshopItem>, HelperError> {
        Err(Self::unavailable("getItems"))
    }

    fn dependencies(
        &mut self,
        _ids: &[String],
    ) -> Result<BTreeMap<String, Vec<String>>, HelperError> {
        Err(Self::unavailable("getDependencies"))
    }

    fn authors(&mut self, _ids: &[String]) -> Result<BTreeMap<String, String>, HelperError> {
        Err(Self::unavailable("getAuthors"))
    }

    fn catalog(
        &mut self,
        _query: &WorkshopCatalogQuery,
    ) -> Result<WorkshopCatalogPage, HelperError> {
        Err(Self::unavailable("queryWorkshop"))
    }

    fn item_states(&mut self, _ids: &[String]) -> Result<Vec<WorkshopItemState>, HelperError> {
        Err(Self::unavailable("monitorWorkshopItems"))
    }

    fn command_action(&mut self, request: &HelperRequest) -> Result<CommandResult, HelperError> {
        Err(Self::unavailable(request.command.as_str()))
    }
}

fn load_backend(
    paths: &HelperPaths,
    app_id: &str,
) -> Result<Box<dyn SteamHelperBackend>, HelperError> {
    match HelperBackendKind::select(paths)? {
        HelperBackendKind::Fixture => Ok(Box::new(FixtureSteamBackend::from_path(
            paths.fixture_path.as_deref(),
        )?)),
        #[cfg(windows)]
        HelperBackendKind::Native => Ok(Box::new(NativeSteamBackend::new(app_id)?)),
        #[cfg(not(windows))]
        HelperBackendKind::Native => Ok(Box::new(NativeSteamBackend::new(app_id))),
    }
}

fn execute_backend_command(
    backend: &mut dyn SteamHelperBackend,
    request: &HelperRequest,
) -> Result<String, HelperError> {
    match request.command {
        HelperCommand::Probe => Err(HelperError::usage(
            "probe must be handled before loading backend",
        )),
        HelperCommand::GetSubscribedIds => to_json(&backend.subscribed_ids()?),
        HelperCommand::GetModsData => to_json(&backend.mods_data(&request.ids())?),
        HelperCommand::GetItems => to_json(&backend.items(&request.ids())?),
        HelperCommand::GetDependencies => to_json(&backend.dependencies(&request.ids())?),
        HelperCommand::GetAuthors => to_json(&backend.authors(&request.ids())?),
        HelperCommand::QueryWorkshop => {
            let query = request.json_payload::<WorkshopCatalogQuery>()?;
            to_json(&backend.catalog(&query)?)
        }
        HelperCommand::MonitorWorkshopItems => Err(HelperError::usage(
            "monitorWorkshopItems requires the streaming helper entry point",
        )),
        HelperCommand::Subscribe
        | HelperCommand::Download
        | HelperCommand::Unsubscribe
        | HelperCommand::CheckState => to_json(&backend.command_action(request)?),
    }
}

fn probe_helper(paths: &HelperPaths, request: &HelperRequest) -> Result<String, HelperError> {
    let selected_backend = HelperBackendKind::select(paths)?;
    let fixture_available = paths
        .fixture_path
        .as_deref()
        .is_some_and(|path| load_fixture(Some(path)).is_ok());
    let native_status = native_backend_probe_status(selected_backend);

    to_json(&HelperProbe {
        app_id: request.app_id.clone(),
        selected_backend: selected_backend.as_str().to_string(),
        fixture_configured: paths.fixture_path.is_some(),
        fixture_available,
        command_log_configured: paths.command_log_path.is_some(),
        native_implemented: native_status.implemented,
        native_available: native_status.available,
        native_status: native_status.message,
        windows_runtime_redistributables: WINDOWS_RUNTIME_REDISTRIBUTABLES
            .iter()
            .map(|file| (*file).to_string())
            .collect(),
        windows_runtime_redistributable_statuses: windows_runtime_redistributable_statuses(),
        windows_link_libraries: WINDOWS_LINK_LIBRARIES
            .iter()
            .map(|file| (*file).to_string())
            .collect(),
    })
}

fn windows_runtime_redistributable_statuses() -> Vec<RuntimeRedistributableStatus> {
    let helper_dir = env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    WINDOWS_RUNTIME_REDISTRIBUTABLES
        .iter()
        .map(|file_name| {
            let expected_path = helper_dir.join(file_name);
            RuntimeRedistributableStatus {
                file_name: (*file_name).to_string(),
                expected_path: expected_path.display().to_string(),
                present: expected_path.is_file(),
            }
        })
        .collect()
}

struct NativeBackendProbeStatus {
    implemented: bool,
    available: bool,
    message: String,
}

#[cfg(windows)]
fn native_backend_probe_status(selected_backend: HelperBackendKind) -> NativeBackendProbeStatus {
    let message = if matches!(selected_backend, HelperBackendKind::Native) {
        "native Steamworks backend is built for Windows; command execution will initialize Steamworks for the requested app".to_string()
    } else {
        "native Steamworks backend is built for Windows but not selected".to_string()
    };

    NativeBackendProbeStatus {
        implemented: true,
        available: false,
        message,
    }
}

#[cfg(not(windows))]
fn native_backend_probe_status(selected_backend: HelperBackendKind) -> NativeBackendProbeStatus {
    let message = if matches!(selected_backend, HelperBackendKind::Native) {
        "native Steamworks backend is only available in Windows builds".to_string()
    } else {
        "native Steamworks backend is not selected".to_string()
    };

    NativeBackendProbeStatus {
        implemented: false,
        available: false,
        message,
    }
}

#[cfg(windows)]
fn parse_app_id(app_id: &str) -> Result<u32, HelperError> {
    app_id
        .trim()
        .parse::<u32>()
        .map_err(|error| HelperError::usage(format!("invalid Steam app ID {app_id:?}: {error}")))
}

#[cfg(windows)]
fn parse_u64_id(id: &str) -> Result<u64, HelperError> {
    id.trim()
        .parse::<u64>()
        .map_err(|error| HelperError::usage(format!("invalid Steam workshop ID {id:?}: {error}")))
}

#[cfg(windows)]
fn workshop_item_from_query_results(
    results: &steamworks::QueryResults<'_>,
    index: u32,
) -> Option<WorkshopItem> {
    let item = results.get(index)?;
    let workshop_id = item.published_file_id.0.to_string();
    let statistic = |kind| results.statistic(index, kind).unwrap_or(0);
    Some(WorkshopItem {
        published_file_id: item.published_file_id.0.to_string(),
        title: item.title,
        description: item.description,
        tags: item.tags,
        owner: WorkshopOwner {
            steam_id64: item.owner.raw().to_string(),
        },
        time_updated: u64::from(item.time_updated),
        time_created: u64::from(item.time_created),
        preview_url: results
            .preview_url(index)
            .filter(|url| valid_preview_url(url)),
        child_ids: results
            .get_children(index)
            .unwrap_or_default()
            .into_iter()
            .map(|child| child.0.to_string())
            .collect(),
        statistics: WorkshopItemStatistics {
            subscriptions: statistic(steamworks::UGCStatisticType::Subscriptions),
            favorites: statistic(steamworks::UGCStatisticType::Favorites),
            followers: statistic(steamworks::UGCStatisticType::Followers),
            views: statistic(steamworks::UGCStatisticType::UniqueWebsiteViews),
            comments: statistic(steamworks::UGCStatisticType::Comments),
        },
        state: WorkshopItemState {
            workshop_id,
            ..WorkshopItemState::default()
        },
        ..WorkshopItem::default()
    })
}

fn load_fixture(fixture_path: Option<&Path>) -> Result<HelperFixture, HelperError> {
    let fixture_path = fixture_path.ok_or_else(|| {
        HelperError::unavailable(format!(
            "{FIXTURE_ENV} is required for the fixture backend; select native on a Windows build to use live Steamworks"
        ))
    })?;
    let json = fs::read_to_string(fixture_path).map_err(|error| {
        HelperError::unavailable(format!(
            "failed to read Steam helper fixture {}: {error}",
            fixture_path.display()
        ))
    })?;
    let mut fixture = serde_json::from_str::<HelperFixture>(&json).map_err(|error| {
        HelperError::data(format!(
            "failed to parse Steam helper fixture {}: {error}",
            fixture_path.display()
        ))
    })?;
    normalize_fixture(&mut fixture);
    Ok(fixture)
}

fn normalize_fixture(fixture: &mut HelperFixture) {
    fixture.subscribed_ids = normalize_id_list(fixture.subscribed_ids.iter().map(String::as_str));
    fixture.favorite_ids = normalize_id_list(fixture.favorite_ids.iter().map(String::as_str));
    fixture.published_ids = normalize_id_list(fixture.published_ids.iter().map(String::as_str));
    fixture.voted_up_ids = normalize_id_list(fixture.voted_up_ids.iter().map(String::as_str));
    fixture.voted_down_ids = normalize_id_list(fixture.voted_down_ids.iter().map(String::as_str));
    fixture.followed_ids = normalize_id_list(fixture.followed_ids.iter().map(String::as_str));
    fixture.dependencies = fixture
        .dependencies
        .iter()
        .filter_map(|(key, values)| {
            normalize_workshop_id(key)
                .map(|key| (key, normalize_id_list(values.iter().map(String::as_str))))
        })
        .collect();
    fixture.authors = fixture
        .authors
        .iter()
        .filter(|(key, _)| normalize_workshop_id(key).is_some())
        .map(|(key, value)| (key.trim().to_string(), value.clone()))
        .collect();
    for item in fixture.mods.iter_mut().chain(fixture.items.iter_mut()) {
        item.preview_url = item.preview_url.take().filter(|url| valid_preview_url(url));
        item.child_ids = normalize_id_list(item.child_ids.iter().map(String::as_str));
    }
    if fixture.catalog.is_empty() {
        fixture.catalog = fixture
            .mods
            .iter()
            .chain(fixture.items.iter())
            .filter_map(legacy_catalog_item)
            .collect();
    }
    fixture.catalog.retain_mut(|item| {
        let Some(id) = normalize_workshop_id(&item.workshop_id) else {
            return false;
        };
        item.workshop_id.clone_from(&id);
        item.state.workshop_id.clone_from(&id);
        item.child_ids = normalize_id_list(item.child_ids.iter().map(String::as_str));
        item.preview_url = item.preview_url.take().filter(|url| valid_preview_url(url));
        true
    });
}

fn legacy_catalog_item(item: &WorkshopItem) -> Option<WorkshopCatalogItem> {
    let workshop_id = normalize_workshop_id(&item.published_file_id)?;
    Some(WorkshopCatalogItem {
        workshop_id: workshop_id.clone(),
        title: item.title.clone(),
        description: item.description.clone(),
        owner_steam_id: item.owner.steam_id64.clone(),
        author: item.owner.steam_id64.clone(),
        updated_at_ms: item.time_updated.saturating_mul(1_000),
        tags: item.tags.clone(),
        state: WorkshopItemState {
            workshop_id,
            ..WorkshopItemState::default()
        },
        ..WorkshopCatalogItem::default()
    })
}

fn fixture_scope_ids(
    fixture: &HelperFixture,
    scope: WorkshopCatalogScope,
) -> Option<BTreeSet<String>> {
    let ids = match scope {
        WorkshopCatalogScope::Discover => return None,
        WorkshopCatalogScope::Subscribed => &fixture.subscribed_ids,
        WorkshopCatalogScope::Favorites => &fixture.favorite_ids,
        WorkshopCatalogScope::Published => &fixture.published_ids,
        WorkshopCatalogScope::VotedUp => &fixture.voted_up_ids,
        WorkshopCatalogScope::VotedDown => &fixture.voted_down_ids,
        WorkshopCatalogScope::Followed => &fixture.followed_ids,
    };
    Some(ids.iter().cloned().collect())
}

fn fixture_item_matches_query(item: &WorkshopCatalogItem, query: &WorkshopCatalogQuery) -> bool {
    let search = query.search_text.to_ascii_lowercase();
    if !search.is_empty()
        && !item.title.to_ascii_lowercase().contains(&search)
        && !item.description.to_ascii_lowercase().contains(&search)
    {
        return false;
    }
    if query.required_tags.is_empty() {
        return true;
    }
    let item_tags = item
        .tags
        .iter()
        .map(|tag| tag.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let matches = query
        .required_tags
        .iter()
        .map(|tag| item_tags.contains(&tag.to_ascii_lowercase()));
    if query.match_any_tag {
        matches.into_iter().any(|matched| matched)
    } else {
        matches.into_iter().all(|matched| matched)
    }
}

fn sort_fixture_catalog(items: &mut [WorkshopCatalogItem], sort: WorkshopCatalogSort) {
    items.sort_by(|left, right| match sort {
        WorkshopCatalogSort::Relevance
        | WorkshopCatalogSort::Popular
        | WorkshopCatalogSort::Score => right.score.total_cmp(&left.score),
        WorkshopCatalogSort::Newest => right.created_at_ms.cmp(&left.created_at_ms),
        WorkshopCatalogSort::Trending
        | WorkshopCatalogSort::Updated
        | WorkshopCatalogSort::SubscriptionDate => right.updated_at_ms.cmp(&left.updated_at_ms),
        WorkshopCatalogSort::MostSubscribed => right
            .statistics
            .subscriptions
            .cmp(&left.statistics.subscriptions),
        WorkshopCatalogSort::Oldest => left.created_at_ms.cmp(&right.created_at_ms),
        WorkshopCatalogSort::Title => left
            .title
            .to_ascii_lowercase()
            .cmp(&right.title.to_ascii_lowercase()),
    });
}

fn valid_preview_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(value: &u64) -> bool {
    *value == 0
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}

fn filter_dependencies(
    dependencies: &BTreeMap<String, Vec<String>>,
    requested: &BTreeSet<String>,
) -> BTreeMap<String, Vec<String>> {
    dependencies
        .iter()
        .filter(|(key, _)| requested.contains(*key))
        .map(|(key, values)| {
            (
                key.clone(),
                normalize_id_list(values.iter().map(String::as_str)),
            )
        })
        .collect()
}

fn filter_authors(
    authors: &BTreeMap<String, String>,
    requested: &BTreeSet<String>,
) -> BTreeMap<String, String> {
    authors
        .iter()
        .filter(|(key, _)| requested.contains(*key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn lookup_items(fixture: &HelperFixture, requested: &BTreeSet<String>) -> Vec<WorkshopItem> {
    let mut by_id = BTreeMap::new();
    for item in fixture.mods.iter().chain(fixture.items.iter()) {
        if let Some(id) = normalized_item_id(item) {
            by_id.insert(id, item.clone());
        }
    }

    requested
        .iter()
        .filter_map(|id| by_id.get(id).cloned())
        .collect()
}

fn normalized_item_id(item: &WorkshopItem) -> Option<String> {
    normalize_workshop_id(&item.published_file_id)
}

fn parse_optional_delay(value: Option<&String>) -> Result<Option<u64>, HelperError> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            value.trim().parse::<u64>().map_err(|error| {
                HelperError::usage(format!("invalid Steam helper delay_ms {value:?}: {error}"))
            })
        })
        .transpose()
}

fn append_command_log(
    path: Option<&Path>,
    request: &HelperRequest,
    response: Option<&str>,
) -> Result<(), HelperError> {
    let Some(path) = path else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            HelperError::unavailable(format!(
                "failed to create Steam helper command log directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    let query = (request.command == HelperCommand::QueryWorkshop)
        .then(|| request.json_payload::<WorkshopCatalogQuery>().ok())
        .flatten();
    let result_count = response
        .filter(|_| request.command == HelperCommand::QueryWorkshop)
        .and_then(|response| serde_json::from_str::<WorkshopCatalogPage>(response).ok())
        .map(|page| page.items.len());
    let entry = CommandResult {
        ok: true,
        app_id: request.app_id.clone(),
        command: request.command.as_str().to_string(),
        ids: request.ids(),
        update_requested_ids: Vec::new(),
        delay_ms: request.delay_ms,
        query_scope: query.as_ref().map(|query| format!("{:?}", query.scope)),
        query_sort: query.as_ref().map(|query| format!("{:?}", query.sort)),
        query_page: query.as_ref().map(|query| query.page),
        result_count,
    };
    let line = to_json(&entry)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| {
            HelperError::unavailable(format!(
                "failed to open Steam helper command log {}: {error}",
                path.display()
            ))
        })?;
    writeln!(file, "{line}").map_err(|error| {
        HelperError::unavailable(format!(
            "failed to append Steam helper command log {}: {error}",
            path.display()
        ))
    })
}

fn normalize_id_list<'a>(values: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut ids = Vec::new();
    for value in values {
        if let Some(id) = normalize_workshop_id(value)
            && seen.insert(id.clone())
        {
            ids.push(id);
        }
    }
    ids
}

fn normalize_workshop_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty() && trimmed.chars().all(|character| character.is_ascii_digit()))
        .then(|| trimmed.to_string())
}

fn to_json(value: &impl Serialize) -> Result<String, HelperError> {
    serde_json::to_string(value)
        .map_err(|error| HelperError::data(format!("failed to encode helper response: {error}")))
}

fn deserialize_string_values<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let values = Vec::<Value>::deserialize(deserializer)?;
    Ok(values
        .iter()
        .filter_map(string_from_json_value)
        .collect::<Vec<_>>())
}

fn deserialize_dependency_map<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let values = BTreeMap::<String, Vec<Value>>::deserialize(deserializer)?;
    Ok(values
        .iter()
        .map(|(key, values)| {
            (
                key.clone(),
                values
                    .iter()
                    .filter_map(string_from_json_value)
                    .collect::<Vec<_>>(),
            )
        })
        .collect())
}

fn deserialize_string_map<'de, D>(deserializer: D) -> Result<BTreeMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let values = BTreeMap::<String, Value>::deserialize(deserializer)?;
    Ok(values
        .iter()
        .filter_map(|(key, value)| string_from_json_value(value).map(|value| (key.clone(), value)))
        .collect())
}

fn deserialize_string_value<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(string_from_json_value(&value).unwrap_or_default())
}

fn deserialize_u64_value<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(match value {
        Value::Number(number) => number.as_u64().unwrap_or_default(),
        Value::String(string) => string.trim().parse::<u64>().unwrap_or_default(),
        _ => 0,
    })
}

fn string_from_json_value(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn subscribed_ids_are_normalized_and_deduped() {
        let fixture_path = write_fixture(
            r#"{
                "subscribedIds": ["111", "bad", "111", 222, " 333 "],
                "mods": [],
                "dependencies": {},
                "authors": {}
            }"#,
        );
        let paths = HelperPaths::new(Some(fixture_path.clone()), None);

        let output = run_with_args(["1142710", "getSubscribedIds"], &paths).unwrap();

        assert_eq!(output, r#"["111","222","333"]"#);
        let _ = fs::remove_file(fixture_path);
    }

    #[test]
    fn mods_data_filters_requested_mods_and_dependencies() {
        let fixture_path = write_fixture(
            r#"{
                "subscribedIds": ["111", "222", "333"],
                "mods": [
                    {
                        "publishedFileId": 111,
                        "title": "Main Mod",
                        "owner": { "steamId64": "76561198000000001" },
                        "timeUpdated": "1234"
                    },
                    {
                        "publishedFileId": "333",
                        "title": "Other Mod",
                        "owner": { "steamId64": "76561198000000003" },
                        "timeUpdated": 3333
                    }
                ],
                "dependencies": {
                    "111": ["222", "bad", "222"],
                    "333": ["444"]
                },
                "authors": {
                    "76561198000000001": "Main Author",
                    "76561198000000002": "Dependency Author"
                }
            }"#,
        );
        let paths = HelperPaths::new(Some(fixture_path.clone()), None);

        let output = run_with_args(["1142710", "getModsData", "111,111,bad"], &paths).unwrap();

        assert_eq!(
            output,
            r#"{"subscribedIds":[],"mods":[{"publishedFileId":"111","title":"Main Mod","owner":{"steamId64":"76561198000000001"},"timeUpdated":1234}],"items":[],"dependencies":{"111":["222"]},"authors":{"76561198000000001":"Main Author","76561198000000002":"Dependency Author"}}"#
        );
        let _ = fs::remove_file(fixture_path);
    }

    #[test]
    fn get_items_filters_dependency_titles() {
        let fixture_path = write_fixture(
            r#"{
                "subscribedIds": [],
                "mods": [
                    {
                        "publishedFileId": "111",
                        "title": "Main Mod",
                        "owner": { "steamId64": "76561198000000001" },
                        "timeUpdated": 1
                    }
                ],
                "items": [
                    {
                        "publishedFileId": "222",
                        "title": "Dependency Mod",
                        "owner": { "steamId64": "76561198000000002" },
                        "timeUpdated": 2,
                        "timeCreated": 1,
                        "previewUrl": "https://cdn.example/222.jpg",
                        "childIds": ["333", "bad", "333"],
                        "statistics": {"subscriptions": 12, "comments": 2},
                        "state": {"installed": true}
                    }
                ],
                "dependencies": {},
                "authors": {}
            }"#,
        );
        let paths = HelperPaths::new(Some(fixture_path.clone()), None);

        let output = run_with_args(["1142710", "getItems", "222,333"], &paths).unwrap();

        let items = serde_json::from_str::<Vec<WorkshopItem>>(&output).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].published_file_id, "222");
        assert_eq!(items[0].author, "76561198000000002");
        assert_eq!(
            items[0].preview_url.as_deref(),
            Some("https://cdn.example/222.jpg")
        );
        assert_eq!(items[0].child_ids, ["333"]);
        assert_eq!(items[0].statistics.subscriptions, 12);
        assert_eq!(items[0].statistics.comments, 2);
        assert!(items[0].state.installed);
        assert_eq!(items[0].state.workshop_id, "222");
        let _ = fs::remove_file(fixture_path);
    }

    #[test]
    fn get_dependencies_filters_requested_ids() {
        let fixture_path = write_fixture(
            r#"{
                "subscribedIds": [],
                "dependencies": {
                    "111": ["222", "bad", "222"],
                    "333": ["444"]
                }
            }"#,
        );
        let paths = HelperPaths::new(Some(fixture_path.clone()), None);

        let output = run_with_args(["1142710", "getDependencies", "111,111,bad"], &paths).unwrap();

        assert_eq!(output, r#"{"111":["222"]}"#);
        let _ = fs::remove_file(fixture_path);
    }

    #[test]
    fn get_authors_filters_requested_ids() {
        let fixture_path = write_fixture(
            r#"{
                "subscribedIds": [],
                "authors": {
                    "76561198000000001": "Main Author",
                    "76561198000000002": "Dependency Author",
                    "not-a-steam-id": "Ignored"
                }
            }"#,
        );
        let paths = HelperPaths::new(Some(fixture_path.clone()), None);

        let output = run_with_args(
            [
                "1142710",
                "getAuthors",
                "76561198000000002,76561198000000002,bad",
            ],
            &paths,
        )
        .unwrap();

        assert_eq!(output, r#"{"76561198000000002":"Dependency Author"}"#);
        let _ = fs::remove_file(fixture_path);
    }

    #[test]
    fn catalog_query_filters_sorts_and_supports_user_lists() {
        let fixture_path = write_fixture(
            r#"{
                "favoriteIds": ["222"],
                "catalog": [
                    {"workshopId":"111","title":"Units Alpha","description":"balance","tags":["Units"],"score":0.9,"createdAtMs":10},
                    {"workshopId":"222","title":"Campaign Beta","description":"campaign","tags":["Campaign"],"score":0.5,"createdAtMs":20}
                ]
            }"#,
        );
        let paths = HelperPaths::new(Some(fixture_path.clone()), None);
        let discover = serde_json::to_string(&WorkshopCatalogQuery {
            search_text: "units".to_string(),
            required_tags: vec!["Units".to_string()],
            ..WorkshopCatalogQuery::default()
        })
        .unwrap();
        let page = run_with_args(["1142710", "queryWorkshop", &discover], &paths).unwrap();
        let page = serde_json::from_str::<WorkshopCatalogPage>(&page).unwrap();
        assert_eq!(page.total_results, 1);
        assert_eq!(page.items[0].workshop_id, "111");

        let favorites = serde_json::to_string(&WorkshopCatalogQuery {
            scope: WorkshopCatalogScope::Favorites,
            sort: WorkshopCatalogSort::Updated,
            ..WorkshopCatalogQuery::default()
        })
        .unwrap();
        let page = run_with_args(["1142710", "queryWorkshop", &favorites], &paths).unwrap();
        let page = serde_json::from_str::<WorkshopCatalogPage>(&page).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].workshop_id, "222");
        let _ = fs::remove_file(fixture_path);
    }

    #[test]
    fn streaming_monitor_emits_snapshots_and_completion() {
        let fixture_path = write_fixture(
            r#"{
                "monitorSnapshots": [
                    [{"workshopId":"111","subscribed":true,"downloading":true,"bytesDownloaded":5,"bytesTotal":10}],
                    [{"workshopId":"111","subscribed":true,"installed":true,"bytesDownloaded":10,"bytesTotal":10}]
                ]
            }"#,
        );
        let paths = HelperPaths::new(Some(fixture_path.clone()), None);
        let payload = r#"{"ids":["111"],"intervalMs":1,"timeoutMs":2000}"#;
        let mut lines = Vec::new();
        run_streaming_with_args(
            ["1142710", "monitorWorkshopItems", payload],
            &paths,
            |line| {
                lines.push(line.to_string());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("\"event\":\"snapshot\""));
        assert!(lines[2].contains("\"event\":\"complete\""));
        assert!(lines[2].contains("\"reason\":\"complete\""));
        let _ = fs::remove_file(fixture_path);
    }

    #[test]
    fn streaming_monitor_reports_timeout_for_unfinished_items() {
        let fixture_path = write_fixture(
            r#"{
                "monitorSnapshots": [
                    [{"workshopId":"111","subscribed":true,"downloading":true,"bytesDownloaded":5,"bytesTotal":10}]
                ]
            }"#,
        );
        let paths = HelperPaths::new(Some(fixture_path.clone()), None);
        let payload = r#"{"ids":["111"],"intervalMs":250,"timeoutMs":1000}"#;
        let mut lines = Vec::new();

        run_streaming_with_args(
            ["1142710", "monitorWorkshopItems", payload],
            &paths,
            |line| {
                lines.push(line.to_string());
                Ok(())
            },
        )
        .unwrap();

        assert!(lines.last().unwrap().contains("\"event\":\"complete\""));
        assert!(lines.last().unwrap().contains("\"reason\":\"timeout\""));
        let _ = fs::remove_file(fixture_path);
    }

    #[test]
    fn streaming_monitor_stops_when_output_pipe_breaks() {
        let fixture_path = write_fixture(
            r#"{
                "monitorSnapshots": [
                    [{"workshopId":"111","subscribed":true,"downloading":true}]
                ]
            }"#,
        );
        let paths = HelperPaths::new(Some(fixture_path.clone()), None);
        let payload = r#"{"ids":["111"],"intervalMs":250,"timeoutMs":2000}"#;

        let error =
            run_streaming_with_args(["1142710", "monitorWorkshopItems", payload], &paths, |_| {
                Err(HelperError::output("broken pipe"))
            })
            .unwrap_err();

        assert_eq!(error, HelperError::output("broken pipe"));
        let _ = fs::remove_file(fixture_path);
    }

    #[test]
    fn command_actions_emit_result_and_log_delay() {
        let fixture_path = write_fixture(r#"{"subscribedIds":[]}"#);
        let log_path = unique_temp_path("wh3mm-steam-helper-log", "jsonl");
        let paths = HelperPaths::new(Some(fixture_path.clone()), Some(log_path.clone()));

        let output =
            run_with_args(["1142710", "download", "111;bad;111;222", "250"], &paths).unwrap();

        assert_eq!(
            output,
            r#"{"ok":true,"appId":"1142710","command":"download","ids":["111","222"],"delayMs":250}"#
        );
        assert_eq!(
            fs::read_to_string(&log_path).unwrap(),
            format!("{output}\n")
        );
        let _ = fs::remove_file(fixture_path);
        let _ = fs::remove_file(log_path);
    }

    #[test]
    fn command_log_creates_parent_directory() {
        let fixture_path = write_fixture(r#"{"subscribedIds":[]}"#);
        let root = unique_temp_path("wh3mm-steam-helper-log-dir", "tmp");
        let log_path = root.join("diagnostics").join("helper.jsonl");
        let paths = HelperPaths::new(Some(fixture_path.clone()), Some(log_path.clone()));

        let output = run_with_args(["1142710", "checkState", "111"], &paths).unwrap();

        assert_eq!(
            fs::read_to_string(&log_path).unwrap(),
            format!("{output}\n")
        );
        let _ = fs::remove_file(fixture_path);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(windows))]
    #[test]
    fn missing_fixture_reports_native_backend_unavailable() {
        let error = run_with_args(
            ["1142710", "getSubscribedIds"],
            &HelperPaths::new(None, None),
        )
        .unwrap_err();

        assert_eq!(error.exit_code(), 2);
        assert!(error.to_string().contains("native Steamworks backend"));
    }

    #[cfg(not(windows))]
    #[test]
    fn explicit_native_backend_reports_unavailable_even_with_fixture() {
        let fixture_path = write_fixture(r#"{"subscribedIds":["111"]}"#);
        let paths = HelperPaths::with_backend("native", Some(fixture_path.clone()), None);

        let error = run_with_args(["1142710", "getSubscribedIds"], &paths).unwrap_err();

        assert_eq!(error.exit_code(), 2);
        assert!(error.to_string().contains("native Steamworks backend"));
        let _ = fs::remove_file(fixture_path);
    }

    #[test]
    fn explicit_fixture_backend_requires_fixture_path() {
        let paths = HelperPaths::with_backend("fixture", None, None);

        let error = run_with_args(["1142710", "getSubscribedIds"], &paths).unwrap_err();

        assert_eq!(error.exit_code(), 2);
        assert!(error.to_string().contains("WH3MM_STEAM_HELPER_FIXTURE"));
    }

    #[test]
    fn rejects_unknown_backend_selector() {
        let paths = HelperPaths::with_backend("mystery", None, None);

        let error = run_with_args(["1142710", "getSubscribedIds"], &paths).unwrap_err();

        assert_eq!(error.exit_code(), 64);
        assert!(error.to_string().contains("expected fixture or native"));
    }

    #[test]
    fn probe_reports_fixture_backend_state() {
        let fixture_path = write_fixture(r#"{"subscribedIds":["111"]}"#);
        let paths = HelperPaths::new(Some(fixture_path.clone()), None);

        let output = run_with_args(["1142710", "probe"], &paths).unwrap();
        let probe = serde_json::from_str::<Value>(&output).unwrap();

        assert_eq!(probe["appId"], "1142710");
        assert_eq!(probe["selectedBackend"], "fixture");
        assert_eq!(probe["fixtureConfigured"], true);
        assert_eq!(probe["fixtureAvailable"], true);
        assert_eq!(probe["commandLogConfigured"], false);
        assert_eq!(probe["nativeImplemented"], cfg!(windows));
        assert_eq!(probe["nativeAvailable"], false);
        assert_eq!(
            probe["windowsRuntimeRedistributables"][0],
            "steam_api64.dll"
        );
        assert_eq!(
            probe["windowsRuntimeRedistributableStatuses"][0]["fileName"],
            "steam_api64.dll"
        );
        assert!(probe["windowsRuntimeRedistributableStatuses"][0]["expectedPath"].is_string());
        assert!(probe["windowsRuntimeRedistributableStatuses"][0]["present"].is_boolean());
        assert_eq!(probe["windowsLinkLibraries"][0], "steam_api64.lib");
        let _ = fs::remove_file(fixture_path);
    }

    #[test]
    fn probe_reports_native_backend_unavailable_without_fixture() {
        let output = run_with_args(["1142710", "probe"], &HelperPaths::new(None, None)).unwrap();
        let probe = serde_json::from_str::<Value>(&output).unwrap();

        assert_eq!(probe["selectedBackend"], "native");
        assert_eq!(probe["fixtureConfigured"], false);
        assert_eq!(probe["fixtureAvailable"], false);
        assert_eq!(probe["nativeAvailable"], false);
        let native_status = probe["nativeStatus"].as_str().unwrap();
        if cfg!(windows) {
            assert!(native_status.contains("built for Windows"));
        } else {
            assert!(native_status.contains("only available in Windows builds"));
        }
    }

    #[test]
    fn probe_reports_missing_fixture_for_explicit_fixture_backend() {
        let output = run_with_args(
            ["1142710", "probe"],
            &HelperPaths::with_backend("fixture", None, None),
        )
        .unwrap();
        let probe = serde_json::from_str::<Value>(&output).unwrap();

        assert_eq!(probe["selectedBackend"], "fixture");
        assert_eq!(probe["fixtureConfigured"], false);
        assert_eq!(probe["fixtureAvailable"], false);
        assert!(
            probe["nativeStatus"]
                .as_str()
                .unwrap()
                .contains("not selected")
        );
    }

    fn write_fixture(json: &str) -> PathBuf {
        let path = unique_temp_path("wh3mm-steam-helper-fixture", "json");
        fs::write(&path, json).unwrap();
        path
    }

    fn unique_temp_path(prefix: &str, extension: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let process_id = std::process::id();
        std::env::temp_dir().join(format!(
            "{prefix}-{process_id}-{nanos}-{counter}.{extension}"
        ))
    }
}
