# DeepSeek-TUI Shell Detached Status Refresh

Date: 2026-05-13

Status: completed

## Gap

Detached shell records were inspectable and cancelable, but a durable manifest
could remain `running` forever if the owning process exited before it refreshed
the final shell status. That made `exec_shell_list`, `exec_shell_show`, and
`exec_shell_wait` over detached jobs less trustworthy.

## Spec

1. When loading a detached durable `running` shell record, probe the persisted
   pid before rendering or canceling it.
2. If the pid is no longer alive, update the manifest status to `exited`.
3. Preserve captured stdout/stderr logs and byte counts while updating status.
4. Keep the existing `killed` update when detached cancel succeeds.
5. Leave true exit-code recovery out of scope because a manifest cannot recover
   a reaped process status after the owner process is gone.

## Implementation

- Added `refresh_durable_running_status` and `detached_process_is_alive`.
- `exec_shell_list`, `exec_shell_show`, `exec_shell_wait`, and detached cancel
  now refresh stale `running` manifests before rendering or acting.
- Stale records use `status: exited` with `exit_code: null` because the final
  process status is no longer recoverable.

## Verification

- `/home/willamhou/.cargo/bin/cargo test exec_shell --lib`
- `/home/willamhou/.cargo/bin/cargo test serve --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `/home/willamhou/.cargo/bin/cargo check`
- `git diff --check`

## Remaining Gap

This is best-effort pid probing and can still be fooled by pid reuse. A full
solution needs a supervisor/PTY session owner that can report authoritative
exit status and expose an authenticated control channel.
