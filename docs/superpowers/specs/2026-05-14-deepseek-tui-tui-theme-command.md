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
- Added command-palette and composer slash completions.
- Updated TUI documentation and the DeepSeek-TUI parity plan.

## Verification

- `cargo test theme_command_switches_and_renders_theme_state --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

Theme state is local to the running TUI process and is not yet persisted to
`.dscode/config.toml`. Persistent theme settings remain a separate config task.
