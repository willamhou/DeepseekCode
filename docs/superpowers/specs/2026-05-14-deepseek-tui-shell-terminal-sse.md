# DeepSeek-TUI Shell Terminal SSE

Date: 2026-05-14

Status: implemented

## Gap

Shell-supervisor jobs now produce durable `terminal-events.jsonl`, and the
human CLI can replay or attach snapshots through command/response calls. HTTP
runtime clients still lacked a native streaming contract for terminal output,
so external TUI/workbench clients had to poll `exec_shell_attach`-style
responses.

## Implementation

- Added `GET /v1/shell/jobs/{task_id}/events/stream`.
- The endpoint reads the workspace shell job manifest from
  `{cwd}/.dscode/shell-jobs/{task_id}/manifest.json`.
- `cwd` is accepted as a query parameter; if omitted, the HTTP runtime process
  current directory is used.
- `cursor` and `since_seq` resume from a terminal event sequence.
- `limit_bytes` bounds replay payload size, with a default of 20KB and a 100KB
  cap.
- `wait_ms` and `poll_ms` provide bounded long-polling for new terminal events.
- `follow=1` streams later events on one connection with `max_events`,
  `max_ms`, and `poll_ms` controls.
- SSE frames use:
  - `id: <terminal event seq>`
  - `event: terminal_<kind>`
  - `data: deepseek.exec_shell.terminal_event.v1 JSON`
- `/runtime` now advertises the shell terminal SSE endpoint and capabilities.

## Verification

- `cargo test shell_terminal_event_stream_endpoint_replays_sse_frames --lib`
- `cargo test event_stream_endpoint_replays_sse_frames --lib`
- `cargo test exec_shell_replay_reads_terminal_event_log_by_cursor --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

This closes the HTTP runtime streaming surface for shell terminal event logs.
ACP push-frame integration is covered by `session/shell/subscribe`, and MCP
progress integration is covered by `exec_shell_terminal_events` with
`notifications/progress`. Remaining shell-supervisor parity work is actual
installed systemd/launchd service smoke evidence and Windows ConPTY.
