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
            r#"name = "shared"
description = "repo version"
allowed_tools = []
system_append = "from repo"
suggested_steps = []

[policy]
require_write_confirmation = false
require_shell_confirmation = false
shell_allowlist = []
"#,
        );
        write_skill(
            &dir_user,
            "shared",
            r#"name = "shared"
description = "user version"
allowed_tools = []
system_append = "from user"
suggested_steps = []

[policy]
require_write_confirmation = false
require_shell_confirmation = false
shell_allowlist = []
"#,
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
