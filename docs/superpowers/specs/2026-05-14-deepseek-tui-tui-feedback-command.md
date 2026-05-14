# DeepSeek-TUI TUI Feedback Command

Status: implemented

## Gap

DeepSeek-TUI exposes `/feedback [bug|feature|security]` so users can discover
where to report bugs, request features, or review the security policy from
inside the TUI. DeepSeekCode did not have a matching TUI command, so feedback
routes were not discoverable from the workbench.

## Implementation

- Added built-in `feedback` / `/feedback`, `feedback show`, and
  `/feedback show` parsing before custom slash-command fallback.
- Added `feedback bug`, `feedback feature`, and `feedback security` aliases,
  including DeepSeek-TUI-style numeric shortcuts.
- Rendered the repository, issue, and security-policy links in the right-side
  detail panel with `TuiMcpDetailKind::Feedback`.
- Added command-palette and composer slash completions for feedback commands.
- Kept the command terminal-native: it shows links instead of launching a GUI
  browser from the TUI process.
- Updated TUI documentation and the DeepSeek-TUI parity plan.

## Verification

- `cargo test feedback_command_renders_links --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

The interactive feedback picker is covered by
`2026-05-14-deepseek-tui-tui-feedback-picker.md`. The terminal command path is
covered by explicit feedback subcommands and the detail panel.
