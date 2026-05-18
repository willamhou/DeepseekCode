# DeepSeek-TUI npm Wrapper Entrypoint Smoke

## Source

Comparison source: `Hmbown/DeepSeek-TUI` refreshed at
`/tmp/deepseek-tui-compare-20260514`, `origin/main` at `eeccf7d`.

DeepSeek-TUI's public command is exercised through its published command
surface. DeepSeekCode now has a binary-level PTY smoke for bare `deepseek`, but
the npm wrapper is also part of the intended public install path.

## Gap

The existing local npm checks verified wrapper binary resolution and `version`,
but they did not prove that the wrapper can start the full-screen TUI entrypoint
under a real PTY. The wrapper file was also not executable in the git checkout,
which weakens local and packed-bin confidence.

## Implemented Behavior

- `npm/bin/deepseek.js` is committed executable.
- `npm/scripts/test-tui-entrypoint-wrapper.js` runs the Rust binary's
  `deepseek tui --entrypoint-smoke` command while using `npm/bin/deepseek.js`
  as the smoked bare command.
- The smoke sets `DEEPSEEK_BINARY` so the wrapper resolves the selected local or
  release binary without needing published optional packages.
- The verifier parses `deepseek.tui.entrypoint_smoke.v1` JSON and requires
  `ok`, alternate-screen entry/exit, rendered TUI, and the expected wrapper
  path.
- npm docs, release docs, and README locale checks include the wrapper
  entrypoint smoke.

## Validation

```bash
node --check npm/bin/deepseek.js
node --check npm/scripts/test-tui-entrypoint-wrapper.js
DEEPSEEK_BINARY=target/debug/deepseek node npm/scripts/test-tui-entrypoint-wrapper.js
npm --prefix npm test
git diff --check
```

## Remaining

This proves the local npm wrapper entrypoint. Published optional-package proof
still depends on release credentials and registry/tap publication.
