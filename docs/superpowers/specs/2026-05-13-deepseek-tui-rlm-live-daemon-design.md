# DeepSeek-TUI RLM Live Daemon Design

Date: 2026-05-13

Status: designed

## Gap

DeepSeekCode now has strong RLM coverage for common workflows: one-shot
`rlm`, aliases, `rlm_process`, chunk/map-reduce/recursive planners, restricted
Python helpers, persistent Python helper processes, durable model-session
summaries under `.dscode/rlm-model/`, `rlm_process_sessions`, and
session-only continuation for existing non-empty sessions.

The remaining gap is that model-backed `rlm_process` still runs as bounded
child-agent calls. It persists summaries, not a live model-session worker with
its own lifecycle, queue, cancellation, streaming status, and recovery
semantics after the caller exits.

## Success Criteria

- `rlm_process session_id=<id> live=true` creates or reuses a long-lived
  workspace-local RLM daemon session.
- A live session owns a runtime thread, turn queue, active turn status, model
  configuration, bounded recent context, and durable event log.
- The caller can exit while the live RLM session remains inspectable through
  `rlm_process_sessions`.
- Follow-up `rlm_process` calls enqueue turns onto the same live session instead
  of rebuilding only from persisted summaries.
- Cancellation targets the active live RLM turn without destroying the whole
  session unless explicitly requested.
- Recovery never claims provider-level model-state resume unless the provider
  exposes it. If the daemon dies, DeepSeekCode restarts from durable runtime
  thread context and the bounded RLM summary log.

## Proposed Architecture

### Process Model

- Add an RLM daemon worker managed by `serve`, local TUI, or `agents service`.
- Store daemon session records under `.dscode/rlm-daemon/<session_id>/`.
- Link every live RLM session to a durable runtime thread:
  - `runtime_thread_id`
  - `runtime_session_id` when applicable
  - `model`
  - `mode`
  - `workspace`
- Keep the existing `.dscode/rlm-model/<session_id>.json` summaries as a
  compatibility export, not the authoritative live state.

### Session Manifest

Each live session writes `manifest.json` with:

- `session_id`
- `status`: `idle`, `running`, `cancelled`, `failed`, or `stopped`
- `daemon_pid`
- `daemon_epoch`
- `runtime_thread_id`
- `active_turn_id`
- `queued_turns`
- `model`
- `workspace`
- `created_at`
- `updated_at`
- `last_error`

Older `.dscode/rlm-model/` summary sessions remain valid and can be imported
into a live session as initial context.

### Event Log

Each live session writes `events.jsonl` with monotonic `seq`:

- `session_started`
- `turn_queued`
- `turn_started`
- `model_delta`
- `tool_call`
- `tool_result`
- `turn_completed`
- `turn_failed`
- `turn_cancelled`
- `session_stopped`

The event log gives TUI/MCP/ACP clients a stable streaming and replay surface
without coupling them to one in-memory worker.

### Tool Contract

`rlm_process` keeps the current bounded behavior by default during migration.
When `live=true` is supplied:

- `session_id` is required.
- `task` is required.
- `file_path` or `content` is required only for new empty live sessions.
- Existing non-empty live sessions may continue with `task + session_id`.
- Output includes:
  - `meta.rlm_live=true`
  - `meta.rlm_session_id=<id>`
  - `meta.rlm_runtime_thread_id=<thread>`
  - `meta.rlm_turn_id=<turn>`
  - `meta.rlm_status=<status>`

`rlm_process_sessions include_live=true` should report both legacy summary
sessions and live daemon sessions, including daemon status, active turn, queue
length, runtime thread id, and last error.

Future tools:

- `rlm_process_wait`
- `rlm_process_cancel`
- `rlm_process_events`

### Cancellation

Live RLM cancellation should reuse the existing runtime cancellation model:

- append a durable cancel event for the active runtime task or turn
- propagate cancellation through cancel-aware model and tool execution
- mark the active RLM turn as `turn_cancelled`
- keep the session alive unless `stop=true`

### Recovery

On daemon restart:

- scan `.dscode/rlm-daemon/*/manifest.json`
- mark sessions with dead `daemon_pid` as `stale`
- reload linked runtime thread context
- import the bounded `.dscode/rlm-model/<session_id>.json` summary if present
- resume only queued turns that were not started
- mark interrupted active turns as failed or cancelled with a recovery event

This is honest recovery from durable context, not provider-level model state
resumption.

## Implementation Slices

1. Live-session manifest and inventory:
   - add `.dscode/rlm-daemon/<session_id>/manifest.json`
   - extend `rlm_process_sessions include_live=true`
   - extend `rlm_process_sessions include_turns=true`
   - no model execution yet
   - status: implemented by
     `2026-05-13-deepseek-tui-rlm-live-session-inventory.md` and
     `2026-05-13-deepseek-tui-rlm-live-turn-inventory.md`
2. Runtime-thread-backed live session:
   - create/reuse runtime thread per live RLM session
   - persist per-turn payloads with task, input, and execution options
   - enqueue turns
   - persist `events.jsonl`
   - status: implemented by
     `2026-05-13-deepseek-tui-rlm-live-turn-queue.md` and
     `2026-05-13-deepseek-tui-rlm-live-turn-payload.md`
3. Tool routing:
   - `rlm_process live=true`
   - `rlm_process_wait`
   - `rlm_process_cancel`
   - `rlm_process_recover`
   - `rlm_process_stop`
   - `rlm_process_run_next`
   - `rlm_process_drain`
   - MCP/ACP schema updates
   - status: partial; `rlm_process live=true` queueing and
     `rlm_process_events` read-only replay plus `rlm_process_wait`
     long-polling are implemented;
     `rlm_process_cancel` is implemented for queued pending turns only;
     `rlm_process_recover` is implemented for interrupted running turns in one
     live session or across all live session manifests with `all=true`;
     `rlm_process_stop` is implemented for idle session shutdown and queued
     turn cancellation;
     `rlm_process_run_next` is implemented as a single-step worker bridge;
     `rlm_process_drain` is implemented as a bounded FIFO batch worker bridge
4. Streaming and cancellation:
   - `rlm_process_events`
   - active turn cancellation via runtime cancel events
   - status: partial; event-log replay/wait, queued-turn cancellation, and
     single-step/batch worker completion are implemented; worker streaming,
     resident daemon service packaging, and active worker cancellation remain open
5. Recovery:
   - daemon restart scan
   - stale pid detection
   - interrupted-turn recovery records
   - status: partial; `rlm_process_recover` can requeue or fail interrupted
     running turns for one session or all live manifests and records
     `turn_recovered`; stale pid ownership checks remain open
6. Service packaging:
   - systemd/launchd templates for RLM daemon alongside runtime and diagnostics
   - status: partial; `deepseek agents daemon` now runs one queued live RLM
     turn per tick through `rlm_process_run_next`, and the existing
     systemd/launchd templates already run that daemon; explicit RLM daemon
     lifecycle commands remain open

## Verification Plan

Future implementation should add these gates:

- `cargo test rlm_live_session_manifest_inventory --lib`
- `cargo test rlm_process_live_enqueues_turn_on_runtime_thread --lib`
- `cargo test rlm_process_live_session_only_continuation --lib`
- `cargo test rlm_process_live_cancel_marks_active_turn --lib`
- `cargo test rlm_live_daemon_recovery_marks_interrupted_turn --lib`
- `cargo test runtime_daemon_tick_routes_live_rlm_turns_through_rlm_worker --lib`
- `cargo test serve --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Current Decision

Do not rename the existing bounded child-agent `rlm_process` implementation as a
live daemon. It is already useful and should remain the default until a real
live worker exists. The remaining executable RLM slices should focus on
streaming model/tool deltas, active worker cancellation, stale daemon pid
detection, and explicit lifecycle commands.
