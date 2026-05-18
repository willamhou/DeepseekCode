# DeepSeek-TUI Model-Backed Demo Capture

**Status:** implemented
**Comparison source:** `Hmbown/DeepSeek-TUI` refreshed at `/tmp/deepseek-tui-compare-20260514`, HEAD `9483248a9f35b5f2b56c34b5b84fbc5334473c9d`.

## Gap

DeepSeekCode has a deterministic README TUI snapshot, but the remaining public
product gap calls out richer model-backed evidence: a real coding loop that
edits code, runs tests, and produces reviewable output. Without a repo-native
capture workflow, the README could only describe this as future manual work.

## Implementation

- Added `docs/demo/record-model-backed-demo.sh`.
- The script creates a disposable Rust crate with a failing test, records the
  initial failure, runs `deepseek exec` with write/shell approvals scoped to
  the disposable repository, then records `git diff` plus a final `cargo test`.
- Real model-backed runs require `DEEPSEEK_API_KEY`; `DEEPSEEK_DEMO_ALLOW_OFFLINE=1`
  is explicitly documented as rehearsal-only evidence.
- `DEEPSEEK_DEMO_KEY_FILE` and `--api-key-stdin` let operators provide the key
  without putting it directly in shell history or committed docs; key files must
  live outside the repository.
- `--dry-run` prints the planned fixture, transcript path, budget, and prompt
  without creating a repo or calling a model.
- `docs/demo/README.md` and all README locales now point to the recorder.

## Verification

- `bash -n docs/demo/record-model-backed-demo.sh`
- `docs/demo/record-model-backed-demo.sh --dry-run`
- `cargo fmt --check`
- `cargo check`
- `cargo test --lib -- --test-threads=1`
- `git diff --check`

## Remaining

The capture workflow is implemented, but the actual README media asset still
requires a successful online model-backed run and a reviewed GIF/MP4/SVG derived
from that transcript.
