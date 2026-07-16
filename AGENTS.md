## Local Skills

### Available skills

- `wh3mm-modernization`: Maintain architecture, roadmap, and progress state for WH3 Mod Manager Rust migration work, including parity with the TypeScript/Electron project. (file: `skills/wh3mm-modernization/SKILL.md`)

## Repository Roles

- Rust rewrite root: the current repository root
- TypeScript/Electron reference root: sibling checkout `../WH3-Mod-Manager`, when available

Treat the TypeScript project as the parity/reference implementation. Do not edit the TypeScript project from this Rust repo unless the user explicitly asks for cross-repo changes.

## Trigger Rules

Use `wh3mm-modernization` when a request involves:
- Rust migration decisions or implementation
- architecture review of the mod manager
- pack/schema/parser parity
- performance or jitter analysis
- optimization planning and execution tracking
- bug hunting and stabilization work
- updating long-running handoff docs for future agents

## Canonical Modernization Docs

- `skills/wh3mm-modernization/references/architecture.md`
- `skills/wh3mm-modernization/references/plan.md`
- `skills/wh3mm-modernization/references/progress.md`
- `skills/wh3mm-modernization/references/handoff.md`
- `skills/wh3mm-modernization/references/bug-hunting.md`
- `skills/wh3mm-modernization/references/backend-reference.md`

## Maintenance Rules

1. Read `progress.md` and `plan.md` before major modernization work.
2. Update `plan.md` and `progress.md` in place after major modernization work; keep them compact and current instead of append-only.
3. Treat Git as the detailed history. Do not append per-commit UI notes or repeated validation results to canonical docs.
4. Keep soft limits of 160 lines for `plan.md`, 220 for `progress.md`, and 260 for `architecture.md`; consolidate in the same session when exceeded.
5. Keep only the latest milestone, validation baseline, artifact evidence, risks, and next actions; replace stale IDs, counts, dates, and recommendations.
6. Update `architecture.md` when architectural understanding changes.
7. Keep `wh3mm-core` free of Dioxus, Slint, WebView, and platform-shell dependencies.
8. Keep `wh3mm-ui` toolkit-neutral; Dioxus and future Slint shells should render shared view models.
9. Run the bug-hunting checklist from `bug-hunting.md` on touched code and fix confirmed bugs.
10. Treat Steam/workshop request-volume safety as high-priority stabilization work.
11. Validate the Rust workspace with:
   - `cargo fmt --all --check`
   - `cargo test --workspace`
   - `cargo clippy --workspace`
12. Validate the standalone Dioxus shell separately with:
   - `cargo fmt --manifest-path apps/wh3mm-dioxus/Cargo.toml --check`
   - `cargo check --manifest-path apps/wh3mm-dioxus/Cargo.toml`
   - `cargo clippy --manifest-path apps/wh3mm-dioxus/Cargo.toml`
