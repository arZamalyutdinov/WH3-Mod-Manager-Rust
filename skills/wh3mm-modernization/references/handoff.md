# WH3MM Modernization Handoff

Last updated: 2026-07-16

Rust repo: the current repository root
TS reference repo: sibling checkout `../WH3-Mod-Manager`, when available

Current feature branch: `codex/deeper-steam-integration`, based on merged main
`905cc30`, with the native Workshop catalog, safe collection subscription,
bounded download monitoring, and automatic archive synchronization implemented
on this branch. The TS tree remains reference-only and was not modified.

## Start

1. Read `progress.md` and `plan.md`.
2. Read only the relevant section of `architecture.md` or `backend-reference.md`.
3. Read `bug-hunting.md` before touching code.
4. Treat the TS repo as reference-only unless the user explicitly asks for
   cross-repo edits.

## End

1. Update `progress.md` current-state sections in place.
2. Update `plan.md` statuses in place.
3. Update `architecture.md` only when the architecture or boundaries changed.
4. Keep `plan.md` under about 160 lines, `progress.md` under 220, and
   `architecture.md` under 260; consolidate capability summaries if exceeded.
5. Keep only one current validation baseline and one latest milestone. Remove
   superseded artifact IDs, counts, recommendations, and per-edit validation.
6. Record residual risk and a short ordered list of next actions in `progress.md`.

## Validation

For meaningful Rust release work, run:

```sh
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace
cargo fmt --manifest-path apps/wh3mm-dioxus/Cargo.toml --check
cargo check --manifest-path apps/wh3mm-dioxus/Cargo.toml
cargo test --manifest-path apps/wh3mm-dioxus/Cargo.toml -- --nocapture
cargo clippy --manifest-path apps/wh3mm-dioxus/Cargo.toml
```
