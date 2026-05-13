# DeepSeek-TUI RLM Process Session Inventory

Date: 2026-05-13

Status: implemented

## Gap

`rlm_process` can persist bounded model-session summaries under
`.dscode/rlm-model/`, but the model had no read-only way to discover which
durable process sessions already exist before deciding whether to continue a
session, reset it, or create a new one.

## Spec

- Add `rlm_process_sessions` as a read-only tool.
- With no `session_id`, list persisted `.dscode/rlm-model/*.json` sessions with
  id, path, byte size, turn count, updated timestamp, and last task.
- With `session_id`, return whether the session exists, the manifest path,
  byte size, and the full stored session JSON.
- Reuse the same safe `session_id` validation as `rlm_process`.
- Keep list output bounded with `limit`, defaulting to 20 and clamping to
  1-100.
- Expose the tool through model schemas and the default MCP/ACP read-only
  surface because it does not run a child model or write state.

## Implementation

- Added `RlmModelSessionsTool` in `src/tools/rlm.rs`.
- Added `updated_at` preservation for `RlmModelSession` manifests.
- Registered `rlm_process_sessions` in the default tool registry.
- Added OpenAI/Anthropic tool schema coverage in `src/model/deepseek.rs`.
- Added MCP/ACP tool-list exposure in `src/cli/commands/serve.rs`.
- Updated runtime docs, the parity plan, and the post-shell gap review.

## Verification

- `cargo test rlm_process_sessions --lib`
- `cargo test build_tool_specs_include_rlm --lib`
- `cargo test default_registry_includes_dispatch_subagent_only_below_max_depth --lib`
- `cargo test serve --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining Gap

This makes durable RLM process context discoverable, but it is not a true live
model REPL or daemon. Full parity still requires a long-lived model-backed
runtime owner with resumable model state, cancellation semantics, and recovery
after the owning DeepSeekCode process exits.
