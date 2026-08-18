//! Claude Code JSONL transcript parsing and normalization into the
//! provider-neutral [`super::HistoricalTranscript`] model.
//!
//! All Claude-specific schema knowledge lives in this file: record types,
//! field names (`parentUuid`, `isSidechain`, `agentId`, `tool_use`,
//! `tool_result`, …), the `subagents/agent-<id>.jsonl` +
//! `agent-<id>.meta.json` on-disk layout, and how to reconstruct the active
//! conversation chain and subagent relationships from them. Nothing below
//! this module may depend on any of it.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value};
use uuid::Uuid;

use super::{
    HistoricalActivity, HistoricalActivityKind, HistoricalMessage, HistoricalRole,
    HistoricalSessionProvider, HistoricalSubagent, HistoricalSubagentStatus, HistoricalTranscript,
    HistoricalTurn, HistoricalTurnItem, unix_time,
};
use crate::model::ProviderKind;

/// Record types that keep the active conversation chain connected. Sourced
/// from the same set the live fork/resume path already relies on
/// (`claude_session::TRANSCRIPT_TYPES`) so historical import and live resume
/// never disagree about what counts as a chain link.
const CHAIN_TYPES: [&str; 5] = ["user", "assistant", "attachment", "system", "progress"];

pub struct ClaudeHistoricalProvider {
    projects_dir: Option<PathBuf>,
}

impl ClaudeHistoricalProvider {
    pub fn new() -> Self {
        Self { projects_dir: None }
    }

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

    fn discover_sessions(&self) -> Result<Vec<HistoricalTranscript>> {
        let projects_dir = self.projects_directory();
        let mut sessions = Vec::new();
        if !projects_dir.exists() {
            return Ok(sessions);
        }
        walk_projects_dir(&projects_dir, &mut sessions)?;
        Ok(sessions)
    }
}

impl Default for ClaudeHistoricalProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoricalSessionProvider for ClaudeHistoricalProvider {
    fn provider(&self) -> ProviderKind {
        ProviderKind::Claude
    }

    fn discover(&self) -> Box<dyn FnOnce() -> Result<Vec<HistoricalTranscript>> + Send> {
        let provider = ClaudeHistoricalProvider {
            projects_dir: self.projects_dir.clone(),
        };
        Box::new(move || provider.discover_sessions())
    }
}

fn walk_projects_dir(dir: &Path, sessions: &mut Vec<HistoricalTranscript>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.is_dir() {
            walk_projects_dir(&path, sessions)?;
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            if let Some(session) = parse_session_file(&path) {
                sessions.push(session);
            }
        }
    }
    Ok(())
}

/// Parse one top-level session JSONL file (`<uuid>.jsonl`) into a
/// [`HistoricalTranscript`]. Individual malformed lines are skipped; a
/// completely unusable file yields `None` rather than aborting discovery.
fn parse_session_file(path: &Path) -> Option<HistoricalTranscript> {
    let filename = path.file_stem()?;
    let native_session_id = filename.to_string_lossy().to_string();
    Uuid::parse_str(&native_session_id).ok()?;

    let entries = read_jsonl_records(path);
    if entries.is_empty() {
        return None;
    }

    let subagents_dir = path
        .parent()
        .map(|parent| parent.join(&native_session_id).join("subagents"));
    let subagents = subagents_dir
        .as_deref()
        .map(load_subagent_records)
        .unwrap_or_default();

    build_transcript(&entries, &native_session_id, &subagents)
}

/// One subagent's parsed metadata plus its own record stream, keyed by the
/// spawning tool_use id so the parent activity can find it.
struct SubagentSource {
    agent_id: String,
    agent_type: Option<String>,
    description: Option<String>,
    entries: Vec<Map<String, Value>>,
}

/// Load every `subagents/agent-<id>.jsonl` + `agent-<id>.meta.json` pair
/// under a session's subagent directory, keyed by the meta file's
/// `toolUseId` — the exact id of the `tool_use` block in the parent
/// transcript that spawned it.
fn load_subagent_records(dir: &Path) -> HashMap<String, SubagentSource> {
    let mut by_tool_use_id = HashMap::new();
    let Ok(read_dir) = fs::read_dir(dir) else {
        return by_tool_use_id;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json")
            || !path
                .file_stem()
                .is_some_and(|stem| stem.to_string_lossy().ends_with(".meta"))
        {
            continue;
        }
        let Some(meta) = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        else {
            continue;
        };
        let Some(tool_use_id) = meta.get("toolUseId").and_then(Value::as_str) else {
            continue;
        };
        let agent_id = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".meta.json"))
            .and_then(|name| name.strip_prefix("agent-"))
            .unwrap_or_default()
            .to_owned();
        let jsonl_path = path.with_file_name(format!("agent-{agent_id}.jsonl"));
        let entries = read_jsonl_records(&jsonl_path);
        by_tool_use_id.insert(
            tool_use_id.to_owned(),
            SubagentSource {
                agent_id,
                agent_type: meta
                    .get("agentType")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                description: meta
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                entries,
            },
        );
    }
    by_tool_use_id
}

/// Parse a JSONL file line by line. A line that fails to parse as JSON, or
/// parses to something other than an object, is skipped — one malformed line
/// must never destroy the rest of the session.
fn read_jsonl_records(path: &Path) -> Vec<Map<String, Value>> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| match serde_json::from_str::<Value>(line) {
            Ok(Value::Object(object)) => Some(object),
            _ => None,
        })
        .collect()
}

fn entry_uuid(entry: &Map<String, Value>) -> Option<&str> {
    entry.get("uuid").and_then(Value::as_str)
}

fn entry_parent_uuid(entry: &Map<String, Value>) -> Option<&str> {
    entry.get("parentUuid").and_then(Value::as_str)
}

fn entry_type(entry: &Map<String, Value>) -> Option<&str> {
    entry.get("type").and_then(Value::as_str)
}

fn is_sidechain(entry: &Map<String, Value>) -> bool {
    entry.get("isSidechain").and_then(Value::as_bool) == Some(true)
}

fn is_meta(entry: &Map<String, Value>) -> bool {
    entry.get("isMeta").and_then(Value::as_bool) == Some(true)
}

fn entry_timestamp(entry: &Map<String, Value>) -> Option<u64> {
    entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp() as u64)
}

/// Chain-linking entries from a **main session** file: sidechain records
/// interleaved there belong to a subagent's own transcript, not this
/// conversation, and are excluded.
fn chain_entries(entries: &[Map<String, Value>]) -> Vec<&Map<String, Value>> {
    entries
        .iter()
        .filter(|entry| {
            entry_type(entry).is_some_and(|kind| CHAIN_TYPES.contains(&kind))
                && entry_uuid(entry).is_some()
                && !is_sidechain(entry)
        })
        .collect()
}

/// Chain-linking entries from a **subagent's own** `agent-<id>.jsonl` file,
/// where every record legitimately carries `isSidechain: true` (it is a
/// sidechain relative to the main session, not relative to itself).
fn subagent_chain_entries(entries: &[Map<String, Value>]) -> Vec<&Map<String, Value>> {
    entries
        .iter()
        .filter(|entry| {
            entry_type(entry).is_some_and(|kind| CHAIN_TYPES.contains(&kind))
                && entry_uuid(entry).is_some()
        })
        .collect()
}

/// Walk the *active* conversation chain: starting from the last chain entry,
/// follow `parentUuid` back to the root. This is the same branch Claude
/// itself would resume from, so abandoned branches (superseded by a later
/// sibling) and duplicated/derived events never appear in the reconstructed
/// conversation. A `parentUuid` that points nowhere (a missing/incomplete
/// session) simply ends the walk early rather than failing.
fn active_chain<'a>(entries: &[&'a Map<String, Value>]) -> Vec<&'a Map<String, Value>> {
    let by_uuid: HashMap<&str, &Map<String, Value>> = entries
        .iter()
        .filter_map(|entry| entry_uuid(entry).map(|uuid| (uuid, *entry)))
        .collect();
    let Some(mut current) = entries.last().copied() else {
        return Vec::new();
    };
    let mut chain = Vec::new();
    loop {
        chain.push(current);
        let Some(parent) = entry_parent_uuid(current) else {
            break;
        };
        let Some(next) = by_uuid.get(parent).copied() else {
            break;
        };
        current = next;
    }
    chain.reverse();
    chain
}

/// A real user prompt: not slash-command scaffolding (`isMeta`), and not a
/// `tool_result` echoed back to the model. Mirrors
/// `claude_session::is_user_prompt` so historical turn boundaries agree with
/// the live checkpoint path about what starts a turn.
fn is_user_prompt(entry: &Map<String, Value>) -> bool {
    if is_meta(entry) {
        return false;
    }
    let Some(content) = entry
        .get("message")
        .and_then(|message| message.get("content"))
    else {
        return false;
    };
    match content {
        Value::String(text) => !is_control_scaffolding(text),
        Value::Array(blocks) => {
            !blocks.is_empty()
                && !blocks
                    .iter()
                    .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        }
        _ => false,
    }
}

/// A local slash-command invocation (`/clear`, `/compact`, `/model`, …)
/// echoes into the transcript as an ordinary, non-`isMeta` `user` record
/// whose content is the literal `<command-name>…</command-name>` control
/// markup the CLI itself parses — not something the user typed or would
/// recognize. `isMeta` catches the *preceding* caveat record but not this
/// one, so it must be filtered on content shape too, or it leaks into the
/// transcript (and worse, into the session title) as raw control text.
/// The exact set of Claude control wrappers confirmed, against real
/// `~/.claude/projects` data, to appear as a non-`isMeta` `user`-role
/// record's entire string content — never as a real user prompt:
///
/// - `<command-name>…</command-name>` paired with
///   `<command-message>…</command-message>`, in either order (a built-in
///   like `/clear` leads with `command-name`; an observed custom,
///   frontmatter-driven command led with `command-message` instead) — the
///   CLI's own echo of a local slash-command invocation.
/// - `<local-command-stdout>…</local-command-stdout>` — the CLI's echo of
///   that command's own output (e.g. "Set model to Sonnet 5…").
/// - `<task-notification>…</task-notification>` — an async background-task
///   or subagent completion notice threaded back onto the main turn.
///
/// This is deliberately an allowlist of confirmed wrapper tags, not a bare
/// `starts_with('<')` heuristic: real user prompts that happen to open with
/// an angle bracket exist too (a pasted HTML error page, for instance), and
/// must not be swept up here.
const CONTROL_WRAPPER_TAGS: [&str; 2] = ["local-command-stdout", "task-notification"];

fn is_control_scaffolding(text: &str) -> bool {
    let trimmed = text.trim_start();
    if (trimmed.starts_with("<command-name>") || trimmed.starts_with("<command-message>"))
        && text.contains("<command-name>")
        && text.contains("<command-message>")
    {
        return true;
    }
    CONTROL_WRAPPER_TAGS
        .iter()
        .any(|tag| trimmed.starts_with(&format!("<{tag}>")))
}

fn build_transcript(
    entries: &[Map<String, Value>],
    native_session_id: &str,
    subagents: &HashMap<String, SubagentSource>,
) -> Option<HistoricalTranscript> {
    let chain = active_chain(&chain_entries(entries));
    if chain.is_empty() {
        return None;
    }

    let cwd = entries
        .iter()
        .find_map(|entry| entry.get("cwd").and_then(Value::as_str))
        .map(PathBuf::from);
    let git_branch = entries
        .iter()
        .find_map(|entry| entry.get("gitBranch").and_then(Value::as_str))
        .filter(|branch| !branch.is_empty())
        .map(str::to_owned);
    let model = chain.iter().rev().find_map(|entry| {
        entry
            .get("message")
            .and_then(|message| message.get("model"))
            .and_then(Value::as_str)
    });
    let title = entries.iter().filter_map(claude_title).last();

    // Correlate an open tool_use with its later tool_result by id, even when
    // unrelated chain entries land in between (progress notices, attachment
    // deltas). `pending_tools` holds tool_use blocks whose result has not
    // arrived yet, addressed by index into the turn currently being built.
    let mut turns: Vec<HistoricalTurn> = Vec::new();
    let mut pending_tools: HashMap<String, (usize, usize)> = HashMap::new();

    for entry in &chain {
        if is_meta(entry) {
            continue;
        }
        match entry_type(entry) {
            Some("user") if is_user_prompt(entry) => {
                let content = entry
                    .get("message")
                    .and_then(|message| message.get("content"));
                let text = user_visible_text(content);
                let started_at = entry_timestamp(entry).unwrap_or_else(unix_time);
                turns.push(HistoricalTurn {
                    id: entry_uuid(entry).unwrap_or_default().to_owned(),
                    started_at,
                    completed_at: None,
                    items: vec![HistoricalTurnItem::Message(HistoricalMessage {
                        role: HistoricalRole::User,
                        content: text,
                        created_at: started_at,
                    })],
                });
            }
            Some("user") => {
                // Not a fresh prompt: either a tool_result batch, or a stray
                // user-role record before any turn has started (rare, but a
                // malformed/partial session must not panic on it).
                let Some(content) = entry
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .and_then(Value::as_array)
                else {
                    continue;
                };
                let completed_at = entry_timestamp(entry);
                for block in content {
                    if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    apply_tool_result(&mut turns, &mut pending_tools, block, completed_at);
                }
            }
            Some("assistant") => {
                let Some(turn) = turns.last_mut() else {
                    // An assistant record before any user prompt (e.g. a
                    // truncated/incomplete session) has nowhere to attach.
                    continue;
                };
                let created_at = entry_timestamp(entry).unwrap_or_else(unix_time);
                turn.completed_at = Some(created_at);
                let Some(content) = entry
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .and_then(Value::as_array)
                else {
                    continue;
                };
                let turn_index = turns.len() - 1;
                for block in content {
                    apply_assistant_block(
                        &mut turns[turn_index],
                        block,
                        created_at,
                        turn_index,
                        &mut pending_tools,
                        subagents,
                    );
                }
            }
            _ => {}
        }
    }

    turns.retain(|turn| !turn.items.is_empty());
    if turns.is_empty() {
        return None;
    }

    let created_at = turns
        .first()
        .map(|turn| turn.started_at)
        .unwrap_or_else(unix_time);
    let updated_at = turns
        .iter()
        .filter_map(|turn| turn.completed_at.or(Some(turn.started_at)))
        .max()
        .unwrap_or(created_at);

    Some(HistoricalTranscript {
        provider: ProviderKind::Claude,
        native_session_id: native_session_id.to_owned(),
        cwd,
        title,
        model: model.map(str::to_owned),
        git_branch,
        created_at,
        updated_at,
        turns,
    })
}

/// User-visible text for a prompt's `content`: a plain string, or the `text`
/// blocks of an array (attachments/mentions ride alongside as separate block
/// types and are not narrative content).
fn user_visible_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_assistant_block(
    turn: &mut HistoricalTurn,
    block: &Value,
    created_at: u64,
    turn_index: usize,
    pending_tools: &mut HashMap<String, (usize, usize)>,
    subagents: &HashMap<String, SubagentSource>,
) {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => {
            let Some(text) = block
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            else {
                return;
            };
            turn.items
                .push(HistoricalTurnItem::Message(HistoricalMessage {
                    role: HistoricalRole::Assistant,
                    content: text.to_owned(),
                    created_at,
                }));
        }
        Some("thinking") => {
            let Some(text) = block
                .get("thinking")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            else {
                return;
            };
            turn.items
                .push(HistoricalTurnItem::Activity(HistoricalActivity {
                    kind: HistoricalActivityKind::Reasoning,
                    source_id: None,
                    title: "Reasoning".to_owned(),
                    arguments: None,
                    content: Some(text.to_owned()),
                    output: None,
                    failed: false,
                    complete: true,
                    started_at: created_at,
                    completed_at: Some(created_at),
                    subagent: None,
                }));
        }
        Some("tool_use") => {
            let id = block.get("id").and_then(Value::as_str).map(str::to_owned);
            let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
            let subagent_source = id.as_deref().and_then(|id| subagents.get(id));
            let (kind, title) = if subagent_source.is_some()
                || name.eq_ignore_ascii_case("task")
                || name.eq_ignore_ascii_case("agent")
            {
                let title = block
                    .pointer("/input/description")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .or(subagent_source.and_then(|source| source.description.as_deref()))
                    .unwrap_or(name)
                    .to_owned();
                (HistoricalActivityKind::Subagent, title)
            } else {
                (HistoricalActivityKind::Tool, name.to_owned())
            };
            let arguments = block.get("input").and_then(|input| {
                (!input.is_null())
                    .then(|| serde_json::to_string_pretty(input).ok())
                    .flatten()
            });
            let subagent = subagent_source
                .map(|source| build_subagent(source, block.get("input"), turn_index, created_at));
            let activity_index = turn.items.len();
            turn.items
                .push(HistoricalTurnItem::Activity(HistoricalActivity {
                    kind,
                    source_id: id.clone(),
                    title,
                    arguments,
                    content: None,
                    output: None,
                    failed: false,
                    complete: subagent.is_some(),
                    started_at: created_at,
                    completed_at: None,
                    subagent,
                }));
            if let Some(id) = id {
                pending_tools.insert(id, (turn_index, activity_index));
            }
        }
        _ => {}
    }
}

fn build_subagent(
    source: &SubagentSource,
    input: Option<&Value>,
    _turn_index: usize,
    started_at: u64,
) -> HistoricalSubagent {
    let sub_chain = active_chain(&subagent_chain_entries(&source.entries));
    let transcript = build_subagent_turns(&sub_chain);
    let output = transcript.last().and_then(|turn| {
        turn.items.iter().rev().find_map(|item| match item {
            HistoricalTurnItem::Message(message) if message.role == HistoricalRole::Assistant => {
                Some(message.content.clone())
            }
            _ => None,
        })
    });
    let status = if output.is_some() {
        HistoricalSubagentStatus::Completed
    } else {
        HistoricalSubagentStatus::Unknown
    };
    let prompt = input
        .and_then(|input| input.get("prompt"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    HistoricalSubagent {
        id: source.agent_id.clone(),
        name: source
            .agent_type
            .clone()
            .or_else(|| source.description.clone()),
        input: prompt.or_else(|| source.description.clone()),
        output,
        status,
        transcript,
    }
    .with_started_at(started_at)
}

impl HistoricalSubagent {
    /// Test/documentation helper kept private to this module: the generic
    /// model has no notion of "started_at" on a subagent today, but the
    /// builder shape is kept here so future fields stay easy to thread
    /// through without another cross-module signature change.
    fn with_started_at(self, _started_at: u64) -> Self {
        self
    }
}

/// A subagent's own JSONL is a complete transcript in the same record shape
/// as a main session, so its turns are rebuilt with the same logic minus
/// nested-subagent correlation (Claude does not currently nest agents deep
/// enough on disk to require recursing further, and `spawnDepth` on the meta
/// file is preserved for a future provider revision that does).
fn build_subagent_turns(chain: &[&Map<String, Value>]) -> Vec<HistoricalTurn> {
    let mut turns: Vec<HistoricalTurn> = Vec::new();
    let mut pending_tools: HashMap<String, (usize, usize)> = HashMap::new();
    let no_subagents = HashMap::new();

    for entry in chain {
        if is_meta(entry) {
            continue;
        }
        match entry_type(entry) {
            Some("user") if turns.is_empty() => {
                // The subagent's own seed prompt (first record) opens its
                // first turn; later `user` records in a subagent stream are
                // tool results, same as the main chain.
                let content = entry
                    .get("message")
                    .and_then(|message| message.get("content"));
                let started_at = entry_timestamp(entry).unwrap_or_else(unix_time);
                turns.push(HistoricalTurn {
                    id: entry_uuid(entry).unwrap_or_default().to_owned(),
                    started_at,
                    completed_at: None,
                    items: vec![HistoricalTurnItem::Message(HistoricalMessage {
                        role: HistoricalRole::User,
                        content: user_visible_text(content),
                        created_at: started_at,
                    })],
                });
            }
            Some("user") => {
                let Some(content) = entry
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .and_then(Value::as_array)
                else {
                    continue;
                };
                let completed_at = entry_timestamp(entry);
                for block in content {
                    if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    apply_tool_result(&mut turns, &mut pending_tools, block, completed_at);
                }
            }
            Some("assistant") => {
                let Some(turn) = turns.last_mut() else {
                    continue;
                };
                let created_at = entry_timestamp(entry).unwrap_or_else(unix_time);
                turn.completed_at = Some(created_at);
                let Some(content) = entry
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .and_then(Value::as_array)
                else {
                    continue;
                };
                let turn_index = turns.len() - 1;
                for block in content {
                    apply_assistant_block(
                        &mut turns[turn_index],
                        block,
                        created_at,
                        turn_index,
                        &mut pending_tools,
                        &no_subagents,
                    );
                }
            }
            _ => {}
        }
    }
    turns.retain(|turn| !turn.items.is_empty());
    turns
}

fn apply_tool_result(
    turns: &mut [HistoricalTurn],
    pending_tools: &mut HashMap<String, (usize, usize)>,
    block: &Value,
    completed_at: Option<u64>,
) {
    let Some(id) = block.get("tool_use_id").and_then(Value::as_str) else {
        return;
    };
    let Some((turn_index, activity_index)) = pending_tools.remove(id) else {
        return;
    };
    let Some(HistoricalTurnItem::Activity(activity)) = turns
        .get_mut(turn_index)
        .and_then(|turn| turn.items.get_mut(activity_index))
    else {
        return;
    };
    activity.failed = block.get("is_error").and_then(Value::as_bool) == Some(true);
    activity.output = block
        .get("content")
        .map(content_block_to_text)
        .filter(|text| !text.is_empty());
    activity.complete = true;
    activity.completed_at = completed_at;
}

/// Flatten a `tool_result`'s `content` (a plain string, or an array of typed
/// blocks) into plain text for the activity's output field. This is the only
/// place raw block text is joined into a single string, and the result is
/// activity output — never message content.
fn content_block_to_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| match block.get("type").and_then(Value::as_str) {
                Some("text") => block.get("text").and_then(Value::as_str).map(str::to_owned),
                None => block.get("text").and_then(Value::as_str).map(str::to_owned),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn claude_title(entry: &Map<String, Value>) -> Option<String> {
    let field = match entry_type(entry) {
        Some("ai-title") => "aiTitle",
        Some("custom-title") => "customTitle",
        _ => return None,
    };
    entry
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn write_jsonl(path: &Path, entries: &[Value]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut file = fs::File::create(path).unwrap();
        for entry in entries {
            serde_json::to_writer(&mut file, entry).unwrap();
            file.write_all(b"\n").unwrap();
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("waku-claude-historical-{label}-{}", Uuid::new_v4()))
    }

    #[test]
    fn simple_user_assistant_becomes_one_turn_two_messages() {
        let session_id = Uuid::new_v4().to_string();
        let user_uuid = Uuid::new_v4().to_string();
        let assistant_uuid = Uuid::new_v4().to_string();
        let entries = vec![
            json!({"type":"user","uuid":user_uuid,"parentUuid":null,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:00.000Z",
                   "message":{"role":"user","content":"hello"}}),
            json!({"type":"assistant","uuid":assistant_uuid,"parentUuid":user_uuid,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:01.000Z",
                   "message":{"role":"assistant","content":[{"type":"text","text":"hi there"}]}}),
        ];
        let entries: Vec<Map<String, Value>> = entries
            .into_iter()
            .map(|v| v.as_object().unwrap().clone())
            .collect();
        let transcript = build_transcript(&entries, &session_id, &HashMap::new()).unwrap();
        assert_eq!(transcript.turns.len(), 1);
        let items = &transcript.turns[0].items;
        assert_eq!(items.len(), 2);
        assert!(
            matches!(&items[0], HistoricalTurnItem::Message(m) if m.role == HistoricalRole::User && m.content == "hello")
        );
        assert!(
            matches!(&items[1], HistoricalTurnItem::Message(m) if m.role == HistoricalRole::Assistant && m.content == "hi there")
        );
    }

    #[test]
    fn tool_use_and_result_become_one_activity_not_messages() {
        let session_id = Uuid::new_v4().to_string();
        let user_uuid = Uuid::new_v4().to_string();
        let assistant_uuid = Uuid::new_v4().to_string();
        let result_uuid = Uuid::new_v4().to_string();
        let entries = vec![
            json!({"type":"user","uuid":user_uuid,"parentUuid":null,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:00.000Z",
                   "message":{"role":"user","content":"read the file"}}),
            json!({"type":"assistant","uuid":assistant_uuid,"parentUuid":user_uuid,"sessionId":session_id,
            "timestamp":"2026-01-01T00:00:01.000Z",
            "message":{"role":"assistant","content":[
                 {"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"/tmp/a.txt"}}
            ]}}),
            json!({"type":"user","uuid":result_uuid,"parentUuid":assistant_uuid,"sessionId":session_id,
            "timestamp":"2026-01-01T00:00:02.000Z",
            "message":{"role":"user","content":[
                 {"type":"tool_result","tool_use_id":"toolu_1","content":"file contents"}
            ]}}),
        ];
        let entries: Vec<Map<String, Value>> = entries
            .into_iter()
            .map(|v| v.as_object().unwrap().clone())
            .collect();
        let transcript = build_transcript(&entries, &session_id, &HashMap::new()).unwrap();
        assert_eq!(transcript.turns.len(), 1);
        let items = &transcript.turns[0].items;
        // user message + one activity — the tool_result must not add a
        // second turn or a third message.
        assert_eq!(items.len(), 2);
        let HistoricalTurnItem::Activity(activity) = &items[1] else {
            panic!("expected an activity, got {:?}", items[1]);
        };
        assert_eq!(activity.kind, HistoricalActivityKind::Tool);
        assert_eq!(activity.title, "Read");
        assert_eq!(activity.source_id.as_deref(), Some("toolu_1"));
        assert_eq!(activity.output.as_deref(), Some("file contents"));
        assert!(activity.complete);
        assert!(!activity.failed);
    }

    #[test]
    fn thinking_becomes_reasoning_activity_not_message() {
        let session_id = Uuid::new_v4().to_string();
        let user_uuid = Uuid::new_v4().to_string();
        let assistant_uuid = Uuid::new_v4().to_string();
        let entries = vec![
            json!({"type":"user","uuid":user_uuid,"parentUuid":null,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:00.000Z",
                   "message":{"role":"user","content":"think about it"}}),
            json!({"type":"assistant","uuid":assistant_uuid,"parentUuid":user_uuid,"sessionId":session_id,
            "timestamp":"2026-01-01T00:00:01.000Z",
            "message":{"role":"assistant","content":[
                 {"type":"thinking","thinking":"pondering deeply"},
                 {"type":"text","text":"here is my answer"}
            ]}}),
        ];
        let entries: Vec<Map<String, Value>> = entries
            .into_iter()
            .map(|v| v.as_object().unwrap().clone())
            .collect();
        let transcript = build_transcript(&entries, &session_id, &HashMap::new()).unwrap();
        let items = &transcript.turns[0].items;
        assert_eq!(items.len(), 3);
        let HistoricalTurnItem::Activity(reasoning) = &items[1] else {
            panic!("expected reasoning activity");
        };
        assert_eq!(reasoning.kind, HistoricalActivityKind::Reasoning);
        assert_eq!(reasoning.content.as_deref(), Some("pondering deeply"));
        assert!(
            matches!(&items[2], HistoricalTurnItem::Message(m) if m.content == "here is my answer")
        );
    }

    #[test]
    fn is_meta_slash_command_scaffolding_never_becomes_a_message() {
        let session_id = Uuid::new_v4().to_string();
        let meta_uuid = Uuid::new_v4().to_string();
        let real_uuid = Uuid::new_v4().to_string();
        let entries = vec![
            json!({"type":"user","uuid":meta_uuid,"parentUuid":null,"sessionId":session_id,"isMeta":true,
                   "timestamp":"2026-01-01T00:00:00.000Z",
                   "message":{"role":"user","content":"<command-name>/clear</command-name>"}}),
            json!({"type":"user","uuid":real_uuid,"parentUuid":meta_uuid,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:01.000Z",
                   "message":{"role":"user","content":"what does this do?"}}),
        ];
        let entries: Vec<Map<String, Value>> = entries
            .into_iter()
            .map(|v| v.as_object().unwrap().clone())
            .collect();
        let transcript = build_transcript(&entries, &session_id, &HashMap::new()).unwrap();
        assert_eq!(transcript.turns.len(), 1);
        assert_eq!(transcript.turns[0].id, real_uuid);
    }

    /// Real Claude Code transcripts echo a local slash-command invocation
    /// (`/clear`, `/compact`, `/model`, …) as an ordinary, non-`isMeta` user
    /// record whose content is the literal `<command-name>…</command-name>`
    /// markup — confirmed against real `~/.claude/projects` data, where this
    /// shape appears without `isMeta` hundreds of times. A session that is
    /// only a `/clear` invocation must not surface that raw control text as
    /// a turn or a title.
    #[test]
    fn slash_command_echo_without_is_meta_never_becomes_a_turn() {
        let session_id = Uuid::new_v4().to_string();
        let caveat_uuid = Uuid::new_v4().to_string();
        let command_uuid = Uuid::new_v4().to_string();
        let entries = vec![
            json!({"type":"user","uuid":caveat_uuid,"parentUuid":null,"sessionId":session_id,"isMeta":true,
                   "timestamp":"2026-01-01T00:00:00.000Z",
                   "message":{"role":"user","content":"<local-command-caveat>Caveat: ...</local-command-caveat>"}}),
            json!({"type":"user","uuid":command_uuid,"parentUuid":caveat_uuid,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:01.000Z",
                   "message":{"role":"user","content":"<command-name>/clear</command-name>\n            <command-message>clear</command-message>\n            <command-args></command-args>"}}),
        ];
        let entries: Vec<Map<String, Value>> = entries
            .into_iter()
            .map(|v| v.as_object().unwrap().clone())
            .collect();
        // A session that is nothing but a slash-command invocation has no
        // real conversation to reconstruct.
        assert!(build_transcript(&entries, &session_id, &HashMap::new()).is_none());
    }

    #[test]
    fn slash_command_echo_between_real_prompts_does_not_split_the_turn() {
        let session_id = Uuid::new_v4().to_string();
        let user_uuid = Uuid::new_v4().to_string();
        let assistant_uuid = Uuid::new_v4().to_string();
        let command_uuid = Uuid::new_v4().to_string();
        let entries = vec![
            json!({"type":"user","uuid":user_uuid,"parentUuid":null,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:00.000Z","message":{"role":"user","content":"hello"}}),
            json!({"type":"assistant","uuid":assistant_uuid,"parentUuid":user_uuid,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:01.000Z",
                   "message":{"role":"assistant","content":[{"type":"text","text":"hi there"}]}}),
            json!({"type":"user","uuid":command_uuid,"parentUuid":assistant_uuid,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:02.000Z",
                   "message":{"role":"user","content":"<command-name>/compact</command-name>\n<command-message>compact</command-message>\n<command-args></command-args>"}}),
        ];
        let entries: Vec<Map<String, Value>> = entries
            .into_iter()
            .map(|v| v.as_object().unwrap().clone())
            .collect();
        let transcript = build_transcript(&entries, &session_id, &HashMap::new()).unwrap();
        assert_eq!(
            transcript.turns.len(),
            1,
            "the trailing /compact echo must not open a new turn"
        );
    }

    /// A custom (frontmatter-driven) slash command has been observed on real
    /// data emitting the same two control tags in the opposite order —
    /// `<command-message>` leading instead of `<command-name>` — which a
    /// naive `starts_with("<command-name>")` check misses entirely. This
    /// caused a real imported session's *title* to become literal
    /// `<command-message>tally-shiphero</command-message>` control text.
    #[test]
    fn slash_command_echo_with_reordered_tags_is_still_recognized() {
        let session_id = Uuid::new_v4().to_string();
        let command_uuid = Uuid::new_v4().to_string();
        let real_uuid = Uuid::new_v4().to_string();
        let entries = vec![
            json!({"type":"user","uuid":command_uuid,"parentUuid":null,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:00.000Z",
                   "message":{"role":"user","content":"<command-message>tally-shiphero</command-message>\n<command-name>/tally-shiphero</command-name>\n<command-args></command-args>"}}),
            json!({"type":"user","uuid":real_uuid,"parentUuid":command_uuid,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:01.000Z",
                   "message":{"role":"user","content":"tally the shiphero invoices for this week"}}),
        ];
        let entries: Vec<Map<String, Value>> = entries
            .into_iter()
            .map(|v| v.as_object().unwrap().clone())
            .collect();
        let transcript = build_transcript(&entries, &session_id, &HashMap::new()).unwrap();
        assert_eq!(transcript.turns.len(), 1);
        assert_eq!(transcript.turns[0].id, real_uuid);
    }

    /// After the `<command-name>`/`<command-message>` echo, the CLI also
    /// echoes the command's own stdout as a *third* synthetic, non-`isMeta`
    /// `user`-role record — confirmed on real data as the source of a real
    /// imported session's title becoming literal
    /// `<local-command-stdout>Set model to Sonnet 5…</local-command-stdout>`.
    #[test]
    fn local_command_stdout_echo_never_becomes_a_turn_or_title() {
        let session_id = Uuid::new_v4().to_string();
        let command_uuid = Uuid::new_v4().to_string();
        let stdout_uuid = Uuid::new_v4().to_string();
        let entries = vec![
            json!({"type":"user","uuid":command_uuid,"parentUuid":null,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:00.000Z",
                   "message":{"role":"user","content":"<command-name>/model</command-name>\n<command-message>model</command-message>\n<command-args>sonnet</command-args>"}}),
            json!({"type":"user","uuid":stdout_uuid,"parentUuid":command_uuid,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:01.000Z",
                   "message":{"role":"user","content":"<local-command-stdout>Set model to Sonnet 5 and saved as your default for new sessions</local-command-stdout>"}}),
        ];
        let entries: Vec<Map<String, Value>> = entries
            .into_iter()
            .map(|v| v.as_object().unwrap().clone())
            .collect();
        assert!(
            build_transcript(&entries, &session_id, &HashMap::new()).is_none(),
            "a session that is only a slash command and its own stdout echo has no real conversation"
        );
    }

    /// A background task or subagent's completion notice is threaded back
    /// onto the main turn as a `<task-notification>…</task-notification>`
    /// wrapper in a non-`isMeta` `user`-role record — confirmed on real data
    /// (130 occurrences). It must never become a visible turn.
    #[test]
    fn task_notification_wrapper_never_becomes_a_turn() {
        let session_id = Uuid::new_v4().to_string();
        let user_uuid = Uuid::new_v4().to_string();
        let assistant_uuid = Uuid::new_v4().to_string();
        let notification_uuid = Uuid::new_v4().to_string();
        let entries = vec![
            json!({"type":"user","uuid":user_uuid,"parentUuid":null,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:00.000Z","message":{"role":"user","content":"run the background task"}}),
            json!({"type":"assistant","uuid":assistant_uuid,"parentUuid":user_uuid,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:01.000Z",
                   "message":{"role":"assistant","content":[{"type":"text","text":"Started it in the background."}]}}),
            json!({"type":"user","uuid":notification_uuid,"parentUuid":assistant_uuid,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:02.000Z",
                   "message":{"role":"user","content":"<task-notification>\n<task-id>abc123</task-id>\n<status>completed</status>\n<summary>Background command finished</summary>\n</task-notification>"}}),
        ];
        let entries: Vec<Map<String, Value>> = entries
            .into_iter()
            .map(|v| v.as_object().unwrap().clone())
            .collect();
        let transcript = build_transcript(&entries, &session_id, &HashMap::new()).unwrap();
        assert_eq!(
            transcript.turns.len(),
            1,
            "the task-notification echo must not open a second turn"
        );
    }

    /// A real user paste that happens to start with an angle-bracket tag
    /// (an HTML error page, in this case pulled from real captured data)
    /// must never be mistaken for control scaffolding — the filter is an
    /// allowlist of confirmed wrapper tags, not a bare `starts_with('<')`.
    #[test]
    fn a_real_prompt_that_pastes_html_is_not_treated_as_scaffolding() {
        let session_id = Uuid::new_v4().to_string();
        let user_uuid = Uuid::new_v4().to_string();
        let entries = vec![json!({
            "type":"user","uuid":user_uuid,"parentUuid":null,"sessionId":session_id,
            "timestamp":"2026-01-01T00:00:00.000Z",
            "message":{"role":"user","content":"<html>\n<head><title>405 Not Allowed</title></head>\n<body><center><h1>405 Not Allowed</h1></center></body>\n</html> why am I getting this from the API?"}
        })];
        let entries: Vec<Map<String, Value>> = entries
            .into_iter()
            .map(|v| v.as_object().unwrap().clone())
            .collect();
        let transcript = build_transcript(&entries, &session_id, &HashMap::new()).unwrap();
        assert_eq!(
            transcript.turns.len(),
            1,
            "a real pasted-HTML prompt must still open a turn"
        );
    }

    #[test]
    fn malformed_lines_are_skipped_not_fatal() {
        let dir = temp_dir("malformed");
        let project = dir.join("-tmp-fixture");
        fs::create_dir_all(&project).unwrap();
        let session_id = Uuid::new_v4().to_string();
        let user_uuid = Uuid::new_v4().to_string();
        let path = project.join(format!("{session_id}.jsonl"));
        let mut file = fs::File::create(&path).unwrap();
        use std::io::Write as _;
        writeln!(file, "not json at all").unwrap();
        writeln!(file, "{{\"broken\": ").unwrap();
        serde_json::to_writer(
            &mut file,
            &json!({"type":"user","uuid":user_uuid,"parentUuid":null,"sessionId":session_id,
                    "timestamp":"2026-01-01T00:00:00.000Z",
                    "message":{"role":"user","content":"still works"}}),
        )
        .unwrap();
        file.write_all(b"\n").unwrap();
        drop(file);

        let transcript = parse_session_file(&path).expect("malformed lines must not be fatal");
        assert_eq!(transcript.turns.len(), 1);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unknown_record_types_are_ignored() {
        let session_id = Uuid::new_v4().to_string();
        let user_uuid = Uuid::new_v4().to_string();
        let entries = vec![
            json!({"type":"some-future-record-type","sessionId":session_id,"data":"whatever"}),
            json!({"type":"user","uuid":user_uuid,"parentUuid":null,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:00.000Z",
                   "message":{"role":"user","content":"hi"}}),
        ];
        let entries: Vec<Map<String, Value>> = entries
            .into_iter()
            .map(|v| v.as_object().unwrap().clone())
            .collect();
        let transcript = build_transcript(&entries, &session_id, &HashMap::new()).unwrap();
        assert_eq!(transcript.turns.len(), 1);
    }

    #[test]
    fn abandoned_branch_is_excluded_from_active_chain() {
        let session_id = Uuid::new_v4().to_string();
        let root = Uuid::new_v4().to_string();
        let abandoned = Uuid::new_v4().to_string();
        let active = Uuid::new_v4().to_string();
        let entries = vec![
            json!({"type":"user","uuid":root,"parentUuid":null,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:00.000Z","message":{"role":"user","content":"start"}}),
            // An abandoned branch off root: a reply that is not the one the
            // chain actually continued from.
            json!({"type":"assistant","uuid":abandoned,"parentUuid":root,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:01.000Z",
                   "message":{"role":"assistant","content":[{"type":"text","text":"abandoned reply"}]}}),
            // The branch actually resumed from: a sibling reply under root.
            json!({"type":"assistant","uuid":active,"parentUuid":root,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:02.000Z",
                   "message":{"role":"assistant","content":[{"type":"text","text":"active reply"}]}}),
        ];
        let entries: Vec<Map<String, Value>> = entries
            .into_iter()
            .map(|v| v.as_object().unwrap().clone())
            .collect();
        let transcript = build_transcript(&entries, &session_id, &HashMap::new()).unwrap();
        let items = &transcript.turns[0].items;
        // Only the chain ending at the *last* entry survives: "active reply".
        let texts: Vec<&str> = items
            .iter()
            .filter_map(|item| match item {
                HistoricalTurnItem::Message(m) => Some(m.content.as_str()),
                _ => None,
            })
            .collect();
        assert!(texts.contains(&"active reply"));
        assert!(!texts.contains(&"abandoned reply"));
    }

    #[test]
    fn missing_tool_result_leaves_activity_incomplete_not_invented() {
        let session_id = Uuid::new_v4().to_string();
        let user_uuid = Uuid::new_v4().to_string();
        let assistant_uuid = Uuid::new_v4().to_string();
        let entries = vec![
            json!({"type":"user","uuid":user_uuid,"parentUuid":null,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:00.000Z","message":{"role":"user","content":"run a command"}}),
            json!({"type":"assistant","uuid":assistant_uuid,"parentUuid":user_uuid,"sessionId":session_id,
            "timestamp":"2026-01-01T00:00:01.000Z",
            "message":{"role":"assistant","content":[
                 {"type":"tool_use","id":"toolu_missing","name":"Bash","input":{"command":"echo hi"}}
            ]}}),
        ];
        let entries: Vec<Map<String, Value>> = entries
            .into_iter()
            .map(|v| v.as_object().unwrap().clone())
            .collect();
        let transcript = build_transcript(&entries, &session_id, &HashMap::new()).unwrap();
        let HistoricalTurnItem::Activity(activity) = &transcript.turns[0].items[1] else {
            panic!("expected activity");
        };
        assert!(!activity.complete);
        assert!(activity.output.is_none());
    }

    #[test]
    fn subagent_tool_use_correlates_with_meta_json_and_own_transcript() {
        let dir = temp_dir("subagent");
        let project = dir.join("-tmp-fixture");
        let session_id = Uuid::new_v4().to_string();
        let session_dir = project.join(&session_id).join("subagents");
        let agent_id = "aabbccdd112233445";
        write_jsonl(
            &session_dir.join(format!("agent-{agent_id}.jsonl")),
            &[
                json!({"type":"user","uuid":"11111111-1111-4111-8111-111111111111","parentUuid":null,
                       "isSidechain":true,"agentId":agent_id,"sessionId":session_id,
                       "timestamp":"2026-01-01T00:00:01.000Z",
                       "message":{"role":"user","content":"investigate the bug"}}),
                json!({"type":"assistant","uuid":"22222222-2222-4222-8222-222222222222",
                       "parentUuid":"11111111-1111-4111-8111-111111111111",
                       "isSidechain":true,"agentId":agent_id,"sessionId":session_id,
                       "timestamp":"2026-01-01T00:00:02.000Z",
                       "message":{"role":"assistant","content":[{"type":"text","text":"Found the root cause in foo.rs"}]}}),
            ],
        );
        fs::write(
            session_dir.join(format!("agent-{agent_id}.meta.json")),
            serde_json::to_string(&json!({
                "agentType": "general-purpose",
                "description": "Investigate the bug",
                "toolUseId": "toolu_agent_1",
                "spawnDepth": 1
            }))
            .unwrap(),
        )
        .unwrap();

        let user_uuid = Uuid::new_v4().to_string();
        let assistant_uuid = Uuid::new_v4().to_string();
        let main_entries = vec![
            json!({"type":"user","uuid":user_uuid,"parentUuid":null,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:00.000Z","message":{"role":"user","content":"find the bug"}}),
            json!({"type":"assistant","uuid":assistant_uuid,"parentUuid":user_uuid,"sessionId":session_id,
            "timestamp":"2026-01-01T00:00:01.000Z",
            "message":{"role":"assistant","content":[
                 {"type":"tool_use","id":"toolu_agent_1","name":"Agent",
                  "input":{"description":"Investigate the bug","prompt":"find the root cause","subagent_type":"general-purpose"}}
            ]}}),
        ];
        let session_path = project.join(format!("{session_id}.jsonl"));
        write_jsonl(&session_path, &main_entries);

        let subagents = load_subagent_records(&session_dir);
        assert_eq!(subagents.len(), 1);
        let main_entries: Vec<Map<String, Value>> = main_entries
            .into_iter()
            .map(|v| v.as_object().unwrap().clone())
            .collect();
        let transcript = build_transcript(&main_entries, &session_id, &subagents).unwrap();

        let HistoricalTurnItem::Activity(activity) = &transcript.turns[0].items[1] else {
            panic!("expected subagent activity");
        };
        assert_eq!(activity.kind, HistoricalActivityKind::Subagent);
        let subagent = activity.subagent.as_ref().expect("subagent attached");
        assert_eq!(subagent.id, agent_id);
        assert_eq!(subagent.status, HistoricalSubagentStatus::Completed);
        assert_eq!(
            subagent.output.as_deref(),
            Some("Found the root cause in foo.rs")
        );
        assert_eq!(subagent.name.as_deref(), Some("general-purpose"));

        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn compact_boundary_does_not_break_chain_reconstruction() {
        let session_id = Uuid::new_v4().to_string();
        let user_uuid = Uuid::new_v4().to_string();
        let boundary_uuid = Uuid::new_v4().to_string();
        let after_uuid = Uuid::new_v4().to_string();
        let assistant_uuid = Uuid::new_v4().to_string();
        let entries = vec![
            json!({"type":"user","uuid":user_uuid,"parentUuid":null,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:00.000Z","message":{"role":"user","content":"long conversation"}}),
            json!({"type":"system","uuid":boundary_uuid,"parentUuid":user_uuid,"sessionId":session_id,
                   "subtype":"compact_boundary","content":"Conversation compacted","level":"info",
                   "timestamp":"2026-01-01T00:05:00.000Z",
                   "compactMetadata":{"trigger":"manual","preTokens":100000,"postTokens":5000}}),
            json!({"type":"user","uuid":after_uuid,"parentUuid":boundary_uuid,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:05:01.000Z","message":{"role":"user","content":"continue"}}),
            json!({"type":"assistant","uuid":assistant_uuid,"parentUuid":after_uuid,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:05:02.000Z",
                   "message":{"role":"assistant","content":[{"type":"text","text":"continuing"}]}}),
        ];
        let entries: Vec<Map<String, Value>> = entries
            .into_iter()
            .map(|v| v.as_object().unwrap().clone())
            .collect();
        let transcript = build_transcript(&entries, &session_id, &HashMap::new()).unwrap();
        // The compact_boundary system record is a chain link but not a turn
        // or message; both real prompts still become turns.
        assert_eq!(transcript.turns.len(), 2);
        assert_eq!(transcript.turns[0].id, user_uuid);
        assert_eq!(transcript.turns[1].id, after_uuid);
    }

    #[test]
    fn long_tool_output_is_preserved_in_full() {
        let session_id = Uuid::new_v4().to_string();
        let user_uuid = Uuid::new_v4().to_string();
        let assistant_uuid = Uuid::new_v4().to_string();
        let result_uuid = Uuid::new_v4().to_string();
        let long_output = "x".repeat(50_000);
        let entries = vec![
            json!({"type":"user","uuid":user_uuid,"parentUuid":null,"sessionId":session_id,
                   "timestamp":"2026-01-01T00:00:00.000Z","message":{"role":"user","content":"dump the file"}}),
            json!({"type":"assistant","uuid":assistant_uuid,"parentUuid":user_uuid,"sessionId":session_id,
            "timestamp":"2026-01-01T00:00:01.000Z",
            "message":{"role":"assistant","content":[
                 {"type":"tool_use","id":"toolu_long","name":"Read","input":{"file_path":"/tmp/big.txt"}}
            ]}}),
            json!({"type":"user","uuid":result_uuid,"parentUuid":assistant_uuid,"sessionId":session_id,
            "timestamp":"2026-01-01T00:00:02.000Z",
            "message":{"role":"user","content":[
                 {"type":"tool_result","tool_use_id":"toolu_long","content":long_output}
            ]}}),
        ];
        let entries: Vec<Map<String, Value>> = entries
            .into_iter()
            .map(|v| v.as_object().unwrap().clone())
            .collect();
        let transcript = build_transcript(&entries, &session_id, &HashMap::new()).unwrap();
        let HistoricalTurnItem::Activity(activity) = &transcript.turns[0].items[1] else {
            panic!("expected activity");
        };
        assert_eq!(activity.output.as_deref().unwrap().len(), 50_000);
    }

    #[test]
    fn discover_sessions_finds_fixture_and_deduplicates_by_file() {
        let dir = temp_dir("discover");
        let project = dir.join("-tmp-fixture-project");
        fs::create_dir_all(&project).unwrap();
        let session_id = Uuid::new_v4().to_string();
        let user_uuid = Uuid::new_v4().to_string();
        let assistant_uuid = Uuid::new_v4().to_string();
        write_jsonl(
            &project.join(format!("{session_id}.jsonl")),
            &[
                json!({"type":"user","uuid":user_uuid,"parentUuid":null,"sessionId":session_id,
                       "cwd":"/tmp/fixture-project","timestamp":"2026-01-01T00:00:00.000Z",
                       "message":{"role":"user","content":"hello"}}),
                json!({"type":"assistant","uuid":assistant_uuid,"parentUuid":user_uuid,"sessionId":session_id,
                       "cwd":"/tmp/fixture-project","timestamp":"2026-01-01T00:00:01.000Z",
                       "message":{"role":"assistant","content":[{"type":"text","text":"hi there"}]}}),
            ],
        );
        let provider = ClaudeHistoricalProvider::with_projects_dir(dir.clone());
        let sessions = provider.discover_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].native_session_id, session_id);
        assert_eq!(sessions[0].cwd, Some(PathBuf::from("/tmp/fixture-project")));
        fs::remove_dir_all(dir).ok();
    }
}
