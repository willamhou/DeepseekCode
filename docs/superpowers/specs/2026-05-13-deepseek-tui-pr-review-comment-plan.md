# DeepSeek-TUI PR Review Comment Plan

## Gap

Remote PR review now chains `github_pr_context include_diff=true` into
`review`, but the result stopped at structured JSON. DeepSeek-TUI-style PR work
needs a safer bridge from review findings to an evidence-backed comment without
posting to GitHub by default.

## Scope

- Add a read-only `pr_review_comment_plan` tool.
- Accept `review_output` JSON from the `review` tool plus optional
  `github_pr_context` text.
- Render Markdown comment body text with findings ordered by severity.
- Return evidence JSON suitable for `github_comment`.
- When PR number/repo can be inferred, return a dry-run `github_comment` input.
- Route remote PR review tasks that ask to draft or prepare a comment through
  `github_pr_context` -> `review` -> `pr_review_comment_plan`.
- Expose the helper through agent schemas, MCP, and ACP as read-only.

## Acceptance

1. `pr_review_comment_plan review_output=<json>` returns `comment_body`,
   `evidence`, and `ready_to_comment`.
2. Supplied `github_pr_context` can infer PR number and repository URL.
3. The suggested `github_comment_input` always uses `dry_run=true`.
4. The offline planner drafts a comment plan only after a successful structured
   `review`.
5. MCP and ACP tool lists advertise `pr_review_comment_plan`.
6. Runtime docs, the parity plan, and benchmark fixtures describe the flow.

## Implemented

- Added `PrReviewCommentPlanTool` in `src/tools/review.rs`.
- Registered `pr_review_comment_plan` in the default agent registry.
- Added OpenAI/Anthropic tool schema entries.
- Added MCP execution/listing and ACP read-only kind mapping.
- Extended the offline planner with a remote PR review comment-plan branch.
- Added seeded benchmark fixtures for review-to-comment planning.

## Verification

- `/home/willamhou/.cargo/bin/cargo test pr_review_comment_plan --lib`
- `/home/willamhou/.cargo/bin/cargo test review --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_tools_list_includes_workspace_and_runtime_tools --lib`
- `/home/willamhou/.cargo/bin/cargo test acp_session_tools_list_new_session_is_read_only --lib`

Actual posting through `github_comment`, inline review comments, and retry loops
remain guarded GitHub-mutation follow-up work.
