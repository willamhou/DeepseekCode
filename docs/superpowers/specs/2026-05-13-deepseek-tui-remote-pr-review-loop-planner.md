# DeepSeek-TUI Remote PR Review Loop Planner

## Context

The `review` tool can now inspect `github_pr_context` output and report PR-level
blockers. The next gap was orchestration: the offline planner still relied on
seeded local diff/readback flows and did not deterministically chain remote PR
context gathering into the review tool.

## Scope

- Detect remote PR review tasks such as `Review pull request #42 on owner/repo`.
- Call `github_pr_context` with `include_diff=true` before reviewing when that
  tool is available and no local PR context has already been observed.
- Call `review target=github_pr_context` over the gathered context.
- Stop after the structured `review` pass rather than falling back into
  speculative file reads.
- Add a seeded benchmark fixture that starts from `github_pr_context` and
  asserts the `review` tool surfaces PR blocker metadata.

## Implementation

- `src/model/deepseek.rs` now derives PR number/repo references from PR review
  tasks, including `owner/repo` tokens and GitHub pull-request URLs.
- The offline planner has an explicit remote PR review route:
  `github_pr_context include_diff=true` -> `review github_pr_context` -> finish.
- `.dscode/benchmarks.txt` and `.dscode/benchmarks.example.txt` include a seeded
  `github_pr_context` fixture with requested changes and a failing status check.
- Runtime docs and the DeepSeek-TUI parity plan describe the deterministic PR
  review loop.

## Verification

- `/home/willamhou/.cargo/bin/cargo test github_pr_context_request --lib`
- `/home/willamhou/.cargo/bin/cargo test remote_pr --lib`
- `/home/willamhou/.cargo/bin/cargo test review --lib`
- `/home/willamhou/.cargo/bin/cargo test parse_manifest --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `git diff --check`

## Remaining

Follow-up work added read-only PR comment planning through
`pr_review_comment_plan`. Live remote PR review fixtures, semantic child review
over real GitHub PR context, and actual posting/retry loops through guarded
GitHub mutations remain future work.
