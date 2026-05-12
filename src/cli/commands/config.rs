use crate::cli::app::ConfigArgs;
use crate::config::load::load_or_default;
use crate::config::types::AppConfig;
use crate::error::app_error;
use crate::error::AppResult;

pub fn run(args: ConfigArgs) -> AppResult<()> {
    if args.init {
        let path = init_config_at(&std::env::current_dir()?, args.force)?;
        println!("initialized config: {}", path.display());
        return Ok(());
    }

    let config = load_or_default()?;
    if args.print_default {
        print_config(&config);
    } else {
        println!(
            "Config file path: {}",
            config.workspace.config_path().display()
        );
    }
    Ok(())
}

fn print_config(config: &AppConfig) {
    println!("model.base_url = {}", config.model.base_url);
    println!("model.model = {}", config.model.model);
    println!("model.api_key_env = {}", config.model.api_key_env);
    println!("model.reasoning_effort = {}", config.model.reasoning_effort);
    println!(
        "approval.require_write_confirmation = {}",
        config.approval.require_write_confirmation
    );
    println!(
        "approval.require_shell_confirmation = {}",
        config.approval.require_shell_confirmation
    );
    println!(
        "approval.require_mcp_confirmation = {}",
        config.approval.require_mcp_confirmation
    );
    println!(
        "approval.mcp_call_allowlist = {}",
        render_string_list(&config.approval.mcp_call_allowlist)
    );
    println!("workspace.config_dir = {}", config.workspace.config_dir);
    println!("workspace.session_dir = {}", config.workspace.session_dir);
    println!(
        "workspace.user_skills_dir = {}",
        config.workspace.user_skills_dir
    );
    println!(
        "workspace.user_commands_dir = {}",
        config.workspace.user_commands_dir
    );
    println!(
        "workspace.user_instructions_file = {}",
        config.workspace.user_instructions_file
    );
    println!("hooks.enabled = {}", config.hooks.enabled);
    println!("hooks.project_dir = {}", config.hooks.project_dir);
    println!("hooks.user_dir = {}", config.hooks.user_dir);
    println!("hooks.timeout_ms = {}", config.hooks.timeout_ms);
    println!("mcp.enabled = {}", config.mcp.enabled);
    println!(
        "mcp.expose_remote_tools = {}",
        config.mcp.expose_remote_tools
    );
    println!("mcp.project_file = {}", config.mcp.project_file);
    println!("mcp.user_file = {}", config.mcp.user_file);
    println!("diagnostics.post_edit = {}", config.diagnostics.post_edit);
}

pub(crate) fn init_config_at(root: &std::path::Path, force: bool) -> AppResult<std::path::PathBuf> {
    let config = AppConfig::default();
    let config_dir = root.join(&config.workspace.config_dir);
    let config_path = config_dir.join("config.toml");

    if config_path.exists() && !force {
        return Err(app_error(format!(
            "config already exists: {} (use --force to overwrite)",
            config_path.display()
        )));
    }

    std::fs::create_dir_all(&config_dir)?;
    std::fs::write(&config_path, render_default_config(&config))?;
    std::fs::create_dir_all(root.join(&config.workspace.session_dir))?;
    std::fs::create_dir_all(root.join(&config.workspace.config_dir).join("commands"))?;
    std::fs::create_dir_all(root.join(&config.workspace.config_dir).join("agents"))?;

    for event in [
        "session_start",
        "session_stop",
        "user_prompt_submit",
        "pre_tool_use",
        "permission_request",
        "post_tool_use",
        "subagent_start",
        "subagent_stop",
        "pre_compact",
    ] {
        std::fs::create_dir_all(root.join(&config.hooks.project_dir).join(event))?;
    }
    let mcp_path = root.join(config.mcp.project_file_path());
    if !mcp_path.exists() {
        if let Some(parent) = mcp_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&mcp_path, render_default_mcp_config())?;
    }

    Ok(config_path)
}

fn render_default_config(config: &AppConfig) -> String {
    format!(
        r#"# DeepSeekCode project configuration
model.base_url = "{base_url}"
model.model = "{model}"
model.api_key_env = "{api_key_env}"
model.reasoning_effort = "{reasoning_effort}"

approval.require_write_confirmation = {require_write_confirmation}
approval.require_shell_confirmation = {require_shell_confirmation}
approval.require_mcp_confirmation = {require_mcp_confirmation}
approval.mcp_call_allowlist = {mcp_call_allowlist}

workspace.config_dir = "{config_dir}"
workspace.session_dir = "{session_dir}"
workspace.user_skills_dir = "{user_skills_dir}"
workspace.user_commands_dir = "{user_commands_dir}"
workspace.user_instructions_file = "{user_instructions_file}"

# Hooks are disabled by default. Enable only for hook scripts you trust.
hooks.enabled = {hooks_enabled}
hooks.project_dir = "{hooks_project_dir}"
hooks.user_dir = "{hooks_user_dir}"
hooks.timeout_ms = {hooks_timeout_ms}

# MCP server discovery supports config inspection plus stdio/http/sse tools/list/call.
# Keep dynamic remote tool exposure off unless you trust the configured MCP servers.
# Use `deepseek mcp list|doctor|tools|call` to inspect or invoke MCP definitions.
mcp.enabled = {mcp_enabled}
mcp.expose_remote_tools = {mcp_expose_remote_tools}
mcp.project_file = "{mcp_project_file}"
mcp.user_file = "{mcp_user_file}"

# Diagnostics can be run manually with `deepseek diagnostics`.
# Set post_edit to true to append diagnostics after successful apply_patch calls.
diagnostics.post_edit = {diagnostics_post_edit}
"#,
        base_url = config.model.base_url,
        model = config.model.model,
        api_key_env = config.model.api_key_env,
        reasoning_effort = config.model.reasoning_effort,
        require_write_confirmation = config.approval.require_write_confirmation,
        require_shell_confirmation = config.approval.require_shell_confirmation,
        require_mcp_confirmation = config.approval.require_mcp_confirmation,
        mcp_call_allowlist = render_string_list(&config.approval.mcp_call_allowlist),
        config_dir = config.workspace.config_dir,
        session_dir = config.workspace.session_dir,
        user_skills_dir = config.workspace.user_skills_dir,
        user_commands_dir = config.workspace.user_commands_dir,
        user_instructions_file = config.workspace.user_instructions_file,
        hooks_enabled = config.hooks.enabled,
        hooks_project_dir = config.hooks.project_dir,
        hooks_user_dir = config.hooks.user_dir,
        hooks_timeout_ms = config.hooks.timeout_ms,
        mcp_enabled = config.mcp.enabled,
        mcp_expose_remote_tools = config.mcp.expose_remote_tools,
        mcp_project_file = config.mcp.project_file,
        mcp_user_file = config.mcp.user_file,
        diagnostics_post_edit = config.diagnostics.post_edit,
    )
}

fn render_string_list(values: &[String]) -> String {
    if values.is_empty() {
        return "[]".to_string();
    }
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!("\"{}\"", value.replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn render_default_mcp_config() -> &'static str {
    r#"{
  "mcpServers": {
    "example-filesystem": {
      "disabled": true,
      "transport": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "."]
    }
  }
}
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "deepseek-config-{name}-{}-{suffix}",
            std::process::id()
        ))
    }

    #[test]
    fn init_config_creates_project_bootstrap_files() {
        let root = temp_root("init");
        let path = init_config_at(&root, false).unwrap();

        assert_eq!(path, root.join(".dscode/config.toml"));
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("model.base_url"));
        assert!(content.contains("hooks.enabled = false"));
        assert!(root.join(".dscode/sessions").is_dir());
        assert!(root.join(".dscode/commands").is_dir());
        assert!(root.join(".dscode/hooks/pre_tool_use").is_dir());
        assert!(root.join(".dscode/mcp.json").is_file());
        let mcp = std::fs::read_to_string(root.join(".dscode/mcp.json")).unwrap();
        assert!(mcp.contains("mcpServers"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn init_config_refuses_existing_file_without_force() {
        let root = temp_root("exists");
        let path = init_config_at(&root, false).unwrap();
        std::fs::write(&path, "sentinel").unwrap();

        let error = init_config_at(&root, false).unwrap_err();
        assert!(error.to_string().contains("config already exists"));

        init_config_at(&root, true).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("DeepSeekCode project configuration"));

        let _ = std::fs::remove_dir_all(root);
    }
}
