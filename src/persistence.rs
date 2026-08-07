//! Local state storage.
//!
//! Sessions and projects live in SQLite (`app.db`), settings in a readable
//! `settings.json` beside it, and binary payloads in [`crate::blob_store`].
//!
//! A save writes only the rows whose contents changed, so a streaming turn
//! costs a few kilobytes no matter how much history exists. Fields the sidebar
//! sorts on are promoted to columns so listing sessions never has to
//! deserialize a transcript. The schema is defined in `db/schema.ts` and
//! applied by [`apply_migrations`].

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::blob_store::BlobStore;
use crate::computer_use::ComputerAppGrant;
use crate::identity::DATA_DIRECTORY_NAME;
use crate::model::{
    AgentSession, FavoriteModel, InteractionMode, Message, Project, ProviderKind, RuntimeMode,
    TranscriptBlockContent,
};
use crate::theme::ThemePreference;

const STATE_VERSION: u32 = 5;
const OLDEST_SUPPORTED_STATE_VERSION: u32 = 1;

pub const DEFAULT_SIDEBAR_WIDTH: f32 = 252.0;
pub const DEFAULT_RIGHT_PANEL_WIDTH: f32 = 460.0;

fn default_panel_visibility() -> bool {
    true
}

fn default_computer_use_enabled() -> bool {
    true
}

fn default_sidebar_width() -> f32 {
    DEFAULT_SIDEBAR_WIDTH
}

fn default_right_panel_width() -> f32 {
    DEFAULT_RIGHT_PANEL_WIDTH
}

/// Everything except projects and sessions. Small enough to rewrite wholesale
/// on any change, so it lives in a single row.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AppSettings {
    pub version: u32,
    pub selected_project: Option<Uuid>,
    pub selected_session: Option<Uuid>,
    pub last_provider: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_service_tier: Option<String>,
    #[serde(default)]
    pub favorite_models: Vec<FavoriteModel>,
    #[serde(default)]
    pub theme: ThemePreference,
    #[serde(default = "default_panel_visibility")]
    pub sidebar_visible: bool,
    #[serde(default = "default_panel_visibility")]
    pub right_panel_visible: bool,
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,
    #[serde(default = "default_right_panel_width")]
    pub right_panel_width: f32,
    #[serde(default = "default_computer_use_enabled")]
    pub computer_use_enabled: bool,
    #[serde(default)]
    pub computer_use_allowed_apps: Vec<ComputerAppGrant>,
    /// Providers switched off for new sessions in the Providers settings.
    #[serde(default)]
    pub disabled_providers: Vec<ProviderKind>,
    /// Per-provider binary overrides from the Providers settings; empty means
    /// detect from PATH.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub provider_binary_overrides: HashMap<ProviderKind, String>,
}

/// The legacy single-document format, still read once to migrate.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PersistedState {
    pub version: u32,
    pub projects: Vec<Project>,
    pub sessions: Vec<AgentSession>,
    pub selected_project: Option<Uuid>,
    pub selected_session: Option<Uuid>,
    pub last_provider: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_service_tier: Option<String>,
    #[serde(default)]
    pub favorite_models: Vec<FavoriteModel>,
    #[serde(default)]
    pub theme: ThemePreference,
    #[serde(default = "default_panel_visibility")]
    pub sidebar_visible: bool,
    #[serde(default = "default_panel_visibility")]
    pub right_panel_visible: bool,
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,
    #[serde(default = "default_right_panel_width")]
    pub right_panel_width: f32,
    #[serde(default = "default_computer_use_enabled")]
    pub computer_use_enabled: bool,
    #[serde(default)]
    pub computer_use_allowed_apps: Vec<ComputerAppGrant>,
    /// Providers switched off for new sessions in the Providers settings.
    #[serde(default)]
    pub disabled_providers: Vec<ProviderKind>,
    /// Per-provider binary overrides from the Providers settings; empty means
    /// detect from PATH.
    #[serde(default)]
    pub provider_binary_overrides: HashMap<ProviderKind, String>,
    /// Sessions changed since the last save.
    ///
    /// The app knows what it touched, so it says so rather than making the
    /// store rediscover it. Every `&mut AgentSession` is handed out by
    /// [`Self::session_mut`], which records the id here; a save then writes
    /// exactly these rows instead of re-serializing the whole history to work
    /// out what moved.
    #[serde(skip)]
    dirty_sessions: HashSet<Uuid>,
}

impl PersistedState {
    /// The only way to get a mutable session. Marks it for the next save.
    pub fn session_mut(&mut self, id: Uuid) -> Option<&mut AgentSession> {
        let session = self.sessions.iter_mut().find(|session| session.id == id)?;
        self.dirty_sessions.insert(id);
        Some(session)
    }

    /// Records a session as changed without borrowing it, for the few paths
    /// that mutate through a slice or add a session outright.
    pub fn mark_session_dirty(&mut self, id: Uuid) {
        self.dirty_sessions.insert(id);
    }

    pub fn push_session(&mut self, session: AgentSession) {
        self.dirty_sessions.insert(session.id);
        self.sessions.push(session);
    }

    pub fn empty() -> Self {
        Self {
            version: STATE_VERSION,
            projects: Vec::new(),
            sessions: Vec::new(),
            selected_project: None,
            selected_session: None,
            last_provider: ProviderKind::Codex,
            last_model: None,
            last_reasoning_effort: None,
            last_service_tier: None,
            favorite_models: Vec::new(),
            theme: ThemePreference::System,
            sidebar_visible: true,
            right_panel_visible: true,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            right_panel_width: DEFAULT_RIGHT_PANEL_WIDTH,
            computer_use_enabled: true,
            computer_use_allowed_apps: Vec::new(),
            disabled_providers: Vec::new(),
            provider_binary_overrides: HashMap::new(),
            dirty_sessions: HashSet::new(),
        }
    }

    pub fn fresh(cwd: PathBuf) -> Self {
        let project = Project::from_path(cwd);
        let session = AgentSession::new(project.id, ProviderKind::Codex);
        Self {
            selected_project: Some(project.id),
            selected_session: Some(session.id),
            projects: vec![project],
            sessions: vec![session],
            ..Self::empty()
        }
    }

    pub fn new_session(&self, project_id: Uuid, provider: ProviderKind) -> AgentSession {
        let mut session = AgentSession::new(project_id, provider);
        if provider == self.last_provider {
            session.model.clone_from(&self.last_model);
            session
                .reasoning_effort
                .clone_from(&self.last_reasoning_effort);
            session.service_tier.clone_from(&self.last_service_tier);
        }
        session
    }

    fn settings(&self) -> AppSettings {
        AppSettings {
            version: STATE_VERSION,
            selected_project: self.selected_project,
            selected_session: self.persistable_selected_session(),
            last_provider: self.last_provider,
            last_model: self.last_model.clone(),
            last_reasoning_effort: self.last_reasoning_effort.clone(),
            last_service_tier: self.last_service_tier.clone(),
            favorite_models: self.favorite_models.clone(),
            theme: self.theme,
            sidebar_visible: self.sidebar_visible,
            right_panel_visible: self.right_panel_visible,
            sidebar_width: self.sidebar_width,
            right_panel_width: self.right_panel_width,
            computer_use_enabled: self.computer_use_enabled,
            computer_use_allowed_apps: self.computer_use_allowed_apps.clone(),
            disabled_providers: self.disabled_providers.clone(),
            provider_binary_overrides: self.provider_binary_overrides.clone(),
        }
    }

    fn apply_settings(&mut self, settings: AppSettings) {
        self.version = STATE_VERSION;
        self.selected_project = settings.selected_project;
        self.selected_session = settings.selected_session;
        self.last_provider = settings.last_provider;
        self.last_model = settings.last_model;
        self.last_reasoning_effort = settings.last_reasoning_effort;
        self.last_service_tier = settings.last_service_tier;
        self.favorite_models = settings.favorite_models;
        self.theme = settings.theme;
        self.sidebar_visible = settings.sidebar_visible;
        self.right_panel_visible = settings.right_panel_visible;
        self.sidebar_width = settings.sidebar_width;
        self.right_panel_width = settings.right_panel_width;
        self.computer_use_enabled = settings.computer_use_enabled;
        self.computer_use_allowed_apps = settings.computer_use_allowed_apps;
        self.disabled_providers = settings.disabled_providers;
        self.provider_binary_overrides = settings.provider_binary_overrides;
    }

    /// A session only earns a row once it has started; drafts stay in memory.
    /// A draft selection is stored as no selection at all, so relaunching
    /// recreates a draft and lands on the new-session page the user quit from.
    fn persistable_selected_session(&self) -> Option<Uuid> {
        self.selected_session.filter(|selected| {
            self.sessions
                .iter()
                .any(|session| session.id == *selected && session.has_started())
        })
    }

    fn ensure_runtime_session(&mut self) {
        if self.selected_session.is_some_and(|selected_session| {
            self.sessions
                .iter()
                .any(|session| session.id == selected_session)
        }) {
            return;
        }
        self.selected_session = None;
        let Some(project_id) = self.selected_project.filter(|selected_project| {
            self.projects
                .iter()
                .any(|project| project.id == *selected_project)
        }) else {
            return;
        };
        let session = self.new_session(project_id, self.last_provider);
        self.selected_session = Some(session.id);
        self.sessions.push(session);
    }

    fn migrate_loaded(&mut self) {
        for session in &mut self.sessions {
            let before = (
                session.turns.len(),
                session.last_reply_at,
                session.provider_cursor.is_some(),
            );
            session.migrate_legacy_state();
            session.backfill_last_reply_at();
            // Migration rewrote this session, so the stored row is stale.
            if before
                != (
                    session.turns.len(),
                    session.last_reply_at,
                    session.provider_cursor.is_some(),
                )
            {
                self.dirty_sessions.insert(session.id);
            }
        }
        self.version = STATE_VERSION;
        normalize_computer_app_grants(&mut self.computer_use_allowed_apps);
        self.backfill_remembered_selection();
    }

    fn backfill_remembered_selection(&mut self) {
        let Some(session) = self
            .selected_session
            .and_then(|selected| self.sessions.iter().find(|session| session.id == selected))
            .cloned()
        else {
            return;
        };
        if self.last_model.is_none() {
            self.last_model = session.model;
        }
        if self.last_reasoning_effort.is_none() {
            self.last_reasoning_effort = session.reasoning_effort;
        }
        if self.last_service_tier.is_none() {
            self.last_service_tier = session.service_tier;
        }
    }
}

/// Rewrites inline `data:` payloads into blob references, in place.
///
/// Done on the way to disk so a screenshot is written once and then dropped
/// from memory: the transcript keeps a short reference, and rendering loads the
/// file through GPUI's image cache instead of base64-decoding on every frame.
fn externalize_blobs<'a>(
    sessions: impl IntoIterator<Item = &'a mut AgentSession>,
    blobs: &BlobStore,
) {
    for session in sessions {
        for block in &mut session.transcript_blocks {
            let TranscriptBlockContent::Activities(activities) = &mut block.content else {
                continue;
            };
            for activity in activities {
                for image in &mut activity.image_urls {
                    if crate::blob_store::is_blob_reference(image) {
                        continue;
                    }
                    let stored = blobs.store_data_url(image);
                    if stored.len() < image.len() {
                        *image = stored;
                    }
                }
            }
        }
    }
}

/// Every blob reference named by any stored session.
///
/// Read from the database rather than from memory: a session that has not been
/// hydrated has an empty transcript in memory, and treating that as "owns no
/// images" would delete screenshots that are still in use.
fn live_blob_references(connection: &Connection) -> io::Result<HashSet<String>> {
    let mut statement = connection
        .prepare("SELECT data FROM session_details")
        .map_err(to_io_error)?;
    let mut references = HashSet::new();
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(to_io_error)?;
    for data in rows.filter_map(Result::ok) {
        // Scanning the raw JSON keeps this independent of how deeply the
        // reference is nested inside a transcript block.
        let mut rest = data.as_str();
        while let Some(start) = rest.find(crate::blob_store::BLOB_SCHEME) {
            rest = &rest[start..];
            let end = rest.find('"').unwrap_or(rest.len());
            references.insert(rest[..end].to_owned());
            rest = &rest[end..];
        }
    }
    Ok(references)
}

fn fingerprint(value: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn to_io_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

include!(concat!(env!("OUT_DIR"), "/migrations.rs"));

const MIGRATIONS_TABLE: &str = "CREATE TABLE IF NOT EXISTS migrations (
         tag        TEXT PRIMARY KEY,
         applied_at INTEGER NOT NULL
     )";

/// Brings a database up to the latest schema.
///
/// Migrations are authored in `db/schema.ts` and generated by
/// `bun run db:generate`; `build.rs` embeds the resulting SQL in filename
/// order. Each one that is not already named in `migrations` runs in its own
/// transaction and is recorded, so applying is idempotent.
pub fn apply_migrations(connection: &Connection) -> io::Result<usize> {
    connection
        .execute_batch(MIGRATIONS_TABLE)
        .map_err(to_io_error)?;
    let mut applied = 0;
    for (tag, sql) in MIGRATIONS {
        let already_applied: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM migrations WHERE tag = ?1)",
                params![tag],
                |row| row.get(0),
            )
            .map_err(to_io_error)?;
        if already_applied {
            continue;
        }
        let transaction = connection.unchecked_transaction().map_err(to_io_error)?;
        transaction
            .execute_batch(sql)
            .map_err(|error| io::Error::other(format!("migration {tag} failed: {error}")))?;
        transaction
            .execute(
                "INSERT INTO migrations(tag, applied_at) VALUES(?1, ?2)",
                params![tag, crate::model::unix_time() as i64],
            )
            .map_err(to_io_error)?;
        transaction.commit().map_err(to_io_error)?;
        applied += 1;
    }
    Ok(applied)
}

struct Storage {
    connection: Connection,
    /// Sessions known to have a row. Used to spot deletions and to catch a
    /// session that became persistable without being marked dirty.
    persisted_sessions: HashSet<Uuid>,
    /// Per session, the fingerprint of each message row as this connection last
    /// wrote it, so a save only touches the messages that actually changed.
    /// See [`write_messages`].
    written_messages: HashMap<Uuid, HashMap<Uuid, u64>>,
    saved_projects: u64,
    saved_settings: u64,
}

pub struct StateStore {
    path: PathBuf,
    /// Settings are a few hundred bytes and worth keeping hand-editable, so
    /// they stay a plain JSON file beside the database.
    settings_path: PathBuf,
    storage: Mutex<Option<Storage>>,
    blobs: Arc<BlobStore>,
}

impl StateStore {
    /// Where the database lives.
    ///
    /// Debug builds keep it in the checkout's gitignored `temp/`, so
    /// development never touches the installed app's data and a bad state is
    /// thrown away by deleting one directory. Release builds use the usual
    /// per-user application support directory.
    pub fn default_path() -> PathBuf {
        if cfg!(debug_assertions) {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("temp")
                .join("app.db")
        } else {
            dirs::data_local_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join(DATA_DIRECTORY_NAME)
                .join("app.db")
        }
    }

    pub fn new(path: PathBuf) -> Self {
        let directory = path.parent().unwrap_or_else(|| Path::new(".")).to_owned();
        let root = directory.join("blobs");
        crate::blob_store::set_shared_root(root.clone());
        let blobs = Arc::new(BlobStore::new(root));
        Self {
            settings_path: directory.join("settings.json"),
            path,
            storage: Mutex::new(None),
            blobs,
        }
    }

    #[cfg(test)]
    pub fn blobs(&self) -> Arc<BlobStore> {
        Arc::clone(&self.blobs)
    }

    fn open(&self) -> io::Result<Connection> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(&self.path).map_err(to_io_error)?;
        // WAL keeps a streaming save from blocking on readers, and NORMAL
        // sync is the right durability trade for per-second UI state.
        connection
            .execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")
            .map_err(to_io_error)?;
        apply_migrations(&connection)?;
        Ok(connection)
    }

    pub fn load_or_fresh(&self, cwd: PathBuf) -> PersistedState {
        let mut state = self.load().unwrap_or_else(|_| {
            if cwd.parent().is_none() {
                PersistedState::empty()
            } else {
                PersistedState::fresh(cwd)
            }
        });
        state.ensure_runtime_session();
        // The session that opens on launch is the one session whose transcript
        // is needed immediately; the rest stay as list rows until selected.
        if let Some(selected) = state.selected_session
            && let Some(session) = state
                .sessions
                .iter_mut()
                .find(|session| session.id == selected)
        {
            let _ = self.hydrate(session);
        }
        state
    }

    fn read_settings(&self) -> io::Result<Option<AppSettings>> {
        let Ok(bytes) = fs::read(&self.settings_path) else {
            return Ok(None);
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(to_io_error)
    }

    fn write_settings(&self, settings: &AppSettings) -> io::Result<()> {
        if let Some(parent) = self.settings_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_vec_pretty(settings).map_err(to_io_error)?;
        let temporary = self.settings_path.with_extension("json.tmp");
        fs::write(&temporary, data)?;
        fs::rename(temporary, &self.settings_path)
    }

    pub fn load(&self) -> io::Result<PersistedState> {
        let connection = self.open()?;
        let mut state = PersistedState::empty();

        // A missing settings file just means defaults; the database is still
        // the source of truth for projects and sessions.
        let settings = self.read_settings()?;
        let from_version = settings
            .as_ref()
            .map_or(STATE_VERSION, |settings| settings.version);
        if !(OLDEST_SUPPORTED_STATE_VERSION..=STATE_VERSION).contains(&from_version) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported Waku state version",
            ));
        }
        if let Some(settings) = settings {
            state.apply_settings(settings);
        }

        let mut projects = connection
            .prepare("SELECT id, name, path, created_at FROM projects ORDER BY position")
            .map_err(to_io_error)?;
        state.projects = projects
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(to_io_error)?
            .filter_map(Result::ok)
            .filter_map(|(id, name, path, created_at)| {
                Some(Project {
                    id: Uuid::parse_str(&id).ok()?,
                    name,
                    path: PathBuf::from(path),
                    created_at: created_at as u64,
                })
            })
            .collect();
        drop(projects);

        // Only the columns the session list needs. Transcripts and messages are
        // fetched per session by `hydrate`, so startup cost does not grow with
        // how much history exists.
        let mut sessions = connection
            .prepare(
                "SELECT id, project_id, title, provider, model, status,
                        created_at, updated_at, last_reply_at
                 FROM sessions ORDER BY updated_at",
            )
            .map_err(to_io_error)?;
        let mut persisted_sessions = HashSet::new();
        state.sessions = sessions
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                ))
            })
            .map_err(to_io_error)?
            .filter_map(Result::ok)
            .filter_map(|row| {
                let session = session_skeleton(row)?;
                persisted_sessions.insert(session.id);
                Some(session)
            })
            .collect();
        drop(sessions);

        state.migrate_loaded();

        *self.storage.lock() = Some(Storage {
            connection,
            persisted_sessions,
            // A fresh connection has written nothing yet. Sessions loaded here
            // are skeletons anyway, so the first save of one is a full write.
            written_messages: HashMap::new(),
            saved_projects: 0,
            saved_settings: 0,
        });
        Ok(state)
    }

    /// Fills in a session's transcript, turns and messages.
    ///
    /// Startup loads only list columns, so this runs when a session is first
    /// selected. It reads one row plus that session's messages — cheap enough
    /// to do inline, and a no-op once the session is already loaded.
    pub fn hydrate(&self, session: &mut AgentSession) -> io::Result<()> {
        if session.detail_loaded {
            return Ok(());
        }
        let mut guard = self.storage.lock();
        if guard.is_none() {
            *guard = Some(Storage {
                connection: self.open()?,
                persisted_sessions: HashSet::new(),
                written_messages: HashMap::new(),
                saved_projects: 0,
                saved_settings: 0,
            });
        }
        let connection = &guard.as_ref().expect("storage opened above").connection;
        let id = session.id.to_string();

        let data: Option<String> = connection
            .query_row(
                "SELECT data FROM session_details WHERE session_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(to_io_error)?;
        // A session with no row has nothing stored to load; it is already whole.
        let Some(data) = data else {
            session.detail_loaded = true;
            return Ok(());
        };
        let stored = serde_json::from_str::<AgentSession>(&data).map_err(to_io_error)?;
        session.transcript_blocks = stored.transcript_blocks;
        session.turns = stored.turns;
        session.provider_cursor = stored.provider_cursor;
        session.runtime_mode = stored.runtime_mode;
        session.interaction_mode = stored.interaction_mode;
        session.reasoning_effort = stored.reasoning_effort;
        session.service_tier = stored.service_tier;

        let mut statement = connection
            .prepare(
                "SELECT id, turn_id, role, content, created_at, streaming
                 FROM messages WHERE session_id = ?1 ORDER BY position",
            )
            .map_err(to_io_error)?;
        session.messages = statement
            .query_map(params![id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(to_io_error)?
            .filter_map(Result::ok)
            .filter_map(message_from_row)
            .collect();

        session.detail_loaded = true;
        Ok(())
    }

    /// Persists whatever the app marked as changed, so a streaming turn writes
    /// one session row and a selection change writes no rows at all.
    pub fn save(&self, state: &mut PersistedState) -> io::Result<()> {
        // Only changed sessions can hold a new inline payload, so the blob walk
        // follows the same set rather than every transcript on every save.
        let dirty = state.dirty_sessions.clone();
        externalize_blobs(
            state
                .sessions
                .iter_mut()
                .filter(|session| dirty.contains(&session.id)),
            &self.blobs,
        );

        let mut guard = self.storage.lock();
        if guard.is_none() {
            *guard = Some(Storage {
                connection: self.open()?,
                persisted_sessions: HashSet::new(),
                written_messages: HashMap::new(),
                saved_projects: 0,
                saved_settings: 0,
            });
        }
        let storage = guard.as_mut().expect("storage opened above");

        let settings = state.settings();
        let settings_fingerprint =
            fingerprint(&serde_json::to_string(&settings).map_err(to_io_error)?);
        if settings_fingerprint != storage.saved_settings {
            self.write_settings(&settings)?;
            storage.saved_settings = settings_fingerprint;
        }

        let transaction = storage
            .connection
            .unchecked_transaction()
            .map_err(to_io_error)?;

        let projects = serde_json::to_string(&state.projects).map_err(to_io_error)?;
        let projects_fingerprint = fingerprint(&projects);
        if projects_fingerprint != storage.saved_projects {
            transaction
                .execute("DELETE FROM projects", [])
                .map_err(to_io_error)?;
            for (position, project) in state.projects.iter().enumerate() {
                transaction
                    .execute(
                        INSERT_PROJECT,
                        params![
                            project.id.to_string(),
                            project.name,
                            project.path.to_string_lossy(),
                            position as i64,
                            project.created_at as i64
                        ],
                    )
                    .map_err(to_io_error)?;
            }
            storage.saved_projects = projects_fingerprint;
        }

        // Only sessions the app reported as changed are written. A draft that
        // has not started yet owns no row, so it counts as removed until it does.
        let mut live = HashSet::with_capacity(state.sessions.len());
        // Applied only after the commit below, so a transaction that rolls back
        // does not leave this connection believing rows it never wrote are on
        // disk — which would make the next save skip them for good.
        let mut written_messages = Vec::new();
        for session in state
            .sessions
            .iter()
            .filter(|session| session.has_started())
        {
            live.insert(session.id);
            // A skeleton's empty transcript means "not fetched", not "empty".
            // Writing one would erase the stored history.
            if !session.detail_loaded {
                continue;
            }
            if !state.dirty_sessions.contains(&session.id)
                && storage.persisted_sessions.contains(&session.id)
            {
                continue;
            }
            let data = session_data(session)?;
            transaction
                .execute(
                    UPSERT_SESSION,
                    rusqlite::params_from_iter(session_params(session)),
                )
                .map_err(to_io_error)?;
            transaction
                .execute(UPSERT_SESSION_DETAIL, params![session.id.to_string(), data])
                .map_err(to_io_error)?;
            written_messages.push((
                session.id,
                write_messages(
                    &transaction,
                    session,
                    storage.written_messages.get(&session.id).unwrap_or(&EMPTY),
                )?,
            ));
            storage.persisted_sessions.insert(session.id);
        }

        let removed = storage
            .persisted_sessions
            .iter()
            .copied()
            .filter(|id| !live.contains(id))
            .collect::<Vec<_>>();
        for id in removed {
            let key = id.to_string();
            transaction
                .execute("DELETE FROM sessions WHERE id = ?1", params![key])
                .map_err(to_io_error)?;
            transaction
                .execute(
                    "DELETE FROM session_details WHERE session_id = ?1",
                    params![key],
                )
                .map_err(to_io_error)?;
            transaction
                .execute("DELETE FROM messages WHERE session_id = ?1", params![key])
                .map_err(to_io_error)?;
            storage.persisted_sessions.remove(&id);
            storage.written_messages.remove(&id);
        }

        transaction.commit().map_err(to_io_error)?;
        // Now that the rows are durable, and not before.
        for (session_id, fingerprints) in written_messages {
            storage.written_messages.insert(session_id, fingerprints);
        }
        state.dirty_sessions.clear();
        Ok(())
    }

    /// Builds a blob sweep.
    ///
    /// Both halves are filesystem and database work, so the whole thing runs on
    /// a background executor; it opens its own connection rather than borrowing
    /// the store's.
    pub fn blob_sweep(&self) -> impl FnOnce() + Send + 'static {
        let blobs = Arc::clone(&self.blobs);
        let path = self.path.clone();
        move || {
            let Ok(connection) = Connection::open(&path) else {
                return;
            };
            let Ok(live) = live_blob_references(&connection) else {
                return;
            };
            let _ = blobs.retain(&live);
        }
    }
}

type SessionColumns = (
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    i64,
    i64,
    Option<i64>,
);

/// Builds a list-only session from its columns. `messages`,
/// `transcript_blocks` and `turns` stay empty until [`StateStore::hydrate`].
///
/// Built field by field rather than through `AgentSession::new`, which would
/// spend a random-number syscall per row on an id that is then overwritten.
fn session_skeleton(row: SessionColumns) -> Option<AgentSession> {
    let (id, project_id, title, provider, model, status, created_at, updated_at, last_reply_at) =
        row;
    Some(AgentSession {
        id: Uuid::parse_str(&id).ok()?,
        title,
        project_id: Uuid::parse_str(&project_id).ok()?,
        provider: serde_json::from_value(serde_json::Value::String(provider)).ok()?,
        model,
        // Hydration replaces these; the list never reads them.
        runtime_mode: RuntimeMode::default(),
        interaction_mode: InteractionMode::default(),
        reasoning_effort: None,
        service_tier: None,
        status: serde_json::from_value(serde_json::Value::String(status)).ok()?,
        created_at: created_at as u64,
        updated_at: updated_at as u64,
        last_reply_at: last_reply_at.map(|at| at as u64),
        provider_cursor: None,
        available_commands: Vec::new(),
        provider_session_id: None,
        messages: Vec::new(),
        transcript_blocks: Vec::new(),
        turns: Vec::new(),
        detail_loaded: false,
    })
}

type MessageColumns = (String, Option<String>, String, String, i64, i64);

fn message_from_row(row: MessageColumns) -> Option<Message> {
    let (id, turn_id, role, content, created_at, streaming) = row;
    Some(Message {
        id: Uuid::parse_str(&id).ok()?,
        turn_id: turn_id.as_deref().and_then(|id| Uuid::parse_str(id).ok()),
        role: serde_json::from_value(serde_json::Value::String(role)).ok()?,
        content,
        created_at: created_at as u64,
        streaming: streaming != 0,
    })
}

/// Serializes a session for the `data` column, omitting `messages`.
///
/// They are rows in `messages` instead, so there is no copy in `data` that
/// could go stale.
fn session_data(session: &AgentSession) -> io::Result<String> {
    let mut value = serde_json::to_value(session).map_err(to_io_error)?;
    if let Some(object) = value.as_object_mut() {
        object.remove("messages");
    }
    serde_json::to_string(&value).map_err(to_io_error)
}

const UPSERT_MESSAGE: &str = "INSERT INTO messages(
         id, session_id, turn_id, position, role, content, created_at, streaming
     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
     ON CONFLICT(id) DO UPDATE SET
         session_id = excluded.session_id,
         turn_id    = excluded.turn_id,
         position   = excluded.position,
         role       = excluded.role,
         content    = excluded.content,
         created_at = excluded.created_at,
         streaming  = excluded.streaming";

/// Replaces a session's messages with the given list.
///
/// Appending during a turn touches only the new rows; the delete clears any
/// tail left behind when a conversation is forked or truncated.
/// Writes the messages whose stored row would actually differ.
///
/// A streaming turn saves once a second, and every one of those saves used to
/// re-upsert the whole transcript: a `to_string` of the id, a clone of the
/// body, a `serde_json` round-trip for the role, and a statement, per message.
/// At two thousand messages that is upwards of 15ms — several frames — spent
/// rewriting rows that are byte-for-byte what SQLite already holds.
///
/// `written` is what the connection was last told, keyed by message id, so the
/// comparison costs one hash of each body instead of a write of it. Returns the
/// map to remember for next time; the caller installs it only once the
/// transaction commits, so a rolled-back write is not recorded as done.
fn write_messages(
    transaction: &Connection,
    session: &AgentSession,
    written: &HashMap<Uuid, u64>,
) -> io::Result<HashMap<Uuid, u64>> {
    use rusqlite::types::Value;
    let session_id = session.id.to_string();
    let mut current = HashMap::with_capacity(session.messages.len());
    for (position, message) in session.messages.iter().enumerate() {
        let fingerprint = message_fingerprint(message, position);
        current.insert(message.id, fingerprint);
        if written.get(&message.id) == Some(&fingerprint) {
            continue;
        }
        transaction
            .execute(
                UPSERT_MESSAGE,
                rusqlite::params_from_iter([
                    Value::Text(message.id.to_string()),
                    Value::Text(session_id.clone()),
                    message
                        .turn_id
                        .map_or(Value::Null, |id| Value::Text(id.to_string())),
                    Value::Integer(position as i64),
                    Value::Text(tag_of(message.role)),
                    Value::Text(message.content.clone()),
                    Value::Integer(message.created_at as i64),
                    Value::Integer(i64::from(message.streaming)),
                ]),
            )
            .map_err(to_io_error)?;
    }
    transaction
        .execute(
            "DELETE FROM messages WHERE session_id = ?1 AND position >= ?2",
            params![session_id, session.messages.len() as i64],
        )
        .map_err(to_io_error)?;
    Ok(current)
}

/// Fingerprint of every column [`write_messages`] stores for a message.
///
/// Covers the body as well as the metadata: an edit that preserves length —
/// a typo fix — has to be caught, so length and position alone will not do.
/// Folded a word at a time because this runs over the whole transcript on
/// every save, which is what [`fingerprint`]'s byte-at-a-time loop is too slow
/// for.
/// Stand-in for "this connection has written nothing for that session yet".
static EMPTY: std::sync::LazyLock<HashMap<Uuid, u64>> = std::sync::LazyLock::new(HashMap::new);

fn message_fingerprint(message: &Message, position: usize) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut fold = |value: u64| {
        hash ^= value;
        hash = hash.wrapping_mul(PRIME).rotate_left(23);
    };

    fold(position as u64);
    fold(message.created_at);
    fold(u64::from(message.streaming));
    fold(fingerprint(&tag_of(message.role)));
    let (high, low) = message.id.as_u64_pair();
    fold(high);
    fold(low);
    match message.turn_id {
        Some(turn_id) => {
            let (high, low) = turn_id.as_u64_pair();
            fold(high);
            fold(low);
        }
        // Distinct from a turn id that happens to be zero.
        None => fold(u64::MAX),
    }

    let bytes = message.content.as_bytes();
    let mut chunks = bytes.chunks_exact(8);
    for chunk in &mut chunks {
        fold(u64::from_le_bytes(
            chunk.try_into().expect("chunks_exact yields 8 bytes"),
        ));
    }
    let mut tail = [0u8; 8];
    let remainder = chunks.remainder();
    tail[..remainder.len()].copy_from_slice(remainder);
    fold(u64::from_le_bytes(tail));
    fold(bytes.len() as u64);
    hash
}

/// Columns the sidebar sorts and filters on are stored alongside the JSON so
/// listing sessions never has to deserialize a transcript.
const UPSERT_SESSION: &str = "INSERT INTO sessions(
         id, project_id, title, provider, model, status,
         created_at, updated_at, last_reply_at
     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
     ON CONFLICT(id) DO UPDATE SET
         project_id    = excluded.project_id,
         title         = excluded.title,
         provider      = excluded.provider,
         model         = excluded.model,
         status        = excluded.status,
         created_at    = excluded.created_at,
         updated_at    = excluded.updated_at,
         last_reply_at = excluded.last_reply_at";

const INSERT_PROJECT: &str = "INSERT INTO projects(id, name, path, position, created_at)
     VALUES(?1, ?2, ?3, ?4, ?5)
     ON CONFLICT(id) DO UPDATE SET
         name       = excluded.name,
         path       = excluded.path,
         position   = excluded.position,
         created_at = excluded.created_at";

/// The transcript, written alongside the list row it belongs to.
const UPSERT_SESSION_DETAIL: &str = "INSERT INTO session_details(session_id, data)
     VALUES(?1, ?2)
     ON CONFLICT(session_id) DO UPDATE SET data = excluded.data";

/// Serializes an enum to the same string the JSON blob uses, so a column and
/// its JSON counterpart can never disagree about spelling.
fn tag_of(value: impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn session_params(session: &AgentSession) -> Vec<rusqlite::types::Value> {
    use rusqlite::types::Value;
    vec![
        Value::Text(session.id.to_string()),
        Value::Text(session.project_id.to_string()),
        Value::Text(session.title.clone()),
        Value::Text(tag_of(session.provider)),
        session.model.clone().map_or(Value::Null, Value::Text),
        Value::Text(tag_of(session.status)),
        Value::Integer(session.created_at as i64),
        Value::Integer(session.updated_at as i64),
        session
            .last_reply_at
            .map_or(Value::Null, |at| Value::Integer(at as i64)),
    ]
}

fn normalize_computer_app_grants(grants: &mut Vec<ComputerAppGrant>) {
    let mut seen_bundle_ids = HashSet::new();
    grants.retain(|grant| {
        !grant.bundle_id.trim().is_empty() && seen_bundle_ids.insert(grant.bundle_id.clone())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ActivityItem, ActivityKind, FavoriteModel, MessageRole, ReasoningBlock, TranscriptBlock,
        TranscriptBlockContent,
    };
    use base64::Engine as _;

    fn temporary_directory() -> PathBuf {
        std::env::temp_dir().join(format!("waku-state-{}", Uuid::new_v4()))
    }

    fn store_in(directory: &Path) -> StateStore {
        StateStore::new(directory.join("app.db"))
    }

    /// `load` returns list-only sessions by design; tests that assert on
    /// transcripts fetch them the way the app does when a session is opened.
    fn load_hydrated(store: &StateStore) -> PersistedState {
        let mut state = store.load().unwrap();
        for session in &mut state.sessions {
            store.hydrate(session).unwrap();
        }
        state
    }

    #[test]
    fn projects_round_trip_as_columns_with_created_at() {
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/some project"));
        let project = state.projects[0].clone();
        assert!(project.created_at > 0, "a new project is dated");
        store.save(&mut state).unwrap();

        // Stored as columns, not as a JSON blob.
        let connection = Connection::open(directory.join("app.db")).unwrap();
        let (name, path, created_at): (String, String, i64) = connection
            .query_row(
                "SELECT name, path, created_at FROM projects WHERE id = ?1",
                params![project.id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(name, project.name);
        assert_eq!(path, project.path.to_string_lossy());
        assert_eq!(created_at as u64, project.created_at);
        drop(connection);

        let restored = store_in(&directory).load().unwrap();
        assert_eq!(restored.projects[0].name, project.name);
        assert_eq!(restored.projects[0].path, project.path);
        assert_eq!(restored.projects[0].created_at, project.created_at);

        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn load_returns_list_columns_and_hydrate_fills_the_transcript() {
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        let id = state.sessions[0].id;
        state.sessions[0].title = "Investigate".into();
        state.sessions[0].begin_turn("Ask");
        state.sessions[0].push_message(MessageRole::Assistant, "an answer");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        store.save(&mut state).unwrap();

        let reopened = store_in(&directory);
        let mut restored = reopened.load().unwrap();
        let session = &restored.sessions[0];
        // The list has everything it renders...
        assert_eq!(session.title, "Investigate");
        assert_eq!(session.id, id);
        assert!(session.last_reply_at.is_some());
        // ...and none of what it does not.
        assert!(!session.detail_loaded);
        assert!(session.messages.is_empty());
        assert!(session.turns.is_empty());
        // A skeleton still counts as started, since only started sessions
        // are stored at all.
        assert!(session.has_started());

        reopened.hydrate(&mut restored.sessions[0]).unwrap();
        let session = &restored.sessions[0];
        assert!(session.detail_loaded);
        assert_eq!(session.turns.len(), 1);
        assert!(
            session
                .messages
                .iter()
                .any(|message| message.content == "an answer")
        );

        fs::remove_dir_all(directory).ok();
    }

    /// Rows this store's connection has inserted, updated or deleted.
    fn rows_written(store: &StateStore) -> u64 {
        store
            .storage
            .lock()
            .as_ref()
            .expect("the store has saved at least once")
            .connection
            .total_changes()
    }

    /// A streaming turn saves once a second, and every one of those saves used
    /// to rewrite the entire transcript — measurably several frames of work at
    /// a couple of thousand messages. Counted in rows, because rows written is
    /// exactly what used to grow with history.
    #[test]
    fn a_save_only_writes_the_messages_that_changed() {
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        let id = state.sessions[0].id;
        for turn in 0..20 {
            state.sessions[0].begin_turn(format!("prompt {turn}"));
            state.sessions[0].push_message(MessageRole::Assistant, format!("reply {turn}"));
            state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        }
        store.save(&mut state).unwrap();
        assert!(state.sessions[0].messages.len() >= 40);

        // A dirty session always rewrites its own two rows; that is the floor
        // the message count is measured against.
        let before = rows_written(&store);
        state.mark_session_dirty(id);
        store.save(&mut state).unwrap();
        let floor = rows_written(&store) - before;

        let before = rows_written(&store);
        state
            .session_mut(id)
            .unwrap()
            .push_message(MessageRole::Assistant, "one more");
        store.save(&mut state).unwrap();
        assert_eq!(
            rows_written(&store) - before,
            floor + 1,
            "a new message costs one row, not the whole transcript"
        );

        // Length is not enough to tell rows apart: a typo fix keeps it.
        let before = rows_written(&store);
        let session = state.session_mut(id).unwrap();
        let original = session.messages[3].content.clone();
        let edited = original.chars().rev().collect::<String>();
        assert_eq!(edited.len(), original.len());
        assert_ne!(edited, original);
        session.messages[3].content = edited.clone();
        store.save(&mut state).unwrap();
        assert_eq!(
            rows_written(&store) - before,
            floor + 1,
            "an edit that preserves length is still written"
        );

        let reopened = store_in(&directory);
        let mut restored = reopened.load().unwrap();
        reopened.hydrate(&mut restored.sessions[0]).unwrap();
        let messages = &restored.sessions[0].messages;
        assert_eq!(messages[3].content, edited, "the edit reached disk");
        assert_eq!(
            messages.last().unwrap().content,
            "one more",
            "and so did the append"
        );

        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn a_skeleton_is_never_written_back_over_stored_history() {
        // The failure this guards against is silent and total: saving a session
        // whose transcript was never fetched would replace it with nothing.
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.sessions[0].begin_turn("Ask");
        state.sessions[0].push_message(MessageRole::User, "keep me");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        store.save(&mut state).unwrap();

        let reopened = store_in(&directory);
        let mut restored = reopened.load().unwrap();
        assert!(!restored.sessions[0].detail_loaded);
        // Mark it dirty anyway, the worst case.
        let id = restored.sessions[0].id;
        restored.mark_session_dirty(id);
        reopened.save(&mut restored).unwrap();

        let checked = load_hydrated(&store_in(&directory));
        assert_eq!(checked.sessions[0].turns.len(), 1, "turns survived");
        assert!(
            checked.sessions[0]
                .messages
                .iter()
                .any(|message| message.content == "keep me"),
            "messages survived"
        );

        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn blob_sweep_keeps_images_of_sessions_that_are_not_loaded() {
        // Sweeping from memory would treat an unhydrated session as owning no
        // images and delete screenshots that are still referenced.
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        let payload = vec![4u8; 32 * 1024];
        let data_url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&payload)
        );
        let id = state.sessions[0].id;
        state.sessions[0].begin_turn("Screenshot");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        state
            .session_mut(id)
            .unwrap()
            .transcript_blocks
            .push(TranscriptBlock {
                after_message: 0,
                turn_id: None,
                content: TranscriptBlockContent::Activities(vec![
                    ActivityItem::new(None, ActivityKind::Tool, "Screenshot", None, true)
                        .with_image_urls(vec![data_url]),
                ]),
            });
        store.save(&mut state).unwrap();

        // Reopen without hydrating anything, then sweep.
        let reopened = store_in(&directory);
        let restored = reopened.load().unwrap();
        assert!(!restored.sessions[0].detail_loaded);
        reopened.blob_sweep()();

        let checked = load_hydrated(&store_in(&directory));
        let TranscriptBlockContent::Activities(activities) =
            &checked.sessions[0].transcript_blocks[0].content
        else {
            panic!("expected activities");
        };
        let path = store
            .blobs()
            .path_for(&activities[0].image_urls[0])
            .unwrap();
        assert_eq!(fs::read(path).unwrap(), payload, "the image survived");

        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn default_path_is_build_specific() {
        let path = StateStore::default_path();
        assert_eq!(path.file_name(), Some(std::ffi::OsStr::new("app.db")));
        let directory = path.parent().and_then(Path::file_name);

        // Debug builds stay inside the checkout so development never writes to
        // the installed app's data.
        #[cfg(debug_assertions)]
        {
            assert_eq!(directory, Some(std::ffi::OsStr::new("temp")));
            assert!(path.starts_with(env!("CARGO_MANIFEST_DIR")));
        }
        #[cfg(not(debug_assertions))]
        assert_eq!(directory, Some(std::ffi::OsStr::new("Waku")));
    }

    #[test]
    fn state_round_trips() {
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.sessions[0].model = Some("gpt-5.6-luna".into());
        state.last_model = Some("gpt-5.6-luna".into());
        state.sessions[0].reasoning_effort = Some("xhigh".into());
        state.last_reasoning_effort = Some("xhigh".into());
        state.sessions[0].service_tier = Some("fast".into());
        state.last_service_tier = Some("fast".into());
        state.sessions[0].runtime_mode = crate::model::RuntimeMode::Auto;
        state.favorite_models.push(FavoriteModel {
            provider: ProviderKind::Codex,
            model: "gpt-5.6-luna".into(),
        });
        state.theme = ThemePreference::Light;
        state.sidebar_visible = false;
        state.right_panel_visible = false;
        state.sidebar_width = 318.0;
        state.right_panel_width = 612.0;
        state.computer_use_enabled = false;
        state.computer_use_allowed_apps.push(ComputerAppGrant {
            bundle_id: "com.apple.Safari".into(),
            app_name: "Safari".into(),
        });
        state.sessions[0].begin_turn("Persist this session");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
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
        store.save(&mut state).unwrap();

        let restored = load_hydrated(&store_in(&directory));
        assert_eq!(restored.projects[0].name, "project");
        assert_eq!(restored.sessions.len(), 1);
        assert_eq!(restored.sessions[0].model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(restored.last_model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(restored.last_reasoning_effort.as_deref(), Some("xhigh"));
        assert_eq!(restored.last_service_tier.as_deref(), Some("fast"));
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
        assert_eq!(restored.theme, ThemePreference::Light);
        assert!(!restored.sidebar_visible);
        assert!(!restored.right_panel_visible);
        assert_eq!(restored.sidebar_width, 318.0);
        assert_eq!(restored.right_panel_width, 612.0);
        assert!(!restored.computer_use_enabled);
        assert_eq!(
            restored.computer_use_allowed_apps,
            state.computer_use_allowed_apps
        );
        assert_eq!(restored.sessions[0].transcript_blocks.len(), 2);
        assert!(matches!(
            &restored.sessions[0].transcript_blocks[0].content,
            TranscriptBlockContent::Reasoning(reasoning)
                if reasoning.content == "Checking the source"
        ));
        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn unchanged_sessions_are_not_rewritten() {
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.sessions[0].begin_turn("First");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        let quiet = {
            let mut session = state.new_session(state.projects[0].id, ProviderKind::Codex);
            session.begin_turn("Quiet");
            session.finish_active_turn(crate::model::TurnStatus::Completed);
            session
        };
        let quiet_id = quiet.id;
        state.sessions.push(quiet);
        store.save(&mut state).unwrap();

        // Stamp the quiet session's row so any write would overwrite the mark.
        let connection = Connection::open(directory.join("app.db")).unwrap();
        connection
            .execute(
                "UPDATE sessions SET title = 'untouched' WHERE id = ?1",
                params![quiet_id.to_string()],
            )
            .unwrap();

        // Change the other session, through the accessor that marks it dirty.
        let active_id = state.sessions[0].id;
        let session = state.session_mut(active_id).unwrap();
        session.begin_turn("Second");
        session.finish_active_turn(crate::model::TurnStatus::Completed);
        store.save(&mut state).unwrap();

        let quiet_title: String = connection
            .query_row(
                "SELECT title FROM sessions WHERE id = ?1",
                params![quiet_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            quiet_title, "untouched",
            "a session nobody touched was not rewritten"
        );
        drop(connection);

        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn a_session_changed_without_the_accessor_is_not_written() {
        // The dirty set is the contract: bypassing `session_mut` means the
        // change does not reach disk. This pins that so the invariant is
        // visible rather than discovered later as data loss.
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        let id = state.sessions[0].id;
        state.sessions[0].begin_turn("First");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        store.save(&mut state).unwrap();

        state.sessions[0].title = "bypassed".into();
        store.save(&mut state).unwrap();
        assert_eq!(
            store_in(&directory).load().unwrap().sessions[0].title,
            "New task",
            "an unmarked change stays in memory"
        );

        // Going through the accessor persists it.
        state.session_mut(id).unwrap().title = "marked".into();
        store.save(&mut state).unwrap();
        assert_eq!(
            store_in(&directory).load().unwrap().sessions[0].title,
            "marked"
        );

        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn a_new_session_is_written_even_without_being_marked() {
        // Safety net: a session with no row yet is always written, so a missed
        // mark can never lose a whole session.
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.sessions[0].begin_turn("Unmarked");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        state.dirty_sessions.clear();

        store.save(&mut state).unwrap();

        assert_eq!(store_in(&directory).load().unwrap().sessions.len(), 1);
        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn settings_live_in_a_readable_json_file() {
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.theme = ThemePreference::Light;
        state.sidebar_width = 301.0;
        store.save(&mut state).unwrap();

        let settings = directory.join("settings.json");
        let text = fs::read_to_string(&settings).unwrap();
        assert!(
            text.contains('\n'),
            "settings are pretty-printed for editing"
        );
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["sidebar_width"], 301.0);
        // Session history stays in the database, not in the settings file.
        assert!(value.get("sessions").is_none());
        assert!(value.get("projects").is_none());

        // A hand edit is picked up on the next load.
        let edited = text.replace("301.0", "277.0");
        fs::write(&settings, edited).unwrap();
        assert_eq!(store_in(&directory).load().unwrap().sidebar_width, 277.0);

        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn migrations_run_once_and_are_recorded() {
        let connection = Connection::open_in_memory().unwrap();

        assert_eq!(
            apply_migrations(&connection).unwrap(),
            MIGRATIONS.len(),
            "all run on a fresh database"
        );

        let recorded: Vec<String> = connection
            .prepare("SELECT tag FROM migrations ORDER BY tag")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert_eq!(
            recorded,
            MIGRATIONS
                .iter()
                .map(|(tag, _)| tag.to_string())
                .collect::<Vec<_>>()
        );

        // Re-running is a no-op; a second CREATE TABLE would otherwise error.
        assert_eq!(apply_migrations(&connection).unwrap(), 0);
    }

    #[test]
    fn recorded_migrations_are_skipped() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(MIGRATIONS_TABLE).unwrap();
        // Claim every migration already ran. The tables do not exist, so
        // anything that did run would fail loudly.
        for (tag, _) in MIGRATIONS {
            connection
                .execute(
                    "INSERT INTO migrations(tag, applied_at) VALUES(?1, 0)",
                    params![tag],
                )
                .unwrap();
        }

        assert_eq!(apply_migrations(&connection).unwrap(), 0);
        assert!(
            connection
                .query_row::<i64, _, _>("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
                .is_err(),
            "nothing ran, so the schema was never created"
        );
    }

    #[test]
    fn a_half_applied_run_resumes_from_where_it_stopped() {
        let connection = Connection::open_in_memory().unwrap();
        apply_migrations(&connection).unwrap();

        // Drop the record of the last migration without dropping its tables,
        // as an interrupted run would leave things.
        let (last, _) = MIGRATIONS.last().expect("at least one migration");
        connection
            .execute("DELETE FROM migrations WHERE tag = ?1", params![last])
            .unwrap();

        // It re-runs and fails loudly rather than silently skipping, because
        // the tables it creates already exist.
        let error = apply_migrations(&connection).unwrap_err();
        assert!(
            error.to_string().contains(last),
            "the failure names the migration: {error}"
        );
    }

    #[test]
    fn messages_round_trip_through_their_own_table() {
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.sessions[0].begin_turn("Ask");
        state.sessions[0].push_message(MessageRole::User, "how do I center a div");
        state.sessions[0].push_message(MessageRole::Assistant, "flexbox");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        let expected = state.sessions[0].messages.clone();
        store.save(&mut state).unwrap();

        // The JSON column must not carry a second copy that could drift.
        let connection = Connection::open(directory.join("app.db")).unwrap();
        let data: String = connection
            .query_row("SELECT data FROM session_details LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(
            !serde_json::from_str::<serde_json::Value>(&data)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("messages"),
            "messages live only in their own table"
        );
        drop(connection);

        let restored = load_hydrated(&store_in(&directory));
        let messages = &restored.sessions[0].messages;
        assert_eq!(messages.len(), expected.len());
        assert!(expected.len() >= 2, "the turn and both replies are present");
        for (restored, expected) in messages.iter().zip(&expected) {
            assert_eq!(restored.id, expected.id);
            assert_eq!(restored.role, expected.role);
            assert_eq!(restored.content, expected.content);
            assert_eq!(restored.turn_id, expected.turn_id);
            assert_eq!(restored.created_at, expected.created_at);
            assert_eq!(restored.streaming, expected.streaming);
        }

        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn truncating_a_conversation_drops_the_orphaned_message_rows() {
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.sessions[0].begin_turn("First");
        state.sessions[0].push_message(MessageRole::User, "one");
        state.sessions[0].push_message(MessageRole::Assistant, "two");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        store.save(&mut state).unwrap();

        let id = state.sessions[0].id;
        state.session_mut(id).unwrap().messages.truncate(1);
        store.save(&mut state).unwrap();

        let connection = Connection::open(directory.join("app.db")).unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "the tail row was deleted, not left behind");
        drop(connection);

        assert_eq!(
            load_hydrated(&store_in(&directory)).sessions[0]
                .messages
                .len(),
            1
        );
        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn deleting_a_session_removes_its_messages() {
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.sessions[0].begin_turn("Keep");
        state.sessions[0].push_message(MessageRole::User, "keep me");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        let mut extra = state.new_session(state.projects[0].id, ProviderKind::Codex);
        extra.begin_turn("Remove");
        extra.push_message(MessageRole::User, "delete me");
        extra.finish_active_turn(crate::model::TurnStatus::Completed);
        let removed_id = extra.id;
        state.sessions.push(extra);
        store.save(&mut state).unwrap();

        state.sessions.retain(|session| session.id != removed_id);
        store.save(&mut state).unwrap();

        let connection = Connection::open(directory.join("app.db")).unwrap();
        let orphans: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                params![removed_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0);

        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn a_message_edit_alone_marks_the_session_dirty() {
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.sessions[0].begin_turn("Ask");
        state.sessions[0].push_message(MessageRole::User, "before");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        store.save(&mut state).unwrap();

        // Nothing outside the message list changes, so the session JSON is
        // identical; only the message row differs.
        let id = state.sessions[0].id;
        state.session_mut(id).unwrap().messages[0].content = "after".into();
        store.save(&mut state).unwrap();

        assert_eq!(
            load_hydrated(&store_in(&directory)).sessions[0].messages[0].content,
            "after"
        );
        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn promoted_columns_match_the_json_payload() {
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.sessions[0].title = "Investigate the parser".into();
        state.sessions[0].model = Some("gpt-5.6-luna".into());
        state.sessions[0].begin_turn("Go");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        let session = state.sessions[0].clone();
        store.save(&mut state).unwrap();

        let connection = Connection::open(directory.join("app.db")).unwrap();
        let (title, provider, model, status, created, updated, last_reply): (
            String,
            String,
            Option<String>,
            String,
            i64,
            i64,
            Option<i64>,
        ) = connection
            .query_row(
                "SELECT title, provider, model, status, created_at, updated_at, last_reply_at
                 FROM sessions WHERE id = ?1",
                params![session.id.to_string()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(title, "Investigate the parser");
        assert_eq!(provider, tag_of(session.provider));
        assert_eq!(model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(status, tag_of(session.status));
        assert_eq!(created as u64, session.created_at);
        assert_eq!(updated as u64, session.updated_at);
        assert_eq!(last_reply.map(|at| at as u64), session.last_reply_at);
        assert!(last_reply.is_some(), "a finished turn sets last_reply_at");

        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn last_reply_at_tracks_replies_not_every_edit() {
        let mut session = AgentSession::new(Uuid::new_v4(), ProviderKind::Codex);
        assert!(session.last_reply_at.is_none(), "no reply yet");

        session.begin_turn("Ask");
        assert!(session.last_reply_at.is_none(), "still running");
        session.finish_active_turn(crate::model::TurnStatus::Completed);
        let replied_at = session.last_reply_at.expect("reply recorded");

        // A later edit moves updated_at but must not look like a new reply.
        session.title = "Renamed".into();
        session.updated_at = replied_at + 500;
        assert_eq!(session.last_reply_at, Some(replied_at));

        // A second turn does move it.
        session.begin_turn("Again");
        session.finish_active_turn(crate::model::TurnStatus::Failed);
        assert!(session.last_reply_at >= Some(replied_at));
    }

    #[test]
    fn last_reply_at_is_derived_for_sessions_stored_without_it() {
        let mut session = AgentSession::new(Uuid::new_v4(), ProviderKind::Codex);
        session.begin_turn("Ask");
        session.finish_active_turn(crate::model::TurnStatus::Completed);
        let completed_at = session.turns.last().unwrap().completed_at.unwrap();

        // Drop the field, as a session written before it existed would be.
        session.last_reply_at = None;
        session.backfill_last_reply_at();
        assert_eq!(session.last_reply_at, Some(completed_at));

        // A session that never ran has nothing to derive.
        let mut fresh = AgentSession::new(Uuid::new_v4(), ProviderKind::Codex);
        fresh.backfill_last_reply_at();
        assert!(fresh.last_reply_at.is_none());
    }

    #[test]
    fn sessions_can_be_listed_without_deserializing_transcripts() {
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.sessions[0].begin_turn("First");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        let mut second = state.new_session(state.projects[0].id, ProviderKind::Codex);
        second.title = "Newer".into();
        second.begin_turn("Second");
        second.finish_active_turn(crate::model::TurnStatus::Completed);
        second.updated_at = state.sessions[0].updated_at + 100;
        state.sessions.push(second);
        store.save(&mut state).unwrap();

        let connection = Connection::open(directory.join("app.db")).unwrap();
        let mut statement = connection
            .prepare("SELECT title FROM sessions ORDER BY updated_at DESC")
            .unwrap();
        let titles: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect();

        assert_eq!(titles.first().map(String::as_str), Some("Newer"));
        assert_eq!(titles.len(), 2);

        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn reopening_does_not_rewrite_untouched_sessions() {
        let directory = temporary_directory();
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.sessions[0].begin_turn("Stored");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        store_in(&directory).save(&mut state).unwrap();

        // A fresh store reloads and then saves without any edits in between.
        let reopened = store_in(&directory);
        let mut restored = reopened.load().unwrap();
        assert!(
            restored.dirty_sessions.is_empty(),
            "loading marks nothing dirty"
        );

        let connection = Connection::open(directory.join("app.db")).unwrap();
        connection
            .execute_batch("UPDATE sessions SET title = 'untouched'")
            .unwrap();
        reopened.save(&mut restored).unwrap();

        let title: String = connection
            .query_row("SELECT title FROM sessions LIMIT 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            title, "untouched",
            "no row was rewritten after a plain load"
        );
        drop(connection);

        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn large_images_are_externalized_and_referenced() {
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        let payload = vec![9u8; 64 * 1024];
        let data_url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&payload)
        );
        state.sessions[0].begin_turn("Screenshot");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        let id = state.sessions[0].id;
        state
            .session_mut(id)
            .unwrap()
            .transcript_blocks
            .push(TranscriptBlock {
                after_message: 0,
                turn_id: None,
                content: TranscriptBlockContent::Activities(vec![
                    ActivityItem::new(None, ActivityKind::Tool, "Screenshot", None, true)
                        .with_image_urls(vec![data_url]),
                ]),
            });

        store.save(&mut state).unwrap();

        let restored = load_hydrated(&store_in(&directory));
        let TranscriptBlockContent::Activities(activities) =
            &restored.sessions[0].transcript_blocks[0].content
        else {
            panic!("expected activities");
        };
        let reference = &activities[0].image_urls[0];
        assert!(crate::blob_store::is_blob_reference(reference));
        let path = store.blobs().path_for(reference).unwrap();
        assert_eq!(fs::read(path).unwrap(), payload);

        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn legacy_signed_computer_grants_migrate_to_bundle_ids() {
        let legacy = serde_json::json!({
            "bundleId": "net.imput.helium",
            "teamId": "S4Q33XPHB4",
            "appName": "Helium"
        });
        let grant: ComputerAppGrant = serde_json::from_value(legacy).unwrap();
        assert_eq!(grant.key(), "net.imput.helium");

        let mut grants = vec![
            grant,
            ComputerAppGrant {
                bundle_id: "net.imput.helium".into(),
                app_name: "Helium Preview".into(),
            },
            ComputerAppGrant {
                bundle_id: String::new(),
                app_name: "Missing identity".into(),
            },
        ];
        normalize_computer_app_grants(&mut grants);
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].app_name, "Helium");

        let saved = serde_json::to_value(&grants[0]).unwrap();
        assert_eq!(
            saved.get("bundleId").and_then(|value| value.as_str()),
            Some("net.imput.helium")
        );
        assert!(saved.get("teamId").is_none());
    }

    #[test]
    fn blank_sessions_stay_runtime_only() {
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));

        store.save(&mut state).unwrap();
        let restored = store_in(&directory).load().unwrap();

        assert!(restored.sessions.is_empty());
        assert!(restored.selected_session.is_none());
        assert_eq!(restored.selected_project, state.selected_project);
        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn quitting_on_a_draft_relaunches_to_the_new_session_page() {
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        let started_id = state.sessions[0].id;
        state.sessions[0].begin_turn("Persist this session");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        let draft = state.new_session(state.projects[0].id, ProviderKind::Codex);
        state.selected_session = Some(draft.id);
        state.sessions.push(draft);

        store.save(&mut state).unwrap();
        let restored = store_in(&directory).load().unwrap();

        // Only the started session earned a row, and the draft selection was
        // stored as no selection so launch recreates the new-session page.
        assert_eq!(restored.sessions.len(), 1);
        assert_eq!(restored.sessions[0].id, started_id);
        assert_eq!(restored.selected_session, None);

        let relaunched = store_in(&directory).load_or_fresh(PathBuf::from("/tmp/project"));
        let selected = relaunched.selected_session.expect("draft selected");
        assert_ne!(selected, started_id);
        let session = relaunched
            .sessions
            .iter()
            .find(|session| session.id == selected)
            .expect("draft exists");
        assert!(!session.has_started());
        assert_eq!(session.project_id, relaunched.selected_project.unwrap());
        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn deleted_sessions_lose_their_row() {
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.sessions[0].begin_turn("Keep");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        let mut extra = state.new_session(state.projects[0].id, ProviderKind::Codex);
        extra.begin_turn("Remove");
        extra.finish_active_turn(crate::model::TurnStatus::Completed);
        let removed_id = extra.id;
        state.sessions.push(extra);
        store.save(&mut state).unwrap();

        state.sessions.retain(|session| session.id != removed_id);
        store.save(&mut state).unwrap();

        let restored = store_in(&directory).load().unwrap();
        assert_eq!(restored.sessions.len(), 1);
        assert!(restored.sessions.iter().all(|s| s.id != removed_id));
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
    fn selected_model_and_traits_are_used_for_new_sessions() {
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.last_provider = ProviderKind::Grok;
        state.last_model = Some("grok-code-fast-1".into());
        state.last_reasoning_effort = Some("high".into());
        state.last_service_tier = Some("fast".into());

        let remembered = state.new_session(state.projects[0].id, ProviderKind::Grok);
        let other_provider = state.new_session(state.projects[0].id, ProviderKind::Codex);

        assert_eq!(remembered.model.as_deref(), Some("grok-code-fast-1"));
        assert_eq!(remembered.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(remembered.service_tier.as_deref(), Some("fast"));
        assert!(other_provider.model.is_none());
        assert!(other_provider.reasoning_effort.is_none());
        assert!(other_provider.service_tier.is_none());
    }

    #[test]
    fn missing_remembered_selection_is_backfilled_from_selected_session() {
        let directory = temporary_directory();
        let store = store_in(&directory);
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        let id = state.sessions[0].id;
        state.sessions[0].begin_turn("Started");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        let session = state.session_mut(id).unwrap();
        session.model = Some("gpt-5.6-luna".into());
        session.reasoning_effort = Some("xhigh".into());
        session.service_tier = Some("fast".into());
        store.save(&mut state).unwrap();

        // Drop the remembered selection from settings, as a file written before
        // those fields existed would have.
        let settings_path = directory.join("settings.json");
        let mut settings: serde_json::Value =
            serde_json::from_slice(&fs::read(&settings_path).unwrap()).unwrap();
        for key in ["last_model", "last_reasoning_effort", "last_service_tier"] {
            settings.as_object_mut().unwrap().remove(key);
        }
        fs::write(&settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();

        let reopened = store_in(&directory);
        let mut restored = reopened.load().unwrap();
        reopened.hydrate(&mut restored.sessions[0]).unwrap();
        restored.backfill_remembered_selection();

        assert_eq!(restored.last_model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(restored.last_reasoning_effort.as_deref(), Some("xhigh"));
        assert_eq!(restored.last_service_tier.as_deref(), Some("fast"));
        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn app_bundle_root_directory_starts_with_onboarding() {
        let directory = temporary_directory();
        let state = store_in(&directory).load_or_fresh(PathBuf::from("/"));
        assert!(state.projects.is_empty());
        assert!(state.selected_session.is_none());
        fs::remove_dir_all(directory).ok();
    }
}
