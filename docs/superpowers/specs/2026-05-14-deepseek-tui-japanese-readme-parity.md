# DeepSeek-TUI Japanese README Parity

**Status:** implemented on 2026-05-14
**Comparison source:** `Hmbown/DeepSeek-TUI` refreshed at `/tmp/deepseek-tui-compare-20260514`, HEAD `9483248a9f35b5f2b56c34b5b84fbc5334473c9d`.

## Gap

DeepSeek-TUI exposes a localized public README surface that includes English,
Simplified Chinese, and Japanese. DeepSeekCode only linked English and
Simplified Chinese, leaving the public landing page weaker for non-Chinese and
non-English users even though the repository had already been made public.

This is a public-surface parity issue, not a full runtime localization feature.
It improves the first-contact install and status story while preserving the
larger backlog item for TUI onboarding/runtime localization.

## Implementation

- Add `README.ja-JP.md` with localized status, feature surface, install paths,
  release archive verification, GHCR usage, remaining gaps, demo asset notes,
  development checks, documentation links, and repository safety notes.
- Update the language switcher in `README.md` and `README.zh-CN.md` to link
  English, Simplified Chinese, and Japanese consistently.
- Refresh the DeepSeek-TUI parity plan comparison HEAD to
  `9483248a9f35b5f2b56c34b5b84fbc5334473c9d` and record localized README
  surface as part of the current public comparison.

## Verification

- `rg -n "README\\.ja-JP|日本語" README.md README.zh-CN.md README.ja-JP.md`
- `git diff --check`

## Residual Gap

The README now has language parity for the public landing surface, but the
runtime/TUI still does not provide a full locale system, language onboarding,
or translated in-app strings. Those remain separate product polish gaps.
