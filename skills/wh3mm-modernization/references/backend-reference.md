# WH3MM Backend Reference Map

Last updated: 2026-07-16

Use this as a compact guide to the TS parity sources. Open the TS files only
when working on the matching Rust behavior.

TS reference root: sibling checkout `../WH3-Mod-Manager`, when available

## High-Value TS Files

- `src/ipcMainListeners.ts`
  - game launch path
  - mod-list file generation
  - generated packs
  - Steam helper call sites
  - pack overwrite and flow execution integration
- `src/packFileSerializer.ts`
  - pack serialization/parsing behavior
  - `executeFlowsForPack`
  - start-game pack behavior
  - copy/write pack helpers
- `src/sub.ts`
  - Steam helper protocol commands
  - TS command pacing constants
  - getSubscribedIds, getModsData, getDependencies, getAuthors
  - sub/download/unsubscribe/checkState
- `src/modFunctions.ts`
  - workshop metadata retrieval and dependency metadata behavior
- `src/appConfigFunctions.ts`, `src/appSlice.ts`, `src/rendererConfigSync.ts`
  - config persistence and userFlowOptions state shape
- `src/components/NodeEditor.tsx`
  - serialized flow graph and flow option types
- `src/nodeExecutor.ts`
  - TS flow graph execution engine
- `src/index.d.ts`
  - shared TS type definitions

## Rust Counterparts

- Pack/schema: `crates/wh3mm-core/src/pack.rs`, `db.rs`, `schema.rs`
- Discovery: `crates/wh3mm-core/src/discovery.rs`
- Persistence: `crates/wh3mm-core/src/persistence.rs`
- TS config bridge: `crates/wh3mm-core/src/ts_config.rs`
- Compatibility: `crates/wh3mm-core/src/compat.rs`
- Launch planning: `crates/wh3mm-core/src/launcher.rs`
- Runtime launch: `crates/wh3mm-runtime/src/lib.rs`
- Steam safety/core: `crates/wh3mm-core/src/steam.rs`
- Steam helper app: `apps/wh3mm-steam-helper/src/lib.rs`
- Dioxus shell: `apps/wh3mm-dioxus/src/main.rs`
- Flow summaries: `crates/wh3mm-core/src/flows.rs`
- Pack overwrites: `crates/wh3mm-core/src/overwrites.rs`
- Start-game packs: `crates/wh3mm-core/src/start_game.rs`

## TS Behaviors Already Mirrored

- mod sorting/load-order import semantics for TS config
- `used_mods.txt` plus `my_mods.txt` fallback
- repeated working-directory launch lines
- selected mod order preservation
- data/modding copy behavior with overwrite guard
- merged-pack source exclusion
- start-game options for skip intros, script logging, auto battle
- DB-backed MakeUnitsGenerals generation
- TS `packDataOverwrites` import/export and replacement-pack generation
- raw `userFlowOptions` preservation
- Steam command pacing at 250ms through helper adapter
- Steam metadata safety guardrails: dedupe, batching, retry/backoff, cooldown,
  cache

## TS Behaviors Not Fully Mirrored

- `whmmflows` node graph execution
- full NodeEditor/nodeExecutor runtime
- live Steam download progress callbacks
- TS-level XML/Lua syntax checks
- Steam upload/update flows
- full updater behavior

## Flow Execution Notes

TS reference:

- `packFileSerializer.ts::executeFlowsForPack`
- `NodeEditor.tsx::SerializedNodeGraph`
- `NodeEditor.tsx::FlowOption`
- `nodeExecutor.ts`

Current Rust only detects and summarizes flow files. If porting execution:

1. Parse typed graph/node/connection structures.
2. Apply user option placeholders.
3. Implement `useCurrentPack`.
4. Inject schema metadata where TS does.
5. Execute a narrow node subset or port the executor.
6. Collect generated packs from save nodes and append them to launch.

## Steam Helper Notes

TS helper uses command strings:

- `getSubscribedIds`
- `getModsData`
- `getItems`
- `getDependencies`
- `getAuthors`
- `sub`
- `download`
- `unsubscribe`
- `checkState`

Rust helper keeps this shape where useful and adds structured JSON results for
command details. Keep any new polling bounded and explicitly documented because
Steam request flooding was a known risk in the TS app.
