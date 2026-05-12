use std::collections::BTreeMap;
use std::env;
use std::path::Path;
use std::process::Command;

use crate::cli::app::DoctorArgs;
use crate::config::load::load_or_default;
use crate::config::types::AppConfig;
use crate::error::AppResult;
use crate::util::json::{json_value_to_string, JsonValue};

pub fn run(args: DoctorArgs) -> AppResult<()> {
    let config = load_or_default()?;
    if args.json {
        println!("{}", render_json_report(&config));
        return Ok(());
    }

    println!("DeepSeekCode doctor");
    print_workspace_section(&config);
    print_skills_section(&config);
    print_model_section(&config);
    print_capabilities_section(&config);
    print_api_key_section(&config);
    print_network_section(&config);
    print_github_section();
    print_hints_section(&config);
    Ok(())
}

fn render_json_report(config: &AppConfig) -> String {
    json_value_to_string(&JsonValue::Object(build_json_report(config)))
}

fn build_json_report(config: &AppConfig) -> BTreeMap<String, JsonValue> {
    let mut root = BTreeMap::new();
    root.insert(
        "version".to_string(),
        JsonValue::String(env!("CARGO_PKG_VERSION").to_string()),
    );
    root.insert(
        "workspace".to_string(),
        JsonValue::Object(build_workspace_json(config)),
    );
    root.insert(
        "model".to_string(),
        JsonValue::Object(build_model_json(config)),
    );
    root.insert(
        "capabilities".to_string(),
        JsonValue::Object(build_capabilities_json(config)),
    );
    root.insert(
        "api_key".to_string(),
        JsonValue::Object(build_api_key_json(config)),
    );
    root.insert(
        "skills".to_string(),
        JsonValue::Object(build_skills_json(config)),
    );
    root.insert("mcp".to_string(), JsonValue::Object(build_mcp_json(config)));
    root.insert(
        "network".to_string(),
        JsonValue::Object(object([
            (
                "probe",
                JsonValue::String("skipped_in_json_mode".to_string()),
            ),
            (
                "reason",
                JsonValue::String(
                    "doctor --json is stable for local supervisors and does not perform live network probes"
                        .to_string(),
                ),
            ),
        ])),
    );
    root.insert(
        "binaries".to_string(),
        JsonValue::Object(object([
            ("curl", JsonValue::Bool(command_available("curl"))),
            ("gh", JsonValue::Bool(command_available("gh"))),
        ])),
    );
    root
}

fn build_workspace_json(config: &AppConfig) -> BTreeMap<String, JsonValue> {
    let config_path = config.workspace.config_path();
    let session_dir = config.workspace.session_dir();
    object([
        (
            "cwd",
            JsonValue::String(
                env::current_dir()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|_| ".".to_string()),
            ),
        ),
        (
            "config_path",
            JsonValue::String(config_path.display().to_string()),
        ),
        ("config_present", JsonValue::Bool(config_path.exists())),
        (
            "session_dir",
            JsonValue::String(session_dir.display().to_string()),
        ),
        ("session_dir_present", JsonValue::Bool(session_dir.exists())),
        (
            "commands_dir",
            JsonValue::String(config.workspace.commands_dir().display().to_string()),
        ),
        (
            "user_commands_dir",
            JsonValue::String(config.workspace.user_commands_dir().display().to_string()),
        ),
        (
            "user_instructions_file",
            JsonValue::String(config.workspace.user_instructions_file.clone()),
        ),
    ])
}

fn build_model_json(config: &AppConfig) -> BTreeMap<String, JsonValue> {
    let flavor = ApiFlavor::detect(&config.model.base_url);
    object([
        ("base_url", JsonValue::String(config.model.base_url.clone())),
        ("model", JsonValue::String(config.model.model.clone())),
        (
            "api_key_env",
            JsonValue::String(config.model.api_key_env.clone()),
        ),
        ("protocol", JsonValue::String(flavor.label().to_string())),
        (
            "endpoint",
            JsonValue::String(flavor.endpoint(&config.model.base_url)),
        ),
    ])
}

fn build_capabilities_json(config: &AppConfig) -> BTreeMap<String, JsonValue> {
    let capabilities = infer_model_capabilities(&config.model.model, &config.model.base_url);
    object([
        (
            "context_window",
            JsonValue::String(capabilities.context_window),
        ),
        (
            "max_output_tokens",
            JsonValue::String(capabilities.max_output_tokens),
        ),
        (
            "coding_optimized",
            JsonValue::Bool(capabilities.coding_optimized),
        ),
        ("image_input", JsonValue::Bool(capabilities.image_input)),
        ("web_search", JsonValue::Bool(capabilities.web_search)),
        ("note", JsonValue::String(capabilities.note)),
    ])
}

fn build_api_key_json(config: &AppConfig) -> BTreeMap<String, JsonValue> {
    match env::var(&config.model.api_key_env) {
        Ok(value) if !value.trim().is_empty() => object([
            ("env", JsonValue::String(config.model.api_key_env.clone())),
            ("source", JsonValue::String("env".to_string())),
            ("present", JsonValue::Bool(true)),
            ("empty", JsonValue::Bool(false)),
            ("masked", JsonValue::String("redacted".to_string())),
        ]),
        Ok(_) => object([
            ("env", JsonValue::String(config.model.api_key_env.clone())),
            ("source", JsonValue::String("env_empty".to_string())),
            ("present", JsonValue::Bool(false)),
            ("empty", JsonValue::Bool(true)),
            ("masked", JsonValue::String(String::new())),
        ]),
        Err(_) => object([
            ("env", JsonValue::String(config.model.api_key_env.clone())),
            ("source", JsonValue::String("missing".to_string())),
            ("present", JsonValue::Bool(false)),
            ("empty", JsonValue::Bool(false)),
            ("masked", JsonValue::String(String::new())),
        ]),
    }
}

fn build_skills_json(config: &AppConfig) -> BTreeMap<String, JsonValue> {
    let user_dir = crate::skills::tilde::expand_tilde(&config.workspace.user_skills_dir);
    let repo_path = crate::skills::paths::resolve_repo_skills_dir();
    match crate::skills::registry::SkillRegistry::load_dirs(&[
        repo_path.as_path(),
        user_dir.as_path(),
    ]) {
        Ok((_registry, stats)) => {
            let mut paths = Vec::new();
            for (path, count) in stats.by_path {
                paths.push(JsonValue::Object(object([
                    ("path", JsonValue::String(path.display().to_string())),
                    ("present", JsonValue::Bool(path.exists())),
                    ("count", JsonValue::Number(count.to_string())),
                ])));
            }
            object([
                ("status", JsonValue::String("ok".to_string())),
                ("total", JsonValue::Number(stats.total.to_string())),
                ("paths", JsonValue::Array(paths)),
                (
                    "overridden",
                    JsonValue::Array(
                        stats
                            .overridden
                            .into_iter()
                            .map(JsonValue::String)
                            .collect(),
                    ),
                ),
            ])
        }
        Err(error) => object([
            ("status", JsonValue::String("error".to_string())),
            ("total", JsonValue::Number("0".to_string())),
            ("paths", JsonValue::Array(Vec::new())),
            ("overridden", JsonValue::Array(Vec::new())),
            ("error", JsonValue::String(error.to_string())),
        ]),
    }
}

fn build_mcp_json(config: &AppConfig) -> BTreeMap<String, JsonValue> {
    let project_file = config.mcp.project_file_path();
    let user_file = config.mcp.user_file_path();
    object([
        ("enabled", JsonValue::Bool(config.mcp.enabled)),
        (
            "expose_remote_tools",
            JsonValue::Bool(config.mcp.expose_remote_tools),
        ),
        (
            "project_file",
            JsonValue::String(project_file.display().to_string()),
        ),
        ("project_present", JsonValue::Bool(project_file.exists())),
        (
            "user_file",
            JsonValue::String(user_file.display().to_string()),
        ),
        ("user_present", JsonValue::Bool(user_file.exists())),
    ])
}

fn print_workspace_section(config: &AppConfig) {
    let config_path = config.workspace.config_path();
    let session_dir = config.workspace.session_dir();

    println!();
    println!("[workspace]");
    println!(
        "  config path: {} ({})",
        config_path.display(),
        existence_label(&config_path)
    );
    println!(
        "  session dir: {} ({})",
        session_dir.display(),
        existence_label(&session_dir)
    );
}

fn print_skills_section(config: &AppConfig) {
    println!();
    println!("[skills]");
    let user_dir = crate::skills::tilde::expand_tilde(&config.workspace.user_skills_dir);
    let repo_path = crate::skills::paths::resolve_repo_skills_dir();
    match crate::skills::registry::SkillRegistry::load_dirs(&[
        repo_path.as_path(),
        user_dir.as_path(),
    ]) {
        Ok((_registry, stats)) => {
            println!("  loaded: {} skills", stats.total);
            for (path, count) in &stats.by_path {
                let label = path.display();
                if path.exists() {
                    println!("    {label}: {count} loaded");
                } else {
                    println!("    {label}: not found (skip)");
                }
            }
            if !stats.overridden.is_empty() {
                println!("  user overrides: {}", stats.overridden.join(", "));
            }
        }
        Err(error) => {
            println!("  error: {error}");
        }
    }
}

fn print_model_section(config: &AppConfig) {
    let flavor = ApiFlavor::detect(&config.model.base_url);

    println!();
    println!("[model]");
    println!("  base_url: {}", config.model.base_url);
    println!("  model: {}", config.model.model);
    println!("  api_key_env: {}", config.model.api_key_env);
    println!("  protocol: {}", flavor.label());
    println!("  endpoint: {}", flavor.endpoint(&config.model.base_url));
}

fn print_capabilities_section(config: &AppConfig) {
    let capabilities = infer_model_capabilities(&config.model.model, &config.model.base_url);
    println!();
    println!("[capabilities]");
    println!("  context_window: {}", capabilities.context_window);
    println!("  max_output_tokens: {}", capabilities.max_output_tokens);
    println!(
        "  coding_optimized: {}",
        yes_no(capabilities.coding_optimized)
    );
    println!("  image_input: {}", yes_no(capabilities.image_input));
    println!("  web_search: {}", yes_no(capabilities.web_search));
    if !capabilities.note.is_empty() {
        println!("  note: {}", capabilities.note);
    }
}

fn print_api_key_section(config: &AppConfig) {
    println!();
    println!("[api key]");
    match env::var(&config.model.api_key_env) {
        Ok(value) if !value.trim().is_empty() => {
            println!(
                "  {}: present ({})",
                config.model.api_key_env,
                mask_secret(value.trim())
            );
        }
        Ok(_) => {
            println!(
                "  {}: set but empty (export a real key)",
                config.model.api_key_env
            );
        }
        Err(_) => {
            println!(
                "  {}: missing — remote calls will fall back to the offline planner",
                config.model.api_key_env
            );
        }
    }
}

fn print_network_section(config: &AppConfig) {
    println!();
    println!("[network]");
    let host =
        host_from_url(&config.model.base_url).unwrap_or_else(|| config.model.base_url.clone());
    match probe_host(&config.model.base_url) {
        ProbeOutcome::Reachable { status } => {
            println!("  curl HEAD {host}: ok (status {status})");
        }
        ProbeOutcome::HttpStatus { status } => {
            println!("  curl HEAD {host}: reachable (status {status})");
        }
        ProbeOutcome::CurlMissing => {
            println!("  curl is not installed — install curl to enable remote calls");
        }
        ProbeOutcome::CurlError { message } => {
            println!("  curl HEAD {host}: failed ({message})");
        }
    }
}

fn print_github_section() {
    println!();
    println!("[github]");
    let version = std::process::Command::new("gh")
        .args(["--version"])
        .output();
    match version {
        Ok(out) if out.status.success() => {
            let first_line = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            println!("  gh CLI: {first_line}");
            let auth = std::process::Command::new("gh")
                .args(["auth", "status"])
                .output();
            match auth {
                Ok(auth_out) if auth_out.status.success() => {
                    println!("  gh auth: ok");
                }
                Ok(_) => {
                    println!("  gh auth: not authenticated (run `gh auth login`)");
                }
                Err(error) => {
                    println!("  gh auth: could not check ({error})");
                }
            }
        }
        Ok(_) | Err(_) => {
            println!("  gh CLI: not installed (install from https://cli.github.com/ for `deepseek pr` commands)");
        }
    }
}

fn print_hints_section(config: &AppConfig) {
    println!();
    println!("[hints]");
    println!(
        "  OpenAI-compatible path: {}/chat/completions (default for DeepSeek)",
        config.model.base_url.trim_end_matches('/')
    );
    println!(
        "  Anthropic-compatible path: append /anthropic to base_url, e.g. {}/anthropic",
        config.model.base_url.trim_end_matches('/')
    );
    println!(
        "  Run `deepseek smoke` (or `deepseek smoke --flavor anthropic`) to send a single live request."
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelCapabilities {
    context_window: String,
    max_output_tokens: String,
    coding_optimized: bool,
    image_input: bool,
    web_search: bool,
    note: String,
}

fn infer_model_capabilities(model: &str, base_url: &str) -> ModelCapabilities {
    let model_lower = model.to_ascii_lowercase();
    let base_lower = base_url.to_ascii_lowercase();
    if matches!(
        model_lower.trim(),
        "auto" | "auto-deepseek" | "deepseek-auto"
    ) {
        return ModelCapabilities {
            context_window: "1000000".to_string(),
            max_output_tokens: "provider-default".to_string(),
            coding_optimized: true,
            image_input: false,
            web_search: false,
            note: "DeepSeek auto router profile; simple tasks use v4-flash, complex planning/review work uses v4-pro."
                .to_string(),
        };
    }
    if model_lower.contains("codex-mini") {
        return ModelCapabilities {
            context_window: "200000".to_string(),
            max_output_tokens: "100000".to_string(),
            coding_optimized: true,
            image_input: true,
            web_search: false,
            note:
                "Codex-mini-style capability profile; verify provider limits with `deepseek smoke`."
                    .to_string(),
        };
    }
    if model_lower.contains("codex") || model_lower.contains("gpt-5") {
        return ModelCapabilities {
            context_window: "400000".to_string(),
            max_output_tokens: "128000".to_string(),
            coding_optimized: model_lower.contains("codex"),
            image_input: true,
            web_search: false,
            note:
                "OpenAI Codex/GPT-5-style profile; exact limits depend on the configured provider."
                    .to_string(),
        };
    }
    if model_lower.contains("deepseek") || base_lower.contains("deepseek") {
        return ModelCapabilities {
            context_window: "provider-default".to_string(),
            max_output_tokens: "provider-default".to_string(),
            coding_optimized: model_lower.contains("coder"),
            image_input: false,
            web_search: false,
            note: "`deepseek exec --image` keeps file references for DeepSeek text-only profiles; OpenAI/Anthropic vision-capable profiles send native image payloads.".to_string(),
        };
    }

    ModelCapabilities {
        context_window: "unknown".to_string(),
        max_output_tokens: "unknown".to_string(),
        coding_optimized: false,
        image_input: false,
        web_search: false,
        note: "Unknown provider profile; use `deepseek smoke` and provider docs to verify limits."
            .to_string(),
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn existence_label(path: &Path) -> &'static str {
    if path.exists() {
        "exists"
    } else {
        "missing"
    }
}

fn mask_secret(secret: &str) -> String {
    let visible = 4;
    if secret.len() <= visible {
        return "*".repeat(secret.len());
    }
    let tail = &secret[secret.len() - visible..];
    format!("{}{tail}", "*".repeat(secret.len() - visible))
}

#[derive(Clone, Copy)]
enum ApiFlavor {
    OpenAi,
    Anthropic,
}

impl ApiFlavor {
    fn detect(base_url: &str) -> Self {
        if base_url.trim_end_matches('/').ends_with("/anthropic") {
            Self::Anthropic
        } else {
            Self::OpenAi
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI-compatible (tools / function-calling)",
            Self::Anthropic => "Anthropic-compatible (tools / tool_use content blocks)",
        }
    }

    fn endpoint(self, base_url: &str) -> String {
        let trimmed = base_url.trim_end_matches('/');
        match self {
            Self::OpenAi => format!("{trimmed}/chat/completions"),
            Self::Anthropic => format!("{trimmed}/messages"),
        }
    }
}

enum ProbeOutcome {
    Reachable { status: String },
    HttpStatus { status: String },
    CurlMissing,
    CurlError { message: String },
}

fn probe_host(base_url: &str) -> ProbeOutcome {
    let target = host_probe_url(base_url);
    let result = Command::new("curl")
        .args([
            "-sS",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "--max-time",
            "5",
            "-I",
            &target,
        ])
        .output();

    match result {
        Ok(output) => {
            let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

            if !output.status.success() {
                let message = if stderr.is_empty() {
                    "curl returned a non-zero exit code".to_string()
                } else {
                    stderr
                };
                return ProbeOutcome::CurlError { message };
            }

            if status.is_empty() || status == "000" {
                return ProbeOutcome::CurlError {
                    message: "no http response (connection failed or dns error)".to_string(),
                };
            }

            if status.starts_with('2') || status.starts_with('3') {
                ProbeOutcome::Reachable { status }
            } else {
                ProbeOutcome::HttpStatus { status }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ProbeOutcome::CurlMissing,
        Err(error) => ProbeOutcome::CurlError {
            message: error.to_string(),
        },
    }
}

fn host_probe_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/anthropic") {
        trimmed.trim_end_matches("/anthropic").to_string()
    } else {
        trimmed.to_string()
    }
}

fn host_from_url(base_url: &str) -> Option<String> {
    let without_scheme = base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url);
    let host = without_scheme.split('/').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

fn command_available(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn object<const N: usize>(items: [(&str, JsonValue); N]) -> BTreeMap<String, JsonValue> {
    let mut map = BTreeMap::new();
    for (key, value) in items {
        map.insert(key.to_string(), value);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::json::{json_as_object, json_as_string, parse_root_object};

    #[test]
    fn detects_anthropic_base_url() {
        assert!(matches!(
            ApiFlavor::detect("https://api.deepseek.com/anthropic"),
            ApiFlavor::Anthropic
        ));
        assert!(matches!(
            ApiFlavor::detect("https://api.deepseek.com"),
            ApiFlavor::OpenAi
        ));
        assert!(matches!(
            ApiFlavor::detect("https://api.deepseek.com/anthropic/"),
            ApiFlavor::Anthropic
        ));
    }

    #[test]
    fn endpoint_appends_correct_path() {
        assert_eq!(
            ApiFlavor::OpenAi.endpoint("https://api.deepseek.com"),
            "https://api.deepseek.com/chat/completions"
        );
        assert_eq!(
            ApiFlavor::Anthropic.endpoint("https://api.deepseek.com/anthropic"),
            "https://api.deepseek.com/anthropic/messages"
        );
    }

    #[test]
    fn masks_short_and_long_secrets() {
        assert_eq!(mask_secret("abc"), "***");
        assert_eq!(mask_secret("abcd"), "****");
        assert_eq!(mask_secret("abcdef"), "**cdef");
        assert_eq!(mask_secret("sk-1234567890"), "*********7890");
    }

    #[test]
    fn host_probe_url_strips_anthropic_suffix() {
        assert_eq!(
            host_probe_url("https://api.deepseek.com/anthropic"),
            "https://api.deepseek.com"
        );
        assert_eq!(
            host_probe_url("https://api.deepseek.com"),
            "https://api.deepseek.com"
        );
    }

    #[test]
    fn host_from_url_extracts_host() {
        assert_eq!(
            host_from_url("https://api.deepseek.com/v1"),
            Some("api.deepseek.com".to_string())
        );
        assert_eq!(
            host_from_url("api.deepseek.com"),
            Some("api.deepseek.com".to_string())
        );
    }

    #[test]
    fn infer_model_capabilities_recognizes_codex_and_deepseek_profiles() {
        let codex = infer_model_capabilities("gpt-5.3-codex", "https://api.openai.com/v1");
        assert_eq!(codex.context_window, "400000");
        assert!(codex.image_input);
        assert!(codex.coding_optimized);

        let deepseek = infer_model_capabilities("deepseek-coder", "https://api.deepseek.com");
        assert_eq!(deepseek.context_window, "provider-default");
        assert!(!deepseek.image_input);
        assert!(deepseek.coding_optimized);

        let auto = infer_model_capabilities("auto", "https://api.deepseek.com");
        assert_eq!(auto.context_window, "1000000");
        assert!(auto.coding_optimized);
    }

    #[test]
    fn print_skills_section_does_not_panic() {
        // Smoke: with a default config, calling print_skills_section should not panic
        // even if the user-skills directory doesn't exist (it usually won't).
        let config = AppConfig::default();
        super::print_skills_section(&config);
    }

    #[test]
    fn json_report_is_valid_and_includes_stable_sections() {
        let config = AppConfig::default();
        let report = render_json_report(&config);
        let root = parse_root_object(&report).expect("doctor json should parse");

        assert_eq!(
            root.get("version").and_then(json_as_string),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert!(root.get("workspace").and_then(json_as_object).is_some());
        assert!(root.get("model").and_then(json_as_object).is_some());
        assert!(root.get("capabilities").and_then(json_as_object).is_some());
        assert!(root.get("api_key").and_then(json_as_object).is_some());
        assert!(root.get("skills").and_then(json_as_object).is_some());
        assert!(root.get("mcp").and_then(json_as_object).is_some());
        assert!(root.get("network").and_then(json_as_object).is_some());
        assert!(root.get("binaries").and_then(json_as_object).is_some());
    }

    #[test]
    fn json_report_skips_live_network_probe() {
        let config = AppConfig::default();
        let report = render_json_report(&config);
        let root = parse_root_object(&report).expect("doctor json should parse");
        let network = root
            .get("network")
            .and_then(json_as_object)
            .expect("network object should exist");

        assert_eq!(
            network.get("probe").and_then(json_as_string),
            Some("skipped_in_json_mode")
        );
    }

    #[test]
    fn json_api_key_status_does_not_include_secret_tail() {
        let env_name = format!(
            "DSCODE_DOCTOR_JSON_TEST_KEY_{}_{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        );
        std::env::set_var(&env_name, "sk-test-secret-tail-123456");
        let mut config = AppConfig::default();
        config.model.api_key_env = env_name.clone();

        let api_key = build_api_key_json(&config);
        assert_eq!(
            api_key.get("masked").and_then(json_as_string),
            Some("redacted")
        );
        assert!(!json_value_to_string(&JsonValue::Object(api_key)).contains("123456"));
        std::env::remove_var(env_name);
    }
}
