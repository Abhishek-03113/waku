//! Provider-neutral historical session discovery and import.
//!
//! This module defines the boundary between provider-specific transcript
//! parsing and Waku's generic session model. A provider adapter (for example
//! [`claude::ClaudeHistoricalProvider`]) reads its own native on-disk format
//! and produces a [`HistoricalTranscript`] — a shape that describes
//! conversation concepts (turns, messages, tool activity, subagent work),
//! never a provider's own record types or field names. Everything below this
//! module (the [`normalize`] step, [`crate::persistence`], and the UI) reads
//! only [`HistoricalTranscript`] and the existing Waku session model; it must
//! never learn a provider's schema.
//!
//! Adding a new provider means implementing [`HistoricalSessionProvider`] and
//! registering it in [`discover_all_providers`] — nothing else in this crate
//! should need to change.

pub mod claude;
mod normalize;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use uuid::Uuid;

use crate::model::ProviderKind;

pub use normalize::normalize_transcript;

/// A complete, provider-neutral historical conversation discovered from a
/// provider's native storage.
#[derive(Debug, Clone)]
pub struct HistoricalTranscript {
    /// The provider that owns this session.
    pub provider: ProviderKind,
    /// Provider's native session identifier (e.g., Claude's session UUID).
    pub native_session_id: String,
    /// Working directory extracted from the session, if available.
    pub cwd: Option<PathBuf>,
    /// Title priority, already resolved by the provider adapter down to a
    /// single best candidate: explicit session name, provider-generated
    /// title, or `None` (the normalizer falls back to the first user prompt,
    /// then a generic placeholder).
    pub title: Option<String>,
    /// Provider model identifier active for the session, if known.
    pub model: Option<String>,
    /// Git branch active in the session's working directory, if known.
    pub git_branch: Option<String>,
    /// When the session was created (Unix timestamp in seconds).
    pub created_at: u64,
    /// When the session was last updated (Unix timestamp in seconds).
    pub updated_at: u64,
    /// The reconstructed main conversation, in chronological order.
    pub turns: Vec<HistoricalTurn>,
}

/// One logical exchange: a user prompt plus everything Claude (or any other
/// provider) did in response, up to but not including the next user prompt.
#[derive(Debug, Clone)]
pub struct HistoricalTurn {
    /// Stable identifier for this turn, derived from the provider's
    /// user-message identifier so re-import is idempotent and deterministic.
    pub id: String,
    pub started_at: u64,
    pub completed_at: Option<u64>,
    /// Ordered conversation content and internal work, interleaved exactly as
    /// it happened. A message never carries tool or reasoning content; those
    /// are [`HistoricalTurnItem::Activity`] instead.
    pub items: Vec<HistoricalTurnItem>,
}

/// One ordered item within a turn: either user-visible conversation text or
/// internal agent work.
#[derive(Debug, Clone)]
pub enum HistoricalTurnItem {
    Message(HistoricalMessage),
    Activity(HistoricalActivity),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoricalRole {
    User,
    Assistant,
    System,
}

/// User-visible conversation text. Only actual prompts and replies become
/// this — never tool calls, tool results, thinking, or provider metadata.
#[derive(Debug, Clone)]
pub struct HistoricalMessage {
    pub role: HistoricalRole,
    pub content: String,
    pub created_at: u64,
}

/// What kind of internal work an activity represents. This is a small,
/// provider-neutral vocabulary; the normalizer maps it onto Waku's existing
/// `ActivityKind` classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoricalActivityKind {
    /// Model reasoning shown separately from the reply.
    Reasoning,
    /// A tool/function call and its result.
    Tool,
    /// Work delegated to a subagent.
    Subagent,
}

/// Internal agent work: a tool call, a reasoning span, or delegated subagent
/// work. Never becomes a `Message` — the normalizer turns this into an
/// `ActivityItem` attached to its turn.
#[derive(Debug, Clone)]
pub struct HistoricalActivity {
    pub kind: HistoricalActivityKind,
    /// Provider-native identifier for this activity (e.g. a tool_use id),
    /// preserved so a result arriving later can be correlated even across
    /// unrelated events in between.
    pub source_id: Option<String>,
    pub title: String,
    /// Structured input/arguments the tool was called with, if any.
    pub arguments: Option<String>,
    /// Reasoning text, when `kind` is `Reasoning`.
    pub content: Option<String>,
    /// Tool/subagent output, once available.
    pub output: Option<String>,
    pub failed: bool,
    /// Whether this activity ever received a completion (a matching
    /// tool_result, a subagent settling). An activity with no result by the
    /// end of the transcript stays incomplete rather than being invented one.
    pub complete: bool,
    pub started_at: u64,
    pub completed_at: Option<u64>,
    /// Present only when `kind` is `Subagent`.
    pub subagent: Option<HistoricalSubagent>,
}

/// Work delegated to a subagent, correlated with its spawning activity by the
/// provider adapter using that provider's own identifiers. The generic model
/// never learns where `id` came from (Claude's `agentId`, or any other
/// provider's equivalent).
#[derive(Debug, Clone)]
pub struct HistoricalSubagent {
    pub id: String,
    pub name: Option<String>,
    pub input: Option<String>,
    pub output: Option<String>,
    pub status: HistoricalSubagentStatus,
    /// Optional detailed transcript of the subagent's own work, for
    /// expandable inspection. Not shown by default.
    pub transcript: Vec<HistoricalTurn>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoricalSubagentStatus {
    Completed,
    Failed,
    /// The provider's transcript ended before this subagent reported back.
    Unknown,
}

/// A provider-specific adapter that can discover historical sessions from its
/// own native storage and parse them into provider-neutral transcripts.
///
/// All schema knowledge — record types, field names, on-disk layout,
/// relationship reconstruction — must stay inside the implementation of this
/// trait. Nothing downstream of [`HistoricalTranscript`] may branch on which
/// provider produced it.
pub trait HistoricalSessionProvider: Send + Sync {
    fn provider(&self) -> ProviderKind;

    /// Discover and parse all historical sessions from this provider's
    /// storage. Returns a closure so discovery (disk I/O) can run off the
    /// calling thread; individual session parse failures are skipped
    /// gracefully rather than aborting the whole discovery.
    fn discover(&self) -> Box<dyn FnOnce() -> Result<Vec<HistoricalTranscript>> + Send>;
}

/// Result of discovering sessions across every registered provider.
#[derive(Debug, Default)]
pub struct DiscoveryResult {
    /// Discovered sessions, keyed by (provider, native_session_id).
    pub sessions: HashMap<(ProviderKind, String), HistoricalTranscript>,
    /// Errors encountered during discovery, keyed by provider.
    pub errors: HashMap<ProviderKind, String>,
}

/// The registry of providers historical discovery runs against. Registering
/// a new provider here is the only change a future provider needs in this
/// module — no other generic code should ever need to change.
fn registered_providers() -> Vec<Box<dyn HistoricalSessionProvider>> {
    vec![Box::new(claude::ClaudeHistoricalProvider::new())]
}

/// Discover historical sessions from every registered provider.
///
/// This is the main entry point for background discovery. It runs discovery
/// for each provider and collects results, handling errors gracefully. This
/// function contains no provider-specific logic: it only iterates the
/// registry and merges results.
pub fn discover_all_providers() -> impl FnOnce() -> Result<DiscoveryResult> + Send + 'static {
    || {
        let mut result = DiscoveryResult::default();
        for provider in registered_providers() {
            let kind = provider.provider();
            match (provider.discover())() {
                Ok(sessions) => {
                    for session in sessions {
                        let key = (session.provider, session.native_session_id.clone());
                        result.sessions.insert(key, session);
                    }
                }
                Err(error) => {
                    result.errors.insert(kind, error.to_string());
                }
            }
        }
        Ok(result)
    }
}

pub(crate) fn unix_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Deterministic id derived from a seed string, used so re-importing (or
/// re-normalizing) the same provider transcript always produces the same
/// session/turn/message ids instead of new random ones each time. Built from
/// a stable hash rather than UUIDv5 to avoid pulling in the `uuid` crate's
/// `v5` feature for one call site.
pub(crate) fn stable_uuid(seed: &str) -> Uuid {
    use std::hash::{Hash, Hasher};
    let mut first = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut first);
    let high = first.finish();
    let mut second = std::collections::hash_map::DefaultHasher::new();
    (seed, "waku-historical-uuid-salt").hash(&mut second);
    let low = second.finish();
    let bytes = [
        (high >> 56) as u8,
        (high >> 48) as u8,
        (high >> 40) as u8,
        (high >> 32) as u8,
        (high >> 24) as u8,
        (high >> 16) as u8,
        (high >> 8) as u8,
        high as u8,
        (low >> 56) as u8,
        (low >> 48) as u8,
        (low >> 40) as u8,
        (low >> 32) as u8,
        (low >> 24) as u8,
        (low >> 16) as u8,
        (low >> 8) as u8,
        low as u8,
    ];
    Uuid::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    //! Proves the generic historical pipeline — discovery trait,
    //! `HistoricalTranscript`, and `normalize_transcript` — needs zero
    //! knowledge of Claude's on-disk schema. `FakeHistoricalProvider` never
    //! touches JSONL, `parentUuid`, `isSidechain`, or any other Claude
    //! concept; if this test required any of that, the generic/provider
    //! boundary in this module would be wrong.
    use super::*;

    struct FakeHistoricalProvider;

    impl HistoricalSessionProvider for FakeHistoricalProvider {
        fn provider(&self) -> ProviderKind {
            // Stands in for a hypothetical non-Claude provider; the point of
            // this test is that nothing downstream branches on which one.
            ProviderKind::Pi
        }

        fn discover(&self) -> Box<dyn FnOnce() -> Result<Vec<HistoricalTranscript>> + Send> {
            Box::new(|| {
                Ok(vec![HistoricalTranscript {
                    provider: ProviderKind::Pi,
                    native_session_id: "fake-session-1".to_owned(),
                    cwd: None,
                    title: Some("A fake provider's own title".to_owned()),
                    model: Some("fake-model-1".to_owned()),
                    git_branch: None,
                    created_at: 10,
                    updated_at: 20,
                    turns: vec![HistoricalTurn {
                        id: "fake-turn-1".to_owned(),
                        started_at: 10,
                        completed_at: Some(20),
                        items: vec![
                            HistoricalTurnItem::Message(HistoricalMessage {
                                role: HistoricalRole::User,
                                content: "does this work without Claude?".to_owned(),
                                created_at: 10,
                            }),
                            HistoricalTurnItem::Activity(HistoricalActivity {
                                kind: HistoricalActivityKind::Tool,
                                source_id: Some("fake-tool-1".to_owned()),
                                title: "search".to_owned(),
                                arguments: None,
                                content: None,
                                output: Some("found it".to_owned()),
                                failed: false,
                                complete: true,
                                started_at: 12,
                                completed_at: Some(15),
                                subagent: None,
                            }),
                            HistoricalTurnItem::Message(HistoricalMessage {
                                role: HistoricalRole::Assistant,
                                content: "yes, entirely provider-neutral".to_owned(),
                                created_at: 20,
                            }),
                        ],
                    }],
                }])
            })
        }
    }

    #[test]
    fn fake_provider_produces_a_valid_transcript_through_the_generic_trait() {
        let provider = FakeHistoricalProvider;
        let sessions = (provider.discover())().expect("fake discovery succeeds");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].provider, ProviderKind::Pi);
    }

    #[test]
    fn fake_provider_transcript_normalizes_into_a_clean_agent_session() {
        let provider = FakeHistoricalProvider;
        let transcript = (provider.discover())().unwrap().remove(0);

        let session = normalize_transcript(&transcript, Uuid::new_v4());

        assert!(session.is_imported);
        assert_eq!(session.native_session_id.as_deref(), Some("fake-session-1"));
        assert_eq!(session.title, "A fake provider's own title");
        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.messages.len(), 2);
        assert_eq!(
            session.messages[0].content,
            "does this work without Claude?"
        );
        assert_eq!(
            session.messages[1].content,
            "yes, entirely provider-neutral"
        );
        // The tool activity landed as an ActivityItem between the two
        // messages, never as a third message.
        assert_eq!(session.transcript_blocks.len(), 1);
        assert_eq!(session.transcript_blocks[0].after_message, 1);
        assert_eq!(session.transcript_blocks[0].activities.len(), 1);
        assert_eq!(
            session.transcript_blocks[0].activities[0].output.as_deref(),
            Some("found it")
        );
    }
}
