# DeepSeek-TUI Parity: TUI Translate Command

## Context

DeepSeek-TUI exposes `/translate` as a session toggle. When enabled, it adds a
locale output requirement to the system prompt so future model replies are
written in the user's UI language, with code identifiers and technical strings
preserved. DeepSeek-TUI also has a post-hoc fallback translator for English text
that leaks through.

DeepSeekCode did not yet reserve `/translate`, so the command could fall through
to project custom slash commands.

## Goals

- Add built-in `translate` / `/translate` before custom slash fallback.
- Support DeepSeek-TUI aliases `translation` and the typo alias `transale`.
- Keep the toggle session-local and expose `on`, `off`, `toggle`, and `show`
  forms.
- Pass the enabled state into local TUI agent turns so the runtime system prompt
  includes a language-output requirement.
- Document the initially remaining semantic delta: DeepSeekCode did not yet run
  a second post-hoc translation API call over completed assistant messages.

## Acceptance

- `translate` / `/translate` toggles TUI state and renders a detail panel.
- `translation off` and `transale on` aliases are accepted.
- Future local TUI agent turns receive a system prompt containing
  `## Language Output Requirement` and the detected target language.
- Help, slash completions, settings, and TUI docs mention the command.
- Unit tests cover command toggling and prompt-instruction injection.

## Follow-Up

The post-hoc fallback delta was closed in
`2026-05-14-deepseek-tui-tui-translate-posthoc-fallback.md`.
