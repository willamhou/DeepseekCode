# DeepSeek-TUI Default Entrypoint Parity

## Source

Comparison source: `Hmbown/DeepSeek-TUI` refreshed at
`/tmp/deepseek-tui-compare-20260514`, `origin/main` at `eeccf7d`.

DeepSeek-TUI's public `deepseek` binary is a dispatcher. When it receives no
subcommand, it delegates directly to the companion TUI runtime.

## Gap

DeepSeekCode exposed the full-screen workbench only through `deepseek tui`,
while bare `deepseek` still opened the older line-oriented REPL. That left the
default user experience short of DeepSeek-TUI and made the public README claim
less direct than the actual code-agent terminal surface.

## Target Behavior

- In an interactive terminal, bare `deepseek` starts the full-screen
  coding-agent workbench.
- `deepseek chat`, `deepseek repl`, and `deepseek interactive` remain explicit
  aliases for the line-oriented REPL.
- Non-full-screen contexts keep the previous REPL path, which already fails
  closed with a clear "requires a TTY" message and points users to `deepseek
  run` for one-shot tasks.
- `deepseek tui` remains an explicit workbench command for scripts, docs, and
  demo captures.

## Validation

- CLI parser tests cover no-arg TTY routing to TUI.
- CLI parser tests cover no-arg non-full-screen contexts staying on the REPL
  path.
- Help and README/docs copy distinguish the new default entrypoint from
  explicit `deepseek chat` and `deepseek tui`.
