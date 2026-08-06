use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use serde_json::{Value, json};

use crate::model::ProviderResumeCursor;

const SERVER_START_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

pub fn fork_session_at_turn(
    binary: &Path,
    cwd: &Path,
    session_id: &str,
    retained_turns: usize,
) -> anyhow::Result<ProviderResumeCursor> {
    let server = OpenCodeServer::start(binary, cwd)?;
    let session_path = format!("/session/{}/message", encode_path_segment(session_id));
    let messages = server.request("GET", &session_path, None)?;
    let message_ids = messages
        .as_array()
        .ok_or_else(|| anyhow!("OpenCode returned an invalid message list"))?
        .iter()
        .filter_map(|message| {
            (is_native_user_turn(message))
                .then(|| message.pointer("/info/id").and_then(Value::as_str))
                .flatten()
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    let fork_at = fork_message_id(&message_ids, retained_turns)?;
    let body = fork_at.map_or_else(|| json!({}), |message_id| json!({"messageID": message_id}));
    let fork_path = format!("/session/{}/fork", encode_path_segment(session_id));
    let fork = server.request("POST", &fork_path, Some(&body))?;
    let fork_id = fork
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| fork.pointer("/data/id").and_then(Value::as_str))
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("OpenCode returned no forked session ID"))?;
    Ok(ProviderResumeCursor::OpenCode {
        session_id: fork_id.to_owned(),
    })
}

fn fork_message_id(message_ids: &[String], retained_turns: usize) -> anyhow::Result<Option<&str>> {
    if retained_turns > message_ids.len() {
        bail!(
            "OpenCode has only {} native turns, but Waku needs {retained_turns}",
            message_ids.len()
        );
    }
    Ok(message_ids.get(retained_turns).map(String::as_str))
}

pub(crate) struct OpenCodeServer {
    child: Child,
    pub(crate) port: u16,
    pid: u32,
}

impl OpenCodeServer {
    pub(crate) fn start(binary: &Path, cwd: &Path) -> anyhow::Result<Self> {
        Self::start_with_env(binary, cwd, &[])
    }

    /// Starts the server with extra environment, so a caller can hand it the
    /// Computer Use configuration the same way a one-shot invocation got it.
    pub(crate) fn start_with_env(
        binary: &Path,
        cwd: &Path,
        environment: &[(String, String)],
    ) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .context("could not reserve a local port for OpenCode")?;
        let port = listener.local_addr()?.port();
        drop(listener);

        let mut command = crate::command_env::command(binary);
        for (name, value) in environment {
            command.env(name, value);
        }
        let child = command
            .args([
                "serve",
                "--hostname",
                "127.0.0.1",
                "--port",
                &port.to_string(),
            ])
            .env("OPENCODE_SERVER_PASSWORD", "")
            .env("OPENCODE_SERVER_USERNAME", "opencode")
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to start `opencode serve`")?;
        let pid = child.id();
        let mut server = Self { child, port, pid };
        let started_at = Instant::now();
        loop {
            if server.request("GET", "/global/health", None).is_ok() {
                return Ok(server);
            }
            if let Some(status) = server.child.try_wait()? {
                bail!("OpenCode session server exited during startup ({status})");
            }
            if started_at.elapsed() >= SERVER_START_TIMEOUT {
                bail!("timed out starting the OpenCode session server");
            }
            thread::sleep(Duration::from_millis(40));
        }
    }

    pub(crate) fn request(&self, method: &str, path: &str, body: Option<&Value>) -> anyhow::Result<Value> {
        let body = body.map(serde_json::to_vec).transpose()?;
        let response = http_request(self.port, method, path, body.as_deref())?;
        serde_json::from_slice(&response)
            .with_context(|| format!("OpenCode returned invalid JSON for {method} {path}"))
    }
}

fn is_native_user_turn(message: &Value) -> bool {
    message.pointer("/info/role").and_then(Value::as_str) == Some("user")
        && message
            .get("parts")
            .and_then(Value::as_array)
            .is_some_and(|parts| {
                parts.iter().any(|part| {
                    part.get("type").and_then(Value::as_str) == Some("text")
                        && part.get("synthetic").and_then(Value::as_bool) != Some(true)
                })
            })
}

impl OpenCodeServer {
    /// Ends the server without owning it mutably.
    ///
    /// A long-lived reader blocked on the event stream keeps a handle alive, and
    /// that stream only closes when the server exits — so waiting for the last
    /// handle to drop deadlocks and leaks the process. The owner kills it
    /// directly instead, which closes the stream and releases the readers.
    pub(crate) fn shutdown(&self) {
        if self.pid == 0 {
            return;
        }
        #[cfg(unix)]
        {
            let _ = std::process::Command::new("/bin/kill")
                .args(["-TERM", &self.pid.to_string()])
                .status();
        }
    }
}

impl Drop for OpenCodeServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn http_request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
) -> anyhow::Result<Vec<u8>> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .with_context(|| format!("could not connect to OpenCode on local port {port}"))?;
    stream.set_read_timeout(Some(HTTP_TIMEOUT))?;
    stream.set_write_timeout(Some(HTTP_TIMEOUT))?;
    let body = body.unwrap_or_default();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()?;

    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    parse_http_response(&response)
}

fn parse_http_response(response: &[u8]) -> anyhow::Result<Vec<u8>> {
    let Some(header_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        bail!("OpenCode returned an invalid HTTP response");
    };
    let headers = std::str::from_utf8(&response[..header_end])?;
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .ok_or_else(|| anyhow!("OpenCode returned an invalid HTTP status"))?;
    let body = &response[header_end + 4..];
    let body = if headers.lines().any(|line| {
        line.eq_ignore_ascii_case("transfer-encoding: chunked")
            || line
                .to_ascii_lowercase()
                .starts_with("transfer-encoding: chunked")
    }) {
        decode_chunked(body)?
    } else {
        body.to_vec()
    };
    if !(200..300).contains(&status) {
        let detail = String::from_utf8_lossy(&body);
        bail!("OpenCode session request failed with HTTP {status}: {detail}");
    }
    Ok(body)
}

fn decode_chunked(mut input: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut output = Vec::new();
    loop {
        let Some(line_end) = input.windows(2).position(|window| window == b"\r\n") else {
            bail!("OpenCode returned an invalid chunked response");
        };
        let size_text = std::str::from_utf8(&input[..line_end])?
            .split(';')
            .next()
            .unwrap_or_default();
        let size = usize::from_str_radix(size_text.trim(), 16)
            .context("OpenCode returned an invalid HTTP chunk size")?;
        input = &input[line_end + 2..];
        if size == 0 {
            break;
        }
        if input.len() < size + 2 || &input[size..size + 2] != b"\r\n" {
            bail!("OpenCode returned a truncated HTTP chunk");
        }
        output.extend_from_slice(&input[..size]);
        input = &input[size + 2..];
    }
    Ok(output)
}

pub(crate) fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_fork_message_excludes_the_next_user_turn() {
        let messages = vec!["one".to_owned(), "two".to_owned(), "three".to_owned()];
        assert_eq!(fork_message_id(&messages, 0).unwrap(), Some("one"));
        assert_eq!(fork_message_id(&messages, 2).unwrap(), Some("three"));
        assert_eq!(fork_message_id(&messages, 3).unwrap(), None);
        assert!(fork_message_id(&messages, 4).is_err());
    }

    #[test]
    fn native_turn_filter_ignores_compaction_and_synthetic_user_messages() {
        assert!(is_native_user_turn(&json!({
            "info": {"role": "user"},
            "parts": [{"type": "text", "text": "hello"}]
        })));
        assert!(!is_native_user_turn(&json!({
            "info": {"role": "user"},
            "parts": [{"type": "compaction", "auto": true}]
        })));
        assert!(!is_native_user_turn(&json!({
            "info": {"role": "user"},
            "parts": [{"type": "text", "text": "continue", "synthetic": true}]
        })));
    }

    #[test]
    fn parses_content_length_and_chunked_http_responses() {
        assert_eq!(
            parse_http_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}").unwrap(),
            b"{}"
        );
        assert_eq!(
            parse_http_response(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"id\r\n4\r\n\":1}\r\n0\r\n\r\n"
            )
            .unwrap(),
            b"{\"id\":1}"
        );
    }
}
