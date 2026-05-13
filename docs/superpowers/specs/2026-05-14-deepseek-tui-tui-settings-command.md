# DeepSeek-TUI TUI Settings Command

Status: implemented

## Gap

DeepSeek-TUI exposes `/settings` and `/config` as a first-class configuration
entry point. DeepSeekCode had focused configuration commands for mode, model,
provider, network, memory, and MCP, but no single workbench settings overview.

## Implementation

- Added built-in `settings` / `/settings` parsing before custom slash-command
  fallback.
- Added `config` / `/config` aliases for DeepSeek-TUI-style configuration
  discovery.
- Rendered current mode, selected workspace config path, user config path,
  workbench state, and focused configuration command entry points in the
  right-side detail panel with `TuiMcpDetailKind::Settings`.
- Routed settings commands from both command palette and focused composer
  without starting a model turn.
- Added command-palette and composer slash completions.
- Updated TUI documentation and the DeepSeek-TUI parity plan.

## Verification

- `cargo test settings_command_renders_configuration_entry_points --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

DeepSeekCode's `/settings` is a terminal-native overview instead of
DeepSeek-TUI's modal settings editor. Mutation remains explicit through focused
commands such as `/model`, `/provider`, `/network`, `/memory`, and `/mcp`.
