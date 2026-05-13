# DeepSeek-TUI RLM Live Turn Payload

## Status

Implemented.

## Goal

Make queued live `rlm_process` turns recoverable by persisting the full turn
payload next to the live session manifest. A later worker should be able to
claim a runtime task and reconstruct the exact long-input request without
depending on the original CLI process.

## Behavior

- When `rlm_process live=true` enqueues a runtime task, write:
  `.dscode/rlm-daemon/<session_id>/turns/<task-id>.json`
- Store:
  - payload kind/version
  - live `session_id`
  - `runtime_thread_id`
  - runtime `task_id`
  - status `queued`
  - original task text
  - steps/max-depth value
  - model
  - workspace
  - input label
  - input content
  - input character and line counts
  - created/updated timestamps
- For continuation calls that use only an existing `session_id`, persist an
  empty input content with label `live session context only`.
- When `rlm_process_cancel` cancels a queued turn, mark the payload
  `cancelled` and record `cancelled_at` / `cancel_reason` when the payload
  exists.

## Verification

- `rlm_process_live_enqueues_runtime_turn_and_manifest` now verifies that
  payload files are written for both fresh-input and session-only queued turns.
- `rlm_process_cancel_cancels_queued_live_turns` now verifies that cancellation
  updates the payload status and reason.

## Remaining Gap

`rlm_process_run_next` now consumes this payload for a single queued turn. The
remaining gap is native push/SSE streaming polish, forced cross-process worker
interruption, and lifecycle commands.
