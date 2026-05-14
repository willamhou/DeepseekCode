# DeepSeek-TUI Parity: `/skill new` Skill Creator Alias

## Context

DeepSeek-TUI advertises `/skill new` as the entry point for creating a skill.
In its command handler, `new` resolves to the bundled `skill-creator` skill.
DeepSeekCode currently treats `new` as a normal skill name, so the TUI reports
it missing.

## Goals

- Add a bundled `skill-creator` TOML skill for DeepSeekCode's local skill
  format.
- Route `skill new` and `/skill new` to the `skill-creator` detail view.
- Add command/composer completions for the alias.
- Preserve normal `/skill <name>` lookup and local skill management
  subcommands.

## Acceptance

- `skill new` in the command palette queues a skill show action for
  `skill-creator`.
- `/skill new` from the composer queues the same action before custom slash
  fallback.
- `skills/skill-creator.toml` loads through the normal repo skill registry.
- Full `tui` tests continue passing.
