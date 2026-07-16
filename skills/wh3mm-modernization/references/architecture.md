# WH3MM Rust Architecture

Last updated: 2026-07-16

Compact current architecture only. Update in place.

## Repository Boundary

- Rust rewrite root: the current repository root
- TS/Electron reference root: sibling checkout `../WH3-Mod-Manager`, when available
- The TS repo is parity reference only unless the user explicitly asks for
  cross-repo edits.

## Crates And Apps

- `crates/wh3mm-core`
  - domain owner
  - no Dioxus, Slint, WebView, process-spawn, shell, or platform UI deps
  - owns pack/schema parsing, persistence models, launch planning, compatibility
    analysis, Steam safety planning, TS config bridge, start-game packs,
    overwrite packs, and flow summaries
- `crates/wh3mm-ui`
  - toolkit-neutral view models and presenters
  - owns archive row enrichment plus presentation-only sort specifications and
    stable sorting; sorting never mutates core launch order
  - Dioxus and any future Slint shell should render this layer where practical
- `crates/wh3mm-runtime`
  - filesystem, process, Windows game-folder validation, launch preparation,
    Steam helper process runner, Steam command adapters
- `apps/wh3mm-dioxus`
  - first desktop shell
  - owns file pickers, app config paths, UI state, and command wiring
  - archive, detail-thumbnail, and responsive rail pieces live in bounded
    `components` modules with shared CSS
  - serves discovered local thumbnails through a tokenized allowlist handler;
    arbitrary filesystem paths are never accepted from requests
- `apps/wh3mm-steam-helper`
  - CLI helper protocol
  - fixture backend for tests
  - Windows native Steamworks backend for live helper behavior

## Current Data Flow

1. Dioxus discovers/selects a WH3 folder or mod folder.
2. `wh3mm-core::discover_mods` inventories packs, parses the exact
   case-insensitive CA manifest set, discovers thumbnails/mtimes, and enriches
   data-shadowed Workshop records without replacing their data paths.
3. Core persistence reapplies enablement/order/categories/hidden/locks.
4. `wh3mm-ui` projects rows and pack previews.
5. Launch actions build `WindowsLaunchOptions`.
6. `wh3mm-core::plan_windows_launch` builds the launch plan.
7. `wh3mm-runtime` writes mod-list/generated packs, copies `data/modding`
   packs, and spawns WH3.

## Persistence

Dioxus stores application config under the platform app config dir, overridable by
`WH3MM_CONFIG_DIR`, with legacy current-directory fallback:

- `wh3mm_game_folder.json`
- `wh3mm_mod_state.json`
- `wh3mm_mod_user_config.json`
- `wh3mm_presets.json`
- `wh3mm_steam_helper.json`
- `wh3mm_workshop_metadata.json`
- `wh3mm_ui_preferences.json`
- `diagnostics/wh3mm-dioxus.log`
- `diagnostics/wh3mm-crash.log`
- `diagnostics/wh3mm-steam-helper-commands.jsonl`
- `diagnostics/wh3mm-diagnostic-<unix_ms>.txt`

Core persistence uses atomic temp-file rename and refuses unsafe empty
overwrites for key state files.

## TS Config Bridge

`crates/wh3mm-core/src/ts_config.rs` imports/exports release-critical TS
`config.json` fields:

- active mod order/enablement
- presets
- categories/colors, hidden mods, always-enabled mods
- selected WH3 folder
- start-game options already ported
- `packDataOverwrites`
- raw `userFlowOptions`
- merged-mod source paths
- close-on-play and high-priority launch lifecycle flags

Renderer/runtime-only TS fields are intentionally ignored.

## Pack And Compatibility

Core supports:

- PFH5 index parsing
- compressed payload reading
- DB/loc metadata
- primitive DB rows read/write
- WH3 schema resolution from DB metadata
- lossy and strict pack reads
- real Steam pack fixture tests

Compatibility analysis currently covers:

- file collisions
- missing dependency packs
- DB key collisions
- missing DB references with TS exception lists
- unique ID collisions
- Lua listener collisions
- XML-like file references
- table/script/file-reference decode errors

The Dioxus shell runs full compatibility analysis on one guarded background
worker. It snapshots the enabled ordered set before starting, keeps the UI
responsive, and discards a completed report if that input changes meanwhile.

Application updates use one bounded background request to GitHub's latest
stable-release endpoint at startup. Settings can force another check; only one
check runs at a time, there is no polling/retry loop, and an available-update
button opens a fixed repository release URL instead of downloading or executing
an asset.

Remaining parity gap: fuller TS-level XML/Lua syntax checks.

## Launch Architecture

`wh3mm-core::launcher` plans only. It does not write files or spawn processes.
Runtime mutations live in `wh3mm-runtime`.

Implemented:

- TS-style `used_mods.txt` content and `my_mods.txt` fallback in runtime
- repeated `add_working_directory` lines
- Windows-style path comparison on non-Windows dev hosts
- `data/modding` copy planning and newer-target guard
- generated pack groups after normal enabled mods
- replaced-source filtering for overwrite packs
- merged-pack source exclusion
- save-name campaign-load args
- start-game temp packs
- DB-backed MakeUnitsGenerals generation
- direct process spawn
- best-effort Windows high-priority process update
- Dioxus close-on-play scheduling

Remaining: live Windows WH3 spawn validation and real-mod smoke coverage.

## Steam Architecture

Core owns request safety. Runtime/helper own process/native boundaries.

Implemented:

- dedupe and normalization of workshop IDs
- batch planning, cache TTL, failure cooldown, retry/backoff planning
- `SteamWorkshopMetadataAdapter`
- TS-helper-shaped metadata parser
- persistent Workshop title/author/description/tag/update cache with a 24-hour
  UI refresh policy and immediate stale-cache presentation
- runtime helper process runner with timeout and env overrides
- command adapter with TS-like 250ms command delay
- command actions split into at most 40 IDs, pause one second between batches,
  and reject one bulk operation above 200 IDs
- fixture helper backend
- Windows native Steamworks helper backend
- toolkit-neutral catalog/detail/state/monitor models covering Discover plus
  Subscribed, Favorites, Published, Voted Up, Voted Down, and Followed lists
- `queryWorkshop` returns one cached 50-item page with scope-compatible sort,
  search/tag filtering, preview, author, statistics, collection children, and
  local state; Dioxus issues it only for explicit apply/sort/page actions and
  rejects responses whose normalized-query fingerprint is stale
- `monitorWorkshopItems` keeps one helper/Steamworks client alive and emits
  bounded JSONL snapshots at one-second intervals for at most ten minutes;
  runtime owns cancellation and child cleanup, while Dioxus permits one monitor
  and replaces it with the unfinished/new-ID union
- responsive Workshop browser cards/details show safe HTTP(S) previews and
  text-only Steam content; Community URLs are built only from numeric IDs
- collection child details page by 50; missing-item subscription is confirmed
  and capped at 200 before normal command batching
- successful installation rediscovers data/Workshop folders and reapplies
  persisted user state; unsubscribe never deletes local folders
- Dioxus helper path/backend persistence
- Dioxus helper commands run on bounded background threads with a single
  in-flight UI operation guard
- automatic refresh requests only missing/stale IDs once per process request
  fingerprint; manual refresh forces the current archive through the same
  dedupe/batching/delay/retry/cooldown path
- helper probe, refresh, check update, subscribe, download, unsubscribe,
  resubscribe actions
- app-provided `WH3MM_STEAM_HELPER_COMMAND_LOG` path for helper command JSONL
  diagnostics
- guarded local workshop directory cleanup
- bounded resubscribe verification
- readiness panel for local helper/DLL/schema/WH3/workshop paths
- last-command detail panel for requested/confirmed/update IDs and
  resubscribe verification

Remaining: live Windows validation of native catalog scopes, collection
subscription, monitored progress/cancellation, and post-download rediscovery.

## User Flows

Implemented:

- raw TS `userFlowOptions` round-trip preservation
- detection of `whmmflows\...` pack files
- lossy JSON summary of flow files, graph toggles, counts, options, and parse
  errors
- Dioxus pack viewer summary

Not implemented:

- TS node graph execution
- option placeholder substitution
- `useCurrentPack` injection
- schema injection into flow nodes
- generated flow packs from save nodes

## Windows Packaging

Workflow:

- `.github/workflows/release-windows.yml`
- stages `out/windows-payload`
- manual dispatch defaults to artifact-only validation; release publishing is
  explicit
- pushes to `codex/windows-validation` and
  `codex/windows-feedback-ui-parity` build artifact-only validation files
  without publishing a release
- copies Dioxus app, Steam helper, the WH3 compressed schema,
  `steam_api64.dll`, and the Windows verification guide
- embeds the TS `modmanager.ico` into the GUI executable, uses the same bytes
  in the Dioxus header/window, and brands NSIS plus installed shortcuts
- runs `scripts/windows-release-smoke.ps1`, including embedded-icon comparison
- builds portable zip
- builds NSIS installer from `installer/windows/wh3mm-rust-installer.nsi`

Steamworks files:

- `steamworks/dist/win64/steam_api64.dll`
- `steamworks/dist/win64/steam_api64.lib`

These are copied from the TS project layout and are sufficient for current
Windows packaging. Do not use the TS `.node` module in Rust.

Packaged schema/help lookup prefers the executable directory, so installed and
portable builds do not depend on their inherited working directory.

## Validation Boundary

Local macOS/Linux tests prove parser/runtime/unit behavior, but not release
readiness. The 1.0 release requires Windows evidence:

- artifact install/unzip
- readiness panel
- native helper probe/refresh/commands
- launch preview/prepare
- actual WH3 process start with selected mod order
