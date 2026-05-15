# DeepSeek-TUI Shell Supervisor Status Truthfulness

## Source Comparison

- Upstream: `Hmbown/DeepSeek-TUI` `origin/main` at
  `b834548d3b1dd60d08f8023d64ba129945f44420`
- Local parity source: `docs/superpowers/plans/2026-05-10-deepseek-tui-parity.md`

## Gap

DeepSeekCode's shell supervisor has moved beyond the earlier protocol skeleton:
the daemon now advertises and bridges `start`, `wait`, `replay`, `attach`,
`stdin`, `resize`, and `cancel`, and `tty=true` can create native-supervisor PTY
jobs on supported Unix/Linux builds. The user-visible
`exec_shell_supervisor_status` note and runtime documentation still described
native PTY ownership, live attach, and resize as not implemented until a future
real supervisor process.

That stale wording made the product look behind its actual shell-supervisor
surface and weakened gap review accuracy.

## Scope

- Update `exec_shell_supervisor_status`, durable shell show, and durable shell
  attach notes to describe the current daemon control methods.
- State the current truth precisely:
  - supervisor `tty=true` can own native-supervisor PTY jobs on supported
    Unix/Linux builds;
  - `attach` output remains durable terminal/log replay, not full terminal
    takeover;
  - broader platform proof remains open.
- Add regression assertions that the old "not implemented until a real
  supervisor process" wording does not return.
- Update runtime docs and the parity plan audit text.

## Non-Goals

- No new PTY behavior.
- No cross-platform PTY implementation.
- No interactive terminal takeover UI.

## Acceptance

- `exec_shell_supervisor_status` lists the supported daemon methods without
  stale pre-native-PTY wording.
- The status output mentions native-supervisor PTY jobs for `tty=true` and the
  durable replay boundary.
- Durable shell `show` and `attach` output no longer describe
  native-supervisor capabilities as future-only.
- `docs/runtime.md` matches the current protocol and remaining boundary.
- Focused and full library tests pass.

## Verification

- `/home/willamhou/.cargo/bin/cargo test exec_shell_supervisor_status_reports_manifest_without_secret --lib`
- `/home/willamhou/.cargo/bin/cargo test exec_shell_supervisor_status --lib`
- `/home/willamhou/.cargo/bin/cargo test shell_supervisor --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `/home/willamhou/.cargo/bin/cargo check`
- `/home/willamhou/.cargo/bin/cargo test --lib -- --test-threads=1`
