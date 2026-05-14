use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::error::{app_error, AppResult};
use crate::skills::tilde::expand_tilde;
use crate::util::json::{
    json_as_array, json_as_object, json_as_string, json_value_to_string, parse_root_object,
    JsonValue,
};

const TRUST_FILE_ENV: &str = "DSCODE_WORKSPACE_TRUST_FILE";
const DEFAULT_TRUST_FILE: &str = "~/.config/dscode/workspace-trust.json";

#[derive(Debug, Default, Clone)]
struct TrustFile {
    workspaces: BTreeMap<String, Vec<String>>,
    trust_modes: BTreeMap<String, bool>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WorkspaceTrust {
    trust_mode: bool,
    paths: Vec<PathBuf>,
}

impl WorkspaceTrust {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn load_for(workspace: &Path) -> Self {
        match trust_file_path() {
            Some(path) => Self::load_from_file(workspace, &path),
            None => Self::empty(),
        }
    }

    #[must_use]
    pub(crate) fn load_from_file(workspace: &Path, file_path: &Path) -> Self {
        let key = workspace_key(workspace);
        let file = read_trust_file_at(file_path).unwrap_or_default();
        let trust_mode = file.trust_modes.get(&key).copied().unwrap_or(false);
        let paths = file
            .workspaces
            .get(&key)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(PathBuf::from)
            .collect();
        Self { trust_mode, paths }
    }

    #[must_use]
    pub fn trust_mode(&self) -> bool {
        self.trust_mode
    }

    #[must_use]
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    #[must_use]
    pub fn permits(&self, candidate: &Path) -> bool {
        if self.trust_mode {
            return true;
        }
        let normalized = resolve_candidate_path(candidate);
        self.paths
            .iter()
            .any(|trusted| normalized.starts_with(trusted))
    }
}

pub fn add(workspace: &Path, path: &Path) -> AppResult<PathBuf> {
    let trust_path = trust_file_path()
        .ok_or_else(|| app_error("home directory not available for workspace trust file"))?;
    add_at(workspace, path, &trust_path)
}

pub(crate) fn add_at(workspace: &Path, path: &Path, trust_path: &Path) -> AppResult<PathBuf> {
    let canonical = canonicalize_or_keep(path);
    let key = workspace_key(workspace);
    let mut file = read_trust_file_at(trust_path).unwrap_or_default();
    let stored = canonical.to_string_lossy().to_string();
    let entry = file.workspaces.entry(key).or_default();
    if !entry.iter().any(|value| value == &stored) {
        entry.push(stored);
        entry.sort();
        entry.dedup();
    }
    write_trust_file_at(&file, trust_path)?;
    Ok(canonical)
}

pub fn remove(workspace: &Path, path: &Path) -> AppResult<bool> {
    let Some(trust_path) = trust_file_path() else {
        return Ok(false);
    };
    remove_at(workspace, path, &trust_path)
}

pub(crate) fn remove_at(workspace: &Path, path: &Path, trust_path: &Path) -> AppResult<bool> {
    let canonical = canonicalize_or_keep(path);
    let key = workspace_key(workspace);
    let mut file = read_trust_file_at(trust_path).unwrap_or_default();
    let stored = canonical.to_string_lossy().to_string();
    let changed = match file.workspaces.get_mut(&key) {
        Some(entry) => {
            let len_before = entry.len();
            entry.retain(|value| value != &stored);
            let changed = entry.len() != len_before;
            if entry.is_empty() {
                file.workspaces.remove(&key);
            }
            changed
        }
        None => false,
    };
    if changed {
        write_trust_file_at(&file, trust_path)?;
    }
    Ok(changed)
}

pub fn set_trust_mode(workspace: &Path, enabled: bool) -> AppResult<bool> {
    let trust_path = trust_file_path()
        .ok_or_else(|| app_error("home directory not available for workspace trust file"))?;
    set_trust_mode_at(workspace, enabled, &trust_path)
}

pub(crate) fn set_trust_mode_at(
    workspace: &Path,
    enabled: bool,
    trust_path: &Path,
) -> AppResult<bool> {
    let key = workspace_key(workspace);
    let mut file = read_trust_file_at(trust_path).unwrap_or_default();
    let previous = file.trust_modes.get(&key).copied().unwrap_or(false);
    if enabled {
        file.trust_modes.insert(key, true);
    } else {
        file.trust_modes.remove(&key);
    }
    let changed = previous != enabled;
    if changed {
        write_trust_file_at(&file, trust_path)?;
    }
    Ok(changed)
}

pub fn resolve_trust_command_path(workspace: &Path, raw: &str) -> PathBuf {
    let expanded = expand_tilde(raw.trim());
    if expanded.is_absolute() {
        expanded
    } else {
        workspace.join(expanded)
    }
}

pub fn resolve_workspace_path(
    workspace: &Path,
    raw_path: &str,
    tool_name: &str,
) -> AppResult<PathBuf> {
    let trust = WorkspaceTrust::load_for(workspace);
    resolve_workspace_path_with_trust(workspace, raw_path, tool_name, &trust)
}

fn resolve_workspace_path_with_trust(
    workspace: &Path,
    raw_path: &str,
    tool_name: &str,
    trust: &WorkspaceTrust,
) -> AppResult<PathBuf> {
    let raw_path = raw_path.trim();
    if raw_path.is_empty() {
        return Err(unsafe_path_error(tool_name, raw_path));
    }
    let raw = Path::new(raw_path);
    let candidate = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        workspace.join(raw)
    };
    if trust.trust_mode() {
        return Ok(resolve_candidate_path(&candidate));
    }

    let workspace_root = normalize_existing_or_lexical(workspace);
    let target = resolve_candidate_path(&candidate);
    if target.starts_with(&workspace_root) || trust.permits(&target) {
        return Ok(target);
    }
    Err(unsafe_path_error(tool_name, raw_path))
}

fn unsafe_path_error(tool_name: &str, raw_path: &str) -> Box<dyn std::error::Error> {
    app_error(format!(
        "unsafe {tool_name} path outside workspace: {raw_path}"
    ))
}

fn resolve_candidate_path(candidate: &Path) -> PathBuf {
    if candidate.exists() {
        return normalize_existing_or_lexical(candidate);
    }

    let mut existing_ancestor = candidate.to_path_buf();
    let mut suffix = Vec::new();
    while !existing_ancestor.exists() {
        if let Some(name) = existing_ancestor.file_name() {
            suffix.push(name.to_owned());
        }
        let Some(parent) = existing_ancestor.parent() else {
            break;
        };
        if parent.as_os_str().is_empty() || parent == existing_ancestor {
            break;
        }
        existing_ancestor = parent.to_path_buf();
    }

    let mut resolved = normalize_existing_or_lexical(&existing_ancestor);
    for part in suffix.into_iter().rev() {
        resolved.push(part);
    }
    normalize_lexical(&resolved)
}

fn trust_file_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var(TRUST_FILE_ENV) {
        let path = path.trim();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    let path = expand_tilde(DEFAULT_TRUST_FILE);
    (!path.starts_with("~")).then_some(path)
}

fn workspace_key(workspace: &Path) -> String {
    canonicalize_or_keep(workspace)
        .to_string_lossy()
        .into_owned()
}

fn canonicalize_or_keep(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn normalize_existing_or_lexical(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| normalize_lexical(path))
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn read_trust_file_at(path: &Path) -> AppResult<TrustFile> {
    if !path.exists() {
        return Ok(TrustFile::default());
    }
    let raw = fs::read_to_string(path)?;
    let root = parse_root_object(&raw)?;
    let mut file = TrustFile::default();

    if let Some(workspaces) = root.get("workspaces").and_then(json_as_object) {
        for (workspace, value) in workspaces {
            if let Some(paths) = string_array(value) {
                file.workspaces.insert(workspace.clone(), paths);
            } else if let Some(object) = json_as_object(value) {
                if let Some(paths) = object.get("paths").and_then(string_array) {
                    file.workspaces.insert(workspace.clone(), paths);
                }
                if let Some(JsonValue::Bool(enabled)) = object.get("trust_mode") {
                    if *enabled {
                        file.trust_modes.insert(workspace.clone(), true);
                    }
                }
            }
        }
    }
    if let Some(trust_modes) = root.get("trust_modes").and_then(json_as_object) {
        for (workspace, value) in trust_modes {
            if let JsonValue::Bool(true) = value {
                file.trust_modes.insert(workspace.clone(), true);
            }
        }
    }
    Ok(file)
}

fn string_array(value: &JsonValue) -> Option<Vec<String>> {
    Some(
        json_as_array(value)?
            .iter()
            .filter_map(json_as_string)
            .map(str::to_string)
            .collect(),
    )
}

fn write_trust_file_at(file: &TrustFile, path: &Path) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut workspaces = BTreeMap::new();
    for (workspace, paths) in &file.workspaces {
        workspaces.insert(
            workspace.clone(),
            JsonValue::Array(paths.iter().cloned().map(JsonValue::String).collect()),
        );
    }
    let mut trust_modes = BTreeMap::new();
    for (workspace, enabled) in &file.trust_modes {
        if *enabled {
            trust_modes.insert(workspace.clone(), JsonValue::Bool(true));
        }
    }
    let mut root = BTreeMap::new();
    root.insert("workspaces".to_string(), JsonValue::Object(workspaces));
    root.insert("trust_modes".to_string(), JsonValue::Object(trust_modes));
    let mut rendered = json_value_to_string(&JsonValue::Object(root));
    rendered.push('\n');
    fs::write(path, rendered)?;
    Ok(())
}

pub fn render_trust_file_hint() -> String {
    trust_file_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| DEFAULT_TRUST_FILE.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir()
            .canonicalize()
            .unwrap_or_else(|_| std::env::temp_dir());
        base.join(format!(
            "deepseek-workspace-trust-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn add_remove_and_mode_are_scoped_per_workspace() {
        let root = temp_root("scoped");
        let ws_a = root.join("a");
        let ws_b = root.join("b");
        let external = root.join("shared");
        fs::create_dir_all(&ws_a).unwrap();
        fs::create_dir_all(&ws_b).unwrap();
        fs::create_dir_all(&external).unwrap();
        let trust_path = root.join("trust.json");

        add_at(&ws_a, &external, &trust_path).unwrap();
        set_trust_mode_at(&ws_b, true, &trust_path).unwrap();

        let trust_a = WorkspaceTrust::load_from_file(&ws_a, &trust_path);
        assert!(!trust_a.trust_mode());
        assert!(trust_a.permits(&external.join("note.txt")));

        let trust_b = WorkspaceTrust::load_from_file(&ws_b, &trust_path);
        assert!(trust_b.trust_mode());
        assert!(trust_b.permits(&root.join("anywhere.txt")));

        assert!(remove_at(&ws_a, &external, &trust_path).unwrap());
        let trust_a = WorkspaceTrust::load_from_file(&ws_a, &trust_path);
        assert!(!trust_a.permits(&external.join("note.txt")));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolve_workspace_path_allows_trusted_external_path() {
        let root = temp_root("resolve");
        let workspace = root.join("workspace");
        let external = root.join("external");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&external).unwrap();
        let trust_path = root.join("trust.json");
        add_at(&workspace, &external, &trust_path).unwrap();

        let trust = WorkspaceTrust::load_from_file(&workspace, &trust_path);
        let resolved = resolve_workspace_path_with_trust(
            &workspace,
            &external.join("note.txt").display().to_string(),
            "write_file",
            &trust,
        )
        .unwrap();
        assert!(resolved.starts_with(external.canonicalize().unwrap()));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolve_workspace_path_rejects_untrusted_external_path() {
        let root = temp_root("reject");
        let workspace = root.join("workspace");
        let external = root.join("external");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&external).unwrap();

        let error = resolve_workspace_path(
            &workspace,
            &external.join("note.txt").display().to_string(),
            "write_file",
        )
        .unwrap_err();
        assert!(error.to_string().contains("unsafe write_file path"));

        let _ = fs::remove_dir_all(root);
    }
}
