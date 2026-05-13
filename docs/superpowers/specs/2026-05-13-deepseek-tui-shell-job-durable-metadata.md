# DeepSeek-TUI Shell Job Durable Metadata Parity

Date: 2026-05-13

Status: completed

## Gap

DeepSeek-TUI-style shell jobs could already run in the background, stream
output, accept stdin, and be cancelled while the owning DeepSeekCode process was
alive. Their job metadata and captured output were process-local, so restarting
or reconnecting through another surface lost the ability to inspect historical
stdout/stderr for a known `task_id`.

## Spec

1. Persist every `exec_shell background=true` and `task_shell_start` job under
   `<cwd>/.dscode/shell-jobs/<task_id>/`.
2. Store a JSON manifest with schema marker, task id, command, cwd, pid,
   status, exit code, timestamps, and stdout/stderr byte counts.
3. Append stdout and stderr chunks to durable `stdout.log` and `stderr.log`
   files while keeping the existing in-memory delta buffers.
4. Let `exec_shell_list cwd=<path>` merge live in-process jobs with durable
   detached records from that workspace.
5. Let `exec_shell_show` and `exec_shell_wait` fall back to a durable detached
   snapshot when a task id is not attached to the current process.
6. Make detached snapshots explicit with `managed: false` and a note that stdin
   and cancel control still require the original attached process.

## Implementation

- `BackgroundShellJob` now records durable timestamps and a per-task record
  directory.
- `persist_job_snapshot` writes `deepseek.exec_shell.job.v1` manifests on
  spawn, refresh, output render, stdin writes, and cancellation.
- Shell stdout/stderr reader threads append each chunk to the durable log files.
- `list_durable_shell_jobs` and `render_durable_snapshot` load detached records
  for `exec_shell_list`, `exec_shell_show`, and `exec_shell_wait`.

## Verification

- `/home/willamhou/.cargo/bin/cargo test exec_shell_background_writes_durable_record_for_detached_show --lib`
- `/home/willamhou/.cargo/bin/cargo test exec_shell --lib`
- `/home/willamhou/.cargo/bin/cargo test serve --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `/home/willamhou/.cargo/bin/cargo check`
- `git diff --check`

## Remaining Gap

This slice does not provide cross-process stdin or cancel takeover for a shell
job that is no longer attached to the current DeepSeekCode process. Detached
records are inspectable, but control remains process-local.
