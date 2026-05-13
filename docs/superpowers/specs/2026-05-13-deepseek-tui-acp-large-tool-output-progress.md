# DeepSeek-TUI ACP Large Tool Output Progress

Date: 2026-05-13

Status: completed

## Gap

DeepSeekCode ACP tool calls already emit standard `tool_call` and final
`tool_call_update` notifications, but large tool outputs still arrive as one
completion payload. ACP clients cannot progressively render a long file read,
large grep output, or verbose command result before the final JSON-RPC response.

## Spec

1. Preserve the existing small-output `session/tools/call` response order:
   initial `tool_call`, final `tool_call_update`, final JSON-RPC result.
2. For large tool outputs, insert bounded intermediate
   `sessionUpdate: "tool_call_update"` notifications with `status:
   "in_progress"` before the final completion update.
3. Mark intermediate raw output as partial and include `chunkIndex`,
   `chunkCount`, and `truncated` metadata so clients can render progress
   without treating it as final output.
4. Keep the final JSON-RPC result and final completed/failed tool update
   unchanged and complete.
5. Preserve loaded-session runtime metadata on every progress update.

## Verification

- `/home/willamhou/.cargo/bin/cargo test acp_session_tools_call_streams_large_tool_output_updates --lib`
- `/home/willamhou/.cargo/bin/cargo test acp_session_tools_call --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `git diff --check`

## Implementation

- ACP `session/tools/call` now emits up to four intermediate progress chunks for
  outputs larger than 4096 characters.
- Progress chunks use the standard `tool_call_update` shape with matching
  `toolCallId`, `status: "in_progress"`, text content, and partial `rawOutput`.
- Small outputs keep the previous three-response shape for compatibility.
