# DeepSeek-TUI Shell Cancel-All Detached Slice

## Context

DeepSeek-TUI exposes `exec_shell_cancel all=true` for bulk cancellation of
running background shell tasks. DeepSeekCode already supported that for jobs
owned by the current process, and also had durable detached shell records, but
`all=true` did not scan those detached records. After a process restart or a
different client attach, bulk cancel could miss still-running shell work.

## Implemented

- `exec_shell_cancel all=true cwd=<path>` now cancels current-process managed
  jobs and detached durable `running` records under
  `<cwd>/.dscode/shell-jobs/`.
- Detached jobs are cancelled through the existing persisted pid/process-group
  path and their manifests are updated to `status="killed"`.
- The summary distinguishes `managed` and `detached` task ids so operators can
  see what was cancelled.

## Verification

- `cargo test exec_shell_cancel --lib`
- `cargo test exec_shell --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining Gap

This is still best-effort process-group cancellation. Native supervisor-owned
PTY sessions will need stronger session ownership and terminal event cleanup.
