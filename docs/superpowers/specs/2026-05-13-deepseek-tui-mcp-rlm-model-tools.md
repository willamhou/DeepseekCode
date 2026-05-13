# DeepSeek-TUI MCP RLM Model Tool Parity

Date: 2026-05-13

Status: completed

## Gap

MCP server mode now exposes local RLM planning/Python helpers, but the
model-running DeepSeek-TUI-compatible RLM entrypoints (`rlm`, aliases, process,
and batch forms) remained agent-only. MCP clients could plan chunks locally but
could not dispatch bounded child-agent RLM analysis through the same tool names.

## Spec

1. Expose model-running RLM tools only when trusted side effects or durable
   runtime approvals are enabled: `rlm`, `rlm_query`, `llm_query`,
   `rlm_process`, `rlm_batch`, `rlm_query_batched`, and `llm_query_batched`.
2. Keep these tools hidden by default because they can spend model tokens and
   use networked model APIs.
3. In durable approval mode, route calls through
   `permission_request kind=mcp` / `permission_response` before dispatching the
   child-agent RLM work.
4. Reuse the existing bounded `RlmTool` / `RlmBatchTool` implementations and
   their child step limits.
5. Document the MCP tool table and narrow Phase G2's remaining RLM gap.
6. Add focused tests for default hiding, opt-in visibility, default call
   rejection, and durable denial without invoking the model.

## Verification

- `/home/willamhou/.cargo/bin/cargo test mcp_tools_list_includes --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_tools_call --lib`
- `/home/willamhou/.cargo/bin/cargo test acp_session_tools_list_new_session_is_read_only --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `git diff --check`

## Implementation

- MCP `tools/list` now includes model-running RLM tools only in trusted
  side-effect or durable approval modes.
- MCP `tools/call` rejects model-running RLM tools by default.
- Durable approval mode writes `permission_request kind=mcp` and waits for the
  matching response before invoking `RlmTool` / `RlmBatchTool`.
