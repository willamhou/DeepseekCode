# Runtime Contract

This document is the public draft for local supervisors, future TUI/workbench
code, and release checks that need a stable DeepSeekCode integration surface.

The current contract is intentionally small. It records what is stable now and
what must remain explicit until the full durable runtime lands.

## TUI Workbench Shell

`deepseek tui` starts the ratatui/crossterm full-screen workbench shell. It has
Plan / Agent / YOLO mode tabs, sidebar, transcript/composer frame, task panel,
session picker, command palette, and approval modal surfaces. `deepseek tui
--demo --once` renders a deterministic non-interactive snapshot for release and
CI checks.

The TUI reads file-backed durable sessions, linked threads, and thread item
timelines from `.dscode/runtime/` at startup and refreshes them while the
interactive workbench is open. The session picker and thread navigator switch
the visible runtime snapshot, the transcript shows durable items for the active
thread, and the task panel shows active thread usage totals, cache-hit rate,
cache chart, estimated cost, input/output cost split, cost chart,
recent active-thread runtime tasks, active-thread automations, and
1M-context strategy when durable usage records exist. The approval modal opens
for pending durable `permission_request` events. Composer submissions append a
durable user turn/item and start a background agent run for the active thread;
the run creates a running assistant message item, streams assistant deltas into
it through item updates, and then appends tool result items, usage, and task
records back into runtime. For TUI-started agent runs, those assistant and
reasoning item updates are also sent through an in-process live channel that the
TUI drains before each draw, so visible streaming is not tied to the 1s durable
refresh interval. A local runtime watcher also polls the durable store and sends
full snapshot live events when thread/task/approval/usage state changes, so
writes from other DeepSeekCode processes are visible in the foreground TUI
before the slower periodic refresh. For TUI-started agent runs,
durable `permission_response` events unblock the running write/shell/MCP
permission gate. The active running assistant turn can be cancelled with a
durable `cancel_requested` event, and the run marks its turn/item/task
`cancelled` at the next checkpoint. Cancel-aware tool execution now kills
`run_shell` process groups, and remote model streams observe cancellation
between SSE frames. The command palette can also compact the active durable
thread through the same non-destructive runtime compaction path as
`POST /v1/threads/{id}/compact`, create pending active-thread runtime tasks, and
trigger current-thread automations into pending runtime tasks. It is not yet a
complete runtime client: HTTP-runtime TUI sessions now use per-thread SSE
follow streams for foreground refresh, but fully interrupting a blocked model
socket read, richer progress controls, and general external command execution
remain future work.

## HTTP Runtime

Start the local runtime skeleton:

```bash
deepseek serve --http
```

The full-screen workbench can connect to that runtime instead of reading the
local `.dscode/runtime` store directly:

```bash
deepseek tui --runtime-url http://127.0.0.1:13000
```

In HTTP mode, `deepseek tui` builds snapshots from the runtime endpoints,
writes composer/approval/cancel/task/automation/compaction actions back over
HTTP, and follows known thread event streams with `follow=1` for lower-latency
foreground refresh.

Use `--addr HOST:PORT` to override the bind address and `--once` for one
request in tests:

```bash
deepseek serve --http --addr 127.0.0.1:0 --once
```

Endpoints:

| Endpoint | Method | Status |
|---|---:|---|
| `/health`, `/v1/health` | `GET`, `HEAD` | Stable health JSON |
| `/runtime`, `/v1/runtime` | `GET`, `HEAD` | Stable capability JSON |
| `/v1/automations` | `GET`, `POST` | File-backed durable automation records |
| `/v1/automations/{id}` | `GET`, `HEAD` | Automation detail |
| `/v1/automations/{id}/trigger` | `POST` | Create a pending task from an active automation |
| `/v1/diagnostics` | `GET`, `POST` | Runtime-hosted diagnostics broker with warmed LSP reuse |
| `/v1/sessions` | `GET`, `POST` | File-backed durable session records |
| `/v1/sessions/{id}` | `GET`, `HEAD` | Session detail with linked threads |
| `/v1/sessions/{id}/automations` | `GET`, `POST` | Automation records for one session |
| `/v1/sessions/{id}/threads` | `POST` | Create a thread inside one session |
| `/v1/sessions/{id}/tasks` | `GET`, `POST` | Task records for one session |
| `/v1/tasks` | `GET`, `POST` | File-backed durable task records |
| `/v1/tasks/{id}` | `GET`, `HEAD`, `PATCH`, `POST` | Task detail or status/summary update |
| `/v1/tasks/{id}/claim` | `POST` | Atomically claim a pending task for an external runner |
| `/v1/tasks/{id}/cancel` | `POST` | Cancel a task and append a durable cancellation event |
| `/v1/tasks/{id}/pause` | `POST` | Pause a pending task before it is claimed |
| `/v1/tasks/{id}/resume` | `POST` | Resume a paused task back to pending |
| `/v1/threads` | `GET`, `POST` | File-backed durable thread records |
| `/v1/threads/{id}` | `GET`, `HEAD` | Thread detail with recorded turns and items |
| `/v1/threads/{id}/automations` | `GET`, `POST` | Automation records for one thread |
| `/v1/threads/{id}/compact` | `POST` | Append a durable compaction summary and audit event |
| `/v1/threads/{id}/items` | `GET`, `POST` | Item timeline records for one thread |
| `/v1/threads/{id}/items/{item_id}` | `GET`, `HEAD` | Item detail |
| `/v1/threads/{id}/turns` | `POST` | Append a completed turn record |
| `/v1/threads/{id}/turns/{turn_id}/items` | `GET`, `POST` | Item records for one turn |
| `/v1/threads/{id}/tasks` | `GET`, `POST` | Task records for one thread |
| `/v1/threads/{id}/events` | `GET`, `HEAD`, `POST` | Append-only event replay and permission request append |
| `/v1/threads/{id}/events/stream` | `GET`, `HEAD` | SSE replay, bounded wait, or follow-mode streaming of append-only events |
| `/v1/threads/{id}/usage` | `GET`, `HEAD` | Usage records for one thread |
| `/v1/threads/{id}/usage/summary` | `GET`, `HEAD` | Usage accounting and 1M-context policy for one thread |
| `/v1/usage` | `GET`, `HEAD` | Usage records across threads, optionally filtered by `thread_id` |
| `/v1/usage/summary` | `GET`, `HEAD` | Usage accounting and 1M-context policy, optionally filtered by `thread_id` |

The HTTP listener handles accepted connections in worker threads, so bounded
SSE waits and follow-mode streams do not block concurrent writes such as
`POST /v1/threads/{id}/turns` or `POST /v1/threads/{id}/events`. External
approval bridging and engine-driven resume/fork are not implemented yet.
`/runtime` advertises only the durable capabilities that currently have a
backing store.
Tasks can be claimed by an external runner, paused/resumed while still queued,
cancelled through a first-class task endpoint, and active automations can be
triggered into pending tasks. Usage records now include cache-hit/cache-miss
token telemetry and estimated USD micro-costs for recognized DeepSeek V4 model
names.

`deepseek agents run-task` and the daemon runner also publish durable
`permission_request` events for permissioned write/shell/MCP calls and wait for
a matching `permission_response` on the same thread. This lets the TUI approval
modal or an HTTP client approve background tasks without being the process that
started the agent loop.

For local always-on use, render supervisor files instead of hand-writing
systemd or launchd units:

```bash
deepseek agents service --kind systemd --out ./services --workdir "$PWD" --bin "$(command -v deepseek)"
deepseek agents service --kind launchd --out ./services --workdir "$PWD" --bin "$(command -v deepseek)"
```

The rendered set runs `deepseek serve --http`,
`deepseek agents daemon --json`, and
`deepseek diagnostics --watch --changed` against the selected workspace. It is
still a local service template, not a hosted multi-user runtime.

### Health Schema

`/health` returns schema `deepseek.runtime.health.v1`:

```json
{
  "status": "ok",
  "service": "DeepSeekCode",
  "version": "0.0.0",
  "runtime": "http",
  "schema": "deepseek.runtime.health.v1"
}
```

### Runtime Metadata Schema

`/runtime` returns the current API version, endpoints, and truthful capability
flags:

```json
{
  "service": "DeepSeekCode",
  "version": "0.0.0",
  "api_version": "v1",
  "transport": "http",
  "endpoints": [
    "/health",
    "/v1/health",
    "/runtime",
    "/v1/runtime",
    "/v1/automations",
    "/v1/automations/{id}",
    "/v1/automations/{id}/trigger",
    "/v1/diagnostics",
    "/v1/sessions",
    "/v1/sessions/{id}",
    "/v1/sessions/{id}/automations",
    "/v1/sessions/{id}/threads",
    "/v1/sessions/{id}/tasks",
    "/v1/tasks",
    "/v1/tasks/{id}",
    "/v1/tasks/{id}/claim",
    "/v1/tasks/{id}/cancel",
    "/v1/tasks/{id}/pause",
    "/v1/tasks/{id}/resume",
    "/v1/threads",
    "/v1/threads/{id}",
    "/v1/threads/{id}/automations",
    "/v1/threads/{id}/compact",
    "/v1/threads/{id}/items",
    "/v1/threads/{id}/items/{item_id}",
    "/v1/threads/{id}/turns",
    "/v1/threads/{id}/turns/{turn_id}/items",
    "/v1/threads/{id}/tasks",
    "/v1/threads/{id}/events",
    "/v1/threads/{id}/events/stream",
    "/v1/threads/{id}/usage",
    "/v1/threads/{id}/usage/summary",
    "/v1/usage",
    "/v1/usage/summary"
  ],
  "capabilities": {
    "health": true,
    "runtime_metadata": true,
    "sessions": true,
    "threads": true,
    "thread_compaction": true,
    "turns": true,
    "items": true,
    "events": true,
    "events_write": true,
    "cancellation_events": true,
    "events_sse": true,
    "diagnostics": true,
    "diagnostics_changed": true,
    "diagnostics_broker": true,
    "events_sse_wait": true,
    "events_sse_follow": true,
    "tasks": true,
    "task_claim": true,
    "task_cancel": true,
    "task_pause": true,
    "task_resume": true,
    "task_updates": true,
    "automations": true,
    "automation_trigger": true,
    "usage": true,
    "usage_summary": true
  }
}
```

## Durable Runtime v1

The first durable runtime slice stores records under `.dscode/runtime/`:

```text
.dscode/runtime/
  sessions/<session-id>.json
  threads/<thread-id>.json
  turns/<thread-id>/<turn-id>.json
  items/<thread-id>/<item-id>.json
  tasks/<task-id>.json
  automations/<automation-id>.json
  events/<thread-id>.jsonl
  usage/<thread-id>/<usage-id>.json
```

`POST /v1/sessions` accepts an optional JSON object:

```json
{
  "title": "Daily work",
  "workspace": "."
}
```

It returns schema `deepseek.runtime.session.v1`:

```json
{
  "schema": "deepseek.runtime.session.v1",
  "session": {
    "id": "session-...",
    "created_at": "epoch+1745960000",
    "updated_at": "epoch+1745960000",
    "title": "Daily work",
    "workspace": ".",
    "status": "active",
    "active_thread_id": null,
    "thread_count": 0
  }
}
```

`GET /v1/sessions?limit=50` returns schema
`deepseek.runtime.sessions.v1`. `GET /v1/sessions/{id}` returns the session
plus all linked threads. `POST /v1/sessions/{id}/threads` accepts the same body
as `POST /v1/threads` and links the new thread to that session.

`POST /v1/threads` accepts an optional JSON object:

```json
{
  "title": "Investigate build failure",
  "workspace": ".",
  "model": "deepseek-coder",
  "mode": "agent",
  "session_id": "session-..."
}
```

It returns schema `deepseek.runtime.thread.v1` with a `thread` object.
Missing fields default to `Untitled thread`, `.`, `deepseek-coder`, and
`agent`.

`GET /v1/threads?limit=50` returns schema `deepseek.runtime.threads.v1`:

```json
{
  "schema": "deepseek.runtime.threads.v1",
  "threads": [
    {
      "id": "thread-...",
      "session_id": "session-...",
      "created_at": "epoch+1745960000",
      "updated_at": "epoch+1745960000",
      "title": "Investigate build failure",
      "workspace": ".",
      "model": "deepseek-coder",
      "mode": "agent",
      "status": "active",
      "latest_turn_id": null,
      "event_seq": 1
    }
  ]
}
```

`POST /v1/threads/{id}/turns` appends a completed turn. The request body must
contain non-empty `content`; `role` defaults to `user` and may be `user`,
`assistant`, `tool`, or `system`.

```json
{
  "role": "user",
  "content": "continue the investigation"
}
```

`GET /v1/threads/{id}` returns the thread plus its recorded turns and items.

`POST /v1/threads/{id}/items` appends an item to the thread timeline.
`POST /v1/threads/{id}/turns/{turn_id}/items` appends an item linked to one
turn. The request body must contain non-empty `content`; `item_type` defaults
to `message`, `status` defaults to `completed`, and `role` may be `user`,
`assistant`, `tool`, or `system` when provided.

```json
{
  "item_type": "message",
  "role": "assistant",
  "content": "done",
  "status": "completed"
}
```

Allowed item types are `message`, `tool_call`, `tool_result`, `reasoning`,
`diagnostic`, `event`, and `summary`. Allowed item statuses are `pending`, `running`,
`completed`, `failed`, and `cancelled`. `GET /v1/threads/{id}/items` and
`GET /v1/threads/{id}/turns/{turn_id}/items` return schema
`deepseek.runtime.items.v1`; `GET /v1/threads/{id}/items/{item_id}` returns
schema `deepseek.runtime.item.v1`.

For TUI-started agent runs, streamed `reasoning_content`, `thinking_delta`, and
`reasoning_delta` chunks are persisted as linked `reasoning` items on the
assistant turn and emitted to the foreground TUI through the same live item
channel as assistant text deltas. The agent loop also replays compact recent
reasoning summaries inside subsequent model requests during the same run.

`POST /v1/threads/{id}/compact` appends a non-destructive compaction marker for
long contexts. The runtime keeps all original turn and item records, then adds a
system summary turn, a linked `summary` item, and a `thread_compacted` audit
event that names the summarized and preserved tail turns. The request body is
optional:

```json
{
  "keep_tail_turns": 8,
  "summary": "Optional externally generated summary of the older context."
}
```

If `summary` is omitted, the runtime creates a deterministic extractive summary
from older turns. `keep_tail_turns` defaults to `8` and is capped at `200`; the
request is rejected if it leaves no turns to summarize. The response uses schema
`deepseek.runtime.thread_compaction.v1`:

```json
{
  "schema": "deepseek.runtime.thread_compaction.v1",
  "compaction": {
    "thread_id": "thread-...",
    "keep_tail_turns": 8,
    "summarized_turn_count": 42,
    "kept_turn_count": 8,
    "summary_source": "provided",
    "summarized_turn_ids": ["turn-..."],
    "kept_turn_ids": ["turn-..."],
    "summary_turn": {
      "id": "turn-...",
      "role": "system",
      "content": "Optional externally generated summary of the older context."
    },
    "summary_item": {
      "id": "item-...",
      "item_type": "summary",
      "role": "system"
    },
    "event": {
      "kind": "thread_compacted"
    }
  }
}
```

`GET /v1/threads/{id}/events?since_seq=N` replays append-only events after the
given sequence number. Current event kinds are `thread_created`,
`turn_recorded`, `turn_updated`, `item_recorded`, `item_updated`,
`usage_recorded`, `task_recorded`, `automation_recorded`,
`thread_compacted`, `permission_request`, `permission_response`, and
`cancel_requested`.

`POST /v1/threads/{id}/events` currently accepts `permission_request`,
`permission_response`, and `cancel_requested` events. `permission_request` uses
the same core payload fields as `exec --json` permission notifications and is
consumed by `deepseek tui`:

```json
{
  "type": "permission_request",
  "tool": "run_shell",
  "kind": "shell",
  "target": "cargo test",
  "status": "pending",
  "input": {
    "command": "cargo test"
  }
}
```

`permission_response` records the durable approval decision for a request:

```json
{
  "type": "permission_response",
  "request_id": "event-...",
  "decision": "approved"
}
```

Allowed decisions are `approved` and `denied`.

`cancel_requested` records a durable cancellation request for a thread, turn,
or task. `deepseek tui` emits it for the active running assistant turn when `c`
or the `cancel` command is used. `POST /v1/tasks/{id}/cancel` emits the same
event with `task_id` set:

```json
{
  "type": "cancel_requested",
  "turn_id": "turn-...",
  "task_id": "task-...",
  "reason": "user requested cancellation"
}
```

TUI-started agent runs check this event between model/tool steps and while
waiting for approval responses. Cancel-aware model/tool paths also poll the
same signal: `run_shell` starts commands in a separate process group and kills
that group on cancellation, while remote model streams abort between SSE
frames or while the stdout pipe is blocked waiting for the next frame. The
cancel-aware pipe reader polls every 100ms and drops the curl process instead of
waiting for the transport timeout.
It returns schema `deepseek.runtime.event.v1` with the appended event record.
`deepseek tui` uses these response events to unblock write/shell/MCP permission
gates for agent runs started from the TUI composer. Other runtimes still need to
provide their own resolver before relying on durable response events for live
agent wake-up.

`GET /v1/threads/{id}/events/stream?since_seq=N` returns the same durable event
replay as `text/event-stream`. Add `wait_ms=M` to hold the request open until a
new event appears after `since_seq` or the bounded wait expires. Add `follow=1`
for a long-lived stream that keeps polling and writing new frames on the same
connection until the client disconnects. `poll_ms=M` controls the store polling
interval. `wait_ms` is clamped to 30 seconds and `poll_ms` is clamped to
10-1000ms. Tests and short-lived clients can add `max_events=N` or `max_ms=N`
to close follow mode deterministically. Each frame uses the event sequence as
the SSE `id`, the runtime event kind as the SSE `event`, and the full event
record as a single-line JSON `data` payload:

```text
id: 2
event: turn_recorded
data: {"created_at":"epoch+1745960000","id":"event-...","kind":"turn_recorded","payload":{"role":"user","status":"completed","turn_id":"turn-..."},"seq":2,"thread_id":"thread-...","turn_id":"turn-..."}
```

Without `follow=1`, this endpoint closes after returning the replayed or newly
observed frames. With `follow=1`, it leaves the response open and streams later
events without a `Content-Length`; clients should resume from the last seen SSE
`id` after reconnecting. The HTTP listener handles connections concurrently, so
a waiting or following stream can observe events written by another request
without blocking that writer behind the stream request.

`GET /v1/tasks?session_id={id}&thread_id={id}&limit=50`,
`GET /v1/sessions/{id}/tasks`, and `GET /v1/threads/{id}/tasks` return schema
`deepseek.runtime.tasks.v1` with durable task metadata. `POST /v1/tasks`,
`POST /v1/sessions/{id}/tasks`, and `POST /v1/threads/{id}/tasks` create a
task record. `GET /v1/tasks/{id}` returns schema `deepseek.runtime.task.v1`;
`PATCH /v1/tasks/{id}` updates task `status` and/or `summary` and records a
`task_updated` event when the task is linked to a thread. `POST
/v1/tasks/{id}/claim` accepts `{"runner_id":"..."}` and moves a `pending` task
to `running`, recording a `task_claimed` event for thread-linked tasks. `POST
/v1/tasks/{id}/cancel` accepts `{"reason":"..."}`, moves a `pending`,
`running`, or already `cancelled` task to `cancelled`, records `task_updated`,
and appends a linked `cancel_requested` event when the task belongs to a thread.
`POST /v1/tasks/{id}/pause` moves a `pending` task to `paused` so daemon and
external runners skip it; `POST /v1/tasks/{id}/resume` moves a `paused` task
back to `pending`. Both pause and resume accept optional `{"summary":"..."}` and
record `task_updated` for thread-linked tasks:

```json
{
  "schema": "deepseek.runtime.task.v1",
  "task": {
    "id": "task-...",
    "session_id": "session-...",
    "thread_id": "thread-...",
    "parent_task_id": null,
    "kind": "exec",
    "status": "completed",
    "summary": "done",
    "created_at": "epoch+1745960000",
    "updated_at": "epoch+1745960000"
  }
}
```

Task status must be `pending`, `paused`, `running`, `completed`, `failed`, or
`cancelled`. Claiming provides the durable handoff point for external runners
or the built-in local runner; pause/resume controls queued work before it is
claimed; cancellation provides the matching durable stop signal for TUI, HTTP
clients, and daemon-managed work. For one-shot local execution,
`deepseek agents run-task <task-id>` claims a pending thread-linked task, runs
the agent loop in the thread workspace, appends user/assistant turns, tool
result items, usage, and final task status to the same durable thread, and
creates a pre-run rollback snapshot when possible. For background execution,
`deepseek agents daemon [--interval-ms 1000] [--budget N]` polls the same
runtime store, triggers due active automations, executes one thread-linked
pending task per tick, and performs non-destructive compaction for threads whose
latest usage record crosses the 800k-token warning threshold. The daemon skips a
thread when a `thread_compacted` event already exists after the latest
`usage_recorded` event, so repeated ticks do not append duplicate summaries.

`GET /v1/automations?session_id={id}&thread_id={id}&limit=50`,
`GET /v1/sessions/{id}/automations`, and
`GET /v1/threads/{id}/automations` return schema
`deepseek.runtime.automations.v1` with durable automation metadata.
`POST /v1/automations`, `POST /v1/sessions/{id}/automations`, and
`POST /v1/threads/{id}/automations` create an automation record.
`GET /v1/automations/{id}` returns schema
`deepseek.runtime.automation.v1`. `POST /v1/automations/{id}/trigger` accepts an
optional `{"prompt":"..."}` override, requires automation `status = "active"`,
updates `last_run_at`, creates a pending `automation` task, and records
`automation_triggered` when the automation is linked to a thread:

The local daemon treats `next_run_at` values of the form `epoch+SECONDS` as due
when they are less than or equal to the current epoch time. Recurring schedules
can use `every:60s`, `every:5m`, `every:1h`, or `@every 1h`; `manual` and
`once` leave `next_run_at` unset after firing.

```json
{
  "schema": "deepseek.runtime.automation.v1",
  "automation": {
    "id": "automation-...",
    "session_id": "session-...",
    "thread_id": "thread-...",
    "name": "Nightly check",
    "status": "active",
    "schedule": "daily",
    "prompt": "run diagnostics",
    "created_at": "epoch+1745960000",
    "updated_at": "epoch+1745960000",
    "last_run_at": null,
    "next_run_at": "epoch+1745963600"
  }
}
```

Automation status must be `active`, `paused`, `completed`, `failed`, or
`cancelled`. The runtime supports manual trigger-to-task handoff; schedule
evaluation, background execution, and notification delivery remain future work.

`GET /v1/threads/{id}/usage` and `GET /v1/usage?thread_id={id}&limit=50`
return schema `deepseek.runtime.usage.v1`:

```json
{
  "schema": "deepseek.runtime.usage.v1",
  "thread_id": "thread-...",
  "usage": [
    {
      "id": "usage-...",
      "thread_id": "thread-...",
      "turn_id": "turn-...",
      "model": "deepseek-v4-flash",
      "source": "exec",
      "prompt_tokens": 12345,
      "completion_tokens": 678,
      "total_tokens": 13023,
      "prompt_cache_hit_tokens": 10000,
      "prompt_cache_miss_tokens": 2345,
      "estimated_input_cost_microusd": 356,
      "estimated_output_cost_microusd": 190,
      "estimated_total_cost_microusd": 546,
      "pricing_source": "DeepSeek V4 Flash official USD pricing, effective 2026-04-26",
      "created_at": "epoch+1745960000"
    }
  ]
}
```

`GET /v1/threads/{id}/usage/summary` and
`GET /v1/usage/summary?thread_id={id}` return schema
`deepseek.runtime.usage_summary.v1` with aggregate token accounting and a
truthful 1M-context policy view:

```json
{
  "schema": "deepseek.runtime.usage_summary.v1",
  "thread_id": "thread-...",
  "record_count": 2,
  "prompt_tokens": 850012,
  "completion_tokens": 103,
  "total_tokens": 850115,
  "prompt_cache_hit_tokens": 250007,
  "prompt_cache_miss_tokens": 600005,
  "prompt_cache_hit_basis_points": 2941,
  "estimated_input_cost_microusd": 84701,
  "estimated_output_cost_microusd": 29,
  "estimated_total_cost_microusd": 84730,
  "unpriced_record_count": 0,
  "pricing_source": "DeepSeek official USD pricing table when model is recognized; unknown models are excluded from estimated cost",
  "latest_total_tokens": 850100,
  "context_window_tokens": 1000000,
  "warning_threshold_tokens": 800000,
  "hard_threshold_tokens": 900000,
  "latest_context_remaining_tokens": 149900,
  "latest_context_utilization_basis_points": 8501,
  "context_strategy": "prepare_compaction",
  "compaction_recommended": true,
  "compaction_endpoint": "/v1/threads/thread-.../compact"
}
```

The summary uses cumulative usage records for accounting and the latest usage
record as the current context-window estimate. Strategy values are `normal`,
`monitor`, `prepare_compaction`, and `must_compact_or_chunk`; the latter two set
`compaction_recommended` and point at the thread compaction endpoint when a
thread is in scope.

Successful `deepseek exec` runs now append a durable runtime session containing
one linked thread with user and assistant turns, matching message items, a
usage record sourced as `exec`, and a completed `exec` task record. The model
client preserves DeepSeek/OpenAI-compatible `prompt_cache_hit_tokens` /
`prompt_cache_miss_tokens` and Anthropic-compatible cache read/creation fields
when providers return them. Unknown model names keep cost fields as `null`;
recognized `deepseek-v4-flash`, `deepseek-v4-pro`, `deepseek-chat`, and
`deepseek-reasoner` records receive local USD micro-cost estimates based on the
current official DeepSeek V4 pricing table.

Record ids are restricted to ASCII alphanumeric plus `-` and `_`, must not
start with `.`, and must not contain `..`.

## Session Records

There are two existing session surfaces:

- REPL sessions saved with `/save <name>` as JSON under
  `.dscode/sessions/<name>.json`.
- Non-interactive `exec` resume snapshots saved as legacy TOML under
  `.dscode/sessions/session-<epoch>.toml`.

The durable runtime will eventually replace the split storage with a single
thread/session/event model. Until then, integrations should treat both legacy
formats as local files, not as the durable runtime API.

### REPL Session JSON v2

`/save` writes version `2` JSON atomically. `/load` accepts v1 and v2; v1 loads
with an empty todo list in memory and upgrades on the next save.

```json
{
  "version": 2,
  "name": "fix-pr-42",
  "saved_at": "epoch+1745960000",
  "skill": "pr-review",
  "budget": 30,
  "transcript": [
    { "role": "user", "content": "..." },
    { "role": "assistant", "content": "..." },
    {
      "role": "tool",
      "name": "read_file",
      "input": { "path": "src/lib.rs" },
      "output": "...",
      "status": "ok"
    }
  ],
  "tokens": { "prompt": 12345, "completion": 6789 },
  "todos": [
    {
      "content": "Run tests",
      "activeForm": "Running tests",
      "status": "pending"
    }
  ]
}
```

Allowed transcript roles are `user`, `assistant`, and `tool`. Tool status is
`ok` or `failed`. Todo status is `pending`, `in_progress`, or `completed`.

### Exec Snapshot TOML

`deepseek exec` resume uses a minimal snapshot for compatibility:

```toml
id = "session-1745960000"
task = "original task"
profile = "rust"
```

This format is not a complete transcript and should not be extended for TUI or
supervisor integrations.

## Subagent Thread Artifacts

Parallel subagent dispatch writes markdown artifacts under
`.dscode/agent-threads/<thread-id>.md` and stores the active thread marker in
`.dscode/agent-threads/active`.

Current artifact shape:

```markdown
# Agent Thread thread-...

Task: inspect repository layout
Agent: reviewer
Skill: -
Steps: 3

## Summary

meta.child_task=...
meta.child_outcome=ok
...
```

Thread IDs must be ASCII alphanumeric plus `-` or `_`, must not start with `.`,
and must not contain `..`. These artifacts are inspectable through:

```bash
deepseek agents threads
deepseek agents show-thread <thread-id>
deepseek agents switch <thread-id>
deepseek agents current
deepseek agents clear-current
```

They are not yet durable runtime threads. Future durable records should migrate
the same concepts into structured thread, turn, event, and child-task tables.

## Diagnostics

`deepseek diagnostics` runs local language diagnostics for the current
workspace. When a supported language server is available and concrete files are
provided, it first opens those files over stdio LSP and consumes
`textDocument/publishDiagnostics`; if the LSP attempt is unavailable, times out,
or fails, it falls back to a local compiler/type-check command:

```bash
deepseek diagnostics
deepseek diagnostics --changed
deepseek diagnostics src/lib.rs src/repl/slash.rs
deepseek diagnostics --watch --changed
deepseek diagnostics --watch --interval-ms 750 src/lib.rs
```

The current fallback engines are:

| Language | LSP server | Fallback engine |
|---|---|---|
| Rust | `rust-analyzer` | `cargo check --message-format=short` |
| TypeScript | `typescript-language-server` | `tsc --noEmit --pretty false` |
| JavaScript | `typescript-language-server` | `tsc` when `tsconfig.json` exists, otherwise `npm run type-check` when configured |
| Python | `pyright-langserver` | `python -m py_compile <files>` or `compileall` |
| Go | `gopls` | `go test ./...` |

The agent tool registry also exposes a read-only `diagnostics` tool. Automatic
post-edit diagnostics are opt-in:

```toml
diagnostics.post_edit = true
```

When enabled, successful `apply_patch` calls append a `post-edit diagnostics`
section to the tool result for the edited paths. Agent-loop `apply_patch`
reuses a warmed stdio LSP session inside the tool instance across repeated
edits. `deepseek diagnostics --watch` keeps the CLI process alive and reuses one
warmed stdio LSP session across ticks for concrete file paths, sending
`didChange` on subsequent checks. Both paths fall back to compiler checks
whenever the warmed LSP session is unavailable or times out. The generated
systemd/launchd service set can run `deepseek diagnostics --watch --changed` as
an always-on local diagnostics worker for the workspace.

HTTP runtime clients can call the runtime-hosted diagnostics broker:

```bash
curl -sS http://127.0.0.1:13000/v1/diagnostics \
  -H 'content-type: application/json' \
  -d '{"changed":true}'

curl -sS http://127.0.0.1:13000/v1/diagnostics \
  -H 'content-type: application/json' \
  -d '{"paths":["src/lib.rs","src/tui.rs"]}'
```

`POST /v1/diagnostics` accepts optional `cwd`, `changed`, and `paths` fields
and returns schema `deepseek.runtime.diagnostics.v1` with `skipped`, `files`,
and a serialized diagnostics `report`. The HTTP runtime process holds a warmed
diagnostics broker, so repeated requests for the same workspace reuse the same
stdio LSP session when the selected language server is available. HTTP TUI
sessions use this endpoint for `diagnostics [--changed|paths...]` instead of
spawning a separate local diagnostics process.

## Rollback Snapshots

`deepseek restore` is a local rollback surface for git worktree changes. It is
separate from the HTTP durable runtime and is intended as the first slice of
turn-revert support:

```bash
deepseek restore snapshot before-risky-turn
deepseek restore list
deepseek restore show <snapshot-id-or-runtime-turn-id> --patch
deepseek restore revert-turn <snapshot-id-or-runtime-turn-id>
deepseek restore revert-turn <snapshot-id-or-runtime-turn-id> --apply
```

Snapshots are stored under `.dscode/rollback/snapshots/<snapshot-id>/` with a
manifest, `status.txt`, binary-safe `diff.patch`, `staged.patch`, and
`unstaged.patch` files, and captured untracked regular files under
`untracked/`. Restoring a snapshot verifies that the current git `HEAD` still
matches the commit captured in the snapshot. Dry-run is the default; `--apply`
reverses the current tracked diff, restores the snapshot staged index and
unstaged worktree split, restores captured untracked files, lists the restored
changed files, and runs a post-restore diagnostic pass for those files.

When `deepseek exec` runs inside a git worktree, it creates a pre-run rollback
snapshot and, after a successful run, binds that snapshot to the durable runtime
assistant turn id. TUI-started agent runs also create a pre-run snapshot when
started from a git worktree and bind it to the running assistant turn as soon as
the turn exists. REPL prompts also create a pre-turn snapshot when possible and
expose the latest one through `/restore show last` and `/revert_turn last`.
`restore show` and `restore revert-turn` accept either the snapshot id or that
runtime turn id.

In local file-backed TUI sessions, the same rollback surface is available from
the command palette:

```text
restore snapshot [label]
restore list [limit]
restore show <snapshot-id-or-runtime-turn-id|last>
revert turn <snapshot-id-or-runtime-turn-id|last> [--apply]
```

`last` resolves to the active thread's latest durable turn id. These commands
are intentionally local-only because rollback applies to the client's git
worktree; `deepseek tui --runtime-url ...` reports rollback as unsupported
instead of mutating a remote host implicitly.

Current boundaries are explicit:

- untracked restore currently covers regular files, not symlinks or directories;
- untracked files created after the snapshot are not cleaned unless they became
  tracked changes in the git diff;
- rollback storage under `.dscode/rollback` is excluded from untracked capture;
- older snapshots without split patch files restore through the legacy combined
  `diff.patch` path and do not recover staged-index fidelity;
- automatic turn binding currently covers `deepseek exec` and TUI-started agent
  runs, not REPL live turns;
- `deepseek diagnostics --watch`, the generated diagnostics service template,
  agent-loop post-edit diagnostics, and `serve --http` `/v1/diagnostics` reuse
  warmed stdio LSP sessions inside their owning process. Cross-process
  diagnostics now goes through the HTTP runtime broker; a dedicated standalone
  diagnostics daemon protocol is still future work.

## Durable Runtime Roadmap

The durable runtime should expose these logical records before the TUI depends
on it for full parity:

| Record | Required fields |
|---|---|
| `session` | `id`, `created_at`, `updated_at`, `title`, `workspace`, `status`, `active_thread_id`, `thread_count` |
| `thread` | `id`, `session_id`, `created_at`, `updated_at`, `title`, `workspace`, `model`, `mode`, `status`, `latest_turn_id`, `event_seq` |
| `turn` | `id`, `thread_id`, `index`, `role`, `content`, `status`, `created_at` |
| `event` | `id`, `thread_id`, `turn_id`, `seq`, `kind`, `payload`, `created_at` |
| `task` | `id`, `session_id`, `thread_id`, `parent_task_id`, `kind`, `status`, `summary`, `created_at`, `updated_at` |
| `automation` | `id`, `session_id`, `thread_id`, `name`, `status`, `schedule`, `prompt`, `created_at`, `updated_at`, `last_run_at`, `next_run_at` |
| `tool_call` | `id`, `turn_id`, `tool`, `input_json`, `output`, `status` |
| `usage` | `id`, `thread_id`, `turn_id`, `model`, `source`, `prompt_tokens`, `completion_tokens`, `total_tokens`, `prompt_cache_hit_tokens`, `prompt_cache_miss_tokens`, `estimated_input_cost_microusd`, `estimated_output_cost_microusd`, `estimated_total_cost_microusd`, `pricing_source`, `created_at` |

SQLite is the target backing store once dependency and release strategy are
explicit. Automation metadata plus manual trigger-to-task handoff are available;
schedule evaluation, built-in worker execution, and notification delivery remain
future work. Usage is available for token counts, cache telemetry,
recognized-model cost estimates, aggregate accounting, and 1M-context policy
through the summary endpoint.

## Public Readiness Checklist

Before marking a runtime surface public:

- `deepseek doctor --json` emits valid JSON without live network probes.
- `deepseek serve --http` exposes `/health` and `/runtime` with truthful
  capability flags.
- Every new endpoint has a documented schema, error shape, and versioned path.
- Mutation endpoints have approval and audit behavior described before release.
- Durable records have migration rules and tests for old records.
- Release notes include `deepseek doctor --json`, `/health`, and `/runtime`
  output from the release artifact.
- `cargo fmt --check`, `cargo test`, and `deepseek benchmark` pass.
- Any TUI/workbench feature depending on runtime state names the exact
  capability flag it requires.
