# DeepSeek-TUI MCP Shell Session Parity

Date: 2026-05-13

Status: completed

## Gap

DeepSeekCode already has DeepSeek-TUI-compatible background shell session tools
in the agent registry (`exec_shell`, `task_shell_start`, wait/poll/show/list,
stdin, and cancel). MCP server mode only exposes `run_shell` / `run_tests`, so
MCP clients cannot start and control long-running shell sessions through the
same terminal-agent surface.

## Spec

1. Expose read-only MCP shell session inspection tools:
   `exec_shell_list`, `exec_shell_show`, `exec_shell_wait`, `exec_wait`, and
   `task_shell_wait`.
2. Expose mutating MCP shell session tools only when trusted side effects or
   durable approvals are enabled: `exec_shell`, `task_shell_start`,
   `exec_shell_interact`, `exec_interact`, and `exec_shell_cancel`.
3. Keep command safety: starts continue to use the existing safe shell command
   allowlist before execution.
4. In durable approval mode, route starts/interact/cancel through
   `permission_request kind=shell` / `permission_response` before execution.
5. Document the MCP tool table and narrow Phase G2's remaining long-tail
   side-effect list.
6. Add focused tests for visibility and approved start/wait/cancel behavior.

## Verification

- `/home/willamhou/.cargo/bin/cargo test mcp_tools_list_includes --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_tools_call --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_tools_call_starts_waits_and_cancels_shell_session_after_runtime_approval --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_tools_call_rejects_unsafe_shell_session_before_runtime_approval --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `git diff --check`

## Implementation

- MCP `tools/list` now advertises read-only shell-session inspection/wait tools
  by default.
- MCP mutating shell-session tools are exposed only when trusted side effects or
  durable runtime approvals are enabled.
- Durable approval mode records `permission_request kind=shell` before
  start/stdin/cancel operations and waits for the matching
  `permission_response`.
- Documentation now includes the MCP tool table entries and Phase G2 remaining
  work no longer lists shell-session exposure as an open MCP gap.
