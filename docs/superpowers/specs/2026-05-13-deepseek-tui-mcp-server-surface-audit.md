# DeepSeek-TUI MCP Server Surface Audit

Date: 2026-05-13

Status: completed

## Gap

The Phase G2 parity plan understated and partially misstated the current
`serve --mcp` tool surface. It still described the side-effect surface as
stopping at `run_shell`, `apply_patch`, `write_file`, `delete_file`,
`copy_file`, and `move_file`, even though `edit_file`, `revert_turn`,
`github_comment`, and `github_close_issue` are already wired through durable
runtime approvals. It also listed `web_run` as MCP-visible in this section,
while the MCP stdio server exposes narrower first-class read-only web/market
tools.

## Spec

1. Align the Phase G2 read-only MCP tool list with the actual
   `mcp_tool_definitions` / `execute_mcp_tool` surface.
2. Document the full approval-gated MCP write surface now available:
   `apply_patch`, `write_file`, `edit_file`, `delete_file`, `copy_file`,
   `move_file`, `revert_turn`, `github_comment`, and `github_close_issue`.
3. Keep a true remaining gap for long-tail agent-only side-effect tools that
   are not yet MCP-server-visible, plus the existing ACP streaming gap.
4. Strengthen focused tests so `tools/list` asserts the full read-only surface
   and `tools/call github_close_issue` is covered through durable approvals.

## Implementation

- Updated the Phase G2 parity plan to match the actual MCP server read-only
  tool surface.
- Documented the full durable-approval MCP write surface, including
  `edit_file`, `revert_turn`, `github_comment`, and `github_close_issue`.
- Narrowed the remaining MCP server gap to long-tail agent-only side-effect
  tools that still need explicit MCP safety contracts.
- Expanded focused MCP tests for read-only `tools/list` coverage and
  `github_close_issue` durable-approval execution.

## Verification

- `/home/willamhou/.cargo/bin/cargo test mcp_tools_list_includes_workspace_and_runtime_tools --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_tools_list_includes_write_tools_only_with_durable_approvals --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_tools_call_executes_github_close_issue_after_runtime_approval --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_tools_call_executes_github_comment_after_runtime_approval --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_tools_call_executes_edit_file_after_runtime_approval --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_tools_call_executes_revert_turn_after_runtime_approval --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `git diff --check`
