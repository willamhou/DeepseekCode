# `deepseek tui` — Terminal Workbench

`deepseek tui` starts the ratatui/crossterm full-screen workbench shell.

Current surfaces:

- Plan / Agent / YOLO mode tabs
- sidebar with mode, selected durable session metadata, and key hints
- transcript backed by durable thread items when available
- composer input that appends user turns/items to the active durable thread
  and starts a background agent response in interactive TUI sessions
- task panel with active thread status, runtime item count, recent runtime
  tasks, active-thread automations, usage total, cache-hit rate, cache chart,
  estimated cost, input/output cost split, cost chart, and 1M-context policy
  when usage records exist
- command palette with local UI commands and active-thread runtime actions
- session picker populated from `.dscode/runtime/sessions`, linked threads, and
  item timelines
- thread navigator populated from the selected session's durable runtime
  threads
- live runtime refresh for file-backed sessions, threads, and items while the
  TUI is open; assistant deltas from TUI-started runs update a running durable
  assistant item before final completion and are also pushed through an
  in-process live channel drained before each draw
- external runtime writes are picked up by a local runtime watcher that sends
  full snapshot live events into the same draw loop, so task/approval/item
  changes from other DeepSeekCode processes do not wait for the slower refresh
- `deepseek tui --runtime-url http://HOST:PORT` connects the workbench to a
  running HTTP runtime, builds the initial UI from `/v1/sessions` and linked
  thread detail endpoints, writes composer/approval/cancel/task/automation/
  compaction actions back through HTTP, and subscribes to each known thread via
  `/v1/threads/{id}/events/stream?follow=1`
- approval modal backed by durable `permission_request` runtime events
  appended directly or through `POST /v1/threads/{id}/events`
- approval accept/deny records durable `permission_response` events and can
  unblock permissioned tools for agent runs started from the TUI composer
- background TUI agent runs append assistant messages, tool result items, usage
  records, and completed/failed task records back into the active thread
- active-thread runtime task records are loaded from `.dscode/runtime/tasks`
  and rendered in the task panel for progress visibility
- `task <summary>` / `task create <summary>` creates a pending active-thread
  `agent` task for the durable task daemon or external runners
- `task pause [id]` / `task resume [id]` pauses or resumes pending durable
  runtime tasks from the active thread; omitting `id` selects the first
  pending/paused task in the current task panel
- active-thread automation records are loaded from `.dscode/runtime/automations`;
  `automation trigger [id] [prompt override]` creates a pending automation task
  through the runtime store
- `compact` / `compact <tail>` appends a durable active-thread compaction
  summary and `thread_compacted` audit event through the runtime store
- `c` / `cancel` records a durable `cancel_requested` event for the active
  running assistant turn; TUI-started agent runs stop at cancellation
  checkpoints and mark the assistant item/task `cancelled`
- TUI-started agent runs create a pre-run rollback snapshot in git worktrees
  and bind it to the durable assistant turn for `restore show` /
  `restore revert-turn`
- local file-backed TUI sessions expose rollback commands in the command
  palette: `restore snapshot [label]`, `restore list [limit]`,
  `restore show <snapshot-id|turn-id|last>`, and
  `revert turn <snapshot-id|turn-id|last> [--apply]`; `last` resolves to the
  active thread's latest durable turn id
- `diagnostics [--changed|paths...]` runs through the local diagnostics runner
  in file-backed TUI sessions and through `POST /v1/diagnostics` in HTTP
  runtime TUI sessions, so remote runtime mode can reuse the runtime process'
  warmed LSP diagnostics broker

Useful commands:

```bash
deepseek tui
deepseek tui --demo
deepseek tui --demo --once
deepseek serve --http --addr 127.0.0.1:13000
deepseek tui --runtime-url http://127.0.0.1:13000
```

`--once` renders a deterministic ratatui test-backend snapshot to stdout. Use
it for CI and release smoke checks where a real TTY is not available.

Key bindings:

| Key | Behaviour |
|---|---|
| `Tab` | Cycle Plan / Agent / YOLO mode |
| `p`, `a`, `y` | Switch directly to Plan, Agent, or YOLO |
| `i` | Focus composer |
| `Enter` | Submit composer text while focused |
| `Left`, `Right` | Move the focused composer or command palette cursor |
| `Backspace`, `Delete` | Edit the focused composer or command palette text |
| `Up`, `Down`, `PageUp`, `PageDown` | Scroll transcript history when no modal input is active |
| `Home`, `End` | Move the focused input cursor, or jump transcript scrollback to oldest/newest when no modal input is active |
| `:` | Open command palette |
| `s` | Open session picker |
| `t` | Open thread navigator |
| `!` | Open approval modal |
| `c` | Cancel the active running assistant turn |
| `q`, `Esc` | Quit, or close the active modal |

Command palette commands currently implemented:

| Command | Behaviour |
|---|---|
| `mode plan`, `plan` | Switch to Plan mode |
| `mode agent`, `agent` | Switch to Agent mode |
| `mode yolo`, `yolo` | Switch to YOLO mode |
| `sessions` | Open the session picker |
| `threads`, `thread` | Open the thread navigator |
| `thread next`, `thread prev` | Move between durable threads in the selected session |
| `thread <id>` | Jump to a durable thread by id, switching sessions if needed |
| `tasks`, `task` | Show active-thread task count in the status bar |
| `task <summary>`, `task create <summary>` | Create a pending active-thread runtime task |
| `task pause`, `task pause <id>` | Pause a pending active-thread runtime task |
| `task resume`, `task resume <id>` | Resume a paused active-thread runtime task |
| `automations`, `automation` | Show active-thread automation count in the status bar |
| `automation trigger`, `automation run` | Trigger the first active automation in the current thread |
| `automation trigger <id> [prompt]` | Trigger one current-thread automation with an optional prompt override |
| `compact`, `compact <tail>` | Compact the active durable thread, keeping the latest N turns |
| `thread compact`, `thread compact <tail>` | Alias for active thread compaction |
| `diagnostics`, `diagnostics <paths...>` | Run local workspace or path-scoped diagnostics and summarize the result in the status bar |
| `diagnostics --changed`, `diag changed` | Run diagnostics against git changed files |
| `restore snapshot [label]` | Create a local rollback snapshot from the current git worktree |
| `restore list [limit]` | Summarize recent local rollback snapshots in the status bar |
| `restore show <id|last>` | Summarize one rollback snapshot or runtime-turn-bound snapshot |
| `revert turn <id|last> [--apply]` | Dry-run or apply a local rollback snapshot |
| `approval` | Open the approval modal |
| `cancel`, `stop` | Cancel the active running assistant turn |

Current boundaries are explicit: this is a true TUI shell with a first
agent-connected composer path, not the complete workbench yet. Agent responses
run in the background and stream into a running durable assistant item; for
TUI-started runs those item updates also repaint through the live channel at the
TUI draw cadence instead of waiting for the 1s durable refresh. Cancellation now
reaches cancel-aware model/tool paths, and `run_shell` kills its process group
when the durable cancel event is seen. External runtime writers are surfaced by
the local watcher for file-backed TUI sessions, and HTTP-runtime TUI sessions
use per-thread SSE follow streams plus a slower HTTP refresh fallback for newly
created threads. Richer progress controls and fully interrupting a blocked
model socket read are still future work. Durable approval responses unblock
permission gates for TUI-started agent runs and background runtime runner tasks.
Command palette actions cover local UI commands plus the first runtime
mutations for approval, cancellation, message submit, and active-thread
compaction/automation triggering. Rollback commands are local-only because they
operate on the client's git worktree; HTTP-runtime TUI sessions report that
rollback requires local file-backed TUI. General external command execution is
still future work.
