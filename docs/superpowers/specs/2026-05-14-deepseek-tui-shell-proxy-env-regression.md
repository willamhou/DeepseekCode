# DeepSeek-TUI Shell Proxy Env Regression

**Status:** implemented
**Comparison source:** `Hmbown/DeepSeek-TUI` refreshed at `/tmp/deepseek-tui-compare-20260514`; latest fetched `origin/main` `13e7957621448792beda06ec8615e33cb374adce`.

## Gap

The latest DeepSeek-TUI refresh added a child-task environment regression for
standard proxy variables such as `HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY`, and
lower-case equivalents. DeepSeekCode shell tools already inherit the parent
process environment through Rust's `Command`, but there was no regression proof
that model-launched foreground or background shell tasks preserve proxy
configuration required in corporate, WSL, and locked-down network environments.

## Implementation

- Added a test-only environment mutation lock for shell env regression tests.
- Added `run_shell` coverage for inherited upper/lower-case proxy variables.
- Added `run_shell` coverage showing explicit `env.KEY` values override parent
  proxy values.
- Added background `exec_shell` coverage for inherited upper/lower-case proxy
  variables.
- Added background `exec_shell` coverage showing explicit `env.KEY` values
  override parent proxy values.

## Verification

- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `/home/willamhou/.cargo/bin/cargo test proxy_env --lib -- --test-threads=1`
- `/home/willamhou/.cargo/bin/cargo check`
- `/home/willamhou/.cargo/bin/cargo test --lib -- --test-threads=1`
- `git diff --check`

## Remaining

This closes the shell proxy-env regression slice. It does not add a filtered
child environment allowlist because DeepSeekCode does not currently clear the
parent environment for shell tools; changing that would be a separate security
policy decision.
