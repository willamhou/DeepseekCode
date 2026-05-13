# DeepSeek-TUI RLM Live Drain

## Status

Implemented.

## Goal

Narrow the live RLM daemon-loop gap by adding a bounded batch worker primitive
that drains queued live `rlm_process` turns in FIFO order.

This is still not a resident daemon. It reuses the single-step
`rlm_process_run_next` execution path so future service packaging can loop over
one tested claiming/completion primitive instead of duplicating behavior.

## Behavior

- Add `rlm_process_drain`.
- Require `session_id`.
- Accept `max_turns`, defaulting to 10 and clamped to 1-100.
- Select pending live RLM runtime tasks for the session's runtime thread in
  FIFO order by `created_at`, then task id.
- With `dry_run=true`, return the selected task ids, status, creation time, and
  summary without claiming or running anything.
- Without `dry_run`, call `rlm_process_run_next` once per selected task id.
- Return `ran_count`, remaining `queued_turns`, and each child run summary.

## MCP/ACP

- Register `rlm_process_drain` as a model-visible tool.
- Expose it through MCP only with trusted side effects or durable runtime
  approvals because it can spend model tokens and mutates runtime state.
- Classify it as an ACP `execute` tool.

## Verification

- `rlm_process_drain_dry_run_lists_fifo_batch` verifies FIFO selection,
  `max_turns` limiting, non-mutating dry-run behavior, and 1-100 clamping.
- Regression commands:
  - `cargo test rlm_process --lib`
  - `cargo test build_tool_specs_include_rlm --lib`
  - `cargo test default_registry_includes_dispatch_subagent_only_below_max_depth --lib`
  - `cargo test serve --lib`
  - `cargo fmt --check`
  - `cargo check`
  - `git diff --check`

## Remaining Gap

DeepSeekCode still needs native push/SSE streaming polish, forced cross-process
worker interruption, and supervisor/CLI lifecycle commands.
