# DeepSeek-TUI RLM Live Event Replay

Date: 2026-05-13

Status: implemented

## Gap

`rlm_process live=true` can enqueue turns and append `turn_queued` records to
`.dscode/rlm-daemon/<session_id>/events.jsonl`, but there was no model-visible
read-only event replay tool. TUI/MCP/ACP clients needed a cursor-based surface
before live worker streaming can be implemented honestly.

## Spec

- Add read-only `rlm_process_events`.
- Require `session_id` and validate it with the same safe id rules used by
  `rlm_process`.
- Read `.dscode/rlm-daemon/<session_id>/events.jsonl`.
- Support `cursor`, `after_seq`, or `since_seq` aliases and return only events
  with `seq` greater than the cursor.
- Support bounded `limit`, defaulting to 50 and clamped to 1-500.
- Return `cursor`, `next_cursor`, `exists`, and parsed event JSON.
- Missing event logs return `exists:false` and an empty event list.
- Expose the tool through the default registry, model tool specs, and MCP
  read-only surface.

## Implementation

- Added `RlmLiveEventsTool`.
- Added cursor and limit parsing helpers.
- Reused the live RLM event-log path and JSON parser.
- Registered the tool in the agent registry, static model tool specs, and MCP
  server tool definitions.
- Added a cursor regression test over two queued live turns.

## Verification

- `/home/willamhou/.cargo/bin/cargo test rlm_process_events_reads_live_event_cursor --lib`
- `/home/willamhou/.cargo/bin/cargo test rlm_process --lib`
- `/home/willamhou/.cargo/bin/cargo test build_tool_specs_include_rlm --lib`
- `/home/willamhou/.cargo/bin/cargo test default_registry_includes_dispatch_subagent_only_below_max_depth --lib`
- `/home/willamhou/.cargo/bin/cargo test serve --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `/home/willamhou/.cargo/bin/cargo check`
- `git diff --check`

## Remaining Gap

This is event-log replay, not live model execution. The next RLM daemon slices
still need a worker that claims queued turns, appends model/tool deltas, handles
active-turn cancellation, and records recovery state after interruption.
