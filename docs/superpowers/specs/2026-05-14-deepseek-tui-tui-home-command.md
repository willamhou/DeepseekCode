# DeepSeek-TUI TUI Home Command

Status: implemented

## Gap

DeepSeek-TUI exposes `/home` with `stats` and `overview` aliases as a compact
dashboard for the current TUI state. DeepSeekCode already had detailed
`/status`, `/tokens`, and task panels, but lacked the single glanceable home
entry point and compatible aliases.

## Implementation

- Added built-in `home` / `/home` parsing before custom slash-command fallback.
- Added DeepSeek-TUI-compatible `stats` / `/stats` and `overview` /
  `/overview` aliases.
- Rendered selected session/thread, transcript, task, automation, pending
  approval/user-input, active usage, context, cost, and quick-action links in
  the right-side detail panel with `TuiMcpDetailKind::Home`.
- Added command-palette and composer slash completions for the home aliases.
- Kept `/status` as the deeper diagnostics view and `/home` as the compact
  dashboard.
- Updated TUI documentation and the DeepSeek-TUI parity plan.

## Verification

- `cargo test home_command_renders_runtime_dashboard --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

DeepSeekCode's dashboard reflects its durable runtime state and omits
DeepSeek-TUI-only local UI fields such as active skill and subagent cache. Those
remain covered by dedicated skill/task panels.
