# DeepSeek-TUI Live Dogfood Run

**Status:** implemented
**Comparison source:** `Hmbown/DeepSeek-TUI` refreshed at
`/tmp/deepseek-tui-compare-20260514`, `origin/main`
`b8345488978265cd94990364edcdefbb21bc5f15`.

## Gap

`deepseek dogfood live-plan` made the live evidence backlog visible, but the
operator still had to copy per-category replay commands manually. That slows
down the 100+ model-backed sample collection needed before claiming a smaller
Claude/Codex/DeepSeek-TUI gap.

## Implementation

- Added `deepseek dogfood live-run`.
- The command reuses the live-plan target model and selects the next recommended
  benchmark cases across `write_validate`, `recovery`, and `pr_workflow`.
- It is safe by default:
  - dry-run unless `--execute` is present;
  - default batch limit is 3;
  - repeated `--category <name>` filters the selected categories;
  - `--execute` refuses to run unless the current model transport is `online`.
- It supports the same target-shaping flags as `live-plan`:
  - `--manifest <path>`;
  - `--target-live-runs <n>`;
  - `--target-live-success-rate <percent>`;
  - repeated `--target-category <category>:<min-runs>:<min-success-percent>`.

## Verification

- Parser coverage for `dogfood live-run`.
- Unit coverage for category filtering and total run limiting.
- Command smoke:
  - `deepseek dogfood live-run --limit 3`
  - `deepseek dogfood live-run --limit 2 --category recovery`

## Remaining

This closes the manual command-copy step, but it does not by itself satisfy the
release evidence gate. The ledger still needs enough successful online
model-backed rows for the strict `dogfood report` live thresholds to pass.
