# DeepSeek-TUI Homebrew Formula Verifier

## Source

Comparison source: `Hmbown/DeepSeek-TUI` refreshed at
`/tmp/deepseek-tui-compare-20260514`, `origin/main` at `eeccf7d`.

DeepSeek-TUI has a stronger public install surface, including Homebrew
distribution. DeepSeekCode has a Homebrew formula template and release workflow
hooks, but local verification still depended on Ruby/Homebrew being installed.

## Gap

The formula template can drift from `Cargo.toml`, release asset names, or the
expected install/test contract. The checkout also currently carries placeholder
zero SHA-256 values by design, so the verifier must distinguish template mode
from release-publish mode.

## Implemented Behavior

- `packaging/homebrew/verify-formula.js` validates the formula without requiring
  Homebrew.
- Template mode checks:
  - class, desc, homepage, and `Cargo.toml` version alignment
  - macOS arm64, macOS x64, and Linux x64 release URLs
  - three SHA-256 entries with valid 64-character hex shape
  - install block maps the binary to `deepseek`
  - test block runs `deepseek version` and `deepseek doctor --json`
  - Ruby syntax when `ruby` is available, otherwise reports a skip
- `--release` mode rejects placeholder zero SHA-256 values after
  `deepseek update homebrew-formula` renders real release checksums.
- README, install docs, release docs, and the parity plan now include the
  verifier.
- The Release Matrix packaging and tap-publish jobs run the verifier in
  template mode and rendered-release mode.

## Validation

```bash
node --check packaging/homebrew/verify-formula.js
node packaging/homebrew/verify-formula.js
node packaging/homebrew/verify-formula.js --release  # expected failure for checked-in placeholder SHA values
target/debug/deepseek update homebrew-formula --version 0.1.1 --repo willamhou/DeepSeekCode --dist /tmp/dsc-homebrew-shas --formula packaging/homebrew/deepseek.rb --out /tmp/dsc-homebrew-formula/deepseek.rb
node packaging/homebrew/verify-formula.js --formula /tmp/dsc-homebrew-formula/deepseek.rb --release
git diff --check
```

`--release` is expected to fail against the checked-in template until real
release asset checksums replace the placeholder values.

## Remaining

Actual Homebrew tap availability still requires `HOMEBREW_TAP_REPOSITORY`,
`HOMEBREW_TAP_TOKEN`, a tag workflow run, and external tap verification.
