# PR / CI Integration

`deepseek pr` is a subcommand group for working with GitHub pull requests via the
`gh` CLI. Three actions are supported in v1.

`dscode pr` remains supported as a compatibility alias, but the primary command
spelling is `deepseek pr`.

## Prerequisites

- `gh` CLI version 2.40+ installed (`brew install gh` or see <https://cli.github.com/>)
- Authenticated: `gh auth login`
- `deepseek doctor` should show `gh auth: ok` in the `[github]` section

## Commands

### `deepseek pr review <pr>`

Run a read-only review pass over the PR diff. The agent is restricted to
`list_files`, `read_file`, `search_text`, and `git_diff` — no writes.

```
deepseek pr review 42                     # review PR #42 in the current repo
deepseek pr review owner/repo#42          # explicit owner/repo
deepseek pr review https://github.com/.../pull/42
deepseek pr review 42 --post              # also post a summary comment
deepseek pr review 42 --out review.md     # also write to a local file
```

The terminal trace contains the full review report. With `--post` and `--out`,
v1 sends a summary stub that points the reader back to the terminal trace.
Capturing the planner's complete report into the comment body is a v2 item.

### `deepseek pr fix <pr>`

Pull the failing CI job's tail log and iterate locally to fix it. The agent is
allowed to call `apply_patch`, `run_shell`, etc., subject to P3 confirm prompts
(or the `DSCODE_AUTO_APPROVE_*` env vars). Step budget is 12 (vs. the default 4)
to fit a read → patch → shell → re-read cycle.

You must be on the PR's head branch first:

```
gh pr checkout 42
deepseek pr fix 42
deepseek pr fix 42 --job test-rust        # restrict to one CI job
```

If no failed CI jobs are found, the command exits cleanly with a notice.

### `deepseek pr patch <pr>`

Apply additional changes to the PR head; default leaves changes in the worktree.

```
gh pr checkout 42
deepseek pr patch 42
deepseek pr patch 42 --commit             # also commit (clean worktree required); does NOT push
```

`--commit` will refuse to run if the worktree has uncommitted changes — commit
or stash first.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success (or "no failures" for `pr fix`) |
| 1 | Internal error (planner / `gh` returned non-zero) |
| 2 | User declined a prompt or branch / worktree precondition failed |
| 3 | `gh` not installed or not authenticated |

## Safety model

All three commands inherit the existing approval pipeline:
- Writes (`apply_patch`) and shell commands (`run_shell`) prompt before running
  unless `DSCODE_AUTO_APPROVE_WRITES=1` / `DSCODE_AUTO_APPROVE_SHELL=1` are set
- Non-TTY runs (CI / piped input) auto-deny with a clear stderr message naming
  the env-var bypass
- All user-controlled values (PR title, file paths, commands) pass through an
  ANSI-stripping sanitizer before reaching the prompt
- Branch and clean-worktree checks gate `pr fix` and `pr patch --commit` so
  changes can't accidentally land on the wrong branch

## v1 limitations

- GitHub-only (`gh` CLI). GitLab / Gitea support is not planned for v1.
- `--push` is not implemented; `--commit` stops at a local commit.
- `--max-attempts` is not implemented; rerun `deepseek pr fix` for another round.
- Inline review comments are not implemented; `--post` posts one summary comment.
- `pr review --post` body is a pointer to the terminal trace, not the full
  review markdown. Capturing planner output into the comment is a v2 item.
