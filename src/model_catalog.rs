use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::model::{ProviderKind, ProviderModel, ProviderModelOption};

const CODEX_RPC_TIMEOUT: Duration = Duration::from_secs(5);
const PI_RPC_TIMEOUT: Duration = Duration::from_secs(10);

pub fn fallback_models(provider: ProviderKind) -> Vec<ProviderModel> {
    match provider {
        ProviderKind::Codex => [
            ProviderModel::new("gpt-5.6-sol", "GPT-5.6-Sol").default(),
            ProviderModel::new("gpt-5.6-terra", "GPT-5.6-Terra"),
            ProviderModel::new("gpt-5.6-luna", "GPT-5.6-Luna"),
            ProviderModel::new("gpt-5.5", "GPT-5.5"),
            ProviderModel::new("gpt-5.4", "GPT-5.4"),
        ]
        .into_iter()
        .map(|model| {
            model
                .reasoning(
                    reasoning_options(["low", "medium", "high", "xhigh"]),
                    "medium",
                )
                .service_tiers(
                    [ProviderModelOption::new("fast", "Fast")
                        .description("Faster responses at a higher usage rate.")],
                    "default",
                )
        })
        .collect(),
        ProviderKind::Claude => vec![
            claude_reasoning_model("claude-fable-5", "Claude Fable 5"),
            claude_reasoning_model("claude-opus-5", "Claude Opus 5"),
            claude_reasoning_model("claude-opus-4-8", "Claude Opus 4.8"),
            claude_reasoning_model("claude-opus-4-7", "Claude Opus 4.7"),
            claude_reasoning_model("claude-opus-4-6", "Claude Opus 4.6"),
            claude_reasoning_model("claude-opus-4-5", "Claude Opus 4.5"),
            claude_reasoning_model("claude-sonnet-5", "Claude Sonnet 5").default(),
            claude_reasoning_model("claude-sonnet-4-6", "Claude Sonnet 4.6"),
            ProviderModel::new("claude-haiku-4-5", "Claude Haiku 4.5"),
        ],
        ProviderKind::OpenCode => Vec::new(),
        ProviderKind::Grok => {
            vec![ProviderModel::new("grok-build", "Grok Build").default()]
        }
        // Pi's catalog depends on the user's configured LLM providers. A
        // fabricated fallback would make unavailable models look selectable.
        ProviderKind::Pi => Vec::new(),
    }
}

pub fn discover_models(provider: ProviderKind, binary: &Path) -> Vec<ProviderModel> {
    let discovered = match provider {
        ProviderKind::Codex => discover_codex_models(binary),
        // Claude Code accepts model aliases and full IDs but does not expose a
        // model inventory command. Keep this catalog aligned with the
        // version-gated list used by T3 Code.
        ProviderKind::Claude => Vec::new(),
        ProviderKind::OpenCode => discover_opencode_models(binary),
        ProviderKind::Grok => discover_grok_models(binary),
        ProviderKind::Pi => discover_pi_models(binary),
    };
    if discovered.is_empty() {
        fallback_models(provider)
    } else {
        deduplicate(discovered)
    }
}

fn discover_opencode_models(binary: &Path) -> Vec<ProviderModel> {
    let Ok(output) = crate::command_env::command(binary).arg("models").output() else {
        return Vec::new();
    };
    parse_opencode_models(&String::from_utf8_lossy(&output.stdout))
}

fn parse_opencode_models(output: &str) -> Vec<ProviderModel> {
    output
        .lines()
        .filter_map(|line| {
            let id = strip_ansi(line).trim().to_owned();
            if id.is_empty() || id.split_whitespace().count() != 1 || !id.contains('/') {
                return None;
            }
            let (provider, model) = id.split_once('/')?;
            if provider.is_empty() || model.is_empty() {
                return None;
            }
            Some(
                ProviderModel::new(id.clone(), display_name_from_slug(model))
                    .sub_provider(display_name_from_slug(provider)),
            )
        })
        .collect()
}

fn discover_grok_models(binary: &Path) -> Vec<ProviderModel> {
    let Ok(output) = crate::command_env::command(binary).arg("models").output() else {
        return Vec::new();
    };
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    parse_grok_models(&combined)
}

fn parse_grok_models(output: &str) -> Vec<ProviderModel> {
    let cleaned = strip_ansi(output);
    let default_model = cleaned.lines().find_map(|line| {
        line.trim()
            .strip_prefix("Default model:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    });
    let mut in_models = false;
    cleaned
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line == "Available models:" {
                in_models = true;
                return None;
            }
            if !in_models {
                return None;
            }
            let id = line
                .strip_prefix('*')
                .map(str::trim)?
                .strip_suffix(" (default)")
                .unwrap_or_else(|| line.strip_prefix('*').unwrap_or(line).trim())
                .trim();
            if id.is_empty() || id.chars().any(char::is_whitespace) {
                return None;
            }
            let mut model = ProviderModel::new(id, display_name_from_slug(id));
            model.is_default = default_model.as_deref() == Some(id);
            Some(model)
        })
        .collect()
}

fn discover_pi_models(binary: &Path) -> Vec<ProviderModel> {
    let Ok(mut child) = crate::command_env::command(binary)
        .args([
            "--mode",
            "rpc",
            "--no-session",
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-context-files",
        ])
        .env("PI_SKIP_VERSION_CHECK", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        return Vec::new();
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        return Vec::new();
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                let _ = tx.send(value);
            }
        }
    });

    let state_request = json!({"id": "waku-state", "type": "get_state"});
    let models_request = json!({"id": "waku-models", "type": "get_available_models"});
    let result = if write_json_line(&mut stdin, &state_request).is_ok()
        && let Some(state) = recv_pi_rpc_response(&rx, "waku-state", PI_RPC_TIMEOUT)
        && write_json_line(&mut stdin, &models_request).is_ok()
        && let Some(models) = recv_pi_rpc_response(&rx, "waku-models", PI_RPC_TIMEOUT)
    {
        parse_pi_model_response(&state, &models)
    } else {
        Vec::new()
    };
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn parse_pi_model_response(state: &Value, response: &Value) -> Vec<ProviderModel> {
    let default_provider = state
        .pointer("/data/model/provider")
        .and_then(Value::as_str);
    let default_model = state.pointer("/data/model/id").and_then(Value::as_str);
    let default_slug = default_provider
        .zip(default_model)
        .map(|(provider, model)| format!("{provider}/{model}"));
    let current_thinking = state.pointer("/data/thinkingLevel").and_then(Value::as_str);

    response
        .pointer("/data/models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| {
            let provider = value.get("provider").and_then(Value::as_str)?;
            let model_id = value.get("id").and_then(Value::as_str)?;
            if provider.trim().is_empty() || model_id.trim().is_empty() {
                return None;
            }
            let slug = format!("{provider}/{model_id}");
            let name = value
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| display_name_from_slug(model_id));
            let mut model = ProviderModel::new(slug.clone(), name)
                .sub_provider(display_name_from_slug(provider));
            model.is_default = default_slug.as_deref() == Some(slug.as_str());
            model.reasoning_efforts = pi_reasoning_options(value);
            if !model.reasoning_efforts.is_empty() {
                let preferred = model
                    .is_default
                    .then_some(current_thinking)
                    .flatten()
                    .filter(|effort| {
                        model
                            .reasoning_efforts
                            .iter()
                            .any(|option| option.id == **effort)
                    })
                    .or_else(|| {
                        model
                            .reasoning_efforts
                            .iter()
                            .any(|option| option.id == "medium")
                            .then_some("medium")
                    })
                    .or_else(|| {
                        model
                            .reasoning_efforts
                            .first()
                            .map(|option| option.id.as_str())
                    });
                model.default_reasoning_effort = preferred.map(str::to_owned);
            }
            Some(model)
        })
        .collect()
}

fn pi_reasoning_options(model: &Value) -> Vec<ProviderModelOption> {
    if model.get("reasoning").and_then(Value::as_bool) != Some(true) {
        return Vec::new();
    }
    let level_map = model.get("thinkingLevelMap").and_then(Value::as_object);
    ["off", "minimal", "low", "medium", "high", "xhigh", "max"]
        .into_iter()
        .filter(|level| {
            let mapped = level_map.and_then(|map| map.get(*level));
            if matches!(*level, "xhigh" | "max") {
                mapped.is_some_and(|value| !value.is_null())
            } else {
                mapped.is_none_or(|value| !value.is_null())
            }
        })
        .map(|level| ProviderModelOption::new(level, reasoning_effort_label(level)))
        .collect()
}

fn discover_codex_models(binary: &Path) -> Vec<ProviderModel> {
    let Ok(mut child) = crate::command_env::command(binary)
        .args(["app-server", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        return Vec::new();
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        return Vec::new();
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                let _ = tx.send(value);
            }
        }
    });

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
        || recv_rpc_response(&rx, 0, CODEX_RPC_TIMEOUT).is_none()
        || write_json_line(&mut stdin, &json!({"method": "initialized", "params": {}})).is_err()
    {
        let _ = child.kill();
        return Vec::new();
    }

    let mut models = Vec::new();
    let mut cursor: Option<String> = None;
    for request_id in 1..=32_u64 {
        let params = cursor
            .as_ref()
            .map_or_else(|| json!({}), |cursor| json!({"cursor": cursor}));
        if write_json_line(
            &mut stdin,
            &json!({"method": "model/list", "id": request_id, "params": params}),
        )
        .is_err()
        {
            break;
        }
        let Some(response) = recv_rpc_response(&rx, request_id, CODEX_RPC_TIMEOUT) else {
            break;
        };
        models.extend(parse_codex_model_response(&response));
        cursor = response
            .pointer("/result/nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    models
}

fn parse_codex_model_response(response: &Value) -> Vec<ProviderModel> {
    response
        .pointer("/result/data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| {
            let id = value.get("model").and_then(Value::as_str)?;
            let name = value
                .get("displayName")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| display_name_from_slug(id));
            let mut model = ProviderModel::new(id, normalize_codex_name(&name));
            model.is_default = value
                .get("isDefault")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            model.reasoning_efforts = value
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|option| {
                    let id = option.get("reasoningEffort").and_then(Value::as_str)?;
                    let description = option
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    Some(
                        ProviderModelOption::new(id, reasoning_effort_label(id))
                            .description(description),
                    )
                })
                .collect();
            model.default_reasoning_effort = value
                .get("defaultReasoningEffort")
                .and_then(Value::as_str)
                .map(str::to_owned);
            model.service_tiers = value
                .get("serviceTiers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|tier| {
                    let id = tier.get("id").and_then(Value::as_str)?;
                    let name = tier
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|name| !name.trim().is_empty())
                        .unwrap_or(id);
                    let description = tier
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    Some(ProviderModelOption::new(id, name).description(description))
                })
                .collect();
            if model.service_tiers.is_empty() {
                model.service_tiers = value
                    .get("additionalSpeedTiers")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(|id| {
                        ProviderModelOption::new(
                            id,
                            if id == "fast" {
                                "Fast".to_owned()
                            } else {
                                display_name_from_slug(id)
                            },
                        )
                    })
                    .collect();
            }
            model.default_service_tier = value
                .get("defaultServiceTier")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| (!model.service_tiers.is_empty()).then(|| "default".to_owned()));
            Some(model)
        })
        .collect()
}

fn reasoning_effort_label(effort: &str) -> String {
    match effort {
        "none" => "None",
        "minimal" => "Minimal",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Extra High",
        "max" => "Max",
        "ultra" => "Ultra",
        other => return display_name_from_slug(other),
    }
    .to_owned()
}

fn reasoning_options<const N: usize>(efforts: [&str; N]) -> Vec<ProviderModelOption> {
    efforts
        .into_iter()
        .map(|effort| ProviderModelOption::new(effort, reasoning_effort_label(effort)))
        .collect()
}

fn claude_reasoning_model(id: &str, name: &str) -> ProviderModel {
    ProviderModel::new(id, name).reasoning(
        reasoning_options(["low", "medium", "high", "xhigh", "max"]),
        "high",
    )
}

fn recv_rpc_response(rx: &Receiver<Value>, id: u64, timeout: Duration) -> Option<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        let value = rx.recv_timeout(remaining).ok()?;
        if value.get("id").and_then(Value::as_u64) == Some(id) {
            return Some(value);
        }
    }
}

fn recv_pi_rpc_response(rx: &Receiver<Value>, id: &str, timeout: Duration) -> Option<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        let value = rx.recv_timeout(remaining).ok()?;
        if value.get("id").and_then(Value::as_str) == Some(id) {
            return (value.get("success").and_then(Value::as_bool) == Some(true)).then_some(value);
        }
    }
}

fn write_json_line(writer: &mut impl Write, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn normalize_codex_name(name: &str) -> String {
    let name = if name
        .get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("gpt"))
    {
        format!("GPT{}", &name[3..])
    } else {
        name.to_owned()
    };
    let mut capitalize_next = false;
    name.chars()
        .flat_map(|char| {
            if capitalize_next {
                capitalize_next = false;
                char.to_uppercase().collect::<Vec<_>>()
            } else {
                capitalize_next = char == '-';
                vec![char]
            }
        })
        .collect()
}

fn display_name_from_slug(slug: &str) -> String {
    let words = slug
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| match part.to_ascii_lowercase().as_str() {
            "gpt" => "GPT".to_owned(),
            "ai" => "AI".to_owned(),
            "xai" => "xAI".to_owned(),
            _ if part
                .chars()
                .all(|char| char.is_ascii_digit() || char == '.') =>
            {
                part.to_owned()
            }
            _ => {
                let mut chars = part.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().collect::<String>() + chars.as_str()
                })
            }
        })
        .collect::<Vec<_>>();
    if words.first().is_some_and(|word| word == "GPT") {
        words.join("-")
    } else {
        words.join(" ")
    }
}

fn strip_ansi(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(char) = chars.next() {
        if char == '\u{1b}' {
            for code in chars.by_ref() {
                if code.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            output.push(char);
        }
    }
    output
}

fn deduplicate(models: Vec<ProviderModel>) -> Vec<ProviderModel> {
    let mut seen = std::collections::HashSet::new();
    models
        .into_iter()
        .filter(|model| seen.insert(model.id.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_opencode_provider_qualified_models() {
        let models = parse_opencode_models(
            "opencode/big-pickle\n\u{1b}[32mgithub-copilot/gpt-5.4\u{1b}[0m\nnoise here\n",
        );
        assert_eq!(models.len(), 2);
        assert_eq!(models[1].id, "github-copilot/gpt-5.4");
        assert_eq!(models[1].name, "GPT-5.4");
        assert_eq!(models[1].sub_provider.as_deref(), Some("Github Copilot"));
    }

    #[test]
    fn parses_grok_default_and_available_models() {
        let models = parse_grok_models(
            "Default model: grok-4.5\n\nAvailable models:\n  * grok-4.5 (default)\n",
        );
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "grok-4.5");
        assert!(models[0].is_default);
    }

    #[test]
    fn parses_codex_model_list_metadata() {
        let response = json!({
            "id": 1,
            "result": {
                "data": [{
                    "model": "gpt-5.6-luna",
                    "displayName": "gpt-5.6-luna",
                    "isDefault": true,
                    "supportedReasoningEfforts": [
                        {"reasoningEffort": "low", "description": "Quick responses"},
                        {"reasoningEffort": "xhigh", "description": "Deep reasoning"}
                    ],
                    "defaultReasoningEffort": "xhigh",
                    "serviceTiers": [{
                        "id": "fast",
                        "name": "Fast",
                        "description": "Priority processing"
                    }],
                    "defaultServiceTier": "default"
                }]
            }
        });
        let models = parse_codex_model_response(&response);
        assert_eq!(models[0].name, "GPT-5.6-Luna");
        assert!(models[0].is_default);
        assert_eq!(models[0].reasoning_efforts[1].label, "Extra High");
        assert_eq!(models[0].default_reasoning_effort.as_deref(), Some("xhigh"));
        assert_eq!(models[0].service_tiers[0].id, "fast");
        assert_eq!(
            models[0].service_tiers[0].description.as_deref(),
            Some("Priority processing")
        );
    }

    #[test]
    fn parses_pi_models_and_model_specific_thinking_levels() {
        let state = json!({
            "id": "waku-state",
            "type": "response",
            "success": true,
            "data": {
                "model": {"provider": "github-copilot", "id": "gpt-5.6-terra"},
                "thinkingLevel": "xhigh"
            }
        });
        let response = json!({
            "id": "waku-models",
            "type": "response",
            "success": true,
            "data": {"models": [
                {
                    "provider": "github-copilot",
                    "id": "gpt-5.6-terra",
                    "name": "GPT-5.6 Terra",
                    "reasoning": true,
                    "thinkingLevelMap": {"off": null, "xhigh": "xhigh", "max": "max"}
                },
                {
                    "provider": "openai",
                    "id": "gpt-4o-mini",
                    "name": "GPT-4o mini",
                    "reasoning": false
                }
            ]}
        });

        let models = parse_pi_model_response(&state, &response);

        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "github-copilot/gpt-5.6-terra");
        assert_eq!(models[0].sub_provider.as_deref(), Some("Github Copilot"));
        assert!(models[0].is_default);
        assert_eq!(
            models[0]
                .reasoning_efforts
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["minimal", "low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(models[0].default_reasoning_effort.as_deref(), Some("xhigh"));
        assert!(models[1].reasoning_efforts.is_empty());
    }
}
