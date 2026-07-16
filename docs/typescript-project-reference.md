# TypeScript Project Reference

The current WH3 Mod Manager TypeScript/Electron project should be available as
a sibling checkout at:

`../WH3-Mod-Manager`

Use it as the behavioral reference for the Rust rewrite. Do not modify it from this repository unless explicitly requested.

## High-Value Reference Files

- `src/index.ts`: Electron app lifecycle, windows, timers, update flow.
- `src/ipcMainListeners.ts`: main backend orchestration, IPC handlers, watchers, pack operations, Steam actions.
- `src/appData.ts`: central mutable backend state.
- `src/packFileSerializer.ts`: pack parser/writer/merge engine and DB/loc handling.
- `src/packFileHandler.ts`: lightweight pack header handling.
- `src/schema.ts`: schema cache and compressed schema loading.
- `src/resolveTable.ts`: table/schema resolution helpers.
- `src/DBClone.ts`: DB reference traversal and writeback behavior.
- `src/modFunctions.ts`: mod discovery and workshop metadata flow.
- `src/sub.ts`: Steam helper child process.
- `src/preload.ts`: renderer-facing IPC API.

## Rust Mapping

- `wh3mm-core` should replace pack/schema/app-state domain behavior first.
- `wh3mm-ui` should hold presentation models that can be rendered by Dioxus now and Slint later.
- `apps/wh3mm-dioxus` is an adapter shell, not a domain owner.
- Steam/workshop behavior should preserve request safety: dedupe, bounded concurrency, backoff, stop conditions, and caching.

## Fixture Needs

Real pack fixtures needed for parity:

- small/simple uncompressed `.pack`
- compressed DB-heavy `.pack`
- `.loc` pack
- dependency/movie-byte-mask pack
- one weird-but-valid pack known to work in current WH3MM

Keep large/private fixture files out of Git unless the user explicitly approves committing them.
