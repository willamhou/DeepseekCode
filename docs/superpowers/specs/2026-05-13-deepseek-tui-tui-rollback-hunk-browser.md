# DeepSeek-TUI TUI Rollback Hunk Browser

## Context

DeepSeekCode's TUI rollback panel can show snapshot metadata, bounded patch
previews, dry-run restore plans, applied changed files, and an explicit
confirmation modal before apply restores. The remaining Phase F TUI rollback UX
gap is finer-grained diff inspection before deciding whether to restore.

## Scope

- Add TUI command-palette commands:
  - `restore hunks <id|last>` lists parsed patch hunks for a rollback snapshot
    or runtime-turn-bound snapshot.
  - `restore diff <id|last>` is an alias for the hunk list.
  - `restore hunk <id|last> [index]` shows a single 1-based hunk in the
    existing right-side rollback detail panel.
- Parse unified diff hunk headers from stored snapshot patches.
- Keep full restore/apply behavior unchanged; this slice is an inspection-only
  feature.

## Non-Goals

- Selective hunk restore/apply, covered later by
  `2026-05-13-deepseek-tui-tui-rollback-hunk-restore.md`.
- Editing patch content.
- Remote HTTP-runtime rollback support.

## Verification

- `cargo test command_palette_requests_rollback_hunk_browser --lib`
- `cargo test handle_tui_action_lists_shows_hunks_and_restores_rollback_snapshot --lib`
- `cargo test rollback --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `git diff --check`

## Follow-Up

- Selective hunk restore is covered by
  `2026-05-13-deepseek-tui-tui-rollback-hunk-restore.md`.
