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
   - no native PTY yet
2. Native Unix PTY backend:
   - `openpty`/`fork` or equivalent FFI
   - supervisor-owned master fd
   - child process group
   - terminal event log writer
3. Replay and attach:
   - `stream=terminal`
   - event cursor support
   - MCP/ACP schema exposure
4. Resize:
   - `exec_shell_resize`
   - `TIOCSWINSZ`
   - `SIGWINCH`
   - tests that verify `stty size` changes inside a running PTY
5. Owner-exit integration:
   - integration test that starts a supervised job through a short-lived CLI
     process, exits the owner, then shows/waits/cancels through a new process
6. Service packaging:
   - systemd/launchd templates can supervise the shell supervisor alongside
     runtime and diagnostics services
7. Windows ConPTY:
   - separate platform design and tests

## Verification Plan

Future implementation should add these gates:

- `cargo test exec_shell_supervisor_protocol --lib`
- `cargo test exec_shell_supervisor_replay_terminal_events --lib`
- `cargo test exec_shell_supervisor_resize_updates_tty_size --lib`
- `cargo test exec_shell_supervisor_survives_owner_exit --test integration`
- `cargo test serve --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Current Decision

Do not add fake live resize or fake attach on top of the `script` backend. The
next shell implementation slice should start with the supervisor protocol
skeleton or the native Unix PTY backend. Until then, docs and tool outputs must
continue to describe current PTY support as `script` execution with initial
geometry, durable logs, FIFO stdin, and best-effort detached process control.
