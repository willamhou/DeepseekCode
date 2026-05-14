# DeepSeek-TUI Parity: TUI LSP Command

## Context

DeepSeek-TUI exposes `/lsp status`, `/lsp on`, and `/lsp off` to inspect and
toggle inline diagnostics. DeepSeekCode already has post-edit diagnostics via
`diagnostics.post_edit`, but the TUI did not have the DeepSeek-TUI-compatible
slash entry point.

## Goals

- Add `lsp` / `/lsp` command-palette and composer support.
- Treat no argument, `status`, and `show` as status inspection.
- Support `on|enable|enabled|true|1` and `off|disable|disabled|false|0`
  aliases.
- Add `lsp help` / `/lsp help` detail rendering.
- Persist toggles in the selected workspace `.dscode/config.toml` as
  `diagnostics.post_edit`.
- Render current state in the right-side detail panel.
- Reject unsupported arguments with a concise usage error.
- Keep HTTP-runtime TUI behavior explicit: `/lsp` is local-file-backed because
  it edits project config.

## Design

The parser produces `TuiLspCommand` values. Help is handled in `TuiApp` because
it only renders local guidance. Status and toggles enqueue `TuiAction::Lsp` with
the selected session workspace so the local runtime handler can read or update
the project config.

The implementation maps DeepSeek-TUI's LSP toggle to DeepSeekCode's existing
diagnostics mechanism instead of creating an independent LSP state. When
enabled, successful file edits can run the existing post-edit diagnostics path;
manual diagnostics remain available through `diagnostics [--changed|paths...]`.

## Acceptance

- `lsp`, `/lsp`, `lsp status`, and `/lsp status` show the selected workspace
  `diagnostics.post_edit` state.
- `lsp on` / `/lsp on` writes `diagnostics.post_edit = true`.
- `lsp off` / `/lsp off` writes `diagnostics.post_edit = false`.
- `lsp help` / `/lsp help` renders command help and selected workspace context.
- Invalid arguments show `usage: lsp [on|off|status] or /lsp [on|off|status]`.
- HTTP-runtime TUI rejects `TuiAction::Lsp` as local-only.
- Tests cover command-palette routing, help, invalid arguments, local config
  writes, status rendering, and HTTP local-only rejection.
