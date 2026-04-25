use std::env;

#[derive(Debug)]
pub struct Cli {
    pub command: Option<Command>,
}

impl Cli {
    pub fn parse() -> Self {
        let mut args = env::args().skip(1).collect::<Vec<_>>();
        if args.is_empty() {
            return Self {
                command: Some(Command::Chat(ChatArgs::default())),
            };
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
            _ => {
                let mut combined = vec![first];
                combined.extend(args);
                let (skill, positional) = parse_common_flags(combined);
                let task = positional.join(" ");
                let task = if task.is_empty() { None } else { Some(task) };
                Command::Chat(ChatArgs { task, skill })
            }
        };

        Self {
            command: Some(command),
        }
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
