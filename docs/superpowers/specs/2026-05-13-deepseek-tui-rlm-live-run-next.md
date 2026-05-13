# DeepSeek-TUI RLM Live Run Next

## Status

Implemented.

## Goal

Narrow the live RLM daemon execution gap by adding a single-step worker bridge
that can claim one queued live `rlm_process` turn, reconstruct it from the
persisted payload, run the bounded model-backed child flow, and record the
result.

This is not a resident daemon yet. It is the smallest execution primitive that
future service packaging can loop over.

## Behavior

- Add `rlm_process_run_next`.
- Require `session_id`.
- Select the oldest queued pending `rlm_process` task for the live session, or
  accept `task_id` / `turn_id` / `id` for a specific queued turn.
- Load `.dscode/rlm-daemon/<session_id>/turns/<task-id>.json`.
- Reject payloads that are not `queued` or that point at a different runtime
  thread.
- With `dry_run=true`, return the selected payload and rendered child task
  without claiming or running anything.
- Without `dry_run`:
  - claim the runtime task
  - mark the payload `running`
  - update the live manifest to `status=running` and set `active_turn_id`
  - append `turn_started`
  - run the bounded child-agent RLM task using the persisted task/input/steps
  - mark the runtime task and payload `completed` or `failed`
  - append `turn_completed` or `turn_failed`
  - refresh `queued_turns` and clear `active_turn_id` after completion/failure

## MCP/ACP

- Register `rlm_process_run_next` as a model-visible tool.
- Expose it through MCP only with trusted side effects or durable runtime
  approvals because it can spend model tokens and mutates runtime state.
- Classify it as an ACP `execute` tool.

## Verification

- `rlm_process_run_next_dry_run_loads_oldest_payload` verifies FIFO selection,
  payload loading, child-task rendering, step preservation, and non-mutating
  dry-run behavior.
- Regression commands:
  - `cargo test rlm_process --lib`
  - `cargo test build_tool_specs_include_rlm --lib`
  - `cargo test default_registry_includes_dispatch_subagent_only_below_max_depth --lib`
  - `cargo test serve --lib`
  - `cargo fmt --check`
  - `cargo check`
  - `git diff --check`

## Remaining Gap

DeepSeekCode still needs TUI/ACP subscription polish and supervisor/CLI
lifecycle commands.
