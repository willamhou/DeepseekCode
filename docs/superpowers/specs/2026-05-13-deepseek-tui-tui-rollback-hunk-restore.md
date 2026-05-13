# DeepSeek-TUI TUI Rollback Hunk Restore

Date: 2026-05-13

Status: completed

## Gap

The TUI rollback panel could inspect snapshot hunks, dry-run full restores, and
apply full snapshot restores behind a confirmation modal. The remaining Phase F
TUI rollback gap was selective restore: operators could not apply one reviewed
hunk while leaving other snapshot hunks untouched.

## Spec

1. Keep existing hunk browsing commands unchanged:
   - `restore hunks <id|last>`
   - `restore diff <id|last>`
   - `restore hunk <id|last> [index]`
2. Add hunk restore checks:
   - `restore hunk <id|last> <index> --check`
   - `restore hunk-check <id|last> <index>`
   - `restore check-hunk <id|last> <index>`
3. Add hunk restore apply with the existing confirmation modal:
   - `restore hunk <id|last> <index> --apply`
   - `restore hunk-apply <id|last> <index>`
   - `restore apply-hunk <id|last> <index>`
4. Build a single-hunk patch from the stored snapshot diff prelude plus the
   selected hunk body. `--check` runs `git apply --check`; confirmed apply runs
   `git apply`.
5. Preserve safety boundaries:
   - Resolve `last` to the active thread's latest turn id.
   - Require the current git `HEAD` to match the snapshot `git_head`.
   - Keep rollback hunk apply local-only in file-backed TUI sessions.
   - Do not change full snapshot restore behavior.

## Verification

- `/home/willamhou/.cargo/bin/cargo test command_palette_requests_rollback_hunk_browser --lib`
- `/home/willamhou/.cargo/bin/cargo test handle_tui_action_restores_single_rollback_hunk --lib`
- `/home/willamhou/.cargo/bin/cargo test handle_tui_action_lists_shows_hunks_and_restores_rollback_snapshot --lib`
- `/home/willamhou/.cargo/bin/cargo test tui::tests --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `git diff --check`

## Implementation

- TUI actions now include `RestoreRollbackHunk { id, hunk, apply }`.
- The rollback confirmation modal can represent either full snapshot apply or one
  selected hunk apply.
- The local TUI action handler parses stored snapshot patch hunks with their diff
  prelude, creates a standalone one-hunk patch, and applies/checks it against the
  local git worktree.
