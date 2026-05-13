# DeepSeek-TUI TUI Status Command

Status: implemented

## Gap

DeepSeek-TUI exposes `/status` as a compact runtime report covering the current
workspace, provider/model posture, session, MCP, token usage, cache telemetry,
context pressure, and estimated cost. DeepSeekCode already tracked much of the
same durable session/thread/task/usage state in the TUI, but users had to infer
it from separate panels and status lines.

## Implementation

- Added built-in `status` / `/status` parsing before custom slash-command
  fallback, so a project `/status.md` cannot shadow the runtime status command.
- Added `TuiMcpDetailKind::Status` for the right-side detail panel.
- Rendered a read-only TUI status report with version, UI mode, loaded
  sessions/threads, selected session, active thread, transcript item counts,
  task and automation counts, pending approvals/user-input requests, usage
  records, total/latest tokens, cache hit/miss rate, context policy, estimated
  cost, and input/output cost split when priced.
- Routed both command-palette `status` and composer `/status` without creating
  model turns or runtime actions.
- Updated TUI documentation and the DeepSeek-TUI parity plan.

## Verification

- `cargo test status --lib`
- `cargo test composer_intercepts_memory_prefix_and_slash_commands --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

DeepSeekCode does not yet expose a separate provider/model switcher or
status-line editor equivalent to DeepSeek-TUI's adjacent `/model` and
`/statusline` commands, so those remain separate command-registry parity gaps.
