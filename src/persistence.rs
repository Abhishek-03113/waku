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
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::blob_store::BlobStore;
use crate::computer_use::ComputerAppGrant;
use crate::identity::DATA_DIRECTORY_NAME;
use crate::model::{
    AgentSession, FavoriteModel, Message, MessageRole, Project, ProviderKind,
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
    }

    /// A session only earns a row once it has started; drafts stay in memory.
    fn persistable_selected_session(&self) -> Option<Uuid> {
        self.selected_session
            .filter(|selected| {
                self.sessions
                    .iter()
                    .any(|session| session.id == *selected && session.has_started())
            })
            .or_else(|| {
                self.selected_project.and_then(|project| {
                    self.sessions
                        .iter()
                        .filter(|session| session.project_id == project && session.has_started())
                        .max_by_key(|session| session.updated_at)
                        .map(|session| session.id)
                })
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

    fn migrate_loaded(&mut self, from_version: u32) {
        if from_version < 3 {
            for session in &mut self.sessions {
                session.migrate_pre_access_modes();
            }
        }
        for session in &mut self.sessions {
            session.migrate_legacy_state();
            session.backfill_last_reply_at();
        }
        self.version = STATE_VERSION;
        normalize_computer_app_grants(&mut self.computer_use_allowed_apps);
        self.backfill_remembered_selection();
    }

    fn backfill_remembered_selection(&mut self) {
        let Some(session) = self
            .selected_session
            .and_then(|selected| {
                self.sessions
                    .iter()
                    .find(|session| session.id == selected)
            })
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
fn externalize_blobs(sessions: &mut [AgentSession], blobs: &BlobStore) {
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

fn live_blob_references(sessions: &[AgentSession]) -> HashSet<String> {
    let mut references = HashSet::new();
    for session in sessions {
        for block in &session.transcript_blocks {
            let TranscriptBlockContent::Activities(activities) = &block.content else {
                continue;
            };
            for activity in activities {
                for image in &activity.image_urls {
                    if crate::blob_store::is_blob_reference(image) {
                        references.insert(image.clone());
                    }
                }
            }
        }
    }
    references
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
    /// Fingerprint of each session row as last written, so a save can skip rows
    /// whose contents did not change.
    saved_sessions: HashMap<Uuid, u64>,
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
        state
    }

    fn read_settings(&self) -> io::Result<Option<AppSettings>> {
        let Ok(bytes) = fs::read(&self.settings_path) else {
            return Ok(None);
        };
        serde_json::from_slice(&bytes).map(Some).map_err(to_io_error)
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

        let Some(settings) = self.read_settings()? else {
            // Nothing stored yet: adopt the legacy JSON document if present.
            let migrated = self.migrate_legacy_document(&connection)?;
            *self.storage.lock() = Some(Storage {
                connection,
                saved_sessions: HashMap::new(),
                saved_projects: 0,
                saved_settings: 0,
            });
            return Ok(migrated);
        };

        let from_version = settings.version;
        if !(OLDEST_SUPPORTED_STATE_VERSION..=STATE_VERSION).contains(&from_version) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported Waku state version",
            ));
        }
        state.apply_settings(settings);

        let mut projects = connection
            .prepare("SELECT data FROM projects ORDER BY position")
            .map_err(to_io_error)?;
        state.projects = projects
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(to_io_error)?
            .filter_map(Result::ok)
            .filter_map(|data| serde_json::from_str::<Project>(&data).ok())
            .collect();
        drop(projects);

        let mut sessions = connection
            .prepare("SELECT data FROM sessions ORDER BY updated_at")
            .map_err(to_io_error)?;
        // Seed each row's fingerprint from the bytes as stored, so the first
        // save after launch writes only what actually changed. A session that
        // migration rewrote will not match its stored form, and so is written.
        let mut saved_sessions = HashMap::new();
        state.sessions = sessions
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(to_io_error)?
            .filter_map(Result::ok)
            .filter_map(|data| {
                let session = serde_json::from_str::<AgentSession>(&data).ok()?;
                saved_sessions.insert(session.id, fingerprint(&data));
                Some(session)
            })
            .collect();
        drop(sessions);

        let mut by_session = read_messages(&connection)?;
        for session in &mut state.sessions {
            session.messages = by_session.remove(&session.id).unwrap_or_default();
            if let Some(seed) = saved_sessions.get_mut(&session.id) {
                *seed ^= fingerprint(
                    &serde_json::to_string(&session.messages).map_err(to_io_error)?,
                );
            }
        }

        state.migrate_loaded(from_version);

        *self.storage.lock() = Some(Storage {
            connection,
            saved_sessions,
            saved_projects: 0,
            saved_settings: 0,
        });
        Ok(state)
    }

    /// Reads a pre-SQLite `state.json`, writes it into the database, and keeps
    /// the original as a `.backup` rather than deleting it.
    fn migrate_legacy_document(&self, connection: &Connection) -> io::Result<PersistedState> {
        let legacy_path = self.path.with_file_name("state.json");
        let bytes = fs::read(&legacy_path)?;
        let mut state = serde_json::from_slice::<PersistedState>(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let from_version = state.version;
        if !(OLDEST_SUPPORTED_STATE_VERSION..=STATE_VERSION).contains(&from_version) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported Waku state version",
            ));
        }
        state.migrate_loaded(from_version);
        externalize_blobs(&mut state.sessions, &self.blobs);
        write_all(connection, &state)?;
        self.write_settings(&state.settings())?;
        fs::rename(&legacy_path, legacy_path.with_extension("json.backup")).ok();
        Ok(state)
    }

    /// Persists whatever changed. Sessions whose serialized form matches the
    /// last write are skipped, so a streaming save writes one row.
    pub fn save(&self, state: &mut PersistedState) -> io::Result<()> {
        externalize_blobs(&mut state.sessions, &self.blobs);

        let mut guard = self.storage.lock();
        if guard.is_none() {
            *guard = Some(Storage {
                connection: self.open()?,
                saved_sessions: HashMap::new(),
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

        let transaction = storage.connection.unchecked_transaction().map_err(to_io_error)?;

        let projects = serde_json::to_string(&state.projects).map_err(to_io_error)?;
        let projects_fingerprint = fingerprint(&projects);
        if projects_fingerprint != storage.saved_projects {
            transaction
                .execute("DELETE FROM projects", [])
                .map_err(to_io_error)?;
            for (position, project) in state.projects.iter().enumerate() {
                transaction
                    .execute(
                        "INSERT INTO projects(id, position, data) VALUES(?1, ?2, ?3)",
                        params![
                            project.id.to_string(),
                            position as i64,
                            serde_json::to_string(project).map_err(to_io_error)?
                        ],
                    )
                    .map_err(to_io_error)?;
            }
            storage.saved_projects = projects_fingerprint;
        }

        let mut live = HashSet::with_capacity(state.sessions.len());
        for session in state.sessions.iter().filter(|session| session.has_started()) {
            live.insert(session.id);
            let data = session_data(session)?;
            // Messages are their own rows, so they must be part of what decides
            // whether this session changed.
            let session_fingerprint = fingerprint(&data)
                ^ fingerprint(&serde_json::to_string(&session.messages).map_err(to_io_error)?);
            if storage.saved_sessions.get(&session.id) == Some(&session_fingerprint) {
                continue;
            }
            transaction
                .execute(
                    UPSERT_SESSION,
                    rusqlite::params_from_iter(session_params(session, &data)),
                )
                .map_err(to_io_error)?;
            write_messages(&transaction, session)?;
            storage.saved_sessions.insert(session.id, session_fingerprint);
        }

        let removed = storage
            .saved_sessions
            .keys()
            .copied()
            .filter(|id| !live.contains(id))
            .collect::<Vec<_>>();
        for id in removed {
            let key = id.to_string();
            transaction
                .execute("DELETE FROM sessions WHERE id = ?1", params![key])
                .map_err(to_io_error)?;
            transaction
                .execute("DELETE FROM messages WHERE session_id = ?1", params![key])
                .map_err(to_io_error)?;
            storage.saved_sessions.remove(&id);
        }

        transaction.commit().map_err(to_io_error)
    }

    /// Builds a blob sweep for the current state. Collecting the live set is
    /// cheap and happens on the caller's thread; run the returned closure on a
    /// background executor, since it walks the blob directory.
    pub fn blob_sweep(&self, state: &PersistedState) -> impl FnOnce() + Send + 'static {
        let blobs = Arc::clone(&self.blobs);
        let live = live_blob_references(&state.sessions);
        move || {
            let _ = blobs.retain(&live);
        }
    }
}

/// Reads every message row, grouped by session and in conversation order.
///
/// One pass over the table rather than a query per session, so loading stays
/// proportional to the data rather than to the session count.
fn read_messages(connection: &Connection) -> io::Result<HashMap<Uuid, Vec<Message>>> {
    let mut statement = connection
        .prepare(
            "SELECT session_id, id, turn_id, role, content, created_at, streaming
             FROM messages ORDER BY session_id, position",
        )
        .map_err(to_io_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .map_err(to_io_error)?;

    let mut by_session: HashMap<Uuid, Vec<Message>> = HashMap::new();
    for row in rows.filter_map(Result::ok) {
        let (session_id, id, turn_id, role, content, created_at, streaming) = row;
        let (Ok(session_id), Ok(id)) = (Uuid::parse_str(&session_id), Uuid::parse_str(&id)) else {
            continue;
        };
        let Ok(role) = serde_json::from_value::<MessageRole>(serde_json::Value::String(role))
        else {
            continue;
        };
        by_session.entry(session_id).or_default().push(Message {
            id,
            turn_id: turn_id.as_deref().and_then(|id| Uuid::parse_str(id).ok()),
            role,
            content,
            created_at: created_at as u64,
            streaming: streaming != 0,
        });
    }
    Ok(by_session)
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
fn write_messages(
    transaction: &Connection,
    session: &AgentSession,
) -> io::Result<()> {
    use rusqlite::types::Value;
    let session_id = session.id.to_string();
    for (position, message) in session.messages.iter().enumerate() {
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
    Ok(())
}

/// Columns the sidebar sorts and filters on are stored alongside the JSON so
/// listing sessions never has to deserialize a transcript.
const UPSERT_SESSION: &str = "INSERT INTO sessions(
         id, project_id, title, provider, model, status,
         created_at, updated_at, last_reply_at, data
     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
     ON CONFLICT(id) DO UPDATE SET
         project_id    = excluded.project_id,
         title         = excluded.title,
         provider      = excluded.provider,
         model         = excluded.model,
         status        = excluded.status,
         created_at    = excluded.created_at,
         updated_at    = excluded.updated_at,
         last_reply_at = excluded.last_reply_at,
         data          = excluded.data";

/// Serializes an enum to the same string the JSON blob uses, so a column and
/// its JSON counterpart can never disagree about spelling.
fn tag_of(value: impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn session_params(session: &AgentSession, data: &str) -> Vec<rusqlite::types::Value> {
    use rusqlite::types::Value;
    vec![
        Value::Text(session.id.to_string()),
        Value::Text(session.project_id.to_string()),
        Value::Text(session.title.clone()),
        Value::Text(tag_of(session.provider)),
        session
            .model
            .clone()
            .map_or(Value::Null, Value::Text),
        Value::Text(tag_of(session.status)),
        Value::Integer(session.created_at as i64),
        Value::Integer(session.updated_at as i64),
        session
            .last_reply_at
            .map_or(Value::Null, |at| Value::Integer(at as i64)),
        Value::Text(data.to_owned()),
    ]
}

/// Writes projects and sessions wholesale, for the one-time legacy migration.
/// Settings are written separately, to `settings.json`.
fn write_all(connection: &Connection, state: &PersistedState) -> io::Result<()> {
    let transaction = connection.unchecked_transaction().map_err(to_io_error)?;
    for (position, project) in state.projects.iter().enumerate() {
        transaction
            .execute(
                "INSERT INTO projects(id, position, data) VALUES(?1, ?2, ?3)
                 ON CONFLICT(id) DO UPDATE SET position = excluded.position, data = excluded.data",
                params![
                    project.id.to_string(),
                    position as i64,
                    serde_json::to_string(project).map_err(to_io_error)?
                ],
            )
            .map_err(to_io_error)?;
    }
    for session in state.sessions.iter().filter(|session| session.has_started()) {
        let data = session_data(session)?;
        transaction
            .execute(
                UPSERT_SESSION,
                rusqlite::params_from_iter(session_params(session, &data)),
            )
            .map_err(to_io_error)?;
        write_messages(&transaction, session)?;
    }
    transaction.commit().map_err(to_io_error)
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
        ActivityItem, ActivityKind, FavoriteModel, ReasoningBlock, TranscriptBlock,
        TranscriptBlockContent,
    };
    use base64::Engine as _;

    fn temporary_directory() -> PathBuf {
        std::env::temp_dir().join(format!("waku-state-{}", Uuid::new_v4()))
    }

    fn store_in(directory: &Path) -> StateStore {
        StateStore::new(directory.join("app.db"))
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

        let restored = store_in(&directory).load().unwrap();
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

        // Touch one session; the other must keep its stored row untouched.
        let before = {
            let guard = store.storage.lock();
            let storage = guard.as_ref().unwrap();
            storage.saved_sessions[&quiet_id]
        };
        state.sessions[0].begin_turn("Second");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        store.save(&mut state).unwrap();

        let guard = store.storage.lock();
        let storage = guard.as_ref().unwrap();
        assert_eq!(storage.saved_sessions[&quiet_id], before);
        drop(guard);

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
        assert!(text.contains('\n'), "settings are pretty-printed for editing");
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
            MIGRATIONS.iter().map(|(tag, _)| tag.to_string()).collect::<Vec<_>>()
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
            .query_row("SELECT data FROM sessions LIMIT 1", [], |row| row.get(0))
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

        let restored = store_in(&directory).load().unwrap();
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

        state.sessions[0].messages.truncate(1);
        store.save(&mut state).unwrap();

        let connection = Connection::open(directory.join("app.db")).unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "the tail row was deleted, not left behind");
        drop(connection);

        assert_eq!(store_in(&directory).load().unwrap().sessions[0].messages.len(), 1);
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
        state.sessions[0].messages[0].content = "after".into();
        store.save(&mut state).unwrap();

        assert_eq!(
            store_in(&directory).load().unwrap().sessions[0].messages[0].content,
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
        let seeded = {
            let guard = reopened.storage.lock();
            guard.as_ref().unwrap().saved_sessions.clone()
        };
        assert_eq!(seeded.len(), 1, "load seeds a fingerprint per stored row");

        reopened.save(&mut restored).unwrap();

        let guard = reopened.storage.lock();
        assert_eq!(
            guard.as_ref().unwrap().saved_sessions,
            seeded,
            "an unedited session keeps its fingerprint, so no row was rewritten"
        );
        drop(guard);

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
        state.sessions[0].transcript_blocks.push(TranscriptBlock {
            after_message: 0,
            turn_id: None,
            content: TranscriptBlockContent::Activities(vec![
                ActivityItem::new(None, ActivityKind::Tool, "Screenshot", None, true)
                    .with_image_urls(vec![data_url]),
            ]),
        });

        store.save(&mut state).unwrap();

        let restored = store_in(&directory).load().unwrap();
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
    fn legacy_json_document_migrates_into_sqlite() {
        let directory = temporary_directory();
        fs::create_dir_all(&directory).unwrap();
        let mut legacy = PersistedState::fresh(PathBuf::from("/tmp/project"));
        legacy.version = 4;
        legacy.sessions[0].begin_turn("Legacy");
        legacy.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        legacy.theme = ThemePreference::Light;
        fs::write(
            directory.join("state.json"),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();

        let restored = store_in(&directory).load().unwrap();

        assert_eq!(restored.version, STATE_VERSION);
        assert_eq!(restored.theme, ThemePreference::Light);
        assert_eq!(restored.sessions.len(), 1);
        assert!(directory.join("state.json.backup").exists());
        assert!(!directory.join("state.json").exists());
        // The migrated database is now the source of truth.
        let reopened = store_in(&directory).load().unwrap();
        assert_eq!(reopened.sessions.len(), 1);

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
    fn selected_draft_falls_back_to_latest_started_session_on_disk() {
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

        assert_eq!(restored.sessions.len(), 1);
        assert_eq!(restored.sessions[0].id, started_id);
        assert_eq!(restored.selected_session, Some(started_id));
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
    fn version_one_sessions_migrate_provider_ids_and_turns() {
        let directory = temporary_directory();
        fs::create_dir_all(&directory).unwrap();
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
        for key in [
            "favorite_models",
            "theme",
            "last_model",
            "last_reasoning_effort",
            "last_service_tier",
            "sidebar_visible",
            "right_panel_visible",
            "sidebar_width",
            "right_panel_width",
        ] {
            value.as_object_mut().unwrap().remove(key);
        }
        value["sessions"][0]
            .as_object_mut()
            .unwrap()
            .remove("model");
        value["sessions"][0]["provider_session_id"] =
            serde_json::Value::String("thread-123".into());
        fs::write(
            directory.join("state.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();

        let restored = store_in(&directory).load().unwrap();
        assert_eq!(restored.version, STATE_VERSION);
        assert!(restored.favorite_models.is_empty());
        assert_eq!(restored.theme, ThemePreference::System);
        assert!(restored.sidebar_visible);
        assert!(restored.right_panel_visible);
        assert_eq!(restored.sidebar_width, DEFAULT_SIDEBAR_WIDTH);
        assert_eq!(restored.right_panel_width, DEFAULT_RIGHT_PANEL_WIDTH);
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
        fs::create_dir_all(&directory).unwrap();
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.sessions[0].begin_turn("Started");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        state.sessions[0].model = Some("gpt-5.6-luna".into());
        state.sessions[0].reasoning_effort = Some("xhigh".into());
        state.sessions[0].service_tier = Some("fast".into());
        let mut value = serde_json::to_value(state).unwrap();
        for key in ["last_model", "last_reasoning_effort", "last_service_tier"] {
            value.as_object_mut().unwrap().remove(key);
        }
        fs::write(
            directory.join("state.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();

        let restored = store_in(&directory).load().unwrap();

        assert_eq!(restored.last_model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(restored.last_reasoning_effort.as_deref(), Some("xhigh"));
        assert_eq!(restored.last_service_tier.as_deref(), Some("fast"));
        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn version_two_combined_modes_migrate_to_access_and_interaction_settings() {
        let directory = temporary_directory();
        fs::create_dir_all(&directory).unwrap();
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.version = 2;
        state.sessions[0].runtime_mode = crate::model::RuntimeMode::Plan;
        state.sessions[0].begin_turn("One");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        let mut auto_session = AgentSession::new(state.projects[0].id, ProviderKind::Codex);
        auto_session.runtime_mode = crate::model::RuntimeMode::Auto;
        auto_session.begin_turn("Two");
        auto_session.finish_active_turn(crate::model::TurnStatus::Completed);
        state.sessions.push(auto_session);
        fs::write(
            directory.join("state.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();

        let restored = store_in(&directory).load().unwrap();
        assert_eq!(restored.version, STATE_VERSION);
        let plan = restored
            .sessions
            .iter()
            .find(|session| session.interaction_mode == crate::model::InteractionMode::Plan)
            .expect("plan session");
        let build = restored
            .sessions
            .iter()
            .find(|session| session.interaction_mode == crate::model::InteractionMode::Build)
            .expect("build session");
        assert_eq!(plan.runtime_mode, crate::model::RuntimeMode::Ask);
        assert_eq!(build.runtime_mode, crate::model::RuntimeMode::AutoAcceptEdits);

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

