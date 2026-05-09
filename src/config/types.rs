#[derive(Debug, Clone)]
pub struct AppConfig {
    pub model: ModelConfig,
    pub approval: ApprovalConfig,
    pub workspace: WorkspaceConfig,
    pub hooks: HooksConfig,
    pub mcp: McpConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            model: ModelConfig::default(),
            approval: ApprovalConfig::default(),
            workspace: WorkspaceConfig::default(),
            hooks: HooksConfig::default(),
            mcp: McpConfig::default(),
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
    pub require_mcp_confirmation: bool,
    pub mcp_call_allowlist: Vec<String>,
}

impl Default for ApprovalConfig {
    fn default() -> Self {
        Self {
            require_write_confirmation: true,
            require_shell_confirmation: true,
            require_mcp_confirmation: true,
            mcp_call_allowlist: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HooksConfig {
    pub enabled: bool,
    pub project_dir: String,
    pub user_dir: String,
    pub timeout_ms: u64,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            project_dir: ".dscode/hooks".to_string(),
            user_dir: "~/.config/dscode/hooks".to_string(),
            timeout_ms: 5_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct McpConfig {
    pub enabled: bool,
    pub project_file: String,
    pub user_file: String,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            project_file: ".dscode/mcp.json".to_string(),
            user_file: "~/.config/dscode/mcp.json".to_string(),
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
