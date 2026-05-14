use crate::cli::app::CompletionShell;
use crate::error::{app_error, AppResult};

pub fn run(shell: CompletionShell) -> AppResult<()> {
    let script = match shell {
        CompletionShell::Bash => bash_completion(),
        CompletionShell::Zsh => zsh_completion(),
        CompletionShell::Fish => fish_completion(),
    };
    if script.trim().is_empty() {
        return Err(app_error(
            "completion script generation returned empty output",
        ));
    }
    print!("{script}");
    Ok(())
}

fn command_words() -> &'static [&'static str] {
    &[
        "agents",
        "benchmark",
        "chat",
        "completion",
        "config",
        "diagnostics",
        "diff",
        "doctor",
        "dogfood",
        "exec",
        "interactive",
        "mcp",
        "pr",
        "repl",
        "resume",
        "restore",
        "run",
        "smoke",
        "tui",
        "update",
        "version",
    ]
}

fn dogfood_words() -> &'static [&'static str] {
    &[
        "run",
        "external-fixture",
        "replay-benchmark",
        "report",
        "export-benchmark",
        "promote-benchmark",
    ]
}

fn pr_words() -> &'static [&'static str] {
    &["review", "fix", "patch"]
}

fn mcp_words() -> &'static [&'static str] {
    &[
        "list", "doctor", "tools", "prompts", "call", "prompt", "init",
    ]
}

fn agents_words() -> &'static [&'static str] {
    &[
        "list",
        "show",
        "validate",
        "run-task",
        "daemon",
        "rlm-status",
        "rlm-events",
        "rlm-wait",
        "rlm-cancel",
        "rlm-recover",
        "rlm-stop",
        "rlm-run-next",
        "rlm-drain",
        "service",
        "threads",
        "show-thread",
        "switch",
        "current",
        "clear-current",
    ]
}

fn update_words() -> &'static [&'static str] {
    &["package", "verify-install", "install-package", "rollback"]
}

fn restore_words() -> &'static [&'static str] {
    &["snapshot", "list", "show", "revert-turn"]
}

fn shell_words() -> &'static [&'static str] {
    &["bash", "zsh", "fish"]
}

fn bash_completion() -> String {
    let commands = command_words().join(" ");
    let dogfood = dogfood_words().join(" ");
    let pr = pr_words().join(" ");
    let mcp = mcp_words().join(" ");
    let agents = agents_words().join(" ");
    let update = update_words().join(" ");
    let restore = restore_words().join(" ");
    let shells = shell_words().join(" ");
    format!(
        r#"_deepseek()
{{
    local cur prev words cword
    _init_completion || return

    if [[ $cword -eq 1 ]]; then
        COMPREPLY=( $(compgen -W "{commands}" -- "$cur") )
        return
    fi

    case "${{words[1]}}" in
        dogfood)
            if [[ $cword -eq 2 ]]; then
                COMPREPLY=( $(compgen -W "{dogfood}" -- "$cur") )
            fi
            ;;
        pr)
            if [[ $cword -eq 2 ]]; then
                COMPREPLY=( $(compgen -W "{pr}" -- "$cur") )
            fi
            ;;
        mcp)
            if [[ $cword -eq 2 ]]; then
                COMPREPLY=( $(compgen -W "{mcp}" -- "$cur") )
            fi
            ;;
        agents)
            if [[ $cword -eq 2 ]]; then
                COMPREPLY=( $(compgen -W "{agents}" -- "$cur") )
            fi
            ;;
        update)
            if [[ $cword -eq 2 ]]; then
                COMPREPLY=( $(compgen -W "{update}" -- "$cur") )
            fi
            ;;
        restore)
            if [[ $cword -eq 2 ]]; then
                COMPREPLY=( $(compgen -W "{restore}" -- "$cur") )
            fi
            ;;
        completion)
            if [[ $cword -eq 2 ]]; then
                COMPREPLY=( $(compgen -W "{shells}" -- "$cur") )
            fi
            ;;
    esac
}}

complete -F _deepseek deepseek
"#
    )
}

fn zsh_completion() -> String {
    let commands = command_words();
    let dogfood = dogfood_words();
    let pr = pr_words();
    let mcp = mcp_words();
    let agents = agents_words();
    let update = update_words();
    let restore = restore_words();
    let shells = shell_words();
    format!(
        r#"#compdef deepseek

_deepseek() {{
  local -a commands
  commands=(
{commands}
  )

  if (( CURRENT == 2 )); then
    _describe 'command' commands
    return
  fi

  case "$words[2]" in
    dogfood)
      _values 'dogfood action' {dogfood}
      ;;
    pr)
      _values 'pr action' {pr}
      ;;
    mcp)
      _values 'mcp action' {mcp}
      ;;
    agents)
      _values 'agents action' {agents}
      ;;
    update)
      _values 'update action' {update}
      ;;
    restore)
      _values 'restore action' {restore}
      ;;
    completion)
      _values 'shell' {shells}
      ;;
  esac
}}

_deepseek "$@"
"#,
        commands = commands
            .iter()
            .map(|value| format!("    '{value}'"))
            .collect::<Vec<_>>()
            .join("\n"),
        dogfood = dogfood
            .iter()
            .map(|value| format!("'{value}'"))
            .collect::<Vec<_>>()
            .join(" "),
        pr = pr
            .iter()
            .map(|value| format!("'{value}'"))
            .collect::<Vec<_>>()
            .join(" "),
        mcp = mcp
            .iter()
            .map(|value| format!("'{value}'"))
            .collect::<Vec<_>>()
            .join(" "),
        agents = agents
            .iter()
            .map(|value| format!("'{value}'"))
            .collect::<Vec<_>>()
            .join(" "),
        update = update
            .iter()
            .map(|value| format!("'{value}'"))
            .collect::<Vec<_>>()
            .join(" "),
        restore = restore
            .iter()
            .map(|value| format!("'{value}'"))
            .collect::<Vec<_>>()
            .join(" "),
        shells = shells
            .iter()
            .map(|value| format!("'{value}'"))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn fish_completion() -> String {
    let mut lines = Vec::new();
    for command in command_words() {
        lines.push(format!(
            "complete -c deepseek -n '__fish_use_subcommand' -a '{command}'"
        ));
    }
    for action in dogfood_words() {
        lines.push(format!(
            "complete -c deepseek -n '__fish_seen_subcommand_from dogfood' -a '{action}'"
        ));
    }
    for action in pr_words() {
        lines.push(format!(
            "complete -c deepseek -n '__fish_seen_subcommand_from pr' -a '{action}'"
        ));
    }
    for action in mcp_words() {
        lines.push(format!(
            "complete -c deepseek -n '__fish_seen_subcommand_from mcp' -a '{action}'"
        ));
    }
    for action in agents_words() {
        lines.push(format!(
            "complete -c deepseek -n '__fish_seen_subcommand_from agents' -a '{action}'"
        ));
    }
    for action in update_words() {
        lines.push(format!(
            "complete -c deepseek -n '__fish_seen_subcommand_from update' -a '{action}'"
        ));
    }
    for action in restore_words() {
        lines.push(format!(
            "complete -c deepseek -n '__fish_seen_subcommand_from restore' -a '{action}'"
        ));
    }
    for shell in shell_words() {
        lines.push(format!(
            "complete -c deepseek -n '__fish_seen_subcommand_from completion' -a '{shell}'"
        ));
    }
    format!("{}\n", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::{bash_completion, fish_completion, zsh_completion};

    #[test]
    fn bash_completion_mentions_deepseek_commands() {
        let script = bash_completion();
        assert!(script.contains("complete -F _deepseek deepseek"));
        assert!(script.contains("benchmark"));
        assert!(script.contains("agents"));
        assert!(script.contains("diagnostics"));
        assert!(script.contains("exec"));
        assert!(script.contains("restore"));
        assert!(script.contains("revert-turn"));
        assert!(script.contains("tui"));
        assert!(script.contains("update"));
        assert!(script.contains("verify-install"));
        assert!(script.contains("completion"));
        assert!(script.contains("mcp"));
        assert!(script.contains("tools"));
        assert!(script.contains("prompts"));
        assert!(script.contains("call"));
    }

    #[test]
    fn zsh_completion_mentions_subcommands() {
        let script = zsh_completion();
        assert!(script.contains("#compdef deepseek"));
        assert!(script.contains("dogfood"));
        assert!(script.contains("agents"));
        assert!(script.contains("validate"));
        assert!(script.contains("threads"));
        assert!(script.contains("completion"));
        assert!(script.contains("install-package"));
        assert!(script.contains("restore"));
        assert!(script.contains("snapshot"));
        assert!(script.contains("tools"));
        assert!(script.contains("prompt"));
        assert!(script.contains("call"));
    }

    #[test]
    fn fish_completion_mentions_shell_variants() {
        let script = fish_completion();
        assert!(script.contains("complete -c deepseek"));
        assert!(script.contains("bash"));
        assert!(script.contains("fish"));
        assert!(script.contains("agents"));
        assert!(script.contains("validate"));
        assert!(script.contains("show-thread"));
        assert!(script.contains("rollback"));
        assert!(script.contains("revert-turn"));
        assert!(script.contains("tools"));
        assert!(script.contains("prompts"));
        assert!(script.contains("call"));
    }
}
