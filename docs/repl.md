# `deepseek chat` — REPL Mode

`deepseek chat` enters a persistent interactive REPL with cross-turn transcript,
slash commands, and JSON session save/load. Each user message triggers an agent
loop with up to 20 steps (configurable via `/budget`). Bare `deepseek` now
starts the full-screen TUI workbench when stdin and stdout are both real TTYs.

Explicit aliases are also supported:

- `deepseek repl`
- `deepseek interactive`

## Prerequisites

- A real terminal (TTY). Piped stdin is rejected; use `deepseek exec -`
  for one-shot tasks in scripts.
- Optional: `DEEPSEEK_API_KEY` exported for live LLM-driven planning.
  Without it, the offline planner produces shallow output.

## Slash commands

| Command | Behaviour |
|---|---|
| `/quit`, `/q`, `/exit` | Exit the REPL (exit code 0) |
| `/help`, `/h`, `/?` | Show this help |
| `/clear` | Wipe transcript and token counters; keep budget and skill |
| `/compact` | Summarize older transcript turns and keep the recent tail verbatim |
| `/budget [N]` | Show current budget; or set new value (1..200) |
| `/skill [name\|-]` | Show / switch / clear the active skill |
| `/diff` | Show pending git diff |
| `/restore snapshot [label]` | Capture a rollback snapshot for tracked changes, untracked files, directory metadata, and supported Unix special files |
| `/restore list` | List recent rollback snapshots |
| `/restore show <id\|last>` | Inspect rollback snapshot metadata by snapshot id, bound runtime turn id, or the latest REPL turn snapshot |
| `/revert_turn <id\|last> [--apply]` | Dry-run or apply a rollback snapshot by snapshot id, bound runtime turn id, or `last`; dry-run is the default |
| `/save <name>` | Save the session to `.dscode/sessions/<name>.json` |
| `/load <name>` | Restore a saved session (replaces current state) |
| `/todos` | Show the current todo list (read-only inspection) |
| `/cost` | Show prompt / completion / total token counters |
| `/mcp/<server>/<prompt> [json]` | Load an MCP prompt and submit it as the next user turn |
| `/mcp__server__prompt [json]` | Claude-style alias for an MCP prompt slash command |
| `/name [args]` | Run a custom markdown command from `.dscode/commands/name.md` or the configured user commands dir |

For each submitted prompt, the REPL tries to create a pre-turn rollback
snapshot when the current directory is a git worktree. Use `/revert_turn last`
to inspect the restore plan for the latest REPL turn, and
`/revert_turn last --apply` to restore it.

### Custom Slash Commands

Custom commands are prompt-backed markdown files for repeated workflows. Store project commands in
`.dscode/commands/` and personal commands in `~/.config/dscode/commands/` by default. User commands
override project commands with the same name.
The local TUI uses the same command files from both the composer and command
palette.

Examples:

```text
.dscode/commands/review.md
.dscode/commands/pr/fix.md
~/.config/dscode/commands/commit-message.md
```

Invoke them by filename:

```text
> /review src/repl/slash.rs
> /pr/fix 42
```

Inside the markdown body, `$ARGUMENTS` expands to all arguments, `$0` / `$1` expand positional
arguments, and `$ARGUMENTS[0]` / `$ARGUMENTS[1]` are the long indexed forms. If no argument
placeholder appears, DeepseekCode appends `ARGUMENTS: ...` to the prompt automatically.

### MCP Prompt Slash Commands

Connected MCP servers can expose prompt templates through `prompts/list` and `prompts/get`.
DeepseekCode can load those prompts directly from the REPL:

```text
> /mcp/github/review_pr {"number":42}
> /mcp__github__review_pr {"number":42}
```

The prompt result is wrapped with source metadata and submitted as the next user turn. JSON
arguments are optional, but when present they must be a JSON object.

## Workspace Instructions

At the start of each agent loop, DeepseekCode loads bounded markdown instructions into the system
prompt. This gives repeated project rules a first-class place instead of requiring users to paste
them into every task.

Default sources:

```text
~/.config/dscode/AGENTS.md
<git-root>/AGENTS.override.md
<git-root>/AGENTS.md
<git-root>/CLAUDE.md
<git-root>/.claude/CLAUDE.md
```

For subdirectories, DeepseekCode walks from the git root to the current directory. At each directory
level it reads the first existing file in this precedence order: `AGENTS.override.md`, `AGENTS.md`,
`CLAUDE.md`, `.claude/CLAUDE.md`. Later files are appended later in the prompt, so more local
instructions naturally win when they conflict. Each loaded file is capped at 32 KiB.

Set `workspace.user_instructions_file = ""` in `.dscode/config.toml` to disable user-level
instructions, or point it at another personal instruction file.

## Hooks

Hooks are local executable scripts for lightweight policy and context injection. They are disabled
by default; enable them explicitly in `.dscode/config.toml`:

```text
hooks.enabled = true
hooks.project_dir = ".dscode/hooks"
hooks.user_dir = "~/.config/dscode/hooks"
hooks.timeout_ms = 5000
```

Supported event directories:

```text
.dscode/hooks/session_start/*
.dscode/hooks/session_stop/*
.dscode/hooks/user_prompt_submit/*
.dscode/hooks/pre_tool_use/*
.dscode/hooks/permission_request/*
.dscode/hooks/post_tool_use/*
.dscode/hooks/subagent_start/*
.dscode/hooks/subagent_stop/*
.dscode/hooks/pre_compact/*
.dscode/hooks/shell_env/*
```

Scripts must be executable. DeepseekCode runs user hooks first, then project hooks, in lexical path
order. Each script receives a JSON payload on stdin and `DSCODE_HOOK_EVENT` in the environment.
`user_prompt_submit`, `pre_tool_use`, and `permission_request` scripts block the turn or tool call
when they exit nonzero or return `{"decision":"deny","reason":"..."}`. Other hook failures are
added back as advisory hook observations.
`shell_env` runs immediately before `run_shell`, `exec_shell`, and `task_shell_start` tool
execution. Its stdout is parsed as `KEY=VALUE` or `export KEY=VALUE` lines and injected into that
one shell process; only applied key names are reported back to the model, not values.

## Cross-turn context

Every user message is appended to the transcript; the LLM receives the
full conversation (user, assistant, tool turns) as part of the next
prompt. To keep token usage bounded:

- The latest 3 assistant turns are sent verbatim; older assistant turns
  are head-truncated to their first line.
- Tool outputs run through the same per-kind summarisation as the
  one-shot loop (shell tail, file head, diff hunk-headers).

`/compact` mutates the transcript: older turns are replaced by one
assistant summary turn, while the latest 8 turns are kept verbatim. If
hooks are enabled, DeepseekCode runs `pre_compact` before rewriting the
transcript; hook output is printed as advisory context.

`/clear` wipes the transcript when you want to start fresh without
restarting the binary.

Streaming token output is enabled by default — see
[`docs/streaming.md`](streaming.md) for protocol detail and color rules.

## Sessions

`/save <name>` writes the full transcript + budget + skill + token
counters + todos to `.dscode/sessions/<name>.json` atomically (temp file +
rename). `/load <name>` parses the JSON, accepts schema version 1 or 2,
and replaces the current REPL state — or fails without modifying state
if the file is missing, corrupt, or has an unknown version.

### Schema v2

```json
{
  "version": 2,
  "name": "fix-pr-42",
  "saved_at": "epoch+1745960000",
  "skill": "pr-review",
  "budget": 30,
  "transcript": [
    {"role": "user", "content": "..."},
    {"role": "assistant", "content": "..."},
    {"role": "tool", "name": "read_file", "input": {"path": "x.rs"}, "output": "...", "status": "ok"}
  ],
  "tokens": {"prompt": 12345, "completion": 6789},
  "todos": [
    {"content": "Run tests", "activeForm": "Running tests", "status": "pending"}
  ]
}
```

Schema v1 is still accepted for older saved sessions; it has the same shape
without `todos`. Loading v1 gives an empty todo list in memory, and the next
`/save` writes v2.

The runtime integration contract is tracked separately in
[`docs/runtime.md`](runtime.md).

## Current limitations

- No up/down arrow history. Use `rlwrap deepseek` for a quick
  workaround.
- Ctrl+C does not interrupt an in-flight LLM call; let the curl call
  finish or `Ctrl+\` to force-kill.
- `/save` overwrites without confirmation.
- `saved_at` uses an epoch-second placeholder rather than RFC3339
  (no chrono dependency in v1).

These are tracked as Phase 9b candidates.

## Examples

Start a session, ask a question, save:
```
$ deepseek
> what does src/repl/repl.rs do?
[planner runs, prints tool calls and final assistant message]
> /save my-investigation
saved -> .dscode/sessions/my-investigation.json
> /quit
```

Restart and resume:
```
$ deepseek
> /load my-investigation
loaded my-investigation (transcript: 2 turns, tokens: 1234 / 567)
> can you also check src/repl/transcript.rs?
```

Switch skills mid-session:
```
> /skill pr-review
skill switched to pr-review
> review the latest commit
```
