# DeepSeek-TUI MCP Interactive Helper Surface

Date: 2026-05-13

Status: completed

## Gap

DeepSeekCode had agent-visible DeepSeek-TUI helpers for loading skills,
requesting user input, and notifying the user, but MCP/ACP clients could not
see or call those names. The runtime MCP table also overstated `image_analyze`
as MCP-visible even though it still needs a model-token and network contract.

## Spec

1. Expose `load_skill` through MCP/ACP as a local read-only helper that loads
   configured TOML skills and returns policy/context text.
2. Expose `request_user_input` through MCP/ACP as a validation/rendering helper
   for 1-3 DeepSeek-TUI-style questions; MCP/ACP clients can surface the
   returned prompt to the user and continue with the answer.
3. Expose `notify` through MCP/ACP with the existing bounded title/body
   validation and optional terminal attention signal.
4. Keep `note` / `remember` out of this slice because they append durable files.
5. Keep `image_analyze` out of this slice because it calls an external
   model/vision API and needs an explicit MCP model-token/network contract.
6. Add focused list and call tests, plus runtime/plan documentation updates.

## Verification

- `/home/willamhou/.cargo/bin/cargo test mcp_tools_call_executes_interactive_helper_tools --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_tools_list_includes_workspace_and_runtime_tools --lib`
- `/home/willamhou/.cargo/bin/cargo test acp_session_tools_list_new_session_is_read_only --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_ --lib`
- `/home/willamhou/.cargo/bin/cargo test acp_ --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `git diff --check`

## Implementation

- MCP `tools/list` now advertises `load_skill`, `request_user_input`, and
  `notify`.
- MCP `tools/call` dispatches those names to their existing local tool
  implementations.
- ACP inherits all three helpers through its session-scoped MCP adapter.
- The MCP runtime table no longer claims `image_analyze` is MCP-visible.
