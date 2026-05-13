# DeepSeek-TUI RLM Live Turn Cancel

## Status

Implemented.

## Goal

Close the next live RLM control gap by allowing queued live `rlm_process`
turns to be cancelled before any worker claims them.

This intentionally does not implement cancellation for an active model worker.
That remains a separate streaming/worker-runtime slice.

## Behavior

- Add `rlm_process_cancel`.
- Require `session_id`.
- Accept one queued turn by `task_id`, `turn_id`, or `id`.
- Accept `all=true` to cancel every queued pending turn for the live session.
- Require either a specific task id or `all=true`.
- Only cancel runtime tasks that:
  - belong to the live session manifest's `runtime_thread_id`
  - have `kind == "rlm_process"`
  - have `status == "pending"`
- Reject specific tasks that are running, completed, failed, cancelled, from a
  different thread, or not `rlm_process`.
- Use `RuntimeStore::cancel_task` so runtime task status and thread cancel
  events remain consistent.
- Append a live RLM `turn_cancelled` event to
  `.dscode/rlm-daemon/<session_id>/events.jsonl`.
- Refresh `queued_turns` in
  `.dscode/rlm-daemon/<session_id>/manifest.json`.

## MCP/ACP

- Register `rlm_process_cancel` as a model-visible tool.
- Expose it through MCP only when durable runtime approvals are enabled.
- Classify it as an ACP `execute` tool because it mutates runtime state but
  does not edit workspace files.

## Verification

- Unit test covers:
  - enqueueing two live RLM turns
  - cancelling a specific queued turn
  - preserving the other pending turn
  - appending `turn_cancelled`
  - refreshing `queued_turns`
  - cancelling remaining queued turns with `all=true`
  - rejecting calls without `task_id`/`turn_id` or `all=true`
- Follow-on regression commands:
  - `cargo test rlm_process_cancel_cancels_queued_live_turns --lib`
  - `cargo test rlm_process --lib`
  - `cargo test build_tool_specs_include_rlm --lib`
  - `cargo test default_registry_includes_dispatch_subagent_only_below_max_depth --lib`
  - `cargo test serve --lib`
  - `cargo fmt --check`
  - `cargo check`
  - `git diff --check`

## Remaining Gap

The live daemon still needs native push/SSE streaming polish, forced
cross-process worker interruption, and richer lifecycle commands.
