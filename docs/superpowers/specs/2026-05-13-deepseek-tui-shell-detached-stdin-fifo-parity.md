# DeepSeek-TUI Shell Detached Stdin FIFO Parity

Date: 2026-05-13

Status: implemented

## Gap

Background shell jobs already had DeepSeek-TUI-compatible start, poll, wait,
show, list, attached stdin, close-stdin, cancel, durable manifests/logs,
detached status refresh, and detached best-effort cancellation. The remaining
common shell gap was that a running job whose in-memory manager was gone could
be inspected and cancelled, but could not receive stdin.

## Spec

- Background shell stdout/stderr should be written directly to durable
  `<cwd>/.dscode/shell-jobs/<task_id>/stdout.log` and `stderr.log`, so logs do
  not depend on an in-process reader thread.
- On Unix, new background shell jobs should receive stdin from a durable FIFO
  at `<cwd>/.dscode/shell-jobs/<task_id>/stdin.fifo`.
- A small keeper process should keep the FIFO writer side open so the child
  does not see EOF until `close_stdin=true`.
- `exec_shell_interact cwd=<path> task_id=<id>` should write to the FIFO for a
  running detached job when the manifest records a live stdin path.
- `close_stdin=true` should close detached stdin by killing the keeper process
  recorded in the manifest.
- Older durable records without FIFO stdin should continue to return an
  explicit detached-stdin diagnostic instead of looking like unknown tasks.

## Implementation

- Replaced background shell stdout/stderr reader threads with direct durable
  log files.
- Added Unix FIFO stdin setup for new background jobs.
- Persisted optional `stdin_path`, `stdin_keeper_pid`, and `stdin_closed` fields
  in shell job manifests.
- Added detached `exec_shell_interact` support for FIFO-backed running jobs.
- Updated detached status refresh to mark zombie/exited child pids as `exited`
  and close the recorded stdin keeper.
- Kept non-Unix behavior on the existing attached process pipe path.

## Verification

- `cargo test exec_shell --lib`
- `cargo test serve --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining Gap

This is stdin takeover for FIFO-backed Unix jobs, not full PTY takeover. It
does not provide terminal resize, raw-mode TTY behavior, terminal replay, or a
dedicated supervisor daemon that owns process lifecycle independently of the
CLI. Those remain future shell-supervisor/PTY work.
