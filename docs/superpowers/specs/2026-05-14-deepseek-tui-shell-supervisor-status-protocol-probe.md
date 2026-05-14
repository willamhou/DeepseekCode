# DeepSeek-TUI parity: shell supervisor status protocol probe

Status: implemented
Date: 2026-05-14

## Gap

The workspace shell supervisor daemon supports the read-only `status` protocol
method, but `exec_shell_supervisor_status` only validated socket `health` and
daemon `show` inventory. That left the daemon lifecycle/status endpoint
untested from the operator-facing model tool.

## Implementation

- Keep `health` as the readiness gate.
- When `health` returns `ok`, send a bounded `status` request before `show`.
- Render `protocol_status` and `protocol_status_active_jobs` in the status
  summary.
- Keep native PTY methods outside this slice; unsupported supervisor-owned PTY
  methods still return structured `unsupported` responses.

## Verification

- `cargo test exec_shell_supervisor_status_probes_read_only_protocol_methods --lib`
- `cargo test shell_supervisor --lib`
- `cargo check`
- `cargo fmt --check`
- `cargo test --lib -- --test-threads=1`
- `git diff --check`

## Remaining Gap

This closes read-only `health` / `status` / `show` protocol observability. It
does not implement native supervisor-owned PTY process start, attach, stdin,
resize, replay, wait, or cancel.
