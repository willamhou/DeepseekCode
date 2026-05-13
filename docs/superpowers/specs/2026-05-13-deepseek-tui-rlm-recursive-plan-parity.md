# DeepSeek-TUI RLM Recursive Plan Parity

## Context

DeepSeekCode already had RLM chunk planning and flat map-reduce planning. The
remaining recursive-helper gap was the planning layer needed when a long input
creates more map outputs than one batch can reduce cleanly. DeepSeek-TUI-style
RLM workflows often build recursive reduce trees with helper refs instead of a
single flat reduce prompt.

## Scope

- Add a read-only `rlm_recursive_plan` tool.
- Accept the same `task`/`question` plus `file_path`/`content` long-input shape
  as `rlm_map_reduce_plan`.
- Reuse existing chunk sizing, overlap, `include_text`, `map_limit`, and `steps`
  behavior.
- Add `fan_in` to control the maximum inputs per reduce group, clamped to 2-16.
- Return stable `map:<index>` and `roundN:groupM` refs, initial map tasks,
  recursive reduce groups, omitted map-task metadata, and a final output ref.
- Do not run child agents or Python; this is a deterministic planning helper.

## Implementation

- `src/tools/rlm.rs` now exposes `RlmRecursivePlanTool`.
- Static OpenAI/Anthropic tool schemas, default registry, and MCP stdio tool
  execution/listing now include `rlm_recursive_plan`.
- Runtime docs and the DeepSeek-TUI parity plan record the recursive planning
  helper as landed.

## Verification

- `/home/willamhou/.cargo/bin/cargo test rlm_recursive --lib`
- `/home/willamhou/.cargo/bin/cargo test rlm --lib`
- `/home/willamhou/.cargo/bin/cargo test build_tool_specs_include_rlm --lib`
- `/home/willamhou/.cargo/bin/cargo test default_registry_includes_dispatch_subagent_only_below_max_depth --lib`
- `/home/willamhou/.cargo/bin/cargo test mcp_tools_call_executes_rlm_recursive_plan --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `git diff --check`

## Remaining

Live semantic review fixtures, remote PR comment/retry loops, and a full
long-lived DeepSeek-TUI Python RLM process remain out of this slice.
