use std::env;

#[derive(Debug)]
pub struct Cli {
    pub command: Option<Command>,
}

#[derive(Debug)]
pub enum PrAction {
    Review {
        reference: String,
        post: bool,
        out: Option<String>,
    },
    Fix {
        reference: String,
        job: Option<String>,
    },
    Patch {
        reference: String,
        commit: bool,
    },
}

pub fn parse_pr_subcommand(args: Vec<String>) -> Result<PrAction, String> {
    let mut iter = args.into_iter();
    let action = iter
        .next()
        .ok_or_else(|| "pr requires a sub-action: review|fix|patch".to_string())?;
    let reference = iter
        .next()
        .ok_or_else(|| format!("pr {action} requires a PR reference"))?;
    let rest: Vec<String> = iter.collect();

    match action.as_str() {
        "review" => {
            let mut post = false;
            let mut out = None;
            let mut index = 0;
            while index < rest.len() {
                match rest[index].as_str() {
                    "--post" => {
                        post = true;
                        index += 1;
                    }
                    "--out" if index + 1 < rest.len() => {
                        out = Some(rest[index + 1].clone());
                        index += 2;
                    }
                    other => {
                        return Err(format!("unknown flag for `pr review`: {other}"));
                    }
                }
            }
            Ok(PrAction::Review {
                reference,
                post,
                out,
            })
        }
        "fix" => {
            let mut job = None;
            let mut index = 0;
            while index < rest.len() {
                match rest[index].as_str() {
                    "--job" if index + 1 < rest.len() => {
                        job = Some(rest[index + 1].clone());
                        index += 2;
                    }
                    other => {
                        return Err(format!("unknown flag for `pr fix`: {other}"));
                    }
                }
            }
            Ok(PrAction::Fix { reference, job })
        }
        "patch" => {
            let mut commit = false;
            let mut index = 0;
            while index < rest.len() {
                match rest[index].as_str() {
                    "--commit" => {
                        commit = true;
                        index += 1;
                    }
                    other => {
                        return Err(format!("unknown flag for `pr patch`: {other}"));
                    }
                }
            }
            Ok(PrAction::Patch { reference, commit })
        }
        other => Err(format!(
            "unknown pr sub-action `{other}`; expected review|fix|patch"
        )),
    }
}

impl Cli {
    pub fn parse() -> Result<Self, String> {
        let argv = env::args().skip(1).collect::<Vec<_>>();
        Self::from_argv(argv)
    }

    pub fn from_argv(mut args: Vec<String>) -> Result<Self, String> {
        if args.is_empty() {
            return Ok(Self {
                command: Some(Command::Chat(ChatArgs::default())),
            });
        }

        let first = args.remove(0);
        let command = match first.as_str() {
            "run" => {
                let (skill, positional) = parse_common_flags(args);
                let task = positional
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "Run task".to_string());
                Command::Run(RunArgs { task, skill })
            }
            "diff" => Command::Diff(DiffArgs {}),
            "resume" => Command::Resume(ResumeArgs { session: None }),
            "config" => Command::Config(ConfigArgs {
                print_default: args.iter().any(|arg| arg == "--print-default"),
            }),
            "doctor" => Command::Doctor(DoctorArgs {}),
            "smoke" => Command::Smoke(parse_smoke_args(args)),
            "pr" => Command::Pr(parse_pr_subcommand(args)?),
            _ => {
                let mut combined = vec![first];
                combined.extend(args);
                let (skill, positional) = parse_common_flags(combined);
                let task = positional.join(" ");
                let task = if task.is_empty() { None } else { Some(task) };
                Command::Chat(ChatArgs { task, skill })
            }
        };

        Ok(Self {
            command: Some(command),
        })
    }
}

#[derive(Debug)]
pub enum Command {
    Chat(ChatArgs),
    Run(RunArgs),
    Diff(DiffArgs),
    Resume(ResumeArgs),
    Config(ConfigArgs),
    Doctor(DoctorArgs),
    Smoke(SmokeArgs),
    Pr(PrAction),
}

impl Default for Command {
    fn default() -> Self {
        Self::Chat(ChatArgs::default())
    }
}

#[derive(Debug, Default)]
pub struct ChatArgs {
    pub task: Option<String>,
    pub skill: Option<String>,
}

#[derive(Debug)]
pub struct RunArgs {
    pub task: String,
    pub skill: Option<String>,
}

#[derive(Debug)]
pub struct DiffArgs {}

#[derive(Debug)]
pub struct ResumeArgs {
    pub session: Option<String>,
}

#[derive(Debug)]
pub struct ConfigArgs {
    pub print_default: bool,
}

#[derive(Debug)]
pub struct DoctorArgs {}

#[derive(Debug, Default)]
pub struct SmokeArgs {
    pub flavor: Option<SmokeFlavor>,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum SmokeFlavor {
    OpenAi,
    Anthropic,
}

fn parse_smoke_args(args: Vec<String>) -> SmokeArgs {
    let mut smoke = SmokeArgs::default();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--flavor" => {
                if index + 1 < args.len() {
                    smoke.flavor = match args[index + 1].as_str() {
                        "openai" | "openai-compatible" => Some(SmokeFlavor::OpenAi),
                        "anthropic" | "anthropic-compatible" => Some(SmokeFlavor::Anthropic),
                        _ => smoke.flavor,
                    };
                    index += 2;
                    continue;
                }
            }
            "--prompt" => {
                if index + 1 < args.len() {
                    smoke.prompt = Some(args[index + 1].clone());
                    index += 2;
                    continue;
                }
            }
            _ => {}
        }
        index += 1;
    }

    smoke
}

fn parse_common_flags(args: Vec<String>) -> (Option<String>, Vec<String>) {
    let mut skill = None;
    let mut positional = Vec::new();
    let mut index = 0;

    while index < args.len() {
        if args[index] == "--skill" {
            if index + 1 < args.len() {
                skill = Some(args[index + 1].clone());
                index += 2;
                continue;
            }
        }

        positional.push(args[index].clone());
        index += 1;
    }

    (skill, positional)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pr_review_with_post_flag() {
        let args = vec!["review".to_string(), "42".to_string(), "--post".to_string()];
        let parsed = parse_pr_subcommand(args).unwrap();
        assert!(matches!(
            parsed,
            PrAction::Review {
                ref reference,
                post: true,
                out: None,
            } if reference == "42"
        ));
    }

    #[test]
    fn parses_pr_fix_with_job_flag() {
        let args = vec![
            "fix".to_string(),
            "owner/repo#7".to_string(),
            "--job".to_string(),
            "test-rust".to_string(),
        ];
        let parsed = parse_pr_subcommand(args).unwrap();
        match parsed {
            PrAction::Fix { reference, job } => {
                assert_eq!(reference, "owner/repo#7");
                assert_eq!(job.as_deref(), Some("test-rust"));
            }
            _ => panic!("expected fix"),
        }
    }

    #[test]
    fn parses_pr_patch_with_commit_flag() {
        let args = vec!["patch".to_string(), "5".to_string(), "--commit".to_string()];
        let parsed = parse_pr_subcommand(args).unwrap();
        assert!(matches!(
            parsed,
            PrAction::Patch {
                commit: true,
                ref reference,
            } if reference == "5"
        ));
    }

    #[test]
    fn rejects_unknown_pr_subaction() {
        let args = vec!["delete".to_string(), "5".to_string()];
        assert!(parse_pr_subcommand(args).is_err());
    }

    #[test]
    fn cli_from_argv_routes_pr_subcommand_to_command_pr() {
        let argv = vec![
            "pr".to_string(),
            "review".to_string(),
            "42".to_string(),
            "--post".to_string(),
        ];
        let cli = Cli::from_argv(argv).expect("parse should succeed");
        match cli.command {
            Some(Command::Pr(PrAction::Review { reference, post, out: _ })) => {
                assert_eq!(reference, "42");
                assert!(post);
            }
            other => panic!("expected Command::Pr(Review), got {:?}", other),
        }
    }

    #[test]
    fn cli_from_argv_propagates_pr_parse_error() {
        let argv = vec!["pr".to_string(), "delete".to_string(), "5".to_string()];
        let result = Cli::from_argv(argv);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown pr sub-action"));
    }

    #[test]
    fn cli_from_argv_falls_back_to_chat_for_unknown_first_arg() {
        let argv = vec!["explore".to_string(), "the".to_string(), "repo".to_string()];
        let cli = Cli::from_argv(argv).expect("parse should succeed");
        match cli.command {
            Some(Command::Chat(args)) => {
                assert_eq!(args.task.as_deref(), Some("explore the repo"));
            }
            other => panic!("expected Command::Chat, got {:?}", other),
        }
    }
}
