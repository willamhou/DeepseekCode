# DeepSeek-TUI Native Supervisor PTY Backend

Date: 2026-05-14

Status: completed first Linux slice

## Context

DeepSeekCode already had durable shell jobs, FIFO stdin, `script`-backed TTY
execution, supervisor protocol routing, and terminal event replay. The missing
piece was a real PTY master owned by the long-running shell-supervisor process
instead of the short-lived command caller.

## Implementation

- Linux shell-supervisor `tty=true` starts now request `pty_backend:
  native-supervisor`.
- The native backend uses Unix FFI (`posix_openpt`, `grantpt`, `unlockpt`,
  `setsid`, `TIOCSCTTY`) without adding a new dependency.
- The supervisor process owns the PTY master fd and child process group.
- PTY bytes are copied into `stdout.log` for existing shell-log consumers and
  into `terminal-events.jsonl` as `started`, `output`, `input`, `resize`,
  `exit`, and `cancelled` events.
- Managed native jobs render and persist `attachable=true`, `resizable=true`,
  `supervisor_pid`, `supervisor_socket`, `supervisor_epoch`,
  `terminal_event_log`, and `terminal_event_seq`.
- `exec_shell_resize` uses `TIOCSWINSZ` and `SIGWINCH` for running native
  supervisor jobs, while ordinary `script` jobs keep the prior `stty` fallback.
- Ordinary `exec_shell tty=true` and `task_shell_start tty=true` outside the
  supervisor still use the conservative `script` backend.

## Verification

- `cargo test shell_supervisor_protocol_tty_start_records_native_pty_events --lib`
- `cargo test shell_supervisor_protocol_native_pty_resize_records_event --lib`
- `cargo test shell_supervisor_protocol --lib`
- `cargo test task_shell_start_tty_uses_script_pty_backend --lib`
- `cargo test exec_shell_replay_reads_terminal_event_log_by_cursor --lib`
- `cargo test exec_shell_resize_updates_running_tty_geometry --lib`
- `cargo fmt --check`
- `cargo check`

## Residual

This is not the final PTY parity endpoint. Still open:

- owner-exit integration against a separately launched supervisor daemon;
- streaming attach frames over MCP/ACP/HTTP instead of single-response replay;
- child-observed resize verification such as `stty size` from inside the PTY;
- detached native PTY control from a fresh process through the supervisor
  socket;
- Windows ConPTY.
