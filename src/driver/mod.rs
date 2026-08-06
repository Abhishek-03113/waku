mod acp;
mod activity;
mod amp;
mod claude;
mod codex;
mod computer_use;
mod opencode;
mod support;
mod pi;

use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::Sender;

use crate::computer_use::ComputerToolRequest;
use crate::model::{DriverEvent, InteractionMode, ProviderKind, ProviderResumeCursor, RuntimeMode};

#[derive(Clone)]
pub struct DriverHandle {
    inner: Arc<dyn DriverControl>,
}

impl DriverHandle {
    pub fn prompt(&self, prompt: String) {
        self.inner.prompt(prompt);
    }

    pub fn cancel(&self) {
        self.inner.cancel();
    }

    pub fn cancel_computer_use(&self) {
        self.inner.cancel_computer_use();
    }

    pub fn respond(&self, request_id: String, option_id: String) {
        self.inner.respond(request_id, option_id);
    }

    pub fn run_computer_tool(&self, request: ComputerToolRequest) {
        self.inner.run_computer_tool(request);
    }

    pub fn reject_computer_tool(&self, request: ComputerToolRequest, reason: String) {
        self.inner.reject_computer_tool(request, reason);
    }

    pub fn apply_options(&self, options: SessionOptions) -> bool {
        self.inner.apply_options(options)
    }

    pub fn rollback(&self, turns: usize) -> anyhow::Result<Option<ProviderResumeCursor>> {
        self.inner.rollback(turns)
    }

    pub fn fork(&self, turns_to_remove: usize) -> anyhow::Result<ProviderResumeCursor> {
        self.inner.fork(turns_to_remove)
    }
}

pub trait DriverControl: Send + Sync {
    fn prompt(&self, prompt: String);
    fn cancel(&self);
    fn cancel_computer_use(&self) {}
    fn respond(&self, request_id: String, option_id: String);
    fn run_computer_tool(&self, _request: ComputerToolRequest) {}
    fn reject_computer_tool(&self, _request: ComputerToolRequest, _reason: String) {}
    /// Applies changed turn options to the live session, returning whether the
    /// transport could do it without being restarted. A `false` answer is the
    /// driver asking to be torn down and recreated with the new options.
    fn apply_options(&self, _options: SessionOptions) -> bool {
        false
    }
    fn rollback(&self, turns: usize) -> anyhow::Result<Option<ProviderResumeCursor>>;
    fn fork(&self, _turns_to_remove: usize) -> anyhow::Result<ProviderResumeCursor> {
        anyhow::bail!("conversation forking is not supported by this provider transport")
    }
}

pub struct DriverStartOptions {
    pub binary: PathBuf,
    pub cwd: PathBuf,
    pub mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    pub computer_use_enabled: bool,
    pub provider_cursor: Option<ProviderResumeCursor>,
}

/// The subset of `DriverStartOptions` a user can change without starting a new
/// task. Transports that carry these per turn can absorb a change in place;
/// the rest have to be restarted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionOptions {
    pub mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
}

pub fn start(
    provider: ProviderKind,
    options: DriverStartOptions,
    events: Sender<DriverEvent>,
) -> anyhow::Result<DriverHandle> {
    let inner: Arc<dyn DriverControl> = match provider {
        ProviderKind::Codex => Arc::new(codex::CodexDriver::start(options, events)?),
        ProviderKind::Pi => Arc::new(pi::PiDriver::start(options, events)?),
        // Cursor and Grok both serve a long-lived ACP session, which is the only
        // way their Supervised mode can actually ask the user rather than
        // silently forcing or denying.
        ProviderKind::Cursor | ProviderKind::Grok => {
            Arc::new(acp::AcpDriver::start(provider, options, events)?)
        }
        // OpenCode's own server is its real API, and it is what exposes
        // interactive permission requests.
        ProviderKind::OpenCode => Arc::new(opencode::OpenCodeDriver::start(options, events)?),
        // Claude serves a realtime stream of user messages on stdin — the same
        // transport the Agent SDK drives — which is what lets its Supervised
        // mode ask rather than decide alone.
        ProviderKind::Claude => Arc::new(claude::ClaudeDriver::start(options, events)?),
        // Amp reads newline-delimited user messages on stdin and stays alive
        // until stdin closes, so it too serves the whole conversation.
        ProviderKind::Amp => Arc::new(amp::AmpDriver::start(options, events)?),
    };
    Ok(DriverHandle { inner })
}
