# DeepSeek-TUI Remote PR Semantic Review Planner

## Context

`review` already supports `semantic=true` for a read-only child-agent pass, and
remote PR review already gathers `github_pr_context include_diff=true` before
calling `review`. The missing link was planner intent: explicit semantic/deep PR
review requests still used the deterministic-only review call.

## Scope

- Detect explicit semantic/deep/thorough/behavioral/real-bug/logic-bug review
  wording in remote PR review tasks.
- Preserve the existing remote PR route:
  `github_pr_context include_diff=true` -> `review target=github_pr_context`.
- Add `semantic=true` to the `review` tool input only when the task asks for
  semantic-style review.
- Add benchmark support for checking tool input arguments, then seed a remote PR
  semantic review fixture.

## Implementation

- `src/model/deepseek.rs` adds `task_requests_semantic_review` and applies it
  when building remote PR `review` tool input.
- `src/cli/commands/benchmark.rs` adds
  `expect_tool_input_contains = "tool:key=value"` for planner argument
  regression coverage.
- `.dscode/benchmarks.txt` and `.dscode/benchmarks.example.txt` include seeded
  remote PR semantic review fixtures that assert `review:semantic=true`.
- Runtime docs and the DeepSeek-TUI parity plan describe the semantic remote PR
  route.

## Verification

- `/home/willamhou/.cargo/bin/cargo test semantic_review --lib`
- `/home/willamhou/.cargo/bin/cargo test benchmark_evaluation_accepts_named_tool_input_expectation --lib`
- `/home/willamhou/.cargo/bin/cargo test benchmark --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `git diff --check`

## Remaining

Live remote PR retry fixtures still need more real dogfood samples before the
overall parity claim can be considered fully closed.
