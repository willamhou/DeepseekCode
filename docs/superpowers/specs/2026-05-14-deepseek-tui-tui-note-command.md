# DeepSeek-TUI TUI Note Command

Status: implemented

## Gap

DeepSeek-TUI exposes `/note` for persistent workspace notes, including add,
list, show, edit, remove, clear, and path subcommands. DeepSeekCode already had
agent/MCP-visible `note` tooling and TUI user-memory commands, but local TUI
users could not manage `memory.notes_path` notes directly.

## Implementation

- Added built-in `note` / `/note` parsing before custom slash-command fallback.
- Supported `/note <text>`, `/note add <text>`, `/note list`, `/note show <n>`,
  `/note edit <n> <text>`, `/note remove <n>`, `/note clear`, `/note path`, and
  `/note help`.
- Routed note commands from both command palette and focused composer without
  starting a model turn.
- Added local file-backed handling over the configured `memory.notes_path`.
- Rendered list/show/help/path results in the right-side detail panel with
  `TuiMcpDetailKind::Note`.
- Added command completions, help metadata, TUI docs, and the DeepSeek-TUI
  parity plan entry.

## Verification

- `cargo test command_palette_requests_note_actions --lib`
- `cargo test handle_tui_action_manages_note_file --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

Note editing uses explicit replacement text in the command instead of spawning
an external editor inside the TUI. That matches the current local TUI approach
for memory editing, which prints editor commands instead of taking over the
terminal process.
