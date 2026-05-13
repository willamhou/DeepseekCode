# DeepSeek-TUI TUI Project Init

Status: implemented

## Gap

DeepSeek-TUI exposes `/init` to generate a project-level `AGENTS.md` file for
assistant instructions. DeepSeekCode already loaded `AGENTS.md` through the
workspace instruction chain, but the TUI did not provide an in-workbench command
to create that file.

## Implementation

- Added `core::instructions::init_project_instructions_at`, which creates
  `AGENTS.md`, detects common project types, includes starter build/test
  commands, refuses to overwrite an existing file, and ensures `.dscode/` is
  gitignored when the workspace is a git repo.
- Added `InitProjectInstructions` as a TUI action and routed `init` plus
  `/init` from the command palette and composer before custom slash fallback.
- Wired the local file-backed TUI handler to create the file in the selected
  session workspace; HTTP-runtime TUI reports the command as local-only because
  it writes to the client's workspace.
- Updated TUI documentation and the DeepSeek-TUI parity plan.

## Verification

- `cargo test init_project_instructions --lib`
- `cargo test project_instructions_init --lib`
- `cargo test initializes_project_instructions --lib`
- `cargo test project_init --lib`
- `cargo test composer_intercepts_memory_prefix_and_slash_commands --lib`
- `cargo test tui --lib`
- `cargo fmt --check`
- `cargo check`
- `git diff --check`

## Remaining

The generated starter content is intentionally conservative. More project
detectors can be added later without changing the TUI command surface.
