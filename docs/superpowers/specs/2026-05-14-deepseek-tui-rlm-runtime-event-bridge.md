# DeepSeek-TUI RLM Runtime Event Bridge

Date: 2026-05-14

Status: implemented

## Gap

Live RLM worker progress was available through `.dscode/rlm-daemon` JSONL,
`rlm_process_events`, `rlm_process_wait`, HTTP RLM SSE, and CLI lifecycle
commands. The existing TUI runtime watcher, however, subscribes to aggregate
runtime events, so live RLM progress did not naturally flow through the same
subscription path as turns, tasks, approvals, and user-input requests.

## Spec

- Add a safe RuntimeStore API for appending external thread events while
  updating the owning thread/session cursor metadata.
- Mirror every live RLM JSONL event into the runtime thread as
  `kind=rlm_live_event`.
- Include the original RLM event under `payload.event` plus `session_id` so
  HTTP runtime SSE, TUI watchers, and ACP/runtime clients share one
  subscription shape.
- Keep the dedicated RLM JSONL event log and `/v1/rlm/live/.../events/stream`
  unchanged for clients that want the raw per-session stream.
- Let the TUI HTTP watcher convert mirrored `rlm_live_event` frames into a
  concise status line while still refreshing the runtime snapshot.

## Verification

- `cargo test rlm_live_events_mirror_runtime_thread_events --lib`
- `cargo test runtime_store_appends_external_thread_event --lib`
- `cargo test runtime_http_watcher_formats_rlm_live_event_status --lib`
- `cargo test rlm_process_events --lib`
- `cargo test serve --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

ACP-specific push subscriptions remain open; daemon package/service UX is now
covered by generated agents-daemon service templates.
