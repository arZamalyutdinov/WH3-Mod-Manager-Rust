# WH3 Mod Manager

Native Rust mod manager for Total War: Warhammer III.

The current TypeScript/Electron app remains the parity reference. In local
development this repo is usually checked out beside the TS project as
`../WH3-Mod-Manager`.

## Workspace

- `crates/wh3mm-core`: UI-agnostic domain core, parser/schema logic, app state, ports.
- `crates/wh3mm-ui`: toolkit-neutral intents and view models. Keep this usable by Dioxus and Slint.
- `apps/wh3mm-dioxus`: desktop application. It is intentionally excluded from the default workspace so core checks stay dependency-light.
- `schema/`: Creative Assembly schema assets copied from the TS project for parser/schema work.
- `steamworks/dist/win64/`: Windows Steamworks DLL/import library layout mirrored from the TS app for native helper builds.
- `installer/windows/`: NSIS installer script for Windows releases.
- local agent notes may exist under `skills/`, but Markdown planning docs are
  intentionally not part of the normal commit set.

## Current State

Implemented so far:

- WH3 `PFH5` pack index parsing.
- Dependency-pack and packed-file index parsing.
- DB/loc metadata reading.
- Zstd payload decompression using the WH3 four-byte compressed payload prefix rule.
- Strict and lossy pack-content loading.
- Primitive schema-driven DB row decoding.
- WH3 schema JSON / `.json.zst` loading.
- DB metadata to schema-version resolution.
- Toolkit-neutral pack summary and DB table preview view models.
- Dioxus desktop app with game/mod discovery, persisted mod order/enablement,
  TS config import/export, launch preview/prepare/spawn, start-game pack
  generation, compatibility summaries, Steam helper controls, and a bounded
  GitHub release update check.

The remaining release checks require a Windows machine with Steam, WH3, and a
representative mod library.

## Windows Packaging

The Windows workflow is `.github/workflows/release-windows.yml`.

Manual `workflow_dispatch` builds downloadable GitHub Actions artifacts without
publishing a release by default:

- `WH3-Mod-Manager-Rust-<tag>-win32-x64.zip`
- `WH3-Mod-Manager-Rust-Installer-<tag>.exe`

Enable the `publish_release` input only when the files should also be published
to a GitHub Release. Publishing runs without an explicit tag auto-resolve the
next release tag in the same style as the TypeScript app workflow. Tag pushes
publish using the pushed tag.

Pushes to the configured Windows validation branches (currently
`codex/windows-validation` and `codex/windows-feedback-ui-parity`) also
build artifact-only files and never publish a release.

The staged payload contains:

- `wh3mm-dioxus.exe`
- `helpers/wh3mm-steam-helper.exe`
- `helpers/steam_api64.dll`
- `schema/schema_wh3.json.zst`
- `WINDOWS-VERIFICATION.md`

The Rust repo intentionally copies only `steam_api64.dll` and `steam_api64.lib` from the TS Steamworks folder. The TS `steamworksjs.win32-x64-msvc.node` module is Electron-specific and is not used by the Rust helper.

The workflow runs a packaged-payload smoke check before creating artifacts:

```powershell
.\scripts\windows-release-smoke.ps1 -PayloadDir .\out\windows-payload
```

On Windows, the same script can be pointed at an extracted zip or installed app
directory to verify the app exe, helper exe, schema, verification guide, Steam
runtime DLL placement, and fixture-mode helper protocol. The full manual
checklist is [WINDOWS-VERIFICATION.md](WINDOWS-VERIFICATION.md).

## Validation

Core workspace:

```sh
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace
```

Dioxus shell:

```sh
cargo fmt --manifest-path apps/wh3mm-dioxus/Cargo.toml --check
cargo check --manifest-path apps/wh3mm-dioxus/Cargo.toml
cargo clippy --manifest-path apps/wh3mm-dioxus/Cargo.toml
```

Run the Dioxus shell:

```sh
cargo run --manifest-path apps/wh3mm-dioxus/Cargo.toml
```

Run it against a pack file:

```sh
cargo run --manifest-path apps/wh3mm-dioxus/Cargo.toml -- /path/to/mod.pack
```
