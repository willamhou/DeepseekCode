# DeepSeek-TUI ACP Standard Tool Updates

Date: 2026-05-13

Status: completed

## Gap

DeepSeekCode's ACP `session/tools/call` bridge emitted custom started/result
notifications (`tool_call_update` plus `tool_result_update`). The current ACP
tool-call shape expects an initial `sessionUpdate: "tool_call"` and subsequent
`sessionUpdate: "tool_call_update"` events keyed by the same `toolCallId`, with
standard status, content, raw input, and raw output fields.

## Spec

1. Emit a standard initial `tool_call` session update before the final JSON-RPC
   result, including `toolCallId`, title, kind, `in_progress` status, and
   `rawInput`.
2. Emit a standard final `tool_call_update` session update with the same
   `toolCallId`, `completed` / `failed` status, text content, and `rawOutput`.
3. Preserve loaded-runtime traceability by carrying runtime turn/item ids under
   `_meta.runtime`.
4. Keep the existing final JSON-RPC `session/tools/call` result unchanged for
   clients that only consume request/response.
5. Update docs and tests to validate the standard payload shape.

## Verification

- `/home/willamhou/.cargo/bin/cargo test acp_session_tools_call --lib`
- `/home/willamhou/.cargo/bin/cargo test acp_loaded_session_tools_call_write_file_uses_runtime_approval --lib`
- `/home/willamhou/.cargo/bin/cargo test acp_ --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `git diff --check`

## Implementation

- ACP tool-call notifications now use `toolCallId` and ACP status names.
- Tool categories are mapped to ACP `kind` values such as `read`, `edit`,
  `execute`, `search`, `fetch`, `think`, and `other`.
- Runtime ids moved from custom top-level fields into `_meta.runtime`.
