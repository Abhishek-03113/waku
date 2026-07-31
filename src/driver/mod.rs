mod codex;
mod headless;

use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::Sender;

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

    pub fn respond(&self, request_id: String, option_id: String) {
        self.inner.respond(request_id, option_id);
    }

    pub fn rollback(&self, turns: usize) -> anyhow::Result<()> {
        self.inner.rollback(turns)
    }
}

pub trait DriverControl: Send + Sync {
    fn prompt(&self, prompt: String);
    fn cancel(&self);
    fn respond(&self, request_id: String, option_id: String);
    fn rollback(&self, turns: usize) -> anyhow::Result<()>;
}

pub struct DriverStartOptions {
    pub binary: PathBuf,
    pub cwd: PathBuf,
    pub mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    pub provider_cursor: Option<ProviderResumeCursor>,
}

pub fn start(
    provider: ProviderKind,
    options: DriverStartOptions,
    events: Sender<DriverEvent>,
) -> anyhow::Result<DriverHandle> {
    let inner: Arc<dyn DriverControl> = match provider {
        ProviderKind::Codex => Arc::new(codex::CodexDriver::start(options, events)?),
        ProviderKind::Claude | ProviderKind::OpenCode | ProviderKind::Grok => {
            Arc::new(headless::HeadlessDriver::start(provider, options, events)?)
        }
    };
    Ok(DriverHandle { inner })
}
