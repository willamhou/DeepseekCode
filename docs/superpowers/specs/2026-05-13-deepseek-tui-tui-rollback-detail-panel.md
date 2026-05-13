# DeepSeek-TUI TUI Rollback Detail Panel

## Context

DeepSeekCode already exposed rollback commands in the local file-backed TUI, but
the visible result was compressed into the status bar. DeepSeek-TUI-style
workbenches should let a user inspect rollback state without leaving the
terminal UI, especially before applying a destructive restore.

## Scope

- Render `restore list`, `restore show`, and `revert turn` results in the
  scrollable right-side detail panel.
- Keep existing status-bar summaries for quick feedback.
- Show snapshot metadata, runtime thread/turn binding, patch byte counts,
  untracked file/symlink counts, and a bounded patch preview.
- Show dry-run/apply restore plans with changed-file details.
- Keep rollback commands local-only in HTTP runtime TUI mode.

## Implementation

- Added `TuiMcpDetailKind::Rollback` so rollback details can reuse the existing
  right-side detail panel and scrolling behavior.
- `handle_tui_action` now fills the rollback panel after snapshot create/list,
  snapshot show, dry-run restore, and applied restore.
- Snapshot show reads `diff.patch` through the rollback store and renders a
  bounded patch preview.
- Runtime/TUI docs and the DeepSeek-TUI parity plan now describe the rollback
  detail panel.

## Verification

- `cargo test handle_tui_action_lists_shows_and_restores_rollback_snapshot --lib`
- `cargo test render_mcp_detail_uses_right_side_panel --lib`
- `cargo fmt --check`
- `git diff --check`

## Remaining

This slice improves inspection. A dedicated rollback confirmation modal and
interactive diff hunk browsing remain future work.
