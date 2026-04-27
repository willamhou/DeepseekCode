use std::env;
use std::path::Path;
use std::process::Command;

use crate::cli::app::DoctorArgs;
use crate::config::load::load_or_default;
use crate::config::types::AppConfig;
use crate::error::AppResult;

pub fn run(_args: DoctorArgs) -> AppResult<()> {
    let config = load_or_default()?;
    println!("DeepseekCode doctor");
    print_workspace_section(&config);
    print_model_section(&config);
    print_api_key_section(&config);
    print_network_section(&config);
    print_github_section();
    print_hints_section(&config);
    Ok(())
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
    let host = host_from_url(&config.model.base_url).unwrap_or_else(|| config.model.base_url.clone());
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
            println!("  gh CLI: not installed (install from https://cli.github.com/ for `dscode pr` commands)");
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
        "  Run `dscode smoke` (or `dscode smoke --flavor anthropic`) to send a single live request."
    );
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
            Self::Anthropic => "Anthropic-compatible (json plan fallback today)",
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
    let without_scheme = base_url.split_once("://").map(|(_, rest)| rest).unwrap_or(base_url);
    let host = without_scheme.split('/').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(host_from_url("api.deepseek.com"), Some("api.deepseek.com".to_string()));
    }
}
