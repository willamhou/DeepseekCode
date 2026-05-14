# DeepSeek-TUI parity: shell supervisor active job count

Status: implemented
Date: 2026-05-14

## Gap

The shell supervisor daemon's read-only `status` response exposed
`active_jobs`, but the field was still a placeholder `0`. After
`exec_shell_supervisor_status` started probing `status`, that made the operator
summary prove protocol reachability without proving useful job-center state.

## Implementation

- Add a narrow `count_active_durable_shell_jobs` helper that counts refreshed
  durable shell manifests with `status = "running"` without exposing private
  shell record internals.
- Use that count in shell supervisor protocol responses and in the startup
  manifest written by `deepseek agents shell-supervisor --json`.
- Preserve `active_jobs_error` in protocol responses if the durable job scan
  fails, while keeping unsupported native PTY methods structured as
  `unsupported`.

## Verification

- `cargo test shell_supervisor_protocol_status_counts_active_durable_jobs --lib`
- `cargo test shell_supervisor --lib`
- `cargo check`
- `cargo fmt --check`
- `cargo test --lib -- --test-threads=1`
- `git diff --check`

## Remaining Gap

This makes read-only supervisor status reflect durable job state. It still does
not implement supervisor-owned native PTY process start, attach, stdin, resize,
replay, wait, or cancel.
