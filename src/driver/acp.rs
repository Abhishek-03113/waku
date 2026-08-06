//! Agent Client Protocol transport.
//!
//! ACP is a bidirectional JSON-RPC session over stdio: one agent process serves
//! the whole conversation, streams `session/update` notifications, and asks the
//! client for tool permission with a real request it expects an answer to. That
//! makes it the only transport besides Codex's app-server where Waku's
//! Supervised mode means what it says.
//!
//! The payload shapes here were read off live agents (`opencode acp`,
//! `grok agent stdio`), not the specification alone.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::Stdio;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use crossbeam_channel::{Sender, bounded, unbounded};
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::activity;
use crate::driver::{DriverControl, DriverStartOptions, SessionOptions};
use crate::model::{
    ActivityKind, DriverEvent, InteractionMode, PermissionOption, ProviderKind,
    ProviderResumeCursor, RuntimeMode,
};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const PROTOCOL_VERSION: u64 = 1;

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

type PendingResponses = Arc<Mutex<HashMap<u64, Sender<Result<Value, String>>>>>;

pub struct AcpDriver {
    commands: Sender<CommandMessage>,
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
    computer_use: Option<super::support::HeadlessComputerUseRuntime>,
}

/// Per-provider launch details. Everything below this is protocol, not provider.
struct AcpLaunch {
    args: Vec<String>,
    env: Vec<(String, String)>,
}

fn launch_for(provider: ProviderKind) -> anyhow::Result<AcpLaunch> {
    match provider {
        ProviderKind::Cursor => Ok(AcpLaunch {
            args: vec!["acp".into()],
            env: Vec::new(),
        }),
        ProviderKind::Grok => Ok(AcpLaunch {
            args: vec!["agent".into(), "stdio".into()],
            env: vec![("GROK_OAUTH2_REFERRER".into(), "waku".into())],
        }),
        ProviderKind::OpenCode => Ok(AcpLaunch {
            args: vec!["acp".into()],
            env: Vec::new(),
        }),
        _ => Err(anyhow!(
            "{} does not speak the Agent Client Protocol",
            provider.display_name()
        )),
    }
}

impl AcpDriver {
    pub fn start(
        provider: ProviderKind,
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
            computer_use_enabled,
            provider_cursor,
        } = options;
        // A Cursor branch has no native session yet: its retained history is
        // carried in the cursor and replayed in the first prompt.
        let fork_context = match &provider_cursor {
            Some(ProviderResumeCursor::Cursor { fork_context, .. }) => fork_context.clone(),
            _ => None,
        };
        let resume_session_id = match provider_cursor {
            Some(cursor) if cursor.provider() == provider => {
                let id = cursor.native_id();
                (!id.is_empty()).then(|| id.to_owned())
            }
            Some(cursor) => {
                return Err(anyhow!(
                    "cannot resume {} from a {} cursor",
                    provider.display_name(),
                    cursor.provider().display_name()
                ));
            }
            None => None,
        };
        let launch = launch_for(provider)?;

        // Grok's Computer Use support is an isolated GROK_HOME plus a rules
        // argument, and it is transport-independent — the ACP session needs the
        // same setup the headless turns had.
        let computer_use = (provider == ProviderKind::Grok && computer_use_enabled)
            .then(|| super::support::HeadlessComputerUseRuntime::start(provider, events.clone()))
            .transpose()?;

        let mut command = crate::command_env::command(&binary);
        command.args(&launch.args).current_dir(&cwd);
        for (name, value) in &launch.env {
            command.env(name, value);
        }
        super::support::configure_grok_computer_use_command(
            &mut command,
            computer_use.as_ref().map(|runtime| &runtime.config),
        );
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
                format!("failed to start {} in ACP mode", provider.display_name())
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("{} stdin unavailable", provider.display_name()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("{} stdout unavailable", provider.display_name()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("{} stderr unavailable", provider.display_name()))?;

        let (commands, command_rx) = unbounded();
        let pending: PendingResponses = Arc::new(Mutex::new(HashMap::new()));
        // The prompt request stays open for the whole turn, so it is tracked
        // apart from the blocking request table: the writer must stay free to
        // send a cancel while it is outstanding.
        let prompt_request = Arc::new(Mutex::new(None::<u64>));
        let auto_approve = mode != RuntimeMode::Ask;

        let reader_pending = pending.clone();
        let reader_prompt = prompt_request.clone();
        let reader_commands = commands.clone();
        let reader_events = events.clone();
        let reader_thread = thread::Builder::new()
            .name(format!("waku-{}-acp-reader", provider.id()))
            .spawn(move || {
                let mut state = AcpStreamState::default();
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let Ok(value) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    handle_message(
                        value,
                        &reader_pending,
                        &reader_prompt,
                        &reader_commands,
                        &reader_events,
                        auto_approve,
                        &mut state,
                    );
                }
                fail_pending(&reader_pending, "the ACP agent exited");
            })?;

        let writer_pending = pending.clone();
        let writer_prompt = prompt_request;
        let writer_events = events.clone();
        let provider_name = provider.display_name();
        thread::Builder::new()
            .name(format!("waku-{}-acp-writer", provider.id()))
            .spawn(move || {
                let mut stdin = stdin;
                let mut next_id = 0_u64;
                let session = (|| -> Result<String, String> {
                    let initialize = request(
                        &mut stdin,
                        &writer_pending,
                        &mut next_id,
                        "initialize",
                        json!({
                            "protocolVersion": PROTOCOL_VERSION,
                            "clientCapabilities": {
                                // Waku does not proxy the agent's file or
                                // terminal access, so it claims neither. An
                                // advertised capability the client cannot honor
                                // strands the agent mid-tool-call.
                                "fs": {"readTextFile": false, "writeTextFile": false},
                                "terminal": false
                            }
                        }),
                    )?;
                    let can_load = initialize
                        .pointer("/result/agentCapabilities/loadSession")
                        .and_then(Value::as_bool)
                        == Some(true);
                    let new_session = json!({"cwd": cwd, "mcpServers": []});
                    let opened = match resume_session_id.as_deref() {
                        Some(session_id) if can_load => {
                            let loaded = request(
                                &mut stdin,
                                &writer_pending,
                                &mut next_id,
                                "session/load",
                                json!({
                                    "sessionId": session_id,
                                    "cwd": cwd,
                                    "mcpServers": []
                                }),
                            );
                            match loaded {
                                // A resume that the agent no longer recognizes
                                // must not strand the task: start fresh and let
                                // the new session id replace the stale cursor.
                                Ok(_) => return Ok(session_id.to_owned()),
                                Err(_) => request(
                                    &mut stdin,
                                    &writer_pending,
                                    &mut next_id,
                                    "session/new",
                                    new_session,
                                )?,
                            }
                        }
                        _ => request(
                            &mut stdin,
                            &writer_pending,
                            &mut next_id,
                            "session/new",
                            new_session,
                        )?,
                    };
                    let session_id = opened
                        .pointer("/result/sessionId")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| "the ACP agent returned no session id".to_owned())?;
                    if let Some(mode) = desired_mode(&opened, mode, interaction_mode) {
                        // Not fatal: an agent without the mode still runs, it
                        // just runs with its default posture.
                        let _ = request(
                            &mut stdin,
                            &writer_pending,
                            &mut next_id,
                            "session/set_mode",
                            json!({"sessionId": session_id, "modeId": mode}),
                        );
                    }
                    Ok(session_id)
                })();

                let session_id = match session {
                    Ok(session_id) => session_id,
                    Err(error) => {
                        let _ = writer_events.send(DriverEvent::Error(format!(
                            "Could not open a {provider_name} session: {error}"
                        )));
                        let _ = writer_events.send(DriverEvent::TurnFinished {
                            success: false,
                            summary: Some(format!("{provider_name} could not start a session.")),
                        });
                        return;
                    }
                };
                let _ = writer_events.send(DriverEvent::Connected {
                    provider_cursor: Some(ProviderResumeCursor::from_session_id(
                        provider,
                        session_id.clone(),
                    )),
                });

                let mut fork_context = fork_context;
                let mut current_model = model;
                apply_model(
                    &mut stdin,
                    &writer_pending,
                    &mut next_id,
                    &session_id,
                    current_model.as_deref(),
                    reasoning_effort.as_deref(),
                    &writer_events,
                );

                while let Ok(message) = command_rx.recv() {
                    match message {
                        CommandMessage::Prompt(text) => {
                            let text = fork_context
                                .take()
                                .map(|context| {
                                    crate::cursor_session::prompt_with_fork_context(&context, &text)
                                })
                                .unwrap_or(text);
                            let _ = writer_events.send(DriverEvent::TurnStarted);
                            next_id += 1;
                            *writer_prompt.lock() = Some(next_id);
                            let sent = write_line(
                                &mut stdin,
                                &json!({
                                    "jsonrpc": "2.0",
                                    "id": next_id,
                                    "method": "session/prompt",
                                    "params": {
                                        "sessionId": session_id,
                                        "prompt": [{"type": "text", "text": text}]
                                    }
                                }),
                            );
                            if let Err(error) = sent {
                                *writer_prompt.lock() = None;
                                let _ = writer_events.send(DriverEvent::Error(format!(
                                    "{provider_name} transport write failed: {error}"
                                )));
                                let _ = writer_events.send(DriverEvent::TurnFinished {
                                    success: false,
                                    summary: Some(format!(
                                        "{provider_name} could not receive the prompt."
                                    )),
                                });
                            }
                        }
                        CommandMessage::Cancel => {
                            // A notification, not a request: the outstanding
                            // `session/prompt` is what reports the cancellation.
                            let _ = write_line(
                                &mut stdin,
                                &json!({
                                    "jsonrpc": "2.0",
                                    "method": "session/cancel",
                                    "params": {"sessionId": session_id}
                                }),
                            );
                        }
                        CommandMessage::Respond {
                            request_id,
                            option_id,
                        } => {
                            let Ok(id) = request_id.parse::<u64>() else {
                                continue;
                            };
                            let _ = write_line(
                                &mut stdin,
                                &json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {
                                        "outcome": {
                                            "outcome": "selected",
                                            "optionId": option_id
                                        }
                                    }
                                }),
                            );
                        }
                        CommandMessage::Options(options) => {
                            if options.model != current_model {
                                current_model = options.model;
                                apply_model(
                                    &mut stdin,
                                    &writer_pending,
                                    &mut next_id,
                                    &session_id,
                                    current_model.as_deref(),
                                    options.reasoning_effort.as_deref(),
                                    &writer_events,
                                );
                            }
                        }
                        CommandMessage::Shutdown => break,
                    }
                }
            })?;

        let last_visible_stderr = Arc::new(Mutex::new(None::<String>));
        let stderr_last_error = last_visible_stderr.clone();
        let stderr_events = events.clone();
        let stderr_thread = thread::Builder::new()
            .name(format!("waku-{}-acp-stderr", provider.id()))
            .spawn(move || {
                let lines = BufReader::new(stderr)
                    .lines()
                    .map_while(Result::ok)
                    .filter(|line| !line.trim().is_empty())
                    .collect::<Vec<_>>();
                if let Some(message) = super::support::provider_stderr_error(lines) {
                    let error = format!("{provider_name}: {message}");
                    *stderr_last_error.lock() = Some(error.clone());
                    let _ = stderr_events.send(DriverEvent::Error(error));
                }
            })?;

        thread::Builder::new()
            .name(format!("waku-{}-acp-process", provider.id()))
            .spawn(move || {
                let status = child.wait();
                let _ = reader_thread.join();
                let _ = stderr_thread.join();
                if let Ok(status) = status
                    && !status.success()
                    && last_visible_stderr.lock().is_none()
                {
                    let _ = events.send(DriverEvent::Error(format!(
                        "{provider_name} exited with {status}"
                    )));
                }
                let _ = events.send(DriverEvent::ProcessExited);
            })?;

        Ok(Self {
            commands,
            mode,
            interaction_mode,
            computer_use,
        })
    }
}

impl DriverControl for AcpDriver {
    fn prompt(&self, prompt: String) {
        let _ = self.commands.send(CommandMessage::Prompt(prompt));
    }

    fn cancel(&self) {
        let _ = self.commands.send(CommandMessage::Cancel);
    }

    fn cancel_computer_use(&self) {
        if let Some(computer_use) = self.computer_use.as_ref() {
            computer_use.stop();
        }
    }

    fn respond(&self, request_id: String, option_id: String) {
        let _ = self.commands.send(CommandMessage::Respond {
            request_id,
            option_id,
        });
    }

    fn apply_options(&self, options: SessionOptions) -> bool {
        // The permission posture is decided when the session opens, because it
        // is what decides whether approvals reach the user at all.
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

impl Drop for AcpDriver {
    fn drop(&mut self) {
        self.cancel_computer_use();
        let _ = self.commands.send(CommandMessage::Shutdown);
    }
}

/// Picks the agent's own mode id for Waku's interaction mode.
///
/// Only Plan maps here. Supervised deliberately stays in the agent mode: ACP's
/// read-only "ask" mode answers questions instead of asking permission, whereas
/// Supervised means the agent still acts — it just checks first, which is what
/// `session/request_permission` already does.
fn desired_mode(
    opened: &Value,
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
) -> Option<String> {
    if interaction_mode != InteractionMode::Plan && mode != RuntimeMode::Plan {
        return None;
    }
    let modes = opened.pointer("/result/modes")?;
    let available = modes.get("availableModes").and_then(Value::as_array)?;
    let plan = available.iter().find_map(|entry| {
        let id = entry.get("id").and_then(Value::as_str)?;
        id.eq_ignore_ascii_case("plan").then(|| id.to_owned())
    })?;
    (modes.get("currentModeId").and_then(Value::as_str) != Some(plan.as_str())).then_some(plan)
}

fn apply_model(
    stdin: &mut impl Write,
    pending: &PendingResponses,
    next_id: &mut u64,
    session_id: &str,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
    events: &Sender<DriverEvent>,
) {
    let Some(model) = model else {
        return;
    };
    if let Err(error) = request(
        stdin,
        pending,
        next_id,
        "session/set_model",
        json!({"sessionId": session_id, "modelId": model}),
    ) {
        let _ = events.send(DriverEvent::Error(format!(
            "Could not select the model: {error}"
        )));
        return;
    }
    // Reasoning effort is a session config option rather than a first-class
    // field, and not every agent offers one; a rejection is not turn-fatal.
    if let Some(effort) = reasoning_effort {
        let _ = request(
            stdin,
            pending,
            next_id,
            "session/set_config_option",
            json!({"sessionId": session_id, "configId": "mode", "value": effort}),
        );
    }
}

fn request(
    stdin: &mut impl Write,
    pending: &PendingResponses,
    next_id: &mut u64,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    *next_id += 1;
    let id = *next_id;
    let (response_tx, response_rx) = bounded(1);
    pending.lock().insert(id, response_tx);
    let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    if let Err(error) = write_line(stdin, &message) {
        pending.lock().remove(&id);
        return Err(format!("transport write failed: {error}"));
    }
    match response_rx.recv_timeout(HANDSHAKE_TIMEOUT) {
        Ok(response) => response,
        Err(_) => {
            pending.lock().remove(&id);
            Err(format!("{method} timed out"))
        }
    }
}

fn write_line(writer: &mut impl Write, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn fail_pending(pending: &PendingResponses, message: &str) {
    for (_, response) in pending.lock().drain() {
        let _ = response.send(Err(message.to_owned()));
    }
}

#[derive(Default)]
struct AcpStreamState {
    tools: HashMap<String, (ActivityKind, String)>,
}

#[allow(clippy::too_many_arguments)]
fn handle_message(
    value: Value,
    pending: &PendingResponses,
    prompt_request: &Mutex<Option<u64>>,
    commands: &Sender<CommandMessage>,
    events: &Sender<DriverEvent>,
    auto_approve: bool,
    state: &mut AcpStreamState,
) {
    let id = value.get("id").and_then(Value::as_u64);
    let method = value.get("method").and_then(Value::as_str);

    // A message with an id and no method is a reply to something Waku sent.
    if let Some(id) = id
        && method.is_none()
    {
        if *prompt_request.lock() == Some(id) {
            *prompt_request.lock() = None;
            finish_turn(&value, events);
            return;
        }
        let result = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map_or_else(|| Ok(value.clone()), |error| Err(error.to_owned()));
        if let Some(response) = pending.lock().remove(&id) {
            let _ = response.send(result);
        }
        return;
    }

    let Some(method) = method else {
        return;
    };
    let params = value.get("params").cloned().unwrap_or(Value::Null);

    // An id alongside a method makes this the agent asking Waku for something.
    if let Some(id) = id {
        if method == "session/request_permission" {
            request_permission(id, &params, commands, events, auto_approve);
        }
        return;
    }

    if method != "session/update" {
        // Everything else on this channel is agent-private control traffic
        // (`_x.ai/*` and friends). It must never reach the transcript.
        return;
    }
    let update = params.get("update").unwrap_or(&Value::Null);
    match update.get("sessionUpdate").and_then(Value::as_str) {
        Some("agent_message_chunk") => {
            if let Some(text) = update
                .pointer("/content/text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                let _ = events.send(DriverEvent::TextDelta(text.to_owned()));
            }
        }
        Some("agent_thought_chunk") => {
            if let Some(text) = update
                .pointer("/content/text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                let _ = events.send(DriverEvent::ReasoningDelta(text.to_owned()));
            }
        }
        Some("tool_call" | "tool_call_update") => {
            tool_activity(update, events, state);
        }
        Some("plan") => {
            let _ = events.send(DriverEvent::Activity {
                id: Some("acp-plan".into()),
                kind: ActivityKind::Plan,
                title: "Plan updated".into(),
                detail: None,
                complete: false,
            });
        }
        // `user_message_chunk` is Waku's own prompt echoed back, and
        // `usage_update` / `available_commands_update` / `session_info_update`
        // are not transcript content.
        _ => {}
    }
}

fn finish_turn(value: &Value, events: &Sender<DriverEvent>) {
    if let Some(error) = value
        .pointer("/error/data/message")
        .or_else(|| value.pointer("/error/message"))
        .and_then(Value::as_str)
    {
        let _ = events.send(DriverEvent::Error(error.to_owned()));
        let _ = events.send(DriverEvent::TurnFinished {
            success: false,
            summary: None,
        });
        return;
    }
    let stop_reason = value
        .pointer("/result/stopReason")
        .and_then(Value::as_str)
        .unwrap_or("end_turn");
    let _ = events.send(DriverEvent::TurnFinished {
        success: matches!(stop_reason, "end_turn" | "cancelled"),
        summary: match stop_reason {
            "end_turn" | "cancelled" => None,
            "max_tokens" => Some("The agent ran out of context for this turn.".into()),
            "refusal" => Some("The agent declined this turn.".into()),
            other => Some(format!("The agent stopped: {other}.")),
        },
    });
}

fn request_permission(
    id: u64,
    params: &Value,
    commands: &Sender<CommandMessage>,
    events: &Sender<DriverEvent>,
    auto_approve: bool,
) {
    let options = params
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|option| {
                    let id = option.get("optionId").and_then(Value::as_str)?;
                    let kind = option.get("kind").and_then(Value::as_str).unwrap_or_default();
                    Some(PermissionOption {
                        id: id.to_owned(),
                        label: option
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or(id)
                            .to_owned(),
                        allow: kind.starts_with("allow"),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if auto_approve {
        // Outside Supervised the user has already answered this question once,
        // for the whole session. Prefer the durable allow so the agent stops
        // asking for the same tool.
        let choice = options
            .iter()
            .find(|option| option.allow && option.id.contains("always"))
            .or_else(|| options.iter().find(|option| option.allow));
        if let Some(choice) = choice {
            let _ = commands.send(CommandMessage::Respond {
                request_id: id.to_string(),
                option_id: choice.id.clone(),
            });
        }
        return;
    }

    let title = params
        .pointer("/toolCall/title")
        .and_then(Value::as_str)
        .unwrap_or("Run a tool")
        .to_owned();
    // The agent explains why it is asking — "Not in allowlist: cat, pwd" — and
    // that reason is the whole basis for the user's decision. Only fall back to
    // the tool kind when it says nothing.
    let detail = permission_reason(params).unwrap_or_else(|| {
        params
            .pointer("/toolCall/kind")
            .and_then(Value::as_str)
            .map(|kind| format!("The agent wants to {kind}."))
            .unwrap_or_else(|| "The agent is asking for permission.".to_owned())
    });
    let _ = events.send(DriverEvent::Permission {
        request_id: id.to_string(),
        title,
        detail,
        options,
    });
}

/// Pulls the agent's own explanation out of a permission request's tool call.
///
/// ACP nests it as `content: [{type: "content", content: {type: "text", …}}]`,
/// and agents also emit the inner shape directly, so both are accepted.
fn permission_reason(params: &Value) -> Option<String> {
    let content = params
        .pointer("/toolCall/content")
        .and_then(Value::as_array)?;
    let reason = content
        .iter()
        .filter_map(|entry| {
            entry
                .pointer("/content/text")
                .or_else(|| entry.get("text"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
        })
        .collect::<Vec<_>>()
        .join("\n");
    (!reason.is_empty()).then(|| truncate(&reason, 400))
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    text.chars()
        .take(max_chars)
        .chain(std::iter::once('…'))
        .collect()
}

fn tool_activity(update: &Value, events: &Sender<DriverEvent>, state: &mut AcpStreamState) {
    let id = update
        .get("toolCallId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let status = update
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending");
    let complete = matches!(status, "completed" | "failed");
    let failed = status == "failed";

    let wire_kind = update.get("kind").and_then(Value::as_str);
    let wire_title = update.get("title").and_then(Value::as_str);
    let stored = id.as_ref().and_then(|id| {
        if complete {
            state.tools.remove(id)
        } else {
            state.tools.get(id).cloned()
        }
    });
    let kind = wire_kind
        .map(classify)
        .or_else(|| stored.as_ref().map(|(kind, _)| *kind))
        .unwrap_or(ActivityKind::Tool);
    let arguments = update.get("rawInput").filter(|value| !value.is_null());
    let title = activity::input_title(arguments)
        .or_else(|| {
            wire_title
                .filter(|title| !title.is_empty())
                .map(str::to_owned)
        })
        .or_else(|| stored.map(|(_, title)| title))
        .unwrap_or_else(|| "Tool".to_owned());
    if !complete && let Some(id) = id.as_ref() {
        state.tools.insert(id.clone(), (kind, title.clone()));
    }

    let output = update
        .get("content")
        .filter(|value| !value.is_null())
        .or_else(|| update.get("rawOutput").filter(|value| !value.is_null()));
    let item = activity::tool_activity(
        id, kind, title, arguments, output, output, failed, complete,
    );
    let _ = events.send(DriverEvent::RichActivity(item));
}

/// ACP names tool kinds in the same vocabulary Grok's headless stream already
/// used, which is why this mapping is shared with it.
fn classify(kind: &str) -> ActivityKind {
    match kind {
        "execute" => ActivityKind::Command,
        "edit" | "delete" | "move" => ActivityKind::FileChange,
        "read" | "search" | "fetch" => ActivityKind::Search,
        "think" => ActivityKind::Reasoning,
        _ => ActivityKind::Tool,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn harness() -> (
        PendingResponses,
        Mutex<Option<u64>>,
        Sender<CommandMessage>,
        crossbeam_channel::Receiver<CommandMessage>,
        Sender<DriverEvent>,
        crossbeam_channel::Receiver<DriverEvent>,
        AcpStreamState,
    ) {
        let (commands, command_rx) = unbounded();
        let (events, event_rx) = unbounded();
        (
            Arc::new(Mutex::new(HashMap::new())),
            Mutex::new(None),
            commands,
            command_rx,
            events,
            event_rx,
            AcpStreamState::default(),
        )
    }

    /// Drives a real agent through the actual driver. Ignored by default: it
    /// needs the CLI installed, credentials, and the network. Run with
    /// `cargo test --bin waku acp_session_against_a_real_agent -- --ignored`.
    #[test]
    #[ignore = "requires an installed, authenticated cursor-agent"]
    fn acp_session_against_a_real_agent() {
        let binary = crate::command_env::find_executable("cursor-agent")
            .expect("cursor-agent is not installed");
        let (events, event_rx) = unbounded();
        let driver = AcpDriver::start(
            ProviderKind::Cursor,
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
        .expect("the ACP session should open");

        let connected = event_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the agent should report its session");
        assert!(matches!(
            connected,
            DriverEvent::Connected {
                provider_cursor: Some(ProviderResumeCursor::Cursor { .. })
            }
        ));

        driver.prompt("Reply with exactly: OK. Do not use any tools.".into());
        let mut text = String::new();
        let mut finished = None;
        while let Ok(event) = event_rx.recv_timeout(Duration::from_secs(120)) {
            match event {
                DriverEvent::TextDelta(delta) => text.push_str(&delta),
                DriverEvent::TurnFinished { success, .. } => {
                    finished = Some(success);
                    break;
                }
                DriverEvent::Error(error) => panic!("the agent reported: {error}"),
                _ => {}
            }
        }
        assert_eq!(finished, Some(true), "the turn should settle successfully");
        assert!(
            text.contains("OK"),
            "expected the reply to stream through, got {text:?}"
        );
    }

    #[test]
    fn streams_text_reasoning_and_correlated_tools_in_wire_order() {
        let (pending, prompt, commands, _command_rx, events, event_rx, mut state) = harness();
        // Payloads copied from a live `opencode acp` session.
        let wire = [
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"thinking"}}}}),
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call","toolCallId":"call_1","title":"read","kind":"read","status":"pending","rawInput":{}}}}),
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call_update","toolCallId":"call_1","status":"completed","title":"fixture.txt","content":[{"type":"content","content":{"type":"text","text":"waku probe fixture"}}]}}}),
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"OK"}}}}),
            // Agent-private control traffic must not reach the transcript.
            json!({"jsonrpc":"2.0","method":"_x.ai/models/update","params":{"currentModelId":"grok-4.5"}}),
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"usage_update","used":9677}}}),
        ];
        for message in wire {
            handle_message(
                message, &pending, &prompt, &commands, &events, true, &mut state,
            );
        }

        let mut seen = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            seen.push(event);
        }
        assert!(matches!(&seen[0], DriverEvent::ReasoningDelta(text) if text == "thinking"));
        assert!(
            matches!(&seen[1], DriverEvent::RichActivity(item)
                if item.kind == ActivityKind::Search && !item.complete)
        );
        assert!(
            matches!(&seen[2], DriverEvent::RichActivity(item)
                if item.complete
                    && item.title == "fixture.txt"
                    && item.output.as_deref().is_some_and(|output| output.contains("waku probe fixture")))
        );
        assert!(matches!(&seen[3], DriverEvent::TextDelta(text) if text == "OK"));
        assert_eq!(seen.len(), 4, "control traffic leaked into the transcript");
    }

    #[test]
    fn supervised_mode_asks_the_user_and_auto_modes_answer_themselves() {
        let (pending, prompt, commands, command_rx, events, event_rx, mut state) = harness();
        let permission = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "session/request_permission",
            "params": {
                "sessionId": "s",
                // Shape observed from a live `cursor-agent acp` approval.
                "toolCall": {
                    "title": "rm -rf build",
                    "kind": "execute",
                    "content": [
                        {"type": "content", "content": {"type": "text", "text": "Not in allowlist: rm"}}
                    ]
                },
                "options": [
                    {"optionId": "allow", "name": "Allow once", "kind": "allow_once"},
                    {"optionId": "allow-always", "name": "Always allow", "kind": "allow_always"},
                    {"optionId": "reject", "name": "Reject", "kind": "reject_once"}
                ]
            }
        });

        handle_message(
            permission.clone(),
            &pending,
            &prompt,
            &commands,
            &events,
            false,
            &mut state,
        );
        let DriverEvent::Permission {
            request_id,
            options,
            detail,
            title,
        } = event_rx.try_recv().unwrap()
        else {
            panic!("Supervised mode must surface the request to the user");
        };
        assert_eq!(request_id, "7");
        assert_eq!(options.iter().filter(|option| option.allow).count(), 2);
        assert_eq!(title, "rm -rf build");
        // The agent's own reason is what the user is actually deciding on.
        assert_eq!(detail, "Not in allowlist: rm");
        assert!(command_rx.try_recv().is_err());

        handle_message(
            permission, &pending, &prompt, &commands, &events, true, &mut state,
        );
        let Ok(CommandMessage::Respond { option_id, .. }) = command_rx.try_recv() else {
            panic!("auto modes must answer without the user");
        };
        assert_eq!(option_id, "allow-always");
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn the_open_prompt_request_settles_the_turn_exactly_once() {
        let (pending, prompt, commands, _command_rx, events, event_rx, mut state) = harness();
        *prompt.lock() = Some(3);

        handle_message(
            json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}),
            &pending,
            &prompt,
            &commands,
            &events,
            true,
            &mut state,
        );

        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::TurnFinished { success: true, .. }
        ));
        assert!(prompt.lock().is_none());
        // A late duplicate must not settle a turn that already ended.
        handle_message(
            json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}),
            &pending,
            &prompt,
            &commands,
            &events,
            true,
            &mut state,
        );
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn a_failed_prompt_reports_the_agent_error_and_fails_the_turn() {
        let (pending, prompt, commands, _command_rx, events, event_rx, mut state) = harness();
        *prompt.lock() = Some(3);

        // Shape observed from `grok agent stdio` with an exhausted balance.
        handle_message(
            json!({"jsonrpc":"2.0","id":3,"error":{"code":-32603,"message":"Internal error","data":{"message":"API error (status 402 Payment Required): Grok Build usage balance exhausted","http_status":402}}}),
            &pending,
            &prompt,
            &commands,
            &events,
            true,
            &mut state,
        );

        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::Error(message) if message.contains("402")
        ));
        assert!(matches!(
            event_rx.try_recv().unwrap(),
            DriverEvent::TurnFinished { success: false, .. }
        ));
    }
}
