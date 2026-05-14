# DeepSeek-TUI parity: shell supervisor status show probe

Status: implemented
Date: 2026-05-14

## Gap

The shell supervisor daemon can answer `show` with durable job inventory, but
`exec_shell_supervisor_status` only probed `health`. Operator-facing status
therefore proved the socket was responsive without proving the job-center
protocol path worked. A follow-up slice also probes `status` so the tool proves
the daemon's read-only lifecycle dashboard path before showing job inventory.

## Implementation

- `exec_shell_supervisor_status` keeps the bounded `health` request as the
  readiness signal.
- When `health` returns `ok`, it opens bounded protocol requests for `status`
  and `show`.
- The status summary now includes `protocol_status`,
  `protocol_status_active_jobs`, `protocol_show`, and a `protocol_job_inventory`
  block populated from `job_inventory` when available.
- If `status` fails or returns an unexpected response, the failure is reported
  in `protocol_status` without changing the native PTY boundary.
- If `show` fails or returns an unexpected response, the failure is reported in
  `protocol_show` without changing the native PTY boundary.

## Verification

- `cargo test exec_shell_supervisor_status_probes_read_only_protocol_methods --lib`
- `cargo test shell_supervisor --lib`
- `cargo check`
- `cargo fmt --check`
- `cargo test --lib -- --test-threads=1`
- `git diff --check`

## Remaining Gap

This proves the read-only daemon job-center path. It does not implement native
supervisor-owned PTY process start, attach, stdin, resize, replay, wait, or
cancel.
