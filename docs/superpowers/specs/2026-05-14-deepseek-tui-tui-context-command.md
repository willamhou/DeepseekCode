# DeepSeek-TUI TUI Context Command

Status: implemented

## Gap

DeepSeek-TUI exposes `/context` and the `/ctx` alias to open a context
inspector. DeepSeekCode had `/tokens`, `/status`, and durable compaction
controls, but no matching context-inspector command surface in the TUI.

## Implementation

- Added built-in `context` / `/context` and `ctx` / `/ctx` parsing before
  custom slash-command fallback.
- Rendered a right-side `Context` detail panel with selected thread identity,
  transcript item/display-line counts, item state/type counts, reasoning replay
  settings, active usage records, context window usage, remaining tokens,
  compaction strategy, latest/total token counts, and cache hit/miss telemetry.
- Routed context commands from both command palette and focused composer without
  starting a model turn.
- Added command-palette and composer slash completions plus help metadata.
- Updated TUI documentation and the DeepSeek-TUI parity plan.

## Verification

- `cargo test context_command_renders_active_context_inspector --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

DeepSeekCode renders the inspector in the existing right-side detail panel
instead of a dedicated modal. That matches the current local TUI architecture
used by `/status`, `/tokens`, and `/reasoning`.
