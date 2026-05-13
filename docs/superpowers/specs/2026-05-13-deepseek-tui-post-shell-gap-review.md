# DeepSeek-TUI Post-Shell Gap Review

Date: 2026-05-13

Status: completed

## Context

This review follows the shell durable metadata and detached-control diagnostic
slices. It re-checks the current DeepSeek-TUI parity plan for remaining gaps
that are still material after the main TUI, runtime, MCP/ACP, rollback, shell,
web, review, RLM helper, and request-user-input surfaces landed.

## Findings

1. Shell parity is now feature-complete for the common DeepSeek-TUI workflow:
   start, list, show, poll, wait, stdin, close stdin, cancel, ACP streaming, and
   detached manifest/log inspection. Later slices also add best-effort
   detached cancel by persisted pid/process group, direct durable stdout/stderr
   logs, Unix FIFO detached stdin for new background jobs, and `tty=true`
   execution through the Unix `script` PTY backend. A later slice also adds
   initial PTY geometry with `tty_rows` plus `tty_cols`, durable stdout/stderr
   log-slice replay with `exec_shell_replay`, and owner/process-group manifest
   metadata. The remaining shell gap is a dedicated PTY supervisor with live
   resize, attachable terminal replay, and robust ownership after the owner
   DeepSeekCode process has exited.
2. TUI interaction parity no longer has a first-order open item in the plan.
   The workbench has command history/completion, modal approvals, user-input
   Other answers, MCP manager keyboard/mouse/bulk flows, task multiselect, shell
   job commands, memory commands, reasoning browser/search/pins, rollback
   panels, hunk browsing, hunk apply, and live runtime refresh.
3. Rollback fidelity is strong for tracked diffs plus untracked regular files,
   empty directories, Unix directory modes, FIFOs, sockets, and symlinks. The
   remaining edge is platform-specific special-file recreation beyond that set,
   especially device nodes and Windows symlink behavior.
4. RLM parity covers model-running one-shot/batch tools, chunk/map-reduce/
   recursive planners, restricted Python helpers, stateful Python sessions,
   persistent Python REPL processes, durable `rlm_process` model-session
   context plus session-only continuation, `rlm_process_sessions` inventory,
   and MCP/ACP exposure. The remaining RLM gap is a true live model-backed RLM
   REPL/daemon rather than persisted child-agent summaries.
5. Remote PR review/comment workflows have deterministic planners, guarded
   mutation tools, failure recovery, and readiness checks. Remaining validation
   depends on external live GitHub fixtures with explicit write authorization.
6. Package/release visibility is substantially covered by docs, readiness
   gates, public repo metadata, topics, and install/update helpers, but live npm
   / Homebrew / cross-platform release publication remains external.

## Result

The remaining gap is concentrated in hard infrastructure and external-fixture
areas rather than the everyday DeepSeek-TUI terminal workflow. The objective is
not complete because dedicated shell PTY supervisor ownership, true live
model-backed RLM daemon semantics, platform-specific rollback edges, and live
external publishing/write fixtures still need either more architecture or
explicit external resources. Shell cancel has since narrowed to best-effort
detached process-group cancellation; detached stdin has since narrowed to Unix
FIFO control for new jobs; `tty=true` has since narrowed to a Unix `script` PTY
backend; initial PTY geometry has since narrowed to `tty_rows` plus `tty_cols`;
durable shell replay has since narrowed to byte-offset stdout/stderr slices;
shell ownership diagnostics now persist stable child pid, owner pid, and process
group metadata; RLM process semantics have since narrowed to durable
model-session context plus session-only continuation; live RLM daemon
manifest/inventory discovery, runtime-thread-backed turn queueing, cursor event
replay/wait, per-turn payload persistence and inventory, queued-turn
cancellation, a single-step live RLM worker bridge, and bounded FIFO live RLM
draining have landed. Live PTY resize, attachable terminal replay/supervisor
takeover, resident live RLM daemon service packaging, delta streaming, active
worker cancellation, and recovery remain open.

## Next Candidate Specs

- Shell supervisor/PTY implementation now has a design spec:
  `2026-05-13-deepseek-tui-shell-supervisor-pty-design.md`. The next executable
  slice should be either the supervisor protocol skeleton or native Unix PTY
  backend.
- True live model-backed RLM REPL/daemon implementation now has a design spec:
  `2026-05-13-deepseek-tui-rlm-live-daemon-design.md`. The next executable
  slice should be resident service-loop packaging, streaming deltas, active
  worker cancellation, or restart recovery because queueing, event replay/wait,
  payload persistence, queued cancellation, single-step execution, and bounded
  drain have landed.
- Platform restore strategy for device nodes and Windows symlink semantics.
- Live GitHub write-fixture harness behind an explicit opt-in test repository.
