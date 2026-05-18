# DeepSeek-TUI Human Shell Control CLI

Date: 2026-05-14

Status: first slice implemented

## Context

DeepSeekCode already had shell-supervisor protocol methods and tool-level
control for durable shell jobs, including Linux native PTY jobs. The remaining
usability gap was a human-facing CLI entry point for those protocol controls.

## Implementation

- Added `deepseek agents shell ...` as a thin CLI wrapper around the
  workspace-local shell supervisor socket.
- Supported actions:
  - `status`
  - `show`
  - `start`
  - `wait`
  - `replay`
  - `attach`
  - `stdin` / `send`
  - `resize`
  - `cancel`
  - `shutdown`
- `start` accepts `--tty`, `--cwd`, `--rows`, `--cols`, `--json`, and command
  arguments after `--`.
- `resize` accepts `--rows` / `--cols` or positional `rows cols`.
- `stdin` accepts `--input`, `--close-stdin`, and `--timeout-ms`.
- `attach` accepts `--follow`, `--poll-ms`, and `--max-ms` for a human-facing
  terminal follow loop that prints only new terminal payloads while the job is
  still running.
- Non-JSON output prints the relevant supervisor summary; `--json` prints the
  raw protocol response. In `--follow --json` mode, each follow iteration is
  printed as newline-delimited protocol JSON.
- Shell completions now include both `shell` and `shell-supervisor` under
  `agents`.
- Release service documentation now points operators to `deepseek agents shell
  ...` for human protocol control.

## Verification

- `cargo test cli_from_argv_routes_agents_subcommands --lib`
- `cargo test agents_shell_cli_args_build_protocol_requests --lib`
- `cargo test agents_shell_attach_follow_parses_cursor_status_and_payload --lib`
- `cargo test shell_supervisor_protocol --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Residual

This is still not a full-screen terminal takeover: `--follow` is a bounded
cursor-following stream over durable attach snapshots. Remaining
shell-supervisor parity work is actual installed systemd/launchd service smoke
evidence and Windows ConPTY. HTTP shell terminal SSE, ACP
`session/shell/subscribe`, and MCP `exec_shell_terminal_events` progress
notifications now cover protocol terminal event consumption.
