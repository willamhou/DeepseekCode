# DeepSeek-TUI RLM Live Recovery

## Status

Implemented.

## Goal

Narrow the live RLM restart-recovery gap by adding a deterministic recovery
tool for interrupted live `rlm_process` turns.

This does not claim provider-level model-state resume. It recovers honestly from
DeepSeekCode's durable runtime task state, live manifest, persisted turn
payloads, and live event log.

## Behavior

- Add `rlm_process_recover`.
- Require `session_id`, unless `all=true` scans all live sessions.
- Accept `limit` for `all=true`, defaulting to 20 and clamped to 1-100.
- Read `.dscode/rlm-daemon/<session_id>/manifest.json`.
- Inspect the manifest `active_turn_id`, linked runtime `rlm_process` tasks,
  and persisted turn payloads under `turns/`.
- Treat `running` runtime tasks or `running` payloads as interrupted recovery
  candidates.
- Default `mode=requeue`:
  - set recoverable runtime tasks back to `pending`
  - set their persisted payload status back to `queued`
  - clear stale `active_turn_id`
  - refresh manifest `queued_turns`
  - append `turn_recovered`
- `mode=fail` marks interrupted candidates failed instead.
- `dry_run=true` reports selected actions without mutating state.
- `all=true` returns per-session recovery summaries and continues past
  per-session errors.

## MCP/ACP

- Register `rlm_process_recover` as a model-visible tool.
- Expose it through MCP only with durable runtime approvals because it mutates
  runtime task state and live RLM payloads.
- Classify it as an ACP `execute` tool.

## Verification

- `rlm_process_recover_requeues_interrupted_active_turn` verifies dry-run
  preview, requeue recovery, manifest active-turn clearing, queued-turn refresh,
  payload status recovery, and `turn_recovered` event replay.
- `rlm_process_recover_all_scans_live_sessions` verifies workspace-wide
  manifest scanning, per-session recovery, aggregate recovered counts, and
  `limit` parsing.
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
