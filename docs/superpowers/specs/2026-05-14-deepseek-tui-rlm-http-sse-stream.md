# DeepSeek-TUI RLM HTTP SSE Stream

Date: 2026-05-14

Status: implemented

## Gap

Live RLM worker progress was available through `rlm_process_events` and
`rlm_process_wait`, but HTTP clients still needed to poll JSON responses. The
runtime already had SSE endpoints for runtime thread events, so live RLM needed
the same native stream shape for DeepSeek-TUI-style external observers.

## Spec

- Add `GET /v1/rlm/live/<session_id>/events/stream`.
- Read `.dscode/rlm-daemon/<session_id>/events.jsonl` through the same live RLM
  event cursor contract as `rlm_process_events`.
- Accept `cursor` or `since_seq`.
- Accept bounded wait options `wait_ms` and `poll_ms`.
- Support `follow=1` with `max_events`, `max_ms`, and `poll_ms`, matching the
  existing runtime event stream follow-mode behavior.
- Emit SSE frames with:
  - `id: <seq>`
  - `event: <kind>`
  - `data: <raw live RLM event JSON>`
- Advertise the endpoint in `/runtime`.

## Verification

- `cargo test rlm_live_event_stream_endpoint --lib`
- `cargo test serve --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

ACP-specific push subscriptions remain open; daemon package/service UX is now
covered by generated agents-daemon service templates.
