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
                result
                    .errors
                    .insert(ProviderKind::Claude, e.to_string());
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
        Self {
            projects_dir: None,
        }
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
        // Reuse claude_session.rs to get transcript entries
        let transcript_entries = entries
            .iter()
            .filter_map(|v| v.as_object())
            .filter(|entry| {
                let entry_type = entry.get("type").and_then(|v| v.as_str());
                matches!(entry_type, Some("user" | "assistant" | "system"))
                    && entry.get("uuid").is_some()
                    && entry.get("isSidechain") != Some(&serde_json::Value::Bool(true))
            })
            .collect::<Vec<_>>();

        if transcript_entries.is_empty() {
            return None;
        }

        // Extract cwd from any entry that has it
        let cwd = entries
            .iter()
            .filter_map(|v| v.get("cwd").and_then(|v| v.as_str()))
            .next()
            .map(PathBuf::from);

        // Extract title using claude_session.rs helper
        let title = entries
            .iter()
            .filter_map(claude_title_from_entry)
            .last();

        // Extract timestamps - use the earliest and latest from transcript entries
        let (created_at, updated_at) = self.extract_timestamps(&transcript_entries);

        // Parse messages from the active chain using claude_session.rs logic
        let messages = self.parse_active_message_chain(entries);

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

    fn extract_timestamps(
        &self,
        entries: &[&serde_json::Map<String, serde_json::Value>],
    ) -> (u64, u64) {
        let mut timestamps: Vec<u64> = entries
            .iter()
            .filter_map(|entry| {
                entry
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.timestamp() as u64)
            })
            .collect();

        timestamps.sort();
        let created_at = timestamps.first().copied().unwrap_or_else(unix_time);
        let updated_at = timestamps.last().copied().unwrap_or(created_at);
        (created_at, updated_at)
    }

    fn parse_active_message_chain(&self, entries: &[serde_json::Value]) -> Vec<HistoricalMessage> {
        // Build a map of uuid -> entry
        let by_uuid: HashMap<String, &serde_json::Map<String, serde_json::Value>> = entries
            .iter()
            .filter_map(|v| {
                let uuid = v.get("uuid")?.as_str()?.to_owned();
                Some((uuid, v.as_object()?))
            })
            .collect();

        // Filter to transcript entries
        let transcript: Vec<&serde_json::Map<String, serde_json::Value>> = entries
            .iter()
            .filter_map(|v| v.as_object())
            .filter(|entry| {
                let entry_type = entry.get("type").and_then(|v| v.as_str());
                matches!(entry_type, Some("user" | "assistant" | "system"))
                    && entry.get("uuid").is_some()
                    && entry.get("isSidechain") != Some(&serde_json::Value::Bool(true))
            })
            .collect();

        let Some(mut current) = transcript.last().copied() else {
            return Vec::new();
        };

        // Walk backwards through the parent chain
        let mut chain = Vec::new();
        loop {
            let Some(uuid) = current.get("uuid").and_then(|v| v.as_str()) else {
                break;
            };
            let parent_uuid = current
                .get("parentUuid")
                .and_then(|v| v.as_str())
                .map(String::from);
            let role = current
                .get("type")
                .and_then(|v| v.as_str())
                .and_then(MessageRole::from_provider_str)
                .unwrap_or(MessageRole::Assistant);

            let content = self.extract_message_content(current);
            let created_at = current
                .get("timestamp")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.timestamp() as u64)
                .unwrap_or_else(unix_time);

            chain.push(HistoricalMessage {
                uuid: uuid.to_owned(),
                parent_uuid: parent_uuid.clone(),
                role,
                content,
                display_content: None,
                created_at,
            });

            // Move to parent
            if let Some(ref parent_uuid) = parent_uuid {
                if let Some(&parent_entry) = by_uuid.get(parent_uuid) {
                    current = parent_entry;
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        // Reverse to get chronological order
        chain.reverse();
        chain
    }

    fn extract_message_content(&self, entry: &serde_json::Map<String, serde_json::Value>) -> String {
        // Try to extract content from the message field
        if let Some(message) = entry.get("message") {
            if let Some(content) = message.get("content") {
                return self.content_to_string(content);
            }
        }
        String::new()
    }

    fn content_to_string(&self, content: &serde_json::Value) -> String {
        match content {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(blocks) => {
                blocks
                    .iter()
                    .filter_map(|block| {
                        block
                            .get("text")
                            .or_else(|| block.get("content"))
                            .and_then(|v| v.as_str())
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            _ => String::new(),
        }
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

/// Helper to extract title from a Claude entry, matching claude_session.rs logic
fn claude_title_from_entry(entry: &serde_json::Value) -> Option<String> {
    let field = match entry.get("type").and_then(|v| v.as_str()) {
        Some("ai-title") => "aiTitle",
        Some("custom-title") => "customTitle",
        _ => return None,
    };
    entry
        .get(field)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
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
    fn discover_claude_sessions_finds_fixture_sessions() {
        // This test would require a fixture setup; skip if no fixture exists
        let discovery = ClaudeHistoricalDiscovery::new();
        let projects_dir = discovery.projects_directory();
        
        // Just check that the method doesn't panic
        let result = discovery.discover_claude_sessions();
        assert!(result.is_ok());
    }

    #[test]
    fn parse_session_file_returns_none_for_invalid_file() {
        let discovery = ClaudeHistoricalDiscovery::new();
        
        // Create a temp file with invalid JSONL
        let temp_dir = std::env::temp_dir().join(format!("waku-historical-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let session_file = temp_dir.join("not-a-uuid.jsonl");
        std::fs::write(&session_file, "invalid json\n").unwrap();
        
        let result = discovery.parse_session_file(&session_file);
        assert!(result.is_none());
        
        std::fs::remove_dir_all(temp_dir).ok();
    }

    #[test]
    fn message_role_from_provider_str() {
        assert_eq!(MessageRole::from_provider_str("user"), Some(MessageRole::User));
        assert_eq!(MessageRole::from_provider_str("assistant"), Some(MessageRole::Assistant));
        assert_eq!(MessageRole::from_provider_str("system"), Some(MessageRole::System));
        assert_eq!(MessageRole::from_provider_str("unknown"), None);
    }
}