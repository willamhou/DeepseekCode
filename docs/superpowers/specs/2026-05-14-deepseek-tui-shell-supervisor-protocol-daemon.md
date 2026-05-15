# DeepSeek-TUI Shell Supervisor Protocol Daemon Slice

## Context

DeepSeek-TUI keeps terminal work inside a durable TUI process. DeepSeekCode now
has detached shell logs, attach snapshots, and a supervisor manifest/status
schema, but the workspace supervisor process was still only a planned contract.
This slice adds a real daemon entrypoint and service surface without claiming
native supervisor-owned PTY support yet.

## Implemented

- Added `deepseek agents shell-supervisor [--once] [--json]`.
- On Unix, non-`--once` mode creates `.dscode/shell-supervisor`, binds
  `supervisor.sock`, writes `manifest.json`, and serves one newline-JSON
  request per connection.
- Supported methods are `health`, `status`, `show`, and `shutdown`.
- Unsupported methods return `status="unsupported"` with `pty_backend="none"`
  and `native_pty=false`, preserving the boundary before native PTY sessions.
- The supervisor manifest and status output distinguish supported protocol
  methods from unsupported future PTY methods.
- `exec_shell_supervisor_status` performs a bounded `health` round-trip when a
  socket exists, so an alive pid plus stale socket cannot report `ready`.
- Malformed protocol requests return structured error responses without stopping
  the daemon.
- `deepseek agents service` now renders systemd/launchd shell-supervisor service
  templates, and `deepseek update package` includes static packaged templates.

## Verification

- `cargo test cli_from_argv_routes_agents_subcommands --lib`
- `cargo test shell_supervisor --lib`
- `cargo test exec_shell_supervisor_status --lib`
- `cargo test service_templates_render_runtime_and_agent_supervisors --lib`
- `cargo test create_release_package_copies_binary_and_writes_scripts --lib`
- `cargo run -- agents shell-supervisor --once --json`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining Gap

At this slice, the daemon was intentionally a protocol skeleton. Later
shell-supervisor start/control slices added durable `start`, `wait`, `replay`,
`attach`, `stdin`, `resize`, and `cancel` methods, plus native-supervisor PTY
jobs on supported Unix/Linux builds. The remaining shell gap is full
interactive terminal takeover and broader platform proof.
