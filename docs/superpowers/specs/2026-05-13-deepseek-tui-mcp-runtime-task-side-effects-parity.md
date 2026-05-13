# DeepSeek-TUI MCP Runtime Task Side Effects Parity

Date: 2026-05-13

Status: completed

## Gap

Phase G2 still has a real long-tail MCP side-effect gap. Agent-visible durable
task tools (`task_create`, `task_cancel`) can already create and cancel runtime
task metadata, and HTTP/TUI runtime paths can do the same, but `serve --mcp`
only exposes read-only task listing/reading. That leaves MCP clients unable to
drive the durable task queue without using the HTTP runtime API.

## Spec

1. Add MCP `runtime_create_task` for creating pending runtime tasks with
   `summary` / `prompt`, optional `kind`, `status`, `session_id`, `thread_id`,
   and `parent_task_id`.
2. Add MCP `runtime_cancel_task` for cancelling a task by `task_id` / `id` with
   an optional `reason`.
3. Expose both tools only when durable MCP approvals are enabled, matching the
   existing write-tool contract.
4. Route both calls through `permission_request` / `permission_response` before
   mutating runtime state.
5. Document the MCP tool table and narrow the G2 remaining task-surface gap.
6. Add focused tests for tool visibility, approved task creation, and approved
   task cancellation.

## Implementation

- Added `runtime_create_task` and `runtime_cancel_task` to `serve --mcp`
  `tools/list` when durable runtime approvals are enabled.
- Routed both tools through runtime `permission_request` /
  `permission_response` before mutating `.dscode/runtime` task metadata.
- Updated runtime docs and the DeepSeek-TUI parity plan to mark runtime task
  MCP side effects as landed and to narrow the remaining long-tail side-effect
  candidates.
- Added focused tests for tool visibility, approved task creation, and approved
  task cancellation.

## Verification

- `/home/willamhou/.cargo/bin/cargo test mcp_tools_list_includes_write_tools_only_with_durable_approvals --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_tools_call_creates_runtime_task_after_runtime_approval --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_tools_call_cancels_runtime_task_after_runtime_approval --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `git diff --check`
