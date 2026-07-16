---
name: wh3mm-modernization
description: Maintain and execute the WH3 Mod Manager modernization workflow. Use when assessing architecture or performance bottlenecks, planning optimization work, evaluating Rust migration options, or updating architecture/plan/progress handoff docs for future agents.
---

# Wh3mm Modernization

## Overview

Maintain a compact shared technical baseline for performance and modernization
work in this repository. Keep architecture, roadmap, and progress documents
current enough for continuation without turning them into append-only history.

## Canonical References

- `references/architecture.md`: Current system architecture, bottlenecks, and migration options.
- `references/plan.md`: Active execution plan with phase/task status.
- `references/progress.md`: Compact current snapshot, recent change notes, and validation baseline.
- `references/handoff.md`: Session-to-session checklist and update protocol.
- `references/bug-hunting.md`: Mandatory bug-finding and bug-fixing directives, including Steam request safety.

## Workflow

1. Read `references/handoff.md`.
2. Read `references/progress.md` to understand the latest state.
3. Read `references/plan.md` and pick the highest-priority pending task.
4. Read code needed for that task and update `references/architecture.md` if understanding changed.
5. Read and apply `references/bug-hunting.md` before code changes.
6. Execute changes, including bug fixes discovered along the way.
7. Update `references/plan.md` task status in place.
8. Update `references/progress.md` in place with:
   - current state changes
   - validation evidence
   - confirmed bug findings/fixes
   - open risks/blockers
   - next recommended action

## Update Rules

- Keep docs compact. Prefer replacing stale summaries over appending history.
- Treat Git history as the detailed change log. Do not preserve a diary of
  commits, UI micro-iterations, or repeated validation passes in references.
- Keep soft size limits of 160 lines for `plan.md`, 220 for `progress.md`, and
  260 for `architecture.md`. If a file exceeds its limit, consolidate it in the
  same session unless the extra material is essential current reference data.
- `plan.md` should contain capability-level status and pending work only. Fold
  completed substeps into a small number of `done` summaries.
- `progress.md` should contain the current snapshot, one latest milestone,
  current validation evidence, current risks, and next actions. Replace
  superseded dates, counts, artifact IDs, and recommendations.
- Record validation once per meaningful milestone. Never append one validation
  bullet for each small follow-up edit.
- Keep `references/plan.md` status aligned with actual code state, not intent.
- Keep architecture notes specific, current, and file-referenced where useful.
- If work is blocked, record the blocker and exact missing input in `references/progress.md`.
- Add dated notes only when they preserve information not obvious from code,
  tests, or the current snapshot.
- Before handoff, check line counts and search for stale branch, commit,
  artifact, date, and test-count references in the canonical docs.
- Treat bug investigation as part of each task, not a separate optional phase.
- If Steam/API rate limiting or abuse patterns are detected, prioritize safety fixes before feature work.

## Scope

Use this skill for:
- performance/jitter diagnosis
- startup/render/path hot-spot analysis
- Electron optimization work
- Rust sidecar/core feasibility and migration planning
- maintaining long-running engineering context across sessions

Do not use this skill for:
- unrelated gameplay/mod content tasks
- translation/content-writing-only tasks
- one-off UI copy tweaks with no architecture impact

## Example Triggers

- "Analyze why the mod manager feels jittery."
- "Should we rewrite this in Rust?"
- "Update the roadmap and continue the optimization effort."
- "What should the next agent work on?"

## Deliverable Pattern

When this skill is used for major modernization work, leave these artifacts
updated:
- `references/architecture.md`
- `references/plan.md`
- `references/progress.md`
- `references/bug-hunting.md` only when directives need refinement

If one is unchanged after meaningful work, state briefly why.
