# WH3MM Bug Hunting Directive

Last updated: 2026-02-21

This guide is mandatory for modernization sessions.

## Core Rule

Every task must include:
1. Targeted bug investigation in touched areas.
2. Fixes for confirmed bugs found during that investigation.
3. Short evidence notes in `progress.md`.

Do not defer obvious, localized bug fixes just because the primary task is performance.

## Investigation Checklist (minimum)

For each touched area, check:
1. Render lifecycle misuse (state updates in render, missing effect deps, repeated listeners).
2. Async/race issues (re-entrant handlers, stale state checks, uncontrolled retries).
3. IPC/event storms (duplicate sends, noisy loops, unnecessary payload size).
4. Data correctness (dedupe assumptions, invalid-path handling, error branches).
5. Resource hygiene (intervals/timeouts/watchers/processes not torn down).
6. UI integrity (overlapping panels/menus/tooltips, incorrect z-index stacking, non-responsive clipping at common desktop sizes).

## Steam/Workshop Safety Directive (high priority)

Background: users reported Steam workshop blocking by IP in past usage.

When touching Steam-related paths, enforce these rules:
1. Deduplicate IDs before requests.
2. Apply explicit request throttling:
   - bounded concurrency
   - minimum delay between batches
3. Add retry policy with exponential backoff and jitter.
4. Stop aggressive retries on likely rate-limit/abuse signals (forbidden/too many requests or repeated failures).
5. Cache recent successful responses when practical to avoid immediate repeat calls.
6. Gate polling loops so they do not amplify request volume during unstable periods.
7. Prefer bounded queues over uncoordinated parallel forks/spawns.

## Steam Paths to Audit First

- `src/modFunctions.ts` (`fetchModData` and related workshop retrieval flow)
- `src/sub.ts` (Steam helper commands)
- `src/ipcMainListeners.ts` (call sites that trigger workshop helper actions)
- `src/index.ts` (periodic/polling triggers)

## Definition of Done for a Bug Fix

1. Reproduction conditions documented (even brief).
2. Root cause identified in code.
3. Fix implemented with minimal regression risk.
4. Basic validation run (manual or automated).
5. `progress.md` updated with:
   - bug summary
   - touched files
   - validation notes
   - residual risk if any
