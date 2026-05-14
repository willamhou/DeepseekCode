# DeepSeek-TUI Shell Terminal Event Replay

Date: 2026-05-14

Status: completed

## Context

The shell-supervisor PTY design reserves `terminal-events.jsonl` for
supervisor-owned PTY sessions. Before a native Unix PTY backend can be useful
to the TUI/MCP/ACP surfaces, existing shell tools need to understand terminal
event logs instead of only stdout/stderr byte logs.

## Implementation

- `exec_shell_replay` now accepts `stream=terminal` / `stream=events`.
- Terminal replay reads the durable `terminal_event_log` declared in a shell
  job manifest and uses `cursor` as an event sequence cursor.
- `exec_shell_attach` now switches to `terminal_event_attach` mode when a job
  has a terminal event log; older jobs still use durable stdout byte replay.
- `exec_shell_attach wait_ms=<n>` waits for new terminal events, completion,
  or timeout when the cursor is already caught up.
- Terminal event rendering supports `seq`, `kind`, optional `timestamp`/`ts`,
  and either `preview`, `data`, `text`, or structured resize/status fields.
- Shell-supervisor protocol replay requests now pass through the `cursor`
  argument.

## Verification

- `cargo test exec_shell_replay_reads_terminal_event_log_by_cursor --lib`
- `cargo test shell_supervisor_protocol --lib`

## Residual

This replay/attach plumbing now has a Linux native PTY producer, but streaming
terminal event consumption is now covered by HTTP runtime shell terminal SSE,
ACP `session/shell/subscribe`, and MCP `exec_shell_terminal_events` progress
notifications. Broader service-manager lifecycle coverage and Windows ConPTY
remain open implementation slices.
