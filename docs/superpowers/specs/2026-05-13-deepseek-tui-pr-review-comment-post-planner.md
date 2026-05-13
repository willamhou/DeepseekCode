# DeepSeek-TUI PR Review Comment Post Planner

## Gap

`pr_review_comment_plan` created a safe Markdown/evidence handoff, but the
offline planner still stopped there even when the operator explicitly asked to
post the PR comment. DeepSeek-TUI-style PR loops should be able to continue into
the guarded GitHub mutation path without bypassing approval.

## Scope

- Detect explicit posting language: `post`, `publish`, `leave a comment`,
  `add a comment`, `submit comment`, or `send comment`.
- Do not treat `draft`, `prepare`, or `plan only` as posting requests.
- After a successful `pr_review_comment_plan`, derive `github_comment` input
  from the plan's `github_comment_input`.
- Set `dry_run=false` only for explicit post requests.
- Keep actual GitHub mutation behind the existing `github_comment` write tool
  and approval policy.

## Acceptance

1. Draft/prepare PR comment tasks still finish after `pr_review_comment_plan`.
2. Explicit post PR comment tasks call `github_comment` after the plan exists.
3. The derived input preserves target, PR number, repo, body, and evidence.
4. The derived input sets `dry_run=false`.
5. Runtime docs and the parity plan describe the guarded handoff.

## Implemented

- Added a planner branch from successful `pr_review_comment_plan` to
  `github_comment` when explicit posting language is present.
- Added a parser for `github_comment_input` from the comment-plan JSON.
- Added unit coverage for both explicit post and draft-only behavior.

## Verification

- `/home/willamhou/.cargo/bin/cargo test remote_pr --lib`

Follow-up work added failure recovery by rebuilding the comment plan with the
previous error recorded in evidence. Live GitHub posting fixtures and inline
review-comment posting remain follow-up work.
