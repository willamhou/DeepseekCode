# DeepSeek-TUI ACP Shell Terminal Subscribe

Date: 2026-05-14

Status: implemented

## Gap

HTTP clients can now stream shell-supervisor terminal events through
`/v1/shell/jobs/{task_id}/events/stream`, but ACP clients still had to call
`exec_shell_attach` repeatedly and parse text snapshots. That left the ACP
surface behind the HTTP runtime for attach-style shell terminal updates.

## Implementation

- Added ACP extension method `session/shell/subscribe`.
- `initialize` now advertises
  `sessionCapabilities.shellTerminalEvents.subscribe` with
  `cursor = "terminal_event_seq"`.
- The method accepts:
  - `sessionId`
  - `taskId` / `task_id`
  - `cursor` / `sinceSeq` / `since_seq`
  - `limit`
  - `limitBytes` / `limit_bytes`
  - `waitMs` / `wait_ms`
  - `pollMs` / `poll_ms`
  - `tail`
- It reads `{session.cwd}/.dscode/shell-jobs/{taskId}/terminal-events.jsonl`
  through the same terminal event snapshot helper used by HTTP SSE.
- It emits ACP `session/update` notifications using standard
  `tool_call_update` payloads with stable `toolCallId = shell_<taskId>`.
- Updates include `_meta.deepseek.kind =
  "deepseek.acp.shell_terminal_event.v1"`, terminal `seq`, and optional runtime
  session/thread ids when the ACP session was loaded from durable runtime
  state.
- The final JSON-RPC result returns `cursor`, `nextCursor`, `taskId`, `status`,
  `running`, `updates`, `truncated`, and `timedOut`.

## Verification

- `cargo test acp_session_shell_subscribe_pushes_terminal_events --lib`
- `cargo test acp_initialize_advertises_baseline_agent --lib`
- `cargo test shell_terminal_event_stream_endpoint_replays_sse_frames --lib`
- `cargo test acp_ --lib -- --test-threads=1`
- `cargo test --lib -- --test-threads=1`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

ACP now has a first-order shell terminal push subscription. MCP terminal
progress is covered separately by `exec_shell_terminal_events` plus
`notifications/progress`. Remaining shell-supervisor parity is broader
service-manager lifecycle coverage and Windows ConPTY.
