use std::fs;
use std::path::Path;

use super::types::AppConfig;
use crate::error::app_error;
use crate::error::AppResult;

pub fn load_or_default() -> AppResult<AppConfig> {
    load_dotenv_if_present()?;
    let mut config = AppConfig::default();
    let path = config.workspace.config_path();

    if Path::new(&path).exists() {
        let content = fs::read_to_string(path)?;
        parse_config(&content, &mut config)?;
    }

    apply_env_overrides(&mut config);
    Ok(config)
}

fn load_dotenv_if_present() -> AppResult<()> {
    let path = Path::new(".env");
    if !path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(path)?;
    for raw_line in content.lines() {
        let Some((key, value)) = parse_dotenv_assignment(raw_line) else {
            continue;
        };
        if std::env::var_os(&key).is_none() {
            std::env::set_var(key, value);
        }
    }
    Ok(())
}

fn parse_dotenv_assignment(raw_line: &str) -> Option<(String, String)> {
    let line = raw_line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let line = line.strip_prefix("export ").unwrap_or(line).trim();
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    if key.is_empty()
        || !key
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        || key.chars().next().is_some_and(|ch| ch.is_ascii_digit())
    {
        return None;
    }
    Some((key.to_string(), unquote(value.trim())))
}

fn apply_env_overrides(config: &mut AppConfig) {
    if let Ok(base_url) = std::env::var("DEEPSEEK_BASE_URL") {
        if !base_url.trim().is_empty() {
            config.model.base_url = base_url;
        }
    }
    if let Ok(model) = std::env::var("DEEPSEEK_MODEL") {
        if !model.trim().is_empty() {
            config.model.model = model;
        }
    }
    if let Ok(api_key_env) = std::env::var("DEEPSEEK_API_KEY_ENV") {
        if !api_key_env.trim().is_empty() {
            config.model.api_key_env = api_key_env;
        }
    }
}

fn parse_config(content: &str, config: &mut AppConfig) -> AppResult<()> {
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        match key {
            "model.base_url" => config.model.base_url = unquote(value),
            "model.model" => config.model.model = unquote(value),
            "model.api_key_env" => config.model.api_key_env = unquote(value),
            "approval.require_write_confirmation" => {
                config.approval.require_write_confirmation = parse_bool(value)?
            }
            "approval.require_shell_confirmation" => {
                config.approval.require_shell_confirmation = parse_bool(value)?
            }
            "approval.require_mcp_confirmation" => {
                config.approval.require_mcp_confirmation = parse_bool(value)?
            }
            "hooks.enabled" => {
                config.hooks.enabled = parse_bool(value)?;
            }
            "hooks.project_dir" => {
                config.hooks.project_dir = unquote(value);
            }
            "hooks.user_dir" => {
                config.hooks.user_dir = unquote(value);
            }
            "hooks.timeout_ms" => {
                config.hooks.timeout_ms = parse_u64(value)?;
            }
            "mcp.enabled" => {
                config.mcp.enabled = parse_bool(value)?;
            }
            "mcp.project_file" => {
                config.mcp.project_file = unquote(value);
            }
            "mcp.user_file" => {
                config.mcp.user_file = unquote(value);
            }
            "workspace.config_dir" => config.workspace.config_dir = unquote(value),
            "workspace.session_dir" => config.workspace.session_dir = unquote(value),
            "workspace.user_skills_dir" => {
                config.workspace.user_skills_dir = unquote(value);
            }
            "workspace.user_commands_dir" => {
                config.workspace.user_commands_dir = unquote(value);
            }
            "workspace.user_instructions_file" => {
                config.workspace.user_instructions_file = unquote(value);
            }
            _ => {}
        }
    }

    Ok(())
}

fn parse_bool(value: &str) -> AppResult<bool> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(app_error(format!("invalid boolean value: {value}"))),
    }
}

fn parse_u64(value: &str) -> AppResult<u64> {
    value
        .trim_matches('"')
        .parse::<u64>()
        .map_err(|_| app_error(format!("invalid integer value: {value}")))
}

fn unquote(value: &str) -> String {
    value.trim().trim_matches('"').to_string()
}

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

    #[test]
    fn parse_config_overrides_user_commands_dir_from_toml() {
        let mut config = AppConfig::default();
        let toml = "workspace.user_commands_dir = \"/custom/commands\"\n";
        parse_config(toml, &mut config).unwrap();
        assert_eq!(config.workspace.user_commands_dir, "/custom/commands");
    }

    #[test]
    fn parse_config_overrides_user_instructions_file_from_toml() {
        let mut config = AppConfig::default();
        let toml = "workspace.user_instructions_file = \"/custom/AGENTS.md\"\n";
        parse_config(toml, &mut config).unwrap();
        assert_eq!(config.workspace.user_instructions_file, "/custom/AGENTS.md");
    }

    #[test]
    fn parse_config_overrides_approval_from_toml() {
        let mut config = AppConfig::default();
        let toml = r#"
approval.require_write_confirmation = false
approval.require_shell_confirmation = false
approval.require_mcp_confirmation = false
"#;
        parse_config(toml, &mut config).unwrap();

        assert!(!config.approval.require_write_confirmation);
        assert!(!config.approval.require_shell_confirmation);
        assert!(!config.approval.require_mcp_confirmation);
    }

    #[test]
    fn parse_config_overrides_hooks_from_toml() {
        let mut config = AppConfig::default();
        let toml = r#"
hooks.enabled = true
hooks.project_dir = ".dscode/custom-hooks"
hooks.user_dir = "/custom/user-hooks"
hooks.timeout_ms = 1234
"#;
        parse_config(toml, &mut config).unwrap();

        assert!(config.hooks.enabled);
        assert_eq!(config.hooks.project_dir, ".dscode/custom-hooks");
        assert_eq!(config.hooks.user_dir, "/custom/user-hooks");
        assert_eq!(config.hooks.timeout_ms, 1234);
    }

    #[test]
    fn parse_config_overrides_mcp_from_toml() {
        let mut config = AppConfig::default();
        let toml = r#"
mcp.enabled = false
mcp.project_file = ".dscode/custom-mcp.json"
mcp.user_file = "/custom/user-mcp.json"
"#;
        parse_config(toml, &mut config).unwrap();

        assert!(!config.mcp.enabled);
        assert_eq!(config.mcp.project_file, ".dscode/custom-mcp.json");
        assert_eq!(config.mcp.user_file, "/custom/user-mcp.json");
    }

    #[test]
    fn parse_dotenv_assignment_accepts_simple_values_and_quotes() {
        assert_eq!(
            parse_dotenv_assignment("DEEPSEEK_MODEL=deepseek-v3.2"),
            Some(("DEEPSEEK_MODEL".to_string(), "deepseek-v3.2".to_string()))
        );
        assert_eq!(
            parse_dotenv_assignment("DEEPSEEK_BASE_URL=\"https://example.test/v1\""),
            Some((
                "DEEPSEEK_BASE_URL".to_string(),
                "https://example.test/v1".to_string()
            ))
        );
    }

    #[test]
    fn parse_dotenv_assignment_accepts_export_prefix() {
        assert_eq!(
            parse_dotenv_assignment("export DEEPSEEK_API_KEY=secret"),
            Some(("DEEPSEEK_API_KEY".to_string(), "secret".to_string()))
        );
    }

    #[test]
    fn parse_dotenv_assignment_rejects_comments_and_bad_keys() {
        assert_eq!(parse_dotenv_assignment("# DEEPSEEK_API_KEY=x"), None);
        assert_eq!(parse_dotenv_assignment("1BAD=x"), None);
        assert_eq!(parse_dotenv_assignment("BAD-NAME=x"), None);
    }
}
