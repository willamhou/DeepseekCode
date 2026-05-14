# DeepSeek-TUI Parity: TUI Diff Command

## Context

DeepSeek-TUI exposes `/diff` as a read-only command that runs
`git diff --name-only` and `git diff --stat` in the workspace, then displays
changed tracked files and a stat summary. DeepSeekCode already has agent-level
`git_diff` and rollback diff surfaces, but the TUI command palette did not have
the direct user-facing `/diff` command.

## Goals

- Add `diff` / `/diff` command palette and composer support.
- Add `diff help` / `/diff help` explaining scope and selected workspace.
- Render changed tracked files and `git diff --stat` in the right-side detail
  panel.
- Report a clear no-change message when `git diff --name-only` is empty.
- Report git invocation failures with stderr where available.
- Reject diff in HTTP-runtime TUI mode because it inspects files from the local
  TUI process rather than the remote runtime host.

## Design

`TuiAction::ShowDiff { workspace }` carries the selected session workspace from
the UI layer into the local file-backed action handler. The handler runs:

```text
git diff --name-only
git diff --stat
```

The renderer mirrors DeepSeek-TUI's shape: a changed-file count, one path per
line, and a `Stat` section when stat output is available. The command is
read-only and intentionally excludes untracked files, matching upstream
behavior.

## Acceptance

- `/diff` queues a workspace diff action for the selected durable session.
- `/diff help` renders selected-workspace behavior in the detail panel.
- Local file-backed TUI renders changed files and stat output for a modified
  git worktree.
- Empty diffs render "No changes since session start".
- HTTP-runtime TUI rejects diff as local-only.
- Tests cover command routing, local diff rendering, and HTTP rejection.
