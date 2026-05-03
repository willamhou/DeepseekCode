# todo_write — Phase 10a

The `todo_write` tool lets the LLM maintain a structured task list during a
session, modeled after Claude Code's TodoWrite. The list is **session-scoped**:
it lives across REPL turns inside `dscode chat`, can be saved/loaded via
`/save` / `/load`, and resets on `/clear` or new session.

## Schema

Each todo has three fields:

- `content` (imperative form, e.g. "Run tests")
- `activeForm` (present continuous, e.g. "Running tests")
- `status` (one of `pending`, `in_progress`, `completed`)

The list is replaced wholesale every call (LLM sends the new full state).

## When the LLM uses it

The system prompt nudges the LLM to use `todo_write` when:

- the request involves three or more distinct steps, OR
- it spans multiple files / non-trivial refactoring, OR
- it requires running tests or shell commands as part of completion.

The LLM is prompted to mark exactly one todo as `in_progress` at a time. dscode
does **not** strictly validate this — the renderer shows multiple
in_progress items if they appear, so the user can see the LLM going off track.

## Cap

Up to 100 todos per call. Beyond that, the tool errors out with a clear
message; this is defense-in-depth, not a typical limit (real workflows rarely
exceed 20-30 todos).

## Slash commands

- `/todos` — show the current list (read-only inspection).
- `/clear` — wipe transcript + todos + token counters.
- `/save <name>` — write current Repl state including todos to v2 JSON.
- `/load <name>` — restore from v1 (todos default to empty) or v2.

## Persistence

Saved sessions use schema v2:

```json
{
  "version": 2,
  "name": "...",
  "saved_at": "epoch+...",
  "skill": null,
  "budget": 20,
  "transcript": [...],
  "tokens": { "prompt": ..., "completion": ... },
  "todos": [
    { "content": "Run tests", "activeForm": "Running tests", "status": "pending" }
  ]
}
```

Loading a v1 file gives an empty todos list in memory; the file stays v1 until
the next `/save`, which silently upgrades it to v2.
