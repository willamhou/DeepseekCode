# DeepSeek-TUI Shell Detached Control Diagnostics

Date: 2026-05-13

Status: completed

## Gap

Durable shell job metadata made detached `exec_shell_list`, `exec_shell_show`,
and `exec_shell_wait` useful after a process restart. However, attempting
stdin or cancel control against a detached-but-known durable task id still
looked the same as an entirely unknown task. That made the new boundary harder
for TUI and MCP clients to explain.

## Spec

1. Teach `exec_shell_interact` / `exec_interact` to accept optional `cwd` for
   detached durable record lookup.
2. Teach `exec_shell_cancel` to accept optional `cwd` for the same lookup.
3. If a task id is not attached to the current process but has a durable
   manifest under `<cwd>/.dscode/shell-jobs/<task_id>/`, return an explicit
   detached-control diagnostic.
4. Preserve the normal unknown-task error when no active job or durable record
   exists.
5. Preserve live attached stdin and cancel behavior.

## Implementation

- Added `durable_shell_job_exists` for cheap manifest detection.
- Added `detached_or_unknown_shell_task_error` to share the control-boundary
  message across stdin and cancel paths.
- `exec_shell_interact` now forwards `cwd` into the follow-up wait call.
- The existing durable shell job test now verifies detached show/wait plus
  detached stdin/cancel diagnostics.

## Verification

- `/home/willamhou/.cargo/bin/cargo test exec_shell --lib`
- `/home/willamhou/.cargo/bin/cargo test serve --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `/home/willamhou/.cargo/bin/cargo check`
- `git diff --check`
