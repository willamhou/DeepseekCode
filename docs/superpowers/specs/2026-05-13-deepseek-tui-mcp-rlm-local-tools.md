# DeepSeek-TUI MCP RLM Local Tool Parity

Date: 2026-05-13

Status: completed

## Gap

DeepSeekCode already exposes DeepSeek-TUI-compatible RLM planning and Python
helper tools to agent runs, but MCP server mode did not expose any RLM surface.
MCP clients therefore could not reuse local chunk planning, map-reduce planning,
or persisted RLM Python helper inventories.

## Spec

1. Expose deterministic, local RLM helpers through MCP by default:
   `rlm_chunk_plan`, `rlm_map_reduce_plan`, `rlm_python`, and
   `rlm_python_sessions`.
2. Keep model-running RLM child-agent tools out of MCP for this slice until
   their model/cost/network approval contract is explicit.
3. Expose stateful `rlm_python_session` only when trusted side effects or
   durable runtime approvals are enabled, because it writes
   `.dscode/rlm-python` JSON state and can keep a process alive.
4. In durable approval mode, route `rlm_python_session` through
   `permission_request kind=write` / `permission_response` before execution.
5. Document the MCP tool table and narrow Phase G2's remaining RLM gap to
   model-running child-agent RLM tools.
6. Add focused tests for visibility, local chunk planning, and default
   `rlm_python_session` rejection.

## Verification

- `/home/willamhou/.cargo/bin/cargo test mcp_tools_list_includes --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_tools_call --lib`
- `/home/willamhou/.cargo/bin/cargo test acp_session_tools_list_new_session_is_read_only --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `git diff --check`

## Implementation

- MCP state now carries `AppConfig` so RLM session inventory/state tools use the
  same `.dscode` config root as the server.
- MCP `tools/list` advertises local RLM planning and restricted Python helpers
  by default.
- MCP `tools/list` advertises `rlm_python_session` only with trusted side
  effects or durable runtime approvals.
- ACP session tools inherit the same MCP/RLM bridge through the existing
  session-scoped MCP state adapter.
