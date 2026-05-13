# DeepSeek-TUI Review PR Context Signals

## Context

DeepSeekCode already has a DeepSeek-TUI-compatible `review` tool that accepts
workspace files, local git diffs, and `github_pr_context` output. The remote PR
path could inspect the patch, but it did not surface PR-level blockers from the
GitHub metadata that `github_pr_context` already includes.

## Scope

- Parse the `json:` block inside `github_pr_context` output.
- Report requested changes from `reviewDecision=CHANGES_REQUESTED`.
- Report required review from `reviewDecision=REVIEW_REQUIRED`.
- Report failing, errored, timed out, cancelled, or action-required status
  checks from `statusCheckRollup`.
- Warn when PR context is reviewed without `include_diff=true`.
- Keep fetching GitHub data out of `review`; callers still gather context with
  `github_pr_context`.

## Implementation

- `src/tools/review.rs` now parses the GitHub PR context JSON block with the
  existing JSON parser.
- GitHub PR review/status signals are added to the deterministic issue list for
  both `github_pr_context` and `github_pr_diff` sources.
- `review` schema descriptions and runtime docs now call out PR review/status
  signals.
- The DeepSeek-TUI parity plan records this as a remote PR review-loop slice and
  keeps live semantic review/comment retry loops as remaining work.

## Verification

- `/home/willamhou/.cargo/bin/cargo test review_github_pr_context --lib`
- `/home/willamhou/.cargo/bin/cargo test review_accepts_github_pr_context_with_diff --lib`
- `/home/willamhou/.cargo/bin/cargo test review --lib`
- `/home/willamhou/.cargo/bin/cargo test build_tool_specs_include_review --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `git diff --check`

## Remaining

Live remote PR review fixtures remain out of this slice. Later slices added
semantic child review routing over real PR context plus automatic comment,
inline-comment, and retry planning loops.
