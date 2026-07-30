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

    pub fn glyph(self) -> &'static str {
        match self {
            Self::Claude => "C",
            Self::Codex => "O",
            Self::OpenCode => "◈",
            Self::Grok => "X",
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

    pub fn integration_label(self) -> &'static str {
        match self {
            Self::Claude => "stream-json",
            Self::Codex => "app-server",
            Self::OpenCode => "JSON events",
            Self::Grok => "ACP",
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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProviderProbe {
    pub provider: ProviderKind,
    pub installed: bool,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
}

impl ProviderProbe {
    pub fn detect(provider: ProviderKind) -> Self {
        let path = find_in_path(provider.command());
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
        Self {
            provider,
            installed: path.is_some(),
            path,
            version,
        }
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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentSession {
    pub id: Uuid,
    pub title: String,
    pub project_id: Uuid,
    pub provider: ProviderKind,
    pub runtime_mode: RuntimeMode,
    pub status: SessionStatus,
    pub created_at: u64,
    pub updated_at: u64,
    pub provider_session_id: Option<String>,
    pub messages: Vec<Message>,
}

impl AgentSession {
    pub fn new(project_id: Uuid, provider: ProviderKind) -> Self {
        let now = unix_time();
        Self {
            id: Uuid::new_v4(),
            title: "New task".to_owned(),
            project_id,
            provider,
            runtime_mode: RuntimeMode::Ask,
            status: SessionStatus::Idle,
            created_at: now,
            updated_at: now,
            provider_session_id: None,
            messages: Vec::new(),
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
    pub role: MessageRole,
    pub content: String,
    pub created_at: u64,
    pub streaming: bool,
}

impl Message {
    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            role,
            content: content.into(),
            created_at: unix_time(),
            streaming: false,
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
        provider_session_id: Option<String>,
    },
    TurnStarted,
    TextDelta(String),
    ReasoningDelta(String),
    Activity {
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

#[derive(Clone, Debug)]
pub struct ActivityItem {
    pub id: Uuid,
    pub kind: ActivityKind,
    pub title: String,
    pub detail: Option<String>,
    pub complete: bool,
}

impl ActivityItem {
    pub fn new(
        kind: ActivityKind,
        title: impl Into<String>,
        detail: Option<String>,
        complete: bool,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            kind,
            title: title.into(),
            detail,
            complete,
        }
    }
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
        assert_eq!(ProviderKind::OpenCode.integration_label(), "JSON events");
        assert_eq!(ProviderKind::Grok.integration_label(), "ACP");
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
}
