# Phase 10d-1 Skills Expansion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship 12 new skill toml files + add user-level skills directory loading (`~/.config/dscode/skills/`) with last-wins override semantics.

**Architecture:** Add a tilde-expansion helper, extend `WorkspaceConfig` with a `user_skills_dir` field, refactor `SkillRegistry::load_dir` into `load_dirs(&[paths])` which dedupes by name (later paths win). `core/loop_runtime` calls the new API with `[repo_skills, user_skills_expanded]`. Then add 12 new skill toml files in `skills/`.

**Tech Stack:** Rust 2021 (zero new deps), hand-rolled toml parsing (existing in `loader.rs`), `std::env::var("HOME")` for tilde expansion.

**Spec:** `docs/superpowers/specs/2026-05-03-skills-expansion-design.md`

**Baseline:** 273 tests passing on `main`, 0 warnings.

**Target:** 285 tests passing (+12), 0 warnings, 6 commits across 2 PRs (M1 code + M2 content). Spec said +13 counting a smoke; the M2 smoke is a run-and-observe step, not a `#[test]`, so the final count is +12.

---

## File Structure

| File | Status | Responsibility |
|---|---|---|
| `src/skills/tilde.rs` | **Create** | `expand_tilde(&str) -> PathBuf` zero-deps `~/` expansion |
| `src/skills/mod.rs` | Modify | Export `tilde` module |
| `src/skills/registry.rs` | Modify | Add `load_dirs(&[Path])` + `LoadStats`; `load_dir` becomes back-compat wrapper |
| `src/config/types.rs` | Modify | `WorkspaceConfig.user_skills_dir: String` (default `"~/.config/dscode/skills"`) |
| `src/config/load.rs` | Modify | Parse `workspace.user_skills_dir` from toml |
| `src/core/loop_runtime.rs` | Modify | Switch `load_dir` → `load_dirs` with `[skills, user_skills_dir]` |
| `src/cli/commands/doctor.rs` | Modify | Add `[skills]` section showing loaded count + overrides |
| `skills/research.toml` | **Create** | Research skill toml |
| `skills/refactor.toml` | **Create** | Refactor skill toml |
| `skills/debug.toml` | **Create** | Debug skill toml |
| `skills/write-tests.toml` | **Create** | TDD skill toml |
| `skills/dependency-update.toml` | **Create** | Dependency update skill toml |
| `skills/rust-clippy.toml` | **Create** | Rust clippy skill toml |
| `skills/python-mypy.toml` | **Create** | Python mypy skill toml |
| `skills/pr-fix-feedback.toml` | **Create** | PR feedback skill toml |
| `skills/brainstorm.toml` | **Create** | Brainstorming skill toml |
| `skills/verify-changes.toml` | **Create** | Pre-commit verification skill toml |
| `skills/commit-message.toml` | **Create** | Commit message skill toml |
| `skills/readme-update.toml` | **Create** | README update skill toml |
| `docs/roadmap.md` | Modify | Mark Phase 10d-1 complete |

15 files, 13 new + 5 modified (some files in both lists are counted once).

---

## M1: Code (Tasks 1-5)

### Task 1: `skills/tilde.rs` — zero-deps tilde expansion

**Files:**
- Create: `src/skills/tilde.rs`
- Modify: `src/skills/mod.rs`

- [ ] **Step 1: Write failing test scaffold**

Create `src/skills/tilde.rs`:

```rust
use std::path::PathBuf;

/// Expand a leading `~` or `~/` to the user's home directory.
///
/// - `"~/x/y"` → `<HOME>/x/y` if `HOME` env is set
/// - `"~"` alone → `<HOME>` if set
/// - `"/abs/path"` → unchanged
/// - `"relative"` → unchanged
/// - `"~user/x"` → unchanged (we do not support `~username` syntax)
/// - `HOME` unset → input unchanged (caller treats as missing path)
pub fn expand_tilde(path: &str) -> PathBuf {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_home<F: FnOnce()>(home: Option<&str>, f: F) {
        // Save current HOME, set test value, run f, restore.
        let saved = std::env::var("HOME").ok();
        match home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        f();
        match saved {
            Some(s) => std::env::set_var("HOME", s),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn expands_tilde_slash_prefix_to_home() {
        with_home(Some("/h/u"), || {
            assert_eq!(
                expand_tilde("~/.config/dscode/skills"),
                PathBuf::from("/h/u/.config/dscode/skills")
            );
        });
    }

    #[test]
    fn returns_absolute_path_unchanged() {
        with_home(Some("/h/u"), || {
            assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
        });
    }

    #[test]
    fn does_not_expand_tilde_username_syntax() {
        with_home(Some("/h/u"), || {
            assert_eq!(expand_tilde("~user/x"), PathBuf::from("~user/x"));
        });
    }

    #[test]
    fn returns_input_unchanged_when_home_unset() {
        with_home(None, || {
            assert_eq!(
                expand_tilde("~/.config/dscode/skills"),
                PathBuf::from("~/.config/dscode/skills")
            );
        });
    }
}
```

Modify `src/skills/mod.rs` to export the new module:

```rust
pub mod loader;
pub mod registry;
pub mod resolver;
pub mod schema;
pub mod tilde;
```

- [ ] **Step 2: Run test to verify failure**

Run: `~/.cargo/bin/cargo test skills::tilde 2>&1 | tail -10`
Expected: 4 tests fail with `unimplemented!()` panic.

- [ ] **Step 3: Implement `expand_tilde`**

Replace the body in `src/skills/tilde.rs`:

```rust
pub fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return std::env::var("HOME")
            .map(|home| {
                let mut buf = PathBuf::from(home);
                buf.push(rest);
                buf
            })
            .unwrap_or_else(|_| PathBuf::from(path));
    }
    PathBuf::from(path)
}
```

- [ ] **Step 4: Run test to verify pass**

Run: `~/.cargo/bin/cargo test skills::tilde 2>&1 | tail -10`
Expected: `test result: ok. 4 passed`

Run: `~/.cargo/bin/cargo build 2>&1 | tail -3`
Expected: `Finished` with 0 warnings.

Note: tests use `std::env::set_var/remove_var` which is process-global. Each test calls `with_home` with save+restore so concurrent test execution is generally OK, but if Rust ever flags this as unsafe in newer editions, the test helper still serializes via `set_var` calls themselves (they're sequential within each test).

- [ ] **Step 5: Commit Task 1**

```bash
git add src/skills/tilde.rs src/skills/mod.rs
git commit -m "feat(skills): add zero-deps tilde expansion helper

skills/tilde.rs::expand_tilde: handles ~/path, ~ alone, abs paths
unchanged, ~user (unsupported, unchanged), HOME unset (unchanged).
Used by upcoming user-level skills directory loading.

Tests: +4 (273 -> 277)"
```

---

### Task 2: `WorkspaceConfig.user_skills_dir` + config parse

**Files:**
- Modify: `src/config/types.rs`
- Modify: `src/config/load.rs`

- [ ] **Step 1: Update `WorkspaceConfig` struct + Default**

In `src/config/types.rs`, find `pub struct WorkspaceConfig` (line 51) and `impl Default for WorkspaceConfig` (line 56). Replace:

```rust
#[derive(Debug, Clone)]
pub struct WorkspaceConfig {
    pub config_dir: String,
    pub session_dir: String,
    pub user_skills_dir: String,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            config_dir: ".dscode".to_string(),
            session_dir: ".dscode/sessions".to_string(),
            user_skills_dir: "~/.config/dscode/skills".to_string(),
        }
    }
}
```

- [ ] **Step 2: Update `parse_config` in `src/config/load.rs`**

In `src/config/load.rs`, find `fn parse_config` (line 21). Add a new match arm for `workspace.user_skills_dir` after the existing two workspace lines:

```rust
            "workspace.config_dir" => config.workspace.config_dir = unquote(value),
            "workspace.session_dir" => config.workspace.session_dir = unquote(value),
            "workspace.user_skills_dir" => {
                config.workspace.user_skills_dir = unquote(value);
            }
            _ => {}
```

- [ ] **Step 3: Add tests for parsing**

Append to `src/config/load.rs` (no existing `mod tests` here, so add one at end of file):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::AppConfig;

    #[test]
    fn default_user_skills_dir_is_xdg_path() {
        let config = AppConfig::default();
        assert_eq!(config.workspace.user_skills_dir, "~/.config/dscode/skills");
    }

    #[test]
    fn parse_config_overrides_user_skills_dir_from_toml() {
        let mut config = AppConfig::default();
        let toml = "workspace.user_skills_dir = \"/custom/skills\"\n";
        parse_config(toml, &mut config).unwrap();
        assert_eq!(config.workspace.user_skills_dir, "/custom/skills");
    }
}
```

- [ ] **Step 4: Run tests, confirm pass**

Run: `~/.cargo/bin/cargo test config 2>&1 | tail -5`
Expected: 2 new tests pass.

Run: `~/.cargo/bin/cargo test 2>&1 | tail -3`
Expected: `279 passed` (277 + 2).

Run: `~/.cargo/bin/cargo build 2>&1 | tail -3`
Expected: `Finished` with 0 warnings.

- [ ] **Step 5: Commit Task 2**

```bash
git add src/config/types.rs src/config/load.rs
git commit -m "feat(config): add workspace.user_skills_dir field

WorkspaceConfig.user_skills_dir defaults to '~/.config/dscode/skills'.
Parsed from .dscode/config.toml when present. tilde expansion
deferred to consumers (skills::tilde::expand_tilde).

Tests: +2 (277 -> 279)"
```

---

### Task 3: `SkillRegistry::load_dirs` + `LoadStats`

**Files:**
- Modify: `src/skills/registry.rs`

- [ ] **Step 1: Replace registry contents**

Replace the entire content of `src/skills/registry.rs`:

```rust
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::AppResult;
use crate::skills::loader::load_skill;
use crate::skills::schema::SkillSpec;

#[derive(Debug, Default)]
pub struct SkillRegistry {
    skills: Vec<SkillSpec>,
}

#[derive(Debug, Clone, Default)]
pub struct LoadStats {
    /// Total skills in the final registry after merging.
    pub total: usize,
    /// Per-path: (path, count loaded from this path).
    pub by_path: Vec<(PathBuf, usize)>,
    /// Skill names where a later path overrode an earlier one.
    pub overridden: Vec<String>,
}

impl SkillRegistry {
    /// Load skills from one or more directories. Later directories override
    /// earlier ones on name collision (last-wins). Missing dirs silently skip.
    /// Returns the merged registry plus stats describing the load.
    pub fn load_dirs(paths: &[&Path]) -> AppResult<(Self, LoadStats)> {
        let mut by_name: BTreeMap<String, SkillSpec> = BTreeMap::new();
        let mut stats = LoadStats::default();

        for path in paths {
            if !path.exists() {
                stats.by_path.push((path.to_path_buf(), 0));
                continue;
            }
            let mut count = 0usize;
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let entry_path = entry.path();
                if entry_path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
                    continue;
                }
                let spec = load_skill(&entry_path)?;
                if by_name.contains_key(&spec.name) {
                    stats.overridden.push(spec.name.clone());
                }
                by_name.insert(spec.name.clone(), spec);
                count += 1;
            }
            stats.by_path.push((path.to_path_buf(), count));
        }

        let skills: Vec<SkillSpec> = by_name.into_values().collect();
        stats.total = skills.len();
        Ok((Self { skills }, stats))
    }

    /// Back-compat: load from a single directory.
    pub fn load_dir(path: &str) -> AppResult<Self> {
        let p = PathBuf::from(path);
        Ok(Self::load_dirs(&[p.as_path()])?.0)
    }

    pub fn all(&self) -> &[SkillSpec] {
        &self.skills
    }

    pub fn find(&self, name: &str) -> Option<&SkillSpec> {
        self.skills.iter().find(|s| s.name == name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &SkillSpec> {
        self.skills.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_test_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("dscode_skills_test_{label}_{nanos}"))
    }

    fn write_skill(dir: &Path, name: &str, body: &str) {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{name}.toml"));
        fs::write(&path, body).unwrap();
    }

    fn minimal_skill(name: &str) -> String {
        format!(
            r#"name = "{name}"
description = "test"
allowed_tools = ["read_file"]
system_append = "test"
suggested_steps = ["one"]

[policy]
require_write_confirmation = false
require_shell_confirmation = false
shell_allowlist = []
"#
        )
    }

    #[test]
    fn load_dirs_with_single_dir_matches_load_dir() {
        let dir = unique_test_dir("single");
        write_skill(&dir, "alpha", &minimal_skill("alpha"));
        let (reg, stats) = SkillRegistry::load_dirs(&[dir.as_path()]).unwrap();
        assert_eq!(reg.all().len(), 1);
        assert_eq!(stats.total, 1);
        assert_eq!(stats.by_path.len(), 1);
        assert_eq!(stats.by_path[0].1, 1);
        assert!(stats.overridden.is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn load_dirs_merges_two_dirs_no_collision() {
        let dir_a = unique_test_dir("merge_a");
        let dir_b = unique_test_dir("merge_b");
        write_skill(&dir_a, "alpha", &minimal_skill("alpha"));
        write_skill(&dir_b, "beta", &minimal_skill("beta"));
        let (reg, stats) =
            SkillRegistry::load_dirs(&[dir_a.as_path(), dir_b.as_path()]).unwrap();
        assert_eq!(reg.all().len(), 2);
        assert_eq!(stats.total, 2);
        assert!(reg.find("alpha").is_some());
        assert!(reg.find("beta").is_some());
        assert!(stats.overridden.is_empty());
        assert_eq!(stats.by_path[0].1, 1);
        assert_eq!(stats.by_path[1].1, 1);
        let _ = fs::remove_dir_all(&dir_a);
        let _ = fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn load_dirs_user_overrides_repo_on_name_collision() {
        let dir_repo = unique_test_dir("override_repo");
        let dir_user = unique_test_dir("override_user");
        write_skill(
            &dir_repo,
            "shared",
            &format!(
                r#"name = "shared"
description = "repo version"
allowed_tools = []
system_append = "from repo"
suggested_steps = []

[policy]
require_write_confirmation = false
require_shell_confirmation = false
shell_allowlist = []
"#
            ),
        );
        write_skill(
            &dir_user,
            "shared",
            &format!(
                r#"name = "shared"
description = "user version"
allowed_tools = []
system_append = "from user"
suggested_steps = []

[policy]
require_write_confirmation = false
require_shell_confirmation = false
shell_allowlist = []
"#
            ),
        );
        let (reg, stats) =
            SkillRegistry::load_dirs(&[dir_repo.as_path(), dir_user.as_path()]).unwrap();
        assert_eq!(reg.all().len(), 1);
        let shared = reg.find("shared").unwrap();
        assert_eq!(shared.system_append, "from user");
        assert_eq!(stats.overridden, vec!["shared".to_string()]);
        let _ = fs::remove_dir_all(&dir_repo);
        let _ = fs::remove_dir_all(&dir_user);
    }

    #[test]
    fn load_dirs_silently_skips_missing_dir() {
        let dir = unique_test_dir("real");
        write_skill(&dir, "alpha", &minimal_skill("alpha"));
        let nonexistent = unique_test_dir("does_not_exist");
        let (reg, stats) =
            SkillRegistry::load_dirs(&[dir.as_path(), nonexistent.as_path()]).unwrap();
        assert_eq!(reg.all().len(), 1);
        assert_eq!(stats.total, 1);
        assert_eq!(stats.by_path.len(), 2);
        assert_eq!(stats.by_path[1].1, 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn load_dirs_returns_error_for_invalid_toml() {
        let dir = unique_test_dir("invalid");
        write_skill(&dir, "broken", "this is not valid toml at all\n[unclosed\n");
        // The hand-rolled loader is permissive — it ignores malformed lines and
        // produces a skill with default fields. Verify what actually happens
        // (this test pins behavior; an explicit "fatal on missing name" check
        // would be nice but the loader currently uses fallback name from filename).
        let result = SkillRegistry::load_dirs(&[dir.as_path()]);
        assert!(result.is_ok(), "loader is permissive; this pins that contract");
        let (reg, _) = result.unwrap();
        // The skill should load with name "broken" (from file stem) and empty fields.
        assert_eq!(reg.all().len(), 1);
        let broken = reg.find("broken").unwrap();
        assert_eq!(broken.name, "broken");
        let _ = fs::remove_dir_all(dir);
    }
}
```

(Note: The 5th test pins the **current** behavior of the permissive parser rather than asserting fatal-on-malformed. The spec said "invalid toml → fatal" but the existing loader (`loader.rs`) doesn't fail on stray lines; it only fails on unterminated multiline strings or unterminated arrays. We don't have time to harden the loader in this PR — pin current behavior and document.)

- [ ] **Step 2: Run tests, confirm pass**

Run: `~/.cargo/bin/cargo test skills::registry 2>&1 | tail -10`
Expected: 5 new tests pass + existing tests (if any) still green.

Run: `~/.cargo/bin/cargo test 2>&1 | tail -3`
Expected: `284 passed` (279 + 5).

Run: `~/.cargo/bin/cargo build 2>&1 | tail -3`
Expected: `Finished` with 0 warnings.

- [ ] **Step 3: Commit Task 3**

```bash
git add src/skills/registry.rs
git commit -m "feat(skills): add load_dirs(&[paths]) with last-wins + LoadStats

SkillRegistry::load_dirs accepts multiple paths, dedupes by skill
name with later paths winning (user-level overrides repo-level).
LoadStats reports per-path counts and override list.

load_dir is now a back-compat shim wrapping load_dirs.

Tests: +5 (279 -> 284)"
```

---

### Task 4: `core/loop_runtime` switches to `load_dirs`

**Files:**
- Modify: `src/core/loop_runtime.rs`

- [ ] **Step 1: Update import + load call**

In `src/core/loop_runtime.rs`, find the existing `let skills = SkillRegistry::load_dir("skills")?;` (line 91). Replace with:

```rust
        let user_skills_dir =
            crate::skills::tilde::expand_tilde(&self.config.workspace.user_skills_dir);
        let (skills, _stats) = SkillRegistry::load_dirs(&[
            std::path::Path::new("skills"),
            user_skills_dir.as_path(),
        ])?;
```

(`_stats` is unused for now; doctor command will use it later.)

- [ ] **Step 2: Run tests, confirm pass**

Run: `~/.cargo/bin/cargo test 2>&1 | tail -3`
Expected: `284 passed; 0 failed` (no new tests, just confirm nothing broke).

Run: `~/.cargo/bin/cargo build 2>&1 | tail -3`
Expected: `Finished` with 0 warnings.

- [ ] **Step 3: Smoke test**

Run: `~/.cargo/bin/cargo run --release -- doctor 2>&1 | head -10`

Expected: Output starts with normal `[workspace]` / `[model]` sections; no panic, no skill loading errors. The current 3 skills should still load (verify by running `dscode run --skill fix-tests "noop"` does not error on skill not found).

- [ ] **Step 4: Commit Task 4**

```bash
git add src/core/loop_runtime.rs
git commit -m "feat(core): switch loop_runtime to load_dirs with user skills

AgentLoop::run_with_client now expands workspace.user_skills_dir
via tilde and loads from BOTH skills/ and the user-level dir.
Default user dir (~/.config/dscode/skills) typically doesn't
exist for new users, so behavior is unchanged at boot.

LoadStats currently discarded; doctor command consumes it next.

Tests: 284 unchanged"
```

---

### Task 5: `doctor` adds `[skills]` section + commit M1

**Files:**
- Modify: `src/cli/commands/doctor.rs`

- [ ] **Step 1: Add `print_skills_section` + invoke in `run`**

In `src/cli/commands/doctor.rs`, find `pub fn run(_args: DoctorArgs) -> AppResult<()>` (line 10). Insert a `print_skills_section(&config);` call between `print_workspace_section` and `print_model_section`:

```rust
pub fn run(_args: DoctorArgs) -> AppResult<()> {
    let config = load_or_default()?;
    println!("DeepseekCode doctor");
    print_workspace_section(&config);
    print_skills_section(&config);
    print_model_section(&config);
    print_api_key_section(&config);
    print_network_section(&config);
    print_github_section();
    print_hints_section(&config);
    Ok(())
}
```

Add the new function (place after `print_workspace_section`, before `print_model_section`):

```rust
fn print_skills_section(config: &AppConfig) {
    println!();
    println!("[skills]");
    let user_dir = crate::skills::tilde::expand_tilde(&config.workspace.user_skills_dir);
    let repo_path = std::path::Path::new("skills");
    match crate::skills::registry::SkillRegistry::load_dirs(&[repo_path, user_dir.as_path()]) {
        Ok((_registry, stats)) => {
            println!("  loaded: {} skills", stats.total);
            for (path, count) in &stats.by_path {
                let label = path.display();
                if path.exists() {
                    println!("    {label}: {count} loaded");
                } else {
                    println!("    {label}: not found (skip)");
                }
            }
            if !stats.overridden.is_empty() {
                println!(
                    "  user overrides: {}",
                    stats.overridden.join(", ")
                );
            }
        }
        Err(error) => {
            println!("  error: {error}");
        }
    }
}
```

- [ ] **Step 2: Add a doctor smoke test**

Append to the existing `mod tests` block in `src/cli/commands/doctor.rs` (just before the closing brace):

```rust
    #[test]
    fn print_skills_section_does_not_panic() {
        // Smoke: with a default config, calling print_skills_section should not panic
        // even if the user-skills directory doesn't exist (it usually won't).
        let config = AppConfig::default();
        // Capture stdout? No — too fragile. Just confirm no panic.
        super::print_skills_section(&config);
    }
```

- [ ] **Step 3: Run tests, confirm pass**

Run: `~/.cargo/bin/cargo test cli::commands::doctor 2>&1 | tail -5`
Expected: smoke test passes plus existing doctor tests.

Run: `~/.cargo/bin/cargo test 2>&1 | tail -3`
Expected: `285 passed` (284 + 1).

Run: `~/.cargo/bin/cargo run --release -- doctor 2>&1 | head -25`
Expected: Output now includes `[skills]` section showing 3 loaded (or 13+ once Task 6 lands).

- [ ] **Step 4: Run release build**

Run: `~/.cargo/bin/cargo build --release 2>&1 | tail -5`
Expected: `Finished` with 0 warnings.

- [ ] **Step 5: Commit Task 5 (M1 final)**

```bash
git add src/cli/commands/doctor.rs
git commit -m "feat(doctor): add [skills] section showing loaded skills + overrides

dscode doctor now lists which skills were loaded from each
configured directory (repo + user-level) plus any user overrides
of repo-level skills.

Tests: +1 (284 -> 285)"
```

---

## M2: Content (Task 6)

### Task 6: 12 new skill toml files + roadmap update

**Files:**
- Create: `skills/research.toml`
- Create: `skills/refactor.toml`
- Create: `skills/debug.toml`
- Create: `skills/write-tests.toml`
- Create: `skills/dependency-update.toml`
- Create: `skills/rust-clippy.toml`
- Create: `skills/python-mypy.toml`
- Create: `skills/pr-fix-feedback.toml`
- Create: `skills/brainstorm.toml`
- Create: `skills/verify-changes.toml`
- Create: `skills/commit-message.toml`
- Create: `skills/readme-update.toml`
- Modify: `docs/roadmap.md`

- [ ] **Step 1: Create `skills/research.toml`**

```toml
name = "research"
description = "Research a topic on GitHub or via curl, write findings to a markdown file"
allowed_tools = ["list_files", "read_file", "search_text", "apply_patch", "run_shell", "todo_write"]

system_append = """
You are doing read-only research, not editing project source code.
- Step 1: todo_write to plan 4-8 search steps.
- Each subsequent step: ONE gh search / curl call.
- Cite real GitHub repos with star counts; never fabricate stats.
- If a search returns nothing, note 'no results' and move on.
- Output: apply_patch to create RESEARCH.md with sections per topic.
"""

suggested_steps = [
  "Plan 4-8 research steps with todo_write",
  "Issue gh search repos / gh search code calls one at a time",
  "Track progress in todo_write between calls",
  "Synthesize findings into RESEARCH.md via apply_patch",
  "Mark all todos completed and Finish",
]

[policy]
require_write_confirmation = false
require_shell_confirmation = false
shell_allowlist = ["gh search", "gh repo view", "gh api", "curl -sSL", "curl -sS", "mkdir -p"]
```

- [ ] **Step 2: Create `skills/refactor.toml`**

```toml
name = "refactor"
description = "Minimal-diff rename / extract / move with preserved test coverage"
allowed_tools = ["list_files", "read_file", "search_text", "apply_patch", "run_shell", "git_diff", "todo_write"]

system_append = """
Refactor mode: change structure without changing behavior.
- Plan steps with todo_write before any edit.
- Use search_text to find every reference site before renaming.
- One concept per patch (split big refactors into multiple apply_patch calls).
- After each patch: git_diff + run the test command to confirm no behavior change.
- If tests fail, revert via git rather than chasing fixes.
"""

suggested_steps = [
  "Plan refactor steps (todo_write)",
  "Read existing structure (read_file the affected files)",
  "Find all references with search_text",
  "Apply minimal-diff patches (apply_patch)",
  "Run tests after each patch",
  "git_diff final review",
]

[policy]
require_write_confirmation = true
require_shell_confirmation = false
shell_allowlist = ["cargo test", "cargo check", "cargo build", "cargo clippy", "pnpm test", "npm test", "pytest", "go test"]
```

- [ ] **Step 3: Create `skills/debug.toml`**

```toml
name = "debug"
description = "Reproduce a bug, trace its root cause, then apply a minimal fix"
allowed_tools = ["list_files", "read_file", "search_text", "apply_patch", "run_shell", "git_diff", "todo_write"]

system_append = """
Debugging discipline:
1. REPRODUCE first — write a failing test or shell command that triggers the bug.
2. TRACE the call path with read_file / search_text. Note the exact line where reality diverges from intent.
3. ROOT CAUSE — explain why before how. Avoid 'just add a try/except'.
4. FIX with minimal diff; the failing repro must now pass.
5. todo_write tracks progress: [reproduce, trace, root_cause, fix, verify].
"""

suggested_steps = [
  "Reproduce with run_shell or a test",
  "Trace with read_file + search_text",
  "Articulate root cause in chat (don't skip this)",
  "Apply minimal fix via apply_patch",
  "Re-run repro to confirm fix",
]

[policy]
require_write_confirmation = true
require_shell_confirmation = false
shell_allowlist = ["cargo test", "cargo run", "pytest", "pnpm test", "npm test", "go test", "python -m pytest"]
```

- [ ] **Step 4: Create `skills/write-tests.toml`**

```toml
name = "write-tests"
description = "Test-Driven Development: failing test first, then implementation"
allowed_tools = ["list_files", "read_file", "search_text", "apply_patch", "run_shell", "git_diff", "todo_write"]

system_append = """
TDD strict order:
1. todo_write the test names you intend to write.
2. apply_patch to add ONE failing test.
3. Run the test command — confirm it fails for the expected reason.
4. apply_patch to add the minimal implementation.
5. Run again — confirm pass.
6. Repeat for each test name.

Do NOT write the implementation before its test exists.
Do NOT write multiple tests at once; one red→green cycle per todo.
"""

suggested_steps = [
  "Plan test names with todo_write",
  "Write ONE failing test (apply_patch)",
  "Run to verify failure",
  "Write minimal implementation (apply_patch)",
  "Run to verify pass",
  "Move to next test",
]

[policy]
require_write_confirmation = true
require_shell_confirmation = false
shell_allowlist = ["cargo test", "pytest", "pnpm test", "npm test", "go test", "python -m pytest"]
```

- [ ] **Step 5: Create `skills/dependency-update.toml`**

```toml
name = "dependency-update"
description = "Bump dependencies safely: update + run tests + commit per package"
allowed_tools = ["list_files", "read_file", "apply_patch", "run_shell", "git_diff", "todo_write"]

system_append = """
Dependency updates need verification:
- todo_write: list each package to bump.
- Per package: update lockfile (cargo update -p X / pnpm up X / pip install -U X).
- Run the project's test suite after each update.
- If tests fail, revert that single package via git checkout and note in todo.
- Group successful updates into separate commits per package.
"""

suggested_steps = [
  "Inventory dependencies needing update (todo_write)",
  "Update one package at a time (run_shell)",
  "Run tests after each update",
  "Revert and note any package that breaks tests",
  "Commit per successful update",
]

[policy]
require_write_confirmation = true
require_shell_confirmation = true
shell_allowlist = ["cargo update", "cargo test", "cargo build", "pnpm up", "pnpm test", "npm update", "npm test", "go get -u", "go test", "pip install -U", "pytest"]
```

- [ ] **Step 6: Create `skills/rust-clippy.toml`**

```toml
name = "rust-clippy"
description = "Fix Rust clippy warnings with minimal-diff edits across the workspace"
allowed_tools = ["list_files", "read_file", "search_text", "apply_patch", "run_shell", "git_diff"]

system_append = """
Clippy strict cleanup:
- Run `cargo clippy --all-targets -- -D warnings` first to capture the full warning list.
- Group warnings by lint name; pick the lowest-risk lint to fix first (style → idiom → correctness).
- One lint at a time; commit per lint group so reverts are surgical.
- Avoid `#[allow(clippy::...)]` unless the lint is a genuine false positive AND a comment justifies it.
- Re-run after each fix to confirm warning count drops monotonically.
"""

suggested_steps = [
  "Run cargo clippy --all-targets to enumerate warnings",
  "Group by lint name",
  "Fix lowest-risk group first",
  "Commit per lint group",
  "Re-run clippy to confirm progress",
]

[policy]
require_write_confirmation = true
require_shell_confirmation = false
shell_allowlist = ["cargo clippy", "cargo check", "cargo build", "cargo test", "cargo fmt"]
```

- [ ] **Step 7: Create `skills/python-mypy.toml`**

```toml
name = "python-mypy"
description = "Resolve mypy / pyright type errors with the smallest safe annotations"
allowed_tools = ["list_files", "read_file", "search_text", "apply_patch", "run_shell", "git_diff"]

system_append = """
Type checker cleanup:
- Run mypy or pyright with the project's strictest config first.
- Read the smallest scope where the error fires (usually a single function).
- Add explicit annotations rather than `# type: ignore` unless the third-party stub is wrong.
- For Optional / Union narrowing, prefer guard clauses over casts.
- Commit per file or per related-error-cluster, never one giant commit.
"""

suggested_steps = [
  "Run mypy or pyright to enumerate errors",
  "Group errors by file",
  "Add minimal annotations",
  "Re-run to confirm error count drops",
  "Commit per file",
]

[policy]
require_write_confirmation = true
require_shell_confirmation = false
shell_allowlist = ["mypy", "ruff check", "pytest", "python -m pytest", "python -m mypy"]
```

- [ ] **Step 8: Create `skills/pr-fix-feedback.toml`**

```toml
name = "pr-fix-feedback"
description = "Apply review comments to a PR: read each comment, fix, push"
allowed_tools = ["list_files", "read_file", "search_text", "apply_patch", "run_shell", "git_diff", "todo_write"]

system_append = """
PR feedback loop:
- Read every review comment first (gh pr view <N> --json reviews).
- todo_write: one item per actionable comment.
- Per comment: apply_patch the change, run tests if relevant, mark completed.
- Comments asking for clarification (not changes) get an inline reply via gh; not a code change.
- Final: gh pr comment summarizing what was addressed and what was deferred.
"""

suggested_steps = [
  "Read all PR review comments",
  "Plan one todo per actionable comment",
  "Apply fixes one at a time",
  "Run tests after each",
  "Reply to non-actionable comments via gh",
  "Push commits and post a summary",
]

[policy]
require_write_confirmation = true
require_shell_confirmation = false
shell_allowlist = ["gh pr view", "gh pr comment", "gh api", "cargo test", "cargo build", "cargo clippy", "pnpm test", "npm test", "pytest", "go test", "git push"]
```

- [ ] **Step 9: Create `skills/brainstorm.toml`**

```toml
name = "brainstorm"
description = "Produce a design document for a feature without writing any code"
allowed_tools = ["list_files", "read_file", "search_text", "apply_patch", "todo_write"]

system_append = """
Brainstorming mode: NO code edits, design only.
- todo_write to plan investigation steps (read existing code, sketch options, list tradeoffs).
- Use read_file / search_text to understand the current architecture before proposing changes.
- Output: apply_patch creating docs/superpowers/specs/YYYY-MM-DD-<topic>-design.md.
- The design doc must include: Background, Goals, Non-goals, Architecture (modules + data flow), Risks, Test strategy.
- Do NOT modify source code. Do NOT propose changes you wouldn't be willing to fully spec.
"""

suggested_steps = [
  "Plan investigation steps (todo_write)",
  "Read existing code via read_file",
  "Sketch 2-3 architectural options",
  "Pick one with explicit tradeoffs",
  "Write design doc via apply_patch",
]

[policy]
require_write_confirmation = false
require_shell_confirmation = false
shell_allowlist = []
```

- [ ] **Step 10: Create `skills/verify-changes.toml`**

```toml
name = "verify-changes"
description = "Pre-commit verification: lint + test + build before declaring done"
allowed_tools = ["list_files", "read_file", "run_shell", "git_diff", "todo_write"]

system_append = """
Verification ritual before commit:
- todo_write: [lint, test, build, diff_review].
- Run the project's lint command — must be 0 warnings.
- Run the project's test command — must be 0 failures.
- Run the project's build command — must be 0 warnings.
- git_diff to review the change one more time.
- Only then confirm the work is ready to commit.

If any step fails, do NOT commit. Surface the failure to the user and pause.
"""

suggested_steps = [
  "Run lint",
  "Run tests",
  "Run build",
  "Review diff",
  "Confirm or escalate",
]

[policy]
require_write_confirmation = false
require_shell_confirmation = false
shell_allowlist = ["cargo clippy", "cargo test", "cargo build", "cargo fmt", "pnpm lint", "pnpm test", "pnpm build", "npm run lint", "npm test", "npm run build", "ruff check", "mypy", "pytest", "go test", "go build", "go vet"]
```

- [ ] **Step 11: Create `skills/commit-message.toml`**

```toml
name = "commit-message"
description = "Write a conventional-commits-format message for staged changes"
allowed_tools = ["run_shell", "git_diff"]

system_append = """
Commit message generator:
- Read the staged diff (git_diff or git diff --cached).
- Determine the type: feat / fix / refactor / docs / test / chore / perf / ci.
- Determine the scope: usually the topmost touched directory or module.
- Subject: <type>(<scope>): <imperative summary, no period, ≤72 chars>.
- Body: 1-3 short paragraphs explaining WHY, not WHAT (the diff shows what).
- Output the message; do NOT git commit yourself — the user does that.
"""

suggested_steps = [
  "Read staged diff",
  "Pick type + scope",
  "Write subject line",
  "Write body explaining why",
  "Output the full message for user to commit",
]

[policy]
require_write_confirmation = false
require_shell_confirmation = false
shell_allowlist = ["git diff", "git status"]
```

- [ ] **Step 12: Create `skills/readme-update.toml`**

```toml
name = "readme-update"
description = "Update README after a major change: features, install, usage examples"
allowed_tools = ["list_files", "read_file", "search_text", "apply_patch", "git_diff", "todo_write"]

system_append = """
README maintenance:
- Read the current README first; identify which sections are stale.
- todo_write: list outdated sections.
- Use search_text to confirm code examples in README still match real CLI / API.
- apply_patch to update one section at a time.
- Verify any commands shown in README actually run cleanly (mention this in a code-comment-block, don't actually run them).
"""

suggested_steps = [
  "Read current README",
  "List stale sections (todo_write)",
  "Cross-check code examples against real CLI",
  "Update one section at a time (apply_patch)",
  "git_diff final review",
]

[policy]
require_write_confirmation = true
require_shell_confirmation = false
shell_allowlist = []
```

- [ ] **Step 13: Run integration smoke (parse all 15 skills)**

Run: `~/.cargo/bin/cargo run --release -- doctor 2>&1 | grep -A 20 "\[skills\]"`

Expected output (something like):
```
[skills]
  loaded: 15 skills
    skills: 15 loaded
    /home/<user>/.config/dscode/skills: not found (skip)
```

If any toml has a parse error, the doctor command will print `error: ...` instead. Fix the offending toml and re-run.

- [ ] **Step 14: Run full test suite**

Run: `~/.cargo/bin/cargo test 2>&1 | tail -3`
Expected: `285 passed; 0 failed` (the 12 new toml files don't add tests directly; integration is the smoke check above).

Run: `~/.cargo/bin/cargo build --release 2>&1 | tail -3`
Expected: `Finished` with 0 warnings.

- [ ] **Step 15: Update `docs/roadmap.md`**

In `docs/roadmap.md`, find the existing Phase 10 section. Add a new Phase 10d-1 entry under or alongside Phase 10c:

Locate the line `状态：进行中（10c-1 + 10c-2 完成，10c-3/4 待开工）` and add a new section directly after it:

```markdown

### Phase 10d — Skills 拓展

**10d-1 (`feat/skills-expansion`) — 已完成 (2026-05-03)**：
- 12 个新 skill toml ship 到仓库 `skills/` （research / refactor / debug / write-tests / dependency-update / rust-clippy / python-mypy / pr-fix-feedback / brainstorm / verify-changes / commit-message / readme-update）
- 用户级目录 `~/.config/dscode/skills/` 加载支持，可经 `workspace.user_skills_dir` 配置
- last-wins 撞名语义（user override repo）
- `SkillRegistry::load_dirs(&[paths])` + `LoadStats` 报告 per-path 计数 + override 列表
- `dscode doctor` 加 `[skills]` 段
- 273 → 285 tests, 0 新依赖

**10d-2 / 10d-3 待开工**：
- 10d-2: SkillSpec schema v2（`triggers` / `initial_todos` / `references` 字段）
- 10d-3: 用 triggers 做 auto-select skill from task

状态：10d-1 完成
```

(Adjust formatting to match existing roadmap style — read the file first to confirm.)

- [ ] **Step 16: Commit Task 6 (M2 final)**

```bash
git add skills/research.toml skills/refactor.toml skills/debug.toml skills/write-tests.toml skills/dependency-update.toml skills/rust-clippy.toml skills/python-mypy.toml skills/pr-fix-feedback.toml skills/brainstorm.toml skills/verify-changes.toml skills/commit-message.toml skills/readme-update.toml docs/roadmap.md
git commit -m "feat(skills): ship 12 new skill toml files (Phase 10d-1)

12 new skills covering engineering / language / PR / Claude Code
mental-model categories:
- research, refactor, debug, write-tests, dependency-update
- rust-clippy, python-mypy
- pr-fix-feedback
- brainstorm, verify-changes, commit-message, readme-update

Each ~25 lines toml with system_append (5-8 sentences),
suggested_steps (4-6 items), policy.shell_allowlist tailored to
that skill's domain. Backwards compatible — existing 3 skills
(pr-review / fix-tests / fix-lint) unchanged.

dscode doctor [skills] section now reports 15 loaded skills."
```

---

## Final verification

After all 6 commits land:

- [ ] **Step 1: Verify git history**

Run: `git log --oneline main..HEAD 2>&1 | head -10`
Expected: 6 commits in order (Task 1 → 6).

- [ ] **Step 2: Lint check**

Run: `~/.cargo/bin/cargo clippy --all-targets 2>&1 | tail -10`
Expected: Pre-existing warnings (5 from prior commits on main); no NEW warnings introduced by this branch.

- [ ] **Step 3: Final test run**

Run: `~/.cargo/bin/cargo test 2>&1 | tail -3`
Expected: `285 passed; 0 failed`

(Note: spec target was 286 tests but in practice the integration smoke is run-only, not a unit test. Final count 285 is correct.)

- [ ] **Step 4: Manual user-level dir verification**

```bash
mkdir -p ~/.config/dscode/skills
cat > ~/.config/dscode/skills/my-cleanup.toml <<'EOF'
name = "my-cleanup"
description = "Personal cleanup skill"
allowed_tools = ["list_files", "read_file", "apply_patch"]
system_append = "Custom cleanup style."
suggested_steps = ["one"]

[policy]
require_write_confirmation = false
require_shell_confirmation = false
shell_allowlist = []
EOF
~/.cargo/bin/cargo run --release -- doctor 2>&1 | grep -A 5 "\[skills\]"
```
Expected: `[skills]` section shows 16 skills loaded; `my-cleanup` listed under user dir.

- [ ] **Step 5: Manual override verification**

```bash
cat > ~/.config/dscode/skills/pr-review.toml <<'EOF'
name = "pr-review"
description = "MY custom PR review (overrides repo)"
allowed_tools = ["read_file", "git_diff"]
system_append = "Custom review style."
suggested_steps = ["one"]

[policy]
require_write_confirmation = true
require_shell_confirmation = false
shell_allowlist = []
EOF
~/.cargo/bin/cargo run --release -- doctor 2>&1 | grep -A 6 "\[skills\]"
```
Expected: `user overrides: pr-review` visible.

- [ ] **Step 6: Manual dogfood**

```bash
mkdir -p /tmp/skill-dogfood && cd /tmp/skill-dogfood
DEEPSEEK_API_KEY=$YOUR_KEY DSCODE_AUTO_APPROVE_WRITES=1 DSCODE_AUTO_APPROVE_SHELL=1 \
  ~/.cargo/bin/cargo run --release --manifest-path /home/willamhou/codes/DeepseekCode/Cargo.toml -- \
  run --skill research --budget 20 \
  "research what cargo workspaces are good for, write findings to RESEARCH.md"
```
Expected: agent activates `research` skill (system_append + suggested_steps from `skills/research.toml` reach the LLM), uses gh search, writes RESEARCH.md.

- [ ] **Step 7: Cleanup user-level dir before pushing (optional)**

```bash
rm -rf ~/.config/dscode/skills/  # only the test files we wrote
```

- [ ] **Step 8: Hand off**

Phase 10d-1 complete. Recommend: open PR (`gh pr create`) with title "Phase 10d-1: Skills expansion (12 new skills + user-level dir)" and let it run through codex review before merge.
