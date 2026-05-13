# DeepSeek-TUI TUI Links Command

Status: implemented

## Gap

DeepSeek-TUI exposes `/links` with `dashboard` and `api` aliases so users can
discover the DeepSeek platform dashboard and API documentation from inside the
TUI. DeepSeekCode did not have a matching command, so repository, release,
project docs, and DeepSeek API links were not discoverable from the workbench.

## Implementation

- Added built-in `links` / `/links` parsing before custom slash-command
  fallback.
- Added DeepSeek-TUI-compatible `dashboard` / `/dashboard` and `api` / `/api`
  aliases.
- Rendered DeepSeekCode repository, issues, releases, docs, DeepSeek platform,
  and DeepSeek API documentation links in the right-side detail panel with
  `TuiMcpDetailKind::Links`.
- Added command-palette and composer slash completions for the links aliases.
- Kept the command terminal-native: it shows links instead of launching a GUI
  browser from the TUI process.
- Updated TUI documentation and the DeepSeek-TUI parity plan.

## Verification

- `cargo test links_command_renders_repository_and_api_links --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

DeepSeekCode includes project-specific repository/docs/release links in
addition to DeepSeek-TUI's platform/docs links. It does not yet provide a modal
link picker; the aliases open a persistent detail panel.
