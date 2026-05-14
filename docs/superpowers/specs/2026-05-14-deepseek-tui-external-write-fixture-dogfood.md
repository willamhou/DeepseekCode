# DeepSeek-TUI External Write-Fixture Dogfood

Date: 2026-05-14

Status: completed

## Context

The parity plan still called out live external write-fixture validation as a
remaining evidence gap. Local benchmark fixtures already cover isolated
write/validate flows, but there was no explicit CLI route for running the same
kind of task against a disposable real repository outside this checkout.

## Implementation

- Added `deepseek dogfood external-fixture` with:
  - required `--workdir <path>` pointing at a git repository outside the current
    checkout;
  - required write-and-validate task wording;
  - `--dry-run` preflight with no model call, shell command, or ledger write;
  - isolated workdir copy for the actual run;
  - temporary auto-approval for writes, shell, and MCP only inside that isolated
    copy;
  - optional `--benchmark-gate`, `--budget`, and `--notes`.
- Dogfood reports now include an `External write fixtures` evidence counter,
  counting successful `external-write-fixture` rows categorized as
  `write_validate`.
- README, install docs, and release docs document the opt-in evidence flow.

## Verification

- Parser coverage for `dogfood external-fixture`.
- Unit coverage for external repository preflight, write/validate task
  validation, report evidence counting, and note marking.

## Residual

The harness is now present, but the public gap is not fully closed until we
record successful model-backed runs against one or more disposable external
repositories and keep that evidence in the dogfood report/release notes.
