# DeepSeek-TUI MCP Shell Terminal Progress

Date: 2026-05-14

Status: implemented

## Gap

HTTP clients can follow shell-supervisor terminal event logs through SSE, and
ACP clients can consume the same records with `session/shell/subscribe`.
MCP clients still had only text snapshots from `exec_shell_attach`, so they
could not request MCP-native progress notifications for terminal event replay.

## Implementation

- Added read-only MCP tool `exec_shell_terminal_events`.
- The tool reads `{cwd}/.dscode/shell-jobs/{task_id}/terminal-events.jsonl`
  through the same terminal event snapshot helper as HTTP SSE and ACP.
- Parameters:
  - `task_id` / `id`
  - `cwd`
  - `cursor` / `since_seq` / `sinceSeq`
  - `limit`
  - `limit_bytes` / `limitBytes`
  - `tail`
  - `wait_ms` / `waitMs`
  - `poll_ms` / `pollMs`
- The final MCP `tools/call` result returns
  `deepseek.exec_shell.terminal_events.v1`, `next_cursor`, event count,
  `running`, `timed_out`, `truncated`, and terminal event lines.
- When the caller supplies `params._meta.progressToken`, the MCP stdio server
  writes `notifications/progress` frames before the final response. Each frame
  uses the original progress token and includes
  `_meta.deepseek.kind = deepseek.mcp.shell_terminal_event.v1`, `taskId`, event
  `seq`, event kind, and shell status.

## Verification

- `cargo test mcp_tools_call_shell_terminal_events_emits_progress_notifications --lib`
- `cargo test mcp_tools_list_includes_workspace_and_runtime_tools --lib`
- `cargo test mcp_ --lib -- --test-threads=1`
- `cargo fmt --check`
- `cargo check`
- `cargo test --lib -- --test-threads=1`
- `git diff --check`

## Remaining

MCP now has cursor-based shell terminal event replay plus MCP-native progress
notifications for active `tools/call` requests. Remaining shell-supervisor
parity is broader service-manager lifecycle coverage and Windows ConPTY.
