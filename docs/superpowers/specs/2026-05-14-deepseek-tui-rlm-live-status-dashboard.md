# DeepSeek-TUI RLM Live Status Dashboard

Date: 2026-05-14

Status: implemented

## Gap

Live RLM state was inspectable through `rlm_process_sessions include_live=true`,
but operators and clients still had to combine manifest fields, runtime task
state, payload state, owner liveness, and queue counts manually before deciding
whether to wait, recover, run-next, drain, or restart a session.

## Spec

- Add read-only `rlm_process_status`.
- With `session_id`, return one live-session lifecycle object.
- Without `session_id`, return all live sessions up to `limit` plus workspace
  totals.
- Report:
  - manifest status, active turn, runtime thread/session ids
  - daemon pid/epoch, `daemon_alive`, `daemon_stale`, and `daemon_owner`
  - manifest queue count and runtime pending queue count
  - runtime task counts and persisted turn payload counts
  - next pending turn id
  - recommended next commands
- Missing sessions return `exists=false` instead of mutating state.
- Expose the tool through agent, MCP, and ACP read-only surfaces.

## Verification

- `cargo test rlm_process_status_summarizes_live_queue_and_stale_owner --lib`
- `cargo test rlm_process --lib`
- `cargo test default_registry_includes_dispatch_subagent_only_below_max_depth --lib`
- `cargo test build_tool_specs_include_rlm --lib`
- `cargo test serve --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

TUI/ACP subscription polish and richer lifecycle commands remain open.
