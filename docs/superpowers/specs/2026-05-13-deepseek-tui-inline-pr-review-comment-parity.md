# DeepSeek-TUI Inline PR Review Comment Parity

## Context

Remote PR review could gather GitHub PR context, run structured review, prepare a
top-level PR comment, post it through guarded `github_comment`, and rebuild the
plan after a denied post. The remaining functional gap was line-level review
comments on changed PR files.

## Scope

- Add an approval-gated `github_pr_review_comment` write tool.
- Support a single inline comment or a batch of comments with `path`, `line`,
  `body`, `commit_id`, optional side/range fields, evidence, and dry-run mode.
- Extend `pr_review_comment_plan` to emit a dry-run inline review-comment input
  when review findings include `path` + `line` and PR context exposes
  `headRefOid`.
- Route explicit inline/line/file/diff comment posting requests from
  `pr_review_comment_plan` into the guarded inline write tool.
- Expose the tool through MCP/ACP only when durable approvals are enabled.

## Implementation

- `src/tools/github.rs` adds `GithubPrReviewCommentTool`, validates evidence and
  inline comment payloads without `gh` in dry-run mode, and posts real comments
  through `gh api --method POST repos/{owner}/{repo}/pulls/<n>/comments`.
- `src/tools/review.rs` now infers the PR head SHA from `github_pr_context`
  JSON and emits `github_pr_review_comment_input` alongside the existing
  top-level `github_comment_input`.
- `src/model/deepseek.rs` advertises the new tool and routes explicit inline PR
  comment tasks to it before considering top-level PR comments. Failed or
  denied inline comment attempts reuse the existing PR comment failure recovery
  path by rebuilding the comment plan with the previous error recorded.
- `.dscode/benchmarks.txt` includes
  `fixture-pr-inline-comment-failure-recovery-plan`, a seeded no-write fixture
  for failed inline comment recovery.
- `src/tools/registry.rs` and `src/cli/commands/serve.rs` register the tool,
  preserve write approval prompts, and keep MCP/ACP exposure hidden unless
  durable runtime approvals are enabled.
- `docs/runtime.md` and the DeepSeek-TUI parity plan describe the inline review
  comment path.

## Verification

- `/home/willamhou/.cargo/bin/cargo test github_pr_review_comment --lib`
- `/home/willamhou/.cargo/bin/cargo test inline_pr_review --lib`
- `/home/willamhou/.cargo/bin/cargo test pr_review_comment_plan --lib`
- `/home/willamhou/.cargo/bin/cargo test benchmark --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_tools_call_executes_github_pr_review_comment_after_runtime_approval --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `git diff --check`

## Remaining

Real remote PR posting fixtures are still needed to strengthen end-to-end
confidence against GitHub permissions and API responses; those require an
external test repository and explicit write authorization.
