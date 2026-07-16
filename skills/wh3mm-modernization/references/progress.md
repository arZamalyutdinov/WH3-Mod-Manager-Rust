# WH3MM Modernization Progress

Last updated: 2026-07-11

This is the current-state handoff. Git is the history; this file keeps only the
latest implementation baseline, evidence, risks, and next actions.

## Current Focus

Windows 1.0 release preparation. Implementation is broad enough for a real
acceptance pass; the remaining release gates require a Windows machine with
Steam, WH3, and a representative mod library.

Do not publish 1.0 until the installed or portable artifact proves discovery,
Steam enrichment, responsive layout, persistence, and WH3 launch.

## Current Implementation Snapshot

### Architecture

- `wh3mm-core` owns UI-free domain, parser/schema, discovery, persistence,
  launch planning, compatibility, Steam safety, TS config, and generated packs.
- `wh3mm-ui` owns toolkit-neutral enriched archive rows and presentation-only
  sorting; it never mutates preset or launch order.
- `wh3mm-runtime` owns filesystem, process, Windows registry/install discovery,
  launch preparation, and Steam helper adapters.
- Dioxus owns desktop state/wiring and bounded archive/detail/rail components;
  the helper app owns fixture/native Steamworks protocol execution.

### Discovery, Metadata, And Archive

- Discovers packs from game `data`, `data/modding`, Workshop content, and extra
  local folders.
- Filters only exact case-insensitive entries from `data/manifest.txt`, using
  the TS `gameToManifest.wh3` list when the manifest is unavailable.
- Records source, local mtime, and TS-style thumbnail precedence. A data pack
  shadowing a same-named Workshop pack keeps its data path and inherits the
  Workshop association/thumbnail.
- Persists Workshop title, author, description, tags, and update time in
  `wh3mm_workshop_metadata.json`.
- Displays cached metadata immediately. Automatic refresh requests only
  missing/stale entries older than 24 hours and is request-fingerprint gated;
  manual refresh forces the current set through the same safety path.
- Archive rows show order, status, thumbnail, title/pack, author, and updated
  time. Sorting is stable, missing metadata stays last, and preferences persist
  in `wh3mm_ui_preferences.json`.
- Local thumbnails are served only through tokenized allowlisting with PNG/JPEG
  MIME checks. Rails collapse into drawers below 1280px and 960px.

### Persistence, Launch, And Compatibility

- Persists mod enablement/order, presets, categories, hidden/locked state,
  selected paths, and helper configuration with atomic writes where required.
- Imports/exports release-critical TS state including merged sources, start-game
  options, pack-data overwrites, close-on-play, priority, and raw flow options.
- Plans TS-style `used_mods.txt` with `my_mods.txt` fallback, preserves order,
  handles data/modding copies and generated pack groups, and spawns WH3.
- Supports overwrite packs and implemented start-game options, including
  schema-backed MakeUnitsGenerals generation.
- Compatibility covers pack/file/DB-key/reference/unique-ID/Lua-listener and
  XML-like reference collisions with lossy read diagnostics.
- Full compatibility analysis runs on one guarded background worker. Results
  use an enabled-order snapshot and are discarded if the input changes.

### Steam Safety And Diagnostics

- Steam requests normalize/deduplicate IDs, batch at bounded size, delay
  batches, retry with backoff/jitter, and apply per-ID/global cooldowns.
- The browser exposes explicit-submit Discover search/tag/sort/paging and six
  read-only user lists. Normalized query fingerprints reject stale responses;
  native queries use Steam's 60-second cache and return one 50-item page.
- Cards/details expose safe previews, text-only descriptions, author, dates,
  tags, statistics, dependencies, collections, and local install state.
  Collection children load lazily; confirmed missing-item subscriptions are
  capped at 200 and command batches at 40 with the existing per-ID delay plus a
  one-second inter-batch pause.
- One cancellable helper process streams local state/byte progress once per
  second for at most ten minutes. A new download replaces monitoring with the
  unfinished/new-ID union; successful installation automatically rediscovers
  archive folders and reapplies persisted user state.
- Dioxus runs helper work off the UI thread and permits one active Steam
  operation. Automatic refresh does not retry repeatedly during rendering.
- Native and fixture helper backends cover metadata, dependencies/authors,
  subscribe/download/unsubscribe/check-state, and guarded resubscribe flows.
- Diagnostics include startup/app status, panic/crash, helper command JSONL,
  readiness, last command details, and one-click snapshots/folder access. The
  app log is created during startup instead of waiting for the first event.
- A bounded background GitHub check runs once at startup. Settings exposes a
  guarded manual check, and newer stable versions surface a fixed-repository
  release-page button; there is no polling, automatic download, or installer
  execution.

### Windows Packaging

- `.github/workflows/release-windows.yml` builds the helper and Dioxus shell,
  stages the five-file payload, runs the smoke script, and creates portable ZIP
  and NSIS installer artifacts.
- Packaged payload: GUI app, console helper, `steam_api64.dll`, compressed WH3
  schema, and `WINDOWS-VERIFICATION.md`.
- The exact TS `modmanager.ico` is embedded into the GUI executable, reused by
  the native window and app header, and supplied to NSIS/shortcuts/uninstall
  metadata. Windows smoke compares the embedded 32px icon to the source asset.
- Validation-branch and manual non-publishing runs remain artifact-only.

## Latest Milestone

2026-07-11 deeper native Steam Workshop integration:

- Added UI-neutral catalog, details/statistics, collection, local-state, byte
  progress, and monitor completion models with validation and fingerprints.
- Extended fixture/native helpers with paged catalog/user-list queries, cached
  author resolution and enriched items, plus bounded JSONL monitoring.
- Added runtime catalog and streaming-monitor adapters, strict 40-ID action
  batches, one-second batch spacing, cancellation/child cleanup, and a 200-ID
  bulk ceiling.
- Replaced the raw-command-first page with an explicit-query responsive browser,
  detail actions, collection confirmation/paging, progress UI, monitoring
  cancellation/replacement, and automatic state-preserving archive refresh.
- Bug audit confirmed and fixed command logging that rewrote the entire log on
  every command, stale catalog-response replacement, collection subscription
  of already-subscribed children, unbounded action batches, and monitor output
  failure handling. Rendering/sorting does not trigger catalog requests, monitor
  replacement retains unfinished IDs without allowing an old worker to clear
  new state, samples are not logged, and previews/Community links are validated.

## Current Validation Evidence

Local mandatory suite passed without warnings on 2026-07-11:

- 124 `wh3mm-core` unit tests
- 2 pack/schema and 4 real-pack integration tests
- 43 `wh3mm-runtime` tests
- 18 `wh3mm-steam-helper` tests
- 8 `wh3mm-ui` tests
- 73 Dioxus app-side tests
- workspace and standalone Dioxus format/check/clippy commands
- Windows helper cross-check for `x86_64-pc-windows-msvc`

Windows Actions run 12 passed on commit `31e6e96`:

- native x64 helper and GUI app release builds
- staged payload smoke, portable ZIP, NSIS installer, artifact upload
- artifact `wh3mm-rust-windows-validation-12-31e6e96`
- artifact SHA-256
  `06bfe2030bed77f6b290113037526111754b6ef25f85e6746dd0954c87fce0d5`
- downloaded outer/nested ZIP integrity and five-file payload verified
- PE inspection: x64 GUI app, x64 console helper, and x64 Steam DLL

Run 12 predates the local compatibility-worker and icon-parity fixes. Per user
request, no new Windows artifact was published for this follow-up.

## Current Gaps And Risks

Release gates:

- No live Windows visual pass at the required viewport sizes.
- No live native helper/catalog/monitor request-volume evidence from a real
  Windows library.
- No current real-install WH3 launch proof for this artifact.
- MakeUnitsGenerals and overwrite generation still need representative mods.
- Installed/portable diagnostic usability remains unproven in a failure case.
- Compatibility analysis is now non-blocking but has no cancellation once its
  worker starts; large real libraries still need completion-time profiling.

Known parity gaps:

- `whmmflows` node execution and generated flow packs are not ported.
- Fuller TS XML/Lua syntax checks are not ported.
- Linux/macOS runtime and packaging remain secondary.

## Next Recommended Work

1. On Windows, exercise all catalog scopes/sorts, collection paging and safe
   subscription, monitor cancellation/replacement/timeout, and automatic
   archive restoration while inspecting bounded helper logs.
2. Verify the Workshop/archive responsive layouts at all target sizes and
   capture screenshots plus Steam command logs.
3. Restart before refresh to prove immediate cached metadata, then compare
   automatic and forced manual request volume.
4. Preview/prepare and launch WH3; verify mod-list and in-game order.
5. Collect diagnostics, record confirmed issues, and update this file by
   replacing the relevant current-state lines rather than appending a diary.
