# DeepSeek-TUI Parity: TUI Translate Post-Hoc Fallback

## Context

The first DeepSeekCode `/translate` slice added command parity and prompt-level
locale-output requirements for future local TUI agent turns. DeepSeek-TUI also
has a second layer: if assistant output still appears to be predominantly
English, it calls a focused translator and displays the localized result.

## Goals

- Add a deterministic English-leak heuristic for completed local TUI assistant
  messages when `/translate` is enabled and the target language is not English.
- Reuse the configured model endpoint for a focused translation request that
  receives only the final assistant message and the target language.
- Preserve code blocks, URLs, paths, commands, API names, and identifiers in the
  translator system prompt.
- Keep the original final message if translation is unavailable or fails, and
  record a failed `posthoc_translate` tool result instead of failing the agent
  turn.
- Add unit coverage for the heuristic, no-key failure path, and translation
  response parsing.

## Acceptance

- English-heavy final assistant text triggers post-hoc translation when
  `/translate` is enabled.
- Chinese/CJK-heavy text and English target language do not trigger fallback
  translation.
- Missing API key keeps the original message and records a failed
  `posthoc_translate` tool event.
- OpenAI-compatible and Anthropic-style translation responses parse to trimmed
  translated text.
