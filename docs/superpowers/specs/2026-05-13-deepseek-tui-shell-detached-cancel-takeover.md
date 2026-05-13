# DeepSeek-TUI Shell Detached Cancel Takeover

Date: 2026-05-13

Status: completed

## Gap

Durable shell records made detached background jobs inspectable, but canceling
a detached `running` job still required the original DeepSeekCode process to be
alive. That left a practical gap for long-running shell work that survives a
TUI or MCP server restart.

## Spec

1. Let `exec_shell_cancel cwd=<path> task_id=<id>` load a detached durable
   manifest when the job is not attached to the current process.
2. If the durable record is not `running`, return a non-error summary with the
   last known status.
3. If the durable record is `running` and has a safe recorded pid, best-effort
   kill its Unix process group, falling back to the process pid.
4. After a successful detached cancel, update the manifest status to `killed`
   and refresh its timestamp/byte counts.
5. Keep detached stdin unavailable; stdin still needs the original process
   pipe.

## Implementation

- Added `cancel_detached_shell_job` to the `exec_shell_cancel` fallback path.
- Added `write_durable_shell_job_manifest` so detached control paths can update
  persisted state without an in-memory `BackgroundShellJob`.
- Added `kill_detached_process_group` with Unix process-group cancellation and
  basic pid safety checks.
- Kept unknown task ids as unknown and non-running durable records as
  non-error status reports.
- A later detached status refresh slice probes stale `running` manifests before
  rendering or canceling them, so already-exited detached records do not stay
  permanently `running`.

## Verification

- `/home/willamhou/.cargo/bin/cargo test exec_shell --lib`
- `/home/willamhou/.cargo/bin/cargo test serve --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `/home/willamhou/.cargo/bin/cargo check`
- `git diff --check`

## Remaining Gap

Detached stdin and PTY takeover remain out of scope because the original stdin
pipe is not recoverable from a manifest alone. A stronger solution would need a
real shell supervisor or PTY/session daemon with an authenticated control
channel.
