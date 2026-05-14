# DeepSeek-TUI Parity Plan

**Status:** active
**Source comparison:** `Hmbown/DeepSeek-TUI` refreshed at `/tmp/deepseek-tui-compare-20260514`, HEAD `9483248a9f35b5f2b56c34b5b84fbc5334473c9d`.
**Current repo:** `willamhou/DeepSeekCode` (`PUBLIC` after 2026-05-12 repo publication), release command `deepseek`, compatibility alias `dscode`.

## Objective

Move DeepseekCode from a regression-gated CLI code agent toward a full terminal workbench comparable to DeepSeek-TUI, while preserving the existing benchmark/dogfood/trend/live gates.

## Baseline Gap

DeepSeek-TUI is a multi-crate Rust workspace with a dedicated `deepseek` dispatcher, `deepseek-tui` runtime, TUI state machine, HTTP/SSE runtime API, MCP server mode, SQLite-backed durable state, LSP diagnostics, keybindings, package wrappers, release automation, localized public README surface, and newer app-server/web distribution surfaces. Its current public install story includes npm, crates.io, Homebrew, GitHub Release binaries, and GHCR Docker references.

DeepseekCode is currently a mostly single-crate CLI with a strong deterministic test/benchmark/dogfood surface and about `36.8k` lines under `src/`. Its CLI core is relatively close, but the terminal product surface is still incomplete.

## Current Gap Audit (2026-05-14)

Recent parity slices landed public repo metadata, multilingual README/demo
surface, TUI `/setup` onboarding, guided setup controls, CLI stdin auth
persistence, a masked in-TUI credential wizard, and a first-run setup stepper.
The largest remaining DeepSeek-TUI / Claude Code CLI / Codex CLI gaps are now:

- native supervisor-owned PTY attach/stdin/resize/replay/wait/cancel polish;
- live external write-fixture validation across real repositories;
- release-channel proof for npm and Homebrew once credentials are available;
- completion-state and validation polish inside the first-run wizard;
- richer model-backed demo evidence beyond deterministic TUI snapshots.

## Deliverables

1. True TUI
   - Add a `ratatui`/`crossterm` UI behind a stable command path.
   - Support Plan / Agent / YOLO modes.
   - Add approval modal, command palette, transcript scrolling, sidebar, and session picker.

2. Durable Runtime
   - Add durable thread/session records, resume/fork, event timeline, crash checkpoint, task queue, and job center.
   - Target SQLite for durable state once dependency and release strategy are explicit.

3. Tool Surface Expansion
   - Add `web_search`, `fetch_url`, git history tools, background shell wait/interact/cancel, large-output retrieval, test runner, structured data validation, project map, turn revert, and guarded GitHub write tools.

4. DeepSeek-Native UX
   - Add auto model routing, reasoning effort tiers, token/cost status, prefix-cache reporting, and long-context management.

5. LSP Diagnostics
   - Run language server diagnostics after write/edit/patch operations and inject actionable diagnostics into the next model turn.

6. Subagent / RLM
   - Expand beyond current subagent dispatch into role-aware spawn/wait/send/resume/cancel/list/assign flows.
   - Add cheap flash fan-out / RLM-style one-shot child analysis.

7. MCP And Runtime API
   - Add local supervisor contracts: `doctor --json`, `serve --http`, `serve --mcp`, and later `serve --acp`.
   - Surface sessions, threads, turns, events, tasks, automations, usage, skills, and MCP introspection.

8. Packaging
   - Add npm wrapper, Cargo package strategy, Homebrew/Docker artifacts, cross-platform release assets, version sync, and stronger release checklist.

## Current Increments

The first low-risk foundation is `deepseek doctor --json`.

Acceptance:
- `deepseek doctor --json` emits valid JSON.
- JSON includes version, workspace, model, capabilities, API key status without secret leakage, skills, MCP, network probe state, and local binary availability.
- JSON mode does not perform live network probes.
- Release docs require `doctor --json` output as an artifact.

The second low-risk tool-surface increment is read-only Git history:

Acceptance:
- `git_log`, `git_show`, and `git_blame` are first-class agent tools.
- Tool schemas are exposed to OpenAI/Anthropic-compatible tool calling.
- The offline planner routes direct Git history/blame requests without falling back to shell.
- Default benchmark coverage includes all three tools.

## Phase Order

### Phase A: Integration Contract

- `doctor --json`
- `serve --http` skeleton with health endpoint
- thread/session schema draft
- public-readiness checklist

Status: `done locally`

Artifacts:

- `src/cli/commands/doctor.rs`
- `src/cli/commands/serve.rs`
- `docs/runtime.md`
- `docs/release.md`
- `docs/install.md`

### Phase B: Durable State

- Thread/session/event model
- Resume/fork over durable records
- Crash checkpoint
- Background job metadata

Status: `started`

Landed first slice:

- `src/core/runtime.rs` file-backed session/thread/turn/item/event/task/automation/usage store under `.dscode/runtime/`
- `GET /v1/automations`, `POST /v1/automations`, `GET /v1/automations/{id}`
- `GET /v1/sessions`, `POST /v1/sessions`, `GET /v1/sessions/{id}`
- `GET /v1/sessions/{id}/automations`, `POST /v1/sessions/{id}/automations`
- `POST /v1/sessions/{id}/threads`
- `GET /v1/sessions/{id}/tasks`, `POST /v1/sessions/{id}/tasks`
- `GET /v1/tasks`, `POST /v1/tasks`, `GET /v1/tasks/{id}`
- `GET /v1/threads`, `POST /v1/threads`, `GET /v1/threads/{id}`
- `GET /v1/threads/{id}/automations`, `POST /v1/threads/{id}/automations`
- `POST /v1/threads/{id}/turns`
- `GET /v1/threads/{id}/items`, `POST /v1/threads/{id}/items`, `GET /v1/threads/{id}/items/{item_id}`
- `GET /v1/threads/{id}/turns/{turn_id}/items`, `POST /v1/threads/{id}/turns/{turn_id}/items`
- `POST /v1/threads/{id}/fork` for durable thread context forks with remapped
  turn/item ids and `thread_forked` audit events
- `GET /v1/threads/{id}/tasks`, `POST /v1/threads/{id}/tasks`
- `GET /v1/threads/{id}/events?since_seq=N`
- `GET /v1/threads/{id}/events/stream?since_seq=N&wait_ms=M` for SSE replay plus bounded live wait frames
- `GET /v1/threads/{id}/events/stream?since_seq=N&follow=1` for long-lived SSE follow streams that emit multiple runtime events on one connection until disconnect
- `GET /v1/events/stream?since=thread:N&follow=1` for aggregate cross-thread SSE replay/follow streams that include newly created threads without a per-thread subscription first
- non-`--once` HTTP runtime accepts connections concurrently, so bounded/following SSE streams do not block concurrent runtime writes
- `POST /v1/threads/{id}/events` for appending durable `permission_request` and `permission_response` events consumed by the TUI approval modal
- `GET /v1/threads/{id}/usage`, `GET /v1/usage?thread_id={id}`
- `GET /v1/threads/{id}/usage/summary`, `GET /v1/usage/summary?thread_id={id}` for aggregate token accounting, cache telemetry, recognized DeepSeek V4 cost estimates, and 1M-context policy
- Successful `deepseek exec` runs now append durable sessions, linked user/assistant turns, matching message items, completed task records, and token/cache/cost usage records
- `/runtime` now advertises `sessions`, `threads`, `thread_fork`, `turns`, `items`, `events`, `events_write`, `events_sse`, `events_sse_wait`, `events_sse_follow`, `events_global_sse`, `events_global_sse_follow`, `tasks`, `automations`, `usage`, and `usage_summary` as available; `deepseek tui --runtime-url http://HOST:PORT` can build snapshots from the HTTP runtime, write foreground actions back over HTTP, and subscribe to the aggregate runtime event stream with `follow=1`

### Phase C: Tool Surface

- Background shell manager
- Web aggregate/search/fetch (`web_run`, `web_search`, and `fetch_url` landed as
  agent/MCP/ACP-visible read-only network tools); DeepSeek-TUI-compatible
  `finance` landed as a read-only quote lookup tool; `web_run.image_query`
  landed with DuckDuckGo Images/default and configurable test gateway support;
  `web_run.click` landed for static cached-page link navigation;
  `web_run.screenshot` landed for DeepSeek-TUI-compatible cached PDF page text
  extraction through local `pdftotext`; `web_run.open` and `web_run.click`
  now honor `lineno` plus `response_length` with line-windowed page views; a
  DeepSeek-TUI-style `network.default` / `network.allow` / `network.deny` host
  policy now gates shared web fetches with deny-wins precedence and best-effort
  local network audit logging; `network.default = "prompt"` now flows through
  AgentLoop/runtime/TUI `kind = "network"` permission requests, with direct
  non-registry tool execution still fail-closed; `deepseek config network
  allow|deny <host>` persists host decisions back into project config
- DeepSeek-TUI `shell_env` hook parity landed: enabled local hooks can now run
  immediately before `run_shell`, `exec_shell`, and `task_shell_start`, parse
  `KEY=VALUE` / `export KEY=VALUE` stdout, inject those values into the spawned
  shell process, and report only applied key names back to the model/runtime
- DeepSeek-TUI optional document/image helpers (`pandoc_convert` and
  `image_ocr`) landed as local dependency wrappers with clear missing-binary
  errors
- DeepSeek-TUI `image_analyze` landed as an agent-visible OpenAI-compatible
  vision tool using workspace-relative image paths and configurable
  `vision.*` settings
- Git status/history (`git_status`, `git_log`, `git_show`, `git_blame`
  landed as read-only tools)
- GitHub read-only context (`github_issue_context` and `github_pr_context`)
  landed as agent/MCP/ACP-visible `gh`-backed tools
- GitHub guarded mutations (`github_comment`, `github_pr_review_comment`, and
  `github_close_issue`) landed as evidence-gated tools and as MCP/ACP write
  tools only under durable approvals
- `git_diff` now accepts DeepSeek-TUI-style `path`, `cached`, `unified`, and
  `max_chars` controls
- DeepSeek-TUI read-only search aliases (`list_dir`, `grep_files`, and
  `file_search`) landed as agent/MCP/ACP-visible tools
- DeepSeek-TUI file mutation tools (`write_file`, `edit_file`, and `fim_edit`)
  landed as agent-visible approval-gated tools; `edit_file` is also
  MCP/ACP-visible under durable approvals alongside existing MCP `write_file`
- DeepSeek-TUI `load_skill` landed as a read-only agent tool backed by the
  existing DeepSeekCode TOML skill registry and user-skill override rules
- DeepSeek-TUI `notify` landed as a read-only agent tool backed by a bounded
  terminal-bell implementation
- DeepSeek-TUI `note` landed as an agent-visible persistent notes append tool;
  opt-in user memory now loads the configured memory file into the system prompt
  and exposes DeepSeek-TUI-compatible `remember`
- Project structure mapping (`project_map`) landed as an agent/MCP/ACP-visible
  read-only tool
- Structured data validation (`validate_data`) landed for JSON/TOML content or
  files as an agent/MCP/ACP-visible read-only tool
- Test runner entrypoint (`run_tests`) landed as an agent-visible tool and as a
  gated MCP/ACP side-effect tool that reuses shell approval semantics
- Turn rollback (`revert_turn`) landed as an agent-visible write tool and as a
  gated MCP/ACP durable-approval write tool backed by rollback snapshots
- Todo/plan/checklist compatibility tools (`update_plan`, `todo_add`,
  `todo_update`, `todo_list`, `checklist_write`, `checklist_add`,
  `checklist_update`, `checklist_list`) landed on the agent-visible in-memory
  todo list
- DeepSeek-TUI-compatible durable work aliases (`task_create`, `task_list`,
  `task_read`, `task_cancel`, `task_gate_run`, `automation_create`,
  `automation_list`, `automation_read`, `automation_update`, `automation_pause`,
  `automation_resume`, `automation_delete`, `automation_run`) landed on the
  agent-visible tool surface, backed by `.dscode/runtime` and the existing safe
  shell runner for gates
- DeepSeek-TUI-compatible sub-agent lifecycle aliases (`agent_spawn`,
  `agent_result`, `agent_list`, `agent_cancel`, `close_agent`, `resume_agent`,
  `send_input`) landed as runtime-backed durable sub-agent task tools; spawn and
  follow-up input enqueue pending tasks that TUI/daemon runners can execute
- DeepSeek-TUI-compatible PR-attempt tools (`pr_attempt_record`,
  `pr_attempt_list`, `pr_attempt_read`, `pr_attempt_preflight`) landed as
  agent-visible evidence tools backed by `.dscode/runtime/pr_attempts` patch
  artifacts and non-mutating `git apply --check` preflight
- DeepSeek-TUI-compatible `request_user_input` landed as an agent-visible
  structured clarification tool with 1-3 question validation and non-mutating
  `meta.user_input_required=true` prompt output for plain non-runtime runs;
  durable `user_input_request` / `user_input_response` runtime events, a TUI
  modal for unresolved option selections, short free-form Other answers, and
  blocking runtime-backed TUI/daemon agent-loop resume with `answers_json`
  landed
- DeepSeek-TUI-compatible `recall_archive` landed as an agent-visible read-only
  recall tool over `.dscode/runtime` threads, turns, items, and compaction
  summaries, with `query`, `thread_id`, and `max_results` inputs
- DeepSeek-TUI-compatible `tool_search_tool_regex` and
  `tool_search_tool_bm25` landed as agent-visible local tool catalog discovery
  helpers returning `tool_reference` payloads
- DeepSeek-TUI-compatible `review` landed as an agent-visible local structured
  review tool for safe relative files and git diffs, returning deterministic
  issue/suggestion JSON plus local behavioral signals for missing test changes,
  public API changes, and dependency/configuration changes; `semantic=true`
  now runs an optional read-only child-agent semantic review over the same
  source and deterministic baseline; `github_context` / `pr_context` can now
  review `github_pr_context include_diff=true` output through the same pipeline
- DeepSeek-TUI-style large output spillover and `retrieve_tool_result` landed
  for bounded summary/head/tail/lines/query retrieval of oversized successful
  tool outputs
- DeepSeek-TUI-compatible shell names (`exec_shell`, `exec_shell_wait`,
  `exec_wait`, `exec_shell_interact`, `exec_interact`, `exec_shell_cancel`,
  `task_shell_start`, `task_shell_wait`) landed with in-process background job
  polling, stdin, and cancellation; shell jobs now also persist metadata plus
  stdout/stderr logs under `<cwd>/.dscode/shell-jobs/<task_id>/`, so detached
  `exec_shell_list` / `exec_shell_show` / `exec_shell_wait` calls can inspect
  prior records; detached running jobs can also be best-effort cancelled by
  persisted pid/process group, stale detached `running` manifests are refreshed
  to `exited` when the pid is gone, and new Unix background jobs use durable
  FIFO stdin plus direct stdout/stderr log files so detached
  `exec_shell_interact cwd=<path> task_id=<id>` can write stdin or close it
  without the original in-memory manager; `tty=true` now runs new background
  shell jobs through the Unix `script` PTY backend and persists `tty` /
  `pty_backend` metadata; `tty_rows` plus `tty_cols` set and persist initial
  PTY geometry; `exec_shell_resize` updates durable PTY geometry and sends a
  best-effort `stty rows/cols` command through attached stdin or detached FIFO
  for running TTY jobs; `exec_shell_replay` now replays durable stdout/stderr
  log slices by byte offset for restart-safe shell-log replay; `exec_shell_attach`
  now provides terminal-oriented attach snapshots over durable stdout PTY/log
  bytes with cursor, tail, wait, and TTY geometry metadata; shell manifests now
  carry supervisor capability fields (`attachable`, `resizable`,
  `supervisor_pid`, `supervisor_socket`, `supervisor_epoch`,
  `terminal_event_log`, `terminal_event_seq`) so current `script` records
  explicitly render as non-attachable/non-supervisor while future
  `native-supervisor` records can be read without downgrading their backend;
  `exec_shell_supervisor_status` now exposes the planned workspace-local
  `.dscode/shell-supervisor` manifest/socket status and protocol method names
  without leaking `control_token_hash`; `deepseek agents shell-supervisor
  --json` now starts a Unix newline-JSON protocol daemon bridge that writes
  the workspace supervisor manifest, binds `supervisor.sock`, answers
  health/status/show/start/wait/replay/attach/stdin/resize/cancel/shutdown,
  includes durable `.dscode/shell-jobs` inventory in `show` responses, can
  `start` safe workspace-contained `task_shell_start` background jobs owned by
  the supervisor process, and bridges wait/replay/attach/stdin/resize/cancel
  control requests through the durable shell job tools while native
  supervisor-owned PTYs remain a later slice; `deepseek agents service`
  and packaged systemd/launchd templates include that shell-supervisor service;
  `exec_shell_supervisor_status` now probes socket health before reporting a
  daemon as ready, reads healthy daemon `status` active-job counts backed by
  durable shell manifests, refreshes the workspace supervisor manifest during
  protocol requests, and also reads healthy daemon `show` inventory into the
  status summary;
  foreground `exec_shell timeout_ms` / `detach_after_ms` now uses the durable
  background job table and returns `meta.backgrounded=true` plus a `task_id`
  when the command is still running, approximating DeepSeek-TUI's
  foreground-to-background shell control for non-interactive API/model calls;
  `exec_shell_cancel all=true` now cancels current-process shell jobs and
  detached durable `running` records in the same workspace;
  manifests now keep stable child pid, process-group, and owner-pid metadata so
  detached snapshots can report owner liveness separately from child status
- richer structured data validation

### Phase D: TUI

- Ratatui app shell
- Transcript/composer/status
- Mode switching
- Approval modal
- Command palette/sidebar/session picker

Status: `started`

Landed first slice:

- `Cargo.toml` now includes `ratatui` and `crossterm`
- `src/tui.rs` implements a full-screen ratatui/crossterm TUI shell
- `deepseek tui`, `deepseek tui --demo`, and `deepseek tui --demo --once`
- Plan / Agent / YOLO mode tabs
- sidebar, transcript/composer frame, task panel, command bar
- command palette, session picker, thread navigator, and approval modal surfaces
- session picker reads file-backed durable session metadata from `.dscode/runtime/sessions`
- TUI startup preloads linked runtime threads and item timelines, and the session picker plus thread navigator switch the visible durable transcript snapshot
- interactive TUI refreshes file-backed runtime sessions, threads, and item timelines while open; `--once` remains deterministic
- TUI refresh also reads durable `permission_request` events and opens the approval modal with real tool/kind/target details
- composer focus/input can append user turns and message items to the active durable runtime thread
- approval accept/deny writes durable `permission_response` events and answered requests no longer reopen after refresh
- interactive composer submissions now start a background agent run for the active durable thread
- background TUI agent runs create a running assistant message item, stream assistant deltas into it through durable item updates, and then write final assistant messages, tool result items, usage records, and completed/failed task records back into runtime
- TUI-started agent runs also send assistant/reasoning item updates through an in-process live event channel drained before each draw, so visible token streaming is no longer tied to the 1s durable refresh interval
- interactive TUI starts a local runtime watcher that detects external durable runtime writes and sends full snapshot live events into the draw loop for faster item/task/approval/usage visibility
- HTTP-runtime TUI now follows `/v1/events/stream?follow=1` with a per-thread cursor map, so cross-process writes and newly created remote threads can push a foreground snapshot refresh without waiting for the slower fallback refresh; mirrored live RLM worker events now arrive on the same aggregate stream as `rlm_live_event` and update the TUI status line
- TUI-started agent runs use a runtime-backed approval resolver: permissioned write/shell/MCP tool calls append durable `permission_request` events, wait for the modal's `permission_response`, and then continue approved calls or record denied tool observations
- `deepseek agents run-task` and daemon-executed tasks also append durable permission requests and wait for matching thread `permission_response` events, so external TUI/HTTP clients can approve background tasks
- TUI-started agent runs now create a running runtime task, expose `c` / `cancel` for the active running assistant turn, write a durable `cancel_requested` event, and mark the turn/item/task `cancelled` at cooperative checkpoints
- TUI task panel now loads active-thread runtime task records and shows status counts, short task ids, updated timestamps, and kind/status/summary progress for recent background work
- TUI command palette can create pending active-thread `agent` tasks with `task <summary>` / `task create <summary>`, so daemon or external runners can pick up new work from the workbench
- TUI command palette can select active-thread durable tasks with `task next`, `task prev`, `task select <id>`, and task-panel mouse clicks; selected compatible tasks become the default target for `task pause`, `task resume`, and `task cancel`
- TUI command palette can cancel active-thread durable tasks with `task cancel [id]`, falling back to the first running, pending, or paused task in the task panel when no compatible selected task exists, and routing through the same durable task-cancel event path as HTTP/external runners
- TUI task panel now loads active-thread automations, and the command palette can trigger current-thread automations into pending runtime tasks with optional prompt overrides
- task panel now surfaces active thread usage totals, cache-hit rate, cache chart, estimated cost, input/output cost split, cost chart, and 1M-context policy from durable usage records
- command palette executes local UI commands for mode switching, session picker, thread navigator, thread next/prev/id switching, and approval modal, plus runtime mutations for active-thread `task <summary>`, `compact [tail]`, `automation trigger [id]`, cancel, approval response, and composer submit
- local file-backed TUI command palette now includes a full-width MCP manager
  screen through `mcp` / `mcp manager` and project-level MCP manager commands:
  `mcp list/status/reload`, `mcp init [--force]`,
  `mcp add stdio|http|sse`, `mcp enable|disable|remove`, and `mcp validate`
- local file-backed TUI command palette can target user-level MCP config for
  mutation commands with `mcp user add|enable|disable|remove ...`; unscoped MCP
  mutations remain project-level
- local file-backed TUI command palette now includes MCP discovery detail
  commands: `mcp tools|prompts|resources|resource-templates [server]`, rendered
  in the scrollable right-side panel with `Esc` / `mcp close` to return to
  tasks
- full-width MCP manager can also render discovery detail with
  `mcp manager tools|prompts|resources|resource-templates [server]`, so the
  manager screen is no longer inventory-only
- full-width MCP manager now renders overview/tools/prompts/resources/templates/
  health tabs and supports `mcp manager tab <tab>` plus
  `mcp manager filter <query>` line filtering
- TUI input now preserves key modifiers through `KeyEvent`, so the composer and
  command palette support Ctrl+A/E/U/K/W plus Ctrl+Left/Ctrl+Right word motion,
  and Ctrl-modified global letters no longer accidentally trigger plain
  one-key commands
- TUI command palette now keeps a bounded command history and uses Up/Down to
  recall prior commands or restore the in-progress draft
- TUI command palette now supports Tab prefix completion for built-in workbench
  commands, including common-prefix expansion and candidate hints
- TUI session picker and thread navigator now support PageUp/PageDown plus
  Home/End for faster keyboard selection across longer runtime lists
- TUI session picker and thread navigator now support command-palette filters
  (`session filter <query>` and `thread filter <query>`) that narrow visible
  durable runtime lists while keeping keyboard navigation bounded to matches
- TUI composer and command palette now treat `sessions` / `/sessions`,
  `session` / `/session`, and `resume` / `/resume` as built-in session-picker
  commands, including `/sessions filter <query>`, so they no longer fall
  through to custom slash command execution
- TUI composer and command palette now support DeepSeek-TUI's
  `sessions prune <days>` / `/sessions prune <days>` housekeeping subcommand
  for local file-backed runtime sessions, deleting old sessions and their
  linked runtime records while rejecting the command in HTTP runtime mode
- TUI composer and command palette now catch DeepSeek-TUI's hidden legacy
  migration commands `/set` and `/deepseek` before custom slash fallback,
  surfacing the same replacement guidance while keeping them out of help and
  completion
- TUI composer and command palette now route DeepSeek-TUI-style `/task` and
  `/tasks` slash commands before custom slash fallback, supporting
  `/task add <prompt>`, `/task list`, `/task show <id>`, and
  `/task cancel <id>` through the existing active-thread runtime task flows
- TUI now enables terminal mouse capture for first-line workbench navigation:
  click Plan/Agent/YOLO tabs to switch mode, click visible session/thread
  picker rows to select them, scroll the wheel through the active
  scroll/navigation target, and click the transcript panel to focus composer
- full-width TUI MCP manager now supports mouse tab switching, server-row
  selection, and action-strip clicks for enable, disable, remove, tools, and
  reload over the same keyboard/config mutation paths
- full-width TUI MCP manager now supports server multi-select with `Space`,
  `A`, `U`, `E`, and `D`, plus Ctrl+click row toggling; bulk enable/disable
  selected servers reuses the existing per-server MCP config mutation actions
- full-width TUI MCP manager now supports drag-select across visible server
  rows, selecting the visible server-row range for the existing bulk
  enable/disable action path while preserving normal click-to-select behavior
- task panel now summarizes active-thread runtime item state/type counts and
  the latest item content, making streamed background agent run progress and
  tool activity visible alongside durable task records
- task panel now supports cross-surface multi-select with Ctrl+click,
  drag-select, `task select all`, and `task select clear`; selected compatible
  tasks can be paused, resumed, or cancelled in bulk through the existing task
  action flow and `task bulk pause|resume|cancel` aliases
- local file-backed TUI command palette can start allowlisted background shell
  jobs through `shell <command>` / `shell run <command>` / `! <command>`, then
  list, inspect, feed, close stdin for, or stop them with
  `shell list|show|attach|poll|wait|stdin|close-stdin|resize|cancel` and
  DeepSeek-TUI-style
  `jobs list|show|attach|poll|wait|stdin|close-stdin|resize|cancel` aliases over
  the existing `exec_shell` job manager; shell metadata and stdout/stderr logs
  are persisted for detached later inspection through the same job id and cwd;
  `shell supervisor` / `jobs supervisor` also expose workspace-local shell
  supervisor manifest, socket, protocol health, and `show` job inventory in the
  shell detail panel
- local file-backed TUI composer slash commands now route DeepSeek-TUI-style
  palette-backed `/mcp`, `/jobs`, and `/restore` forms through the same built-in
  local dispatcher before custom slash fallback
- local file-backed TUI composer slash completion now advertises the same
  DeepSeek-TUI-style `/compact`, `/mcp`, `/jobs`, and `/restore` command
  families that the composer can execute, closing the discoverability half of
  those slash-command slices
- TUI unit coverage now includes a source-level DeepSeek-TUI slash registry
  audit that checks every refreshed upstream first-class command name is present
  in composer slash hints, reducing future execution/completion drift
- local file-backed TUI command palette now routes unallowlisted foreground
  shell commands through an explicit modal approval; approved commands run once
  through a trusted TUI-only background shell path without adding an allowlist
  bypass to model-registered shell tools
- local file-backed TUI composer now intercepts single-`#` memory notes and
  `/memory show|path|clear|edit|help` commands without starting model turns;
  the command palette also exposes `memory show|path|clear|edit|help` over the
  same opt-in `memory.memory_path` used by the `remember` tool
- local file-backed TUI now supports DeepSeek-TUI-style `/note` workspace notes
  over `memory.notes_path`, including add/list/show/edit/remove/clear/path
  commands from the composer or command palette
- local file-backed TUI now supports DeepSeek-TUI-style `/anchor` workspace
  anchors over `.dscode/anchors.md`, including add/list/remove/path commands
  from the composer or command palette
- local file-backed TUI now supports DeepSeek-TUI-style `/queue` follow-up
  message management, including automatic composer queueing while an assistant
  item is running, list/edit/drop/clear commands, and idle-transition dispatch
  for the next queued message
- local file-backed TUI now supports DeepSeek-TUI-style `/share` active-thread
  export, rendering durable thread items as standalone HTML and attempting a
  public GitHub Gist upload through the authenticated `gh` CLI while preserving
  a local HTML export path on upload failure
- local file-backed TUI now supports DeepSeek-TUI-style `/export [path]`
  active-thread Markdown export, resolving relative paths inside the selected
  workspace and defaulting to `chat_export_<timestamp>.md`
- local file-backed TUI now supports DeepSeek-TUI-style `/save [path]` and
  `/load <path>` session snapshots, writing active durable session/thread JSON
  under the selected workspace by default and importing snapshots into fresh
  runtime session/thread ids without overwriting existing history
- TUI now supports DeepSeek-TUI-style `/attach <path>` plus `/image` and
  `/media` aliases, validating local image/video files and inserting an
  editable attachment reference into the composer with workspace-relative
  `image_analyze` guidance for images
- local file-backed TUI now supports DeepSeek-TUI-style `/lsp [on|off|status]`,
  mapping the command to the selected workspace `diagnostics.post_edit` config
  and rendering the current diagnostics state in the detail panel
- TUI now supports DeepSeek-TUI-style `/change`, `/changes`, and `/changelog`
  aliases, rendering the latest bundled DeepSeekCode changelog entry in the
  detail panel
- local file-backed TUI now supports DeepSeek-TUI-style `/system`, rendering a
  selected-workspace runtime system prompt preview with workspace instructions,
  user memory, selected latest user message, skill/planning metadata, and prompt
  text in the detail panel
- TUI now supports DeepSeek-TUI-style `/edit`, loading the selected thread's
  latest user message back into the composer; submitting the edited replacement
  now creates a non-destructive rollback fork before sending the revised prompt
- local file-backed TUI now supports DeepSeek-TUI-style `/undo` and `/retry`:
  both fork the selected durable thread before the latest user request, `/undo`
  switches to that branch, and `/retry` resubmits the latest request there while
  preserving the original thread
- local file-backed TUI now supports DeepSeek-TUI-style `/cycles`, `/cycle <n>`,
  and `/recall <query>` over DeepSeekCode durable compaction summaries and the
  existing `recall_archive` runtime search tool
- local file-backed TUI now supports DeepSeek-TUI-style `/review <target>` as a
  built-in before custom slash fallback, rendering deterministic `review` tool
  JSON in the detail panel
- local file-backed TUI now supports DeepSeek-TUI-style `/profile <name>` with
  `profile list` and `profile clear`, backed by `workspace.active_profile` and
  `profiles.<name>.*` / `[profiles.name]` config overlays applied before env
  overrides
- local file-backed TUI now supports DeepSeek-TUI-style
  `/trust [on|off|add <path>|remove <path>|list]`, backed by a per-workspace
  trust store at `~/.config/dscode/workspace-trust.json`; trusted external
  paths and all-path trust mode are honored by shared local/MCP workspace path
  resolution
- local file-backed TUI now supports DeepSeek-TUI-style `/logout`, adapted to
  DeepSeekCode's env-based credential model by clearing the selected
  workspace's model/vision API key env vars from the current TUI process and
  removing matching assignments from the selected workspace `.env` file without
  touching unrelated entries
- local file-backed TUI now supports DeepSeek-TUI-style `/clear` conversation
  reset by creating and switching to a fresh empty active thread in the selected
  durable session without deleting older thread history
- local file-backed TUI now supports DeepSeek-TUI-style `/diff`, rendering
  changed tracked files and `git diff --stat` for the selected session
  workspace in the detail panel
- local file-backed TUI now supports DeepSeek-TUI-style `/subagents` and
  `/agents` active-thread sub-agent task inspection plus `/agent [0-3] <task>`
  queueing into pending `subagent` runtime tasks for daemon/external runners
- TUI now supports DeepSeek-TUI-style `/rlm [0-3] <file_or_text>` and
  `/recursive` aliases, routing a persistent `rlm_process live=true` kickoff
  prompt through the active durable thread so local and HTTP runtime sessions
  share the existing message-submit path
- TUI now supports DeepSeek-TUI-style `/relay [focus]` plus `/batonpass` and
  `/接力` aliases, routing a session-handoff prompt through the active durable
  thread and targeting DeepSeekCode's `.dscode/handoff.md` relay artifact
- local file-backed TUI now supports DeepSeek-TUI-style `/hooks` read-only
  inspection, listing the configured hook enabled state, timeout, project/user
  hook roots, executable scripts by event directory, and supported event names
- TUI now supports DeepSeek-TUI-style `/goal`, tracking an in-memory session
  objective with optional token budget progress from active-thread usage
  telemetry
- local file-backed TUI composer and command palette now expand project/user
  custom markdown slash commands from `.dscode/commands/*.md` and the
  configured user commands dir, reusing REPL argument expansion semantics and
  submitting the rendered prompt to the active durable thread
- local file-backed TUI composer now shows DeepSeek-TUI-style slash-command
  hints while typing `/...` and uses `Tab` to complete built-in local slash
  commands, project `.dscode/commands/**/*.md` custom commands, configured
  user custom commands, and configured `/skill <name>` entries before
  submission
- local file-backed TUI custom slash fallback now matches DeepSeek-TUI's direct
  skill namespace: when `/name` is not a native or project/user custom command
  and `name` is a configured skill, the TUI renders that skill detail instead
  of reporting a missing custom slash command; completions include both
  `/skill <name>` and `/<name>`
- local file-backed TUI now supports DeepSeek-TUI-style local user-skill
  management with `/skill trust <name>` and `/skill uninstall <name>`, writing
  `.trusted` markers beside user TOML skills, removing user skill TOML files
  and trust markers, and returning actionable messages for missing or
  bundled-only skills
- local file-backed TUI composer now supports DeepSeek-TUI-style draft stash:
  `Ctrl+S` parks the current composer text in `.dscode/tui/composer-stash.json`,
  and `stash list|pop|clear` plus `/stash list|pop|clear` list, restore, or
  clear parked drafts
- local file-backed TUI now supports DeepSeek-TUI-style `/rename <title>` and
  command-palette `rename <title>` for renaming the selected durable session,
  persisting the updated title through the runtime store
- local file-backed TUI now supports DeepSeek-TUI-style `/init` and
  command-palette `init` for creating project `AGENTS.md` instructions in the
  selected session workspace, with lightweight project-type detection and
  `.dscode/` gitignore bootstrap
- local file-backed TUI now supports DeepSeek-TUI-style `/network` and
  command-palette `network list|allow|deny|remove|default`, editing the selected
  workspace `.dscode/config.toml` network policy and rendering the result in the
  detail panel
- TUI now supports DeepSeek-TUI-style `/status` and command-palette `status`,
  rendering a read-only runtime summary for the selected session, active thread,
  transcript items, tasks, automations, approvals, user-input requests, token
  usage, cache hit/miss telemetry, context policy, and estimated cost
- TUI now supports DeepSeek-TUI-style `/tokens` and `/cost` plus command-palette
  `tokens` / `cost`, rendering active-thread token totals, last input/output
  telemetry, cache hit/miss rate, context usage, and approximate input/output
  spend in the detail panel without starting a model turn
- TUI now supports DeepSeek-TUI-style `/cache [count|inspect|warmup]` plus
  command-palette `cache`, rendering durable active-thread cache hit/miss
  telemetry as a read-only detail view; `inspect` and `warmup` explicitly
  surface the current prompt-hash persistence and non-mutating warmup limits
- TUI now supports DeepSeek-TUI-style `/model [name]` and `/models` plus
  command-palette `model` / `models`, opening an interactive local model
  picker, showing and updating the selected workspace `model.model` in
  `.dscode/config.toml`, and listing an offline DeepSeekCode model catalog;
  online API model fetching remains a separate runtime/API-backed gap
- TUI now supports DeepSeek-TUI-style `/provider [name] [model]` plus
  command-palette `provider`, opening an interactive two-pane provider/model
  picker, showing the selected workspace provider inferred from
  `model.base_url`, and switching local provider presets by updating
  `model.base_url`, `model.api_key_env`, and `model.model`; remote-runtime
  provider mutation remains a separate gap
- TUI now supports DeepSeek-TUI-style `/skills [prefix]`, `/skill <name>`,
  `/skill trust <name>`, and `/skill uninstall <name>` plus command-palette
  `skills` / `skill`, listing and inspecting DeepSeekCode's configured
  repo/user TOML skill registry, writing local user-skill trust markers, and
  deleting user skill TOML files while protecting bundled repo skills
- TUI now supports DeepSeek-TUI-style `/skills --remote` / `/skills remote`
  browsing of the configured community skill registry URL, rendering remote
  skill names, descriptions, and sources through the right-side detail panel
  while directing users to the install/sync commands for supported remote
  sources
- TUI now supports DeepSeek-TUI-style `/skill new` by routing the alias to a
  bundled `skill-creator` TOML skill that guides creation of focused
  DeepSeekCode local skills
- TUI now supports a DeepSeekCode-native `/skill install <registry-name|url>`
  and `/skill update <name>` slice for direct TOML, SKILL.md, GitHub, and
  tar.gz/zip skill sources, resolving registry entries through
  `skills.registry_url`, converting imported SKILL.md bundles into TOML user
  skills under `workspace.user_skills_dir`, and tracking source/checksum
  metadata in `.installed-from`
- TUI now supports DeepSeek-TUI-style `/skills sync` and `/skills --sync` for
  the configured community registry, caching supported TOML, SKILL.md, GitHub,
  tar.gz, and zip skill entries under `skills.cache_dir`, reporting
  downloaded/up-to-date/skipped/failed counts, and skipping unsupported source
  entries with an actionable reason
- TUI now supports DeepSeek-TUI-style `/feedback [bug|feature|security]` plus
  command-palette `feedback`, opening a feedback target picker and rendering
  repository feedback targets and security-policy links in the detail panel
  without attempting to launch a GUI browser from the terminal
- TUI now supports DeepSeek-TUI-style `/links` plus `dashboard` and `api`
  aliases, opening a link target picker and rendering DeepSeekCode
  repository/docs and DeepSeek platform/API documentation links in the detail
  panel without attempting to launch a GUI browser from the terminal
- TUI now supports DeepSeek-TUI-style `/home` plus `stats` and `overview`
  aliases, rendering a compact runtime dashboard with session/thread, task,
  usage, pending approval/user input, and quick-action links in the detail
  panel
- TUI now supports DeepSeek-TUI-style `/mode [agent|plan|yolo|1|2|3]`,
  showing current mode/options when no target is supplied and switching the
  local Plan / Agent / YOLO workbench mode from the palette or composer
- TUI now supports DeepSeek-TUI-style `/help [command]` plus `/?`, rendering a
  categorized command index or command-specific usage/aliases in the detail
  panel
- TUI now supports DeepSeek-TUI-style `/settings` plus `/config`, rendering
  current mode, workspace/user config locations, workbench state, and focused
  configuration command entry points in the detail panel
- TUI `/config` now routes common DeepSeek-TUI-style key commands through
  existing focused DeepSeekCode config flows: `model`, `provider`, `profile`,
  `mode`, `theme`, `verbose`, and `translate`; `/config tui|native|web`
  surfaces the active config surface instead of returning usage errors
- TUI now supports DeepSeek-TUI-style `/theme [dark|light|grayscale|system]`,
  cycling or switching local TUI theme state and wiring theme accents into tabs,
  sidebar, command bar, and command palette rendering; theme choice now
  persists across local TUI restarts in `.dscode/tui/theme.json`
- TUI now supports DeepSeek-TUI-style `/statusline`, rendering current command
  bar items, shortcuts, and related status/config commands in the detail panel
- TUI now supports DeepSeek-TUI-style `/verbose [on|off]`, keeping reasoning
  transcript entries compact by default while allowing full live thinking text
  through command-palette or composer toggles
- TUI now supports DeepSeek-TUI-style `/translate` plus `translation` and
  `transale` aliases as a session-local output-language toggle; when enabled,
  future local agent turns add a `## Language Output Requirement` system prompt
  block for the detected UI locale while preserving code, paths, URLs, and
  identifiers
- TUI `/translate` now also has DeepSeek-TUI-style post-hoc fallback for local
  agent runs: English-heavy completed assistant messages are translated through
  a focused no-tools model request when an API key is available, while failures
  keep the original answer and record a `posthoc_translate` tool result
- TUI now supports DeepSeek-TUI-style `/context` plus `/ctx`, rendering an
  active-thread context inspector with context window, compaction strategy,
  token/cache telemetry, item counts, and reasoning replay state
- TUI now supports DeepSeek-TUI-style `/exit` plus `/quit` and `/q` aliases
  from both the command palette and focused composer
- AgentLoop cancellation now propagates into cancel-aware model/tool execution; `run_shell` starts commands in a process group and kills that group when a durable cancel event is observed, while remote model streams and blocked model process-pipe reads stop through cancel-aware polling
- deterministic `--once` snapshot path for CI/release smoke tests

Remaining:

- source-level command audit no longer shows unmatched DeepSeek-TUI slash
  command names in DeepSeekCode's built-in TUI command layer; `/translate` now
  has prompt-level locale-output parity plus a focused post-hoc fallback for
  completed local TUI assistant messages
- harder cross-process/platform/external buckets remain: dedicated shell
  supervisor ownership after owner-process exit and attachable terminal replay
  beyond durable terminal attach snapshots now have an explicit
  shell-supervisor/PTY design spec; side-git/platform restore fidelity beyond
  the Unix special files already captured and external live PR/release fixtures
  remain open

### Phase E: DeepSeek-Native Product UX

- Auto model router
- Reasoning tiers
- Cost/cache telemetry
- 1M-context compaction controls

Status: `completed`

Landed first slice:

- `model.model = "auto"` and `DEEPSEEK_MODEL=auto` now route simple work to `deepseek-v4-flash` and complex planning/review/architecture/security/migration/recovery work to `deepseek-v4-pro`; `model.reasoning_effort = "auto"` maps the same route to off/high/max thinking tiers
- remote usage records attach the resolved model name so Runtime cost accounting records `deepseek-v4-flash` / `deepseek-v4-pro` instead of an opaque `auto`
- model usage parsing now preserves OpenAI-compatible `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens`, `prompt_tokens_details.cached_tokens`, and Anthropic-compatible cache read/creation counters when providers return them
- runtime usage records persist prompt cache hit/miss tokens and recognized DeepSeek V4 USD micro-cost estimates
- usage summary aggregates cache hit rate, estimated input/output/total cost, unpriced record count, and 1M-context strategy
- TUI usage panel renders cache and cost split bars so DeepSeek prefix-cache and cost behavior is visible during durable sessions
- TUI command palette and composer slash commands can trigger non-destructive
  active-thread compaction with `compact [tail]` / `/compact [tail]`, reusing
  the runtime `thread_compacted` audit event path
- `model.reasoning_effort = "off|high|max|auto"` and `DEEPSEEK_REASONING_EFFORT` now map to official DeepSeek V4 thinking/reasoning parameters for OpenAI-compatible and Anthropic-compatible requests; streaming parsers surface reasoning deltas separately from final answer text
- runtime-backed TUI and daemon agent runs preload compact persisted reasoning
  items from the active durable thread into the next model request, extending
  reasoning replay beyond a single in-process tool loop
- daemon automatic compaction now attempts model-generated older-context
  summaries when the configured model API key is present, records
  `summary_source = "model"`, and falls back to deterministic extractive
  summaries when the key is absent or summary generation fails
- TUI command palette now exposes a reasoning browser through `reasoning`,
  `reasoning latest`, and `reasoning show <selector>`, and local replay
  controls through `reasoning replay <0..20>` for TUI-started agent runs
- TUI reasoning browser now supports `reasoning search <query>` with highlighted
  matching excerpts, plus `reasoning pin <selector>`, `reasoning pins`, and
  `reasoning unpin <selector|all>` for local per-turn replay pinning beyond the
  latest-N replay window
- local file-backed TUI sessions persist the reasoning replay limit and pinned
  turn ids in `.dscode/tui/reasoning-replay.json`, so operator replay
  preferences survive TUI restarts without changing durable runtime thread
  records

Remaining:

- no open Phase E product UX gaps identified

### Phase F: LSP + Revert

- Language server registry
- Post-edit diagnostics
- Turn snapshots and `revert_turn`

Status: `started`

Landed first slice:

- `src/language/diagnostics.rs` adds a diagnostics runner that prefers stdio LSP `textDocument/publishDiagnostics` for opened files when the language server is available, then falls back to compiler/type-check commands
- `deepseek diagnostics [--changed] [paths...]` exposes manual diagnostics for Rust, TypeScript, JavaScript, Python, and Go workspaces
- `deepseek diagnostics --json` emits `deepseek.diagnostics.report.v1`,
  while `deepseek diagnostics --watch --json` emits newline-delimited
  `deepseek.diagnostics.daemon_tick.v1` records for standalone supervisor
  consumption
- `deepseek diagnostics --watch` keeps a warmed stdio LSP session alive inside the watcher process, and `deepseek agents service` renders a JSON diagnostics watch supervisor for local always-on use
- agent registry exposes a read-only `diagnostics` tool, and OpenAI/Anthropic tool schemas include it
- `diagnostics.post_edit = true` enables opt-in post-edit diagnostics appended to successful `apply_patch` tool results
- `serve --http` exposes `/v1/diagnostics` as a runtime diagnostics broker
  with warmed LSP session reuse inside the runtime process, and HTTP-runtime
  TUI sessions route `diagnostics [--changed|paths...]` through that broker
- `src/core/rollback.rs` stores rollback snapshots under `.dscode/rollback/snapshots/`, including combined, staged, and unstaged tracked diffs plus captured untracked regular files, empty directories, Unix directory mode metadata, Unix FIFOs, Unix sockets, and Unix symlinks
- `deepseek restore snapshot [label]`, `restore list`, `restore show <id> [--patch]`, and `restore revert-turn <id> [--apply]`
- REPL `/restore snapshot [label]`, `/restore list`, `/restore show <id>`, and `/revert_turn <id> [--apply]`
- Snapshot restore checks that git `HEAD` matches the captured commit, dry-runs by default, and applies only when `--apply` is passed
- Applied restores now report restored changed files and run post-restore diagnostics through the same fallback diagnostic runner
- Applied restores now restore captured untracked regular files, captured empty
  directories, captured untracked Unix directory modes, captured untracked Unix
  FIFOs, and captured untracked Unix sockets and symlinks, while excluding
  rollback storage from untracked capture
- Applied restores preserve the snapshot staged-index versus unstaged-worktree split for new split-patch snapshots
- Applied restores now preserve captured Unix device node metadata in rollback
  manifests and attempt best-effort `mknod` recreation for untracked character
  and block devices when the OS/user permits it
- `deepseek exec` creates a pre-run rollback snapshot in git worktrees and binds it to the successful assistant runtime turn id; restore/show accept either snapshot id or bound turn id
- TUI-started agent runs create a pre-run rollback snapshot in git worktrees and bind it to the running assistant turn id as soon as the durable turn exists
- TUI rollback commands now render list/show/revert results in the scrollable
  right-side rollback detail panel, including snapshot metadata, untracked
  file/directory/special-file counts, bounded patch previews, dry-run plans,
  and applied changed-file lists
- TUI rollback `--apply` commands now open an explicit confirmation modal
  before queueing the local worktree restore action
- TUI rollback hunk browser commands (`restore hunks`, `restore diff`, and
  `restore hunk`) parse stored snapshot patches and render hunk lists or a
  selected hunk in the right-side rollback panel
- TUI rollback hunk restore commands (`restore hunk <id|last> <index> --check`
  and `restore hunk <id|last> <index> --apply`, plus `hunk-check` /
  `hunk-apply` aliases) check or apply one selected snapshot hunk through the
  existing local-only git worktree and confirmation-modal path
- REPL live prompts create pre-turn rollback snapshots in git worktrees, record
  the latest snapshot id for `/restore show last` and `/revert_turn last`, and
  print the rollback hint after tool-using turns

Remaining:

- side-git/worktree snapshot strategy for platform-specific restore fidelity,
  especially Windows symlink recreation and privilege-constrained device-node
  restore validation

### Phase G: Subagent/RLM

- Role-aware child agents
- Child lifecycle controls
- RLM-lite tool now exists: `rlm` wraps `context` + `question` into bounded
  child-agent analysis using the existing subagent depth limit and model schema
- `rlm` / `rlm_query` / `llm_query` now also accept DeepSeek-TUI-style
  `task` plus workspace-relative `file_path` or inline `content` input, so
  long-input processing can be invoked without first reshaping it into
  `context` + `question`
- `rlm_process` now exists as an explicit DeepSeek-TUI-compatible long-input
  entrypoint; this currently reuses the bounded child-agent process adapter,
  and optional `session_id` / `reset` fields persist prior process summaries
  under `.dscode/rlm-model/` so later calls receive durable process-style
  context; existing non-empty sessions can also be continued with
  `task + session_id` and no new `file_path` / `content`; the full live model
  REPL/daemon loop now has a design spec covering live-session manifests,
  runtime-thread-backed turn queues, event logs, cancellation, and recovery
- RLM batch helper now exists: `rlm_batch` maps shared `context` plus up to 16
  `questions` onto parallel bounded child-agent analyses, matching the
  `llm_query_batched` / `rlm_query_batched` usage pattern without a Python REPL
- RLM chunk planning now exists: `rlm_chunk_plan` accepts `file_path` or
  `content` and returns coverage-aware chunk offsets plus optional text, making
  map-reduce setup available without writing a Python helper first
- RLM map-reduce planning now exists: `rlm_map_reduce_plan` accepts a `task`
  plus `file_path` or `content`, returns chunks, ready-to-dispatch map task JSON,
  omitted-map metadata, and a reduce prompt without directly running child
  agents
- RLM recursive planning now exists: `rlm_recursive_plan` accepts the same
  long-input shape and returns initial map tasks plus multi-round fan-in reduce
  groups with stable `map:<index>` / `roundN:groupM` refs for recursive helper
  workflows that exceed one batch
- DeepSeek-TUI-compatible RLM helper aliases now exist: `rlm_query` mirrors
  `rlm`, `llm_query` mirrors `rlm`, and `rlm_query_batched` /
  `llm_query_batched` mirror `rlm_batch`; all aliases use the same model
  schemas and subagent depth gate as the original tools
- RLM Python helper slice now exists: `rlm_python` runs short restricted Python
  scripts over optional `context` / `ctx` / `question` variables for pure computation,
  counting, text splitting, and aggregation; it blocks import/file/network/
  subprocess-style tokens, clamps timeout, returns stdout plus
  JSON-serializable variables, and exposes DeepSeek-TUI-style
  `chunk_context` / `chunk_coverage` helpers for coverage-aware chunking plus
  `SHOW_VARS`, `repl_get`, `repl_set`, `FINAL`, and `FINAL_VAR` helper names
- RLM stateful Python helper slice now exists: `rlm_python_session` persists an
  explicit JSON `state` dictionary by `session_id` under `.dscode/rlm-python/`,
  preloads safe state keys as Python locals on later calls, and writes
  JSON-serializable locals back into state after each call, allowing repeated
  helper calls to build chunk indexes, counters, and aggregation caches with
  `reset=true` for clearing state
- RLM persistent Python process slice now exists: `rlm_python_session` accepts
  `persistent=true` to reuse a restricted long-lived Python REPL process for the
  same `session_id` while still syncing JSON state to disk; `reset=true` clears
  state and rebuilds that cached process
- RLM Python session inventory now exists: `rlm_python_sessions` can list or
  inspect persisted session state without running Python, making helper caches
  discoverable before choosing whether to continue, reset, or create a session;
  it now also reports `process.active` / `process.pid` for persistent Python
  REPLs alive in the current DeepSeekCode process
- RLM model-session inventory now exists: `rlm_process_sessions` can list or
  inspect persisted `.dscode/rlm-model/` process summaries without running a
  child model, so durable long-input RLM sessions are discoverable before
  continuing or resetting them; `include_live=true` also surfaces normalized
  `.dscode/rlm-daemon/<session_id>/manifest.json` live-session records so the
  live RLM daemon roadmap has a model-visible inventory layer;
  `include_turns=true` adds per-turn live payload inventory with runtime status
  and bounded result/error previews; `rlm_process live=true session_id=<id>`
  now creates or reuses a live-session runtime thread, persists per-turn
  payloads, and enqueues pending `rlm_process` runtime tasks without running a
  model worker yet; `rlm_process_events` replays queued live-session event logs
  by cursor as the read-only polling surface and now includes worker
  reasoning/text deltas plus model/tool call/result events emitted during
  `rlm_process_run_next`;
  `rlm_process_wait` adds cursor-based long-polling for those event logs, and
  `/v1/rlm/live/<session_id>/events/stream` exposes the same log over HTTP SSE;
  each live RLM event is also mirrored into the owning runtime thread as
  `rlm_live_event`, so aggregate runtime SSE, HTTP-mode TUI, and runtime-event
  clients can subscribe through one path;
  `rlm_process_cancel` cancels queued pending or active running live turns,
  marks payloads cancelled when present, refreshes `queued_turns`, preserves a
  live owner while cancellation is pending, and appends `turn_cancelled` events;
  active workers now observe runtime task cancellation through the agent cancel
  path and clear the live manifest as `status=cancelled`; `force=true` can
  explicitly SIGTERM an external daemon owner on Unix and append
  `worker_interrupted`;
  `rlm_process_run_next` now claims one queued payload, records `turn_started`,
  runs the bounded model-backed RLM child flow, and records `turn_completed` /
  `turn_failed`, giving live sessions a single-step worker bridge that the
  resident `deepseek agents daemon` now runs per tick; `rlm_process_drain`
  repeats that worker path for a bounded FIFO batch so queued sessions can be
  drained manually or through the packaged service loop; `rlm_process_recover`
  can requeue or fail interrupted
  `running` live turns for one session or all live manifests and records
  `turn_recovered`; `deepseek agents daemon` now runs one queued live RLM turn
  per tick through the RLM worker path instead of the generic runtime task path;
  `rlm_process_stop` stops idle live sessions and blocks accidental reuse until
  `reset=true`; live RLM worker ownership now stamps daemon pid/epoch while a
  turn is running, and `rlm_process_sessions include_live=true` reports
  `daemon_alive`, `daemon_stale`, and `daemon_owner` so dead owners are visible
  before recovery; `rlm_process_recover` also skips live-owned running turns
  unless `force=true` is supplied; `deepseek agents daemon` now runs safe
  all-session live RLM recovery before claiming one queued live RLM turn per
  tick; `rlm_process_status` now provides a read-only lifecycle dashboard with
  owner liveness, queue/running counts, and recommended next commands; terminal
  operators can now call the same read-only lifecycle surface through
  `deepseek agents rlm-status`, `deepseek agents rlm-events`, and
  `deepseek agents rlm-wait`; stateful lifecycle controls are also available as
  `deepseek agents rlm-cancel`, `rlm-recover`, `rlm-stop`, `rlm-run-next`, and
  `rlm-drain`; `deepseek agents service` and packaged systemd/launchd templates
  now explicitly document that the agents daemon is also the live RLM worker
  loop, including stale-owner recovery and one queued live RLM turn per tick
- Review remote PR context signals now exist: `review` parses
  `github_pr_context` JSON to report requested changes, failing/cancelled status
  checks, and missing `include_diff=true` context before optional semantic
  review delegation
- Remote PR review loop planning now exists: the offline planner routes PR
  review tasks through `github_pr_context include_diff=true` and then `review`
  over that gathered context, with a seeded benchmark fixture covering blocker
  metadata
- Remote PR semantic review planning now exists: explicit semantic/deep/
  thorough/behavioral/real-bug/logic-bug PR review requests set
  `review semantic=true` over gathered `github_pr_context`, with a seeded
  benchmark assertion over tool input
- Remote PR comment planning now exists: `pr_review_comment_plan` converts
  structured `review` JSON plus optional `github_pr_context` into Markdown body
  text, evidence JSON, and a dry-run `github_comment` input; the offline planner
  invokes it for remote PR review tasks that ask to draft or prepare a comment
- Guarded PR comment post planning now exists: when the task explicitly asks to
  post/publish/leave/add/submit/send the prepared PR comment, the offline
  planner hands the comment plan to `github_comment` with `dry_run=false`, still
  relying on the existing write-approval path before any GitHub mutation runs
- PR comment failure recovery now exists: if the guarded `github_comment`
  or `github_pr_review_comment` attempt fails or is denied, the planner rebuilds
  a fresh `pr_review_comment_plan` with the previous comment error recorded in
  body and evidence, instead of blindly re-sending the same GitHub mutation
- Inline PR review-comment posting now exists: `github_pr_review_comment`
  posts evidence-backed line-level review comments through `gh api` after write
  approval, `pr_review_comment_plan` emits dry-run inline comment input when PR
  context includes a head commit SHA and findings have `path` + `line`, and the
  offline planner routes explicit inline/line/file/diff comment requests to that
  guarded tool
- Seeded inline PR review-comment retry fixture now exists:
  `fixture-pr-inline-comment-failure-recovery-plan` covers failed
  `github_pr_review_comment` recovery back into `pr_review_comment_plan` with
  the previous API/policy error preserved in the next plan input and output
- Live remote PR readiness is now checkable without mutation:
  `deepseek pr live-status <pr>` verifies `gh` auth, PR metadata/diff,
  changed files, branch alignment, and repo permissions; `--require-write`
  conservatively gates guarded comment fixtures on repo write-capable
  permissions
- Remaining: real GitHub permission/API live fixtures for remote PR retry,
  which require an external test repository and explicit write authorization

### Phase G2: MCP Server Mode

Status: `completed`

Landed first slice:

- `deepseek serve --mcp` now runs a line-delimited JSON-RPC MCP stdio server
- `initialize`, `notifications/initialized`, `tools/list`, `tools/call`,
  `prompts/list`, `prompts/get`, `resources/list`, `resources/templates/list`,
  and `resources/read`
- read-only workspace tools exposed through MCP: `list_files`, `list_dir`,
  `read_file`, `retrieve_tool_result`, `search_text`, `grep_files`,
  `file_search`, `web_run`, `web_search`, `fetch_url`, `finance`,
  `pandoc_convert`, `image_ocr`, `git_status`, `git_diff`, `project_map`,
  `validate_data`, `git_log`, `git_show`, `git_blame`,
  `github_issue_context`, `github_pr_context`, and `diagnostics`; local
  read-only helpers exposed through MCP:
  `review`, `pr_review_comment_plan`, `recall_archive`,
  `tool_search_tool_regex`, and `tool_search_tool_bm25`; interactive/local
  helpers exposed through MCP:
  `load_skill`, `request_user_input`, and `notify`
- code-executing MCP side-effect tools exposed only with trusted side effects or
  durable approvals: `run_tests`, `run_shell`
- DeepSeek-TUI-compatible MCP shell-session tools: read-only
  `exec_shell_list`, `exec_shell_show`, `exec_shell_wait`, `exec_wait`, and
  `task_shell_wait` are available by default; mutating `exec_shell`,
  `task_shell_start`, `exec_shell_interact`, `exec_interact`, and
  `exec_shell_cancel` are exposed only with trusted side effects or durable
  `permission_request kind=shell` approvals; list/show/wait can also inspect
  detached durable shell manifests/logs from the requested `cwd`
- local RLM MCP helpers exposed by default: `rlm_chunk_plan`,
  `rlm_map_reduce_plan`, restricted pure-compute `rlm_python`, and read-only
  `rlm_python_sessions`; stateful `rlm_python_session` is hidden by default and
  exposed only with trusted side effects or durable
  `permission_request kind=write` approvals
- model-running RLM MCP tools (`rlm`, `rlm_query`, `llm_query`, `rlm_process`,
  `rlm_batch`, `rlm_query_batched`, and `llm_query_batched`) are hidden by
  default and exposed only with trusted side effects or durable
  `permission_request kind=mcp` approvals because they can spend model tokens
  and use networked model APIs
- read-only runtime tools exposed through MCP: `runtime_health`,
  `runtime_list_sessions`, `runtime_list_threads`, `runtime_read_thread`,
  `runtime_list_tasks`, `runtime_read_task`
- approval-gated runtime task side-effect tools exposed through MCP:
  `runtime_create_task` and `runtime_cancel_task`
- approval-gated runtime automation side-effect tools exposed through MCP:
  `runtime_create_automation`, `runtime_update_automation`,
  `runtime_pause_automation`, `runtime_resume_automation`,
  `runtime_delete_automation`, and `runtime_trigger_automation`
- runtime sub-agent lifecycle tools exposed through MCP: read-only
  `runtime_list_agents` / `runtime_agent_result`, plus approval-gated
  `runtime_spawn_agent`, `runtime_cancel_agent`, `runtime_close_agent`,
  `runtime_resume_agent`, and `runtime_send_agent_input`
- read-only MCP prompt templates exposed through `prompts/list` / `prompts/get`:
  `review_code`, `explain_code`, and `plan_task`
- read-only MCP resources exposed through `resources/list` / `resources/read`:
  workspace root, runtime sessions, runtime threads, and runtime tasks
- read-only MCP resource templates exposed through `resources/templates/list`:
  runtime session/thread/task URI templates
- opt-in side-effect MCP tool exposure: `run_shell` is hidden by default and
  only appears in `tools/list` when the server environment sets
  `DSCODE_MCP_ENABLE_SIDE_EFFECTS=1`; it still reuses the existing
  safe-command allowlist
- durable approval MCP side-effect bridge: `DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1`
  creates an approval thread and routes `tools/call run_shell` through
  runtime `permission_request` / `permission_response`; alternatively
  `DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id>` binds an existing runtime thread
- MCP `apply_patch` is now exposed only in durable approval mode and routes
  write requests through `permission_request kind=write` before reusing the
  existing unified-patch validator
- MCP `write_file` is now exposed only in durable approval mode, writes UTF-8
  content to safe relative paths under the MCP workspace, rejects path escapes
  and symlink targets, and records the approval decision before mutating files
- MCP `edit_file` is now exposed only in durable approval mode and reuses the
  exact-text edit validator after recording the approval decision
- MCP `delete_file` is now exposed only in durable approval mode, deletes one
  regular file at a safe relative path under the MCP workspace, rejects path
  escapes/directories/symlink targets, and records the approval decision before
  mutating files
- MCP `copy_file` is now exposed only in durable approval mode, copies one
  regular file between safe relative paths under the MCP workspace, rejects path
  escapes/directories/symlink sources and existing destinations, and records the
  approval decision before mutating files
- MCP `move_file` is now exposed only in durable approval mode, moves one
  regular file between safe relative paths under the MCP workspace, rejects path
  escapes/directories/symlink sources and existing destinations, and records the
  approval decision before mutating files
- MCP `revert_turn`, `github_comment`, `github_pr_review_comment`, and
  `github_close_issue` are now exposed only in durable approval mode, routing
  rollback restore and GitHub write actions through runtime permission requests
  before executing
- focused unit tests cover initialize, tool listing, `read_file`, and runtime
  session listing; resource tests cover listing and reading runtime thread JSON;
  stdio smoke validates real `deepseek serve --mcp` output
- `deepseek mcp add-self` resolves the current binary and writes a stdio server
  entry (`serve --mcp`, optional `--workspace`) to user or project MCP config
- `deepseek serve --mcp --workspace <path>` runs the MCP server from an explicit
  workspace
- `deepseek serve --acp` now runs an Agent Client Protocol stdio adapter:
  `initialize`, `session/new`, `session/list`, `session/load`,
  checkpoint read/restore methods, `session/tools/list`,
  `session/tools/call`, `session/rlm/subscribe`, `session/prompt`,
  `session/cancel`, and `shutdown`
- ACP `session/list` exposes durable runtime sessions and `session/load` maps
  a runtime session/thread workspace into an in-process ACP session
- Loaded ACP `session/prompt` records user/assistant durable turns and message
  items back to the bound runtime thread; token usage is recorded as source
  `acp` when available
- ACP now advertises checkpoint replay/restore/apply support and handles
  `session/checkpoints`, `session/checkpoint/read`, and
  `session/checkpoint/restore`; loaded-session checkpoint listing and restore
  requests filter rollback snapshots to the bound runtime thread, read can
  include the unified patch, and restore is dry-run unless `apply=true` is
  passed
- ACP now exposes a session-scoped tool bridge through `session/tools/list` and
  `session/tools/call`; read-only tools run from the ACP session workspace, and
  side-effect tools appear only for loaded runtime-thread sessions where they
  reuse durable `permission_request` / `permission_response` approval events
- MCP and ACP read-only tool bridges now expose the DeepSeek-TUI-compatible
  aggregate `web_run` wrapper in addition to the narrower `web_search`,
  `fetch_url`, and `finance` tools
- MCP and ACP read-only tool bridges now expose agent-compatible local helper
  tools `review`, `pr_review_comment_plan`, `recall_archive`,
  `tool_search_tool_regex`, and `tool_search_tool_bm25`
- MCP and ACP tool bridges now expose `image_ocr` and `pandoc_convert`; inline
  `pandoc_convert` is available by default, while `output_path` conversion
  requires durable write approvals
- MCP and ACP tool bridges now expose interactive/local helpers `load_skill`,
  `request_user_input`, and `notify`, while keeping durable memory writes and
  external vision-model calls out of the default MCP surface
- MCP and ACP tool bridges now expose model-running vision helper
  `image_analyze` only with trusted side effects or durable
  `permission_request kind=mcp` approvals, matching the token/network safety
  contract used by model-running RLM tools
- MCP and ACP tool bridges now expose persistent note/memory helpers `note` and
  enabled `remember` only with durable `permission_request kind=write`
  approvals, keeping note and memory file appends out of the default surface
- ACP loaded-session tool calls now create an assistant runtime turn with
  `tool_call` and `tool_result` items, and side-effect permission requests are
  linked to that same turn for auditability
- ACP `session/tools/call` now emits `session/update` notifications for
  standard `tool_call` and `tool_call_update` payloads before the final JSON-RPC
  result; loaded-session updates include runtime turn/item ids under
  `_meta.runtime` to align ACP clients with durable runtime audit records
- ACP `session/tools/call` now emits bounded intermediate `tool_call_update`
  progress chunks for large tool outputs before the final completion update,
  while preserving the existing small-output response shape
- ACP `session/tools/call` now supports opt-in true process-level stdout/stderr
  streaming for `exec_shell` and `task_shell_start` through `stream=true` or
  `follow=true`, flushing partial `tool_call_update` deltas while shell jobs are
  still running before the final completion result
- ACP `session/rlm/subscribe` now lets loaded runtime-thread sessions consume
  mirrored `rlm_live_event` runtime events by cursor and receive standard
  `session/update` `tool_call` / `tool_call_update` notifications before the
  final `nextCursor` response
- `deepseek serve --acp --workspace <path>` starts ACP from an explicit
  workspace
- `deepseek mcp add/get/remove/enable/disable/validate` covers common MCP
  config CRUD and validation without hand-editing JSON
- `deepseek tui` command palette now covers the same first-line MCP manager
  flow for project-level config, matching DeepSeek-TUI's `/mcp add/list/status`
  style command surface; `mcp` / `mcp manager` opens a full-width inventory
  screen, it can also target user config with `mcp user ...`, and it renders
  tools/prompts/resources/templates discovery summaries in either the
  full-width manager or scrollable right-side panel
- the full-width TUI MCP manager now supports keyboard-native `Tab` /
  `Shift+Tab` tab cycling and `r` reload/list refresh, reducing dependence on
  command strings once the manager is open
- the full-width TUI MCP manager now renders a selected-server action strip and
  supports `n`/`p` selection plus selected-server `e` enable, `d` disable,
  `x` remove confirmation, and `t` tools actions over the existing config
  mutation path
- selected-server `x` in the full-width TUI MCP manager now opens a remove
  confirmation modal; `y`/`Enter` queues the config removal and `n`/`Esc`
  cancels without mutating config
- `mcp validate` now reports per-server tools/prompts/resources/resource-template
  health in the TUI detail panel instead of only reusing raw tools discovery
  output
- `deepseek mcp resources [server]` and `deepseek mcp resource <server> <uri>`
  now cover stdio / HTTP / SSE `resources/list` and `resources/read`
- `deepseek mcp resource-templates [server]` now covers stdio / HTTP / SSE
  `resources/templates/list`
- agent runs expose read-only `mcp_list_prompts`, `mcp_get_prompt`,
  `mcp_list_resources`, `mcp_read_resource`, and `mcp_list_resource_templates`
  bridge tools whenever project/user MCP config exists

Remaining:

- no open Phase G2 MCP/ACP parity gaps identified; continue monitoring upstream
  MCP/ACP protocol drift and client interoperability reports

### Phase H: Packaging

- npm wrapper
- release artifact matrix
- Docker/Homebrew plan
- version sync and publish dry-run checks

Status: `started`

Landed first slice:

- `Dockerfile` and `.dockerignore` for source-built local Docker images
- `npm/package.json`, `npm/bin/deepseek.js`, and `npm/README.md` for a Node wrapper that launches packaged target-triple binaries or `DEEPSEEK_BINARY`
- `docs/install.md` and `docs/release.md` include Docker and npm wrapper verification commands
- `Cargo.toml` now carries package metadata (`description`, `readme`, `license-file`, repository/homepage, keywords, categories) and an explicit `publish = false` Cargo registry policy until crates.io/private registry ownership is decided
- `.github/workflows/release.yml` defines a release matrix for Linux x64, macOS x64, macOS arm64, and Windows x64, plus packaging checks for Cargo metadata, npm wrapper, npm dry-pack, and Homebrew formula syntax
- Release matrix archives now include sibling `.sha256` files for published asset verification and Homebrew formula updates
- Release matrix creates GitHub signed artifact attestations for each archive and checksum file with `actions/attest`
- `packaging/homebrew/deepseek.rb` provides a Homebrew formula template for macOS arm64/x64 and Linux x64 release assets
- `packaging/systemd/` and `packaging/launchd/` provide runtime service placeholders; `deepseek agents service` renders workspace-specific systemd/launchd files for `serve --http`, `agents daemon --json`, `diagnostics --watch --changed`, and `agents shell-supervisor --json`
- `deepseek update package` includes `SERVICES.md` and packaged service templates under `services/`
- Cargo registry distribution now has an explicit source-build/package-only
  decision: the release workflow skips Cargo registry publishing while
  `Cargo.toml publish = false` is present
- `deepseek update publish-status` now audits npm/Homebrew publish readiness
  without side effects, including token/tap config, platform npm tarballs,
  release archives, and non-placeholder `.sha256` files when artifact
  directories are supplied; `--strict` fails on blocked or skipped checks and
  `--json` emits `deepseek.publish_status.v1` for CI/release scripts
- `deepseek update publish-status` now also emits a public install audit for
  source checkout, GitHub Release, npm, Homebrew, GHCR, and Cargo registry
  policy, with explicit `source_available`, `ready_to_publish`,
  `requires_publish`, and `source_only_policy` states plus verification commands
  for live release evidence
- `deepseek pr live-status <pr> --json` emits
  `deepseek.pr_live_status.v1`, making live PR fixture readiness scriptable
  without posting GitHub comments
- Release Matrix now runs `cargo test -- --test-threads=1`, matching the stable
  local release gate for tests that share process-global current-directory and
  background-shell state; `docs/release.md` and generated release notes name the
  same serial test gate

Remaining:

- Actual published npm package with uploaded platform binaries
- Published Homebrew tap with real release asset SHA-256 values
- Tagged GitHub Release and GHCR image evidence for the public binary/container
  install channels

## Completion Audit Gate

This plan is complete only when every deliverable has:
- code merged into a tracked source path,
- docs that explain user-facing usage,
- focused unit/integration tests,
- inclusion in release or benchmark/dogfood verification where applicable,
- a source-level comparison note explaining remaining differences from DeepSeek-TUI.
