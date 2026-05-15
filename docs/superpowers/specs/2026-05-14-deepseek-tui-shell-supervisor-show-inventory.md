# DeepSeek-TUI parity: shell supervisor show inventory

Status: implemented
Date: 2026-05-14

## Gap

DeepSeek-TUI presents shell work as an inspectable job center. DeepSeekCode had
a workspace-local shell supervisor protocol skeleton with a supported `show`
method name, but `show` returned the same generic protocol status as `health`
and `status`. Clients could not use the supervisor protocol itself to inspect
durable shell jobs.

## Implementation

- `deepseek agents shell-supervisor --json` still exposes only the current
  newline-JSON protocol skeleton and does not claim native PTY ownership.
- Supported `show` responses now include a `job_inventory` string rendered by
  `ExecShellListTool` for the request cwd.
- If inventory rendering fails, the response remains a supported `show`
  response and includes `job_inventory_error` for diagnostics.
- At this slice, native PTY control methods remained structured unsupported
  responses. Later shell-supervisor start/control slices promote `start`,
  `wait`, `replay`, `attach`, `stdin`, `resize`, and `cancel` to supported
  daemon methods.

## Verification

- `cargo test shell_supervisor_protocol_show_includes_job_inventory --lib`
- `cargo test shell_supervisor --lib`
- `cargo check`
- `cargo fmt --check`
- `cargo test --lib -- --test-threads=1`
- `git diff --check`

## Remaining Gap

This closed the read-only supervisor job-inventory slice. Later slices added
supervisor process start, attach, stdin, resize, replay, wait, and cancellation
bridges; the remaining boundary is full interactive terminal takeover and
broader platform proof.
