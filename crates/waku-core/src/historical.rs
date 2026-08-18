//! Provider-neutral historical session discovery.
//!
//! This module provides an abstraction for discovering and importing historical
//! agent sessions from various providers' native storage locations.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::claude_session;
use crate::model::ProviderKind;

/// Represents a discovered historical session from a provider's native storage.
#[derive(Debug, Clone)]
pub struct HistoricalSession {
    /// The provider that owns this session.
    pub provider: ProviderKind,
    /// Provider's native session identifier (e.g., Claude's UUID).
    pub native_session_id: String,
    /// Working directory extracted from the session, if available.
    pub cwd: Option<PathBuf>,
    /// Title of the session (AI-generated or custom).
    pub title: Option<String>,
    /// When the session was created (Unix timestamp in seconds).
    pub created_at: u64,
    /// When the session was last updated (Unix timestamp in seconds).
    pub updated_at: u64,
    /// Parsed messages from the session.
    pub messages: Vec<HistoricalMessage>,
}

/// A single message within a historical session.
#[derive(Debug, Clone)]
pub struct HistoricalMessage {
    /// Message UUID from the provider.
    pub uuid: String,
    /// Parent message UUID (for chain reconstruction).
    pub parent_uuid: Option<String>,
    /// Role of the message sender.
    pub role: MessageRole,
    /// The content of the message.
    pub content: String,
    /// Display content (user-visible text).
    pub display_content: Option<String>,
    /// When the message was created.
    pub created_at: u64,
}

/// Message role within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

impl MessageRole {
    pub fn from_provider_str(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

/// Trait for discovering historical sessions from a provider's storage.
pub trait HistoricalSessionDiscovery {
    /// Returns the provider this discovery implementation supports.
    fn provider(&self) -> ProviderKind;

    /// Discover all historical sessions from this provider.
    ///
    /// Returns a list of sessions found. Individual parse failures should be
    /// skipped gracefully rather than aborting the entire discovery.
    fn discover(&self) -> impl FnOnce() -> Result<Vec<HistoricalSession>> + Send + 'static;
}

/// Result of discovering sessions for all providers.
#[derive(Debug, Default)]
pub struct DiscoveryResult {
    /// Discovered sessions, keyed by (provider, native_session_id).
    pub sessions: HashMap<(ProviderKind, String), HistoricalSession>,
    /// Errors encountered during discovery, keyed by provider.
    pub errors: HashMap<ProviderKind, String>,
}

/// Discover historical sessions from all supported providers.
///
/// This is the main entry point for background discovery. It runs discovery
/// for each provider and collects results, handling errors gracefully.
pub fn discover_all_providers() -> impl FnOnce() -> Result<DiscoveryResult> + Send + 'static {
    || {
        let mut result = DiscoveryResult::default();

        // Discover from Claude (currently the only supported provider)
        let claude_discovery = ClaudeHistoricalDiscovery::new();
        match (claude_discovery.discover())() {
            Ok(sessions) => {
                for session in sessions {
                    let key = (session.provider, session.native_session_id.clone());
                    result.sessions.insert(key, session);
                }
            }
            Err(e) => {
                result.errors.insert(ProviderKind::Claude, e.to_string());
            }
        }

        Ok(result)
    }
}

/// Historical session discovery for Claude Code.
pub struct ClaudeHistoricalDiscovery {
    /// Base directory to scan (defaults to ~/.claude/projects).
    projects_dir: Option<PathBuf>,
}

impl ClaudeHistoricalDiscovery {
    pub fn new() -> Self {
        Self { projects_dir: None }
    }

    /// Create a discovery instance with a custom projects directory (for testing).
    #[cfg(test)]
    pub fn with_projects_dir(path: PathBuf) -> Self {
        Self {
            projects_dir: Some(path),
        }
    }

    fn projects_directory(&self) -> PathBuf {
        if let Some(ref dir) = self.projects_dir {
            return dir.clone();
        }
        let config_directory = std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
            .unwrap_or_else(|| PathBuf::from(".claude"));
        config_directory.join("projects")
    }

    /// Discover all Claude sessions from the projects directory.
    fn discover_claude_sessions(&self) -> Result<Vec<HistoricalSession>> {
        let projects_dir = self.projects_directory();
        let mut sessions = Vec::new();

        // Check if the directory exists
        if !projects_dir.exists() {
            return Ok(sessions);
        }

        // Walk through all subdirectories looking for .jsonl files
        self.walk_projects_dir(&projects_dir, &mut sessions)?;

        Ok(sessions)
    }

    fn walk_projects_dir(&self, dir: &Path, sessions: &mut Vec<HistoricalSession>) -> Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue, // Skip entries we can't read
            };
            let path = entry.path();

            if path.is_dir() {
                // Recursively search subdirectories
                self.walk_projects_dir(&path, sessions)?;
            } else if path.extension().is_some_and(|ext| ext == "jsonl") {
                // Try to parse as a Claude session file
                // Skip individual file errors gracefully
                if let Some(session) = self.parse_session_file(&path) {
                    sessions.push(session);
                }
            }
        }

        Ok(())
    }

    fn parse_session_file(&self, path: &Path) -> Option<HistoricalSession> {
        // Get session ID from the filename (UUID.jsonl)
        let filename = path.file_stem()?;
        let native_session_id = filename.to_string_lossy().to_string();

        // Validate it looks like a UUID
        Uuid::parse_str(&native_session_id).ok()?;

        // Reuse claude_session.rs reading logic
        let entries = claude_session::read_entries(path).ok()?;
        if entries.is_empty() {
            return None;
        }

        // Extract session metadata
        self.extract_session_from_entries(&entries, &native_session_id)
    }

    fn extract_session_from_entries(
        &self,
        entries: &[serde_json::Value],
        native_session_id: &str,
    ) -> Option<HistoricalSession> {
        // Reuse claude_session.rs's transcript filtering so historical import
        // stays in step with the same TRANSCRIPT_TYPES, uuid, and sidechain
        // rules the live fork/resume paths already rely on.
        if claude_session::transcript_entries(entries).is_empty() {
            return None;
        }

        // Extract cwd from any entry that has it
        let cwd = entries
            .iter()
            .filter_map(|v| v.get("cwd").and_then(|v| v.as_str()))
            .next()
            .map(PathBuf::from);

        // Extract title using claude_session.rs's own title-tracking logic
        let title = entries
            .iter()
            .filter_map(claude_session::claude_title)
            .last();

        // Parse messages from the active chain using claude_session.rs logic,
        // then derive session-level timestamps from the same chain.
        let messages = self.parse_active_message_chain(entries);
        let (created_at, updated_at) = extract_timestamps(&messages);

        Some(HistoricalSession {
            provider: ProviderKind::Claude,
            native_session_id: native_session_id.to_owned(),
            cwd,
            title,
            created_at,
            updated_at,
            messages,
        })
    }

    /// Walk the active message chain (the same branch Claude itself would
    /// resume from) and convert each transcript entry into a flattened,
    /// human-readable `HistoricalMessage`.
    fn parse_active_message_chain(&self, entries: &[serde_json::Value]) -> Vec<HistoricalMessage> {
        claude_session::active_chain(entries)
            .into_iter()
            .filter_map(|entry| {
                let uuid = entry.get("uuid")?.as_str()?.to_owned();
                let entry_type = entry.get("type").and_then(|v| v.as_str());
                // Only user/assistant/system turns become messages; progress
                // and attachment entries only exist to keep the parent chain
                // connected through tool calls.
                let role = entry_type.and_then(MessageRole::from_provider_str)?;

                // Slash-command scaffolding (`<command-name>`, `<command-message>`
                // control blocks) is logged as an ordinary user turn with
                // `isMeta: true`. The live turn-checkpoint path already skips
                // these (see `claude_session::is_user_prompt`); historical
                // import must too, or the transcript reads as raw control text.
                if entry.get("isMeta").and_then(|v| v.as_bool()) == Some(true) {
                    return None;
                }

                let parent_uuid = entry
                    .get("parentUuid")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let content = entry
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .map(content_block_to_text)
                    .unwrap_or_default();
                let created_at = entry
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.timestamp() as u64)
                    .unwrap_or_else(unix_time);

                Some(HistoricalMessage {
                    uuid,
                    parent_uuid,
                    role,
                    content,
                    display_content: None,
                    created_at,
                })
            })
            .collect()
    }
}

/// Session-level created/updated timestamps derived from the active chain
/// already extracted for the session, since [`claude_session::active_chain`]
/// walks entries `historical.rs` no longer keeps around separately.
fn extract_timestamps(messages: &[HistoricalMessage]) -> (u64, u64) {
    let created_at = messages
        .first()
        .map(|message| message.created_at)
        .unwrap_or_else(unix_time);
    let updated_at = messages
        .last()
        .map(|message| message.created_at)
        .unwrap_or(created_at);
    (created_at, updated_at)
}

/// Flatten one Claude message's `content` (a plain string, or an array of
/// typed content blocks) into readable text for Waku's plain-text transcript.
///
/// This mirrors the block types the live streaming driver switches on in
/// `driver::claude::handle_message` (text, thinking, tool_use, tool_result) —
/// unlike a naive `text`/`content` key guess, which silently drops
/// `tool_use` (no `text` key) and `tool_result` blocks whose `content` is
/// itself an array of blocks rather than a plain string.
fn content_block_to_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter_map(content_block_to_text_opt)
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn content_block_to_text_opt(block: &serde_json::Value) -> Option<String> {
    match block.get("type").and_then(|v| v.as_str()) {
        Some("text") => block
            .get("text")
            .and_then(|v| v.as_str())
            .filter(|text| !text.is_empty())
            .map(str::to_owned),
        Some("thinking") => block
            .get("thinking")
            .and_then(|v| v.as_str())
            .filter(|text| !text.is_empty())
            .map(|text| format!("[thinking] {text}")),
        Some("tool_use") => {
            let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
            let input = block
                .get("input")
                .map(|input| input.to_string())
                .unwrap_or_default();
            Some(format!("[tool call] {name}({input})"))
        }
        Some("tool_result") => {
            let failed = block.get("is_error").and_then(|v| v.as_bool()) == Some(true);
            let text = block
                .get("content")
                .map(content_block_to_text)
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| "(no output)".to_owned());
            Some(if failed {
                format!("[tool result, failed] {text}")
            } else {
                format!("[tool result] {text}")
            })
        }
        // Untyped/plain string content (rare on-disk) and unknown block
        // types are read the way a naive parser would, but only as a
        // fallback rather than the default for every block.
        None => block
            .get("text")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        _ => None,
    }
}

impl Default for ClaudeHistoricalDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoricalSessionDiscovery for ClaudeHistoricalDiscovery {
    fn provider(&self) -> ProviderKind {
        ProviderKind::Claude
    }

    fn discover(&self) -> impl FnOnce() -> Result<Vec<HistoricalSession>> + Send + 'static {
        let discovery = ClaudeHistoricalDiscovery {
            projects_dir: self.projects_dir.clone(),
        };
        move || discovery.discover_claude_sessions()
    }
}

fn unix_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_session_file_returns_none_for_invalid_file() {
        let discovery = ClaudeHistoricalDiscovery::new();

        // Create a temp file with invalid JSONL
        let temp_dir =
            std::env::temp_dir().join(format!("waku-historical-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let session_file = temp_dir.join("not-a-uuid.jsonl");
        std::fs::write(&session_file, "invalid json\n").unwrap();

        let result = discovery.parse_session_file(&session_file);
        assert!(result.is_none());

        std::fs::remove_dir_all(temp_dir).ok();
    }

    #[test]
    fn message_role_from_provider_str() {
        assert_eq!(
            MessageRole::from_provider_str("user"),
            Some(MessageRole::User)
        );
        assert_eq!(
            MessageRole::from_provider_str("assistant"),
            Some(MessageRole::Assistant)
        );
        assert_eq!(
            MessageRole::from_provider_str("system"),
            Some(MessageRole::System)
        );
        assert_eq!(MessageRole::from_provider_str("unknown"), None);
    }

    #[test]
    fn content_block_to_text_reads_every_block_type() {
        let text = serde_json::json!({"type": "text", "text": "hello"});
        assert_eq!(content_block_to_text_opt(&text).as_deref(), Some("hello"));

        let thinking = serde_json::json!({"type": "thinking", "thinking": "pondering"});
        assert_eq!(
            content_block_to_text_opt(&thinking).as_deref(),
            Some("[thinking] pondering")
        );

        let tool_use = serde_json::json!({
            "type": "tool_use",
            "name": "Read",
            "input": {"file_path": "/tmp/a.txt"}
        });
        let tool_use_text = content_block_to_text_opt(&tool_use).unwrap();
        assert!(tool_use_text.starts_with("[tool call] Read("));
        assert!(tool_use_text.contains("/tmp/a.txt"));

        // A tool_result whose content is itself an array of blocks (the
        // common on-disk shape) must not be dropped by a naive
        // `.as_str()` on `content`.
        let tool_result = serde_json::json!({
            "type": "tool_result",
            "tool_use_id": "abc",
            "content": [{"type": "text", "text": "file contents"}]
        });
        assert_eq!(
            content_block_to_text_opt(&tool_result).as_deref(),
            Some("[tool result] file contents")
        );

        let failed_result = serde_json::json!({
            "type": "tool_result",
            "is_error": true,
            "content": "boom"
        });
        assert_eq!(
            content_block_to_text_opt(&failed_result).as_deref(),
            Some("[tool result, failed] boom")
        );
    }

    #[test]
    fn parse_active_message_chain_reconstructs_tool_use_and_thinking() {
        let session_id = Uuid::new_v4().to_string();
        let user_uuid = Uuid::new_v4().to_string();
        let assistant_uuid = Uuid::new_v4().to_string();
        let entries = vec![
            serde_json::json!({
                "type": "user",
                "uuid": user_uuid,
                "parentUuid": null,
                "sessionId": session_id,
                "timestamp": "2026-01-01T00:00:00.000Z",
                "message": {"role": "user", "content": "read the file"}
            }),
            serde_json::json!({
                "type": "assistant",
                "uuid": assistant_uuid,
                "parentUuid": user_uuid,
                "sessionId": session_id,
                "timestamp": "2026-01-01T00:00:01.000Z",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "let me check"},
                        {"type": "tool_use", "name": "Read", "input": {"file_path": "/tmp/a.txt"}},
                        {"type": "text", "text": "done reading"}
                    ]
                }
            }),
        ];

        let discovery = ClaudeHistoricalDiscovery::new();
        let messages = discovery.parse_active_message_chain(&entries);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, MessageRole::User);
        assert_eq!(messages[0].content, "read the file");
        assert_eq!(messages[1].role, MessageRole::Assistant);
        assert!(messages[1].content.contains("[thinking] let me check"));
        assert!(messages[1].content.contains("[tool call] Read("));
        assert!(messages[1].content.contains("done reading"));
    }

    #[test]
    fn parse_active_message_chain_skips_meta_control_entries() {
        let session_id = Uuid::new_v4().to_string();
        let meta_uuid = Uuid::new_v4().to_string();
        let real_uuid = Uuid::new_v4().to_string();
        let entries = vec![
            serde_json::json!({
                "type": "user",
                "uuid": meta_uuid,
                "parentUuid": null,
                "sessionId": session_id,
                "isMeta": true,
                "timestamp": "2026-01-01T00:00:00.000Z",
                "message": {
                    "role": "user",
                    "content": "<command-name>/clear</command-name>\n<command-message>clear</command-message>\n<command-args></command-args>"
                }
            }),
            serde_json::json!({
                "type": "user",
                "uuid": real_uuid,
                "parentUuid": meta_uuid,
                "sessionId": session_id,
                "timestamp": "2026-01-01T00:00:01.000Z",
                "message": {"role": "user", "content": "what does this function do?"}
            }),
        ];

        let discovery = ClaudeHistoricalDiscovery::new();
        let messages = discovery.parse_active_message_chain(&entries);

        assert_eq!(
            messages.len(),
            1,
            "the isMeta entry must not become a message"
        );
        assert_eq!(messages[0].uuid, real_uuid);
        assert_eq!(messages[0].content, "what does this function do?");
    }

    #[test]
    fn discover_claude_sessions_finds_fixture_sessions() {
        let temp_dir =
            std::env::temp_dir().join(format!("waku-historical-fixture-{}", Uuid::new_v4()));
        let project_dir = temp_dir.join("-tmp-fixture-project");
        std::fs::create_dir_all(&project_dir).unwrap();

        let session_id = Uuid::new_v4().to_string();
        let user_uuid = Uuid::new_v4().to_string();
        let assistant_uuid = Uuid::new_v4().to_string();
        let entries = vec![
            serde_json::json!({
                "type": "user",
                "uuid": user_uuid,
                "parentUuid": null,
                "sessionId": session_id,
                "cwd": "/tmp/fixture-project",
                "timestamp": "2026-01-01T00:00:00.000Z",
                "message": {"role": "user", "content": "hello"}
            }),
            serde_json::json!({
                "type": "assistant",
                "uuid": assistant_uuid,
                "parentUuid": user_uuid,
                "sessionId": session_id,
                "cwd": "/tmp/fixture-project",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "hi there"}]}
            }),
        ];
        let session_file = project_dir.join(format!("{session_id}.jsonl"));
        let mut file = std::fs::File::create(&session_file).unwrap();
        for entry in &entries {
            use std::io::Write;
            serde_json::to_writer(&mut file, entry).unwrap();
            file.write_all(b"\n").unwrap();
        }

        let discovery = ClaudeHistoricalDiscovery::with_projects_dir(temp_dir.clone());
        let sessions = discovery.discover_claude_sessions().unwrap();

        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
        assert_eq!(session.native_session_id, session_id);
        assert_eq!(session.cwd, Some(PathBuf::from("/tmp/fixture-project")));
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[1].content, "hi there");

        std::fs::remove_dir_all(temp_dir).ok();
    }
}
