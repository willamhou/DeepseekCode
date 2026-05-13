# DeepSeek-TUI RLM Force Worker Interrupt

Date: 2026-05-14

Status: implemented

## Gap

Active live RLM cancellation was cooperative. That is enough when
`rlm_process_run_next` is still polling the runtime task, but it does not cover
an operator takeover where the active worker is an external process and must be
interrupted explicitly.

## Spec

- Keep normal `rlm_process_cancel` cooperative by default.
- Add `force=true` as an explicit operator override.
- Only attempt forced interruption when the cancel request targets an active
  running turn.
- Read the active owner from the live manifest `daemon_pid`.
- Return `active_owner_cancelled` so operators can distinguish an active owner
  cancel from a non-owner running task cancel.
- Refuse unsafe pids, including pid `<= 1`, pids outside `i32`, and the current
  process pid.
- On Unix, send SIGTERM to a live external `daemon_pid`.
- If SIGTERM is accepted:
  - return `interrupted=true`
  - append `worker_interrupted`
  - clear `active_turn_id`, `daemon_pid`, and `daemon_epoch`
  - leave the runtime task and payload cancelled
- On unsupported platforms or non-live/stale owners, return structured
  interrupt diagnostics without changing the default cooperative cancel safety.

## Verification

- `cargo test rlm_process_cancel_force_interrupts_external_owner --lib`
- `cargo test rlm_process_cancel --lib`
- `cargo test rlm_process --lib`
- `cargo test build_tool_specs_include_rlm --lib`
- `cargo test serve --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

TUI/ACP subscription polish and stateful lifecycle CLI wrappers remain open.
