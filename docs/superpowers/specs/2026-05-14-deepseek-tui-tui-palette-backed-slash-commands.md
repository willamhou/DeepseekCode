# DeepSeek-TUI TUI Palette-Backed Slash Commands

Status: implemented

## Gap

DeepSeek-TUI registers `/mcp`, `/jobs`, and `/restore` as first-class slash
commands. DeepSeekCode already supported the corresponding command-palette
forms, but composer input with a leading slash could fall through to the custom
slash-command fallback instead of the built-in local dispatcher.

## Implementation

- Added a small composer bridge for palette-backed built-ins:
  `/mcp`, `/jobs` / `/job`, and `/restore` / `/revert`.
- Normalized those composer inputs by removing the leading slash and routed them
  through the existing command-palette dispatcher.
- Preserved custom slash fallback for all other `/name` inputs.
- Updated TUI documentation and the DeepSeek-TUI parity plan.

## Verification

- `cargo test composer_routes_palette_backed_slash_commands_before_custom_fallback --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

No known composer slash fallback gap remains for `/mcp`, `/jobs`, or `/restore`.
