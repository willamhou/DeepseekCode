# Release Checklist

This checklist keeps a release tied to the same gates that protect the agent workflow.

## Preflight

Start from the remote mainline:

```bash
git fetch origin
git switch main
git merge --ff-only origin/main
```

Confirm the version and workspace health:

```bash
deepseek version
deepseek doctor
deepseek doctor --json
```

For source builds, `deepseek version` must match the version in `Cargo.toml`.
`deepseek doctor --json` must emit valid JSON for local supervisors and release automation.

For launch-quality README media, use the model-backed demo recorder against a
disposable fixture before generating GIF/MP4 assets:

```bash
DEEPSEEK_API_KEY=... docs/demo/record-model-backed-demo.sh
```

## Required Gates

Run the full local release gate before tagging or publishing:

```bash
cargo fmt --check
cargo test -- --test-threads=1
cargo package --allow-dirty
deepseek benchmark
docs/demo/record-model-backed-demo.sh --dry-run
```

`deepseek benchmark` must pass all three layers:

- benchmark case expectations
- benchmark trend gate
- dogfood live gate

The live gate blocks release when new dogfood failures, stuck runs, or manual interventions appear after the previous benchmark snapshot.
Failed benchmark gates do not advance the saved benchmark history baseline. After triaging known live failures, use
`deepseek benchmark --accept-live-baseline` only to intentionally accept the current dogfood snapshot; do not use it for normal release checks.

## Dogfood Replay

Replay at least one standard write/validate task and one retry task:

```bash
deepseek dogfood run --from-benchmark fixture-write-validate-rust-mini --notes "release replay"
deepseek dogfood run --from-benchmark fixture-retry-write-validate-python-mini --notes "release retry replay"
deepseek dogfood report --limit 5
```

If a replay exposes a new failure, fix the root cause before publishing. Do not release by overriding the dogfood outcome.

For external write-fixture evidence, use a disposable git repository outside
this checkout. Always dry-run first; the real run copies the repository to an
isolated workdir and records an `external-write-fixture` dogfood row:

```bash
deepseek dogfood external-fixture --workdir /tmp/disposable-repo --dry-run \
  'replace `a - b` with `a + b` in src/lib.rs and validate with cargo test'
deepseek dogfood external-fixture --workdir /tmp/disposable-repo --benchmark-gate \
  'replace `a - b` with `a + b` in src/lib.rs and validate with cargo test'
deepseek dogfood report --limit 10
```

For a release-readiness evidence gate, make the report fail closed when the
ledger does not have enough live proof:

```bash
deepseek dogfood report --limit 20 \
  --require-min-runs 100 \
  --require-success-rate 90 \
  --require-recent-clean 20 \
  --require-external-write-fixtures 3 \
  --require-category write_validate:25:90 \
  --require-category recovery:25:90 \
  --require-category pr_workflow:25:90
```

## Artifact

For a local release binary:

```bash
cargo build --release
./target/release/deepseek version
./target/release/deepseek doctor
./target/release/deepseek doctor --json
./target/release/deepseek update package --bin ./target/release/deepseek
./target/release/deepseek update verify-install --bin ./target/release/deepseek
./target/release/deepseek agents service --kind all --out target/service-smoke --bin ./target/release/deepseek --workdir "$PWD"
test -f target/service-smoke/SERVICES.md
./target/release/deepseek agents rlm-status --json
cargo package --allow-dirty
(cd npm && npm run check:version)
(cd npm && npm test)
DEEPSEEK_BINARY=./target/release/deepseek node npm/bin/deepseek.js version
node npm/scripts/stage-platform-package.js --platform linux-x64 --binary ./target/release/deepseek
node npm/scripts/verify-platform-package.js --platform linux-x64
./target/release/deepseek update publish-status
```

For the runtime contract, start `./target/release/deepseek serve --http` and
capture `/health` plus `/runtime` from the release binary before publishing.
Also smoke the MCP stdio server with `initialize` and `tools/list`:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  '{"jsonrpc":"2.0","id":3,"method":"prompts/list","params":{}}' \
  '{"jsonrpc":"2.0","id":4,"method":"prompts/get","params":{"name":"review_code","arguments":{"path":"README.md"}}}' \
  '{"jsonrpc":"2.0","id":5,"method":"resources/list","params":{}}' \
  '{"jsonrpc":"2.0","id":6,"method":"resources/templates/list","params":{}}' \
  | ./target/release/deepseek serve --mcp
```

For the side-effect MCP surface, smoke the trusted direct path in a temporary
workspace:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"run_shell","arguments":{"command":"pwd"}}}' \
  | DSCODE_MCP_ENABLE_SIDE_EFFECTS=1 ./target/release/deepseek serve --mcp
```

The durable approval path for `run_shell` and `apply_patch` is covered by
`cargo test mcp`; release notes should call out
`DSCODE_MCP_ENABLE_DURABLE_APPROVALS=1` and
`DSCODE_MCP_APPROVAL_THREAD_ID=<thread-id>` when documenting MCP clients.

Smoke the ACP stdio adapter with `initialize` and `session/list`:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}}' \
  '{"jsonrpc":"2.0","id":2,"method":"session/list","params":{"limit":5}}' \
  | ./target/release/deepseek serve --acp
```

Smoke MCP self-registration in a temporary workspace so it does not touch your
user MCP config:

```bash
tmp="$(mktemp -d)"
(cd "$tmp" && "$OLDPWD/target/release/deepseek" mcp add-self --project --name release-deepseek)
(cd "$tmp" && "$OLDPWD/target/release/deepseek" mcp add release-http --project --url http://127.0.0.1:3999/mcp --disabled)
(cd "$tmp" && "$OLDPWD/target/release/deepseek" mcp get release-http)
(cd "$tmp" && "$OLDPWD/target/release/deepseek" mcp resources)
(cd "$tmp" && "$OLDPWD/target/release/deepseek" mcp resource-templates)
(cd "$tmp" && "$OLDPWD/target/release/deepseek" mcp remove release-http --project)
```

The TUI full-width MCP manager screen and scrollable right-side discovery detail panel are covered by the
focused unit filter:

```bash
cargo test mcp
```

For the Docker artifact:

```bash
docker build -t deepseek-code:<version> .
docker run --rm deepseek-code:<version> version
```

Tag releases also publish the source-built image to GHCR through the Release
Matrix workflow:

```bash
docker pull ghcr.io/<owner>/<repo>:<version>
docker run --rm ghcr.io/<owner>/<repo>:<version> version
```

For the GitHub release matrix:

```bash
gh workflow run "Release Matrix"
gh run watch
```

The workflow builds release binaries for Linux x64, macOS x64, macOS arm64,
and Windows x64. Linux runs the full serial test suite with
`cargo test -- --test-threads=1`; macOS and Windows run
`cargo check --all-targets` before the release binary/package smoke so the
platform matrix still catches compile drift without depending on Unix-specific
test fixtures.
The workflow also runs packaging checks for Cargo metadata, `cargo package`,
Cargo/npm/Homebrew version sync, the npm wrapper, root/platform npm dry
packaging, Homebrew formula syntax, Docker image build/run smoke, and runtime
service template rendering.
Each platform build also smoke-runs the binary after staging it into the
matching npm platform package, before packing the tarball that may be published
to npm.
Each platform artifact includes a sibling `.sha256` file, for example
`deepseek-macos-arm64.tar.gz.sha256`. The build job also creates GitHub signed
artifact attestations for each archive and checksum file.

Before relying on a tag workflow to publish npm or Homebrew, run the local
readiness check. Without artifact directories it reports metadata and missing
external configuration only:

```bash
deepseek update publish-status
deepseek update publish-status --json
```

After downloading release matrix assets and npm platform package artifacts, run
the strict gate:

```bash
deepseek update publish-status \
  --dist dist-assets \
  --npm-dist npm-dist \
  --strict
```

`--strict` fails when `NPM_TOKEN`/`NODE_AUTH_TOKEN`,
`HOMEBREW_TAP_REPOSITORY`, `HOMEBREW_TAP_TOKEN`, platform release archives,
non-placeholder `.sha256` files, or platform npm package tarballs are missing.
The text and JSON output also include a `public_install` audit for source
checkout, GitHub Release, npm, Homebrew, GHCR, and Cargo registry policy. Treat
`ready_to_publish` as local readiness only: do not advertise npm, Homebrew,
Docker, or release-binary install paths until the corresponding verification
command in that audit succeeds against the live public channel.

When the workflow runs from a `v*` tag, it also creates or updates the matching
GitHub Release and uploads every platform archive plus checksum file as release
assets. It also packs platform npm packages from the compiled binaries. Manual
`workflow_dispatch` runs keep assets as workflow artifacts only. Tag runs also
publish a GHCR Docker image as `ghcr.io/<owner>/<repo>:<version>`,
`ghcr.io/<owner>/<repo>:v<version>`, and `ghcr.io/<owner>/<repo>:latest` with
OCI source, revision, and version labels. Tag runs also run the Cargo registry
policy job after packaging checks and run `npm publish` after platform package
artifacts are available. Cargo registry distribution is intentionally
source-build/package-only for now: `Cargo.toml` sets `publish = false`, and the
Cargo registry workflow job exits successfully when that policy is present.
Remove that flag only after there is an explicit crates.io or private registry
ownership decision. The npm publish step is skipped when `NPM_TOKEN` is not
configured. The Homebrew tap publish step is skipped unless
`HOMEBREW_TAP_REPOSITORY` and `HOMEBREW_TAP_TOKEN` are configured; when enabled,
it renders `Formula/deepseek.rb` from the uploaded release checksums and pushes
it to the tap repository after the GitHub Release assets are published. The
npm publish step fails if the tag does not match the package version it
publishes.

Verify downloaded release artifacts with:

```bash
gh attestation verify deepseek-macos-arm64.tar.gz --repo <owner>/<repo>
gh attestation verify deepseek-macos-arm64.tar.gz.sha256 --repo <owner>/<repo>
```

For the Homebrew formula:

```bash
ruby -c packaging/homebrew/deepseek.rb
deepseek update homebrew-formula \
  --version <version> \
  --repo <owner>/<repo> \
  --dist <downloaded-release-artifact-directory> \
  --formula packaging/homebrew/deepseek.rb
ruby -c packaging/homebrew/deepseek.rb
```

Before publishing a tap, download the release matrix `.sha256` files next to
their archives and run `deepseek update homebrew-formula`. The updater reads
`deepseek-linux-x64.tar.gz.sha256`, `deepseek-macos-x64.tar.gz.sha256`, and
`deepseek-macos-arm64.tar.gz.sha256`, then rewrites the formula with matching
release URLs and checksums.

To automate tap publishing from the tag workflow, set repository variable
`HOMEBREW_TAP_REPOSITORY` to the tap repository, for example
`owner/homebrew-tap`, and set secret `HOMEBREW_TAP_TOKEN` to a token with write
access to that repository.

Release notes should include:

- version
- commit SHA
- platform
- `deepseek version` output
- `deepseek doctor --json` output
- `deepseek serve --http` `/health` and `/runtime` output
- `deepseek serve --mcp` `initialize`, `tools/list`, `prompts/list`,
  `prompts/get`, `resources/list`, and `resources/templates/list` output; plus
  trusted `DSCODE_MCP_ENABLE_SIDE_EFFECTS=1` `run_shell` smoke in a temp workspace
  and unit coverage for the durable approval path, including MCP `apply_patch`
- `deepseek serve --acp` `initialize` and `session/list` output
- `deepseek mcp add-self --project` and `mcp add/get/remove --project`
  temp-workspace smoke output
- `cargo test mcp` output covering TUI MCP manager command parsing, full-width
  manager rendering, local project/user config mutation, right-side MCP detail rendering/scrolling, and MCP
  prompt/resource/template client bridges
- `release.json` from `deepseek update package`
- `SERVICES.md`, generated service-template smoke output including
  `target/service-smoke/SERVICES.md`, and
  `deepseek agents rlm-status --json` output showing the live RLM service
  lifecycle surface
- `npm test` output from `npm/`
- root and platform npm package tarball names
- Docker image tag and `docker run ... version` output
- release matrix run URL, artifact names, `.sha256` file contents, and
  attestation verification output
- Homebrew formula SHA-256 values
- release gate result
- upgrade and rollback instructions

`deepseek` is the release artifact name. `dscode` is only a compatibility alias.

## Upgrade And Rollback

Source upgrade:

```bash
git pull
cargo install --path . --force
deepseek version
deepseek doctor
```

Binary upgrade:

```bash
deepseek update install-package --package target/deepseek-release/deepseek-<version>-<platform>
```

Replace the binary, then validate:

```bash
deepseek update verify-install --bin "$(command -v deepseek)"
```

Rollback:

```bash
deepseek update rollback
deepseek update verify-install --bin "$(command -v deepseek)"
```
