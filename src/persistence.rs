use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::{AgentSession, Project, ProviderKind};

const STATE_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PersistedState {
    pub version: u32,
    pub projects: Vec<Project>,
    pub sessions: Vec<AgentSession>,
    pub selected_project: Option<Uuid>,
    pub selected_session: Option<Uuid>,
    pub last_provider: ProviderKind,
}

impl PersistedState {
    pub fn empty() -> Self {
        Self {
            version: STATE_VERSION,
            projects: Vec::new(),
            sessions: Vec::new(),
            selected_project: None,
            selected_session: None,
            last_provider: ProviderKind::Codex,
        }
    }

    pub fn fresh(cwd: PathBuf) -> Self {
        let project = Project::from_path(cwd);
        let session = AgentSession::new(project.id, ProviderKind::Codex);
        Self {
            version: STATE_VERSION,
            selected_project: Some(project.id),
            selected_session: Some(session.id),
            projects: vec![project],
            sessions: vec![session],
            last_provider: ProviderKind::Codex,
        }
    }
}

pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    pub fn default_path() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("Waku")
            .join("state.json")
    }

    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load_or_fresh(&self, cwd: PathBuf) -> PersistedState {
        self.load().unwrap_or_else(|_| {
            if cwd.parent().is_none() {
                PersistedState::empty()
            } else {
                PersistedState::fresh(cwd)
            }
        })
    }

    pub fn load(&self) -> io::Result<PersistedState> {
        let bytes = fs::read(&self.path)?;
        let state = serde_json::from_slice::<PersistedState>(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if state.version != STATE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported Waku state version",
            ));
        }
        Ok(state)
    }

    pub fn save(&self, state: &PersistedState) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_vec_pretty(state)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temporary_path = temporary_path(&self.path);
        fs::write(&temporary_path, data)?;
        fs::rename(temporary_path, &self.path)
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    PathBuf::from(temporary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ActivityItem, ActivityKind, ReasoningBlock, TranscriptBlock, TranscriptBlockContent,
    };

    #[test]
    fn state_round_trips() {
        let directory = std::env::temp_dir().join(format!("waku-state-{}", Uuid::new_v4()));
        let store = StateStore::new(directory.join("state.json"));
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.sessions[0].transcript_blocks.extend([
            TranscriptBlock {
                after_message: 1,
                content: TranscriptBlockContent::Reasoning(ReasoningBlock {
                    content: "Checking the source".into(),
                    started_at_ms: 1_000,
                    finished_at_ms: 2_500,
                }),
            },
            TranscriptBlock {
                after_message: 1,
                content: TranscriptBlockContent::Activities(vec![ActivityItem::new(
                    Some("tool-1".into()),
                    ActivityKind::Search,
                    "Read src/main.rs",
                    Some("{\"path\":\"src/main.rs\"}".into()),
                    true,
                )]),
            },
        ]);
        store.save(&state).unwrap();
        let restored = store.load().unwrap();
        assert_eq!(restored.projects[0].name, "project");
        assert_eq!(restored.sessions.len(), 1);
        assert_eq!(restored.sessions[0].transcript_blocks.len(), 2);
        assert!(matches!(
            &restored.sessions[0].transcript_blocks[0].content,
            TranscriptBlockContent::Reasoning(reasoning)
                if reasoning.content == "Checking the source"
        ));
        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn sessions_without_transcript_blocks_remain_compatible() {
        let session = AgentSession::new(Uuid::new_v4(), ProviderKind::Grok);
        let mut value = serde_json::to_value(session).unwrap();
        value.as_object_mut().unwrap().remove("transcript_blocks");

        let restored = serde_json::from_value::<AgentSession>(value).unwrap();
        assert!(restored.transcript_blocks.is_empty());
    }

    #[test]
    fn app_bundle_root_directory_starts_with_onboarding() {
        let store = StateStore::new(
            std::env::temp_dir()
                .join(format!("waku-empty-{}", Uuid::new_v4()))
                .join("state.json"),
        );
        let state = store.load_or_fresh(PathBuf::from("/"));
        assert!(state.projects.is_empty());
        assert!(state.selected_session.is_none());
    }
}
