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
trigger current-thread automations into pending runtime tasks. Local
file-backed TUI sessions also expose a full-width MCP manager screen through
`mcp` / `mcp manager`, project-level MCP manager commands (`mcp init`,
`mcp add stdio|http|sse`, `mcp enable|disable|remove`, and `mcp validate`)
that operate on `.dscode/mcp.json`, plus
`mcp manager tools|prompts|resources|resource-templates [server]` detail views
that render configured MCP discovery output in the full-width manager screen.
The manager renders overview/tools/prompts/resources/templates/health tabs,
supports `mcp manager tab <tab>` switching, and filters visible manager lines
with `mcp manager filter <query>`.
The shorter `mcp tools|prompts|resources|resource-templates [server]` commands
keep the scrollable right-side panel for quick lookup.
`Esc` or `mcp close` returns the panel to the main workbench. Unscoped
TUI MCP mutation commands target project config; `mcp user ...` targets the
user MCP config for add/enable/disable/remove. `mcp validate` reports per-server
tools/prompts/resources/resource-template health in the same scrollable
right-side panel.
HTTP-runtime TUI sessions report that MCP manager commands require local
file-backed TUI. It is not yet a complete runtime client: HTTP-runtime TUI
sessions now use the aggregate runtime SSE stream for foreground refresh across
known and newly created threads, but fully interrupting a blocked model socket
read, richer progress controls, and general external command execution remain
future work.

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
HTTP, and follows `/v1/events/stream?follow=1` for lower-latency foreground
refresh across known and newly created threads.

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
| `/v1/events/stream` | `GET`, `HEAD` | Aggregate SSE replay, bounded wait, or follow-mode streaming across runtime threads |
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
    "/v1/events/stream",
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
    "events_sse_wait": true,
    "events_sse_follow": true,
    "events_global_sse": true,
    "events_global_sse_follow": true,
    "diagnostics": true,
    "diagnostics_changed": true,
    "diagnostics_broker": true,
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

## MCP Stdio Server

`deepseek serve --mcp` exposes a local MCP stdio server for clients that want
DeepSeekCode's workspace and runtime inspection tools without screen-scraping
the TUI. It stays read-only by default.

Quick registration:

```bash
deepseek mcp add-self
deepseek mcp add-self --name deepseek-code --workspace /path/to/workspace
```

`mcp add-self` resolves the current binary path and writes a stdio server entry
that launches `deepseek serve --mcp`. By default it writes the user MCP config;
use `--project` to write the current workspace config instead.

The default surface is intentionally read-only. It supports:

- `initialize`
- `notifications/initialized`
- `tools/list`
- `tools/call`
- `prompts/list`
- `prompts/get`
- `resources/list`
- `resources/templates/list`
- `resources/read`

Exposed tools:

| Tool | Purpose |
|---|---|
| `list_files` | List workspace files with depth and result limits |
| `list_dir` | DeepSeek-TUI-compatible alias for listing workspace files and directories |
| `read_file` | Read a UTF-8 file with line numbers |
| `retrieve_tool_result` | Retrieve spilled large tool outputs by ref with summary/head/tail/lines/query modes |
| `search_text` | Literal text search in the workspace |
| `grep_files` | DeepSeek-TUI-compatible literal text search alias |
| `file_search` | Find workspace files by filename or path with optional extension filters |
| `web_run` | DeepSeek-TUI-style aggregate web wrapper for search/image/open/click/find/finance/PDF screenshot |
| `web_search` | Search the web and return ranked URLs/snippets |
| `fetch_url` | Fetch a known HTTP/HTTPS URL and return decoded text or raw content |
| `finance` | Fetch a live stock, ETF, index, or crypto quote |
| `pandoc_convert` | Convert workspace documents via local `pandoc` |
| `image_ocr` | Extract text from workspace images via local `tesseract` |
| `image_analyze` | Hidden by default; exposed with trusted `DSCODE_MCP_ENABLE_SIDE_EFFECTS=1` or durable runtime approvals, and analyzes workspace images through an OpenAI-compatible vision model |
| `git_status` | Show concise git status for the workspace |
| `git_diff` | Show working-tree or staged diff, optionally scoped by path/context |
| `project_map` | Render a high-level tree, summary, and key files |
| `validate_data` | Validate JSON or TOML from inline content or a file |
| `git_log` | Read recent git history |
| `git_show` | Show one commit/ref with patch |
| `git_blame` | Read blame for a file and line range |
| `load_skill` | Load a configured TOML skill by name with policy, references, suggested steps, and system prompt context |
| `request_user_input` | Validate and render a DeepSeek-TUI-style user-input request for MCP/ACP clients to surface |
| `notify` | Fire a single terminal attention signal for long-running task completion or user attention |
| `github_issue_context` | Read GitHub issue metadata, body, labels, assignees, and optional comments through `gh` |
| `github_pr_context` | Read GitHub PR metadata, comments, reviews, checks, files, and optional patch diff through `gh` |
| `review` | Run deterministic local code review over a workspace file, git diff, or supplied GitHub PR context |
| `pr_review_comment_plan` | Convert structured review JSON plus optional PR context into a read-only GitHub PR comment body and evidence plan |
| `recall_archive` | Search durable runtime threads, turns, and items for prior context |
| `tool_search_tool_regex` | Search the static DeepSeekCode tool catalog with a lightweight regex-like pattern |
| `tool_search_tool_bm25` | Rank static DeepSeekCode tools by local term matching over names, descriptions, and schemas |
| `diagnostics` | Run workspace or path-scoped diagnostics |
| `run_tests` | Hidden by default; exposed with trusted `DSCODE_MCP_ENABLE_SIDE_EFFECTS=1` or durable runtime approvals, and runs supported test commands through the existing shell approval path |
| `run_shell` | Hidden by default; exposed with trusted `DSCODE_MCP_ENABLE_SIDE_EFFECTS=1` or durable runtime approvals, and still limited by the existing safe-command allowlist |
| `exec_shell_list` | List in-process background shell jobs |
| `exec_shell_show` | Show one in-process background shell job snapshot |
| `exec_shell_wait` | Wait for or poll one background shell job and return incremental output |
| `exec_wait` | Alias for `exec_shell_wait` |
| `task_shell_wait` | DeepSeek-TUI-compatible wait/poll helper for `task_shell_start` jobs |
| `exec_shell` | Hidden by default; exposed with trusted `DSCODE_MCP_ENABLE_SIDE_EFFECTS=1` or durable runtime approvals, and starts foreground/background safe shell commands |
| `task_shell_start` | Hidden by default; exposed with trusted `DSCODE_MCP_ENABLE_SIDE_EFFECTS=1` or durable runtime approvals, and starts background safe shell commands |
| `exec_shell_interact` | Hidden by default; exposed with trusted `DSCODE_MCP_ENABLE_SIDE_EFFECTS=1` or durable runtime approvals, and sends stdin to a background shell job |
| `exec_interact` | Alias for `exec_shell_interact` |
| `exec_shell_cancel` | Hidden by default; exposed with trusted `DSCODE_MCP_ENABLE_SIDE_EFFECTS=1` or durable runtime approvals, and cancels one or all background shell jobs |
| `rlm_chunk_plan` | Plan DeepSeek-TUI-style RLM chunks for a workspace file or inline content without running child agents |
| `rlm_map_reduce_plan` | Plan a local RLM map-reduce workflow without running child agents |
| `rlm_recursive_plan` | Plan a multi-round recursive RLM map/reduce workflow without running child agents |
| `rlm_python` | Run restricted pure-compute Python helper code with imports/files/network/subprocess blocked |
| `rlm_python_sessions` | List or inspect persisted `rlm_python_session` JSON state without running Python |
| `rlm_python_session` | Hidden by default; exposed with trusted `DSCODE_MCP_ENABLE_SIDE_EFFECTS=1` or durable runtime approvals, and writes `.dscode/rlm-python` helper state |
| `rlm` | Hidden by default; exposed with trusted `DSCODE_MCP_ENABLE_SIDE_EFFECTS=1` or durable runtime approvals, and runs bounded model-backed RLM child analysis |
| `rlm_query` | Alias for `rlm` |
| `llm_query` | Alias for `rlm` |
| `rlm_process` | Hidden by default; exposed with trusted `DSCODE_MCP_ENABLE_SIDE_EFFECTS=1` or durable runtime approvals, and runs bounded model-backed long-input RLM analysis |
| `rlm_batch` | Hidden by default; exposed with trusted `DSCODE_MCP_ENABLE_SIDE_EFFECTS=1` or durable runtime approvals, and runs batched bounded model-backed RLM child analyses |
| `rlm_query_batched` | Alias for `rlm_batch` |
| `llm_query_batched` | Alias for `rlm_batch` |
| `apply_patch` | Hidden by default; exposed only with durable runtime approvals and applies unified diffs through the existing patch validator |
| `write_file` | Agent-visible write tool; hidden in MCP/ACP by default and exposed there only with durable runtime approvals; writes UTF-8 text to safe relative paths |
| `note` | Hidden by default; exposed only with durable runtime approvals and appends to the configured notes file |
| `remember` | Hidden by default; exposed only when memory is enabled and durable runtime approvals are available; appends to the configured memory file |
| `edit_file` | Agent-visible write tool for exact search/replace in one UTF-8 file under the workspace |
| `fim_edit` | Agent-visible DeepSeek-TUI-compatible Fill-in-the-Middle file edit tool using prefix/suffix anchors |
| `delete_file` | Hidden by default; exposed only with durable runtime approvals and deletes one regular file at a safe relative path under the MCP workspace |
| `copy_file` | Hidden by default; exposed only with durable runtime approvals and copies one regular file between safe relative paths under the MCP workspace |
| `move_file` | Hidden by default; exposed only with durable runtime approvals and moves one regular file between safe relative paths under the MCP workspace |
| `revert_turn` | Hidden by default; exposed only with durable runtime approvals and restores files from rollback snapshots |
| `github_comment` | Hidden by default; exposed only with durable runtime approvals and posts evidence-backed GitHub issue/PR comments through `gh` |
| `github_pr_review_comment` | Hidden by default; exposed only with durable runtime approvals and posts evidence-backed inline PR review comments through `gh api` |
| `github_close_issue` | Hidden by default; exposed only with durable runtime approvals and closes completed GitHub issues through `gh` after structured evidence |
| `runtime_health` | Return MCP server health metadata |
| `runtime_list_sessions` | List durable runtime sessions |
| `runtime_list_threads` | List durable runtime threads |
| `runtime_read_thread` | Read one durable thread with turns and items |
| `runtime_list_tasks` | List durable runtime tasks |
| `runtime_read_task` | Read one durable runtime task |
| `runtime_create_task` | Hidden by default; exposed only with durable runtime approvals and creates durable runtime tasks |
| `runtime_cancel_task` | Hidden by default; exposed only with durable runtime approvals and cancels durable runtime tasks |
| `runtime_create_automation` | Hidden by default; exposed only with durable runtime approvals and creates durable runtime automations |
| `runtime_update_automation` | Hidden by default; exposed only with durable runtime approvals and updates durable runtime automations |
| `runtime_pause_automation` | Hidden by default; exposed only with durable runtime approvals and pauses durable runtime automations |
| `runtime_resume_automation` | Hidden by default; exposed only with durable runtime approvals and resumes durable runtime automations |
| `runtime_delete_automation` | Hidden by default; exposed only with durable runtime approvals and deletes durable runtime automations |
| `runtime_trigger_automation` | Hidden by default; exposed only with durable runtime approvals and triggers durable runtime automations into pending tasks |
| `runtime_list_agents` | List durable runtime sub-agent tasks |
| `runtime_agent_result` | Read one durable runtime sub-agent snapshot |
| `runtime_spawn_agent` | Hidden by default; exposed only with durable runtime approvals and creates a thread plus pending sub-agent task |
| `runtime_cancel_agent` | Hidden by default; exposed only with durable runtime approvals and cancels a durable runtime sub-agent task |
| `runtime_close_agent` | Hidden by default; exposed only with durable runtime approvals and closes a durable runtime sub-agent task |
| `runtime_resume_agent` | Hidden by default; exposed only with durable runtime approvals and resumes or forks a durable runtime sub-agent task |
| `runtime_send_agent_input` | Hidden by default; exposed only with durable runtime approvals and appends input plus a follow-up sub-agent task |

`image_analyze` is hidden by default because it can spend model tokens and make
network calls to an external vision API; durable MCP mode routes it through
`permission_request kind=mcp` before execution. `note` and `remember` are
hidden by default because they append durable note or memory files; durable MCP
mode routes them through `permission_request kind=write` before execution.

Exposed MCP prompts:

| Prompt | Purpose |
|---|---|
| `review_code` | Review a file or code area for bugs, regressions, maintainability, and test gaps |
| `explain_code` | Explain how a file, module, or symbol works |
| `plan_task` | Create an implementation plan for a coding task in the current workspace |

Exposed MCP resources:

| Resource URI | Purpose |
|---|---|
| `file://<workspace>` | Workspace root metadata |
| `deepseekcode://runtime/sessions/<id>` | Durable runtime session JSON |
| `deepseekcode://runtime/threads/<id>` | Durable runtime thread with turns and items |
| `deepseekcode://runtime/tasks/<id>` | Durable runtime task JSON |

Exposed MCP resource templates:

| Resource URI template | Purpose |
|---|---|
| `deepseekcode://runtime/sessions/{id}` | Durable runtime session JSON by id |
| `deepseekcode://runtime/threads/{id}` | Durable runtime thread JSON by id |
| `deepseekcode://runtime/tasks/{id}` | Durable runtime task JSON by id |

`run_shell`, `run_tests`, `exec_shell`, `task_shell_start`,
`exec_shell_interact`, `exec_interact`, `exec_shell_cancel`,
`rlm_python_session`, `rlm`, `rlm_query`, `llm_query`, `rlm_process`,
`rlm_batch`, `rlm_query_batched`, `llm_query_batched`, `apply_patch`,
`write_file`, `edit_file`, `delete_file`, `copy_file`, and `move_file` are
hidden from `tools/list` and rejected by `tools/call` unless the MCP server
process opts in.
`DSCODE_MCP_ENABLE_SIDE_EFFECTS=1` keeps the trusted direct execution path for
allowlisted shell/test/shell-session tools. `DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1`
creates a runtime approval thread for that server and routes shell-session
starts/stdin/cancel calls through durable `permission_request kind=shell` /
`permission_response` events alongside the existing `run_shell`, `run_tests`,
patch, RLM session-state, model-running RLM, and file-write approval paths, so
the existing TUI approval modal or HTTP runtime can approve or deny the call.
Operators can also bind an existing runtime thread with
`DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id>`. All modes reuse the existing
safe-command allowlist, patch scope validation, and workspace path checks; they
do not expose arbitrary shell, unrestricted file writes, task mutation, or MCP
prompt/resource mutation.

DeepSeekCode also acts as an MCP client for configured stdio / HTTP / SSE
servers. In addition to `tools/list`, `tools/call`, `prompts/list`, and
`prompts/get`, the client now supports `resources/list`, `resources/read`,
and `resources/templates/list` for parameterized resource URI templates.

```bash
deepseek mcp resources [server-name]
deepseek mcp resource-templates [server-name]
deepseek mcp resource <server-name> <resource-uri>
```

When a project or user MCP config exists, agent runs expose read-only bridge
tools `mcp_list_prompts`, `mcp_get_prompt`, `mcp_list_resources`,
`mcp_read_resource`, and `mcp_list_resource_templates` alongside
`mcp_list_tools` and `mcp_call`.

## ACP Stdio Adapter

`deepseek serve --acp` exposes a conservative Agent Client Protocol stdio
adapter for editors and local clients that speak newline-delimited JSON-RPC.

The first slice supports:

- `initialize`
- `session/new`
- `session/list`
- `session/load`
- `session/checkpoints`
- `session/checkpoint/read`
- `session/checkpoint/restore`
- `session/tools/list`
- `session/tools/call`
- `session/prompt`
- `session/cancel`
- `shutdown`

Prompt requests are sent through the configured DeepSeek model client with no
tool surface exposed. The adapter emits a `session/update` agent message chunk
and then returns `{"stopReason":"end_turn"}` for `session/prompt`.
`session/list` returns durable runtime sessions from the configured workspace,
and `session/load` maps a runtime `sessionId` plus optional `threadId` into an
in-process ACP session using that session/thread workspace. When a loaded ACP
session is prompted, DeepSeekCode records user and assistant turns/items back to
that runtime thread; token usage is recorded with source `acp` when the model
provider returns usage metadata. `initialize` advertises checkpoint
read/restore/apply support through `sessionCapabilities.checkpoints`;
`session/checkpoints` lists rollback checkpoints, and when called with a loaded
`sessionId` it filters to checkpoints bound to that runtime thread.
`session/checkpoint/read` returns the checkpoint manifest and can include the
unified diff with `includePatch=true`. `session/checkpoint/restore` returns a
structured restore plan; it is dry-run by default and only mutates the git
worktree when the client passes `apply=true`. When `sessionId` or `threadId` is
provided, restore is scoped to checkpoints bound to that runtime thread.
`session/tools/list` and `session/tools/call` expose the same workspace/runtime
tool bridge as the MCP stdio server, scoped to the ACP session workspace.
Read-only tools are available for any ACP session. `run_shell`, `exec_shell`,
`task_shell_start`, `exec_shell_interact`, `exec_interact`,
`exec_shell_cancel`, `apply_patch`, `write_file`, `edit_file`, `delete_file`,
`copy_file`, and `move_file` are available only when the ACP session is loaded
from a runtime thread, and they reuse that thread's durable permission events
before mutating the workspace. Loaded-session tool calls also create an
assistant turn with `tool_call` and `tool_result` runtime items; side-effect
permission requests are linked to that same turn for auditability. ACP
`session/tools/call` now emits standard-shaped `session/update` notifications
before the final JSON-RPC result: an initial `sessionUpdate: "tool_call"` with
`toolCallId`, title, kind, status, and `rawInput`, followed by
bounded intermediate `sessionUpdate: "tool_call_update"` progress chunks for
large tool outputs. `exec_shell` and `task_shell_start` also support opt-in live
stdout/stderr streaming with `stream=true` or `follow=true`; the adapter flushes
partial `tool_call_update` deltas while the background shell job is still
running, then sends a final `tool_call_update` with the matching `toolCallId`,
final status, complete text content, and `rawOutput`.
Loaded-session updates include the runtime turn/item ids under `_meta.runtime`
so clients can align incremental UI state with the durable audit trail.

```bash
deepseek serve --acp
deepseek serve --acp --workspace /path/to/workspace
```

Use `serve --http` for the durable runtime API and `serve --mcp` when another
client needs DeepSeekCode's tools as MCP tools.

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
reasoning summaries inside subsequent model requests during the same run, and
runtime-backed TUI/daemon agent runs preload the latest persisted reasoning
items from the active durable thread so later runs can see prior thinking
continuity. In the TUI, `reasoning`, `reasoning latest`,
`reasoning show <selector>`, and `reasoning search <query>` render persisted
reasoning in the scrollable right-side detail panel, while
`reasoning replay <0..20>` controls how many latest persisted reasoning entries
local TUI-started agent runs preload. `reasoning pin <selector>`,
`reasoning pins`, and `reasoning unpin <selector|all>` keep selected reasoning
turns in local replay even after they fall outside the latest-N window.

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
from older turns. Manual requests with `summary` use `summary_source =
"provided"`; daemon-generated model summaries use `summary_source = "model"`.
`keep_tail_turns` defaults to `8` and is capped at `200`; the request is
rejected if it leaves no turns to summarize. The response uses schema
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
`thread_compacted`, `permission_request`, `permission_response`,
`user_input_request`, `user_input_response`, and `cancel_requested`.

`POST /v1/threads/{id}/events` currently accepts `permission_request`,
`permission_response`, `user_input_request`, `user_input_response`, and
`cancel_requested` events. `permission_request` uses the same core payload
fields as `exec --json` permission notifications and is consumed by
`deepseek tui`:

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

`user_input_request` records structured clarification questions for the TUI:

```json
{
  "type": "user_input_request",
  "questions": [
    {
      "header": "Mode",
      "id": "mode",
      "question": "Which execution mode should be used?",
      "options": [
        {"label": "Plan", "description": "Draft a plan first."},
        {"label": "Apply", "description": "Implement directly."}
      ]
    }
  ]
}
```

`user_input_response` records selected option labels by question id:

```json
{
  "type": "user_input_response",
  "request_id": "event-...",
  "answers": {
    "mode": "Plan"
  }
}
```

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

`GET /v1/events/stream` is the aggregate form for foreground clients that need
cross-thread push. It accepts the same `wait_ms`, `poll_ms`, `follow`,
`max_events`, and `max_ms` options. A `since=thread-1:4,thread-2:9` cursor
resumes each known thread independently, while `since_seq=N` applies one default
cursor to threads not listed in `since`. Aggregate frames use `thread_id:seq` as
the SSE `id`, so a connected TUI can receive newly created thread events without
first discovering and subscribing to that thread.

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
latest usage record crosses the 800k-token warning threshold. When the
configured model API key environment variable is present, daemon compaction asks
the model for a concise older-context summary and records
`summary_source = "model"`; if the key is absent or summary generation fails, it
falls back to the deterministic extractive summary so compaction still
proceeds. The daemon skips a thread when a `thread_compacted` event already
exists after the latest `usage_recorded` event, so repeated ticks do not append
duplicate summaries.

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
Agent-visible todo tools include `todo_write`, `todo_add`, `todo_update`, and
`todo_list`; DeepSeek-TUI-compatible `update_plan` maps structured
`{explanation, plan:[{step,status}]}` updates onto the same in-memory list, and
checklist aliases `checklist_write`, `checklist_add`, `checklist_update`, and
`checklist_list` operate on it as well.
Agent-visible durable work tools include DeepSeek-TUI-compatible `task_create`,
`task_list`, `task_read`, `task_cancel`, `task_gate_run`, `automation_create`,
`automation_list`, `automation_read`, `automation_update`, `automation_pause`,
`automation_resume`, `automation_delete`, and `automation_run`, backed by
`.dscode/runtime`. Task creation/cancellation and automation
creation/update/pause/resume/delete/run use write approval, `task_gate_run` uses
shell approval, and task/automation reads are approval-free. MCP server mode now
also exposes durable-approval `runtime_create_task`, `runtime_cancel_task`, and
`runtime_*_automation` tools for task queue and automation metadata writes.
`automation_create` accepts DeepSeek-TUI-style `name`/`prompt`/`rrule` inputs
and stores the recurrence in the local runtime `schedule` field; `schedule`
remains accepted as
a local alias. `automation_update` accepts `name`, `prompt`, `rrule`/`schedule`,
`status`, `paused`, and `next_run_at`.
DeepSeek-TUI-compatible sub-agent lifecycle tools are also exposed on the
agent-visible surface: `agent_spawn`, `agent_result`, `agent_list`,
`agent_cancel`, `close_agent`, `resume_agent`, and `send_input`. DeepSeekCode
maps these to durable runtime tasks: `agent_spawn` creates a runtime thread and
pending `subagent` task, `agent_result`/`agent_list` read task/thread snapshots,
`agent_cancel`/`close_agent` cancel pending or running sub-agent tasks,
`resume_agent` requeues work, and `send_input` appends a user message to the
sub-agent thread while queuing a follow-up `subagent_input` task. Mutation tools
use the same write-approval policy as other runtime state changes. MCP server
mode mirrors the lifecycle with `runtime_list_agents`, `runtime_agent_result`,
and durable-approval `runtime_spawn_agent`, `runtime_cancel_agent`,
`runtime_close_agent`, `runtime_resume_agent`, and `runtime_send_agent_input`.
PR attempt evidence tools include DeepSeek-TUI-compatible
`pr_attempt_record`, `pr_attempt_list`, `pr_attempt_read`, and
`pr_attempt_preflight`. They store attempt metadata and captured patch
artifacts under `.dscode/runtime/pr_attempts`; preflight runs
`git apply --check` against the recorded patch and reports `would_apply`
without mutating the worktree.
Agent-visible user clarification includes DeepSeek-TUI-compatible
`request_user_input`. It validates 1-3 structured questions with 2-3 labeled
options each. Plain non-runtime CLI runs return a non-mutating
`meta.user_input_required=true` summary so the model can stop and ask the user
for the requested selections. Runtime-backed TUI and daemon task runs append a
durable `user_input_request`, wait for the matching `user_input_response`, and
return the selected answers to the next model step as `answers_json`.
Runtime threads also support appending both event kinds through
`POST /v1/threads/{id}/events`; local and remote TUI snapshots render
unresolved requests in a user-input modal. Number keys select labeled options,
and `o` opens a short free-form Other answer that is submitted into the same
structured response event.
DeepSeek-TUI-compatible `recall_archive` searches durable runtime threads,
turns, items, and compaction summaries for older context that may have fallen
out of the current prompt. It accepts `query`, optional `thread_id`, and
`max_results`/`limit`; the DeepSeek-TUI `cycle` argument is accepted at the
schema layer for compatibility, while local recall is backed by `.dscode/runtime`
instead of numbered cycle JSONL files.
Agent-visible deferred discovery includes DeepSeek-TUI-compatible
`tool_search_tool_regex` and `tool_search_tool_bm25`. They search the static
DeepSeekCode tool schema catalog by name, description, and parameter schema, and
return `tool_search_tool_search_result` payloads with `tool_reference` items.
Agent-visible local code review includes DeepSeek-TUI-compatible `review`.
This first slice supports safe relative files and git diffs, returns structured
`summary` / `issues` / `suggestions` / `overall_assessment` JSON, and reports
deterministic markers such as conflict markers, panic-prone Rust calls, debug
prints, and broad `unsafe` usage. It also reports local behavioral signals for
source diffs without test changes, public API changes, dependency/configuration
changes, and public API files without local test markers. Set `semantic=true`
to run an additional read-only child-agent semantic review over the same source
and deterministic evidence; optional `steps`, `agent`, and `skill` tune that
review pass. Remote PR review first gathers context with
`github_pr_context include_diff=true`, then passes that output into
`review target=github_pr_context github_context=<context>` so the same structured
review pipeline can inspect the PR diff without fetching GitHub data itself.
When the context includes PR JSON, `review` also reports remote review blockers
such as requested changes, failing/cancelled status checks, and missing
`include_diff=true` context. The offline planner has an explicit remote PR
review route: when `github_pr_context` and `review` are both available, PR review
tasks gather `github_pr_context include_diff=true` first and then run `review`
over the gathered context. If the PR task explicitly asks for a semantic, deep,
thorough, behavioral, real-bug, or logic-bug review, the planner sets
`semantic=true` on that review call so the read-only child-agent semantic pass is
included. If the task asks to draft or prepare a PR comment and
`pr_review_comment_plan` is available, the planner turns the structured review
JSON plus optional PR context into Markdown body text, evidence JSON, and a
dry-run `github_comment` input. If the task explicitly says to post, publish,
leave, add, submit, or send the comment, the planner can pass that prepared
input to the guarded `github_comment` write tool with `dry_run=false`; when the
task explicitly asks for inline/line/file/diff comments and the comment plan has
line-level findings plus a PR head commit SHA, the planner can instead pass the
prepared batch to the guarded `github_pr_review_comment` tool. The normal
write-approval path still controls whether any GitHub mutation runs.
Agent-visible skill tooling includes DeepSeek-TUI-compatible `load_skill`.
DeepSeekCode maps that tool onto its existing TOML skill registry: repo skills
and the configured `workspace.user_skills_dir` are searched with user skills
overriding repo skills, and the tool returns the selected skill's context,
references, suggested steps, initial todos, and policy.
Agent-visible persistent notes include DeepSeek-TUI-compatible `note`, which
appends durable maintainer/agent context to `memory.notes_path`. User memory is
opt-in with `memory.enabled = true` or `DSCODE_MEMORY=on`; when enabled,
DeepSeekCode loads `memory.memory_path` into the system prompt and exposes
DeepSeek-TUI-compatible `remember` for timestamped single-sentence memory
bullets. Local file-backed TUI sessions also mirror DeepSeek-TUI's fast memory
workflow: composer lines beginning with a single `#` append to user memory
without starting a model turn, and `/memory show|path|clear|edit|help` plus
command-palette `memory ...` inspect or manage the same file.
Agent-visible notifications include DeepSeek-TUI-compatible `notify`. It
requires a short `title`, accepts an optional `body`, emits a terminal bell by
default, and is silent when `DSCODE_NOTIFY=off`.

Agent-visible web tools include DeepSeek-TUI-compatible `web_run`,
`web_search`, and `fetch_url`. `web_run` is an aggregate compatibility wrapper
for `search_query`, `image_query`, stored-ref or direct-URL `open`, numbered
link `click`, stored-ref or URL-scoped `find`, `finance`, and cached-PDF
`screenshot` arrays. Search results are stored as `searchN` and
`turnNsearchN`; opened pages are cached as `openN` and `turnNopenN`; clicked
pages are cached as `clickN` and `turnNclickN`. Static HTML links are extracted
without executing JavaScript or preserving browser cookies. `open` and `click`
honor `lineno` and top-level `response_length` by returning line-numbered page
windows (`short` 40 lines, `medium` 80, `long` 160) while keeping the full page
in cache for `find` and later navigation. Opened PDF responses cache page text
through local `pdftotext` / Poppler, and `screenshot` returns a requested cached
PDF `pageno`; browser/DOM bitmap screenshots are still out of scope.
`web_search` accepts `query`, `q`, or a JSON
`search_query` compatibility array and currently uses DuckDuckGo HTML search
unless `DSCODE_WEB_SEARCH_URL_TEMPLATE` is set. `image_query` uses DuckDuckGo
Images unless `DSCODE_IMAGE_SEARCH_URL_TEMPLATE` is set. `fetch_url` supports
`format=text`, `markdown`, or `raw`. DeepSeek-TUI-compatible `finance` accepts
`ticker` or `symbol`, maps common `type=crypto` bare tickers to `-USD`, and uses
a Yahoo Finance-compatible quote endpoint unless `DSCODE_FINANCE_URL_TEMPLATE`
is set. These tools are read-only network tools and block localhost/private
hosts by default; set `DSCODE_ALLOW_LOCAL_FETCH=1` only for trusted local
testing. They also honor a DeepSeek-TUI-style host policy from
`.dscode/config.toml` or environment overrides:

```toml
network.default = "allow" # allow | deny | prompt
network.allow = ["api.deepseek.com", ".example.com"]
network.deny = ["tracking.example.com"]
network.audit = true
network.audit_path = "~/.config/dscode/network-audit.log"
```

`network.deny` wins over `network.allow`. A leading dot matches subdomains but
not the apex domain, so `.example.com` matches `docs.example.com` but not
`example.com`. When `network.default = "prompt"`, AgentLoop/runtime/TUI
execution emits a `permission_request` with `kind = "network"` for the host list
and, after approval, marks that single tool invocation as network-approved.
Direct tool execution without the registry approval path still fails closed with
a clear approval-required error; set `DSCODE_AUTO_APPROVE_NETWORK=1` only in
trusted non-interactive runs. When `network.audit = true`, each attempted web
fetch appends a best-effort plaintext audit line with timestamp, host, tool
name, and decision; audit write failures never block the network tool. The same
settings can be overridden with comma-separated `DSCODE_NETWORK_ALLOW` /
`DSCODE_NETWORK_DENY`, `DSCODE_NETWORK_DEFAULT`, `DSCODE_NETWORK_AUDIT`, and
`DSCODE_NETWORK_AUDIT_PATH`.

Agent-visible document tools include DeepSeek-TUI-compatible `pandoc_convert`
and `image_ocr`. `pandoc_convert` converts a workspace `source_path` to a
whitelisted `target_format` through local `pandoc`; text formats can return
inline output, while `docx`, `odt`, and `epub` require `output_path`.
`image_ocr` runs local `tesseract <image> -` and returns extracted text inline.
Both tools are also exposed through MCP/ACP; `pandoc_convert output_path=...`
requires durable write approvals in those modes. Both tools report clear
missing-binary errors if the corresponding local dependency is not installed.

`image_analyze` reads a workspace-relative `image_path`, base64-encodes PNG,
JPEG, GIF, WebP, or BMP content, and sends it to the configured
OpenAI-compatible vision `/chat/completions` endpoint. Configure
`vision.base_url`, `vision.model`, and `vision.api_key_env` in
`.dscode/config.toml`, or override them per call with `base_url`, `model`, and
`api_key_env`.

Agent-visible GitHub context tools include DeepSeek-TUI-compatible
`github_issue_context` and `github_pr_context`. They call the GitHub CLI
through argv, accept optional `repo` / `repository` scoping, and keep output
bounded with `max_chars`; PR context can include a bounded `gh pr diff --patch`
excerpt with `include_diff=true`.
GitHub mutation tools `github_comment`, `github_pr_review_comment`, and
`github_close_issue` require write approval. Comments require a non-empty
evidence JSON object; inline PR review comments also require a PR number,
line-level comments with `path`, `line`, and `body`, plus a PR head `commit_id`
or per-comment `commit_id`. Issue closure also requires acceptance criteria plus
`files_changed`, `tests_run`, and `final_status` evidence, and refuses dirty
worktrees unless `allow_dirty=true`. For PR review comments,
`pr_review_comment_plan` can prepare the top-level body, evidence, dry-run
`github_comment_input`, and, when enough line-level context exists, a dry-run
`github_pr_review_comment_input` without invoking `gh`. The offline planner
performs write handoff only for explicit post/publish/leave/add/submit/send
wording; draft and prepare tasks stop at the read-only plan. If a guarded
`github_comment` or `github_pr_review_comment` attempt fails or is denied, the
planner does not blindly resend the same mutation. It rebuilds a fresh
`pr_review_comment_plan` with the previous comment error recorded in the comment
body and evidence, leaving the next retry to an explicit follow-up and approval.

Agent runs also expose DeepSeek-TUI-compatible `revert_turn`. It restores
workspace files from rollback snapshots by `snapshot_id`, `checkpoint_id`,
`turn_id`, or recent `turn_offset`, and it supports `dry_run=true` /
`apply=false` previews. Restores mutate files but do not rewrite conversation
history, so the tool follows the existing write confirmation path.

Large successful tool outputs above 100 KiB are spilled to
`~/.deepseek/tool_outputs/` and replaced inline with a bounded head plus a
`retrieve_tool_result ref=<id>` hint. The `retrieve_tool_result` tool supports
`summary`, `head`, `tail`, `lines`, and `query` modes, with bounded byte,
match, and context-line limits so later turns can fetch only the relevant slice.

Agent runs also expose DeepSeek-TUI-compatible shell tool names:
`exec_shell`, `task_shell_start`, `task_shell_wait`, `exec_shell_wait`,
`exec_wait`, `exec_shell_interact`, `exec_interact`, and `exec_shell_cancel`.
Foreground `exec_shell` reuses the existing safe `run_shell` execution path.
`exec_shell background=true` and `task_shell_start` create in-process background
jobs, return a `task_id`, and can be polled by `task_shell_wait` or
`exec_shell_wait`, sent stdin, or cancelled by the companion tools. These
background jobs are not yet durable across process exits. MCP server mode
exposes `exec_shell_list`, `exec_shell_show`, `exec_shell_wait`, `exec_wait`,
and `task_shell_wait` as read-only tools by default, while `exec_shell`,
`task_shell_start`, `exec_shell_interact`, `exec_interact`, and
`exec_shell_cancel` require trusted side effects or durable runtime approvals.

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

Agent runs expose `dispatch_subagent` and `dispatch_subagents` for bounded child
analysis. They also expose `rlm` / `rlm_query` / `llm_query`, RLM-lite tools
that wrap either `context` + `question` inputs or DeepSeek-TUI-style
`task` + `file_path` / `content` long-input requests into the same bounded
child-agent execution path. `file_path` is workspace-relative only, `content`
is capped at 200k chars, and `max_depth` is accepted as a compatibility alias
for the child step budget. `rlm_process` is also exposed as the explicit
DeepSeek-TUI-compatible long-input process entrypoint; it currently uses the
same bounded child-agent adapter, not a full long-lived REPL loop.
`rlm_batch` / `rlm_query_batched` /
`llm_query_batched` map shared context plus up to 16 questions onto parallel
child analyses.
`rlm_chunk_plan` provides a read-only chunk planning helper for DeepSeek-TUI-style
map-reduce setup without needing Python. It accepts the same `file_path` or
`content` long-input shape, returns chunk start/end offsets, coverage metadata,
and chunk text by default, and supports `include_text=false` for offset-only
planning. `max_chars` defaults to 20000, `overlap` must be smaller than
`max_chars`, and the input remains capped at 200k chars.
`rlm_map_reduce_plan` adds the next read-only planning layer: given a `task`
plus `file_path` or `content`, it returns the same chunks, ready-to-dispatch map
task JSON for the first `map_limit` chunks, an omitted-map count, and a reduce
prompt for combining map outputs. It does not run child agents by itself.
`rlm_recursive_plan` extends that into a multi-round fan-in reduce tree for
larger inputs: it returns stable `map:<index>` and `roundN:groupM` refs,
initial map tasks, recursive reduce groups, omitted map-task metadata, and a
final output ref without running child agents.
The `rlm_python` helper adds a restricted Python execution slice for pure
calculation, text splitting, counting, and aggregation over optional `context`
(`ctx` alias) and `question` variables; it rejects import/file/network/subprocess-style
tokens, clamps timeout, and returns stdout plus JSON-serializable variables.
It also exposes DeepSeek-TUI-style `chunk_context(max_chars, overlap)` and
`chunk_coverage(chunks)` helpers for coverage-aware map-reduce setup, plus
REPL-compatible `SHOW_VARS()`, `repl_get()`, `repl_set()`, `FINAL()`, and
`FINAL_VAR()` helper names for prompt/code portability.
`rlm_python_session` adds an explicit `session_id` and persisted JSON `state`
dictionary under `.dscode/rlm-python/`. Safe identifier-shaped state keys are
also preloaded as locals on the next call, and JSON-serializable user locals are
saved back into state after each call, so simple REPL-style snippets such as
`count += 1` work across repeated calls without explicit `repl_set`. `reset=true`
clears that session before running. When `persistent=true` is set,
`rlm_python_session` also keeps a long-lived restricted Python REPL process for
the same `session_id` inside the current DeepSeekCode process; subsequent
persistent calls reuse the same Python PID while still writing the JSON state
file, and `reset=true` closes and rebuilds that cached process. `rlm_python_sessions`
lists those persisted helper sessions or inspects a specific `session_id`
without running Python, returning the JSON object state, file metadata, and
`process.active` / `process.pid` when a persistent REPL is alive in the current
DeepSeekCode process.
This gives the model DeepSeek-TUI-style Recursive Language Model entrypoints for
synthesis/classification tasks with both file-backed and optional process-backed
Python helper state. MCP server mode exposes the local RLM planning helpers
(`rlm_chunk_plan`, `rlm_map_reduce_plan`, `rlm_recursive_plan`), restricted pure-compute
`rlm_python`, and read-only `rlm_python_sessions` by default. Stateful
`rlm_python_session` is hidden by default and requires trusted side effects or
durable runtime approvals because it writes `.dscode/rlm-python` state.
Model-running child-agent RLM tools (`rlm`, `rlm_query`, `llm_query`,
`rlm_process`, `rlm_batch`, `rlm_query_batched`, and `llm_query_batched`) are
also hidden by default and require trusted side effects or durable
`permission_request kind=mcp` approvals because they can spend model tokens and
use networked model APIs.

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
deepseek diagnostics --json --changed
deepseek diagnostics --watch --changed
deepseek diagnostics --watch --json --interval-ms 750 src/lib.rs
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
systemd/launchd service set runs `deepseek diagnostics --watch --changed
--json` as an always-on local diagnostics worker for the workspace.

`deepseek diagnostics --json` emits one structured JSON object with schema
`deepseek.diagnostics.report.v1`. `deepseek diagnostics --watch --json` emits
one newline-delimited JSON object per tick with schema
`deepseek.diagnostics.daemon_tick.v1`, including `cwd`, `watch`, `tick`,
`changed`, `skipped`, `files`, and `report`. When `--changed` finds no files,
the JSON output still emits a tick with `skipped: true`, an empty `files` array,
and `report: null`, so supervisors can distinguish idle ticks from process
failures.

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
`unstaged.patch` files, captured untracked regular files under `untracked/`,
manifest entries for captured empty untracked directories, and manifest entries
for captured untracked Unix FIFOs and symlinks. Restoring a snapshot verifies
that the current git `HEAD` still matches the commit captured in the snapshot.
Dry-run is the default; `--apply` reverses the current tracked diff, restores
the snapshot staged index and unstaged worktree split, restores captured
untracked files, empty directories, FIFOs, and symlinks, lists the restored
changed files, and runs a post-restore diagnostic pass for those files.

When `deepseek exec` runs inside a git worktree, it creates a pre-run rollback
snapshot and, after a successful run, binds that snapshot to the durable runtime
assistant turn id. TUI-started agent runs also create a pre-run snapshot when
started from a git worktree and bind it to the running assistant turn as soon as
the turn exists. REPL prompts also create a pre-turn snapshot when possible and
expose the latest one through `/restore show last` and `/revert_turn last`.
`restore show` and `restore revert-turn` accept snapshot ids, and for snapshots
bound by `exec`, TUI, or ACP flows they also accept the runtime turn id. REPL
snapshots are intentionally session-local and use the snapshot id or `last`
alias rather than a durable runtime turn id.

In local file-backed TUI sessions, the same rollback surface is available from
the command palette:

```text
restore snapshot [label]
restore list [limit]
restore show <snapshot-id-or-runtime-turn-id|last>
restore hunks <snapshot-id-or-runtime-turn-id|last>
restore hunk <snapshot-id-or-runtime-turn-id|last> [index]
restore hunk <snapshot-id-or-runtime-turn-id|last> <index> --check
restore hunk <snapshot-id-or-runtime-turn-id|last> <index> --apply
revert turn <snapshot-id-or-runtime-turn-id|last> [--apply]
```

`last` resolves to the active thread's latest durable turn id. These commands
show list/show/hunk/revert details in the scrollable right-side rollback panel,
and `--apply` opens a confirmation modal before mutating files. Hunk apply
builds a single-hunk patch from the stored snapshot diff and runs `git apply` on
the local worktree; `--check` verifies that patch without mutation. They are
intentionally local-only because rollback applies to the client's git worktree;
`deepseek tui --runtime-url ...` reports rollback as unsupported instead of
mutating a remote host implicitly.

ACP clients can use `session/checkpoints`, `session/checkpoint/read`, and
`session/checkpoint/restore` against the stdio adapter. Restore is dry-run unless
`apply=true` is passed, and loaded-session restore requests are constrained to
the runtime thread bound to that ACP session.

Current boundaries are explicit:

- untracked restore currently covers regular files, empty directories, Unix
  FIFOs, and Unix symlinks, not non-empty directory metadata, sockets, device
  nodes, or Windows symlink recreation;
- untracked files created after the snapshot are not cleaned unless they became
  tracked changes in the git diff;
- rollback storage under `.dscode/rollback` is excluded from untracked capture;
- older snapshots without split patch files restore through the legacy combined
  `diff.patch` path and do not recover staged-index fidelity;
- runtime-turn binding currently covers `deepseek exec`, TUI-started agent
  runs, and loaded ACP checkpoint flows; REPL live turn snapshots are not bound
  to durable runtime turns because plain REPL transcripts are not durable
  runtime threads yet;
- `deepseek diagnostics --watch`, the generated diagnostics service template,
  agent-loop post-edit diagnostics, and `serve --http` `/v1/diagnostics` reuse
  warmed stdio LSP sessions inside their owning process. Cross-process
  diagnostics can go through the HTTP runtime broker or consume the
  newline-delimited `deepseek.diagnostics.daemon_tick.v1` protocol from
  `deepseek diagnostics --watch --json`. The standalone CLI protocol is
  intentionally stdout-based and does not yet expose a separate socket server.

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
