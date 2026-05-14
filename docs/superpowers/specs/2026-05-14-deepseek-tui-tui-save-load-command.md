# DeepSeek-TUI Parity: TUI Save and Load Commands

## Context

DeepSeek-TUI exposes `/save [path]` and `/load <path>` for file-based session
checkpoints. DeepSeekCode already persists sessions, threads, turns, and items
under `.dscode/runtime`, and already supports Markdown/HTML export, but it
lacked a TUI-level JSON snapshot round trip for portable session restore.

## Goals

- Add `save` / `/save` command palette and composer support.
- Add optional `save <path>` / `/save <path>` path selection.
- Add `load <path>` / `/load <path>` command palette and composer support.
- Add `save help` / `/save help` and `load help` / `/load help` detail panels.
- Write a JSON snapshot containing the active durable session, active thread,
  turns, and items.
- Default save path to `session_<timestamp>.json` in the selected workspace.
- Resolve relative paths under the selected workspace; allow absolute and
  `~/...` paths.
- Import loaded snapshots into a new durable session/thread with fresh runtime
  ids, preserving turn and item content/order without overwriting existing
  history.
- Reject save/load in HTTP-runtime TUI mode because these commands read/write
  local files from the TUI process.

## Design

`TuiAction::SaveSession { session_id, thread_id, path }` and
`TuiAction::LoadSession { workspace, path }` keep UI parsing separate from
filesystem and runtime-store mutation. The local file-backed TUI handler writes
snapshots with this schema:

```json
{
  "kind": "deepseek.tui.session_snapshot.v1",
  "session": {},
  "thread": {},
  "turns": [],
  "items": []
}
```

The load path parses the snapshot, creates a new session titled
`Imported: <source session>`, creates a new active thread titled
`Imported: <source thread>`, appends saved turns/items in index order, maps old
turn ids to new turn ids for item linkage, and records a
`session_snapshot_loaded` runtime event.

This is intentionally a DeepSeekCode snapshot format rather than a byte-for-byte
copy of DeepSeek-TUI's internal `SavedSession`, because DeepSeekCode's durable
runtime model is session/thread/turn/item based.

## Acceptance

- `/save` queues a session snapshot action for the selected durable session and
  active thread.
- `/save <path>` preserves the requested path on the queued action.
- `/save help` renders path rules and current session/thread metadata.
- `/load <path>` queues a load action using the selected workspace for relative
  paths.
- `/load` without a path is rejected with a usage error.
- `/load help` explains import semantics and fresh runtime ids.
- Local file-backed TUI writes a snapshot containing session/thread metadata and
  transcript content.
- Local file-backed TUI imports that snapshot into a new durable session/thread
  and preserves item content.
- HTTP-runtime TUI rejects save/load as local-only.
- Tests cover command routing, help detail rendering, missing-path rejection,
  HTTP rejection, snapshot writing, and snapshot import.
