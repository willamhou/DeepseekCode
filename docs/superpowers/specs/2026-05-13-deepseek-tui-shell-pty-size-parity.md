# DeepSeek-TUI Shell PTY Size Parity

Date: 2026-05-13

Status: implemented

## Gap

`tty=true` background jobs now run through the Unix `script` PTY backend, but
the PTY was created with whatever default geometry the backend provided.
Terminal-aware commands could detect a TTY, but callers could not request a
known initial terminal size.

## Spec

- Accept optional `tty_rows` and `tty_cols` on `exec_shell background=true
  tty=true` and `task_shell_start tty=true`.
- Require both dimensions together and reject dimensions unless `tty=true` is
  set.
- Validate dimensions as bounded positive integers.
- For Unix `script` PTY jobs, set the initial terminal size before running the
  requested command and expose matching `LINES` / `COLUMNS` environment
  values.
- Persist `tty_rows` and `tty_cols` in durable shell manifests.
- Render `tty_rows`, `tty_cols`, and list-level `tty_size` in start, wait,
  show, list, and detached snapshots.
- Advertise the parameters through model tool specs and MCP schemas.

## Implementation

- Added `ShellTtyOptions` and `ShellTtySize` to carry PTY enablement and
  initial geometry through the background shell manager.
- Wrapped `script -c` commands with `stty rows <rows> cols <cols>; ...` when a
  size is requested.
- Added manifest read/write support for nullable `tty_rows` and `tty_cols`.
- Added rendered output fields and durable list summaries for PTY size.
- Forwarded `task_shell_start` geometry parameters into the shared
  `exec_shell` path.
- Updated model and MCP schemas.

## Verification

- `/home/willamhou/.cargo/bin/cargo test task_shell_start_tty_size_sets_script_geometry --lib`
- `/home/willamhou/.cargo/bin/cargo test task_shell_start_tty_uses_script_pty_backend --lib`
- `/home/willamhou/.cargo/bin/cargo test exec_shell --lib`
- `/home/willamhou/.cargo/bin/cargo test build_tool_specs_include_exec_shell_background_tools --lib`
- `/home/willamhou/.cargo/bin/cargo test serve --lib`
- `/home/willamhou/.cargo/bin/cargo fmt --check`
- `/home/willamhou/.cargo/bin/cargo check`
- `git diff --check`

## Remaining Gap

This is initial geometry control, not live terminal resize. DeepSeekCode still
does not provide an attachable PTY session, resize event stream, terminal replay
protocol, or independent supervisor daemon that owns PTY lifecycle after the
starting CLI process exits.
