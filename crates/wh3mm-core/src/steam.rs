//! Steam Workshop request safety state.
//!
//! This module intentionally does not perform network or Steam helper calls.
//! It owns the dedupe/cache/cooldown/batching decisions so platform adapters
//! can stay bounded and predictable.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

use serde::{Deserialize, Serialize};

const WH3MM_WORKSHOP_ID: &str = "2845454582";

/// Maximum number of Workshop items returned by one Steam UGC query page.
pub const WORKSHOP_CATALOG_PAGE_SIZE: usize = 50;
/// Maximum number of items accepted by one explicit bulk action.
pub const WORKSHOP_BULK_ACTION_LIMIT: usize = 200;

/// Workshop catalog to query.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub enum WorkshopCatalogScope {
    #[default]
    Discover,
    Subscribed,
    Favorites,
    Published,
    VotedUp,
    VotedDown,
    Followed,
}

impl WorkshopCatalogScope {
    /// Whether Steam supports full-text and tag filters for this scope.
    #[must_use]
    pub fn supports_discovery_filters(self) -> bool {
        self == Self::Discover
    }
}

/// Sorting mode for Workshop discovery or user-list queries.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub enum WorkshopCatalogSort {
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

impl WorkshopCatalogSort {
    /// Whether this sort is accepted for the requested catalog scope.
    #[must_use]
    pub fn supports_scope(self, scope: WorkshopCatalogScope) -> bool {
        if scope == WorkshopCatalogScope::Discover {
            matches!(
                self,
                Self::Relevance
                    | Self::Popular
                    | Self::Newest
                    | Self::Trending
                    | Self::MostSubscribed
                    | Self::Updated
            )
        } else {
            matches!(
                self,
                Self::Newest
                    | Self::Oldest
                    | Self::Title
                    | Self::Updated
                    | Self::SubscriptionDate
                    | Self::Score
            )
        }
    }
}

/// One explicit, paginated Workshop catalog request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub struct WorkshopCatalogQuery {
    pub scope: WorkshopCatalogScope,
    pub sort: WorkshopCatalogSort,
    #[serde(default)]
    pub search_text: String,
    #[serde(default)]
    pub required_tags: Vec<String>,
    #[serde(default)]
    pub match_any_tag: bool,
    pub page: u32,
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
    /// Returns a trimmed, deduplicated request or a user-facing validation error.
    ///
    /// # Errors
    ///
    /// Returns an error when the page is zero, the selected sort is invalid for
    /// the scope, or Discover-only filters are supplied to a user-list scope.
    pub fn normalized(&self) -> Result<Self, String> {
        if self.page == 0 {
            return Err("Workshop catalog pages start at 1".to_string());
        }
        if !self.sort.supports_scope(self.scope) {
            return Err(format!(
                "Workshop sort {:?} is not supported for {:?}",
                self.sort, self.scope
            ));
        }
        if !self.scope.supports_discovery_filters()
            && (!self.search_text.trim().is_empty() || !self.required_tags.is_empty())
        {
            return Err("Search text and tag filters are only supported by Discover".to_string());
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
        let sort = if self.scope == WorkshopCatalogScope::Discover
            && self.sort == WorkshopCatalogSort::Relevance
            && search_text.is_empty()
        {
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

    /// Stable fingerprint used to reject stale asynchronous responses.
    ///
    /// # Errors
    ///
    /// Returns the same validation errors as [`Self::normalized`], or a JSON
    /// serialization error if the normalized query cannot be encoded.
    pub fn fingerprint(&self) -> Result<String, String> {
        serde_json::to_string(&self.normalized()?).map_err(|error| error.to_string())
    }
}

/// Kind of published Workshop entry.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub enum WorkshopCatalogItemKind {
    #[default]
    Item,
    Collection,
}

/// Read-only aggregate statistics returned by Steam.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
#[allow(missing_docs)]
pub struct WorkshopItemStatistics {
    pub subscriptions: u64,
    pub favorites: u64,
    pub followers: u64,
    pub views: u64,
    pub comments: u64,
}

/// Local Steam client state and optional download byte counters.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
#[allow(missing_docs)]
#[allow(clippy::struct_excessive_bools)]
pub struct WorkshopItemState {
    pub workshop_id: String,
    pub subscribed: bool,
    pub installed: bool,
    pub needs_update: bool,
    pub downloading: bool,
    pub download_pending: bool,
    pub bytes_downloaded: Option<u64>,
    pub bytes_total: Option<u64>,
}

impl WorkshopItemState {
    /// Download progress in the inclusive range 0..=100 when totals are known.
    #[must_use]
    pub fn progress_percent(&self) -> Option<u8> {
        let total = self.bytes_total?;
        (total > 0)
            .then(|| ((self.bytes_downloaded.unwrap_or(0).min(total) * 100) / total).min(100) as u8)
    }

    /// Whether this item no longer needs monitoring.
    #[must_use]
    pub fn download_complete(&self) -> bool {
        self.installed && !self.needs_update && !self.downloading && !self.download_pending
    }
}

/// Enriched Workshop catalog item suitable for toolkit-neutral presentation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
#[allow(missing_docs)]
pub struct WorkshopCatalogItem {
    pub workshop_id: String,
    pub kind: WorkshopCatalogItemKind,
    pub title: String,
    pub description: String,
    pub owner_steam_id: String,
    pub author: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub tags: Vec<String>,
    pub preview_url: Option<String>,
    pub file_size: u64,
    pub upvotes: u32,
    pub downvotes: u32,
    pub score: f32,
    pub child_ids: Vec<String>,
    pub statistics: WorkshopItemStatistics,
    pub state: WorkshopItemState,
}

/// One page of Workshop catalog results.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub struct WorkshopCatalogPage {
    pub page: u32,
    pub total_results: u32,
    pub was_cached: bool,
    pub items: Vec<WorkshopCatalogItem>,
}

/// One bounded monitor sample for a set of Workshop IDs.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub struct WorkshopMonitorSnapshot {
    pub elapsed_ms: u64,
    pub items: Vec<WorkshopItemState>,
}

/// Why a bounded Workshop monitor stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub enum WorkshopMonitorCompletionReason {
    Complete,
    Timeout,
    Cancelled,
}

/// Final state emitted by a Workshop monitor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub struct WorkshopMonitorCompletion {
    pub reason: WorkshopMonitorCompletionReason,
    pub snapshot: WorkshopMonitorSnapshot,
}

/// Replaces item states by normalized Workshop ID without changing item order.
pub fn merge_workshop_item_states(items: &mut [WorkshopCatalogItem], states: &[WorkshopItemState]) {
    let states = states
        .iter()
        .filter_map(|state| normalize_workshop_id(&state.workshop_id).map(|id| (id, state)))
        .collect::<BTreeMap<_, _>>();
    for item in items {
        if let Some(state) = states.get(&item.workshop_id) {
            item.state = (*state).clone();
        }
    }
}

/// Resolved Steam Workshop metadata used by mod-list and dependency flows.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkshopModData {
    /// Workshop ID.
    pub workshop_id: String,
    /// User-facing Steam Workshop title.
    pub title: String,
    /// Resolved author display name, when available.
    pub author: String,
    /// Steam Workshop description, when supplied by the helper.
    #[serde(default)]
    pub description: String,
    /// Steam Workshop tags, when supplied by the helper.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Required workshop dependency IDs.
    pub dependency_ids: Vec<String>,
    /// Required workshop dependency IDs paired with known titles.
    pub dependency_id_to_name: Vec<(String, String)>,
    /// Last update timestamp in Unix milliseconds.
    pub last_changed_ms: u64,
}

/// Versioned on-disk cache of Workshop metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkshopMetadataCache {
    /// Cache schema version.
    pub version: u32,
    /// Cached entries keyed by their embedded Workshop IDs.
    pub entries: Vec<WorkshopMetadataCacheEntry>,
}

/// One Workshop metadata record plus the time it was fetched.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkshopMetadataCacheEntry {
    /// Metadata returned by the helper.
    pub metadata: WorkshopModData,
    /// Cache insertion/update time in Unix milliseconds.
    pub fetched_at_ms: u64,
}

impl Default for WorkshopMetadataCache {
    fn default() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
        }
    }
}

impl WorkshopMetadataCache {
    /// Returns every cached metadata row matching the normalized requested IDs.
    #[must_use]
    pub fn metadata_for_ids(&self, workshop_ids: &[String]) -> Vec<WorkshopModData> {
        let requested = workshop_ids
            .iter()
            .filter_map(|id| normalize_workshop_id(id))
            .collect::<BTreeSet<_>>();
        self.entries
            .iter()
            .filter(|entry| requested.contains(&entry.metadata.workshop_id))
            .map(|entry| entry.metadata.clone())
            .collect()
    }

    /// Returns normalized IDs that are absent or older than `ttl_ms`.
    #[must_use]
    pub fn ids_needing_refresh(
        &self,
        workshop_ids: &[String],
        now_ms: u64,
        ttl_ms: u64,
    ) -> Vec<String> {
        let entries = self
            .entries
            .iter()
            .map(|entry| (entry.metadata.workshop_id.as_str(), entry.fetched_at_ms))
            .collect::<BTreeMap<_, _>>();
        workshop_ids
            .iter()
            .filter_map(|id| normalize_workshop_id(id))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|id| {
                entries
                    .get(id.as_str())
                    .is_none_or(|fetched_at_ms| now_ms.saturating_sub(*fetched_at_ms) >= ttl_ms)
            })
            .collect()
    }

    /// Inserts or replaces metadata rows and records one shared fetch time.
    pub fn merge(&mut self, metadata: &[WorkshopModData], fetched_at_ms: u64) {
        for metadata in metadata {
            let Some(workshop_id) = normalize_workshop_id(&metadata.workshop_id) else {
                continue;
            };
            let mut metadata = metadata.clone();
            metadata.workshop_id.clone_from(&workshop_id);
            if let Some(existing) = self
                .entries
                .iter_mut()
                .find(|entry| entry.metadata.workshop_id == workshop_id)
            {
                existing.metadata = metadata;
                existing.fetched_at_ms = fetched_at_ms;
            } else {
                self.entries.push(WorkshopMetadataCacheEntry {
                    metadata,
                    fetched_at_ms,
                });
            }
        }
        self.entries
            .sort_by(|left, right| left.metadata.workshop_id.cmp(&right.metadata.workshop_id));
    }

    /// Removes cache entries that are no longer part of the supplied library.
    pub fn retain_ids(&mut self, workshop_ids: &[String]) {
        let requested = workshop_ids
            .iter()
            .filter_map(|id| normalize_workshop_id(id))
            .collect::<BTreeSet<_>>();
        self.entries
            .retain(|entry| requested.contains(&entry.metadata.workshop_id));
    }
}

/// Safety settings for workshop metadata requests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamWorkshopSafetyConfig {
    /// Maximum IDs in one metadata request batch.
    pub batch_size: usize,
    /// Minimum delay adapters should wait between non-empty batches.
    pub batch_delay: Duration,
    /// Successful metadata cache TTL.
    pub cache_ttl: Duration,
    /// Per-ID cooldown after failed or missing responses.
    pub failure_cooldown: Duration,
    /// Global cooldown after repeated batch failures.
    pub global_cooldown: Duration,
    /// Base retry delay for the first retry attempt.
    pub retry_base_delay: Duration,
    /// Deterministic upper bound for retry jitter supplied by adapters.
    pub retry_jitter_max: Duration,
    /// Maximum number of attempts per batch.
    pub max_retries: u32,
    /// Consecutive failed batches required before global cooldown starts.
    pub failure_threshold_for_global_cooldown: u32,
}

impl Default for SteamWorkshopSafetyConfig {
    fn default() -> Self {
        Self {
            batch_size: 40,
            batch_delay: Duration::from_millis(1_000),
            cache_ttl: Duration::from_secs(10 * 60),
            failure_cooldown: Duration::from_secs(2 * 60),
            global_cooldown: Duration::from_secs(60),
            retry_base_delay: Duration::from_millis(1_200),
            retry_jitter_max: Duration::from_millis(800),
            max_retries: 3,
            failure_threshold_for_global_cooldown: 3,
        }
    }
}

/// Cached workshop metadata value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CachedWorkshopData<T> {
    /// Workshop ID.
    pub workshop_id: String,
    /// Adapter-owned data.
    pub data: T,
}

/// Result of queueing a set of requested workshop IDs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueWorkshopIdsResult<T> {
    /// Fresh cached data that can be returned immediately.
    pub cached: Vec<CachedWorkshopData<T>>,
    /// Newly queued IDs.
    pub queued: Vec<String>,
    /// IDs skipped because they are cooling down after a failure/missing response.
    pub skipped_cooling_down: Vec<String>,
    /// IDs skipped because they are already queued or in flight.
    pub skipped_pending: Vec<String>,
    /// Invalid, empty, non-numeric, or duplicate input IDs skipped.
    pub skipped_invalid_or_duplicate: Vec<String>,
}

/// One planned metadata batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkshopBatchPlan {
    /// IDs to request.
    pub ids: Vec<String>,
    /// Delay a caller should wait before this batch may be sent.
    pub wait_before_request: Duration,
}

/// Retry schedule for a batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetryDelayPlan {
    /// Attempt number that failed, starting at `1`.
    pub failed_attempt: u32,
    /// Delay before the next attempt.
    pub delay: Duration,
}

/// Error returned by a Steam/workshop adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamWorkshopAdapterError {
    /// Stable error category.
    pub kind: SteamWorkshopAdapterErrorKind,
    /// Human-readable diagnostic.
    pub message: String,
}

impl SteamWorkshopAdapterError {
    /// Creates an adapter error.
    #[must_use]
    pub fn new(kind: SteamWorkshopAdapterErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Returns true when this error should trigger aggressive cooldown behavior.
    #[must_use]
    pub fn should_cool_down_globally(&self) -> bool {
        matches!(
            self.kind,
            SteamWorkshopAdapterErrorKind::RateLimited
                | SteamWorkshopAdapterErrorKind::Forbidden
                | SteamWorkshopAdapterErrorKind::RepeatedFailure
        )
    }
}

/// Stable Steam/workshop adapter error categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SteamWorkshopAdapterErrorKind {
    /// Steam or HTTP reported rate limiting.
    RateLimited,
    /// Steam or HTTP rejected the request.
    Forbidden,
    /// Repeated failures exhausted retry policy.
    RepeatedFailure,
    /// Steam client/API was unavailable.
    Unavailable,
    /// Adapter returned malformed data.
    MalformedResponse,
    /// Other adapter failure.
    Other,
}

/// Adapter boundary for live Steam Workshop metadata lookups.
pub trait SteamWorkshopMetadataAdapter {
    /// Fetches metadata for a bounded batch of normalized workshop IDs.
    ///
    /// # Errors
    ///
    /// Returns [`SteamWorkshopAdapterError`] when Steam, HTTP, or another
    /// external adapter boundary fails.
    fn fetch_mod_data_batch(
        &mut self,
        workshop_ids: &[String],
    ) -> Result<Vec<WorkshopModData>, SteamWorkshopAdapterError>;
}

/// Parses the legacy TypeScript Steam helper `getModsData` response into the
/// Rust workshop metadata model.
///
/// `dependency_items_json` may contain the helper `getItems` response for
/// dependency IDs that were not present in the primary response.
///
/// # Errors
///
/// Returns [`SteamWorkshopAdapterError`] when either helper response is not
/// valid JSON for the expected TypeScript helper shape.
pub fn parse_ts_steam_helper_mod_data_response(
    mods_data_json: &str,
    dependency_items_json: Option<&str>,
) -> Result<Vec<WorkshopModData>, SteamWorkshopAdapterError> {
    let mods_data = serde_json::from_str::<TsSteamModsData>(mods_data_json).map_err(|error| {
        SteamWorkshopAdapterError::new(
            SteamWorkshopAdapterErrorKind::MalformedResponse,
            format!("failed to parse TS Steam helper getModsData response: {error}"),
        )
    })?;
    let dependency_items = dependency_items_json
        .map(|json| {
            serde_json::from_str::<Vec<TsSteamWorkshopItem>>(json).map_err(|error| {
                SteamWorkshopAdapterError::new(
                    SteamWorkshopAdapterErrorKind::MalformedResponse,
                    format!("failed to parse TS Steam helper getItems response: {error}"),
                )
            })
        })
        .transpose()?
        .unwrap_or_default();

    Ok(workshop_mod_data_from_ts_helper_response(
        mods_data,
        &dependency_items,
    ))
}

/// Returns dependency IDs from a TypeScript helper `getModsData` response that
/// need a follow-up helper `getItems` call to resolve dependency titles.
///
/// # Errors
///
/// Returns [`SteamWorkshopAdapterError`] when the helper response is malformed.
pub fn ts_steam_helper_dependency_ids_needing_titles(
    mods_data_json: &str,
) -> Result<Vec<String>, SteamWorkshopAdapterError> {
    let mods_data = serde_json::from_str::<TsSteamModsData>(mods_data_json).map_err(|error| {
        SteamWorkshopAdapterError::new(
            SteamWorkshopAdapterErrorKind::MalformedResponse,
            format!("failed to parse TS Steam helper getModsData response: {error}"),
        )
    })?;
    let primary_ids = mods_data
        .mods
        .iter()
        .filter_map(|item| normalize_workshop_id(&item.published_file_id))
        .collect::<BTreeSet<_>>();
    let mut dependency_ids = BTreeSet::new();
    for dependency_id in mods_data.dependencies.values().flatten() {
        if let Some(dependency_id) = normalize_workshop_id(dependency_id)
            && dependency_id != WH3MM_WORKSHOP_ID
            && !primary_ids.contains(&dependency_id)
        {
            dependency_ids.insert(dependency_id);
        }
    }

    Ok(dependency_ids.into_iter().collect())
}

/// Result of one attempt to advance a workshop metadata queue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkshopMetadataFetchStep {
    /// No queued IDs are waiting.
    Idle,
    /// A batch exists but should not be sent yet.
    Waiting {
        /// Delay remaining before a request may be sent.
        wait_before_request: Duration,
    },
    /// Adapter returned data for a ready batch.
    Fetched {
        /// IDs requested from the adapter.
        requested_ids: Vec<String>,
        /// Metadata received from the adapter.
        data: Vec<WorkshopModData>,
        /// Requested IDs absent from the adapter response.
        missing_ids: Vec<String>,
    },
    /// Adapter failed for a ready batch.
    Failed {
        /// IDs requested from the adapter.
        requested_ids: Vec<String>,
        /// Adapter error.
        error: SteamWorkshopAdapterError,
        /// Retry delays configured for future adapter use.
        retry_delays: Vec<RetryDelayPlan>,
    },
}

/// Queue/cache/cooldown state for safe workshop metadata fetches.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamWorkshopRequestState<T> {
    config: SteamWorkshopSafetyConfig,
    cache: BTreeMap<String, TimedValue<T>>,
    failure_cooldown_until: BTreeMap<String, u64>,
    queued: VecDeque<String>,
    queued_set: BTreeSet<String>,
    in_flight: BTreeSet<String>,
    consecutive_failures: u32,
    global_cooldown_until: u64,
    last_batch_at: Option<u64>,
}

impl<T> SteamWorkshopRequestState<T> {
    /// Creates empty request state.
    #[must_use]
    pub fn new(config: SteamWorkshopSafetyConfig) -> Self {
        Self {
            config,
            cache: BTreeMap::new(),
            failure_cooldown_until: BTreeMap::new(),
            queued: VecDeque::new(),
            queued_set: BTreeSet::new(),
            in_flight: BTreeSet::new(),
            consecutive_failures: 0,
            global_cooldown_until: 0,
            last_batch_at: None,
        }
    }

    /// Returns the active safety configuration.
    #[must_use]
    pub fn config(&self) -> &SteamWorkshopSafetyConfig {
        &self.config
    }

    /// Returns the number of queued IDs.
    #[must_use]
    pub fn queued_len(&self) -> usize {
        self.queued.len()
    }

    /// Returns the number of in-flight IDs.
    #[must_use]
    pub fn in_flight_len(&self) -> usize {
        self.in_flight.len()
    }

    /// Returns whether global cooldown is active at `now_ms`.
    #[must_use]
    pub fn is_global_cooling_down(&self, now_ms: u64) -> bool {
        self.global_cooldown_until > now_ms
    }

    /// Returns the current global cooldown-until timestamp in milliseconds.
    #[must_use]
    pub fn global_cooldown_until(&self) -> u64 {
        self.global_cooldown_until
    }

    /// Normalizes, dedupes, serves fresh cache, and queues eligible IDs.
    pub fn queue_workshop_ids(
        &mut self,
        ids: impl IntoIterator<Item = impl AsRef<str>>,
        now_ms: u64,
    ) -> QueueWorkshopIdsResult<T>
    where
        T: Clone,
    {
        self.prune(now_ms);
        let mut seen = BTreeSet::new();
        let mut result = QueueWorkshopIdsResult {
            cached: Vec::new(),
            queued: Vec::new(),
            skipped_cooling_down: Vec::new(),
            skipped_pending: Vec::new(),
            skipped_invalid_or_duplicate: Vec::new(),
        };

        for raw_id in ids {
            let Some(workshop_id) = normalize_workshop_id(raw_id.as_ref()) else {
                result
                    .skipped_invalid_or_duplicate
                    .push(raw_id.as_ref().trim().to_string());
                continue;
            };
            if !seen.insert(workshop_id.clone()) {
                result.skipped_invalid_or_duplicate.push(workshop_id);
                continue;
            }

            if let Some(cached) = self.cache.get(&workshop_id) {
                result.cached.push(CachedWorkshopData {
                    workshop_id,
                    data: cached.value.clone(),
                });
                continue;
            }

            if self
                .failure_cooldown_until
                .get(&workshop_id)
                .is_some_and(|cooldown_until| *cooldown_until > now_ms)
            {
                result.skipped_cooling_down.push(workshop_id);
                continue;
            }

            if self.queued_set.contains(&workshop_id) || self.in_flight.contains(&workshop_id) {
                result.skipped_pending.push(workshop_id);
                continue;
            }

            self.queued.push_back(workshop_id.clone());
            self.queued_set.insert(workshop_id.clone());
            result.queued.push(workshop_id);
        }

        result
    }

    /// Plans and marks the next eligible batch as in-flight.
    pub fn next_batch(&mut self, now_ms: u64) -> Option<WorkshopBatchPlan> {
        if self.queued.is_empty() {
            return None;
        }

        let wait_for_global = self.global_cooldown_until.saturating_sub(now_ms);
        let wait_for_batch_delay = self.last_batch_at.map_or(0, |last_batch_at| {
            last_batch_at
                .saturating_add(duration_ms(self.config.batch_delay))
                .saturating_sub(now_ms)
        });
        let wait_before_request = Duration::from_millis(wait_for_global.max(wait_for_batch_delay));

        let batch_len = self.config.batch_size.max(1).min(self.queued.len());
        let ids = (0..batch_len)
            .filter_map(|_| {
                let workshop_id = self.queued.pop_front()?;
                self.queued_set.remove(&workshop_id);
                self.in_flight.insert(workshop_id.clone());
                Some(workshop_id)
            })
            .collect::<Vec<_>>();

        if ids.is_empty() {
            None
        } else {
            Some(WorkshopBatchPlan {
                ids,
                wait_before_request,
            })
        }
    }

    /// Returns the delay before a queued batch may be sent without claiming it.
    #[must_use]
    pub fn wait_before_next_batch(&self, now_ms: u64) -> Option<Duration> {
        if self.queued.is_empty() {
            return None;
        }

        Some(Duration::from_millis(
            self.wait_before_next_batch_ms(now_ms),
        ))
    }

    /// Records a successful batch response.
    pub fn finish_success(
        &mut self,
        requested_ids: &[String],
        received: impl IntoIterator<Item = CachedWorkshopData<T>>,
        now_ms: u64,
    ) {
        self.consecutive_failures = 0;
        self.last_batch_at = Some(now_ms);

        let mut received_ids = BTreeSet::new();
        for cached in received {
            received_ids.insert(cached.workshop_id.clone());
            self.cache.insert(
                cached.workshop_id.clone(),
                TimedValue {
                    value: cached.data,
                    fetched_at: now_ms,
                },
            );
            self.failure_cooldown_until.remove(&cached.workshop_id);
        }

        for workshop_id in requested_ids {
            self.in_flight.remove(workshop_id);
            if !received_ids.contains(workshop_id) {
                self.failure_cooldown_until.insert(
                    workshop_id.clone(),
                    now_ms.saturating_add(duration_ms(self.config.failure_cooldown)),
                );
            }
        }
    }

    /// Records a failed batch response.
    pub fn finish_failure(&mut self, requested_ids: &[String], now_ms: u64) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_batch_at = Some(now_ms);

        for workshop_id in requested_ids {
            self.in_flight.remove(workshop_id);
            self.failure_cooldown_until.insert(
                workshop_id.clone(),
                now_ms.saturating_add(duration_ms(self.config.failure_cooldown)),
            );
        }

        if self.consecutive_failures >= self.config.failure_threshold_for_global_cooldown {
            self.global_cooldown_until =
                now_ms.saturating_add(duration_ms(self.config.global_cooldown));
        }
    }

    /// Returns deterministic retry delays for attempts before final failure.
    #[must_use]
    pub fn retry_delays(&self) -> Vec<RetryDelayPlan> {
        (1..self.config.max_retries)
            .map(|failed_attempt| {
                let multiplier = 2_u32.saturating_pow(failed_attempt.saturating_sub(1));
                RetryDelayPlan {
                    failed_attempt,
                    delay: self
                        .config
                        .retry_base_delay
                        .saturating_mul(multiplier)
                        .saturating_add(self.config.retry_jitter_max),
                }
            })
            .collect()
    }

    fn prune(&mut self, now_ms: u64) {
        let cache_ttl_ms = duration_ms(self.config.cache_ttl);
        self.cache
            .retain(|_, cached| now_ms.saturating_sub(cached.fetched_at) <= cache_ttl_ms);
        self.failure_cooldown_until
            .retain(|_, cooldown_until| *cooldown_until > now_ms);
    }

    fn wait_before_next_batch_ms(&self, now_ms: u64) -> u64 {
        let wait_for_global = self.global_cooldown_until.saturating_sub(now_ms);
        let wait_for_batch_delay = self.last_batch_at.map_or(0, |last_batch_at| {
            last_batch_at
                .saturating_add(duration_ms(self.config.batch_delay))
                .saturating_sub(now_ms)
        });
        wait_for_global.max(wait_for_batch_delay)
    }
}

impl SteamWorkshopRequestState<WorkshopModData> {
    /// Advances the queue only when a batch is ready to send now.
    ///
    /// This is the safe adapter integration path for polling runtimes: queued
    /// IDs are not claimed as in-flight while a delay or global cooldown is
    /// still active.
    pub fn fetch_ready_workshop_batch<A>(
        &mut self,
        adapter: &mut A,
        now_ms: u64,
    ) -> WorkshopMetadataFetchStep
    where
        A: SteamWorkshopMetadataAdapter,
    {
        let Some(wait_before_request) = self.wait_before_next_batch(now_ms) else {
            return WorkshopMetadataFetchStep::Idle;
        };

        if !wait_before_request.is_zero() {
            return WorkshopMetadataFetchStep::Waiting {
                wait_before_request,
            };
        }

        let Some(batch) = self.next_batch(now_ms) else {
            return WorkshopMetadataFetchStep::Idle;
        };
        let requested_ids = batch.ids;

        match adapter.fetch_mod_data_batch(&requested_ids) {
            Ok(data) => {
                let received_ids = data
                    .iter()
                    .map(|mod_data| mod_data.workshop_id.clone())
                    .collect::<BTreeSet<_>>();
                let missing_ids = requested_ids
                    .iter()
                    .filter(|workshop_id| !received_ids.contains(*workshop_id))
                    .cloned()
                    .collect::<Vec<_>>();
                self.finish_success(
                    &requested_ids,
                    data.iter().cloned().map(|mod_data| CachedWorkshopData {
                        workshop_id: mod_data.workshop_id.clone(),
                        data: mod_data,
                    }),
                    now_ms,
                );

                WorkshopMetadataFetchStep::Fetched {
                    requested_ids,
                    data,
                    missing_ids,
                }
            }
            Err(error) => {
                self.finish_failure(&requested_ids, now_ms);
                if error.should_cool_down_globally() {
                    self.global_cooldown_until = self
                        .global_cooldown_until
                        .max(now_ms.saturating_add(duration_ms(self.config.global_cooldown)));
                }
                WorkshopMetadataFetchStep::Failed {
                    requested_ids,
                    error,
                    retry_delays: self.retry_delays(),
                }
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TimedValue<T> {
    value: T,
    fetched_at: u64,
}

/// Normalizes a Steam Workshop ID for request planning.
#[must_use]
pub fn normalize_workshop_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty() && trimmed.chars().all(|character| character.is_ascii_digit()))
        .then(|| trimmed.to_string())
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn workshop_mod_data_from_ts_helper_response(
    mods_data: TsSteamModsData,
    dependency_items: &[TsSteamWorkshopItem],
) -> Vec<WorkshopModData> {
    let mut mod_id_to_name = BTreeMap::new();
    for item in dependency_items.iter().chain(mods_data.mods.iter()) {
        if let Some(workshop_id) = normalize_workshop_id(&item.published_file_id) {
            mod_id_to_name.insert(workshop_id, item.title.clone());
        }
    }

    mods_data
        .mods
        .into_iter()
        .filter_map(|item| {
            let workshop_id = normalize_workshop_id(&item.published_file_id)?;
            let dependency_ids = mods_data
                .dependencies
                .get(&workshop_id)
                .into_iter()
                .flatten()
                .filter_map(|dependency_id| normalize_workshop_id(dependency_id))
                .filter(|dependency_id| dependency_id != WH3MM_WORKSHOP_ID)
                .collect::<Vec<_>>();
            let dependency_id_to_name = dependency_ids
                .iter()
                .map(|dependency_id| {
                    (
                        dependency_id.clone(),
                        mod_id_to_name
                            .get(dependency_id)
                            .cloned()
                            .unwrap_or_default(),
                    )
                })
                .collect::<Vec<_>>();
            let author = mods_data
                .authors
                .get(&item.owner.steam_id64)
                .cloned()
                .unwrap_or_default();

            Some(WorkshopModData {
                workshop_id,
                title: item.title,
                author,
                description: item.description,
                tags: item.tags,
                dependency_ids,
                dependency_id_to_name,
                last_changed_ms: item.time_updated.saturating_mul(1_000),
            })
        })
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TsSteamModsData {
    #[serde(default)]
    mods: Vec<TsSteamWorkshopItem>,
    #[serde(default)]
    dependencies: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    authors: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TsSteamWorkshopItem {
    published_file_id: String,
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tags: Vec<String>,
    owner: TsSteamWorkshopOwner,
    #[serde(default)]
    time_updated: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TsSteamWorkshopOwner {
    steam_id64: String,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        CachedWorkshopData, SteamWorkshopAdapterError, SteamWorkshopAdapterErrorKind,
        SteamWorkshopMetadataAdapter, SteamWorkshopRequestState, SteamWorkshopSafetyConfig,
        WorkshopCatalogItem, WorkshopCatalogQuery, WorkshopCatalogScope, WorkshopCatalogSort,
        WorkshopItemState, WorkshopMetadataCache, WorkshopMetadataFetchStep, WorkshopModData,
        merge_workshop_item_states, normalize_workshop_id, parse_ts_steam_helper_mod_data_response,
        ts_steam_helper_dependency_ids_needing_titles,
    };

    #[test]
    fn normalizes_numeric_workshop_ids_only() {
        assert_eq!(normalize_workshop_id(" 12345 "), Some("12345".to_string()));
        assert_eq!(normalize_workshop_id(""), None);
        assert_eq!(normalize_workshop_id("abc"), None);
        assert_eq!(normalize_workshop_id("12x"), None);
    }

    #[test]
    fn catalog_query_normalizes_tags_search_and_relevance() {
        let query = WorkshopCatalogQuery {
            search_text: "  balance  ".to_string(),
            required_tags: vec![
                "Units".to_string(),
                " units ".to_string(),
                "Campaign".to_string(),
            ],
            sort: WorkshopCatalogSort::Relevance,
            ..WorkshopCatalogQuery::default()
        };
        let normalized = query.normalized().unwrap();
        assert_eq!(normalized.search_text, "balance");
        assert_eq!(normalized.required_tags, ["Units", "Campaign"]);
        assert_eq!(normalized.sort, WorkshopCatalogSort::Relevance);

        let empty = WorkshopCatalogQuery {
            sort: WorkshopCatalogSort::Relevance,
            ..WorkshopCatalogQuery::default()
        };
        assert_eq!(
            empty.normalized().unwrap().sort,
            WorkshopCatalogSort::Popular
        );
    }

    #[test]
    fn catalog_query_rejects_discovery_filters_on_user_lists() {
        let query = WorkshopCatalogQuery {
            scope: WorkshopCatalogScope::Favorites,
            sort: WorkshopCatalogSort::Updated,
            search_text: "units".to_string(),
            ..WorkshopCatalogQuery::default()
        };
        assert!(query.normalized().is_err());
    }

    #[test]
    fn catalog_fingerprint_is_stable_after_normalization() {
        let left = WorkshopCatalogQuery {
            search_text: "  balance".to_string(),
            required_tags: vec!["Units".to_string(), "units".to_string()],
            ..WorkshopCatalogQuery::default()
        };
        let right = WorkshopCatalogQuery {
            search_text: "balance".to_string(),
            required_tags: vec!["Units".to_string()],
            ..WorkshopCatalogQuery::default()
        };
        assert_eq!(left.fingerprint().unwrap(), right.fingerprint().unwrap());
    }

    #[test]
    fn merges_item_states_and_clamps_progress() {
        let mut items = vec![WorkshopCatalogItem {
            workshop_id: "11".to_string(),
            ..WorkshopCatalogItem::default()
        }];
        let state = WorkshopItemState {
            workshop_id: "11".to_string(),
            downloading: true,
            bytes_downloaded: Some(150),
            bytes_total: Some(100),
            ..WorkshopItemState::default()
        };
        merge_workshop_item_states(&mut items, std::slice::from_ref(&state));
        assert_eq!(items[0].state, state);
        assert_eq!(items[0].state.progress_percent(), Some(100));
    }

    #[test]
    fn queues_unique_uncached_ids_and_skips_invalid_duplicates_and_pending() {
        let mut state = SteamWorkshopRequestState::<String>::new(test_config());

        let first = state.queue_workshop_ids(["1", "1", "abc", "2"], 0);
        let second = state.queue_workshop_ids(["1", "2", "3"], 0);

        assert_eq!(first.queued, vec!["1", "2"]);
        assert_eq!(first.skipped_invalid_or_duplicate, vec!["1", "abc"]);
        assert_eq!(second.queued, vec!["3"]);
        assert_eq!(second.skipped_pending, vec!["1", "2"]);
    }

    #[test]
    fn returns_fresh_cached_data_without_queueing() {
        let mut state = SteamWorkshopRequestState::new(test_config());
        let batch = queue_and_take_batch(&mut state, ["1"], 0);
        state.finish_success(
            &batch,
            [CachedWorkshopData {
                workshop_id: "1".to_string(),
                data: "cached".to_string(),
            }],
            10,
        );

        let result = state.queue_workshop_ids(["1"], 20);

        assert_eq!(result.cached[0].data, "cached");
        assert!(result.queued.is_empty());
    }

    #[test]
    fn skips_missing_response_ids_until_failure_cooldown_expires() {
        let mut state = SteamWorkshopRequestState::<String>::new(test_config());
        let batch = queue_and_take_batch(&mut state, ["1", "2"], 0);

        state.finish_success(
            &batch,
            [CachedWorkshopData {
                workshop_id: "1".to_string(),
                data: "ok".to_string(),
            }],
            10,
        );

        let cooling = state.queue_workshop_ids(["2"], 20);
        let after_cooldown = state.queue_workshop_ids(["2"], 2_020);

        assert_eq!(cooling.skipped_cooling_down, vec!["2"]);
        assert_eq!(after_cooldown.queued, vec!["2"]);
    }

    #[test]
    fn plans_batches_with_batch_delay_and_global_cooldown() {
        let mut state = SteamWorkshopRequestState::<String>::new(test_config());
        let first_batch = queue_and_take_batch(&mut state, ["1", "2", "3"], 0);
        state.finish_failure(&first_batch, 0);
        let second_batch = queue_and_take_batch(&mut state, ["4"], 10);
        state.finish_failure(&second_batch, 10);
        state.queue_workshop_ids(["5"], 20);

        let third_plan = state.next_batch(20).unwrap();

        assert_eq!(third_plan.ids, vec!["5"]);
        assert_eq!(third_plan.wait_before_request, Duration::from_millis(1_990));
        assert_eq!(state.global_cooldown_until(), 2_010);
    }

    #[test]
    fn retry_delays_use_exponential_backoff_with_max_jitter() {
        let state = SteamWorkshopRequestState::<String>::new(test_config());

        let delays = state.retry_delays();

        assert_eq!(delays.len(), 2);
        assert_eq!(delays[0].delay, Duration::from_millis(150));
        assert_eq!(delays[1].delay, Duration::from_millis(250));
    }

    #[test]
    fn ready_fetch_waits_without_claiming_queued_ids() {
        let mut state = SteamWorkshopRequestState::<WorkshopModData>::new(test_config());
        let first_batch = queue_and_take_metadata_batch(&mut state, ["1"], 0);
        state.finish_success(
            &first_batch,
            [CachedWorkshopData {
                workshop_id: "1".to_string(),
                data: workshop_mod_data("1", "One"),
            }],
            100,
        );
        state.queue_workshop_ids(["2"], 200);
        let mut adapter = RecordingWorkshopAdapter::ok(vec![workshop_mod_data("2", "Two")]);

        let step = state.fetch_ready_workshop_batch(&mut adapter, 200);

        assert_eq!(
            step,
            WorkshopMetadataFetchStep::Waiting {
                wait_before_request: Duration::from_millis(900),
            }
        );
        assert_eq!(state.queued_len(), 1);
        assert_eq!(state.in_flight_len(), 0);
        assert!(adapter.requested_batches.is_empty());
    }

    #[test]
    fn ready_fetch_records_success_and_missing_ids() {
        let mut state = SteamWorkshopRequestState::<WorkshopModData>::new(test_config());
        state.queue_workshop_ids(["1", "2"], 0);
        let mut adapter = RecordingWorkshopAdapter::ok(vec![workshop_mod_data("1", "One")]);

        let step = state.fetch_ready_workshop_batch(&mut adapter, 0);

        assert_eq!(adapter.requested_batches, vec![vec!["1", "2"]]);
        assert_eq!(
            step,
            WorkshopMetadataFetchStep::Fetched {
                requested_ids: vec!["1".to_string(), "2".to_string()],
                data: vec![workshop_mod_data("1", "One")],
                missing_ids: vec!["2".to_string()],
            }
        );
        assert_eq!(state.in_flight_len(), 0);
        assert_eq!(
            state.queue_workshop_ids(["1"], 10).cached,
            vec![CachedWorkshopData {
                workshop_id: "1".to_string(),
                data: workshop_mod_data("1", "One"),
            }]
        );
        assert_eq!(
            state.queue_workshop_ids(["2"], 10).skipped_cooling_down,
            vec!["2"]
        );
    }

    #[test]
    fn rate_limit_adapter_failure_starts_global_cooldown_immediately() {
        let mut state = SteamWorkshopRequestState::<WorkshopModData>::new(test_config());
        state.queue_workshop_ids(["1"], 0);
        let error = SteamWorkshopAdapterError::new(
            SteamWorkshopAdapterErrorKind::RateLimited,
            "too many requests",
        );
        let mut adapter = RecordingWorkshopAdapter::err(error.clone());

        let step = state.fetch_ready_workshop_batch(&mut adapter, 0);

        assert_eq!(
            step,
            WorkshopMetadataFetchStep::Failed {
                requested_ids: vec!["1".to_string()],
                error,
                retry_delays: state.retry_delays(),
            }
        );
        assert_eq!(state.global_cooldown_until(), 2_000);
        assert!(state.is_global_cooling_down(1_999));
        assert_eq!(state.in_flight_len(), 0);
    }

    #[test]
    fn persistent_metadata_cache_returns_fresh_rows_and_flags_stale_or_missing_ids() {
        let mut cache = WorkshopMetadataCache::default();
        cache.merge(
            &[workshop_mod_data("2", "Two"), workshop_mod_data("1", "One")],
            1_000,
        );

        let ids = vec!["2".to_string(), "1".to_string(), "3".to_string()];
        assert_eq!(
            cache
                .metadata_for_ids(&ids)
                .into_iter()
                .map(|row| row.workshop_id)
                .collect::<Vec<_>>(),
            ["1", "2"]
        );
        assert_eq!(cache.ids_needing_refresh(&ids, 1_999, 1_000), ["3"]);
        assert_eq!(
            cache.ids_needing_refresh(&ids, 2_000, 1_000),
            ["1", "2", "3"]
        );
    }

    #[test]
    fn persistent_metadata_cache_normalizes_replaces_and_retain_ids() {
        let mut cache = WorkshopMetadataCache::default();
        cache.merge(&[workshop_mod_data(" 2 ", "Old")], 100);
        cache.merge(&[workshop_mod_data("2", "New")], 200);
        cache.merge(&[workshop_mod_data("invalid", "Ignored")], 300);

        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.entries[0].metadata.title, "New");
        assert_eq!(cache.entries[0].fetched_at_ms, 200);

        cache.retain_ids(&["3".to_string()]);
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn parses_ts_steam_helper_mod_data_response() {
        let mods_data_json = r#"
            {
              "mods": [
                {
                  "publishedFileId": "111",
                  "title": "Main Mod",
                  "description": "A detailed description",
                  "tags": ["Units", "Campaign"],
                  "owner": { "steamId64": "76561198000000001" },
                  "timeUpdated": 1234
                }
              ],
              "dependencies": {
                "111": ["222", "2845454582", "not-an-id"]
              },
              "authors": {
                "76561198000000001": "Mod Author"
              }
            }
        "#;
        let dependency_items_json = r#"
            [
              {
                "publishedFileId": "222",
                "title": "Dependency Mod",
                "owner": { "steamId64": "76561198000000002" },
                "timeUpdated": 999
              }
            ]
        "#;

        let parsed =
            parse_ts_steam_helper_mod_data_response(mods_data_json, Some(dependency_items_json))
                .unwrap();

        assert_eq!(
            parsed,
            vec![WorkshopModData {
                workshop_id: "111".to_string(),
                title: "Main Mod".to_string(),
                author: "Mod Author".to_string(),
                description: "A detailed description".to_string(),
                tags: vec!["Units".to_string(), "Campaign".to_string()],
                dependency_ids: vec!["222".to_string()],
                dependency_id_to_name: vec![("222".to_string(), "Dependency Mod".to_string())],
                last_changed_ms: 1_234_000,
            }]
        );
    }

    #[test]
    fn extracts_ts_steam_helper_dependency_ids_needing_titles() {
        let mods_data_json = r#"
            {
              "mods": [
                {
                  "publishedFileId": "111",
                  "title": "Main Mod",
                  "owner": { "steamId64": "76561198000000001" },
                  "timeUpdated": 1234
                },
                {
                  "publishedFileId": "333",
                  "title": "Already Included Dependency",
                  "owner": { "steamId64": "76561198000000003" },
                  "timeUpdated": 999
                }
              ],
              "dependencies": {
                "111": ["222", "333", "222", "2845454582", "nope"]
              },
              "authors": {}
            }
        "#;

        let dependency_ids = ts_steam_helper_dependency_ids_needing_titles(mods_data_json).unwrap();

        assert_eq!(dependency_ids, ["222"]);
    }

    #[test]
    fn rejects_malformed_ts_steam_helper_response() {
        let error = parse_ts_steam_helper_mod_data_response("{", None).unwrap_err();

        assert_eq!(error.kind, SteamWorkshopAdapterErrorKind::MalformedResponse);
        assert!(error.message.contains("getModsData"));
    }

    fn queue_and_take_batch(
        state: &mut SteamWorkshopRequestState<String>,
        ids: impl IntoIterator<Item = &'static str>,
        now_ms: u64,
    ) -> Vec<String> {
        state.queue_workshop_ids(ids, now_ms);
        state.next_batch(now_ms).unwrap().ids
    }

    fn queue_and_take_metadata_batch(
        state: &mut SteamWorkshopRequestState<WorkshopModData>,
        ids: impl IntoIterator<Item = &'static str>,
        now_ms: u64,
    ) -> Vec<String> {
        state.queue_workshop_ids(ids, now_ms);
        state.next_batch(now_ms).unwrap().ids
    }

    fn workshop_mod_data(workshop_id: &str, title: &str) -> WorkshopModData {
        WorkshopModData {
            workshop_id: workshop_id.to_string(),
            title: title.to_string(),
            author: "author".to_string(),
            description: String::new(),
            tags: Vec::new(),
            dependency_ids: Vec::new(),
            dependency_id_to_name: Vec::new(),
            last_changed_ms: 1_000,
        }
    }

    struct RecordingWorkshopAdapter {
        requested_batches: Vec<Vec<String>>,
        result: Result<Vec<WorkshopModData>, SteamWorkshopAdapterError>,
    }

    impl RecordingWorkshopAdapter {
        fn ok(data: Vec<WorkshopModData>) -> Self {
            Self {
                requested_batches: Vec::new(),
                result: Ok(data),
            }
        }

        fn err(error: SteamWorkshopAdapterError) -> Self {
            Self {
                requested_batches: Vec::new(),
                result: Err(error),
            }
        }
    }

    impl SteamWorkshopMetadataAdapter for RecordingWorkshopAdapter {
        fn fetch_mod_data_batch(
            &mut self,
            workshop_ids: &[String],
        ) -> Result<Vec<WorkshopModData>, SteamWorkshopAdapterError> {
            self.requested_batches.push(workshop_ids.to_vec());
            self.result.clone()
        }
    }

    fn test_config() -> SteamWorkshopSafetyConfig {
        SteamWorkshopSafetyConfig {
            batch_size: 2,
            batch_delay: Duration::from_millis(1_000),
            cache_ttl: Duration::from_millis(1_000),
            failure_cooldown: Duration::from_millis(2_000),
            global_cooldown: Duration::from_millis(2_000),
            retry_base_delay: Duration::from_millis(100),
            retry_jitter_max: Duration::from_millis(50),
            max_retries: 3,
            failure_threshold_for_global_cooldown: 2,
        }
    }
}
