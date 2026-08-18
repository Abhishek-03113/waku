//! Provider-neutral normalization: turns a [`super::HistoricalTranscript`]
//! into a Waku [`AgentSession`] using Waku's existing turn/message/activity
//! model. This module must never branch on which provider produced the
//! transcript, and must never reference a provider's own record types or
//! field names — see the module doc on [`super`] for the boundary this
//! enforces.

use uuid::Uuid;

use super::{
    HistoricalActivityKind, HistoricalRole, HistoricalSubagentStatus, HistoricalTranscript,
    HistoricalTurn, HistoricalTurnItem, stable_uuid,
};
use crate::model::{
    ActivityItem, ActivityKind, AgentSession, AgentTurn, InteractionMode, Message, MessageRole,
    ReasoningBlock, RuntimeMode, SessionStatus, SessionWorkspace, TranscriptBlock, TurnStatus,
};

/// Normalize a discovered historical transcript into a fully-formed,
/// read-only Waku session. `project_id` is assigned by the caller (the
/// generic importer resolves it from the transcript's `cwd`).
pub fn normalize_transcript(transcript: &HistoricalTranscript, project_id: Uuid) -> AgentSession {
    let session_id = stable_uuid(&format!(
        "{}:{}",
        transcript.provider.id(),
        transcript.native_session_id
    ));

    let mut messages = Vec::new();
    let mut transcript_blocks = Vec::new();
    let mut turns = Vec::new();

    for turn in &transcript.turns {
        let turn_id = stable_uuid(&format!(
            "{}:{}:turn:{}",
            transcript.provider.id(),
            transcript.native_session_id,
            turn.id
        ));
        normalize_turn(turn, turn_id, &mut messages, &mut transcript_blocks);
        turns.push(build_agent_turn(turn, turn_id, turns.len() + 1));
    }

    let title = resolve_title(transcript);

    AgentSession {
        id: session_id,
        title: title
            .clone()
            .unwrap_or_else(|| AgentSession::DEFAULT_TITLE.to_owned()),
        auto_title: title,
        project_id,
        workspace: SessionWorkspace::Local,
        provider: transcript.provider,
        model: transcript.model.clone(),
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: InteractionMode::Build,
        reasoning_effort: None,
        service_tier: None,
        context_window: None,
        agent_preset: None,
        status: SessionStatus::Idle,
        created_at: transcript.created_at,
        updated_at: transcript.updated_at,
        last_reply_at: Some(transcript.updated_at),
        is_imported: true,
        native_session_id: Some(transcript.native_session_id.clone()),
        provider_cursor: None,
        available_commands: Vec::new(),
        context_usage: None,
        runtime_event_cursor: None,
        provider_session_id: None,
        messages,
        transcript_blocks,
        turns,
        queued_messages: Vec::new(),
        detail_loaded: true,
    }
}

/// Title priority: explicit provider session title, then the first
/// meaningful user prompt (trimmed to a reasonable length), then `None` —
/// which lets `AgentSession::display_title` fall back to the generic
/// "New task" placeholder rather than overwriting a real title with a
/// hardcoded "Imported session" string that would outrank it.
fn resolve_title(transcript: &HistoricalTranscript) -> Option<String> {
    if let Some(title) = transcript
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
    {
        return Some(title.to_owned());
    }
    transcript
        .turns
        .iter()
        .flat_map(|turn| &turn.items)
        .find_map(|item| match item {
            HistoricalTurnItem::Message(message) if message.role == HistoricalRole::User => {
                let text = message.content.trim();
                (!text.is_empty()).then(|| first_line(text))
            }
            _ => None,
        })
}

fn first_line(text: &str) -> String {
    const MAX_CHARS: usize = 120;
    let line = text.lines().next().unwrap_or(text).trim();
    let mut short: String = line.chars().take(MAX_CHARS).collect();
    if short.chars().count() < line.chars().count() {
        short.push('…');
    }
    short
}

fn build_agent_turn(turn: &HistoricalTurn, turn_id: Uuid, turn_count: usize) -> AgentTurn {
    AgentTurn {
        id: turn_id,
        turn_count,
        status: TurnStatus::Completed,
        provider_turn_started: true,
        provider_resume_at: None,
        started_at: turn.started_at,
        completed_at: Some(turn.completed_at.unwrap_or(turn.started_at)),
        checkpoint: None,
    }
}

fn normalize_turn(
    turn: &HistoricalTurn,
    turn_id: Uuid,
    messages: &mut Vec<Message>,
    transcript_blocks: &mut Vec<TranscriptBlock>,
) {
    let mut pending_activities: Vec<ActivityItem> = Vec::new();

    let flush = |pending: &mut Vec<ActivityItem>,
                 messages: &[Message],
                 transcript_blocks: &mut Vec<TranscriptBlock>| {
        if pending.is_empty() {
            return;
        }
        transcript_blocks.push(TranscriptBlock {
            after_message: messages.len(),
            turn_id: Some(turn_id),
            activities: std::mem::take(pending),
        });
    };

    for item in &turn.items {
        match item {
            HistoricalTurnItem::Message(message) => {
                flush(&mut pending_activities, messages, transcript_blocks);
                messages.push(Message {
                    id: stable_uuid(&format!(
                        "{turn_id}:message:{}:{}",
                        messages.len(),
                        message.created_at
                    )),
                    turn_id: Some(turn_id),
                    role: match message.role {
                        HistoricalRole::User => MessageRole::User,
                        HistoricalRole::Assistant => MessageRole::Assistant,
                        HistoricalRole::System => MessageRole::System,
                    },
                    content: message.content.clone(),
                    display_content: None,
                    attachments: Vec::new(),
                    created_at: message.created_at,
                    streaming: false,
                });
            }
            HistoricalTurnItem::Activity(activity) => {
                pending_activities.push(normalize_activity(activity));
            }
        }
    }
    flush(&mut pending_activities, messages, transcript_blocks);
}

fn normalize_activity(activity: &super::HistoricalActivity) -> ActivityItem {
    match activity.kind {
        HistoricalActivityKind::Reasoning => ActivityItem::from_reasoning(
            ReasoningBlock {
                content: activity.content.clone().unwrap_or_default(),
                started_at_ms: activity.started_at * 1000,
                finished_at_ms: activity.completed_at.unwrap_or(activity.started_at) * 1000,
            },
            activity.complete,
        ),
        HistoricalActivityKind::Tool => {
            let kind = ActivityKind::from_tool_name(&activity.title);
            let mut item = ActivityItem::new(
                activity.source_id.clone(),
                kind,
                activity.title.clone(),
                None,
                activity.complete,
            );
            item.arguments = activity.arguments.clone();
            item.output = activity.output.clone();
            item.failed = activity.failed;
            item
        }
        HistoricalActivityKind::Subagent => {
            // `ActivityKind::Tool` is the closest existing generic
            // classification for delegated work — Waku's activity vocabulary
            // has no dedicated "subagent" kind, and none is needed: the title
            // prefix plus expandable output already reads as delegated work
            // (see `activity_tool_display_name`/`activity_disclosure_sections`
            // in `src/app/components.rs`, which render any `Tool` activity's
            // `output` as an expandable section with no Claude-specific code).
            let title = format!("Agent: {}", activity.title);
            let mut item = ActivityItem::new(
                activity.source_id.clone(),
                ActivityKind::Tool,
                title,
                None,
                activity.complete,
            );
            item.arguments = activity.arguments.clone();
            if let Some(subagent) = &activity.subagent {
                item.output = subagent.output.clone();
                item.failed = subagent.status == HistoricalSubagentStatus::Failed;
            }
            item
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::historical::{HistoricalActivity, HistoricalMessage};
    use crate::model::ProviderKind;

    fn sample_transcript() -> HistoricalTranscript {
        HistoricalTranscript {
            provider: ProviderKind::Claude,
            native_session_id: "abc-123".to_owned(),
            cwd: None,
            title: None,
            model: Some("claude-sonnet-5".to_owned()),
            git_branch: None,
            created_at: 1000,
            updated_at: 2000,
            turns: vec![HistoricalTurn {
                id: "turn-1".to_owned(),
                started_at: 1000,
                completed_at: Some(1500),
                items: vec![
                    HistoricalTurnItem::Message(HistoricalMessage {
                        role: HistoricalRole::User,
                        content: "please read config.toml".to_owned(),
                        created_at: 1000,
                    }),
                    HistoricalTurnItem::Activity(HistoricalActivity {
                        kind: HistoricalActivityKind::Tool,
                        source_id: Some("toolu_1".to_owned()),
                        title: "Read".to_owned(),
                        arguments: Some("{\"file_path\":\"config.toml\"}".to_owned()),
                        content: None,
                        output: Some("[dependencies]".to_owned()),
                        failed: false,
                        complete: true,
                        started_at: 1100,
                        completed_at: Some(1200),
                        subagent: None,
                    }),
                    HistoricalTurnItem::Message(HistoricalMessage {
                        role: HistoricalRole::Assistant,
                        content: "The file has one dependency section.".to_owned(),
                        created_at: 1500,
                    }),
                ],
            }],
        }
    }

    #[test]
    fn normalizes_turn_into_messages_and_one_transcript_block() {
        let transcript = sample_transcript();
        let session = normalize_transcript(&transcript, Uuid::new_v4());

        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].role, MessageRole::User);
        assert_eq!(session.messages[1].role, MessageRole::Assistant);
        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].status, TurnStatus::Completed);

        // The activity sits between the two messages: after_message == 1.
        assert_eq!(session.transcript_blocks.len(), 1);
        let block = &session.transcript_blocks[0];
        assert_eq!(block.after_message, 1);
        assert_eq!(block.turn_id, Some(session.turns[0].id));
        assert_eq!(block.activities.len(), 1);
        assert_eq!(block.activities[0].kind, ActivityKind::FileRead);
        assert_eq!(
            block.activities[0].output.as_deref(),
            Some("[dependencies]")
        );

        assert!(session.is_imported);
        assert_eq!(session.native_session_id.as_deref(), Some("abc-123"));
        assert_eq!(session.title, "please read config.toml");
    }

    #[test]
    fn explicit_title_wins_over_first_prompt() {
        let mut transcript = sample_transcript();
        transcript.title = Some("Config investigation".to_owned());
        let session = normalize_transcript(&transcript, Uuid::new_v4());
        assert_eq!(session.title, "Config investigation");
    }

    #[test]
    fn repeated_normalization_is_deterministic() {
        let transcript = sample_transcript();
        let project_id = Uuid::new_v4();
        let first = normalize_transcript(&transcript, project_id);
        let second = normalize_transcript(&transcript, project_id);
        assert_eq!(first.id, second.id);
        assert_eq!(first.turns[0].id, second.turns[0].id);
        assert_eq!(first.messages[0].id, second.messages[0].id);
    }

    #[test]
    fn subagent_activity_carries_output_but_no_extra_message() {
        let mut transcript = sample_transcript();
        transcript.turns[0]
            .items
            .push(HistoricalTurnItem::Activity(HistoricalActivity {
                kind: HistoricalActivityKind::Subagent,
                source_id: Some("toolu_agent".to_owned()),
                title: "Explore billing implementation".to_owned(),
                arguments: None,
                content: None,
                output: Some("Found the billing module at src/billing.rs".to_owned()),
                failed: false,
                complete: true,
                started_at: 1300,
                completed_at: Some(1450),
                subagent: Some(super::super::HistoricalSubagent {
                    id: "agent-1".to_owned(),
                    name: Some("Explore".to_owned()),
                    input: Some("find the billing implementation".to_owned()),
                    output: Some("Found the billing module at src/billing.rs".to_owned()),
                    status: HistoricalSubagentStatus::Completed,
                    transcript: Vec::new(),
                }),
            }));
        let session = normalize_transcript(&transcript, Uuid::new_v4());
        assert_eq!(
            session.messages.len(),
            2,
            "subagent work must not add a message"
        );
        let subagent_activity = session
            .transcript_blocks
            .iter()
            .flat_map(|block| &block.activities)
            .find(|activity| activity.source_id.as_deref() == Some("toolu_agent"))
            .expect("subagent activity present");
        assert_eq!(
            subagent_activity.output.as_deref(),
            Some("Found the billing module at src/billing.rs")
        );
        assert_eq!(
            subagent_activity.title,
            "Agent: Explore billing implementation"
        );
    }
}
