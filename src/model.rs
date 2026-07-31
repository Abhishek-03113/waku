use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderKind {
    Claude,
    #[default]
    Codex,
    OpenCode,
    Grok,
}

impl ProviderKind {
    pub const ALL: [Self; 4] = [Self::Claude, Self::Codex, Self::OpenCode, Self::Grok];

    pub fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Grok => "grok",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex CLI",
            Self::OpenCode => "OpenCode",
            Self::Grok => "Grok Build",
        }
    }

    pub fn short_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
            Self::Grok => "Grok",
        }
    }

    pub fn command(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Grok => "grok",
        }
    }

    pub fn supports_conversation_rollback(self) -> bool {
        matches!(self, Self::Codex)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "provider"
)]
pub enum ProviderResumeCursor {
    Claude {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume_at: Option<String>,
    },
    Codex {
        thread_id: String,
    },
    OpenCode {
        session_id: String,
    },
    Grok {
        session_id: String,
    },
}

impl ProviderResumeCursor {
    pub fn from_session_id(provider: ProviderKind, id: String) -> Self {
        match provider {
            ProviderKind::Claude => Self::Claude {
                session_id: id,
                resume_at: None,
            },
            ProviderKind::Codex => Self::Codex { thread_id: id },
            ProviderKind::OpenCode => Self::OpenCode { session_id: id },
            ProviderKind::Grok => Self::Grok { session_id: id },
        }
    }

    pub fn provider(&self) -> ProviderKind {
        match self {
            Self::Claude { .. } => ProviderKind::Claude,
            Self::Codex { .. } => ProviderKind::Codex,
            Self::OpenCode { .. } => ProviderKind::OpenCode,
            Self::Grok { .. } => ProviderKind::Grok,
        }
    }

    pub fn native_id(&self) -> &str {
        match self {
            Self::Claude { session_id, .. }
            | Self::OpenCode { session_id }
            | Self::Grok { session_id } => session_id,
            Self::Codex { thread_id } => thread_id,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeMode {
    Plan,
    #[default]
    Ask,
    Auto,
}

impl RuntimeMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Plan => "Plan",
            Self::Ask => "Ask",
            Self::Auto => "Auto",
        }
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
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProviderProbe {
    pub provider: ProviderKind,
    pub installed: bool,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
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
            version: None,
            models: crate::model_catalog::fallback_models(provider),
        }
    }

    pub fn detect(provider: ProviderKind) -> Self {
        let mut probe = Self::pending(provider);
        let path = probe.path.clone();
        let version = path.as_ref().and_then(|path| {
            std::process::Command::new(path)
                .arg("--version")
                .output()
                .ok()
                .and_then(|output| {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let value = format!("{stdout}{stderr}");
                    value
                        .lines()
                        .find(|line| !line.trim().is_empty())
                        .map(|line| line.trim().to_owned())
                })
        });
        if let Some(path) = path.as_deref() {
            probe.models = crate::model_catalog::discover_models(provider, path);
        }
        probe.version = version;
        probe
    }

    pub fn preferred_model(&self) -> Option<&ProviderModel> {
        self.models
            .iter()
            .find(|model| model.is_default)
            .or_else(|| self.models.first())
    }
}

fn find_in_path(command: &str) -> Option<PathBuf> {
    let candidate = Path::new(command);
    if candidate.components().count() > 1 && candidate.is_file() {
        return Some(candidate.to_path_buf());
    }
    let mut directories = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        directories.extend([
            home.join(".local/bin"),
            home.join(".bun/bin"),
            home.join(".cargo/bin"),
            home.join(".local/share/mise/shims"),
        ]);
    }
    directories.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
    ]);
    directories
        .into_iter()
        .map(|directory| directory.join(command))
        .find(|candidate| candidate.is_file())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub path: PathBuf,
}

impl Project {
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
        }
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
    pub created_at: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentTurn {
    pub id: Uuid,
    pub turn_count: usize,
    pub status: TurnStatus,
    #[serde(default)]
    pub provider_turn_started: bool,
    pub started_at: u64,
    pub completed_at: Option<u64>,
    #[serde(default)]
    pub checkpoint: Option<Checkpoint>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentSession {
    pub id: Uuid,
    pub title: String,
    pub project_id: Uuid,
    pub provider: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub runtime_mode: RuntimeMode,
    pub status: SessionStatus,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub provider_cursor: Option<ProviderResumeCursor>,
    /// Read-only compatibility field for v1 state files. New saves omit it.
    #[serde(default, skip_serializing)]
    pub provider_session_id: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub transcript_blocks: Vec<TranscriptBlock>,
    #[serde(default)]
    pub turns: Vec<AgentTurn>,
}

impl AgentSession {
    pub fn new(project_id: Uuid, provider: ProviderKind) -> Self {
        let now = unix_time();
        Self {
            id: Uuid::new_v4(),
            title: "New task".to_owned(),
            project_id,
            provider,
            model: None,
            runtime_mode: RuntimeMode::Ask,
            status: SessionStatus::Idle,
            created_at: now,
            updated_at: now,
            provider_cursor: None,
            provider_session_id: None,
            messages: Vec::new(),
            transcript_blocks: Vec::new(),
            turns: Vec::new(),
        }
    }

    pub fn set_title_from_prompt(&mut self, prompt: &str) {
        if self.messages.len() > 1 || self.title != "New task" {
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
            self.title = title;
        }
    }

    pub fn migrate_legacy_state(&mut self) {
        if self.provider_cursor.is_none()
            && let Some(id) = self.provider_session_id.take()
        {
            self.provider_cursor = Some(ProviderResumeCursor::from_session_id(self.provider, id));
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

    pub fn mark_active_turn_provider_started(&mut self) {
        if let Some(turn) = self
            .turns
            .last_mut()
            .filter(|turn| turn.status == TurnStatus::Running)
        {
            turn.provider_turn_started = true;
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
        turn.status = status;
        turn.completed_at = Some(unix_time());
        Some((turn.id, turn.turn_count))
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
    Permission {
        request_id: String,
        title: String,
        detail: String,
        options: Vec<PermissionOption>,
    },
    TurnFinished {
        success: bool,
        summary: Option<String>,
    },
    Error(String),
    ProcessExited,
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
            complete,
        }
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
    fn prompt_generates_a_short_session_title() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Codex);
        session.set_title_from_prompt("build a really polished local agent interface for rust");
        assert_eq!(
            session.title,
            "build a really polished local agent interface"
        );
    }

    #[test]
    fn provider_ids_are_stable() {
        assert_eq!(ProviderKind::Claude.id(), "claude");
        assert_eq!(ProviderKind::Codex.command(), "codex");
        assert_eq!(ProviderKind::OpenCode.command(), "opencode");
        assert_eq!(ProviderKind::Grok.command(), "grok");
    }

    #[test]
    fn prompt_title_truncation_is_unicode_safe() {
        let project = Project::from_path(PathBuf::from("/tmp/waku"));
        let mut session = AgentSession::new(project.id, ProviderKind::Claude);
        let prompt = "界".repeat(70);
        session.set_title_from_prompt(&prompt);
        assert_eq!(session.title.chars().count(), 54);
        assert!(session.title.ends_with('…'));
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
    fn provider_resume_cursor_is_explicitly_tagged() {
        let cursor = ProviderResumeCursor::Claude {
            session_id: "session-1".into(),
            resume_at: Some("message-9".into()),
        };
        let value = serde_json::to_value(&cursor).unwrap();
        assert_eq!(value["provider"], "claude");
        assert_eq!(value["sessionId"], "session-1");
        assert_eq!(value["resumeAt"], "message-9");
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
}
