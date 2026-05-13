# DeepSeek-TUI TUI Session Rename

Status: implemented

## Gap

DeepSeek-TUI includes `/rename <new title>` to persistently rename the current
session. DeepSeekCode's durable runtime sessions already had titles, but the TUI
had no command-palette or slash-style way to rename the selected session.

## Implementation

- Added `RuntimeStore::rename_session`, which updates a session title and
  `updated_at` timestamp through the same JSON persistence path as other runtime
  session mutations.
- Added `RenameSession` as a TUI action and routed `rename <title>` plus
  `/rename <title>` from the command palette and composer before custom slash
  command fallback.
- Enforced a 100-character title limit matching the DeepSeek-TUI command.
- Wired local file-backed TUI handling to persist the title and update the
  in-memory session list; HTTP-runtime TUI reports rename as local-only until
  the runtime API grows a session metadata mutation endpoint.
- Updated TUI documentation and the DeepSeek-TUI parity plan.

## Verification

- `cargo test rename --lib`
- `cargo test composer_intercepts_memory_prefix_and_slash_commands --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

HTTP-runtime session metadata mutation remains unsupported; the local TUI path
is complete for file-backed sessions.
