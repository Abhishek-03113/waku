use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, ContentBlock, InitializeRequest, LoadSessionRequest, NewSessionRequest,
    PromptRequest, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification, SetSessionModeRequest, TextContent,
};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, ConnectionTo};
use anyhow::Context as _;
use crossbeam_channel::Sender;
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::driver::DriverControl;
use crate::model::{ActivityKind, DriverEvent, PermissionOption, RuntimeMode};

enum GrokCommand {
    Prompt(String),
    Cancel,
    Shutdown,
}

pub struct GrokDriver {
    commands: mpsc::UnboundedSender<GrokCommand>,
    permissions: Arc<Mutex<HashMap<String, oneshot::Sender<String>>>>,
}

impl GrokDriver {
    pub fn start(
        binary: PathBuf,
        cwd: PathBuf,
        mode: RuntimeMode,
        existing_session_id: Option<String>,
        events: Sender<DriverEvent>,
    ) -> anyhow::Result<Self> {
        let (commands, command_rx) = mpsc::unbounded_channel();
        let permissions = Arc::new(Mutex::new(HashMap::new()));
        let worker_permissions = permissions.clone();
        thread::Builder::new()
            .name("waku-grok-acp".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = events.send(DriverEvent::Error(format!(
                            "Failed to create Grok runtime: {error}"
                        )));
                        return;
                    }
                };
                runtime.block_on(run_grok(
                    binary,
                    cwd,
                    mode,
                    existing_session_id,
                    events.clone(),
                    worker_permissions,
                    command_rx,
                ));
                let _ = events.send(DriverEvent::ProcessExited);
            })
            .context("failed to start Grok ACP thread")?;
        Ok(Self {
            commands,
            permissions,
        })
    }
}

impl DriverControl for GrokDriver {
    fn prompt(&self, prompt: String) {
        let _ = self.commands.send(GrokCommand::Prompt(prompt));
    }

    fn cancel(&self) {
        let _ = self.commands.send(GrokCommand::Cancel);
    }

    fn respond(&self, request_id: String, option_id: String) {
        if let Some(sender) = self.permissions.lock().remove(&request_id) {
            let _ = sender.send(option_id);
        }
    }
}

impl Drop for GrokDriver {
    fn drop(&mut self) {
        let _ = self.commands.send(GrokCommand::Shutdown);
    }
}

async fn run_grok(
    binary: PathBuf,
    cwd: PathBuf,
    mode: RuntimeMode,
    existing_session_id: Option<String>,
    events: Sender<DriverEvent>,
    permissions: Arc<Mutex<HashMap<String, oneshot::Sender<String>>>>,
    mut commands: mpsc::UnboundedReceiver<GrokCommand>,
) {
    let mut arguments = vec!["agent"];
    if mode == RuntimeMode::Auto {
        arguments.push("--always-approve");
    }
    arguments.push("stdio");
    let agent = AcpAgent::new(AcpAgentConfig::new(binary).args(arguments));
    let notification_events = events.clone();
    let permission_events = events.clone();
    let connection_events = events.clone();
    let result = agent_client_protocol::Client
        .builder()
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                if let Ok(value) = serde_json::to_value(notification.update) {
                    parse_acp_update(value, &notification_events);
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _connection| {
                let token = Uuid::new_v4().to_string();
                let (sender, receiver) = oneshot::channel();
                permissions.lock().insert(token.clone(), sender);
                let options = request
                    .options
                    .iter()
                    .map(|option| PermissionOption {
                        id: option.option_id.to_string(),
                        label: option.name.to_string(),
                        allow: format!("{:?}", option.kind)
                            .to_ascii_lowercase()
                            .contains("allow"),
                    })
                    .collect::<Vec<_>>();
                let serialized = serde_json::to_value(&request).unwrap_or_default();
                let title = serialized
                    .pointer("/toolCall/title")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Grok needs approval")
                    .to_owned();
                let detail = serialized
                    .pointer("/toolCall/rawInput")
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "Review this tool request before continuing.".into());
                let _ = permission_events.send(DriverEvent::Permission {
                    request_id: token,
                    title,
                    detail,
                    options,
                });
                match receiver.await {
                    Ok(option_id) => responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                            option_id,
                        )),
                    )),
                    Err(_) => responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Cancelled,
                    )),
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent, |connection: ConnectionTo<Agent>| async move {
            let initialized = connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            let (session_id, modes) = if let Some(existing_session_id) = existing_session_id
                && initialized.agent_capabilities.load_session
            {
                match connection
                    .send_request(LoadSessionRequest::new(
                        existing_session_id.clone(),
                        cwd.clone(),
                    ))
                    .block_task()
                    .await
                {
                    Ok(response) => (existing_session_id.into(), response.modes),
                    Err(_) => {
                        let response = connection
                            .send_request(NewSessionRequest::new(cwd.clone()))
                            .block_task()
                            .await?;
                        (response.session_id, response.modes)
                    }
                }
            } else {
                let response = connection
                    .send_request(NewSessionRequest::new(cwd))
                    .block_task()
                    .await?;
                (response.session_id, response.modes)
            };
            if mode == RuntimeMode::Plan
                && let Some(plan_mode) = modes.and_then(|modes| {
                    modes.available_modes.into_iter().find(|available| {
                        available
                            .id
                            .to_string()
                            .to_ascii_lowercase()
                            .contains("plan")
                            || available.name.to_ascii_lowercase().contains("plan")
                    })
                })
            {
                let _ = connection
                    .send_request(SetSessionModeRequest::new(session_id.clone(), plan_mode.id))
                    .block_task()
                    .await;
            }
            let _ = connection_events.send(DriverEvent::Connected {
                provider_session_id: Some(session_id.to_string()),
            });
            while let Some(command) = commands.recv().await {
                match command {
                    GrokCommand::Prompt(prompt) => {
                        let _ = connection_events.send(DriverEvent::TurnStarted);
                        let prompt_request = connection
                            .send_request(PromptRequest::new(
                                session_id.clone(),
                                vec![ContentBlock::Text(TextContent::new(prompt))],
                            ))
                            .block_task();
                        tokio::pin!(prompt_request);
                        let response = loop {
                            tokio::select! {
                                response = &mut prompt_request => break Some(response),
                                command = commands.recv() => match command {
                                    Some(GrokCommand::Cancel) => {
                                        let _ = connection.send_notification(
                                            CancelNotification::new(session_id.clone())
                                        );
                                    }
                                    Some(GrokCommand::Shutdown) | None => break None,
                                    Some(GrokCommand::Prompt(_)) => {}
                                }
                            }
                        };
                        let Some(response) = response else {
                            break;
                        };
                        match response {
                            Ok(response) => {
                                let _ = connection_events.send(DriverEvent::TurnFinished {
                                    success: true,
                                    summary: Some(format!("{:?}", response.stop_reason)),
                                });
                            }
                            Err(error) => {
                                let _ =
                                    connection_events.send(DriverEvent::Error(error.to_string()));
                                let _ = connection_events.send(DriverEvent::TurnFinished {
                                    success: false,
                                    summary: None,
                                });
                            }
                        }
                    }
                    GrokCommand::Cancel => {}
                    GrokCommand::Shutdown => break,
                }
            }
            Ok(())
        })
        .await;
    if let Err(error) = result {
        let _ = events.send(DriverEvent::Error(format!("Grok ACP failed: {error}")));
    }
}

fn parse_acp_update(value: serde_json::Value, events: &Sender<DriverEvent>) {
    let update_type = value
        .get("sessionUpdate")
        .and_then(serde_json::Value::as_str);
    match update_type {
        Some("agent_message_chunk") => {
            if let Some(text) = value
                .pointer("/content/text")
                .and_then(serde_json::Value::as_str)
            {
                let _ = events.send(DriverEvent::TextDelta(text.to_owned()));
            }
        }
        Some("agent_thought_chunk") => {
            if let Some(text) = value
                .pointer("/content/text")
                .and_then(serde_json::Value::as_str)
            {
                let _ = events.send(DriverEvent::ReasoningDelta(text.to_owned()));
            }
        }
        Some("tool_call") | Some("tool_call_update") => {
            let title = value
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Tool")
                .to_owned();
            let status = value
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let detail = value.get("rawInput").map(|value| value.to_string());
            let _ = events.send(DriverEvent::Activity {
                kind: classify_acp_tool(&title),
                title,
                detail,
                complete: matches!(status, "completed" | "failed"),
            });
        }
        Some("plan") => {
            let _ = events.send(DriverEvent::Activity {
                kind: ActivityKind::Plan,
                title: "Plan updated".into(),
                detail: value.get("entries").map(|value| value.to_string()),
                complete: false,
            });
        }
        _ => {}
    }
}

fn classify_acp_tool(title: &str) -> ActivityKind {
    let title = title.to_ascii_lowercase();
    if title.contains("command") || title.contains("shell") {
        ActivityKind::Command
    } else if title.contains("edit") || title.contains("file") {
        ActivityKind::FileChange
    } else if title.contains("search") {
        ActivityKind::Search
    } else {
        ActivityKind::Tool
    }
}
