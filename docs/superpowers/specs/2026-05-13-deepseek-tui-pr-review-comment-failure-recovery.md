# DeepSeek-TUI PR Review Comment Failure Recovery

## Gap

Explicit PR comment posting could route into the guarded `github_comment` tool,
but a failed or denied comment attempt ended the loop without producing a useful
retry artifact. DeepSeek-TUI-style PR work should keep the operator oriented
after write failures without blindly repeating GitHub mutations.

## Scope

- Detect a last-step failed `github_comment` observation.
- Rebuild `pr_review_comment_plan` once after that failure.
- Pass the prior failure summary as `comment_error`.
- Include that error in the regenerated comment body and evidence.
- Do not immediately call `github_comment` again after the retry plan is built.

## Acceptance

1. A failed `github_comment` after an initial comment plan calls
   `pr_review_comment_plan` again.
2. The retry plan input includes the original review output, PR context, number,
   repo, and `comment_error`.
3. `pr_review_comment_plan` includes `previous_comment_error` in evidence and a
   visible note in the comment body.
4. After the retry plan is generated, the planner finishes instead of resending
   the GitHub mutation.

## Implemented

- Added a last-failed-`github_comment` branch in the offline planner.
- Added optional `comment_error` / `previous_comment_error` /
  `retry_reason` input aliases to `pr_review_comment_plan`.
- Added unit coverage for failed-post replanning and retry-plan completion.
- Added seeded benchmark fixtures for the failure-recovery route.

## Verification

- `/home/willamhou/.cargo/bin/cargo test pr_comment --lib`
- `/home/willamhou/.cargo/bin/cargo test pr_review_comment_plan --lib`

Live GitHub posting fixtures, inline review comments, and policy-specific UI
retry prompts remain future work.
