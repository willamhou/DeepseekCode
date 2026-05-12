use std::collections::BTreeMap;
use std::io;
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, Event, KeyCode},
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
use crate::util::json::{json_as_object, json_as_string, JsonValue};

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
    },
    Status(String),
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
pub struct TuiUsageSummary {
    pub thread_id: String,
    pub record_count: usize,
    pub total_tokens: u64,
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
        const CONTEXT_WINDOW_TOKENS: u64 = 1_000_000;

        let mut total_tokens = 0_u64;
        let mut prompt_cache_hit_tokens = 0_u64;
        let mut prompt_cache_miss_tokens = 0_u64;
        let mut estimated_input_cost_microusd = Some(0_u64);
        let mut estimated_output_cost_microusd = Some(0_u64);
        let mut estimated_total_cost_microusd = Some(0_u64);
        for record in records {
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
        let latest_total_tokens = records
            .first()
            .map(|record| record.total_tokens)
            .unwrap_or(0);
        let context_remaining_tokens = CONTEXT_WINDOW_TOKENS.saturating_sub(latest_total_tokens);
        Self {
            thread_id: thread_id.to_string(),
            record_count: records.len(),
            total_tokens,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiAction {
    SubmitUserMessage {
        thread_id: String,
        content: String,
    },
    RespondApproval {
        thread_id: String,
        turn_id: Option<String>,
        request_id: String,
        decision: String,
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
    CreateRollbackSnapshot {
        label: Option<String>,
    },
    ListRollbackSnapshots {
        limit: usize,
    },
    ShowRollbackSnapshot {
        id: String,
    },
    RevertTurn {
        id: String,
        apply: bool,
    },
    RunDiagnostics {
        changed: bool,
        paths: Vec<String>,
    },
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

#[derive(Debug, Clone)]
pub struct TuiApp {
    mode: TuiMode,
    sessions: Vec<TuiSession>,
    threads: Vec<TuiThread>,
    items: Vec<TuiItem>,
    task_records: Vec<TuiTaskRecord>,
    automation_records: Vec<TuiAutomationRecord>,
    usage_summaries: Vec<TuiUsageSummary>,
    approvals: Vec<TuiApprovalRequest>,
    active_approval_id: Option<String>,
    dismissed_approval_ids: Vec<String>,
    selected_session: usize,
    selected_thread_id: Option<String>,
    show_command_palette: bool,
    show_session_picker: bool,
    show_thread_picker: bool,
    show_approval_modal: bool,
    command_query: String,
    command_cursor: usize,
    composer: String,
    composer_cursor: usize,
    composer_focused: bool,
    transcript_scroll: usize,
    pending_actions: Vec<TuiAction>,
    status: String,
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
            sessions,
            threads,
            items,
            task_records,
            automation_records,
            usage_summaries,
            approvals,
            active_approval_id: None,
            dismissed_approval_ids: Vec::new(),
            selected_session: 0,
            selected_thread_id: None,
            show_command_palette: false,
            show_session_picker: false,
            show_thread_picker: false,
            show_approval_modal: false,
            command_query: String::new(),
            command_cursor: 0,
            composer: String::new(),
            composer_cursor: 0,
            composer_focused: false,
            transcript_scroll: 0,
            pending_actions: Vec::new(),
            status: "ready".to_string(),
            transcript: Vec::new(),
            tasks: Vec::new(),
        };
        app.selected_thread_id = app.default_thread_id_for_selected_session();
        app.refresh_runtime_view();
        app.sync_approval_modal();
        app
    }

    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    pub fn apply_live_event(&mut self, event: TuiLiveEvent) {
        match event {
            TuiLiveEvent::UpsertItem(item) => {
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
            }
            TuiLiveEvent::ReplaceRuntime {
                sessions,
                threads,
                items,
                tasks,
                automations,
                usage_summaries,
                approvals,
            } => self.replace_runtime_with_usage_tasks_automations_and_approvals(
                sessions,
                threads,
                items,
                tasks,
                automations,
                usage_summaries,
                approvals,
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
        self.refresh_runtime_view();
        let opened_approval = self.sync_approval_modal();

        let counts = (
            self.sessions.len(),
            self.threads.len(),
            self.items.len(),
            self.task_records.len(),
            self.automation_records.len(),
            self.usage_summaries.len(),
            self.approvals.len(),
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
            self.status = if counts.6 == 0 {
                format!(
                    "runtime refreshed: sessions={} threads={} items={}{}{}{}",
                    counts.0, counts.1, counts.2, tasks, automations, usage
                )
            } else {
                format!(
                    "runtime refreshed: sessions={} threads={} items={}{}{}{} approvals={}",
                    counts.0, counts.1, counts.2, tasks, automations, usage, counts.6
                )
            };
        }
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

    fn selected_thread_index_for_session(&self) -> Option<usize> {
        let selected_thread_id = self.selected_thread_id.as_deref()?;
        self.threads_for_selected_session()
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

    fn active_running_assistant_item(&self) -> Option<&TuiItem> {
        self.active_thread_items().into_iter().rev().find(|item| {
            item.status == "running"
                && item.item_type == "message"
                && item.role.as_deref() == Some("assistant")
        })
    }

    fn active_approval(&self) -> Option<&TuiApprovalRequest> {
        let approval_id = self.active_approval_id.as_deref()?;
        self.approvals
            .iter()
            .find(|approval| approval.id == approval_id)
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

    fn sync_approval_modal(&mut self) -> bool {
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
            let threads = self.threads_for_selected_session();
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

    fn select_thread_by_id(&mut self, thread_id: &str) -> bool {
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

    fn select_relative_thread(&mut self, offset: isize) {
        let len = self.threads_for_selected_session().len();
        if len == 0 {
            self.status = "no threads in selected session".to_string();
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

        let (transcript, item_count) = {
            let items = self.active_thread_items();
            let item_count = items.len();
            let transcript = if items.is_empty() {
                vec![
                    format!("Thread: {}", thread.title),
                    "No durable items recorded for this thread yet.".to_string(),
                ]
            } else {
                items
                    .iter()
                    .map(|item| {
                        let role = item.role.as_deref().unwrap_or(item.item_type.as_str());
                        format!(
                            "{} [{}]: {}",
                            role,
                            item.status,
                            clip_line(&item.content, 120)
                        )
                    })
                    .collect()
            };
            (transcript, item_count)
        };
        self.transcript = transcript;
        self.tasks = vec![
            format!("Active thread: {}", thread.title),
            format!("Thread mode/status: {} / {}", thread.mode, thread.status),
            format!("Runtime items: {item_count}"),
            format!("Event seq: {}", thread.event_seq),
        ];
        let active_task_lines = {
            let active_tasks = self.active_thread_tasks();
            if active_tasks.is_empty() {
                Vec::new()
            } else {
                let mut lines = vec![format!("Runtime tasks: {}", active_tasks.len())];
                lines.extend(active_tasks.iter().take(4).map(|task| {
                    format!(
                        "Task {} [{}]: {}",
                        task.kind,
                        task.status,
                        clip_line(&task.summary, 70)
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
        if self.show_approval_modal {
            return self.handle_approval_key(code);
        }
        if self.composer_focused {
            return self.handle_composer_key(code);
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
                let Some(thread_id) = self.selected_thread_id.clone() else {
                    self.status = "composer has no active durable thread".to_string();
                    return true;
                };
                self.composer.clear();
                self.composer_cursor = 0;
                self.pending_actions
                    .push(TuiAction::SubmitUserMessage { thread_id, content });
                self.status = "submitting composer message".to_string();
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
                self.execute_palette_command(&command);
            }
            KeyCode::Backspace => {
                backspace_at_cursor(&mut self.command_query, &mut self.command_cursor);
            }
            KeyCode::Delete => {
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
            KeyCode::Char(ch) => {
                insert_char_at_cursor(&mut self.command_query, &mut self.command_cursor, ch);
            }
            _ => {}
        }
        true
    }

    fn execute_palette_command(&mut self, command: &str) {
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
            ["threads"] | ["thread"] => {
                self.show_thread_picker = true;
                self.show_session_picker = false;
                self.status = "thread navigator opened".to_string();
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
                self.status = "commands: mode plan|agent|yolo, sessions, threads, task <summary>|pause [id]|resume [id], diagnostics [--changed|paths...], restore snapshot|list|show, revert turn <id> [--apply], compact, approval, cancel".to_string();
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
                self.select_session(
                    (self.selected_session + 1).min(self.sessions.len().saturating_sub(1)),
                );
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_session(self.selected_session.saturating_sub(1));
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
            KeyCode::Char('s') => {
                self.show_thread_picker = false;
                self.show_session_picker = true;
            }
            _ => {}
        }
        true
    }

    fn handle_approval_key(&mut self, code: KeyCode) -> bool {
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
        let selected = self
            .active_thread_tasks()
            .into_iter()
            .find(|task| task.status == "pending")
            .map(|task| task.id.clone());
        if let Some(task_id) = selected {
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
        let selected = self
            .active_thread_tasks()
            .into_iter()
            .find(|task| task.status == "paused")
            .map(|task| task.id.clone());
        if let Some(task_id) = selected {
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

    fn request_revert_turn(&mut self, id: &str, apply: bool) {
        let Some(id) = self.resolve_rollback_id(id) else {
            return;
        };
        self.pending_actions.push(TuiAction::RevertTurn {
            id: id.clone(),
            apply,
        });
        self.status = if apply {
            format!("rollback apply requested: {id}")
        } else {
            format!("rollback dry-run requested: {id}")
        };
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

fn context_strategy(latest_total_tokens: u64) -> &'static str {
    match latest_total_tokens {
        900_000.. => "must_compact_or_chunk",
        800_000.. => "prepare_compaction",
        500_000.. => "monitor",
        _ => "normal",
    }
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
    execute!(stdout, EnterAlternateScreen)?;
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
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
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
        }
        live(app)?;
        terminal.draw(|frame| draw(frame, app))?;
        let poll_timeout = refresh_interval
            .saturating_sub(last_refresh.elapsed())
            .min(Duration::from_millis(200));
        if event::poll(poll_timeout)? {
            if let Event::Key(key) = event::read()? {
                if !app.handle_key(key.code) {
                    break;
                }
                let actions = app.drain_actions();
                if !actions.is_empty() {
                    for next_action in actions {
                        action(app, next_action)?;
                    }
                    refresh(app)?;
                    last_refresh = Instant::now();
                }
            }
        }
    }
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
                .fg(Color::Cyan)
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
    let columns = Layout::horizontal([
        Constraint::Length(32),
        Constraint::Min(36),
        Constraint::Length(32),
    ])
    .split(area);
    draw_sidebar(frame, app, columns[0]);
    draw_transcript(frame, app, columns[1]);
    draw_tasks(frame, app, columns[2]);
}

fn draw_sidebar(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let session = app.sessions.get(app.selected_session);
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Mode: ", Style::default().fg(Color::Gray)),
            Span::styled(app.mode.title(), Style::default().fg(Color::Green)),
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
    lines.push(Line::from("PgUp/PgDn: scroll"));
    lines.push(Line::from("q: quit"));

    let sidebar = Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title("Sidebar"));
    frame.render_widget(sidebar, area);
}

fn draw_transcript(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let composer_marker = if app.composer_focused { "*" } else { "" };
    let composer = display_with_cursor(&app.composer, app.composer_cursor, app.composer_focused);
    let lines = app
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
    let items = app
        .tasks
        .iter()
        .map(|task| ListItem::new(task.as_str()))
        .collect::<Vec<_>>();
    let tasks =
        List::new(items).block(Block::default().borders(Borders::ALL).title("Plan / Tasks"));
    frame.render_widget(tasks, area);
}

fn draw_status(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let status = Paragraph::new(vec![Line::from(vec![
        Span::styled("Status: ", Style::default().fg(Color::Gray)),
        Span::raw(app.status.as_str()),
        Span::raw(" | "),
        Span::styled("Palette", Style::default().fg(Color::Yellow)),
        Span::raw(" : "),
        Span::styled("Sessions", Style::default().fg(Color::Yellow)),
        Span::raw(" s "),
        Span::styled("Threads", Style::default().fg(Color::Yellow)),
        Span::raw(" t "),
        Span::styled("Approval", Style::default().fg(Color::Yellow)),
        Span::raw(" !"),
        Span::raw(" "),
        Span::styled("Cancel", Style::default().fg(Color::Yellow)),
        Span::raw(" c"),
    ])])
    .block(Block::default().borders(Borders::ALL).title("Command Bar"));
    frame.render_widget(status, area);
}

fn draw_session_picker(frame: &mut Frame, app: &TuiApp) {
    let area = left_middle_rect(frame.area(), 42, 18);
    frame.render_widget(Clear, area);
    let items = app
        .sessions
        .iter()
        .enumerate()
        .map(|(index, session)| {
            let prefix = if index == app.selected_session {
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
    let picker = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Session Picker"),
    );
    frame.render_widget(picker, area);
}

fn draw_thread_picker(frame: &mut Frame, app: &TuiApp) {
    let area = right_middle_rect(frame.area(), 52, 20);
    frame.render_widget(Clear, area);
    let items = app
        .threads_for_selected_session()
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
    let items = if items.is_empty() {
        vec![ListItem::new("No durable threads in selected session")]
    } else {
        items
    };
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
        Line::from("Examples: mode agent | task pause | diagnostics --changed | revert turn last"),
        Line::from(""),
        Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Cyan)),
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
    let lines = if let Some(approval) = app.active_approval() {
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

fn top_center_rect(area: Rect, width: u16, height: u16) -> Rect {
    fixed_rect(
        area,
        width,
        height,
        area.x + area.width.saturating_sub(width.min(area.width)) / 2,
        area.y + 3,
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_cycles_through_plan_agent_yolo() {
        assert_eq!(TuiMode::Plan.next(), TuiMode::Agent);
        assert_eq!(TuiMode::Agent.next(), TuiMode::Yolo);
        assert_eq!(TuiMode::Yolo.next(), TuiMode::Plan);
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
            Vec::new(),
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
        assert!(output.contains("Runtime tasks: 1"));
        assert!(output.contains("Task agent [running]"));
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
        for ch in "revert turn last --apply".chars() {
            assert!(app.handle_key(KeyCode::Char(ch)));
        }
        assert!(app.handle_key(KeyCode::Enter));

        assert_eq!(
            app.drain_actions(),
            vec![TuiAction::RevertTurn {
                id: "turn-latest".to_string(),
                apply: true,
            }]
        );
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
}
