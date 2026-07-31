use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::identity::DATA_DIRECTORY_NAME;
use crate::model::{AgentSession, FavoriteModel, Project, ProviderKind};

const STATE_VERSION: u32 = 3;
const OLDEST_SUPPORTED_STATE_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PersistedState {
    pub version: u32,
    pub projects: Vec<Project>,
    pub sessions: Vec<AgentSession>,
    pub selected_project: Option<Uuid>,
    pub selected_session: Option<Uuid>,
    pub last_provider: ProviderKind,
    #[serde(default)]
    pub favorite_models: Vec<FavoriteModel>,
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
            favorite_models: Vec::new(),
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
            favorite_models: Vec::new(),
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
            .join(DATA_DIRECTORY_NAME)
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
        let mut state = serde_json::from_slice::<PersistedState>(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if !(OLDEST_SUPPORTED_STATE_VERSION..=STATE_VERSION).contains(&state.version) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported Waku state version",
            ));
        }
        if state.version < 3 {
            for session in &mut state.sessions {
                session.migrate_pre_access_modes();
            }
        }
        state.version = STATE_VERSION;
        for session in &mut state.sessions {
            session.migrate_legacy_state();
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
        ActivityItem, ActivityKind, FavoriteModel, ReasoningBlock, TranscriptBlock,
        TranscriptBlockContent,
    };

    #[test]
    fn default_path_uses_build_specific_data_directory() {
        let path = StateStore::default_path();
        let data_directory = path.parent().and_then(Path::file_name);

        #[cfg(debug_assertions)]
        assert_eq!(data_directory, Some(std::ffi::OsStr::new("Waku Debug")));
        #[cfg(not(debug_assertions))]
        assert_eq!(data_directory, Some(std::ffi::OsStr::new("Waku")));
    }

    #[test]
    fn state_round_trips() {
        let directory = std::env::temp_dir().join(format!("waku-state-{}", Uuid::new_v4()));
        let store = StateStore::new(directory.join("state.json"));
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.sessions[0].model = Some("gpt-5.6-luna".into());
        state.sessions[0].reasoning_effort = Some("xhigh".into());
        state.sessions[0].service_tier = Some("fast".into());
        state.sessions[0].runtime_mode = crate::model::RuntimeMode::Auto;
        state.favorite_models.push(FavoriteModel {
            provider: ProviderKind::Codex,
            model: "gpt-5.6-luna".into(),
        });
        state.sessions[0].transcript_blocks.extend([
            TranscriptBlock {
                after_message: 1,
                turn_id: None,
                content: TranscriptBlockContent::Reasoning(ReasoningBlock {
                    content: "Checking the source".into(),
                    started_at_ms: 1_000,
                    finished_at_ms: 2_500,
                }),
            },
            TranscriptBlock {
                after_message: 1,
                turn_id: None,
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
        assert_eq!(restored.sessions[0].model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(
            restored.sessions[0].reasoning_effort.as_deref(),
            Some("xhigh")
        );
        assert_eq!(restored.sessions[0].service_tier.as_deref(), Some("fast"));
        assert_eq!(
            restored.sessions[0].runtime_mode,
            crate::model::RuntimeMode::Auto
        );
        assert_eq!(restored.favorite_models, state.favorite_models);
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
    fn version_one_sessions_migrate_provider_ids_and_turns() {
        let directory = std::env::temp_dir().join(format!("waku-v1-state-{}", Uuid::new_v4()));
        let path = directory.join("state.json");
        let store = StateStore::new(path.clone());
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.version = 1;
        state.sessions[0].messages.push(crate::model::Message::new(
            crate::model::MessageRole::User,
            "hello",
        ));
        state.sessions[0].messages.push(crate::model::Message::new(
            crate::model::MessageRole::Assistant,
            "hi",
        ));
        let mut value = serde_json::to_value(&state).unwrap();
        value.as_object_mut().unwrap().remove("favorite_models");
        value["sessions"][0]
            .as_object_mut()
            .unwrap()
            .remove("model");
        value["sessions"][0]["provider_session_id"] =
            serde_json::Value::String("thread-123".into());
        fs::create_dir_all(&directory).unwrap();
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

        let restored = store.load().unwrap();
        assert_eq!(restored.version, STATE_VERSION);
        assert!(restored.favorite_models.is_empty());
        assert!(restored.sessions[0].model.is_none());
        assert_eq!(
            restored.sessions[0]
                .provider_cursor
                .as_ref()
                .map(crate::model::ProviderResumeCursor::native_id),
            Some("thread-123")
        );
        assert_eq!(restored.sessions[0].turns.len(), 1);
        assert!(
            restored.sessions[0]
                .messages
                .iter()
                .all(|message| message.turn_id.is_some())
        );

        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn version_two_combined_modes_migrate_to_access_and_interaction_settings() {
        let directory = std::env::temp_dir().join(format!("waku-v2-{}", Uuid::new_v4()));
        let path = directory.join("state.json");
        let store = StateStore::new(path.clone());
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.version = 2;
        state.sessions[0].runtime_mode = crate::model::RuntimeMode::Plan;
        let mut auto_session =
            AgentSession::new(state.projects[0].id, crate::model::ProviderKind::Codex);
        auto_session.runtime_mode = crate::model::RuntimeMode::Auto;
        state.sessions.push(auto_session);
        fs::create_dir_all(&directory).unwrap();
        fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();

        let restored = store.load().unwrap();
        assert_eq!(restored.version, STATE_VERSION);
        assert_eq!(
            restored.sessions[0].runtime_mode,
            crate::model::RuntimeMode::Ask
        );
        assert_eq!(
            restored.sessions[0].interaction_mode,
            crate::model::InteractionMode::Plan
        );
        assert_eq!(
            restored.sessions[1].runtime_mode,
            crate::model::RuntimeMode::AutoAcceptEdits
        );
        assert_eq!(
            restored.sessions[1].interaction_mode,
            crate::model::InteractionMode::Build
        );

        fs::remove_dir_all(directory).ok();
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
