# WH3MM Modernization Plan

Last updated: 2026-07-11

This is the active roadmap, not a change log. Keep completed work summarized by
capability and keep pending evidence easy to find.

## Status Legend

- `todo`: not started or not proven
- `doing`: active or partly proven
- `done`: implemented and validated for the stated scope
- `blocked`: no safe progress without external input

## Current Priority

Finish the Windows 1.0 release with live evidence from a current artifact.
Linux and macOS remain secondary until the Windows workflows are proven against
a real WH3 installation and mod library.

## Windows 1.0

### Implemented

- `done` Workspace boundaries:
  - UI-free `wh3mm-core`
  - toolkit-neutral `wh3mm-ui`
  - process/filesystem/Steam adapters in `wh3mm-runtime`
  - Dioxus desktop shell and native/fixture Steam helper apps
- `done` Pack and schema foundation:
  - PFH5 index/payload parsing, zstd decompression, DB/loc metadata
  - schema resolution, primitive DB row read/write, and real-pack coverage
- `done` Mod discovery and persistence:
  - game `data`, `data/modding`, Workshop, and extra-folder discovery
  - exact case-insensitive CA manifest filtering with TS WH3 fallback
  - explicit source, mtime, thumbnails, duplicate suppression, and
    data-shadowed Workshop metadata inheritance
  - enablement/order, presets, categories, hidden/locked state, and TS config
    import/export for release-critical fields
- `done` Mod archive and settings UI:
  - sparse goal-directed archive with real thumbnails, authors, and timestamps
  - presentation-only sorting for order/status/name/author/updated with saved
    preferences and unchanged launch order
  - search/filter recovery states, detail navigation, category/state/order
    actions, compact toast feedback, and contextual/overflow tools
  - allowlisted local thumbnail serving, inline SVG icons, bounded components,
    and responsive library/tools drawers
  - guarded background compatibility analysis with visible operation state and
    stale-input rejection instead of UI-thread pack parsing
- `done` Steam and Workshop safety:
  - normalized/deduplicated IDs, bounded batches, delays, retry/backoff,
    cooldowns, and a single-operation UI guard
  - fixture and native Windows helper backends plus probe/refresh/command flows
  - persisted Workshop title/author/description/tag/update cache
  - immediate cached display and missing/stale-only 24-hour background refresh
  - explicit-submit native catalog with Discover and six read-only user lists,
    compatible sorting/tag filtering, paging, responsive details, and stale
    response rejection behind Steam's 60-second query cache
  - confirmation-gated collection subscription of missing children, capped at
    200 IDs and executed in paced batches of at most 40
  - one cancellable JSONL download monitor with byte progress, replacement by
    the unfinished/new-ID union, and automatic state-preserving archive refresh
- `done` Launch/runtime parity for implemented options:
  - TS-style mod-list generation and fallback, stable mod order, generated pack
    slots, merged-source exclusion, and data/modding copy planning
  - overwrite/start-game packs, MakeUnitsGenerals generation, direct spawn,
    close-on-play, and best-effort high priority
- `done` Packaging and diagnostics:
  - Windows workflow, portable ZIP, NSIS installer, staged-payload smoke test
  - packaged schema/helper/Steam DLL/help lookup
  - TS-parity app icon in the Dioxus header/window, Windows executable,
    installer, uninstall metadata, and shortcuts
  - app, crash, Steam-command, readiness, and diagnostic snapshot reporting
  - startup app-log creation so the diagnostics folder exists before failures
  - bounded startup/manual GitHub stable-release check with a fixed-repository
    update link and no automatic download, install, polling, or retry loop
- `done` Current validation artifact:
  - Actions run 12 built and smoked commit `31e6e96`
  - artifact `wh3mm-rust-windows-validation-12-31e6e96` passed downloaded ZIP
    integrity, digest, payload, and PE-shape inspection

### Live Evidence Still Required

- `todo` Inspect the archive at 1920x1080, 1366x768, and minimum window size on
  Windows; verify drawer behavior, no clipping, and no horizontal scrolling.
- `todo` Confirm real CA packs are absent, genuine local packs remain, and
  Workshop images/authors/timestamps load from cache and safe refresh.
- `todo` Inspect Steam command logs after automatic and manual enrichment to
  prove bounded requests and no render-triggered refresh loop.
- `todo` Probe the native helper and exercise safe Workshop commands with Steam
  running on Windows.
- `todo` Exercise catalog scopes, collection subscription, monitored downloads,
  cancellation/replacement, and automatic archive restoration on native Steam.
- `todo` Preview/prepare and launch a real WH3 installation with selected mods;
  verify the resulting mod-list order in game.
- `todo` Smoke MakeUnitsGenerals and imported pack-data overwrites with real
  mods.
- `todo` Collect and assess the first installed/portable diagnostic bundle.

## Remaining Parity Decisions

- `doing` Compatibility analysis covers file/DB/reference/listener collisions;
  fuller TS-level XML/Lua syntax checks remain.
- `doing` Raw `userFlowOptions` and `whmmflows` summaries are supported;
  node-graph execution and generated flow packs remain unported and should be
  prioritized only if Windows release testing proves them blocking.

## Later / Secondary

- `todo` Parser memory/latency benchmark against the TS implementation.
- `todo` Broader real-pack fixture matrix.
- `todo` Slint-versus-Dioxus reevaluation after Windows 1.0 evidence.
- `todo` Linux Steam/Proton runtime support and packaging.
- `todo` Release checksum/signature strategy.

## Definition Of 1.0-Ready

- Current Windows ZIP or installer runs and reports packaged prerequisites.
- App discovers the real WH3 install, mods, and Workshop content correctly.
- Enablement/order survives restart and launches WH3 in matching order.
- Native Steam metadata and safe command flows work without request storms.
- TS config import preserves release-critical mod and launch state.
- Failures produce a useful app/helper diagnostic bundle.
