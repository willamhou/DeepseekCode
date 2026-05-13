# Custom Subagents

Custom subagents are Markdown files with YAML frontmatter. Project agents live in
`.dscode/agents/*.md`; user agents live in `~/.config/dscode/agents/*.md`.

## File Format

```md
---
name: reviewer
description: Reviews code and points out concrete risks
tools: [read_file, search_text, git_diff]
model: deepseek-coder
---
Review the assigned scope. Prioritize correctness bugs, regressions, and missing
tests. Return concise findings with file paths and line numbers when possible.
```

Supported frontmatter fields:

- `name`: stable agent name, using letters, numbers, `_`, `-`, or `.`
- `description`: one-line summary shown in `deepseek agents list`
- `tools`: optional advisory tool list for the child task prompt
- `model`: optional advisory model label

The Markdown body is the subagent prompt.

## CLI Management

List configured agents:

```sh
deepseek agents list
```

Show one agent:

```sh
deepseek agents show reviewer
```

Validate all configured agent files:

```sh
deepseek agents validate
```

Validate one file:

```sh
deepseek agents validate .dscode/agents/reviewer.md
```

Run a pending durable runtime task that is linked to a runtime thread:

```sh
deepseek agents run-task task-...
deepseek agents run-task --budget 8 --json task-...
```

`run-task` claims the task, executes the local agent loop in the thread
workspace, appends user/assistant turns, tool results, usage, and task status
back to `.dscode/runtime`, and creates a pre-run rollback snapshot when the
workspace is a git worktree. Permissioned write/shell/MCP tool calls append a
durable `permission_request` event and wait for a matching
`permission_response`, so a TUI or HTTP runtime client can approve or deny a
background daemon task.

Run a local runtime daemon that polls pending tasks and due automations:

```sh
deepseek agents daemon
deepseek agents daemon --interval-ms 1000 --budget 8
deepseek agents daemon --once --json
```

`daemon` triggers active automations whose `next_run_at` is due, supports
recurring schedules such as `every:60s`, `every:5m`, and `@every 1h`, executes
one thread-linked pending task per tick through the same durable `run-task`
path, recovers stale live RLM ownership, and runs one queued live RLM turn per
tick.

Render local supervisor files for running the HTTP runtime, the durable task
and live RLM worker daemon, diagnostics watch, and the shell supervisor
protocol skeleton as long-lived services:

```sh
deepseek agents service --kind systemd --out ./services --workdir "$PWD" --bin "$(command -v deepseek)"
deepseek agents service --kind launchd --out ./services --workdir "$PWD" --bin "$(command -v deepseek)"
```

The generated files are reviewable templates; the command does not enable or
start services. Static placeholder templates also live under
`packaging/systemd/` and `packaging/launchd/`, while release packages include
them under `services/`.

The shell supervisor service runs `deepseek agents shell-supervisor --json`.
It binds the workspace-local `.dscode/shell-supervisor/supervisor.sock` and
writes a manifest for `exec_shell_supervisor_status`; native PTY sessions are
still a later implementation slice.

After a supervisor starts the agents daemon, use the RLM lifecycle commands to
check live worker state:

```sh
deepseek agents rlm-status --json
deepseek agents rlm-events <session_id> --cursor 0 --json
deepseek agents rlm-wait <session_id> --cursor 0 --timeout-ms 5000 --json
```

List subagent thread artifacts created by parallel dispatch:

```sh
deepseek agents threads
```

Show or switch the active thread:

```sh
deepseek agents show-thread thread-...
deepseek agents switch thread-...
deepseek agents current
deepseek agents clear-current
```

## Dispatch

`dispatch_subagent` accepts an optional `agent` argument. When set, DeepseekCode
loads the matching project or user agent and injects its prompt into the child
task. Project agents take precedence over user agents with the same name.

`dispatch_subagents` accepts a `tasks` JSON array and runs up to 4 child
subagents concurrently. Each task object requires `task` and may include
`agent`, `skill`, and `steps`:

```json
[
  {"task": "Review src/api.rs for correctness risks", "agent": "reviewer", "steps": "4"},
  {"task": "Inspect docs/install.md for release gaps", "steps": "3"}
]
```

The consolidated result includes per-child thread IDs, outcomes, next actions,
and `.dscode/agent-threads/*.md` artifacts. Use `deepseek agents switch <id>`
to mark the thread you want to inspect or continue from.
