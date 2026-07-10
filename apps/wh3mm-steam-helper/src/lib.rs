//! Steam helper executable protocol support.
//!
//! This crate currently provides a fixture-backed implementation of the
//! process contract consumed by `SteamWorkshopHelperProcessRunner`. It also
//! owns the backend boundary that a native Windows Steamworks implementation
//! will fill without changing the Dioxus/runtime helper protocol.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fmt, fs,
    path::{Path, PathBuf},
};
#[cfg(windows)]
use std::{
    sync::mpsc,
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
        append_command_log(paths.command_log_path.as_deref(), &request)?;
        return Ok(response);
    }

    let mut backend = load_backend(paths, &request.app_id)?;
    let response = execute_backend_command(backend.as_mut(), &request)?;
    append_command_log(paths.command_log_path.as_deref(), &request)?;
    Ok(response)
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
            PayloadKind::CommaIds | PayloadKind::SemicolonIds => {
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
            PayloadKind::None => Vec::new(),
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HelperCommand {
    Probe,
    GetSubscribedIds,
    GetModsData,
    GetItems,
    GetDependencies,
    GetAuthors,
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
            Self::Subscribe => "sub",
            Self::Download => "download",
            Self::Unsubscribe => "unsubscribe",
            Self::CheckState => "checkState",
        }
    }

    fn payload_kind(self) -> PayloadKind {
        match self {
            Self::Probe | Self::GetSubscribedIds => PayloadKind::None,
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

    fn command_action(&mut self, request: &HelperRequest) -> Result<CommandResult, HelperError>;
}

struct FixtureSteamBackend {
    fixture: HelperFixture,
}

impl FixtureSteamBackend {
    fn from_path(path: Option<&Path>) -> Result<Self, HelperError> {
        Ok(Self {
            fixture: load_fixture(path)?,
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
        })
    }

    fn items(&mut self, ids: &[String]) -> Result<Vec<WorkshopItem>, HelperError> {
        let requested = ids.iter().cloned().collect::<BTreeSet<_>>();
        Ok(lookup_items(&self.fixture, &requested))
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

    fn command_action(&mut self, request: &HelperRequest) -> Result<CommandResult, HelperError> {
        Ok(CommandResult {
            ok: true,
            app_id: request.app_id.clone(),
            command: request.command.as_str().to_string(),
            ids: request.ids(),
            update_requested_ids: Vec::new(),
            delay_ms: request.delay_ms,
        })
    }
}

#[cfg(windows)]
struct NativeSteamBackend {
    client: steamworks::Client,
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
        Ok(Self { client })
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
            .fetch(move |result| {
                let response = result
                    .map(|results| {
                        results
                            .iter()
                            .filter_map(|item| item.map(workshop_item_from_query_result))
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
        })
    }

    fn items(&mut self, ids: &[String]) -> Result<Vec<WorkshopItem>, HelperError> {
        self.workshop_items_for_ids(ids)
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
            | HelperCommand::GetAuthors => {
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
        })
    }
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
fn workshop_item_from_query_result(item: steamworks::QueryResult) -> WorkshopItem {
    WorkshopItem {
        published_file_id: item.published_file_id.0.to_string(),
        title: item.title,
        description: item.description,
        tags: item.tags,
        owner: WorkshopOwner {
            steam_id64: item.owner.raw().to_string(),
        },
        time_updated: u64::from(item.time_updated),
    }
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

fn append_command_log(path: Option<&Path>, request: &HelperRequest) -> Result<(), HelperError> {
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
    let entry = CommandResult {
        ok: true,
        app_id: request.app_id.clone(),
        command: request.command.as_str().to_string(),
        ids: request.ids(),
        update_requested_ids: Vec::new(),
        delay_ms: request.delay_ms,
    };
    let line = to_json(&entry)?;
    let mut existing = fs::read_to_string(path).unwrap_or_default();
    existing.push_str(&line);
    existing.push('\n');
    fs::write(path, existing).map_err(|error| {
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
                        "timeUpdated": 2
                    }
                ],
                "dependencies": {},
                "authors": {}
            }"#,
        );
        let paths = HelperPaths::new(Some(fixture_path.clone()), None);

        let output = run_with_args(["1142710", "getItems", "222,333"], &paths).unwrap();

        assert_eq!(
            output,
            r#"[{"publishedFileId":"222","title":"Dependency Mod","owner":{"steamId64":"76561198000000002"},"timeUpdated":2}]"#
        );
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
