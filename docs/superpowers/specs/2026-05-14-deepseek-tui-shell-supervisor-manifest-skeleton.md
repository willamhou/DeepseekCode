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
- Added read-only `exec_shell_supervisor_status` to inspect the planned
  workspace-local `.dscode/shell-supervisor` manifest/socket state and protocol
  method names without launching a supervisor.
- Normal tool output deliberately does not print `control_token_hash`.
- Runtime docs and the DeepSeek-TUI parity plan documented the boundary for
  this slice: it only added manifest/protocol metadata. Later
  shell-supervisor start/control slices add native-supervisor PTY jobs on
  supported Unix/Linux builds.

## Verification

- `cargo test exec_shell_supervisor_status --lib`
- `cargo test exec_shell --lib`
- `cargo test default_registry_includes_exec_shell_background_tools --lib`
- `cargo test build_tool_specs_include_exec_shell_background_tools --lib`
- `cargo test mcp_tools_list --lib`
- `cargo test serve --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

This reduces ambiguity and prepares the schema for supervisor-owned PTYs, but it
does not start a workspace shell supervisor, open a native PTY, stream terminal
events, resize with `TIOCSWINSZ`, or prove owner-process-independent survival.
Those remain in the shell supervisor PTY design.
