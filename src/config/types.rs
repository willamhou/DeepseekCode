#[derive(Debug, Clone)]
pub struct AppConfig {
    pub model: ModelConfig,
    pub approval: ApprovalConfig,
    pub workspace: WorkspaceConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            model: ModelConfig::default(),
            approval: ApprovalConfig::default(),
            workspace: WorkspaceConfig::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelConfig {
    pub base_url: String,
    pub model: String,
    pub api_key_env: String,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.deepseek.com".to_string(),
            model: "deepseek-coder".to_string(),
            api_key_env: "DEEPSEEK_API_KEY".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApprovalConfig {
    pub require_write_confirmation: bool,
    pub require_shell_confirmation: bool,
}

impl Default for ApprovalConfig {
    fn default() -> Self {
        Self {
            require_write_confirmation: true,
            require_shell_confirmation: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceConfig {
    pub config_dir: String,
    pub session_dir: String,
    pub user_skills_dir: String,
    pub user_commands_dir: String,
    pub user_instructions_file: String,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            config_dir: ".dscode".to_string(),
            session_dir: ".dscode/sessions".to_string(),
            user_skills_dir: "~/.config/dscode/skills".to_string(),
            user_commands_dir: "~/.config/dscode/commands".to_string(),
            user_instructions_file: "~/.config/dscode/AGENTS.md".to_string(),
        }
    }
}
