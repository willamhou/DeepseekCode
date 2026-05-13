# DeepSeek-TUI Task Shell Tools Parity Spec

日期：2026-05-13

对比对象：`Hmbown/DeepSeek-TUI`，`main` HEAD `3382242`

## 背景

DeepSeek-TUI exposes `task_shell_start` and `task_shell_wait` as the preferred
names for long-running background shell commands. DeepSeekCode already supports
background shell work through `exec_shell background=true` plus
`exec_shell_wait`, but the DeepSeek-TUI names are not available.

## 目标

- Add agent-visible `task_shell_start` and `task_shell_wait`.
- `task_shell_start` accepts `command`, optional `cwd`, `timeout_ms`, `stdin`,
  and `tty`, starts a background shell job, and returns a `task_id`.
- `task_shell_wait` accepts `task_id`, optional `wait`, `timeout_ms`, `gate`,
  and `command`, and delegates to the existing background shell wait path.
- Reuse the existing shell safety, allowlist, approval, polling, and
  cancellation behavior.
- Expose both tools in the default registry and DeepSeek model schema.

## 非目标

- This slice does not persist background shell jobs across process exits. A
  later shell-job durable metadata slice adds detached manifest/log inspection;
  cross-process stdin/cancel takeover remains out of scope.
- This slice does not attach gate artifacts to runtime tasks.
- This slice does not implement PTY/TTY behavior beyond accepting the
  compatibility field.

## 验收标准

1. `task_shell_start command=<safe command>` starts a background job and returns
   a `task_id`.
2. `task_shell_wait task_id=<id>` can poll the started job.
3. Unsafe commands are rejected by the existing shell safety path.
4. `task_shell_start` uses shell approval classification.
5. The default registry exposes both tool names.
6. The model schema exposes both tool names.

## 实现结果

- Added `task_shell_start` and `task_shell_wait` in `src/tools/exec_shell.rs`.
  `task_shell_start` delegates to `exec_shell background=true`, preserves the
  existing shell safety and approval path, accepts the DeepSeek-TUI
  compatibility `tty` field, and rewrites the polling hint to
  `task_shell_wait`.
- Added `task_shell_wait` as an alias-style wrapper around the existing
  background shell wait path, with optional `gate` and `command` compatibility
  metadata.
- Registered both tools in the default registry and shell approval
  classification in `src/tools/registry.rs`.
- Added model schemas for both tool names in `src/model/deepseek.rs`.
- Documented the aliases in `docs/runtime.md` and the parity plan.
- Stabilized the related RLM file-input test by holding the existing cwd lock
  and reusing one cwd snapshot, after full-suite parallel execution exposed a
  global cwd race unrelated to the task-shell implementation.

## 验证

- `cargo test task_shell`: passed, 1 test.
- `cargo test build_tool_specs_include_exec_shell_background_tools`: passed.
- `cargo test default_registry_includes_exec_shell_background_tools`: passed.
- `cargo test default_registry_includes_read_only_git_history_tools`: passed.
- `cargo test rlm_process_input_reads_safe_workspace_relative_file -- --nocapture`:
  passed after the cwd-race test fix.
- `cargo test`: passed, 982 tests.
- `cargo fmt --check`: passed.
- `git diff --check`: passed.
- `cargo package --allow-dirty`: passed; packaged 291 files and verified.
