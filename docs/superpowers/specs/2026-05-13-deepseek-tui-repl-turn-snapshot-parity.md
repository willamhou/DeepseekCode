# DeepSeek-TUI REPL Turn Snapshot Parity

Date: 2026-05-13

## Gap

DeepSeek-TUI-style terminal workflows make risky live turns reversible. DeepSeekCode
already had rollback snapshots for `deepseek exec`, TUI-started agent turns, and
manual REPL `/restore snapshot`, but the parity plan still tracked live REPL turn
snapshots as an open gap.

## Scope

- Verify that non-slash REPL prompts create a pre-turn rollback snapshot when
  the current directory is a git worktree.
- Keep REPL snapshots session-local: `last` resolves to the latest REPL snapshot
  id, while durable runtime turn-id binding remains for exec/TUI/ACP flows.
- Document the distinction so the remaining gap list does not overstate REPL
  rollback parity.

## Acceptance

- A REPL turn snapshot captures the current git worktree diff before model/tool
  execution.
- The snapshot label starts with `REPL turn before:` and uses a compact prompt
  summary.
- REPL snapshots are not bound to runtime thread/turn ids.
- Runtime docs and the DeepSeek-TUI parity plan describe the behavior.

## Implementation

- Confirmed `src/repl/repl.rs` calls `create_turn_snapshot` before dispatching
  a non-slash prompt and records `last_rollback_snapshot_id`.
- Added a focused unit test that initializes a git worktree, changes a tracked
  file, creates the REPL turn snapshot, and verifies patch bytes, label, and
  lack of runtime binding.
- Updated runtime docs and the DeepSeek-TUI parity plan to move REPL automatic
  snapshots from remaining gap to landed behavior.

## Verification

- `cargo test create_turn_snapshot_captures_repl_worktree_state --lib` passed.
- `cargo test repl_turn_snapshot_label_compacts_prompt --lib` passed.
- `cargo fmt --check` passed.
- `git diff --check` passed.
- `cargo test -- --test-threads=1` passed: 1098 tests.

## Remaining Differences

- Plain REPL transcripts are still not durable runtime threads, so REPL
  snapshots are addressable by snapshot id or session-local `last`, not by a
  runtime turn id.
