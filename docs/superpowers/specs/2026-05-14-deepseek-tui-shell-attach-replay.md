# DeepSeek-TUI Shell Attach Replay

Status: implemented

## Gap

After PTY resize support, DeepSeekCode still exposed shell output primarily as
wait deltas, clipped snapshots, or stream-specific durable log replay. That was
usable for model polling, but it was not a good attach-style terminal contract:
clients had to know which stream to replay, carry offsets manually, and stitch
metadata from separate show calls.

DeepSeek-TUI's shell/job-center surface emphasizes inspecting live or recent
tool work from the terminal. DeepSeekCode needed a single read-only attach
snapshot that terminal clients can poll with a cursor while still being honest
that the implementation is durable-log replay, not resident PTY takeover.

## Implementation

- Added `exec_shell_attach`.
- The tool accepts `task_id`, optional `cwd`, `cursor` / `offset`,
  `limit_bytes`, `tail`, and `wait_ms`.
- It refreshes in-memory jobs when available, then reads the durable job
  manifest and stdout PTY/log bytes.
- Output includes command/status, TTY backend and geometry, cursor fields,
  `next_offset`, `total_bytes`, timeout state, and a `terminal:` block.
- For TTY jobs, stdout is the PTY transcript. For non-TTY jobs, the attach view
  is stdout-oriented and explicitly points callers to
  `exec_shell_replay stream=stderr` for stderr-only logs.
- MCP exposes `exec_shell_attach` as a read-only default tool.
- The model tool registry and DeepSeek static tool schema now include
  `exec_shell_attach`.
- The local TUI command palette now supports `shell attach <id> [cursor|tail]`
  and `jobs attach <id> [cursor|tail]`.

## Verification

- `cargo test exec_shell_attach --lib`
- `cargo test exec_shell --lib`
- `cargo test default_registry_includes_exec_shell_background_tools --lib`
- `cargo test build_tool_specs_include_exec_shell_background_tools --lib`
- `cargo test command_palette_requests_shell_job_actions --lib`
- `cargo test mcp_tools_list --lib`
- `cargo test tui --lib`
- `cargo test serve --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

This closes the attach-style durable replay surface, but it is still not a
resident PTY supervisor. The remaining hard shell gap is a supervisor process
that owns PTYs independently from the original DeepSeekCode process and can
provide true interactive terminal takeover, resize propagation from terminal
events, and screen-state replay beyond durable stdout bytes.
