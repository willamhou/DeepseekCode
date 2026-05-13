# DeepSeek-TUI TUI Statusline Command

Status: implemented

## Gap

DeepSeek-TUI exposes `/statusline` to configure footer/status-line items.
DeepSeekCode had a fixed command bar with status text and shortcuts, but no
command entry point that explains the active statusline surface.

## Implementation

- Added built-in `statusline` / `/statusline` parsing before custom
  slash-command fallback.
- Added `status-line` / `/status-line` aliases for users who type the hyphenated
  form.
- Rendered current status, mode, theme, active detail panel, command bar items,
  shortcuts, and related status/config commands in the right-side detail panel
  with `TuiMcpDetailKind::StatusLine`.
- Routed statusline commands from both command palette and focused composer
  without starting a model turn.
- Added command-palette and composer slash completions.
- Updated TUI documentation and the DeepSeek-TUI parity plan.

## Verification

- `cargo test statusline_command_renders_command_bar_detail --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

DeepSeekCode currently exposes a fixed command bar instead of DeepSeek-TUI's
interactive persisted footer-item picker. Interactive statusline persistence
remains a separate config task.
