# DeepSeek-TUI v0.1.1 Release Sync

**Status:** implemented and published on 2026-05-14
**Comparison source:** `Hmbown/DeepSeek-TUI` refreshed at `/tmp/deepseek-tui-compare-20260514`, HEAD `9483248a9f35b5f2b56c34b5b84fbc5334473c9d`.

## Gap

The public `v0.1.0` release did not include the latest onboarding parity
work: CLI stdin auth persistence, masked TUI auth, and `/setup wizard`.

This slice prepared and published a synchronized `v0.1.1` release so those
capabilities are now available through the GitHub Release and GHCR paths.

## Implementation

- Bump `Cargo.toml` and `Cargo.lock` to `0.1.1`.
- Bump the npm root package, optional dependency pins, and every platform
  package to `0.1.1`.
- Bump the local Homebrew formula template URLs to `v0.1.1`.
- Make the release workflow Homebrew render smoke read the version from
  `Cargo.toml` instead of hardcoding the version in workflow YAML.

## Verification

- `cd npm && npm run check:version`
- `cd npm && npm test`
- `cargo check`
- `cargo build`
- `cargo fmt --check`
- `git diff --check`
- `target/debug/deepseek version` prints `deepseek 0.1.1`
- `gh run watch 25859387517 --repo willamhou/DeepSeekCode --exit-status`
- `gh release view v0.1.1 --repo willamhou/DeepSeekCode`
- `docker pull ghcr.io/willamhou/deepseekcode:0.1.1`
- `docker pull ghcr.io/willamhou/deepseekcode:v0.1.1`
- `docker run --rm ghcr.io/willamhou/deepseekcode:0.1.1 version` prints
  `deepseek 0.1.1`
- `npm view @deepseek-code/cli version` returns `E404`, matching the workflow
  log line `NPM_TOKEN is not configured; skipping npm publish.`

## Residual Gap

The `v0.1.1` public release is published and smoke-tested for GitHub Release and
GHCR. npm and Homebrew remain intentionally unpublished until registry/tap
credentials are configured and externally verified.
