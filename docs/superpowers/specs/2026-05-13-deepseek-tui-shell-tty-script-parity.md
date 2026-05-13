# DeepSeek-TUI Shell TTY Script Parity

Date: 2026-05-13

Status: implemented

## Gap

DeepSeekCode background shell jobs had durable logs, detached inspection,
best-effort detached cancellation, and Unix FIFO detached stdin. They still
accepted `tty` only as compatibility metadata, so commands that check for a TTY
or adjust terminal behavior could not run through a PTY-backed path.

## Spec

- `exec_shell background=true tty=true` should request PTY-backed background
  execution instead of treating `tty` as inert metadata.
- `task_shell_start tty=true` should forward the request to the same background
  shell path and report `meta.tty=true`.
- On Unix, the implementation should use the existing `script` utility as the
  PTY backend when it is available.
- Foreground `exec_shell tty=true` should return a clear error because the
  safe foreground `run_shell` path is not PTY-backed.
- Tool specs and MCP schemas should advertise the `tty` option.
- Durable shell manifests and rendered snapshots should expose `tty` and
  `pty_backend`, so detached show/wait/list calls preserve the execution mode.
  A later slice adds persisted initial PTY geometry.

## Implementation

- Added a `tty` flag to background shell jobs and durable manifests.
- Routed `tty=true` background jobs through `script -q -f -e -c <command>
  /dev/null` with `TERM=xterm-256color`.
- Preserved the existing `sh -lc` path for non-TTY jobs.
- Added `tty` / `pty_backend` output to start, wait, show, list, and detached
  durable snapshots.
- A later slice adds `tty_rows` / `tty_cols` to set and persist initial PTY
  geometry.
- Forwarded `task_shell_start tty=true` into `exec_shell background=true
  tty=true`.
- Updated OpenAI-format tool specs and MCP tool definitions.

## Verification

- `/home/willamhou/.cargo/bin/cargo test task_shell_start_tty_uses_script_pty_backend --lib`
- `/home/willamhou/.cargo/bin/cargo test exec_shell --lib`
- `/home/willamhou/.cargo/bin/cargo test build_tool_specs_include_exec_shell_background_tools --lib`
- `/home/willamhou/.cargo/bin/cargo test serve --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `/home/willamhou/.cargo/bin/cargo check`
- `git diff --check`

## Remaining Gap

This narrows `tty=true` from inert compatibility metadata to a real PTY-backed
execution path for new Unix background jobs. It is still not a dedicated shell
supervisor: even with the later initial-geometry slice, there is no live
terminal resize control, replay protocol, attachable interactive terminal UI,
or independent daemon that owns PTY lifecycle after the original DeepSeekCode
process exits.
