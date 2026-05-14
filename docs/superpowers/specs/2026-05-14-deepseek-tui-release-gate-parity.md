# DeepSeek-TUI parity: release gate evidence

Status: implemented
Date: 2026-05-14

## Gap

DeepSeek-TUI has public release evidence for installable versions. On
2026-05-14, `Hmbown/DeepSeek-TUI` reported latest release `v0.8.36`, public
topics `cli`, `deepseek`, `llm`, `rust`, `terminal`, and `tui`, and a public
Cargo/npm install story. DeepSeekCode is now public with matching core topics.
At the start of this work, `willamhou/DeepSeekCode` still had no tagged GitHub
Release, GHCR image, npm package, or Homebrew tap evidence. The `v0.1.1`
release now provides public GitHub Release assets and a verified GHCR image;
npm and Homebrew remain blocked on registry/tap credentials.

Before creating a public tag, the local release gate exposed a blocker:
`cargo test` with default parallelism failed due existing process-global test
state such as current directory guards and background shell jobs. The stable
gate already used elsewhere is serial test execution.

## Implementation

- Updated `.github/workflows/release.yml` to run
  `cargo test -- --test-threads=1` in the release build matrix.
- Updated the generated release notes text in the workflow so published release
  notes name the actual serial test gate.
- Changed the npm artifact directory setup from a single-line `run:` command to
  a block scalar so the JavaScript object literal `recursive: true` is not
  parsed as YAML structure by GitHub Actions.
- Fixed the first real release workflow run failures:
  - Windows compilation no longer imports Unix-only `OpenOptionsExt`.
  - Rollback and workspace-trust path comparisons now tolerate macOS
    `/var` -> `/private/var` canonicalization.
  - Unix socket tests use shorter temporary paths to stay below platform socket
    path limits.
  - TUI export/hooks assertions inspect stored detail text instead of depending
    on viewport truncation of long macOS paths.
  - Docker image builds copy `CHANGELOG.md`, which is required by the TUI
    changelog view.
- Fixed the next release matrix failure by classifying macOS `patch` output
  that says `No file to patch` as the same missing-target-file diagnostic as
  GNU patch.
- Narrowed non-Linux release matrix gates to `cargo check --all-targets` plus
  release binary/package/npm platform smoke. Linux remains the full behavioral
  gate with `cargo test -- --test-threads=1`; macOS and Windows now verify
  compile/package viability without depending on Unix-specific test fixtures.
- Updated the macOS x64 runner from the old Intel label to `macos-15-intel`
  after the tag workflow queued indefinitely on `macos-13`.
- Updated `docs/release.md` to use the same serial test command in the local
  release gate.
- Published tag `v0.1.0`; final Release Matrix run `25855954581`
  completed successfully and generated release notes from the tagged commit.
- Published tag `v0.1.1`; Release Matrix run `25859387517` completed
  successfully and generated release notes from commit
  `4a47ada9c56dd54200fa71f5b94b930892038f5c`.

## Verification

- `cargo test` with default parallelism reproduced the release blocker:
  `1400 passed; 6 failed`.
- `cargo test -- --test-threads=1` (`1407 passed`)
- `cargo check --all-targets`
- `cargo metadata --no-deps --format-version 1`
- `cargo package --allow-dirty`
- `node npm/scripts/check-version-sync.js`
- `(cd npm && npm test)`
- `(cd npm && npm pack --dry-run)`
- `for package_dir in npm/platforms/*; do (cd "$package_dir" && npm_config_cache=/tmp/deepseek-npm-cache npm pack --dry-run); done`
- `cargo fmt --check`
- `git diff --check`
- `rg -n '^\s*run: .*: ' .github/workflows/release.yml` returned no matches
  after the workflow YAML fix.
- Focused release-run regression tests:
  - `cargo test snapshot_restore_round_trip --lib -- --test-threads=1`
  - `cargo test tools::apply_patch::tests --lib -- --test-threads=1`
  - `cargo test tools::apply_patch::tests::diagnoses_missing_file --lib -- --test-threads=1`
  - `cargo test add_remove_and_mode_are_scoped_per_workspace --lib -- --test-threads=1`
  - `cargo test exec_shell_supervisor_status_probes_read_only_protocol_methods --lib -- --test-threads=1`
  - `cargo test handle_tui_action_exports_thread_markdown --lib -- --test-threads=1`
  - `cargo test handle_tui_action_renders_hooks_inventory --lib -- --test-threads=1`
- `docker build -t deepseek-code:ci .`
- `docker run --rm deepseek-code:ci version`
- `gh run watch 25855954581 --repo willamhou/DeepSeekCode --interval 30 --exit-status`
- `gh release view v0.1.0 --repo willamhou/DeepSeekCode`
- `git ls-remote https://github.com/willamhou/DeepSeekCode.git HEAD`
- `docker pull ghcr.io/willamhou/deepseekcode:0.1.0`
- `docker run --rm ghcr.io/willamhou/deepseekcode:0.1.0 version`
- `npm view @deepseek-code/cli version` returned `E404`, matching the release
  workflow log line `NPM_TOKEN is not configured; skipping npm publish.`
- `gh release view v0.1.1 --repo willamhou/DeepSeekCode`
- `docker pull ghcr.io/willamhou/deepseekcode:0.1.1`
- `docker pull ghcr.io/willamhou/deepseekcode:v0.1.1`
- `docker run --rm ghcr.io/willamhou/deepseekcode:0.1.1 version` printed
  `deepseek 0.1.1`
- `npm view @deepseek-code/cli version` returned `E404`; the `v0.1.1` release
  workflow log again showed `NPM_TOKEN is not configured; skipping npm
  publish.`
- The `v0.1.1` `Publish Homebrew Tap` job skipped because
  `HOMEBREW_TAP_REPOSITORY` and `HOMEBREW_TAP_TOKEN` were not configured.
- Local Homebrew formula syntax smoke was not run because `ruby` is not
  installed in this workspace image; the GitHub-hosted release runner still
  performs `ruby -c packaging/homebrew/deepseek.rb`.
- The `Publish Homebrew Tap` job skipped its tap write because
  `HOMEBREW_TAP_REPOSITORY` and `HOMEBREW_TAP_TOKEN` were not configured.

## Remaining Gap

DeepSeekCode now has public source, release assets, and a runnable GHCR image.
npm and Homebrew still require registry/tap credentials before they can match
DeepSeek-TUI's public install channels. Cargo registry distribution remains
source-only by policy because `Cargo.toml` has `publish = false`.
