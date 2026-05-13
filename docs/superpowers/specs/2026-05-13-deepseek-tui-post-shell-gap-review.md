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
   logs, and Unix FIFO detached stdin for new background jobs. The remaining
   shell gap is true PTY takeover and robust supervisor ownership after the
   owner DeepSeekCode process has exited.
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
   context plus `rlm_process_sessions` inventory, and MCP/ACP exposure. The
   remaining RLM gap is a true live model-backed RLM REPL/daemon rather than
   persisted child-agent summaries.
5. Remote PR review/comment workflows have deterministic planners, guarded
   mutation tools, failure recovery, and readiness checks. Remaining validation
   depends on external live GitHub fixtures with explicit write authorization.
6. Package/release visibility is substantially covered by docs, readiness
   gates, public repo metadata, topics, and install/update helpers, but live npm
   / Homebrew / cross-platform release publication remains external.

## Result

The remaining gap is concentrated in hard infrastructure and external-fixture
areas rather than the everyday DeepSeek-TUI terminal workflow. The objective is
not complete because true shell PTY/supervisor ownership, true live model-backed
RLM daemon semantics, platform-specific rollback edges, and live external
publishing/write fixtures still need either more architecture or explicit
external resources. Shell cancel has since narrowed to best-effort detached
process-group cancellation; detached stdin has since narrowed to Unix FIFO
control for new jobs; RLM process semantics have since narrowed to durable
model-session context. PTY takeover and a true live model daemon remain open.

## Next Candidate Specs

- Shell supervisor/PTY design for true PTY sessions and stronger cross-process
  ownership guarantees.
- True live model-backed RLM REPL/daemon design, likely backed by durable
  runtime threads rather than persisted child-agent summaries.
- Platform restore strategy for device nodes and Windows symlink semantics.
- Live GitHub write-fixture harness behind an explicit opt-in test repository.
