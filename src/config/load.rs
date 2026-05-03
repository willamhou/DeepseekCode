use std::fs;
use std::path::Path;

use crate::error::AppResult;
use crate::error::app_error;
use super::types::AppConfig;

pub fn load_or_default() -> AppResult<AppConfig> {
    let mut config = AppConfig::default();
    let path = config.workspace.config_path();

    if !Path::new(&path).exists() {
        return Ok(config);
    }

    let content = fs::read_to_string(path)?;
    parse_config(&content, &mut config)?;
    Ok(config)
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
            "workspace.config_dir" => config.workspace.config_dir = unquote(value),
            "workspace.session_dir" => config.workspace.session_dir = unquote(value),
            "workspace.user_skills_dir" => {
                config.workspace.user_skills_dir = unquote(value);
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
}
