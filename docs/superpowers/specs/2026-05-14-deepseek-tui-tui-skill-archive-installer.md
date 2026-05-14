# DeepSeek-TUI Parity: TUI Skill Archive Installer

## Context

DeepSeek-TUI installs community skills from `github:owner/repo`, bare GitHub
repository URLs, direct tarball URLs, direct `SKILL.md` URLs, and registry names
that resolve to those sources. DeepSeekCode already supports local TOML skills,
direct TOML install/update, registry browsing, and registry sync, but it still
rejects GitHub/tarball/SKILL.md bundle sources.

DeepSeekCode's runtime skill format remains a single TOML file. This slice
therefore imports compatible `SKILL.md` bundles by parsing required frontmatter
and converting the Markdown body into `system_append`.

## Goals

- Resolve `github:owner/repo` and bare `https://github.com/owner/repo` sources
  to GitHub `main.tar.gz` with `master.tar.gz` fallback.
- Support direct `.tar.gz` / `.tgz` sources that contain a safe `SKILL.md`.
- Support direct `SKILL.md` text URLs.
- Support registry entries whose `source` is GitHub, tarball, `SKILL.md`, or
  direct TOML.
- Convert imported `SKILL.md` into DeepSeekCode TOML under
  `workspace.user_skills_dir`.
- Preserve `.installed-from` source/checksum metadata so `/skill update <name>`
  can refresh archive and `SKILL.md` imports.
- Keep zip and non-HTTP archive formats explicitly unsupported.

## Acceptance

- `/skill install github:owner/repo` attempts `main.tar.gz` then
  `master.tar.gz`.
- Installing a tarball with `SKILL.md` writes `<name>.toml` and an
  `.installed-from` marker.
- Installing a direct `SKILL.md` URL writes an imported TOML skill.
- Installing a registry name works when that registry entry points at a
  supported archive, `SKILL.md`, or TOML source.
- `/skill update <name>` refreshes an installed archive or `SKILL.md` import
  and reports no change when the converted TOML checksum is unchanged.
- `/skills sync` caches supported archive and `SKILL.md` registry entries as
  TOML files.
- Malformed archives, missing `SKILL.md`, unsafe paths, missing frontmatter, and
  zip sources produce actionable detail-panel messages.
- Existing TOML install/update and local skill commands remain unchanged.
- Full `tui` tests continue passing.
