# DeepSeek-TUI RLM Live Turn Inventory

## Status

Implemented.

## Goal

Expose per-turn live RLM state through the existing session inventory surface so
TUI/MCP/ACP clients can render queued, running, completed, failed, and
cancelled live RLM turns without reading payload files directly.

## Behavior

- Extend `rlm_process_sessions` with `include_turns=true`.
- `include_turns=true` implies `include_live=true`.
- For a specific `session_id`, return `live_turns`.
- For list mode, attach `turns` to each live session entry.
- Use the existing `limit` as a cap for both listed sessions and listed turns.
- Read `.dscode/rlm-daemon/<session_id>/turns/*.json`.
- Sort turns by payload `created_at`, then `task_id`.
- Return per-turn metadata:
  - `task_id`
  - payload path and byte size
  - payload status
  - runtime task status and updated timestamp when available
  - task text
  - steps, model, workspace
  - created/updated timestamps
  - input label, char count, and line count
  - cancellation reason
  - bounded result/error previews with character counts and truncation flags
- Do not return long input content from the inventory list.

## Verification

- `rlm_process_live_enqueues_runtime_turn_and_manifest` now verifies:
  - `include_turns=true` implies live inventory for a specific session
  - `live_turns` includes both queued turn ids
  - runtime task status is present
  - input labels are present
  - list mode includes `turns`
- Regression commands:
  - `cargo test rlm_process --lib`
  - `cargo test build_tool_specs_include_rlm --lib`
  - `cargo test serve --lib`
  - `cargo fmt --check`
  - `cargo check`
  - `git diff --check`

## Remaining Gap

This is read-only inventory. The remaining live RLM gaps are TUI/ACP
subscription polish and operator lifecycle commands.
