use std::path::PathBuf;

/// Resolve the path to the repo's bundled `skills/` directory in an
/// install-portable way. Resolution order:
///
/// 1. `DSCODE_SKILLS_DIR` env var (test override / escape hatch — returned
///    as-is even if non-existent, so callers see "not found (skip)" reporting)
/// 2. `<exe-dir>/skills` if it exists (binary-adjacent; works for portable
///    tarball installs)
/// 3. `<CARGO_MANIFEST_DIR>/skills` if it exists (baked at compile time;
///    works for `cargo run` / `cargo test` / installed-via-cargo from any CWD
///    where the source tree is still present)
/// 4. `./skills` (CWD-relative fallback; preserves prior behaviour)
pub fn resolve_repo_skills_dir() -> PathBuf {
    resolve_repo_skills_dir_with(
        std::env::var("DSCODE_SKILLS_DIR").ok(),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(PathBuf::from)),
        env!("CARGO_MANIFEST_DIR"),
    )
}

fn resolve_repo_skills_dir_with(
    env_override: Option<String>,
    exe_dir: Option<PathBuf>,
    manifest_dir: &str,
) -> PathBuf {
    if let Some(p) = env_override {
        return PathBuf::from(p);
    }
    if let Some(dir) = exe_dir {
        let candidate = dir.join("skills");
        if candidate.exists() {
            return candidate;
        }
    }
    let baked = PathBuf::from(manifest_dir).join("skills");
    if baked.exists() {
        return baked;
    }
    PathBuf::from("skills")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_tmp(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("dscode_paths_test_{label}_{nanos}"))
    }

    #[test]
    fn env_override_takes_precedence_even_if_nonexistent() {
        let result = resolve_repo_skills_dir_with(
            Some("/nope/this/does/not/exist".to_string()),
            None,
            env!("CARGO_MANIFEST_DIR"),
        );
        assert_eq!(result, PathBuf::from("/nope/this/does/not/exist"));
    }

    #[test]
    fn exe_adjacent_skills_dir_used_when_exists() {
        let dir = unique_tmp("exe_adjacent");
        fs::create_dir_all(dir.join("skills")).unwrap();
        let result = resolve_repo_skills_dir_with(None, Some(dir.clone()), "/nonexistent");
        assert_eq!(result, dir.join("skills"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn falls_back_to_manifest_dir_when_exe_adjacent_missing() {
        let dir = unique_tmp("manifest");
        fs::create_dir_all(dir.join("skills")).unwrap();
        let result = resolve_repo_skills_dir_with(
            None,
            Some(PathBuf::from("/no_exe_dir_here")),
            dir.to_str().unwrap(),
        );
        assert_eq!(result, dir.join("skills"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn falls_back_to_cwd_when_all_else_missing() {
        let result = resolve_repo_skills_dir_with(
            None,
            Some(PathBuf::from("/no_exe_dir_here")),
            "/no_manifest_dir_here",
        );
        assert_eq!(result, PathBuf::from("skills"));
    }
}
