# DeepSeek-TUI TUI MCP Manager Discovery Refresh

**Status:** implemented
**Comparison source:** `Hmbown/DeepSeek-TUI` refreshed at `/tmp/deepseek-tui-compare-20260514`; latest fetched `origin/main` `13e7957621448792beda06ec8615e33cb374adce`, including upstream commit `11c655b` (`fix(tui): refresh mcp discovery on manager open`).

## Gap

DeepSeek-TUI refreshes deferred MCP discovery when the MCP manager opens, so
the manager reflects tools/resources that are already available to the model.
DeepSeekCode's local TUI MCP manager previously rendered config and server
inventory, while discovery/health refresh stayed behind the explicit
`mcp validate` action.

## Implementation

- Extended the local TUI MCP manager summary with a `Discovery refresh` section.
- Reused `validate_servers_summary` so opening the manager refreshes MCP
  discovery and health using the same code path as explicit validation.
- Kept the manager open path resilient by rendering discovery errors inline
  instead of failing the whole manager view.
- Added a regression assertion that opening the MCP manager includes discovery
  refresh output.

## Verification

- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `/home/willamhou/.cargo/bin/cargo test handle_tui_action_manages_project_mcp_config --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_manager --lib`
- `/home/willamhou/.cargo/bin/cargo check`
- `/home/willamhou/.cargo/bin/cargo test --lib -- --test-threads=1`
- `git diff --check`

## Remaining

This closes the stale manager-open discovery gap for DeepSeekCode's local TUI.
The keyboard `r reload` action still routes through the existing MCP inventory
status action; broader reload semantics can be tightened in a later slice if
needed.
