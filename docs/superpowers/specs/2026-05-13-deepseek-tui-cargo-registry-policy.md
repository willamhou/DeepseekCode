# DeepSeek-TUI Cargo Registry Policy

## Context

Phase H still listed "Actual Cargo publish or explicit private registry release
decision" as an open packaging item. The repository already has release
artifacts, npm packaging, Docker, Homebrew templates, and `cargo package`
verification, but publishing a binary-oriented CLI crate to crates.io needs an
ownership/name decision that should not be guessed by CI.

## Scope

- Make the Cargo registry stance explicit in tracked source.
- Keep `cargo package` verification available for source-build and release
  artifact validation.
- Prevent tag workflows from attempting `cargo publish` while that policy is in
  force.
- Document how to reverse the policy once a crates.io or private registry owner
  is selected.

## Implementation

- `Cargo.toml` sets `publish = false`.
- `.github/workflows/release.yml` renames the Cargo publish job to a registry
  policy job and exits successfully when `publish = false` is present.
- `docs/release.md` and `docs/install.md` describe Cargo as
  source-build/package-only until a crates.io or private registry decision is
  made.
- The DeepSeek-TUI parity plan removes the Cargo registry decision from Phase H
  remaining work.

## Verification

- `/home/willamhou/.cargo/bin/cargo package --allow-dirty`
- `/home/willamhou/.cargo/bin/cargo check`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `git diff --check`

## Remaining

Actual npm publication and Homebrew tap publication still require external
registry/tap credentials and real release assets.
