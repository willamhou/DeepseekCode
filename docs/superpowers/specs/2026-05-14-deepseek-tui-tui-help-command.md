# DeepSeek-TUI TUI Help Command

Status: implemented

## Gap

DeepSeek-TUI exposes `/help [command]` and `/?` as first-class command
discovery. DeepSeekCode had a terse command-palette `help` status string, but
no persistent help panel, no composer slash help path, and no command-specific
topic detail.

## Implementation

- Added built-in `help` / `/help` / `?` / `/?` parsing before custom
  slash-command fallback.
- Added a categorized help index rendered in the right-side detail panel with
  `TuiMcpDetailKind::Help`.
- Added command-specific topic lookup by command name or alias, including
  usage, aliases, and description.
- Routed help commands from both command palette and focused composer without
  starting a model turn.
- Added command-palette and composer slash completions for common help topics.
- Updated TUI documentation and the DeepSeek-TUI parity plan.

## Verification

- `cargo test help_command_renders_index_and_topics --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

DeepSeekCode's help panel is terminal-native and does not open DeepSeek-TUI's
modal help view. It covers the active DeepSeekCode TUI command surface and
aliases.
