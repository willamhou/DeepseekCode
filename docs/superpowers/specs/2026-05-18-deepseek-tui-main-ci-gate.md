# DeepSeek-TUI Main CI Gate

**Status:** implemented
**Comparison source:** `Hmbown/DeepSeek-TUI` refreshed at `/tmp/deepseek-tui-compare-20260514`, latest fetched `origin/main` `eeccf7d`.

## Gap

The repository had a release workflow for tags and manual dispatch, but normal
`main` pushes and pull requests did not run a public CI gate. That left recent
release-proof work dependent on local checks until the next tag workflow.

## Implementation

- Added `.github/workflows/ci.yml`.
- The CI workflow runs on `main` pushes, pull requests, and manual dispatch.
- Linux checks cover Rust formatting, library tests, debug `deepseek` build,
  secret scanning, model-demo recorder/verifier/renderer self-tests, npm
  metadata checks, npm wrapper TUI entrypoint smoke, and Homebrew formula
  validation.
- macOS x64 and Windows x64 platform checks cover `cargo check --all-targets`,
  debug binary build, and `deepseek version`, giving normal PRs the same basic
  non-Linux compile/runtime proof that previously only existed in release jobs.
- The workflow keeps expensive release-only work such as Docker packaging,
  attestation, npm publishing, and tap publishing in the release matrix.
- CI and release workflows use `actions/checkout@v5` and
  `actions/setup-node@v5`, and opt into Node 24 action runtime behavior ahead
  of GitHub's announced Node 20 action runtime removal.

## Verification

- Local equivalents:
  - `cargo fmt --check`
  - `cargo test --lib -- --test-threads=1`
  - `cargo check --all-targets`
  - `cargo build --bin deepseek`
  - `node scripts/check-secrets.js`
  - `docs/demo/record-model-backed-demo.sh --dry-run`
  - `docs/demo/record-model-backed-demo.sh --redaction-self-test`
  - `docs/demo/verify-model-backed-demo.js --self-test`
  - `docs/demo/render-model-backed-demo-svg.js --self-test`
  - `node npm/scripts/check-version-sync.js`
  - `npm --prefix npm test`
  - `DEEPSEEK_BINARY=target/debug/deepseek node npm/scripts/test-tui-entrypoint-wrapper.js`
  - `node packaging/homebrew/verify-formula.js`
  - `git diff --check`
