use crate::cli::app::ConfigArgs;
use crate::config::load::load_or_default;
use crate::config::types::AppConfig;
use crate::core::network_policy::{decide, normalize_host, NetworkDecision};
use crate::error::app_error;
use crate::error::AppResult;

pub fn run(args: ConfigArgs) -> AppResult<()> {
    if let Some(host) = args.network_allow {
        let result =
            persist_network_rule_at(&std::env::current_dir()?, &host, NetworkRuleTarget::Allow)?;
        print_network_rule_result(&result);
        return Ok(());
    }
    if let Some(host) = args.network_deny {
        let result =
            persist_network_rule_at(&std::env::current_dir()?, &host, NetworkRuleTarget::Deny)?;
        print_network_rule_result(&result);
        return Ok(());
    }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NetworkRuleTarget {
    Allow,
    Deny,
}

impl NetworkRuleTarget {
    fn key(self) -> &'static str {
        match self {
            Self::Allow => "network.allow",
            Self::Deny => "network.deny",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetworkRuleResult {
    pub(crate) path: std::path::PathBuf,
    pub(crate) key: &'static str,
    pub(crate) host: String,
    pub(crate) changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetworkPolicySummary {
    pub(crate) path: std::path::PathBuf,
    pub(crate) default: String,
    pub(crate) allow: Vec<String>,
    pub(crate) deny: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetworkDefaultResult {
    pub(crate) path: std::path::PathBuf,
    pub(crate) value: String,
    pub(crate) changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiagnosticsConfigSummary {
    pub(crate) path: std::path::PathBuf,
    pub(crate) post_edit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiagnosticsPostEditResult {
    pub(crate) path: std::path::PathBuf,
    pub(crate) value: bool,
    pub(crate) changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelConfigSummary {
    pub(crate) path: std::path::PathBuf,
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) api_key_env: String,
    pub(crate) reasoning_effort: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelSetResult {
    pub(crate) path: std::path::PathBuf,
    pub(crate) previous: String,
    pub(crate) model: String,
    pub(crate) changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderConfigSummary {
    pub(crate) path: std::path::PathBuf,
    pub(crate) provider: String,
    pub(crate) label: String,
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) api_key_env: String,
    pub(crate) reasoning_effort: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderSetResult {
    pub(crate) path: std::path::PathBuf,
    pub(crate) previous_provider: String,
    pub(crate) provider: String,
    pub(crate) label: String,
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) api_key_env: String,
    pub(crate) changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProviderPreset {
    name: &'static str,
    label: &'static str,
    base_url: &'static str,
    api_key_env: &'static str,
    default_model: &'static str,
}

fn print_network_rule_result(result: &NetworkRuleResult) {
    if result.changed {
        println!("{}: added {}", result.key, result.host);
    } else {
        println!("{}: {} already present", result.key, result.host);
    }
    println!("config: {}", result.path.display());
}

pub(crate) fn network_policy_summary_at(root: &std::path::Path) -> AppResult<NetworkPolicySummary> {
    let path = network_config_path_at(root);
    if !path.exists() {
        init_config_at(root, false)?;
    }
    let content = std::fs::read_to_string(&path)?;
    let default = read_string_key(&content, "network.default").unwrap_or_else(|| {
        crate::config::types::NetworkConfig::default()
            .default
            .to_string()
    });
    let allow = read_string_list_key(&content, "network.allow");
    let deny = read_string_list_key(&content, "network.deny");
    Ok(NetworkPolicySummary {
        path,
        default,
        allow,
        deny,
    })
}

pub(crate) fn set_network_rule_at(
    root: &std::path::Path,
    host: &str,
    target: NetworkRuleTarget,
) -> AppResult<NetworkRuleResult> {
    let host = normalize_network_rule(host)?;
    let path = network_config_path_at(root);
    if !path.exists() {
        init_config_at(root, false)?;
    }
    let content = std::fs::read_to_string(&path)?;
    let mut allow = read_string_list_key(&content, "network.allow");
    let mut deny = read_string_list_key(&content, "network.deny");
    let changed = match target {
        NetworkRuleTarget::Allow => {
            let removed = remove_normalized_host(&mut deny, &host);
            insert_sorted_unique(&mut allow, &host) || removed
        }
        NetworkRuleTarget::Deny => {
            let removed = remove_normalized_host(&mut allow, &host);
            insert_sorted_unique(&mut deny, &host) || removed
        }
    };
    if changed {
        let updated = replace_or_append_string_list_key(&content, "network.allow", &allow);
        let updated = replace_or_append_string_list_key(&updated, "network.deny", &deny);
        std::fs::write(&path, updated)?;
    }
    Ok(NetworkRuleResult {
        path,
        key: target.key(),
        host,
        changed,
    })
}

pub(crate) fn remove_network_rule_at(
    root: &std::path::Path,
    host: &str,
) -> AppResult<NetworkRuleResult> {
    let host = normalize_network_rule(host)?;
    let path = network_config_path_at(root);
    if !path.exists() {
        init_config_at(root, false)?;
    }
    let content = std::fs::read_to_string(&path)?;
    let mut allow = read_string_list_key(&content, "network.allow");
    let mut deny = read_string_list_key(&content, "network.deny");
    let changed =
        remove_normalized_host(&mut allow, &host) || remove_normalized_host(&mut deny, &host);
    if changed {
        let updated = replace_or_append_string_list_key(&content, "network.allow", &allow);
        let updated = replace_or_append_string_list_key(&updated, "network.deny", &deny);
        std::fs::write(&path, updated)?;
    }
    Ok(NetworkRuleResult {
        path,
        key: "network.allow/network.deny",
        host,
        changed,
    })
}

pub(crate) fn set_network_default_at(
    root: &std::path::Path,
    value: &str,
) -> AppResult<NetworkDefaultResult> {
    let value = match value.trim().to_ascii_lowercase().as_str() {
        "allow" => "allow",
        "deny" | "block" => "deny",
        "prompt" | "ask" => "prompt",
        _ => return Err(app_error("network default must be allow, deny, or prompt")),
    }
    .to_string();
    let path = network_config_path_at(root);
    if !path.exists() {
        init_config_at(root, false)?;
    }
    let content = std::fs::read_to_string(&path)?;
    let previous = read_string_key(&content, "network.default");
    let changed = previous.as_deref() != Some(value.as_str());
    if changed {
        let updated = replace_or_append_string_key(&content, "network.default", &value);
        std::fs::write(&path, updated)?;
    }
    Ok(NetworkDefaultResult {
        path,
        value,
        changed,
    })
}

pub(crate) fn diagnostics_config_summary_at(
    root: &std::path::Path,
) -> AppResult<DiagnosticsConfigSummary> {
    let path = network_config_path_at(root);
    if !path.exists() {
        init_config_at(root, false)?;
    }
    let content = std::fs::read_to_string(&path)?;
    let post_edit = read_bool_key(&content, "diagnostics.post_edit")
        .unwrap_or_else(|| AppConfig::default().diagnostics.post_edit);
    Ok(DiagnosticsConfigSummary { path, post_edit })
}

pub(crate) fn set_diagnostics_post_edit_at(
    root: &std::path::Path,
    enabled: bool,
) -> AppResult<DiagnosticsPostEditResult> {
    let path = network_config_path_at(root);
    if !path.exists() {
        init_config_at(root, false)?;
    }
    let content = std::fs::read_to_string(&path)?;
    let previous = read_bool_key(&content, "diagnostics.post_edit")
        .unwrap_or_else(|| AppConfig::default().diagnostics.post_edit);
    let changed = previous != enabled;
    if changed {
        let updated = replace_or_append_bool_key(&content, "diagnostics.post_edit", enabled);
        std::fs::write(&path, updated)?;
    }
    Ok(DiagnosticsPostEditResult {
        path,
        value: enabled,
        changed,
    })
}

pub(crate) fn model_config_summary_at(root: &std::path::Path) -> AppResult<ModelConfigSummary> {
    let path = network_config_path_at(root);
    if !path.exists() {
        init_config_at(root, false)?;
    }
    let content = std::fs::read_to_string(&path)?;
    let defaults = AppConfig::default();
    Ok(ModelConfigSummary {
        path,
        base_url: read_string_key(&content, "model.base_url").unwrap_or(defaults.model.base_url),
        model: read_string_key(&content, "model.model").unwrap_or(defaults.model.model),
        api_key_env: read_string_key(&content, "model.api_key_env")
            .unwrap_or(defaults.model.api_key_env),
        reasoning_effort: read_string_key(&content, "model.reasoning_effort")
            .unwrap_or(defaults.model.reasoning_effort),
    })
}

pub(crate) fn set_model_at(root: &std::path::Path, model: &str) -> AppResult<ModelSetResult> {
    let model = normalize_model_value(model)?;
    let path = network_config_path_at(root);
    if !path.exists() {
        init_config_at(root, false)?;
    }
    let content = std::fs::read_to_string(&path)?;
    let previous = read_string_key(&content, "model.model")
        .unwrap_or_else(|| AppConfig::default().model.model);
    let changed = previous != model;
    if changed {
        let updated = replace_or_append_string_key(&content, "model.model", &model);
        std::fs::write(&path, updated)?;
    }
    Ok(ModelSetResult {
        path,
        previous,
        model,
        changed,
    })
}

pub(crate) fn provider_config_summary_at(
    root: &std::path::Path,
) -> AppResult<ProviderConfigSummary> {
    let model = model_config_summary_at(root)?;
    let preset = infer_provider_preset(&model.base_url);
    Ok(ProviderConfigSummary {
        path: model.path,
        provider: preset.name.to_string(),
        label: preset.label.to_string(),
        base_url: model.base_url,
        model: model.model,
        api_key_env: model.api_key_env,
        reasoning_effort: model.reasoning_effort,
    })
}

pub(crate) fn set_provider_at(
    root: &std::path::Path,
    provider: &str,
    model: Option<&str>,
) -> AppResult<ProviderSetResult> {
    let preset = parse_provider_preset(provider).ok_or_else(|| {
        app_error(format!(
            "unknown provider `{provider}`; expected one of: {}",
            provider_preset_names()
        ))
    })?;
    let path = network_config_path_at(root);
    if !path.exists() {
        init_config_at(root, false)?;
    }
    let content = std::fs::read_to_string(&path)?;
    let previous_base_url = read_string_key(&content, "model.base_url")
        .unwrap_or_else(|| AppConfig::default().model.base_url);
    let previous_provider = infer_provider_preset(&previous_base_url).name.to_string();
    let previous_model = read_string_key(&content, "model.model")
        .unwrap_or_else(|| AppConfig::default().model.model);
    let previous_api_key_env = read_string_key(&content, "model.api_key_env")
        .unwrap_or_else(|| AppConfig::default().model.api_key_env);
    let model = match model {
        Some(model) => provider_model_value(preset, model)?,
        None => preset.default_model.to_string(),
    };
    let changed = previous_base_url != preset.base_url
        || previous_model != model
        || previous_api_key_env != preset.api_key_env;
    if changed {
        let updated = replace_or_append_string_key(&content, "model.base_url", preset.base_url);
        let updated =
            replace_or_append_string_key(&updated, "model.api_key_env", preset.api_key_env);
        let updated = replace_or_append_string_key(&updated, "model.model", &model);
        std::fs::write(&path, updated)?;
    }
    Ok(ProviderSetResult {
        path,
        previous_provider,
        provider: preset.name.to_string(),
        label: preset.label.to_string(),
        base_url: preset.base_url.to_string(),
        model,
        api_key_env: preset.api_key_env.to_string(),
        changed,
    })
}

fn print_config(config: &AppConfig) {
    println!("model.base_url = {}", config.model.base_url);
    println!("model.model = {}", config.model.model);
    println!("model.api_key_env = {}", config.model.api_key_env);
    println!("model.reasoning_effort = {}", config.model.reasoning_effort);
    println!("vision.base_url = {}", config.vision.base_url);
    println!("vision.model = {}", config.vision.model);
    println!("vision.api_key_env = {}", config.vision.api_key_env);
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
    println!("memory.enabled = {}", config.memory.enabled);
    println!("memory.notes_path = {}", config.memory.notes_path);
    println!("memory.memory_path = {}", config.memory.memory_path);
    println!("network.default = {}", config.network.default);
    println!(
        "network.allow = {}",
        render_string_list(&config.network.allow)
    );
    println!(
        "network.deny = {}",
        render_string_list(&config.network.deny)
    );
    println!("network.audit = {}", config.network.audit);
    println!("network.audit_path = {}", config.network.audit_path);
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
        "shell_env",
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

fn persist_network_rule_at(
    root: &std::path::Path,
    host: &str,
    target: NetworkRuleTarget,
) -> AppResult<NetworkRuleResult> {
    let host = normalize_network_rule(host)?;
    let default_config = AppConfig::default();
    let path = root.join(default_config.workspace.config_path());
    if !path.exists() {
        init_config_at(root, false)?;
    }

    let content = std::fs::read_to_string(&path)?;
    let mut allow = read_string_list_key(&content, "network.allow");
    let mut deny = read_string_list_key(&content, "network.deny");
    let changed = match target {
        NetworkRuleTarget::Allow => insert_sorted_unique(&mut allow, &host),
        NetworkRuleTarget::Deny => insert_sorted_unique(&mut deny, &host),
    };

    if target == NetworkRuleTarget::Allow {
        let mut future = crate::config::types::NetworkConfig::default();
        future.allow = allow.clone();
        future.deny = deny.clone();
        if decide(&future, &host) == NetworkDecision::Deny {
            return Err(app_error(format!(
                "network.deny already matches `{host}`; remove the deny rule before adding an allow rule"
            )));
        }
    }

    if changed {
        let values = match target {
            NetworkRuleTarget::Allow => &allow,
            NetworkRuleTarget::Deny => &deny,
        };
        let updated = replace_or_append_string_list_key(&content, target.key(), values);
        std::fs::write(&path, updated)?;
    }

    Ok(NetworkRuleResult {
        path,
        key: target.key(),
        host,
        changed,
    })
}

fn insert_sorted_unique(values: &mut Vec<String>, value: &str) -> bool {
    if values.iter().any(|existing| existing == value) {
        return false;
    }
    values.push(value.to_string());
    values.sort();
    true
}

fn remove_normalized_host(values: &mut Vec<String>, host: &str) -> bool {
    let before = values.len();
    values.retain(|existing| normalize_host(existing) != host);
    before != values.len()
}

fn network_config_path_at(root: &std::path::Path) -> std::path::PathBuf {
    let default_config = AppConfig::default();
    root.join(default_config.workspace.config_path())
}

fn normalize_network_rule(host: &str) -> AppResult<String> {
    let normalized = normalize_host(host);
    if normalized.is_empty()
        || normalized.contains('/')
        || normalized.contains('\\')
        || normalized.contains(',')
        || normalized.contains('"')
        || normalized.contains('\'')
        || normalized.chars().any(char::is_whitespace)
    {
        return Err(app_error(
            "network host rule must be a host, .subdomain suffix, or *.subdomain suffix",
        ));
    }
    Ok(normalized)
}

fn normalize_model_value(model: &str) -> AppResult<String> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return Err(app_error("model must not be empty"));
    }
    let normalized = match trimmed.to_ascii_lowercase().as_str() {
        "auto" | "auto-deepseek" | "deepseek-auto" => "auto".to_string(),
        "flash" | "v4-flash" => "deepseek-v4-flash".to_string(),
        "pro" | "v4-pro" => "deepseek-v4-pro".to_string(),
        "chat" => "deepseek-chat".to_string(),
        "coder" => "deepseek-coder".to_string(),
        "reasoner" => "deepseek-reasoner".to_string(),
        _ => trimmed.to_string(),
    };
    if normalized
        .chars()
        .any(|ch| ch.is_control() || ch.is_whitespace() || matches!(ch, '"' | '\'' | '\\'))
    {
        return Err(app_error(
            "model must be a single model id token without quotes or whitespace",
        ));
    }
    Ok(normalized)
}

fn provider_presets() -> &'static [ProviderPreset] {
    &[
        ProviderPreset {
            name: "deepseek",
            label: "DeepSeek",
            base_url: "https://api.deepseek.com",
            api_key_env: "DEEPSEEK_API_KEY",
            default_model: "deepseek-v4-pro",
        },
        ProviderPreset {
            name: "nvidia-nim",
            label: "NVIDIA NIM",
            base_url: "https://integrate.api.nvidia.com/v1",
            api_key_env: "NVIDIA_API_KEY",
            default_model: "deepseek-ai/deepseek-v4-pro",
        },
        ProviderPreset {
            name: "openai",
            label: "OpenAI-compatible",
            base_url: "https://api.openai.com/v1",
            api_key_env: "OPENAI_API_KEY",
            default_model: "gpt-4.1",
        },
        ProviderPreset {
            name: "atlascloud",
            label: "AtlasCloud",
            base_url: "https://api.atlascloud.ai/v1",
            api_key_env: "ATLASCLOUD_API_KEY",
            default_model: "deepseek-ai/deepseek-v4-flash",
        },
        ProviderPreset {
            name: "openrouter",
            label: "OpenRouter",
            base_url: "https://openrouter.ai/api/v1",
            api_key_env: "OPENROUTER_API_KEY",
            default_model: "deepseek/deepseek-v4-pro",
        },
        ProviderPreset {
            name: "novita",
            label: "Novita AI",
            base_url: "https://api.novita.ai/v1",
            api_key_env: "NOVITA_API_KEY",
            default_model: "deepseek/deepseek-v4-pro",
        },
        ProviderPreset {
            name: "fireworks",
            label: "Fireworks AI",
            base_url: "https://api.fireworks.ai/inference/v1",
            api_key_env: "FIREWORKS_API_KEY",
            default_model: "accounts/fireworks/models/deepseek-v4-pro",
        },
        ProviderPreset {
            name: "sglang",
            label: "SGLang",
            base_url: "http://localhost:30000/v1",
            api_key_env: "DEEPSEEK_API_KEY",
            default_model: "deepseek-ai/DeepSeek-V4-Pro",
        },
        ProviderPreset {
            name: "vllm",
            label: "vLLM",
            base_url: "http://localhost:8000/v1",
            api_key_env: "DEEPSEEK_API_KEY",
            default_model: "deepseek-ai/DeepSeek-V4-Pro",
        },
        ProviderPreset {
            name: "ollama",
            label: "Ollama",
            base_url: "http://localhost:11434/v1",
            api_key_env: "OLLAMA_API_KEY",
            default_model: "deepseek-coder:1.3b",
        },
    ]
}

fn provider_preset_names() -> String {
    provider_presets()
        .iter()
        .map(|preset| preset.name)
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_provider_preset(value: &str) -> Option<ProviderPreset> {
    let normalized = value.trim().to_ascii_lowercase();
    let name = match normalized.as_str() {
        "deepseek" | "deep-seek" => "deepseek",
        "nvidia" | "nvidia_nim" | "nvidia-nim" | "nim" => "nvidia-nim",
        "openai" | "open-ai" => "openai",
        "atlas" | "atlascloud" | "atlas-cloud" | "atlas_cloud" => "atlascloud",
        "openrouter" | "open_router" => "openrouter",
        "novita" => "novita",
        "fireworks" | "fireworks-ai" => "fireworks",
        "sglang" | "sg-lang" => "sglang",
        "vllm" | "v-llm" => "vllm",
        "ollama" | "ollama-local" => "ollama",
        _ => return None,
    };
    provider_presets()
        .iter()
        .copied()
        .find(|preset| preset.name == name)
}

fn infer_provider_preset(base_url: &str) -> ProviderPreset {
    let lower = base_url.trim_end_matches('/').to_ascii_lowercase();
    if lower.contains("integrate.api.nvidia.com") {
        return parse_provider_preset("nvidia-nim").expect("nvidia preset");
    }
    if lower.contains("api.openai.com") {
        return parse_provider_preset("openai").expect("openai preset");
    }
    if lower.contains("api.atlascloud.ai") {
        return parse_provider_preset("atlascloud").expect("atlascloud preset");
    }
    if lower.contains("openrouter.ai") {
        return parse_provider_preset("openrouter").expect("openrouter preset");
    }
    if lower.contains("api.novita.ai") {
        return parse_provider_preset("novita").expect("novita preset");
    }
    if lower.contains("api.fireworks.ai") {
        return parse_provider_preset("fireworks").expect("fireworks preset");
    }
    if lower.contains("localhost:30000") || lower.contains("127.0.0.1:30000") {
        return parse_provider_preset("sglang").expect("sglang preset");
    }
    if lower.contains("localhost:8000") || lower.contains("127.0.0.1:8000") {
        return parse_provider_preset("vllm").expect("vllm preset");
    }
    if lower.contains("localhost:11434") || lower.contains("127.0.0.1:11434") {
        return parse_provider_preset("ollama").expect("ollama preset");
    }
    parse_provider_preset("deepseek").expect("deepseek preset")
}

fn provider_model_value(preset: ProviderPreset, raw: &str) -> AppResult<String> {
    let model = normalize_model_value(raw)?;
    let lower = model.to_ascii_lowercase();
    let mapped = match (preset.name, lower.as_str()) {
        ("nvidia-nim", "deepseek-v4-pro") => "deepseek-ai/deepseek-v4-pro",
        ("nvidia-nim", "deepseek-v4-flash") => "deepseek-ai/deepseek-v4-flash",
        ("openrouter", "deepseek-v4-pro") => "deepseek/deepseek-v4-pro",
        ("openrouter", "deepseek-v4-flash") => "deepseek/deepseek-v4-flash",
        ("novita", "deepseek-v4-pro") => "deepseek/deepseek-v4-pro",
        ("novita", "deepseek-v4-flash") => "deepseek/deepseek-v4-flash",
        ("fireworks", "deepseek-v4-pro") => "accounts/fireworks/models/deepseek-v4-pro",
        ("fireworks", "deepseek-v4-flash") => "accounts/fireworks/models/deepseek-v4-flash",
        ("sglang", "deepseek-v4-pro") => "deepseek-ai/DeepSeek-V4-Pro",
        ("sglang", "deepseek-v4-flash") => "deepseek-ai/DeepSeek-V4-Flash",
        ("vllm", "deepseek-v4-pro") => "deepseek-ai/DeepSeek-V4-Pro",
        ("vllm", "deepseek-v4-flash") => "deepseek-ai/DeepSeek-V4-Flash",
        _ => return Ok(model),
    };
    Ok(mapped.to_string())
}

fn read_string_key(content: &str, key: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix(key) else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(value) = rest.strip_prefix('=') else {
            continue;
        };
        return Some(unquote_config_string(value.trim()));
    }
    None
}

fn read_bool_key(content: &str, key: &str) -> Option<bool> {
    for line in content.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix(key) else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(value) = rest.strip_prefix('=') else {
            continue;
        };
        return match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Some(true),
            "false" | "0" | "no" | "off" => Some(false),
            _ => None,
        };
    }
    None
}

fn unquote_config_string(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].replace("\\\"", "\"")
    } else {
        trimmed.to_string()
    }
}

fn read_string_list_key(content: &str, key: &str) -> Vec<String> {
    for line in content.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix(key) else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(value) = rest.strip_prefix('=') else {
            continue;
        };
        return parse_string_list_literal(value.trim());
    }
    Vec::new()
}

fn parse_string_list_literal(value: &str) -> Vec<String> {
    let Some(start) = value.find('[') else {
        return Vec::new();
    };
    let Some(end) = value[start + 1..].find(']') else {
        return Vec::new();
    };
    let body = &value[start + 1..start + 1 + end];
    let mut values = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut escaped = false;
    for ch in body.chars() {
        if !in_string {
            if ch == '"' {
                in_string = true;
                current.clear();
            }
            continue;
        }
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => {
                in_string = false;
                values.push(current.clone());
            }
            _ => current.push(ch),
        }
    }
    values
}

fn replace_or_append_string_list_key(content: &str, key: &str, values: &[String]) -> String {
    let rendered = format!("{key} = {}", render_string_list(values));
    replace_or_append_line(content, key, rendered)
}

fn replace_or_append_string_key(content: &str, key: &str, value: &str) -> String {
    let rendered = format!("{key} = \"{}\"", value.replace('"', "\\\""));
    replace_or_append_line(content, key, rendered)
}

fn replace_or_append_bool_key(content: &str, key: &str, value: bool) -> String {
    replace_or_append_line(content, key, format!("{key} = {value}"))
}

fn replace_or_append_line(content: &str, key: &str, rendered: String) -> String {
    let mut replaced = false;
    let mut lines = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim_start();
        if !replaced
            && trimmed
                .strip_prefix(key)
                .is_some_and(|rest| rest.trim_start().starts_with('='))
        {
            lines.push(rendered.clone());
            replaced = true;
        } else {
            lines.push(line.to_string());
        }
    }
    if !replaced {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(rendered);
    }
    let mut updated = lines.join("\n");
    updated.push('\n');
    updated
}

fn render_default_config(config: &AppConfig) -> String {
    format!(
        r#"# DeepSeekCode project configuration
model.base_url = "{base_url}"
model.model = "{model}"
model.api_key_env = "{api_key_env}"
model.reasoning_effort = "{reasoning_effort}"

# Optional OpenAI-compatible vision model for the image_analyze tool.
vision.base_url = "{vision_base_url}"
vision.model = "{vision_model}"
vision.api_key_env = "{vision_api_key_env}"

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

# User memory is opt-in. `note` appends to notes_path; `remember` is exposed
# only when memory.enabled is true and appends to memory_path.
memory.enabled = {memory_enabled}
memory.notes_path = "{memory_notes_path}"
memory.memory_path = "{memory_memory_path}"

# Read-only web/search/fetch tools honor this DeepSeek-TUI-style host policy.
# Deny entries win over allow entries. A leading dot matches subdomains only.
network.default = "{network_default}"
network.allow = {network_allow}
network.deny = {network_deny}
network.audit = {network_audit}
network.audit_path = "{network_audit_path}"
"#,
        base_url = config.model.base_url,
        model = config.model.model,
        api_key_env = config.model.api_key_env,
        reasoning_effort = config.model.reasoning_effort,
        vision_base_url = config.vision.base_url,
        vision_model = config.vision.model,
        vision_api_key_env = config.vision.api_key_env,
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
        memory_enabled = config.memory.enabled,
        memory_notes_path = config.memory.notes_path,
        memory_memory_path = config.memory.memory_path,
        network_default = config.network.default,
        network_allow = render_string_list(&config.network.allow),
        network_deny = render_string_list(&config.network.deny),
        network_audit = config.network.audit,
        network_audit_path = config.network.audit_path,
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
        assert!(content.contains("vision.model"));
        assert!(content.contains("network.default"));
        assert!(content.contains("network.audit_path"));
        assert!(content.contains("hooks.enabled = false"));
        assert!(root.join(".dscode/sessions").is_dir());
        assert!(root.join(".dscode/commands").is_dir());
        assert!(root.join(".dscode/hooks/pre_tool_use").is_dir());
        assert!(root.join(".dscode/hooks/shell_env").is_dir());
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

    #[test]
    fn persist_network_allow_adds_normalized_host_to_config() {
        let root = temp_root("network-allow");
        init_config_at(&root, false).unwrap();

        let result =
            persist_network_rule_at(&root, "*.Example.com", NetworkRuleTarget::Allow).unwrap();

        assert!(result.changed);
        assert_eq!(result.host, ".example.com");
        let content = std::fs::read_to_string(root.join(".dscode/config.toml")).unwrap();
        assert!(content.contains(r#"network.allow = [".example.com"]"#));

        let second =
            persist_network_rule_at(&root, ".example.com", NetworkRuleTarget::Allow).unwrap();
        assert!(!second.changed);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn persist_network_allow_refuses_matching_deny_rule() {
        let root = temp_root("network-deny-wins");
        init_config_at(&root, false).unwrap();
        persist_network_rule_at(&root, ".example.com", NetworkRuleTarget::Deny).unwrap();

        let error = persist_network_rule_at(&root, "api.example.com", NetworkRuleTarget::Allow)
            .unwrap_err();

        assert!(error.to_string().contains("network.deny already matches"));

        let _ = std::fs::remove_dir_all(root);
    }
}
