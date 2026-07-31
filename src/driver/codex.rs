use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use crossbeam_channel::{Sender, bounded, unbounded};
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::driver::{DriverControl, DriverStartOptions};
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
    Rollback {
        turns: usize,
        response: Sender<Result<(), String>>,
    },
    Shutdown,
}

pub struct CodexDriver {
    commands: Sender<CommandMessage>,
}

impl CodexDriver {
    pub fn start(options: DriverStartOptions, events: Sender<DriverEvent>) -> anyhow::Result<Self> {
        let DriverStartOptions {
            binary,
            cwd,
            mode,
            interaction_mode,
            model,
            reasoning_effort,
            service_tier,
            provider_cursor,
        } = options;
        let provider_session_id = match provider_cursor {
            Some(ProviderResumeCursor::Codex { thread_id }) => Some(thread_id),
            Some(cursor) => {
                return Err(anyhow!(
                    "cannot resume Codex from a {} cursor",
                    cursor.provider().display_name()
                ));
            }
            None => None,
        };
        let mut child = Command::new(binary)
            .args(["app-server", "--stdio"])
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to start `codex app-server`")?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Codex stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Codex stdout unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("Codex stderr unavailable"))?;
        let (commands, command_rx) = unbounded();
        let thread_id = Arc::new(Mutex::new(None::<String>));
        let turn_id = Arc::new(Mutex::new(None::<String>));
        let pending_responses =
            Arc::new(Mutex::new(HashMap::<u64, Sender<Result<(), String>>>::new()));

        let writer_thread_id = thread_id.clone();
        let writer_turn_id = turn_id.clone();
        let writer_pending_responses = pending_responses.clone();
        let writer_events = events.clone();
        let cwd_string = cwd.display().to_string();
        thread::Builder::new()
            .name("waku-codex-writer".into())
            .spawn(move || {
                let mut stdin = stdin;
                let initialize = json!({
                    "method": "initialize",
                    "id": 0,
                    "params": {
                        "clientInfo": {
                            "name": "waku",
                            "title": "Waku",
                            "version": env!("CARGO_PKG_VERSION")
                        },
                        "capabilities": {
                            "experimentalApi": true
                        }
                    }
                });
                if write_json_line(&mut stdin, &initialize).is_err()
                    || write_json_line(
                        &mut stdin,
                        &json!({
                            "method": "initialized",
                            "params": {}
                        }),
                    )
                    .is_err()
                {
                    let _ = writer_events.send(DriverEvent::Error(
                        "Failed to initialize Codex app-server".into(),
                    ));
                    return;
                }

                let (approval_policy, sandbox, approvals_reviewer) =
                    codex_permissions(mode, interaction_mode);
                let open_thread = if let Some(thread_id) = provider_session_id {
                    let mut params = json!({
                        "threadId": thread_id,
                        "cwd": cwd_string,
                        "approvalPolicy": approval_policy,
                        "sandbox": sandbox,
                        "approvalsReviewer": approvals_reviewer
                    });
                    if let Some(model) = model.as_deref() {
                        params["model"] = json!(model);
                    }
                    if let Some(service_tier) = service_tier.as_deref() {
                        params["serviceTier"] = json!(service_tier);
                    }
                    json!({
                        "method": "thread/resume",
                        "id": 1,
                        "params": params
                    })
                } else {
                    let mut params = json!({
                        "cwd": cwd_string,
                        "approvalPolicy": approval_policy,
                        "sandbox": sandbox,
                        "approvalsReviewer": approvals_reviewer,
                        "serviceName": "waku"
                    });
                    if let Some(model) = model.as_deref() {
                        params["model"] = json!(model);
                    }
                    if let Some(service_tier) = service_tier.as_deref() {
                        params["serviceTier"] = json!(service_tier);
                    }
                    json!({
                        "method": "thread/start",
                        "id": 1,
                        "params": params
                    })
                };
                let _ = write_json_line(&mut stdin, &open_thread);

                let mut next_request_id = 10_u64;
                while let Ok(command) = command_rx.recv() {
                    let message = match command {
                        CommandMessage::Prompt(text) => {
                            let Some(thread_id) = wait_for_thread_id(&writer_thread_id) else {
                                let _ = writer_events.send(DriverEvent::Error(
                                    "Codex did not finish opening its thread.".into(),
                                ));
                                continue;
                            };
                            next_request_id += 1;
                            let mut params = json!({
                                "threadId": thread_id,
                                "input": [{"type": "text", "text": text}],
                                "approvalPolicy": approval_policy,
                                "approvalsReviewer": approvals_reviewer,
                                "sandboxPolicy": codex_sandbox_policy(sandbox)
                            });
                            if let Some(model) = model.as_deref() {
                                params["model"] = json!(model);
                            }
                            if let Some(reasoning_effort) = reasoning_effort.as_deref() {
                                params["effort"] = json!(reasoning_effort);
                            }
                            if let Some(service_tier) = service_tier.as_deref() {
                                params["serviceTier"] = json!(service_tier);
                            }
                            json!({
                                "method": "turn/start",
                                "id": next_request_id,
                                "params": params
                            })
                        }
                        CommandMessage::Cancel => {
                            let (Some(thread_id), Some(turn_id)) = (
                                writer_thread_id.lock().clone(),
                                writer_turn_id.lock().clone(),
                            ) else {
                                continue;
                            };
                            next_request_id += 1;
                            json!({
                                "method": "turn/interrupt",
                                "id": next_request_id,
                                "params": {"threadId": thread_id, "turnId": turn_id}
                            })
                        }
                        CommandMessage::Respond {
                            request_id,
                            option_id,
                        } => {
                            let id = parse_rpc_id(&request_id);
                            json!({
                                "id": id,
                                "result": {"decision": option_id}
                            })
                        }
                        CommandMessage::Rollback { turns, response } => {
                            let Some(thread_id) = wait_for_thread_id(&writer_thread_id) else {
                                let _ = response
                                    .send(Err("Codex did not finish opening its thread.".into()));
                                continue;
                            };
                            next_request_id += 1;
                            let request_id = next_request_id;
                            writer_pending_responses.lock().insert(request_id, response);
                            let message = json!({
                                "method": "thread/rollback",
                                "id": request_id,
                                "params": {
                                    "threadId": thread_id,
                                    "numTurns": turns
                                }
                            });
                            if let Err(error) = write_json_line(&mut stdin, &message)
                                && let Some(response) =
                                    writer_pending_responses.lock().remove(&request_id)
                            {
                                let _ = response
                                    .send(Err(format!("Codex transport write failed: {error}")));
                            }
                            continue;
                        }
                        CommandMessage::Shutdown => break,
                    };
                    if let Err(error) = write_json_line(&mut stdin, &message) {
                        let _ = writer_events.send(DriverEvent::Error(format!(
                            "Codex transport write failed: {error}"
                        )));
                        break;
                    }
                }
            })?;

        let reader_thread_id = thread_id.clone();
        let reader_turn_id = turn_id.clone();
        let reader_pending_responses = pending_responses.clone();
        let reader_events = events.clone();
        thread::Builder::new()
            .name("waku-codex-reader".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    match line {
                        Ok(line) if !line.trim().is_empty() => {
                            match serde_json::from_str::<Value>(&line) {
                                Ok(value) => handle_codex_message(
                                    value,
                                    &reader_thread_id,
                                    &reader_turn_id,
                                    &reader_pending_responses,
                                    &reader_events,
                                ),
                                Err(error) => {
                                    let _ = reader_events.send(DriverEvent::Error(format!(
                                        "Codex sent invalid JSON: {error}"
                                    )));
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(error) => {
                            let _ = reader_events.send(DriverEvent::Error(format!(
                                "Codex transport read failed: {error}"
                            )));
                            break;
                        }
                    }
                }
                let _ = reader_events.send(DriverEvent::ProcessExited);
            })?;

        thread::Builder::new()
            .name("waku-codex-stderr".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if is_visible_stderr_notice(&line) {
                        let _ = events.send(DriverEvent::Error(clean_stderr(&line)));
                    }
                }
            })?;

        Ok(Self { commands })
    }
}

fn codex_permissions(
    mode: RuntimeMode,
    interaction_mode: InteractionMode,
) -> (&'static str, &'static str, &'static str) {
    if interaction_mode == InteractionMode::Plan || mode == RuntimeMode::Plan {
        return ("never", "read-only", "user");
    }
    match mode {
        RuntimeMode::Ask => ("untrusted", "read-only", "user"),
        RuntimeMode::AutoAcceptEdits => ("on-request", "workspace-write", "user"),
        RuntimeMode::Auto => ("on-request", "workspace-write", "auto_review"),
        RuntimeMode::FullAccess => ("never", "danger-full-access", "user"),
        RuntimeMode::Plan => unreachable!("handled above"),
    }
}

fn codex_sandbox_policy(sandbox: &str) -> Value {
    match sandbox {
        "read-only" => json!({"type": "readOnly"}),
        "danger-full-access" => json!({"type": "dangerFullAccess"}),
        _ => json!({"type": "workspaceWrite"}),
    }
}

impl DriverControl for CodexDriver {
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

    fn rollback(&self, turns: usize) -> anyhow::Result<()> {
        if turns == 0 {
            return Ok(());
        }
        let (response_tx, response_rx) = bounded(1);
        self.commands
            .send(CommandMessage::Rollback {
                turns,
                response: response_tx,
            })
            .context("Codex driver stopped before rollback")?;
        response_rx
            .recv_timeout(Duration::from_secs(15))
            .context("timed out waiting for Codex conversation rollback")?
            .map_err(anyhow::Error::msg)
    }
}

impl Drop for CodexDriver {
    fn drop(&mut self) {
        let _ = self.commands.send(CommandMessage::Shutdown);
    }
}

fn write_json_line(writer: &mut impl Write, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn wait_for_thread_id(thread_id: &Mutex<Option<String>>) -> Option<String> {
    for _ in 0..500 {
        if let Some(thread_id) = thread_id.lock().clone() {
            return Some(thread_id);
        }
        thread::sleep(Duration::from_millis(20));
    }
    None
}

fn handle_codex_message(
    value: Value,
    thread_id: &Mutex<Option<String>>,
    turn_id: &Mutex<Option<String>>,
    pending_responses: &Mutex<HashMap<u64, Sender<Result<(), String>>>>,
    events: &Sender<DriverEvent>,
) {
    if let Some(id) = value.get("id").and_then(Value::as_u64)
        && id != 1
        && let Some(response) = pending_responses.lock().remove(&id)
    {
        let result = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map_or_else(|| Ok(()), |error| Err(error.to_owned()));
        let _ = response.send(result);
        return;
    }

    if value.get("id").and_then(Value::as_u64) == Some(1) {
        if let Some(id) = value.pointer("/result/thread/id").and_then(Value::as_str) {
            *thread_id.lock() = Some(id.to_owned());
            let _ = events.send(DriverEvent::Connected {
                provider_cursor: Some(ProviderResumeCursor::Codex {
                    thread_id: id.to_owned(),
                }),
            });
        } else if let Some(error) = value.pointer("/error/message").and_then(Value::as_str) {
            let _ = events.send(DriverEvent::Error(error.to_owned()));
        }
        return;
    }

    let Some(method) = value.get("method").and_then(Value::as_str) else {
        if let Some(error) = value.pointer("/error/message").and_then(Value::as_str) {
            let _ = events.send(DriverEvent::Error(error.to_owned()));
        }
        return;
    };
    let params = value.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "turn/started" => {
            if let Some(id) = params.pointer("/turn/id").and_then(Value::as_str) {
                *turn_id.lock() = Some(id.to_owned());
            }
            let _ = events.send(DriverEvent::TurnStarted);
        }
        "item/agentMessage/delta" => {
            if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                let _ = events.send(DriverEvent::TextDelta(delta.to_owned()));
            }
        }
        "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
            if let Some(delta) = params
                .get("delta")
                .and_then(Value::as_str)
                .filter(|delta| !delta.is_empty())
            {
                let _ = events.send(DriverEvent::ReasoningDelta(delta.to_owned()));
            }
        }
        "item/started" | "item/completed" => {
            if let Some(item) = params.get("item") {
                let complete = method == "item/completed";
                let kind = codex_activity_kind(item);
                if let Some(kind) = kind {
                    let title = codex_item_title(item);
                    let detail = codex_item_detail(item);
                    let _ = events.send(DriverEvent::Activity {
                        id: item.get("id").and_then(Value::as_str).map(str::to_owned),
                        kind,
                        title,
                        detail,
                        complete,
                    });
                }
            }
        }
        "turn/completed" => {
            *turn_id.lock() = None;
            let status = params
                .pointer("/turn/status")
                .and_then(Value::as_str)
                .unwrap_or("completed");
            let error = params
                .pointer("/turn/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let _ = events.send(DriverEvent::TurnFinished {
                success: status == "completed",
                summary: error,
            });
        }
        "error" => {
            if let Some(message) = params.get("message").and_then(Value::as_str) {
                let _ = events.send(DriverEvent::Error(message.to_owned()));
            }
        }
        "mcpServer/startupStatus/updated"
            if params.get("status").and_then(Value::as_str) == Some("failed") =>
        {
            if let Some(message) = params.get("error").and_then(Value::as_str) {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("MCP");
                let _ = events.send(DriverEvent::Error(format!("{name}: {message}")));
            }
        }
        method if value.get("id").is_some() && method.contains("requestApproval") => {
            let request_id = rpc_id_string(value.get("id").unwrap());
            let (title, detail) = approval_copy(method, &params);
            let _ = events.send(DriverEvent::Permission {
                request_id,
                title,
                detail,
                options: vec![
                    PermissionOption {
                        id: "accept".into(),
                        label: "Allow once".into(),
                        allow: true,
                    },
                    PermissionOption {
                        id: "acceptForSession".into(),
                        label: "Allow for session".into(),
                        allow: true,
                    },
                    PermissionOption {
                        id: "decline".into(),
                        label: "Deny".into(),
                        allow: false,
                    },
                ],
            });
        }
        _ => {}
    }
}

fn codex_activity_kind(item: &Value) -> Option<ActivityKind> {
    let item_type = item
        .get("type")
        .and_then(Value::as_str)?
        .to_ascii_lowercase();
    if item_type.contains("command") {
        Some(ActivityKind::Command)
    } else if item_type.contains("filechange") || item_type.contains("patch") {
        Some(ActivityKind::FileChange)
    } else if item_type.contains("websearch") {
        Some(ActivityKind::Search)
    } else if item_type.contains("plan") || item_type.contains("todo") {
        Some(ActivityKind::Plan)
    } else if item_type.contains("tool") || item_type.contains("collab") {
        Some(ActivityKind::Tool)
    } else {
        None
    }
}

fn codex_item_title(item: &Value) -> String {
    if let Some(command) = item.get("command").and_then(Value::as_str) {
        return command.to_owned();
    }
    if let Some(query) = item.get("query").and_then(Value::as_str) {
        return format!("Search for {query}");
    }
    if let Some(name) = item.get("tool").and_then(Value::as_str) {
        return split_camel_case(name);
    }
    let item_type = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("Activity");
    split_camel_case(item_type)
}

fn codex_item_detail(item: &Value) -> Option<String> {
    item.get("cwd")
        .and_then(Value::as_str)
        .or_else(|| item.get("path").and_then(Value::as_str))
        .or_else(|| item.get("status").and_then(Value::as_str))
        .map(str::to_owned)
}

fn approval_copy(method: &str, params: &Value) -> (String, String) {
    if method.contains("commandExecution") {
        let command = params
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("Run a command");
        ("Command approval".into(), command.into())
    } else {
        let reason = params
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("Apply the proposed file changes");
        ("File change approval".into(), reason.into())
    }
}

fn rpc_id_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn parse_rpc_id(value: &str) -> Value {
    value
        .parse::<u64>()
        .map(Value::from)
        .unwrap_or_else(|_| Value::String(value.to_owned()))
}

fn split_camel_case(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 4);
    for (index, character) in value.chars().enumerate() {
        if index > 0 && character.is_ascii_uppercase() {
            output.push(' ');
        }
        output.push(character);
    }
    let mut characters = output.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
        .unwrap_or_else(|| "Activity".into())
}

fn clean_stderr(line: &str) -> String {
    line.split_once(": ")
        .map(|(_, message)| message.to_owned())
        .unwrap_or_else(|| line.to_owned())
}

fn is_visible_stderr_notice(line: &str) -> bool {
    let lowercase = line.to_ascii_lowercase();
    if lowercase.contains("transport channel closed")
        || lowercase.contains("missing authorization header")
    {
        return false;
    }
    line.contains(" ERROR ")
        || line.contains('⚠')
        || lowercase.contains("fatal")
        || lowercase.contains("warning")
        || lowercase.contains("mcp startup incomplete")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_modes_match_codex_permission_profiles() {
        assert_eq!(
            codex_permissions(RuntimeMode::Ask, InteractionMode::Build),
            ("untrusted", "read-only", "user")
        );
        assert_eq!(
            codex_permissions(RuntimeMode::AutoAcceptEdits, InteractionMode::Build),
            ("on-request", "workspace-write", "user")
        );
        assert_eq!(
            codex_permissions(RuntimeMode::Auto, InteractionMode::Build),
            ("on-request", "workspace-write", "auto_review")
        );
        assert_eq!(
            codex_permissions(RuntimeMode::FullAccess, InteractionMode::Build),
            ("never", "danger-full-access", "user")
        );
        assert_eq!(
            codex_permissions(RuntimeMode::FullAccess, InteractionMode::Plan),
            ("never", "read-only", "user")
        );
    }

    #[test]
    fn rollback_rpc_responses_are_routed_to_the_waiting_request() {
        let thread_id = Mutex::new(Some("thread-1".to_owned()));
        let turn_id = Mutex::new(None);
        let pending = Mutex::new(HashMap::new());
        let (response_tx, response_rx) = bounded(1);
        pending.lock().insert(42, response_tx);
        let (event_tx, event_rx) = unbounded();

        handle_codex_message(
            json!({"id": 42, "result": {}}),
            &thread_id,
            &turn_id,
            &pending,
            &event_tx,
        );

        assert_eq!(response_rx.recv().unwrap(), Ok(()));
        assert!(pending.lock().is_empty());
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn rollback_rpc_errors_are_returned_without_becoming_stream_errors() {
        let thread_id = Mutex::new(Some("thread-1".to_owned()));
        let turn_id = Mutex::new(None);
        let pending = Mutex::new(HashMap::new());
        let (response_tx, response_rx) = bounded(1);
        pending.lock().insert(43, response_tx);
        let (event_tx, event_rx) = unbounded();

        handle_codex_message(
            json!({"id": 43, "error": {"message": "cannot roll back"}}),
            &thread_id,
            &turn_id,
            &pending,
            &event_tx,
        );

        assert_eq!(
            response_rx.recv().unwrap(),
            Err("cannot roll back".to_owned())
        );
        assert!(event_rx.try_recv().is_err());
    }
}
