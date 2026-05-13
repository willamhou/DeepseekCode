# `deepseek tui` — Terminal Workbench

`deepseek tui` starts the ratatui/crossterm full-screen workbench shell.

Current surfaces:

- Plan / Agent / YOLO mode tabs
- sidebar with mode, selected durable session metadata, and key hints
- transcript backed by durable thread items when available
- composer input that appends user turns/items to the active durable thread
  and starts a background agent response in interactive TUI sessions
- composer `# <note>` memory capture and `/memory` local commands for
  opt-in user memory without starting a model turn
- persistent workspace notes with `note` / `/note`, including
  `note add|list|show|edit|remove|clear|path`
- hook inspection with `hooks` / `/hooks`, including `hooks list` and
  `hooks events` over the configured project/user hook roots
- composer and command palette custom slash commands from project
  `.dscode/commands/*.md` or the configured user commands dir
- composer slash-command hints and `Tab` completion for built-in local
  slash commands, project `.dscode/commands/**/*.md` entries, configured user
  command entries, and configured skill names while typing `/...`
- feedback links with `feedback` / `/feedback` and
  `feedback bug|feature|security`
- repository and DeepSeek API links with `links` / `/links` plus
  `dashboard` / `/dashboard` and `api` / `/api` aliases
- home dashboard with `home` / `/home` plus `stats` / `/stats` and
  `overview` / `/overview` aliases
- session goal tracking with `goal` / `/goal`, including optional token budget
  display from active-thread usage telemetry
- slash-mode switching with `mode` / `/mode` and
  `mode agent|plan|yolo|1|2|3`
- help index and command topics with `help` / `/help`, `help <command>`, and
  `/?`
- settings overview with `settings` / `/settings` and `config` / `/config`
- local TUI theme switching with `theme` / `/theme` and
  `theme dark|light|grayscale|system`
- statusline overview with `statusline` / `/statusline`
- verbose transcript switching with `verbose` / `/verbose` and
  `verbose on|off|show`, keeping reasoning compact by default while allowing
  full live thinking text on demand
- context inspection with `context` / `/context` and `ctx` / `/ctx`, showing
  active-thread context window, compaction strategy, token/cache telemetry, and
  reasoning replay state
- slash quit aliases with `exit` / `/exit`, `quit` / `/quit`, and `q` / `/q`
- composer draft stash: `Ctrl+S` parks the current draft, and
  `stash list|pop|clear` / `/stash list|pop|clear` manage parked drafts
- session rename from the command palette or slash-style composer command with
  `rename <title>` / `/rename <title>`
- project instruction initialization with `init` / `/init`, creating
  `AGENTS.md` in the selected session workspace
- network policy controls with `network list|allow|deny|remove|default`,
  editing the selected session workspace `.dscode/config.toml`
- runtime status inspection with `status` / `/status`, summarizing selected
  session, active thread, transcript items, tasks, automations, pending
  approvals/user input, token/cache telemetry, context policy, and estimated
  cost in the detail panel
- token and cost inspection with `tokens` / `/tokens` and `cost` / `/cost`,
  matching DeepSeek-TUI's runtime usage and approximate spend commands
- cache telemetry inspection with `cache [count]` / `/cache [count]` plus
  read-only `cache inspect` / `cache warmup` explanations over durable usage
  records
- model configuration inspection and switching with `model` / `/model`,
  `model <name>` / `/model <name>`, and offline `models` / `/models`
- provider preset inspection and switching with `provider` / `/provider`,
  `provider list`, and `provider <name> [model]`
- skill registry inspection with `skills [prefix]` / `/skills [prefix]` and
  `skill <name>` / `/skill <name>` over DeepSeekCode's configured TOML skills
- composer and command-palette editing preserve terminal modifier keys, including
  Ctrl-based line, word, and cursor controls
- task panel with active thread status, runtime item count, item state/type
  progress counts, latest item summary, recent runtime tasks, active-thread
  automations, usage total, cache-hit rate, cache chart, estimated cost,
  input/output cost split, cost chart, and 1M-context policy when usage records
  exist
- command palette with local UI commands and active-thread runtime actions
- command palette history with `Up` / `Down` recall while the palette is active
- command palette prefix completion with `Tab` for built-in workbench commands
- session picker populated from `.dscode/runtime/sessions`, linked threads, and
  item timelines
- thread navigator populated from the selected session's durable runtime
  threads
- session picker and thread navigator filters through `session filter <query>`
  and `thread filter <query>` for large durable runtime lists
- mouse capture for workbench navigation: click Plan/Agent/YOLO tabs to switch
  modes, click visible session/thread picker rows to select them, scroll the
  wheel to reuse the active scroll/navigation target, and click the transcript
  panel to focus the composer
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
  compaction actions back through HTTP, and subscribes to the aggregate
  `/v1/events/stream?follow=1` runtime stream so foreground refresh covers
  known and newly created threads
- approval modal backed by durable `permission_request` runtime events
  appended directly or through `POST /v1/threads/{id}/events`
- approval accept/deny records durable `permission_response` events and can
  unblock permissioned tools for agent runs started from the TUI composer
- user-input modal backed by durable `user_input_request` runtime events;
  number keys choose predefined options, and `o` opens a short free-form Other
  answer editor that writes the same structured response event
- background TUI agent runs append assistant messages, tool result items, usage
  records, and completed/failed task records back into the active thread
- active-thread runtime task records are loaded from `.dscode/runtime/tasks`
  and rendered in the task panel with status counts, short task ids, updated
  timestamps, clipped summaries, and a `>` marker for the selected task
- task panel rows support cross-surface multi-select with Ctrl+click,
  drag-select, `task select all`, and `task select clear`; when tasks are
  selected, `task pause`, `task resume`, and `task cancel` operate on compatible
  selected tasks, and explicit `task bulk pause|resume|cancel` aliases are
  available
- active-thread runtime items are summarized in the task panel with state/type
  counts and the latest item, so streamed agent progress and tool activity are
  visible while a background run is active
- persisted reasoning items can be inspected from the command palette with
  `reasoning`, `reasoning latest`, `reasoning show <selector>`, and
  `reasoning search <query>`; the same panel exposes `reasoning replay <N>` and
  `reasoning pin <selector>` for controlling which persisted reasoning items
  local TUI-started agent runs replay into the next request. The local
  file-backed TUI stores the replay limit and pinned turn ids in
  `.dscode/tui/reasoning-replay.json`.
- `task <summary>` / `task create <summary>` creates a pending active-thread
  `agent` task for the durable task daemon or external runners
- `task next`, `task prev`, and `task select <id>` move the selected task in
  the active thread's task panel; left-clicking a visible task row also selects
  it
- `task pause [id]`, `task resume [id]`, and `task cancel [id]` control durable
  runtime tasks from the active thread; omitting `id` uses the selected task
  when its status is compatible, then falls back to the first matching
  pending/paused/cancellable task in the current task panel
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
  `restore show <snapshot-id|turn-id|last>`,
  `restore hunks <snapshot-id|turn-id|last>`,
  `restore hunk <snapshot-id|turn-id|last> [index]`, and
  `revert turn <snapshot-id|turn-id|last> [--apply]`; `last` resolves to the
  active thread's latest durable turn id, and list/show/revert results render a
  scrollable right-side rollback detail panel with patch or restore-plan
  context. Apply restores open a confirmation modal before mutating files
- `diagnostics [--changed|paths...]` runs through the local diagnostics runner
  in file-backed TUI sessions and through `POST /v1/diagnostics` in HTTP
  runtime TUI sessions, so remote runtime mode can reuse the runtime process'
  warmed LSP diagnostics broker
- local file-backed TUI sessions expose a safe background shell command path
  through `shell <command>` / `shell run <command>` and `! <command>`. Commands
  use the same allowlist as `exec_shell` by default; unallowlisted foreground
  commands open an explicit approval modal and only run once after approval.
  Approved commands start as process-local background jobs, and can be listed
  with `shell list`, inspected with `shell show <id>` /
  `shell attach <id> [cursor|tail]` / `shell poll <id>` /
  `shell wait <id> [ms]`, fed with `shell stdin <id> <input>` /
  `shell close-stdin <id>`, resized with `shell resize <id> <rows> <cols>`,
  stopped with `shell cancel <id|all>`, or paired with workspace-local
  supervisor protocol health through `shell supervisor`; `/jobs`-style
  `jobs list|show|attach|poll|wait|stdin|close-stdin|resize|cancel|supervisor`
  aliases are also accepted
- local file-backed TUI sessions expose user-memory controls when
  `memory.enabled = true` or `DSCODE_MEMORY=on`: a composer line beginning with
  a single `#` appends a durable memory note without submitting a user turn, and
  `/memory show|path|clear|edit|help` plus command-palette
  `memory show|path|clear|edit|help` inspect or manage the configured
  `memory.memory_path`
- local file-backed TUI sessions expose an MCP manager screen through `mcp` /
  `mcp manager`; `mcp manager tools|prompts|resources|resource-templates [server]`
  renders discovery summaries in that full-width screen. The manager includes
  overview/tools/prompts/resources/templates/health tabs and supports
  `mcp manager tab <tab>` plus `Tab` / `Shift+Tab` keyboard cycling, `r` reload,
  `n` / `p` server selection, selected-server `e` enable, `d` disable,
  `x` remove, `t` tools, `Space` multi-select toggle, `A` select visible,
  `U` clear selection, `E` / `D` bulk enable/disable selected servers, and
  `mcp manager filter <query>` for line filtering.
  The full-width manager also supports mouse tab clicks, server-row selection,
  Ctrl+click server-row multi-select toggles, drag-select across visible server
  rows, and action-strip clicks for enable, disable, remove, tools, and reload.
  When server rows are
  multi-selected, enable/disable action-strip clicks apply to the selection.
  They also expose
  project-level MCP manager commands in the command palette: `mcp init [--force]`,
  `mcp add stdio <name> <command> [args...]`, `mcp add http <name> <url>`,
  `mcp add sse <name> <url>`, `mcp enable|disable|remove <name>`, and
  `mcp validate`; they also expose `mcp tools [server]`,
  `mcp prompts [server]`, `mcp resources [server]`, and
  `mcp resource-templates [server]` detail views in the scrollable right-side
  panel; `Esc` or `mcp close` returns that panel to the task view. The same
  add/enable/disable/remove commands can target user config with
  `mcp user ...`; unscoped commands keep using project config.

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
| `Tab` | Complete command-palette input while the palette is active; complete built-in, project, user, or skill `/...` commands while the composer is focused; otherwise cycle Plan / Agent / YOLO mode |
| `Tab`, `Shift+Tab` | Cycle MCP manager tabs while the full-width MCP manager is visible |
| `n`, `p`, `e`, `d`, `x`, `t`, `r`, `Space`, `A`, `U`, `E`, `D` | Select next/previous MCP server, enable, disable, remove, show tools, reload, toggle/select/clear multi-select, or bulk enable/disable while the full-width MCP manager is visible |
| `p`, `a`, `y` | Switch directly to Plan, Agent, or YOLO |
| `i` | Focus composer |
| `Enter` | Submit composer text while focused |
| `Left`, `Right` | Move the focused composer or command palette cursor |
| `Backspace`, `Delete` | Edit the focused composer or command palette text |
| `Ctrl+A`, `Ctrl+E` | Move the focused composer or command palette cursor to start/end |
| `Ctrl+U`, `Ctrl+K`, `Ctrl+W` | Clear line, delete to end of line, or delete previous word in the focused composer or command palette |
| `Ctrl+S` | Stash the focused composer draft and clear the composer |
| `Ctrl+Left`, `Ctrl+Right` | Move by word in the focused composer or command palette |
| `Ctrl+C` | Quit the TUI |
| `Up`, `Down`, `PageUp`, `PageDown` | Recall command-palette history while the palette is active; move through session/thread pickers while visible; scroll the MCP manager/detail panel while visible; otherwise scroll transcript history |
| `Home`, `End` | Move the focused input cursor; jump session/thread pickers, MCP manager/detail, or transcript scrollback to first/last positions |
| `:` | Open command palette |
| `s` | Open session picker |
| `t` | Open thread navigator |
| `!` | Open approval modal |
| `1`, `2`, `3`, `o` | Answer an active user-input modal with a predefined option or an Other answer |
| `c` | Cancel the active running assistant turn |
| `q`, `Esc` | Quit, close the active modal, or close MCP manager/detail while it is visible |

Mouse controls:

| Mouse | Behaviour |
|---|---|
| Left click Plan / Agent / YOLO tab | Switch mode |
| Left click visible session picker row | Select that session and close the picker |
| Left click visible thread navigator row | Select that thread and close the navigator |
| Left click MCP manager tab | Switch full-width MCP manager tab |
| Left click MCP manager server row | Select that server for action-strip commands |
| Ctrl+left click MCP manager server row | Toggle that server in the multi-select set |
| Drag over MCP manager server rows | Select the visible server-row range for bulk actions |
| Left click MCP manager action strip | Run the clicked enable, disable, remove, tools, or reload action |
| Left click visible task panel task row | Select that active-thread runtime task for default task actions |
| Ctrl+left click visible task panel task row | Toggle that task in the multi-select set |
| Drag over visible task panel task rows | Select the visible task-row range for bulk actions |
| Left click transcript panel | Focus composer input |
| Scroll wheel | Reuse the active PageUp/PageDown target: picker navigation, MCP/detail scroll, or transcript scrollback |

Command palette commands currently implemented:

| Command | Behaviour |
|---|---|
| `help`, `/help`, `/?` | Show the TUI help index in the right-side detail panel |
| `help <command>`, `/help <command>` | Show command-specific usage, aliases, and description |
| `settings`, `/settings`, `config`, `/config` | Show mode, config file locations, and focused configuration command entry points |
| `theme`, `/theme` | Cycle the local TUI theme and show the theme detail panel |
| `theme show`, `/theme show` | Show current theme and available theme commands |
| `theme dark|light|grayscale|system`, `/theme dark|light|grayscale|system` | Switch the local TUI theme |
| `statusline`, `/statusline` | Show command bar items, shortcuts, and related status/config commands |
| `verbose`, `/verbose` | Toggle whether live reasoning text is rendered in full in the transcript |
| `verbose on|off|show`, `/verbose on|off|show` | Enable, disable, or inspect verbose transcript mode |
| `context`, `/context`, `ctx`, `/ctx` | Show active-thread context window, token/cache, item, and reasoning replay state |
| `goal`, `/goal` | Show the current TUI session goal and token budget progress |
| `goal <objective> [budget: N]`, `/goal <objective> [budget: N]` | Set or replace the current TUI session goal |
| `goal clear`, `/goal clear` | Clear the current TUI session goal |
| `exit`, `/exit`, `quit`, `/quit`, `q`, `/q` | Quit the TUI workbench |
| `mode`, `/mode` | Show current mode and mode-switching commands in the right-side detail panel |
| `mode agent|plan|yolo|1|2|3`, `/mode agent|plan|yolo|1|2|3` | Switch Plan / Agent / YOLO mode |
| `mode plan`, `plan` | Switch to Plan mode |
| `mode agent`, `agent` | Switch to Agent mode |
| `mode yolo`, `yolo` | Switch to YOLO mode |
| `sessions` | Open the session picker |
| `session filter <query>`, `session filter` | Filter or clear visible sessions in the session picker |
| `threads`, `thread` | Open the thread navigator |
| `thread filter <query>`, `thread filter` | Filter or clear visible threads in the thread navigator |
| `thread next`, `thread prev` | Move between durable threads in the selected session |
| `thread <id>` | Jump to a durable thread by id, switching sessions if needed |
| `/name [args]` | Expand a custom markdown slash command from `.dscode/commands/name.md` or the configured user commands dir, then submit it to the active thread |
| `init`, `/init` | Create project `AGENTS.md` instructions in the selected session workspace |
| `rename <title>`, `/rename <title>` | Rename the selected durable session and persist the new title |
| `stash`, `stash list`, `/stash list` | List parked composer drafts in the right-side detail panel |
| `stash pop`, `/stash pop` | Restore the most recently stashed composer draft |
| `stash clear`, `/stash clear` | Clear all parked composer drafts |
| `tasks`, `task` | Show active-thread task count in the status bar |
| `task <summary>`, `task create <summary>` | Create a pending active-thread runtime task |
| `task next`, `task prev` | Move the selected active-thread runtime task |
| `task select <id>` | Select an active-thread runtime task by id |
| `task select all`, `task select clear` | Select visible task-panel rows for bulk actions, or clear selected tasks |
| `task pause`, `task pause <id>` | Pause a pending active-thread runtime task |
| `task resume`, `task resume <id>` | Resume a paused active-thread runtime task |
| `task cancel`, `task cancel <id>` | Cancel a pending, paused, or running active-thread runtime task |
| `task bulk pause`, `task bulk resume`, `task bulk cancel` | Apply the default task action to compatible selected task-panel rows |
| `shell <command>`, `shell run <command>`, `! <command>` | Start an allowlisted local background shell job, or request foreground approval for an unallowlisted command |
| `shell list`, `jobs list` | List known local background shell jobs |
| `shell show <id>`, `jobs show <id>` | Show a shell job snapshot with accumulated output |
| `shell attach <id> [cursor|tail]`, `jobs attach <id> [cursor|tail]` | Replay terminal-oriented durable stdout PTY/log bytes |
| `shell poll <id>`, `jobs poll <id>` | Poll one local background shell job without waiting |
| `shell wait <id> [ms]`, `jobs wait <id> [ms]` | Wait briefly for one local background shell job and show output deltas |
| `shell stdin <id> <input>`, `jobs stdin <id> <input>` | Send stdin to a running local background shell job |
| `shell close-stdin <id>`, `jobs close-stdin <id>` | Close stdin for a running local background shell job |
| `shell resize <id> <rows> <cols>`, `jobs resize <id> <rows> <cols>` | Resize a TTY-backed shell job with best-effort control |
| `shell cancel <id|all>`, `jobs cancel <id|all>` | Cancel one or all local background shell jobs |
| `shell supervisor`, `jobs supervisor` | Show workspace-local shell supervisor manifest, socket, and protocol health |
| `memory`, `memory show` | Show configured user memory in the right-side detail panel |
| `memory path` | Show the configured user memory path and enabled/disabled state |
| `memory clear` | Empty the configured user memory file when memory is enabled |
| `memory edit` | Print the editor command for the configured user memory file |
| `memory help` | Show local memory command help |
| `note <text>`, `/note <text>`, `note add <text>` | Append a persistent workspace note to `memory.notes_path` |
| `note list`, `note show <n>` | List notes or show one note in the right-side detail panel |
| `note edit <n> <text>`, `note remove <n>`, `note clear`, `note path` | Replace, remove, clear, or locate persistent workspace notes |
| `hooks`, `/hooks`, `hooks list`, `/hooks list` | Show hook enabled state, timeout, project/user roots, event directories, and executable scripts |
| `hooks events`, `/hooks events`, `hook events`, `/hook events` | Show supported hook event directory names |
| `network`, `network list`, `/network list` | Show `network.default`, `network.allow`, and `network.deny` in the right-side detail panel |
| `network allow <host>`, `/network allow <host>` | Allow a host in the selected workspace project config |
| `network deny <host>`, `/network deny <host>` | Deny a host in the selected workspace project config |
| `network remove <host>`, `/network remove <host>` | Remove a host from allow and deny lists |
| `network default <allow\|deny\|prompt>` | Set the default host policy |
| `status`, `/status` | Show selected session, active thread, task/input, usage, cache, context, and cost status in the right-side detail panel |
| `tokens`, `/tokens` | Show active-thread context, last input/output tokens, cache hit/miss, cumulative token usage, and approximate cost |
| `cost`, `/cost` | Show active-thread approximate total, input, and output cost with telemetry caveats |
| `cache`, `/cache`, `cache <count>` | Show active-thread durable cache hit/miss summary, hit rate, cache chart, context, and approximate cost |
| `cache inspect`, `cache warmup` | Explain durable read-only cache limits: no persisted prompt layer hashes and no TUI-issued warmup request |
| `model`, `/model` | Show selected workspace model config in the right-side detail panel |
| `model <name>`, `/model <name>` | Update selected workspace `model.model`; aliases include `auto`, `flash`, `pro`, `chat`, `coder`, and `reasoner` |
| `models`, `/models`, `model list` | Show the offline DeepSeekCode model catalog and current project model |
| `provider`, `/provider` | Show selected workspace provider preset inferred from `model.base_url` |
| `provider list` | Show supported provider presets: DeepSeek, NVIDIA NIM, OpenAI-compatible, AtlasCloud, OpenRouter, Novita, Fireworks, SGLang, vLLM, and Ollama |
| `provider <name> [model]`, `/provider <name> [model]` | Update selected workspace `model.base_url`, `model.api_key_env`, and `model.model` with provider defaults or an optional model override |
| `skills`, `/skills`, `skills <prefix>` | List configured TOML skills from repo and user skill directories |
| `skill <name>`, `/skill <name>` | Show one configured TOML skill's description, triggers, tools, references, policy, and system append |
| `feedback`, `/feedback` | Show DeepSeekCode feedback targets in the right-side detail panel |
| `feedback bug|feature|security`, `/feedback bug|feature|security` | Show GitHub issue or security-policy links for the selected feedback type |
| `links`, `/links`, `dashboard`, `/dashboard`, `api`, `/api` | Show DeepSeekCode repository/docs links and DeepSeek platform/API docs in the right-side detail panel |
| `home`, `/home`, `stats`, `/stats`, `overview`, `/overview` | Show a compact runtime dashboard with session/thread, task, usage, pending input, and quick-action links |
| `automations`, `automation` | Show active-thread automation count in the status bar |
| `automation trigger`, `automation run` | Trigger the first active automation in the current thread |
| `automation trigger <id> [prompt]` | Trigger one current-thread automation with an optional prompt override |
| `compact`, `compact <tail>` | Compact the active durable thread, keeping the latest N turns |
| `thread compact`, `thread compact <tail>` | Alias for active thread compaction |
| `reasoning`, `reasoning list` | Show active-thread reasoning items in the right-side detail panel |
| `reasoning latest`, `reasoning show <latest\|index\|item-id\|turn-id>` | Show full reasoning item content |
| `reasoning search <query>` | Show matching reasoning items with highlighted excerpts |
| `reasoning replay <0..20>` | Set how many persisted reasoning entries local TUI agent runs replay |
| `reasoning pin <latest\|index\|item-id\|turn-id>` | Keep one reasoning turn in local replay beyond the latest-N window |
| `reasoning pins`, `reasoning unpin <selector\|all>` | Inspect or clear local reasoning replay pins |
| `mcp`, `mcp manager`, `mcp open` | Open the full-width MCP manager screen with merged inventory, config sources, and common actions |
| `mcp manager tab overview|tools|prompts|resources|resource-templates|health` | Switch the full-width MCP manager tab |
| `mcp manager filter <query>`, `mcp manager filter` | Filter or clear visible lines in the full-width MCP manager screen |
| `mcp manager tools|prompts|resources|resource-templates [server]` | Show MCP discovery summaries in the full-width manager screen |
| `mcp list`, `mcp status`, `mcp reload` | Summarize merged MCP config inventory in the status bar |
| `mcp tools [server]` | List configured MCP server tools in the scrollable right-side panel |
| `mcp prompts [server]` | List configured MCP server prompts in the scrollable right-side panel |
| `mcp resources [server]` | List configured MCP server resources in the scrollable right-side panel |
| `mcp resource-templates [server]`, `mcp templates [server]` | List MCP resource URI templates in the scrollable right-side panel |
| `mcp close`, `mcp clear` | Close the MCP manager/detail panel and restore the main workbench |
| `mcp init`, `mcp init --force` | Create or replace project `.dscode/mcp.json` |
| `mcp add stdio <name> <command> [args...]` | Add a project-level stdio MCP server |
| `mcp add http <name> <url>` | Add a project-level HTTP MCP server |
| `mcp add sse <name> <url>` | Add a project-level SSE MCP server |
| `mcp enable <name>`, `mcp disable <name>` | Enable or disable a project MCP server |
| `mcp remove <name>` | Remove a project MCP server |
| `mcp user add stdio|http|sse ...` | Add a user-level MCP server |
| `mcp user enable|disable|remove <name>` | Enable, disable, or remove a user MCP server |
| `mcp validate` | Validate enabled MCP servers and show tools/prompts/resources/templates health in the scrollable right-side panel |
| `diagnostics`, `diagnostics <paths...>` | Run local workspace or path-scoped diagnostics and summarize the result in the status bar |
| `diagnostics --changed`, `diag changed` | Run diagnostics against git changed files |
| `restore snapshot [label]` | Create a local rollback snapshot from the current git worktree |
| `restore list [limit]` | Show recent local rollback snapshots in the right-side detail panel |
| `restore show <id|last>` | Show one rollback snapshot or runtime-turn-bound snapshot with patch preview |
| `restore hunks <id|last>`, `restore diff <id|last>` | List parsed rollback patch hunks in the right-side detail panel |
| `restore hunk <id|last> [index]` | Show one 1-based rollback patch hunk |
| `revert turn <id|last> [--apply]` | Dry-run or apply a local rollback snapshot and show the restore plan; `--apply` requires modal confirmation |
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
use the aggregate HTTP SSE stream for cross-thread push, including newly
created threads. Runtime task progress now supports visible selection plus
default selected-task pause/resume/cancel actions. Durable approval responses unblock
permission gates for TUI-started agent runs and background runtime runner tasks.
Command palette actions cover local UI commands plus the first runtime
mutations for approval, cancellation, message submit, and active-thread
compaction/automation triggering. Session and thread pickers can now keep
command-palette filters while navigating large durable lists, and mouse capture
covers first-line mode, picker, scroll, and composer-focus actions. The MCP manager now has a full-width
inventory/config screen and can render tools/prompts/resources/templates
discovery in that screen; `Tab` and `Shift+Tab` cycle manager tabs, while `r`
refreshes the MCP inventory. The manager also keeps a selected-server action
strip so `n`/`p` can move selection and `e`/`d`/`x`/`t` can enable, disable,
open a remove confirmation modal, or inspect tools for that server. The shorter
discovery commands still use the scrollable right-side detail panel. Project
instruction init, session rename, rollback, memory, network policy, composer
stash, custom slash commands, and MCP manager commands are local-only because
they operate on the client's runtime/session files, git worktree, project/user
MCP config, configured MCP transports, `.dscode/tui/composer-stash.json`,
custom command markdown files, and user memory file; HTTP-runtime TUI
sessions report that those commands require local file-backed TUI. General
external command execution is currently limited to the allowlisted local
background shell path.
