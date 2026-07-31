mod codex;
mod headless;

use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::Sender;

use crate::model::{DriverEvent, ProviderKind, ProviderResumeCursor, RuntimeMode};

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

pub fn start(
    provider: ProviderKind,
    binary: PathBuf,
    cwd: PathBuf,
    mode: RuntimeMode,
    model: Option<String>,
    provider_cursor: Option<ProviderResumeCursor>,
    events: Sender<DriverEvent>,
) -> anyhow::Result<DriverHandle> {
    let inner: Arc<dyn DriverControl> = match provider {
        ProviderKind::Codex => Arc::new(codex::CodexDriver::start(
            binary,
            cwd,
            mode,
            model,
            provider_cursor,
            events,
        )?),
        ProviderKind::Claude | ProviderKind::OpenCode | ProviderKind::Grok => {
            Arc::new(headless::HeadlessDriver::start(
                provider,
                binary,
                cwd,
                mode,
                model,
                provider_cursor,
                events,
            )?)
        }
    };
    Ok(DriverHandle { inner })
}
