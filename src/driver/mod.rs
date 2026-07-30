mod codex;
mod grok;
mod headless;

use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::Sender;

use crate::model::{DriverEvent, ProviderKind, RuntimeMode};

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

    pub fn respond(&self, request_id: String, option_id: String) {
        self.inner.respond(request_id, option_id);
    }
}

pub trait DriverControl: Send + Sync {
    fn prompt(&self, prompt: String);
    fn cancel(&self);
    fn respond(&self, request_id: String, option_id: String);
}

pub fn start(
    provider: ProviderKind,
    binary: PathBuf,
    cwd: PathBuf,
    mode: RuntimeMode,
    provider_session_id: Option<String>,
    events: Sender<DriverEvent>,
) -> anyhow::Result<DriverHandle> {
    let inner: Arc<dyn DriverControl> = match provider {
        ProviderKind::Codex => Arc::new(codex::CodexDriver::start(
            binary,
            cwd,
            mode,
            provider_session_id,
            events,
        )?),
        ProviderKind::Claude | ProviderKind::OpenCode => Arc::new(headless::HeadlessDriver::start(
            provider,
            binary,
            cwd,
            mode,
            provider_session_id,
            events,
        )?),
        ProviderKind::Grok => Arc::new(grok::GrokDriver::start(
            binary,
            cwd,
            mode,
            provider_session_id,
            events,
        )?),
    };
    Ok(DriverHandle { inner })
}
