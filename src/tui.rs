use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{fs, io};

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{CrosstermBackend, TestBackend},
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs, Wrap},
    Frame, Terminal,
};

use crate::core::runtime::{
    AutomationRecord, ItemRecord, RuntimeEvent, SessionRecord, TaskRecord, ThreadRecord,
    UsageRecord,
};
use crate::error::AppResult;
use crate::tools::run_shell::is_safe_shell_command;
use crate::util::json::{
    json_as_array, json_as_object, json_as_string, json_as_u64, json_value_to_string,
    parse_json_value, parse_root_object, JsonValue,
};

const USER_INPUT_OTHER_MAX_CHARS: usize = 200;
const DEEPSEEK_CODE_REPO_URL: &str = "https://github.com/willamhou/DeepSeekCode";
const DEEPSEEK_CODE_BUG_URL: &str =
    "https://github.com/willamhou/DeepSeekCode/issues/new?labels=bug";
const DEEPSEEK_CODE_FEATURE_URL: &str =
    "https://github.com/willamhou/DeepSeekCode/issues/new?labels=enhancement";
const DEEPSEEK_CODE_SECURITY_URL: &str =
    "https://github.com/willamhou/DeepSeekCode/security/policy";
const DEEPSEEK_CODE_ISSUES_URL: &str = "https://github.com/willamhou/DeepSeekCode/issues";
const DEEPSEEK_CODE_RELEASES_URL: &str = "https://github.com/willamhou/DeepSeekCode/releases";
const DEEPSEEK_CODE_DOCS_URL: &str = "https://github.com/willamhou/DeepSeekCode/tree/main/docs";
const DEEPSEEK_PLATFORM_URL: &str = "https://platform.deepseek.com";
const DEEPSEEK_API_DOCS_URL: &str = "https://api-docs.deepseek.com";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiMode {
    Plan,
    Agent,
    Yolo,
}

impl TuiMode {
    fn title(self) -> &'static str {
        match self {
            Self::Plan => "Plan",
            Self::Agent => "Agent",
            Self::Yolo => "YOLO",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Plan => Self::Agent,
            Self::Agent => Self::Yolo,
            Self::Yolo => Self::Plan,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Plan => 0,
            Self::Agent => 1,
            Self::Yolo => 2,
        }
    }

    fn from_command_arg(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "plan" | "2" => Some(Self::Plan),
            "agent" | "1" => Some(Self::Agent),
            "yolo" | "3" => Some(Self::Yolo),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiTheme {
    Dark,
    Light,
    Grayscale,
    System,
}

impl TuiTheme {
    fn title(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::Grayscale => "Grayscale",
            Self::System => "System",
        }
    }

    fn command_name(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::Grayscale => "grayscale",
            Self::System => "system",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Grayscale,
            Self::Grayscale => Self::System,
            Self::System => Self::Dark,
        }
    }

    fn from_command_arg(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            "grayscale" | "gray" | "grey" => Some(Self::Grayscale),
            "system" => Some(Self::System),
            _ => None,
        }
    }

    fn accent_color(self) -> Color {
        match self {
            Self::Dark => Color::Cyan,
            Self::Light => Color::Blue,
            Self::Grayscale => Color::White,
            Self::System => Color::Green,
        }
    }

    fn hint_color(self) -> Color {
        match self {
            Self::Dark => Color::Yellow,
            Self::Light => Color::Blue,
            Self::Grayscale => Color::Gray,
            Self::System => Color::Yellow,
        }
    }

    fn label_color(self) -> Color {
        match self {
            Self::Light => Color::Black,
            Self::Grayscale => Color::Gray,
            _ => Color::Gray,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiSession {
    pub id: String,
    pub title: String,
    pub workspace: String,
    pub status: String,
    pub active_thread_id: Option<String>,
    pub thread_count: u64,
}

impl From<SessionRecord> for TuiSession {
    fn from(session: SessionRecord) -> Self {
        Self {
            id: session.id,
            title: session.title,
            workspace: session.workspace,
            status: session.status,
            active_thread_id: session.active_thread_id,
            thread_count: session.thread_count,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiThread {
    pub id: String,
    pub session_id: Option<String>,
    pub title: String,
    pub mode: String,
    pub status: String,
    pub latest_turn_id: Option<String>,
    pub event_seq: u64,
}

impl From<ThreadRecord> for TuiThread {
    fn from(thread: ThreadRecord) -> Self {
        Self {
            id: thread.id,
            session_id: thread.session_id,
            title: thread.title,
            mode: thread.mode,
            status: thread.status,
            latest_turn_id: thread.latest_turn_id,
            event_seq: thread.event_seq,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiItem {
    pub id: String,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub index: u64,
    pub item_type: String,
    pub role: Option<String>,
    pub content: String,
    pub status: String,
}

impl From<ItemRecord> for TuiItem {
    fn from(item: ItemRecord) -> Self {
        Self {
            id: item.id,
            thread_id: item.thread_id,
            turn_id: item.turn_id,
            index: item.index,
            item_type: item.item_type,
            role: item.role,
            content: item.content,
            status: item.status,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiLiveEvent {
    UpsertItem(TuiItem),
    ReplaceRuntime {
        sessions: Vec<TuiSession>,
        threads: Vec<TuiThread>,
        items: Vec<TuiItem>,
        tasks: Vec<TuiTaskRecord>,
        automations: Vec<TuiAutomationRecord>,
        usage_summaries: Vec<TuiUsageSummary>,
        approvals: Vec<TuiApprovalRequest>,
        user_inputs: Vec<TuiUserInputRequest>,
    },
    Status(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiQueuedMessage {
    pub thread_id: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiTaskRecord {
    pub id: String,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub parent_task_id: Option<String>,
    pub kind: String,
    pub status: String,
    pub summary: String,
    pub updated_at: String,
}

impl From<TaskRecord> for TuiTaskRecord {
    fn from(task: TaskRecord) -> Self {
        Self {
            id: task.id,
            session_id: task.session_id,
            thread_id: task.thread_id,
            parent_task_id: task.parent_task_id,
            kind: task.kind,
            status: task.status,
            summary: task.summary,
            updated_at: task.updated_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiAutomationRecord {
    pub id: String,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub name: String,
    pub status: String,
    pub schedule: String,
    pub prompt: String,
    pub updated_at: String,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
}

impl From<AutomationRecord> for TuiAutomationRecord {
    fn from(automation: AutomationRecord) -> Self {
        Self {
            id: automation.id,
            session_id: automation.session_id,
            thread_id: automation.thread_id,
            name: automation.name,
            status: automation.status,
            schedule: automation.schedule,
            prompt: automation.prompt,
            updated_at: automation.updated_at,
            last_run_at: automation.last_run_at,
            next_run_at: automation.next_run_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiApprovalRequest {
    pub id: String,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub tool: String,
    pub kind: String,
    pub target: String,
    pub status: String,
}

impl TuiApprovalRequest {
    pub fn from_runtime_event(event: &RuntimeEvent) -> Option<Self> {
        if event.kind != "permission_request" {
            return None;
        }
        let payload = json_as_object(&event.payload)?;
        Some(Self {
            id: event.id.clone(),
            thread_id: event.thread_id.clone(),
            turn_id: event.turn_id.clone(),
            tool: payload_string(payload, "tool", "unknown"),
            kind: payload_string(payload, "kind", "permission"),
            target: payload_string(payload, "target", ""),
            status: payload_string(payload, "status", "pending"),
        })
    }

    pub fn response_request_id(event: &RuntimeEvent) -> Option<String> {
        if event.kind != "permission_response" {
            return None;
        }
        let payload = json_as_object(&event.payload)?;
        payload
            .get("request_id")
            .and_then(json_as_string)
            .map(str::to_string)
    }

    fn is_pending(&self) -> bool {
        self.status == "pending"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiUserInputRequest {
    pub id: String,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub status: String,
    pub questions: Vec<TuiUserInputQuestion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiUserInputQuestion {
    pub header: String,
    pub id: String,
    pub question: String,
    pub options: Vec<TuiUserInputOption>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiUserInputOption {
    pub label: String,
    pub description: String,
}

impl TuiUserInputRequest {
    pub fn from_runtime_event(event: &RuntimeEvent) -> Option<Self> {
        if event.kind != "user_input_request" {
            return None;
        }
        let payload = json_as_object(&event.payload)?;
        let questions = payload.get("questions").and_then(json_as_array)?;
        let questions = questions
            .iter()
            .filter_map(TuiUserInputQuestion::from_json)
            .collect::<Vec<_>>();
        if questions.is_empty() {
            return None;
        }
        Some(Self {
            id: event.id.clone(),
            thread_id: event.thread_id.clone(),
            turn_id: event.turn_id.clone(),
            status: payload_string(payload, "status", "pending"),
            questions,
        })
    }

    pub fn response_request_id(event: &RuntimeEvent) -> Option<String> {
        if event.kind != "user_input_response" {
            return None;
        }
        let payload = json_as_object(&event.payload)?;
        payload
            .get("request_id")
            .and_then(json_as_string)
            .map(str::to_string)
    }

    fn is_pending(&self) -> bool {
        self.status == "pending"
    }
}

impl TuiUserInputQuestion {
    fn from_json(value: &JsonValue) -> Option<Self> {
        let object = json_as_object(value)?;
        let options = object
            .get("options")
            .and_then(json_as_array)?
            .iter()
            .filter_map(TuiUserInputOption::from_json)
            .collect::<Vec<_>>();
        if options.is_empty() {
            return None;
        }
        Some(Self {
            header: payload_string(object, "header", ""),
            id: payload_string(object, "id", ""),
            question: payload_string(object, "question", ""),
            options,
        })
    }
}

impl TuiUserInputOption {
    fn from_json(value: &JsonValue) -> Option<Self> {
        let object = json_as_object(value)?;
        Some(Self {
            label: payload_string(object, "label", ""),
            description: payload_string(object, "description", ""),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiUsageSummary {
    pub thread_id: String,
    pub record_count: usize,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub latest_prompt_tokens: u64,
    pub latest_completion_tokens: u64,
    pub latest_total_tokens: u64,
    pub prompt_cache_hit_tokens: u64,
    pub prompt_cache_miss_tokens: u64,
    pub estimated_input_cost_microusd: Option<u64>,
    pub estimated_output_cost_microusd: Option<u64>,
    pub estimated_total_cost_microusd: Option<u64>,
    pub context_remaining_tokens: u64,
    pub context_strategy: String,
}

impl TuiUsageSummary {
    pub fn from_usage_records(thread_id: &str, records: &[UsageRecord]) -> Self {
        let mut prompt_tokens = 0_u64;
        let mut completion_tokens = 0_u64;
        let mut total_tokens = 0_u64;
        let mut prompt_cache_hit_tokens = 0_u64;
        let mut prompt_cache_miss_tokens = 0_u64;
        let mut estimated_input_cost_microusd = Some(0_u64);
        let mut estimated_output_cost_microusd = Some(0_u64);
        let mut estimated_total_cost_microusd = Some(0_u64);
        for record in records {
            prompt_tokens = prompt_tokens.saturating_add(record.prompt_tokens);
            completion_tokens = completion_tokens.saturating_add(record.completion_tokens);
            total_tokens = total_tokens.saturating_add(record.total_tokens);
            prompt_cache_hit_tokens =
                prompt_cache_hit_tokens.saturating_add(record.prompt_cache_hit_tokens);
            prompt_cache_miss_tokens =
                prompt_cache_miss_tokens.saturating_add(record.prompt_cache_miss_tokens);
            match (
                estimated_input_cost_microusd,
                record.estimated_input_cost_microusd,
            ) {
                (Some(total), Some(next)) => {
                    estimated_input_cost_microusd = Some(total.saturating_add(next));
                }
                _ => estimated_input_cost_microusd = None,
            }
            match (
                estimated_output_cost_microusd,
                record.estimated_output_cost_microusd,
            ) {
                (Some(total), Some(next)) => {
                    estimated_output_cost_microusd = Some(total.saturating_add(next));
                }
                _ => estimated_output_cost_microusd = None,
            }
            match (
                estimated_total_cost_microusd,
                record.estimated_total_cost_microusd,
            ) {
                (Some(total), Some(next)) => {
                    estimated_total_cost_microusd = Some(total.saturating_add(next));
                }
                _ => estimated_total_cost_microusd = None,
            }
        }
        let latest_prompt_tokens = records
            .first()
            .map(|record| record.prompt_tokens)
            .unwrap_or(0);
        let latest_completion_tokens = records
            .first()
            .map(|record| record.completion_tokens)
            .unwrap_or(0);
        let latest_total_tokens = records
            .first()
            .map(|record| record.total_tokens)
            .unwrap_or(0);
        let context_remaining_tokens =
            TUI_CONTEXT_WINDOW_TOKENS.saturating_sub(latest_total_tokens);
        Self {
            thread_id: thread_id.to_string(),
            record_count: records.len(),
            prompt_tokens,
            completion_tokens,
            total_tokens,
            latest_prompt_tokens,
            latest_completion_tokens,
            latest_total_tokens,
            prompt_cache_hit_tokens,
            prompt_cache_miss_tokens,
            estimated_input_cost_microusd,
            estimated_output_cost_microusd,
            estimated_total_cost_microusd,
            context_remaining_tokens,
            context_strategy: context_strategy(latest_total_tokens).to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiMcpDetailKind {
    Manager,
    Tools,
    Prompts,
    Resources,
    ResourceTemplates,
    Health,
    Shell,
    Memory,
    Network,
    Lsp,
    Change,
    System,
    Edit,
    Status,
    Tokens,
    Cost,
    Cache,
    Diff,
    Clear,
    Model,
    Provider,
    Skills,
    Feedback,
    Links,
    Home,
    Note,
    Subagents,
    Rlm,
    Relay,
    Hooks,
    Goal,
    Anchor,
    Queue,
    Share,
    Export,
    Save,
    Load,
    Attach,
    Mode,
    Help,
    Settings,
    Theme,
    StatusLine,
    Verbose,
    Context,
    Rollback,
    Reasoning,
    ComposerStash,
}

impl TuiMcpDetailKind {
    pub(crate) fn command_name(&self) -> &'static str {
        match self {
            Self::Manager => "manager",
            Self::Tools => "tools",
            Self::Prompts => "prompts",
            Self::Resources => "resources",
            Self::ResourceTemplates => "resource-templates",
            Self::Health => "health",
            Self::Shell => "shell",
            Self::Memory => "memory",
            Self::Network => "network",
            Self::Lsp => "lsp",
            Self::Change => "change",
            Self::System => "system",
            Self::Edit => "edit",
            Self::Status => "status",
            Self::Tokens => "tokens",
            Self::Cost => "cost",
            Self::Cache => "cache",
            Self::Diff => "diff",
            Self::Clear => "clear",
            Self::Model => "model",
            Self::Provider => "provider",
            Self::Skills => "skills",
            Self::Feedback => "feedback",
            Self::Links => "links",
            Self::Home => "home",
            Self::Note => "note",
            Self::Subagents => "subagents",
            Self::Rlm => "rlm",
            Self::Relay => "relay",
            Self::Hooks => "hooks",
            Self::Goal => "goal",
            Self::Anchor => "anchor",
            Self::Queue => "queue",
            Self::Share => "share",
            Self::Export => "export",
            Self::Save => "save",
            Self::Load => "load",
            Self::Attach => "attach",
            Self::Mode => "mode",
            Self::Help => "help",
            Self::Settings => "settings",
            Self::Theme => "theme",
            Self::StatusLine => "statusline",
            Self::Verbose => "verbose",
            Self::Context => "context",
            Self::Rollback => "rollback",
            Self::Reasoning => "reasoning",
            Self::ComposerStash => "stash",
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::Manager => "MCP Manager",
            Self::Tools => "MCP Tools",
            Self::Prompts => "MCP Prompts",
            Self::Resources => "MCP Resources",
            Self::ResourceTemplates => "MCP Resource Templates",
            Self::Health => "MCP Health",
            Self::Shell => "Shell Jobs",
            Self::Memory => "Memory",
            Self::Network => "Network",
            Self::Lsp => "LSP",
            Self::Change => "Changelog",
            Self::System => "System Prompt",
            Self::Edit => "Edit",
            Self::Status => "Status",
            Self::Tokens => "Tokens",
            Self::Cost => "Cost",
            Self::Cache => "Cache",
            Self::Diff => "Diff",
            Self::Clear => "Clear",
            Self::Model => "Model",
            Self::Provider => "Provider",
            Self::Skills => "Skills",
            Self::Feedback => "Feedback",
            Self::Links => "Links",
            Self::Home => "Home",
            Self::Note => "Note",
            Self::Subagents => "Subagents",
            Self::Rlm => "RLM",
            Self::Relay => "Relay",
            Self::Hooks => "Hooks",
            Self::Goal => "Goal",
            Self::Anchor => "Anchor",
            Self::Queue => "Queue",
            Self::Share => "Share",
            Self::Export => "Export",
            Self::Save => "Save",
            Self::Load => "Load",
            Self::Attach => "Attach",
            Self::Mode => "Mode",
            Self::Help => "Help",
            Self::Settings => "Settings",
            Self::Theme => "Theme",
            Self::StatusLine => "Statusline",
            Self::Verbose => "Verbose Transcript",
            Self::Context => "Context",
            Self::Rollback => "Rollback",
            Self::Reasoning => "Reasoning",
            Self::ComposerStash => "Composer Stash",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Manager => Self::Tools,
            Self::Tools => Self::Prompts,
            Self::Prompts => Self::Resources,
            Self::Resources => Self::ResourceTemplates,
            Self::ResourceTemplates => Self::Health,
            Self::Health => Self::Manager,
            Self::Shell => Self::Manager,
            Self::Memory => Self::Manager,
            Self::Network => Self::Manager,
            Self::Lsp => Self::Manager,
            Self::Change => Self::Manager,
            Self::System => Self::Manager,
            Self::Edit => Self::Manager,
            Self::Status => Self::Manager,
            Self::Tokens => Self::Manager,
            Self::Cost => Self::Manager,
            Self::Cache => Self::Manager,
            Self::Diff => Self::Manager,
            Self::Clear => Self::Manager,
            Self::Model => Self::Manager,
            Self::Provider => Self::Manager,
            Self::Skills => Self::Manager,
            Self::Feedback => Self::Manager,
            Self::Links => Self::Manager,
            Self::Home => Self::Manager,
            Self::Note => Self::Manager,
            Self::Subagents => Self::Manager,
            Self::Rlm => Self::Manager,
            Self::Relay => Self::Manager,
            Self::Hooks => Self::Manager,
            Self::Goal => Self::Manager,
            Self::Anchor => Self::Manager,
            Self::Queue => Self::Manager,
            Self::Export => Self::Manager,
            Self::Share => Self::Manager,
            Self::Save => Self::Manager,
            Self::Load => Self::Manager,
            Self::Attach => Self::Manager,
            Self::Mode => Self::Manager,
            Self::Help => Self::Manager,
            Self::Settings => Self::Manager,
            Self::Theme => Self::Manager,
            Self::StatusLine => Self::Manager,
            Self::Verbose => Self::Manager,
            Self::Context => Self::Manager,
            Self::Rollback => Self::Manager,
            Self::Reasoning => Self::Manager,
            Self::ComposerStash => Self::Manager,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Manager => Self::Health,
            Self::Tools => Self::Manager,
            Self::Prompts => Self::Tools,
            Self::Resources => Self::Prompts,
            Self::ResourceTemplates => Self::Resources,
            Self::Health => Self::ResourceTemplates,
            Self::Shell => Self::Manager,
            Self::Memory => Self::Manager,
            Self::Network => Self::Manager,
            Self::Lsp => Self::Manager,
            Self::Change => Self::Manager,
            Self::System => Self::Manager,
            Self::Edit => Self::Manager,
            Self::Status => Self::Manager,
            Self::Tokens => Self::Manager,
            Self::Cost => Self::Manager,
            Self::Cache => Self::Manager,
            Self::Diff => Self::Manager,
            Self::Clear => Self::Manager,
            Self::Model => Self::Manager,
            Self::Provider => Self::Manager,
            Self::Skills => Self::Manager,
            Self::Feedback => Self::Manager,
            Self::Links => Self::Manager,
            Self::Home => Self::Manager,
            Self::Note => Self::Manager,
            Self::Subagents => Self::Manager,
            Self::Rlm => Self::Manager,
            Self::Relay => Self::Manager,
            Self::Hooks => Self::Manager,
            Self::Goal => Self::Manager,
            Self::Anchor => Self::Manager,
            Self::Queue => Self::Manager,
            Self::Export => Self::Manager,
            Self::Share => Self::Manager,
            Self::Save => Self::Manager,
            Self::Load => Self::Manager,
            Self::Attach => Self::Manager,
            Self::Mode => Self::Manager,
            Self::Help => Self::Manager,
            Self::Settings => Self::Manager,
            Self::Theme => Self::Manager,
            Self::StatusLine => Self::Manager,
            Self::Verbose => Self::Manager,
            Self::Context => Self::Manager,
            Self::Rollback => Self::Manager,
            Self::Reasoning => Self::Manager,
            Self::ComposerStash => Self::Manager,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiMemoryCommand {
    Show,
    Path,
    Clear,
    Edit,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiMcpConfigScope {
    Project,
    User,
}

impl TuiMcpConfigScope {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "user",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TuiMcpServerEntry {
    name: String,
    source: String,
    enabled: bool,
}

impl TuiMcpServerEntry {
    fn scope(&self) -> Option<TuiMcpConfigScope> {
        parse_tui_mcp_scope(&self.source)
    }

    fn selection_key(&self) -> String {
        format!("{}:{}", self.source, self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TuiMcpPendingRemove {
    scope: TuiMcpConfigScope,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TuiRollbackPendingApply {
    id: String,
    hunk: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TuiMcpManagerMouseAction {
    Enable,
    Disable,
    Remove,
    Tools,
    Reload,
}

fn parse_tui_mcp_scope(value: &str) -> Option<TuiMcpConfigScope> {
    match value {
        "project" => Some(TuiMcpConfigScope::Project),
        "user" => Some(TuiMcpConfigScope::User),
        _ => None,
    }
}

fn composer_memory_note(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if trimmed.starts_with('#') && !trimmed.starts_with("##") && !trimmed.starts_with("#!") {
        Some(trimmed.to_string())
    } else {
        None
    }
}

fn parse_memory_command_name(value: &str) -> Option<TuiMemoryCommand> {
    match value {
        "" | "show" => Some(TuiMemoryCommand::Show),
        "path" => Some(TuiMemoryCommand::Path),
        "clear" => Some(TuiMemoryCommand::Clear),
        "edit" => Some(TuiMemoryCommand::Edit),
        "help" => Some(TuiMemoryCommand::Help),
        _ => None,
    }
}

fn composer_memory_command(content: &str) -> Option<Result<TuiMemoryCommand, String>> {
    let trimmed = content.trim();
    let rest = trimmed.strip_prefix("/memory")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] => Some(Ok(TuiMemoryCommand::Show)),
        [name] => Some(
            parse_memory_command_name(name)
                .ok_or_else(|| "usage: /memory [show|path|clear|edit|help]".to_string()),
        ),
        _ => Some(Err("usage: /memory [show|path|clear|edit|help]".to_string())),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ComposerStashEntry {
    created_at: String,
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TuiComposerStashCommand {
    List,
    Pop,
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiNetworkCommand {
    List,
    Allow { host: String },
    Deny { host: String },
    Remove { host: String },
    Default { value: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiLspCommand {
    Status,
    Set { enabled: bool },
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiModelCommand {
    Show,
    List,
    Set { model: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiModeCommand {
    Show,
    Set(TuiMode),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiHelpCommand {
    Show,
    Topic(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiThemeCommand {
    Show,
    Cycle,
    Set(TuiTheme),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiNoteCommand {
    Add { content: String },
    List,
    Show { index: usize },
    Edit { index: usize, content: String },
    Remove { index: usize },
    Clear,
    Path,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiHooksCommand {
    List,
    Events,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiGoalCommand {
    Show,
    Set {
        objective: String,
        token_budget: Option<u64>,
    },
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiAnchorCommand {
    Add { content: String },
    List,
    Remove { index: usize },
    Path,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiQueueCommand {
    List,
    Edit { index: usize },
    Drop { index: usize },
    Clear,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiShareCommand {
    Export,
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiExportCommand {
    Export { path: Option<String> },
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiSaveCommand {
    Save { path: Option<String> },
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiLoadCommand {
    Load { path: String },
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiAttachCommand {
    Attach { path: String },
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiChangeCommand {
    Show,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiSystemCommand {
    Show,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiEditCommand {
    LoadLast,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiClearCommand {
    Clear,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiDiffCommand {
    Show,
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiSubagentsCommand {
    List,
    Spawn { max_depth: usize, task: String },
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiRlmCommand {
    Start { max_depth: usize, target: String },
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiRelayCommand {
    Create { focus: Option<String> },
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiVerboseCommand {
    Show,
    Toggle,
    Set(bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiProviderCommand {
    Show,
    List,
    Set {
        provider: String,
        model: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiSkillsCommand {
    List { prefix: Option<String> },
    Show { name: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiFeedbackCommand {
    Show,
    Bug,
    Feature,
    Security,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TuiCacheCommand {
    Summary { count: Option<usize> },
    Inspect,
    Warmup,
}

fn parse_tui_stash_command(line: &str) -> Option<Result<TuiComposerStashCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/stash")
        .or_else(|| strip_tui_command_prefix(trimmed, "stash"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/park"))
        .or_else(|| strip_tui_command_prefix(trimmed, "park"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["list"] | ["ls"] | ["show"] => Some(Ok(TuiComposerStashCommand::List)),
        ["pop"] | ["restore"] => Some(Ok(TuiComposerStashCommand::Pop)),
        ["clear"] | ["wipe"] | ["drop"] => Some(Ok(TuiComposerStashCommand::Clear)),
        _ => Some(Err(
            "usage: stash [list|pop|clear] or /stash [list|pop|clear]".to_string(),
        )),
    }
}

fn parse_tui_help_command(line: &str) -> Option<Result<TuiHelpCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/help")
        .or_else(|| strip_tui_command_prefix(trimmed, "help"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/?"))
        .or_else(|| strip_tui_command_prefix(trimmed, "?"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] => Some(Ok(TuiHelpCommand::Show)),
        [topic] if !topic.starts_with('-') => Some(Ok(TuiHelpCommand::Topic(
            topic.trim_start_matches('/').to_string(),
        ))),
        _ => Some(Err(
            "usage: help [command], /help [command], or /?".to_string()
        )),
    }
}

fn parse_tui_network_command(line: &str) -> Option<Result<TuiNetworkCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/network")
        .or_else(|| strip_tui_command_prefix(trimmed, "network"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["list"] | ["ls"] | ["show"] => Some(Ok(TuiNetworkCommand::List)),
        ["allow", host] => Some(Ok(TuiNetworkCommand::Allow {
            host: (*host).to_string(),
        })),
        ["deny", host] | ["block", host] => Some(Ok(TuiNetworkCommand::Deny {
            host: (*host).to_string(),
        })),
        ["remove", host] | ["forget", host] => Some(Ok(TuiNetworkCommand::Remove {
            host: (*host).to_string(),
        })),
        ["default", value] => Some(Ok(TuiNetworkCommand::Default {
            value: (*value).to_string(),
        })),
        _ => Some(Err(
            "usage: network [list|allow <host>|deny <host>|remove <host>|default <allow|deny|prompt>]"
                .to_string(),
        )),
    }
}

fn parse_tui_lsp_command(line: &str) -> Option<Result<TuiLspCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/lsp")
        .or_else(|| strip_tui_command_prefix(trimmed, "lsp"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["status"] | ["show"] => Some(Ok(TuiLspCommand::Status)),
        ["help" | "--help" | "-h"] => Some(Ok(TuiLspCommand::Help)),
        ["on" | "enable" | "enabled" | "true" | "1"] => {
            Some(Ok(TuiLspCommand::Set { enabled: true }))
        }
        ["off" | "disable" | "disabled" | "false" | "0"] => {
            Some(Ok(TuiLspCommand::Set { enabled: false }))
        }
        _ => Some(Err(
            "usage: lsp [on|off|status] or /lsp [on|off|status]".to_string()
        )),
    }
}

fn parse_tui_status_command(line: &str) -> Option<Result<(), String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/status")
        .or_else(|| strip_tui_command_prefix(trimmed, "status"))?;
    if rest.trim().is_empty() {
        Some(Ok(()))
    } else {
        Some(Err("usage: status or /status".to_string()))
    }
}

fn parse_tui_statusline_command(line: &str) -> Option<Result<(), String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/statusline")
        .or_else(|| strip_tui_command_prefix(trimmed, "statusline"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/status-line"))
        .or_else(|| strip_tui_command_prefix(trimmed, "status-line"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["show" | "help" | "--help" | "-h"] => Some(Ok(())),
        _ => Some(Err(
            "usage: statusline, statusline show, /statusline, or /statusline show".to_string(),
        )),
    }
}

fn parse_tui_verbose_command(line: &str) -> Option<Result<TuiVerboseCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/verbose")
        .or_else(|| strip_tui_command_prefix(trimmed, "verbose"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["toggle"] => Some(Ok(TuiVerboseCommand::Toggle)),
        ["show" | "status" | "help" | "--help" | "-h"] => Some(Ok(TuiVerboseCommand::Show)),
        ["on" | "true" | "1" | "yes"] => Some(Ok(TuiVerboseCommand::Set(true))),
        ["off" | "false" | "0" | "no"] => Some(Ok(TuiVerboseCommand::Set(false))),
        _ => Some(Err(
            "usage: verbose [on|off|toggle|show] or /verbose [on|off|toggle|show]".to_string(),
        )),
    }
}

fn parse_tui_context_command(line: &str) -> Option<Result<(), String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/context")
        .or_else(|| strip_tui_command_prefix(trimmed, "context"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/ctx"))
        .or_else(|| strip_tui_command_prefix(trimmed, "ctx"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["show" | "inspect" | "help" | "--help" | "-h"] => Some(Ok(())),
        _ => Some(Err("usage: context, ctx, /context, or /ctx".to_string())),
    }
}

fn parse_tui_exit_command(line: &str) -> Option<Result<(), String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/exit")
        .or_else(|| strip_tui_command_prefix(trimmed, "exit"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/quit"))
        .or_else(|| strip_tui_command_prefix(trimmed, "quit"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/q"))
        .or_else(|| strip_tui_command_prefix(trimmed, "q"))?;
    if rest.trim().is_empty() {
        Some(Ok(()))
    } else {
        Some(Err("usage: exit, quit, q, /exit, /quit, or /q".to_string()))
    }
}

fn parse_tui_tokens_command(line: &str) -> Option<Result<(), String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/tokens")
        .or_else(|| strip_tui_command_prefix(trimmed, "tokens"))?;
    if rest.trim().is_empty() {
        Some(Ok(()))
    } else {
        Some(Err("usage: tokens or /tokens".to_string()))
    }
}

fn parse_tui_cost_command(line: &str) -> Option<Result<(), String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/cost")
        .or_else(|| strip_tui_command_prefix(trimmed, "cost"))?;
    if rest.trim().is_empty() {
        Some(Ok(()))
    } else {
        Some(Err("usage: cost or /cost".to_string()))
    }
}

fn parse_tui_cache_command(line: &str) -> Option<Result<TuiCacheCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/cache")
        .or_else(|| strip_tui_command_prefix(trimmed, "cache"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] => Some(Ok(TuiCacheCommand::Summary { count: None })),
        ["inspect"] => Some(Ok(TuiCacheCommand::Inspect)),
        ["warmup"] => Some(Ok(TuiCacheCommand::Warmup)),
        [count] => match count.parse::<usize>() {
            Ok(count) if count > 0 => Some(Ok(TuiCacheCommand::Summary { count: Some(count) })),
            _ => Some(Err(
                "usage: cache [count|inspect|warmup] or /cache [count|inspect|warmup]".to_string(),
            )),
        },
        _ => Some(Err(
            "usage: cache [count|inspect|warmup] or /cache [count|inspect|warmup]".to_string(),
        )),
    }
}

fn parse_tui_mode_command(line: &str) -> Option<Result<TuiModeCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/mode")
        .or_else(|| strip_tui_command_prefix(trimmed, "mode"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["show" | "list" | "help" | "--help" | "-h"] => Some(Ok(TuiModeCommand::Show)),
        [value] => match TuiMode::from_command_arg(value) {
            Some(mode) => Some(Ok(TuiModeCommand::Set(mode))),
            None => Some(Err(
                "usage: mode [agent|plan|yolo|1|2|3] or /mode [agent|plan|yolo|1|2|3]".to_string(),
            )),
        },
        _ => Some(Err(
            "usage: mode [agent|plan|yolo|1|2|3] or /mode [agent|plan|yolo|1|2|3]".to_string(),
        )),
    }
}

fn parse_tui_theme_command(line: &str) -> Option<Result<TuiThemeCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/theme")
        .or_else(|| strip_tui_command_prefix(trimmed, "theme"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] => Some(Ok(TuiThemeCommand::Cycle)),
        ["show" | "list" | "help" | "--help" | "-h"] => Some(Ok(TuiThemeCommand::Show)),
        [value] => match TuiTheme::from_command_arg(value) {
            Some(theme) => Some(Ok(TuiThemeCommand::Set(theme))),
            None => Some(Err(
                "usage: theme [dark|light|grayscale|system] or /theme [dark|light|grayscale|system]"
                    .to_string(),
            )),
        },
        _ => Some(Err(
            "usage: theme [dark|light|grayscale|system] or /theme [dark|light|grayscale|system]"
                .to_string(),
        )),
    }
}

fn parse_tui_model_command(line: &str) -> Option<Result<TuiModelCommand, String>> {
    let trimmed = line.trim();
    if let Some(rest) = strip_tui_command_prefix(trimmed, "/models")
        .or_else(|| strip_tui_command_prefix(trimmed, "models"))
    {
        return if rest.trim().is_empty() {
            Some(Ok(TuiModelCommand::List))
        } else {
            Some(Err(
                "usage: model [name|list], models, /model [name|list], or /models".to_string(),
            ))
        };
    }
    let rest = strip_tui_command_prefix(trimmed, "/model")
        .or_else(|| strip_tui_command_prefix(trimmed, "model"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] => Some(Ok(TuiModelCommand::Show)),
        ["list" | "ls" | "show"] => Some(Ok(TuiModelCommand::List)),
        [model] if !model.starts_with('-') => Some(Ok(TuiModelCommand::Set {
            model: (*model).to_string(),
        })),
        _ => Some(Err(
            "usage: model [name|list], models, /model [name|list], or /models".to_string(),
        )),
    }
}

fn parse_tui_provider_command(line: &str) -> Option<Result<TuiProviderCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/provider")
        .or_else(|| strip_tui_command_prefix(trimmed, "provider"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] => Some(Ok(TuiProviderCommand::Show)),
        ["list" | "ls" | "show"] => Some(Ok(TuiProviderCommand::List)),
        [provider] if !provider.starts_with('-') => Some(Ok(TuiProviderCommand::Set {
            provider: (*provider).to_string(),
            model: None,
        })),
        [provider, model] if !provider.starts_with('-') && !model.starts_with('-') => {
            Some(Ok(TuiProviderCommand::Set {
                provider: (*provider).to_string(),
                model: Some((*model).to_string()),
            }))
        }
        _ => Some(Err(
            "usage: provider [name [model]|list] or /provider [name [model]|list]".to_string(),
        )),
    }
}

fn parse_tui_skills_command(line: &str) -> Option<Result<TuiSkillsCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/skills")
        .or_else(|| strip_tui_command_prefix(trimmed, "skills"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] => Some(Ok(TuiSkillsCommand::List { prefix: None })),
        ["remote" | "--remote" | "sync" | "--sync"] => Some(Err(
            "remote skill registry sync is not supported in the TUI; manage configured TOML skills on disk"
                .to_string(),
        )),
        [prefix] if !prefix.starts_with('-') => Some(Ok(TuiSkillsCommand::List {
            prefix: Some((*prefix).to_string()),
        })),
        _ => Some(Err("usage: skills [prefix] or /skills [prefix]".to_string())),
    }
}

fn parse_tui_skill_command(line: &str) -> Option<Result<TuiSkillsCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/skill")
        .or_else(|| strip_tui_command_prefix(trimmed, "skill"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] => Some(Err("usage: skill <name> or /skill <name>".to_string())),
        ["install" | "update" | "uninstall" | "trust", ..] => Some(Err(
            "skill install/update/uninstall/trust is not supported in the TUI; edit configured TOML skill files"
                .to_string(),
        )),
        [name] if !name.starts_with('-') => Some(Ok(TuiSkillsCommand::Show {
            name: (*name).to_string(),
        })),
        _ => Some(Err("usage: skill <name> or /skill <name>".to_string())),
    }
}

fn parse_tui_feedback_command(line: &str) -> Option<Result<TuiFeedbackCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/feedback")
        .or_else(|| strip_tui_command_prefix(trimmed, "feedback"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["help" | "--help" | "-h"] => Some(Ok(TuiFeedbackCommand::Show)),
        ["1" | "bug" | "bug-report" | "bug_report"] => Some(Ok(TuiFeedbackCommand::Bug)),
        ["2" | "feature" | "feature-request" | "feature_request" | "enhancement"] => {
            Some(Ok(TuiFeedbackCommand::Feature))
        }
        ["3" | "security" | "vulnerability" | "private"] => Some(Ok(TuiFeedbackCommand::Security)),
        _ => Some(Err(
            "usage: feedback [bug|feature|security] or /feedback [bug|feature|security]"
                .to_string(),
        )),
    }
}

fn parse_tui_note_command(line: &str) -> Option<Result<TuiNoteCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/note")
        .or_else(|| strip_tui_command_prefix(trimmed, "note"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["help" | "--help" | "-h"] => Some(Ok(TuiNoteCommand::Help)),
        ["list" | "ls"] => Some(Ok(TuiNoteCommand::List)),
        ["path"] => Some(Ok(TuiNoteCommand::Path)),
        ["clear"] => Some(Ok(TuiNoteCommand::Clear)),
        ["show", index] => parse_note_index_arg(index)
            .map(|index| TuiNoteCommand::Show { index })
            .map_or_else(
                || Some(Err("usage: note show <n> or /note show <n>".to_string())),
                |command| Some(Ok(command)),
            ),
        ["remove" | "rm" | "delete", index] => parse_note_index_arg(index)
            .map(|index| TuiNoteCommand::Remove { index })
            .map_or_else(
                || Some(Err("usage: note remove <n> or /note remove <n>".to_string())),
                |command| Some(Ok(command)),
            ),
        ["show"] => Some(Err("usage: note show <n> or /note show <n>".to_string())),
        ["remove" | "rm" | "delete"] => {
            Some(Err("usage: note remove <n> or /note remove <n>".to_string()))
        }
        ["edit", index, content @ ..] if !content.is_empty() => match parse_note_index_arg(index) {
            Some(index) => Some(Ok(TuiNoteCommand::Edit {
                index,
                content: content.join(" "),
            })),
            None => Some(Err(
                "usage: note edit <n> <text> or /note edit <n> <text>".to_string()
            )),
        },
        ["edit", ..] => Some(Err(
            "usage: note edit <n> <text> or /note edit <n> <text>".to_string()
        )),
        ["add", content @ ..] if !content.is_empty() => Some(Ok(TuiNoteCommand::Add {
            content: content.join(" "),
        })),
        ["add"] => Some(Err("usage: note add <text> or /note add <text>".to_string())),
        content @ [_, ..] => Some(Ok(TuiNoteCommand::Add {
            content: content.join(" "),
        })),
    }
}

fn parse_tui_hooks_command(line: &str) -> Option<Result<TuiHooksCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/hooks")
        .or_else(|| strip_tui_command_prefix(trimmed, "hooks"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/hook"))
        .or_else(|| strip_tui_command_prefix(trimmed, "hook"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["list" | "ls" | "show"] => Some(Ok(TuiHooksCommand::List)),
        ["events" | "event" | "list-events"] => Some(Ok(TuiHooksCommand::Events)),
        ["help" | "--help" | "-h"] => Some(Ok(TuiHooksCommand::Events)),
        _ => Some(Err(
            "usage: hooks [list|events] or /hooks [list|events]".to_string()
        )),
    }
}

fn parse_tui_goal_command(line: &str) -> Option<Result<TuiGoalCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/goal")
        .or_else(|| strip_tui_command_prefix(trimmed, "goal"))?;
    let arg = rest.trim();
    if arg.is_empty() || matches!(arg, "show" | "status" | "help" | "--help" | "-h") {
        return Some(Ok(TuiGoalCommand::Show));
    }
    if matches!(arg, "clear" | "reset" | "done") {
        return Some(Ok(TuiGoalCommand::Clear));
    }
    let (objective, token_budget) = parse_tui_goal_budget(arg);
    if objective.trim().is_empty() {
        return Some(Err(
            "usage: goal <objective> [budget: N] or /goal clear".to_string()
        ));
    }
    Some(Ok(TuiGoalCommand::Set {
        objective,
        token_budget,
    }))
}

fn parse_tui_goal_budget(value: &str) -> (String, Option<u64>) {
    if let Some((objective, rest)) = value.split_once(" | budget:") {
        return (
            objective.trim().to_string(),
            rest.split_whitespace()
                .next()
                .and_then(|value| value.parse::<u64>().ok()),
        );
    }
    if let Some((objective, rest)) = value.split_once("budget:") {
        return (
            objective.trim().to_string(),
            rest.split_whitespace()
                .next()
                .and_then(|value| value.parse::<u64>().ok()),
        );
    }
    (value.trim().to_string(), None)
}

fn format_tui_goal_elapsed(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{seconds}s")
    }
}

fn parse_tui_anchor_command(line: &str) -> Option<Result<TuiAnchorCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/anchor")
        .or_else(|| strip_tui_command_prefix(trimmed, "anchor"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["help" | "--help" | "-h"] => Some(Ok(TuiAnchorCommand::Help)),
        ["list" | "ls"] => Some(Ok(TuiAnchorCommand::List)),
        ["path"] => Some(Ok(TuiAnchorCommand::Path)),
        ["remove" | "rm" | "delete", index] => parse_note_index_arg(index)
            .map(|index| TuiAnchorCommand::Remove { index })
            .map_or_else(
                || {
                    Some(Err(
                        "usage: anchor remove <n> or /anchor remove <n>".to_string()
                    ))
                },
                |command| Some(Ok(command)),
            ),
        ["remove" | "rm" | "delete"] => Some(Err(
            "usage: anchor remove <n> or /anchor remove <n>".to_string()
        )),
        ["add", content @ ..] if !content.is_empty() => Some(Ok(TuiAnchorCommand::Add {
            content: content.join(" "),
        })),
        ["add"] => Some(Err(
            "usage: anchor add <text> or /anchor add <text>".to_string()
        )),
        content @ [_, ..] => Some(Ok(TuiAnchorCommand::Add {
            content: content.join(" "),
        })),
    }
}

fn parse_tui_queue_command(line: &str) -> Option<Result<TuiQueueCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/queue")
        .or_else(|| strip_tui_command_prefix(trimmed, "queue"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/queued"))
        .or_else(|| strip_tui_command_prefix(trimmed, "queued"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["list" | "ls" | "show"] => Some(Ok(TuiQueueCommand::List)),
        ["help" | "--help" | "-h"] => Some(Ok(TuiQueueCommand::Help)),
        ["edit", index] => parse_note_index_arg(index)
            .map(|index| TuiQueueCommand::Edit { index })
            .map_or_else(
                || Some(Err("usage: queue edit <n> or /queue edit <n>".to_string())),
                |command| Some(Ok(command)),
            ),
        ["edit"] => Some(Err("usage: queue edit <n> or /queue edit <n>".to_string())),
        ["drop" | "remove" | "rm", index] => parse_note_index_arg(index)
            .map(|index| TuiQueueCommand::Drop { index })
            .map_or_else(
                || Some(Err("usage: queue drop <n> or /queue drop <n>".to_string())),
                |command| Some(Ok(command)),
            ),
        ["drop" | "remove" | "rm"] => {
            Some(Err("usage: queue drop <n> or /queue drop <n>".to_string()))
        }
        ["clear"] => Some(Ok(TuiQueueCommand::Clear)),
        _ => Some(Err(
            "usage: queue [list|edit <n>|drop <n>|clear] or /queue [list|edit <n>|drop <n>|clear]"
                .to_string(),
        )),
    }
}

fn parse_tui_share_command(line: &str) -> Option<Result<TuiShareCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/share")
        .or_else(|| strip_tui_command_prefix(trimmed, "share"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] => Some(Ok(TuiShareCommand::Export)),
        ["help" | "--help" | "-h"] => Some(Ok(TuiShareCommand::Help)),
        _ => Some(Err(
            "usage: share or /share; use share help for details".to_string()
        )),
    }
}

fn parse_tui_export_command(line: &str) -> Option<Result<TuiExportCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/export")
        .or_else(|| strip_tui_command_prefix(trimmed, "export"))?;
    let path = rest.trim();
    match path {
        "" => Some(Ok(TuiExportCommand::Export { path: None })),
        "help" | "--help" | "-h" => Some(Ok(TuiExportCommand::Help)),
        value => Some(Ok(TuiExportCommand::Export {
            path: Some(value.to_string()),
        })),
    }
}

fn parse_tui_save_command(line: &str) -> Option<Result<TuiSaveCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/save")
        .or_else(|| strip_tui_command_prefix(trimmed, "save"))?;
    let path = rest.trim();
    match path {
        "" => Some(Ok(TuiSaveCommand::Save { path: None })),
        "help" | "--help" | "-h" => Some(Ok(TuiSaveCommand::Help)),
        value => Some(Ok(TuiSaveCommand::Save {
            path: Some(value.to_string()),
        })),
    }
}

fn parse_tui_load_command(line: &str) -> Option<Result<TuiLoadCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/load")
        .or_else(|| strip_tui_command_prefix(trimmed, "load"))?;
    let path = rest.trim();
    match path {
        "" => Some(Err("usage: load <path> or /load <path>".to_string())),
        "help" | "--help" | "-h" => Some(Ok(TuiLoadCommand::Help)),
        value => Some(Ok(TuiLoadCommand::Load {
            path: value.to_string(),
        })),
    }
}

fn parse_tui_attach_command(line: &str) -> Option<Result<TuiAttachCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/attach")
        .or_else(|| strip_tui_command_prefix(trimmed, "attach"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/image"))
        .or_else(|| strip_tui_command_prefix(trimmed, "image"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/media"))
        .or_else(|| strip_tui_command_prefix(trimmed, "media"))?;
    let path = rest.trim();
    match path {
        "" => Some(Err(
            "usage: attach <image-or-video-path> or /attach <image-or-video-path>".to_string(),
        )),
        "help" | "--help" | "-h" => Some(Ok(TuiAttachCommand::Help)),
        value => Some(Ok(TuiAttachCommand::Attach {
            path: unquote_tui_path(value),
        })),
    }
}

fn unquote_tui_path(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}

const DEEPSEEKCODE_CHANGELOG: &str = include_str!("../CHANGELOG.md");
const TUI_CHANGELOG_MAX_CHARS: usize = 4096;

fn latest_changelog_section(content: &str) -> Option<String> {
    let lines = content.lines().collect::<Vec<_>>();
    let start = lines
        .iter()
        .position(|line| line.trim_start().starts_with("## "))?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, line)| line.trim_start().starts_with("## "))
        .map_or(lines.len(), |(index, _)| index);
    let section = lines[start..end].join("\n").trim().to_string();
    if section.is_empty() {
        None
    } else {
        Some(section)
    }
}

fn inline_changelog_section(section: &str) -> String {
    let char_count = section.chars().count();
    if char_count <= TUI_CHANGELOG_MAX_CHARS {
        return section.to_string();
    }
    let truncated = section
        .chars()
        .take(TUI_CHANGELOG_MAX_CHARS)
        .collect::<String>();
    format!(
        "{truncated}\n\n[... {} characters omitted from bundled changelog]",
        char_count - TUI_CHANGELOG_MAX_CHARS
    )
}

fn parse_tui_change_command(line: &str) -> Option<Result<TuiChangeCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/change")
        .or_else(|| strip_tui_command_prefix(trimmed, "change"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/changes"))
        .or_else(|| strip_tui_command_prefix(trimmed, "changes"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/changelog"))
        .or_else(|| strip_tui_command_prefix(trimmed, "changelog"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["latest"] | ["show"] => Some(Ok(TuiChangeCommand::Show)),
        ["help" | "--help" | "-h"] => Some(Ok(TuiChangeCommand::Help)),
        _ => Some(Err(
            "usage: change or /change; use change help for details".to_string()
        )),
    }
}

fn parse_tui_system_command(line: &str) -> Option<Result<TuiSystemCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/system")
        .or_else(|| strip_tui_command_prefix(trimmed, "system"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["show"] | ["prompt"] => Some(Ok(TuiSystemCommand::Show)),
        ["help" | "--help" | "-h"] => Some(Ok(TuiSystemCommand::Help)),
        _ => Some(Err(
            "usage: system or /system; use system help for details".to_string()
        )),
    }
}

fn parse_tui_edit_command(line: &str) -> Option<Result<TuiEditCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/edit")
        .or_else(|| strip_tui_command_prefix(trimmed, "edit"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["last"] => Some(Ok(TuiEditCommand::LoadLast)),
        ["help" | "--help" | "-h"] => Some(Ok(TuiEditCommand::Help)),
        _ => Some(Err(
            "usage: edit or /edit; use edit help for details".to_string()
        )),
    }
}

fn parse_tui_clear_command(line: &str) -> Option<Result<TuiClearCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/clear")
        .or_else(|| strip_tui_command_prefix(trimmed, "clear"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] => Some(Ok(TuiClearCommand::Clear)),
        ["help" | "--help" | "-h"] => Some(Ok(TuiClearCommand::Help)),
        _ => Some(Err("usage: clear or /clear".to_string())),
    }
}

fn parse_tui_diff_command(line: &str) -> Option<Result<TuiDiffCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/diff")
        .or_else(|| strip_tui_command_prefix(trimmed, "diff"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["show"] => Some(Ok(TuiDiffCommand::Show)),
        ["help" | "--help" | "-h"] => Some(Ok(TuiDiffCommand::Help)),
        _ => Some(Err("usage: diff or /diff".to_string())),
    }
}

fn parse_tui_subagents_command(line: &str) -> Option<Result<TuiSubagentsCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/subagents")
        .or_else(|| strip_tui_command_prefix(trimmed, "subagents"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/agents"))
        .or_else(|| strip_tui_command_prefix(trimmed, "agents"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["list" | "ls" | "show"] => Some(Ok(TuiSubagentsCommand::List)),
        ["help" | "--help" | "-h"] => Some(Ok(TuiSubagentsCommand::Help)),
        _ => Some(Err(
            "usage: subagents, agents, /subagents, or /agents".to_string()
        )),
    }
}

fn parse_tui_agent_command(line: &str) -> Option<Result<TuiSubagentsCommand, String>> {
    let trimmed = line.trim();
    let (rest, slash_command) = if let Some(rest) = strip_tui_command_prefix(trimmed, "/agent") {
        (rest, true)
    } else if let Some(rest) = strip_tui_command_prefix(trimmed, "agent") {
        (rest, false)
    } else {
        return None;
    };
    let arg = rest.trim();
    if arg.is_empty() {
        if !slash_command {
            return None;
        }
        return Some(Err(
            "usage: agent [0-3] <task> or /agent [0-3] <task>".to_string()
        ));
    }
    if matches!(arg, "help" | "--help" | "-h") {
        return Some(Ok(TuiSubagentsCommand::Help));
    }
    let mut parts = arg.split_whitespace();
    let first = parts.next().unwrap_or_default();
    let (max_depth, task) = match first.parse::<usize>() {
        Ok(depth) if depth <= 3 => (depth, parts.collect::<Vec<_>>().join(" ")),
        Ok(_) => {
            return Some(Err("agent depth must be between 0 and 3".to_string()));
        }
        Err(_) => (1, arg.to_string()),
    };
    let task = task.trim();
    if task.is_empty() {
        return Some(Err(
            "usage: agent [0-3] <task> or /agent [0-3] <task>".to_string()
        ));
    }
    Some(Ok(TuiSubagentsCommand::Spawn {
        max_depth,
        task: task.to_string(),
    }))
}

fn parse_tui_rlm_command(line: &str) -> Option<Result<TuiRlmCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/rlm")
        .or_else(|| strip_tui_command_prefix(trimmed, "rlm"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/recursive"))
        .or_else(|| strip_tui_command_prefix(trimmed, "recursive"))?;
    let arg = rest.trim();
    if matches!(arg, "help" | "--help" | "-h") {
        return Some(Ok(TuiRlmCommand::Help));
    }
    if arg.is_empty() {
        return Some(Err(
            "usage: rlm [0-3] <file_or_text> or /rlm [0-3] <file_or_text>".to_string(),
        ));
    }
    let mut parts = arg.split_whitespace();
    let first = parts.next().unwrap_or_default();
    let (max_depth, target) = match first.parse::<usize>() {
        Ok(depth) if depth <= 3 => (depth, parts.collect::<Vec<_>>().join(" ")),
        Ok(_) => {
            return Some(Err("rlm depth must be between 0 and 3".to_string()));
        }
        Err(_) => (1, arg.to_string()),
    };
    let target = target.trim();
    if target.is_empty() {
        return Some(Err(
            "usage: rlm [0-3] <file_or_text> or /rlm [0-3] <file_or_text>".to_string(),
        ));
    }
    Some(Ok(TuiRlmCommand::Start {
        max_depth,
        target: target.to_string(),
    }))
}

fn parse_tui_relay_command(line: &str) -> Option<Result<TuiRelayCommand, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/relay")
        .or_else(|| strip_tui_command_prefix(trimmed, "relay"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/batonpass"))
        .or_else(|| strip_tui_command_prefix(trimmed, "batonpass"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/接力"))
        .or_else(|| strip_tui_command_prefix(trimmed, "接力"))?;
    let focus = rest.trim();
    if matches!(focus, "help" | "--help" | "-h") {
        return Some(Ok(TuiRelayCommand::Help));
    }
    Some(Ok(TuiRelayCommand::Create {
        focus: if focus.is_empty() {
            None
        } else {
            Some(focus.to_string())
        },
    }))
}

fn parse_note_index_arg(value: &str) -> Option<usize> {
    value.parse::<usize>().ok().filter(|index| *index > 0)
}

fn parse_tui_links_command(line: &str) -> Option<Result<(), String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/links")
        .or_else(|| strip_tui_command_prefix(trimmed, "links"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/dashboard"))
        .or_else(|| strip_tui_command_prefix(trimmed, "dashboard"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/api"))
        .or_else(|| strip_tui_command_prefix(trimmed, "api"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["help" | "--help" | "-h"] => Some(Ok(())),
        _ => Some(Err(
            "usage: links, dashboard, api, /links, /dashboard, or /api".to_string(),
        )),
    }
}

fn parse_tui_home_command(line: &str) -> Option<Result<(), String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/home")
        .or_else(|| strip_tui_command_prefix(trimmed, "home"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/stats"))
        .or_else(|| strip_tui_command_prefix(trimmed, "stats"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/overview"))
        .or_else(|| strip_tui_command_prefix(trimmed, "overview"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["help" | "--help" | "-h"] => Some(Ok(())),
        _ => Some(Err(
            "usage: home, stats, overview, /home, /stats, or /overview".to_string(),
        )),
    }
}

fn parse_tui_settings_command(line: &str) -> Option<Result<(), String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/settings")
        .or_else(|| strip_tui_command_prefix(trimmed, "settings"))
        .or_else(|| strip_tui_command_prefix(trimmed, "/config"))
        .or_else(|| strip_tui_command_prefix(trimmed, "config"))?;
    let args = rest.split_whitespace().collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["show" | "help" | "--help" | "-h"] => Some(Ok(())),
        _ => Some(Err(
            "usage: settings, config, /settings, or /config".to_string()
        )),
    }
}

fn strip_tui_command_prefix<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = value.strip_prefix(prefix)?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest)
    } else {
        None
    }
}

fn parse_tui_rename_command(line: &str) -> Option<Result<String, String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/rename")
        .or_else(|| strip_tui_command_prefix(trimmed, "rename"))?;
    let title = rest.trim();
    if title.is_empty() {
        return Some(Err(
            "usage: rename <new title> or /rename <new title>".to_string()
        ));
    }
    if title.chars().count() > MAX_TUI_RENAME_TITLE_CHARS {
        return Some(Err(format!(
            "rename title must be <= {MAX_TUI_RENAME_TITLE_CHARS} characters"
        )));
    }
    Some(Ok(title.to_string()))
}

fn parse_tui_init_command(line: &str) -> Option<Result<(), String>> {
    let trimmed = line.trim();
    let rest = strip_tui_command_prefix(trimmed, "/init")
        .or_else(|| strip_tui_command_prefix(trimmed, "init"))?;
    if rest.trim().is_empty() {
        Some(Ok(()))
    } else {
        Some(Err("usage: init or /init".to_string()))
    }
}

fn parse_tui_custom_slash_command(line: &str) -> Option<(String, Vec<String>)> {
    let mut tokens = line.split_whitespace();
    let command = tokens.next()?;
    if command == "/" || !command.starts_with('/') {
        return None;
    }
    Some((
        command.to_string(),
        tokens.map(|token| token.to_string()).collect(),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiAction {
    SubmitUserMessage {
        thread_id: String,
        content: String,
    },
    RunCustomSlashCommand {
        thread_id: String,
        command: String,
        args: Vec<String>,
    },
    RenameSession {
        session_id: String,
        title: String,
    },
    InitProjectInstructions {
        workspace: String,
    },
    Network {
        workspace: String,
        command: TuiNetworkCommand,
    },
    Lsp {
        workspace: String,
        command: TuiLspCommand,
    },
    ShowSystemPrompt {
        workspace: String,
        mode: TuiMode,
        task: Option<String>,
    },
    Model {
        workspace: String,
        command: TuiModelCommand,
    },
    Provider {
        workspace: String,
        command: TuiProviderCommand,
    },
    Skills {
        command: TuiSkillsCommand,
    },
    Note {
        command: TuiNoteCommand,
    },
    Anchor {
        workspace: String,
        command: TuiAnchorCommand,
    },
    ShareSession {
        thread_id: String,
    },
    ExportThread {
        thread_id: String,
        path: Option<String>,
    },
    SaveSession {
        session_id: String,
        thread_id: String,
        path: Option<String>,
    },
    LoadSession {
        workspace: String,
        path: String,
    },
    ClearConversation {
        session_id: String,
        previous_thread_id: Option<String>,
    },
    ShowDiff {
        workspace: String,
    },
    CreateSubagentTask {
        thread_id: String,
        task: String,
        max_depth: usize,
    },
    Hooks {
        command: TuiHooksCommand,
    },
    RespondApproval {
        thread_id: String,
        turn_id: Option<String>,
        request_id: String,
        decision: String,
    },
    RespondUserInput {
        thread_id: String,
        turn_id: Option<String>,
        request_id: String,
        answers: BTreeMap<String, String>,
    },
    CancelRun {
        thread_id: String,
        turn_id: Option<String>,
    },
    CreateTask {
        thread_id: String,
        summary: String,
    },
    PauseTask {
        task_id: String,
    },
    ResumeTask {
        task_id: String,
    },
    CancelTask {
        task_id: String,
    },
    CreateRollbackSnapshot {
        label: Option<String>,
    },
    ListRollbackSnapshots {
        limit: usize,
    },
    ShowRollbackSnapshot {
        id: String,
    },
    ShowRollbackHunk {
        id: String,
        hunk: Option<usize>,
    },
    RestoreRollbackHunk {
        id: String,
        hunk: usize,
        apply: bool,
    },
    RevertTurn {
        id: String,
        apply: bool,
    },
    RunDiagnostics {
        changed: bool,
        paths: Vec<String>,
    },
    RunShell {
        command: String,
    },
    RunApprovedShell {
        command: String,
    },
    ListShell,
    ShowShell {
        task_id: String,
    },
    AttachShell {
        task_id: String,
        cursor: Option<usize>,
        tail: bool,
    },
    ShellSupervisorStatus,
    SendShellStdin {
        task_id: String,
        input: String,
        close: bool,
    },
    WaitShell {
        task_id: String,
        wait: bool,
        timeout_ms: u64,
    },
    ResizeShell {
        task_id: String,
        rows: u16,
        cols: u16,
    },
    CancelShell {
        task_id: Option<String>,
        all: bool,
    },
    AppendMemory {
        note: String,
    },
    Memory {
        command: TuiMemoryCommand,
    },
    McpManager,
    McpList,
    McpInit {
        force: bool,
    },
    McpAddStdio {
        scope: TuiMcpConfigScope,
        name: String,
        command: String,
        args: Vec<String>,
    },
    McpAddRemote {
        scope: TuiMcpConfigScope,
        name: String,
        transport: String,
        url: String,
    },
    McpRemove {
        scope: TuiMcpConfigScope,
        name: String,
    },
    McpSetEnabled {
        scope: TuiMcpConfigScope,
        name: String,
        enabled: bool,
    },
    McpDetails {
        kind: TuiMcpDetailKind,
        server: Option<String>,
    },
    McpManagerDetails {
        kind: TuiMcpDetailKind,
        server: Option<String>,
    },
    McpValidate,
    TriggerAutomation {
        automation_id: String,
        prompt_override: Option<String>,
    },
    CompactThread {
        thread_id: String,
        keep_tail_turns: usize,
    },
}

const DEFAULT_TUI_COMPACTION_KEEP_TAIL_TURNS: usize = 8;
const MAX_TUI_COMPACTION_KEEP_TAIL_TURNS: usize = 200;
const DEFAULT_TUI_REASONING_REPLAY_LIMIT: usize = 3;
const MAX_TUI_REASONING_REPLAY_LIMIT: usize = 20;
const TUI_CONTEXT_WINDOW_TOKENS: u64 = 1_000_000;
const TUI_REASONING_REPLAY_PREF_KIND: &str = "deepseek.tui.reasoning_replay.v1";
const MAX_TUI_COMMAND_HISTORY: usize = 100;
const MAX_TUI_COMPOSER_STASH_ENTRIES: usize = 100;
const MAX_TUI_RENAME_TITLE_CHARS: usize = 100;
const TUI_QUEUE_PREVIEW_LIMIT: usize = 120;
const TUI_PICKER_PAGE_SIZE: usize = 5;

struct TuiHelpCommandInfo {
    category: &'static str,
    name: &'static str,
    aliases: &'static [&'static str],
    usage: &'static str,
    description: &'static str,
}

const TUI_HELP_COMMANDS: &[TuiHelpCommandInfo] = &[
    TuiHelpCommandInfo {
        category: "Workbench",
        name: "mode",
        aliases: &[],
        usage: "/mode [agent|plan|yolo|1|2|3]",
        description: "Show or switch Plan / Agent / YOLO mode.",
    },
    TuiHelpCommandInfo {
        category: "Workbench",
        name: "home",
        aliases: &["stats", "overview"],
        usage: "/home",
        description: "Show the compact runtime dashboard.",
    },
    TuiHelpCommandInfo {
        category: "Workbench",
        name: "links",
        aliases: &["dashboard", "api"],
        usage: "/links",
        description: "Show DeepSeekCode and DeepSeek API links.",
    },
    TuiHelpCommandInfo {
        category: "Workbench",
        name: "feedback",
        aliases: &[],
        usage: "/feedback [bug|feature|security]",
        description: "Show bug, feature, and security feedback targets.",
    },
    TuiHelpCommandInfo {
        category: "Interaction",
        name: "subagents",
        aliases: &["agents"],
        usage: "/subagents",
        description: "List runtime-backed sub-agent tasks in the active thread.",
    },
    TuiHelpCommandInfo {
        category: "Interaction",
        name: "agent",
        aliases: &[],
        usage: "/agent [0-3] <task>",
        description: "Queue a persistent sub-agent task for the active thread.",
    },
    TuiHelpCommandInfo {
        category: "Runtime Work",
        name: "rlm",
        aliases: &["recursive"],
        usage: "/rlm [0-3] <file_or_text>",
        description: "Ask the active agent to open a persistent RLM process.",
    },
    TuiHelpCommandInfo {
        category: "Runtime Work",
        name: "relay",
        aliases: &["batonpass", "接力"],
        usage: "/relay [focus]",
        description: "Ask the active agent to write a compact session relay.",
    },
    TuiHelpCommandInfo {
        category: "Interaction",
        name: "note",
        aliases: &[],
        usage: "/note [add|list|show|edit|remove|clear|path]",
        description: "Manage persistent workspace notes.",
    },
    TuiHelpCommandInfo {
        category: "Interaction",
        name: "anchor",
        aliases: &[],
        usage: "/anchor [add|list|remove|path]",
        description: "Pin workspace facts in .dscode/anchors.md.",
    },
    TuiHelpCommandInfo {
        category: "Interaction",
        name: "queue",
        aliases: &["queued"],
        usage: "/queue [list|edit <n>|drop <n>|clear]",
        description: "Manage follow-up messages queued while the active turn is busy.",
    },
    TuiHelpCommandInfo {
        category: "Interaction",
        name: "share",
        aliases: &[],
        usage: "/share",
        description: "Export the active thread transcript as shareable HTML/Gist.",
    },
    TuiHelpCommandInfo {
        category: "Interaction",
        name: "export",
        aliases: &[],
        usage: "/export [path]",
        description: "Write the active thread transcript to a local Markdown file.",
    },
    TuiHelpCommandInfo {
        category: "Config",
        name: "hooks",
        aliases: &["hook"],
        usage: "/hooks [list|events]",
        description: "Inspect local hook roots and supported hook event names.",
    },
    TuiHelpCommandInfo {
        category: "Workbench",
        name: "goal",
        aliases: &[],
        usage: "/goal [objective [budget: N]|clear]",
        description: "Set, inspect, or clear the current TUI session goal.",
    },
    TuiHelpCommandInfo {
        category: "Workbench",
        name: "diff",
        aliases: &[],
        usage: "/diff",
        description: "Show changed files and git diff stat for the selected workspace.",
    },
    TuiHelpCommandInfo {
        category: "Workbench",
        name: "clear",
        aliases: &[],
        usage: "/clear",
        description: "Start a fresh active-thread conversation without deleting history.",
    },
    TuiHelpCommandInfo {
        category: "Workbench",
        name: "settings",
        aliases: &["config"],
        usage: "/settings",
        description: "Show TUI and workspace configuration entry points.",
    },
    TuiHelpCommandInfo {
        category: "Workbench",
        name: "theme",
        aliases: &[],
        usage: "/theme [dark|light|grayscale|system]",
        description: "Show, cycle, or switch the local TUI theme.",
    },
    TuiHelpCommandInfo {
        category: "Workbench",
        name: "statusline",
        aliases: &["status-line"],
        usage: "/statusline",
        description: "Show command bar items and status-line shortcuts.",
    },
    TuiHelpCommandInfo {
        category: "Workbench",
        name: "exit",
        aliases: &["quit", "q"],
        usage: "/exit",
        description: "Quit the TUI workbench.",
    },
    TuiHelpCommandInfo {
        category: "Runtime",
        name: "verbose",
        aliases: &[],
        usage: "/verbose [on|off|toggle|show]",
        description: "Toggle full reasoning text in the live transcript.",
    },
    TuiHelpCommandInfo {
        category: "Runtime",
        name: "status",
        aliases: &[],
        usage: "/status",
        description: "Show detailed runtime, session, task, and usage state.",
    },
    TuiHelpCommandInfo {
        category: "Runtime",
        name: "tokens",
        aliases: &[],
        usage: "/tokens",
        description: "Show active-thread token and context telemetry.",
    },
    TuiHelpCommandInfo {
        category: "Runtime",
        name: "context",
        aliases: &["ctx"],
        usage: "/context",
        description: "Inspect active-thread context window and compaction state.",
    },
    TuiHelpCommandInfo {
        category: "Runtime",
        name: "cost",
        aliases: &[],
        usage: "/cost",
        description: "Show active-thread approximate spend.",
    },
    TuiHelpCommandInfo {
        category: "Runtime",
        name: "cache",
        aliases: &[],
        usage: "/cache [count|inspect|warmup]",
        description: "Show prompt-cache telemetry and read-only cache notes.",
    },
    TuiHelpCommandInfo {
        category: "Runtime",
        name: "change",
        aliases: &["changes", "changelog"],
        usage: "/change",
        description: "Show the latest bundled DeepSeekCode changelog entry.",
    },
    TuiHelpCommandInfo {
        category: "Runtime",
        name: "system",
        aliases: &[],
        usage: "/system",
        description: "Show the selected workspace runtime system prompt preview.",
    },
    TuiHelpCommandInfo {
        category: "Runtime",
        name: "edit",
        aliases: &[],
        usage: "/edit",
        description: "Load the latest selected user message back into the composer.",
    },
    TuiHelpCommandInfo {
        category: "Config",
        name: "model",
        aliases: &[],
        usage: "/model [name|list]",
        description: "Inspect or update the selected workspace model.",
    },
    TuiHelpCommandInfo {
        category: "Config",
        name: "models",
        aliases: &[],
        usage: "/models",
        description: "List the offline model catalog.",
    },
    TuiHelpCommandInfo {
        category: "Config",
        name: "provider",
        aliases: &[],
        usage: "/provider [name [model]|list]",
        description: "Inspect or update the selected workspace provider preset.",
    },
    TuiHelpCommandInfo {
        category: "Config",
        name: "network",
        aliases: &[],
        usage: "/network [list|allow|deny|remove|default]",
        description: "Inspect or edit selected workspace network policy.",
    },
    TuiHelpCommandInfo {
        category: "Config",
        name: "lsp",
        aliases: &[],
        usage: "/lsp [on|off|status]",
        description: "Inspect or edit post-edit diagnostics for the selected workspace.",
    },
    TuiHelpCommandInfo {
        category: "Skills",
        name: "skills",
        aliases: &[],
        usage: "/skills [prefix]",
        description: "List configured TOML skills.",
    },
    TuiHelpCommandInfo {
        category: "Skills",
        name: "skill",
        aliases: &[],
        usage: "/skill <name>",
        description: "Show one configured skill.",
    },
    TuiHelpCommandInfo {
        category: "Interaction",
        name: "memory",
        aliases: &[],
        usage: "/memory [show|path|clear|edit|help]",
        description: "Inspect or manage local user memory.",
    },
    TuiHelpCommandInfo {
        category: "Interaction",
        name: "stash",
        aliases: &["park"],
        usage: "/stash [list|pop|clear]",
        description: "List, restore, or clear parked composer drafts.",
    },
    TuiHelpCommandInfo {
        category: "Interaction",
        name: "rename",
        aliases: &[],
        usage: "/rename <new title>",
        description: "Rename the selected durable session.",
    },
    TuiHelpCommandInfo {
        category: "Interaction",
        name: "init",
        aliases: &[],
        usage: "/init",
        description: "Create project instructions in AGENTS.md.",
    },
    TuiHelpCommandInfo {
        category: "Runtime Work",
        name: "task",
        aliases: &["tasks"],
        usage: "task <summary>|pause|resume|cancel|select",
        description: "Create and manage active-thread runtime tasks.",
    },
    TuiHelpCommandInfo {
        category: "Runtime Work",
        name: "jobs",
        aliases: &["job"],
        usage: "jobs [list|show|wait|poll|stdin|cancel]",
        description: "Inspect and control local shell jobs.",
    },
    TuiHelpCommandInfo {
        category: "Runtime Work",
        name: "mcp",
        aliases: &[],
        usage: "mcp manager|tools|prompts|resources|init|add|enable|disable|remove|validate",
        description: "Inspect and manage MCP server configuration.",
    },
    TuiHelpCommandInfo {
        category: "Runtime Work",
        name: "diagnostics",
        aliases: &[],
        usage: "diagnostics [--changed|paths...]",
        description: "Queue local diagnostics for changed files or paths.",
    },
    TuiHelpCommandInfo {
        category: "Runtime Work",
        name: "restore",
        aliases: &["revert"],
        usage: "restore snapshot|list|show|hunks|apply-hunk|revert-turn",
        description: "Inspect or apply rollback snapshots and hunks.",
    },
    TuiHelpCommandInfo {
        category: "Runtime Work",
        name: "reasoning",
        aliases: &[],
        usage: "reasoning [list|latest|show|replay|search|pin]",
        description: "Inspect and pin local reasoning items for replay.",
    },
    TuiHelpCommandInfo {
        category: "Runtime Work",
        name: "compact",
        aliases: &[],
        usage: "compact [tail]",
        description: "Compact the active durable thread.",
    },
];

const TUI_COMMAND_COMPLETIONS: &[&str] = &[
    "mode plan",
    "mode agent",
    "mode yolo",
    "mode 1",
    "mode 2",
    "mode 3",
    "theme",
    "theme show",
    "theme dark",
    "theme light",
    "theme grayscale",
    "theme system",
    "exit",
    "quit",
    "q",
    "diff",
    "diff help",
    "clear",
    "clear help",
    "plan",
    "agent",
    "yolo",
    "sessions",
    "session filter ",
    "sessions filter ",
    "threads",
    "thread next",
    "thread prev",
    "thread filter ",
    "threads filter ",
    "rename ",
    "init",
    "tasks",
    "task create ",
    "task next",
    "task prev",
    "task select ",
    "task select all",
    "task select clear",
    "task pause",
    "task resume",
    "task cancel",
    "task bulk pause",
    "task bulk resume",
    "task bulk cancel",
    "shell ",
    "shell run ",
    "shell list",
    "shell show ",
    "shell attach ",
    "shell supervisor",
    "shell stdin ",
    "shell close-stdin ",
    "shell wait ",
    "shell poll ",
    "shell resize ",
    "shell cancel ",
    "jobs list",
    "jobs show ",
    "jobs attach ",
    "jobs supervisor",
    "jobs stdin ",
    "jobs close-stdin ",
    "jobs wait ",
    "jobs poll ",
    "jobs resize ",
    "jobs cancel ",
    "! ",
    "memory",
    "memory show",
    "memory path",
    "memory clear",
    "memory edit",
    "memory help",
    "subagents",
    "agents",
    "agent ",
    "rlm ",
    "recursive ",
    "relay ",
    "batonpass ",
    "note ",
    "note add ",
    "note list",
    "note show ",
    "note edit ",
    "note remove ",
    "note clear",
    "note path",
    "anchor ",
    "anchor add ",
    "anchor list",
    "anchor remove ",
    "anchor path",
    "queue",
    "queue list",
    "queue edit ",
    "queue drop ",
    "queue clear",
    "queued",
    "share",
    "share help",
    "export",
    "export ",
    "export help",
    "save",
    "save ",
    "save help",
    "load ",
    "load help",
    "attach ",
    "attach help",
    "image ",
    "media ",
    "hooks",
    "hooks list",
    "hooks events",
    "hook events",
    "goal ",
    "goal clear",
    "network",
    "network allow ",
    "network deny ",
    "network remove ",
    "network default ",
    "lsp",
    "lsp on",
    "lsp off",
    "lsp status",
    "lsp help",
    "status",
    "statusline",
    "statusline show",
    "verbose",
    "verbose on",
    "verbose off",
    "verbose show",
    "tokens",
    "context",
    "ctx",
    "cost",
    "cache",
    "cache inspect",
    "cache warmup",
    "change",
    "change help",
    "changelog",
    "system",
    "system help",
    "edit",
    "edit help",
    "model",
    "model auto",
    "model deepseek-v4-flash",
    "model deepseek-v4-pro",
    "models",
    "provider",
    "provider list",
    "provider deepseek",
    "provider nvidia-nim",
    "provider openrouter",
    "provider ollama",
    "skills",
    "skill ",
    "feedback",
    "feedback bug",
    "feedback feature",
    "feedback security",
    "links",
    "dashboard",
    "api",
    "home",
    "stats",
    "overview",
    "automations",
    "automation trigger",
    "compact",
    "thread compact",
    "stash",
    "stash list",
    "stash pop",
    "stash clear",
    "reasoning",
    "reasoning list",
    "reasoning latest",
    "reasoning show ",
    "reasoning replay ",
    "reasoning search ",
    "reasoning pin ",
    "reasoning pins",
    "reasoning unpin ",
    "mcp",
    "mcp manager",
    "mcp manager tab overview",
    "mcp manager tab tools",
    "mcp manager tab prompts",
    "mcp manager tab resources",
    "mcp manager tab resource-templates",
    "mcp manager tab health",
    "mcp manager filter ",
    "mcp manager tools",
    "mcp manager prompts",
    "mcp manager resources",
    "mcp manager resource-templates",
    "mcp list",
    "mcp status",
    "mcp reload",
    "mcp tools",
    "mcp prompts",
    "mcp resources",
    "mcp resource-templates",
    "mcp close",
    "mcp init",
    "mcp init --force",
    "mcp add stdio ",
    "mcp add http ",
    "mcp add sse ",
    "mcp enable ",
    "mcp disable ",
    "mcp remove ",
    "mcp user add stdio ",
    "mcp user add http ",
    "mcp user add sse ",
    "mcp user enable ",
    "mcp user disable ",
    "mcp user remove ",
    "mcp validate",
    "diagnostics",
    "diagnostics --changed",
    "restore snapshot",
    "restore list",
    "restore show ",
    "restore hunks ",
    "restore diff ",
    "restore hunk ",
    "restore hunk-apply ",
    "restore hunk-check ",
    "restore apply-hunk ",
    "restore check-hunk ",
    "restore revert-turn ",
    "revert turn ",
    "approval",
    "cancel",
    "help",
    "help mode",
    "help links",
    "help mcp",
    "?",
    "settings",
    "config",
];
const TUI_COMPOSER_SLASH_COMPLETIONS: &[&str] = &[
    "/help",
    "/help mode",
    "/help links",
    "/help mcp",
    "/?",
    "/settings",
    "/config",
    "/diff",
    "/diff help",
    "/clear",
    "/clear help",
    "/memory",
    "/memory show",
    "/memory path",
    "/memory clear",
    "/memory edit",
    "/memory help",
    "/subagents",
    "/agents",
    "/agent ",
    "/rlm ",
    "/recursive ",
    "/relay ",
    "/batonpass ",
    "/接力 ",
    "/note ",
    "/note add ",
    "/note list",
    "/note show ",
    "/note edit ",
    "/note remove ",
    "/note clear",
    "/note path",
    "/anchor ",
    "/anchor add ",
    "/anchor list",
    "/anchor remove ",
    "/anchor path",
    "/queue",
    "/queue list",
    "/queue edit ",
    "/queue drop ",
    "/queue clear",
    "/queued",
    "/share",
    "/share help",
    "/export",
    "/export ",
    "/export help",
    "/save",
    "/save ",
    "/save help",
    "/load ",
    "/load help",
    "/attach ",
    "/attach help",
    "/image ",
    "/media ",
    "/hooks",
    "/hooks list",
    "/hooks events",
    "/hook events",
    "/goal ",
    "/goal clear",
    "/stash",
    "/stash list",
    "/stash pop",
    "/stash clear",
    "/network",
    "/network allow ",
    "/network deny ",
    "/network remove ",
    "/network default ",
    "/lsp",
    "/lsp on",
    "/lsp off",
    "/lsp status",
    "/lsp help",
    "/status",
    "/statusline",
    "/statusline show",
    "/verbose",
    "/verbose on",
    "/verbose off",
    "/verbose show",
    "/tokens",
    "/context",
    "/ctx",
    "/cost",
    "/cache",
    "/cache inspect",
    "/cache warmup",
    "/change",
    "/change help",
    "/changelog",
    "/system",
    "/system help",
    "/edit",
    "/edit help",
    "/mode",
    "/mode agent",
    "/mode plan",
    "/mode yolo",
    "/mode 1",
    "/mode 2",
    "/mode 3",
    "/theme",
    "/theme show",
    "/theme dark",
    "/theme light",
    "/theme grayscale",
    "/theme system",
    "/exit",
    "/quit",
    "/q",
    "/model",
    "/model auto",
    "/model deepseek-v4-flash",
    "/model deepseek-v4-pro",
    "/models",
    "/provider",
    "/provider list",
    "/provider deepseek",
    "/provider nvidia-nim",
    "/provider openrouter",
    "/provider ollama",
    "/skills",
    "/skill ",
    "/feedback",
    "/feedback bug",
    "/feedback feature",
    "/feedback security",
    "/links",
    "/dashboard",
    "/api",
    "/home",
    "/stats",
    "/overview",
    "/rename ",
    "/init",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReasoningReplayPreferences {
    replay_limit: usize,
    pinned_turn_ids: BTreeSet<String>,
}

fn read_reasoning_replay_preferences(path: &Path) -> AppResult<Option<ReasoningReplayPreferences>> {
    if !path.is_file() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    let root = parse_root_object(&content)?;
    let replay_limit = root
        .get("replay_limit")
        .and_then(json_as_u64)
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_TUI_REASONING_REPLAY_LIMIT)
        .min(MAX_TUI_REASONING_REPLAY_LIMIT);
    let pinned_turn_ids = root
        .get("pinned_turn_ids")
        .and_then(json_as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(json_as_string)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    Ok(Some(ReasoningReplayPreferences {
        replay_limit,
        pinned_turn_ids,
    }))
}

fn write_reasoning_replay_preferences(
    path: &Path,
    replay_limit: usize,
    pinned_turn_ids: &BTreeSet<String>,
) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut root = BTreeMap::new();
    root.insert(
        "kind".to_string(),
        JsonValue::String(TUI_REASONING_REPLAY_PREF_KIND.to_string()),
    );
    root.insert(
        "replay_limit".to_string(),
        JsonValue::Number(replay_limit.min(MAX_TUI_REASONING_REPLAY_LIMIT).to_string()),
    );
    root.insert(
        "pinned_turn_ids".to_string(),
        JsonValue::Array(
            pinned_turn_ids
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    fs::write(path, json_value_to_string(&JsonValue::Object(root)))?;
    Ok(())
}

fn read_composer_stash(path: &Path) -> AppResult<Vec<ComposerStashEntry>> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path)?;
    if content.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value = parse_json_value(content.trim())?;
    let Some(items) = json_as_array(&value) else {
        return Ok(Vec::new());
    };
    let mut entries = Vec::new();
    for item in items {
        let Some(object) = json_as_object(item) else {
            continue;
        };
        let Some(text) = object.get("text").and_then(json_as_string) else {
            continue;
        };
        let created_at = object
            .get("created_at")
            .and_then(json_as_string)
            .unwrap_or("unknown")
            .to_string();
        entries.push(ComposerStashEntry {
            created_at,
            text: text.to_string(),
        });
    }
    if entries.len() > MAX_TUI_COMPOSER_STASH_ENTRIES {
        let overflow = entries.len() - MAX_TUI_COMPOSER_STASH_ENTRIES;
        entries.drain(0..overflow);
    }
    Ok(entries)
}

fn write_composer_stash(path: &Path, entries: &[ComposerStashEntry]) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let items = entries
        .iter()
        .map(|entry| {
            let mut object = BTreeMap::new();
            object.insert(
                "created_at".to_string(),
                JsonValue::String(entry.created_at.clone()),
            );
            object.insert("text".to_string(), JsonValue::String(entry.text.clone()));
            JsonValue::Object(object)
        })
        .collect::<Vec<_>>();
    fs::write(path, json_value_to_string(&JsonValue::Array(items)))?;
    Ok(())
}

fn composer_stash_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("epoch+{seconds}")
}

fn composer_stash_preview(text: &str, max_chars: usize) -> String {
    let first_line = text.lines().next().unwrap_or("").trim();
    if first_line.chars().count() <= max_chars {
        return first_line.to_string();
    }
    let mut preview = first_line
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    preview.push_str("...");
    preview
}

#[derive(Debug, Clone)]
pub struct TuiApp {
    mode: TuiMode,
    theme: TuiTheme,
    sessions: Vec<TuiSession>,
    threads: Vec<TuiThread>,
    items: Vec<TuiItem>,
    task_records: Vec<TuiTaskRecord>,
    automation_records: Vec<TuiAutomationRecord>,
    usage_summaries: Vec<TuiUsageSummary>,
    approvals: Vec<TuiApprovalRequest>,
    user_inputs: Vec<TuiUserInputRequest>,
    active_approval_id: Option<String>,
    dismissed_approval_ids: Vec<String>,
    pending_shell_approval: Option<String>,
    active_user_input_id: Option<String>,
    dismissed_user_input_ids: Vec<String>,
    user_input_answers: BTreeMap<String, String>,
    user_input_question_index: usize,
    user_input_other_mode: bool,
    user_input_other_value: String,
    selected_session: usize,
    selected_thread_id: Option<String>,
    selected_task_id: Option<String>,
    selected_task_ids: BTreeSet<String>,
    task_drag_anchor_id: Option<String>,
    session_picker_filter: String,
    thread_picker_filter: String,
    show_command_palette: bool,
    show_session_picker: bool,
    show_thread_picker: bool,
    show_approval_modal: bool,
    show_user_input_modal: bool,
    show_mcp_manager: bool,
    command_query: String,
    command_cursor: usize,
    command_history: Vec<String>,
    command_history_index: Option<usize>,
    command_history_draft: String,
    composer: String,
    composer_cursor: usize,
    composer_focused: bool,
    queued_messages: VecDeque<TuiQueuedMessage>,
    queued_draft: Option<TuiQueuedMessage>,
    composer_stash: Vec<ComposerStashEntry>,
    composer_stash_path: Option<PathBuf>,
    transcript_scroll: usize,
    verbose_transcript: bool,
    goal_objective: Option<String>,
    goal_token_budget: Option<u64>,
    goal_started_at: Option<SystemTime>,
    reasoning_replay_limit: usize,
    reasoning_replay_pinned_turn_ids: BTreeSet<String>,
    reasoning_replay_preferences_path: Option<PathBuf>,
    extra_slash_completions: Vec<String>,
    pending_actions: Vec<TuiAction>,
    status: String,
    mcp_detail: Option<(TuiMcpDetailKind, String)>,
    mcp_detail_scroll: usize,
    mcp_manager_filter: String,
    mcp_manager_selected_server: usize,
    mcp_manager_selected_server_keys: BTreeSet<String>,
    mcp_manager_drag_anchor_key: Option<String>,
    mcp_remove_confirmation: Option<TuiMcpPendingRemove>,
    rollback_apply_confirmation: Option<TuiRollbackPendingApply>,
    last_frame_area: Rect,
    transcript: Vec<String>,
    tasks: Vec<String>,
}

impl TuiApp {
    pub fn new(sessions: Vec<TuiSession>) -> Self {
        Self::with_runtime(sessions, Vec::new(), Vec::new())
    }

    pub fn with_runtime(
        sessions: Vec<TuiSession>,
        threads: Vec<TuiThread>,
        items: Vec<TuiItem>,
    ) -> Self {
        Self::with_runtime_usage_and_approvals(sessions, threads, items, Vec::new(), Vec::new())
    }

    pub fn with_runtime_and_approvals(
        sessions: Vec<TuiSession>,
        threads: Vec<TuiThread>,
        items: Vec<TuiItem>,
        approvals: Vec<TuiApprovalRequest>,
    ) -> Self {
        Self::with_runtime_usage_and_approvals(sessions, threads, items, Vec::new(), approvals)
    }

    pub fn with_runtime_usage_and_approvals(
        sessions: Vec<TuiSession>,
        threads: Vec<TuiThread>,
        items: Vec<TuiItem>,
        usage_summaries: Vec<TuiUsageSummary>,
        approvals: Vec<TuiApprovalRequest>,
    ) -> Self {
        Self::with_runtime_usage_tasks_and_approvals(
            sessions,
            threads,
            items,
            Vec::new(),
            usage_summaries,
            approvals,
        )
    }

    pub fn with_runtime_usage_tasks_and_approvals(
        sessions: Vec<TuiSession>,
        threads: Vec<TuiThread>,
        items: Vec<TuiItem>,
        task_records: Vec<TuiTaskRecord>,
        usage_summaries: Vec<TuiUsageSummary>,
        approvals: Vec<TuiApprovalRequest>,
    ) -> Self {
        Self::with_runtime_usage_tasks_automations_and_approvals(
            sessions,
            threads,
            items,
            task_records,
            Vec::new(),
            usage_summaries,
            approvals,
        )
    }

    pub fn with_runtime_usage_tasks_automations_and_approvals(
        sessions: Vec<TuiSession>,
        threads: Vec<TuiThread>,
        items: Vec<TuiItem>,
        task_records: Vec<TuiTaskRecord>,
        automation_records: Vec<TuiAutomationRecord>,
        usage_summaries: Vec<TuiUsageSummary>,
        approvals: Vec<TuiApprovalRequest>,
    ) -> Self {
        Self::with_runtime_usage_tasks_automations_approvals_and_user_inputs(
            sessions,
            threads,
            items,
            task_records,
            automation_records,
            usage_summaries,
            approvals,
            Vec::new(),
        )
    }

    pub fn with_runtime_usage_tasks_automations_approvals_and_user_inputs(
        sessions: Vec<TuiSession>,
        threads: Vec<TuiThread>,
        items: Vec<TuiItem>,
        task_records: Vec<TuiTaskRecord>,
        automation_records: Vec<TuiAutomationRecord>,
        usage_summaries: Vec<TuiUsageSummary>,
        approvals: Vec<TuiApprovalRequest>,
        user_inputs: Vec<TuiUserInputRequest>,
    ) -> Self {
        let sessions = if sessions.is_empty() {
            vec![TuiSession {
                id: "local".to_string(),
                title: "No durable sessions yet".to_string(),
                workspace: ".".to_string(),
                status: "empty".to_string(),
                active_thread_id: None,
                thread_count: 0,
            }]
        } else {
            sessions
        };
        let mut app = Self {
            mode: TuiMode::Plan,
            theme: TuiTheme::Dark,
            sessions,
            threads,
            items,
            task_records,
            automation_records,
            usage_summaries,
            approvals,
            user_inputs,
            active_approval_id: None,
            dismissed_approval_ids: Vec::new(),
            pending_shell_approval: None,
            active_user_input_id: None,
            dismissed_user_input_ids: Vec::new(),
            user_input_answers: BTreeMap::new(),
            user_input_question_index: 0,
            user_input_other_mode: false,
            user_input_other_value: String::new(),
            selected_session: 0,
            selected_thread_id: None,
            selected_task_id: None,
            selected_task_ids: BTreeSet::new(),
            task_drag_anchor_id: None,
            session_picker_filter: String::new(),
            thread_picker_filter: String::new(),
            show_command_palette: false,
            show_session_picker: false,
            show_thread_picker: false,
            show_approval_modal: false,
            show_user_input_modal: false,
            show_mcp_manager: false,
            command_query: String::new(),
            command_cursor: 0,
            command_history: Vec::new(),
            command_history_index: None,
            command_history_draft: String::new(),
            composer: String::new(),
            composer_cursor: 0,
            composer_focused: false,
            queued_messages: VecDeque::new(),
            queued_draft: None,
            composer_stash: Vec::new(),
            composer_stash_path: None,
            transcript_scroll: 0,
            verbose_transcript: false,
            goal_objective: None,
            goal_token_budget: None,
            goal_started_at: None,
            reasoning_replay_limit: DEFAULT_TUI_REASONING_REPLAY_LIMIT,
            reasoning_replay_pinned_turn_ids: BTreeSet::new(),
            reasoning_replay_preferences_path: None,
            extra_slash_completions: Vec::new(),
            pending_actions: Vec::new(),
            status: "ready".to_string(),
            mcp_detail: None,
            mcp_detail_scroll: 0,
            mcp_manager_filter: String::new(),
            mcp_manager_selected_server: 0,
            mcp_manager_selected_server_keys: BTreeSet::new(),
            mcp_manager_drag_anchor_key: None,
            mcp_remove_confirmation: None,
            rollback_apply_confirmation: None,
            last_frame_area: Rect::default(),
            transcript: Vec::new(),
            tasks: Vec::new(),
        };
        app.selected_thread_id = app.default_thread_id_for_selected_session();
        app.refresh_runtime_view();
        app.sync_user_input_modal();
        app.sync_approval_modal();
        app
    }

    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    pub fn reasoning_replay_limit(&self) -> usize {
        self.reasoning_replay_limit
    }

    pub fn reasoning_replay_pinned_turn_ids(&self) -> Vec<String> {
        self.reasoning_replay_pinned_turn_ids
            .iter()
            .cloned()
            .collect()
    }

    pub fn enable_reasoning_replay_preferences(&mut self, path: PathBuf) {
        self.reasoning_replay_preferences_path = Some(path.clone());
        match read_reasoning_replay_preferences(&path) {
            Ok(Some(preferences)) => {
                self.reasoning_replay_limit = preferences.replay_limit;
                self.reasoning_replay_pinned_turn_ids = preferences.pinned_turn_ids;
            }
            Ok(None) => {}
            Err(error) => {
                self.status = format!(
                    "failed to load reasoning replay preferences from {}: {error}",
                    path.display()
                );
            }
        }
    }

    pub fn set_extra_slash_completions<I, S>(&mut self, completions: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.extra_slash_completions = completions
            .into_iter()
            .map(Into::into)
            .filter(|completion| {
                completion.starts_with('/')
                    && completion
                        .chars()
                        .all(|ch| !ch.is_control() && ch != '\n' && ch != '\r')
            })
            .collect();
        self.extra_slash_completions.sort();
        self.extra_slash_completions.dedup();
    }

    #[cfg(test)]
    pub fn extra_slash_completions_for_test(&self) -> &[String] {
        &self.extra_slash_completions
    }

    pub fn enable_composer_stash(&mut self, path: PathBuf) {
        self.composer_stash_path = Some(path.clone());
        match read_composer_stash(&path) {
            Ok(entries) => {
                self.composer_stash = entries;
            }
            Err(error) => {
                self.status = format!(
                    "failed to load composer stash from {}: {error}",
                    path.display()
                );
            }
        }
    }

    fn persist_reasoning_replay_preferences(&mut self) {
        let Some(path) = self.reasoning_replay_preferences_path.as_ref() else {
            return;
        };
        if let Err(error) = write_reasoning_replay_preferences(
            path,
            self.reasoning_replay_limit,
            &self.reasoning_replay_pinned_turn_ids,
        ) {
            self.status = format!(
                "{}; failed to save replay preferences: {error}",
                self.status
            );
        }
    }

    pub fn set_mcp_detail(&mut self, kind: TuiMcpDetailKind, detail: impl Into<String>) {
        self.show_mcp_manager = false;
        self.mcp_detail = Some((kind, detail.into()));
        self.mcp_detail_scroll = 0;
        self.mcp_manager_selected_server = 0;
        self.mcp_manager_selected_server_keys.clear();
        self.mcp_manager_drag_anchor_key = None;
        self.mcp_remove_confirmation = None;
        self.rollback_apply_confirmation = None;
    }

    #[cfg(test)]
    pub(crate) fn mcp_detail_for_test(&self) -> Option<(TuiMcpDetailKind, &str)> {
        self.mcp_detail
            .as_ref()
            .map(|(kind, detail)| (*kind, detail.as_str()))
    }

    pub fn set_mcp_manager(&mut self, detail: impl Into<String>) {
        self.show_mcp_manager = true;
        self.mcp_detail = Some((TuiMcpDetailKind::Manager, detail.into()));
        self.mcp_detail_scroll = 0;
        self.mcp_manager_selected_server = 0;
        self.mcp_manager_selected_server_keys.clear();
        self.mcp_manager_drag_anchor_key = None;
        self.mcp_remove_confirmation = None;
        self.rollback_apply_confirmation = None;
    }

    pub fn set_mcp_manager_detail(&mut self, kind: TuiMcpDetailKind, detail: impl Into<String>) {
        self.show_mcp_manager = true;
        self.mcp_detail = Some((kind, detail.into()));
        self.mcp_detail_scroll = 0;
        self.mcp_manager_selected_server_keys.clear();
        self.mcp_manager_drag_anchor_key = None;
        self.mcp_remove_confirmation = None;
        self.rollback_apply_confirmation = None;
    }

    pub fn set_mcp_manager_filter(&mut self, filter: impl Into<String>) {
        self.mcp_manager_filter = filter.into();
        self.mcp_detail_scroll = 0;
        self.show_mcp_manager = true;
        self.mcp_manager_drag_anchor_key = None;
        if self.mcp_manager_filter.trim().is_empty() {
            self.status = "mcp manager filter cleared".to_string();
        } else {
            self.status = format!("mcp manager filter: {}", self.mcp_manager_filter);
        }
    }

    pub fn clear_mcp_detail(&mut self) {
        if self.mcp_detail.take().is_some() {
            self.show_mcp_manager = false;
            self.mcp_detail_scroll = 0;
            self.mcp_manager_selected_server_keys.clear();
            self.mcp_remove_confirmation = None;
            self.status = "mcp detail closed".to_string();
        } else {
            self.status = "no mcp detail open".to_string();
        }
    }

    pub fn apply_live_event(&mut self, event: TuiLiveEvent) {
        match event {
            TuiLiveEvent::UpsertItem(item) => {
                let was_busy = self.active_thread_busy();
                let should_refresh =
                    self.selected_thread_id.as_deref() == Some(item.thread_id.as_str());
                if let Some(existing) = self
                    .items
                    .iter_mut()
                    .find(|existing| existing.id == item.id)
                {
                    *existing = item;
                } else {
                    self.items.push(item);
                }
                if should_refresh {
                    self.refresh_runtime_view();
                }
                self.dispatch_next_queued_message_after_idle(was_busy);
            }
            TuiLiveEvent::ReplaceRuntime {
                sessions,
                threads,
                items,
                tasks,
                automations,
                usage_summaries,
                approvals,
                user_inputs,
            } => self.replace_runtime_with_usage_tasks_automations_approvals_and_user_inputs(
                sessions,
                threads,
                items,
                tasks,
                automations,
                usage_summaries,
                approvals,
                user_inputs,
            ),
            TuiLiveEvent::Status(status) => self.set_status(status),
        }
    }

    pub fn replace_runtime(
        &mut self,
        sessions: Vec<TuiSession>,
        threads: Vec<TuiThread>,
        items: Vec<TuiItem>,
    ) {
        self.replace_runtime_with_approvals(sessions, threads, items, Vec::new());
    }

    pub fn replace_runtime_with_approvals(
        &mut self,
        sessions: Vec<TuiSession>,
        threads: Vec<TuiThread>,
        items: Vec<TuiItem>,
        approvals: Vec<TuiApprovalRequest>,
    ) {
        self.replace_runtime_with_usage_and_approvals(
            sessions,
            threads,
            items,
            Vec::new(),
            approvals,
        );
    }

    pub fn replace_runtime_with_usage_and_approvals(
        &mut self,
        sessions: Vec<TuiSession>,
        threads: Vec<TuiThread>,
        items: Vec<TuiItem>,
        usage_summaries: Vec<TuiUsageSummary>,
        approvals: Vec<TuiApprovalRequest>,
    ) {
        self.replace_runtime_with_usage_tasks_and_approvals(
            sessions,
            threads,
            items,
            Vec::new(),
            usage_summaries,
            approvals,
        );
    }

    pub fn replace_runtime_with_usage_tasks_and_approvals(
        &mut self,
        sessions: Vec<TuiSession>,
        threads: Vec<TuiThread>,
        items: Vec<TuiItem>,
        task_records: Vec<TuiTaskRecord>,
        usage_summaries: Vec<TuiUsageSummary>,
        approvals: Vec<TuiApprovalRequest>,
    ) {
        self.replace_runtime_with_usage_tasks_automations_and_approvals(
            sessions,
            threads,
            items,
            task_records,
            Vec::new(),
            usage_summaries,
            approvals,
        );
    }

    pub fn replace_runtime_with_usage_tasks_automations_and_approvals(
        &mut self,
        sessions: Vec<TuiSession>,
        threads: Vec<TuiThread>,
        items: Vec<TuiItem>,
        task_records: Vec<TuiTaskRecord>,
        automation_records: Vec<TuiAutomationRecord>,
        usage_summaries: Vec<TuiUsageSummary>,
        approvals: Vec<TuiApprovalRequest>,
    ) {
        self.replace_runtime_with_usage_tasks_automations_approvals_and_user_inputs(
            sessions,
            threads,
            items,
            task_records,
            automation_records,
            usage_summaries,
            approvals,
            Vec::new(),
        );
    }

    pub fn replace_runtime_with_usage_tasks_automations_approvals_and_user_inputs(
        &mut self,
        sessions: Vec<TuiSession>,
        threads: Vec<TuiThread>,
        items: Vec<TuiItem>,
        task_records: Vec<TuiTaskRecord>,
        automation_records: Vec<TuiAutomationRecord>,
        usage_summaries: Vec<TuiUsageSummary>,
        approvals: Vec<TuiApprovalRequest>,
        user_inputs: Vec<TuiUserInputRequest>,
    ) {
        let was_busy = self.active_thread_busy();
        let previous_session_id = self.selected_session().map(|session| session.id.clone());
        let previous_thread_id = self.selected_thread_id.clone();
        let previous_counts = (
            self.sessions.len(),
            self.threads.len(),
            self.items.len(),
            self.task_records.len(),
            self.automation_records.len(),
            self.usage_summaries.len(),
            self.approvals.len(),
            self.user_inputs.len(),
        );

        self.sessions = if sessions.is_empty() {
            vec![TuiSession {
                id: "local".to_string(),
                title: "No durable sessions yet".to_string(),
                workspace: ".".to_string(),
                status: "empty".to_string(),
                active_thread_id: None,
                thread_count: 0,
            }]
        } else {
            sessions
        };
        self.threads = threads;
        self.items = items;
        self.task_records = task_records;
        self.automation_records = automation_records;
        self.usage_summaries = usage_summaries;
        self.approvals = approvals;
        self.user_inputs = user_inputs;

        self.selected_session = previous_session_id
            .and_then(|id| self.sessions.iter().position(|session| session.id == id))
            .unwrap_or(0);
        self.selected_thread_id = previous_thread_id.filter(|thread_id| {
            let Some(session) = self.selected_session() else {
                return false;
            };
            self.threads.iter().any(|thread| {
                thread.id == *thread_id && thread.session_id.as_deref() == Some(session.id.as_str())
            })
        });
        if self.selected_thread_id.is_none() {
            self.selected_thread_id = self.default_thread_id_for_selected_session();
        }
        self.ensure_selected_session_matches_filter();
        self.ensure_selected_thread_matches_filter();
        self.refresh_runtime_view();
        let opened_user_input = self.sync_user_input_modal();
        let opened_approval = if opened_user_input {
            false
        } else {
            self.sync_approval_modal()
        };

        let counts = (
            self.sessions.len(),
            self.threads.len(),
            self.items.len(),
            self.task_records.len(),
            self.automation_records.len(),
            self.usage_summaries.len(),
            self.approvals.len(),
            self.user_inputs.len(),
        );
        if counts != previous_counts && !opened_approval {
            let tasks = if counts.3 == 0 {
                String::new()
            } else {
                format!(" tasks={}", counts.3)
            };
            let automations = if counts.4 == 0 {
                String::new()
            } else {
                format!(" automations={}", counts.4)
            };
            let usage = if counts.5 == 0 {
                String::new()
            } else {
                format!(" usage={}", counts.5)
            };
            let user_inputs = if counts.7 == 0 {
                String::new()
            } else {
                format!(" user_inputs={}", counts.7)
            };
            self.status = if counts.6 == 0 {
                format!(
                    "runtime refreshed: sessions={} threads={} items={}{}{}{}{}",
                    counts.0, counts.1, counts.2, tasks, automations, usage, user_inputs
                )
            } else {
                format!(
                    "runtime refreshed: sessions={} threads={} items={}{}{}{} approvals={}{}",
                    counts.0, counts.1, counts.2, tasks, automations, usage, counts.6, user_inputs
                )
            };
        }
        self.dispatch_next_queued_message_after_idle(was_busy);
    }

    pub fn demo() -> Self {
        let sessions = vec![
            TuiSession {
                id: "session-demo-plan".to_string(),
                title: "Plan product parity".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-demo-plan".to_string()),
                thread_count: 3,
            },
            TuiSession {
                id: "session-demo-runtime".to_string(),
                title: "Runtime API smoke".to_string(),
                workspace: "fixtures/runtime".to_string(),
                status: "paused".to_string(),
                active_thread_id: Some("thread-demo-runtime".to_string()),
                thread_count: 2,
            },
        ];
        let threads = vec![
            TuiThread {
                id: "thread-demo-plan".to_string(),
                session_id: Some("session-demo-plan".to_string()),
                title: "Close TUI parity".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: Some("turn-demo-plan".to_string()),
                event_seq: 12,
            },
            TuiThread {
                id: "thread-demo-runtime".to_string(),
                session_id: Some("session-demo-runtime".to_string()),
                title: "Runtime API smoke".to_string(),
                mode: "agent".to_string(),
                status: "paused".to_string(),
                latest_turn_id: Some("turn-demo-runtime".to_string()),
                event_seq: 8,
            },
        ];
        let items = vec![
            TuiItem {
                id: "item-demo-plan".to_string(),
                thread_id: "thread-demo-plan".to_string(),
                turn_id: Some("turn-demo-plan".to_string()),
                index: 1,
                item_type: "message".to_string(),
                role: Some("assistant".to_string()),
                content:
                    "Plan/Agent/YOLO, approval, sessions, threads, and runtime refresh are visible."
                        .to_string(),
                status: "completed".to_string(),
            },
            TuiItem {
                id: "item-demo-runtime".to_string(),
                thread_id: "thread-demo-runtime".to_string(),
                turn_id: Some("turn-demo-runtime".to_string()),
                index: 1,
                item_type: "message".to_string(),
                role: Some("assistant".to_string()),
                content: "HTTP runtime snapshot loaded from durable records.".to_string(),
                status: "completed".to_string(),
            },
        ];
        let task_records = vec![TuiTaskRecord {
            id: "task-demo-progress".to_string(),
            session_id: Some("session-demo-plan".to_string()),
            thread_id: Some("thread-demo-plan".to_string()),
            parent_task_id: None,
            kind: "agent".to_string(),
            status: "running".to_string(),
            summary: "closing remaining TUI/runtime parity gaps".to_string(),
            updated_at: "epoch+1".to_string(),
        }];
        let automation_records = vec![TuiAutomationRecord {
            id: "automation-demo-nightly".to_string(),
            session_id: Some("session-demo-plan".to_string()),
            thread_id: Some("thread-demo-plan".to_string()),
            name: "Nightly diagnostics".to_string(),
            status: "active".to_string(),
            schedule: "daily".to_string(),
            prompt: "run diagnostics and summarize failures".to_string(),
            updated_at: "epoch+1".to_string(),
            last_run_at: None,
            next_run_at: Some("epoch+86400".to_string()),
        }];
        let mut app = Self::with_runtime_usage_tasks_automations_and_approvals(
            sessions,
            threads,
            items,
            task_records,
            automation_records,
            Vec::new(),
            Vec::new(),
        );
        app.show_command_palette = true;
        app.show_session_picker = true;
        app.show_thread_picker = true;
        app.show_approval_modal = true;
        app.command_query = "mode agent".to_string();
        app.command_cursor = app.command_query.len();
        app.status = "demo surfaces visible".to_string();
        app
    }

    fn selected_session(&self) -> Option<&TuiSession> {
        self.sessions.get(self.selected_session)
    }

    fn filtered_session_indices(&self) -> Vec<usize> {
        let filter = self.session_picker_filter.trim();
        self.sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| session_matches_filter(session, filter))
            .map(|(index, _)| index)
            .collect()
    }

    fn selected_session_picker_index(&self) -> Option<usize> {
        self.filtered_session_indices()
            .iter()
            .position(|index| *index == self.selected_session)
    }

    fn select_session_by_picker_index(&mut self, index: usize) {
        let sessions = self.filtered_session_indices();
        if sessions.is_empty() {
            self.status = if self.session_picker_filter.trim().is_empty() {
                "no sessions available".to_string()
            } else {
                format!(
                    "no sessions match filter: {}",
                    self.session_picker_filter.trim()
                )
            };
            return;
        }
        let session_index = sessions[index.min(sessions.len().saturating_sub(1))];
        self.select_session(session_index);
        self.ensure_selected_thread_matches_filter();
    }

    fn select_relative_session(&mut self, offset: isize) {
        let sessions = self.filtered_session_indices();
        if sessions.is_empty() {
            self.status = if self.session_picker_filter.trim().is_empty() {
                "no sessions available".to_string()
            } else {
                format!(
                    "no sessions match filter: {}",
                    self.session_picker_filter.trim()
                )
            };
            return;
        }
        let current = self.selected_session_picker_index().unwrap_or(0);
        let next = if offset < 0 {
            current.saturating_sub(offset.unsigned_abs())
        } else {
            current.saturating_add(offset as usize)
        }
        .min(sessions.len().saturating_sub(1));
        self.select_session_by_picker_index(next);
    }

    fn ensure_selected_session_matches_filter(&mut self) {
        let filter = self.session_picker_filter.trim();
        if filter.is_empty() {
            return;
        }
        let sessions = self.filtered_session_indices();
        if sessions.is_empty() || sessions.contains(&self.selected_session) {
            return;
        }
        self.select_session(sessions[0]);
    }

    fn set_session_picker_filter(&mut self, filter: impl Into<String>) {
        self.session_picker_filter = filter.into();
        self.show_session_picker = true;
        self.show_thread_picker = false;
        self.ensure_selected_session_matches_filter();
        self.ensure_selected_thread_matches_filter();
        let filter = self.session_picker_filter.trim();
        if filter.is_empty() {
            self.status = "session filter cleared".to_string();
        } else {
            let count = self.filtered_session_indices().len();
            self.status = format!("session filter: {filter} ({count} match)");
        }
    }

    fn default_thread_id_for_selected_session(&self) -> Option<String> {
        let session = self.selected_session()?;
        if let Some(active_id) = session.active_thread_id.as_deref() {
            if self.threads.iter().any(|thread| {
                thread.id == active_id && thread.session_id.as_deref() == Some(session.id.as_str())
            }) {
                return Some(active_id.to_string());
            }
        }
        self.threads
            .iter()
            .find(|thread| thread.session_id.as_deref() == Some(session.id.as_str()))
            .map(|thread| thread.id.clone())
    }

    fn active_thread(&self) -> Option<&TuiThread> {
        let thread_id = self.selected_thread_id.as_deref()?;
        self.threads.iter().find(|thread| thread.id == thread_id)
    }

    fn threads_for_selected_session(&self) -> Vec<&TuiThread> {
        let Some(session) = self.selected_session() else {
            return Vec::new();
        };
        self.threads
            .iter()
            .filter(|thread| thread.session_id.as_deref() == Some(session.id.as_str()))
            .collect()
    }

    fn filtered_threads_for_selected_session(&self) -> Vec<&TuiThread> {
        let filter = self.thread_picker_filter.trim();
        self.threads_for_selected_session()
            .into_iter()
            .filter(|thread| thread_matches_filter(thread, filter))
            .collect()
    }

    fn ensure_selected_thread_matches_filter(&mut self) {
        let filter = self.thread_picker_filter.trim();
        if filter.is_empty() {
            return;
        }
        let selected_thread_id = self.selected_thread_id.as_deref();
        let threads = self.filtered_threads_for_selected_session();
        if threads.is_empty()
            || selected_thread_id
                .is_some_and(|thread_id| threads.iter().any(|thread| thread.id == thread_id))
        {
            return;
        }
        let thread_id = threads[0].id.clone();
        self.selected_thread_id = Some(thread_id);
        self.transcript_scroll = 0;
        self.refresh_runtime_view();
    }

    fn set_thread_picker_filter(&mut self, filter: impl Into<String>) {
        self.thread_picker_filter = filter.into();
        self.show_thread_picker = true;
        self.show_session_picker = false;
        self.ensure_selected_thread_matches_filter();
        let filter = self.thread_picker_filter.trim();
        if filter.is_empty() {
            self.status = "thread filter cleared".to_string();
        } else {
            let count = self.filtered_threads_for_selected_session().len();
            self.status = format!("thread filter: {filter} ({count} match)");
        }
    }

    fn selected_thread_index_for_session(&self) -> Option<usize> {
        let selected_thread_id = self.selected_thread_id.as_deref()?;
        self.filtered_threads_for_selected_session()
            .iter()
            .position(|thread| thread.id == selected_thread_id)
    }

    fn active_thread_items(&self) -> Vec<&TuiItem> {
        let Some(thread) = self.active_thread() else {
            return Vec::new();
        };
        let mut items = self
            .items
            .iter()
            .filter(|item| item.thread_id == thread.id)
            .collect::<Vec<_>>();
        items.sort_by_key(|item| item.index);
        items
    }

    fn active_reasoning_items(&self) -> Vec<&TuiItem> {
        self.active_thread_items()
            .into_iter()
            .filter(|item| item.item_type == "reasoning")
            .collect()
    }

    fn active_running_assistant_item(&self) -> Option<&TuiItem> {
        self.active_thread_items().into_iter().rev().find(|item| {
            item.status == "running"
                && item.item_type == "message"
                && item.role.as_deref() == Some("assistant")
        })
    }

    fn active_thread_busy(&self) -> bool {
        self.active_running_assistant_item().is_some()
    }

    fn active_approval(&self) -> Option<&TuiApprovalRequest> {
        let approval_id = self.active_approval_id.as_deref()?;
        self.approvals
            .iter()
            .find(|approval| approval.id == approval_id)
    }

    fn active_user_input(&self) -> Option<&TuiUserInputRequest> {
        let request_id = self.active_user_input_id.as_deref()?;
        self.user_inputs
            .iter()
            .find(|request| request.id == request_id)
    }

    fn active_usage_summary(&self) -> Option<&TuiUsageSummary> {
        let thread_id = self.selected_thread_id.as_deref()?;
        self.usage_summaries
            .iter()
            .find(|summary| summary.thread_id == thread_id)
    }

    fn active_thread_tasks(&self) -> Vec<&TuiTaskRecord> {
        let thread_id = self.selected_thread_id.as_deref();
        let mut tasks = self
            .task_records
            .iter()
            .filter(|task| task.thread_id.as_deref() == thread_id)
            .collect::<Vec<_>>();
        tasks.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        tasks
    }

    fn active_task_by_id(&self, task_id: &str) -> Option<&TuiTaskRecord> {
        self.active_thread_tasks()
            .into_iter()
            .find(|task| task.id == task_id)
    }

    fn selected_task(&self) -> Option<&TuiTaskRecord> {
        let task_id = self.selected_task_id.as_deref()?;
        self.active_task_by_id(task_id)
    }

    fn ensure_selected_task(&mut self) {
        let tasks = self.active_thread_tasks();
        if tasks.is_empty() {
            self.selected_task_id = None;
            self.selected_task_ids.clear();
            self.task_drag_anchor_id = None;
            return;
        }
        if self
            .selected_task_id
            .as_ref()
            .is_some_and(|task_id| tasks.iter().any(|task| task.id == *task_id))
        {
            return;
        }
        self.selected_task_id = Some(tasks[0].id.clone());
    }

    fn retain_active_task_selection(&mut self) {
        let active_task_ids = self
            .active_thread_tasks()
            .into_iter()
            .map(|task| task.id.clone())
            .collect::<BTreeSet<_>>();
        self.selected_task_ids
            .retain(|task_id| active_task_ids.contains(task_id));
        if self
            .task_drag_anchor_id
            .as_ref()
            .is_some_and(|task_id| !active_task_ids.contains(task_id))
        {
            self.task_drag_anchor_id = None;
        }
    }

    fn task_panel_task_index_for_row(&self, row_index: usize) -> Option<usize> {
        const TASK_PANEL_HEADER_LINES: usize = 2;
        const TASK_PANEL_LINES_PER_TASK: usize = 3;

        let task_row =
            row_index.checked_sub(self.task_panel_base_line_count() + TASK_PANEL_HEADER_LINES)?;
        let task_index = task_row / TASK_PANEL_LINES_PER_TASK;
        if task_index < self.active_thread_tasks().len().min(4) {
            Some(task_index)
        } else {
            None
        }
    }

    fn task_panel_base_line_count(&self) -> usize {
        4 + runtime_item_progress_lines(&self.active_thread_items()).len()
    }

    fn select_task_by_index(&mut self, index: usize) {
        let selected = self.active_thread_tasks().get(index).map(|task| {
            (
                task.id.clone(),
                task.status.clone(),
                clip_line(&task.summary, 60),
            )
        });
        if let Some((task_id, status, summary)) = selected {
            self.selected_task_id = Some(task_id.clone());
            self.refresh_runtime_view();
            self.status = format!("selected task: {task_id} [{status}] {summary}");
        } else {
            self.status = "no task at selected row".to_string();
        }
    }

    fn toggle_task_selection_by_index(&mut self, index: usize) {
        let selected = self.active_thread_tasks().get(index).map(|task| {
            (
                task.id.clone(),
                task.status.clone(),
                clip_line(&task.summary, 60),
            )
        });
        let Some((task_id, status, summary)) = selected else {
            self.status = "no task at selected row".to_string();
            return;
        };
        self.selected_task_id = Some(task_id.clone());
        self.task_drag_anchor_id = Some(task_id.clone());
        if self.selected_task_ids.remove(&task_id) {
            self.refresh_runtime_view();
            self.status = format!(
                "task unselected: {task_id} [{status}] {} selected",
                self.selected_task_ids.len()
            );
        } else {
            self.selected_task_ids.insert(task_id.clone());
            self.refresh_runtime_view();
            self.status = format!(
                "task selected for bulk action: {task_id} [{status}] {summary} ({} selected)",
                self.selected_task_ids.len()
            );
        }
    }

    fn drag_select_task_by_index(&mut self, index: usize) {
        let task_ids = self
            .active_thread_tasks()
            .into_iter()
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        let Some(current_id) = task_ids.get(index).cloned() else {
            self.status = "no task at selected row".to_string();
            return;
        };
        let anchor_id = self
            .task_drag_anchor_id
            .get_or_insert_with(|| current_id.clone())
            .clone();
        let anchor_index = task_ids.iter().position(|task_id| task_id == &anchor_id);
        let current_index = task_ids.iter().position(|task_id| task_id == &current_id);
        let selected_ids = match (anchor_index, current_index) {
            (Some(anchor), Some(current)) => {
                let start = anchor.min(current);
                let end = anchor
                    .max(current)
                    .min(3)
                    .min(task_ids.len().saturating_sub(1));
                task_ids[start..=end].to_vec()
            }
            _ => vec![anchor_id, current_id.clone()],
        };
        for task_id in selected_ids {
            self.selected_task_ids.insert(task_id);
        }
        self.select_task_by_index(index);
        self.status = format!(
            "task drag selected range: {} selected",
            self.selected_task_ids.len()
        );
    }

    fn select_task_by_id(&mut self, task_id: &str) -> bool {
        let selected = self.active_task_by_id(task_id).map(|task| {
            (
                task.id.clone(),
                task.status.clone(),
                clip_line(&task.summary, 60),
            )
        });
        let Some((task_id, status, summary)) = selected else {
            self.status = format!("task not found in active thread: {task_id}");
            return false;
        };
        self.selected_task_id = Some(task_id.clone());
        self.refresh_runtime_view();
        self.status = format!("selected task: {task_id} [{status}] {summary}");
        true
    }

    fn select_relative_task(&mut self, offset: isize) {
        let tasks = self.active_thread_tasks();
        if tasks.is_empty() {
            self.selected_task_id = None;
            self.status = "no runtime tasks in active thread".to_string();
            return;
        }
        let current = self
            .selected_task_id
            .as_ref()
            .and_then(|task_id| tasks.iter().position(|task| task.id == *task_id))
            .unwrap_or(0);
        let next = if offset < 0 {
            current.saturating_sub(offset.unsigned_abs())
        } else {
            current.saturating_add(offset as usize)
        }
        .min(tasks.len().saturating_sub(1));
        drop(tasks);
        self.select_task_by_index(next);
    }

    fn default_task_for_statuses(&self, statuses: &[&str]) -> Option<String> {
        if let Some(task) = self.selected_task() {
            if statuses.contains(&task.status.as_str()) {
                return Some(task.id.clone());
            }
        }
        let tasks = self.active_thread_tasks();
        statuses.iter().find_map(|status| {
            tasks
                .iter()
                .find(|task| task.status == *status)
                .map(|task| task.id.clone())
        })
    }

    fn selected_task_ids_for_statuses(&self, statuses: &[&str]) -> Vec<String> {
        if self.selected_task_ids.is_empty() {
            return Vec::new();
        }
        self.active_thread_tasks()
            .into_iter()
            .filter(|task| self.selected_task_ids.contains(&task.id))
            .filter(|task| statuses.contains(&task.status.as_str()))
            .map(|task| task.id.clone())
            .collect()
    }

    fn select_visible_tasks(&mut self) {
        let tasks = self.active_thread_tasks();
        let visible_ids = tasks
            .iter()
            .take(4)
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        drop(tasks);
        if visible_ids.is_empty() {
            self.status = "no visible runtime tasks to select".to_string();
            return;
        }
        for task_id in &visible_ids {
            self.selected_task_ids.insert(task_id.clone());
        }
        self.selected_task_id = Some(visible_ids[0].clone());
        self.refresh_runtime_view();
        self.status = format!("selected {} visible task(s)", visible_ids.len());
    }

    fn clear_task_selection(&mut self) {
        let count = self.selected_task_ids.len();
        self.selected_task_ids.clear();
        self.task_drag_anchor_id = None;
        self.refresh_runtime_view();
        self.status = format!("cleared {count} selected task(s)");
    }

    fn active_thread_automations(&self) -> Vec<&TuiAutomationRecord> {
        let thread_id = self.selected_thread_id.as_deref();
        let mut automations = self
            .automation_records
            .iter()
            .filter(|automation| automation.thread_id.as_deref() == thread_id)
            .collect::<Vec<_>>();
        automations.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        automations
    }

    fn active_automation_by_id(&self, automation_id: &str) -> Option<&TuiAutomationRecord> {
        self.active_thread_automations()
            .into_iter()
            .find(|automation| automation.id == automation_id)
    }

    fn next_pending_approval_id(&self) -> Option<String> {
        self.approvals
            .iter()
            .find(|approval| {
                approval.is_pending()
                    && !self
                        .dismissed_approval_ids
                        .iter()
                        .any(|id| id == &approval.id)
            })
            .map(|approval| approval.id.clone())
    }

    fn next_pending_user_input_id(&self) -> Option<String> {
        self.user_inputs
            .iter()
            .find(|request| {
                request.is_pending()
                    && !self
                        .dismissed_user_input_ids
                        .iter()
                        .any(|id| id == &request.id)
            })
            .map(|request| request.id.clone())
    }

    fn sync_user_input_modal(&mut self) -> bool {
        let had_active = self.active_user_input_id.is_some();
        if let Some(active_id) = self.active_user_input_id.as_deref() {
            let still_pending = self.user_inputs.iter().any(|request| {
                request.id == active_id
                    && request.is_pending()
                    && !self
                        .dismissed_user_input_ids
                        .iter()
                        .any(|dismissed_id| dismissed_id == active_id)
            });
            if still_pending {
                return false;
            }
        }

        self.active_user_input_id = self.next_pending_user_input_id();
        self.user_input_answers.clear();
        self.user_input_question_index = 0;
        self.user_input_other_mode = false;
        self.user_input_other_value.clear();
        if let Some(request) = self.active_user_input() {
            let first_question = request
                .questions
                .first()
                .map(|question| question.header.clone())
                .unwrap_or_else(|| "Input".to_string());
            self.show_user_input_modal = true;
            self.status = format!("user input requested: {}", clip_line(&first_question, 80));
            true
        } else {
            if had_active {
                self.show_user_input_modal = false;
            }
            false
        }
    }

    fn sync_approval_modal(&mut self) -> bool {
        if self.pending_shell_approval.is_some() {
            return false;
        }
        let had_active_approval = self.active_approval_id.is_some();
        if let Some(active_id) = self.active_approval_id.as_deref() {
            let still_pending = self.approvals.iter().any(|approval| {
                approval.id == active_id
                    && approval.is_pending()
                    && !self
                        .dismissed_approval_ids
                        .iter()
                        .any(|dismissed_id| dismissed_id == active_id)
            });
            if still_pending {
                return false;
            }
        }

        self.active_approval_id = self.next_pending_approval_id();
        if let Some(approval) = self.active_approval() {
            let kind = approval.kind.clone();
            let target = approval.target.clone();
            self.show_approval_modal = true;
            self.status = format!("approval requested: {} {}", kind, clip_line(&target, 80));
            true
        } else {
            if had_active_approval {
                self.show_approval_modal = false;
            }
            false
        }
    }

    fn select_session(&mut self, index: usize) {
        self.selected_session = index.min(self.sessions.len().saturating_sub(1));
        self.selected_thread_id = self.default_thread_id_for_selected_session();
        self.transcript_scroll = 0;
        self.refresh_runtime_view();
    }

    fn select_thread_by_index(&mut self, index: usize) {
        let selected = {
            let threads = self.filtered_threads_for_selected_session();
            if threads.is_empty() {
                None
            } else {
                let thread = threads[index.min(threads.len().saturating_sub(1))];
                Some((thread.id.clone(), thread.title.clone()))
            }
        };
        if let Some((thread_id, title)) = selected {
            self.selected_thread_id = Some(thread_id.clone());
            self.transcript_scroll = 0;
            self.refresh_runtime_view();
            self.status = format!("selected thread: {} {}", thread_id, clip_line(&title, 60));
        } else {
            self.status = "no threads in selected session".to_string();
        }
    }

    pub fn select_thread_by_id(&mut self, thread_id: &str) -> bool {
        let selected = self
            .threads
            .iter()
            .find(|thread| thread.id == thread_id)
            .cloned();
        let Some(thread) = selected else {
            self.status = format!("thread not found: {thread_id}");
            return false;
        };
        if let Some(session_id) = thread.session_id.as_deref() {
            if let Some(index) = self
                .sessions
                .iter()
                .position(|session| session.id == session_id)
            {
                self.selected_session = index;
            }
        }
        self.selected_thread_id = Some(thread.id.clone());
        self.transcript_scroll = 0;
        self.refresh_runtime_view();
        self.status = format!(
            "selected thread: {} {}",
            thread.id,
            clip_line(&thread.title, 60)
        );
        true
    }

    pub fn clear_transient_conversation_state(&mut self) {
        self.queued_messages.clear();
        self.queued_draft = None;
        self.composer.clear();
        self.composer_cursor = 0;
        self.transcript_scroll = 0;
        self.mcp_detail = None;
        self.mcp_detail_scroll = 0;
        self.selected_task_ids.clear();
    }

    fn select_relative_thread(&mut self, offset: isize) {
        let len = self.filtered_threads_for_selected_session().len();
        if len == 0 {
            self.status = if self.thread_picker_filter.trim().is_empty() {
                "no threads in selected session".to_string()
            } else {
                format!(
                    "no threads match filter: {}",
                    self.thread_picker_filter.trim()
                )
            };
            return;
        }
        let current = self.selected_thread_index_for_session().unwrap_or(0);
        let next = if offset < 0 {
            current.saturating_sub(offset.unsigned_abs()).min(len - 1)
        } else {
            current.saturating_add(offset as usize).min(len - 1)
        };
        self.select_thread_by_index(next);
    }

    fn refresh_runtime_view(&mut self) {
        let Some(thread) = self.active_thread().cloned() else {
            self.selected_task_id = None;
            self.selected_task_ids.clear();
            self.task_drag_anchor_id = None;
            self.transcript = vec![
                "DeepSeekCode TUI shell".to_string(),
                "Use Tab or p/a/y to switch Plan, Agent, and YOLO modes.".to_string(),
                "Press : for command palette, s for sessions, ! for approval modal.".to_string(),
            ];
            self.tasks = vec![
                "Wire real agent loop streaming into the transcript".to_string(),
                "Bind approval modal to write/shell/MCP permission requests".to_string(),
                "Add live runtime refresh for session and thread updates".to_string(),
            ];
            return;
        };
        self.ensure_selected_task();
        self.retain_active_task_selection();

        let (transcript, item_count, item_progress_lines) = {
            let items = self.active_thread_items();
            let item_count = items.len();
            let item_progress_lines = runtime_item_progress_lines(&items);
            let transcript = if items.is_empty() {
                vec![
                    format!("Thread: {}", thread.title),
                    "No durable items recorded for this thread yet.".to_string(),
                ]
            } else {
                items
                    .iter()
                    .flat_map(|item| format_transcript_item_lines(item, self.verbose_transcript))
                    .collect()
            };
            (transcript, item_count, item_progress_lines)
        };
        self.transcript = transcript;
        self.tasks = vec![
            format!("Active thread: {}", thread.title),
            format!("Thread mode/status: {} / {}", thread.mode, thread.status),
            format!("Runtime items: {item_count}"),
            format!("Reasoning replay: latest {}", self.reasoning_replay_limit),
            format!("Event seq: {}", thread.event_seq),
        ];
        self.tasks.extend(item_progress_lines);
        let active_task_lines = {
            let active_tasks = self.active_thread_tasks();
            if active_tasks.is_empty() {
                Vec::new()
            } else {
                let selected_task_id = self.selected_task_id.as_deref();
                let selected_task_ids = self.selected_task_ids.clone();
                let mut lines = vec![format!("Runtime tasks: {}", active_tasks.len())];
                if !selected_task_ids.is_empty() {
                    lines.push(format!("Selected tasks: {}", selected_task_ids.len()));
                }
                lines.push(task_status_counts_line(&active_tasks));
                lines.extend(active_tasks.iter().take(4).flat_map(|task| {
                    task_progress_lines(
                        task,
                        selected_task_id == Some(task.id.as_str()),
                        selected_task_ids.contains(&task.id),
                    )
                }));
                lines
            }
        };
        if !active_task_lines.is_empty() {
            self.tasks.extend(active_task_lines);
        }
        let active_automation_lines = {
            let active_automations = self.active_thread_automations();
            if active_automations.is_empty() {
                Vec::new()
            } else {
                let mut lines = vec![format!("Automations: {}", active_automations.len())];
                lines.extend(active_automations.iter().take(3).map(|automation| {
                    format!(
                        "Automation {} [{}]: {}",
                        automation.name,
                        automation.status,
                        clip_line(&automation.schedule, 50)
                    )
                }));
                lines
            }
        };
        if !active_automation_lines.is_empty() {
            self.tasks.extend(active_automation_lines);
        }
        if let Some(summary) = self.active_usage_summary().cloned() {
            self.tasks.push(format!(
                "Usage total: {} tokens in {} records",
                summary.total_tokens, summary.record_count
            ));
            self.tasks.push(format!(
                "Context: {} remaining / {}",
                summary.context_remaining_tokens, summary.context_strategy
            ));
            if matches!(
                summary.context_strategy.as_str(),
                "prepare_compaction" | "must_compact_or_chunk"
            ) {
                self.tasks
                    .push("Compact active thread: :compact [tail]".to_string());
            }
            self.tasks.push(format!(
                "Cache hit: {} / {} ({})",
                summary.prompt_cache_hit_tokens,
                summary
                    .prompt_cache_hit_tokens
                    .saturating_add(summary.prompt_cache_miss_tokens),
                format_cache_hit_rate(
                    summary.prompt_cache_hit_tokens,
                    summary.prompt_cache_miss_tokens
                )
            ));
            self.tasks.push(format!(
                "Cache chart: {}",
                format_ratio_bar(
                    summary.prompt_cache_hit_tokens,
                    summary.prompt_cache_miss_tokens,
                    18,
                    '#',
                    '.'
                )
            ));
            if let Some(cost) = summary.estimated_total_cost_microusd {
                self.tasks
                    .push(format!("Est. cost: {}", format_microusd(cost)));
                if let (Some(input), Some(output)) = (
                    summary.estimated_input_cost_microusd,
                    summary.estimated_output_cost_microusd,
                ) {
                    self.tasks.push(format!(
                        "Cost split: in {} / out {}",
                        format_microusd(input),
                        format_microusd(output)
                    ));
                    self.tasks.push(format!(
                        "Cost chart: {}",
                        format_ratio_bar(input, output, 18, 'i', 'o')
                    ));
                }
            } else {
                self.tasks.push("Est. cost: unpriced model".to_string());
            }
        }
        if let Some(running) = self.active_running_assistant_item() {
            self.tasks.push(format!(
                "Running assistant: {} chars streamed",
                running.content.chars().count()
            ));
            self.tasks.push("Cancel active run: c".to_string());
        }
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C')) {
                return false;
            }
            if self.composer_focused && matches!(key.code, KeyCode::Char('s') | KeyCode::Char('S'))
            {
                self.stash_composer_draft();
                return true;
            }
            if self.show_command_palette {
                let _ = handle_text_control_key(
                    &mut self.command_query,
                    &mut self.command_cursor,
                    key.code,
                );
                return true;
            }
            if self.composer_focused {
                let _ = handle_text_control_key(
                    &mut self.composer,
                    &mut self.composer_cursor,
                    key.code,
                );
                return true;
            }
            return true;
        }

        self.handle_key(key.code)
    }

    fn handle_mouse_event(&mut self, mouse: MouseEvent) -> bool {
        let drag_select = match mouse.kind {
            MouseEventKind::ScrollUp => return self.handle_key(KeyCode::PageUp),
            MouseEventKind::ScrollDown => return self.handle_key(KeyCode::PageDown),
            MouseEventKind::Down(MouseButton::Left) => {
                self.mcp_manager_drag_anchor_key = None;
                self.task_drag_anchor_id = None;
                false
            }
            MouseEventKind::Drag(MouseButton::Left) => true,
            MouseEventKind::Up(MouseButton::Left) => {
                self.mcp_manager_drag_anchor_key = None;
                self.task_drag_anchor_id = None;
                return true;
            }
            _ => return true,
        };

        if drag_select {
            if self.show_mcp_manager {
                self.handle_mcp_manager_mouse(mouse.column, mouse.row, mouse.modifiers, true);
            } else {
                self.handle_body_mouse(mouse.column, mouse.row, mouse.modifiers, true);
            }
            return true;
        }

        if self.show_session_picker && self.handle_session_picker_mouse(mouse.column, mouse.row) {
            return true;
        }
        if self.show_thread_picker && self.handle_thread_picker_mouse(mouse.column, mouse.row) {
            return true;
        }
        if self.handle_mode_tab_mouse(mouse.column, mouse.row) {
            return true;
        }
        if self.show_mcp_manager
            && self.handle_mcp_manager_mouse(mouse.column, mouse.row, mouse.modifiers, false)
        {
            return true;
        }
        if self.handle_body_mouse(mouse.column, mouse.row, mouse.modifiers, false) {
            return true;
        }
        true
    }

    fn handle_mode_tab_mouse(&mut self, column: u16, row: u16) -> bool {
        let Some((tabs, _, _)) = self.frame_layout() else {
            return false;
        };
        if !point_in_rect(column, row, tabs) {
            return false;
        }
        let relative_x = column
            .saturating_sub(tabs.x)
            .min(tabs.width.saturating_sub(1));
        let third = (tabs.width / 3).max(1);
        self.mode = match relative_x / third {
            0 => TuiMode::Plan,
            1 => TuiMode::Agent,
            _ => TuiMode::Yolo,
        };
        self.status = format!("mode set: {}", self.mode.title());
        true
    }

    fn handle_body_mouse(
        &mut self,
        column: u16,
        row: u16,
        modifiers: KeyModifiers,
        drag_select: bool,
    ) -> bool {
        let Some((_, body, _)) = self.frame_layout() else {
            return false;
        };
        if !point_in_rect(column, row, body) || self.show_mcp_manager {
            return false;
        }
        let columns = body_columns(body);
        if point_in_rect(column, row, columns[1]) {
            if !drag_select {
                self.composer_focused = true;
                self.status = "composer focused".to_string();
            }
            return true;
        }
        if point_in_rect(column, row, columns[2]) {
            return self.handle_task_panel_mouse(column, row, columns[2], modifiers, drag_select);
        }
        false
    }

    fn handle_task_panel_mouse(
        &mut self,
        column: u16,
        row: u16,
        area: Rect,
        modifiers: KeyModifiers,
        drag_select: bool,
    ) -> bool {
        let Some(row_index) = block_row_index(column, row, area) else {
            return false;
        };
        let Some(task_index) = self.task_panel_task_index_for_row(row_index) else {
            if !drag_select {
                self.status = "task panel focused".to_string();
            }
            return true;
        };
        if drag_select {
            self.drag_select_task_by_index(task_index);
        } else if modifiers.contains(KeyModifiers::CONTROL) {
            self.toggle_task_selection_by_index(task_index);
        } else {
            let task_id = self
                .active_thread_tasks()
                .get(task_index)
                .map(|task| task.id.clone());
            self.task_drag_anchor_id = task_id;
            self.select_task_by_index(task_index);
        }
        true
    }

    fn handle_session_picker_mouse(&mut self, column: u16, row: u16) -> bool {
        let area = session_picker_rect(self.last_frame_area);
        let Some(row_index) = block_row_index(column, row, area) else {
            return false;
        };
        let visible_index = if self.session_picker_filter.trim().is_empty() {
            row_index
        } else if row_index == 0 {
            return true;
        } else {
            row_index - 1
        };
        if self.filtered_session_indices().get(visible_index).is_none() {
            return true;
        }
        self.select_session_by_picker_index(visible_index);
        self.show_session_picker = false;
        if let Some(session) = self.selected_session() {
            self.status = format!("selected session: {}", session.id);
        }
        true
    }

    fn handle_thread_picker_mouse(&mut self, column: u16, row: u16) -> bool {
        let area = thread_picker_rect(self.last_frame_area);
        let Some(row_index) = block_row_index(column, row, area) else {
            return false;
        };
        let visible_index = if self.thread_picker_filter.trim().is_empty() {
            row_index
        } else if row_index == 0 {
            return true;
        } else {
            row_index - 1
        };
        if self
            .filtered_threads_for_selected_session()
            .get(visible_index)
            .is_none()
        {
            return true;
        }
        self.select_thread_by_index(visible_index);
        self.show_thread_picker = false;
        true
    }

    fn handle_mcp_manager_mouse(
        &mut self,
        column: u16,
        row: u16,
        modifiers: KeyModifiers,
        drag_select: bool,
    ) -> bool {
        let Some((_, body, _)) = self.frame_layout() else {
            return false;
        };
        if !point_in_rect(column, row, body) {
            return false;
        }
        let Some(inner_row) = block_row_index(column, row, body) else {
            return true;
        };
        let content_row = inner_row.saturating_add(self.mcp_detail_scroll);
        let column_offset = usize::from(column.saturating_sub(body.x.saturating_add(1)));
        let Some((kind, detail)) = self.mcp_detail.clone() else {
            return true;
        };
        match content_row {
            0 => {
                if !drag_select {
                    if let Some(next) = mcp_manager_tab_at_column(kind, column_offset) {
                        self.request_mcp_manager_tab(next);
                    }
                }
            }
            2 => {
                if !drag_select {
                    let line = render_mcp_manager_server_actions(
                        &detail,
                        self.mcp_manager_selected_server,
                        self.mcp_manager_selected_server_keys.len(),
                    );
                    if let Some(action) = mcp_manager_action_at_column(&line, column_offset) {
                        match action {
                            TuiMcpManagerMouseAction::Enable => {
                                self.request_mcp_manager_enabled(true);
                            }
                            TuiMcpManagerMouseAction::Disable => {
                                self.request_mcp_manager_enabled(false);
                            }
                            TuiMcpManagerMouseAction::Remove => {
                                self.request_selected_mcp_server_remove()
                            }
                            TuiMcpManagerMouseAction::Tools => {
                                self.request_selected_mcp_server_tools()
                            }
                            TuiMcpManagerMouseAction::Reload => self.request_mcp_reload(),
                        }
                    }
                }
            }
            row if row >= 4 => {
                let filtered = filter_mcp_manager_detail(&detail, self.mcp_manager_filter.trim());
                if let Some(line) = filtered.lines().nth(row - 4) {
                    if let Some(server) = parse_mcp_manager_server_entry(line) {
                        if drag_select {
                            self.drag_select_mcp_manager_server_entry(&server);
                        } else if modifiers.contains(KeyModifiers::CONTROL) {
                            self.toggle_mcp_manager_server_entry(&server);
                        } else {
                            self.mcp_manager_drag_anchor_key = Some(server.selection_key());
                            self.select_mcp_manager_server_entry(&server);
                        }
                    }
                }
            }
            _ => {}
        }
        true
    }

    fn frame_layout(&self) -> Option<(Rect, Rect, Rect)> {
        if self.last_frame_area.width == 0 || self.last_frame_area.height == 0 {
            return None;
        }
        let root = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(3),
        ])
        .split(self.last_frame_area);
        Some((root[0], root[1], root[2]))
    }

    fn handle_key(&mut self, code: KeyCode) -> bool {
        if self.show_command_palette {
            return self.handle_command_palette_key(code);
        }
        if self.show_session_picker {
            return self.handle_session_picker_key(code);
        }
        if self.show_thread_picker {
            return self.handle_thread_picker_key(code);
        }
        if self.show_user_input_modal {
            return self.handle_user_input_key(code);
        }
        if self.show_approval_modal {
            return self.handle_approval_key(code);
        }
        if self.mcp_remove_confirmation.is_some() {
            return self.handle_mcp_remove_confirmation_key(code);
        }
        if self.rollback_apply_confirmation.is_some() {
            return self.handle_rollback_apply_confirmation_key(code);
        }
        if self.composer_focused {
            return self.handle_composer_key(code);
        }
        if self.mcp_detail.is_some() {
            match code {
                KeyCode::Tab if self.show_mcp_manager => {
                    self.request_mcp_manager_tab(self.active_mcp_manager_tab().next());
                    return true;
                }
                KeyCode::BackTab if self.show_mcp_manager => {
                    self.request_mcp_manager_tab(self.active_mcp_manager_tab().previous());
                    return true;
                }
                KeyCode::Char('r') if self.show_mcp_manager => {
                    self.request_mcp_reload();
                    return true;
                }
                KeyCode::Char(' ') if self.show_mcp_manager => {
                    self.toggle_selected_mcp_manager_server();
                    return true;
                }
                KeyCode::Char('A') if self.show_mcp_manager => {
                    self.select_all_visible_mcp_manager_servers();
                    return true;
                }
                KeyCode::Char('U') if self.show_mcp_manager => {
                    self.clear_mcp_manager_server_selection();
                    return true;
                }
                KeyCode::Char('n') if self.show_mcp_manager => {
                    self.select_relative_mcp_server(1);
                    return true;
                }
                KeyCode::Char('p') if self.show_mcp_manager => {
                    self.select_relative_mcp_server(-1);
                    return true;
                }
                KeyCode::Char('e') if self.show_mcp_manager => {
                    self.request_mcp_manager_enabled(true);
                    return true;
                }
                KeyCode::Char('d') if self.show_mcp_manager => {
                    self.request_mcp_manager_enabled(false);
                    return true;
                }
                KeyCode::Char('E') if self.show_mcp_manager => {
                    self.request_selected_mcp_servers_enabled(true);
                    return true;
                }
                KeyCode::Char('D') if self.show_mcp_manager => {
                    self.request_selected_mcp_servers_enabled(false);
                    return true;
                }
                KeyCode::Char('x') if self.show_mcp_manager => {
                    self.request_selected_mcp_server_remove();
                    return true;
                }
                KeyCode::Char('t') if self.show_mcp_manager => {
                    self.request_selected_mcp_server_tools();
                    return true;
                }
                KeyCode::Esc => {
                    self.clear_mcp_detail();
                    return true;
                }
                KeyCode::Up => {
                    self.scroll_mcp_detail_up(1);
                    return true;
                }
                KeyCode::Down => {
                    self.scroll_mcp_detail_down(1);
                    return true;
                }
                KeyCode::PageUp => {
                    self.scroll_mcp_detail_up(8);
                    return true;
                }
                KeyCode::PageDown => {
                    self.scroll_mcp_detail_down(8);
                    return true;
                }
                KeyCode::Home => {
                    self.scroll_mcp_detail_to_top();
                    return true;
                }
                KeyCode::End => {
                    self.scroll_mcp_detail_to_bottom();
                    return true;
                }
                _ => {}
            }
        }
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return false,
            KeyCode::Tab => self.mode = self.mode.next(),
            KeyCode::Char('p') => self.mode = TuiMode::Plan,
            KeyCode::Char('a') => self.mode = TuiMode::Agent,
            KeyCode::Char('y') => self.mode = TuiMode::Yolo,
            KeyCode::Char('i') => {
                self.composer_focused = true;
                self.composer_cursor = self.composer.len();
                self.status = "composer focused".to_string();
            }
            KeyCode::Char(':') => {
                self.show_command_palette = true;
                self.command_query.clear();
                self.command_cursor = 0;
                self.command_history_index = None;
                self.command_history_draft.clear();
            }
            KeyCode::Char('s') => self.show_session_picker = true,
            KeyCode::Char('t') => self.show_thread_picker = true,
            KeyCode::Char('c') => self.request_cancel_run(),
            KeyCode::Up => self.scroll_transcript_up(1),
            KeyCode::Down => self.scroll_transcript_down(1),
            KeyCode::PageUp => self.scroll_transcript_up(8),
            KeyCode::PageDown => self.scroll_transcript_down(8),
            KeyCode::Home => self.scroll_transcript_to_top(),
            KeyCode::End => self.scroll_transcript_to_latest(),
            KeyCode::Char('!') => {
                if self.active_approval_id.is_none() {
                    self.active_approval_id = self.next_pending_approval_id();
                }
                self.show_approval_modal = true;
            }
            _ => {}
        }
        true
    }

    fn handle_composer_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Esc => {
                self.composer_focused = false;
                self.status = "composer closed".to_string();
            }
            KeyCode::Enter => {
                let content = self.composer.trim().to_string();
                if content.is_empty() {
                    self.status = "composer is empty".to_string();
                    return true;
                }
                if let Some(draft) = self.queued_draft.take() {
                    self.composer.clear();
                    self.composer_cursor = 0;
                    self.submit_or_queue_user_message(draft.thread_id, content);
                    return true;
                }
                if let Some(note) = composer_memory_note(&content) {
                    self.composer.clear();
                    self.composer_cursor = 0;
                    self.pending_actions.push(TuiAction::AppendMemory { note });
                    self.status = "remembering composer note".to_string();
                    return true;
                }
                if let Some(command) = parse_tui_exit_command(&content) {
                    match command {
                        Ok(()) => return false,
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = composer_memory_command(&content) {
                    match command {
                        Ok(command) => {
                            self.composer.clear();
                            self.composer_cursor = 0;
                            self.pending_actions.push(TuiAction::Memory { command });
                            self.status = "memory command queued".to_string();
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_help_command(&content) {
                    match command {
                        Ok(command) => {
                            self.show_help_detail(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_note_command(&content) {
                    match command {
                        Ok(command) => {
                            self.request_note_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_subagents_command(&content)
                    .or_else(|| parse_tui_agent_command(&content))
                {
                    match command {
                        Ok(command) => {
                            self.handle_subagents_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_rlm_command(&content) {
                    match command {
                        Ok(command) => {
                            self.handle_rlm_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_relay_command(&content) {
                    match command {
                        Ok(command) => {
                            self.handle_relay_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_anchor_command(&content) {
                    match command {
                        Ok(command) => {
                            self.request_anchor_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_queue_command(&content) {
                    match command {
                        Ok(command) => {
                            let keep_composer = matches!(command, TuiQueueCommand::Edit { .. });
                            self.handle_queue_command(command);
                            if !keep_composer {
                                self.composer.clear();
                                self.composer_cursor = 0;
                            }
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_share_command(&content) {
                    match command {
                        Ok(command) => {
                            self.handle_share_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_export_command(&content) {
                    match command {
                        Ok(command) => {
                            self.handle_export_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_save_command(&content) {
                    match command {
                        Ok(command) => {
                            self.handle_save_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_load_command(&content) {
                    match command {
                        Ok(command) => {
                            self.handle_load_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_attach_command(&content) {
                    match command {
                        Ok(command) => {
                            self.handle_attach_command(command);
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_change_command(&content) {
                    match command {
                        Ok(command) => {
                            self.handle_change_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_system_command(&content) {
                    match command {
                        Ok(command) => {
                            self.handle_system_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_edit_command(&content) {
                    match command {
                        Ok(command) => {
                            let keep_composer = self.handle_edit_command(command);
                            if !keep_composer {
                                self.composer.clear();
                                self.composer_cursor = 0;
                            }
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_clear_command(&content) {
                    match command {
                        Ok(command) => {
                            self.handle_clear_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_diff_command(&content) {
                    match command {
                        Ok(command) => {
                            self.handle_diff_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_hooks_command(&content) {
                    match command {
                        Ok(command) => {
                            self.request_hooks_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_goal_command(&content) {
                    match command {
                        Ok(command) => {
                            self.handle_goal_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_stash_command(&content) {
                    match command {
                        Ok(command) => self.handle_composer_stash_command(command),
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_network_command(&content) {
                    match command {
                        Ok(command) => {
                            self.request_network_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_lsp_command(&content) {
                    match command {
                        Ok(command) => {
                            self.handle_lsp_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_status_command(&content) {
                    match command {
                        Ok(()) => {
                            self.show_status_detail();
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_statusline_command(&content) {
                    match command {
                        Ok(()) => {
                            self.show_statusline_detail();
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_verbose_command(&content) {
                    match command {
                        Ok(command) => {
                            self.handle_verbose_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_context_command(&content) {
                    match command {
                        Ok(()) => {
                            self.show_context_detail();
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_tokens_command(&content) {
                    match command {
                        Ok(()) => {
                            self.show_tokens_detail();
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_cost_command(&content) {
                    match command {
                        Ok(()) => {
                            self.show_cost_detail();
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_cache_command(&content) {
                    match command {
                        Ok(command) => {
                            self.show_cache_detail(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_mode_command(&content) {
                    match command {
                        Ok(command) => {
                            self.handle_mode_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_theme_command(&content) {
                    match command {
                        Ok(command) => {
                            self.handle_theme_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_model_command(&content) {
                    match command {
                        Ok(command) => {
                            self.request_model_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_provider_command(&content) {
                    match command {
                        Ok(command) => {
                            self.request_provider_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) =
                    parse_tui_skills_command(&content).or_else(|| parse_tui_skill_command(&content))
                {
                    match command {
                        Ok(command) => {
                            self.request_skills_command(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_feedback_command(&content) {
                    match command {
                        Ok(command) => {
                            self.show_feedback_detail(command);
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_links_command(&content) {
                    match command {
                        Ok(()) => {
                            self.show_links_detail();
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_home_command(&content) {
                    match command {
                        Ok(()) => {
                            self.show_home_detail();
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_settings_command(&content) {
                    match command {
                        Ok(()) => {
                            self.show_settings_detail();
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(title) = parse_tui_rename_command(&content) {
                    match title {
                        Ok(title) => {
                            if self.request_session_rename(title) {
                                self.composer.clear();
                                self.composer_cursor = 0;
                            }
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some(command) = parse_tui_init_command(&content) {
                    match command {
                        Ok(()) => {
                            self.request_project_instructions_init();
                            self.composer.clear();
                            self.composer_cursor = 0;
                        }
                        Err(message) => {
                            self.status = message;
                        }
                    }
                    return true;
                }
                if let Some((command, args)) = parse_tui_custom_slash_command(&content) {
                    if self.request_custom_slash_command(command, args) {
                        self.composer.clear();
                        self.composer_cursor = 0;
                    }
                    return true;
                }
                let Some(thread_id) = self.selected_thread_id.clone() else {
                    self.status = "composer has no active durable thread".to_string();
                    return true;
                };
                self.composer.clear();
                self.composer_cursor = 0;
                self.submit_or_queue_user_message(thread_id, content);
            }
            KeyCode::Backspace => {
                backspace_at_cursor(&mut self.composer, &mut self.composer_cursor);
            }
            KeyCode::Delete => {
                delete_at_cursor(&mut self.composer, self.composer_cursor);
            }
            KeyCode::Left => {
                self.composer_cursor = previous_char_boundary(&self.composer, self.composer_cursor);
            }
            KeyCode::Right => {
                self.composer_cursor = next_char_boundary(&self.composer, self.composer_cursor);
            }
            KeyCode::Home => {
                self.composer_cursor = 0;
            }
            KeyCode::End => {
                self.composer_cursor = self.composer.len();
            }
            KeyCode::Tab => {
                self.complete_composer_slash_command();
            }
            KeyCode::Char(ch) => {
                insert_char_at_cursor(&mut self.composer, &mut self.composer_cursor, ch);
            }
            _ => {}
        }
        true
    }

    fn handle_command_palette_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Esc => self.show_command_palette = false,
            KeyCode::Enter => {
                let command = self.command_query.trim().to_string();
                self.show_command_palette = false;
                self.record_command_history(&command);
                if let Some(command) = parse_tui_exit_command(&command) {
                    match command {
                        Ok(()) => return false,
                        Err(message) => {
                            self.status = message;
                            return true;
                        }
                    }
                }
                self.execute_palette_command(&command);
            }
            KeyCode::Backspace => {
                self.command_history_index = None;
                backspace_at_cursor(&mut self.command_query, &mut self.command_cursor);
            }
            KeyCode::Delete => {
                self.command_history_index = None;
                delete_at_cursor(&mut self.command_query, self.command_cursor);
            }
            KeyCode::Left => {
                self.command_cursor =
                    previous_char_boundary(&self.command_query, self.command_cursor);
            }
            KeyCode::Right => {
                self.command_cursor = next_char_boundary(&self.command_query, self.command_cursor);
            }
            KeyCode::Home => {
                self.command_cursor = 0;
            }
            KeyCode::End => {
                self.command_cursor = self.command_query.len();
            }
            KeyCode::Up => {
                self.navigate_command_history(-1);
            }
            KeyCode::Down => {
                self.navigate_command_history(1);
            }
            KeyCode::Tab => {
                self.complete_command_palette();
            }
            KeyCode::Char(ch) => {
                self.command_history_index = None;
                insert_char_at_cursor(&mut self.command_query, &mut self.command_cursor, ch);
            }
            _ => {}
        }
        true
    }

    fn record_command_history(&mut self, command: &str) {
        if command.is_empty() {
            return;
        }
        if self
            .command_history
            .last()
            .is_some_and(|last| last == command)
        {
            self.command_history_index = None;
            self.command_history_draft.clear();
            return;
        }
        self.command_history.push(command.to_string());
        if self.command_history.len() > MAX_TUI_COMMAND_HISTORY {
            let overflow = self.command_history.len() - MAX_TUI_COMMAND_HISTORY;
            self.command_history.drain(0..overflow);
        }
        self.command_history_index = None;
        self.command_history_draft.clear();
    }

    fn navigate_command_history(&mut self, direction: isize) {
        if self.command_history.is_empty() {
            self.status = "command history is empty".to_string();
            return;
        }
        if self.command_history_index.is_none() {
            self.command_history_draft = self.command_query.clone();
        }
        let len = self.command_history.len();
        let next_index = if direction < 0 {
            Some(self.command_history_index.unwrap_or(len).saturating_sub(1))
        } else {
            match self.command_history_index {
                Some(index) if index + 1 < len => Some(index + 1),
                Some(_) => None,
                None => None,
            }
        };
        self.command_history_index = next_index;
        if let Some(index) = next_index {
            self.command_query = self.command_history[index].clone();
            self.status = format!("command history {}/{}", index + 1, len);
        } else {
            self.command_query = self.command_history_draft.clone();
            self.command_history_draft.clear();
            self.status = "command history draft restored".to_string();
        }
        self.command_cursor = self.command_query.len();
    }

    fn complete_command_palette(&mut self) {
        self.command_history_index = None;
        let cursor = clamp_char_boundary(&self.command_query, self.command_cursor);
        let prefix = &self.command_query[..cursor];
        let matches = TUI_COMMAND_COMPLETIONS
            .iter()
            .copied()
            .filter(|command| command.starts_with(prefix))
            .collect::<Vec<_>>();
        if matches.is_empty() {
            self.status = format!(
                "no command completion for `{}`",
                clip_line(prefix.trim(), 60)
            );
            return;
        }
        let completed = longest_common_prefix(&matches);
        if completed.len() > prefix.len() {
            let suffix = self.command_query[cursor..].to_string();
            let new_cursor = completed.len();
            let mut next_query = completed;
            next_query.push_str(&suffix);
            self.command_query = next_query;
            self.command_cursor = new_cursor;
            self.status = if matches.len() == 1 {
                "command completed".to_string()
            } else {
                format!("command prefix completed ({} matches)", matches.len())
            };
            return;
        }
        let preview = matches
            .iter()
            .take(4)
            .map(|value| value.trim_end())
            .collect::<Vec<_>>()
            .join(", ");
        self.status = format!("{} command completion(s): {}", matches.len(), preview);
    }

    fn complete_composer_slash_command(&mut self) {
        let cursor = clamp_char_boundary(&self.composer, self.composer_cursor);
        let prefix = &self.composer[..cursor];
        if !prefix.starts_with('/') {
            self.status = "composer slash completion starts with /".to_string();
            return;
        }
        let matches = composer_slash_completion_matches(self, prefix);
        if matches.is_empty() {
            self.status = format!("no slash completion for `{}`", clip_line(prefix.trim(), 60));
            return;
        }
        let match_refs = matches.iter().map(String::as_str).collect::<Vec<_>>();
        let completed = longest_common_prefix(&match_refs);
        if completed.len() > prefix.len() {
            let suffix = self.composer[cursor..].to_string();
            let new_cursor = completed.len();
            let mut next_composer = completed;
            next_composer.push_str(&suffix);
            self.composer = next_composer;
            self.composer_cursor = new_cursor;
            self.status = if matches.len() == 1 {
                "slash command completed".to_string()
            } else {
                format!("slash prefix completed ({} matches)", matches.len())
            };
            return;
        }
        let preview = matches
            .iter()
            .take(5)
            .map(|value| value.trim_end())
            .collect::<Vec<_>>()
            .join(", ");
        self.status = format!("{} slash completion(s): {}", matches.len(), preview);
    }

    fn execute_palette_command(&mut self, command: &str) {
        let command = command.trim();
        if let Some(command) = command.strip_prefix('!') {
            self.request_shell_run(command.trim().to_string());
            return;
        }
        if let Some(command) = parse_tui_help_command(command) {
            match command {
                Ok(command) => {
                    self.show_help_detail(command);
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_stash_command(command) {
            match command {
                Ok(command) => self.handle_composer_stash_command(command),
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_note_command(command) {
            match command {
                Ok(command) => self.request_note_command(command),
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) =
            parse_tui_subagents_command(command).or_else(|| parse_tui_agent_command(command))
        {
            match command {
                Ok(command) => self.handle_subagents_command(command),
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_rlm_command(command) {
            match command {
                Ok(command) => self.handle_rlm_command(command),
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_relay_command(command) {
            match command {
                Ok(command) => self.handle_relay_command(command),
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_anchor_command(command) {
            match command {
                Ok(command) => self.request_anchor_command(command),
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_queue_command(command) {
            match command {
                Ok(command) => self.handle_queue_command(command),
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_share_command(command) {
            match command {
                Ok(command) => self.handle_share_command(command),
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_export_command(command) {
            match command {
                Ok(command) => self.handle_export_command(command),
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_save_command(command) {
            match command {
                Ok(command) => self.handle_save_command(command),
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_load_command(command) {
            match command {
                Ok(command) => self.handle_load_command(command),
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_attach_command(command) {
            match command {
                Ok(command) => self.handle_attach_command(command),
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_change_command(command) {
            match command {
                Ok(command) => self.handle_change_command(command),
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_system_command(command) {
            match command {
                Ok(command) => self.handle_system_command(command),
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_edit_command(command) {
            match command {
                Ok(command) => {
                    self.handle_edit_command(command);
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_clear_command(command) {
            match command {
                Ok(command) => self.handle_clear_command(command),
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_diff_command(command) {
            match command {
                Ok(command) => self.handle_diff_command(command),
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_hooks_command(command) {
            match command {
                Ok(command) => self.request_hooks_command(command),
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_goal_command(command) {
            match command {
                Ok(command) => self.handle_goal_command(command),
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_network_command(command) {
            match command {
                Ok(command) => {
                    self.request_network_command(command);
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_lsp_command(command) {
            match command {
                Ok(command) => self.handle_lsp_command(command),
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_status_command(command) {
            match command {
                Ok(()) => {
                    self.show_status_detail();
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_statusline_command(command) {
            match command {
                Ok(()) => {
                    self.show_statusline_detail();
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_verbose_command(command) {
            match command {
                Ok(command) => {
                    self.handle_verbose_command(command);
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_context_command(command) {
            match command {
                Ok(()) => {
                    self.show_context_detail();
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_tokens_command(command) {
            match command {
                Ok(()) => {
                    self.show_tokens_detail();
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_cost_command(command) {
            match command {
                Ok(()) => {
                    self.show_cost_detail();
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_cache_command(command) {
            match command {
                Ok(command) => {
                    self.show_cache_detail(command);
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_mode_command(command) {
            match command {
                Ok(command) => {
                    self.handle_mode_command(command);
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_theme_command(command) {
            match command {
                Ok(command) => {
                    self.handle_theme_command(command);
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_model_command(command) {
            match command {
                Ok(command) => {
                    self.request_model_command(command);
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_provider_command(command) {
            match command {
                Ok(command) => {
                    self.request_provider_command(command);
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) =
            parse_tui_skills_command(command).or_else(|| parse_tui_skill_command(command))
        {
            match command {
                Ok(command) => {
                    self.request_skills_command(command);
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_feedback_command(command) {
            match command {
                Ok(command) => {
                    self.show_feedback_detail(command);
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_links_command(command) {
            match command {
                Ok(()) => {
                    self.show_links_detail();
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_home_command(command) {
            match command {
                Ok(()) => {
                    self.show_home_detail();
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_settings_command(command) {
            match command {
                Ok(()) => {
                    self.show_settings_detail();
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(title) = parse_tui_rename_command(command) {
            match title {
                Ok(title) => {
                    self.request_session_rename(title);
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some(command) = parse_tui_init_command(command) {
            match command {
                Ok(()) => {
                    self.request_project_instructions_init();
                }
                Err(message) => {
                    self.status = message;
                }
            }
            return;
        }
        if let Some((command, args)) = parse_tui_custom_slash_command(command) {
            self.request_custom_slash_command(command, args);
            return;
        }
        let words = command.split_whitespace().collect::<Vec<_>>();
        match words.as_slice() {
            [] => self.status = "command palette closed".to_string(),
            ["mode", "plan"] | ["plan"] => {
                self.mode = TuiMode::Plan;
                self.status = "mode set: Plan".to_string();
            }
            ["mode", "agent"] | ["agent"] => {
                self.mode = TuiMode::Agent;
                self.status = "mode set: Agent".to_string();
            }
            ["mode", "yolo"] | ["yolo"] => {
                self.mode = TuiMode::Yolo;
                self.status = "mode set: YOLO".to_string();
            }
            ["sessions"] | ["session"] => {
                self.show_session_picker = true;
                self.show_thread_picker = false;
                self.status = "session picker opened".to_string();
            }
            ["session", "filter"] | ["sessions", "filter"] => {
                self.set_session_picker_filter("");
            }
            ["session", "filter", rest @ ..] | ["sessions", "filter", rest @ ..] => {
                self.set_session_picker_filter(rest.join(" "));
            }
            ["threads"] | ["thread"] => {
                self.show_thread_picker = true;
                self.show_session_picker = false;
                self.status = "thread navigator opened".to_string();
            }
            ["thread", "filter"] | ["threads", "filter"] => {
                self.set_thread_picker_filter("");
            }
            ["thread", "filter", rest @ ..] | ["threads", "filter", rest @ ..] => {
                self.set_thread_picker_filter(rest.join(" "));
            }
            ["thread", "next"] | ["threads", "next"] => self.select_relative_thread(1),
            ["thread", "prev"]
            | ["thread", "previous"]
            | ["threads", "prev"]
            | ["threads", "previous"] => self.select_relative_thread(-1),
            ["compact"] => {
                self.request_compact_thread(DEFAULT_TUI_COMPACTION_KEEP_TAIL_TURNS);
            }
            ["compact", keep_tail] | ["thread", "compact", keep_tail] => {
                self.request_compact_thread_from_arg(keep_tail);
            }
            ["thread", "compact"] => {
                self.request_compact_thread(DEFAULT_TUI_COMPACTION_KEEP_TAIL_TURNS);
            }
            ["thread", id] | ["threads", id] => {
                self.select_thread_by_id(id);
            }
            ["reasoning"] | ["reasoning", "list"] => {
                self.show_reasoning_list();
            }
            ["reasoning", "latest"] | ["reasoning", "last"] => {
                self.show_reasoning_item("latest");
            }
            ["reasoning", "show"] => {
                self.show_reasoning_item("latest");
            }
            ["reasoning", "show", selector] => {
                self.show_reasoning_item(selector);
            }
            ["reasoning", "search"] => {
                self.status = "reasoning search requires a query".to_string();
                self.show_reasoning_list();
            }
            ["reasoning", "search", query @ ..] => {
                self.show_reasoning_search(&query.join(" "));
            }
            ["reasoning", "pin"] => {
                self.pin_reasoning_replay_turn("latest");
            }
            ["reasoning", "pin", selector] => {
                self.pin_reasoning_replay_turn(selector);
            }
            ["reasoning", "pins"] => {
                self.show_reasoning_pins();
            }
            ["reasoning", "unpin"] => {
                self.status = "reasoning unpin requires a selector or all".to_string();
                self.show_reasoning_pins();
            }
            ["reasoning", "unpin", "all"] => {
                self.clear_reasoning_replay_pins();
            }
            ["reasoning", "unpin", selector] => {
                self.unpin_reasoning_replay_turn(selector);
            }
            ["reasoning", "replay"] => {
                self.show_reasoning_list();
                self.status = format!("reasoning replay limit is {}", self.reasoning_replay_limit);
            }
            ["reasoning", "replay", limit] => {
                self.set_reasoning_replay_limit_from_arg(limit);
            }
            ["tasks"] | ["task"] => {
                let count = self.active_thread_tasks().len();
                self.status = if count == 0 {
                    "no runtime tasks in active thread".to_string()
                } else {
                    format!("active thread tasks={count}; use task <summary> to enqueue work")
                };
            }
            ["task", "create"] | ["tasks", "create"] => {
                self.status = "task create requires a summary".to_string();
            }
            ["task", "create", rest @ ..] | ["tasks", "create", rest @ ..] => {
                self.request_create_task(rest.join(" "));
            }
            ["task", "next"] | ["tasks", "next"] => self.select_relative_task(1),
            ["task", "prev"] | ["task", "previous"] | ["tasks", "prev"] | ["tasks", "previous"] => {
                self.select_relative_task(-1)
            }
            ["task", "select", "all"] | ["tasks", "select", "all"] => {
                self.select_visible_tasks();
            }
            ["task", "select", "clear"]
            | ["task", "select", "none"]
            | ["tasks", "select", "clear"]
            | ["tasks", "select", "none"] => {
                self.clear_task_selection();
            }
            ["task", "select", id] | ["tasks", "select", id] => {
                self.select_task_by_id(id);
            }
            ["task", "bulk", "pause"] | ["tasks", "bulk", "pause"] => {
                self.request_default_task_pause();
            }
            ["task", "bulk", "resume"] | ["tasks", "bulk", "resume"] => {
                self.request_default_task_resume();
            }
            ["task", "bulk", "cancel"] | ["tasks", "bulk", "cancel"] => {
                self.request_default_task_cancel();
            }
            ["task", "pause"] | ["tasks", "pause"] => {
                self.request_default_task_pause();
            }
            ["task", "pause", id] | ["tasks", "pause", id] => {
                self.request_task_pause(id);
            }
            ["task", "resume"] | ["tasks", "resume"] => {
                self.request_default_task_resume();
            }
            ["task", "resume", id] | ["tasks", "resume", id] => {
                self.request_task_resume(id);
            }
            ["task", "cancel"] | ["tasks", "cancel"] => {
                self.request_default_task_cancel();
            }
            ["task", "cancel", id] | ["tasks", "cancel", id] => {
                self.request_task_cancel(id);
            }
            ["task", rest @ ..] => {
                self.request_create_task(rest.join(" "));
            }
            ["restore", "snapshot"] => {
                self.request_rollback_snapshot(None);
            }
            ["restore", "snapshot", rest @ ..] => {
                self.request_rollback_snapshot(Some(rest.join(" ")));
            }
            ["restore", "list"] => {
                self.request_rollback_list(20);
            }
            ["restore", "list", limit] => {
                self.request_rollback_list_from_arg(limit);
            }
            ["restore", "show", id] => {
                self.request_rollback_show(id);
            }
            ["restore", "hunks", id] | ["restore", "diff", id] => {
                self.request_rollback_hunk(id, None);
            }
            ["restore", "hunk", id] => {
                self.request_rollback_hunk(id, Some(1));
            }
            ["restore", "hunk", id, hunk] => {
                self.request_rollback_hunk_from_arg(id, hunk);
            }
            ["restore", "hunk", id, hunk, "--apply"]
            | ["restore", "hunk-apply", id, hunk]
            | ["restore", "apply-hunk", id, hunk] => {
                self.request_rollback_hunk_restore_from_arg(id, hunk, true);
            }
            ["restore", "hunk", id, hunk, "--check"]
            | ["restore", "hunk-check", id, hunk]
            | ["restore", "check-hunk", id, hunk] => {
                self.request_rollback_hunk_restore_from_arg(id, hunk, false);
            }
            ["restore", "revert-turn", id] | ["restore", "revert_turn", id] => {
                self.request_revert_turn(id, false);
            }
            ["restore", "revert-turn", id, "--apply"]
            | ["restore", "revert_turn", id, "--apply"]
            | ["revert", "turn", id, "--apply"]
            | ["revert_turn", id, "--apply"] => {
                self.request_revert_turn(id, true);
            }
            ["revert", "turn", id] | ["revert_turn", id] => {
                self.request_revert_turn(id, false);
            }
            ["diagnostics"] | ["diagnostic"] | ["diag"] => {
                self.request_diagnostics(false, Vec::new());
            }
            ["diagnostics", rest @ ..] | ["diagnostic", rest @ ..] | ["diag", rest @ ..] => {
                self.request_diagnostics_from_args(rest);
            }
            ["shell"] | ["sh"] => {
                self.status =
                    "shell commands: run|list|show|attach|supervisor|wait|poll|stdin|close-stdin|resize|cancel"
                        .to_string();
            }
            ["shell", "run"] | ["sh", "run"] => {
                self.status = "shell run requires a command".to_string();
            }
            ["shell", "run", rest @ ..] | ["sh", "run", rest @ ..] => {
                self.request_shell_run(rest.join(" "));
            }
            ["shell", "list"] | ["sh", "list"] | ["jobs"] | ["jobs", "list"] => {
                self.request_shell_list();
            }
            ["shell", "show"]
            | ["shell", "inspect"]
            | ["sh", "show"]
            | ["sh", "inspect"]
            | ["jobs", "show"]
            | ["jobs", "inspect"] => {
                self.status = "shell show requires a task id".to_string();
            }
            ["shell", "show", id]
            | ["shell", "inspect", id]
            | ["sh", "show", id]
            | ["sh", "inspect", id]
            | ["jobs", "show", id]
            | ["jobs", "inspect", id] => {
                self.request_shell_show(id);
            }
            ["shell", "attach"] | ["sh", "attach"] | ["jobs", "attach"] => {
                self.status = "shell attach requires a task id".to_string();
            }
            ["shell", "attach", id] | ["sh", "attach", id] | ["jobs", "attach", id] => {
                self.request_shell_attach(id, None, false);
            }
            ["shell", "attach", id, "tail"]
            | ["sh", "attach", id, "tail"]
            | ["jobs", "attach", id, "tail"] => {
                self.request_shell_attach(id, None, true);
            }
            ["shell", "attach", id, cursor]
            | ["sh", "attach", id, cursor]
            | ["jobs", "attach", id, cursor] => {
                self.request_shell_attach_from_cursor(id, cursor);
            }
            ["shell", "supervisor"]
            | ["shell", "supervisor-status"]
            | ["shell", "status"]
            | ["sh", "supervisor"]
            | ["sh", "supervisor-status"]
            | ["jobs", "supervisor"]
            | ["jobs", "supervisor-status"] => {
                self.request_shell_supervisor_status();
            }
            ["shell", "stdin"]
            | ["shell", "send"]
            | ["sh", "stdin"]
            | ["sh", "send"]
            | ["jobs", "stdin"]
            | ["jobs", "send"] => {
                self.status = "shell stdin requires a task id and input".to_string();
            }
            ["shell", "stdin", id]
            | ["shell", "send", id]
            | ["sh", "stdin", id]
            | ["sh", "send", id]
            | ["jobs", "stdin", id]
            | ["jobs", "send", id] => {
                self.request_shell_stdin(id, String::new(), false);
            }
            ["shell", "stdin", id, rest @ ..]
            | ["shell", "send", id, rest @ ..]
            | ["sh", "stdin", id, rest @ ..]
            | ["sh", "send", id, rest @ ..]
            | ["jobs", "stdin", id, rest @ ..]
            | ["jobs", "send", id, rest @ ..] => {
                self.request_shell_stdin(id, rest.join(" "), false);
            }
            ["shell", "close-stdin"]
            | ["shell", "eof"]
            | ["sh", "close-stdin"]
            | ["sh", "eof"]
            | ["jobs", "close-stdin"]
            | ["jobs", "eof"] => {
                self.status = "shell close-stdin requires a task id".to_string();
            }
            ["shell", "close-stdin", id]
            | ["shell", "eof", id]
            | ["sh", "close-stdin", id]
            | ["sh", "eof", id]
            | ["jobs", "close-stdin", id]
            | ["jobs", "eof", id] => {
                self.request_shell_stdin(id, String::new(), true);
            }
            ["shell", "wait", id] | ["sh", "wait", id] => {
                self.request_shell_wait(id, true, 1_000);
            }
            ["shell", "wait", id, timeout_ms] | ["sh", "wait", id, timeout_ms] => {
                self.request_shell_wait_from_arg(id, true, timeout_ms);
            }
            ["shell", "poll", id] | ["sh", "poll", id] => {
                self.request_shell_wait(id, false, 0);
            }
            ["shell", "resize"] | ["sh", "resize"] => {
                self.status = "shell resize requires a task id, rows, and cols".to_string();
            }
            ["shell", "resize", id, rows, cols] | ["sh", "resize", id, rows, cols] => {
                self.request_shell_resize(id, rows, cols);
            }
            ["shell", "cancel", "all"] | ["sh", "cancel", "all"] => {
                self.request_shell_cancel(None, true);
            }
            ["shell", "cancel", id] | ["sh", "cancel", id] => {
                self.request_shell_cancel(Some((*id).to_string()), false);
            }
            ["jobs", "wait", id] => {
                self.request_shell_wait(id, true, 1_000);
            }
            ["jobs", "wait", id, timeout_ms] => {
                self.request_shell_wait_from_arg(id, true, timeout_ms);
            }
            ["jobs", "poll", id] => {
                self.request_shell_wait(id, false, 0);
            }
            ["jobs", "resize"] => {
                self.status = "shell resize requires a task id, rows, and cols".to_string();
            }
            ["jobs", "resize", id, rows, cols] => {
                self.request_shell_resize(id, rows, cols);
            }
            ["jobs", "cancel", "all"] => {
                self.request_shell_cancel(None, true);
            }
            ["jobs", "cancel", id] => {
                self.request_shell_cancel(Some((*id).to_string()), false);
            }
            ["shell", rest @ ..] | ["sh", rest @ ..] => {
                self.request_shell_run(rest.join(" "));
            }
            ["memory"] | ["memory", "show"] => {
                self.pending_actions.push(TuiAction::Memory {
                    command: TuiMemoryCommand::Show,
                });
                self.status = "memory command queued".to_string();
            }
            ["memory", "path"] => {
                self.pending_actions.push(TuiAction::Memory {
                    command: TuiMemoryCommand::Path,
                });
                self.status = "memory path queued".to_string();
            }
            ["memory", "clear"] => {
                self.pending_actions.push(TuiAction::Memory {
                    command: TuiMemoryCommand::Clear,
                });
                self.status = "memory clear queued".to_string();
            }
            ["memory", "edit"] => {
                self.pending_actions.push(TuiAction::Memory {
                    command: TuiMemoryCommand::Edit,
                });
                self.status = "memory edit queued".to_string();
            }
            ["memory", "help"] => {
                self.pending_actions.push(TuiAction::Memory {
                    command: TuiMemoryCommand::Help,
                });
                self.status = "memory help queued".to_string();
            }
            ["memory", ..] => {
                self.status = "usage: memory [show|path|clear|edit|help]".to_string();
            }
            ["mcp"] | ["mcp", "manager"] | ["mcp", "open"] => {
                self.request_mcp_manager();
            }
            ["mcp", "manager", "filter"] | ["mcp", "open", "filter"] => {
                self.set_mcp_manager_filter("");
            }
            ["mcp", "manager", "filter", rest @ ..] | ["mcp", "open", "filter", rest @ ..] => {
                self.set_mcp_manager_filter(rest.join(" "));
            }
            ["mcp", "manager", "tab", "overview"]
            | ["mcp", "manager", "tab", "manager"]
            | ["mcp", "open", "tab", "overview"]
            | ["mcp", "open", "tab", "manager"] => {
                self.request_mcp_manager();
            }
            ["mcp", "manager", "tab", "tools"] | ["mcp", "open", "tab", "tools"] => {
                self.request_mcp_manager_details(TuiMcpDetailKind::Tools, None);
            }
            ["mcp", "manager", "tab", "prompts"] | ["mcp", "open", "tab", "prompts"] => {
                self.request_mcp_manager_details(TuiMcpDetailKind::Prompts, None);
            }
            ["mcp", "manager", "tab", "resources"] | ["mcp", "open", "tab", "resources"] => {
                self.request_mcp_manager_details(TuiMcpDetailKind::Resources, None);
            }
            ["mcp", "manager", "tab", "resource-templates"]
            | ["mcp", "manager", "tab", "templates"]
            | ["mcp", "open", "tab", "resource-templates"]
            | ["mcp", "open", "tab", "templates"] => {
                self.request_mcp_manager_details(TuiMcpDetailKind::ResourceTemplates, None);
            }
            ["mcp", "manager", "tab", "health"] | ["mcp", "open", "tab", "health"] => {
                self.request_mcp_manager_details(TuiMcpDetailKind::Health, None);
            }
            ["mcp", "manager", "tab", ..] | ["mcp", "open", "tab", ..] => {
                self.status =
                    "usage: mcp manager tab overview|tools|prompts|resources|resource-templates|health"
                        .to_string();
            }
            ["mcp", "manager", "tools"] | ["mcp", "open", "tools"] => {
                self.request_mcp_manager_details(TuiMcpDetailKind::Tools, None);
            }
            ["mcp", "manager", "tools", server] | ["mcp", "open", "tools", server] => {
                self.request_mcp_manager_details(TuiMcpDetailKind::Tools, Some(server));
            }
            ["mcp", "manager", "tools", ..] | ["mcp", "open", "tools", ..] => {
                self.status = "usage: mcp manager tools [server]".to_string();
            }
            ["mcp", "manager", "prompts"] | ["mcp", "open", "prompts"] => {
                self.request_mcp_manager_details(TuiMcpDetailKind::Prompts, None);
            }
            ["mcp", "manager", "prompts", server] | ["mcp", "open", "prompts", server] => {
                self.request_mcp_manager_details(TuiMcpDetailKind::Prompts, Some(server));
            }
            ["mcp", "manager", "prompts", ..] | ["mcp", "open", "prompts", ..] => {
                self.status = "usage: mcp manager prompts [server]".to_string();
            }
            ["mcp", "manager", "resources"] | ["mcp", "open", "resources"] => {
                self.request_mcp_manager_details(TuiMcpDetailKind::Resources, None);
            }
            ["mcp", "manager", "resources", server] | ["mcp", "open", "resources", server] => {
                self.request_mcp_manager_details(TuiMcpDetailKind::Resources, Some(server));
            }
            ["mcp", "manager", "resources", ..] | ["mcp", "open", "resources", ..] => {
                self.status = "usage: mcp manager resources [server]".to_string();
            }
            ["mcp", "manager", "resource-templates"]
            | ["mcp", "manager", "templates"]
            | ["mcp", "open", "resource-templates"]
            | ["mcp", "open", "templates"] => {
                self.request_mcp_manager_details(TuiMcpDetailKind::ResourceTemplates, None);
            }
            ["mcp", "manager", "resource-templates", server]
            | ["mcp", "manager", "templates", server]
            | ["mcp", "open", "resource-templates", server]
            | ["mcp", "open", "templates", server] => {
                self.request_mcp_manager_details(TuiMcpDetailKind::ResourceTemplates, Some(server));
            }
            ["mcp", "manager", "resource-templates", ..]
            | ["mcp", "manager", "templates", ..]
            | ["mcp", "open", "resource-templates", ..]
            | ["mcp", "open", "templates", ..] => {
                self.status = "usage: mcp manager resource-templates [server]".to_string();
            }
            ["mcp", "list"] | ["mcp", "status"] | ["mcp", "reload"] => {
                self.request_mcp_list();
            }
            ["mcp", "close"] | ["mcp", "clear"] => {
                self.clear_mcp_detail();
            }
            ["mcp", "tools"] => {
                self.request_mcp_details(TuiMcpDetailKind::Tools, None);
            }
            ["mcp", "tools", server] => {
                self.request_mcp_details(TuiMcpDetailKind::Tools, Some(server));
            }
            ["mcp", "tools", ..] => {
                self.status = "usage: mcp tools [server]".to_string();
            }
            ["mcp", "prompts"] => {
                self.request_mcp_details(TuiMcpDetailKind::Prompts, None);
            }
            ["mcp", "prompts", server] => {
                self.request_mcp_details(TuiMcpDetailKind::Prompts, Some(server));
            }
            ["mcp", "prompts", ..] => {
                self.status = "usage: mcp prompts [server]".to_string();
            }
            ["mcp", "resources"] => {
                self.request_mcp_details(TuiMcpDetailKind::Resources, None);
            }
            ["mcp", "resources", server] => {
                self.request_mcp_details(TuiMcpDetailKind::Resources, Some(server));
            }
            ["mcp", "resources", ..] => {
                self.status = "usage: mcp resources [server]".to_string();
            }
            ["mcp", "resource-templates"] | ["mcp", "templates"] => {
                self.request_mcp_details(TuiMcpDetailKind::ResourceTemplates, None);
            }
            ["mcp", "resource-templates", server] | ["mcp", "templates", server] => {
                self.request_mcp_details(TuiMcpDetailKind::ResourceTemplates, Some(server));
            }
            ["mcp", "resource-templates", ..] | ["mcp", "templates", ..] => {
                self.status = "usage: mcp resource-templates [server]".to_string();
            }
            ["mcp", "init"] => {
                self.request_mcp_init(false);
            }
            ["mcp", "init", "--force"] => {
                self.request_mcp_init(true);
            }
            ["mcp", "init", ..] => {
                self.status = "usage: mcp init [--force]".to_string();
            }
            ["mcp", "add", "stdio"] => {
                self.status = "usage: mcp add stdio <name> <command> [args...]".to_string();
            }
            ["mcp", "add", "stdio", name] => {
                self.status = format!("mcp stdio server `{name}` requires a command");
            }
            ["mcp", "add", "stdio", name, command, args @ ..] => {
                self.request_mcp_add_stdio(TuiMcpConfigScope::Project, name, command, args);
            }
            ["mcp", "add", "http", name, url] => {
                self.request_mcp_add_remote(TuiMcpConfigScope::Project, name, "http", url);
            }
            ["mcp", "add", "sse", name, url] => {
                self.request_mcp_add_remote(TuiMcpConfigScope::Project, name, "sse", url);
            }
            ["mcp", "add", "http", ..] => {
                self.status = "usage: mcp add http <name> <url>".to_string();
            }
            ["mcp", "add", "sse", ..] => {
                self.status = "usage: mcp add sse <name> <url>".to_string();
            }
            ["mcp", "add", ..] => {
                self.status =
                    "usage: mcp add stdio <name> <command> [args...] | mcp add http <name> <url>"
                        .to_string();
            }
            ["mcp", "remove", name] | ["mcp", "rm", name] => {
                self.request_mcp_remove(TuiMcpConfigScope::Project, name);
            }
            ["mcp", "remove", ..] | ["mcp", "rm", ..] => {
                self.status = "usage: mcp remove <name>".to_string();
            }
            ["mcp", "enable", name] => {
                self.request_mcp_set_enabled(TuiMcpConfigScope::Project, name, true);
            }
            ["mcp", "disable", name] => {
                self.request_mcp_set_enabled(TuiMcpConfigScope::Project, name, false);
            }
            ["mcp", "enable", ..] => {
                self.status = "usage: mcp enable <name>".to_string();
            }
            ["mcp", "disable", ..] => {
                self.status = "usage: mcp disable <name>".to_string();
            }
            ["mcp", "validate"] => {
                self.request_mcp_validate();
            }
            ["mcp", "validate", ..] => {
                self.status = "usage: mcp validate".to_string();
            }
            ["mcp", scope, "add", "stdio"] if parse_tui_mcp_scope(scope).is_some() => {
                self.status = format!("usage: mcp {scope} add stdio <name> <command> [args...]");
            }
            ["mcp", scope, "add", "stdio", name] if parse_tui_mcp_scope(scope).is_some() => {
                self.status = format!("mcp {scope} stdio server `{name}` requires a command");
            }
            ["mcp", scope, "add", "stdio", name, command, args @ ..]
                if parse_tui_mcp_scope(scope).is_some() =>
            {
                self.request_mcp_add_stdio(
                    parse_tui_mcp_scope(scope).unwrap_or(TuiMcpConfigScope::Project),
                    name,
                    command,
                    args,
                );
            }
            ["mcp", scope, "add", "http", name, url] if parse_tui_mcp_scope(scope).is_some() => {
                self.request_mcp_add_remote(
                    parse_tui_mcp_scope(scope).unwrap_or(TuiMcpConfigScope::Project),
                    name,
                    "http",
                    url,
                );
            }
            ["mcp", scope, "add", "sse", name, url] if parse_tui_mcp_scope(scope).is_some() => {
                self.request_mcp_add_remote(
                    parse_tui_mcp_scope(scope).unwrap_or(TuiMcpConfigScope::Project),
                    name,
                    "sse",
                    url,
                );
            }
            ["mcp", scope, "add", "http", ..] if parse_tui_mcp_scope(scope).is_some() => {
                self.status = format!("usage: mcp {scope} add http <name> <url>");
            }
            ["mcp", scope, "add", "sse", ..] if parse_tui_mcp_scope(scope).is_some() => {
                self.status = format!("usage: mcp {scope} add sse <name> <url>");
            }
            ["mcp", scope, "add", ..] if parse_tui_mcp_scope(scope).is_some() => {
                self.status = format!(
                    "usage: mcp {scope} add stdio <name> <command> [args...] | mcp {scope} add http <name> <url>"
                );
            }
            ["mcp", scope, "remove", name] | ["mcp", scope, "rm", name]
                if parse_tui_mcp_scope(scope).is_some() =>
            {
                self.request_mcp_remove(
                    parse_tui_mcp_scope(scope).unwrap_or(TuiMcpConfigScope::Project),
                    name,
                );
            }
            ["mcp", scope, "remove", ..] | ["mcp", scope, "rm", ..]
                if parse_tui_mcp_scope(scope).is_some() =>
            {
                self.status = format!("usage: mcp {scope} remove <name>");
            }
            ["mcp", scope, "enable", name] if parse_tui_mcp_scope(scope).is_some() => {
                self.request_mcp_set_enabled(
                    parse_tui_mcp_scope(scope).unwrap_or(TuiMcpConfigScope::Project),
                    name,
                    true,
                );
            }
            ["mcp", scope, "disable", name] if parse_tui_mcp_scope(scope).is_some() => {
                self.request_mcp_set_enabled(
                    parse_tui_mcp_scope(scope).unwrap_or(TuiMcpConfigScope::Project),
                    name,
                    false,
                );
            }
            ["mcp", scope, "enable", ..] if parse_tui_mcp_scope(scope).is_some() => {
                self.status = format!("usage: mcp {scope} enable <name>");
            }
            ["mcp", scope, "disable", ..] if parse_tui_mcp_scope(scope).is_some() => {
                self.status = format!("usage: mcp {scope} disable <name>");
            }
            ["automations"] | ["automation"] => {
                let count = self.active_thread_automations().len();
                self.status = if count == 0 {
                    "no automations in active thread".to_string()
                } else {
                    format!("active thread automations={count}; use automation trigger [id]")
                };
            }
            ["automation", "trigger"] | ["automation", "run"] => {
                self.request_default_automation_trigger();
            }
            ["automation", "trigger", id] | ["automation", "run", id] => {
                self.request_automation_trigger(id, None);
            }
            ["automation", "trigger", id, rest @ ..] | ["automation", "run", id, rest @ ..] => {
                self.request_automation_trigger(id, Some(rest.join(" ")));
            }
            ["approval"] | ["approve"] => {
                if self.active_approval_id.is_none() {
                    self.active_approval_id = self.next_pending_approval_id();
                }
                self.show_approval_modal = true;
                self.status = "approval modal opened".to_string();
            }
            ["cancel"] | ["stop"] => self.request_cancel_run(),
            ["help"] => {
                self.status = "commands: mode plan|agent|yolo, diff, clear, change, system, edit, goal [objective|clear], sessions [filter], threads [filter], agent [N] <task>, subagents, rlm [N] <file_or_text>, relay [focus], task <summary>|select all|select clear|pause [id]|resume [id]|cancel [id]|bulk pause|bulk resume|bulk cancel, shell <cmd>|list|show|wait|poll|stdin|close-stdin|cancel, stash [list|pop|clear], memory [show|path|clear|edit|help], anchor [text|list|remove], queue [list|edit|drop|clear], share, export [path], mcp manager|list|tools|prompts|resources|resource-templates|close|init|add|enable|disable|remove|user add|user enable|user disable|user remove|validate, diagnostics [--changed|paths...], restore snapshot|list|show, revert turn <id> [--apply], compact, approval, cancel".to_string();
            }
            _ => {
                self.status = format!("unknown command: {command}");
            }
        }
    }

    fn handle_session_picker_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Esc => self.show_session_picker = false,
            KeyCode::Enter => {
                if !self.session_picker_filter.trim().is_empty()
                    && !self
                        .filtered_session_indices()
                        .contains(&self.selected_session)
                {
                    self.status = format!(
                        "no sessions match filter: {}",
                        self.session_picker_filter.trim()
                    );
                    return true;
                }
                if let Some(session) = self.selected_session() {
                    let thread = self
                        .active_thread()
                        .map(|thread| format!(" thread {}", thread.id))
                        .unwrap_or_default();
                    self.status = format!("selected session: {}{}", session.id, thread);
                }
                self.show_session_picker = false;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_relative_session(1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_relative_session(-1);
            }
            KeyCode::PageDown => {
                self.select_relative_session(TUI_PICKER_PAGE_SIZE as isize);
            }
            KeyCode::PageUp => {
                self.select_relative_session(-(TUI_PICKER_PAGE_SIZE as isize));
            }
            KeyCode::Home => {
                self.select_session_by_picker_index(0);
            }
            KeyCode::End => {
                let len = self.filtered_session_indices().len();
                if len == 0 {
                    self.status = if self.session_picker_filter.trim().is_empty() {
                        "no sessions available".to_string()
                    } else {
                        format!(
                            "no sessions match filter: {}",
                            self.session_picker_filter.trim()
                        )
                    };
                } else {
                    self.select_session_by_picker_index(len - 1);
                }
            }
            KeyCode::Char('t') => {
                self.show_session_picker = false;
                self.show_thread_picker = true;
            }
            _ => {}
        }
        true
    }

    fn handle_thread_picker_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Esc => self.show_thread_picker = false,
            KeyCode::Enter => {
                if !self.thread_picker_filter.trim().is_empty()
                    && self.selected_thread_index_for_session().is_none()
                {
                    self.status = format!(
                        "no threads match filter: {}",
                        self.thread_picker_filter.trim()
                    );
                    return true;
                }
                if let Some(thread) = self.active_thread() {
                    self.status = format!(
                        "selected thread: {} {}",
                        thread.id,
                        clip_line(&thread.title, 60)
                    );
                }
                self.show_thread_picker = false;
            }
            KeyCode::Down | KeyCode::Char('j') => self.select_relative_thread(1),
            KeyCode::Up | KeyCode::Char('k') => self.select_relative_thread(-1),
            KeyCode::PageDown => self.select_relative_thread(TUI_PICKER_PAGE_SIZE as isize),
            KeyCode::PageUp => self.select_relative_thread(-(TUI_PICKER_PAGE_SIZE as isize)),
            KeyCode::Home => self.select_thread_by_index(0),
            KeyCode::End => {
                let len = self.filtered_threads_for_selected_session().len();
                if len == 0 {
                    self.status = if self.thread_picker_filter.trim().is_empty() {
                        "no threads in selected session".to_string()
                    } else {
                        format!(
                            "no threads match filter: {}",
                            self.thread_picker_filter.trim()
                        )
                    };
                } else {
                    self.select_thread_by_index(len - 1);
                }
            }
            KeyCode::Char('s') => {
                self.show_thread_picker = false;
                self.show_session_picker = true;
            }
            _ => {}
        }
        true
    }

    fn handle_approval_key(&mut self, code: KeyCode) -> bool {
        if self.pending_shell_approval.is_some() {
            match code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    if let Some(command) = self.pending_shell_approval.take() {
                        self.pending_actions.push(TuiAction::RunApprovedShell {
                            command: command.clone(),
                        });
                        self.status =
                            format!("approved shell command: {}", clip_line(&command, 60));
                    }
                    self.show_approval_modal = false;
                }
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('c') => {
                    if let Some(command) = self.pending_shell_approval.take() {
                        self.status = format!("denied shell command: {}", clip_line(&command, 60));
                    }
                    self.show_approval_modal = false;
                }
                _ => {}
            }
            return true;
        }

        match code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Some(approval) = self.active_approval().cloned() {
                    self.active_approval_id = None;
                    let request_id = approval.id.clone();
                    if !self
                        .dismissed_approval_ids
                        .iter()
                        .any(|id| id == &request_id)
                    {
                        self.dismissed_approval_ids.push(request_id.clone());
                    }
                    self.pending_actions.push(TuiAction::RespondApproval {
                        thread_id: approval.thread_id.clone(),
                        turn_id: approval.turn_id.clone(),
                        request_id: request_id.clone(),
                        decision: "approved".to_string(),
                    });
                    self.status = format!("approval approved: {request_id}");
                } else {
                    self.status = "approval modal closed: no pending request".to_string();
                }
                self.show_approval_modal = false;
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                if let Some(approval) = self.active_approval().cloned() {
                    self.active_approval_id = None;
                    let request_id = approval.id.clone();
                    if !self
                        .dismissed_approval_ids
                        .iter()
                        .any(|id| id == &request_id)
                    {
                        self.dismissed_approval_ids.push(request_id.clone());
                    }
                    self.pending_actions.push(TuiAction::RespondApproval {
                        thread_id: approval.thread_id.clone(),
                        turn_id: approval.turn_id.clone(),
                        request_id: request_id.clone(),
                        decision: "denied".to_string(),
                    });
                    self.status = format!("approval denied: {request_id}");
                } else {
                    self.status = "approval modal closed: no pending request".to_string();
                }
                self.show_approval_modal = false;
            }
            KeyCode::Char('c') => {
                self.request_cancel_run();
                self.show_approval_modal = false;
            }
            _ => {}
        }
        true
    }

    fn handle_user_input_key(&mut self, code: KeyCode) -> bool {
        if self.user_input_other_mode {
            match code {
                KeyCode::Enter => {
                    let answer = self.user_input_other_value.trim().to_string();
                    if answer.is_empty() {
                        self.status = "other answer cannot be empty".to_string();
                    } else {
                        self.submit_user_input_answer(answer);
                    }
                }
                KeyCode::Backspace => {
                    self.user_input_other_value.pop();
                }
                KeyCode::Esc => {
                    self.user_input_other_mode = false;
                    self.user_input_other_value.clear();
                    self.status = "other answer cancelled".to_string();
                }
                KeyCode::Char(ch) if !ch.is_control() => {
                    if self.user_input_other_value.chars().count() < USER_INPUT_OTHER_MAX_CHARS {
                        self.user_input_other_value.push(ch);
                    } else {
                        self.status =
                            format!("other answer limited to {USER_INPUT_OTHER_MAX_CHARS} chars");
                    }
                }
                _ => {}
            }
            return true;
        }

        match code {
            KeyCode::Char(ch) if matches!(ch, '1' | '2' | '3') => {
                let option_index = ch.to_digit(10).unwrap_or(1) as usize - 1;
                let Some(request) = self.active_user_input().cloned() else {
                    self.show_user_input_modal = false;
                    self.status = "user input modal closed: no pending request".to_string();
                    return true;
                };
                let Some(question) = request.questions.get(self.user_input_question_index) else {
                    self.show_user_input_modal = false;
                    return true;
                };
                let Some(option) = question.options.get(option_index) else {
                    self.status = format!("question option {} is not available", option_index + 1);
                    return true;
                };
                self.submit_user_input_answer(option.label.clone());
            }
            KeyCode::Char('o') | KeyCode::Char('O') => {
                self.user_input_other_mode = true;
                self.user_input_other_value.clear();
                self.status = "typing other answer; Enter submits".to_string();
            }
            KeyCode::Esc => {
                if let Some(request) = self.active_user_input().cloned() {
                    if !self
                        .dismissed_user_input_ids
                        .iter()
                        .any(|id| id == &request.id)
                    {
                        self.dismissed_user_input_ids.push(request.id.clone());
                    }
                }
                self.active_user_input_id = None;
                self.user_input_answers.clear();
                self.user_input_question_index = 0;
                self.user_input_other_mode = false;
                self.user_input_other_value.clear();
                self.show_user_input_modal = false;
                self.status = "user input modal dismissed".to_string();
            }
            _ => {}
        }
        true
    }

    fn submit_user_input_answer(&mut self, answer: String) {
        let Some(request) = self.active_user_input().cloned() else {
            self.show_user_input_modal = false;
            self.status = "user input modal closed: no pending request".to_string();
            return;
        };
        let Some(question) = request.questions.get(self.user_input_question_index) else {
            self.show_user_input_modal = false;
            return;
        };
        self.user_input_answers.insert(question.id.clone(), answer);
        self.user_input_other_mode = false;
        self.user_input_other_value.clear();
        if self.user_input_question_index + 1 < request.questions.len() {
            self.user_input_question_index += 1;
            if let Some(next) = request.questions.get(self.user_input_question_index) {
                self.status = format!("answer next question: {}", clip_line(&next.header, 80));
            }
            return;
        }

        let request_id = request.id.clone();
        if !self
            .dismissed_user_input_ids
            .iter()
            .any(|id| id == &request_id)
        {
            self.dismissed_user_input_ids.push(request_id.clone());
        }
        self.pending_actions.push(TuiAction::RespondUserInput {
            thread_id: request.thread_id.clone(),
            turn_id: request.turn_id.clone(),
            request_id: request_id.clone(),
            answers: self.user_input_answers.clone(),
        });
        self.active_user_input_id = None;
        self.user_input_answers.clear();
        self.user_input_question_index = 0;
        self.show_user_input_modal = false;
        self.status = format!("user input answered: {request_id}");
    }

    fn handle_mcp_remove_confirmation_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Some(remove) = self.mcp_remove_confirmation.take() {
                    self.pending_actions.push(TuiAction::McpRemove {
                        scope: remove.scope,
                        name: remove.name.clone(),
                    });
                    self.status = format!(
                        "mcp {} server remove requested: {}",
                        remove.scope.label(),
                        remove.name
                    );
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                if let Some(remove) = self.mcp_remove_confirmation.take() {
                    self.status = format!(
                        "mcp {} server remove cancelled: {}",
                        remove.scope.label(),
                        remove.name
                    );
                }
            }
            _ => {
                self.status =
                    "confirm MCP server removal with y/Enter or cancel with n/Esc".to_string();
            }
        }
        true
    }

    fn handle_rollback_apply_confirmation_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Some(pending) = self.rollback_apply_confirmation.take() {
                    if let Some(hunk) = pending.hunk {
                        self.pending_actions.push(TuiAction::RestoreRollbackHunk {
                            id: pending.id.clone(),
                            hunk,
                            apply: true,
                        });
                        self.status =
                            format!("rollback hunk apply confirmed: {} #{hunk}", pending.id);
                    } else {
                        self.pending_actions.push(TuiAction::RevertTurn {
                            id: pending.id.clone(),
                            apply: true,
                        });
                        self.status = format!("rollback apply confirmed: {}", pending.id);
                    }
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                if let Some(pending) = self.rollback_apply_confirmation.take() {
                    self.status = format!("rollback apply cancelled: {}", pending.id);
                }
            }
            _ => {
                self.status =
                    "confirm rollback apply with y/Enter or cancel with n/Esc".to_string();
            }
        }
        true
    }

    fn request_cancel_run(&mut self) {
        let active_run = self
            .active_running_assistant_item()
            .map(|item| (item.thread_id.clone(), item.turn_id.clone()));
        if let Some((thread_id, turn_id)) = active_run {
            self.pending_actions.push(TuiAction::CancelRun {
                thread_id,
                turn_id: turn_id.clone(),
            });
            self.status = format!(
                "cancel requested for {}",
                turn_id.as_deref().unwrap_or("active run")
            );
        } else {
            self.status = "no running assistant item to cancel".to_string();
        }
    }

    fn scroll_transcript_up(&mut self, lines: usize) {
        let max_scroll = self.max_transcript_scroll();
        self.transcript_scroll = self.transcript_scroll.saturating_add(lines).min(max_scroll);
        self.status = if self.transcript_scroll == 0 {
            "transcript at latest".to_string()
        } else {
            format!("transcript scrolled back {} lines", self.transcript_scroll)
        };
    }

    fn scroll_transcript_down(&mut self, lines: usize) {
        self.transcript_scroll = self.transcript_scroll.saturating_sub(lines);
        self.status = if self.transcript_scroll == 0 {
            "transcript at latest".to_string()
        } else {
            format!("transcript scrolled back {} lines", self.transcript_scroll)
        };
    }

    fn scroll_transcript_to_top(&mut self) {
        self.transcript_scroll = self.max_transcript_scroll();
        self.status = if self.transcript_scroll == 0 {
            "transcript at latest".to_string()
        } else {
            "transcript at oldest".to_string()
        };
    }

    fn scroll_transcript_to_latest(&mut self) {
        self.transcript_scroll = 0;
        self.status = "transcript at latest".to_string();
    }

    fn max_transcript_scroll(&self) -> usize {
        self.transcript.len().saturating_sub(1)
    }

    fn scroll_mcp_detail_up(&mut self, lines: usize) {
        self.mcp_detail_scroll = self.mcp_detail_scroll.saturating_sub(lines);
        self.status = if self.mcp_detail_scroll == 0 {
            "mcp detail at top".to_string()
        } else {
            format!("mcp detail scrolled {} lines", self.mcp_detail_scroll)
        };
    }

    fn scroll_mcp_detail_down(&mut self, lines: usize) {
        let max_scroll = self.max_mcp_detail_scroll();
        self.mcp_detail_scroll = self.mcp_detail_scroll.saturating_add(lines).min(max_scroll);
        self.status = if self.mcp_detail_scroll == max_scroll {
            "mcp detail at bottom".to_string()
        } else {
            format!("mcp detail scrolled {} lines", self.mcp_detail_scroll)
        };
    }

    fn scroll_mcp_detail_to_top(&mut self) {
        self.mcp_detail_scroll = 0;
        self.status = "mcp detail at top".to_string();
    }

    fn scroll_mcp_detail_to_bottom(&mut self) {
        self.mcp_detail_scroll = self.max_mcp_detail_scroll();
        self.status = if self.mcp_detail_scroll == 0 {
            "mcp detail at top".to_string()
        } else {
            "mcp detail at bottom".to_string()
        };
    }

    fn max_mcp_detail_scroll(&self) -> usize {
        self.mcp_detail
            .as_ref()
            .map(|(_, detail)| detail.lines().count().saturating_sub(1))
            .unwrap_or(0)
    }

    fn request_compact_thread_from_arg(&mut self, keep_tail: &str) {
        match keep_tail.parse::<usize>() {
            Ok(value) if value <= MAX_TUI_COMPACTION_KEEP_TAIL_TURNS => {
                self.request_compact_thread(value);
            }
            Ok(_) => {
                self.status = format!(
                    "compact keep_tail_turns must be <= {MAX_TUI_COMPACTION_KEEP_TAIL_TURNS}"
                );
            }
            Err(_) => {
                self.status = format!("invalid compact keep_tail_turns: {keep_tail}");
            }
        }
    }

    fn request_compact_thread(&mut self, keep_tail_turns: usize) {
        let Some(thread_id) = self.selected_thread_id.clone() else {
            self.status = "no active durable thread to compact".to_string();
            return;
        };
        self.pending_actions.push(TuiAction::CompactThread {
            thread_id: thread_id.clone(),
            keep_tail_turns,
        });
        self.status =
            format!("compaction requested for {thread_id} (keep_tail_turns={keep_tail_turns})");
    }

    fn submit_or_queue_user_message(&mut self, thread_id: String, content: String) {
        if self.active_thread_busy() {
            self.queued_messages
                .push_back(TuiQueuedMessage { thread_id, content });
            self.status = format!(
                "message queued for next turn ({} queued)",
                self.queued_messages.len()
            );
            return;
        }
        self.pending_actions
            .push(TuiAction::SubmitUserMessage { thread_id, content });
        self.status = "submitting composer message".to_string();
    }

    fn dispatch_next_queued_message_after_idle(&mut self, was_busy: bool) {
        if !was_busy || self.active_thread_busy() || self.queued_draft.is_some() {
            return;
        }
        let Some(message) = self.queued_messages.pop_front() else {
            return;
        };
        let thread_id = message.thread_id.clone();
        self.pending_actions.push(TuiAction::SubmitUserMessage {
            thread_id: message.thread_id,
            content: message.content,
        });
        self.status = format!(
            "submitted queued message to {thread_id} ({} remaining)",
            self.queued_messages.len()
        );
    }

    fn request_custom_slash_command(&mut self, command: String, args: Vec<String>) -> bool {
        let Some(thread_id) = self.selected_thread_id.clone() else {
            self.status = "custom slash command has no active durable thread".to_string();
            return false;
        };
        self.pending_actions.push(TuiAction::RunCustomSlashCommand {
            thread_id,
            command: command.clone(),
            args,
        });
        self.status = format!("custom slash command queued: {command}");
        true
    }

    fn request_session_rename(&mut self, title: String) -> bool {
        let Some(session) = self.selected_session().cloned() else {
            self.status = "no active durable session to rename".to_string();
            return false;
        };
        if session.status == "empty" && session.id == "local" {
            self.status = "no active durable session to rename".to_string();
            return false;
        }
        self.pending_actions.push(TuiAction::RenameSession {
            session_id: session.id.clone(),
            title: title.clone(),
        });
        self.status = format!("session rename queued: {}", clip_line(&title, 60));
        true
    }

    pub fn rename_session_title(&mut self, session_id: &str, title: String) {
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.title = title;
        }
    }

    fn request_project_instructions_init(&mut self) {
        let workspace = self
            .selected_session()
            .map(|session| session.workspace.clone())
            .unwrap_or_else(|| ".".to_string());
        self.pending_actions
            .push(TuiAction::InitProjectInstructions {
                workspace: workspace.clone(),
            });
        self.status = format!("project instructions init queued: {workspace}");
    }

    fn request_network_command(&mut self, command: TuiNetworkCommand) {
        let workspace = self
            .selected_session()
            .map(|session| session.workspace.clone())
            .unwrap_or_else(|| ".".to_string());
        self.pending_actions.push(TuiAction::Network {
            workspace: workspace.clone(),
            command,
        });
        self.status = format!("network command queued: {workspace}");
    }

    fn handle_lsp_command(&mut self, command: TuiLspCommand) {
        if matches!(command, TuiLspCommand::Help) {
            self.set_mcp_detail(TuiMcpDetailKind::Lsp, self.render_lsp_help_detail());
            self.status = "lsp help shown".to_string();
            return;
        }
        let workspace = self
            .selected_session()
            .map(|session| session.workspace.clone())
            .unwrap_or_else(|| ".".to_string());
        self.pending_actions.push(TuiAction::Lsp {
            workspace: workspace.clone(),
            command,
        });
        self.status = format!("lsp command queued: {workspace}");
    }

    fn render_lsp_help_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode LSP");
        let _ = writeln!(detail, "================");
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "/lsp [on|off|status] controls post-edit diagnostics for the selected workspace."
        );
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Commands");
        let _ = writeln!(detail, "--------");
        let _ = writeln!(detail, "- /lsp status");
        let _ = writeln!(detail, "- /lsp on");
        let _ = writeln!(detail, "- /lsp off");
        if let Some(session) = self.selected_session() {
            let _ = writeln!(detail);
            push_status_row(&mut detail, "Selected workspace:", &session.workspace);
        }
        detail
    }

    fn request_model_command(&mut self, command: TuiModelCommand) {
        let workspace = self
            .selected_session()
            .map(|session| session.workspace.clone())
            .unwrap_or_else(|| ".".to_string());
        self.pending_actions.push(TuiAction::Model {
            workspace: workspace.clone(),
            command,
        });
        self.status = format!("model command queued: {workspace}");
    }

    fn request_provider_command(&mut self, command: TuiProviderCommand) {
        let workspace = self
            .selected_session()
            .map(|session| session.workspace.clone())
            .unwrap_or_else(|| ".".to_string());
        self.pending_actions.push(TuiAction::Provider {
            workspace: workspace.clone(),
            command,
        });
        self.status = format!("provider command queued: {workspace}");
    }

    fn request_skills_command(&mut self, command: TuiSkillsCommand) {
        self.pending_actions.push(TuiAction::Skills { command });
        self.status = "skills command queued".to_string();
    }

    fn request_note_command(&mut self, command: TuiNoteCommand) {
        self.pending_actions.push(TuiAction::Note { command });
        self.status = "note command queued".to_string();
    }

    fn handle_subagents_command(&mut self, command: TuiSubagentsCommand) {
        match command {
            TuiSubagentsCommand::List => {
                self.set_mcp_detail(TuiMcpDetailKind::Subagents, self.render_subagents_detail());
                self.status = "subagents listed".to_string();
            }
            TuiSubagentsCommand::Spawn { max_depth, task } => {
                self.request_subagent_task(max_depth, task);
            }
            TuiSubagentsCommand::Help => {
                self.set_mcp_detail(
                    TuiMcpDetailKind::Subagents,
                    self.render_subagents_help_detail(),
                );
                self.status = "subagents help shown".to_string();
            }
        }
    }

    fn request_subagent_task(&mut self, max_depth: usize, task: String) {
        let task = task.trim().to_string();
        if task.is_empty() {
            self.status = "subagent task is empty".to_string();
            return;
        }
        let Some(thread_id) = self.selected_thread_id.clone() else {
            self.status = "no active durable thread for subagent".to_string();
            return;
        };
        self.pending_actions.push(TuiAction::CreateSubagentTask {
            thread_id: thread_id.clone(),
            task: task.clone(),
            max_depth,
        });
        self.status = format!(
            "subagent queued for {thread_id} (depth={max_depth}): {}",
            clip_line(&task, 60)
        );
    }

    fn render_subagents_help_detail(&self) -> String {
        let mut detail = self.render_subagents_detail();
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Usage");
        let _ = writeln!(detail, "-----");
        let _ = writeln!(
            detail,
            "- /subagents            List active-thread sub-agent tasks"
        );
        let _ = writeln!(
            detail,
            "- /agent [0-3] <task>   Queue a persistent sub-agent task"
        );
        detail
    }

    fn render_subagents_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Subagents");
        let _ = writeln!(detail, "======================");
        let _ = writeln!(detail);
        match self.active_thread() {
            Some(thread) => {
                push_status_row(
                    &mut detail,
                    "Thread:",
                    &format!("{} [{}]", thread.title, thread.id),
                );
            }
            None => {
                let _ = writeln!(detail, "No active durable thread selected.");
                return detail;
            }
        }

        let subagents = self
            .active_thread_tasks()
            .into_iter()
            .filter(|task| task.kind == "subagent" || task.kind == "subagent_input")
            .collect::<Vec<_>>();
        push_status_row(
            &mut detail,
            "Sub-agent tasks:",
            &subagents.len().to_string(),
        );
        let _ = writeln!(detail);
        if subagents.is_empty() {
            let _ = writeln!(detail, "No runtime-backed sub-agent tasks in this thread.");
            let _ = writeln!(detail, "Use /agent <task> to queue one.");
            return detail;
        }

        for task in subagents {
            let _ = writeln!(
                detail,
                "- {} [{}] {}",
                task.id,
                task.status,
                clip_line(&task.summary, 100)
            );
            let _ = writeln!(
                detail,
                "  kind={} updated={} parent={}",
                task.kind,
                task.updated_at,
                task.parent_task_id.as_deref().unwrap_or("-")
            );
        }
        detail
    }

    fn handle_rlm_command(&mut self, command: TuiRlmCommand) {
        match command {
            TuiRlmCommand::Start { max_depth, target } => {
                self.request_rlm_session(max_depth, target);
            }
            TuiRlmCommand::Help => {
                self.set_mcp_detail(TuiMcpDetailKind::Rlm, self.render_rlm_help_detail());
                self.status = "rlm help shown".to_string();
            }
        }
    }

    fn request_rlm_session(&mut self, max_depth: usize, target: String) {
        let target = target.trim().to_string();
        if target.is_empty() {
            self.status = "rlm target is empty".to_string();
            return;
        }
        let Some(thread_id) = self.selected_thread_id.clone() else {
            self.status = "no active durable thread for rlm".to_string();
            return;
        };
        let content = self.render_rlm_user_message(max_depth, &target);
        self.pending_actions.push(TuiAction::SubmitUserMessage {
            thread_id: thread_id.clone(),
            content,
        });
        self.status = format!(
            "rlm request queued for {thread_id} (depth={max_depth}): {}",
            clip_line(&target, 60)
        );
    }

    fn render_rlm_help_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode RLM");
        let _ = writeln!(detail, "================");
        let _ = writeln!(detail);
        match self.active_thread() {
            Some(thread) => {
                push_status_row(
                    &mut detail,
                    "Thread:",
                    &format!("{} [{}]", thread.title, thread.id),
                );
            }
            None => {
                let _ = writeln!(detail, "No active durable thread selected.");
            }
        }
        if let Some(session) = self.selected_session() {
            push_status_row(&mut detail, "Workspace:", &session.workspace);
        }
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Usage");
        let _ = writeln!(detail, "-----");
        let _ = writeln!(
            detail,
            "- /rlm [0-3] <file_or_text>        Open a persistent RLM process"
        );
        let _ = writeln!(detail, "- /recursive [0-3] <file_or_text>  Alias for /rlm");
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "Existing files are passed as file_path; other targets are passed as content."
        );
        detail
    }

    fn render_rlm_user_message(&self, max_depth: usize, target: &str) -> String {
        let source_arg = self.rlm_source_arg(target);
        let session_id = self
            .selected_thread_id
            .as_deref()
            .map(|thread_id| format!("slash_rlm_{}", stable_tui_segment(thread_id)))
            .unwrap_or_else(|| "slash_rlm".to_string());
        format!(
            "Open and use a persistent RLM process for this request. Call `rlm_process` with `live=true`, `session_id: {session_id:?}`, `task: {:?}`, {source_arg}, and `max_depth: {max_depth}`. Use `rlm_process_wait` or `rlm_process_events` to inspect progress, and use the returned context before answering.",
            format!("Analyze this RLM target: {target}")
        )
    }

    fn rlm_source_arg(&self, target: &str) -> String {
        let target_path = Path::new(target);
        let exists = if target_path.is_absolute() {
            target_path.exists()
        } else {
            self.selected_session()
                .map(|session| Path::new(&session.workspace).join(target).exists())
                .unwrap_or_else(|| target_path.exists())
        };
        if exists {
            format!("`file_path: {target:?}`")
        } else {
            format!("`content: {target:?}`")
        }
    }

    fn handle_relay_command(&mut self, command: TuiRelayCommand) {
        match command {
            TuiRelayCommand::Create { focus } => {
                self.request_session_relay(focus);
            }
            TuiRelayCommand::Help => {
                self.set_mcp_detail(TuiMcpDetailKind::Relay, self.render_relay_help_detail());
                self.status = "relay help shown".to_string();
            }
        }
    }

    fn request_session_relay(&mut self, focus: Option<String>) {
        let Some(thread_id) = self.selected_thread_id.clone() else {
            self.status = "no active durable thread for relay".to_string();
            return;
        };
        let content = self.render_relay_user_message(focus.as_deref());
        self.pending_actions.push(TuiAction::SubmitUserMessage {
            thread_id: thread_id.clone(),
            content,
        });
        self.status = match focus
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(focus) => format!(
                "relay request queued for {thread_id}: {}",
                clip_line(focus, 60)
            ),
            None => format!("relay request queued for {thread_id}"),
        };
    }

    fn render_relay_help_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Relay");
        let _ = writeln!(detail, "==================");
        let _ = writeln!(detail);
        match self.active_thread() {
            Some(thread) => {
                push_status_row(
                    &mut detail,
                    "Thread:",
                    &format!("{} [{}]", thread.title, thread.id),
                );
            }
            None => {
                let _ = writeln!(detail, "No active durable thread selected.");
            }
        }
        if let Some(session) = self.selected_session() {
            push_status_row(&mut detail, "Workspace:", &session.workspace);
        }
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Usage");
        let _ = writeln!(detail, "-----");
        let _ = writeln!(detail, "- /relay [focus]     Write a compact handoff brief");
        let _ = writeln!(detail, "- /batonpass [focus] Alias for /relay");
        let _ = writeln!(detail, "- /接力 [focus]       Alias for /relay");
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "The relay target is `.dscode/handoff.md`, DeepSeekCode's equivalent of DeepSeek-TUI's `.deepseek/handoff.md`."
        );
        detail
    }

    fn render_relay_user_message(&self, focus: Option<&str>) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "Create a compact session relay (接力) for a future DeepSeekCode thread."
        );
        let _ = writeln!(out);
        let _ = writeln!(out, "Write or update `.dscode/handoff.md`.");
        let _ = writeln!(
            out,
            "This is DeepSeekCode's equivalent of DeepSeek-TUI's `.deepseek/handoff.md`; title the artifact `# Session relay`."
        );
        let _ = writeln!(out);
        let _ = writeln!(out, "Current session snapshot:");
        if let Some(session) = self.selected_session() {
            let _ = writeln!(out, "- Workspace: {}", session.workspace);
            let _ = writeln!(out, "- Session: {} [{}]", session.title, session.id);
        }
        if let Some(thread) = self.active_thread() {
            let _ = writeln!(out, "- Thread: {} [{}]", thread.title, thread.id);
            let _ = writeln!(out, "- Thread mode: {}", thread.mode);
        }
        let _ = writeln!(out, "- TUI mode: {}", self.mode.title());
        if let Some(focus) = focus.map(str::trim).filter(|value| !value.is_empty()) {
            let _ = writeln!(out, "- Requested relay focus: {focus}");
        }
        if let Some(goal) = self.goal_objective.as_deref() {
            let _ = writeln!(out, "- Goal: {goal}");
        }
        if let Some(budget) = self.goal_token_budget {
            let _ = writeln!(out, "- Goal token budget: {budget}");
        }
        let tasks = self.active_thread_tasks();
        if !tasks.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "Active-thread tasks:");
            for task in tasks.into_iter().take(8) {
                let _ = writeln!(
                    out,
                    "- {} [{}] {}",
                    task.id,
                    task.status,
                    clip_line(&task.summary, 120)
                );
            }
        }
        let _ = writeln!(
            out,
            "\nBefore writing, inspect the current transcript context and any live tool evidence you need. Do not invent test results, file changes, blockers, or decisions."
        );
        let _ = writeln!(
            out,
            "\nUse this compact structure:\n# Session relay\n\n## Goal\n[the user's objective and explicit constraints]\n\n## Current work\n[what is mid-flight and what changed recently]\n\n## Evidence\n[commands, tests, files, commits, or links actually observed]\n\n## Open blockers\n[unknowns, failures, or risks]\n\n## Next steps\n[the shortest actionable continuation path]"
        );
        out
    }

    fn request_anchor_command(&mut self, command: TuiAnchorCommand) {
        let workspace = self
            .selected_session()
            .map(|session| session.workspace.clone())
            .unwrap_or_else(|| ".".to_string());
        self.pending_actions
            .push(TuiAction::Anchor { workspace, command });
        self.status = "anchor command queued".to_string();
    }

    fn request_hooks_command(&mut self, command: TuiHooksCommand) {
        self.pending_actions.push(TuiAction::Hooks { command });
        self.status = "hooks command queued".to_string();
    }

    fn handle_queue_command(&mut self, command: TuiQueueCommand) {
        match command {
            TuiQueueCommand::List => self.show_queue_detail(),
            TuiQueueCommand::Help => {
                self.set_mcp_detail(TuiMcpDetailKind::Queue, self.render_queue_help_detail());
                self.status = "queue help shown".to_string();
            }
            TuiQueueCommand::Edit { index } => self.edit_queued_message(index),
            TuiQueueCommand::Drop { index } => self.drop_queued_message(index),
            TuiQueueCommand::Clear => self.clear_queued_messages(),
        }
    }

    fn show_queue_detail(&mut self) {
        self.set_mcp_detail(TuiMcpDetailKind::Queue, self.render_queue_detail());
        self.status = format!("queue listed: {} message(s)", self.queued_messages.len());
    }

    fn edit_queued_message(&mut self, index: usize) {
        if self.queued_draft.is_some() {
            self.status = "already editing a queued message".to_string();
            return;
        }
        let Some(position) = index.checked_sub(1) else {
            self.status = "queue index must be >= 1".to_string();
            return;
        };
        let Some(message) = self.queued_messages.remove(position) else {
            self.status = format!("queued message {index} not found");
            return;
        };
        self.composer = message.content.clone();
        self.composer_cursor = self.composer.len();
        self.composer_focused = true;
        self.queued_draft = Some(message);
        self.set_mcp_detail(TuiMcpDetailKind::Queue, self.render_queue_detail());
        self.status = format!("editing queued message {index}");
    }

    fn drop_queued_message(&mut self, index: usize) {
        let Some(position) = index.checked_sub(1) else {
            self.status = "queue index must be >= 1".to_string();
            return;
        };
        if self.queued_messages.remove(position).is_none() {
            self.status = format!("queued message {index} not found");
            return;
        }
        self.set_mcp_detail(TuiMcpDetailKind::Queue, self.render_queue_detail());
        self.status = format!("dropped queued message {index}");
    }

    fn clear_queued_messages(&mut self) {
        let queued = self.queued_messages.len();
        let had_draft = self.queued_draft.take().is_some();
        self.queued_messages.clear();
        if had_draft {
            self.composer.clear();
            self.composer_cursor = 0;
        }
        self.set_mcp_detail(TuiMcpDetailKind::Queue, self.render_queue_detail());
        self.status = if queued == 0 && !had_draft {
            "queue already empty".to_string()
        } else {
            "queue cleared".to_string()
        };
    }

    fn render_queue_help_detail(&self) -> String {
        let mut detail = self.render_queue_detail();
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Usage");
        let _ = writeln!(detail, "-----");
        let _ = writeln!(
            detail,
            "- /queue                 List queued follow-up messages"
        );
        let _ = writeln!(
            detail,
            "- /queue edit <n>        Move one queued message into the composer"
        );
        let _ = writeln!(detail, "- /queue drop <n>        Remove one queued message");
        let _ = writeln!(
            detail,
            "- /queue clear           Clear queued messages and any edit draft"
        );
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "Plain composer input is queued automatically while an assistant message is still running."
        );
        detail
    }

    fn render_queue_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Queue");
        let _ = writeln!(detail, "==================");
        let _ = writeln!(detail);
        push_status_row(
            &mut detail,
            "Active turn:",
            if self.active_thread_busy() {
                "busy"
            } else {
                "idle"
            },
        );
        push_status_row(
            &mut detail,
            "Queued:",
            &format!("{} message(s)", self.queued_messages.len()),
        );
        if let Some(draft) = self.queued_draft.as_ref() {
            push_status_row(
                &mut detail,
                "Editing:",
                &format!(
                    "{} -> {}",
                    draft.thread_id,
                    tui_queue_preview(&draft.content)
                ),
            );
        }
        let _ = writeln!(detail);
        if self.queued_messages.is_empty() {
            let _ = writeln!(detail, "No queued messages.");
        } else {
            let _ = writeln!(detail, "Queued messages:");
            for (index, message) in self.queued_messages.iter().enumerate() {
                let _ = writeln!(
                    detail,
                    "{}. [{}] {}",
                    index + 1,
                    message.thread_id,
                    tui_queue_preview(&message.content)
                );
            }
        }
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "Tip: /queue edit <n> to edit, /queue drop <n> to remove."
        );
        detail
    }

    fn handle_share_command(&mut self, command: TuiShareCommand) {
        match command {
            TuiShareCommand::Export => self.request_share_session(),
            TuiShareCommand::Help => {
                self.set_mcp_detail(TuiMcpDetailKind::Share, self.render_share_help_detail());
                self.status = "share help shown".to_string();
            }
        }
    }

    fn request_share_session(&mut self) {
        let Some(thread_id) = self.selected_thread_id.clone() else {
            self.status = "no active durable thread to share".to_string();
            return;
        };
        self.pending_actions
            .push(TuiAction::ShareSession { thread_id });
        self.status =
            "share export queued; requires local runtime and authenticated gh CLI".to_string();
    }

    fn render_share_help_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Share");
        let _ = writeln!(detail, "==================");
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "/share exports the active durable thread transcript as standalone HTML."
        );
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Upload");
        let _ = writeln!(detail, "------");
        let _ = writeln!(
            detail,
            "- Uses `gh gist create --public` when the GitHub CLI is installed and authenticated."
        );
        let _ = writeln!(
            detail,
            "- Keeps the local HTML export path in the detail panel even if upload fails."
        );
        let _ = writeln!(detail);
        match self.active_thread() {
            Some(thread) => {
                push_status_row(
                    &mut detail,
                    "Thread:",
                    &format!("{} [{}]", thread.title, thread.id),
                );
                push_status_row(
                    &mut detail,
                    "Transcript items:",
                    &self.active_thread_items().len().to_string(),
                );
            }
            None => push_status_row(&mut detail, "Thread:", "none selected"),
        }
        detail
    }

    fn handle_export_command(&mut self, command: TuiExportCommand) {
        match command {
            TuiExportCommand::Export { path } => self.request_export_thread(path),
            TuiExportCommand::Help => {
                self.set_mcp_detail(TuiMcpDetailKind::Export, self.render_export_help_detail());
                self.status = "export help shown".to_string();
            }
        }
    }

    fn request_export_thread(&mut self, path: Option<String>) {
        let Some(thread_id) = self.selected_thread_id.clone() else {
            self.status = "no active durable thread to export".to_string();
            return;
        };
        self.pending_actions.push(TuiAction::ExportThread {
            thread_id,
            path: path.clone(),
        });
        self.status = match path {
            Some(path) => format!("markdown export queued: {path}"),
            None => "markdown export queued".to_string(),
        };
    }

    fn render_export_help_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Export");
        let _ = writeln!(detail, "===================");
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "/export [path] writes the active durable thread transcript to Markdown."
        );
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Paths");
        let _ = writeln!(detail, "-----");
        let _ = writeln!(
            detail,
            "- No path: writes chat_export_<timestamp>.md in the selected workspace."
        );
        let _ = writeln!(
            detail,
            "- Relative path: resolved inside the selected workspace."
        );
        let _ = writeln!(detail, "- Absolute path or ~/path: used directly.");
        let _ = writeln!(detail);
        match self.active_thread() {
            Some(thread) => {
                push_status_row(
                    &mut detail,
                    "Thread:",
                    &format!("{} [{}]", thread.title, thread.id),
                );
                push_status_row(
                    &mut detail,
                    "Transcript items:",
                    &self.active_thread_items().len().to_string(),
                );
            }
            None => {
                let _ = writeln!(detail, "No active durable thread selected.");
            }
        }
        detail
    }

    fn handle_save_command(&mut self, command: TuiSaveCommand) {
        match command {
            TuiSaveCommand::Save { path } => self.request_save_session(path),
            TuiSaveCommand::Help => {
                self.set_mcp_detail(TuiMcpDetailKind::Save, self.render_save_help_detail());
                self.status = "save help shown".to_string();
            }
        }
    }

    fn request_save_session(&mut self, path: Option<String>) {
        let Some(session) = self.selected_session().cloned() else {
            self.status = "no durable session to save".to_string();
            return;
        };
        if session.status == "empty" && session.id == "local" {
            self.status = "no durable session to save".to_string();
            return;
        }
        let Some(thread_id) = self.selected_thread_id.clone() else {
            self.status = "no active durable thread to save".to_string();
            return;
        };
        self.pending_actions.push(TuiAction::SaveSession {
            session_id: session.id,
            thread_id,
            path: path.clone(),
        });
        self.status = match path {
            Some(path) => format!("session snapshot save queued: {path}"),
            None => "session snapshot save queued".to_string(),
        };
    }

    fn render_save_help_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Save");
        let _ = writeln!(detail, "=================");
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "/save [path] writes the active durable session and thread to a JSON snapshot."
        );
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Paths");
        let _ = writeln!(detail, "-----");
        let _ = writeln!(
            detail,
            "- No path: writes session_<timestamp>.json in the selected workspace."
        );
        let _ = writeln!(
            detail,
            "- Relative path: resolved inside the selected workspace."
        );
        let _ = writeln!(detail, "- Absolute path or ~/path: used directly.");
        let _ = writeln!(detail);
        match (self.selected_session(), self.active_thread()) {
            (Some(session), Some(thread)) => {
                push_status_row(
                    &mut detail,
                    "Session:",
                    &format!("{} [{}]", session.title, session.id),
                );
                push_status_row(
                    &mut detail,
                    "Thread:",
                    &format!("{} [{}]", thread.title, thread.id),
                );
                push_status_row(
                    &mut detail,
                    "Transcript items:",
                    &self.active_thread_items().len().to_string(),
                );
            }
            _ => {
                let _ = writeln!(detail, "No active durable session and thread selected.");
            }
        }
        detail
    }

    fn handle_load_command(&mut self, command: TuiLoadCommand) {
        match command {
            TuiLoadCommand::Load { path } => self.request_load_session(path),
            TuiLoadCommand::Help => {
                self.set_mcp_detail(TuiMcpDetailKind::Load, self.render_load_help_detail());
                self.status = "load help shown".to_string();
            }
        }
    }

    fn request_load_session(&mut self, path: String) {
        let workspace = self
            .selected_session()
            .map(|session| session.workspace.clone())
            .unwrap_or_else(|| ".".to_string());
        self.pending_actions.push(TuiAction::LoadSession {
            workspace,
            path: path.clone(),
        });
        self.status = format!("session snapshot load queued: {path}");
    }

    fn render_load_help_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Load");
        let _ = writeln!(detail, "=================");
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "/load <path> imports a DeepSeekCode TUI session snapshot into a new durable session."
        );
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Paths");
        let _ = writeln!(detail, "-----");
        let _ = writeln!(
            detail,
            "- Relative path: resolved inside the selected workspace."
        );
        let _ = writeln!(detail, "- Absolute path or ~/path: used directly.");
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "Import creates fresh runtime ids instead of overwriting existing sessions."
        );
        if let Some(session) = self.selected_session() {
            let _ = writeln!(detail);
            push_status_row(&mut detail, "Selected workspace:", &session.workspace);
        }
        detail
    }

    fn handle_attach_command(&mut self, command: TuiAttachCommand) {
        match command {
            TuiAttachCommand::Attach { path } => self.attach_media_to_composer(&path),
            TuiAttachCommand::Help => {
                self.set_mcp_detail(TuiMcpDetailKind::Attach, self.render_attach_help_detail());
                self.status = "attach help shown".to_string();
            }
        }
    }

    fn attach_media_to_composer(&mut self, raw_path: &str) {
        let workspace = self
            .selected_session()
            .map(|session| PathBuf::from(&session.workspace))
            .unwrap_or_else(|| PathBuf::from("."));
        let path = resolve_tui_attachment_path(raw_path, &workspace);
        let canonical_path = match path.canonicalize() {
            Ok(path) => path,
            Err(_) => {
                self.status = format!("attachment not found: {}", path.display());
                return;
            }
        };
        if !canonical_path.is_file() {
            self.status = format!("attachment is not a file: {}", canonical_path.display());
            return;
        }
        let Some(kind) = tui_media_kind(&canonical_path) else {
            self.status =
                "unsupported attachment type; use image/video paths or @path for text".to_string();
            return;
        };
        let reference = tui_attachment_reference(&canonical_path, &workspace);
        let block = render_tui_attachment_block(kind, &reference, &canonical_path);
        self.insert_attachment_block(block);
        self.status = format!("attached {kind}: {}", canonical_path.display());
    }

    fn insert_attachment_block(&mut self, block: String) {
        if self.composer.trim().is_empty() || parse_tui_attach_command(&self.composer).is_some() {
            self.composer = block;
        } else {
            if !self.composer.ends_with('\n') {
                self.composer.push('\n');
            }
            self.composer.push('\n');
            self.composer.push_str(&block);
        }
        self.composer_cursor = self.composer.len();
        self.composer_focused = true;
    }

    fn render_attach_help_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Attach");
        let _ = writeln!(detail, "===================");
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "/attach <path> inserts an image or video attachment reference into the composer."
        );
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Aliases");
        let _ = writeln!(detail, "-------");
        let _ = writeln!(detail, "- /image <path>");
        let _ = writeln!(detail, "- /media <path>");
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "Supported images: png, jpg, jpeg, gif, webp, bmp, tif, tiff, ppm"
        );
        let _ = writeln!(detail, "Supported videos: mp4, mov, m4v, webm, avi, mkv");
        if let Some(session) = self.selected_session() {
            let _ = writeln!(detail);
            push_status_row(&mut detail, "Selected workspace:", &session.workspace);
        }
        detail
    }

    fn handle_change_command(&mut self, command: TuiChangeCommand) {
        let detail = match command {
            TuiChangeCommand::Show => {
                self.status = "changelog shown".to_string();
                self.render_change_detail()
            }
            TuiChangeCommand::Help => {
                self.status = "change help shown".to_string();
                self.render_change_help_detail()
            }
        };
        self.set_mcp_detail(TuiMcpDetailKind::Change, detail);
    }

    fn render_change_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Changelog");
        let _ = writeln!(detail, "======================");
        let _ = writeln!(detail);
        match latest_changelog_section(DEEPSEEKCODE_CHANGELOG) {
            Some(section) => detail.push_str(&inline_changelog_section(&section)),
            None => detail.push_str("No changelog version section found."),
        }
        detail
    }

    fn render_change_help_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Change");
        let _ = writeln!(detail, "===================");
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "/change shows the latest bundled DeepSeekCode changelog entry."
        );
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Aliases");
        let _ = writeln!(detail, "-------");
        let _ = writeln!(detail, "- /changes");
        let _ = writeln!(detail, "- /changelog");
        detail
    }

    fn handle_system_command(&mut self, command: TuiSystemCommand) {
        match command {
            TuiSystemCommand::Show => self.request_system_prompt_preview(),
            TuiSystemCommand::Help => {
                self.set_mcp_detail(TuiMcpDetailKind::System, self.render_system_help_detail());
                self.status = "system help shown".to_string();
            }
        }
    }

    fn request_system_prompt_preview(&mut self) {
        let workspace = self
            .selected_session()
            .map(|session| session.workspace.clone())
            .unwrap_or_else(|| ".".to_string());
        let task = self.latest_selected_user_message();
        self.pending_actions.push(TuiAction::ShowSystemPrompt {
            workspace: workspace.clone(),
            mode: self.mode,
            task,
        });
        self.status = format!("system prompt queued: {workspace}");
    }

    fn latest_selected_user_message(&self) -> Option<String> {
        let thread_id = self.selected_thread_id.as_deref()?;
        self.items
            .iter()
            .filter(|item| {
                item.thread_id == thread_id
                    && item.item_type == "message"
                    && item.role.as_deref() == Some("user")
                    && !item.content.trim().is_empty()
            })
            .max_by_key(|item| item.index)
            .map(|item| item.content.clone())
    }

    fn render_system_help_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode System Prompt");
        let _ = writeln!(detail, "==========================");
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "/system shows the selected workspace runtime system prompt preview."
        );
        let _ = writeln!(
            detail,
            "The preview uses the latest selected user message when one exists."
        );
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Commands");
        let _ = writeln!(detail, "--------");
        let _ = writeln!(detail, "- /system");
        let _ = writeln!(detail, "- /system help");
        if let Some(session) = self.selected_session() {
            let _ = writeln!(detail);
            push_status_row(&mut detail, "Selected workspace:", &session.workspace);
        }
        detail
    }

    fn handle_edit_command(&mut self, command: TuiEditCommand) -> bool {
        match command {
            TuiEditCommand::LoadLast => self.load_latest_user_message_into_composer(),
            TuiEditCommand::Help => {
                self.set_mcp_detail(TuiMcpDetailKind::Edit, self.render_edit_help_detail());
                self.status = "edit help shown".to_string();
                false
            }
        }
    }

    fn load_latest_user_message_into_composer(&mut self) -> bool {
        let Some(content) = self.latest_selected_user_message() else {
            self.status = "no previous message to edit".to_string();
            return false;
        };
        self.composer = content;
        self.composer_cursor = self.composer.len();
        self.composer_focused = true;
        self.status = "last user message loaded into composer".to_string();
        true
    }

    fn render_edit_help_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Edit");
        let _ = writeln!(detail, "=================");
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "/edit loads the latest selected user message back into the composer."
        );
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Commands");
        let _ = writeln!(detail, "--------");
        let _ = writeln!(detail, "- /edit");
        let _ = writeln!(detail, "- /edit help");
        detail
    }

    fn handle_clear_command(&mut self, command: TuiClearCommand) {
        match command {
            TuiClearCommand::Clear => self.request_clear_conversation(),
            TuiClearCommand::Help => {
                self.set_mcp_detail(TuiMcpDetailKind::Clear, self.render_clear_help_detail());
                self.status = "clear help shown".to_string();
            }
        }
    }

    fn request_clear_conversation(&mut self) {
        let Some(session) = self.selected_session().cloned() else {
            self.status = "no durable session to clear".to_string();
            return;
        };
        if session.status == "empty" && session.id == "local" {
            self.status = "no durable session to clear".to_string();
            return;
        }
        self.pending_actions.push(TuiAction::ClearConversation {
            session_id: session.id.clone(),
            previous_thread_id: self.selected_thread_id.clone(),
        });
        self.status = format!("clear conversation queued for session {}", session.id);
    }

    fn render_clear_help_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Clear");
        let _ = writeln!(detail, "==================");
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "/clear starts a fresh active thread in the selected durable session."
        );
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "Existing durable history is kept. The selected session points at the new empty thread, queued follow-ups are dropped, and the visible transcript becomes empty."
        );
        let _ = writeln!(detail);
        match self.selected_session() {
            Some(session) if !(session.status == "empty" && session.id == "local") => {
                push_status_row(
                    &mut detail,
                    "Session:",
                    &format!("{} [{}]", session.title, session.id),
                );
                if let Some(thread) = self.active_thread() {
                    push_status_row(
                        &mut detail,
                        "Current thread:",
                        &format!("{} [{}]", thread.title, thread.id),
                    );
                }
            }
            _ => {
                let _ = writeln!(detail, "No durable session is selected.");
            }
        }
        detail
    }

    fn handle_diff_command(&mut self, command: TuiDiffCommand) {
        match command {
            TuiDiffCommand::Show => self.request_diff_workspace(),
            TuiDiffCommand::Help => {
                self.set_mcp_detail(TuiMcpDetailKind::Diff, self.render_diff_help_detail());
                self.status = "diff help shown".to_string();
            }
        }
    }

    fn request_diff_workspace(&mut self) {
        let workspace = self
            .selected_session()
            .map(|session| session.workspace.clone())
            .unwrap_or_else(|| ".".to_string());
        self.pending_actions.push(TuiAction::ShowDiff {
            workspace: workspace.clone(),
        });
        self.status = format!("diff queued: {workspace}");
    }

    fn render_diff_help_detail(&self) -> String {
        let workspace = self
            .selected_session()
            .map(|session| session.workspace.as_str())
            .unwrap_or(".");
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Diff");
        let _ = writeln!(detail, "=================");
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "/diff shows changed tracked files and `git diff --stat` for the selected workspace."
        );
        let _ = writeln!(detail);
        push_status_row(&mut detail, "Workspace:", workspace);
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "This is read-only and does not include untracked files."
        );
        detail
    }

    fn handle_goal_command(&mut self, command: TuiGoalCommand) {
        match command {
            TuiGoalCommand::Show => {
                self.status = "goal shown".to_string();
            }
            TuiGoalCommand::Set {
                objective,
                token_budget,
            } => {
                self.goal_objective = Some(objective);
                self.goal_token_budget = token_budget;
                self.goal_started_at = Some(SystemTime::now());
                self.status = "goal set".to_string();
            }
            TuiGoalCommand::Clear => {
                self.goal_objective = None;
                self.goal_token_budget = None;
                self.goal_started_at = None;
                self.status = "goal cleared".to_string();
            }
        }
        self.set_mcp_detail(TuiMcpDetailKind::Goal, self.render_goal_detail());
    }

    fn render_goal_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Goal");
        let _ = writeln!(detail, "=================");
        let _ = writeln!(detail);
        match self.goal_objective.as_deref() {
            Some(objective) => {
                push_status_row(&mut detail, "Objective:", objective);
                let elapsed = self
                    .goal_started_at
                    .and_then(|started| started.elapsed().ok())
                    .map(format_tui_goal_elapsed)
                    .unwrap_or_else(|| "unknown".to_string());
                push_status_row(&mut detail, "Elapsed:", &elapsed);
                match self.goal_token_budget {
                    Some(budget) => {
                        push_status_row(&mut detail, "Token budget:", &budget.to_string());
                        match self.active_usage_summary() {
                            Some(summary) => {
                                let percent = if budget > 0 {
                                    (summary.total_tokens as f64 / budget as f64 * 100.0).min(100.0)
                                } else {
                                    0.0
                                };
                                push_status_row(
                                    &mut detail,
                                    "Used tokens:",
                                    &format!("{} ({percent:.0}%)", summary.total_tokens),
                                );
                            }
                            None => push_status_row(
                                &mut detail,
                                "Used tokens:",
                                "no active-thread usage telemetry",
                            ),
                        }
                    }
                    None => push_status_row(&mut detail, "Token budget:", "not set"),
                }
                let _ = writeln!(detail);
                let _ = writeln!(detail, "Commands");
                let _ = writeln!(detail, "--------");
                let _ = writeln!(detail, "- /goal              Show this goal");
                let _ = writeln!(detail, "- /goal clear        Clear the goal");
                let _ = writeln!(
                    detail,
                    "- /goal <objective>  Replace it; append `budget: N` for tokens"
                );
            }
            None => {
                let _ = writeln!(
                    detail,
                    "No goal set. Use /goal <objective> [budget: N] to set one."
                );
                let _ = writeln!(detail);
                let _ = writeln!(detail, "Examples");
                let _ = writeln!(detail, "--------");
                let _ = writeln!(detail, "- /goal Stabilize TUI parity budget: 50000");
                let _ = writeln!(detail, "- /goal clear");
            }
        }
        detail
    }

    fn show_feedback_detail(&mut self, command: TuiFeedbackCommand) {
        let detail = render_feedback_detail(command);
        self.set_mcp_detail(TuiMcpDetailKind::Feedback, detail);
        self.status = match command {
            TuiFeedbackCommand::Show => "feedback options shown".to_string(),
            TuiFeedbackCommand::Bug => "feedback bug link shown".to_string(),
            TuiFeedbackCommand::Feature => "feedback feature link shown".to_string(),
            TuiFeedbackCommand::Security => "feedback security link shown".to_string(),
        };
    }

    fn show_links_detail(&mut self) {
        let detail = render_links_detail();
        self.set_mcp_detail(TuiMcpDetailKind::Links, detail);
        self.status = "links shown".to_string();
    }

    fn show_home_detail(&mut self) {
        let detail = self.render_home_detail();
        self.set_mcp_detail(TuiMcpDetailKind::Home, detail);
        self.status = "home dashboard shown".to_string();
    }

    fn handle_mode_command(&mut self, command: TuiModeCommand) {
        match command {
            TuiModeCommand::Show => self.show_mode_detail(),
            TuiModeCommand::Set(mode) => {
                self.mode = mode;
                self.status = format!("mode set: {}", self.mode.title());
            }
        }
    }

    fn show_mode_detail(&mut self) {
        let detail = self.render_mode_detail();
        self.set_mcp_detail(TuiMcpDetailKind::Mode, detail);
        self.status = "mode options shown".to_string();
    }

    fn show_help_detail(&mut self, command: TuiHelpCommand) {
        let topic_status = match &command {
            TuiHelpCommand::Show => "help shown".to_string(),
            TuiHelpCommand::Topic(topic) => format!("help shown: {topic}"),
        };
        let detail = render_help_detail(&command);
        self.set_mcp_detail(TuiMcpDetailKind::Help, detail);
        self.status = topic_status;
    }

    fn show_settings_detail(&mut self) {
        let detail = self.render_settings_detail();
        self.set_mcp_detail(TuiMcpDetailKind::Settings, detail);
        self.status = "settings shown".to_string();
    }

    fn render_settings_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Settings");
        let _ = writeln!(detail, "=====================");
        let _ = writeln!(detail);
        push_status_row(&mut detail, "Mode:", self.mode.title());
        push_status_row(&mut detail, "Theme:", self.theme.title());
        push_status_row(
            &mut detail,
            "Verbose transcript:",
            if self.verbose_transcript { "on" } else { "off" },
        );
        match self.selected_session() {
            Some(session) => {
                push_status_row(&mut detail, "Workspace:", &session.workspace);
                let _ = writeln!(
                    detail,
                    "  Project config:   {}/.dscode/config.toml",
                    session.workspace
                );
            }
            None => {
                push_status_row(&mut detail, "Workspace:", ".");
                let _ = writeln!(detail, "  Project config:   ./.dscode/config.toml");
            }
        }
        let _ = writeln!(detail, "  User config:      ~/.dscode/config.toml");
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Config Commands");
        let _ = writeln!(detail, "---------------");
        let _ = writeln!(detail, "- /mode [agent|plan|yolo|1|2|3]");
        let _ = writeln!(detail, "- /diff");
        let _ = writeln!(detail, "- /clear");
        let _ = writeln!(detail, "- /theme [dark|light|grayscale|system]");
        let _ = writeln!(detail, "- /verbose [on|off|toggle|show]");
        let _ = writeln!(detail, "- /change");
        let _ = writeln!(detail, "- /system");
        let _ = writeln!(detail, "- /edit");
        let _ = writeln!(detail, "- /goal [objective [budget: N]|clear]");
        let _ = writeln!(detail, "- /model [name|list]");
        let _ = writeln!(detail, "- /provider [name [model]|list]");
        let _ = writeln!(detail, "- /network [list|allow|deny|remove|default]");
        let _ = writeln!(detail, "- /lsp [on|off|status]");
        let _ = writeln!(detail, "- /memory [show|path|clear|edit|help]");
        let _ = writeln!(detail, "- /anchor [add|list|remove|path]");
        let _ = writeln!(detail, "- /queue [list|edit <n>|drop <n>|clear]");
        let _ = writeln!(detail, "- /share");
        let _ = writeln!(detail, "- /hooks [list|events]");
        let _ = writeln!(
            detail,
            "- /mcp manager | /mcp init | /mcp add | /mcp enable | /mcp disable"
        );
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Workbench State");
        let _ = writeln!(detail, "---------------");
        push_status_row(
            &mut detail,
            "Sessions:",
            &format!("{} loaded", self.sessions.len()),
        );
        push_status_row(
            &mut detail,
            "Threads:",
            &format!("{} loaded", self.threads.len()),
        );
        push_status_row(
            &mut detail,
            "Command hist:",
            &format!("{} entry(s)", self.command_history.len()),
        );
        push_status_row(
            &mut detail,
            "Slash extras:",
            &format!("{} configured", self.extra_slash_completions.len()),
        );
        push_status_row(
            &mut detail,
            "Stash:",
            &format!("{} draft(s)", self.composer_stash.len()),
        );
        push_status_row(
            &mut detail,
            "Queue:",
            &format!("{} message(s)", self.queued_messages.len()),
        );
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "Settings are edited through focused commands so local file writes stay explicit."
        );
        detail
    }

    fn handle_theme_command(&mut self, command: TuiThemeCommand) {
        match command {
            TuiThemeCommand::Show => self.show_theme_detail(),
            TuiThemeCommand::Cycle => {
                self.theme = self.theme.next();
                self.show_theme_detail();
                self.status = format!("theme switched: {}", self.theme.title());
            }
            TuiThemeCommand::Set(theme) => {
                self.theme = theme;
                self.show_theme_detail();
                self.status = format!("theme switched: {}", self.theme.title());
            }
        }
    }

    fn show_theme_detail(&mut self) {
        let detail = self.render_theme_detail();
        self.set_mcp_detail(TuiMcpDetailKind::Theme, detail);
        self.status = "theme options shown".to_string();
    }

    fn render_theme_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Theme");
        let _ = writeln!(detail, "==================");
        let _ = writeln!(detail);
        push_status_row(&mut detail, "Current:", self.theme.title());
        push_status_row(&mut detail, "Command:", self.theme.command_name());
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Commands");
        let _ = writeln!(detail, "--------");
        let _ = writeln!(detail, "- /theme             Cycle to the next theme");
        let _ = writeln!(detail, "- /theme show        Show this panel");
        let _ = writeln!(
            detail,
            "- /theme dark        Cyan accents on terminal defaults"
        );
        let _ = writeln!(
            detail,
            "- /theme light       Blue accents for light terminals"
        );
        let _ = writeln!(detail, "- /theme grayscale   Neutral monochrome accents");
        let _ = writeln!(
            detail,
            "- /theme system      Terminal-default color assumptions"
        );
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "The theme is local to the running TUI session; persistent color settings remain a separate config task."
        );
        detail
    }

    fn render_mode_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Mode");
        let _ = writeln!(detail, "=================");
        let _ = writeln!(detail);
        push_status_row(&mut detail, "Current:", self.mode.title());
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Commands");
        let _ = writeln!(detail, "--------");
        let _ = writeln!(
            detail,
            "- /mode agent  Switch to Agent mode (alias: /mode 1)"
        );
        let _ = writeln!(
            detail,
            "- /mode plan   Switch to Plan mode (alias: /mode 2)"
        );
        let _ = writeln!(
            detail,
            "- /mode yolo   Switch to YOLO mode (alias: /mode 3)"
        );
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Keyboard: Tab cycles modes; p/a/y choose directly.");
        detail
    }

    fn show_status_detail(&mut self) {
        let detail = self.render_status_detail();
        self.set_mcp_detail(TuiMcpDetailKind::Status, detail);
        self.status = "status detail refreshed".to_string();
    }

    fn show_statusline_detail(&mut self) {
        let detail = self.render_statusline_detail();
        self.set_mcp_detail(TuiMcpDetailKind::StatusLine, detail);
        self.status = "statusline shown".to_string();
    }

    fn show_context_detail(&mut self) {
        let detail = self.render_context_detail();
        self.set_mcp_detail(TuiMcpDetailKind::Context, detail);
        self.status = "context inspector shown".to_string();
    }

    fn render_context_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Context");
        let _ = writeln!(detail, "====================");
        let _ = writeln!(detail);
        push_status_row(&mut detail, "Mode:", self.mode.title());
        push_status_row(
            &mut detail,
            "Verbose transcript:",
            if self.verbose_transcript { "on" } else { "off" },
        );
        match self.active_thread() {
            Some(thread) => {
                push_status_row(
                    &mut detail,
                    "Thread:",
                    &format!("{} [{}]", thread.title, thread.status),
                );
                push_status_row(&mut detail, "Thread id:", &thread.id);
                push_status_row(
                    &mut detail,
                    "Latest turn:",
                    thread.latest_turn_id.as_deref().unwrap_or("none"),
                );
                push_status_row(&mut detail, "Event seq:", &thread.event_seq.to_string());
            }
            None => {
                push_status_row(&mut detail, "Thread:", "none selected");
            }
        }

        let items = self.active_thread_items();
        push_status_row(
            &mut detail,
            "Transcript:",
            &format!(
                "{} item(s), {} display line(s)",
                items.len(),
                self.transcript.len()
            ),
        );
        if !items.is_empty() {
            push_status_row(
                &mut detail,
                "Item states:",
                &summarize_status_counts(items.iter().map(|item| item.status.as_str())),
            );
            push_status_row(
                &mut detail,
                "Item types:",
                &summarize_status_counts(items.iter().map(|item| item.item_type.as_str())),
            );
        }
        push_status_row(
            &mut detail,
            "Reasoning replay:",
            &format!(
                "latest {} item(s), {} pinned turn(s)",
                self.reasoning_replay_limit,
                self.reasoning_replay_pinned_turn_ids.len()
            ),
        );
        let _ = writeln!(detail);

        match self.active_usage_summary() {
            Some(summary) => {
                push_status_row(
                    &mut detail,
                    "Usage records:",
                    &summary.record_count.to_string(),
                );
                push_status_row(
                    &mut detail,
                    "Context window:",
                    &format_context_usage(summary),
                );
                push_status_row(
                    &mut detail,
                    "Remaining:",
                    &summary.context_remaining_tokens.to_string(),
                );
                push_status_row(&mut detail, "Strategy:", &summary.context_strategy);
                push_status_row(
                    &mut detail,
                    "Latest tokens:",
                    &format!(
                        "{} prompt / {} output / {} total",
                        summary.latest_prompt_tokens,
                        summary.latest_completion_tokens,
                        summary.latest_total_tokens
                    ),
                );
                push_status_row(
                    &mut detail,
                    "Total tokens:",
                    &format!(
                        "{} prompt / {} output / {} total",
                        summary.prompt_tokens, summary.completion_tokens, summary.total_tokens
                    ),
                );
                push_status_row(
                    &mut detail,
                    "Cache hit/miss:",
                    &format!(
                        "{} / {} ({})",
                        summary.prompt_cache_hit_tokens,
                        summary.prompt_cache_miss_tokens,
                        format_cache_hit_rate(
                            summary.prompt_cache_hit_tokens,
                            summary.prompt_cache_miss_tokens
                        )
                    ),
                );
            }
            None => {
                push_status_row(&mut detail, "Usage:", "no active-thread usage records");
                push_status_row(
                    &mut detail,
                    "Context window:",
                    &format!(
                        "{} token nominal DeepSeek context",
                        TUI_CONTEXT_WINDOW_TOKENS
                    ),
                );
            }
        }

        let _ = writeln!(detail);
        let _ = writeln!(detail, "Related Commands");
        let _ = writeln!(detail, "----------------");
        let _ = writeln!(detail, "- /tokens      Token and cache telemetry");
        let _ = writeln!(detail, "- /cache       Prompt-cache history and inspection");
        let _ = writeln!(detail, "- /compact     Compact active thread context");
        let _ = writeln!(detail, "- /reasoning   Inspect persisted reasoning items");
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Aliases: context, ctx");
        detail
    }

    fn handle_verbose_command(&mut self, command: TuiVerboseCommand) {
        match command {
            TuiVerboseCommand::Show => {
                self.show_verbose_detail();
            }
            TuiVerboseCommand::Toggle => {
                self.verbose_transcript = !self.verbose_transcript;
                self.refresh_runtime_view();
                self.show_verbose_detail();
                self.status = format!(
                    "verbose transcript {}",
                    if self.verbose_transcript { "on" } else { "off" }
                );
            }
            TuiVerboseCommand::Set(enabled) => {
                self.verbose_transcript = enabled;
                self.refresh_runtime_view();
                self.show_verbose_detail();
                self.status = format!(
                    "verbose transcript {}",
                    if self.verbose_transcript { "on" } else { "off" }
                );
            }
        }
    }

    fn show_verbose_detail(&mut self) {
        let detail = self.render_verbose_detail();
        self.set_mcp_detail(TuiMcpDetailKind::Verbose, detail);
        self.status = "verbose transcript shown".to_string();
    }

    fn render_verbose_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Verbose Transcript");
        let _ = writeln!(detail, "===============================");
        let _ = writeln!(detail);
        push_status_row(
            &mut detail,
            "Current:",
            if self.verbose_transcript { "on" } else { "off" },
        );
        let reasoning_count = self.active_reasoning_items().len();
        push_status_row(
            &mut detail,
            "Reasoning items:",
            &format!("{reasoning_count} active-thread item(s)"),
        );
        push_status_row(
            &mut detail,
            "Transcript:",
            if self.verbose_transcript {
                "full reasoning text"
            } else {
                "compact reasoning preview"
            },
        );
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Commands");
        let _ = writeln!(detail, "--------");
        let _ = writeln!(detail, "- /verbose          Toggle full reasoning text");
        let _ = writeln!(detail, "- /verbose on       Render live thinking in full");
        let _ = writeln!(detail, "- /verbose off      Keep live thinking compact");
        let _ = writeln!(detail, "- /verbose show     Show this panel");
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "Detailed persisted reasoning remains available through reasoning show/search even when transcript verbosity is off."
        );
        detail
    }

    fn render_statusline_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Statusline");
        let _ = writeln!(detail, "=======================");
        let _ = writeln!(detail);
        push_status_row(&mut detail, "Current status:", &self.status);
        push_status_row(&mut detail, "Mode:", self.mode.title());
        push_status_row(&mut detail, "Theme:", self.theme.title());
        push_status_row(
            &mut detail,
            "Verbose transcript:",
            if self.verbose_transcript { "on" } else { "off" },
        );
        push_status_row(
            &mut detail,
            "Detail panel:",
            self.mcp_detail
                .as_ref()
                .map(|(kind, _)| kind.title())
                .unwrap_or("none"),
        );
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Command Bar Items");
        let _ = writeln!(detail, "-----------------");
        let _ = writeln!(detail, "- Status text: current TUI action/result message");
        let _ = writeln!(detail, "- Palette: `:` opens command palette");
        let _ = writeln!(detail, "- Sessions: `s` opens session picker");
        let _ = writeln!(detail, "- Threads: `t` opens thread navigator");
        let _ = writeln!(detail, "- Approval: `!` opens pending approval modal");
        let _ = writeln!(
            detail,
            "- Cancel: `c` requests active turn/task cancellation"
        );
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Related Commands");
        let _ = writeln!(detail, "----------------");
        let _ = writeln!(
            detail,
            "- /status      Full runtime/session/task/usage detail"
        );
        let _ = writeln!(detail, "- /settings    Configuration entry points");
        let _ = writeln!(detail, "- /theme       Statusline color accents");
        let _ = writeln!(detail, "- /verbose     Transcript reasoning detail");
        let _ = writeln!(detail);
        let _ = writeln!(
            detail,
            "DeepSeekCode currently exposes the statusline as a fixed command bar; interactive item persistence remains a separate config task."
        );
        detail
    }

    fn render_home_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Home");
        let _ = writeln!(detail, "=================");
        let _ = writeln!(detail);
        push_status_row(&mut detail, "Version:", env!("CARGO_PKG_VERSION"));
        push_status_row(&mut detail, "Mode:", self.mode.title());
        match self.selected_session() {
            Some(session) => {
                push_status_row(&mut detail, "Workspace:", &session.workspace);
                push_status_row(
                    &mut detail,
                    "Session:",
                    &format!("{} [{}]", session.title, session.status),
                );
                push_status_row(
                    &mut detail,
                    "Session threads:",
                    &session.thread_count.to_string(),
                );
            }
            None => {
                push_status_row(&mut detail, "Workspace:", ".");
                push_status_row(&mut detail, "Session:", "none selected");
            }
        }
        match self.active_thread() {
            Some(thread) => {
                push_status_row(
                    &mut detail,
                    "Thread:",
                    &format!("{} [{}]", thread.title, thread.status),
                );
                push_status_row(&mut detail, "Thread mode:", &thread.mode);
            }
            None => {
                push_status_row(&mut detail, "Thread:", "none selected");
            }
        }
        let items = self.active_thread_items();
        let tasks = self.active_thread_tasks();
        let automations = self.active_thread_automations();
        push_status_row(
            &mut detail,
            "Transcript:",
            &format!("{} item(s)", items.len()),
        );
        push_status_row(&mut detail, "Tasks:", &format!("{} active", tasks.len()));
        push_status_row(
            &mut detail,
            "Automations:",
            &format!("{} active", automations.len()),
        );
        let active_thread_id = self.selected_thread_id.as_deref();
        let pending_approvals = self
            .approvals
            .iter()
            .filter(|approval| {
                approval.is_pending() && Some(approval.thread_id.as_str()) == active_thread_id
            })
            .count();
        let pending_user_inputs = self
            .user_inputs
            .iter()
            .filter(|request| {
                request.is_pending() && Some(request.thread_id.as_str()) == active_thread_id
            })
            .count();
        push_status_row(
            &mut detail,
            "Pending:",
            &format!(
                "{pending_approvals} approval(s), {pending_user_inputs} input(s), {} queued message(s)",
                self.queued_messages.len()
            ),
        );
        if let Some(summary) = self.active_usage_summary() {
            push_status_row(
                &mut detail,
                "Tokens:",
                &format!(
                    "{} total, {} latest",
                    summary.total_tokens, summary.latest_total_tokens
                ),
            );
            push_status_row(
                &mut detail,
                "Context:",
                &format!(
                    "{} remaining / {}",
                    summary.context_remaining_tokens, summary.context_strategy
                ),
            );
            let cost = summary
                .estimated_total_cost_microusd
                .map(format_microusd)
                .unwrap_or_else(|| "unpriced model".to_string());
            push_status_row(&mut detail, "Approx cost:", &cost);
        } else {
            push_status_row(&mut detail, "Usage:", "no active-thread usage records");
        }
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Quick actions");
        let _ = writeln!(detail, "-------------");
        let _ = writeln!(detail, "- /links       Repository and DeepSeek API links");
        let _ = writeln!(detail, "- /skills      Configured skill registry");
        let _ = writeln!(detail, "- /provider    Provider preset and model config");
        let _ = writeln!(detail, "- /status      Full runtime status");
        let _ = writeln!(detail, "- /tokens      Token and context telemetry");
        let _ = writeln!(detail, "- /mcp manager MCP server inventory");
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Aliases: home, stats, overview");
        detail
    }

    fn render_status_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode TUI Status");
        let _ = writeln!(detail, "=======================");
        let _ = writeln!(detail);
        push_status_row(&mut detail, "Version:", env!("CARGO_PKG_VERSION"));
        push_status_row(&mut detail, "Mode:", self.mode.title());
        push_status_row(&mut detail, "Status:", &self.status);
        push_status_row(
            &mut detail,
            "Sessions:",
            &format!(
                "{} loaded, selected {}",
                self.sessions.len(),
                self.selected_session
            ),
        );
        push_status_row(
            &mut detail,
            "Threads:",
            &format!("{} loaded", self.threads.len()),
        );
        let _ = writeln!(detail);

        match self.selected_session() {
            Some(session) => {
                push_status_row(
                    &mut detail,
                    "Session:",
                    &format!("{} [{}]", session.title, session.status),
                );
                push_status_row(&mut detail, "Session id:", &session.id);
                push_status_row(&mut detail, "Workspace:", &session.workspace);
                push_status_row(
                    &mut detail,
                    "Session threads:",
                    &session.thread_count.to_string(),
                );
            }
            None => {
                push_status_row(&mut detail, "Session:", "none selected");
            }
        }
        let _ = writeln!(detail);

        let items = self.active_thread_items();
        let tasks = self.active_thread_tasks();
        let automations = self.active_thread_automations();
        match self.active_thread() {
            Some(thread) => {
                push_status_row(
                    &mut detail,
                    "Thread:",
                    &format!("{} [{}]", thread.title, thread.status),
                );
                push_status_row(&mut detail, "Thread id:", &thread.id);
                push_status_row(&mut detail, "Thread mode:", &thread.mode);
                push_status_row(
                    &mut detail,
                    "Latest turn:",
                    thread.latest_turn_id.as_deref().unwrap_or("none"),
                );
                push_status_row(&mut detail, "Event seq:", &thread.event_seq.to_string());
            }
            None => {
                push_status_row(&mut detail, "Thread:", "none selected");
            }
        }
        push_status_row(
            &mut detail,
            "Transcript:",
            &format!(
                "{} item(s), {} display line(s)",
                items.len(),
                self.transcript.len()
            ),
        );
        if !items.is_empty() {
            push_status_row(
                &mut detail,
                "Item states:",
                &summarize_status_counts(items.iter().map(|item| item.status.as_str())),
            );
        }
        push_status_row(
            &mut detail,
            "Tasks:",
            &format!(
                "{} active, {} selected",
                tasks.len(),
                self.selected_task_ids.len()
            ),
        );
        if !tasks.is_empty() {
            let task_states = task_status_counts_line(&tasks)
                .strip_prefix("Task states: ")
                .unwrap_or("")
                .to_string();
            push_status_row(&mut detail, "Task states:", &task_states);
        }
        let automation_states = summarize_status_counts(
            automations
                .iter()
                .map(|automation| automation.status.as_str()),
        );
        push_status_row(
            &mut detail,
            "Automations:",
            &format!("{} active ({automation_states})", automations.len()),
        );
        if let Some(running) = self.active_running_assistant_item() {
            push_status_row(
                &mut detail,
                "Running:",
                &format!("assistant item {} chars", running.content.chars().count()),
            );
        }
        let _ = writeln!(detail);

        let active_thread_id = self.selected_thread_id.as_deref();
        let active_approvals = self
            .approvals
            .iter()
            .filter(|approval| Some(approval.thread_id.as_str()) == active_thread_id)
            .collect::<Vec<_>>();
        let active_user_inputs = self
            .user_inputs
            .iter()
            .filter(|request| Some(request.thread_id.as_str()) == active_thread_id)
            .collect::<Vec<_>>();
        push_status_row(
            &mut detail,
            "Approvals:",
            &format!(
                "{} active, {} pending total",
                active_approvals.len(),
                self.approvals
                    .iter()
                    .filter(|approval| approval.is_pending())
                    .count()
            ),
        );
        push_status_row(
            &mut detail,
            "User inputs:",
            &format!(
                "{} active, {} pending total",
                active_user_inputs.len(),
                self.user_inputs
                    .iter()
                    .filter(|request| request.is_pending())
                    .count()
            ),
        );
        push_status_row(
            &mut detail,
            "Queued messages:",
            &format!(
                "{} queued, {} editing",
                self.queued_messages.len(),
                if self.queued_draft.is_some() { 1 } else { 0 }
            ),
        );
        let _ = writeln!(detail);

        match self.active_usage_summary() {
            Some(summary) => {
                push_status_row(
                    &mut detail,
                    "Usage records:",
                    &summary.record_count.to_string(),
                );
                push_status_row(
                    &mut detail,
                    "Total tokens:",
                    &summary.total_tokens.to_string(),
                );
                push_status_row(
                    &mut detail,
                    "Latest tokens:",
                    &summary.latest_total_tokens.to_string(),
                );
                push_status_row(
                    &mut detail,
                    "Prompt tokens:",
                    &format!(
                        "{} total, {} latest",
                        summary.prompt_tokens, summary.latest_prompt_tokens
                    ),
                );
                push_status_row(
                    &mut detail,
                    "Output tokens:",
                    &format!(
                        "{} total, {} latest",
                        summary.completion_tokens, summary.latest_completion_tokens
                    ),
                );
                push_status_row(
                    &mut detail,
                    "Cache hit/miss:",
                    &format!(
                        "{} / {} ({})",
                        summary.prompt_cache_hit_tokens,
                        summary.prompt_cache_miss_tokens,
                        format_cache_hit_rate(
                            summary.prompt_cache_hit_tokens,
                            summary.prompt_cache_miss_tokens
                        )
                    ),
                );
                push_status_row(
                    &mut detail,
                    "Context:",
                    &format!(
                        "{} remaining / {}",
                        summary.context_remaining_tokens, summary.context_strategy
                    ),
                );
                let cost = summary
                    .estimated_total_cost_microusd
                    .map(format_microusd)
                    .unwrap_or_else(|| "unpriced model".to_string());
                push_status_row(&mut detail, "Est. cost:", &cost);
                if let (Some(input), Some(output)) = (
                    summary.estimated_input_cost_microusd,
                    summary.estimated_output_cost_microusd,
                ) {
                    push_status_row(
                        &mut detail,
                        "Cost split:",
                        &format!(
                            "in {} / out {}",
                            format_microusd(input),
                            format_microusd(output)
                        ),
                    );
                }
            }
            None => {
                push_status_row(&mut detail, "Usage:", "no active-thread usage records");
            }
        }

        detail
    }

    fn show_tokens_detail(&mut self) {
        let detail = self.render_tokens_detail();
        self.set_mcp_detail(TuiMcpDetailKind::Tokens, detail);
        self.status = "tokens detail refreshed".to_string();
    }

    fn render_tokens_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Token Usage");
        let _ = writeln!(detail, "========================");
        let _ = writeln!(detail);
        push_status_row(&mut detail, "Mode:", self.mode.title());
        push_status_row(
            &mut detail,
            "Thread:",
            self.active_thread()
                .map(|thread| thread.title.as_str())
                .unwrap_or("none selected"),
        );
        match self.active_usage_summary() {
            Some(summary) => {
                push_status_row(
                    &mut detail,
                    "Active context:",
                    &format_context_usage(summary),
                );
                push_status_row(
                    &mut detail,
                    "Last API input:",
                    &summary.latest_prompt_tokens.to_string(),
                );
                push_status_row(
                    &mut detail,
                    "Last API output:",
                    &summary.latest_completion_tokens.to_string(),
                );
                push_status_row(
                    &mut detail,
                    "Cache hit/miss:",
                    &format!(
                        "{} hit / {} miss ({})",
                        summary.prompt_cache_hit_tokens,
                        summary.prompt_cache_miss_tokens,
                        format_cache_hit_rate(
                            summary.prompt_cache_hit_tokens,
                            summary.prompt_cache_miss_tokens
                        )
                    ),
                );
                push_status_row(
                    &mut detail,
                    "Prompt tokens:",
                    &format!(
                        "{} cumulative across {} record(s)",
                        summary.prompt_tokens, summary.record_count
                    ),
                );
                push_status_row(
                    &mut detail,
                    "Output tokens:",
                    &summary.completion_tokens.to_string(),
                );
                push_status_row(
                    &mut detail,
                    "Total tokens:",
                    &summary.total_tokens.to_string(),
                );
                let cost = summary
                    .estimated_total_cost_microusd
                    .map(format_microusd)
                    .unwrap_or_else(|| "unpriced model".to_string());
                push_status_row(&mut detail, "Approx cost:", &cost);
                push_status_row(
                    &mut detail,
                    "Runtime items:",
                    &self.active_thread_items().len().to_string(),
                );
                push_status_row(
                    &mut detail,
                    "Transcript:",
                    &format!("{} display line(s)", self.transcript.len()),
                );
            }
            None => {
                push_status_row(&mut detail, "Usage:", "no active-thread usage records");
            }
        }
        detail
    }

    fn show_cost_detail(&mut self) {
        let detail = self.render_cost_detail();
        self.set_mcp_detail(TuiMcpDetailKind::Cost, detail);
        self.status = "cost detail refreshed".to_string();
    }

    fn render_cost_detail(&self) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "DeepSeekCode Session Cost");
        let _ = writeln!(detail, "=========================");
        let _ = writeln!(detail);
        match self.active_usage_summary() {
            Some(summary) => {
                let total = summary
                    .estimated_total_cost_microusd
                    .map(format_microusd)
                    .unwrap_or_else(|| "unpriced model".to_string());
                push_status_row(&mut detail, "Approx total:", &total);
                push_status_row(
                    &mut detail,
                    "Input cost:",
                    &format_optional_microusd(summary.estimated_input_cost_microusd),
                );
                push_status_row(
                    &mut detail,
                    "Output cost:",
                    &format_optional_microusd(summary.estimated_output_cost_microusd),
                );
                push_status_row(
                    &mut detail,
                    "Usage records:",
                    &summary.record_count.to_string(),
                );
                push_status_row(
                    &mut detail,
                    "Total tokens:",
                    &summary.total_tokens.to_string(),
                );
                push_status_row(
                    &mut detail,
                    "Cache hit/miss:",
                    &format!(
                        "{} / {} ({})",
                        summary.prompt_cache_hit_tokens,
                        summary.prompt_cache_miss_tokens,
                        format_cache_hit_rate(
                            summary.prompt_cache_hit_tokens,
                            summary.prompt_cache_miss_tokens
                        )
                    ),
                );
                let _ = writeln!(detail);
                let _ = writeln!(
                    detail,
                    "Cost estimates are approximate and use provider usage telemetry when available."
                );
                let _ = writeln!(
                    detail,
                    "Unrecognized models or missing usage prices are reported as unpriced."
                );
            }
            None => {
                push_status_row(&mut detail, "Usage:", "no active-thread usage records");
                let _ = writeln!(detail);
                let _ = writeln!(
                    detail,
                    "Run a model turn in this thread to collect cost telemetry."
                );
            }
        }
        detail
    }

    fn show_cache_detail(&mut self, command: TuiCacheCommand) {
        let detail = self.render_cache_detail(&command);
        self.set_mcp_detail(TuiMcpDetailKind::Cache, detail);
        self.status = "cache detail refreshed".to_string();
    }

    fn render_cache_detail(&self, command: &TuiCacheCommand) -> String {
        let title = match command {
            TuiCacheCommand::Summary { .. } => "DeepSeekCode Cache Telemetry",
            TuiCacheCommand::Inspect => "DeepSeekCode Cache Inspect",
            TuiCacheCommand::Warmup => "DeepSeekCode Cache Warmup",
        };
        let mut detail = String::new();
        let _ = writeln!(detail, "{title}");
        let _ = writeln!(detail, "{}", "=".repeat(title.len()));
        let _ = writeln!(detail);

        match command {
            TuiCacheCommand::Inspect => {
                let _ = writeln!(
                    detail,
                    "Durable TUI usage records do not persist rendered prompt layer hashes or prompt text."
                );
                let _ = writeln!(
                    detail,
                    "This read-only view surfaces the cache telemetry available for the active thread."
                );
                let _ = writeln!(detail);
            }
            TuiCacheCommand::Warmup => {
                let _ = writeln!(
                    detail,
                    "DeepSeekCode TUI does not send a cache warmup request from this read-only command."
                );
                let _ = writeln!(
                    detail,
                    "Run a normal model turn to collect provider-reported prompt cache usage."
                );
                let _ = writeln!(detail);
            }
            TuiCacheCommand::Summary { .. } => {}
        }

        match self.active_usage_summary() {
            Some(summary) => {
                push_status_row(
                    &mut detail,
                    "Usage records:",
                    &summary.record_count.to_string(),
                );
                if let TuiCacheCommand::Summary { count: Some(count) } = command {
                    push_status_row(&mut detail, "Requested turns:", &count.to_string());
                }
                push_status_row(
                    &mut detail,
                    "Prompt tokens:",
                    &summary.prompt_tokens.to_string(),
                );
                push_status_row(
                    &mut detail,
                    "Latest prompt:",
                    &summary.latest_prompt_tokens.to_string(),
                );
                push_status_row(
                    &mut detail,
                    "Cache hit:",
                    &summary.prompt_cache_hit_tokens.to_string(),
                );
                push_status_row(
                    &mut detail,
                    "Cache miss:",
                    &summary.prompt_cache_miss_tokens.to_string(),
                );
                push_status_row(
                    &mut detail,
                    "Cache accounted:",
                    &summary
                        .prompt_cache_hit_tokens
                        .saturating_add(summary.prompt_cache_miss_tokens)
                        .to_string(),
                );
                push_status_row(
                    &mut detail,
                    "Hit rate:",
                    &format_cache_hit_rate(
                        summary.prompt_cache_hit_tokens,
                        summary.prompt_cache_miss_tokens,
                    ),
                );
                push_status_row(
                    &mut detail,
                    "Cache chart:",
                    &format_ratio_bar(
                        summary.prompt_cache_hit_tokens,
                        summary.prompt_cache_miss_tokens,
                        18,
                        '#',
                        '.',
                    ),
                );
                push_status_row(&mut detail, "Context:", &format_context_usage(summary));
                let cost = summary
                    .estimated_total_cost_microusd
                    .map(format_microusd)
                    .unwrap_or_else(|| "unpriced model".to_string());
                push_status_row(&mut detail, "Approx cost:", &cost);
            }
            None => {
                push_status_row(&mut detail, "Usage:", "no active-thread usage records");
            }
        }
        detail
    }

    fn handle_composer_stash_command(&mut self, command: TuiComposerStashCommand) {
        self.reload_composer_stash();
        match command {
            TuiComposerStashCommand::List => self.show_composer_stash(),
            TuiComposerStashCommand::Pop => self.pop_composer_stash(),
            TuiComposerStashCommand::Clear => self.clear_composer_stash(),
        }
    }

    fn stash_composer_draft(&mut self) {
        if self.composer.is_empty() {
            self.status = "composer stash skipped: draft is empty".to_string();
            return;
        }
        self.reload_composer_stash();
        self.composer_stash.push(ComposerStashEntry {
            created_at: composer_stash_timestamp(),
            text: self.composer.clone(),
        });
        if self.composer_stash.len() > MAX_TUI_COMPOSER_STASH_ENTRIES {
            let overflow = self.composer_stash.len() - MAX_TUI_COMPOSER_STASH_ENTRIES;
            self.composer_stash.drain(0..overflow);
        }
        self.composer.clear();
        self.composer_cursor = 0;
        self.status = "draft stashed; use stash pop to restore".to_string();
        self.persist_composer_stash();
    }

    fn show_composer_stash(&mut self) {
        if self.composer_stash.is_empty() {
            self.set_mcp_detail(
                TuiMcpDetailKind::ComposerStash,
                "Composer stash empty.\n\nPress Ctrl+S in the composer to park the current draft.",
            );
            self.status = "composer stash empty".to_string();
            return;
        }
        let mut detail = format!("Composer stash: {} draft(s)\n\n", self.composer_stash.len());
        for (index, entry) in self.composer_stash.iter().enumerate() {
            detail.push_str(&format!(
                "{index}. [{}] {}\n",
                entry.created_at,
                composer_stash_preview(&entry.text, 80)
            ));
        }
        detail.push_str("\nUse stash pop to restore the most recent draft.");
        self.set_mcp_detail(TuiMcpDetailKind::ComposerStash, detail);
        self.status = format!(
            "composer stash listed: {} draft(s)",
            self.composer_stash.len()
        );
    }

    fn pop_composer_stash(&mut self) {
        let Some(entry) = self.composer_stash.pop() else {
            self.status = "composer stash empty; nothing to pop".to_string();
            return;
        };
        self.composer = entry.text;
        self.composer_cursor = self.composer.len();
        self.composer_focused = true;
        let remaining = self.composer_stash.len();
        self.status = match remaining {
            0 => "restored stashed draft; stash now empty".to_string(),
            1 => "restored stashed draft; 1 draft remains".to_string(),
            count => format!("restored stashed draft; {count} drafts remain"),
        };
        self.persist_composer_stash();
    }

    fn clear_composer_stash(&mut self) {
        let count = self.composer_stash.len();
        self.composer_stash.clear();
        self.status = match count {
            0 => "composer stash already empty".to_string(),
            1 => "cleared 1 stashed draft".to_string(),
            count => format!("cleared {count} stashed drafts"),
        };
        self.persist_composer_stash();
        self.set_mcp_detail(TuiMcpDetailKind::ComposerStash, "Composer stash empty.");
    }

    fn reload_composer_stash(&mut self) {
        let Some(path) = self.composer_stash_path.as_ref() else {
            return;
        };
        match read_composer_stash(path) {
            Ok(entries) => self.composer_stash = entries,
            Err(error) => {
                self.status = format!("failed to load composer stash: {error}");
            }
        }
    }

    fn persist_composer_stash(&mut self) {
        let Some(path) = self.composer_stash_path.as_ref() else {
            return;
        };
        if let Err(error) = write_composer_stash(path, &self.composer_stash) {
            self.status = format!("{}; failed to save composer stash: {error}", self.status);
        }
    }

    fn show_reasoning_list(&mut self) {
        let detail = match self.active_thread().cloned() {
            Some(thread) => {
                let items = self.active_reasoning_items();
                let count = items.len();
                let detail = render_reasoning_list_detail(
                    &thread,
                    &items,
                    self.reasoning_replay_limit,
                    &self.reasoning_replay_pinned_turn_ids,
                );
                self.status = format!(
                    "reasoning items={count} replay_limit={} pinned_turns={}",
                    self.reasoning_replay_limit,
                    self.reasoning_replay_pinned_turn_ids.len()
                );
                detail
            }
            None => {
                self.status = "no active durable thread for reasoning".to_string();
                "Reasoning\n\nNo active durable thread.".to_string()
            }
        };
        self.set_mcp_detail(TuiMcpDetailKind::Reasoning, detail);
    }

    fn select_reasoning_item<'a>(
        &self,
        items: &'a [&'a TuiItem],
        selector: &str,
    ) -> Option<(usize, &'a TuiItem)> {
        let selector = selector.trim();
        if selector.eq_ignore_ascii_case("latest")
            || selector.eq_ignore_ascii_case("last")
            || selector.is_empty()
        {
            items
                .last()
                .map(|item| (items.len().saturating_sub(1), *item))
        } else if let Ok(index) = selector.parse::<usize>() {
            index
                .checked_sub(1)
                .and_then(|idx| items.get(idx).map(|item| (idx, *item)))
        } else {
            items
                .iter()
                .position(|item| item.id == selector || item.turn_id.as_deref() == Some(selector))
                .map(|idx| (idx, items[idx]))
        }
    }

    fn show_reasoning_item(&mut self, selector: &str) {
        let Some(_thread) = self.active_thread() else {
            self.status = "no active durable thread for reasoning".to_string();
            self.set_mcp_detail(
                TuiMcpDetailKind::Reasoning,
                "Reasoning\n\nNo active durable thread.",
            );
            return;
        };
        let items = self.active_reasoning_items();
        if items.is_empty() {
            self.status = "no reasoning items in active thread".to_string();
            self.set_mcp_detail(
                TuiMcpDetailKind::Reasoning,
                render_reasoning_empty_detail(
                    self.reasoning_replay_limit,
                    &self.reasoning_replay_pinned_turn_ids,
                ),
            );
            return;
        }
        let selector = selector.trim();
        let selected = self.select_reasoning_item(&items, selector);
        let Some((index, item)) = selected else {
            self.status = format!("reasoning item not found: {selector}");
            self.set_mcp_detail(
                TuiMcpDetailKind::Reasoning,
                format!(
                    "Reasoning\n\nNo reasoning item matched `{selector}`.\n\n{}",
                    render_reasoning_selector_help()
                ),
            );
            return;
        };
        let total = items.len();
        let item_id = item.id.clone();
        let detail = render_reasoning_item_detail(
            item,
            index + 1,
            total,
            self.reasoning_replay_limit,
            &self.reasoning_replay_pinned_turn_ids,
        );
        self.set_mcp_detail(TuiMcpDetailKind::Reasoning, detail);
        self.status = format!(
            "showing reasoning item {}/{} ({})",
            index + 1,
            total,
            item_id
        );
    }

    fn show_reasoning_search(&mut self, query: &str) {
        let query = query.trim();
        if query.is_empty() {
            self.status = "reasoning search requires a query".to_string();
            self.show_reasoning_list();
            return;
        }
        let detail = match self.active_thread().cloned() {
            Some(thread) => {
                let items = self.active_reasoning_items();
                let matched = count_reasoning_search_matches(&items, query);
                let detail = render_reasoning_search_detail(
                    &thread,
                    &items,
                    query,
                    self.reasoning_replay_limit,
                    &self.reasoning_replay_pinned_turn_ids,
                );
                self.status = format!(
                    "reasoning search `{}` matched {}",
                    clip_line(query, 40),
                    matched
                );
                detail
            }
            None => {
                self.status = "no active durable thread for reasoning".to_string();
                "Reasoning search\n\nNo active durable thread.".to_string()
            }
        };
        self.set_mcp_detail(TuiMcpDetailKind::Reasoning, detail);
    }

    fn pin_reasoning_replay_turn(&mut self, selector: &str) {
        let selected = {
            let items = self.active_reasoning_items();
            if items.is_empty() {
                None
            } else {
                self.select_reasoning_item(&items, selector)
                    .map(|(_, item)| {
                        (
                            item.id.clone(),
                            item.turn_id.clone(),
                            item.content.chars().count(),
                        )
                    })
            }
        };
        let Some((item_id, turn_id, chars)) = selected else {
            self.status = format!("reasoning item not found: {selector}");
            return;
        };
        let Some(turn_id) = turn_id else {
            self.status = format!("reasoning item {item_id} has no turn_id to pin");
            return;
        };
        self.reasoning_replay_pinned_turn_ids
            .insert(turn_id.clone());
        self.show_reasoning_pins();
        self.status = format!("pinned reasoning turn {turn_id} ({item_id}, chars={chars})");
        self.persist_reasoning_replay_preferences();
    }

    fn unpin_reasoning_replay_turn(&mut self, selector: &str) {
        let turn_id = {
            let items = self.active_reasoning_items();
            self.select_reasoning_item(&items, selector)
                .and_then(|(_, item)| item.turn_id.clone())
                .or_else(|| Some(selector.trim().to_string()).filter(|value| !value.is_empty()))
        };
        let Some(turn_id) = turn_id else {
            self.status = "reasoning unpin requires a selector".to_string();
            return;
        };
        if self.reasoning_replay_pinned_turn_ids.remove(&turn_id) {
            self.show_reasoning_pins();
            self.status = format!("unpinned reasoning turn {turn_id}");
            self.persist_reasoning_replay_preferences();
        } else {
            self.show_reasoning_pins();
            self.status = format!("reasoning turn was not pinned: {turn_id}");
        }
    }

    fn clear_reasoning_replay_pins(&mut self) {
        let count = self.reasoning_replay_pinned_turn_ids.len();
        self.reasoning_replay_pinned_turn_ids.clear();
        self.show_reasoning_pins();
        self.status = format!("cleared {count} reasoning replay pin(s)");
        self.persist_reasoning_replay_preferences();
    }

    fn show_reasoning_pins(&mut self) {
        let detail = match self.active_thread().cloned() {
            Some(thread) => {
                let items = self.active_reasoning_items();
                render_reasoning_pins_detail(
                    &thread,
                    &items,
                    self.reasoning_replay_limit,
                    &self.reasoning_replay_pinned_turn_ids,
                )
            }
            None => "Reasoning replay pins\n\nNo active durable thread.".to_string(),
        };
        self.set_mcp_detail(TuiMcpDetailKind::Reasoning, detail);
    }

    fn set_reasoning_replay_limit_from_arg(&mut self, limit: &str) {
        match limit.parse::<usize>() {
            Ok(value) if value <= MAX_TUI_REASONING_REPLAY_LIMIT => {
                self.reasoning_replay_limit = value;
                self.show_reasoning_list();
                self.status = format!("reasoning replay limit set to {value}");
                self.persist_reasoning_replay_preferences();
            }
            Ok(_) => {
                self.status =
                    format!("reasoning replay limit must be <= {MAX_TUI_REASONING_REPLAY_LIMIT}");
            }
            Err(_) => {
                self.status = format!("invalid reasoning replay limit: {limit}");
            }
        }
    }

    fn request_create_task(&mut self, summary: String) {
        let summary = summary.trim().to_string();
        if summary.is_empty() {
            self.status = "task summary is empty".to_string();
            return;
        }
        let Some(thread_id) = self.selected_thread_id.clone() else {
            self.status = "no active durable thread for task".to_string();
            return;
        };
        self.pending_actions.push(TuiAction::CreateTask {
            thread_id: thread_id.clone(),
            summary: summary.clone(),
        });
        self.status = format!(
            "task queued for creation in {thread_id}: {}",
            clip_line(&summary, 60)
        );
    }

    fn request_default_task_pause(&mut self) {
        let selected = self.selected_task_ids_for_statuses(&["pending"]);
        if !selected.is_empty() {
            let count = selected.len();
            for task_id in selected {
                self.pending_actions.push(TuiAction::PauseTask { task_id });
            }
            self.status = format!("bulk task pause requested for {count} selected task(s)");
            return;
        }
        if !self.selected_task_ids.is_empty() {
            self.status = "no selected pending task in active thread to pause".to_string();
            return;
        }
        if let Some(task_id) = self.default_task_for_statuses(&["pending"]) {
            self.request_task_pause(&task_id);
        } else {
            self.status = "no pending task in active thread to pause".to_string();
        }
    }

    fn request_task_pause(&mut self, task_id: &str) {
        let Some(task) = self.active_task_by_id(task_id).cloned() else {
            self.status = format!("task not found in active thread: {task_id}");
            return;
        };
        match task.status.as_str() {
            "pending" => {
                self.pending_actions.push(TuiAction::PauseTask {
                    task_id: task.id.clone(),
                });
                self.status = format!("task pause requested: {}", task.id);
            }
            "paused" => {
                self.status = format!("task already paused: {}", task.id);
            }
            status => {
                self.status = format!("task {} cannot be paused from {status}", task.id);
            }
        }
    }

    fn request_default_task_resume(&mut self) {
        let selected = self.selected_task_ids_for_statuses(&["paused"]);
        if !selected.is_empty() {
            let count = selected.len();
            for task_id in selected {
                self.pending_actions.push(TuiAction::ResumeTask { task_id });
            }
            self.status = format!("bulk task resume requested for {count} selected task(s)");
            return;
        }
        if !self.selected_task_ids.is_empty() {
            self.status = "no selected paused task in active thread to resume".to_string();
            return;
        }
        if let Some(task_id) = self.default_task_for_statuses(&["paused"]) {
            self.request_task_resume(&task_id);
        } else {
            self.status = "no paused task in active thread to resume".to_string();
        }
    }

    fn request_task_resume(&mut self, task_id: &str) {
        let Some(task) = self.active_task_by_id(task_id).cloned() else {
            self.status = format!("task not found in active thread: {task_id}");
            return;
        };
        match task.status.as_str() {
            "paused" => {
                self.pending_actions.push(TuiAction::ResumeTask {
                    task_id: task.id.clone(),
                });
                self.status = format!("task resume requested: {}", task.id);
            }
            "pending" => {
                self.status = format!("task already pending: {}", task.id);
            }
            status => {
                self.status = format!("task {} cannot be resumed from {status}", task.id);
            }
        }
    }

    fn request_default_task_cancel(&mut self) {
        let selected =
            self.selected_task_ids_for_statuses(&["running", "pending", "paused", "cancelled"]);
        if !selected.is_empty() {
            let count = selected.len();
            for task_id in selected {
                self.pending_actions.push(TuiAction::CancelTask { task_id });
            }
            self.status = format!("bulk task cancel requested for {count} selected task(s)");
            return;
        }
        if !self.selected_task_ids.is_empty() {
            self.status = "no selected cancellable task in active thread".to_string();
            return;
        }
        if let Some(task_id) = self.default_task_for_statuses(&["running", "pending", "paused"]) {
            self.request_task_cancel(&task_id);
        } else {
            self.status = "no cancellable task in active thread".to_string();
        }
    }

    fn request_task_cancel(&mut self, task_id: &str) {
        let Some(task) = self.active_task_by_id(task_id).cloned() else {
            self.status = format!("task not found in active thread: {task_id}");
            return;
        };
        match task.status.as_str() {
            "pending" | "paused" | "running" | "cancelled" => {
                self.pending_actions.push(TuiAction::CancelTask {
                    task_id: task.id.clone(),
                });
                self.status = format!("task cancel requested: {}", task.id);
            }
            status => {
                self.status = format!("task {} cannot be cancelled from {status}", task.id);
            }
        }
    }

    fn request_rollback_snapshot(&mut self, label: Option<String>) {
        let label = label.and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
        self.pending_actions
            .push(TuiAction::CreateRollbackSnapshot {
                label: label.clone(),
            });
        self.status = match label {
            Some(label) => format!("rollback snapshot requested: {}", clip_line(&label, 60)),
            None => "rollback snapshot requested".to_string(),
        };
    }

    fn request_rollback_list_from_arg(&mut self, limit: &str) {
        match limit.parse::<usize>() {
            Ok(value) if value > 0 && value <= 100 => self.request_rollback_list(value),
            Ok(_) => self.status = "restore list limit must be between 1 and 100".to_string(),
            Err(_) => self.status = format!("invalid restore list limit: {limit}"),
        }
    }

    fn request_rollback_list(&mut self, limit: usize) {
        self.pending_actions
            .push(TuiAction::ListRollbackSnapshots { limit });
        self.status = format!("rollback snapshot list requested (limit={limit})");
    }

    fn request_rollback_show(&mut self, id: &str) {
        let Some(id) = self.resolve_rollback_id(id) else {
            return;
        };
        self.pending_actions
            .push(TuiAction::ShowRollbackSnapshot { id: id.clone() });
        self.status = format!("rollback snapshot show requested: {id}");
    }

    fn request_rollback_hunk_from_arg(&mut self, id: &str, hunk: &str) {
        match hunk.parse::<usize>() {
            Ok(value) if value > 0 => self.request_rollback_hunk(id, Some(value)),
            Ok(_) => self.status = "rollback hunk index must be >= 1".to_string(),
            Err(_) => self.status = format!("invalid rollback hunk index: {hunk}"),
        }
    }

    fn request_rollback_hunk(&mut self, id: &str, hunk: Option<usize>) {
        let Some(id) = self.resolve_rollback_id(id) else {
            return;
        };
        self.pending_actions.push(TuiAction::ShowRollbackHunk {
            id: id.clone(),
            hunk,
        });
        self.status = match hunk {
            Some(hunk) => format!("rollback hunk {hunk} requested: {id}"),
            None => format!("rollback hunks requested: {id}"),
        };
    }

    fn request_rollback_hunk_restore_from_arg(&mut self, id: &str, hunk: &str, apply: bool) {
        match hunk.parse::<usize>() {
            Ok(value) if value > 0 => self.request_rollback_hunk_restore(id, value, apply),
            Ok(_) => self.status = "rollback hunk index must be >= 1".to_string(),
            Err(_) => self.status = format!("invalid rollback hunk index: {hunk}"),
        }
    }

    fn request_rollback_hunk_restore(&mut self, id: &str, hunk: usize, apply: bool) {
        let Some(id) = self.resolve_rollback_id(id) else {
            return;
        };
        if apply {
            self.rollback_apply_confirmation = Some(TuiRollbackPendingApply {
                id: id.clone(),
                hunk: Some(hunk),
            });
            self.status = format!("confirm rollback hunk apply: {id} #{hunk}");
            return;
        }
        self.pending_actions.push(TuiAction::RestoreRollbackHunk {
            id: id.clone(),
            hunk,
            apply: false,
        });
        self.status = format!("rollback hunk check requested: {id} #{hunk}");
    }

    fn request_revert_turn(&mut self, id: &str, apply: bool) {
        let Some(id) = self.resolve_rollback_id(id) else {
            return;
        };
        if apply {
            self.rollback_apply_confirmation = Some(TuiRollbackPendingApply {
                id: id.clone(),
                hunk: None,
            });
            self.status = format!("confirm rollback apply: {id}");
            return;
        }
        self.pending_actions.push(TuiAction::RevertTurn {
            id: id.clone(),
            apply: false,
        });
        self.status = format!("rollback dry-run requested: {id}");
    }

    fn resolve_rollback_id(&mut self, id: &str) -> Option<String> {
        let id = id.trim();
        if id.is_empty() {
            self.status = "rollback id is empty".to_string();
            return None;
        }
        if id == "last" {
            if let Some(turn_id) = self
                .active_thread()
                .and_then(|thread| thread.latest_turn_id.clone())
            {
                return Some(turn_id);
            }
            self.status = "no latest turn id in active thread".to_string();
            return None;
        }
        Some(id.to_string())
    }

    fn request_diagnostics_from_args(&mut self, args: &[&str]) {
        let changed = args
            .iter()
            .any(|arg| matches!(*arg, "changed" | "--changed"));
        let paths = if changed {
            Vec::new()
        } else {
            args.iter().map(|arg| (*arg).to_string()).collect()
        };
        self.request_diagnostics(changed, paths);
    }

    fn request_diagnostics(&mut self, changed: bool, paths: Vec<String>) {
        let target = if changed {
            "changed files".to_string()
        } else if paths.is_empty() {
            "workspace".to_string()
        } else {
            format!("{} paths", paths.len())
        };
        self.pending_actions
            .push(TuiAction::RunDiagnostics { changed, paths });
        self.status = format!("diagnostics requested for {target}");
    }

    fn request_shell_run(&mut self, command: String) {
        let command = command.trim().to_string();
        if command.is_empty() {
            self.status = "shell command is empty".to_string();
            return;
        }
        if !is_safe_shell_command(&command) {
            self.pending_shell_approval = Some(command.clone());
            self.show_approval_modal = true;
            self.status = format!(
                "shell command requires approval: {}",
                clip_line(&command, 60)
            );
            return;
        }
        self.pending_actions.push(TuiAction::RunShell {
            command: command.clone(),
        });
        self.status = format!("shell job requested: {}", clip_line(&command, 60));
    }

    fn request_shell_list(&mut self) {
        self.pending_actions.push(TuiAction::ListShell);
        self.status = "shell job list requested".to_string();
    }

    fn request_shell_show(&mut self, task_id: &str) {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            self.status = "shell show requires a task id".to_string();
            return;
        }
        self.pending_actions.push(TuiAction::ShowShell {
            task_id: task_id.to_string(),
        });
        self.status = format!("shell show requested: {task_id}");
    }

    fn request_shell_attach_from_cursor(&mut self, task_id: &str, cursor: &str) {
        match cursor.parse::<usize>() {
            Ok(cursor) => self.request_shell_attach(task_id, Some(cursor), false),
            Err(_) => self.status = format!("invalid shell attach cursor: {cursor}"),
        }
    }

    fn request_shell_attach(&mut self, task_id: &str, cursor: Option<usize>, tail: bool) {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            self.status = "shell attach requires a task id".to_string();
            return;
        }
        self.pending_actions.push(TuiAction::AttachShell {
            task_id: task_id.to_string(),
            cursor,
            tail,
        });
        self.status = if tail {
            format!("shell attach tail requested: {task_id}")
        } else if let Some(cursor) = cursor {
            format!("shell attach requested: {task_id} @{cursor}")
        } else {
            format!("shell attach requested: {task_id}")
        };
    }

    fn request_shell_supervisor_status(&mut self) {
        self.pending_actions.push(TuiAction::ShellSupervisorStatus);
        self.status = "shell supervisor status requested".to_string();
    }

    fn request_shell_stdin(&mut self, task_id: &str, input: String, close: bool) {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            self.status = "shell stdin requires a task id".to_string();
            return;
        }
        if !close && input.is_empty() {
            self.status = "shell stdin requires input".to_string();
            return;
        }
        self.pending_actions.push(TuiAction::SendShellStdin {
            task_id: task_id.to_string(),
            input,
            close,
        });
        self.status = if close {
            format!("shell stdin close requested: {task_id}")
        } else {
            format!("shell stdin requested: {task_id}")
        };
    }

    fn request_shell_wait_from_arg(&mut self, task_id: &str, wait: bool, timeout_ms: &str) {
        match timeout_ms.parse::<u64>() {
            Ok(value) if value <= 10_000 => self.request_shell_wait(task_id, wait, value),
            Ok(_) => self.status = "shell wait timeout must be <= 10000ms".to_string(),
            Err(_) => self.status = format!("invalid shell wait timeout: {timeout_ms}"),
        }
    }

    fn request_shell_wait(&mut self, task_id: &str, wait: bool, timeout_ms: u64) {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            self.status = "shell task id is empty".to_string();
            return;
        }
        self.pending_actions.push(TuiAction::WaitShell {
            task_id: task_id.to_string(),
            wait,
            timeout_ms,
        });
        self.status = if wait {
            format!("shell wait requested: {task_id}")
        } else {
            format!("shell poll requested: {task_id}")
        };
    }

    fn request_shell_resize(&mut self, task_id: &str, rows: &str, cols: &str) {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            self.status = "shell resize requires a task id".to_string();
            return;
        }
        let Ok(rows) = rows.parse::<u16>() else {
            self.status = format!("invalid shell resize rows: {rows}");
            return;
        };
        let Ok(cols) = cols.parse::<u16>() else {
            self.status = format!("invalid shell resize cols: {cols}");
            return;
        };
        if rows == 0 || cols == 0 || rows > 2000 || cols > 2000 {
            self.status = "shell resize rows/cols must be between 1 and 2000".to_string();
            return;
        }
        self.pending_actions.push(TuiAction::ResizeShell {
            task_id: task_id.to_string(),
            rows,
            cols,
        });
        self.status = format!("shell resize requested: {task_id} {rows}x{cols}");
    }

    fn request_shell_cancel(&mut self, task_id: Option<String>, all: bool) {
        if !all && task_id.as_deref().unwrap_or("").trim().is_empty() {
            self.status = "shell cancel requires a task id or all".to_string();
            return;
        }
        let task_id = task_id.map(|id| id.trim().to_string());
        let target = if all {
            "all".to_string()
        } else {
            task_id.clone().unwrap_or_default()
        };
        self.pending_actions
            .push(TuiAction::CancelShell { task_id, all });
        self.status = format!("shell cancel requested: {target}");
    }

    fn request_mcp_list(&mut self) {
        self.pending_actions.push(TuiAction::McpList);
        self.status = "mcp inventory requested".to_string();
    }

    fn request_mcp_reload(&mut self) {
        self.pending_actions.push(TuiAction::McpList);
        self.status = "mcp manager reload requested".to_string();
    }

    fn request_mcp_manager(&mut self) {
        self.pending_actions.push(TuiAction::McpManager);
        self.status = "mcp manager requested".to_string();
    }

    fn active_mcp_manager_tab(&self) -> TuiMcpDetailKind {
        self.mcp_detail
            .as_ref()
            .map(|(kind, _)| *kind)
            .unwrap_or(TuiMcpDetailKind::Manager)
    }

    fn request_mcp_manager_tab(&mut self, kind: TuiMcpDetailKind) {
        if matches!(kind, TuiMcpDetailKind::Manager) {
            self.request_mcp_manager();
        } else {
            self.request_mcp_manager_details(kind, None);
        }
    }

    fn current_mcp_manager_servers(&self) -> Vec<TuiMcpServerEntry> {
        if !self.show_mcp_manager {
            return Vec::new();
        }
        self.mcp_detail
            .as_ref()
            .map(|(_, detail)| parse_mcp_manager_server_entries(detail))
            .unwrap_or_default()
    }

    fn visible_mcp_manager_servers(&self) -> Vec<TuiMcpServerEntry> {
        if !self.show_mcp_manager {
            return Vec::new();
        }
        self.mcp_detail
            .as_ref()
            .map(|(_, detail)| filter_mcp_manager_detail(detail, self.mcp_manager_filter.trim()))
            .map(|detail| parse_mcp_manager_server_entries(&detail))
            .unwrap_or_default()
    }

    fn selected_mcp_manager_server(&self) -> Option<TuiMcpServerEntry> {
        let servers = self.current_mcp_manager_servers();
        if servers.is_empty() {
            return None;
        }
        Some(servers[self.mcp_manager_selected_server.min(servers.len() - 1)].clone())
    }

    fn selected_mcp_manager_servers(&self) -> Vec<TuiMcpServerEntry> {
        let selected_keys = &self.mcp_manager_selected_server_keys;
        if selected_keys.is_empty() {
            return Vec::new();
        }
        self.current_mcp_manager_servers()
            .into_iter()
            .filter(|server| selected_keys.contains(&server.selection_key()))
            .collect()
    }

    fn select_relative_mcp_server(&mut self, delta: isize) {
        let servers = self.current_mcp_manager_servers();
        if servers.is_empty() {
            self.mcp_manager_selected_server = 0;
            self.status = "mcp manager has no server entries".to_string();
            return;
        }
        let len = servers.len();
        let current = self.mcp_manager_selected_server.min(len - 1);
        let next = if delta >= 0 {
            (current + delta as usize) % len
        } else {
            let steps = delta.unsigned_abs() % len;
            (current + len - steps) % len
        };
        self.mcp_manager_selected_server = next;
        let server = &servers[next];
        let state = if server.enabled {
            "enabled"
        } else {
            "disabled"
        };
        self.status = format!(
            "mcp manager selected server: {} ({}, {state})",
            server.name, server.source
        );
    }

    fn select_mcp_manager_server_entry(&mut self, selected: &TuiMcpServerEntry) {
        let servers = self.current_mcp_manager_servers();
        let Some(index) = servers.iter().position(|server| {
            server.name == selected.name
                && server.source == selected.source
                && server.enabled == selected.enabled
        }) else {
            self.status = format!("mcp manager server not found: {}", selected.name);
            return;
        };
        self.mcp_manager_selected_server = index;
        let state = if selected.enabled {
            "enabled"
        } else {
            "disabled"
        };
        self.status = format!(
            "mcp manager selected server: {} ({}, {state})",
            selected.name, selected.source
        );
    }

    fn toggle_selected_mcp_manager_server(&mut self) {
        let Some(server) = self.selected_mcp_manager_server() else {
            self.status = "mcp manager has no server selected".to_string();
            return;
        };
        self.toggle_mcp_manager_server_entry(&server);
    }

    fn toggle_mcp_manager_server_entry(&mut self, server: &TuiMcpServerEntry) {
        let key = server.selection_key();
        if self.mcp_manager_selected_server_keys.remove(&key) {
            self.status = format!(
                "mcp manager unselected server: {} ({} selected)",
                server.name,
                self.mcp_manager_selected_server_keys.len()
            );
        } else {
            self.mcp_manager_selected_server_keys.insert(key);
            self.status = format!(
                "mcp manager selected server for bulk action: {} ({} selected)",
                server.name,
                self.mcp_manager_selected_server_keys.len()
            );
        }
    }

    fn drag_select_mcp_manager_server_entry(&mut self, server: &TuiMcpServerEntry) {
        let current_key = server.selection_key();
        let anchor_key = self
            .mcp_manager_drag_anchor_key
            .get_or_insert_with(|| current_key.clone())
            .clone();
        let visible = self.visible_mcp_manager_servers();
        let anchor_index = visible
            .iter()
            .position(|entry| entry.selection_key() == anchor_key);
        let current_index = visible
            .iter()
            .position(|entry| entry.selection_key() == current_key);

        match (anchor_index, current_index) {
            (Some(anchor), Some(current)) => {
                let start = anchor.min(current);
                let end = anchor.max(current);
                for entry in &visible[start..=end] {
                    self.mcp_manager_selected_server_keys
                        .insert(entry.selection_key());
                }
            }
            _ => {
                self.mcp_manager_selected_server_keys.insert(anchor_key);
                self.mcp_manager_selected_server_keys
                    .insert(current_key.clone());
            }
        }

        self.select_mcp_manager_server_entry(server);
        self.status = format!(
            "mcp manager drag selected server range: {} selected",
            self.mcp_manager_selected_server_keys.len()
        );
    }

    fn select_all_visible_mcp_manager_servers(&mut self) {
        let servers = self.visible_mcp_manager_servers();
        if servers.is_empty() {
            self.status = "mcp manager has no visible server entries".to_string();
            return;
        }
        for server in &servers {
            self.mcp_manager_selected_server_keys
                .insert(server.selection_key());
        }
        self.status = format!(
            "mcp manager selected {} visible server(s)",
            self.mcp_manager_selected_server_keys.len()
        );
    }

    fn clear_mcp_manager_server_selection(&mut self) {
        let count = self.mcp_manager_selected_server_keys.len();
        self.mcp_manager_selected_server_keys.clear();
        self.status = format!("mcp manager cleared {count} selected server(s)");
    }

    fn request_mcp_manager_enabled(&mut self, enabled: bool) {
        if self.mcp_manager_selected_server_keys.is_empty() {
            self.request_selected_mcp_server_enabled(enabled);
        } else {
            self.request_selected_mcp_servers_enabled(enabled);
        }
    }

    fn request_selected_mcp_server_enabled(&mut self, enabled: bool) {
        let Some(server) = self.selected_mcp_manager_server() else {
            self.status = "mcp manager has no server selected".to_string();
            return;
        };
        let Some(scope) = server.scope() else {
            self.status = format!(
                "mcp server action requires project/user source: {}",
                server.source
            );
            return;
        };
        self.pending_actions.push(TuiAction::McpSetEnabled {
            scope,
            name: server.name.clone(),
            enabled,
        });
        let action = if enabled { "enable" } else { "disable" };
        self.status = format!(
            "mcp {} server {action} requested: {}",
            scope.label(),
            server.name
        );
    }

    fn request_selected_mcp_servers_enabled(&mut self, enabled: bool) {
        let selected = self.selected_mcp_manager_servers();
        if selected.is_empty() {
            self.status = "mcp manager has no bulk-selected servers".to_string();
            return;
        }
        let mut queued = 0usize;
        let mut skipped = 0usize;
        for server in selected {
            let Some(scope) = server.scope() else {
                skipped += 1;
                continue;
            };
            self.pending_actions.push(TuiAction::McpSetEnabled {
                scope,
                name: server.name.clone(),
                enabled,
            });
            queued += 1;
        }
        let action = if enabled { "enable" } else { "disable" };
        self.status = if skipped == 0 {
            format!("mcp manager bulk {action} requested for {queued} server(s)")
        } else {
            format!("mcp manager bulk {action} requested for {queued} server(s), skipped {skipped}")
        };
    }

    fn request_selected_mcp_server_remove(&mut self) {
        let Some(server) = self.selected_mcp_manager_server() else {
            self.status = "mcp manager has no server selected".to_string();
            return;
        };
        let Some(scope) = server.scope() else {
            self.status = format!(
                "mcp server action requires project/user source: {}",
                server.source
            );
            return;
        };
        self.mcp_remove_confirmation = Some(TuiMcpPendingRemove {
            scope,
            name: server.name.clone(),
        });
        self.status = format!(
            "confirm mcp {} server remove: {}",
            scope.label(),
            server.name
        );
    }

    fn request_selected_mcp_server_tools(&mut self) {
        let Some(server) = self.selected_mcp_manager_server() else {
            self.status = "mcp manager has no server selected".to_string();
            return;
        };
        self.request_mcp_manager_details(TuiMcpDetailKind::Tools, Some(&server.name));
    }

    fn request_mcp_details(&mut self, kind: TuiMcpDetailKind, server: Option<&str>) {
        let server = server.map(str::trim).filter(|value| !value.is_empty());
        self.pending_actions.push(TuiAction::McpDetails {
            kind: kind.clone(),
            server: server.map(ToOwned::to_owned),
        });
        self.status = match server {
            Some(server) => format!("mcp {} detail requested for {server}", kind.command_name()),
            None => format!("mcp {} detail requested", kind.command_name()),
        };
    }

    fn request_mcp_manager_details(&mut self, kind: TuiMcpDetailKind, server: Option<&str>) {
        let server = server.map(str::trim).filter(|value| !value.is_empty());
        self.pending_actions.push(TuiAction::McpManagerDetails {
            kind: kind.clone(),
            server: server.map(ToOwned::to_owned),
        });
        self.status = match server {
            Some(server) => format!(
                "mcp manager {} detail requested for {server}",
                kind.command_name()
            ),
            None => format!("mcp manager {} detail requested", kind.command_name()),
        };
    }

    fn request_mcp_init(&mut self, force: bool) {
        self.pending_actions.push(TuiAction::McpInit { force });
        self.status = if force {
            "mcp project config init requested with --force".to_string()
        } else {
            "mcp project config init requested".to_string()
        };
    }

    fn request_mcp_add_stdio(
        &mut self,
        scope: TuiMcpConfigScope,
        name: &str,
        command: &str,
        args: &[&str],
    ) {
        let name = name.trim();
        let command = command.trim();
        if name.is_empty() || command.is_empty() {
            self.status = format!(
                "usage: mcp {} add stdio <name> <command> [args...]",
                scope.label()
            );
            return;
        }
        let args = args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();
        self.pending_actions.push(TuiAction::McpAddStdio {
            scope,
            name: name.to_string(),
            command: command.to_string(),
            args,
        });
        self.status = format!("mcp {} stdio server add requested: {name}", scope.label());
    }

    fn request_mcp_add_remote(
        &mut self,
        scope: TuiMcpConfigScope,
        name: &str,
        transport: &str,
        url: &str,
    ) {
        let name = name.trim();
        let url = url.trim();
        if name.is_empty() || url.is_empty() {
            self.status = format!("usage: mcp {} add {transport} <name> <url>", scope.label());
            return;
        }
        self.pending_actions.push(TuiAction::McpAddRemote {
            scope,
            name: name.to_string(),
            transport: transport.to_string(),
            url: url.to_string(),
        });
        self.status = format!(
            "mcp {} {transport} server add requested: {name}",
            scope.label()
        );
    }

    fn request_mcp_remove(&mut self, scope: TuiMcpConfigScope, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            self.status = format!("usage: mcp {} remove <name>", scope.label());
            return;
        }
        self.pending_actions.push(TuiAction::McpRemove {
            scope,
            name: name.to_string(),
        });
        self.status = format!("mcp {} server remove requested: {name}", scope.label());
    }

    fn request_mcp_set_enabled(&mut self, scope: TuiMcpConfigScope, name: &str, enabled: bool) {
        let name = name.trim();
        if name.is_empty() {
            let scope_label = scope.label();
            self.status = if enabled {
                format!("usage: mcp {scope_label} enable <name>")
            } else {
                format!("usage: mcp {scope_label} disable <name>")
            };
            return;
        }
        self.pending_actions.push(TuiAction::McpSetEnabled {
            scope,
            name: name.to_string(),
            enabled,
        });
        let action = if enabled { "enable" } else { "disable" };
        self.status = format!("mcp {} server {action} requested: {name}", scope.label());
    }

    fn request_mcp_validate(&mut self) {
        self.pending_actions.push(TuiAction::McpValidate);
        self.status = "mcp validate requested".to_string();
    }

    fn request_default_automation_trigger(&mut self) {
        let selected = self
            .active_thread_automations()
            .into_iter()
            .find(|automation| automation.status == "active")
            .or_else(|| self.active_thread_automations().into_iter().next())
            .map(|automation| automation.id.clone());
        if let Some(automation_id) = selected {
            self.request_automation_trigger(&automation_id, None);
        } else {
            self.status = "no automation in active thread to trigger".to_string();
        }
    }

    fn request_automation_trigger(&mut self, automation_id: &str, prompt_override: Option<String>) {
        let Some(automation) = self.active_automation_by_id(automation_id).cloned() else {
            self.status = format!("automation not found in active thread: {automation_id}");
            return;
        };
        let prompt_override = prompt_override.and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
        self.pending_actions.push(TuiAction::TriggerAutomation {
            automation_id: automation.id.clone(),
            prompt_override,
        });
        self.status = format!(
            "automation trigger requested: {} {}",
            automation.id,
            clip_line(&automation.name, 60)
        );
    }

    fn drain_actions(&mut self) -> Vec<TuiAction> {
        std::mem::take(&mut self.pending_actions)
    }
}

fn clip_line(value: &str, max_chars: usize) -> String {
    let line = value.lines().next().unwrap_or("").trim();
    let mut clipped = line.chars().take(max_chars).collect::<String>();
    if line.chars().count() > max_chars {
        clipped.push_str("...");
    }
    clipped
}

fn stable_tui_segment(value: &str) -> String {
    let segment = value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        .take(48)
        .collect::<String>();
    if segment.is_empty() {
        "session".to_string()
    } else {
        segment
    }
}

fn resolve_tui_attachment_path(raw_path: &str, workspace: &Path) -> PathBuf {
    let path = PathBuf::from(unquote_tui_path(raw_path));
    let expanded = expand_tilde_path(path);
    if expanded.is_absolute() {
        expanded
    } else {
        workspace.join(expanded)
    }
}

fn expand_tilde_path(path: PathBuf) -> PathBuf {
    let value = path.to_string_lossy();
    if value == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    path
}

fn tui_media_kind(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "ppm" => Some("image"),
        "mp4" | "mov" | "m4v" | "webm" | "avi" | "mkv" => Some("video"),
        _ => None,
    }
}

fn tui_attachment_media_type(path: &Path, kind: &str) -> String {
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or(kind)
        .to_ascii_lowercase();
    match (kind, ext.as_str()) {
        ("image", "jpg" | "jpeg") => "image/jpeg".to_string(),
        ("image", "png") => "image/png".to_string(),
        ("image", "gif") => "image/gif".to_string(),
        ("image", "webp") => "image/webp".to_string(),
        ("image", "bmp") => "image/bmp".to_string(),
        ("image", "tif" | "tiff") => "image/tiff".to_string(),
        ("image", "ppm") => "image/x-portable-pixmap".to_string(),
        ("video", "mp4" | "m4v") => "video/mp4".to_string(),
        ("video", "mov") => "video/quicktime".to_string(),
        ("video", "webm") => "video/webm".to_string(),
        ("video", "avi") => "video/x-msvideo".to_string(),
        ("video", "mkv") => "video/x-matroska".to_string(),
        _ => format!("{kind}/{ext}"),
    }
}

fn tui_attachment_reference(path: &Path, workspace: &Path) -> String {
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    path.strip_prefix(&workspace)
        .ok()
        .and_then(|relative| relative.to_str())
        .filter(|relative| !relative.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| path.display().to_string())
}

fn render_tui_attachment_block(kind: &str, reference: &str, path: &Path) -> String {
    let media_type = tui_attachment_media_type(path, kind);
    let mut block = format!("Attached {kind} files:\n- {reference} ({media_type})");
    if kind == "image" {
        block.push_str(&format!(
            "\n\nUse image_analyze with image_path=\"{reference}\" if visual inspection is needed."
        ));
    }
    block
}

fn tui_queue_preview(value: &str) -> String {
    clip_line(value, TUI_QUEUE_PREVIEW_LIMIT)
}

fn format_transcript_item_lines(item: &TuiItem, verbose_transcript: bool) -> Vec<String> {
    let role = item.role.as_deref().unwrap_or(item.item_type.as_str());
    if item.item_type == "reasoning" && !verbose_transcript {
        let chars = item.content.chars().count();
        let preview = clip_line(&item.content, 72);
        if preview.is_empty() {
            vec![format!(
                "{role} [{}]: thinking compact ({chars} chars; /verbose on for full text)",
                item.status
            )]
        } else {
            vec![format!(
                "{role} [{}]: thinking compact ({chars} chars) {preview}",
                item.status
            )]
        }
    } else if item.item_type == "reasoning" {
        let mut content_lines = item.content.lines();
        let first = content_lines.next().unwrap_or("").trim();
        let mut lines = vec![format!("{role} [{}]: {first}", item.status)];
        lines.extend(content_lines.map(|line| format!("  {}", line.trim())));
        lines
    } else {
        vec![format!(
            "{} [{}]: {}",
            role,
            item.status,
            clip_line(&item.content, 120)
        )]
    }
}

fn render_reasoning_empty_detail(
    replay_limit: usize,
    pinned_turn_ids: &BTreeSet<String>,
) -> String {
    format!(
        "Reasoning\n\nNo reasoning items recorded for the active thread.\n\n{}",
        render_reasoning_replay_control(replay_limit, pinned_turn_ids)
    )
}

fn render_reasoning_list_detail(
    thread: &TuiThread,
    items: &[&TuiItem],
    replay_limit: usize,
    pinned_turn_ids: &BTreeSet<String>,
) -> String {
    if items.is_empty() {
        return render_reasoning_empty_detail(replay_limit, pinned_turn_ids);
    }

    let mut detail = String::new();
    detail.push_str("Reasoning\n");
    detail.push_str(&format!("thread: {}\n", thread.title));
    detail.push_str(&format!("thread_id: {}\n", thread.id));
    detail.push_str(&format!("items: {}\n", items.len()));
    detail.push_str(&render_reasoning_replay_control(
        replay_limit,
        pinned_turn_ids,
    ));
    detail.push_str("\n\nReasoning items:\n");
    for (index, item) in items.iter().enumerate() {
        let pinned = item
            .turn_id
            .as_deref()
            .is_some_and(|turn_id| pinned_turn_ids.contains(turn_id));
        detail.push_str(&format!(
            "#{} {} status={} chars={}{}",
            index + 1,
            item.id,
            item.status,
            item.content.chars().count(),
            if pinned { " pinned" } else { "" }
        ));
        if let Some(turn_id) = item.turn_id.as_deref() {
            detail.push_str(&format!(" turn={turn_id}"));
        }
        detail.push('\n');
        detail.push_str("  ");
        detail.push_str(&clip_line(&item.content, 160));
        detail.push('\n');
    }
    detail.push_str("\n");
    detail.push_str(render_reasoning_selector_help());
    detail
}

fn render_reasoning_item_detail(
    item: &TuiItem,
    position: usize,
    total: usize,
    replay_limit: usize,
    pinned_turn_ids: &BTreeSet<String>,
) -> String {
    let mut detail = String::new();
    detail.push_str("Reasoning item\n");
    detail.push_str(&format!("position: {position}/{total}\n"));
    detail.push_str(&format!("id: {}\n", item.id));
    detail.push_str(&format!("thread_id: {}\n", item.thread_id));
    if let Some(turn_id) = item.turn_id.as_deref() {
        detail.push_str(&format!("turn_id: {turn_id}\n"));
        if pinned_turn_ids.contains(turn_id) {
            detail.push_str("replay_pin: true\n");
        }
    }
    detail.push_str(&format!("status: {}\n", item.status));
    detail.push_str(&format!("chars: {}\n", item.content.chars().count()));
    detail.push_str(&render_reasoning_replay_control(
        replay_limit,
        pinned_turn_ids,
    ));
    detail.push_str("\n\nContent:\n");
    detail.push_str(&item.content);
    if !item.content.ends_with('\n') {
        detail.push('\n');
    }
    detail
}

fn render_reasoning_search_detail(
    thread: &TuiThread,
    items: &[&TuiItem],
    query: &str,
    replay_limit: usize,
    pinned_turn_ids: &BTreeSet<String>,
) -> String {
    let mut detail = String::new();
    detail.push_str("Reasoning search\n");
    detail.push_str(&format!("thread: {}\n", thread.title));
    detail.push_str(&format!("thread_id: {}\n", thread.id));
    detail.push_str(&format!("query: {}\n", highlight_query(query, query)));
    detail.push_str(&render_reasoning_replay_control(
        replay_limit,
        pinned_turn_ids,
    ));
    detail.push_str("\n\nMatches:\n");

    let mut matched = 0_usize;
    for (index, item) in items.iter().enumerate() {
        if !reasoning_item_matches_query(item, query) {
            continue;
        }
        matched += 1;
        let pinned = item
            .turn_id
            .as_deref()
            .is_some_and(|turn_id| pinned_turn_ids.contains(turn_id));
        detail.push_str(&format!(
            "#{} {} status={} chars={}{}",
            index + 1,
            item.id,
            item.status,
            item.content.chars().count(),
            if pinned { " pinned" } else { "" }
        ));
        if let Some(turn_id) = item.turn_id.as_deref() {
            detail.push_str(&format!(" turn={}", highlight_query(turn_id, query)));
        }
        detail.push('\n');
        detail.push_str("  ");
        detail.push_str(&reasoning_search_excerpt(&item.content, query, 180));
        detail.push('\n');
    }

    if matched == 0 {
        detail.push_str("No matching reasoning items.\n");
    }
    detail.push_str("\n");
    detail.push_str(render_reasoning_selector_help());
    detail
}

fn render_reasoning_pins_detail(
    thread: &TuiThread,
    items: &[&TuiItem],
    replay_limit: usize,
    pinned_turn_ids: &BTreeSet<String>,
) -> String {
    let mut detail = String::new();
    detail.push_str("Reasoning replay pins\n");
    detail.push_str(&format!("thread: {}\n", thread.title));
    detail.push_str(&format!("thread_id: {}\n", thread.id));
    detail.push_str(&render_reasoning_replay_control(
        replay_limit,
        pinned_turn_ids,
    ));
    detail.push_str("\n\nPinned turns:\n");
    if pinned_turn_ids.is_empty() {
        detail.push_str("none\n");
    } else {
        for turn_id in pinned_turn_ids {
            let count = items
                .iter()
                .filter(|item| item.turn_id.as_deref() == Some(turn_id.as_str()))
                .count();
            detail.push_str(&format!("- {turn_id} reasoning_items={count}\n"));
        }
    }
    detail.push_str("\n");
    detail.push_str(render_reasoning_selector_help());
    detail
}

fn render_reasoning_replay_control(
    replay_limit: usize,
    pinned_turn_ids: &BTreeSet<String>,
) -> String {
    let pins = if pinned_turn_ids.is_empty() {
        "none".to_string()
    } else {
        pinned_turn_ids
            .iter()
            .take(6)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let suffix = pinned_turn_ids
        .len()
        .checked_sub(6)
        .filter(|remaining| *remaining > 0)
        .map(|remaining| format!(" (+{remaining} more)"))
        .unwrap_or_default();
    format!(
        "replay_limit: {replay_limit} latest persisted reasoning item(s)\npinned_turns: {pins}{suffix}"
    )
}

fn render_reasoning_selector_help() -> &'static str {
    "Commands: reasoning list | reasoning search <query> | reasoning show <latest|index|item-id|turn-id> | reasoning replay <0..20> | reasoning pin <selector> | reasoning pins | reasoning unpin <selector|all>"
}

fn count_reasoning_search_matches(items: &[&TuiItem], query: &str) -> usize {
    items
        .iter()
        .filter(|item| reasoning_item_matches_query(item, query))
        .count()
}

fn reasoning_item_matches_query(item: &TuiItem, query: &str) -> bool {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return false;
    }
    item.id.to_ascii_lowercase().contains(&query)
        || item
            .turn_id
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase()
            .contains(&query)
        || item.status.to_ascii_lowercase().contains(&query)
        || item.content.to_ascii_lowercase().contains(&query)
}

fn reasoning_search_excerpt(content: &str, query: &str, max_chars: usize) -> String {
    let query_lower = query.trim().to_ascii_lowercase();
    for line in content.lines() {
        if line.to_ascii_lowercase().contains(&query_lower) {
            return highlight_query(&clip_line(line, max_chars), query);
        }
    }
    highlight_query(&clip_line(content, max_chars), query)
}

fn highlight_query(value: &str, query: &str) -> String {
    let needle = query.trim();
    if needle.is_empty() {
        return value.to_string();
    }
    let lower = value.to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    let mut offset = 0_usize;
    let mut highlighted = String::new();
    while let Some(relative) = lower[offset..].find(&needle_lower) {
        let start = offset + relative;
        let end = start + needle_lower.len();
        highlighted.push_str(&value[offset..start]);
        highlighted.push_str("[[");
        highlighted.push_str(&value[start..end]);
        highlighted.push_str("]]");
        offset = end;
    }
    highlighted.push_str(&value[offset..]);
    highlighted
}

fn task_progress_lines(task: &TuiTaskRecord, selected: bool, bulk_selected: bool) -> Vec<String> {
    let marker = if selected {
        ">"
    } else if bulk_selected {
        "*"
    } else {
        " "
    };
    let bulk_suffix = if bulk_selected { " selected" } else { "" };
    vec![
        format!(
            "{marker} Task [{}] {}{}",
            task.status,
            short_task_id(&task.id),
            bulk_suffix
        ),
        format!("  {} updated {}", task.kind, task.updated_at),
        format!("  {}", clip_line(&task.summary, 62)),
    ]
}

fn runtime_item_progress_lines(items: &[&TuiItem]) -> Vec<String> {
    if items.is_empty() {
        return Vec::new();
    }

    let mut statuses = BTreeMap::<&str, usize>::new();
    let mut item_types = BTreeMap::<&str, usize>::new();
    for item in items {
        *statuses.entry(item.status.as_str()).or_insert(0) += 1;
        *item_types.entry(item.item_type.as_str()).or_insert(0) += 1;
    }

    let mut status_parts = Vec::new();
    for status in ["running", "pending", "completed", "failed", "cancelled"] {
        if let Some(count) = statuses.remove(status) {
            status_parts.push(format!("{status}={count}"));
        }
    }
    for (status, count) in statuses {
        status_parts.push(format!("{status}={count}"));
    }

    let mut type_parts = Vec::new();
    for item_type in [
        "message",
        "reasoning",
        "tool_call",
        "tool_result",
        "diagnostic",
        "event",
    ] {
        if let Some(count) = item_types.remove(item_type) {
            type_parts.push(format!("{item_type}={count}"));
        }
    }
    for (item_type, count) in item_types {
        type_parts.push(format!("{item_type}={count}"));
    }

    let latest = items
        .iter()
        .rev()
        .find(|item| !item.content.trim().is_empty())
        .or_else(|| items.last())
        .expect("non-empty items");

    let mut lines = Vec::new();
    push_progress_parts(&mut lines, "Item states", &status_parts);
    push_progress_parts(&mut lines, "Item types", &type_parts);
    lines.push(format!("Latest: {}", latest.item_type));
    lines.push(format!(
        "  [{}] {}",
        latest.status,
        clip_line(&latest.content, 54)
    ));
    lines
}

fn push_progress_parts(lines: &mut Vec<String>, label: &str, parts: &[String]) {
    let Some((first, rest)) = parts.split_first() else {
        return;
    };
    lines.push(format!("{label}: {first}"));
    if !rest.is_empty() {
        lines.push(format!("  {}", rest.join(" ")));
    }
}

fn task_status_counts_line(tasks: &[&TuiTaskRecord]) -> String {
    let mut counts = BTreeMap::<&str, usize>::new();
    for task in tasks {
        *counts.entry(task.status.as_str()).or_insert(0) += 1;
    }

    let mut parts = Vec::new();
    for status in [
        "running",
        "pending",
        "paused",
        "completed",
        "failed",
        "cancelled",
    ] {
        if let Some(count) = counts.remove(status) {
            parts.push(format!("{status}={count}"));
        }
    }
    parts.extend(
        counts
            .into_iter()
            .map(|(status, count)| format!("{status}={count}")),
    );
    format!("Task states: {}", parts.join(" "))
}

fn short_task_id(id: &str) -> String {
    clip_line(id, 22)
}

fn session_matches_filter(session: &TuiSession, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let filter = filter.to_ascii_lowercase();
    [
        session.id.as_str(),
        session.title.as_str(),
        session.workspace.as_str(),
        session.status.as_str(),
    ]
    .into_iter()
    .any(|value| value.to_ascii_lowercase().contains(&filter))
        || session.thread_count.to_string().contains(&filter)
}

fn thread_matches_filter(thread: &TuiThread, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let filter = filter.to_ascii_lowercase();
    [
        thread.id.as_str(),
        thread.title.as_str(),
        thread.mode.as_str(),
        thread.status.as_str(),
        thread.latest_turn_id.as_deref().unwrap_or(""),
    ]
    .into_iter()
    .any(|value| value.to_ascii_lowercase().contains(&filter))
        || thread.event_seq.to_string().contains(&filter)
}

fn display_with_cursor(value: &str, cursor: usize, show_cursor: bool) -> String {
    if !show_cursor {
        return value.to_string();
    }
    let cursor = clamp_char_boundary(value, cursor);
    let mut displayed = String::with_capacity(value.len() + 1);
    displayed.push_str(&value[..cursor]);
    displayed.push('|');
    displayed.push_str(&value[cursor..]);
    displayed
}

fn insert_char_at_cursor(value: &mut String, cursor: &mut usize, ch: char) {
    *cursor = clamp_char_boundary(value, *cursor);
    value.insert(*cursor, ch);
    *cursor += ch.len_utf8();
}

fn backspace_at_cursor(value: &mut String, cursor: &mut usize) {
    *cursor = clamp_char_boundary(value, *cursor);
    if *cursor == 0 {
        return;
    }
    let previous = previous_char_boundary(value, *cursor);
    value.drain(previous..*cursor);
    *cursor = previous;
}

fn delete_at_cursor(value: &mut String, cursor: usize) {
    let cursor = clamp_char_boundary(value, cursor);
    if cursor >= value.len() {
        return;
    }
    let next = next_char_boundary(value, cursor);
    value.drain(cursor..next);
}

fn handle_text_control_key(value: &mut String, cursor: &mut usize, code: KeyCode) -> bool {
    match code {
        KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'a') => {
            *cursor = 0;
            true
        }
        KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'e') => {
            *cursor = value.len();
            true
        }
        KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'u') => {
            value.clear();
            *cursor = 0;
            true
        }
        KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'k') => {
            *cursor = clamp_char_boundary(value, *cursor);
            value.drain(*cursor..);
            true
        }
        KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'w') => {
            *cursor = clamp_char_boundary(value, *cursor);
            let previous = previous_word_boundary(value, *cursor);
            value.drain(previous..*cursor);
            *cursor = previous;
            true
        }
        KeyCode::Left => {
            *cursor = previous_word_boundary(value, *cursor);
            true
        }
        KeyCode::Right => {
            *cursor = next_word_boundary(value, *cursor);
            true
        }
        _ => false,
    }
}

fn composer_slash_completion_matches(app: &TuiApp, prefix: &str) -> Vec<String> {
    if !prefix.starts_with('/') {
        return Vec::new();
    }
    let mut matches = TUI_COMPOSER_SLASH_COMPLETIONS
        .iter()
        .copied()
        .filter(|command| command.starts_with(prefix))
        .map(str::to_string)
        .collect::<Vec<_>>();
    matches.extend(
        app.extra_slash_completions
            .iter()
            .filter(|command| command.starts_with(prefix))
            .cloned(),
    );
    matches.extend(
        project_custom_slash_commands(app)
            .into_iter()
            .filter(|command| command.starts_with(prefix)),
    );
    matches.sort();
    matches.dedup();
    matches
}

fn project_custom_slash_commands(app: &TuiApp) -> Vec<String> {
    let Some(session) = app.selected_session() else {
        return Vec::new();
    };
    discover_custom_slash_commands_dir(&Path::new(&session.workspace).join(".dscode/commands"))
}

pub fn discover_custom_slash_commands_dir(root: &Path) -> Vec<String> {
    let mut commands = Vec::new();
    collect_project_custom_slash_commands(root, Path::new(""), 0, &mut commands);
    commands.sort();
    commands.dedup();
    commands
}

fn collect_project_custom_slash_commands(
    dir: &Path,
    prefix: &Path,
    depth: usize,
    out: &mut Vec<String>,
) {
    if depth > 6 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if path.is_dir() {
            if !valid_custom_slash_segment(name) {
                continue;
            }
            let mut next_prefix = prefix.to_path_buf();
            next_prefix.push(name);
            collect_project_custom_slash_commands(&path, &next_prefix, depth + 1, out);
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if !valid_custom_slash_segment(stem) {
            continue;
        }
        let mut command_path = prefix.to_path_buf();
        command_path.push(stem);
        let command = command_path
            .components()
            .filter_map(|component| component.as_os_str().to_str())
            .collect::<Vec<_>>()
            .join("/");
        if !command.is_empty() {
            out.push(format!("/{command}"));
        }
    }
}

fn valid_custom_slash_segment(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('.')
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

fn composer_slash_hint_line(app: &TuiApp) -> Option<String> {
    if !app.composer_focused {
        return None;
    }
    let cursor = clamp_char_boundary(&app.composer, app.composer_cursor);
    let prefix = &app.composer[..cursor];
    if !prefix.starts_with('/') {
        return None;
    }
    let matches = composer_slash_completion_matches(app, prefix);
    if matches.is_empty() {
        return Some(format!(
            "Slash: no matches for {}",
            clip_line(prefix.trim(), 40)
        ));
    }
    let preview = matches
        .iter()
        .take(6)
        .map(|value| value.trim_end())
        .collect::<Vec<_>>()
        .join("  ");
    let more = matches.len().saturating_sub(6);
    let suffix = if more == 0 {
        String::new()
    } else {
        format!("  +{more}")
    };
    Some(format!("Slash: {preview}{suffix}"))
}

fn longest_common_prefix(values: &[&str]) -> String {
    let Some(first) = values.first() else {
        return String::new();
    };
    let mut prefix = (*first).to_string();
    for value in values.iter().skip(1) {
        while !value.starts_with(&prefix) {
            if prefix.is_empty() {
                return prefix;
            }
            let previous = previous_char_boundary(&prefix, prefix.len());
            prefix.truncate(previous);
        }
    }
    prefix
}

fn previous_word_boundary(value: &str, cursor: usize) -> usize {
    let mut cursor = clamp_char_boundary(value, cursor);
    while cursor > 0 {
        let previous = previous_char_boundary(value, cursor);
        if !value[previous..cursor].chars().all(char::is_whitespace) {
            break;
        }
        cursor = previous;
    }
    while cursor > 0 {
        let previous = previous_char_boundary(value, cursor);
        if value[previous..cursor].chars().all(char::is_whitespace) {
            break;
        }
        cursor = previous;
    }
    cursor
}

fn next_word_boundary(value: &str, cursor: usize) -> usize {
    let mut cursor = clamp_char_boundary(value, cursor);
    while cursor < value.len() {
        let next = next_char_boundary(value, cursor);
        if value[cursor..next].chars().all(char::is_whitespace) {
            break;
        }
        cursor = next;
    }
    while cursor < value.len() {
        let next = next_char_boundary(value, cursor);
        if !value[cursor..next].chars().all(char::is_whitespace) {
            break;
        }
        cursor = next;
    }
    cursor
}

fn previous_char_boundary(value: &str, cursor: usize) -> usize {
    let cursor = clamp_char_boundary(value, cursor);
    if cursor == 0 {
        return 0;
    }
    value[..cursor]
        .char_indices()
        .last()
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn next_char_boundary(value: &str, cursor: usize) -> usize {
    let cursor = clamp_char_boundary(value, cursor);
    if cursor >= value.len() {
        return value.len();
    }
    let mut chars = value[cursor..].char_indices();
    let _current = chars.next();
    chars
        .next()
        .map(|(offset, _)| cursor + offset)
        .unwrap_or(value.len())
}

fn clamp_char_boundary(value: &str, cursor: usize) -> usize {
    let mut cursor = cursor.min(value.len());
    while cursor > 0 && !value.is_char_boundary(cursor) {
        cursor -= 1;
    }
    cursor
}

fn push_status_row(out: &mut String, label: &str, value: &str) {
    let _ = writeln!(out, "  {label:<16} {value}");
}

fn format_context_usage(summary: &TuiUsageSummary) -> String {
    let used = summary.latest_total_tokens.min(TUI_CONTEXT_WINDOW_TOKENS);
    let percent = (used as f64 / TUI_CONTEXT_WINDOW_TOKENS as f64 * 100.0).clamp(0.0, 100.0);
    format!("~{} / {} ({percent:.1}%)", used, TUI_CONTEXT_WINDOW_TOKENS)
}

fn format_optional_microusd(value: Option<u64>) -> String {
    value
        .map(format_microusd)
        .unwrap_or_else(|| "unpriced model".to_string())
}

fn summarize_status_counts<'a>(statuses: impl IntoIterator<Item = &'a str>) -> String {
    let mut counts = BTreeMap::<&str, usize>::new();
    for status in statuses {
        *counts.entry(status).or_insert(0) += 1;
    }
    if counts.is_empty() {
        return "none".to_string();
    }
    counts
        .into_iter()
        .map(|(status, count)| format!("{status}={count}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn context_strategy(latest_total_tokens: u64) -> &'static str {
    match latest_total_tokens {
        900_000.. => "must_compact_or_chunk",
        800_000.. => "prepare_compaction",
        500_000.. => "monitor",
        _ => "normal",
    }
}

fn render_feedback_detail(command: TuiFeedbackCommand) -> String {
    let mut detail = String::new();
    let _ = writeln!(detail, "DeepSeekCode Feedback");
    let _ = writeln!(detail, "=====================");
    let _ = writeln!(detail);
    let _ = writeln!(detail, "Repository: {DEEPSEEK_CODE_REPO_URL}");
    let _ = writeln!(detail);
    match command {
        TuiFeedbackCommand::Show => {
            let _ = writeln!(detail, "Choose a feedback target:");
            let _ = writeln!(detail, "- feedback bug       {DEEPSEEK_CODE_BUG_URL}");
            let _ = writeln!(detail, "- feedback feature   {DEEPSEEK_CODE_FEATURE_URL}");
            let _ = writeln!(detail, "- feedback security  {DEEPSEEK_CODE_SECURITY_URL}");
        }
        TuiFeedbackCommand::Bug => {
            let _ = writeln!(detail, "Bug report");
            let _ = writeln!(detail, "{DEEPSEEK_CODE_BUG_URL}");
            let _ = writeln!(detail);
            let _ = writeln!(
                detail,
                "Include reproduction steps, expected behavior, actual behavior, and version/context."
            );
        }
        TuiFeedbackCommand::Feature => {
            let _ = writeln!(detail, "Feature request");
            let _ = writeln!(detail, "{DEEPSEEK_CODE_FEATURE_URL}");
            let _ = writeln!(detail);
            let _ = writeln!(
                detail,
                "Describe the workflow, why existing behavior is insufficient, and what success looks like."
            );
        }
        TuiFeedbackCommand::Security => {
            let _ = writeln!(detail, "Security report");
            let _ = writeln!(detail, "{DEEPSEEK_CODE_SECURITY_URL}");
            let _ = writeln!(detail);
            let _ = writeln!(
                detail,
                "Review the security policy before sending vulnerability details. Avoid public issue text for sensitive reports."
            );
        }
    }
    detail
}

fn render_links_detail() -> String {
    let mut detail = String::new();
    let _ = writeln!(detail, "DeepSeekCode Links");
    let _ = writeln!(detail, "==================");
    let _ = writeln!(detail);
    let _ = writeln!(detail, "DeepSeekCode");
    let _ = writeln!(detail, "- Repository  {DEEPSEEK_CODE_REPO_URL}");
    let _ = writeln!(detail, "- Issues      {DEEPSEEK_CODE_ISSUES_URL}");
    let _ = writeln!(detail, "- Releases    {DEEPSEEK_CODE_RELEASES_URL}");
    let _ = writeln!(detail, "- Docs        {DEEPSEEK_CODE_DOCS_URL}");
    let _ = writeln!(detail);
    let _ = writeln!(detail, "DeepSeek");
    let _ = writeln!(detail, "- Platform    {DEEPSEEK_PLATFORM_URL}");
    let _ = writeln!(detail, "- API docs    {DEEPSEEK_API_DOCS_URL}");
    let _ = writeln!(detail);
    let _ = writeln!(detail, "Aliases: links, dashboard, api");
    detail
}

fn render_help_detail(command: &TuiHelpCommand) -> String {
    match command {
        TuiHelpCommand::Show => render_help_index_detail(),
        TuiHelpCommand::Topic(topic) => render_help_topic_detail(topic),
    }
}

fn render_help_index_detail() -> String {
    let mut detail = String::new();
    let _ = writeln!(detail, "DeepSeekCode Help");
    let _ = writeln!(detail, "=================");
    let _ = writeln!(detail);
    let _ = writeln!(detail, "Use /help <command> for command-specific help.");
    let _ = writeln!(detail);

    let mut current_category = "";
    for command in TUI_HELP_COMMANDS {
        if command.category != current_category {
            current_category = command.category;
            let _ = writeln!(detail);
            let _ = writeln!(detail, "{current_category}");
            let _ = writeln!(detail, "{}", "-".repeat(current_category.len()));
        }
        let aliases = if command.aliases.is_empty() {
            String::new()
        } else {
            format!(" (aliases: {})", command.aliases.join(", "))
        };
        let _ = writeln!(
            detail,
            "- /{:<12} {}{}",
            command.name, command.description, aliases
        );
    }
    detail
}

fn render_help_topic_detail(topic: &str) -> String {
    let mut detail = String::new();
    let normalized = topic.trim().trim_start_matches('/').to_ascii_lowercase();
    let _ = writeln!(detail, "DeepSeekCode Help");
    let _ = writeln!(detail, "=================");
    let _ = writeln!(detail);

    if let Some(command) = find_tui_help_command(&normalized) {
        let _ = writeln!(detail, "/{}", command.name);
        let _ = writeln!(detail, "{}", "-".repeat(command.name.len() + 1));
        let _ = writeln!(detail, "{}", command.description);
        let _ = writeln!(detail);
        let _ = writeln!(detail, "Usage: {}", command.usage);
        if !command.aliases.is_empty() {
            let _ = writeln!(detail, "Aliases: {}", command.aliases.join(", "));
        }
        return detail;
    }

    let _ = writeln!(detail, "Unknown command: {topic}");
    let _ = writeln!(detail);
    let _ = writeln!(detail, "Known commands:");
    let _ = writeln!(
        detail,
        "{}",
        TUI_HELP_COMMANDS
            .iter()
            .map(|command| format!("/{}", command.name))
            .collect::<Vec<_>>()
            .join(", ")
    );
    detail
}

fn find_tui_help_command(topic: &str) -> Option<&'static TuiHelpCommandInfo> {
    TUI_HELP_COMMANDS.iter().find(|command| {
        command.name == topic || command.aliases.iter().any(|alias| *alias == topic)
    })
}

fn format_cache_hit_rate(cache_hit: u64, cache_miss: u64) -> String {
    let total = cache_hit.saturating_add(cache_miss);
    if total == 0 {
        return "0.00%".to_string();
    }
    let basis_points = cache_hit.saturating_mul(10_000) / total;
    format!("{}.{:02}%", basis_points / 100, basis_points % 100)
}

fn format_microusd(microusd: u64) -> String {
    format!("${}.{:06}", microusd / 1_000_000, microusd % 1_000_000)
}

fn format_ratio_bar(
    left: u64,
    right: u64,
    width: usize,
    left_char: char,
    right_char: char,
) -> String {
    let total = left.saturating_add(right);
    if width == 0 {
        return "[]".to_string();
    }
    if total == 0 {
        return format!("[{}]", "-".repeat(width));
    }
    let mut left_width = ((left as u128 * width as u128 + (total / 2) as u128) / total as u128)
        .min(width as u128) as usize;
    if left > 0 && left_width == 0 {
        left_width = 1;
    }
    if right > 0 && left_width == width {
        left_width = width - 1;
    }
    let right_width = width.saturating_sub(left_width);
    format!(
        "[{}{}]",
        left_char.to_string().repeat(left_width),
        right_char.to_string().repeat(right_width)
    )
}

fn payload_string(root: &BTreeMap<String, JsonValue>, key: &str, default: &str) -> String {
    root.get(key)
        .and_then(json_as_string)
        .unwrap_or(default)
        .to_string()
}

pub fn run_interactive(app: TuiApp) -> AppResult<()> {
    run_interactive_with_refresh(app, Duration::from_secs(1), |_| Ok(()))
}

pub fn run_interactive_with_refresh<F>(
    app: TuiApp,
    refresh_interval: Duration,
    refresh: F,
) -> AppResult<()>
where
    F: FnMut(&mut TuiApp) -> AppResult<()>,
{
    run_interactive_with_refresh_and_actions(app, refresh_interval, refresh, |_, _| Ok(()))
}

pub fn run_interactive_with_refresh_and_actions<F, A>(
    app: TuiApp,
    refresh_interval: Duration,
    refresh: F,
    action: A,
) -> AppResult<()>
where
    F: FnMut(&mut TuiApp) -> AppResult<()>,
    A: FnMut(&mut TuiApp, TuiAction) -> AppResult<()>,
{
    run_interactive_with_refresh_actions_and_live(
        app,
        refresh_interval,
        refresh,
        action,
        |_| Ok(()),
    )
}

pub fn run_interactive_with_refresh_actions_and_live<F, A, L>(
    mut app: TuiApp,
    refresh_interval: Duration,
    mut refresh: F,
    mut action: A,
    mut live: L,
) -> AppResult<()>
where
    F: FnMut(&mut TuiApp) -> AppResult<()>,
    A: FnMut(&mut TuiApp, TuiAction) -> AppResult<()>,
    L: FnMut(&mut TuiApp) -> AppResult<()>,
{
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_loop(
        &mut terminal,
        &mut app,
        refresh_interval,
        &mut refresh,
        &mut action,
        &mut live,
    );
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
    refresh_interval: Duration,
    refresh: &mut dyn FnMut(&mut TuiApp) -> AppResult<()>,
    action: &mut dyn FnMut(&mut TuiApp, TuiAction) -> AppResult<()>,
    live: &mut dyn FnMut(&mut TuiApp) -> AppResult<()>,
) -> AppResult<()> {
    let mut last_refresh = Instant::now();
    loop {
        if last_refresh.elapsed() >= refresh_interval {
            refresh(app)?;
            last_refresh = Instant::now();
            process_pending_actions(app, action, refresh, &mut last_refresh)?;
        }
        live(app)?;
        process_pending_actions(app, action, refresh, &mut last_refresh)?;
        terminal.draw(|frame| {
            app.last_frame_area = frame.area();
            draw(frame, app)
        })?;
        let poll_timeout = refresh_interval
            .saturating_sub(last_refresh.elapsed())
            .min(Duration::from_millis(200));
        if event::poll(poll_timeout)? {
            let keep_running = match event::read()? {
                Event::Key(key) => app.handle_key_event(key),
                Event::Mouse(mouse) => app.handle_mouse_event(mouse),
                _ => true,
            };
            if !keep_running {
                break;
            }
            process_pending_actions(app, action, refresh, &mut last_refresh)?;
        }
    }
    Ok(())
}

fn process_pending_actions(
    app: &mut TuiApp,
    action: &mut dyn FnMut(&mut TuiApp, TuiAction) -> AppResult<()>,
    refresh: &mut dyn FnMut(&mut TuiApp) -> AppResult<()>,
    last_refresh: &mut Instant,
) -> AppResult<()> {
    let actions = app.drain_actions();
    if actions.is_empty() {
        return Ok(());
    }
    for next_action in actions {
        action(app, next_action)?;
    }
    refresh(app)?;
    *last_refresh = Instant::now();
    Ok(())
}

pub fn render_once(app: &TuiApp, width: u16, height: u16) -> AppResult<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| draw(frame, app))?;
    Ok(format!("{}", terminal.backend()))
}

fn draw(frame: &mut Frame, app: &TuiApp) {
    let root = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(12),
        Constraint::Length(3),
    ])
    .split(frame.area());
    draw_tabs(frame, app, root[0]);
    draw_body(frame, app, root[1]);
    draw_status(frame, app, root[2]);
    if app.show_command_palette {
        draw_command_palette(frame, app);
    }
    if app.show_session_picker {
        draw_session_picker(frame, app);
    }
    if app.show_thread_picker {
        draw_thread_picker(frame, app);
    }
    if app.mcp_remove_confirmation.is_some() {
        draw_mcp_remove_confirmation_modal(frame, app);
    }
    if app.rollback_apply_confirmation.is_some() {
        draw_rollback_apply_confirmation_modal(frame, app);
    }
    if app.show_user_input_modal {
        draw_user_input_modal(frame, app);
    }
    if app.show_approval_modal {
        draw_approval_modal(frame, app);
    }
}

fn draw_tabs(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let tabs = ["Plan", "Agent", "YOLO"]
        .into_iter()
        .map(Line::from)
        .collect::<Tabs>()
        .select(app.mode.index())
        .highlight_style(
            Style::default()
                .fg(app.theme.accent_color())
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("DeepSeekCode TUI"),
        );
    frame.render_widget(tabs, area);
}

fn draw_body(frame: &mut Frame, app: &TuiApp, area: Rect) {
    if app.show_mcp_manager {
        draw_mcp_manager(frame, app, area);
        return;
    }

    let columns = body_columns(area);
    draw_sidebar(frame, app, columns[0]);
    draw_transcript(frame, app, columns[1]);
    draw_tasks(frame, app, columns[2]);
}

fn draw_sidebar(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let session = app.sessions.get(app.selected_session);
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Mode: ", Style::default().fg(app.theme.label_color())),
            Span::styled(
                app.mode.title(),
                Style::default().fg(app.theme.accent_color()),
            ),
        ]),
        Line::from(vec![
            Span::styled("Theme: ", Style::default().fg(app.theme.label_color())),
            Span::styled(
                app.theme.title(),
                Style::default().fg(app.theme.hint_color()),
            ),
        ]),
        Line::from(""),
        Line::from("Runtime session"),
    ];
    if let Some(session) = session {
        lines.push(Line::from(format!("title: {}", session.title)));
        lines.push(Line::from(format!("id: {}", session.id)));
        lines.push(Line::from(format!("status: {}", session.status)));
        lines.push(Line::from(format!("threads: {}", session.thread_count)));
        lines.push(Line::from(format!("workspace: {}", session.workspace)));
    }
    lines.push(Line::from(""));
    lines.push(Line::from("Keys"));
    lines.push(Line::from("Tab: mode"));
    lines.push(Line::from("i: composer"));
    lines.push(Line::from(": command palette"));
    lines.push(Line::from("s: session picker"));
    lines.push(Line::from("t: thread navigator"));
    lines.push(Line::from("!: approval modal"));
    lines.push(Line::from("c: cancel run"));
    if app.show_mcp_manager {
        lines.push(Line::from("PgUp/PgDn: scroll MCP manager"));
        lines.push(Line::from("Esc: close MCP manager"));
    } else if app.mcp_detail.is_some() {
        lines.push(Line::from("PgUp/PgDn: scroll detail"));
        lines.push(Line::from("Esc: close detail"));
    } else {
        lines.push(Line::from("PgUp/PgDn: scroll"));
    }
    lines.push(Line::from("q: quit"));

    let sidebar = Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title("Sidebar"));
    frame.render_widget(sidebar, area);
}

fn draw_transcript(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let composer_marker = if app.composer_focused { "*" } else { "" };
    let composer = display_with_cursor(&app.composer, app.composer_cursor, app.composer_focused);
    let mut lines = app
        .transcript
        .iter()
        .map(|line| Line::from(line.as_str()))
        .chain(std::iter::once(Line::from("")))
        .chain(std::iter::once(Line::from(format!(
            "Composer [{}]{}: {}",
            app.mode.title(),
            composer_marker,
            clip_line(&composer, 100)
        ))))
        .collect::<Vec<_>>();
    if let Some(hint) = composer_slash_hint_line(app) {
        lines.push(Line::from(vec![Span::styled(
            clip_line(&hint, 120),
            Style::default().fg(Color::Gray),
        )]));
    }
    let visible_lines = usize::from(area.height.saturating_sub(2)).max(1);
    let max_top = lines.len().saturating_sub(visible_lines);
    let scroll = app.transcript_scroll.min(max_top);
    let top = max_top.saturating_sub(scroll);
    let transcript = Paragraph::new(lines)
        .scroll((top.min(usize::from(u16::MAX)) as u16, 0))
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title("Transcript"));
    frame.render_widget(transcript, area);
}

fn draw_tasks(frame: &mut Frame, app: &TuiApp, area: Rect) {
    if let Some((kind, detail)) = app.mcp_detail.as_ref() {
        let visible_lines = usize::from(area.height.saturating_sub(2)).max(1);
        let max_top = detail.lines().count().saturating_sub(visible_lines);
        let scroll = app.mcp_detail_scroll.min(max_top);
        let detail = Paragraph::new(detail.as_str())
            .scroll((scroll.min(usize::from(u16::MAX)) as u16, 0))
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title(kind.title()));
        frame.render_widget(detail, area);
        return;
    }

    let items = app
        .tasks
        .iter()
        .map(|task| ListItem::new(task.as_str()))
        .collect::<Vec<_>>();
    let tasks =
        List::new(items).block(Block::default().borders(Borders::ALL).title("Plan / Tasks"));
    frame.render_widget(tasks, area);
}

fn draw_mcp_manager(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let (kind, title, detail) = app
        .mcp_detail
        .as_ref()
        .map(|(kind, detail)| (kind, kind.title(), detail.as_str()))
        .unwrap_or((
            &TuiMcpDetailKind::Manager,
            "MCP Manager",
            "MCP manager is loading",
        ));
    let rendered = render_mcp_manager_body(
        kind,
        detail,
        &app.mcp_manager_filter,
        app.mcp_manager_selected_server,
        app.mcp_manager_selected_server_keys.len(),
    );
    let visible_lines = usize::from(area.height.saturating_sub(2)).max(1);
    let max_top = rendered.lines().count().saturating_sub(visible_lines);
    let scroll = app.mcp_detail_scroll.min(max_top);
    let manager = Paragraph::new(rendered)
        .scroll((scroll.min(usize::from(u16::MAX)) as u16, 0))
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(manager, area);
}

fn render_mcp_manager_body(
    kind: &TuiMcpDetailKind,
    detail: &str,
    filter: &str,
    selected_server: usize,
    selected_server_count: usize,
) -> String {
    let filter = filter.trim();
    let filtered = filter_mcp_manager_detail(detail, filter);
    let filter_label = if filter.is_empty() {
        "Filter: none (:mcp manager filter <query>)".to_string()
    } else {
        format!("Filter: {filter} (:mcp manager filter to clear)")
    };
    let server_actions =
        render_mcp_manager_server_actions(detail, selected_server, selected_server_count);
    format!(
        "{}\n{}\n{}\n\n{}",
        render_mcp_manager_tabs(kind),
        filter_label,
        server_actions,
        filtered
    )
}

fn render_mcp_manager_tabs(active: &TuiMcpDetailKind) -> String {
    mcp_manager_tab_specs()
        .iter()
        .map(|(kind, label)| {
            if kind == active {
                format!("[{label}]")
            } else {
                label.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("  ")
}

fn mcp_manager_tab_specs() -> [(TuiMcpDetailKind, &'static str); 6] {
    [
        (TuiMcpDetailKind::Manager, "overview"),
        (TuiMcpDetailKind::Tools, "tools"),
        (TuiMcpDetailKind::Prompts, "prompts"),
        (TuiMcpDetailKind::Resources, "resources"),
        (TuiMcpDetailKind::ResourceTemplates, "templates"),
        (TuiMcpDetailKind::Health, "health"),
    ]
}

fn mcp_manager_tab_at_column(
    active: TuiMcpDetailKind,
    column_offset: usize,
) -> Option<TuiMcpDetailKind> {
    let mut cursor = 0usize;
    for (kind, label) in mcp_manager_tab_specs() {
        let rendered = if kind == active {
            format!("[{label}]")
        } else {
            label.to_string()
        };
        let width = rendered.chars().count();
        if (cursor..cursor.saturating_add(width)).contains(&column_offset) {
            return Some(kind);
        }
        cursor = cursor.saturating_add(width).saturating_add(2);
    }
    None
}

fn render_mcp_manager_server_actions(
    detail: &str,
    selected_server: usize,
    selected_server_count: usize,
) -> String {
    let servers = parse_mcp_manager_server_entries(detail);
    if servers.is_empty() {
        return "Server actions: none".to_string();
    }
    let selected = selected_server.min(servers.len() - 1);
    let server = &servers[selected];
    let state = if server.enabled {
        "enabled"
    } else {
        "disabled"
    };
    format!(
        "Server actions: {}/{} {} ({}, {state}) | n/p select | selected={} | Space | A all | U clear | e enable | d disable | E/D bulk | x remove | t tools | r reload",
        selected + 1,
        servers.len(),
        server.name,
        server.source,
        selected_server_count
    )
}

fn mcp_manager_action_at_column(
    line: &str,
    column_offset: usize,
) -> Option<TuiMcpManagerMouseAction> {
    [
        ("e enable", TuiMcpManagerMouseAction::Enable),
        ("d disable", TuiMcpManagerMouseAction::Disable),
        ("x remove", TuiMcpManagerMouseAction::Remove),
        ("t tools", TuiMcpManagerMouseAction::Tools),
        ("r reload", TuiMcpManagerMouseAction::Reload),
    ]
    .into_iter()
    .find_map(|(needle, action)| {
        let byte_start = line.find(needle)?;
        let start = line[..byte_start].chars().count();
        let end = start.saturating_add(needle.chars().count());
        if (start..end).contains(&column_offset) {
            Some(action)
        } else {
            None
        }
    })
}

fn parse_mcp_manager_server_entries(detail: &str) -> Vec<TuiMcpServerEntry> {
    detail
        .lines()
        .filter_map(parse_mcp_manager_server_entry)
        .collect()
}

fn parse_mcp_manager_server_entry(line: &str) -> Option<TuiMcpServerEntry> {
    let rest = line.trim().strip_prefix("- ")?;
    let source = rest.split("source=").nth(1)?;
    let source = source.split([',', ')']).next()?.trim();
    if source.is_empty() {
        return None;
    }
    let name = rest.split_whitespace().next()?.trim();
    if name.is_empty() {
        return None;
    }
    Some(TuiMcpServerEntry {
        name: name.to_string(),
        source: source.to_string(),
        enabled: rest.contains("[enabled "),
    })
}

fn filter_mcp_manager_detail(detail: &str, filter: &str) -> String {
    if filter.is_empty() {
        return detail.to_string();
    }
    let needle = filter.to_ascii_lowercase();
    let lines = detail
        .lines()
        .filter(|line| line.to_ascii_lowercase().contains(&needle))
        .collect::<Vec<_>>();
    if lines.is_empty() {
        format!("No MCP manager lines match filter: {filter}")
    } else {
        lines.join("\n")
    }
}

fn draw_status(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let status = Paragraph::new(vec![Line::from(vec![
        Span::styled("Status: ", Style::default().fg(app.theme.label_color())),
        Span::raw(app.status.as_str()),
        Span::raw(" | "),
        Span::styled("Palette", Style::default().fg(app.theme.hint_color())),
        Span::raw(" : "),
        Span::styled("Sessions", Style::default().fg(app.theme.hint_color())),
        Span::raw(" s "),
        Span::styled("Threads", Style::default().fg(app.theme.hint_color())),
        Span::raw(" t "),
        Span::styled("Approval", Style::default().fg(app.theme.hint_color())),
        Span::raw(" !"),
        Span::raw(" "),
        Span::styled("Cancel", Style::default().fg(app.theme.hint_color())),
        Span::raw(" c"),
    ])])
    .block(Block::default().borders(Borders::ALL).title("Command Bar"));
    frame.render_widget(status, area);
}

fn draw_session_picker(frame: &mut Frame, app: &TuiApp) {
    let area = session_picker_rect(frame.area());
    frame.render_widget(Clear, area);
    let filter = app.session_picker_filter.trim();
    let session_indices = app.filtered_session_indices();
    let mut items = session_indices
        .iter()
        .map(|index| {
            let session = &app.sessions[*index];
            let prefix = if *index == app.selected_session {
                "> "
            } else {
                "  "
            };
            ListItem::new(format!(
                "{prefix}{} | {} | threads={}",
                session.title, session.status, session.thread_count
            ))
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        items.push(ListItem::new(if filter.is_empty() {
            "No durable sessions"
        } else {
            "No sessions match current filter"
        }));
    }
    if !filter.is_empty() {
        items.insert(
            0,
            ListItem::new(format!(
                "Filter: {filter} ({} match)",
                session_indices.len()
            )),
        );
    }
    let picker = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Session Picker"),
    );
    frame.render_widget(picker, area);
}

fn draw_thread_picker(frame: &mut Frame, app: &TuiApp) {
    let area = thread_picker_rect(frame.area());
    frame.render_widget(Clear, area);
    let filter = app.thread_picker_filter.trim();
    let threads = app.filtered_threads_for_selected_session();
    let thread_match_count = threads.len();
    let mut items = threads
        .into_iter()
        .map(|thread| {
            let prefix = if app.selected_thread_id.as_deref() == Some(thread.id.as_str()) {
                "> "
            } else {
                "  "
            };
            ListItem::new(format!(
                "{prefix}{} | {} | seq={}",
                thread.title, thread.status, thread.event_seq
            ))
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        items.push(ListItem::new(if filter.is_empty() {
            "No durable threads in selected session"
        } else {
            "No threads match current filter"
        }));
    }
    if !filter.is_empty() {
        items.insert(
            0,
            ListItem::new(format!("Filter: {filter} ({thread_match_count} match)")),
        );
    }
    let picker = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Thread Navigator"),
    );
    frame.render_widget(picker, area);
}

fn draw_command_palette(frame: &mut Frame, app: &TuiApp) {
    let area = top_center_rect(frame.area(), 76, 8);
    frame.render_widget(Clear, area);
    let command_query = display_with_cursor(&app.command_query, app.command_cursor, true);
    let palette = Paragraph::new(vec![
        Line::from("Command Palette"),
        Line::from("Examples: mode agent | task pause | shell cargo test | revert turn last"),
        Line::from(""),
        Line::from(vec![
            Span::styled("> ", Style::default().fg(app.theme.accent_color())),
            Span::raw(command_query),
        ]),
    ])
    .wrap(Wrap { trim: true })
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Command Palette"),
    );
    frame.render_widget(palette, area);
}

fn draw_approval_modal(frame: &mut Frame, app: &TuiApp) {
    let area = bottom_center_rect(frame.area(), 68, 11);
    frame.render_widget(Clear, area);
    let lines = if let Some(command) = app.pending_shell_approval.as_deref() {
        vec![
            Line::from("Shell Approval Required"),
            Line::from("Tool: foreground shell"),
            Line::from("Kind: shell"),
            Line::from(format!("Target: {}", clip_line(command, 58))),
            Line::from("Source: TUI command palette"),
            Line::from(""),
            Line::from("[y] run once    [n] deny    [Esc] close"),
        ]
    } else if let Some(approval) = app.active_approval() {
        vec![
            Line::from("Approval Required"),
            Line::from(format!("Tool: {}", clip_line(&approval.tool, 48))),
            Line::from(format!("Kind: {}", clip_line(&approval.kind, 48))),
            Line::from(format!("Target: {}", clip_line(&approval.target, 58))),
            Line::from(format!("Thread: {}", approval.thread_id)),
            Line::from(""),
            Line::from("[y] approve    [n] deny    [c] cancel run"),
        ]
    } else {
        vec![
            Line::from("No Pending Approval"),
            Line::from("Runtime has no pending permission_request events."),
            Line::from(""),
            Line::from("[Esc] close"),
        ]
    };
    let modal = Paragraph::new(lines).alignment(Alignment::Center).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Approval Modal"),
    );
    frame.render_widget(modal, area);
}

fn draw_user_input_modal(frame: &mut Frame, app: &TuiApp) {
    let area = bottom_center_rect(frame.area(), 76, 14);
    frame.render_widget(Clear, area);
    let lines = if let Some(request) = app.active_user_input() {
        let question = request
            .questions
            .get(app.user_input_question_index)
            .or_else(|| request.questions.first());
        let mut lines = vec![
            Line::from("Input Required"),
            Line::from(format!("Thread: {}", request.thread_id)),
            Line::from(format!(
                "Question {}/{}",
                app.user_input_question_index + 1,
                request.questions.len()
            )),
            Line::from(""),
        ];
        if let Some(question) = question {
            lines.push(Line::from(clip_line(&question.header, 64)));
            lines.push(Line::from(clip_line(&question.question, 68)));
            lines.push(Line::from(""));
            for (index, option) in question.options.iter().enumerate() {
                lines.push(Line::from(format!(
                    "[{}] {} - {}",
                    index + 1,
                    clip_line(&option.label, 18),
                    clip_line(&option.description, 42)
                )));
            }
        }
        lines.push(Line::from(""));
        if app.user_input_other_mode {
            let draft = if app.user_input_other_value.is_empty() {
                "<empty>".to_string()
            } else {
                clip_line(&app.user_input_other_value, 58)
            };
            lines.push(Line::from(format!("Other: {draft}")));
            lines.push(Line::from("[Enter] submit other    [Esc] cancel other"));
        } else {
            lines.push(Line::from(
                "[1-3] choose option    [o] Other    [Esc] dismiss",
            ));
        }
        lines
    } else {
        vec![
            Line::from("No Pending Input"),
            Line::from("Runtime has no pending user_input_request events."),
            Line::from(""),
            Line::from("[Esc] close"),
        ]
    };
    let modal = Paragraph::new(lines).alignment(Alignment::Center).block(
        Block::default()
            .borders(Borders::ALL)
            .title("User Input Modal"),
    );
    frame.render_widget(modal, area);
}

fn draw_mcp_remove_confirmation_modal(frame: &mut Frame, app: &TuiApp) {
    let area = bottom_center_rect(frame.area(), 64, 9);
    frame.render_widget(Clear, area);
    let lines = if let Some(remove) = app.mcp_remove_confirmation.as_ref() {
        vec![
            Line::from("Remove MCP Server?"),
            Line::from(format!("Name: {}", clip_line(&remove.name, 48))),
            Line::from(format!("Scope: {}", remove.scope.label())),
            Line::from(""),
            Line::from("This removes the server from MCP config."),
            Line::from("[y] remove    [Enter] remove    [n/Esc] cancel"),
        ]
    } else {
        vec![
            Line::from("No MCP removal pending"),
            Line::from(""),
            Line::from("[Esc] close"),
        ]
    };
    let modal = Paragraph::new(lines).alignment(Alignment::Center).block(
        Block::default()
            .borders(Borders::ALL)
            .title("MCP Remove Confirmation"),
    );
    frame.render_widget(modal, area);
}

fn draw_rollback_apply_confirmation_modal(frame: &mut Frame, app: &TuiApp) {
    let area = bottom_center_rect(frame.area(), 72, 10);
    frame.render_widget(Clear, area);
    let lines = if let Some(pending) = app.rollback_apply_confirmation.as_ref() {
        vec![
            Line::from("Apply Rollback?"),
            Line::from(format!("Target: {}", clip_line(&pending.id, 52))),
            Line::from(
                pending
                    .hunk
                    .map(|hunk| format!("Hunk: #{hunk}"))
                    .unwrap_or_else(|| "Scope: full snapshot".to_string()),
            ),
            Line::from(""),
            Line::from(if pending.hunk.is_some() {
                "This will apply only the selected rollback hunk."
            } else {
                "This will restore files in the local git worktree."
            }),
            Line::from("Run without --apply first to preview the restore plan."),
            Line::from(""),
            Line::from("[y] apply    [Enter] apply    [n/Esc] cancel"),
        ]
    } else {
        vec![
            Line::from("No rollback apply pending"),
            Line::from(""),
            Line::from("[Esc] close"),
        ]
    };
    let modal = Paragraph::new(lines).alignment(Alignment::Center).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Rollback Apply Confirmation"),
    );
    frame.render_widget(modal, area);
}

fn top_center_rect(area: Rect, width: u16, height: u16) -> Rect {
    fixed_rect(
        area,
        width,
        height,
        area.x + area.width.saturating_sub(width.min(area.width)) / 2,
        area.y + 3,
    )
}

fn body_columns(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::horizontal([
        Constraint::Length(32),
        Constraint::Min(36),
        Constraint::Length(32),
    ])
    .split(area)
}

fn session_picker_rect(area: Rect) -> Rect {
    left_middle_rect(area, 42, 18)
}

fn thread_picker_rect(area: Rect) -> Rect {
    right_middle_rect(area, 52, 20)
}

fn bottom_center_rect(area: Rect, width: u16, height: u16) -> Rect {
    let height = height.min(area.height);
    fixed_rect(
        area,
        width,
        height,
        area.x + area.width.saturating_sub(width.min(area.width)) / 2,
        area.y + area.height.saturating_sub(height + 2),
    )
}

fn left_middle_rect(area: Rect, width: u16, height: u16) -> Rect {
    let height = height.min(area.height);
    fixed_rect(
        area,
        width,
        height,
        area.x + 2,
        area.y + area.height.saturating_sub(height) / 2,
    )
}

fn right_middle_rect(area: Rect, width: u16, height: u16) -> Rect {
    let height = height.min(area.height);
    let width = width.min(area.width.saturating_sub(2)).max(1);
    fixed_rect(
        area,
        width,
        height,
        area.x + area.width.saturating_sub(width + 2),
        area.y + area.height.saturating_sub(height) / 2,
    )
}

fn fixed_rect(area: Rect, width: u16, height: u16, x: u16, y: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(1);
    let height = height.min(area.height.saturating_sub(2)).max(1);
    let max_x = area.x + area.width.saturating_sub(width);
    let max_y = area.y + area.height.saturating_sub(height);
    Rect::new(x.min(max_x), y.min(max_y), width, height)
}

fn point_in_rect(column: u16, row: u16, area: Rect) -> bool {
    column >= area.x
        && column < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

fn block_row_index(column: u16, row: u16, area: Rect) -> Option<usize> {
    if !point_in_rect(column, row, area) {
        return None;
    }
    let inner_top = area.y.saturating_add(1);
    let inner_bottom = area.y.saturating_add(area.height).saturating_sub(1);
    if row < inner_top || row >= inner_bottom {
        return None;
    }
    Some(usize::from(row.saturating_sub(inner_top)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_palette_command(app: &mut TuiApp, command: &str) {
        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in command.chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));
    }

    fn temp_root(label: &str) -> PathBuf {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "deepseek-tui-{label}-{}-{suffix}",
            std::process::id()
        ))
    }

    fn left_click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn ctrl_left_click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::CONTROL,
        }
    }

    fn left_drag(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn left_release(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn scroll_down(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn runtime_task(id: &str, status: &str, summary: &str, updated_at: &str) -> TuiTaskRecord {
        TuiTaskRecord {
            id: id.to_string(),
            session_id: Some("session-one".to_string()),
            thread_id: Some("thread-one".to_string()),
            parent_task_id: None,
            kind: "agent".to_string(),
            status: status.to_string(),
            summary: summary.to_string(),
            updated_at: updated_at.to_string(),
        }
    }

    fn app_with_runtime_tasks(task_records: Vec<TuiTaskRecord>) -> TuiApp {
        TuiApp::with_runtime_usage_tasks_automations_and_approvals(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
            task_records,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    #[test]
    fn mode_cycles_through_plan_agent_yolo() {
        assert_eq!(TuiMode::Plan.next(), TuiMode::Agent);
        assert_eq!(TuiMode::Agent.next(), TuiMode::Yolo);
        assert_eq!(TuiMode::Yolo.next(), TuiMode::Plan);
    }

    #[test]
    fn mouse_clicks_switch_mode_tabs() {
        let mut app = TuiApp::new(Vec::new());
        app.last_frame_area = Rect::new(0, 0, 120, 36);

        assert!(app.handle_mouse_event(left_click(48, 1)));
        assert_eq!(app.mode, TuiMode::Agent);
        assert_eq!(app.status, "mode set: Agent");

        assert!(app.handle_mouse_event(left_click(92, 1)));
        assert_eq!(app.mode, TuiMode::Yolo);

        assert!(app.handle_mouse_event(left_click(4, 1)));
        assert_eq!(app.mode, TuiMode::Plan);
    }

    #[test]
    fn exit_command_quits_from_palette_and_composer() {
        let mut app = TuiApp::new(Vec::new());

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "exit".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(!app.handle_key(KeyCode::Enter));

        let mut app = TuiApp::new(Vec::new());
        app.composer_focused = true;
        app.composer = "/quit".to_string();
        app.composer_cursor = app.composer.len();
        assert!(!app.handle_key(KeyCode::Enter));

        let mut app = TuiApp::new(Vec::new());
        app.composer_focused = true;
        app.composer = "/exit now".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        assert_eq!(app.status, "usage: exit, quit, q, /exit, /quit, or /q");
    }

    #[test]
    fn mode_command_shows_and_switches_modes() {
        let mut app = TuiApp::new(Vec::new());

        run_palette_command(&mut app, "/mode");

        assert_eq!(app.status, "mode options shown");
        let (kind, detail) = app.mcp_detail.as_ref().expect("mode detail");
        assert_eq!(*kind, TuiMcpDetailKind::Mode);
        assert!(detail.contains("DeepSeekCode Mode"));
        assert!(detail.contains("/mode agent"));

        run_palette_command(&mut app, "mode 1");

        assert_eq!(app.mode, TuiMode::Agent);
        assert_eq!(app.status, "mode set: Agent");

        app.composer_focused = true;
        app.composer = "/mode 3".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(app.mode, TuiMode::Yolo);
        assert_eq!(app.status, "mode set: YOLO");
        assert_eq!(app.composer, "");
    }

    #[test]
    fn theme_command_switches_and_renders_theme_state() {
        let mut app = TuiApp::new(Vec::new());

        run_palette_command(&mut app, "/theme show");

        assert_eq!(app.status, "theme options shown");
        let (kind, detail) = app.mcp_detail.as_ref().expect("theme detail");
        assert_eq!(*kind, TuiMcpDetailKind::Theme);
        assert!(detail.contains("DeepSeekCode Theme"));
        assert!(detail.contains("Current:"));

        run_palette_command(&mut app, "theme light");

        assert_eq!(app.theme, TuiTheme::Light);
        assert_eq!(app.status, "theme switched: Light");
        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("Theme: Light"));

        app.composer_focused = true;
        app.composer = "/theme grayscale".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(app.theme, TuiTheme::Grayscale);
        assert_eq!(app.status, "theme switched: Grayscale");
        assert_eq!(app.composer, "");
    }

    #[test]
    fn statusline_command_renders_command_bar_detail() {
        let mut app = TuiApp::new(Vec::new());
        app.status = "working".to_string();

        run_palette_command(&mut app, "/statusline");

        assert_eq!(app.status, "statusline shown");
        let (kind, detail) = app.mcp_detail.as_ref().expect("statusline detail");
        assert_eq!(*kind, TuiMcpDetailKind::StatusLine);
        assert!(detail.contains("DeepSeekCode Statusline"));
        assert!(detail.contains("Palette: `:`"));
        assert!(detail.contains("/settings"));

        app.composer_focused = true;
        app.composer = "/statusline show".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(app.status, "statusline shown");
        assert_eq!(app.composer, "");
        let (kind, detail) = app.mcp_detail.as_ref().expect("composer statusline detail");
        assert_eq!(*kind, TuiMcpDetailKind::StatusLine);
        assert!(detail.contains("fixed command bar"));
    }

    #[test]
    fn verbose_command_toggles_reasoning_transcript_detail() {
        let reasoning = "reasoning preview starts with enough filler words before suffix padding padding padding UNIQUE_VERBOSE_SUFFIX marker";
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: Some("turn-one".to_string()),
                event_seq: 1,
            }],
            vec![
                TuiItem {
                    id: "item-one".to_string(),
                    thread_id: "thread-one".to_string(),
                    turn_id: Some("turn-one".to_string()),
                    index: 1,
                    item_type: "message".to_string(),
                    role: Some("assistant".to_string()),
                    content: "final answer".to_string(),
                    status: "completed".to_string(),
                },
                TuiItem {
                    id: "reasoning-one".to_string(),
                    thread_id: "thread-one".to_string(),
                    turn_id: Some("turn-one".to_string()),
                    index: 2,
                    item_type: "reasoning".to_string(),
                    role: Some("assistant".to_string()),
                    content: reasoning.to_string(),
                    status: "running".to_string(),
                },
            ],
        );

        assert!(!app.verbose_transcript);
        assert!(app
            .transcript
            .iter()
            .any(|line| line.contains("thinking compact")));
        assert!(!app
            .transcript
            .iter()
            .any(|line| line.contains("UNIQUE_VERBOSE_SUFFIX")));

        run_palette_command(&mut app, "verbose on");

        assert!(app.verbose_transcript);
        assert_eq!(app.status, "verbose transcript on");
        assert!(app
            .transcript
            .iter()
            .any(|line| line.contains("UNIQUE_VERBOSE_SUFFIX")));
        let (kind, detail) = app.mcp_detail.as_ref().expect("verbose detail");
        assert_eq!(*kind, TuiMcpDetailKind::Verbose);
        assert!(detail.contains("DeepSeekCode Verbose Transcript"));
        assert!(detail.contains("Current:"));

        app.composer_focused = true;
        app.composer = "/verbose off".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));

        assert!(!app.verbose_transcript);
        assert_eq!(app.status, "verbose transcript off");
        assert_eq!(app.composer, "");
        assert!(app
            .transcript
            .iter()
            .any(|line| line.contains("thinking compact")));
        assert!(!app
            .transcript
            .iter()
            .any(|line| line.contains("UNIQUE_VERBOSE_SUFFIX")));
    }

    #[test]
    fn context_command_renders_active_context_inspector() {
        let mut app = TuiApp::with_runtime_usage_and_approvals(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: Some("turn-one".to_string()),
                event_seq: 7,
            }],
            vec![
                TuiItem {
                    id: "item-one".to_string(),
                    thread_id: "thread-one".to_string(),
                    turn_id: Some("turn-one".to_string()),
                    index: 1,
                    item_type: "message".to_string(),
                    role: Some("assistant".to_string()),
                    content: "answer".to_string(),
                    status: "completed".to_string(),
                },
                TuiItem {
                    id: "reasoning-one".to_string(),
                    thread_id: "thread-one".to_string(),
                    turn_id: Some("turn-one".to_string()),
                    index: 2,
                    item_type: "reasoning".to_string(),
                    role: Some("assistant".to_string()),
                    content: "hidden planning".to_string(),
                    status: "completed".to_string(),
                },
            ],
            vec![TuiUsageSummary {
                thread_id: "thread-one".to_string(),
                record_count: 2,
                prompt_tokens: 800,
                completion_tokens: 200,
                total_tokens: 1000,
                latest_prompt_tokens: 500,
                latest_completion_tokens: 125,
                latest_total_tokens: 625,
                prompt_cache_hit_tokens: 300,
                prompt_cache_miss_tokens: 200,
                estimated_input_cost_microusd: Some(100),
                estimated_output_cost_microusd: Some(200),
                estimated_total_cost_microusd: Some(300),
                context_remaining_tokens: TUI_CONTEXT_WINDOW_TOKENS - 625,
                context_strategy: "normal".to_string(),
            }],
            Vec::new(),
        );

        run_palette_command(&mut app, "context");

        assert_eq!(app.status, "context inspector shown");
        let (kind, detail) = app.mcp_detail.as_ref().expect("context detail");
        assert_eq!(*kind, TuiMcpDetailKind::Context);
        assert!(detail.contains("DeepSeekCode Context"));
        assert!(detail.contains("Context window:"));
        assert!(detail.contains("Latest tokens:"));
        assert!(detail.contains("Cache hit/miss:"));
        assert!(detail.contains("Reasoning replay:"));

        app.composer_focused = true;
        app.composer = "/ctx".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(app.status, "context inspector shown");
        assert_eq!(app.composer, "");
        let (kind, detail) = app.mcp_detail.as_ref().expect("ctx detail");
        assert_eq!(*kind, TuiMcpDetailKind::Context);
        assert!(detail.contains("Aliases: context, ctx"));
    }

    #[test]
    fn help_command_renders_index_and_topics() {
        let mut app = TuiApp::new(Vec::new());

        run_palette_command(&mut app, "/help");

        assert_eq!(app.status, "help shown");
        let (kind, detail) = app.mcp_detail.as_ref().expect("help detail");
        assert_eq!(*kind, TuiMcpDetailKind::Help);
        assert!(detail.contains("DeepSeekCode Help"));
        assert!(detail.contains("/mode"));
        assert!(detail.contains("/links"));

        run_palette_command(&mut app, "help mode");

        assert_eq!(app.status, "help shown: mode");
        let (_, detail) = app.mcp_detail.as_ref().expect("mode help detail");
        assert!(detail.contains("Usage: /mode [agent|plan|yolo|1|2|3]"));
        assert!(detail.contains("Show or switch"));

        app.composer_focused = true;
        app.composer = "/? links".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(app.status, "help shown: links");
        assert_eq!(app.composer, "");
        let (_, detail) = app.mcp_detail.as_ref().expect("links help detail");
        assert!(detail.contains("Aliases: dashboard, api"));
    }

    #[test]
    fn settings_command_renders_configuration_entry_points() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: "/tmp/deepseek-settings".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "running".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        run_palette_command(&mut app, "settings");

        assert_eq!(app.status, "settings shown");
        let (kind, detail) = app.mcp_detail.as_ref().expect("settings detail");
        assert_eq!(*kind, TuiMcpDetailKind::Settings);
        assert!(detail.contains("DeepSeekCode Settings"));
        assert!(detail.contains("/tmp/deepseek-settings/.dscode/config.toml"));
        assert!(detail.contains("/provider [name [model]|list]"));
        assert!(detail.contains("/mcp manager"));

        app.composer_focused = true;
        app.composer = "/config".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(app.status, "settings shown");
        assert_eq!(app.composer, "");
        let (kind, detail) = app.mcp_detail.as_ref().expect("composer settings detail");
        assert_eq!(*kind, TuiMcpDetailKind::Settings);
        assert!(detail.contains("Settings are edited through focused commands"));
    }

    #[test]
    fn mouse_click_transcript_focuses_composer() {
        let mut app = TuiApp::new(Vec::new());
        app.last_frame_area = Rect::new(0, 0, 120, 36);
        let (_, body, _) = app.frame_layout().unwrap();
        let columns = body_columns(body);

        assert!(!app.composer_focused);
        assert!(app.handle_mouse_event(left_click(columns[1].x + 2, columns[1].y + 2)));

        assert!(app.composer_focused);
        assert_eq!(app.status, "composer focused");
    }

    #[test]
    fn render_demo_contains_core_surfaces() {
        let app = TuiApp::demo();
        let output = render_once(&app, 120, 36).unwrap();

        assert!(output.contains("DeepSeekCode TUI"));
        assert!(output.contains("Plan"));
        assert!(output.contains("Agent"));
        assert!(output.contains("YOLO"));
        assert!(output.contains("Sidebar"));
        assert!(output.contains("Session Picker"));
        assert!(output.contains("Thread Navigator"));
        assert!(output.contains("Command Palette"));
        assert!(output.contains("Approval Modal"));
    }

    #[test]
    fn render_mcp_detail_uses_right_side_panel() {
        let mut app = TuiApp::new(Vec::new());
        app.set_mcp_detail(
            TuiMcpDetailKind::Tools,
            "MCP remote tools:\n- remote [http]: 1 tool(s)\n  - search_code: Search code",
        );

        let output = render_once(&app, 120, 36).unwrap();

        assert!(output.contains("MCP Tools"));
        assert!(output.contains("MCP remote tools"));
        assert!(output.contains("search_code"));
        assert!(!output.contains("Plan / Tasks"));
    }

    #[test]
    fn render_mcp_manager_uses_full_body_screen() {
        let mut app = TuiApp::new(Vec::new());
        app.set_mcp_manager(
            "MCP Manager\nmcp servers=1 enabled=1 [remote:http:on]\n\nMCP servers:\n- remote [enabled http] http://127.0.0.1:3000/mcp",
        );

        let output = render_once(&app, 120, 36).unwrap();

        assert!(output.contains("MCP Manager"));
        assert!(output.contains("[overview]"));
        assert!(output.contains("tools"));
        assert!(output.contains("Filter: none"));
        assert!(output.contains("remote:http:on"));
        assert!(output.contains("MCP servers"));
        assert!(!output.contains("Transcript"));
        assert!(!output.contains("Plan / Tasks"));
    }

    #[test]
    fn render_mcp_manager_filters_detail_lines() {
        let mut app = TuiApp::new(Vec::new());
        app.set_mcp_manager(
            "MCP Manager\nmcp servers=2 enabled=1 [alpha:http:on] [beta:stdio:off]\n\nMCP servers:\n- alpha [enabled http]\n- beta [disabled stdio]",
        );
        app.set_mcp_manager_filter("beta");

        let output = render_once(&app, 120, 36).unwrap();

        assert!(output.contains("Filter: beta"));
        assert!(output.contains("beta:stdio:off"));
        assert!(output.contains("- beta [disabled stdio]"));
        assert!(!output.contains("- alpha [enabled http]"));
    }

    #[test]
    fn mcp_manager_server_action_strip_renders_selection() {
        let mut app = TuiApp::new(Vec::new());
        app.set_mcp_manager(
            "MCP Manager\nmcp servers=2 enabled=1 [alpha:http:on] [beta:stdio:off]\n\nMCP servers:\n- alpha [enabled http] http://127.0.0.1:3000/mcp (source=project, env=-)\n- beta [disabled stdio] npx -y @mcp/server . (source=user, env=TOKEN)",
        );

        let output = render_once(&app, 160, 36).unwrap();

        assert!(output.contains("Server actions: 1/2 alpha (project, enabled)"));
        assert!(output.contains("n/p select"));
        assert!(output.contains("e enable"));
        assert!(output.contains("x remove"));
    }

    #[test]
    fn mcp_manager_keyboard_cycles_tabs_and_reloads() {
        let mut app = TuiApp::new(Vec::new());
        app.set_mcp_manager("MCP Manager\nmcp servers=1 enabled=1 [remote:http:on]");

        assert!(app.handle_key(KeyCode::Tab));
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpManagerDetails {
                kind: TuiMcpDetailKind::Tools,
                server: None,
            }]
        );
        assert!(app.status.contains("mcp manager tools detail requested"));

        app.set_mcp_manager_detail(TuiMcpDetailKind::Tools, "MCP remote tools:\n- search");
        assert!(app.handle_key(KeyCode::BackTab));
        assert_eq!(app.drain_actions(), vec![TuiAction::McpManager]);
        assert!(app.status.contains("mcp manager requested"));

        app.set_mcp_manager_detail(TuiMcpDetailKind::Health, "MCP health:\n- remote ok");
        assert!(app.handle_key(KeyCode::Tab));
        assert_eq!(app.drain_actions(), vec![TuiAction::McpManager]);

        app.set_mcp_manager("MCP Manager\nmcp servers=1 enabled=1 [remote:http:on]");
        assert!(app.handle_key(KeyCode::Char('r')));
        assert_eq!(app.drain_actions(), vec![TuiAction::McpList]);
        assert_eq!(app.status, "mcp manager reload requested");
    }

    #[test]
    fn mcp_manager_keyboard_actions_target_selected_server() {
        let mut app = TuiApp::new(Vec::new());
        app.set_mcp_manager(
            "MCP Manager\nmcp servers=2 enabled=1 [alpha:http:on] [beta:stdio:off]\n\nMCP servers:\n- alpha [enabled http] http://127.0.0.1:3000/mcp (source=project, env=-)\n- beta [disabled stdio] npx -y @mcp/server . (source=user, env=TOKEN)",
        );

        assert!(app.handle_key(KeyCode::Char('n')));
        assert!(app.status.contains("beta (user, disabled)"));

        assert!(app.handle_key(KeyCode::Char('e')));
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpSetEnabled {
                scope: TuiMcpConfigScope::User,
                name: "beta".to_string(),
                enabled: true,
            }]
        );

        assert!(app.handle_key(KeyCode::Char('p')));
        assert!(app.status.contains("alpha (project, enabled)"));

        assert!(app.handle_key(KeyCode::Char('d')));
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpSetEnabled {
                scope: TuiMcpConfigScope::Project,
                name: "alpha".to_string(),
                enabled: false,
            }]
        );

        assert!(app.handle_key(KeyCode::Char('x')));
        assert!(app.drain_actions().is_empty());
        assert!(app.status.contains("confirm mcp project server remove"));
        let output = render_once(&app, 120, 24).unwrap();
        assert!(output.contains("MCP Remove Confirmation"));
        assert!(output.contains("Name: alpha"));

        assert!(app.handle_key(KeyCode::Char('y')));
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpRemove {
                scope: TuiMcpConfigScope::Project,
                name: "alpha".to_string(),
            }]
        );

        assert!(app.handle_key(KeyCode::Char('t')));
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpManagerDetails {
                kind: TuiMcpDetailKind::Tools,
                server: Some("alpha".to_string()),
            }]
        );
    }

    #[test]
    fn mcp_manager_mouse_clicks_tabs_and_server_rows() {
        let mut app = TuiApp::new(Vec::new());
        let detail = "MCP Manager\nmcp servers=2 enabled=1 [alpha:http:on] [beta:stdio:off]\n\nMCP servers:\n- alpha [enabled http] http://127.0.0.1:3000/mcp (source=project, env=-)\n- beta [disabled stdio] npx -y @mcp/server . (source=user, env=TOKEN)";
        app.set_mcp_manager(detail);
        app.last_frame_area = Rect::new(0, 0, 160, 36);
        let (_, body, _) = app.frame_layout().unwrap();
        let tabs = render_mcp_manager_tabs(&TuiMcpDetailKind::Manager);
        let tools_offset = tabs.find("tools").unwrap() as u16;

        assert!(app.handle_mouse_event(left_click(body.x + 1 + tools_offset, body.y + 1)));
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpManagerDetails {
                kind: TuiMcpDetailKind::Tools,
                server: None,
            }]
        );

        let beta_row = 4 + detail
            .lines()
            .position(|line| line.contains("- beta"))
            .unwrap();
        assert!(app.handle_mouse_event(left_click(body.x + 3, body.y + 1 + beta_row as u16)));

        assert_eq!(app.mcp_manager_selected_server, 1);
        assert!(app.status.contains("beta (user, disabled)"));
    }

    #[test]
    fn mcp_manager_mouse_action_strip_targets_selected_server() {
        let mut app = TuiApp::new(Vec::new());
        let detail = "MCP Manager\nmcp servers=2 enabled=1 [alpha:http:on] [beta:stdio:off]\n\nMCP servers:\n- alpha [enabled http] http://127.0.0.1:3000/mcp (source=project, env=-)\n- beta [disabled stdio] npx -y @mcp/server . (source=user, env=TOKEN)";
        app.set_mcp_manager(detail);
        app.last_frame_area = Rect::new(0, 0, 160, 36);
        app.mcp_manager_selected_server = 1;
        let (_, body, _) = app.frame_layout().unwrap();
        let action_line =
            render_mcp_manager_server_actions(detail, app.mcp_manager_selected_server, 0);

        let enable_offset = action_line.find("e enable").unwrap() as u16;
        assert!(app.handle_mouse_event(left_click(body.x + 1 + enable_offset, body.y + 3)));
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpSetEnabled {
                scope: TuiMcpConfigScope::User,
                name: "beta".to_string(),
                enabled: true,
            }]
        );

        let tools_offset = action_line.find("t tools").unwrap() as u16;
        assert!(app.handle_mouse_event(left_click(body.x + 1 + tools_offset, body.y + 3)));
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpManagerDetails {
                kind: TuiMcpDetailKind::Tools,
                server: Some("beta".to_string()),
            }]
        );

        let reload_offset = action_line.find("r reload").unwrap() as u16;
        assert!(app.handle_mouse_event(left_click(body.x + 1 + reload_offset, body.y + 3)));
        assert_eq!(app.drain_actions(), vec![TuiAction::McpList]);

        let remove_offset = action_line.find("x remove").unwrap() as u16;
        assert!(app.handle_mouse_event(left_click(body.x + 1 + remove_offset, body.y + 3)));
        assert!(app.drain_actions().is_empty());
        assert!(app.status.contains("confirm mcp user server remove: beta"));
        assert_eq!(
            app.mcp_remove_confirmation,
            Some(TuiMcpPendingRemove {
                scope: TuiMcpConfigScope::User,
                name: "beta".to_string(),
            })
        );
    }

    #[test]
    fn mcp_manager_keyboard_bulk_selects_and_sets_enabled() {
        let mut app = TuiApp::new(Vec::new());
        app.set_mcp_manager(
            "MCP Manager\nmcp servers=2 enabled=1 [alpha:http:on] [beta:stdio:off]\n\nMCP servers:\n- alpha [enabled http] http://127.0.0.1:3000/mcp (source=project, env=-)\n- beta [disabled stdio] npx -y @mcp/server . (source=user, env=TOKEN)",
        );

        assert!(app.handle_key(KeyCode::Char('A')));
        assert_eq!(app.mcp_manager_selected_server_keys.len(), 2);
        assert!(app.status.contains("selected 2 visible"));

        assert!(app.handle_key(KeyCode::Char('D')));
        assert_eq!(
            app.drain_actions(),
            vec![
                TuiAction::McpSetEnabled {
                    scope: TuiMcpConfigScope::Project,
                    name: "alpha".to_string(),
                    enabled: false,
                },
                TuiAction::McpSetEnabled {
                    scope: TuiMcpConfigScope::User,
                    name: "beta".to_string(),
                    enabled: false,
                },
            ]
        );
        assert!(app.status.contains("bulk disable requested for 2"));

        assert!(app.handle_key(KeyCode::Char('U')));
        assert!(app.mcp_manager_selected_server_keys.is_empty());
        assert_eq!(app.status, "mcp manager cleared 2 selected server(s)");
    }

    #[test]
    fn mcp_manager_mouse_ctrl_click_toggles_bulk_selection() {
        let mut app = TuiApp::new(Vec::new());
        let detail = "MCP Manager\nmcp servers=2 enabled=1 [alpha:http:on] [beta:stdio:off]\n\nMCP servers:\n- alpha [enabled http] http://127.0.0.1:3000/mcp (source=project, env=-)\n- beta [disabled stdio] npx -y @mcp/server . (source=user, env=TOKEN)";
        app.set_mcp_manager(detail);
        app.last_frame_area = Rect::new(0, 0, 160, 36);
        let (_, body, _) = app.frame_layout().unwrap();
        let beta_row = 4 + detail
            .lines()
            .position(|line| line.contains("- beta"))
            .unwrap();

        assert!(app.handle_mouse_event(ctrl_left_click(body.x + 3, body.y + 1 + beta_row as u16)));
        assert_eq!(app.mcp_manager_selected_server_keys.len(), 1);
        assert!(app.status.contains("beta (1 selected)"));

        let action_line = render_mcp_manager_server_actions(detail, 0, 1);
        let enable_offset = action_line.find("e enable").unwrap() as u16;
        assert!(app.handle_mouse_event(left_click(body.x + 1 + enable_offset, body.y + 3)));
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpSetEnabled {
                scope: TuiMcpConfigScope::User,
                name: "beta".to_string(),
                enabled: true,
            }]
        );
        assert!(app.status.contains("bulk enable requested for 1"));
    }

    #[test]
    fn mcp_manager_mouse_drag_selects_visible_server_range() {
        let mut app = TuiApp::new(Vec::new());
        let detail = "MCP Manager\nmcp servers=3 enabled=2 [alpha:http:on] [beta:stdio:off] [gamma:http:on]\n\nMCP servers:\n- alpha [enabled http] http://127.0.0.1:3000/mcp (source=project, env=-)\n- beta [disabled stdio] npx -y @mcp/server . (source=user, env=TOKEN)\n- gamma [enabled http] http://127.0.0.1:4000/mcp (source=project, env=-)";
        app.set_mcp_manager(detail);
        app.last_frame_area = Rect::new(0, 0, 160, 36);
        let (_, body, _) = app.frame_layout().unwrap();
        let alpha_row = 4 + detail
            .lines()
            .position(|line| line.contains("- alpha"))
            .unwrap();
        let gamma_row = 4 + detail
            .lines()
            .position(|line| line.contains("- gamma"))
            .unwrap();

        assert!(app.handle_mouse_event(left_click(body.x + 3, body.y + 1 + alpha_row as u16)));
        assert!(app.mcp_manager_selected_server_keys.is_empty());
        assert!(app.handle_mouse_event(left_drag(body.x + 3, body.y + 1 + gamma_row as u16)));

        assert_eq!(app.mcp_manager_selected_server_keys.len(), 3);
        assert!(app
            .mcp_manager_selected_server_keys
            .contains("project:alpha"));
        assert!(app.mcp_manager_selected_server_keys.contains("user:beta"));
        assert!(app
            .mcp_manager_selected_server_keys
            .contains("project:gamma"));
        assert_eq!(app.mcp_manager_selected_server, 2);
        assert!(app.status.contains("drag selected server range"));

        assert!(app.handle_mouse_event(left_release(body.x + 3, body.y + 1 + gamma_row as u16)));
        assert_eq!(app.mcp_manager_drag_anchor_key, None);
    }

    #[test]
    fn mcp_manager_remove_confirmation_can_cancel() {
        let mut app = TuiApp::new(Vec::new());
        app.set_mcp_manager(
            "MCP Manager\nmcp servers=1 enabled=1 [alpha:http:on]\n\nMCP servers:\n- alpha [enabled http] http://127.0.0.1:3000/mcp (source=project, env=-)",
        );

        assert!(app.handle_key(KeyCode::Char('x')));
        assert!(app.drain_actions().is_empty());
        assert!(app.handle_key(KeyCode::Char('n')));
        assert!(app.drain_actions().is_empty());
        assert!(app
            .status
            .contains("mcp project server remove cancelled: alpha"));
        assert!(render_once(&app, 120, 24)
            .unwrap()
            .contains("Server actions"));
        assert!(!render_once(&app, 120, 24)
            .unwrap()
            .contains("MCP Remove Confirmation"));
    }

    #[test]
    fn mcp_detail_panel_scrolls_and_closes() {
        let mut app = TuiApp::new(Vec::new());
        let detail = (0..20)
            .map(|index| format!("tool-{index:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.set_mcp_detail(TuiMcpDetailKind::Tools, detail);

        let output = render_once(&app, 120, 16).unwrap();
        assert!(output.contains("tool-00"));
        assert!(!output.contains("tool-19"));

        assert!(app.handle_key(KeyCode::PageDown));
        let output = render_once(&app, 120, 16).unwrap();
        assert!(!output.contains("tool-00"));
        assert!(output.contains("tool-08"));
        assert!(app.status.contains("mcp detail"));

        assert!(app.handle_key(KeyCode::End));
        let output = render_once(&app, 120, 16).unwrap();
        assert!(output.contains("tool-19"));

        assert!(app.handle_key(KeyCode::Esc));
        assert!(app.mcp_detail.is_none());
        assert!(render_once(&app, 120, 16).unwrap().contains("Plan / Tasks"));
    }

    #[test]
    fn ratio_bar_preserves_visible_nonzero_sides() {
        assert_eq!(format_ratio_bar(7, 5, 12, '#', '.'), "[#######.....]");
        assert_eq!(format_ratio_bar(1, 999, 8, 'i', 'o'), "[iooooooo]");
        assert_eq!(format_ratio_bar(0, 0, 4, '#', '.'), "[----]");
    }

    #[test]
    fn usage_summary_aggregates_cost_split() {
        let records = vec![UsageRecord {
            id: "usage-one".to_string(),
            thread_id: "thread-one".to_string(),
            turn_id: None,
            model: "deepseek-v4-flash".to_string(),
            source: "test".to_string(),
            prompt_tokens: 12,
            completion_tokens: 3,
            total_tokens: 15,
            prompt_cache_hit_tokens: 7,
            prompt_cache_miss_tokens: 5,
            estimated_input_cost_microusd: Some(1),
            estimated_output_cost_microusd: Some(1),
            estimated_total_cost_microusd: Some(2),
            pricing_source: Some("test pricing".to_string()),
            created_at: "epoch+1".to_string(),
        }];

        let summary = TuiUsageSummary::from_usage_records("thread-one", &records);

        assert_eq!(summary.estimated_input_cost_microusd, Some(1));
        assert_eq!(summary.estimated_output_cost_microusd, Some(1));
        assert_eq!(summary.estimated_total_cost_microusd, Some(2));
        assert_eq!(summary.prompt_tokens, 12);
        assert_eq!(summary.completion_tokens, 3);
        assert_eq!(summary.latest_prompt_tokens, 12);
        assert_eq!(summary.latest_completion_tokens, 3);
        assert_eq!(summary.prompt_cache_hit_tokens, 7);
        assert_eq!(summary.prompt_cache_miss_tokens, 5);
    }

    #[test]
    fn task_panel_renders_active_thread_runtime_tasks() {
        let app = TuiApp::with_runtime_usage_tasks_and_approvals(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            vec![
                TuiItem {
                    id: "item-one".to_string(),
                    thread_id: "thread-one".to_string(),
                    turn_id: Some("turn-one".to_string()),
                    index: 0,
                    item_type: "message".to_string(),
                    role: Some("assistant".to_string()),
                    content: "working".to_string(),
                    status: "running".to_string(),
                },
                TuiItem {
                    id: "item-two".to_string(),
                    thread_id: "thread-one".to_string(),
                    turn_id: Some("turn-one".to_string()),
                    index: 1,
                    item_type: "tool_call".to_string(),
                    role: None,
                    content: "read_file src/tui.rs".to_string(),
                    status: "completed".to_string(),
                },
            ],
            vec![TuiTaskRecord {
                id: "task-one".to_string(),
                session_id: Some("session-one".to_string()),
                thread_id: Some("thread-one".to_string()),
                parent_task_id: None,
                kind: "agent".to_string(),
                status: "running".to_string(),
                summary: "agent run: implement runtime progress".to_string(),
                updated_at: "epoch+2".to_string(),
            }],
            Vec::new(),
            Vec::new(),
        );

        let output = render_once(&app, 140, 40).unwrap();
        assert!(output.contains("Runtime items: 2"));
        assert!(output.contains("Item states:"));
        assert!(output.contains("running=1"));
        assert!(output.contains("completed=1"));
        assert!(output.contains("Item types:"));
        assert!(output.contains("message=1"));
        assert!(output.contains("tool_call=1"));
        assert!(output.contains("Latest: tool_call"));
        assert!(output.contains("[completed] read_file"));
        assert!(output.contains("Runtime tasks: 1"));
        assert!(output.contains("Task states: running=1"));
        assert!(output.contains("Task [running] task-one"));
        assert!(output.contains("agent updated epoch+2"));
    }

    #[test]
    fn task_panel_renders_active_thread_automations() {
        let app = TuiApp::with_runtime_usage_tasks_automations_and_approvals(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
            Vec::new(),
            vec![TuiAutomationRecord {
                id: "automation-one".to_string(),
                session_id: Some("session-one".to_string()),
                thread_id: Some("thread-one".to_string()),
                name: "Nightly diagnostics".to_string(),
                status: "active".to_string(),
                schedule: "daily".to_string(),
                prompt: "run diagnostics".to_string(),
                updated_at: "epoch+2".to_string(),
                last_run_at: None,
                next_run_at: Some("epoch+3".to_string()),
            }],
            Vec::new(),
            Vec::new(),
        );

        let output = render_once(&app, 140, 40).unwrap();
        assert!(output.contains("Automations: 1"));
        assert!(output.contains("Automation Nightly"));
    }

    #[test]
    fn command_palette_requests_active_automation_trigger() {
        let mut app = TuiApp::with_runtime_usage_tasks_automations_and_approvals(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
            Vec::new(),
            vec![TuiAutomationRecord {
                id: "automation-one".to_string(),
                session_id: Some("session-one".to_string()),
                thread_id: Some("thread-one".to_string()),
                name: "Nightly diagnostics".to_string(),
                status: "active".to_string(),
                schedule: "daily".to_string(),
                prompt: "run diagnostics".to_string(),
                updated_at: "epoch+2".to_string(),
                last_run_at: None,
                next_run_at: Some("epoch+3".to_string()),
            }],
            Vec::new(),
            Vec::new(),
        );

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "automation trigger".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::TriggerAutomation {
                automation_id: "automation-one".to_string(),
                prompt_override: None,
            }]
        );
        assert!(app.status.contains("automation trigger requested"));
    }

    #[test]
    fn command_palette_requests_automation_trigger_with_prompt_override() {
        let mut app = TuiApp::with_runtime_usage_tasks_automations_and_approvals(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
            Vec::new(),
            vec![TuiAutomationRecord {
                id: "automation-one".to_string(),
                session_id: Some("session-one".to_string()),
                thread_id: Some("thread-one".to_string()),
                name: "Nightly diagnostics".to_string(),
                status: "active".to_string(),
                schedule: "daily".to_string(),
                prompt: "run diagnostics".to_string(),
                updated_at: "epoch+2".to_string(),
                last_run_at: None,
                next_run_at: Some("epoch+3".to_string()),
            }],
            Vec::new(),
            Vec::new(),
        );

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "automation trigger automation-one run now".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::TriggerAutomation {
                automation_id: "automation-one".to_string(),
                prompt_override: Some("run now".to_string()),
            }]
        );
    }

    #[test]
    fn command_palette_requests_pending_runtime_task_creation() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "task inspect flaky test".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::CreateTask {
                thread_id: "thread-one".to_string(),
                summary: "inspect flaky test".to_string(),
            }]
        );
        assert!(app.status.contains("task queued for creation"));
    }

    #[test]
    fn command_palette_requests_pending_task_pause() {
        let mut app = TuiApp::with_runtime_usage_tasks_automations_and_approvals(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
            vec![TuiTaskRecord {
                id: "task-one".to_string(),
                session_id: Some("session-one".to_string()),
                thread_id: Some("thread-one".to_string()),
                parent_task_id: None,
                kind: "agent".to_string(),
                status: "pending".to_string(),
                summary: "inspect flaky test".to_string(),
                updated_at: "epoch+1".to_string(),
            }],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "task pause".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::PauseTask {
                task_id: "task-one".to_string(),
            }]
        );
        assert!(app.status.contains("task pause requested"));
    }

    #[test]
    fn command_palette_requests_paused_task_resume_by_id() {
        let mut app = TuiApp::with_runtime_usage_tasks_automations_and_approvals(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
            vec![TuiTaskRecord {
                id: "task-paused".to_string(),
                session_id: Some("session-one".to_string()),
                thread_id: Some("thread-one".to_string()),
                parent_task_id: None,
                kind: "agent".to_string(),
                status: "paused".to_string(),
                summary: "resume me".to_string(),
                updated_at: "epoch+1".to_string(),
            }],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "task resume task-paused".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::ResumeTask {
                task_id: "task-paused".to_string(),
            }]
        );
        assert!(app.status.contains("task resume requested"));
    }

    #[test]
    fn command_palette_requests_running_task_cancel_by_default() {
        let mut app = TuiApp::with_runtime_usage_tasks_automations_and_approvals(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
            vec![TuiTaskRecord {
                id: "task-running".to_string(),
                session_id: Some("session-one".to_string()),
                thread_id: Some("thread-one".to_string()),
                parent_task_id: None,
                kind: "agent".to_string(),
                status: "running".to_string(),
                summary: "stop me".to_string(),
                updated_at: "epoch+1".to_string(),
            }],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "task cancel".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::CancelTask {
                task_id: "task-running".to_string(),
            }]
        );
        assert!(app.status.contains("task cancel requested"));
    }

    #[test]
    fn command_palette_selected_task_drives_default_actions() {
        let mut app = app_with_runtime_tasks(vec![
            runtime_task("task-running", "running", "currently executing", "epoch+3"),
            runtime_task("task-pending", "pending", "queued follow-up", "epoch+2"),
        ]);

        assert_eq!(app.selected_task_id.as_deref(), Some("task-running"));

        run_palette_command(&mut app, "task select task-pending");
        assert_eq!(app.selected_task_id.as_deref(), Some("task-pending"));
        assert!(render_once(&app, 160, 40)
            .unwrap()
            .contains("> Task [pending] task-pending"));

        run_palette_command(&mut app, "task pause");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::PauseTask {
                task_id: "task-pending".to_string(),
            }]
        );

        run_palette_command(&mut app, "task cancel");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::CancelTask {
                task_id: "task-pending".to_string(),
            }]
        );
    }

    #[test]
    fn command_palette_bulk_selected_tasks_drive_default_actions() {
        let mut app = app_with_runtime_tasks(vec![
            runtime_task("task-running", "running", "currently executing", "epoch+4"),
            runtime_task("task-pending", "pending", "queued follow-up", "epoch+3"),
            runtime_task("task-paused", "paused", "waiting on user", "epoch+2"),
        ]);

        run_palette_command(&mut app, "task select all");
        assert_eq!(app.selected_task_ids.len(), 3);
        let output = render_once(&app, 160, 40).unwrap();
        assert!(output.contains("Selected tasks: 3"));

        run_palette_command(&mut app, "task bulk cancel");
        assert_eq!(
            app.drain_actions(),
            vec![
                TuiAction::CancelTask {
                    task_id: "task-running".to_string(),
                },
                TuiAction::CancelTask {
                    task_id: "task-pending".to_string(),
                },
                TuiAction::CancelTask {
                    task_id: "task-paused".to_string(),
                },
            ]
        );
        assert!(app.status.contains("bulk task cancel requested for 3"));

        run_palette_command(&mut app, "task select clear");
        assert!(app.selected_task_ids.is_empty());
        assert!(app.status.contains("cleared 3 selected task"));
    }

    #[test]
    fn mouse_click_selects_task_panel_row() {
        let mut app = app_with_runtime_tasks(vec![
            runtime_task("task-newer", "running", "newer task", "epoch+2"),
            runtime_task("task-older", "pending", "older task", "epoch+1"),
        ]);
        app.last_frame_area = Rect::new(0, 0, 160, 36);
        let (_, body, _) = app.frame_layout().unwrap();
        let columns = body_columns(body);

        assert!(app.handle_mouse_event(left_click(columns[2].x + 2, columns[2].y + 1 + 9)));

        assert_eq!(app.selected_task_id.as_deref(), Some("task-older"));
        assert!(app.status.contains("selected task: task-older"));
        assert!(render_once(&app, 160, 40)
            .unwrap()
            .contains("> Task [pending] task-older"));
    }

    #[test]
    fn mouse_ctrl_click_and_drag_select_task_panel_rows() {
        let mut app = app_with_runtime_tasks(vec![
            runtime_task("task-newer", "running", "newer task", "epoch+3"),
            runtime_task("task-middle", "pending", "middle task", "epoch+2"),
            runtime_task("task-older", "paused", "older task", "epoch+1"),
        ]);
        app.last_frame_area = Rect::new(0, 0, 160, 36);
        let (_, body, _) = app.frame_layout().unwrap();
        let columns = body_columns(body);
        let row_for_task = |index: u16| columns[2].y + 1 + 6 + index * 3;

        assert!(app.handle_mouse_event(ctrl_left_click(columns[2].x + 2, row_for_task(1))));
        assert_eq!(app.selected_task_ids.len(), 1);
        assert!(app.selected_task_ids.contains("task-middle"));

        assert!(app.handle_mouse_event(left_click(columns[2].x + 2, row_for_task(0))));
        assert!(app.handle_mouse_event(left_drag(columns[2].x + 2, row_for_task(2))));
        assert_eq!(app.selected_task_ids.len(), 3);
        assert!(app.selected_task_ids.contains("task-newer"));
        assert!(app.selected_task_ids.contains("task-middle"));
        assert!(app.selected_task_ids.contains("task-older"));
        assert_eq!(app.selected_task_id.as_deref(), Some("task-older"));
        assert!(app.status.contains("task drag selected range"));

        assert!(app.handle_mouse_event(left_release(columns[2].x + 2, row_for_task(2))));
        assert_eq!(app.task_drag_anchor_id, None);
    }

    #[test]
    fn command_palette_requests_rollback_snapshot_and_list() {
        let mut app = TuiApp::new(Vec::new());

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "restore snapshot before risky turn".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::CreateRollbackSnapshot {
                label: Some("before risky turn".to_string()),
            }]
        );

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "restore list 7".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::ListRollbackSnapshots { limit: 7 }]
        );
    }

    #[test]
    fn command_palette_requests_rollback_show_and_revert_last_turn() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: Some("turn-latest".to_string()),
                event_seq: 1,
            }],
            Vec::new(),
        );

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "restore show last".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::ShowRollbackSnapshot {
                id: "turn-latest".to_string(),
            }]
        );

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "restore hunks last".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::ShowRollbackHunk {
                id: "turn-latest".to_string(),
                hunk: None,
            }]
        );

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "restore hunk last 2".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::ShowRollbackHunk {
                id: "turn-latest".to_string(),
                hunk: Some(2),
            }]
        );

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "revert turn last --apply".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert!(app.drain_actions().is_empty());
        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("Rollback Apply Confirmation"));
        assert!(output.contains("Target: turn-latest"));
        assert!(app.status.contains("confirm rollback apply"));

        assert!(app.handle_key(KeyCode::Char('y')));
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::RevertTurn {
                id: "turn-latest".to_string(),
                apply: true,
            }]
        );
    }

    #[test]
    fn command_palette_requests_rollback_hunk_browser() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: Some("turn-latest".to_string()),
                event_seq: 1,
            }],
            Vec::new(),
        );

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "restore diff last".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::ShowRollbackHunk {
                id: "turn-latest".to_string(),
                hunk: None,
            }]
        );
        assert!(app.status.contains("rollback hunks requested"));

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "restore hunk last nope".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert!(app.drain_actions().is_empty());
        assert_eq!(app.status, "invalid rollback hunk index: nope");

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "restore hunk last 2 --check".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::RestoreRollbackHunk {
                id: "turn-latest".to_string(),
                hunk: 2,
                apply: false,
            }]
        );
        assert!(app.status.contains("rollback hunk check requested"));

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "restore hunk last 2 --apply".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert!(app.drain_actions().is_empty());
        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("Rollback Apply Confirmation"));
        assert!(output.contains("Hunk: #2"));
        assert!(app.status.contains("confirm rollback hunk apply"));
        assert!(app.handle_key(KeyCode::Enter));
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::RestoreRollbackHunk {
                id: "turn-latest".to_string(),
                hunk: 2,
                apply: true,
            }]
        );
        assert_eq!(app.status, "rollback hunk apply confirmed: turn-latest #2");
    }

    #[test]
    fn command_palette_confirms_rollback_apply_before_queueing() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: Some("turn-latest".to_string()),
                event_seq: 1,
            }],
            Vec::new(),
        );

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "restore revert-turn last --apply".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert!(app.drain_actions().is_empty());
        assert!(app.rollback_apply_confirmation.is_some());
        assert!(app.status.contains("confirm rollback apply"));

        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::RevertTurn {
                id: "turn-latest".to_string(),
                apply: true,
            }]
        );
        assert!(app.rollback_apply_confirmation.is_none());
        assert_eq!(app.status, "rollback apply confirmed: turn-latest");
    }

    #[test]
    fn command_palette_cancels_rollback_apply_confirmation() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: Some("turn-latest".to_string()),
                event_seq: 1,
            }],
            Vec::new(),
        );

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "revert turn last --apply".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));
        assert!(app.rollback_apply_confirmation.is_some());

        assert!(app.handle_key(KeyCode::Esc));

        assert!(app.rollback_apply_confirmation.is_none());
        assert!(app.drain_actions().is_empty());
        assert_eq!(app.status, "rollback apply cancelled: turn-latest");
    }

    #[test]
    fn command_palette_requests_changed_diagnostics() {
        let mut app = TuiApp::new(Vec::new());

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "diagnostics --changed".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::RunDiagnostics {
                changed: true,
                paths: Vec::new(),
            }]
        );
        assert!(app.status.contains("diagnostics requested"));
    }

    #[test]
    fn command_palette_requests_path_diagnostics() {
        let mut app = TuiApp::new(Vec::new());

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "diag src/lib.rs src/tui.rs".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::RunDiagnostics {
                changed: false,
                paths: vec!["src/lib.rs".to_string(), "src/tui.rs".to_string()],
            }]
        );
        assert!(app.status.contains("2 paths"));
    }

    #[test]
    fn command_palette_requests_custom_slash_command() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        run_palette_command(&mut app, "/review src/lib.rs --strict");

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::RunCustomSlashCommand {
                thread_id: "thread-one".to_string(),
                command: "/review".to_string(),
                args: vec!["src/lib.rs".to_string(), "--strict".to_string()],
            }]
        );
        assert_eq!(app.status, "custom slash command queued: /review");
    }

    #[test]
    fn command_palette_requests_session_rename() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        run_palette_command(&mut app, "rename Focused Work");

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::RenameSession {
                session_id: "session-one".to_string(),
                title: "Focused Work".to_string(),
            }]
        );
        assert_eq!(app.status, "session rename queued: Focused Work");
    }

    #[test]
    fn command_palette_requests_project_instructions_init() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: "/tmp/deepseek-workspace".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        run_palette_command(&mut app, "init");

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::InitProjectInstructions {
                workspace: "/tmp/deepseek-workspace".to_string(),
            }]
        );
        assert_eq!(
            app.status,
            "project instructions init queued: /tmp/deepseek-workspace"
        );
    }

    #[test]
    fn command_palette_requests_shell_job_actions() {
        let mut app = TuiApp::new(Vec::new());

        run_palette_command(&mut app, "shell echo hello");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::RunShell {
                command: "echo hello".to_string(),
            }]
        );
        assert!(app.status.contains("shell job requested"));

        run_palette_command(&mut app, "! git status");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::RunShell {
                command: "git status".to_string(),
            }]
        );

        run_palette_command(&mut app, "shell wait shell-1 250");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::WaitShell {
                task_id: "shell-1".to_string(),
                wait: true,
                timeout_ms: 250,
            }]
        );

        run_palette_command(&mut app, "shell poll shell-1");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::WaitShell {
                task_id: "shell-1".to_string(),
                wait: false,
                timeout_ms: 0,
            }]
        );

        run_palette_command(&mut app, "jobs list");
        assert_eq!(app.drain_actions(), vec![TuiAction::ListShell]);

        run_palette_command(&mut app, "jobs show shell-1");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::ShowShell {
                task_id: "shell-1".to_string(),
            }]
        );

        run_palette_command(&mut app, "jobs attach shell-1 12");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::AttachShell {
                task_id: "shell-1".to_string(),
                cursor: Some(12),
                tail: false,
            }]
        );

        run_palette_command(&mut app, "shell attach shell-1 tail");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::AttachShell {
                task_id: "shell-1".to_string(),
                cursor: None,
                tail: true,
            }]
        );

        run_palette_command(&mut app, "jobs supervisor");
        assert_eq!(app.drain_actions(), vec![TuiAction::ShellSupervisorStatus]);
        assert!(app.status.contains("supervisor status requested"));

        run_palette_command(&mut app, "jobs stdin shell-1 hello world");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::SendShellStdin {
                task_id: "shell-1".to_string(),
                input: "hello world".to_string(),
                close: false,
            }]
        );

        run_palette_command(&mut app, "jobs close-stdin shell-1");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::SendShellStdin {
                task_id: "shell-1".to_string(),
                input: String::new(),
                close: true,
            }]
        );

        run_palette_command(&mut app, "jobs resize shell-1 40 120");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::ResizeShell {
                task_id: "shell-1".to_string(),
                rows: 40,
                cols: 120,
            }]
        );

        run_palette_command(&mut app, "shell cancel all");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::CancelShell {
                task_id: None,
                all: true,
            }]
        );

        run_palette_command(&mut app, "jobs cancel shell-1");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::CancelShell {
                task_id: Some("shell-1".to_string()),
                all: false,
            }]
        );
    }

    #[test]
    fn command_palette_requires_approval_for_unallowlisted_shell_command() {
        let mut app = TuiApp::new(Vec::new());

        run_palette_command(&mut app, "shell printf shell-approved");

        assert!(app.show_approval_modal);
        assert_eq!(app.drain_actions(), Vec::<TuiAction>::new());
        assert!(app.status.contains("requires approval"));

        assert!(app.handle_key(KeyCode::Char('y')));
        assert!(!app.show_approval_modal);
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::RunApprovedShell {
                command: "printf shell-approved".to_string(),
            }]
        );
        assert!(app.status.contains("approved shell command"));
    }

    #[test]
    fn command_palette_requests_mcp_inventory_actions() {
        let mut app = TuiApp::new(Vec::new());

        run_palette_command(&mut app, "mcp");
        assert_eq!(app.drain_actions(), vec![TuiAction::McpManager]);
        assert!(app.status.contains("mcp manager requested"));

        run_palette_command(&mut app, "mcp manager");
        assert_eq!(app.drain_actions(), vec![TuiAction::McpManager]);

        run_palette_command(&mut app, "mcp manager tools remote");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpManagerDetails {
                kind: TuiMcpDetailKind::Tools,
                server: Some("remote".to_string()),
            }]
        );
        assert!(app.status.contains("mcp manager tools detail requested"));

        run_palette_command(&mut app, "mcp manager templates");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpManagerDetails {
                kind: TuiMcpDetailKind::ResourceTemplates,
                server: None,
            }]
        );

        run_palette_command(&mut app, "mcp manager tab health");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpManagerDetails {
                kind: TuiMcpDetailKind::Health,
                server: None,
            }]
        );

        run_palette_command(&mut app, "mcp manager filter remote");
        assert!(app.drain_actions().is_empty());
        assert_eq!(app.mcp_manager_filter, "remote");
        assert!(app.status.contains("mcp manager filter: remote"));

        run_palette_command(&mut app, "mcp manager filter");
        assert!(app.drain_actions().is_empty());
        assert!(app.mcp_manager_filter.is_empty());
        assert_eq!(app.status, "mcp manager filter cleared");

        run_palette_command(&mut app, "mcp list");
        assert_eq!(app.drain_actions(), vec![TuiAction::McpList]);
        assert!(app.status.contains("mcp inventory requested"));

        run_palette_command(&mut app, "mcp reload");
        assert_eq!(app.drain_actions(), vec![TuiAction::McpList]);

        run_palette_command(&mut app, "mcp tools remote");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpDetails {
                kind: TuiMcpDetailKind::Tools,
                server: Some("remote".to_string()),
            }]
        );
        assert!(app.status.contains("mcp tools detail requested"));

        run_palette_command(&mut app, "mcp prompts");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpDetails {
                kind: TuiMcpDetailKind::Prompts,
                server: None,
            }]
        );

        run_palette_command(&mut app, "mcp resources remote");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpDetails {
                kind: TuiMcpDetailKind::Resources,
                server: Some("remote".to_string()),
            }]
        );

        run_palette_command(&mut app, "mcp templates remote");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpDetails {
                kind: TuiMcpDetailKind::ResourceTemplates,
                server: Some("remote".to_string()),
            }]
        );

        app.set_mcp_detail(TuiMcpDetailKind::Tools, "MCP remote tools:\n- fake");
        run_palette_command(&mut app, "mcp close");
        assert!(app.drain_actions().is_empty());
        assert!(app.mcp_detail.is_none());
        assert_eq!(app.status, "mcp detail closed");

        run_palette_command(&mut app, "mcp init --force");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpInit { force: true }]
        );

        run_palette_command(&mut app, "mcp validate");
        assert_eq!(app.drain_actions(), vec![TuiAction::McpValidate]);
    }

    #[test]
    fn command_palette_requests_memory_actions() {
        let mut app = TuiApp::new(Vec::new());

        run_palette_command(&mut app, "memory");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Memory {
                command: TuiMemoryCommand::Show,
            }]
        );

        run_palette_command(&mut app, "memory path");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Memory {
                command: TuiMemoryCommand::Path,
            }]
        );

        run_palette_command(&mut app, "memory clear");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Memory {
                command: TuiMemoryCommand::Clear,
            }]
        );

        run_palette_command(&mut app, "memory edit");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Memory {
                command: TuiMemoryCommand::Edit,
            }]
        );

        run_palette_command(&mut app, "memory help");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Memory {
                command: TuiMemoryCommand::Help,
            }]
        );
    }

    #[test]
    fn command_palette_requests_note_actions() {
        let mut app = TuiApp::new(Vec::new());

        run_palette_command(&mut app, "note add keep release notes short");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Note {
                command: TuiNoteCommand::Add {
                    content: "keep release notes short".to_string(),
                },
            }]
        );

        run_palette_command(&mut app, "note list");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Note {
                command: TuiNoteCommand::List,
            }]
        );

        run_palette_command(&mut app, "note show 2");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Note {
                command: TuiNoteCommand::Show { index: 2 },
            }]
        );

        run_palette_command(&mut app, "note edit 2 updated note");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Note {
                command: TuiNoteCommand::Edit {
                    index: 2,
                    content: "updated note".to_string(),
                },
            }]
        );

        run_palette_command(&mut app, "note remove 2");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Note {
                command: TuiNoteCommand::Remove { index: 2 },
            }]
        );

        app.composer_focused = true;
        app.composer = "/note path".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Note {
                command: TuiNoteCommand::Path,
            }]
        );
        assert_eq!(app.composer, "");
    }

    #[test]
    fn command_palette_requests_anchor_actions() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: "/workspace/project".to_string(),
                status: "active".to_string(),
                active_thread_id: None,
                thread_count: 0,
            }],
            Vec::new(),
            Vec::new(),
        );

        run_palette_command(&mut app, "anchor Never touch .ssh");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Anchor {
                workspace: "/workspace/project".to_string(),
                command: TuiAnchorCommand::Add {
                    content: "Never touch .ssh".to_string(),
                },
            }]
        );

        run_palette_command(&mut app, "anchor list");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Anchor {
                workspace: "/workspace/project".to_string(),
                command: TuiAnchorCommand::List,
            }]
        );

        run_palette_command(&mut app, "anchor remove 2");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Anchor {
                workspace: "/workspace/project".to_string(),
                command: TuiAnchorCommand::Remove { index: 2 },
            }]
        );

        app.composer_focused = true;
        app.composer = "/anchor path".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Anchor {
                workspace: "/workspace/project".to_string(),
                command: TuiAnchorCommand::Path,
            }]
        );
        assert_eq!(app.composer, "");
    }

    #[test]
    fn queue_command_lists_edits_drops_and_clears_messages() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: "/workspace/project".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );
        app.queued_messages.push_back(TuiQueuedMessage {
            thread_id: "thread-one".to_string(),
            content: "First follow-up".to_string(),
        });
        app.queued_messages.push_back(TuiQueuedMessage {
            thread_id: "thread-one".to_string(),
            content: "Second follow-up".to_string(),
        });

        run_palette_command(&mut app, "queue list");
        let (kind, detail) = app.mcp_detail.as_ref().expect("queue detail");
        assert_eq!(*kind, TuiMcpDetailKind::Queue);
        assert!(detail.contains("Queued messages:"));
        assert!(detail.contains("First follow-up"));

        run_palette_command(&mut app, "queue drop 1");
        assert_eq!(app.queued_messages.len(), 1);
        assert_eq!(app.status, "dropped queued message 1");

        run_palette_command(&mut app, "queue edit 1");
        assert_eq!(app.composer, "Second follow-up");
        assert!(app.composer_focused);
        assert!(app.queued_draft.is_some());
        assert!(app.queued_messages.is_empty());

        app.composer = "Edited follow-up".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::SubmitUserMessage {
                thread_id: "thread-one".to_string(),
                content: "Edited follow-up".to_string(),
            }]
        );
        assert!(app.queued_draft.is_none());

        app.queued_messages.push_back(TuiQueuedMessage {
            thread_id: "thread-one".to_string(),
            content: "Clear me".to_string(),
        });
        app.composer_focused = false;
        run_palette_command(&mut app, "/queue clear");
        assert!(app.queued_messages.is_empty());
        assert_eq!(app.status, "queue cleared");
    }

    #[test]
    fn composer_queues_follow_up_while_assistant_is_running() {
        let running_item = TuiItem {
            id: "assistant-item".to_string(),
            thread_id: "thread-one".to_string(),
            turn_id: Some("turn-one".to_string()),
            index: 1,
            item_type: "message".to_string(),
            role: Some("assistant".to_string()),
            content: "working".to_string(),
            status: "running".to_string(),
        };
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: "/workspace/project".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: Some("turn-one".to_string()),
                event_seq: 1,
            }],
            vec![running_item.clone()],
        );

        app.composer_focused = true;
        app.composer = "follow up when ready".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));

        assert!(app.drain_actions().is_empty());
        assert_eq!(app.queued_messages.len(), 1);
        assert!(app.status.contains("message queued for next turn"));

        let mut completed_item = running_item;
        completed_item.status = "completed".to_string();
        app.apply_live_event(TuiLiveEvent::UpsertItem(completed_item));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::SubmitUserMessage {
                thread_id: "thread-one".to_string(),
                content: "follow up when ready".to_string(),
            }]
        );
        assert!(app.queued_messages.is_empty());
        assert!(app.status.contains("submitted queued message"));
    }

    #[test]
    fn share_command_queues_active_thread_export() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: "/workspace/project".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        run_palette_command(&mut app, "share");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::ShareSession {
                thread_id: "thread-one".to_string(),
            }]
        );

        app.composer_focused = true;
        app.composer = "/share help".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        let (kind, detail) = app.mcp_detail.as_ref().expect("share help detail");
        assert_eq!(*kind, TuiMcpDetailKind::Share);
        assert!(detail.contains("gh gist create --public"));
        assert_eq!(app.composer, "");
    }

    #[test]
    fn export_command_queues_active_thread_markdown_export() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: "/workspace/project".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            vec![TuiItem {
                id: "item-one".to_string(),
                thread_id: "thread-one".to_string(),
                turn_id: None,
                index: 1,
                item_type: "message".to_string(),
                role: Some("user".to_string()),
                content: "hello".to_string(),
                status: "completed".to_string(),
            }],
        );

        run_palette_command(&mut app, "export exports/thread.md");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::ExportThread {
                thread_id: "thread-one".to_string(),
                path: Some("exports/thread.md".to_string()),
            }]
        );

        app.composer_focused = true;
        app.composer = "/export help".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        let (kind, detail) = app.mcp_detail.as_ref().expect("export help detail");
        assert_eq!(*kind, TuiMcpDetailKind::Export);
        assert!(detail.contains("/export [path]"));
        assert!(detail.contains("Transcript items:"));
        assert_eq!(app.composer, "");
    }

    #[test]
    fn save_and_load_commands_queue_session_snapshot_actions() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: "/workspace/project".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            vec![TuiItem {
                id: "item-one".to_string(),
                thread_id: "thread-one".to_string(),
                turn_id: None,
                index: 1,
                item_type: "message".to_string(),
                role: Some("user".to_string()),
                content: "hello".to_string(),
                status: "completed".to_string(),
            }],
        );

        run_palette_command(&mut app, "save snapshots/thread.json");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::SaveSession {
                session_id: "session-one".to_string(),
                thread_id: "thread-one".to_string(),
                path: Some("snapshots/thread.json".to_string()),
            }]
        );

        run_palette_command(&mut app, "load snapshots/thread.json");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::LoadSession {
                workspace: "/workspace/project".to_string(),
                path: "snapshots/thread.json".to_string(),
            }]
        );

        app.composer_focused = true;
        app.composer = "/save help".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        let (kind, detail) = app.mcp_detail.as_ref().expect("save help detail");
        assert_eq!(*kind, TuiMcpDetailKind::Save);
        assert!(detail.contains("/save [path]"));
        assert!(detail.contains("Transcript items:"));
        assert_eq!(app.composer, "");

        app.composer_focused = true;
        app.composer = "/load help".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        let (kind, detail) = app.mcp_detail.as_ref().expect("load help detail");
        assert_eq!(*kind, TuiMcpDetailKind::Load);
        assert!(detail.contains("/load <path>"));
        assert!(detail.contains("fresh runtime ids"));
        assert_eq!(app.composer, "");

        app.composer_focused = false;
        run_palette_command(&mut app, "load");
        assert_eq!(app.status, "usage: load <path> or /load <path>");
    }

    #[test]
    fn attach_command_inserts_media_reference_into_composer() {
        let root = temp_root("attach-command");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("photo.png"), b"not decoded").unwrap();
        fs::write(root.join("notes.txt"), b"text").unwrap();
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: root.display().to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        run_palette_command(&mut app, "attach photo.png");
        assert!(app.composer_focused);
        assert!(app.composer.contains("Attached image files:"));
        assert!(app.composer.contains("- photo.png (image/png)"));
        assert!(app.composer.contains("image_path=\"photo.png\""));
        assert!(app.status.contains("attached image:"));

        app.composer = "/image photo.png".to_string();
        app.composer_cursor = app.composer.len();
        app.composer_focused = true;
        assert!(app.handle_key(KeyCode::Enter));
        assert!(app.composer.contains("Attached image files:"));
        assert!(!app.composer.contains("/image photo.png"));

        app.composer_focused = false;
        run_palette_command(&mut app, "attach help");
        let (kind, detail) = app.mcp_detail.as_ref().expect("attach help detail");
        assert_eq!(*kind, TuiMcpDetailKind::Attach);
        assert!(detail.contains("/attach <path>"));
        assert!(detail.contains("/image <path>"));

        run_palette_command(&mut app, "attach notes.txt");
        assert!(app.status.contains("unsupported attachment type"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn change_command_renders_latest_changelog() {
        let mut app = TuiApp::new(Vec::new());

        run_palette_command(&mut app, "change");
        let (kind, detail) = app.mcp_detail.as_ref().expect("change detail");
        assert_eq!(*kind, TuiMcpDetailKind::Change);
        assert!(detail.contains("DeepSeekCode Changelog"));
        assert!(detail.contains("## 0.1.0 - 2026-05-09"));
        assert_eq!(app.status, "changelog shown");

        app.composer_focused = true;
        app.composer = "/changelog help".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        let (kind, detail) = app.mcp_detail.as_ref().expect("change help detail");
        assert_eq!(*kind, TuiMcpDetailKind::Change);
        assert!(detail.contains("/change shows the latest"));
        assert!(detail.contains("/changelog"));
        assert_eq!(app.composer, "");

        app.composer_focused = false;
        run_palette_command(&mut app, "change extra");
        assert_eq!(
            app.status,
            "usage: change or /change; use change help for details"
        );
    }

    #[test]
    fn latest_changelog_section_extracts_first_version_section() {
        let content =
            "# Changelog\n\n## 1.2.0 - Today\n\nLatest\n\n## 1.1.0 - Yesterday\n\nOlder\n";

        let section = latest_changelog_section(content).expect("latest section");

        assert!(section.contains("1.2.0"));
        assert!(section.contains("Latest"));
        assert!(!section.contains("1.1.0"));
    }

    #[test]
    fn system_command_queues_prompt_preview_and_renders_help() {
        let sessions = vec![TuiSession {
            id: "session-1".to_string(),
            title: "Session".to_string(),
            workspace: "/tmp/deepseek-system".to_string(),
            status: "active".to_string(),
            active_thread_id: Some("thread-1".to_string()),
            thread_count: 1,
        }];
        let threads = vec![TuiThread {
            id: "thread-1".to_string(),
            session_id: Some("session-1".to_string()),
            title: "Thread".to_string(),
            mode: "agent".to_string(),
            status: "idle".to_string(),
            latest_turn_id: Some("turn-2".to_string()),
            event_seq: 0,
        }];
        let items = vec![
            TuiItem {
                id: "item-1".to_string(),
                thread_id: "thread-1".to_string(),
                turn_id: Some("turn-1".to_string()),
                index: 1,
                item_type: "message".to_string(),
                role: Some("user".to_string()),
                content: "older request".to_string(),
                status: "completed".to_string(),
            },
            TuiItem {
                id: "item-2".to_string(),
                thread_id: "thread-1".to_string(),
                turn_id: Some("turn-2".to_string()),
                index: 2,
                item_type: "message".to_string(),
                role: Some("user".to_string()),
                content: "latest request".to_string(),
                status: "completed".to_string(),
            },
        ];
        let mut app = TuiApp::with_runtime(sessions, threads, items);

        run_palette_command(&mut app, "system");

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::ShowSystemPrompt {
                workspace: "/tmp/deepseek-system".to_string(),
                mode: TuiMode::Plan,
                task: Some("latest request".to_string()),
            }]
        );
        assert_eq!(app.status, "system prompt queued: /tmp/deepseek-system");

        app.composer_focused = true;
        app.composer = "/system help".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        let (kind, detail) = app.mcp_detail.as_ref().expect("system help detail");
        assert_eq!(*kind, TuiMcpDetailKind::System);
        assert!(detail.contains("/system shows the selected workspace"));
        assert!(detail.contains("/tmp/deepseek-system"));
        assert_eq!(app.status, "system help shown");
        assert_eq!(app.composer, "");

        app.composer_focused = false;
        run_palette_command(&mut app, "system extra");
        assert_eq!(
            app.status,
            "usage: system or /system; use system help for details"
        );
    }

    #[test]
    fn edit_command_loads_latest_user_message_into_composer() {
        let sessions = vec![TuiSession {
            id: "session-1".to_string(),
            title: "Session".to_string(),
            workspace: "/tmp/deepseek-edit".to_string(),
            status: "active".to_string(),
            active_thread_id: Some("thread-1".to_string()),
            thread_count: 1,
        }];
        let threads = vec![TuiThread {
            id: "thread-1".to_string(),
            session_id: Some("session-1".to_string()),
            title: "Thread".to_string(),
            mode: "agent".to_string(),
            status: "idle".to_string(),
            latest_turn_id: Some("turn-2".to_string()),
            event_seq: 0,
        }];
        let items = vec![
            TuiItem {
                id: "item-1".to_string(),
                thread_id: "thread-1".to_string(),
                turn_id: Some("turn-1".to_string()),
                index: 1,
                item_type: "message".to_string(),
                role: Some("user".to_string()),
                content: "older request".to_string(),
                status: "completed".to_string(),
            },
            TuiItem {
                id: "item-2".to_string(),
                thread_id: "thread-1".to_string(),
                turn_id: Some("turn-2".to_string()),
                index: 2,
                item_type: "message".to_string(),
                role: Some("user".to_string()),
                content: "latest request".to_string(),
                status: "completed".to_string(),
            },
        ];
        let mut app = TuiApp::with_runtime(sessions, threads, items);

        app.composer_focused = true;
        app.composer = "/edit".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        assert_eq!(app.composer, "latest request");
        assert_eq!(app.composer_cursor, app.composer.len());
        assert!(app.composer_focused);
        assert_eq!(app.status, "last user message loaded into composer");
        assert!(app.drain_actions().is_empty());

        app.composer_focused = false;
        run_palette_command(&mut app, "edit help");
        let (kind, detail) = app.mcp_detail.as_ref().expect("edit help detail");
        assert_eq!(*kind, TuiMcpDetailKind::Edit);
        assert!(detail.contains("/edit loads the latest selected user message"));

        run_palette_command(&mut app, "edit extra");
        assert_eq!(
            app.status,
            "usage: edit or /edit; use edit help for details"
        );

        let mut empty = TuiApp::new(Vec::new());
        run_palette_command(&mut empty, "edit");
        assert_eq!(empty.status, "no previous message to edit");
    }

    #[test]
    fn clear_command_queues_durable_session_reset() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: "/workspace/project".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        run_palette_command(&mut app, "clear");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::ClearConversation {
                session_id: "session-one".to_string(),
                previous_thread_id: Some("thread-one".to_string()),
            }]
        );

        app.composer_focused = true;
        app.composer = "/clear help".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        let (kind, detail) = app.mcp_detail.as_ref().expect("clear help detail");
        assert_eq!(*kind, TuiMcpDetailKind::Clear);
        assert!(detail.contains("/clear starts a fresh active thread"));
        assert_eq!(app.composer, "");
    }

    #[test]
    fn diff_command_queues_workspace_diff() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: "/workspace/project".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        run_palette_command(&mut app, "diff");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::ShowDiff {
                workspace: "/workspace/project".to_string(),
            }]
        );

        app.composer_focused = true;
        app.composer = "/diff help".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        let (kind, detail) = app.mcp_detail.as_ref().expect("diff help detail");
        assert_eq!(*kind, TuiMcpDetailKind::Diff);
        assert!(detail.contains("/diff shows changed tracked files"));
        assert_eq!(app.composer, "");
    }

    #[test]
    fn subagents_command_lists_and_queues_runtime_subagent_tasks() {
        let mut subagent = runtime_task(
            "task-subagent",
            "pending",
            "max_depth=2: inspect parity gap",
            "epoch+2",
        );
        subagent.kind = "subagent".to_string();
        let agent = runtime_task("task-agent", "running", "normal agent task", "epoch+1");
        let mut app = app_with_runtime_tasks(vec![subagent, agent]);

        run_palette_command(&mut app, "subagents");
        let (kind, detail) = app.mcp_detail.as_ref().expect("subagents detail");
        assert_eq!(*kind, TuiMcpDetailKind::Subagents);
        assert!(detail.contains("Sub-agent tasks:"));
        assert!(detail.contains("task-subagent"));
        assert!(detail.contains("max_depth=2: inspect parity gap"));
        assert!(!detail.contains("normal agent task"));

        run_palette_command(&mut app, "agent 2 inspect parity gap");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::CreateSubagentTask {
                thread_id: "thread-one".to_string(),
                task: "inspect parity gap".to_string(),
                max_depth: 2,
            }]
        );

        run_palette_command(&mut app, "agent 4 invalid depth");
        assert!(app.drain_actions().is_empty());
        assert_eq!(app.status, "agent depth must be between 0 and 3");

        app.composer_focused = true;
        app.composer = "/agent inspect repo".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::CreateSubagentTask {
                thread_id: "thread-one".to_string(),
                task: "inspect repo".to_string(),
                max_depth: 1,
            }]
        );
        assert_eq!(app.composer, "");

        app.composer_focused = true;
        app.composer = "/agents help".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        let (kind, detail) = app.mcp_detail.as_ref().expect("subagents help detail");
        assert_eq!(*kind, TuiMcpDetailKind::Subagents);
        assert!(detail.contains("/agent [0-3] <task>"));
    }

    #[test]
    fn rlm_command_queues_persistent_process_prompt() {
        let root = temp_root("rlm-command");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("notes.txt"), "persistent context").unwrap();
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: root.display().to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        run_palette_command(&mut app, "rlm 2 analyze src/main.rs");
        let actions = app.drain_actions();
        assert_eq!(actions.len(), 1);
        let TuiAction::SubmitUserMessage { thread_id, content } = &actions[0] else {
            panic!("expected rlm submit action");
        };
        assert_eq!(thread_id, "thread-one");
        assert!(content.contains("`rlm_process`"));
        assert!(content.contains("live=true"));
        assert!(content.contains("session_id: \"slash_rlm_thread-one\""));
        assert!(content.contains("content: \"analyze src/main.rs\""));
        assert!(content.contains("max_depth: 2"));

        run_palette_command(&mut app, "rlm 0 notes.txt");
        let actions = app.drain_actions();
        let TuiAction::SubmitUserMessage { content, .. } = &actions[0] else {
            panic!("expected rlm file submit action");
        };
        assert!(content.contains("file_path: \"notes.txt\""));
        assert!(content.contains("max_depth: 0"));

        run_palette_command(&mut app, "rlm 4 invalid depth");
        assert!(app.drain_actions().is_empty());
        assert_eq!(app.status, "rlm depth must be between 0 and 3");

        app.composer_focused = true;
        app.composer = "/recursive inspect project".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        let actions = app.drain_actions();
        let TuiAction::SubmitUserMessage { content, .. } = &actions[0] else {
            panic!("expected recursive submit action");
        };
        assert!(content.contains("max_depth: 1"));
        assert!(content.contains("content: \"inspect project\""));
        assert_eq!(app.composer, "");

        app.composer_focused = true;
        app.composer = "/rlm help".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        let (kind, detail) = app.mcp_detail.as_ref().expect("rlm help detail");
        assert_eq!(*kind, TuiMcpDetailKind::Rlm);
        assert!(detail.contains("/rlm [0-3] <file_or_text>"));
        assert!(detail.contains("/recursive [0-3] <file_or_text>"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn relay_command_queues_session_handoff_prompt() {
        let mut app = app_with_runtime_tasks(vec![runtime_task(
            "task-relay",
            "running",
            "finish relay command parity",
            "epoch+2",
        )]);
        app.goal_objective = Some("Close DeepSeek-TUI relay gap".to_string());
        app.goal_token_budget = Some(2000);

        run_palette_command(&mut app, "relay verify install");
        let actions = app.drain_actions();
        assert_eq!(actions.len(), 1);
        let TuiAction::SubmitUserMessage { thread_id, content } = &actions[0] else {
            panic!("expected relay submit action");
        };
        assert_eq!(thread_id, "thread-one");
        assert!(content.contains("session relay"));
        assert!(content.contains("接力"));
        assert!(content.contains("Write or update `.dscode/handoff.md`"));
        assert!(content.contains("Requested relay focus: verify install"));
        assert!(content.contains("Goal: Close DeepSeek-TUI relay gap"));
        assert!(content.contains("task-relay [running] finish relay command parity"));
        assert!(content.contains("# Session relay"));

        app.composer_focused = true;
        app.composer = "/接力 next hand".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        let actions = app.drain_actions();
        let TuiAction::SubmitUserMessage { content, .. } = &actions[0] else {
            panic!("expected relay alias submit action");
        };
        assert!(content.contains("Requested relay focus: next hand"));
        assert_eq!(app.composer, "");

        app.composer_focused = true;
        app.composer = "/batonpass help".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        let (kind, detail) = app.mcp_detail.as_ref().expect("relay help detail");
        assert_eq!(*kind, TuiMcpDetailKind::Relay);
        assert!(detail.contains("/relay [focus]"));
        assert!(detail.contains("/接力 [focus]"));
    }

    #[test]
    fn command_palette_requests_hooks_actions() {
        let mut app = TuiApp::new(Vec::new());

        run_palette_command(&mut app, "hooks");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Hooks {
                command: TuiHooksCommand::List,
            }]
        );

        run_palette_command(&mut app, "hooks events");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Hooks {
                command: TuiHooksCommand::Events,
            }]
        );

        app.composer_focused = true;
        app.composer = "/hook list".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Hooks {
                command: TuiHooksCommand::List,
            }]
        );
        assert_eq!(app.composer, "");
    }

    #[test]
    fn goal_command_sets_shows_and_clears_budget() {
        let mut app = TuiApp::with_runtime_usage_and_approvals(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: "/workspace/project".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: Some("turn-one".to_string()),
                event_seq: 7,
            }],
            Vec::new(),
            vec![TuiUsageSummary {
                thread_id: "thread-one".to_string(),
                record_count: 2,
                prompt_tokens: 700,
                completion_tokens: 534,
                total_tokens: 1234,
                latest_prompt_tokens: 500,
                latest_completion_tokens: 300,
                latest_total_tokens: 800,
                prompt_cache_hit_tokens: 300,
                prompt_cache_miss_tokens: 100,
                estimated_input_cost_microusd: Some(10),
                estimated_output_cost_microusd: Some(20),
                estimated_total_cost_microusd: Some(30),
                context_remaining_tokens: 999_200,
                context_strategy: "normal".to_string(),
            }],
            Vec::new(),
        );

        run_palette_command(&mut app, "goal Stabilize TUI parity | budget: 2000");
        assert_eq!(app.status, "goal set");
        let (kind, detail) = app.mcp_detail.as_ref().expect("goal detail");
        assert_eq!(*kind, TuiMcpDetailKind::Goal);
        assert!(detail.contains("Objective:"));
        assert!(detail.contains("Stabilize TUI parity"));
        assert!(detail.contains("Token budget:"));
        assert!(detail.contains("2000"));
        assert!(detail.contains("1234 (62%)"));

        run_palette_command(&mut app, "goal");
        assert_eq!(app.status, "goal shown");
        let (_, detail) = app.mcp_detail.as_ref().expect("goal detail");
        assert!(detail.contains("Stabilize TUI parity"));

        app.composer_focused = true;
        app.composer = "/goal clear".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));
        assert_eq!(app.status, "goal cleared");
        assert_eq!(app.composer, "");
        let (kind, detail) = app.mcp_detail.as_ref().expect("goal detail");
        assert_eq!(*kind, TuiMcpDetailKind::Goal);
        assert!(detail.contains("No goal set"));
    }

    #[test]
    fn command_palette_requests_network_actions() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: "/tmp/deepseek-network".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        run_palette_command(&mut app, "network allow api.example.com");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Network {
                workspace: "/tmp/deepseek-network".to_string(),
                command: TuiNetworkCommand::Allow {
                    host: "api.example.com".to_string(),
                },
            }]
        );

        run_palette_command(&mut app, "/network default prompt");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Network {
                workspace: "/tmp/deepseek-network".to_string(),
                command: TuiNetworkCommand::Default {
                    value: "prompt".to_string(),
                },
            }]
        );
    }

    #[test]
    fn command_palette_requests_lsp_actions() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: "/tmp/deepseek-lsp".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        run_palette_command(&mut app, "lsp");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Lsp {
                workspace: "/tmp/deepseek-lsp".to_string(),
                command: TuiLspCommand::Status,
            }]
        );

        run_palette_command(&mut app, "/lsp on");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Lsp {
                workspace: "/tmp/deepseek-lsp".to_string(),
                command: TuiLspCommand::Set { enabled: true },
            }]
        );

        run_palette_command(&mut app, "lsp off");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Lsp {
                workspace: "/tmp/deepseek-lsp".to_string(),
                command: TuiLspCommand::Set { enabled: false },
            }]
        );

        run_palette_command(&mut app, "/lsp help");
        let (kind, detail) = app.mcp_detail.as_ref().expect("lsp help detail");
        assert_eq!(*kind, TuiMcpDetailKind::Lsp);
        assert!(detail.contains("/lsp [on|off|status]"));
        assert!(detail.contains("Selected workspace:"));

        run_palette_command(&mut app, "lsp maybe");
        assert_eq!(
            app.status,
            "usage: lsp [on|off|status] or /lsp [on|off|status]"
        );
    }

    #[test]
    fn command_palette_requests_model_actions() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: "/tmp/deepseek-model".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        run_palette_command(&mut app, "model");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Model {
                workspace: "/tmp/deepseek-model".to_string(),
                command: TuiModelCommand::Show,
            }]
        );

        run_palette_command(&mut app, "/model auto");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Model {
                workspace: "/tmp/deepseek-model".to_string(),
                command: TuiModelCommand::Set {
                    model: "auto".to_string(),
                },
            }]
        );

        run_palette_command(&mut app, "models");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Model {
                workspace: "/tmp/deepseek-model".to_string(),
                command: TuiModelCommand::List,
            }]
        );
    }

    #[test]
    fn command_palette_requests_provider_actions() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: "/tmp/deepseek-provider".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        run_palette_command(&mut app, "provider");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Provider {
                workspace: "/tmp/deepseek-provider".to_string(),
                command: TuiProviderCommand::Show,
            }]
        );

        run_palette_command(&mut app, "/provider nvidia-nim flash");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Provider {
                workspace: "/tmp/deepseek-provider".to_string(),
                command: TuiProviderCommand::Set {
                    provider: "nvidia-nim".to_string(),
                    model: Some("flash".to_string()),
                },
            }]
        );

        run_palette_command(&mut app, "provider list");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Provider {
                workspace: "/tmp/deepseek-provider".to_string(),
                command: TuiProviderCommand::List,
            }]
        );
    }

    #[test]
    fn command_palette_requests_skills_actions() {
        let mut app = TuiApp::new(Vec::new());

        run_palette_command(&mut app, "skills pr");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Skills {
                command: TuiSkillsCommand::List {
                    prefix: Some("pr".to_string()),
                },
            }]
        );

        run_palette_command(&mut app, "skill pr-review");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Skills {
                command: TuiSkillsCommand::Show {
                    name: "pr-review".to_string(),
                },
            }]
        );
    }

    #[test]
    fn feedback_command_renders_links() {
        let mut app = TuiApp::new(Vec::new());

        run_palette_command(&mut app, "feedback");

        assert_eq!(app.status, "feedback options shown");
        let (kind, detail) = app.mcp_detail.as_ref().expect("feedback detail");
        assert_eq!(*kind, TuiMcpDetailKind::Feedback);
        assert!(detail.contains("DeepSeekCode Feedback"));
        assert!(detail.contains("feedback bug"));
        assert!(detail.contains(DEEPSEEK_CODE_FEATURE_URL));

        run_palette_command(&mut app, "/feedback security");

        assert_eq!(app.status, "feedback security link shown");
        let (_, detail) = app.mcp_detail.as_ref().expect("security detail");
        assert!(detail.contains("Security report"));
        assert!(detail.contains(DEEPSEEK_CODE_SECURITY_URL));
    }

    #[test]
    fn links_command_renders_repository_and_api_links() {
        let mut app = TuiApp::new(Vec::new());

        run_palette_command(&mut app, "dashboard");

        assert_eq!(app.status, "links shown");
        let (kind, detail) = app.mcp_detail.as_ref().expect("links detail");
        assert_eq!(*kind, TuiMcpDetailKind::Links);
        assert!(detail.contains("DeepSeekCode Links"));
        assert!(detail.contains(DEEPSEEK_CODE_REPO_URL));
        assert!(detail.contains(DEEPSEEK_API_DOCS_URL));

        run_palette_command(&mut app, "/api");

        let (kind, detail) = app.mcp_detail.as_ref().expect("api detail");
        assert_eq!(*kind, TuiMcpDetailKind::Links);
        assert!(detail.contains(DEEPSEEK_PLATFORM_URL));

        app.composer_focused = true;
        app.composer = "/links".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(app.status, "links shown");
        assert_eq!(app.composer, "");
        let (kind, detail) = app.mcp_detail.as_ref().expect("composer links detail");
        assert_eq!(*kind, TuiMcpDetailKind::Links);
        assert!(detail.contains("Aliases: links, dashboard, api"));
    }

    #[test]
    fn home_command_renders_runtime_dashboard() {
        let mut app = TuiApp::with_runtime_usage_tasks_and_approvals(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: "/tmp/deepseek-home".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "running".to_string(),
                latest_turn_id: Some("turn-one".to_string()),
                event_seq: 7,
            }],
            vec![TuiItem {
                id: "item-one".to_string(),
                thread_id: "thread-one".to_string(),
                turn_id: Some("turn-one".to_string()),
                index: 0,
                item_type: "message".to_string(),
                role: Some("user".to_string()),
                content: "hello".to_string(),
                status: "completed".to_string(),
            }],
            vec![TuiTaskRecord {
                id: "task-one".to_string(),
                session_id: Some("session-one".to_string()),
                thread_id: Some("thread-one".to_string()),
                parent_task_id: None,
                kind: "agent".to_string(),
                status: "running".to_string(),
                summary: "test task".to_string(),
                updated_at: "epoch".to_string(),
            }],
            vec![TuiUsageSummary {
                thread_id: "thread-one".to_string(),
                record_count: 1,
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                latest_prompt_tokens: 10,
                latest_completion_tokens: 5,
                latest_total_tokens: 15,
                prompt_cache_hit_tokens: 3,
                prompt_cache_miss_tokens: 7,
                estimated_input_cost_microusd: Some(1),
                estimated_output_cost_microusd: Some(2),
                estimated_total_cost_microusd: Some(3),
                context_remaining_tokens: TUI_CONTEXT_WINDOW_TOKENS - 15,
                context_strategy: "normal".to_string(),
            }],
            Vec::new(),
        );

        run_palette_command(&mut app, "overview");

        assert_eq!(app.status, "home dashboard shown");
        let (kind, detail) = app.mcp_detail.as_ref().expect("home detail");
        assert_eq!(*kind, TuiMcpDetailKind::Home);
        assert!(detail.contains("DeepSeekCode Home"));
        assert!(detail.contains("/tmp/deepseek-home"));
        assert!(detail.contains("15 total, 15 latest"));
        assert!(detail.contains("/links"));
        assert!(detail.contains("Aliases: home, stats, overview"));

        app.composer_focused = true;
        app.composer = "/stats".to_string();
        app.composer_cursor = app.composer.len();
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(app.status, "home dashboard shown");
        assert_eq!(app.composer, "");
        let (kind, _) = app.mcp_detail.as_ref().expect("composer home detail");
        assert_eq!(*kind, TuiMcpDetailKind::Home);
    }

    #[test]
    fn command_palette_requests_mcp_server_mutations() {
        let mut app = TuiApp::new(Vec::new());

        run_palette_command(&mut app, "mcp add stdio fs npx -y @mcp/server .");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpAddStdio {
                scope: TuiMcpConfigScope::Project,
                name: "fs".to_string(),
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "@mcp/server".to_string(), ".".to_string()],
            }]
        );

        run_palette_command(&mut app, "mcp add http remote http://127.0.0.1:3000/mcp");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpAddRemote {
                scope: TuiMcpConfigScope::Project,
                name: "remote".to_string(),
                transport: "http".to_string(),
                url: "http://127.0.0.1:3000/mcp".to_string(),
            }]
        );

        run_palette_command(&mut app, "mcp disable remote");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpSetEnabled {
                scope: TuiMcpConfigScope::Project,
                name: "remote".to_string(),
                enabled: false,
            }]
        );

        run_palette_command(&mut app, "mcp enable remote");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpSetEnabled {
                scope: TuiMcpConfigScope::Project,
                name: "remote".to_string(),
                enabled: true,
            }]
        );

        run_palette_command(&mut app, "mcp remove remote");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpRemove {
                scope: TuiMcpConfigScope::Project,
                name: "remote".to_string(),
            }]
        );

        run_palette_command(
            &mut app,
            "mcp user add http shared http://127.0.0.1:3001/mcp",
        );
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpAddRemote {
                scope: TuiMcpConfigScope::User,
                name: "shared".to_string(),
                transport: "http".to_string(),
                url: "http://127.0.0.1:3001/mcp".to_string(),
            }]
        );

        run_palette_command(&mut app, "mcp user disable shared");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::McpSetEnabled {
                scope: TuiMcpConfigScope::User,
                name: "shared".to_string(),
                enabled: false,
            }]
        );
    }

    #[test]
    fn command_palette_rejects_empty_task_create() {
        let mut app = TuiApp::new(Vec::new());

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "task create".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert!(app.drain_actions().is_empty());
        assert_eq!(app.status, "task create requires a summary");
    }

    #[test]
    fn command_palette_records_entered_query() {
        let mut app = TuiApp::new(Vec::new());
        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "mode agent".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(app.mode, TuiMode::Agent);
        assert_eq!(app.status, "mode set: Agent");
        assert!(!app.show_command_palette);
    }

    #[test]
    fn composer_submits_user_message_action_for_active_thread() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        assert!(app.handle_key(KeyCode::Char('i')));
        for ch in "hello from tui".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("Composer [Plan]*: hello from tui"));
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(app.composer, "");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::SubmitUserMessage {
                thread_id: "thread-one".to_string(),
                content: "hello from tui".to_string(),
            }]
        );
    }

    #[test]
    fn composer_intercepts_memory_prefix_and_slash_commands() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        assert!(app.handle_key(KeyCode::Char('i')));
        for ch in "# prefer cargo fmt before commits".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(app.composer, "");
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::AppendMemory {
                note: "# prefer cargo fmt before commits".to_string(),
            }]
        );

        for ch in "/memory path".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Memory {
                command: TuiMemoryCommand::Path,
            }]
        );

        for ch in "/rename Focused Session".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::RenameSession {
                session_id: "session-one".to_string(),
                title: "Focused Session".to_string(),
            }]
        );

        for ch in "/init".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::InitProjectInstructions {
                workspace: ".".to_string(),
            }]
        );

        for ch in "/network deny telemetry.example.com".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Network {
                workspace: ".".to_string(),
                command: TuiNetworkCommand::Deny {
                    host: "telemetry.example.com".to_string(),
                },
            }]
        );

        for ch in "/model auto".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Model {
                workspace: ".".to_string(),
                command: TuiModelCommand::Set {
                    model: "auto".to_string(),
                },
            }]
        );

        for ch in "/provider openrouter pro".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Provider {
                workspace: ".".to_string(),
                command: TuiProviderCommand::Set {
                    provider: "openrouter".to_string(),
                    model: Some("pro".to_string()),
                },
            }]
        );

        for ch in "/skills pr".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Skills {
                command: TuiSkillsCommand::List {
                    prefix: Some("pr".to_string()),
                },
            }]
        );

        for ch in "/skill pr-review".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::Skills {
                command: TuiSkillsCommand::Show {
                    name: "pr-review".to_string(),
                },
            }]
        );

        for ch in "/status".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert!(app.drain_actions().is_empty());
        assert_eq!(
            app.mcp_detail.as_ref().map(|(kind, _)| *kind),
            Some(TuiMcpDetailKind::Status)
        );

        for ch in "/tokens".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert!(app.drain_actions().is_empty());
        assert_eq!(
            app.mcp_detail.as_ref().map(|(kind, _)| *kind),
            Some(TuiMcpDetailKind::Tokens)
        );

        for ch in "/cost".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert!(app.drain_actions().is_empty());
        assert_eq!(
            app.mcp_detail.as_ref().map(|(kind, _)| *kind),
            Some(TuiMcpDetailKind::Cost)
        );

        for ch in "/cache 5".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert!(app.drain_actions().is_empty());
        assert_eq!(
            app.mcp_detail.as_ref().map(|(kind, _)| *kind),
            Some(TuiMcpDetailKind::Cache)
        );

        for ch in "/review src/lib.rs".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::RunCustomSlashCommand {
                thread_id: "thread-one".to_string(),
                command: "/review".to_string(),
                args: vec!["src/lib.rs".to_string()],
            }]
        );

        for ch in "## markdown heading".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::SubmitUserMessage {
                thread_id: "thread-one".to_string(),
                content: "## markdown heading".to_string(),
            }]
        );
    }

    #[test]
    fn status_command_renders_active_runtime_summary() {
        let mut app = TuiApp::with_runtime_usage_tasks_automations_approvals_and_user_inputs(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: "/workspace/project".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 2,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "running".to_string(),
                latest_turn_id: Some("turn-one".to_string()),
                event_seq: 42,
            }],
            vec![TuiItem {
                id: "item-one".to_string(),
                thread_id: "thread-one".to_string(),
                turn_id: Some("turn-one".to_string()),
                index: 0,
                item_type: "message".to_string(),
                role: Some("assistant".to_string()),
                content: "streaming answer".to_string(),
                status: "running".to_string(),
            }],
            vec![TuiTaskRecord {
                id: "task-one".to_string(),
                session_id: Some("session-one".to_string()),
                thread_id: Some("thread-one".to_string()),
                parent_task_id: None,
                kind: "agent".to_string(),
                status: "pending".to_string(),
                summary: "review parity".to_string(),
                updated_at: "2026-05-14T00:00:00Z".to_string(),
            }],
            vec![TuiAutomationRecord {
                id: "automation-one".to_string(),
                session_id: Some("session-one".to_string()),
                thread_id: Some("thread-one".to_string()),
                name: "daily".to_string(),
                status: "active".to_string(),
                schedule: "daily".to_string(),
                prompt: "check".to_string(),
                updated_at: "2026-05-14T00:00:00Z".to_string(),
                last_run_at: None,
                next_run_at: None,
            }],
            vec![TuiUsageSummary {
                thread_id: "thread-one".to_string(),
                record_count: 2,
                prompt_tokens: 700,
                completion_tokens: 534,
                total_tokens: 1234,
                latest_prompt_tokens: 500,
                latest_completion_tokens: 300,
                latest_total_tokens: 800,
                prompt_cache_hit_tokens: 300,
                prompt_cache_miss_tokens: 100,
                estimated_input_cost_microusd: Some(10),
                estimated_output_cost_microusd: Some(20),
                estimated_total_cost_microusd: Some(30),
                context_remaining_tokens: 999_200,
                context_strategy: "normal".to_string(),
            }],
            vec![TuiApprovalRequest {
                id: "approval-one".to_string(),
                thread_id: "thread-one".to_string(),
                turn_id: Some("turn-one".to_string()),
                tool: "shell".to_string(),
                kind: "shell".to_string(),
                target: "cargo test".to_string(),
                status: "pending".to_string(),
            }],
            vec![TuiUserInputRequest {
                id: "input-one".to_string(),
                thread_id: "thread-one".to_string(),
                turn_id: Some("turn-one".to_string()),
                status: "pending".to_string(),
                questions: vec![TuiUserInputQuestion {
                    header: "Mode".to_string(),
                    id: "mode".to_string(),
                    question: "Pick mode".to_string(),
                    options: vec![TuiUserInputOption {
                        label: "Agent".to_string(),
                        description: "Run".to_string(),
                    }],
                }],
            }],
        );

        app.execute_palette_command("status");

        assert_eq!(app.status, "status detail refreshed");
        let (kind, detail) = app.mcp_detail.as_ref().expect("status detail");
        assert_eq!(*kind, TuiMcpDetailKind::Status);
        assert!(detail.contains("DeepSeekCode TUI Status"));
        assert!(detail.contains("Session:"));
        assert!(detail.contains("One [active]"));
        assert!(detail.contains("First thread [running]"));
        assert!(detail.contains("Task states:"));
        assert!(detail.contains("pending=1"));
        assert!(detail.contains("Approvals:"));
        assert!(detail.contains("1 active, 1 pending total"));
        assert!(detail.contains("Total tokens:"));
        assert!(detail.contains("1234"));
        assert!(detail.contains("Est. cost:"));
        assert!(detail.contains("$0.000030"));
    }

    #[test]
    fn tokens_and_cost_commands_render_usage_details() {
        let mut app = TuiApp::with_runtime_usage_and_approvals(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: "/workspace/project".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: Some("turn-one".to_string()),
                event_seq: 7,
            }],
            Vec::new(),
            vec![TuiUsageSummary {
                thread_id: "thread-one".to_string(),
                record_count: 2,
                prompt_tokens: 700,
                completion_tokens: 534,
                total_tokens: 1234,
                latest_prompt_tokens: 500,
                latest_completion_tokens: 300,
                latest_total_tokens: 800,
                prompt_cache_hit_tokens: 300,
                prompt_cache_miss_tokens: 100,
                estimated_input_cost_microusd: Some(10),
                estimated_output_cost_microusd: Some(20),
                estimated_total_cost_microusd: Some(30),
                context_remaining_tokens: 999_200,
                context_strategy: "normal".to_string(),
            }],
            Vec::new(),
        );

        app.execute_palette_command("tokens");

        assert_eq!(app.status, "tokens detail refreshed");
        let (kind, detail) = app.mcp_detail.as_ref().expect("tokens detail");
        assert_eq!(*kind, TuiMcpDetailKind::Tokens);
        assert!(detail.contains("DeepSeekCode Token Usage"));
        assert!(detail.contains("Last API input:"));
        assert!(detail.contains("500"));
        assert!(detail.contains("Prompt tokens:"));
        assert!(detail.contains("700 cumulative"));
        assert!(detail.contains("Approx cost:"));
        assert!(detail.contains("$0.000030"));

        app.execute_palette_command("cost");

        assert_eq!(app.status, "cost detail refreshed");
        let (kind, detail) = app.mcp_detail.as_ref().expect("cost detail");
        assert_eq!(*kind, TuiMcpDetailKind::Cost);
        assert!(detail.contains("DeepSeekCode Session Cost"));
        assert!(detail.contains("Approx total:"));
        assert!(detail.contains("$0.000030"));
        assert!(detail.contains("Input cost:"));
        assert!(detail.contains("$0.000010"));
        assert!(detail.contains("Output cost:"));
        assert!(detail.contains("$0.000020"));
    }

    #[test]
    fn cache_command_renders_usage_details_and_read_only_modes() {
        let mut app = TuiApp::with_runtime_usage_and_approvals(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: "/workspace/project".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: Some("turn-one".to_string()),
                event_seq: 7,
            }],
            Vec::new(),
            vec![TuiUsageSummary {
                thread_id: "thread-one".to_string(),
                record_count: 2,
                prompt_tokens: 700,
                completion_tokens: 534,
                total_tokens: 1234,
                latest_prompt_tokens: 500,
                latest_completion_tokens: 300,
                latest_total_tokens: 800,
                prompt_cache_hit_tokens: 300,
                prompt_cache_miss_tokens: 100,
                estimated_input_cost_microusd: Some(10),
                estimated_output_cost_microusd: Some(20),
                estimated_total_cost_microusd: Some(30),
                context_remaining_tokens: 999_200,
                context_strategy: "normal".to_string(),
            }],
            Vec::new(),
        );

        app.execute_palette_command("cache 5");

        assert_eq!(app.status, "cache detail refreshed");
        let (kind, detail) = app.mcp_detail.as_ref().expect("cache detail");
        assert_eq!(*kind, TuiMcpDetailKind::Cache);
        assert!(detail.contains("DeepSeekCode Cache Telemetry"));
        assert!(detail.contains("Requested turns:"));
        assert!(detail.contains("5"));
        assert!(detail.contains("Cache hit:"));
        assert!(detail.contains("300"));
        assert!(detail.contains("Hit rate:"));
        assert!(detail.contains("75.00%"));
        assert!(detail.contains("Cache chart:"));

        app.execute_palette_command("cache inspect");
        let (_, detail) = app.mcp_detail.as_ref().expect("cache inspect detail");
        assert!(detail.contains("DeepSeekCode Cache Inspect"));
        assert!(detail.contains("prompt layer hashes"));

        app.execute_palette_command("cache warmup");
        let (_, detail) = app.mcp_detail.as_ref().expect("cache warmup detail");
        assert!(detail.contains("DeepSeekCode Cache Warmup"));
        assert!(detail.contains("does not send"));
    }

    #[test]
    fn composer_supports_cursor_editing() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        assert!(app.handle_key(KeyCode::Char('i')));
        for ch in "helo".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Left));
        assert!(app.handle_key(KeyCode::Char('l')));

        assert_eq!(app.composer, "hello");
        assert_eq!(app.composer_cursor, 4);
        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("Composer [Plan]*: hell|o"));

        assert!(app.handle_key(KeyCode::Home));
        assert!(app.handle_key(KeyCode::Char('>')));
        assert!(app.handle_key(KeyCode::End));
        assert!(app.handle_key(KeyCode::Backspace));

        assert_eq!(app.composer, ">hell");
        assert_eq!(app.composer_cursor, app.composer.len());
    }

    #[test]
    fn composer_slash_tab_completes_and_renders_hints() {
        let mut app = TuiApp::new(Vec::new());

        assert!(app.handle_key(KeyCode::Char('i')));
        for ch in "/pro".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }

        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("Slash: /provider"));

        assert!(app.handle_key(KeyCode::Tab));
        assert_eq!(app.composer, "/provider");
        assert_eq!(app.composer_cursor, "/provider".len());
        assert!(app.status.contains("slash prefix completed"));

        assert!(app.handle_key(KeyCode::Char(' ')));
        assert!(app.handle_key(KeyCode::Char('n')));
        assert!(app.handle_key(KeyCode::Tab));
        assert_eq!(app.composer, "/provider nvidia-nim");
    }

    #[test]
    fn composer_slash_hints_include_project_custom_commands() {
        let root = temp_root("slash-custom-hints");
        let command_dir = root.join(".dscode/commands/pr");
        fs::create_dir_all(&command_dir).unwrap();
        fs::write(command_dir.join("fix.md"), "Review PR fixes for $ARGUMENTS").unwrap();
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: root.display().to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        assert!(app.handle_key(KeyCode::Char('i')));
        for ch in "/pr".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("Slash: /pr/fix"));

        assert!(app.handle_key(KeyCode::Char('/')));
        assert!(app.handle_key(KeyCode::Tab));
        assert_eq!(app.composer, "/pr/fix");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn composer_slash_hints_include_extra_configured_entries() {
        let mut app = TuiApp::new(Vec::new());
        app.set_extra_slash_completions(vec![
            "/global/fix".to_string(),
            "/skill pr-review".to_string(),
            "not-a-slash".to_string(),
        ]);

        assert!(app.handle_key(KeyCode::Char('i')));
        for ch in "/glo".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("Slash: /global/fix"));
        assert!(!output.contains("not-a-slash"));

        assert!(app.handle_key(KeyCode::Tab));
        assert_eq!(app.composer, "/global/fix");

        app.composer.clear();
        app.composer_cursor = 0;
        for ch in "/skill pr".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Tab));
        assert_eq!(app.composer, "/skill pr-review");
    }

    #[test]
    fn composer_supports_control_key_editing() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        assert!(app.handle_key(KeyCode::Char('i')));
        for ch in "alpha beta gamma".chars() {
            assert!(app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)));
        }
        assert!(app.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL,)));
        assert_eq!(app.composer_cursor, 0);
        assert!(app.handle_key(KeyCode::Char('>')));
        assert!(app.handle_key_event(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL,)));
        assert_eq!(app.composer_cursor, app.composer.len());
        assert!(app.handle_key_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL,)));
        assert_eq!(app.composer, ">alpha beta ");
        assert!(app.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL,)));
        assert_eq!(app.composer, "");
        assert_eq!(app.composer_cursor, 0);
    }

    #[test]
    fn composer_ctrl_s_stashes_and_palette_pop_restores_draft() {
        let mut app = TuiApp::new(Vec::new());

        assert!(app.handle_key(KeyCode::Char('i')));
        for ch in "draft for later".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL,)));

        assert_eq!(app.composer, "");
        assert_eq!(app.status, "draft stashed; use stash pop to restore");

        assert!(app.handle_key(KeyCode::Esc));
        run_palette_command(&mut app, "stash list");
        let detail = app
            .mcp_detail
            .as_ref()
            .map(|(_, detail)| detail.as_str())
            .unwrap_or("");
        assert!(detail.contains("Composer stash: 1 draft(s)"));
        assert!(detail.contains("draft for later"));

        run_palette_command(&mut app, "stash pop");

        assert_eq!(app.composer, "draft for later");
        assert_eq!(app.composer_cursor, app.composer.len());
        assert!(app.composer_focused);
        assert_eq!(app.status, "restored stashed draft; stash now empty");
    }

    #[test]
    fn composer_stash_persists_to_configured_file() {
        let root = temp_root("composer-stash");
        let path = root.join("tui/composer-stash.json");
        let mut app = TuiApp::new(Vec::new());
        app.enable_composer_stash(path.clone());

        assert!(app.handle_key(KeyCode::Char('i')));
        for ch in "persisted draft".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL,)));

        let mut restored = TuiApp::new(Vec::new());
        restored.enable_composer_stash(path);
        run_palette_command(&mut restored, "/stash pop");

        assert_eq!(restored.composer, "persisted draft");
        assert!(restored.composer_focused);
    }

    #[test]
    fn command_palette_supports_cursor_editing() {
        let mut app = TuiApp::new(Vec::new());
        app.mode = TuiMode::Yolo;

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "mode pln".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Left));
        assert!(app.handle_key(KeyCode::Char('a')));
        assert_eq!(app.command_query, "mode plan");

        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("> mode pla|n"));

        assert!(app.handle_key(KeyCode::Enter));
        assert_eq!(app.mode, TuiMode::Plan);
        assert_eq!(app.status, "mode set: Plan");
    }

    #[test]
    fn command_palette_control_keys_edit_without_triggering_global_modes() {
        let mut app = TuiApp::new(Vec::new());
        app.mode = TuiMode::Plan;

        assert!(app.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL,)));
        assert_eq!(app.mode, TuiMode::Plan);

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "mode pln".chars() {
            assert!(app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)));
        }
        assert!(app.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL,)));
        assert_eq!(app.command_cursor, 0);
        assert!(app.handle_key(KeyCode::Char('#')));
        assert!(app.handle_key_event(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL,)));
        assert!(app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL,)));
        assert!(app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL,)));
        assert_eq!(app.command_query, "#mode ");
        for ch in "plan".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert_eq!(app.command_query, "#mode plan");
    }

    #[test]
    fn command_palette_browses_history_with_up_and_down() {
        let mut app = TuiApp::new(Vec::new());

        run_palette_command(&mut app, "mode agent");
        run_palette_command(&mut app, "mode yolo");
        assert_eq!(
            app.command_history,
            vec!["mode agent".to_string(), "mode yolo".to_string()]
        );

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "draft".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Up));
        assert_eq!(app.command_query, "mode yolo");
        assert!(app.status.contains("2/2"));
        assert!(app.handle_key(KeyCode::Up));
        assert_eq!(app.command_query, "mode agent");
        assert!(app.status.contains("1/2"));
        assert!(app.handle_key(KeyCode::Down));
        assert_eq!(app.command_query, "mode yolo");
        assert!(app.handle_key(KeyCode::Down));
        assert_eq!(app.command_query, "draft");
        assert_eq!(app.command_history_index, None);
    }

    #[test]
    fn command_palette_tab_completes_unique_prefix() {
        let mut app = TuiApp::new(Vec::new());

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "mode a".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Tab));

        assert_eq!(app.command_query, "mode agent");
        assert_eq!(app.command_cursor, app.command_query.len());
        assert_eq!(app.status, "command completed");
    }

    #[test]
    fn command_palette_tab_completes_common_prefix() {
        let mut app = TuiApp::new(Vec::new());

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "mcp man".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Tab));

        assert_eq!(app.command_query, "mcp manager");
        assert_eq!(app.command_cursor, app.command_query.len());
        assert!(app.status.contains("matches"));

        assert!(app.handle_key(KeyCode::Tab));
        assert!(app.command_query.starts_with("mcp manager"));
        assert!(app.status.contains("command completion"));
    }

    #[test]
    fn transcript_scrollback_defaults_to_latest_and_pages_up() {
        let items = (0..20)
            .map(|index| TuiItem {
                id: format!("item-{index:02}"),
                thread_id: "thread-one".to_string(),
                turn_id: None,
                index,
                item_type: "message".to_string(),
                role: Some("assistant".to_string()),
                content: format!("scroll-message-{index:02}"),
                status: "completed".to_string(),
            })
            .collect::<Vec<_>>();
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            items,
        );

        let output = render_once(&app, 140, 14).unwrap();
        assert!(output.contains("scroll-message-19"));
        assert!(!output.contains("scroll-message-00"));

        assert!(app.handle_key(KeyCode::PageUp));
        assert_eq!(app.transcript_scroll, 8);
        assert!(app.status.contains("8 lines"));
        let output = render_once(&app, 140, 14).unwrap();
        assert!(output.contains("scroll-message-08"));
        assert!(!output.contains("scroll-message-19"));

        assert!(app.handle_key(KeyCode::End));
        assert_eq!(app.transcript_scroll, 0);
        let output = render_once(&app, 140, 14).unwrap();
        assert!(output.contains("scroll-message-19"));
    }

    #[test]
    fn session_picker_switches_durable_thread_items_into_transcript() {
        let mut app = TuiApp::with_runtime(
            vec![
                TuiSession {
                    id: "session-one".to_string(),
                    title: "One".to_string(),
                    workspace: ".".to_string(),
                    status: "active".to_string(),
                    active_thread_id: Some("thread-one".to_string()),
                    thread_count: 1,
                },
                TuiSession {
                    id: "session-two".to_string(),
                    title: "Two".to_string(),
                    workspace: ".".to_string(),
                    status: "active".to_string(),
                    active_thread_id: Some("thread-two".to_string()),
                    thread_count: 1,
                },
            ],
            vec![
                TuiThread {
                    id: "thread-one".to_string(),
                    session_id: Some("session-one".to_string()),
                    title: "First thread".to_string(),
                    mode: "agent".to_string(),
                    status: "active".to_string(),
                    latest_turn_id: None,
                    event_seq: 3,
                },
                TuiThread {
                    id: "thread-two".to_string(),
                    session_id: Some("session-two".to_string()),
                    title: "Second thread".to_string(),
                    mode: "agent".to_string(),
                    status: "active".to_string(),
                    latest_turn_id: None,
                    event_seq: 5,
                },
            ],
            vec![
                TuiItem {
                    id: "item-one".to_string(),
                    thread_id: "thread-one".to_string(),
                    turn_id: None,
                    index: 1,
                    item_type: "message".to_string(),
                    role: Some("user".to_string()),
                    content: "hello one".to_string(),
                    status: "completed".to_string(),
                },
                TuiItem {
                    id: "item-two".to_string(),
                    thread_id: "thread-two".to_string(),
                    turn_id: None,
                    index: 1,
                    item_type: "message".to_string(),
                    role: Some("assistant".to_string()),
                    content: "hello two".to_string(),
                    status: "completed".to_string(),
                },
            ],
        );

        assert_eq!(app.selected_thread_id.as_deref(), Some("thread-one"));
        assert!(app.transcript.iter().any(|line| line.contains("hello one")));

        assert!(app.handle_key(KeyCode::Char('s')));
        assert!(app.handle_key(KeyCode::Down));
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(app.selected_thread_id.as_deref(), Some("thread-two"));
        assert!(app.transcript.iter().any(|line| line.contains("hello two")));
        assert!(app.tasks.iter().any(|line| line == "Runtime items: 1"));
        assert!(app.status.contains("selected session: session-two"));
    }

    #[test]
    fn session_picker_supports_page_and_edge_navigation() {
        let sessions = (0..7)
            .map(|index| TuiSession {
                id: format!("session-{index}"),
                title: format!("Session {index}"),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: None,
                thread_count: 0,
            })
            .collect::<Vec<_>>();
        let mut app = TuiApp::with_runtime(sessions, Vec::new(), Vec::new());

        assert!(app.handle_key(KeyCode::Char('s')));
        assert!(app.handle_key(KeyCode::PageDown));
        assert_eq!(app.selected_session, 5);
        assert!(app.handle_key(KeyCode::End));
        assert_eq!(app.selected_session, 6);
        assert!(app.handle_key(KeyCode::PageUp));
        assert_eq!(app.selected_session, 1);
        assert!(app.handle_key(KeyCode::Home));
        assert_eq!(app.selected_session, 0);
    }

    #[test]
    fn session_picker_supports_mouse_selection() {
        let sessions = (0..3)
            .map(|index| TuiSession {
                id: format!("session-{index}"),
                title: format!("Session {index}"),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: None,
                thread_count: 0,
            })
            .collect::<Vec<_>>();
        let mut app = TuiApp::with_runtime(sessions, Vec::new(), Vec::new());
        app.last_frame_area = Rect::new(0, 0, 120, 36);
        let area = session_picker_rect(app.last_frame_area);

        assert!(app.handle_key(KeyCode::Char('s')));
        assert!(app.handle_mouse_event(left_click(area.x + 2, area.y + 2)));

        assert_eq!(app.selected_session, 1);
        assert!(!app.show_session_picker);
        assert_eq!(app.status, "selected session: session-1");
    }

    #[test]
    fn session_picker_filters_sessions_from_command_palette() {
        let mut app = TuiApp::with_runtime(
            vec![
                TuiSession {
                    id: "session-alpha".to_string(),
                    title: "Alpha".to_string(),
                    workspace: "/tmp/alpha".to_string(),
                    status: "active".to_string(),
                    active_thread_id: None,
                    thread_count: 1,
                },
                TuiSession {
                    id: "session-beta".to_string(),
                    title: "Beta".to_string(),
                    workspace: "/tmp/beta".to_string(),
                    status: "paused".to_string(),
                    active_thread_id: None,
                    thread_count: 2,
                },
                TuiSession {
                    id: "session-gamma".to_string(),
                    title: "Gamma".to_string(),
                    workspace: "/tmp/gamma".to_string(),
                    status: "active".to_string(),
                    active_thread_id: None,
                    thread_count: 3,
                },
            ],
            Vec::new(),
            Vec::new(),
        );

        run_palette_command(&mut app, "session filter beta");

        assert!(app.show_session_picker);
        assert_eq!(app.session_picker_filter, "beta");
        assert_eq!(app.selected_session, 1);
        assert!(app.status.contains("session filter: beta (1 match)"));
        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("Filter: beta (1 match)"));
        assert!(output.contains("Beta"));
        assert!(!output.contains("Alpha | active"));

        assert!(app.handle_key(KeyCode::Esc));
        run_palette_command(&mut app, "session filter");

        assert!(app.session_picker_filter.is_empty());
        assert_eq!(app.status, "session filter cleared");
        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("Alpha"));
        assert!(output.contains("Gamma"));
    }

    #[test]
    fn thread_navigator_switches_threads_within_selected_session() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 2,
            }],
            vec![
                TuiThread {
                    id: "thread-one".to_string(),
                    session_id: Some("session-one".to_string()),
                    title: "First thread".to_string(),
                    mode: "agent".to_string(),
                    status: "active".to_string(),
                    latest_turn_id: None,
                    event_seq: 3,
                },
                TuiThread {
                    id: "thread-two".to_string(),
                    session_id: Some("session-one".to_string()),
                    title: "Second thread".to_string(),
                    mode: "agent".to_string(),
                    status: "paused".to_string(),
                    latest_turn_id: None,
                    event_seq: 9,
                },
            ],
            vec![
                TuiItem {
                    id: "item-one".to_string(),
                    thread_id: "thread-one".to_string(),
                    turn_id: None,
                    index: 1,
                    item_type: "message".to_string(),
                    role: Some("user".to_string()),
                    content: "thread one body".to_string(),
                    status: "completed".to_string(),
                },
                TuiItem {
                    id: "item-two".to_string(),
                    thread_id: "thread-two".to_string(),
                    turn_id: None,
                    index: 1,
                    item_type: "message".to_string(),
                    role: Some("assistant".to_string()),
                    content: "thread two body".to_string(),
                    status: "completed".to_string(),
                },
            ],
        );

        assert_eq!(app.selected_thread_id.as_deref(), Some("thread-one"));
        assert!(app.handle_key(KeyCode::Char('t')));
        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("Thread Navigator"));
        assert!(output.contains("Second thread"));
        assert!(app.handle_key(KeyCode::Down));
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(app.selected_thread_id.as_deref(), Some("thread-two"));
        assert!(app
            .transcript
            .iter()
            .any(|line| line.contains("thread two body")));
        assert!(app.status.contains("selected thread: thread-two"));
    }

    #[test]
    fn thread_navigator_supports_page_and_edge_navigation() {
        let threads = (0..7)
            .map(|index| TuiThread {
                id: format!("thread-{index}"),
                session_id: Some("session-one".to_string()),
                title: format!("Thread {index}"),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: index as u64,
            })
            .collect::<Vec<_>>();
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-0".to_string()),
                thread_count: 7,
            }],
            threads,
            Vec::new(),
        );

        assert!(app.handle_key(KeyCode::Char('t')));
        assert!(app.handle_key(KeyCode::PageDown));
        assert_eq!(app.selected_thread_id.as_deref(), Some("thread-5"));
        assert!(app.handle_key(KeyCode::End));
        assert_eq!(app.selected_thread_id.as_deref(), Some("thread-6"));
        assert!(app.handle_key(KeyCode::PageUp));
        assert_eq!(app.selected_thread_id.as_deref(), Some("thread-1"));
        assert!(app.handle_key(KeyCode::Home));
        assert_eq!(app.selected_thread_id.as_deref(), Some("thread-0"));
    }

    #[test]
    fn thread_navigator_supports_mouse_selection_and_scroll() {
        let threads = (0..7)
            .map(|index| TuiThread {
                id: format!("thread-{index}"),
                session_id: Some("session-one".to_string()),
                title: format!("Thread {index}"),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: index as u64,
            })
            .collect::<Vec<_>>();
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-0".to_string()),
                thread_count: 7,
            }],
            threads,
            Vec::new(),
        );
        app.last_frame_area = Rect::new(0, 0, 120, 36);
        let area = thread_picker_rect(app.last_frame_area);

        assert!(app.handle_key(KeyCode::Char('t')));
        assert!(app.handle_mouse_event(scroll_down(area.x + 2, area.y + 2)));
        assert_eq!(app.selected_thread_id.as_deref(), Some("thread-5"));
        assert!(app.handle_mouse_event(left_click(area.x + 2, area.y + 3)));

        assert_eq!(app.selected_thread_id.as_deref(), Some("thread-2"));
        assert!(!app.show_thread_picker);
        assert!(app.status.contains("selected thread: thread-2"));
    }

    #[test]
    fn thread_navigator_filters_threads_from_command_palette() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 3,
            }],
            vec![
                TuiThread {
                    id: "thread-one".to_string(),
                    session_id: Some("session-one".to_string()),
                    title: "First thread".to_string(),
                    mode: "agent".to_string(),
                    status: "active".to_string(),
                    latest_turn_id: None,
                    event_seq: 1,
                },
                TuiThread {
                    id: "thread-two".to_string(),
                    session_id: Some("session-one".to_string()),
                    title: "Paused investigation".to_string(),
                    mode: "agent".to_string(),
                    status: "paused".to_string(),
                    latest_turn_id: Some("turn-two".to_string()),
                    event_seq: 2,
                },
                TuiThread {
                    id: "thread-three".to_string(),
                    session_id: Some("session-one".to_string()),
                    title: "Done thread".to_string(),
                    mode: "plan".to_string(),
                    status: "completed".to_string(),
                    latest_turn_id: None,
                    event_seq: 3,
                },
            ],
            Vec::new(),
        );

        run_palette_command(&mut app, "thread filter paused");

        assert!(app.show_thread_picker);
        assert_eq!(app.thread_picker_filter, "paused");
        assert_eq!(app.selected_thread_id.as_deref(), Some("thread-two"));
        assert!(app.status.contains("thread filter: paused (1 match)"));
        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("Filter: paused (1 match)"));
        assert!(output.contains("Paused investigation"));
        assert!(!output.contains("First thread | active"));

        assert!(app.handle_key(KeyCode::Down));
        assert_eq!(app.selected_thread_id.as_deref(), Some("thread-two"));

        assert!(app.handle_key(KeyCode::Esc));
        run_palette_command(&mut app, "thread filter");

        assert!(app.thread_picker_filter.is_empty());
        assert_eq!(app.status, "thread filter cleared");
    }

    #[test]
    fn command_palette_can_jump_between_threads() {
        let mut app = TuiApp::with_runtime(
            vec![
                TuiSession {
                    id: "session-one".to_string(),
                    title: "One".to_string(),
                    workspace: ".".to_string(),
                    status: "active".to_string(),
                    active_thread_id: Some("thread-one".to_string()),
                    thread_count: 1,
                },
                TuiSession {
                    id: "session-two".to_string(),
                    title: "Two".to_string(),
                    workspace: ".".to_string(),
                    status: "active".to_string(),
                    active_thread_id: Some("thread-two".to_string()),
                    thread_count: 1,
                },
            ],
            vec![
                TuiThread {
                    id: "thread-one".to_string(),
                    session_id: Some("session-one".to_string()),
                    title: "First thread".to_string(),
                    mode: "agent".to_string(),
                    status: "active".to_string(),
                    latest_turn_id: None,
                    event_seq: 1,
                },
                TuiThread {
                    id: "thread-two".to_string(),
                    session_id: Some("session-two".to_string()),
                    title: "Second thread".to_string(),
                    mode: "agent".to_string(),
                    status: "active".to_string(),
                    latest_turn_id: None,
                    event_seq: 2,
                },
            ],
            Vec::new(),
        );

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "thread thread-two".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(app.selected_session, 1);
        assert_eq!(app.selected_thread_id.as_deref(), Some("thread-two"));
        assert!(app.status.contains("selected thread: thread-two"));
    }

    #[test]
    fn command_palette_requests_thread_compaction() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "compact 2".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::CompactThread {
                thread_id: "thread-one".to_string(),
                keep_tail_turns: 2,
            }]
        );
        assert!(app.status.contains("compaction requested"));
    }

    #[test]
    fn command_palette_rejects_invalid_compaction_tail() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            Vec::new(),
        );

        assert!(app.handle_key(KeyCode::Char(':')));
        for ch in "compact never".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert!(app.drain_actions().is_empty());
        assert_eq!(app.status, "invalid compact keep_tail_turns: never");
    }

    #[test]
    fn command_palette_opens_reasoning_detail_and_sets_replay_limit() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: Some("turn-one".to_string()),
                event_seq: 1,
            }],
            vec![
                TuiItem {
                    id: "reasoning-one".to_string(),
                    thread_id: "thread-one".to_string(),
                    turn_id: Some("turn-one".to_string()),
                    index: 1,
                    item_type: "reasoning".to_string(),
                    role: Some("assistant".to_string()),
                    content: "first hidden planning note".to_string(),
                    status: "completed".to_string(),
                },
                TuiItem {
                    id: "message-one".to_string(),
                    thread_id: "thread-one".to_string(),
                    turn_id: Some("turn-one".to_string()),
                    index: 2,
                    item_type: "message".to_string(),
                    role: Some("assistant".to_string()),
                    content: "visible answer".to_string(),
                    status: "completed".to_string(),
                },
            ],
        );

        app.show_command_palette = true;
        app.command_query = "reasoning list".to_string();
        app.command_cursor = app.command_query.len();
        assert!(app.handle_key(KeyCode::Enter));

        let output = render_once(&app, 140, 36).unwrap();
        assert!(output.contains("Reasoning"));
        assert!(output.contains("reasoning-one"));
        assert!(output.contains("first hidden planning note"));
        assert!(output.contains("replay_limit: 3"));
        assert!(app.status.contains("reasoning items=1"));

        app.show_command_palette = true;
        app.command_query = "reasoning replay 5".to_string();
        app.command_cursor = app.command_query.len();
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(app.reasoning_replay_limit(), 5);
        let output = render_once(&app, 140, 36).unwrap();
        assert!(output.contains("replay_limit: 5"));
        assert_eq!(app.status, "reasoning replay limit set to 5");
    }

    #[test]
    fn command_palette_shows_reasoning_item_by_selector() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: Some("turn-two".to_string()),
                event_seq: 2,
            }],
            vec![
                TuiItem {
                    id: "reasoning-one".to_string(),
                    thread_id: "thread-one".to_string(),
                    turn_id: Some("turn-one".to_string()),
                    index: 1,
                    item_type: "reasoning".to_string(),
                    role: Some("assistant".to_string()),
                    content: "older reasoning".to_string(),
                    status: "completed".to_string(),
                },
                TuiItem {
                    id: "reasoning-two".to_string(),
                    thread_id: "thread-one".to_string(),
                    turn_id: Some("turn-two".to_string()),
                    index: 2,
                    item_type: "reasoning".to_string(),
                    role: Some("assistant".to_string()),
                    content: "latest reasoning line one\nlatest reasoning line two".to_string(),
                    status: "running".to_string(),
                },
            ],
        );

        app.show_command_palette = true;
        app.command_query = "reasoning show turn-two".to_string();
        app.command_cursor = app.command_query.len();
        assert!(app.handle_key(KeyCode::Enter));

        let output = render_once(&app, 140, 36).unwrap();
        assert!(output.contains("Reasoning item"));
        assert!(output.contains("position: 2/2"));
        assert!(output.contains("turn_id: turn-two"));
        assert!(output.contains("latest reasoning line one"));
        assert!(output.contains("latest reasoning line two"));
        assert!(app.status.contains("showing reasoning item 2/2"));
    }

    #[test]
    fn command_palette_searches_reasoning_with_highlight() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: Some("turn-two".to_string()),
                event_seq: 2,
            }],
            vec![
                TuiItem {
                    id: "reasoning-one".to_string(),
                    thread_id: "thread-one".to_string(),
                    turn_id: Some("turn-one".to_string()),
                    index: 1,
                    item_type: "reasoning".to_string(),
                    role: Some("assistant".to_string()),
                    content: "alpha planning".to_string(),
                    status: "completed".to_string(),
                },
                TuiItem {
                    id: "reasoning-two".to_string(),
                    thread_id: "thread-one".to_string(),
                    turn_id: Some("turn-two".to_string()),
                    index: 2,
                    item_type: "reasoning".to_string(),
                    role: Some("assistant".to_string()),
                    content: "beta branch analysis".to_string(),
                    status: "completed".to_string(),
                },
            ],
        );

        app.show_command_palette = true;
        app.command_query = "reasoning search beta".to_string();
        app.command_cursor = app.command_query.len();
        assert!(app.handle_key(KeyCode::Enter));

        let output = render_once(&app, 140, 36).unwrap();
        assert!(output.contains("Reasoning search"));
        assert!(output.contains("reasoning-two"));
        assert!(output.contains("[[beta]] branch analysis"));
        let detail = app.mcp_detail.as_ref().unwrap().1.as_str();
        assert!(!detail.contains("alpha planning"));
        assert!(app.status.contains("matched 1"));
    }

    #[test]
    fn command_palette_pins_reasoning_turns_for_replay() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: Some("turn-one".to_string()),
                event_seq: 1,
            }],
            vec![TuiItem {
                id: "reasoning-one".to_string(),
                thread_id: "thread-one".to_string(),
                turn_id: Some("turn-one".to_string()),
                index: 1,
                item_type: "reasoning".to_string(),
                role: Some("assistant".to_string()),
                content: "pinned reasoning".to_string(),
                status: "completed".to_string(),
            }],
        );

        app.show_command_palette = true;
        app.command_query = "reasoning pin turn-one".to_string();
        app.command_cursor = app.command_query.len();
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.reasoning_replay_pinned_turn_ids(),
            vec!["turn-one".to_string()]
        );
        let output = render_once(&app, 140, 36).unwrap();
        assert!(output.contains("Reasoning replay pins"));
        assert!(output.contains("pinned_turns: turn-one"));
        assert!(app.status.contains("pinned reasoning turn turn-one"));

        app.show_command_palette = true;
        app.command_query = "reasoning unpin all".to_string();
        app.command_cursor = app.command_query.len();
        assert!(app.handle_key(KeyCode::Enter));

        assert!(app.reasoning_replay_pinned_turn_ids().is_empty());
        let output = render_once(&app, 140, 36).unwrap();
        assert!(output.contains("pinned_turns: none"));
        assert_eq!(app.status, "cleared 1 reasoning replay pin(s)");
    }

    #[test]
    fn reasoning_replay_preferences_persist_across_tui_instances() {
        let root = temp_root("reasoning-prefs");
        let prefs = root.join("reasoning-replay.json");
        let sessions = vec![TuiSession {
            id: "session-one".to_string(),
            title: "One".to_string(),
            workspace: ".".to_string(),
            status: "active".to_string(),
            active_thread_id: Some("thread-one".to_string()),
            thread_count: 1,
        }];
        let threads = vec![TuiThread {
            id: "thread-one".to_string(),
            session_id: Some("session-one".to_string()),
            title: "First thread".to_string(),
            mode: "agent".to_string(),
            status: "active".to_string(),
            latest_turn_id: Some("turn-one".to_string()),
            event_seq: 1,
        }];
        let items = vec![TuiItem {
            id: "reasoning-one".to_string(),
            thread_id: "thread-one".to_string(),
            turn_id: Some("turn-one".to_string()),
            index: 1,
            item_type: "reasoning".to_string(),
            role: Some("assistant".to_string()),
            content: "persisted reasoning".to_string(),
            status: "completed".to_string(),
        }];
        let mut app = TuiApp::with_runtime(sessions.clone(), threads.clone(), items.clone());
        app.enable_reasoning_replay_preferences(prefs.clone());

        run_palette_command(&mut app, "reasoning replay 7");
        run_palette_command(&mut app, "reasoning pin turn-one");

        let saved = fs::read_to_string(&prefs).unwrap();
        assert!(saved.contains("\"kind\":\"deepseek.tui.reasoning_replay.v1\""));
        assert!(saved.contains("\"replay_limit\":7"));
        assert!(saved.contains("\"turn-one\""));

        let mut restored = TuiApp::with_runtime(sessions, threads, items);
        restored.enable_reasoning_replay_preferences(prefs.clone());

        assert_eq!(restored.reasoning_replay_limit(), 7);
        assert_eq!(
            restored.reasoning_replay_pinned_turn_ids(),
            vec!["turn-one".to_string()]
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn replace_runtime_preserves_selected_session_and_updates_items() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 1,
            }],
            vec![TuiItem {
                id: "item-one".to_string(),
                thread_id: "thread-one".to_string(),
                turn_id: None,
                index: 1,
                item_type: "message".to_string(),
                role: Some("user".to_string()),
                content: "before refresh".to_string(),
                status: "completed".to_string(),
            }],
        );

        app.replace_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: None,
                event_seq: 2,
            }],
            vec![
                TuiItem {
                    id: "item-one".to_string(),
                    thread_id: "thread-one".to_string(),
                    turn_id: None,
                    index: 1,
                    item_type: "message".to_string(),
                    role: Some("user".to_string()),
                    content: "before refresh".to_string(),
                    status: "completed".to_string(),
                },
                TuiItem {
                    id: "item-two".to_string(),
                    thread_id: "thread-one".to_string(),
                    turn_id: None,
                    index: 2,
                    item_type: "message".to_string(),
                    role: Some("assistant".to_string()),
                    content: "after refresh".to_string(),
                    status: "completed".to_string(),
                },
            ],
        );

        assert_eq!(app.selected_thread_id.as_deref(), Some("thread-one"));
        assert!(app
            .transcript
            .iter()
            .any(|line| line.contains("after refresh")));
        assert!(app.tasks.iter().any(|line| line == "Runtime items: 2"));
        assert_eq!(
            app.status,
            "runtime refreshed: sessions=1 threads=1 items=2"
        );
    }

    #[test]
    fn live_item_event_upserts_and_refreshes_active_transcript() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: Some("turn-one".to_string()),
                event_seq: 1,
            }],
            vec![TuiItem {
                id: "item-one".to_string(),
                thread_id: "thread-one".to_string(),
                turn_id: Some("turn-one".to_string()),
                index: 1,
                item_type: "message".to_string(),
                role: Some("assistant".to_string()),
                content: "partial".to_string(),
                status: "running".to_string(),
            }],
        );

        app.apply_live_event(TuiLiveEvent::UpsertItem(TuiItem {
            id: "item-one".to_string(),
            thread_id: "thread-one".to_string(),
            turn_id: Some("turn-one".to_string()),
            index: 1,
            item_type: "message".to_string(),
            role: Some("assistant".to_string()),
            content: "partial response without interval refresh".to_string(),
            status: "running".to_string(),
        }));
        app.apply_live_event(TuiLiveEvent::UpsertItem(TuiItem {
            id: "item-two".to_string(),
            thread_id: "thread-one".to_string(),
            turn_id: Some("turn-one".to_string()),
            index: 2,
            item_type: "reasoning".to_string(),
            role: Some("assistant".to_string()),
            content: "thinking live".to_string(),
            status: "running".to_string(),
        }));

        assert_eq!(app.items.len(), 2);
        assert!(app
            .transcript
            .iter()
            .any(|line| line.contains("partial response without interval refresh")));
        assert!(app
            .transcript
            .iter()
            .any(|line| line.contains("thinking live")));
        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("partial response"));
    }

    #[test]
    fn live_runtime_event_replaces_snapshot_and_refreshes_transcript() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: Some("turn-one".to_string()),
                event_seq: 1,
            }],
            Vec::new(),
        );

        app.apply_live_event(TuiLiveEvent::ReplaceRuntime {
            sessions: vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            threads: vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: Some("turn-one".to_string()),
                event_seq: 2,
            }],
            items: vec![TuiItem {
                id: "item-one".to_string(),
                thread_id: "thread-one".to_string(),
                turn_id: Some("turn-one".to_string()),
                index: 1,
                item_type: "message".to_string(),
                role: Some("user".to_string()),
                content: "external runtime write".to_string(),
                status: "completed".to_string(),
            }],
            tasks: Vec::new(),
            automations: Vec::new(),
            usage_summaries: Vec::new(),
            approvals: Vec::new(),
            user_inputs: Vec::new(),
        });

        assert!(app
            .transcript
            .iter()
            .any(|line| line.contains("external runtime write")));
        assert!(app.tasks.iter().any(|line| line == "Runtime items: 1"));
    }

    #[test]
    fn cancel_key_requests_active_running_assistant_turn() {
        let mut app = TuiApp::with_runtime(
            vec![TuiSession {
                id: "session-one".to_string(),
                title: "One".to_string(),
                workspace: ".".to_string(),
                status: "active".to_string(),
                active_thread_id: Some("thread-one".to_string()),
                thread_count: 1,
            }],
            vec![TuiThread {
                id: "thread-one".to_string(),
                session_id: Some("session-one".to_string()),
                title: "First thread".to_string(),
                mode: "agent".to_string(),
                status: "active".to_string(),
                latest_turn_id: Some("turn-one".to_string()),
                event_seq: 4,
            }],
            vec![TuiItem {
                id: "item-one".to_string(),
                thread_id: "thread-one".to_string(),
                turn_id: Some("turn-one".to_string()),
                index: 1,
                item_type: "message".to_string(),
                role: Some("assistant".to_string()),
                content: "partial response".to_string(),
                status: "running".to_string(),
            }],
        );

        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("Running assistant"));
        assert!(app.handle_key(KeyCode::Char('c')));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::CancelRun {
                thread_id: "thread-one".to_string(),
                turn_id: Some("turn-one".to_string()),
            }]
        );
        assert!(app.status.contains("cancel requested"));
    }

    #[test]
    fn replace_runtime_with_approvals_opens_real_approval_modal() {
        let sessions = vec![TuiSession {
            id: "session-one".to_string(),
            title: "One".to_string(),
            workspace: ".".to_string(),
            status: "active".to_string(),
            active_thread_id: Some("thread-one".to_string()),
            thread_count: 1,
        }];
        let threads = vec![TuiThread {
            id: "thread-one".to_string(),
            session_id: Some("session-one".to_string()),
            title: "First thread".to_string(),
            mode: "agent".to_string(),
            status: "active".to_string(),
            latest_turn_id: None,
            event_seq: 1,
        }];
        let approval = TuiApprovalRequest {
            id: "event-one".to_string(),
            thread_id: "thread-one".to_string(),
            turn_id: None,
            tool: "run_shell".to_string(),
            kind: "shell".to_string(),
            target: "cargo test".to_string(),
            status: "pending".to_string(),
        };
        let mut app = TuiApp::with_runtime(sessions.clone(), threads.clone(), Vec::new());

        app.replace_runtime_with_approvals(
            sessions.clone(),
            threads.clone(),
            Vec::new(),
            vec![approval.clone()],
        );

        assert!(app.show_approval_modal);
        assert_eq!(app.active_approval_id.as_deref(), Some("event-one"));
        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("run_shell"));
        assert!(output.contains("shell"));
        assert!(output.contains("cargo test"));

        assert!(app.handle_key(KeyCode::Char('y')));
        assert!(!app.show_approval_modal);
        assert!(app
            .dismissed_approval_ids
            .iter()
            .any(|id| id == "event-one"));
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::RespondApproval {
                thread_id: "thread-one".to_string(),
                turn_id: None,
                request_id: "event-one".to_string(),
                decision: "approved".to_string(),
            }]
        );

        app.replace_runtime_with_approvals(sessions, threads, Vec::new(), vec![approval]);
        assert!(!app.show_approval_modal);
        assert_eq!(app.active_approval_id, None);
    }

    #[test]
    fn replace_runtime_with_user_input_opens_modal_and_records_answer() {
        let sessions = vec![TuiSession {
            id: "session-one".to_string(),
            title: "One".to_string(),
            workspace: ".".to_string(),
            status: "active".to_string(),
            active_thread_id: Some("thread-one".to_string()),
            thread_count: 1,
        }];
        let threads = vec![TuiThread {
            id: "thread-one".to_string(),
            session_id: Some("session-one".to_string()),
            title: "First thread".to_string(),
            mode: "agent".to_string(),
            status: "active".to_string(),
            latest_turn_id: None,
            event_seq: 1,
        }];
        let request = TuiUserInputRequest {
            id: "event-input".to_string(),
            thread_id: "thread-one".to_string(),
            turn_id: None,
            status: "pending".to_string(),
            questions: vec![TuiUserInputQuestion {
                header: "Mode".to_string(),
                id: "mode".to_string(),
                question: "Which mode?".to_string(),
                options: vec![
                    TuiUserInputOption {
                        label: "Plan".to_string(),
                        description: "Plan first.".to_string(),
                    },
                    TuiUserInputOption {
                        label: "Apply".to_string(),
                        description: "Implement now.".to_string(),
                    },
                ],
            }],
        };
        let mut app = TuiApp::with_runtime_usage_tasks_automations_approvals_and_user_inputs(
            sessions,
            threads,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![request],
        );

        assert!(app.show_user_input_modal);
        assert_eq!(app.active_user_input_id.as_deref(), Some("event-input"));
        let output = render_once(&app, 120, 36).unwrap();
        assert!(output.contains("Input Required"));
        assert!(output.contains("Which mode?"));
        assert!(output.contains("Plan"));

        assert!(app.handle_key(KeyCode::Char('1')));
        assert!(!app.show_user_input_modal);
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::RespondUserInput {
                thread_id: "thread-one".to_string(),
                turn_id: None,
                request_id: "event-input".to_string(),
                answers: BTreeMap::from([("mode".to_string(), "Plan".to_string())]),
            }]
        );
    }

    #[test]
    fn user_input_modal_accepts_other_answer() {
        let sessions = vec![TuiSession {
            id: "session-one".to_string(),
            title: "One".to_string(),
            workspace: ".".to_string(),
            status: "active".to_string(),
            active_thread_id: Some("thread-one".to_string()),
            thread_count: 1,
        }];
        let threads = vec![TuiThread {
            id: "thread-one".to_string(),
            session_id: Some("session-one".to_string()),
            title: "First thread".to_string(),
            mode: "agent".to_string(),
            status: "active".to_string(),
            latest_turn_id: None,
            event_seq: 1,
        }];
        let request = TuiUserInputRequest {
            id: "event-input".to_string(),
            thread_id: "thread-one".to_string(),
            turn_id: None,
            status: "pending".to_string(),
            questions: vec![TuiUserInputQuestion {
                header: "Mode".to_string(),
                id: "mode".to_string(),
                question: "Which mode?".to_string(),
                options: vec![
                    TuiUserInputOption {
                        label: "Plan".to_string(),
                        description: "Plan first.".to_string(),
                    },
                    TuiUserInputOption {
                        label: "Apply".to_string(),
                        description: "Implement now.".to_string(),
                    },
                ],
            }],
        };
        let mut app = TuiApp::with_runtime_usage_tasks_automations_approvals_and_user_inputs(
            sessions,
            threads,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![request],
        );

        assert!(app.handle_key(KeyCode::Char('o')));
        assert!(app.user_input_other_mode);
        let empty_output = render_once(&app, 120, 36).unwrap();
        assert!(empty_output.contains("Other: <empty>"));
        assert!(app.handle_key(KeyCode::Enter));
        assert!(app.show_user_input_modal);
        assert!(app.drain_actions().is_empty());
        assert_eq!(app.status, "other answer cannot be empty");

        for ch in "Customx".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Backspace));
        assert!(app.handle_key(KeyCode::Enter));
        assert!(!app.show_user_input_modal);
        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::RespondUserInput {
                thread_id: "thread-one".to_string(),
                turn_id: None,
                request_id: "event-input".to_string(),
                answers: BTreeMap::from([("mode".to_string(), "Custom".to_string())]),
            }]
        );
    }
}
