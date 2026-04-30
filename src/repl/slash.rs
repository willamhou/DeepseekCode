use crate::error::AppResult;
use crate::repl::repl::{Repl, DEFAULT_BUDGET};
use crate::skills::registry::SkillRegistry;

pub enum SlashOutcome {
    NotASlash,
    Continue,
    Quit,
}

pub fn try_handle_slash(repl: &mut Repl, line: &str) -> AppResult<SlashOutcome> {
    if !line.starts_with('/') {
        return Ok(SlashOutcome::NotASlash);
    }
    let mut tokens = line.split_whitespace();
    let command = tokens.next().unwrap_or("");
    let args: Vec<&str> = tokens.collect();

    match command {
        "/quit" | "/q" | "/exit" => Ok(SlashOutcome::Quit),
        "/help" | "/h" | "/?" => {
            print_help();
            Ok(SlashOutcome::Continue)
        }
        "/clear" => {
            repl.transcript.clear();
            repl.tokens_prompt = 0;
            repl.tokens_completion = 0;
            println!(
                "cleared transcript (kept budget={}, skill={})",
                repl.budget,
                repl.skill.as_deref().unwrap_or("-")
            );
            Ok(SlashOutcome::Continue)
        }
        "/budget" => {
            handle_budget(repl, &args);
            Ok(SlashOutcome::Continue)
        }
        "/skill" => {
            handle_skill(repl, &args);
            Ok(SlashOutcome::Continue)
        }
        "/diff" => {
            handle_diff();
            Ok(SlashOutcome::Continue)
        }
        "/cost" => {
            handle_cost(repl);
            Ok(SlashOutcome::Continue)
        }
        "/save" => {
            handle_save(repl, &args);
            Ok(SlashOutcome::Continue)
        }
        "/load" => {
            handle_load(repl, &args);
            Ok(SlashOutcome::Continue)
        }
        other => {
            println!("unknown slash command `{other}`; type /help for the list");
            Ok(SlashOutcome::Continue)
        }
    }
}

fn print_help() {
    println!("slash commands:");
    println!("  /quit, /q, /exit              exit the REPL");
    println!("  /help, /h, /?                 show this help");
    println!("  /clear                        wipe transcript + token counters");
    println!("  /budget [N]                   show or set per-turn step budget (1..200)");
    println!("  /skill [name|-]               show, switch, or clear the active skill");
    println!("  /diff                         show pending git diff");
    println!("  /save <name>                  save the session to .dscode/sessions/<name>.json");
    println!("  /load <name>                  restore a saved session");
    println!("  /cost                         show prompt/completion token totals");
}

fn handle_budget(repl: &mut Repl, args: &[&str]) {
    if args.is_empty() {
        println!(
            "budget: {} (default {DEFAULT_BUDGET})",
            repl.budget
        );
        return;
    }
    if args.len() > 1 {
        println!("usage: /budget [N]");
        return;
    }
    match args[0].parse::<usize>() {
        Ok(value) if (1..=200).contains(&value) => {
            let prev = repl.budget;
            repl.budget = value;
            println!("budget set to {value} (was {prev})");
        }
        Ok(_) => println!("budget out of range; expected 1..=200"),
        Err(_) => println!("budget must be a positive integer; got `{}`", args[0]),
    }
}

fn handle_skill(repl: &mut Repl, args: &[&str]) {
    if args.is_empty() {
        println!("skill: {}", repl.skill.as_deref().unwrap_or("-"));
        return;
    }
    if args.len() > 1 {
        println!("usage: /skill [name|-]");
        return;
    }
    let target = args[0];
    if target == "-" {
        repl.skill = None;
        println!("skill cleared");
        return;
    }
    let registry = match SkillRegistry::load_dir("skills") {
        Ok(reg) => reg,
        Err(error) => {
            println!("could not load skills: {error}");
            return;
        }
    };
    if registry.find(target).is_some() {
        repl.skill = Some(target.to_string());
        println!("skill switched to {target}");
    } else {
        let names: Vec<&str> = registry.iter().map(|s| s.name.as_str()).collect();
        println!(
            "skill `{target}` not found; known: {}",
            if names.is_empty() {
                "(none)".to_string()
            } else {
                names.join(", ")
            }
        );
    }
}

fn handle_diff() {
    match crate::util::process::run_capture("git", &["diff"]) {
        Ok(captured) => {
            if !captured.success {
                println!("git diff failed: {}", captured.stderr.trim());
                return;
            }
            let body = captured.stdout;
            if body.trim().is_empty() {
                println!("no pending changes");
            } else {
                println!("{body}");
            }
        }
        Err(error) => println!("could not run git diff: {error}"),
    }
}

fn handle_cost(repl: &Repl) {
    if repl.tokens_prompt == 0 && repl.tokens_completion == 0 {
        println!("no remote calls yet");
        return;
    }
    let total = repl.tokens_prompt + repl.tokens_completion;
    println!(
        "prompt: {}, completion: {}, total: {}",
        repl.tokens_prompt, repl.tokens_completion, total
    );
}

fn handle_save(repl: &mut Repl, args: &[&str]) {
    let name = match args {
        [name] => *name,
        _ => {
            println!("usage: /save <name>");
            return;
        }
    };
    match crate::repl::session::save(name, repl) {
        Ok(path) => println!("saved -> {}", path.display()),
        Err(error) => println!("save failed: {error}"),
    }
}

fn handle_load(repl: &mut Repl, args: &[&str]) {
    let name = match args {
        [name] => *name,
        _ => {
            println!("usage: /load <name>");
            return;
        }
    };
    match crate::repl::session::load(name, &repl.config) {
        Ok(loaded) => {
            *repl = loaded;
            println!(
                "loaded {name} (transcript: {} turns, tokens: {} / {})",
                repl.transcript.turns.len(),
                repl.tokens_prompt,
                repl.tokens_completion,
            );
        }
        Err(error) => println!("load failed: {error}"),
    }
}

pub fn validate_session_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("session name cannot be empty".into());
    }
    if name.starts_with('.') {
        return Err("session name cannot start with `.`".into());
    }
    if name.contains('/') || name.contains('\\') {
        return Err("session name cannot contain path separators".into());
    }
    if name.contains("..") {
        return Err("session name cannot contain `..`".into());
    }
    if name.chars().any(|c| c.is_control()) {
        return Err("session name cannot contain control characters".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::AppConfig;

    fn fresh_repl() -> Repl {
        Repl::new(AppConfig::default(), None)
    }

    #[test]
    fn returns_not_a_slash_for_plain_text() {
        let mut r = fresh_repl();
        let out = try_handle_slash(&mut r, "hello").unwrap();
        assert!(matches!(out, SlashOutcome::NotASlash));
    }

    #[test]
    fn quit_returns_quit_outcome() {
        let mut r = fresh_repl();
        assert!(matches!(
            try_handle_slash(&mut r, "/quit").unwrap(),
            SlashOutcome::Quit,
        ));
    }

    #[test]
    fn quit_aliases_q_and_exit_also_return_quit() {
        for alias in ["/q", "/exit"] {
            let mut r = fresh_repl();
            assert!(
                matches!(try_handle_slash(&mut r, alias).unwrap(), SlashOutcome::Quit),
                "alias `{alias}` should map to Quit",
            );
        }
    }

    #[test]
    fn help_prints_and_continues() {
        let mut r = fresh_repl();
        assert!(matches!(
            try_handle_slash(&mut r, "/help").unwrap(),
            SlashOutcome::Continue,
        ));
    }

    #[test]
    fn clear_wipes_transcript_and_keeps_budget_skill() {
        let mut r = fresh_repl();
        r.transcript.push_user("a");
        r.transcript.push_assistant("b");
        r.tokens_prompt = 100;
        r.budget = 30;
        r.skill = Some("x".to_string());
        try_handle_slash(&mut r, "/clear").unwrap();
        assert!(r.transcript.turns.is_empty());
        assert_eq!(r.tokens_prompt, 0);
        assert_eq!(r.budget, 30);
        assert_eq!(r.skill.as_deref(), Some("x"));
    }

    #[test]
    fn budget_with_valid_number_updates_budget() {
        let mut r = fresh_repl();
        try_handle_slash(&mut r, "/budget 30").unwrap();
        assert_eq!(r.budget, 30);
    }

    #[test]
    fn budget_with_zero_does_not_update() {
        let mut r = fresh_repl();
        let before = r.budget;
        try_handle_slash(&mut r, "/budget 0").unwrap();
        assert_eq!(r.budget, before);
    }

    #[test]
    fn budget_with_too_large_does_not_update() {
        let mut r = fresh_repl();
        let before = r.budget;
        try_handle_slash(&mut r, "/budget 999").unwrap();
        assert_eq!(r.budget, before);
    }

    #[test]
    fn skill_dash_clears_active_skill() {
        let mut r = fresh_repl();
        r.skill = Some("x".to_string());
        try_handle_slash(&mut r, "/skill -").unwrap();
        assert!(r.skill.is_none());
    }

    #[test]
    fn unknown_slash_is_handled_gracefully() {
        let mut r = fresh_repl();
        assert!(matches!(
            try_handle_slash(&mut r, "/bogus").unwrap(),
            SlashOutcome::Continue,
        ));
    }

    #[test]
    fn validate_session_name_rejects_dotdot() {
        assert!(validate_session_name("foo..bar").is_err());
    }

    #[test]
    fn validate_session_name_rejects_path_separators() {
        assert!(validate_session_name("a/b").is_err());
        assert!(validate_session_name("a\\b").is_err());
    }

    #[test]
    fn validate_session_name_rejects_leading_dot() {
        assert!(validate_session_name(".hidden").is_err());
    }

    #[test]
    fn validate_session_name_rejects_empty() {
        assert!(validate_session_name("").is_err());
    }

    #[test]
    fn validate_session_name_accepts_normal_name() {
        assert!(validate_session_name("fix-pr-42").is_ok());
        assert!(validate_session_name("session_2026").is_ok());
    }

    #[test]
    fn cost_with_no_calls_returns_continue() {
        let mut r = fresh_repl();
        assert!(matches!(
            try_handle_slash(&mut r, "/cost").unwrap(),
            SlashOutcome::Continue,
        ));
    }

    #[test]
    fn cost_with_accumulated_tokens_returns_continue() {
        let mut r = fresh_repl();
        r.tokens_prompt = 100;
        r.tokens_completion = 50;
        assert!(matches!(
            try_handle_slash(&mut r, "/cost").unwrap(),
            SlashOutcome::Continue,
        ));
    }

    #[test]
    fn diff_returns_continue() {
        let mut r = fresh_repl();
        assert!(matches!(
            try_handle_slash(&mut r, "/diff").unwrap(),
            SlashOutcome::Continue,
        ));
    }
}
