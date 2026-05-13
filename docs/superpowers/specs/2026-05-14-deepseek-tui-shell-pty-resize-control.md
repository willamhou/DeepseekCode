# DeepSeek-TUI Shell PTY Resize Control

Status: implemented

## Gap

DeepSeekCode already had PTY-backed background shell starts with initial
`tty_rows` / `tty_cols`, durable stdout/stderr replay, detached FIFO stdin, and
best-effort detached cancellation. The remaining PTY gap called out by the
DeepSeek-TUI parity plan was that running TTY jobs could not be resized after
start, and the local TUI command palette had no resize control.

## Implementation

- Added `exec_shell_resize`.
- The tool requires `task_id` plus `tty_rows` / `tty_cols`, with `rows` / `cols`
  aliases.
- Attached running TTY jobs update the durable manifest and receive a best-effort
  `stty rows <n> cols <n>` command through their stdin control.
- Detached running TTY jobs with Unix FIFO stdin receive the same best-effort
  control command and persist the updated geometry.
- Detached or completed TTY records still update durable geometry metadata for
  later show/replay inspection.
- MCP exposes `exec_shell_resize` only with trusted side effects or durable
  runtime approvals.
- The local TUI command palette now accepts `shell resize <id> <rows> <cols>` and
  `jobs resize <id> <rows> <cols>`.

## Verification

- `cargo test exec_shell_resize --lib`
- `cargo test exec_shell --lib`
- `cargo test default_registry_includes_exec_shell_background_tools --lib`
- `cargo test build_tool_specs_include_exec_shell_background_tools --lib`
- `cargo test command_palette_requests_shell_job_actions --lib`
- `cargo test tui --lib`
- `cargo test mcp_tools_list_includes_run_shell_when_side_effects_enabled --lib`
- `cargo test mcp_tools_list_includes_workspace_and_runtime_tools --lib`
- `cargo test mcp_tools_list_includes_run_shell_when_durable_approvals_enabled --lib`
- `cargo test mcp_tools_list --lib`
- `cargo test serve --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

This is not a resident PTY supervisor. The remaining hard shell gap is a
supervisor process that owns the PTY independently from the original
DeepSeekCode process and can provide true terminal attach/replay semantics
beyond durable log slices.
