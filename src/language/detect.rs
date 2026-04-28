use std::path::Path;

use crate::error::AppResult;
use crate::language::profile::LanguageProfile;

pub fn detect_profile(root: &str) -> AppResult<LanguageProfile> {
    let root = Path::new(root);

    if root.join("Cargo.toml").exists() {
        return Ok(rust_profile(root));
    }
    if root.join("package.json").exists() {
        return Ok(node_profile(root));
    }
    if root.join("pyproject.toml").exists() || root.join("requirements.txt").exists() {
        return Ok(python_profile(root));
    }
    if root.join("go.mod").exists() {
        return Ok(go_profile());
    }
    if root.join("pom.xml").exists() || root.join("build.gradle").exists()
        || root.join("build.gradle.kts").exists()
    {
        return Ok(java_profile(root));
    }
    Ok(generic_profile())
}

fn rust_profile(root: &Path) -> LanguageProfile {
    let is_workspace = match std::fs::read_to_string(root.join("Cargo.toml")) {
        Ok(content) => content.contains("[workspace]"),
        Err(_) => false,
    };
    let mut hints = vec!["Prefer minimal compile-safe changes.".to_string()];
    if is_workspace {
        hints.push(
            "Cargo workspace detected; scope edits to one crate before broadening.".to_string(),
        );
    }
    LanguageProfile {
        name: "rust".to_string(),
        file_priority: vec![
            "Cargo.toml".to_string(),
            "src/main.rs".to_string(),
            "src/lib.rs".to_string(),
            "tests/".to_string(),
        ],
        test_commands: vec!["cargo test".to_string()],
        hints,
    }
}

fn node_profile(root: &Path) -> LanguageProfile {
    let manager = detect_node_package_manager(root);
    let has_typescript = root.join("tsconfig.json").exists();
    let name = if has_typescript { "typescript" } else { "javascript" };

    let test = format!("{} test", manager.test_runner());

    let mut file_priority = vec!["package.json".to_string()];
    if has_typescript {
        file_priority.push("tsconfig.json".to_string());
    }
    file_priority.push("src/".to_string());
    file_priority.push("test/".to_string());

    let hints = vec![
        format!("Detected package manager: {}; use it consistently.", manager.label()),
        "Keep changes narrow and respect the package manager already in use.".to_string(),
    ];

    LanguageProfile {
        name: name.to_string(),
        file_priority,
        test_commands: vec![test],
        hints,
    }
}

fn python_profile(root: &Path) -> LanguageProfile {
    let manager = detect_python_package_manager(root);
    let test = manager.test_command();

    let mut hints = vec![
        "Prefer minimal runtime-safe changes and rerun only relevant tests.".to_string(),
        "Preserve indentation style (tabs vs spaces) of the file you edit.".to_string(),
    ];
    hints.push(format!("Detected package manager: {}.", manager.label()));

    LanguageProfile {
        name: "python".to_string(),
        file_priority: vec![
            "pyproject.toml".to_string(),
            "requirements.txt".to_string(),
            "src/".to_string(),
            "tests/".to_string(),
        ],
        test_commands: vec![test],
        hints,
    }
}

fn go_profile() -> LanguageProfile {
    LanguageProfile {
        name: "go".to_string(),
        file_priority: vec![
            "go.mod".to_string(),
            "cmd/".to_string(),
            "pkg/".to_string(),
            "internal/".to_string(),
        ],
        test_commands: vec!["go test ./...".to_string()],
        hints: vec![
            "Preserve package boundaries and prefer direct fixes over abstractions.".to_string(),
            "Run `go test -race ./...` when concurrency is involved.".to_string(),
        ],
    }
}

fn java_profile(root: &Path) -> LanguageProfile {
    let uses_maven = root.join("pom.xml").exists();
    let test = if uses_maven { "mvn test" } else { "gradle test" };
    let manager = if uses_maven { "Maven" } else { "Gradle" };

    LanguageProfile {
        name: "java".to_string(),
        file_priority: vec![
            "pom.xml".to_string(),
            "build.gradle".to_string(),
            "build.gradle.kts".to_string(),
            "src/main/".to_string(),
            "src/test/".to_string(),
        ],
        test_commands: vec![test.to_string()],
        hints: vec![
            format!("Detected build tool: {manager}; respect its layout."),
            "Minimize package-level churn and avoid renaming public APIs.".to_string(),
        ],
    }
}

fn generic_profile() -> LanguageProfile {
    LanguageProfile {
        name: "generic".to_string(),
        file_priority: vec![
            "README.md".to_string(),
            "docs/".to_string(),
            "src/".to_string(),
        ],
        test_commands: Vec::new(),
        hints: vec!["Start with repository structure and the smallest relevant files.".to_string()],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodePackageManager {
    Pnpm,
    Yarn,
    Npm,
}

impl NodePackageManager {
    fn test_runner(self) -> &'static str {
        match self {
            Self::Pnpm => "pnpm",
            Self::Yarn => "yarn",
            Self::Npm => "npm",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Pnpm => "pnpm (pnpm-lock.yaml)",
            Self::Yarn => "yarn (yarn.lock)",
            Self::Npm => "npm (package-lock.json or no lockfile)",
        }
    }
}

fn detect_node_package_manager(root: &Path) -> NodePackageManager {
    if root.join("pnpm-lock.yaml").exists() {
        NodePackageManager::Pnpm
    } else if root.join("yarn.lock").exists() {
        NodePackageManager::Yarn
    } else {
        NodePackageManager::Npm
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PythonPackageManager {
    Uv,
    Poetry,
    Pip,
}

impl PythonPackageManager {
    fn test_command(self) -> String {
        match self {
            Self::Uv => "uv run pytest".to_string(),
            Self::Poetry => "poetry run pytest".to_string(),
            Self::Pip => "pytest".to_string(),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Uv => "uv (uv.lock)",
            Self::Poetry => "poetry (poetry.lock)",
            Self::Pip => "pip (no managed lockfile)",
        }
    }
}

fn detect_python_package_manager(root: &Path) -> PythonPackageManager {
    if root.join("uv.lock").exists() {
        PythonPackageManager::Uv
    } else if root.join("poetry.lock").exists() {
        PythonPackageManager::Poetry
    } else {
        PythonPackageManager::Pip
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("dscode_detect_{label}_{nanos}"))
    }

    #[test]
    fn detects_typescript_with_pnpm_lockfile() {
        let dir = unique_dir("ts_pnpm");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("package.json"), "{}").unwrap();
        fs::write(dir.join("tsconfig.json"), "{}").unwrap();
        fs::write(dir.join("pnpm-lock.yaml"), "").unwrap();

        let profile = detect_profile(dir.to_str().unwrap()).unwrap();
        assert_eq!(profile.name, "typescript");
        assert_eq!(profile.test_commands, vec!["pnpm test"]);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn detects_javascript_when_no_tsconfig() {
        let dir = unique_dir("js_npm");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("package.json"), "{}").unwrap();
        fs::write(dir.join("package-lock.json"), "").unwrap();

        let profile = detect_profile(dir.to_str().unwrap()).unwrap();
        assert_eq!(profile.name, "javascript");
        assert_eq!(profile.test_commands, vec!["npm test"]);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn detects_yarn_when_yarn_lockfile_present() {
        let dir = unique_dir("ts_yarn");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("package.json"), "{}").unwrap();
        fs::write(dir.join("yarn.lock"), "").unwrap();

        let profile = detect_profile(dir.to_str().unwrap()).unwrap();
        assert_eq!(profile.test_commands, vec!["yarn test"]);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn detects_python_uv_with_uv_lockfile() {
        let dir = unique_dir("py_uv");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("pyproject.toml"), "").unwrap();
        fs::write(dir.join("uv.lock"), "").unwrap();

        let profile = detect_profile(dir.to_str().unwrap()).unwrap();
        assert_eq!(profile.name, "python");
        assert_eq!(profile.test_commands, vec!["uv run pytest"]);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn detects_python_poetry_with_poetry_lockfile() {
        let dir = unique_dir("py_poetry");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("pyproject.toml"), "").unwrap();
        fs::write(dir.join("poetry.lock"), "").unwrap();

        let profile = detect_profile(dir.to_str().unwrap()).unwrap();
        assert_eq!(profile.test_commands, vec!["poetry run pytest"]);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn detects_python_pip_when_no_managed_lockfile() {
        let dir = unique_dir("py_pip");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("requirements.txt"), "").unwrap();

        let profile = detect_profile(dir.to_str().unwrap()).unwrap();
        assert_eq!(profile.test_commands, vec!["pytest"]);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rust_workspace_hint_added_when_workspace_table_present() {
        let dir = unique_dir("rust_ws");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crate-a\"]",
        )
        .unwrap();

        let profile = detect_profile(dir.to_str().unwrap()).unwrap();
        assert_eq!(profile.name, "rust");
        assert!(profile
            .hints
            .iter()
            .any(|hint| hint.contains("workspace detected")));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn java_profile_uses_maven_command_when_pom_xml_present() {
        let dir = unique_dir("java_mvn");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("pom.xml"), "").unwrap();

        let profile = detect_profile(dir.to_str().unwrap()).unwrap();
        assert_eq!(profile.test_commands, vec!["mvn test"]);
        assert!(profile.hints.iter().any(|hint| hint.contains("Maven")));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn java_profile_uses_gradle_command_when_only_build_gradle_present() {
        let dir = unique_dir("java_gradle");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("build.gradle"), "").unwrap();

        let profile = detect_profile(dir.to_str().unwrap()).unwrap();
        assert_eq!(profile.test_commands, vec!["gradle test"]);
        assert!(profile.hints.iter().any(|hint| hint.contains("Gradle")));

        let _ = fs::remove_dir_all(dir);
    }
}
