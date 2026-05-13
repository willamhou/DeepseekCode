# DeepSeek-TUI RLM Active Worker Cancel

Date: 2026-05-14

Status: implemented

## Gap

Live `rlm_process` sessions could cancel queued turns, but once
`rlm_process_run_next` claimed a turn the live manifest stayed owned by that
worker until completion, failure, or manual recovery. That left no normal
DeepSeek-TUI-style path for a client/operator to request cancellation of an
active live RLM worker.

## Spec

- Extend `dispatch_subagent` with an internal cancel-aware execution path while
  preserving the existing public tool behavior for normal subagent dispatch.
- Add a live RLM runtime-task cancel check that reads the claimed task status
  from `RuntimeStore`.
- Pass that cancel check from `rlm_process_run_next` into the child agent loop.
- Allow `rlm_process_cancel` to target pending queued or running active
  `rlm_process` tasks that belong to the live session runtime thread.
- When cancelling an active turn, mark the runtime task and payload
  `cancelled`, append `turn_cancelled`, refresh `queued_turns`, and preserve
  the manifest `active_turn_id` plus daemon pid/epoch until the worker observes
  the cancellation.
- When the worker observes cancellation, return `status=cancelled`, keep the
  runtime task/payload cancelled, append a cancellation event, and clear the
  live manifest owner back to idle.
- Keep forced cross-process termination out of scope; this slice is cooperative
  cancellation through the existing agent cancel path.

## Verification

- `cargo test rlm_process_cancel --lib`
- `cargo test rlm_live_task_cancel_check --lib`
- `cargo test rlm_process --lib`
- `cargo test build_tool_specs_include_rlm --lib`
- `cargo test serve --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

Forced cross-process interruption is implemented separately in
`2026-05-14-deepseek-tui-rlm-force-worker-interrupt.md`. TUI/ACP subscription
ACP-specific push subscriptions remain open; daemon package/service UX is now
covered by generated agents-daemon service templates.
