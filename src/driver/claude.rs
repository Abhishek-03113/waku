//! Claude Code's streaming-input session.
//!
//! `claude` accepts a realtime stream of user messages on stdin and answers on
//! stdout, which is the same transport the Claude Agent SDK's `query()` drives —
//! the SDK is a wrapper around these flags, not a separate capability. One
//! process serves the whole conversation, and `--permission-prompt-tool stdio`
//! makes it ask the host before running a tool instead of deciding alone.
//!
//! Flags and payloads here were read off the real CLI and the SDK's own
//! invocation, not guessed. `--permission-prompt-tool` in particular is absent
//! from `claude --help`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::Stdio;
use std::sync::Arc;
use std::thread;

use anyhow::{Context as _, anyhow};
use crossbeam_channel::{Sender, unbounded};
use parking_lot::Mutex;
use serde_json::{Value, json};
use uuid::Uuid;

use super::activity;
use crate::driver::{DriverControl, DriverStartOptions, SessionOptions};
use crate::model::{
    ActivityKind, DriverEvent, InteractionMode, PermissionOption, ProviderResumeCursor, RuntimeMode,
};

enum CommandMessage {
    Prompt(String),
    Cancel,
    Respond {
        request_id: String,
        option_id: String,
    },
    Options(SessionOptions),
    Shutdown,
}

pub struct ClaudeDriver {
    commands: Sender<CommandMessage>,
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
}

/// The permission posture Claude is launched with.
fn permission_mode(mode: RuntimeMode, interaction_mode: InteractionMode) -> &'static str {
    if interaction_mode == InteractionMode::Plan || mode == RuntimeMode::Plan {
        return "plan";
    }
    match mode {
        RuntimeMode::Ask => "default",
        RuntimeMode::AutoAcceptEdits => "acceptEdits",
        RuntimeMode::Auto => "auto",
        RuntimeMode::FullAccess => "bypassPermissions",
        RuntimeMode::Plan => unreachable!("handled above"),
    }
}

impl ClaudeDriver {
    pub fn start(
        options: DriverStartOptions,
        events: Sender<DriverEvent>,
    ) -> anyhow::Result<Self> {
        let DriverStartOptions {
            binary,
            cwd,
            mode,
            interaction_mode,
            model,
            reasoning_effort,
            service_tier: _,
            computer_use_enabled: _,
            provider_cursor,
        } = options;
        let (resume_session_id, resume_at) = match provider_cursor {
            Some(ProviderResumeCursor::Claude {
                session_id,
                resume_at,
            }) => ((!session_id.is_empty()).then_some(session_id), resume_at),
            Some(cursor) => {
                return Err(anyhow!(
                    "cannot resume Claude Code from a {} cursor",
                    cursor.provider().display_name()
                ));
            }
            None => (None, None),
        };
        // Claude accepts a caller-chosen session id, so the cursor exists before
        // the first turn does and a rewind has something to point at.
        let session_id = resume_session_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        let mut command = crate::command_env::command(&binary);
        command.current_dir(&cwd).args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            // Echoes each user message back, which is how a queued prompt is
            // distinguished from one the agent has started on.
            "--replay-user-messages",
            // Undocumented, and the whole reason Supervised can mean supervised:
            // without it the CLI decides permissions itself and only reports
            // denials after the fact.
            "--permission-prompt-tool",
            "stdio",
            "--permission-mode",
            permission_mode(mode, interaction_mode),
        ]);
        if mode == RuntimeMode::FullAccess && interaction_mode != InteractionMode::Plan {
            command.arg("--dangerously-skip-permissions");
        }
        if let Some(model) = model.as_deref() {
            command.args(["--model", model]);
        }
        if let Some(effort) = reasoning_effort.as_deref() {
            command.args(["--effort", effort]);
        }
        if resume_session_id.is_some() {
            command.args(["--resume", &session_id]);
        } else {
            command.args(["--session-id", &session_id]);
        }

        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to start `claude` in streaming-input mode")?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Claude stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Claude stdout unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("Claude stderr unavailable"))?;

        // The cursor is known up front, so a rewind can address this session
        // before it has produced anything.
        let _ = events.send(DriverEvent::Connected {
            provider_cursor: Some(ProviderResumeCursor::Claude {
                session_id: session_id.clone(),
                resume_at,
            }),
        });

        let (commands, command_rx) = unbounded();
        let auto_approve = mode != RuntimeMode::Ask;
        let turn_active = Arc::new(Mutex::new(false));

        let reader_events = events.clone();
        let reader_commands = commands.clone();
        let reader_turn = turn_active.clone();
        let reader_session = session_id.clone();
        let reader_thread = thread::Builder::new()
            .name("waku-claude-reader".into())
            .spawn(move || {
                let mut state = ClaudeStreamState::default();
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let Ok(value) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    handle_message(
                        &value,
                        &reader_session,
                        &reader_events,
                        &reader_commands,
                        &reader_turn,
                        auto_approve,
                        &mut state,
                    );
                }
            })?;

        let writer_events = events.clone();
        let writer_turn = turn_active;
        thread::Builder::new()
            .name("waku-claude-writer".into())
            .spawn(move || {
                let mut stdin = stdin;
                let mut next_request_id = 0_u64;
                let mut current_model = model;
                while let Ok(message) = command_rx.recv() {
                    let written = match message {
                        CommandMessage::Prompt(text) => {
                            *writer_turn.lock() = true;
                            let _ = writer_events.send(DriverEvent::TurnStarted);
                            write_line(
                                &mut stdin,
                                &json!({
                                    "type": "user",
                                    "message": {
                                        "role": "user",
                                        "content": [{"type": "text", "text": text}]
                                    },
                                    "parent_tool_use_id": null
                                }),
                            )
                        }
                        CommandMessage::Cancel => {
                            next_request_id += 1;
                            write_line(
                                &mut stdin,
                                &json!({
                                    "type": "control_request",
                                    "request_id": format!("waku-{next_request_id}"),
                                    "request": {"subtype": "interrupt"}
                                }),
                            )
                        }
                        CommandMessage::Respond {
                            request_id,
                            option_id,
                        } => {
                            let decision = if option_id == "deny" {
                                json!({
                                    "behavior": "deny",
                                    "message": "The user denied this tool call."
                                })
                            } else {
                                json!({"behavior": "allow"})
                            };
                            write_line(
                                &mut stdin,
                                &json!({
                                    "type": "control_response",
                                    "response": {
                                        "subtype": "success",
                                        "request_id": request_id,
                                        "response": decision
                                    }
                                }),
                            )
                        }
                        CommandMessage::Options(options) => {
                            if options.model == current_model {
                                continue;
                            }
                            current_model = options.model;
                            let Some(model) = current_model.as_deref() else {
                                continue;
                            };
                            next_request_id += 1;
                            write_line(
                                &mut stdin,
                                &json!({
                                    "type": "control_request",
                                    "request_id": format!("waku-{next_request_id}"),
                                    "request": {"subtype": "set_model", "model": model}
                                }),
                            )
                        }
                        CommandMessage::Shutdown => break,
                    };
                    if let Err(error) = written {
                        let _ = writer_events.send(DriverEvent::Error(format!(
                            "Claude transport write failed: {error}"
                        )));
                        // Nothing will settle a turn whose prompt never landed.
                        if std::mem::take(&mut *writer_turn.lock()) {
                            let _ = writer_events.send(DriverEvent::TurnFinished {
                                success: false,
                                summary: Some("Claude could not receive the prompt.".into()),
                            });
                        }
                        break;
                    }
                }
            })?;

        let last_visible_stderr = Arc::new(Mutex::new(None::<String>));
        let stderr_last_error = last_visible_stderr.clone();
        let stderr_events = events.clone();
        let stderr_thread = thread::Builder::new()
            .name("waku-claude-stderr".into())
            .spawn(move || {
                let lines = BufReader::new(stderr)
                    .lines()
                    .map_while(Result::ok)
                    .filter(|line| !line.trim().is_empty())
                    .collect::<Vec<_>>();
                if let Some(message) = super::support::provider_stderr_error(lines) {
                    let error = format!("Claude Code: {message}");
                    *stderr_last_error.lock() = Some(error.clone());
                    let _ = stderr_events.send(DriverEvent::Error(error));
                }
            })?;

        thread::Builder::new()
            .name("waku-claude-process".into())
            .spawn(move || {
                let status = child.wait();
                let _ = reader_thread.join();
                let _ = stderr_thread.join();
                if let Ok(status) = status
                    && !status.success()
                    && last_visible_stderr.lock().is_none()
                {
                    let _ = events.send(DriverEvent::Error(format!(
                        "Claude Code exited with {status}"
                    )));
                }
                let _ = events.send(DriverEvent::ProcessExited);
            })?;

        Ok(Self {
            commands,
            mode,
            interaction_mode,
        })
    }
}

impl DriverControl for ClaudeDriver {
    fn prompt(&self, prompt: String) {
        let _ = self.commands.send(CommandMessage::Prompt(prompt));
    }

    fn cancel(&self) {
        let _ = self.commands.send(CommandMessage::Cancel);
    }

    fn respond(&self, request_id: String, option_id: String) {
        let _ = self.commands.send(CommandMessage::Respond {
            request_id,
            option_id,
        });
    }

    fn apply_options(&self, options: SessionOptions) -> bool {
        // The model has a setter; the permission posture is a launch flag, and
        // changing what a running agent may touch deserves a fresh session.
        if options.mode != self.mode || options.interaction_mode != self.interaction_mode {
            return false;
        }
        self.commands.send(CommandMessage::Options(options)).is_ok()
    }

    fn rollback(&self, _turns: usize) -> anyhow::Result<Option<ProviderResumeCursor>> {
        Err(anyhow!(
            "conversation rollback is not supported by this provider transport"
        ))
    }
}

impl Drop for ClaudeDriver {
    fn drop(&mut self) {
        let _ = self.commands.send(CommandMessage::Shutdown);
    }
}

fn write_line(writer: &mut impl Write, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[derive(Default)]
struct ClaudeStreamState {
    saw_text_delta: bool,
    saw_reasoning_delta: bool,
    tools: HashMap<String, (ActivityKind, String)>,
}

#[allow(clippy::too_many_arguments)]
fn handle_message(
    value: &Value,
    session_id: &str,
    events: &Sender<DriverEvent>,
    commands: &Sender<CommandMessage>,
    turn_active: &Mutex<bool>,
    auto_approve: bool,
    state: &mut ClaudeStreamState,
) {
    match value.get("type").and_then(Value::as_str) {
        Some("system") => {
            // The init handshake carries the CLI's own command registry —
            // built-ins, custom commands, plugins and skills alike.
            if value.get("subtype").and_then(Value::as_str) == Some("init")
                && let Some(commands) = value.get("slash_commands").and_then(Value::as_array)
            {
                let commands = commands
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|name| crate::model::ReportedCommand {
                        name: name.to_owned(),
                        description: String::new(),
                    })
                    .collect::<Vec<_>>();
                if !commands.is_empty() {
                    let _ = events.send(DriverEvent::AvailableCommands(commands));
                }
            }
        }
        Some("control_request") => {
            if value.pointer("/request/subtype").and_then(Value::as_str) == Some("can_use_tool") {
                request_permission(value, events, commands, auto_approve);
            }
        }
        Some("stream_event") => {
            let event = value.get("event").unwrap_or(&Value::Null);
            // Each assistant message re-arms the delta fallback.
            if event.get("type").and_then(Value::as_str) == Some("message_start") {
                state.saw_text_delta = false;
                state.saw_reasoning_delta = false;
            }
            let delta = event.get("delta").unwrap_or(&Value::Null);
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => {
                    if let Some(text) = delta
                        .get("text")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                    {
                        state.saw_text_delta = true;
                        let _ = events.send(DriverEvent::TextDelta(text.to_owned()));
                    }
                }
                Some("thinking_delta") => {
                    if let Some(text) = delta
                        .get("thinking")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                    {
                        state.saw_reasoning_delta = true;
                        let _ = events.send(DriverEvent::ReasoningDelta(text.to_owned()));
                    }
                }
                _ => {}
            }
        }
        Some("assistant") => {
            let Some(content) = value.pointer("/message/content").and_then(Value::as_array) else {
                return;
            };
            for block in content {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") if !state.saw_text_delta => {
                        if let Some(text) = block
                            .get("text")
                            .and_then(Value::as_str)
                            .filter(|text| !text.is_empty())
                        {
                            let _ = events.send(DriverEvent::TextDelta(text.to_owned()));
                        }
                    }
                    Some("thinking") if !state.saw_reasoning_delta => {
                        if let Some(text) = block
                            .get("thinking")
                            .and_then(Value::as_str)
                            .filter(|text| !text.is_empty())
                        {
                            let _ = events.send(DriverEvent::ReasoningDelta(text.to_owned()));
                        }
                    }
                    Some("tool_use") => {
                        let id = block.get("id").and_then(Value::as_str).map(str::to_owned);
                        let wire_title = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("Tool")
                            .to_owned();
                        let kind = super::support::classify_tool(&wire_title);
                        let title =
                            activity::input_title(block.get("input")).unwrap_or(wire_title);
                        if let Some(id) = &id {
                            state.tools.insert(id.clone(), (kind, title.clone()));
                        }
                        let _ = events.send(DriverEvent::RichActivity(activity::tool_activity(
                            id,
                            kind,
                            title,
                            block.get("input"),
                            None,
                            None,
                            false,
                            false,
                        )));
                    }
                    _ => {}
                }
            }
        }
        Some("user") => {
            // `--replay-user-messages` echoes Waku's own prompts back; they are
            // an acknowledgement, not transcript content.
            if value.get("isReplay").and_then(Value::as_bool) == Some(true) {
                return;
            }
            let Some(content) = value.pointer("/message/content").and_then(Value::as_array) else {
                return;
            };
            for block in content {
                if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                    continue;
                }
                let id = block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let (kind, title) = id
                    .as_ref()
                    .and_then(|id| state.tools.remove(id))
                    .unwrap_or((ActivityKind::Tool, "Tool".to_owned()));
                let failed = block.get("is_error").and_then(Value::as_bool) == Some(true);
                let _ = events.send(DriverEvent::RichActivity(activity::tool_activity(
                    id,
                    kind,
                    title,
                    None,
                    block.get("content"),
                    block.get("content"),
                    failed,
                    true,
                )));
            }
        }
        Some("result") => {
            let failed = value.get("is_error").and_then(Value::as_bool) == Some(true);
            if failed && let Some(result) = value.get("result").and_then(Value::as_str) {
                let _ = events.send(DriverEvent::Error(result.to_owned()));
            }
            if !std::mem::take(&mut *turn_active.lock()) {
                return;
            }
            // Claude's own transcript is where a rewind checkpoint comes from,
            // and it is only complete once the turn is.
            if let Ok(Some(message_id)) = crate::claude_session::latest_message_id(session_id) {
                let _ = events.send(DriverEvent::Connected {
                    provider_cursor: Some(ProviderResumeCursor::Claude {
                        session_id: session_id.to_owned(),
                        resume_at: Some(message_id),
                    }),
                });
            }
            let _ = events.send(DriverEvent::TurnFinished {
                success: !failed,
                summary: None,
            });
        }
        // `system` status/thinking-token notices and `rate_limit_event` are not
        // transcript content.
        _ => {}
    }
}

fn request_permission(
    value: &Value,
    events: &Sender<DriverEvent>,
    commands: &Sender<CommandMessage>,
    auto_approve: bool,
) {
    let Some(request_id) = value.get("request_id").and_then(Value::as_str) else {
        return;
    };
    if auto_approve {
        let _ = commands.send(CommandMessage::Respond {
            request_id: request_id.to_owned(),
            option_id: "allow".into(),
        });
        return;
    }

    let request = value.get("request").unwrap_or(&Value::Null);
    let tool = request
        .get("display_name")
        .or_else(|| request.get("tool_name"))
        .and_then(Value::as_str)
        .unwrap_or("a tool");
    // The agent says why it is asking; that reason is what the answer rests on.
    let detail = request
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            request
                .get("blocked_path")
                .and_then(Value::as_str)
                .map(|path| format!("Blocked path: {path}"))
        })
        .unwrap_or_else(|| format!("The agent wants to run {tool}."));
    let _ = events.send(DriverEvent::Permission {
        request_id: request_id.to_owned(),
        title: activity::input_title(request.get("input"))
            .unwrap_or_else(|| format!("Run {tool}?")),
        detail,
        options: vec![
            PermissionOption {
                id: "allow".into(),
                label: "Allow once".into(),
                allow: true,
            },
            PermissionOption {
                id: "deny".into(),
                label: "Deny".into(),
                allow: false,
            },
        ],
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn harness() -> (
        Sender<DriverEvent>,
        crossbeam_channel::Receiver<DriverEvent>,
        Sender<CommandMessage>,
        crossbeam_channel::Receiver<CommandMessage>,
        Mutex<bool>,
        ClaudeStreamState,
    ) {
        let (events, event_rx) = unbounded();
        let (commands, command_rx) = unbounded();
        (
            events,
            event_rx,
            commands,
            command_rx,
            Mutex::new(true),
            ClaudeStreamState::default(),
        )
    }

    /// Drives the real CLI through the actual driver, including a second turn
    /// on the same process — the whole point of the transport. Ignored by
    /// default: needs the CLI installed, credentials, and the network.
    #[test]
    #[ignore = "requires an installed, authenticated claude"]
    fn claude_streaming_session_against_the_real_cli() {
        let binary =
            crate::command_env::find_executable("claude").expect("claude is not installed");
        let (events, event_rx) = unbounded();
        let driver = ClaudeDriver::start(
            DriverStartOptions {
                binary,
                cwd: std::env::temp_dir(),
                mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Build,
                model: None,
                reasoning_effort: None,
                service_tier: None,
                computer_use_enabled: false,
                provider_cursor: None,
            },
            events,
        )
        .expect("the streaming session should start");

        assert!(matches!(
            event_rx
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("the driver should report its session"),
            DriverEvent::Connected {
                provider_cursor: Some(ProviderResumeCursor::Claude { .. })
            }
        ));

        let mut collect = |driver: &ClaudeDriver, prompt: &str| -> String {
            driver.prompt(prompt.to_owned());
            let mut text = String::new();
            while let Ok(event) = event_rx.recv_timeout(std::time::Duration::from_secs(180)) {
                match event {
                    DriverEvent::TextDelta(delta) => text.push_str(&delta),
                    DriverEvent::TurnFinished { success, .. } => {
                        assert!(success, "the turn should settle successfully");
                        return text;
                    }
                    DriverEvent::Error(error) => panic!("the CLI reported: {error}"),
                    _ => {}
                }
            }
            panic!("the turn never settled");
        };

        let first = collect(&driver, "Reply with exactly: BANANA. Use no tools.");
        assert!(first.contains("BANANA"), "expected a reply, got {first:?}");

        // The second turn proves one process is serving the conversation and
        // kept its context — a per-turn spawn could not answer this.
        let second = collect(
            &driver,
            "What word did I just ask you to reply with? Answer with that word only.",
        );
        assert!(
            second.contains("BANANA"),
            "the session should retain context across turns, got {second:?}"
        );
    }

    #[test]
    fn access_modes_map_to_claude_permission_modes() {
        assert_eq!(
            permission_mode(RuntimeMode::Ask, InteractionMode::Build),
            "default"
        );
        assert_eq!(
            permission_mode(RuntimeMode::AutoAcceptEdits, InteractionMode::Build),
            "acceptEdits"
        );
        assert_eq!(
            permission_mode(RuntimeMode::FullAccess, InteractionMode::Build),
            "bypassPermissions"
        );
        assert_eq!(
            permission_mode(RuntimeMode::FullAccess, InteractionMode::Plan),
            "plan"
        );
    }

    #[test]
    fn streams_text_and_tools_and_ignores_its_own_replayed_prompt() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        // Payloads copied from a live streaming-input session.
        let wire = [
            json!({"type":"system","subtype":"init","session_id":"s","tools":[]}),
            // Waku's own prompt, echoed by --replay-user-messages.
            json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":"go"}]},"isReplay":true}),
            json!({"type":"stream_event","event":{"type":"message_start","message":{"role":"assistant"}}}),
            json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"pondering"}}}),
            json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"I'll run that."}}}),
            json!({"type":"assistant","message":{"content":[
                {"type":"text","text":"I'll run that."},
                {"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"echo hi"}}
            ]}}),
            json!({"type":"user","message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":"toolu_1","content":"hi","is_error":false}
            ]}}),
            json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}),
        ];
        for message in wire {
            handle_message(
                &message, "s", &events, &commands, &turn, true, &mut state,
            );
        }

        let mut seen = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            seen.push(event);
        }
        assert!(matches!(&seen[0], DriverEvent::ReasoningDelta(t) if t == "pondering"));
        assert!(matches!(&seen[1], DriverEvent::TextDelta(t) if t == "I'll run that."));
        // The completed assistant block must not repeat the streamed text.
        assert!(
            matches!(&seen[2], DriverEvent::RichActivity(item)
                if item.kind == ActivityKind::Command && !item.complete)
        );
        assert!(
            matches!(&seen[3], DriverEvent::RichActivity(item)
                if item.complete && item.output.as_deref() == Some("hi"))
        );
        assert_eq!(seen.len(), 4, "replayed prompt or control noise leaked");
    }

    #[test]
    fn supervised_mode_asks_the_user_and_auto_modes_answer_themselves() {
        let (events, event_rx, commands, command_rx, turn, mut state) = harness();
        // Shape observed from a live `--permission-prompt-tool stdio` session.
        let request = json!({
            "type": "control_request",
            "request_id": "fa01120e",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "Bash",
                "display_name": "Bash",
                "input": {"command": "echo hi", "description": "Write probe file"},
                "description": "Write probe file",
                "blocked_path": "/tmp/probe.txt",
                "tool_use_id": "toolu_1"
            }
        });

        handle_message(
            &request, "s", &events, &commands, &turn, false, &mut state,
        );
        let DriverEvent::Permission {
            request_id,
            detail,
            options,
            ..
        } = event_rx.try_recv().unwrap()
        else {
            panic!("Supervised mode must surface the request to the user");
        };
        assert_eq!(request_id, "fa01120e");
        assert_eq!(detail, "Write probe file");
        assert_eq!(
            options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
            ["allow", "deny"]
        );
        assert!(command_rx.try_recv().is_err());

        handle_message(&request, "s", &events, &commands, &turn, true, &mut state);
        let Ok(CommandMessage::Respond { option_id, .. }) = command_rx.try_recv() else {
            panic!("auto modes must answer without the user");
        };
        assert_eq!(option_id, "allow");
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn the_result_message_settles_the_turn_exactly_once() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        let result = json!({"type":"result","is_error":false,"stop_reason":"end_turn"});

        handle_message(&result, "s", &events, &commands, &turn, true, &mut state);
        // The checkpoint read is best-effort; the turn must settle regardless.
        let settled = std::iter::from_fn(|| event_rx.try_recv().ok())
            .filter(|event| matches!(event, DriverEvent::TurnFinished { .. }))
            .count();
        assert_eq!(settled, 1);
        assert!(!*turn.lock());

        handle_message(&result, "s", &events, &commands, &turn, true, &mut state);
        assert!(
            !std::iter::from_fn(|| event_rx.try_recv().ok())
                .any(|event| matches!(event, DriverEvent::TurnFinished { .. })),
            "a second result must not settle an already-finished turn"
        );
    }
}
