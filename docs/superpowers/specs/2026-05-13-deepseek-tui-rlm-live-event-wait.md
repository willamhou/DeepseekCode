# DeepSeek-TUI RLM Live Event Wait

## Status

Implemented.

## Goal

Add a cursor-based wait/poll surface for live RLM event logs so clients do not
need to busy-loop `rlm_process_events` while waiting for queue, cancellation,
start, completion, or failure events.

## Behavior

- Add `rlm_process_wait`.
- Require `session_id`.
- Accept `cursor`, `after_seq`, or `since_seq`.
- Accept `limit`, using the same 1-500 clamp as `rlm_process_events`.
- Accept `timeout_ms`, defaulting to 1000 and clamped to 30000.
- Accept `poll_interval_ms`, defaulting to 100 and clamped to 25-1000.
- Return immediately when at least one event newer than the cursor exists.
- Return an empty event list when the timeout elapses.
- Preserve the same `next_cursor` contract as `rlm_process_events`.
- Missing event logs return `exists=false` and an empty event list.

## MCP/ACP

- Register `rlm_process_wait` as a model-visible read tool.
- Expose it through MCP by default because it only reads
  `.dscode/rlm-daemon` event logs.
- Classify it as ACP `read`.

## Verification

- `rlm_process_events_reads_live_event_cursor` covers:
  - immediate wait with an available event
  - immediate wait with no newer event
  - timeout and poll interval parsing clamps
- Regression commands:
  - `cargo test rlm_process --lib`
  - `cargo test build_tool_specs_include_rlm --lib`
  - `cargo test default_registry_includes_dispatch_subagent_only_below_max_depth --lib`
  - `cargo test serve --lib`
  - `cargo fmt --check`
  - `cargo check`
  - `git diff --check`

## Remaining Gap

This is long-polling over JSONL; the follow-on HTTP SSE bridge is implemented
in `2026-05-14-deepseek-tui-rlm-http-sse-stream.md`. Service packaging and
ACP-specific push subscriptions remain open; daemon package/service UX is now
covered by generated agents-daemon service templates.
