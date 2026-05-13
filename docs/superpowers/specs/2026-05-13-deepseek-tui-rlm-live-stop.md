# DeepSeek-TUI RLM Live Stop

## Status

Implemented.

## Goal

Add an explicit lifecycle command for closing an idle live `rlm_process`
session.

## Behavior

- Add `rlm_process_stop`.
- Require `session_id`.
- Refuse to stop a session whose `active_turn_id` still points at a running
  runtime task or running payload.
- Cancel queued pending turns, mark their payloads cancelled, and append
  `turn_cancelled` records.
- Rewrite the live manifest with `status=stopped`, no active turn, and refreshed
  `queued_turns`.
- Append `session_stopped`.
- Block accidental reuse of a stopped live session; `rlm_process live=true`
  requires `reset=true` to start again with the same `session_id`.

## MCP/ACP

- Register `rlm_process_stop` as a model-visible tool.
- Expose it through MCP only with durable runtime approvals because it mutates
  runtime tasks, live payloads, and the live manifest.
- Classify it as an ACP `execute` tool.

## Verification

- `rlm_process_stop_cancels_queue_and_blocks_reuse_until_reset` verifies queued
  cancellation, stopped manifest state, `session_stopped` replay, stopped
  session reuse rejection, and reset-based restart.
- Regression commands:
  - `cargo test rlm_process --lib`
  - `cargo test build_tool_specs_include_rlm --lib`
  - `cargo test default_registry_includes_dispatch_subagent_only_below_max_depth --lib`
  - `cargo test serve --lib`
  - `cargo fmt --check`
  - `cargo check`
  - `git diff --check`

## Remaining Gap

DeepSeekCode still needs ACP-specific push subscriptions. Daemon
package/service UX is now covered by generated agents-daemon service
templates.
