# DeepSeek-TUI Shell Supervisor Manifest Skeleton

Status: implemented

## Gap

The shell supervisor PTY design requires future native-supervisor jobs to carry
explicit backend and capability metadata. Before this slice, durable shell
records only preserved `tty` and a derived `pty_backend` label. That made all
durable TTY jobs render as `script`, and there was no stable place to expose
whether a job is truly attachable or resizable through a supervisor-owned PTY.

This was a problem even before the native PTY backend exists: terminal clients
need to distinguish today's durable stdout attach replay from future live PTY
takeover, and future supervisor manifests must not be downgraded by the existing
show/list/refresh code.

## Implementation

- Extended durable shell manifests with the supervisor capability skeleton:
  `attachable`, `resizable`, `supervisor_pid`, `supervisor_socket`,
  `supervisor_epoch`, `terminal_event_log`, `terminal_event_seq`, and preserved
  `control_token_hash`.
- Current plain-pipe and Unix `script` jobs write `attachable=false` and
  `resizable=false`.
- Durable show, list, wait, and attach output now render backend and supervisor
  capability fields explicitly.
- Future `pty_backend="native-supervisor"` manifests are read and rendered as
  `native-supervisor` instead of being derived back to `script`.
- Normal tool output deliberately does not print `control_token_hash`.
- Runtime docs and the DeepSeek-TUI parity plan document the boundary: this is
  a manifest/protocol skeleton, not native PTY ownership.

## Verification

- `cargo test exec_shell --lib`

## Remaining

This reduces ambiguity and prepares the schema for supervisor-owned PTYs, but it
does not start a workspace shell supervisor, open a native PTY, stream terminal
events, resize with `TIOCSWINSZ`, or prove owner-process-independent survival.
Those remain in the shell supervisor PTY design.
