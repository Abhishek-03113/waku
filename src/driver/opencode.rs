//! OpenCode's own HTTP server.
//!
//! `opencode serve` is OpenCode's real API: one resident process serves the
//! whole conversation, streams server-sent events, and answers permission
//! requests the user can actually be asked. Waku already started this server
//! for a side-quest — forking a session — while running conversations through
//! one-shot `opencode run` invocations; this drives everything through it.
//!
//! Routes and payload shapes here were read off a live server's OpenAPI
//! document and event stream, not guessed.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::thread;

use anyhow::{Context as _, anyhow};
use crossbeam_channel::{Sender, unbounded};
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::activity;
use crate::driver::{DriverControl, DriverStartOptions, SessionOptions};
use crate::model::{
    ActivityKind, DriverEvent, InteractionMode, PermissionOption, ProviderResumeCursor, RuntimeMode,
};
use crate::opencode_session::{OpenCodeServer, encode_path_segment};

enum CommandMessage {
    Prompt(String),
    Cancel,
    Respond {
        request_id: String,
        option_id: String,
    },
    Shutdown,
}

pub struct OpenCodeDriver {
    server: Arc<OpenCodeServer>,
    commands: Sender<CommandMessage>,
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
    computer_use: Option<super::support::HeadlessComputerUseRuntime>,
}

impl OpenCodeDriver {
    pub fn start(options: DriverStartOptions, events: Sender<DriverEvent>) -> anyhow::Result<Self> {
        let DriverStartOptions {
            binary,
            cwd,
            mode,
            interaction_mode,
            model,
            reasoning_effort: _,
            service_tier: _,
            computer_use_enabled,
            provider_cursor,
        } = options;
        let resume_session_id = match provider_cursor {
            Some(ProviderResumeCursor::OpenCode { session_id }) => {
                (!session_id.is_empty()).then_some(session_id)
            }
            Some(cursor) => {
                return Err(anyhow!(
                    "cannot resume OpenCode from a {} cursor",
                    cursor.provider().display_name()
                ));
            }
            None => None,
        };

        let computer_use = computer_use_enabled
            .then(|| {
                super::support::HeadlessComputerUseRuntime::start(
                    crate::model::ProviderKind::OpenCode,
                    events.clone(),
                )
            })
            .transpose()?;
        // The one-shot path handed Computer Use to OpenCode through the
        // environment; the resident server takes it exactly the same way.
        let environment = computer_use
            .as_ref()
            .map(|runtime| super::support::opencode_computer_use_environment(&runtime.config))
            .unwrap_or_default();
        let server = Arc::new(OpenCodeServer::start_with_env(&binary, &cwd, &environment)?);

        // Reuse the native session when resuming so the conversation, and the
        // cursor already persisted for it, stay the same.
        let session_id = match resume_session_id {
            Some(session_id) => session_id,
            None => {
                let created = server
                    .request("POST", "/session", Some(&json!({})))
                    .context("could not open an OpenCode session")?;
                created
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow!("OpenCode returned no session ID"))?
            }
        };
        let _ = events.send(DriverEvent::Connected {
            provider_cursor: Some(ProviderResumeCursor::OpenCode {
                session_id: session_id.clone(),
            }),
        });

        // The agent decides Plan versus Build, and it is fixed for the life of
        // the server because it is chosen when the session opens.
        let agent = if interaction_mode == InteractionMode::Plan || mode == RuntimeMode::Plan {
            "plan"
        } else {
            "build"
        };
        let _ = server.request(
            "POST",
            &format!("/session/{}/agent", encode_path_segment(&session_id)),
            Some(&json!({"agent": agent})),
        );

        let auto_approve = mode != RuntimeMode::Ask;
        let (commands, command_rx) = unbounded();
        let turn_active = Arc::new(Mutex::new(false));

        let stream_server = server.clone();
        let stream_session = session_id.clone();
        let stream_events = events.clone();
        let stream_commands = commands.clone();
        let stream_turn = turn_active.clone();
        thread::Builder::new()
            .name("waku-opencode-events".into())
            .spawn(move || {
                let mut state = OpenCodeStreamState::default();
                // The server-wide stream, not a per-session one: the scoped
                // route exists only under `/api`, and this server is Waku's
                // alone, so filtering by session id here is enough.
                match open_event_stream(stream_server.port, "/event") {
                    Ok(stream) => {
                        for line in BufReader::new(stream).lines().map_while(Result::ok) {
                            let Some(payload) = line.strip_prefix("data:") else {
                                continue;
                            };
                            let Ok(value) = serde_json::from_str::<Value>(payload.trim()) else {
                                continue;
                            };
                            // Another session's traffic must not reach this
                            // task's transcript.
                            let session = value
                                .pointer("/properties/sessionID")
                                .and_then(Value::as_str);
                            if session.is_some_and(|session| session != stream_session) {
                                continue;
                            }
                            handle_event(
                                &value,
                                &stream_events,
                                &stream_commands,
                                &stream_turn,
                                auto_approve,
                                &mut state,
                            );
                        }
                    }
                    Err(error) => {
                        let _ = stream_events.send(DriverEvent::Error(format!(
                            "Could not read the OpenCode event stream: {error}"
                        )));
                    }
                }
                let _ = stream_events.send(DriverEvent::ProcessExited);
            })?;

        let worker_server = server.clone();
        let worker_session = session_id;
        let worker_events = events;
        let worker_turn = turn_active;
        thread::Builder::new()
            .name("waku-opencode-driver".into())
            .spawn(move || {
                while let Ok(message) = command_rx.recv() {
                    match message {
                        CommandMessage::Prompt(text) => {
                            *worker_turn.lock() = true;
                            let _ = worker_events.send(DriverEvent::TurnStarted);
                            // The prompt request does not return until the turn
                            // ends, so it runs off the command loop — otherwise
                            // a Stop could not be sent while it is outstanding.
                            let server = worker_server.clone();
                            let session = worker_session.clone();
                            let events = worker_events.clone();
                            let turn = worker_turn.clone();
                            let mut body = json!({
                                "parts": [{"type": "text", "text": text}]
                            });
                            if let Some(model) = model.as_deref()
                                && let Some((provider_id, model_id)) = model.split_once('/')
                            {
                                body["model"] =
                                    json!({"providerID": provider_id, "modelID": model_id});
                            }
                            let _ = thread::Builder::new()
                                .name("waku-opencode-turn".into())
                                .spawn(move || {
                                    let path = format!(
                                        "/session/{}/message",
                                        encode_path_segment(&session)
                                    );
                                    if let Err(error) = server.request("POST", &path, Some(&body)) {
                                        let _ = events.send(DriverEvent::Error(format!(
                                            "OpenCode rejected the prompt: {error}"
                                        )));
                                        // `session.idle` never arrives for a
                                        // turn that failed to start, so settle
                                        // it here instead of hanging.
                                        if std::mem::take(&mut *turn.lock()) {
                                            let _ = events.send(DriverEvent::TurnFinished {
                                                success: false,
                                                summary: Some(
                                                    "OpenCode could not start the turn.".into(),
                                                ),
                                            });
                                        }
                                    }
                                });
                        }
                        CommandMessage::Cancel => {
                            let path =
                                format!("/session/{}/abort", encode_path_segment(&worker_session));
                            if let Err(error) = worker_server.request("POST", &path, None) {
                                let _ = worker_events.send(DriverEvent::Error(format!(
                                    "Could not stop OpenCode: {error}"
                                )));
                            }
                        }
                        CommandMessage::Respond {
                            request_id,
                            option_id,
                        } => {
                            let path = format!(
                                "/session/{}/permission/{}/reply",
                                encode_path_segment(&worker_session),
                                encode_path_segment(&request_id)
                            );
                            if let Err(error) = worker_server.request(
                                "POST",
                                &path,
                                Some(&json!({"reply": option_id})),
                            ) {
                                let _ = worker_events.send(DriverEvent::Error(format!(
                                    "Could not answer OpenCode's permission request: {error}"
                                )));
                            }
                        }
                        CommandMessage::Shutdown => break,
                    }
                }
            })?;

        Ok(Self {
            server,
            commands,
            mode,
            interaction_mode,
            computer_use,
        })
    }
}

impl DriverControl for OpenCodeDriver {
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
        // The model rides on each prompt, but the agent is chosen when the
        // session opens, so a mode change needs a fresh server.
        options.mode == self.mode && options.interaction_mode == self.interaction_mode
    }

    fn rollback(&self, _turns: usize) -> anyhow::Result<Option<ProviderResumeCursor>> {
        Err(anyhow!(
            "conversation rollback is not supported by this provider transport"
        ))
    }
}

impl Drop for OpenCodeDriver {
    fn drop(&mut self) {
        self.cancel_computer_use();
        let _ = self.commands.send(CommandMessage::Shutdown);
        // Kill the server explicitly: the event-stream reader holds a handle and
        // only unblocks once the stream closes, so refcounting alone would
        // deadlock and leak the process.
        self.server.shutdown();
    }
}

/// Opens the server-sent event stream and leaves it open.
///
/// The shared request helper reads a whole response before returning, which a
/// stream never finishes doing.
fn open_event_stream(port: u16, path: &str) -> anyhow::Result<TcpStream> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .with_context(|| format!("could not connect to OpenCode on local port {port}"))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\nConnection: keep-alive\r\n\r\n"
    )?;
    stream.flush()?;
    // Skip the response head; every later line is stream payload.
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(anyhow!("OpenCode closed the event stream during setup"));
        }
        if line.trim().is_empty() {
            break;
        }
    }
    stream.set_read_timeout(None)?;
    Ok(stream)
}

#[derive(Default)]
struct OpenCodeStreamState {
    tools: HashMap<String, (ActivityKind, String)>,
}

fn handle_event(
    value: &Value,
    events: &Sender<DriverEvent>,
    commands: &Sender<CommandMessage>,
    turn_active: &Mutex<bool>,
    auto_approve: bool,
    state: &mut OpenCodeStreamState,
) {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let properties = value.get("properties").unwrap_or(&Value::Null);

    match kind {
        "message.part.delta" => {
            let Some(delta) = properties.get("delta").and_then(Value::as_str) else {
                return;
            };
            if delta.is_empty() {
                return;
            }
            match properties.get("field").and_then(Value::as_str) {
                Some("text") => {
                    let _ = events.send(DriverEvent::TextDelta(delta.to_owned()));
                }
                Some("reasoning" | "thinking") => {
                    let _ = events.send(DriverEvent::ReasoningDelta(delta.to_owned()));
                }
                _ => {}
            }
        }
        "message.part.updated" => {
            let part = properties.get("part").unwrap_or(&Value::Null);
            if part.get("type").and_then(Value::as_str) == Some("tool") {
                tool_activity(part, events, state);
            }
        }
        "session.idle" => {
            if std::mem::take(&mut *turn_active.lock()) {
                let _ = events.send(DriverEvent::TurnFinished {
                    success: true,
                    summary: None,
                });
            }
        }
        "session.error" => {
            let message = properties
                .pointer("/error/message")
                .or_else(|| properties.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("OpenCode reported an error");
            let _ = events.send(DriverEvent::Error(message.to_owned()));
        }
        _ if kind.starts_with("permission.") => {
            request_permission(properties, events, commands, auto_approve);
        }
        // `session.created`, `session.updated`, `session.diff`, `message.updated`,
        // and the plugin/catalog/reference chatter are not transcript content.
        _ => {}
    }
}

fn request_permission(
    properties: &Value,
    events: &Sender<DriverEvent>,
    commands: &Sender<CommandMessage>,
    auto_approve: bool,
) {
    // The request is either the properties themselves or nested under a key,
    // and it is identified by its `per`-prefixed ID.
    let request = ["permission", "request", "info"]
        .iter()
        .find_map(|key| properties.get(*key))
        .filter(|value| value.get("id").is_some())
        .unwrap_or(properties);
    let Some(request_id) = request.get("id").and_then(Value::as_str) else {
        return;
    };

    if auto_approve {
        let _ = commands.send(CommandMessage::Respond {
            request_id: request_id.to_owned(),
            // Durable, so the agent stops asking about the same permission.
            option_id: "always".into(),
        });
        return;
    }

    let permission = request
        .get("permission")
        .and_then(Value::as_str)
        .unwrap_or("run a tool");
    let patterns = request
        .get("patterns")
        .and_then(Value::as_array)
        .map(|patterns| {
            patterns
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|patterns| !patterns.is_empty());
    let _ = events.send(DriverEvent::Permission {
        request_id: request_id.to_owned(),
        title: patterns
            .clone()
            .unwrap_or_else(|| format!("Allow {permission}?")),
        detail: match patterns {
            Some(_) => format!("The agent is asking for the {permission} permission."),
            None => "The agent is asking for permission.".into(),
        },
        options: vec![
            PermissionOption {
                id: "once".into(),
                label: "Allow once".into(),
                allow: true,
            },
            PermissionOption {
                id: "always".into(),
                label: "Always allow".into(),
                allow: true,
            },
            PermissionOption {
                id: "reject".into(),
                label: "Deny".into(),
                allow: false,
            },
        ],
    });
}

fn tool_activity(part: &Value, events: &Sender<DriverEvent>, state: &mut OpenCodeStreamState) {
    let wire_title = part
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or("Tool")
        .to_owned();
    let id = part
        .get("callID")
        .or_else(|| part.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let arguments = part.pointer("/state/input");
    let complete = matches!(
        part.pointer("/state/status").and_then(Value::as_str),
        Some("completed" | "error")
    );
    let stored = id.as_ref().and_then(|id| {
        if complete {
            state.tools.remove(id)
        } else {
            state.tools.get(id).cloned()
        }
    });
    let kind = stored
        .as_ref()
        .map(|(kind, _)| *kind)
        .unwrap_or_else(|| super::support::classify_tool(&wire_title));
    let title = activity::input_title(arguments)
        .or_else(|| stored.map(|(_, title)| title))
        .unwrap_or(wire_title);
    if !complete && let Some(id) = id.as_ref() {
        state.tools.insert(id.clone(), (kind, title.clone()));
    }
    let failed = part.pointer("/state/status").and_then(Value::as_str) == Some("error")
        || part
            .pointer("/state/error")
            .is_some_and(|error| !error.is_null());
    let output = part
        .pointer("/state/error")
        .filter(|value| !value.is_null())
        .or_else(|| {
            part.pointer("/state/output")
                .filter(|value| !value.is_null())
        });
    let item = activity::tool_activity(
        id,
        kind,
        title,
        arguments,
        output,
        part.get("state"),
        failed,
        complete,
    );
    let _ = events.send(DriverEvent::RichActivity(item));
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
        OpenCodeStreamState,
    ) {
        let (events, event_rx) = unbounded();
        let (commands, command_rx) = unbounded();
        (
            events,
            event_rx,
            commands,
            command_rx,
            Mutex::new(true),
            OpenCodeStreamState::default(),
        )
    }

    /// Drives a real `opencode serve` through the actual driver. Ignored by
    /// default: needs the CLI installed, credentials, and the network. Run with
    /// `cargo test --bin waku opencode_session_against_a_real_server -- --ignored`.
    #[test]
    #[ignore = "requires an installed, authenticated opencode"]
    fn opencode_session_against_a_real_server() {
        let binary =
            crate::command_env::find_executable("opencode").expect("opencode is not installed");
        let (events, event_rx) = unbounded();
        let driver = OpenCodeDriver::start(
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
        .expect("the server should start and open a session");

        let connected = event_rx
            .recv_timeout(std::time::Duration::from_secs(90))
            .expect("the server should report its session");
        assert!(matches!(
            connected,
            DriverEvent::Connected {
                provider_cursor: Some(ProviderResumeCursor::OpenCode { .. })
            }
        ));

        driver.prompt("Reply with exactly: OK. Do not use any tools.".into());
        let mut text = String::new();
        let mut finished = None;
        while let Ok(event) = event_rx.recv_timeout(std::time::Duration::from_secs(180)) {
            match event {
                DriverEvent::TextDelta(delta) => text.push_str(&delta),
                DriverEvent::TurnFinished { success, .. } => {
                    finished = Some(success);
                    break;
                }
                DriverEvent::Error(error) => panic!("the server reported: {error}"),
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
    fn streams_text_and_correlated_tools_and_settles_on_idle() {
        let (events, event_rx, commands, _command_rx, turn, mut state) = harness();
        // Payloads copied from a live `opencode serve` event stream.
        let wire = [
            json!({"type":"message.part.delta","properties":{"sessionID":"ses_1","messageID":"msg_1","partID":"prt_1","field":"text","delta":"OK"}}),
            json!({"type":"message.part.delta","properties":{"field":"reasoning","delta":"thinking"}}),
            json!({"type":"message.part.updated","properties":{"part":{"type":"tool","tool":"read","callID":"call_1","state":{"status":"running","input":{"filePath":"a.txt"}}}}}),
            json!({"type":"message.part.updated","properties":{"part":{"type":"tool","tool":"read","callID":"call_1","state":{"status":"completed","output":"contents"}}}}),
            // Not transcript content.
            json!({"type":"session.diff","properties":{"diff":[]}}),
            json!({"type":"message.updated","properties":{"info":{"role":"assistant"}}}),
            json!({"type":"session.idle","properties":{"sessionID":"ses_1"}}),
        ];
        for event in wire {
            handle_event(&event, &events, &commands, &turn, true, &mut state);
        }

        let mut seen = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            seen.push(event);
        }
        assert!(matches!(&seen[0], DriverEvent::TextDelta(text) if text == "OK"));
        assert!(matches!(&seen[1], DriverEvent::ReasoningDelta(text) if text == "thinking"));
        assert!(matches!(&seen[2], DriverEvent::RichActivity(item)
                if item.kind == ActivityKind::Search && !item.complete));
        assert!(matches!(&seen[3], DriverEvent::RichActivity(item)
                if item.complete && item.title == "read"));
        assert!(matches!(
            &seen[4],
            DriverEvent::TurnFinished { success: true, .. }
        ));
        assert_eq!(seen.len(), 5, "non-transcript events leaked");
        assert!(!*turn.lock(), "the turn should be settled exactly once");
    }

    #[test]
    fn supervised_mode_asks_the_user_and_auto_modes_answer_durably() {
        let (events, event_rx, commands, command_rx, turn, mut state) = harness();
        // Shape from the server's OpenAPI PermissionRequest schema.
        let permission = json!({
            "type": "permission.requested",
            "properties": {
                "id": "per_abc",
                "sessionID": "ses_1",
                "permission": "bash",
                "patterns": ["rm -rf *"],
                "metadata": {},
                "always": []
            }
        });

        handle_event(&permission, &events, &commands, &turn, false, &mut state);
        let DriverEvent::Permission {
            request_id,
            options,
            title,
            ..
        } = event_rx.try_recv().unwrap()
        else {
            panic!("Supervised mode must surface the request to the user");
        };
        assert_eq!(request_id, "per_abc");
        assert_eq!(title, "rm -rf *");
        assert_eq!(
            options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
            ["once", "always", "reject"]
        );
        assert!(command_rx.try_recv().is_err());

        handle_event(&permission, &events, &commands, &turn, true, &mut state);
        let Ok(CommandMessage::Respond { option_id, .. }) = command_rx.try_recv() else {
            panic!("auto modes must answer without the user");
        };
        assert_eq!(option_id, "always");
        assert!(event_rx.try_recv().is_err());
    }
}
