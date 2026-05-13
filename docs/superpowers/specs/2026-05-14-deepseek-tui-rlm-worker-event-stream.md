# DeepSeek-TUI RLM Worker Event Stream

Date: 2026-05-14

Status: implemented

## Gap

`rlm_process_events` and `rlm_process_wait` exposed live-session JSONL logs, but
`rlm_process_run_next` only wrote lifecycle markers (`turn_started`,
`turn_completed`, `turn_failed`). Clients could not observe the active worker's
reasoning/text deltas, model-selected tool calls, actual tool execution, or tool
results while a live RLM turn was running.

## Spec

- Let `dispatch_subagent` accept optional stream and run-event sinks for
  internal callers while preserving the normal `dispatch_subagent` and
  `dispatch_subagents` tool behavior.
- When `rlm_process_run_next` starts a worker, attach live-session event sinks
  to the child agent loop.
- Append these worker events to
  `.dscode/rlm-daemon/<session_id>/events.jsonl` with sequence numbers:
  - `worker_reasoning_delta`
  - `worker_text_delta`
  - `worker_assistant_done`
  - `worker_model_tool_call`
  - `worker_tool_call`
  - `worker_permission_request`
  - `worker_tool_result`
- Include `runtime_thread_id`, `task_id`, tool names, structured tool input,
  status, and bounded output/text previews where applicable.
- Continue exposing the stream through the existing cursor and long-poll tools:
  `rlm_process_events` and `rlm_process_wait`.

## Verification

- `cargo test rlm_live_worker_events --lib`
- `cargo test rlm_process --lib`
- `cargo test build_tool_specs_include_rlm --lib`
- `cargo test serve --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

ACP-specific push subscriptions remain open; daemon package/service UX is now
covered by generated agents-daemon service templates.
