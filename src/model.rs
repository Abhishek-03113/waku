use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderKind {
    Amp,
    Claude,
    #[default]
    Codex,
    Cursor,
    OpenCode,
    Grok,
    Pi,
}

impl ProviderKind {
    pub const ALL: [Self; 7] = [
        Self::Amp,
        Self::Claude,
        Self::Codex,
        Self::Cursor,
        Self::OpenCode,
        Self::Grok,
        Self::Pi,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Amp => "amp",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::OpenCode => "opencode",
            Self::Grok => "grok",
            Self::Pi => "pi",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Amp => "Amp",
            Self::Claude => "Claude Code",
            Self::Codex => "Codex CLI",
            Self::Cursor => "Cursor CLI",
            Self::OpenCode => "OpenCode",
            Self::Grok => "Grok Build",
            Self::Pi => "Pi",
        }
    }

    pub fn short_name(self) -> &'static str {
        match self {
            Self::Amp => "Amp",
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Cursor => "Cursor",
            Self::OpenCode => "OpenCode",
            Self::Grok => "Grok",
            Self::Pi => "Pi",
        }
    }

    pub fn command(self) -> &'static str {
        match self {
            Self::Amp => "amp",
            Self::Claude => "claude",
            Self::Codex => "codex",
            // Cursor documents `agent` as its primary command, but that name is
            // shared by other CLIs. The backward-compatible alias is unambiguous.
            Self::Cursor => "cursor-agent",
            Self::OpenCode => "opencode",
            Self::Grok => "grok",
            Self::Pi => "pi",
        }
    }

    pub fn supports_conversation_rollback(self) -> bool {
        matches!(
            self,
            Self::Amp
                | Self::Claude
                | Self::Codex
                | Self::Cursor
                | Self::OpenCode
                | Self::Grok
                | Self::Pi
        )
    }

    pub fn supports_conversation_fork(self) -> bool {
        matches!(
            self,
            Self::Amp
                | Self::Claude
                | Self::Codex
                | Self::Cursor
                | Self::OpenCode
                | Self::Grok
                | Self::Pi
        )
    }

    pub fn supports_model_discovery(self) -> bool {
        matches!(
            self,
            Self::Codex | Self::Cursor | Self::OpenCode | Self::Grok | Self::Pi
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "provider"
)]
pub enum ProviderResumeCursor {
    Amp {
        thread_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fork_context: Option<String>,
    },
    Claude {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume_at: Option<String>,
    },
    Codex {
        thread_id: String,
    },
    Cursor {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fork_context: Option<String>,
    },
    OpenCode {
        session_id: String,
    },
    Grok {
        session_id: String,
    },
    Pi {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_file: Option<PathBuf>,
    },
}

impl ProviderResumeCursor {
    pub fn from_session_id(provider: ProviderKind, id: String) -> Self {
        match provider {
            ProviderKind::Amp => Self::Amp {
                thread_id: id,
                fork_context: None,
            },
            ProviderKind::Claude => Self::Claude {
                session_id: id,
                resume_at: None,
            },
            ProviderKind::Codex => Self::Codex { thread_id: id },
            ProviderKind::Cursor => Self::Cursor {
                session_id: id,
                fork_context: None,
            },
            ProviderKind::OpenCode => Self::OpenCode { session_id: id },
            ProviderKind::Grok => Self::Grok { session_id: id },
            ProviderKind::Pi => Self::Pi {
                session_id: id,
                session_file: None,
            },
        }
    }

    pub fn provider(&self) -> ProviderKind {
        match self {
            Self::Amp { .. } => ProviderKind::Amp,
            Self::Claude { .. } => ProviderKind::Claude,
            Self::Codex { .. } => ProviderKind::Codex,
            Self::Cursor { .. } => ProviderKind::Cursor,
            Self::OpenCode { .. } => ProviderKind::OpenCode,
            Self::Grok { .. } => ProviderKind::Grok,
            Self::Pi { .. } => ProviderKind::Pi,
        }
    }

    pub fn native_id(&self) -> &str {
        match self {
            Self::Amp { thread_id, .. } => thread_id,
            Self::Claude { session_id, .. }
            | Self::Cursor { session_id, .. }
            | Self::OpenCode { session_id }
            | Self::Grok { session_id }
            | Self::Pi { session_id, .. } => session_id,
            Self::Codex { thread_id } => thread_id,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeMode {
    /// Legacy combined mode. State migration moves this to `interaction_mode`.
    Plan,
    Ask,
    AutoAcceptEdits,
    Auto,
    #[default]
    FullAccess,
}

impl RuntimeMode {
    pub const ACCESS_OPTIONS: [Self; 4] = [
        Self::Ask,
        Self::AutoAcceptEdits,
        Self::Auto,
        Self::FullAccess,
    ];

    pub fn label(self) -> String {
        match self {
            Self::Plan => tr!("mode.plan"),
            Self::Ask => tr!("mode.supervised"),
            Self::AutoAcceptEdits => tr!("mode.auto_accept_edits"),
            Self::Auto => tr!("mode.auto"),
            Self::FullAccess => tr!("mode.full_access"),
        }
    }

    pub fn description(self) -> String {
        match self {
            Self::Plan => tr!("mode.plan_description"),
            Self::Ask => tr!("mode.supervised_description"),
            Self::AutoAcceptEdits => tr!("mode.auto_accept_edits_description"),
            Self::Auto => tr!("mode.auto_description"),
            Self::FullAccess => tr!("mode.full_access_description"),
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Plan | Self::Ask => "icons/lock.svg",
            Self::AutoAcceptEdits => "icons/pencil.svg",
            Self::Auto => "icons/sparkle.svg",
            Self::FullAccess => "icons/lock-open.svg",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InteractionMode {
    #[default]
    Build,
    Plan,
}

impl InteractionMode {
    pub fn label(self) -> String {
        match self {
            Self::Build => tr!("mode.build"),
            Self::Plan => tr!("mode.plan"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderModelOption {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl ProviderModelOption {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        let description = description.into();
        if !description.trim().is_empty() {
            self.description = Some(description);
        }
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderModel {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_provider: Option<String>,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub reasoning_efforts: Vec<ProviderModelOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reasoning_effort: Option<String>,
    #[serde(default)]
    pub service_tiers: Vec<ProviderModelOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_service_tier: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FavoriteModel {
    pub provider: ProviderKind,
    pub model: String,
}

impl ProviderModel {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            sub_provider: None,
            is_default: false,
            reasoning_efforts: Vec::new(),
            default_reasoning_effort: None,
            service_tiers: Vec::new(),
            default_service_tier: None,
        }
    }

    pub fn default(mut self) -> Self {
        self.is_default = true;
        self
    }

    pub fn sub_provider(mut self, sub_provider: impl Into<String>) -> Self {
        self.sub_provider = Some(sub_provider.into());
        self
    }

    pub fn reasoning(
        mut self,
        efforts: impl IntoIterator<Item = ProviderModelOption>,
        default: impl Into<String>,
    ) -> Self {
        self.reasoning_efforts = efforts.into_iter().collect();
        self.default_reasoning_effort = Some(default.into());
        self
    }

    pub fn service_tiers(
        mut self,
        tiers: impl IntoIterator<Item = ProviderModelOption>,
        default: impl Into<String>,
    ) -> Self {
        self.service_tiers = tiers.into_iter().collect();
        self.default_service_tier = Some(default.into());
        self
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProviderProbe {
    pub provider: ProviderKind,
    pub installed: bool,
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub models: Vec<ProviderModel>,
}

impl ProviderProbe {
    pub fn pending(provider: ProviderKind) -> Self {
        let path = find_in_path(provider.command());
        Self {
            provider,
            installed: path.is_some(),
            path,
            models: crate::model_catalog::fallback_models(provider),
        }
    }

    /// A probe resolved through a user-configured binary override instead of
    /// PATH detection. An override that resolves to nothing leaves the
    /// provider uninstalled rather than silently falling back.
    pub fn with_binary_override(provider: ProviderKind, binary: &str) -> Self {
        let path = crate::command_env::resolve_binary_override(binary);
        Self {
            provider,
            installed: path.is_some(),
            path,
            models: crate::model_catalog::fallback_models(provider),
        }
    }

    pub fn discover_models(mut self) -> Self {
        if self.provider.supports_model_discovery()
            && let Some(path) = self.path.as_deref()
        {
            self.models = crate::model_catalog::discover_models(self.provider, path);
        }
        self
    }

    pub fn preferred_model(&self) -> Option<&ProviderModel> {
        self.models
            .iter()
            .find(|model| model.is_default)
            .or_else(|| self.models.first())
    }
}

fn find_in_path(command: &str) -> Option<PathBuf> {
    crate::command_env::find_executable(command)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub path: PathBuf,
    /// When the project was added, unix seconds.
    #[serde(default)]
    pub created_at: u64,
}

/// Filesystem context a task runs in.
///
/// Drafts may carry [`Self::NewWorktree`] until their first prompt. Waku then
/// creates the Git worktree and replaces it with [`Self::Worktree`] before any
/// checkpoint or provider process can observe the task.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum SessionWorkspace {
    /// Work directly in the project's ordinary checkout.
    #[default]
    Local,
    /// Create an isolated worktree when this draft is first submitted. A
    /// selected base branch is remembered without checking it out in the
    /// ordinary project directory.
    NewWorktree {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_branch: Option<String>,
    },
    /// A materialized worktree. `path` preserves a project that points at a
    /// subdirectory of its repository rather than the repository root itself.
    Worktree { path: PathBuf, branch: String },
}

impl SessionWorkspace {
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }

    pub fn is_worktree(&self) -> bool {
        matches!(self, Self::NewWorktree { .. } | Self::Worktree { .. })
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Worktree { path, .. } => Some(path),
            Self::Local | Self::NewWorktree { .. } => None,
        }
    }
}

impl Project {
    pub const PROJECTLESS_NAME: &'static str = "No project";

    pub fn display_name(&self) -> String {
        if self.is_projectless() {
            tr!("project.no_project_name")
        } else {
            self.name.clone()
        }
    }

    pub fn from_path(path: PathBuf) -> Self {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("Project")
            .to_owned();
        Self {
            id: Uuid::new_v4(),
            name,
            path,
            created_at: unix_time(),
        }
    }

    pub fn is_projectless(&self) -> bool {
        crate::projectless::is_projectless_path(&self.path)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionStatus {
    #[default]
    Idle,
    Connecting,
    Working,
    Waiting,
    Failed,
}

impl SessionStatus {
    pub fn is_busy(self) -> bool {
        matches!(self, Self::Connecting | Self::Working | Self::Waiting)
    }
}

/// A follow-up message queued while the agent is busy. It becomes its own
/// turn once the current turn settles successfully.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QueuedMessage {
    pub id: Uuid,
    pub content: String,
    pub created_at: u64,
}

impl QueuedMessage {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            content: content.into(),
            created_at: unix_time(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    Running,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckpointStatus {
    Ready,
    Unavailable,
    Error,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckpointFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Checkpoint {
    pub turn_count: usize,
    pub git_ref: String,
    pub status: CheckpointStatus,
    #[serde(default)]
    pub files: Vec<CheckpointFile>,
    /// Cached once at capture time so a visible transcript row never walks a
    /// potentially huge file list on every frame.
    #[serde(default)]
    pub additions: u64,
    #[serde(default)]
    pub deletions: u64,
    pub created_at: u64,
}

impl Checkpoint {
    pub fn refresh_totals(&mut self) {
        self.additions = self.files.iter().map(|file| file.additions).sum();
        self.deletions = self.files.iter().map(|file| file.deletions).sum();
    }

    pub fn totals_are_current(&self) -> bool {
        self.additions == self.files.iter().map(|file| file.additions).sum::<u64>()
            && self.deletions == self.files.iter().map(|file| file.deletions).sum::<u64>()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentTurn {
    pub id: Uuid,
    pub turn_count: usize,
    pub status: TurnStatus,
    #[serde(default)]
    pub provider_turn_started: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_resume_at: Option<String>,
    pub started_at: u64,
    pub completed_at: Option<u64>,
    #[serde(default)]
    pub checkpoint: Option<Checkpoint>,
}

/// How full the provider's context window is, from the latest main-thread
/// model call. `tokens` is prompt + cache + output of that call; `window` is
/// the model's context size, which the provider only reports once a turn
/// settles — `None` means "not known yet", and the meter degrades to a bare
/// token count.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ContextUsage {
    pub tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentSession {
    pub id: Uuid,
    /// A title explicitly chosen by the user. [`Self::DEFAULT_TITLE`] means
    /// no explicit title has been set, so [`Self::auto_title`] may be shown.
    pub title: String,
    /// Best-effort title supplied by the provider, or derived locally from the
    /// first prompt until the provider reports a better one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_title: Option<String>,
    pub project_id: Uuid,
    /// Local project checkout or an isolated Git worktree for this task.
    #[serde(default, skip_serializing_if = "SessionWorkspace::is_local")]
    pub workspace: SessionWorkspace,
    pub provider: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub runtime_mode: RuntimeMode,
    #[serde(default)]
    pub interaction_mode: InteractionMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    pub status: SessionStatus,
    pub created_at: u64,
    /// Any mutation, including title edits and truncation. Use
    /// [`Self::last_reply_at`] for "when did the agent last respond".
    pub updated_at: u64,
    /// Completion of the most recent assistant turn, whatever its outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reply_at: Option<u64>,
    #[serde(default)]
    pub provider_cursor: Option<ProviderResumeCursor>,
    /// Slash commands the provider reported for this session's live process,
    /// kept so a resumed session still completes them before its next
    /// handshake.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_commands: Vec<ReportedCommand>,
    /// Context-window occupancy from the live stream, kept so a resumed
    /// session's meter starts where the conversation left off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<ContextUsage>,
    /// Read-only compatibility field for v1 state files. New saves omit it.
    #[serde(default, skip_serializing)]
    pub provider_session_id: Option<String>,
    /// Not stored in the session JSON — these are rows in the `messages`
    /// table, reattached when the session is hydrated.
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub transcript_blocks: Vec<TranscriptBlock>,
    #[serde(default)]
    pub turns: Vec<AgentTurn>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queued_messages: Vec<QueuedMessage>,
    /// Whether the transcript has been read from the database.
    ///
    /// Startup loads only the columns the session list needs, so a session
    /// begins as a skeleton with empty `messages`, `transcript_blocks` and
    /// `turns`. Those are empty because nothing fetched them, not because the
    /// session is empty — never persist a skeleton, and never conclude from one
    /// that a session has no history.
    #[serde(skip, default = "detail_loaded_default")]
    pub detail_loaded: bool,
}

/// Anything deserialized from a `data` blob carries its full detail.
fn detail_loaded_default() -> bool {
    true
}

impl AgentSession {
    pub const DEFAULT_TITLE: &'static str = "New task";

    pub fn new(project_id: Uuid, provider: ProviderKind) -> Self {
        let now = unix_time();
        Self {
            id: Uuid::new_v4(),
            title: Self::DEFAULT_TITLE.to_owned(),
            auto_title: None,
            project_id,
            workspace: SessionWorkspace::Local,
            provider,
            model: None,
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Build,
            reasoning_effort: None,
            service_tier: None,
            status: SessionStatus::Idle,
            created_at: now,
            updated_at: now,
            last_reply_at: None,
            detail_loaded: true,
            provider_cursor: None,
            available_commands: Vec::new(),
            context_usage: None,
            provider_session_id: None,
            messages: Vec::new(),
            transcript_blocks: Vec::new(),
            turns: Vec::new(),
            queued_messages: Vec::new(),
        }
    }

    pub fn is_busy(&self) -> bool {
        self.status.is_busy()
    }

    /// Derives [`Self::last_reply_at`] from the turn history when it is not
    /// already known, so a session stored before the field existed still sorts
    /// and displays correctly.
    pub fn backfill_last_reply_at(&mut self) {
        if self.last_reply_at.is_some() {
            return;
        }
        self.last_reply_at = self
            .turns
            .iter()
            .rev()
            .find_map(|turn| turn.completed_at)
            .filter(|_| self.has_started());
    }

    pub fn has_started(&self) -> bool {
        // A skeleton came from a stored row, and only started sessions are
        // stored, so it has started even though its transcript is not loaded.
        !self.detail_loaded
            || !self.turns.is_empty()
            || !self.messages.is_empty()
            || self.provider_cursor.is_some()
    }

    pub fn display_title(&self) -> &str {
        if self.title != Self::DEFAULT_TITLE && !self.title.trim().is_empty() {
            &self.title
        } else {
            self.auto_title
                .as_deref()
                .filter(|title| !title.trim().is_empty())
                .unwrap_or(Self::DEFAULT_TITLE)
        }
    }

    pub fn set_title_from_prompt(&mut self, prompt: &str) {
        if self.messages.len() > 1 || self.title != Self::DEFAULT_TITLE || self.auto_title.is_some()
        {
            return;
        }
        let mut title = prompt
            .split_whitespace()
            .take(7)
            .collect::<Vec<_>>()
            .join(" ");
        if !title.is_empty() {
            if title.chars().count() > 54 {
                title = format!("{}…", title.chars().take(53).collect::<String>());
            }
            self.auto_title = Some(title);
        }
    }

    /// Replaces the provider-owned title without disturbing an explicit user
    /// title. Returns whether the stored fallback changed.
    pub fn set_auto_title(&mut self, title: Option<String>) -> bool {
        let title = title.and_then(|title| {
            let title = title.trim();
            (!title.is_empty()).then(|| title.to_owned())
        });
        if self.auto_title == title {
            return false;
        }
        self.auto_title = title;
        self.updated_at = unix_time();
        true
    }

    pub fn can_choose_model(&self, provider: ProviderKind) -> bool {
        !self.status.is_busy() && (self.messages.is_empty() || self.provider == provider)
    }

    pub fn migrate_legacy_state(&mut self) {
        if self.runtime_mode == RuntimeMode::Plan {
            self.runtime_mode = RuntimeMode::Ask;
            self.interaction_mode = InteractionMode::Plan;
        }
        if self.provider_cursor.is_none()
            && let Some(id) = self.provider_session_id.take()
        {
            self.provider_cursor = Some(ProviderResumeCursor::from_session_id(self.provider, id));
        }
        if self.provider == ProviderKind::Codex {
            for message in &mut self.messages {
                if message.role == MessageRole::Assistant && message.content.contains('\u{e200}') {
                    message.content = strip_legacy_codex_citations(&message.content);
                }
            }
        }
        for block in &mut self.transcript_blocks {
            let TranscriptBlockContent::Activities(activities) = &mut block.content else {
                continue;
            };
            for activity in activities {
                if activity.kind == ActivityKind::Search && activity.title.trim() == "Search for" {
                    activity.title = tr!("activity.browsed_web");
                }
            }
        }

        // Checkpoints written before cached totals were added still have the
        // complete file list. Backfill once on load rather than making every
        // transcript frame rediscover the same totals.
        for turn in &mut self.turns {
            if let Some(checkpoint) = turn.checkpoint.as_mut() {
                checkpoint.refresh_totals();
            }
        }

        if !self.turns.is_empty()
            || !self
                .messages
                .iter()
                .any(|message| message.role == MessageRole::User)
        {
            return;
        }

        let user_indexes = self
            .messages
            .iter()
            .enumerate()
            .filter_map(|(index, message)| (message.role == MessageRole::User).then_some(index))
            .collect::<Vec<_>>();
        for (offset, start) in user_indexes.iter().copied().enumerate() {
            let end = user_indexes
                .get(offset + 1)
                .copied()
                .unwrap_or(self.messages.len());
            let id = Uuid::new_v4();
            let started_at = self.messages[start].created_at;
            let completed_at = self.messages[start..end]
                .iter()
                .map(|message| message.created_at)
                .max()
                .unwrap_or(started_at);
            for message in &mut self.messages[start..end] {
                message.turn_id = Some(id);
            }
            for block in &mut self.transcript_blocks {
                if block.after_message > start && block.after_message <= end {
                    block.turn_id = Some(id);
                }
            }
            self.turns.push(AgentTurn {
                id,
                turn_count: offset + 1,
                status: TurnStatus::Completed,
                provider_turn_started: true,
                provider_resume_at: None,
                started_at,
                completed_at: Some(completed_at),
                checkpoint: None,
            });
        }
    }

    pub fn begin_turn(&mut self, prompt: impl Into<String>) -> Uuid {
        let id = Uuid::new_v4();
        let now = unix_time();
        self.turns.push(AgentTurn {
            id,
            turn_count: self.turns.len() + 1,
            status: TurnStatus::Running,
            provider_turn_started: false,
            provider_resume_at: None,
            started_at: now,
            completed_at: None,
            checkpoint: None,
        });
        self.messages
            .push(Message::new_for_turn(MessageRole::User, prompt, id));
        id
    }

    pub fn active_turn_id(&self) -> Option<Uuid> {
        self.turns
            .last()
            .filter(|turn| turn.status == TurnStatus::Running)
            .map(|turn| turn.id)
    }

    /// Undo [`Self::begin_turn`] for a turn whose provider never started —
    /// the submission-preparation failure path, where the prompt returns to
    /// the composer. The turn and its messages leave the transcript, and a
    /// first-prompt unwind also gives back the default title that
    /// [`Self::set_title_from_prompt`] replaced.
    pub fn unwind_unstarted_turn(&mut self, turn_id: Uuid) {
        let unstarted = self.turns.last().is_some_and(|turn| {
            turn.id == turn_id && turn.status == TurnStatus::Running && !turn.provider_turn_started
        });
        if !unstarted {
            return;
        }
        self.turns.pop();
        self.messages
            .retain(|message| message.turn_id != Some(turn_id));
        if self.messages.is_empty() {
            self.auto_title = None;
        }
    }

    pub fn mark_active_turn_provider_started(&mut self) {
        if let Some(turn) = self
            .turns
            .last_mut()
            .filter(|turn| turn.status == TurnStatus::Running)
        {
            turn.provider_turn_started = true;
        }
    }

    pub fn mark_active_turn_provider_resume_at(&mut self, message_id: String) {
        if let Some(turn) = self
            .turns
            .last_mut()
            .filter(|turn| turn.status == TurnStatus::Running)
        {
            turn.provider_resume_at = Some(message_id);
        }
    }

    pub fn provider_turns_after(&self, turn_count: usize) -> usize {
        self.turns
            .iter()
            .skip(turn_count)
            .filter(|turn| turn.provider_turn_started)
            .count()
    }

    pub fn finish_active_turn(&mut self, status: TurnStatus) -> Option<(Uuid, usize)> {
        let turn = self
            .turns
            .last_mut()
            .filter(|turn| turn.status == TurnStatus::Running)?;
        let completed_at = unix_time();
        turn.status = status;
        turn.completed_at = Some(completed_at);
        let result = (turn.id, turn.turn_count);
        self.last_reply_at = Some(completed_at);
        Some(result)
    }

    pub fn push_message(&mut self, role: MessageRole, content: impl Into<String>) -> Uuid {
        let message = match self.active_turn_id() {
            Some(turn_id) => Message::new_for_turn(role, content, turn_id),
            None => Message::new(role, content),
        };
        let id = message.id;
        self.messages.push(message);
        id
    }

    pub fn truncate_after_turn(&mut self, turn_count: usize) {
        let retained = self
            .turns
            .iter()
            .take(turn_count)
            .map(|turn| turn.id)
            .collect::<std::collections::HashSet<_>>();
        self.turns.truncate(turn_count);
        self.messages.retain(|message| {
            message
                .turn_id
                .is_none_or(|turn_id| retained.contains(&turn_id))
        });
        self.transcript_blocks.retain(|block| {
            block
                .turn_id
                .is_none_or(|turn_id| retained.contains(&turn_id))
        });
        let message_count = self.messages.len();
        for block in &mut self.transcript_blocks {
            block.after_message = block.after_message.min(message_count);
        }
        self.updated_at = unix_time();
    }

    pub fn fork_through_turn(
        &self,
        turn_count: usize,
        provider_cursor: ProviderResumeCursor,
    ) -> Option<Self> {
        if turn_count == 0 || turn_count > self.turns.len() {
            return None;
        }

        let fork_title = tr!("session.fork_title", title = self.display_title());
        let mut fork = self.clone();
        fork.truncate_after_turn(turn_count);
        let fork_id = Uuid::new_v4();
        let turn_ids = fork
            .turns
            .iter()
            .map(|turn| (turn.id, Uuid::new_v4()))
            .collect::<std::collections::HashMap<_, _>>();

        for turn in &mut fork.turns {
            turn.id = turn_ids[&turn.id];
        }
        for message in &mut fork.messages {
            message.id = Uuid::new_v4();
            if let Some(turn_id) = message.turn_id {
                message.turn_id = turn_ids.get(&turn_id).copied();
            }
            message.streaming = false;
        }
        for block in &mut fork.transcript_blocks {
            if let Some(turn_id) = block.turn_id {
                block.turn_id = turn_ids.get(&turn_id).copied();
            }
        }

        let now = unix_time();
        fork.id = fork_id;
        fork.title = Self::DEFAULT_TITLE.to_owned();
        fork.auto_title = Some(fork_title);
        fork.status = SessionStatus::Idle;
        fork.created_at = now;
        fork.updated_at = now;
        fork.provider_cursor = Some(provider_cursor);
        fork.provider_session_id = None;
        // A fork snapshots the conversation, not the pending follow-ups its
        // source session is still holding for the live agent.
        fork.queued_messages.clear();
        Some(fork)
    }
}

fn strip_legacy_codex_citations(text: &str) -> String {
    const START: char = '\u{e200}';
    const END: char = '\u{e201}';
    const SEPARATOR: char = '\u{e202}';

    let mut remaining = text;
    let mut output = String::with_capacity(text.len());
    while let Some(start) = remaining.find(START) {
        output.push_str(&remaining[..start]);
        let marker_start = start + START.len_utf8();
        let Some(end_offset) = remaining[marker_start..].find(END) else {
            output.push_str(&remaining[start..]);
            return output;
        };
        let marker_end = marker_start + end_offset;
        let marker = &remaining[marker_start..marker_end];
        if marker
            .split(SEPARATOR)
            .next()
            .is_some_and(|prefix| prefix != "cite")
        {
            output.push_str(&remaining[start..marker_end + END.len_utf8()]);
        }
        remaining = &remaining[marker_end + END.len_utf8()..];
    }
    output.push_str(remaining);
    output
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Message {
    pub id: Uuid,
    #[serde(default)]
    pub turn_id: Option<Uuid>,
    pub role: MessageRole,
    pub content: String,
    pub created_at: u64,
    pub streaming: bool,
}

impl Message {
    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            turn_id: None,
            role,
            content: content.into(),
            created_at: unix_time(),
            streaming: false,
        }
    }

    pub fn new_for_turn(role: MessageRole, content: impl Into<String>, turn_id: Uuid) -> Self {
        Self {
            turn_id: Some(turn_id),
            ..Self::new(role, content)
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ActivityKind {
    Reasoning,
    Command,
    FileChange,
    Search,
    Plan,
    Tool,
}

#[derive(Clone, Debug)]
pub enum DriverEvent {
    Connected {
        provider_cursor: Option<ProviderResumeCursor>,
    },
    /// A provider-owned, automatically generated session title. `None`
    /// clears that fallback but never overwrites a user-owned title.
    AutoTitleUpdated(Option<String>),
    /// The slash commands the live process itself reports — Claude's
    /// stream-json init handshake and ACP's `available_commands_update`.
    /// Authoritative over filesystem discovery, which cannot see plugin or
    /// dynamically registered commands.
    AvailableCommands(Vec<ReportedCommand>),
    TurnStarted,
    TextDelta(String),
    ReasoningDelta(String),
    Activity {
        id: Option<String>,
        kind: ActivityKind,
        title: String,
        detail: Option<String>,
        complete: bool,
    },
    RichActivity(ActivityItem),
    Permission {
        request_id: String,
        title: String,
        detail: String,
        options: Vec<PermissionOption>,
    },
    ComputerUseUpdated(crate::computer_use::ComputerUseState),
    /// The provider accepted a steering message into the running turn.
    SteerAccepted {
        message: String,
    },
    /// The provider could not steer the running turn (for example it ended
    /// before the request arrived). The app decides the fallback.
    SteerRejected {
        message: String,
        reason: String,
    },
    /// Context-window occupancy reported by the live stream. Fields arrive at
    /// different moments — token counts with each assistant message, the
    /// window size with the settled turn — so each is optional and the app
    /// merges them into [`ContextUsage`].
    UsageUpdated {
        context_tokens: Option<u64>,
        context_window: Option<u64>,
    },
    /// Account-level rate-limit meters carried by the provider's own stream
    /// (Codex's `account/rateLimits/updated`). Same shape the OAuth fetcher
    /// produces for Claude, so the panel renders both identically.
    PlanUsageUpdated(crate::usage::PlanUsage),
    TurnFinished {
        success: bool,
        summary: Option<String>,
    },
    Error(String),
    ProcessExited,
}

/// A slash command a live provider process advertised for its session.
///
/// Claude's init handshake reports bare names; ACP agents report names with
/// descriptions. Sessions persisted by earlier builds stored plain strings,
/// which the untagged repr still accepts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(from = "ReportedCommandRepr")]
pub struct ReportedCommand {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ReportedCommandRepr {
    Name(String),
    Full {
        name: String,
        #[serde(default)]
        description: String,
    },
}

impl From<ReportedCommandRepr> for ReportedCommand {
    fn from(repr: ReportedCommandRepr) -> Self {
        match repr {
            ReportedCommandRepr::Name(name) => Self {
                name,
                description: String::new(),
            },
            ReportedCommandRepr::Full { name, description } => Self { name, description },
        }
    }
}

#[derive(Clone, Debug)]
pub struct PermissionOption {
    pub id: String,
    pub label: String,
    pub allow: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ActivityItem {
    pub id: Uuid,
    #[serde(default)]
    pub source_id: Option<String>,
    pub kind: ActivityKind,
    pub title: String,
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Images returned by a tool, kept separate from text so large data URLs
    /// are never truncated or treated as literal activity output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_urls: Vec<String>,
    #[serde(default)]
    pub failed: bool,
    pub complete: bool,
}

impl ActivityItem {
    pub fn new(
        source_id: Option<String>,
        kind: ActivityKind,
        title: impl Into<String>,
        detail: Option<String>,
        complete: bool,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            source_id,
            kind,
            title: title.into(),
            detail,
            arguments: None,
            output: None,
            image_urls: Vec::new(),
            failed: false,
            complete,
        }
    }

    pub fn with_arguments(mut self, arguments: Option<String>) -> Self {
        self.arguments = arguments;
        self
    }

    pub fn with_output(mut self, output: Option<String>) -> Self {
        self.output = output;
        self
    }

    pub fn with_image_urls(mut self, image_urls: Vec<String>) -> Self {
        self.image_urls = image_urls;
        self
    }

    pub fn with_failed(mut self, failed: bool) -> Self {
        self.failed = failed;
        self
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReasoningBlock {
    pub content: String,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "data")]
pub enum TranscriptBlockContent {
    Reasoning(ReasoningBlock),
    Activities(Vec<ActivityItem>),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TranscriptBlock {
    /// Render this block immediately after this many persisted messages.
    pub after_message: usize,
    #[serde(default)]
    pub turn_id: Option<Uuid>,
    pub content: TranscriptBlockContent,
}

#[derive(Clone, Debug)]
pub struct PendingPermission {
    pub request_id: String,
    pub title: String,
    pub detail: String,
    pub options: Vec<PermissionOption>,
}

pub fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

pub fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

pub fn compact_path(path: &Path) -> String {
    let components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    if components.len() <= 3 {
        return path.display().to_string();
    }
    format!(
        "…/{}/{}",
        components[components.len() - 2],
        components[components.len() - 1]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projectless_projects_are_descendants_of_the_waku_root() {
        let home = dirs::home_dir().expect("test user has a home directory");
        let root = home.join(".waku");
        let legacy = Project::from_path(root.clone());
        let project = Project::from_path(root.join("2026-08-08/new-chat"));
        let ordinary = Project::from_path(home.join("dev/waku"));

        assert!(legacy.is_projectless());
        assert!(project.is_projectless());
        assert!(!ordinary.is_projectless());
    }

    #[test]
    fn prompt_generates_a_short_session_title() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);
        session.set_title_from_prompt("build a really polished local agent interface for rust");
        assert_eq!(
            session.auto_title.as_deref(),
            Some("build a really polished local agent interface")
        );
        assert_eq!(
            session.display_title(),
            "build a really polished local agent interface"
        );
        assert_eq!(session.title, AgentSession::DEFAULT_TITLE);
    }

    #[test]
    fn provider_title_replaces_prompt_fallback_but_not_an_explicit_title() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::OpenCode);
        session.set_title_from_prompt("investigate the broken provider event");

        assert!(session.set_auto_title(Some("Fix provider title events".into())));
        assert_eq!(session.display_title(), "Fix provider title events");

        session.title = "My title".into();
        assert!(session.set_auto_title(Some("A newer provider title".into())));
        assert_eq!(session.display_title(), "My title");
    }

    #[test]
    fn model_selection_keeps_started_sessions_on_their_provider() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);

        assert!(session.can_choose_model(ProviderKind::Claude));

        session.push_message(MessageRole::User, "first turn");
        assert!(session.can_choose_model(ProviderKind::Codex));
        assert!(!session.can_choose_model(ProviderKind::Claude));
    }

    #[test]
    fn model_selection_waits_for_the_active_turn_to_finish() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);
        session.push_message(MessageRole::User, "first turn");

        for status in [
            SessionStatus::Connecting,
            SessionStatus::Working,
            SessionStatus::Waiting,
        ] {
            session.status = status;
            assert!(!session.can_choose_model(ProviderKind::Codex));
        }

        session.status = SessionStatus::Idle;
        assert!(session.can_choose_model(ProviderKind::Codex));
    }

    #[test]
    fn provider_ids_are_stable() {
        assert_eq!(ProviderKind::Amp.id(), "amp");
        assert_eq!(ProviderKind::Claude.id(), "claude");
        assert_eq!(ProviderKind::Codex.command(), "codex");
        assert_eq!(ProviderKind::Cursor.command(), "cursor-agent");
        assert_eq!(ProviderKind::OpenCode.command(), "opencode");
        assert_eq!(ProviderKind::Grok.command(), "grok");
        assert_eq!(ProviderKind::Pi.command(), "pi");
    }

    #[test]
    fn native_conversation_actions_include_every_provider() {
        for provider in [
            ProviderKind::Amp,
            ProviderKind::Claude,
            ProviderKind::Codex,
            ProviderKind::Cursor,
            ProviderKind::OpenCode,
            ProviderKind::Grok,
            ProviderKind::Pi,
        ] {
            assert!(provider.supports_conversation_fork());
            assert!(provider.supports_conversation_rollback());
        }
    }

    #[test]
    fn only_dynamic_provider_catalogs_are_discovered() {
        assert!(!ProviderKind::Amp.supports_model_discovery());
        assert!(!ProviderKind::Claude.supports_model_discovery());
        assert!(ProviderKind::Codex.supports_model_discovery());
        assert!(ProviderKind::Cursor.supports_model_discovery());
        assert!(ProviderKind::OpenCode.supports_model_discovery());
        assert!(ProviderKind::Grok.supports_model_discovery());
        assert!(ProviderKind::Pi.supports_model_discovery());
    }

    #[test]
    fn prompt_title_truncation_is_unicode_safe() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Claude);
        let prompt = "界".repeat(70);
        session.set_title_from_prompt(&prompt);
        let title = session.auto_title.as_deref().unwrap();
        assert_eq!(title.chars().count(), 54);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn a_failed_preparation_unwinds_the_turn_it_eagerly_began() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);

        // A first prompt: the unwind restores the untouched session, default
        // title included, because the prompt returns to the composer.
        session.set_title_from_prompt("Build the thing");
        let turn_id = session.begin_turn("Build the thing");
        session.unwind_unstarted_turn(turn_id);
        assert!(session.turns.is_empty());
        assert!(session.messages.is_empty());
        assert_eq!(session.title, AgentSession::DEFAULT_TITLE);
        assert!(session.auto_title.is_none());

        // A follow-up prompt unwinds only itself.
        let first = session.begin_turn("first");
        session.push_message(MessageRole::Assistant, "done");
        session.finish_active_turn(TurnStatus::Completed);
        session.set_title_from_prompt("first");
        let follow_up = session.begin_turn("second");
        session.unwind_unstarted_turn(follow_up);
        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].id, first);
        assert_eq!(session.messages.len(), 2);

        // A turn the provider has already started never unwinds — losing a
        // live conversation turn would desync the provider transcript.
        let started = session.begin_turn("third");
        session.mark_active_turn_provider_started();
        session.unwind_unstarted_turn(started);
        assert_eq!(session.turns.len(), 2);
        assert_eq!(session.messages.len(), 3);
    }

    #[test]
    fn turn_truncation_removes_owned_messages_and_blocks() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);

        let first_turn = session.begin_turn("first");
        session.push_message(MessageRole::Assistant, "first answer");
        session.transcript_blocks.push(TranscriptBlock {
            after_message: 2,
            turn_id: Some(first_turn),
            content: TranscriptBlockContent::Activities(Vec::new()),
        });
        session.finish_active_turn(TurnStatus::Completed);

        let second_turn = session.begin_turn("second");
        session.push_message(MessageRole::Assistant, "second answer");
        session.transcript_blocks.push(TranscriptBlock {
            after_message: 4,
            turn_id: Some(second_turn),
            content: TranscriptBlockContent::Activities(Vec::new()),
        });
        session.finish_active_turn(TurnStatus::Completed);

        session.truncate_after_turn(1);

        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].id, first_turn);
        assert_eq!(session.messages.len(), 2);
        assert!(
            session
                .messages
                .iter()
                .all(|message| message.turn_id == Some(first_turn))
        );
        assert_eq!(session.transcript_blocks.len(), 1);
        assert_eq!(session.transcript_blocks[0].turn_id, Some(first_turn));
        assert_eq!(session.transcript_blocks[0].after_message, 2);
    }

    #[test]
    fn response_fork_is_a_distinct_idle_session_through_the_selected_turn() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);

        let first_turn = session.begin_turn("first");
        let first_message = session.push_message(MessageRole::Assistant, "first answer");
        session.finish_active_turn(TurnStatus::Completed);
        session.begin_turn("second");
        session.push_message(MessageRole::Assistant, "second answer");
        session.finish_active_turn(TurnStatus::Completed);

        let fork = session
            .fork_through_turn(
                1,
                ProviderResumeCursor::Codex {
                    thread_id: "forked-thread".into(),
                },
            )
            .unwrap();

        assert_ne!(fork.id, session.id);
        assert_eq!(fork.title, AgentSession::DEFAULT_TITLE);
        assert_eq!(fork.auto_title.as_deref(), Some("New task (fork)"));
        assert_eq!(fork.status, SessionStatus::Idle);
        assert_eq!(fork.turns.len(), 1);
        assert_eq!(fork.messages.len(), 2);
        assert_ne!(fork.turns[0].id, first_turn);
        assert_ne!(fork.messages[1].id, first_message);
        assert!(
            fork.messages
                .iter()
                .all(|message| message.turn_id == Some(fork.turns[0].id))
        );
        assert!(matches!(
            fork.provider_cursor,
            Some(ProviderResumeCursor::Codex { ref thread_id }) if thread_id == "forked-thread"
        ));
    }

    #[test]
    fn queued_follow_ups_stay_with_the_source_session_not_the_fork() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);

        session.begin_turn("first");
        session.push_message(MessageRole::Assistant, "first answer");
        session.finish_active_turn(TurnStatus::Completed);
        session
            .queued_messages
            .push(QueuedMessage::new("after you finish, also…"));

        let fork = session
            .fork_through_turn(
                1,
                ProviderResumeCursor::Codex {
                    thread_id: "forked-thread".into(),
                },
            )
            .unwrap();

        assert_eq!(session.queued_messages.len(), 1);
        assert!(fork.queued_messages.is_empty());
    }

    #[test]
    fn follow_up_queue_round_trips_through_serde() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);
        session
            .queued_messages
            .push(QueuedMessage::new("first follow-up"));
        session
            .queued_messages
            .push(QueuedMessage::new("second follow-up"));

        let value = serde_json::to_value(&session).unwrap();
        assert!(value["queued_messages"].is_array());
        let restored: AgentSession = serde_json::from_value(value).unwrap();
        assert_eq!(restored.queued_messages.len(), 2);
        assert_eq!(restored.queued_messages[0].content, "first follow-up");
        assert_eq!(restored.queued_messages[1].content, "second follow-up");
        assert_ne!(
            restored.queued_messages[0].id,
            restored.queued_messages[1].id
        );

        // Sessions without the field (older state files) deserialize as empty.
        let mut legacy = serde_json::to_value(&session).unwrap();
        legacy.as_object_mut().unwrap().remove("queued_messages");
        let legacy_session: AgentSession = serde_json::from_value(legacy).unwrap();
        assert!(legacy_session.queued_messages.is_empty());
    }

    #[test]
    fn planned_worktree_base_branch_is_optional_and_round_trips() {
        let legacy: SessionWorkspace =
            serde_json::from_value(serde_json::json!({ "kind": "newWorktree" })).unwrap();
        assert_eq!(legacy, SessionWorkspace::NewWorktree { base_branch: None });

        let selected = SessionWorkspace::NewWorktree {
            base_branch: Some("release/next".into()),
        };
        let restored = serde_json::from_value(serde_json::to_value(&selected).unwrap()).unwrap();
        assert_eq!(selected, restored);
    }

    #[test]
    fn busy_statuses_cover_connecting_working_and_waiting() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);
        for status in [
            SessionStatus::Connecting,
            SessionStatus::Working,
            SessionStatus::Waiting,
        ] {
            session.status = status;
            assert!(session.is_busy());
        }
        for status in [SessionStatus::Idle, SessionStatus::Failed] {
            session.status = status;
            assert!(!session.is_busy());
        }
    }

    #[test]
    fn provider_resume_cursor_is_explicitly_tagged() {
        let cursor = ProviderResumeCursor::Claude {
            session_id: "session-1".into(),
            resume_at: Some("message-9".into()),
        };
        let value = serde_json::to_value(&cursor).unwrap();
        assert_eq!(value["provider"], "claude");
        assert_eq!(value["sessionId"], "session-1");
        assert_eq!(value["resumeAt"], "message-9");

        let cursor = ProviderResumeCursor::Cursor {
            session_id: String::new(),
            fork_context: Some("[]".into()),
        };
        let value = serde_json::to_value(&cursor).unwrap();
        assert_eq!(value["provider"], "cursor");
        assert_eq!(value["sessionId"], "");
        assert_eq!(value["forkContext"], "[]");
    }

    #[test]
    fn native_rollback_count_ignores_turns_that_never_reached_the_provider() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);

        session.begin_turn("first");
        session.mark_active_turn_provider_started();
        session.finish_active_turn(TurnStatus::Completed);
        session.begin_turn("failed locally");
        session.finish_active_turn(TurnStatus::Failed);
        session.begin_turn("third");
        session.mark_active_turn_provider_started();
        session.finish_active_turn(TurnStatus::Completed);

        assert_eq!(session.provider_turns_after(1), 1);
        assert_eq!(session.provider_turns_after(2), 1);
        assert_eq!(session.provider_turns_after(3), 0);
    }

    #[test]
    fn legacy_empty_search_titles_are_repaired() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);
        session.transcript_blocks.push(TranscriptBlock {
            after_message: 0,
            turn_id: None,
            content: TranscriptBlockContent::Activities(vec![ActivityItem::new(
                Some("search-1".into()),
                ActivityKind::Search,
                "Search for ",
                None,
                true,
            )]),
        });

        session.migrate_legacy_state();

        let TranscriptBlockContent::Activities(activities) = &session.transcript_blocks[0].content
        else {
            panic!("expected activities");
        };
        assert_eq!(activities[0].title, "Browsed the web");
    }

    #[test]
    fn legacy_codex_citation_markers_are_removed() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);
        session.messages.push(Message::new(
            MessageRole::Assistant,
            "Claim.\u{e200}cite\u{e202}turn3view0\u{e202}turn2view2\u{e201}\nNext.",
        ));

        session.migrate_legacy_state();

        assert_eq!(session.messages[0].content, "Claim.\nNext.");
    }

    #[test]
    fn legacy_checkpoint_totals_are_backfilled_from_the_file_summary() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);
        session.begin_turn("Build it");
        session.finish_active_turn(TurnStatus::Completed);
        let mut serialized = serde_json::to_value(Checkpoint {
            turn_count: 1,
            git_ref: "refs/waku/test".into(),
            status: CheckpointStatus::Ready,
            files: vec![
                CheckpointFile {
                    path: "src/app.rs".into(),
                    additions: 7,
                    deletions: 2,
                },
                CheckpointFile {
                    path: "src/model.rs".into(),
                    additions: 3,
                    deletions: 5,
                },
            ],
            additions: 10,
            deletions: 7,
            created_at: 1,
        })
        .unwrap();
        let object = serialized.as_object_mut().unwrap();
        object.remove("additions");
        object.remove("deletions");
        let checkpoint: Checkpoint = serde_json::from_value(serialized).unwrap();
        assert_eq!((checkpoint.additions, checkpoint.deletions), (0, 0));
        session.turns[0].checkpoint = Some(checkpoint);

        session.migrate_legacy_state();

        let checkpoint = session.turns[0].checkpoint.as_ref().unwrap();
        assert_eq!((checkpoint.additions, checkpoint.deletions), (10, 7));
        assert!(checkpoint.totals_are_current());
    }
}
