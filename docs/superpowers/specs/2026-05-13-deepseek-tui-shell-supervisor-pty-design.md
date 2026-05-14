# DeepSeek-TUI Shell Supervisor PTY Design

Date: 2026-05-13

Status: designed

## Gap

DeepSeekCode now supports DeepSeek-TUI-compatible shell names, durable shell
manifests/logs, detached status refresh, detached cancel, Unix FIFO stdin,
`tty=true` execution through `script`, initial PTY geometry, durable log replay,
and owner/process-group metadata. The remaining shell gap is not another
manifest field. It is a real supervisor that owns PTY file descriptors after the
starting CLI process exits.

The current `script` backend is useful for commands that require a TTY, but it
does not expose the PTY master fd to DeepSeekCode. That means DeepSeekCode
cannot honestly implement live resize, raw terminal attach, cursor-preserving
terminal replay, or authoritative PTY lifecycle ownership on top of the current
backend alone.

## Success Criteria

- A supervised shell session survives the original DeepSeekCode CLI process
  exiting.
- A supervisor-owned native PTY backend records enough terminal events for
  deterministic attach/replay from a byte or event cursor.
- Resize control updates the live PTY window size and signals the child process.
- Existing tool names remain compatible: `exec_shell`, `task_shell_start`,
  `exec_shell_wait`, `exec_shell_show`, `exec_shell_replay`,
  `exec_shell_interact`, and `exec_shell_cancel`.
- New attach/resize operations are explicit and fail with clear diagnostics
  when the active backend is still `script` or plain pipes.
- Side-effect policy remains unchanged: shell start, stdin, resize, attach-live,
  and cancel require the same trusted execution or durable approval gates as
  current mutating shell tools.

## Proposed Architecture

### Process Model

- Add a workspace-local shell supervisor process, launched by TUI, `serve`, or
  `agents service` when supervised PTY sessions are requested.
- Store supervisor state under `.dscode/shell-supervisor/`.
- Keep per-job durable records under the existing
  `.dscode/shell-jobs/<task_id>/` layout so current detached show/wait/list
  code has a stable migration path.
- The supervisor, not the initial CLI, owns:
  - native PTY master fd
  - child process group
  - terminal event writer
  - stdin/control socket
  - resize handling

### Manifest Additions

For supervisor-backed sessions, extend `manifest.json` with nullable fields:

- `supervisor_pid`
- `supervisor_socket`
- `supervisor_epoch`
- `pty_backend: "native-supervisor"`
- `terminal_event_log`
- `terminal_event_seq`
- `control_token_hash`
- `attachable: true`
- `resizable: true`

Older records remain valid. Current `script` records continue to render
`attachable: false` and `resizable: false` if these fields are absent.

### Control Protocol

Use a Unix domain socket on Unix:

- socket path: `.dscode/shell-supervisor/supervisor.sock`
- permissions: `0600`
- request authentication: per-workspace random control token, stored hashed in
  supervisor state and never printed in normal tool output
- request/response encoding: newline-delimited JSON

Initial methods:

- `start`
- `show`
- `wait`
- `replay`
- `attach`
- `stdin`
- `resize`
- `cancel`
- `shutdown`

Windows support should be a later ConPTY-specific slice. Until then, Windows
must keep the existing non-supervised fallback behavior and return explicit
unsupported diagnostics for supervised PTY requests.

### Terminal Event Log

Add `.dscode/shell-jobs/<task_id>/terminal-events.jsonl` for supervisor-backed
PTY sessions. Each event has a monotonic `seq`, timestamp, and kind:

- `started`
- `output`
- `input`
- `resize`
- `status`
- `exit`
- `cancelled`

`output` payloads should store raw PTY bytes as base64 plus a display-safe
preview. `exec_shell_replay` can keep its current stdout/stderr byte mode for
old jobs and add `stream=terminal` for supervisor jobs.

### Attach Contract

Attach is an API-level terminal stream, not a full UI widget:

- `exec_shell_attach task_id=<id> cursor=<seq>` replays terminal events from the
  cursor and returns `next_cursor`.
- In MCP/ACP or HTTP modes, attach can optionally keep the request open and
  stream event frames.
- In local TUI mode, the TUI consumes the same event stream and renders an
  attachable terminal pane.

### Resize Contract

`exec_shell_resize task_id=<id> tty_rows=<n> tty_cols=<n>`:

- requires a running supervisor-backed PTY session
- updates the PTY window size with `TIOCSWINSZ`
- sends `SIGWINCH` to the child process group on Unix
- persists a `resize` terminal event
- updates manifest `tty_rows` and `tty_cols`
- returns a clear diagnostic for non-PTY, completed, stale, detached-old, and
  `script` backend jobs

## Implementation Slices

1. Supervisor protocol skeleton:
   - workspace-local socket
   - health/show methods
   - manifest fields
   - status: landed
2. Native Unix PTY backend:
   - Linux `posix_openpt`/`setsid`/`TIOCSCTTY` FFI
   - supervisor-owned master fd for `deepseek agents shell-supervisor`
     `tty=true` starts
   - child process group
   - terminal event log writer
   - status: first Linux slice landed
3. Replay and attach:
   - `stream=terminal`
   - event cursor support
   - MCP/ACP schema exposure remains open
4. Resize:
   - `exec_shell_resize`
   - `TIOCSWINSZ`
   - `SIGWINCH`
   - persisted `resize` terminal event
   - tests verify the native supervisor resize path, event log, and an
     end-to-end `stty size` assertion inside the child PTY after resize
5. Owner-exit integration:
   - integration test that starts the real shell-supervisor daemon, starts a
     supervised native PTY job through one socket connection, drops that
     connection, then replays/resizes/attaches/cancels through fresh socket
     connections
   - detached tool-level stdin/resize/cancel now forward through
     `supervisor_socket` for running native-supervisor manifests
   - status: daemon/socket owner-exit smoke, detached tool forwarding, and
     child-observed resize verification landed
6. Human CLI wrapper:
   - `deepseek agents shell ...` forwards status/show/start/wait/replay/attach/
     stdin/resize/cancel/shutdown requests to the workspace supervisor socket
   - non-JSON mode prints relevant tool summaries; `--json` prints raw protocol
     responses
   - status: first slice landed
7. Service packaging:
   - systemd/launchd templates can supervise the shell supervisor alongside
     runtime and diagnostics services
8. Windows ConPTY:
   - separate platform design and tests

## Verification Plan

Future implementation should add these gates:

- `cargo test exec_shell_supervisor_protocol --lib`
- `cargo test exec_shell_supervisor_replay_terminal_events --lib`
- `cargo test exec_shell_supervisor_resize_updates_tty_size --lib`
- `cargo test --test shell_supervisor_owner_exit`
- `cargo test serve --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Current Decision

Do not add fake live resize or fake attach on top of the `script` backend.
The supervisor protocol skeleton, terminal event replay/attach plumbing, and
the first Linux `native-supervisor` PTY backend have landed. Normal
`exec_shell tty=true` still uses `script`; shell-supervisor `tty=true` starts
own a native PTY master, write `terminal-events.jsonl`, and support live
`TIOCSWINSZ` resize through the in-process supervisor. Remaining hard slices
are MCP-side attach push/progress beyond the HTTP shell terminal SSE endpoint
and ACP `session/shell/subscribe`, service-manager lifecycle coverage, and
Windows ConPTY.
