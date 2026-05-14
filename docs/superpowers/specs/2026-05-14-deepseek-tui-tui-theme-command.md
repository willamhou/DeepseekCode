# DeepSeek-TUI TUI Theme Command

Status: implemented

## Gap

DeepSeek-TUI exposes `/theme [dark|light|grayscale|system]` for runtime theme
switching. DeepSeekCode used fixed TUI accent colors and had no command-driven
theme state.

## Implementation

- Added a local `TuiTheme` state with `Dark`, `Light`, `Grayscale`, and
  `System` modes.
- Added built-in `theme` / `/theme` parsing before custom slash-command
  fallback.
- Matched DeepSeek-TUI's no-argument behavior by cycling the current theme;
  explicit `dark|light|grayscale|system` arguments switch directly.
- Rendered current theme and available theme commands in the right-side detail
  panel with `TuiMcpDetailKind::Theme`.
- Wired theme accent, hint, and label colors into mode tabs, sidebar,
  command bar, and command palette rendering.
- Persisted local TUI theme choice in `.dscode/tui/theme.json` and restored it
  when the local TUI starts.
- Added command-palette and composer slash completions.
- Updated TUI documentation and the DeepSeek-TUI parity plan.

## Verification

- `cargo test theme_command_switches_and_renders_theme_state --lib`
- `cargo test theme_preferences_persist_across_tui_instances --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

No known DeepSeek-TUI theme-command parity blocker remains. A future global
config theme key in `.dscode/config.toml` could be added as product polish, but
local TUI restarts now preserve the selected theme.
