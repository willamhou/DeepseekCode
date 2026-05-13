# DeepSeek-TUI Exec Shell Background Parity Spec

日期：2026-05-13

对比对象：`Hmbown/DeepSeek-TUI`，`main` HEAD `3382242`

## 背景

DeepSeek-TUI exposes shell execution as `exec_shell`, with `background=true`
for long-running commands plus `exec_shell_wait`, `exec_shell_interact`, and
`exec_shell_cancel` for polling, stdin, and cancellation. It also exposes
`exec_wait` / `exec_interact` aliases.

DeepSeekCode already has a safe synchronous `run_shell` tool and cooperative
cancellation, but DeepSeek-TUI-style prompts that call `exec_shell` or expect a
background task id still miss the registry/schema surface.

## 目标

- Add `exec_shell` as a safe shell tool alias that supports foreground execution
  through the existing `run_shell` path.
- Add `background=true` support that returns a `task_id` immediately.
- Add `exec_shell_wait` / `exec_wait` for bounded output polling and completion
  checks.
- Add `exec_shell_interact` / `exec_interact` for stdin writes plus output
  polling.
- Add `exec_shell_cancel` for cancelling a specific running background shell
  job or all running jobs.
- Reuse existing safe-command and approval policy semantics.

## 非目标

- This slice does not add PTY mode.
- This slice does not make background shell jobs durable across process exits.
  A later shell-job durable metadata slice adds detached manifest/log
  inspection; cross-process stdin/cancel takeover remains out of scope.
- This slice does not add full TUI task-panel rendering for shell jobs.

## 验收标准

1. `exec_shell` without `background=true` behaves like `run_shell`.
2. `exec_shell background=true` returns a `task_id` and does not block until
   completion.
3. `exec_shell_wait` can poll a running job and later report completion with
   stdout/stderr deltas.
4. `exec_shell_interact` can send stdin to a running job.
5. `exec_shell_cancel` can kill a running job.
6. Registry permission requests treat `exec_shell` as shell execution, and
   schemas expose the DeepSeek-TUI-compatible names.

## 实现结果

- `src/tools/exec_shell.rs` adds `exec_shell`, `exec_shell_wait` /
  `exec_wait`, `exec_shell_interact` / `exec_interact`, and
  `exec_shell_cancel`.
- Foreground `exec_shell` delegates to the existing safe `run_shell` path.
- `background=true` starts an in-process tracked shell job with piped
  stdout/stderr/stdin and returns a `task_id`.
- Wait/interact/cancel tools poll deltas, write stdin, and kill process groups.
- `src/tools/registry.rs` registers the new tools and treats `exec_shell` as a
  shell permission request.
- `src/model/deepseek.rs` exposes the DeepSeek-TUI-compatible schemas, and
  `docs/runtime.md` documents the non-durable background-job limitation.

## 验证

- `cargo test exec_shell`
- `cargo test build_tool_specs_include_exec_shell_background_tools`
- `cargo test default_registry_includes_exec_shell_background_tools`
- `cargo fmt --check`
- `git diff --check`
- `cargo test`（908 passed）
- `cargo package --allow-dirty`
