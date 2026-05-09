# `deepseek` — REPL Mode

`deepseek` enters a persistent interactive REPL with cross-turn
transcript, slash commands, and JSON session save/load. Each user
message triggers an agent loop with up to 20 steps (configurable via
`/budget`).

Explicit aliases are also supported:

- `deepseek chat`
- `deepseek repl`
- `deepseek interactive`

## Prerequisites

- A real terminal (TTY). Piped stdin is rejected; use `deepseek run "task"`
  for one-shot tasks in scripts.
- Optional: `DEEPSEEK_API_KEY` exported for live LLM-driven planning.
  Without it, the offline planner produces shallow output.

## Slash commands

| Command | Behaviour |
|---|---|
| `/quit`, `/q`, `/exit` | Exit the REPL (exit code 0) |
| `/help`, `/h`, `/?` | Show this help |
| `/clear` | Wipe transcript and token counters; keep budget and skill |
| `/budget [N]` | Show current budget; or set new value (1..200) |
| `/skill [name\|-]` | Show / switch / clear the active skill |
| `/diff` | Show pending git diff |
| `/save <name>` | Save the session to `.dscode/sessions/<name>.json` |
| `/load <name>` | Restore a saved session (replaces current state) |
| `/todos` | Show the current todo list (read-only inspection) |
| `/cost` | Show prompt / completion / total token counters |

## Cross-turn context

Every user message is appended to the transcript; the LLM receives the
full conversation (user, assistant, tool turns) as part of the next
prompt. To keep token usage bounded:

- The latest 3 assistant turns are sent verbatim; older assistant turns
  are head-truncated to their first line.
- Tool outputs run through the same per-kind summarisation as the
  one-shot loop (shell tail, file head, diff hunk-headers).

`/clear` wipes the transcript when you want to start fresh without
restarting the binary.

Streaming token output is enabled by default — see
[`docs/streaming.md`](streaming.md) for protocol detail and color rules.

## Sessions

`/save <name>` writes the full transcript + budget + skill + token
counters to `.dscode/sessions/<name>.json` atomically (temp file +
rename). `/load <name>` parses the JSON, validates schema version 1,
and replaces the current REPL state — or fails without modifying state
if the file is missing, corrupt, or has an unknown version.

### Schema v1

```json
{
  "version": 1,
  "name": "fix-pr-42",
  "saved_at": "epoch+1745960000",
  "skill": "pr-review",
  "budget": 30,
  "transcript": [
    {"role": "user", "content": "..."},
    {"role": "assistant", "content": "..."},
    {"role": "tool", "name": "read_file", "input": {"path": "x.rs"}, "output": "...", "status": "ok"}
  ],
  "tokens": {"prompt": 12345, "completion": 6789}
}
```

## v1 limitations

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
