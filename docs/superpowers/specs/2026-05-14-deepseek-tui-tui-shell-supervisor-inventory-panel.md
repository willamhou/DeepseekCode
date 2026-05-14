# DeepSeek-TUI parity: TUI shell supervisor inventory panel

Status: implemented
Date: 2026-05-14

## Gap

DeepSeekCode's shell supervisor protocol can expose durable shell-job inventory
for `show`, but the local TUI `shell supervisor` / `jobs supervisor` command
still rendered only the supervisor status probe. That left the workbench job
center one step behind the protocol surface.

## Implementation

- TUI `ShellSupervisorStatus` handling now renders supervisor status followed by
  a `Shell job inventory` section from `ExecShellListTool`.
- Inventory failures are shown as `job_inventory_error` in the shell detail
  panel while preserving the supervisor status result.
- This remains a read-only inventory/status panel. It does not implement native
  supervisor-owned PTY start, attach, stdin, resize, replay, wait, or cancel.

## Verification

- `cargo test handle_tui_action_shell_supervisor_shows_job_inventory --lib`
- `cargo test tui --lib`
- `cargo check`
- `cargo fmt --check`
- `cargo test --lib -- --test-threads=1`
- `git diff --check`
