# DeepSeek-TUI MCP Read-Only Helper Surface

Date: 2026-05-13

Status: completed

## Gap

Several DeepSeek-TUI-compatible helper tools were already agent-visible but not
available through `serve --mcp` or ACP's inherited `session/tools/list` bridge:
`review`, `recall_archive`, `tool_search_tool_regex`, and
`tool_search_tool_bm25`.

## Spec

1. Expose the deterministic local `review` helper through MCP/ACP for files,
   diffs, and supplied `github_pr_context` text.
2. Expose `recall_archive` through MCP/ACP so clients can search durable
   runtime turns/items without using the HTTP API directly.
3. Expose `tool_search_tool_regex` and `tool_search_tool_bm25` through MCP/ACP
   so clients can discover the static DeepSeekCode tool catalog.
4. Keep this slice read-only: no `remember`, `note`, `request_user_input`,
   `notify`, or semantic child-agent review is newly enabled through MCP.
5. Add focused MCP call tests and ACP list visibility coverage.

## Verification

- `/home/willamhou/.cargo/bin/cargo test mcp_tools_call_executes_read_only_helper_tools --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_tools_list_includes_workspace_and_runtime_tools --lib`
- `/home/willamhou/.cargo/bin/cargo test acp_session_tools_list_new_session_is_read_only --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_ --lib`
- `/home/willamhou/.cargo/bin/cargo test acp_ --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `git diff --check`

## Implementation

- MCP `tools/list` now advertises `review`, `recall_archive`,
  `tool_search_tool_regex`, and `tool_search_tool_bm25`.
- MCP `tools/call` dispatches those names to the existing local tool
  implementations.
- ACP inherits the same read-only tools through the session-scoped MCP adapter,
  and ACP tool update kind mapping categorizes review/recall as read and tool
  search as search.

Follow-up interactive helper work now exposes `request_user_input` and
`notify`; durable `note` / `remember` writes and semantic child-agent review
remain separate safety contracts.
