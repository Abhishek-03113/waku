use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use gpui::{
    Animation, AnimationExt, AnyElement, App, BoxShadow, ClipboardItem, Context, Corner, Div,
    Entity, FocusHandle, FontWeight, Hsla, IntoElement, ListAlignment, ListState,
    PathPromptOptions, Render, SharedString, StyleRefinement, Timer, Window, div, hsla, list,
    point, prelude::*, pulsating_between, px, rems,
};
use uuid::Uuid;

use crate::driver::{self, DriverHandle};
use crate::input::{ComposerEvent, ComposerInput, preserve_composer_focus_for_context_menu};
use crate::model::{
    ActivityItem, AgentSession, DriverEvent, Message, MessageRole, PendingPermission, Project,
    ProviderKind, ProviderProbe, ReasoningBlock, RuntimeMode, SessionStatus, TranscriptBlock,
    TranscriptBlockContent, compact_path, unix_time, unix_time_millis,
};
use gpui_component::Icon as ComponentIcon;
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::text::{TextView, TextViewStyle};
use unicode_segmentation::UnicodeSegmentation;

use crate::persistence::{PersistedState, StateStore};
use crate::theme::Theme;
use crate::ui::{
    MenuChip, activity_icon, activity_noun, icon, key_hint, provider_color, provider_icon,
    relative_time, section_label, status_color, status_label,
};
use crate::{CancelTurn, FocusComposer, NewSession, ToggleSidebar};

const TRAFFIC_LIGHT_CLEARANCE: f32 = 86.0;
const CONTENT_MAX_WIDTH: f32 = 720.0;
const SIDEBAR_WIDTH: f32 = 252.0;
const FOLLOWUP_TURN_TOP_GAP: f32 = 48.0;
const STREAM_FRAME_INTERVAL: Duration = Duration::from_millis(24);
const STREAM_MARKDOWN_DELAY: Duration = Duration::from_millis(12);
const STREAM_SAVE_INTERVAL: Duration = Duration::from_secs(1);
const STREAM_CATCH_UP_FRAMES: usize = 18;
const STREAM_MIN_GRAPHEMES_PER_FRAME: usize = 12;
const STREAM_MAX_GRAPHEMES_PER_FRAME: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StreamPhase {
    Text,
    Reasoning,
    Activity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StreamDeltaKind {
    Text,
    Reasoning,
}

pub struct Waku {
    state: PersistedState,
    store: StateStore,
    composer: Entity<ComposerInput>,
    probes: Vec<ProviderProbe>,
    driver: Option<DriverHandle>,
    driver_session: Option<Uuid>,
    driver_events: Option<Receiver<DriverEvent>>,
    pending_driver_events: VecDeque<DriverEvent>,
    stream_state_dirty: bool,
    stream_remeasure_pending: bool,
    last_stream_save: Instant,
    stream_phase: Option<StreamPhase>,
    /// User expansion overrides keyed by persisted transcript block index.
    reasoning_expanded: HashMap<usize, bool>,
    activities_expanded: HashMap<usize, bool>,
    /// Individual tool rows the user has opened to read their full detail.
    expanded_activity_items: HashSet<Uuid>,
    pending_permission: Option<PendingPermission>,
    sidebar_visible: bool,
    branch: Option<String>,
    toast: Option<String>,
    transcript_rows: ListState,
}

impl Waku {
    pub fn new(window: &mut Window, cx: &mut App) -> Entity<Self> {
        let composer = cx.new(|cx| ComposerInput::new(window, cx));
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let store = StateStore::new(StateStore::default_path());
        let mut state = store.load_or_fresh(cwd);
        for session in &mut state.sessions {
            if session.status != SessionStatus::Idle {
                session.status = SessionStatus::Idle;
            }
            for message in &mut session.messages {
                message.streaming = false;
            }
            session.transcript_blocks.retain(|block| {
                !matches!(
                    &block.content,
                    TranscriptBlockContent::Reasoning(reasoning)
                        if reasoning.content.trim().is_empty()
                )
            });
            for block in &mut session.transcript_blocks {
                if let TranscriptBlockContent::Activities(activities) = &mut block.content {
                    for activity in activities {
                        activity.complete = true;
                    }
                }
            }
        }
        let probes = ProviderKind::ALL
            .into_iter()
            .map(ProviderProbe::detect)
            .collect();
        let branch = state
            .selected_project
            .and_then(|project_id| {
                state
                    .projects
                    .iter()
                    .find(|project| project.id == project_id)
            })
            .and_then(|project| git_branch(&project.path));

        cx.new(|cx| {
            cx.subscribe(
                &composer,
                |this: &mut Self, _, event: &ComposerEvent, cx| match event {
                    ComposerEvent::Submit(prompt) => this.submit_prompt(prompt.clone(), cx),
                },
            )
            .detach();

            cx.observe(&composer, |_, _, cx| cx.notify()).detach();

            cx.spawn(async move |this, cx| {
                loop {
                    Timer::after(STREAM_FRAME_INTERVAL).await;
                    if this
                        .update(cx, |this, cx| {
                            if this.drain_driver_events() {
                                cx.notify();
                            }
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();

            Self {
                state,
                store,
                composer,
                probes,
                driver: None,
                driver_session: None,
                driver_events: None,
                pending_driver_events: VecDeque::new(),
                stream_state_dirty: false,
                stream_remeasure_pending: false,
                last_stream_save: Instant::now(),
                stream_phase: None,
                reasoning_expanded: HashMap::new(),
                activities_expanded: HashMap::new(),
                expanded_activity_items: HashSet::new(),
                pending_permission: None,
                sidebar_visible: true,
                branch,
                toast: None,
                transcript_rows: ListState::new(0, ListAlignment::Bottom, px(512.0)),
            }
        })
    }

    pub fn composer_focus(&self, cx: &App) -> FocusHandle {
        self.composer.read(cx).focus()
    }

    fn selected_project(&self) -> Option<&Project> {
        let id = self.state.selected_project?;
        self.state.projects.iter().find(|project| project.id == id)
    }

    fn selected_session(&self) -> Option<&AgentSession> {
        let id = self.state.selected_session?;
        self.state.sessions.iter().find(|session| session.id == id)
    }

    fn selected_session_mut(&mut self) -> Option<&mut AgentSession> {
        let id = self.state.selected_session?;
        self.state
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
    }

    fn selected_transcript_blocks(&self) -> &[TranscriptBlock] {
        self.selected_session()
            .map(|session| session.transcript_blocks.as_slice())
            .unwrap_or(&[])
    }

    fn save(&mut self) {
        self.last_stream_save = Instant::now();
        if let Err(error) = self.store.save(&self.state) {
            self.toast = Some(format!("Could not save local state: {error}"));
        } else {
            self.stream_state_dirty = false;
        }
    }

    fn ensure_driver(&mut self) -> anyhow::Result<DriverHandle> {
        let session = self
            .selected_session()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("No session selected"))?;
        if self.driver_session == Some(session.id)
            && let Some(driver) = &self.driver
        {
            return Ok(driver.clone());
        }
        let project = self
            .state
            .projects
            .iter()
            .find(|project| project.id == session.project_id)
            .ok_or_else(|| anyhow::anyhow!("Project not found"))?;
        let binary = self
            .probes
            .iter()
            .find(|probe| probe.provider == session.provider)
            .and_then(|probe| probe.path.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{} is not installed or could not be found",
                    session.provider.display_name()
                )
            })?;
        let (event_tx, event_rx) = unbounded();
        let handle = driver::start(
            session.provider,
            binary,
            project.path.clone(),
            session.runtime_mode,
            session.provider_session_id.clone(),
            event_tx,
        )?;
        self.driver = Some(handle.clone());
        self.driver_session = Some(session.id);
        self.driver_events = Some(event_rx);
        Ok(handle)
    }

    fn submit_prompt(&mut self, prompt: String, cx: &mut Context<Self>) {
        let Some(session) = self.selected_session_mut() else {
            return;
        };
        if matches!(
            session.status,
            SessionStatus::Working | SessionStatus::Connecting
        ) {
            self.toast = Some("The agent is already working. Stop it before sending again.".into());
            cx.notify();
            return;
        }
        session.set_title_from_prompt(&prompt);
        session
            .messages
            .push(Message::new(MessageRole::User, &prompt));
        session.status = SessionStatus::Connecting;
        session.updated_at = unix_time();
        self.pending_driver_events.clear();
        self.stream_remeasure_pending = false;
        self.stream_phase = None;
        self.reasoning_expanded.clear();
        self.activities_expanded.clear();
        self.expanded_activity_items.clear();
        self.pending_permission = None;
        self.toast = None;
        self.transcript_rows.reset(self.transcript_row_count());
        match self.ensure_driver() {
            Ok(driver) => driver.prompt(prompt),
            Err(error) => {
                let message = format!("Could not start the agent: {error}");
                if let Some(session) = self.selected_session_mut() {
                    session.status = SessionStatus::Failed;
                    session
                        .messages
                        .push(Message::new(MessageRole::Assistant, message));
                }
            }
        }
        self.save();
        cx.notify();
    }

    fn collect_driver_events(&mut self) {
        if let Some(receiver) = self.driver_events.clone() {
            while let Ok(event) = receiver.try_recv() {
                self.pending_driver_events.push_back(event);
            }
        }
    }

    fn drain_driver_events(&mut self) -> bool {
        let follow_up_remeasure = std::mem::take(&mut self.stream_remeasure_pending);
        self.collect_driver_events();
        let mut changed = false;
        let mut force_save = false;
        let mut markdown_changed = false;
        let mut revealed_stream_chunk = false;
        while let Some(event) = self.pending_driver_events.front() {
            let kind = stream_delta_kind(event);
            if kind.is_some() && revealed_stream_chunk {
                break;
            }

            let event = if let Some(kind) = kind {
                revealed_stream_chunk = true;
                pop_stream_chunk(&mut self.pending_driver_events, kind)
            } else {
                self.pending_driver_events.pop_front()
            };
            let Some(event) = event else {
                break;
            };
            force_save |= matches!(
                event,
                DriverEvent::Connected { .. }
                    | DriverEvent::Permission { .. }
                    | DriverEvent::TurnFinished { .. }
                    | DriverEvent::Error(_)
                    | DriverEvent::ProcessExited
            );
            markdown_changed |= matches!(event, DriverEvent::TextDelta(_));
            changed = true;
            self.handle_driver_event(event);
        }

        if changed {
            self.stream_state_dirty = true;
        }
        if changed || follow_up_remeasure {
            self.remeasure_transcript_tail();
        }
        self.stream_remeasure_pending = markdown_changed;
        if self.stream_state_dirty
            && (force_save || self.last_stream_save.elapsed() >= STREAM_SAVE_INTERVAL)
        {
            self.save();
        }
        changed || follow_up_remeasure
    }

    /// One list row per message plus each ordered non-message turn block.
    fn transcript_row_count(&self) -> usize {
        let messages = self
            .selected_session()
            .map(|session| session.messages.len())
            .unwrap_or(0);
        messages + self.selected_transcript_blocks().len()
    }

    /// Keep the list's row count in sync with the transcript. Appends keep
    /// the reader's place (or the pinned tail); shrinking resets the view.
    fn sync_transcript_rows(&self) {
        let count = self.transcript_row_count();
        let current = self.transcript_rows.item_count();
        if count > current {
            self.transcript_rows
                .splice(current..current, count - current);
        } else if count < current {
            self.transcript_rows.reset(count);
        }
    }

    /// Streaming mutates current-turn rows in place, so re-measure the part of
    /// the transcript that can still change.
    fn remeasure_transcript_tail(&self) {
        self.sync_transcript_rows();
        let count = self.transcript_rows.item_count();
        let from = self
            .selected_transcript_blocks()
            .first()
            .map(|block| block.after_message.saturating_sub(1))
            .unwrap_or_else(|| count.saturating_sub(2));
        if from < count {
            self.transcript_rows.splice(from..count, count - from);
        }
    }

    fn finish_streaming_assistant(&mut self) {
        if let Some(session) = self.selected_session_mut() {
            for message in &mut session.messages {
                if message.role == MessageRole::Assistant && message.streaming {
                    message.streaming = false;
                }
            }
        }
    }

    fn append_text_delta(&mut self, delta: String) {
        let continuing = self.stream_phase == Some(StreamPhase::Text);
        if !continuing {
            self.finish_streaming_assistant();
        }
        if let Some(session) = self.selected_session_mut() {
            let existing = continuing.then(|| {
                session
                    .messages
                    .iter_mut()
                    .rev()
                    .find(|message| message.role == MessageRole::Assistant && message.streaming)
            });
            if let Some(Some(message)) = existing {
                message.content.push_str(&delta);
            } else {
                let mut message = Message::new(MessageRole::Assistant, delta);
                message.streaming = true;
                session.messages.push(message);
            }
            session.updated_at = unix_time();
        }
        self.stream_phase = Some(StreamPhase::Text);
    }

    fn append_reasoning_delta(&mut self, delta: String) {
        let continuing = self.stream_phase == Some(StreamPhase::Reasoning);
        if !continuing && delta.trim().is_empty() {
            return;
        }
        let now = unix_time_millis();
        if !continuing {
            self.finish_streaming_assistant();
        }
        if let Some(session) = self.selected_session_mut() {
            if continuing
                && let Some(TranscriptBlock {
                    content: TranscriptBlockContent::Reasoning(reasoning),
                    ..
                }) = session.transcript_blocks.last_mut()
            {
                reasoning.content.push_str(&delta);
                reasoning.finished_at_ms = now;
            } else {
                session.transcript_blocks.push(TranscriptBlock {
                    after_message: session.messages.len(),
                    content: TranscriptBlockContent::Reasoning(ReasoningBlock {
                        content: delta,
                        started_at_ms: now,
                        finished_at_ms: now,
                    }),
                });
            }
            session.updated_at = unix_time();
        }
        self.stream_phase = Some(StreamPhase::Reasoning);
    }

    fn update_activity(
        &mut self,
        source_id: Option<String>,
        kind: crate::model::ActivityKind,
        title: String,
        detail: Option<String>,
        complete: bool,
    ) {
        if self.stream_phase == Some(StreamPhase::Text) {
            self.finish_streaming_assistant();
        }

        let continuing = self.stream_phase == Some(StreamPhase::Activity);
        if let Some(session) = self.selected_session_mut() {
            for block in session.transcript_blocks.iter_mut().rev() {
                let TranscriptBlockContent::Activities(activities) = &mut block.content else {
                    continue;
                };
                let matching = activities.iter_mut().rev().find(|activity| {
                    source_id
                        .as_ref()
                        .is_some_and(|id| activity.source_id.as_ref() == Some(id))
                        || (source_id.is_none() && activity.title == title && !activity.complete)
                });
                if let Some(activity) = matching {
                    activity.kind = kind;
                    activity.title = title;
                    activity.complete = complete;
                    if detail.is_some() {
                        activity.detail = detail;
                    }
                    session.updated_at = unix_time();
                    self.stream_phase = Some(StreamPhase::Activity);
                    return;
                }
            }

            let after_message = session.messages.len();
            let item = ActivityItem::new(source_id, kind, title, detail, complete);
            if continuing
                && let Some(TranscriptBlock {
                    after_message: anchor,
                    content: TranscriptBlockContent::Activities(activities),
                }) = session.transcript_blocks.last_mut()
                && *anchor == after_message
            {
                activities.push(item);
            } else {
                session.transcript_blocks.push(TranscriptBlock {
                    after_message,
                    content: TranscriptBlockContent::Activities(vec![item]),
                });
            }
            session.updated_at = unix_time();
        }
        self.stream_phase = Some(StreamPhase::Activity);
    }

    fn complete_turn_blocks(&mut self) {
        if let Some(session) = self.selected_session_mut() {
            for block in &mut session.transcript_blocks {
                if let TranscriptBlockContent::Activities(activities) = &mut block.content {
                    for activity in activities {
                        activity.complete = true;
                    }
                }
            }
        }
    }

    fn turn_has_assistant_message(&self) -> bool {
        self.selected_session()
            .and_then(|session| {
                let last_user = session
                    .messages
                    .iter()
                    .rposition(|message| message.role == MessageRole::User)?;
                Some(
                    session.messages[last_user + 1..]
                        .iter()
                        .any(|message| message.role == MessageRole::Assistant),
                )
            })
            .unwrap_or(false)
    }

    fn accepts_turn_output(&self) -> bool {
        self.selected_session().is_some_and(|session| {
            matches!(
                session.status,
                SessionStatus::Connecting | SessionStatus::Working | SessionStatus::Waiting
            )
        })
    }

    fn handle_driver_event(&mut self, event: DriverEvent) {
        match event {
            DriverEvent::Connected {
                provider_session_id,
            } => {
                if let Some(session) = self.selected_session_mut() {
                    session.provider_session_id = provider_session_id;
                    if session.status == SessionStatus::Connecting {
                        session.status = SessionStatus::Working;
                    }
                }
            }
            DriverEvent::TurnStarted => {
                if let Some(session) = self.selected_session_mut() {
                    session.status = SessionStatus::Working;
                }
            }
            DriverEvent::TextDelta(delta) => {
                if self.accepts_turn_output() {
                    self.append_text_delta(delta);
                }
            }
            DriverEvent::ReasoningDelta(delta) => {
                if self.accepts_turn_output() {
                    self.append_reasoning_delta(delta);
                }
            }
            DriverEvent::Activity {
                id,
                kind,
                title,
                detail,
                complete,
            } => {
                if self.accepts_turn_output() {
                    self.update_activity(id, kind, title, detail, complete);
                }
            }
            DriverEvent::Permission {
                request_id,
                title,
                detail,
                options,
            } => {
                if self.accepts_turn_output() {
                    self.pending_permission = Some(PendingPermission {
                        request_id,
                        title,
                        detail,
                        options,
                    });
                    if let Some(session) = self.selected_session_mut() {
                        session.status = SessionStatus::Waiting;
                    }
                }
            }
            DriverEvent::TurnFinished { success, summary } => {
                self.finish_streaming_assistant();
                self.complete_turn_blocks();
                self.stream_phase = None;
                let needs_fallback = !self.turn_has_assistant_message();
                if let Some(session) = self.selected_session_mut() {
                    session.status = if success {
                        SessionStatus::Idle
                    } else {
                        SessionStatus::Failed
                    };
                    if needs_fallback {
                        session.messages.push(Message::new(
                            MessageRole::Assistant,
                            summary.unwrap_or_else(|| {
                                if success {
                                    "Turn completed.".into()
                                } else {
                                    "The agent stopped before returning a response.".into()
                                }
                            }),
                        ));
                    }
                }
                self.pending_permission = None;
            }
            DriverEvent::Error(error) => {
                self.toast = Some(error.clone());
                let should_append = !self.turn_has_assistant_message()
                    && self
                        .selected_session()
                        .is_some_and(|session| session.status != SessionStatus::Working);
                if let Some(session) = self.selected_session_mut() {
                    if session.status != SessionStatus::Working {
                        session.status = SessionStatus::Failed;
                    }
                    if should_append {
                        session
                            .messages
                            .push(Message::new(MessageRole::Assistant, error));
                    }
                }
            }
            DriverEvent::ProcessExited => {
                self.finish_streaming_assistant();
                self.complete_turn_blocks();
                self.stream_phase = None;
                self.pending_permission = None;
                if let Some(session) = self.selected_session_mut()
                    && matches!(
                        session.status,
                        SessionStatus::Connecting | SessionStatus::Working | SessionStatus::Waiting
                    )
                {
                    session.status = SessionStatus::Failed;
                    session.updated_at = unix_time();
                }
            }
        }
    }

    fn select_project(&mut self, project_id: Uuid, cx: &mut Context<Self>) {
        self.state.selected_project = Some(project_id);
        let next_session = self
            .state
            .sessions
            .iter()
            .filter(|session| session.project_id == project_id)
            .max_by_key(|session| session.updated_at)
            .map(|session| session.id);
        if let Some(session_id) = next_session {
            self.select_session(session_id, cx);
        } else {
            self.create_session_for(project_id, self.state.last_provider, cx);
        }
    }

    fn select_session(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        self.state.selected_session = Some(session_id);
        if let Some((project_id, provider)) = self
            .selected_session()
            .map(|session| (session.project_id, session.provider))
        {
            self.state.selected_project = Some(project_id);
            self.state.last_provider = provider;
        }
        self.reset_live_runtime();
        self.branch = self
            .selected_project()
            .and_then(|project| git_branch(&project.path));
        self.transcript_rows.reset(self.transcript_row_count());
        self.save();
        cx.notify();
    }

    fn create_session_for(
        &mut self,
        project_id: Uuid,
        provider: ProviderKind,
        cx: &mut Context<Self>,
    ) {
        let session = AgentSession::new(project_id, provider);
        let id = session.id;
        self.state.sessions.push(session);
        self.state.selected_project = Some(project_id);
        self.state.selected_session = Some(id);
        self.state.last_provider = provider;
        self.reset_live_runtime();
        self.transcript_rows.reset(0);
        self.save();
        cx.notify();
    }

    fn remove_session(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        let Some(index) = self
            .state
            .sessions
            .iter()
            .position(|session| session.id == session_id)
        else {
            return;
        };
        let project_id = self.state.sessions[index].project_id;
        let was_selected = self.state.selected_session == Some(session_id);
        self.state.sessions.remove(index);

        if !was_selected {
            self.save();
            cx.notify();
            return;
        }

        self.state.selected_session = None;
        let next_session = self
            .state
            .sessions
            .iter()
            .filter(|session| session.project_id == project_id)
            .max_by_key(|session| session.updated_at)
            .map(|session| session.id);
        if let Some(session_id) = next_session {
            self.select_session(session_id, cx);
        } else {
            self.create_session_for(project_id, self.state.last_provider, cx);
        }
    }

    fn new_session_action(&mut self, _: &NewSession, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(project_id) = self.state.selected_project {
            self.create_session_for(project_id, self.state.last_provider, cx);
        }
    }

    fn toggle_sidebar_action(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_visible = !self.sidebar_visible;
        cx.notify();
    }

    fn focus_composer_action(
        &mut self,
        _: &FocusComposer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.composer_focus(cx));
    }

    fn cancel_turn_action(&mut self, _: &CancelTurn, _: &mut Window, cx: &mut Context<Self>) {
        self.cancel_turn(cx);
    }

    fn reset_live_runtime(&mut self) {
        if let Some(driver) = &self.driver {
            driver.cancel();
        }
        self.driver = None;
        self.driver_session = None;
        self.driver_events = None;
        self.pending_driver_events.clear();
        self.stream_remeasure_pending = false;
        self.stream_phase = None;
        self.reasoning_expanded.clear();
        self.activities_expanded.clear();
        self.expanded_activity_items.clear();
        self.pending_permission = None;
        self.toast = None;
    }

    fn choose_provider(&mut self, provider: ProviderKind, cx: &mut Context<Self>) {
        if let Some(session) = self.selected_session_mut()
            && session.messages.is_empty()
        {
            session.provider = provider;
            self.state.last_provider = provider;
            self.reset_live_runtime();
            self.save();
            cx.notify();
        }
    }

    fn set_runtime_mode(&mut self, mode: RuntimeMode, cx: &mut Context<Self>) {
        if let Some(session) = self.selected_session_mut()
            && session.runtime_mode != mode
        {
            session.runtime_mode = mode;
            self.reset_live_runtime();
            self.save();
            cx.notify();
        }
    }

    fn cancel_turn(&mut self, cx: &mut Context<Self>) {
        if let Some(driver) = &self.driver {
            driver.cancel();
        }
        // Do not leave already-received text in the smoothing queue: once the
        // message is marked complete, a later delta would otherwise create a
        // second assistant bubble. Show the received portion immediately.
        self.collect_driver_events();
        while let Some(event) = self.pending_driver_events.pop_front() {
            self.handle_driver_event(event);
        }
        self.finish_streaming_assistant();
        self.complete_turn_blocks();
        self.stream_phase = None;
        let needs_fallback = !self.turn_has_assistant_message();
        if let Some(session) = self.selected_session_mut() {
            session.status = SessionStatus::Idle;
            if needs_fallback {
                session
                    .messages
                    .push(Message::new(MessageRole::Assistant, "Stopped."));
            }
        }
        self.pending_permission = None;
        self.remeasure_transcript_tail();
        self.save();
        cx.notify();
    }

    fn respond_permission(
        &mut self,
        request_id: String,
        option_id: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(driver) = &self.driver {
            driver.respond(request_id, option_id);
        }
        self.pending_permission = None;
        if let Some(session) = self.selected_session_mut() {
            session.status = SessionStatus::Working;
        }
        cx.notify();
    }

    fn add_project(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |this, cx| {
                    if let Some(existing) = this.state.projects.iter().find(|p| p.path == path) {
                        this.select_project(existing.id, cx);
                        return;
                    }
                    let project = Project::from_path(path);
                    let project_id = project.id;
                    this.state.projects.push(project);
                    this.create_session_for(project_id, this.state.last_provider, cx);
                });
            }
        })
        .detach();
    }

    // ── Sidebar ────────────────────────────────────────────────────────────

    fn render_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::dark();
        let selected_project = self.state.selected_project;
        let selected_session = self.state.selected_session;

        let mut projects = div().flex().flex_col();
        for project in &self.state.projects {
            let project_id = project.id;
            let selected = selected_project == Some(project.id);
            projects = projects.child(
                div()
                    .id(SharedString::from(format!("project-{}", project.id)))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .h(px(28.0))
                    .px(px(8.0))
                    .text_size(px(12.5))
                    .line_height(px(16.0))
                    .rounded(px(7.0))
                    .cursor_default()
                    .when(selected, |element| element.bg(theme.overlay))
                    .hover(|element| element.bg(theme.overlay))
                    .active(|element| element.bg(theme.overlay_strong))
                    .child(icon(
                        "icons/folder.svg",
                        13.0,
                        if selected {
                            theme.text_secondary
                        } else {
                            theme.text_tertiary
                        },
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5))
                            .text_color(if selected {
                                theme.text
                            } else {
                                theme.text_secondary
                            })
                            .child(SharedString::from(project.name.clone())),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_project(project_id, cx);
                    })),
            );
        }

        let mut sessions = div().flex().flex_col().gap(px(1.0));
        if let Some(project_id) = selected_project {
            let mut project_sessions = self
                .state
                .sessions
                .iter()
                .filter(|session| session.project_id == project_id)
                .collect::<Vec<_>>();
            project_sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
            for session in project_sessions {
                let session_id = session.id;
                let selected = selected_session == Some(session.id);
                let active = !matches!(session.status, SessionStatus::Idle);
                let waku = cx.entity().downgrade();
                let composer = self.composer.clone();
                sessions = sessions.child(
                    div()
                        .id(SharedString::from(format!("session-{}", session.id)))
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .px(px(8.0))
                        .py(px(6.0))
                        .rounded(px(7.0))
                        .cursor_default()
                        .when(selected, |element| element.bg(theme.overlay))
                        .hover(|element| element.bg(theme.overlay))
                        .active(|element| element.bg(theme.overlay_strong))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .line_height(px(16.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(12.5))
                                        .text_color(if selected {
                                            theme.text
                                        } else {
                                            theme.text_secondary
                                        })
                                        .child(SharedString::from(session.title.clone())),
                                )
                                .when(active, |element| {
                                    element.child(pulse_dot(
                                        format!("session-pulse-{session_id}"),
                                        5.0,
                                        status_color(&theme, session.status),
                                    ))
                                })
                                .child(
                                    div()
                                        .flex_none()
                                        .text_size(px(10.0))
                                        .text_color(theme.text_ghost)
                                        .child(SharedString::from(relative_time(
                                            session.updated_at,
                                        ))),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(5.0))
                                .text_size(px(10.5))
                                .line_height(px(13.0))
                                .child(icon(
                                    provider_icon(session.provider),
                                    10.0,
                                    provider_color(session.provider).opacity(0.8),
                                ))
                                .child(
                                    div()
                                        .text_color(theme.text_tertiary)
                                        .child(session.provider.short_name()),
                                ),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_session(session_id, cx);
                        }))
                        .context_menu_with_id(
                            SharedString::from(format!("session-context-menu-{session_id}")),
                            move |menu, window, cx| {
                                let waku = waku.clone();
                                preserve_composer_focus_for_context_menu(
                                    &composer, menu, window, cx,
                                )
                                .min_w(px(140.0))
                                .item(
                                    PopupMenuItem::new("Remove").on_click(move |_, _, cx| {
                                        let _ = waku.update(cx, |waku, cx| {
                                            waku.remove_session(session_id, cx);
                                        });
                                    }),
                                )
                            },
                        ),
                );
            }
        }

        div()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .child(div().h(px(48.0)).flex_none())
            .child(
                div().px(px(10.0)).child(
                    div()
                        .id("new-session")
                        .h(px(30.0))
                        .px(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .text_size(px(12.5))
                        .line_height(px(16.0))
                        .rounded(px(7.0))
                        .cursor_default()
                        .hover(|element| element.bg(theme.overlay))
                        .active(|element| element.bg(theme.overlay_strong))
                        .child(icon("icons/plus.svg", 13.0, theme.text_secondary))
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(12.5))
                                .text_color(theme.text)
                                .child("New session"),
                        )
                        .child(key_hint(&theme, "⌘N"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(project_id) = this.state.selected_project {
                                this.create_session_for(project_id, this.state.last_provider, cx);
                            }
                        })),
                ),
            )
            .child(
                div()
                    .id("sidebar-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .px(px(10.0))
                    .pt(px(14.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(section_label(&theme, "Projects"))
                            .child(
                                div()
                                    .id("add-project")
                                    .w(px(20.0))
                                    .h(px(20.0))
                                    .rounded(px(6.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_default()
                                    .hover(|element| element.bg(theme.overlay))
                                    .active(|element| element.bg(theme.overlay_strong))
                                    .child(icon("icons/plus.svg", 11.0, theme.text_ghost))
                                    .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
                            ),
                    )
                    .child(projects)
                    .child(div().h(px(16.0)))
                    .child(section_label(&theme, "Sessions"))
                    .child(sessions),
            )
            .child(
                div()
                    .h(px(40.0))
                    .flex_none()
                    .px(px(18.0))
                    .flex()
                    .items_center()
                    .line_height(px(13.0))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(10.5))
                            .text_color(theme.text_ghost)
                            .child("Local only"),
                    )
                    .child(key_hint(&theme, "⌘⇧S")),
            )
    }

    // ── Header ─────────────────────────────────────────────────────────────

    fn render_header(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::dark();
        let session = self.selected_session();
        let provider = session.map(|session| session.provider).unwrap_or_default();
        let status = session.map(|session| session.status).unwrap_or_default();
        div()
            .h(px(46.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .pl(if self.sidebar_visible {
                px(10.0)
            } else {
                px(TRAFFIC_LIGHT_CLEARANCE)
            })
            .pr(px(14.0))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .id("toggle-sidebar")
                    .w(px(26.0))
                    .h(px(26.0))
                    .flex_none()
                    .rounded(px(6.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_default()
                    .hover(|element| element.bg(theme.overlay))
                    .active(|element| element.bg(theme.overlay_strong))
                    .child(icon("icons/panel-left.svg", 14.0, theme.text_tertiary))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_visible = !this.sidebar_visible;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(SharedString::from(
                        session
                            .map(|session| session.title.clone())
                            .unwrap_or_else(|| "New task".into()),
                    )),
            )
            .child(div().flex_1())
            .when(status != SessionStatus::Idle, |element| {
                element.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .text_size(px(11.0))
                        .line_height(px(14.0))
                        .child(match status {
                            SessionStatus::Connecting | SessionStatus::Working => {
                                pulse_dot("header-status-pulse", 5.0, status_color(&theme, status))
                            }
                            _ => div()
                                .w(px(5.0))
                                .h(px(5.0))
                                .rounded_full()
                                .bg(status_color(&theme, status))
                                .into_any_element(),
                        })
                        .child(
                            div()
                                .text_color(status_color(&theme, status))
                                .child(status_label(status)),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(11.5))
                    .line_height(px(14.0))
                    .child(icon(
                        provider_icon(provider),
                        11.0,
                        provider_color(provider).opacity(0.9),
                    ))
                    .child(
                        div()
                            .text_color(theme.text_secondary)
                            .child(provider.short_name()),
                    ),
            )
    }

    // ── Empty states ───────────────────────────────────────────────────────

    fn render_empty_state(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::dark();
        if self.selected_project().is_none() {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .px_8()
                .pb(px(46.0))
                .child(icon("icons/sparkle.svg", 24.0, theme.accent))
                .child(
                    div()
                        .mt(px(16.0))
                        .text_size(px(20.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child("Open a project to begin"),
                )
                .child(
                    div()
                        .mt(px(8.0))
                        .max_w(px(380.0))
                        .text_center()
                        .text_size(px(12.5))
                        .line_height(px(19.0))
                        .text_color(theme.text_tertiary)
                        .child(
                            "Waku runs coding agents in folders you choose. Your code, sessions, and history stay on this Mac.",
                        ),
                )
                .child(
                    div()
                        .id("onboarding-add-project")
                        .mt(px(20.0))
                        .h(px(32.0))
                        .px(px(14.0))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .cursor_default()
                        .bg(theme.inverse)
                        .text_color(theme.on_inverse)
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .hover(|element| element.opacity(0.9))
                        .active(|element| element.opacity(0.8))
                        .child("Open project folder…")
                        .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
                );
        }
        let selected_provider = self
            .selected_session()
            .map(|session| session.provider)
            .unwrap_or_default();
        let project_name = self
            .selected_project()
            .map(|project| project.name.as_str())
            .unwrap_or("your project");
        let probe = self
            .probes
            .iter()
            .find(|probe| probe.provider == selected_provider);
        let caption = match probe {
            Some(probe) if probe.installed => {
                let version = probe
                    .version
                    .as_deref()
                    .unwrap_or("ready")
                    .chars()
                    .take(48)
                    .collect::<String>();
                format!("Ready · {version}")
            }
            _ => format!(
                "Not installed — `{}` was not found on this Mac",
                selected_provider.command()
            ),
        };

        let mut picker = div()
            .flex()
            .items_center()
            .gap(px(2.0))
            .p(px(3.0))
            .rounded(px(9.0))
            .bg(theme.overlay);
        for provider in ProviderKind::ALL {
            let selected = selected_provider == provider;
            let installed = self
                .probes
                .iter()
                .find(|probe| probe.provider == provider)
                .map(|probe| probe.installed)
                .unwrap_or(false);
            picker = picker.child(
                div()
                    .id(SharedString::from(format!("provider-{}", provider.id())))
                    .h(px(28.0))
                    .px(px(11.0))
                    .rounded(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(12.0))
                    .line_height(px(15.0))
                    .cursor_default()
                    .when(selected, |element| element.bg(theme.raised).shadow_sm())
                    .when(!installed, |element| element.opacity(0.5))
                    .active(|element| element.opacity(0.8))
                    .child(icon(
                        provider_icon(provider),
                        11.0,
                        if selected {
                            provider_color(provider)
                        } else {
                            theme.text_tertiary
                        },
                    ))
                    .child(
                        div()
                            .font_weight(if selected {
                                FontWeight::MEDIUM
                            } else {
                                FontWeight::NORMAL
                            })
                            .text_color(if selected {
                                theme.text
                            } else {
                                theme.text_secondary
                            })
                            .child(provider.short_name()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.choose_provider(provider, cx);
                    })),
            );
        }

        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px_8()
            .pb(px(52.0))
            .child(icon("icons/sparkle.svg", 20.0, theme.accent))
            .child(
                div()
                    .mt(px(14.0))
                    .text_size(px(20.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(SharedString::from(format!(
                        "What should we build in {project_name}?"
                    ))),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .text_size(px(12.5))
                    .text_color(theme.text_tertiary)
                    .child("Pick an agent, then describe the outcome you want."),
            )
            .child(div().mt(px(22.0)).child(picker))
            .child(
                div()
                    .mt(px(10.0))
                    .text_size(px(11.0))
                    .text_color(theme.text_ghost)
                    .child(SharedString::from(caption)),
            )
    }

    // ── Transcript ─────────────────────────────────────────────────────────

    fn render_transcript(&self, cx: &mut Context<Self>) -> AnyElement {
        self.sync_transcript_rows();
        let entity = cx.entity().downgrade();
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                list(self.transcript_rows.clone(), move |index, window, cx| {
                    entity
                        .upgrade()
                        .map(|entity| {
                            entity.update(cx, |this, cx| this.transcript_row(index, window, cx))
                        })
                        .unwrap_or_else(|| div().into_any_element())
                })
                .size_full(),
            )
            .into_any_element()
    }

    /// The provider's latest ordered block is still reasoning.
    fn reasoning_live(&self) -> bool {
        self.stream_phase == Some(StreamPhase::Reasoning)
            && self
                .selected_session()
                .is_some_and(|session| session.status == SessionStatus::Working)
    }

    fn toggle_reasoning(&mut self, block_index: usize, current: bool, cx: &mut Context<Self>) {
        self.reasoning_expanded.insert(block_index, !current);
        self.remeasure_transcript_tail();
        cx.notify();
    }

    fn toggle_activities(&mut self, block_index: usize, current: bool, cx: &mut Context<Self>) {
        self.activities_expanded.insert(block_index, !current);
        self.remeasure_transcript_tail();
        cx.notify();
    }

    fn toggle_activity_item(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if !self.expanded_activity_items.remove(&id) {
            self.expanded_activity_items.insert(id);
        }
        self.remeasure_transcript_tail();
        cx.notify();
    }

    /// A single transcript row, self-centered to the content column so the
    /// list can measure it at its true wrap width. Current-turn reasoning and
    /// activity blocks are anchored at the exact boundary between assistant
    /// text segments where their provider events arrived.
    fn transcript_row(
        &self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::dark();
        let composer = self.composer.clone();
        let row_count = self.transcript_row_count();
        let message_count = self
            .selected_session()
            .map(|session| session.messages.len())
            .unwrap_or(0);
        let anchors = self
            .selected_transcript_blocks()
            .iter()
            .map(|block| block.after_message)
            .collect::<Vec<_>>();
        let kind = transcript_row_kinds(message_count, &anchors)
            .get(index)
            .copied()
            .unwrap_or(TranscriptRowKind::Message(index));
        let starts_followup_turn = match kind {
            TranscriptRowKind::Message(message_index) => {
                self.selected_session().is_some_and(|session| {
                    message_starts_followup_turn(&session.messages, message_index)
                })
            }
            TranscriptRowKind::TurnBlock(_) => false,
        };
        let inner = match kind {
            TranscriptRowKind::Message(message_index) => self
                .selected_session()
                .and_then(|session| session.messages.get(message_index))
                .map(|message| render_message(&theme, message, composer, window, cx))
                .unwrap_or_else(|| div().into_any_element()),
            TranscriptRowKind::TurnBlock(block_index) => self
                .selected_transcript_blocks()
                .get(block_index)
                .map(|block| match &block.content {
                    TranscriptBlockContent::Reasoning(reasoning) => {
                        self.render_reasoning_row(reasoning, block_index, &theme, cx)
                    }
                    TranscriptBlockContent::Activities(activities) => {
                        self.render_activities_row(activities, block_index, &theme, cx)
                    }
                })
                .unwrap_or_else(|| div().into_any_element()),
        };
        div()
            .w_full()
            .flex()
            .justify_center()
            .px(px(20.0))
            .py(px(8.0))
            .when(index == 0, |element| element.pt(px(22.0)))
            .when(starts_followup_turn, |element| {
                element.pt(px(FOLLOWUP_TURN_TOP_GAP))
            })
            .when(index + 1 == row_count, |element| element.pb(px(22.0)))
            .child(
                div()
                    .w_full()
                    .max_w(px(CONTENT_MAX_WIDTH))
                    .min_w_0()
                    .child(inner),
            )
            .into_any_element()
    }

    /// The turn's reasoning as a disclosure: open while the provider is
    /// thinking, collapsing to "Thought for Ns" once the answer starts, and
    /// clickable either way.
    fn render_reasoning_row(
        &self,
        reasoning: &ReasoningBlock,
        block_index: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let live =
            self.reasoning_live()
                && self.selected_transcript_blocks().iter().rposition(|block| {
                    matches!(block.content, TranscriptBlockContent::Reasoning(_))
                }) == Some(block_index);
        let expanded = self
            .reasoning_expanded
            .get(&block_index)
            .copied()
            .unwrap_or(live);
        let label = if live {
            "Thinking".to_owned()
        } else {
            format!(
                "Thought for {}s",
                reasoning
                    .finished_at_ms
                    .saturating_sub(reasoning.started_at_ms)
                    .div_ceil(1000)
                    .max(1)
            )
        };
        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .id(SharedString::from(format!("thinking-toggle-{block_index}")))
                    .h(px(22.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(11.0))
                    .line_height(px(14.0))
                    .cursor_default()
                    .child(icon(
                        if expanded {
                            "icons/chevron-down.svg"
                        } else {
                            "icons/chevron-right.svg"
                        },
                        9.0,
                        theme.text_ghost,
                    ))
                    .child(if live {
                        icon("icons/sparkle.svg", 11.0, theme.text_tertiary)
                            .with_animation(
                                SharedString::from(format!("thinking-pulse-{block_index}")),
                                Animation::new(Duration::from_millis(1800))
                                    .repeat()
                                    .with_easing(pulsating_between(0.4, 1.0)),
                                |element, delta| element.opacity(delta),
                            )
                            .into_any_element()
                    } else {
                        icon("icons/sparkle.svg", 11.0, theme.text_ghost).into_any_element()
                    })
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text_tertiary)
                            .child(SharedString::from(label)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_reasoning(block_index, expanded, cx);
                    })),
            )
            .when(expanded, |element| {
                element.child(
                    div()
                        .pl(px(15.0))
                        .text_size(px(12.0))
                        .line_height(px(18.0))
                        .text_color(theme.text_tertiary)
                        .whitespace_normal()
                        .child(SharedString::from(reasoning.content.clone())),
                )
            })
            .into_any_element()
    }

    /// The turn's tool activity as a disclosure: the summary line toggles the
    /// row list, and each row with detail expands to its full content.
    fn render_activities_row(
        &self,
        activities: &[ActivityItem],
        block_index: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let running = activities.iter().any(|activity| !activity.complete);
        let expanded = self
            .activities_expanded
            .get(&block_index)
            .copied()
            .unwrap_or(running);
        let cluster = div().flex().flex_col().gap(px(2.0)).child(
            div()
                .id(SharedString::from(format!("activity-toggle-{block_index}")))
                .h(px(22.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(px(11.0))
                .line_height(px(14.0))
                .cursor_default()
                .child(icon(
                    if expanded {
                        "icons/chevron-down.svg"
                    } else {
                        "icons/chevron-right.svg"
                    },
                    9.0,
                    theme.text_ghost,
                ))
                .when(running, |element| {
                    element.child(pulse_dot(
                        format!("activity-running-{block_index}"),
                        5.0,
                        theme.accent,
                    ))
                })
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text_tertiary)
                        .child(SharedString::from(activity_summary(activities))),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_activities(block_index, expanded, cx);
                })),
        );
        if !expanded {
            return cluster.into_any_element();
        }
        let mut items = div().flex().flex_col().pl(px(15.0));
        for activity in activities {
            let id = activity.id;
            let detail = activity
                .detail
                .clone()
                .filter(|detail| !detail.trim().is_empty());
            let has_detail = detail.is_some();
            let item_expanded = has_detail && self.expanded_activity_items.contains(&id);
            let mut item = div().flex().flex_col().child(
                div()
                    .id(SharedString::from(format!("activity-item-{id}")))
                    .min_h(px(24.0))
                    .px(px(4.0))
                    .py(px(2.0))
                    .rounded(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(11.5))
                    .line_height(px(14.0))
                    .when(has_detail, |element| {
                        element
                            .cursor_default()
                            .hover(|element| element.bg(theme.overlay))
                            .active(|element| element.bg(theme.overlay_strong))
                    })
                    .child(if has_detail {
                        icon(
                            if item_expanded {
                                "icons/chevron-down.svg"
                            } else {
                                "icons/chevron-right.svg"
                            },
                            9.0,
                            theme.text_ghost,
                        )
                        .into_any_element()
                    } else {
                        div().w(px(9.0)).flex_none().into_any_element()
                    })
                    .child(icon(
                        activity_icon(activity.kind),
                        11.0,
                        theme.text_tertiary,
                    ))
                    .child(
                        div()
                            .flex_none()
                            .max_w(px(300.0))
                            .truncate()
                            .text_color(theme.text_secondary)
                            .child(SharedString::from(activity.title.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.0))
                            .text_color(theme.text_ghost)
                            .when(item_expanded, |element| element.invisible())
                            .child(SharedString::from(detail.clone().unwrap_or_default())),
                    )
                    .child(if activity.complete {
                        icon("icons/check.svg", 10.0, theme.text_ghost).into_any_element()
                    } else {
                        pulse_dot(format!("activity-pulse-{id}"), 5.0, theme.accent)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if has_detail {
                            this.toggle_activity_item(id, cx);
                        }
                    })),
            );
            if let Some(detail) = detail.filter(|_| item_expanded) {
                item = item.child(
                    div()
                        .ml(px(21.0))
                        .mt(px(2.0))
                        .mb(px(4.0))
                        .p(px(8.0))
                        .rounded(px(7.0))
                        .bg(theme.inset)
                        .border_1()
                        .border_color(theme.border)
                        .font_family("SF Mono")
                        .text_size(px(10.5))
                        .line_height(px(16.0))
                        .text_color(theme.text_secondary)
                        .whitespace_normal()
                        .child(SharedString::from(detail)),
                );
            }
            items = items.child(item);
        }
        cluster.child(items).into_any_element()
    }

    // ── Permission ─────────────────────────────────────────────────────────

    fn render_permission(&self, cx: &mut Context<Self>) -> Option<Div> {
        let permission = self.pending_permission.as_ref()?;
        let theme = Theme::dark();
        let request_id = permission.request_id.clone();
        let mut buttons = div().flex().items_center().gap(px(8.0)).mt(px(10.0));
        for option in &permission.options {
            let request_id = request_id.clone();
            let option_id = option.id.clone();
            let allow = option.allow;
            buttons = buttons.child(
                div()
                    .id(SharedString::from(format!(
                        "permission-{}-{}",
                        permission.request_id, option.id
                    )))
                    .h(px(28.0))
                    .px(px(13.0))
                    .rounded(px(7.0))
                    .flex()
                    .items_center()
                    .cursor_default()
                    .text_size(px(11.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(allow, |element| {
                        element
                            .bg(theme.inverse)
                            .text_color(theme.on_inverse)
                            .hover(|element| element.opacity(0.9))
                    })
                    .when(!allow, |element| {
                        element
                            .border_1()
                            .border_color(theme.border_strong)
                            .text_color(theme.text_secondary)
                            .hover(|element| element.bg(theme.overlay).text_color(theme.text))
                    })
                    .active(|element| element.opacity(0.8))
                    .child(SharedString::from(option.label.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.respond_permission(request_id.clone(), option_id.clone(), cx);
                    })),
            );
        }
        Some(
            div().px(px(20.0)).pb(px(8.0)).child(
                div()
                    .w_full()
                    .max_w(px(CONTENT_MAX_WIDTH))
                    .mx_auto()
                    .p(px(12.0))
                    .rounded(px(12.0))
                    .border_1()
                    .border_color(theme.border_strong)
                    .bg(theme.raised)
                    .shadow_md()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(icon("icons/alert.svg", 13.0, theme.warning))
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(SharedString::from(permission.title.clone())),
                            ),
                    )
                    .child(
                        div()
                            .id("permission-detail")
                            .mt(px(8.0))
                            .max_h(px(92.0))
                            .overflow_y_scroll()
                            .p(px(8.0))
                            .rounded(px(7.0))
                            .bg(theme.inset)
                            .font_family("SF Mono")
                            .text_size(px(10.5))
                            .line_height(px(16.0))
                            .text_color(theme.text_secondary)
                            .whitespace_normal()
                            .child(SharedString::from(permission.detail.clone())),
                    )
                    .child(buttons),
            ),
        )
    }

    // ── Composer ───────────────────────────────────────────────────────────

    fn render_composer(&self, _window: &Window, cx: &mut Context<Self>) -> Div {
        let theme = Theme::dark();
        let session = self.selected_session();
        let provider = session.map(|session| session.provider).unwrap_or_default();
        let mode = session
            .map(|session| session.runtime_mode)
            .unwrap_or_default();
        let working = session
            .map(|session| {
                matches!(
                    session.status,
                    SessionStatus::Working | SessionStatus::Connecting | SessionStatus::Waiting
                )
            })
            .unwrap_or(false);
        let fresh_session = session
            .map(|session| session.messages.is_empty())
            .unwrap_or(false);
        let has_draft = !self.composer.read(cx).content().trim().is_empty();
        let weak = cx.entity().downgrade();
        let provider_options = ProviderKind::ALL
            .iter()
            .map(|kind| {
                (
                    *kind,
                    self.probes
                        .iter()
                        .find(|probe| probe.provider == *kind)
                        .map(|probe| probe.installed)
                        .unwrap_or(false),
                    *kind == provider,
                )
            })
            .collect::<Vec<_>>();
        div().flex_none().px(px(20.0)).child(
            div()
                .w_full()
                .max_w(px(CONTENT_MAX_WIDTH))
                .mx_auto()
                .rounded(px(13.0))
                .border_1()
                .border_color(theme.border)
                .bg(theme.raised)
                .shadow(vec![BoxShadow {
                    color: hsla(0.0, 0.0, 0.0, 0.24),
                    offset: point(px(0.0), px(6.0)),
                    blur_radius: px(20.0),
                    spread_radius: px(-6.0),
                }])
                .p(px(10.0))
                .child(div().px(px(4.0)).pt(px(2.0)).child(self.composer.clone()))
                .child(
                    div()
                        .mt(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .text_size(px(11.5))
                        .line_height(px(14.0))
                        .child(if fresh_session {
                            // The provider is a real select while the session
                            // has no history; afterwards it is locked in.
                            let weak = weak.clone();
                            let composer = self.composer.clone();
                            MenuChip::new("composer-provider")
                                .icon(
                                    provider_icon(provider),
                                    provider_color(provider).opacity(0.9),
                                )
                                .label(provider.short_name())
                                .dropdown_menu(move |mut menu, _window, cx| {
                                    menu = menu
                                        .action_context(composer.read(cx).focus())
                                        .min_w(px(190.0));
                                    for (kind, installed, checked) in
                                        provider_options.iter().copied()
                                    {
                                        let weak = weak.clone();
                                        menu = menu.item(
                                            PopupMenuItem::new(if installed {
                                                kind.short_name().to_owned()
                                            } else {
                                                format!("{} — not installed", kind.short_name())
                                            })
                                            .icon(
                                                ComponentIcon::default().path(provider_icon(kind)),
                                            )
                                            .checked(checked)
                                            .disabled(!installed)
                                            .on_click(move |_, _, cx| {
                                                let _ = weak.update(cx, |this, cx| {
                                                    this.choose_provider(kind, cx);
                                                });
                                            }),
                                        );
                                    }
                                    menu
                                })
                                .anchor(Corner::BottomLeft)
                                .into_any_element()
                        } else {
                            div()
                                .h(px(24.0))
                                .px(px(7.0))
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .child(icon(
                                    provider_icon(provider),
                                    10.5,
                                    provider_color(provider).opacity(0.9),
                                ))
                                .child(
                                    div()
                                        .text_color(theme.text_secondary)
                                        .child(provider.short_name()),
                                )
                                .into_any_element()
                        })
                        .child({
                            let weak = weak.clone();
                            let composer = self.composer.clone();
                            MenuChip::new("runtime-mode")
                                .label(mode.label())
                                .dropdown_menu(move |mut menu, _window, cx| {
                                    menu = menu
                                        .action_context(composer.read(cx).focus())
                                        .min_w(px(140.0));
                                    for option in
                                        [RuntimeMode::Plan, RuntimeMode::Ask, RuntimeMode::Auto]
                                    {
                                        let weak = weak.clone();
                                        menu = menu.item(
                                            PopupMenuItem::new(option.label())
                                                .checked(option == mode)
                                                .on_click(move |_, _, cx| {
                                                    let _ = weak.update(cx, |this, cx| {
                                                        this.set_runtime_mode(option, cx);
                                                    });
                                                }),
                                        );
                                    }
                                    menu
                                })
                                .anchor(Corner::BottomLeft)
                        })
                        .child(div().flex_1())
                        .child(if working {
                            div()
                                .id("send-or-stop")
                                .w(px(26.0))
                                .h(px(26.0))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_default()
                                .bg(theme.overlay_strong)
                                .hover(|element| element.bg(theme.danger_soft))
                                .active(|element| element.opacity(0.8))
                                .child(icon("icons/stop.svg", 10.0, theme.text))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_turn(cx);
                                }))
                        } else {
                            div()
                                .id("send-or-stop")
                                .w(px(26.0))
                                .h(px(26.0))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(if has_draft {
                                    theme.inverse
                                } else {
                                    theme.overlay_strong
                                })
                                .when(has_draft, |element| {
                                    element
                                        .cursor_default()
                                        .hover(|element| element.opacity(0.9))
                                        .active(|element| element.opacity(0.8))
                                })
                                .child(icon(
                                    "icons/arrow-up.svg",
                                    12.0,
                                    if has_draft {
                                        theme.on_inverse
                                    } else {
                                        theme.text_ghost
                                    },
                                ))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let prompt = this.composer.read(cx).content().trim().to_owned();
                                    if !prompt.is_empty() {
                                        this.composer.update(cx, |input, cx| input.clear(cx));
                                        this.submit_prompt(prompt, cx);
                                    }
                                }))
                        }),
                ),
        )
    }

    fn render_workspace_footer(&self) -> Div {
        let theme = Theme::dark();
        let path = self
            .selected_project()
            .map(|project| compact_path(&project.path))
            .unwrap_or_default();
        div()
            .flex_none()
            .px(px(20.0))
            .pb(px(8.0))
            .pt(px(4.0))
            .child(
                div()
                    .w_full()
                    .max_w(px(CONTENT_MAX_WIDTH))
                    .mx_auto()
                    .h(px(20.0))
                    // Left edge lines up with the composer card's inner icon
                    // column (10px card padding + 7px chip padding).
                    .pl(px(17.0))
                    .pr(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .text_size(px(11.0))
                    .line_height(px(14.0))
                    .when_some(self.branch.clone(), |element, branch| {
                        element
                            .child(icon("icons/git-branch.svg", 10.5, theme.text_tertiary))
                            .child(
                                div()
                                    .text_color(theme.text_secondary)
                                    .child(SharedString::from(branch)),
                            )
                            .child(
                                div()
                                    .w(px(2.5))
                                    .h(px(2.5))
                                    .flex_none()
                                    .rounded_full()
                                    .bg(theme.text_ghost),
                            )
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(10.5))
                            .text_color(theme.text_ghost)
                            .child(SharedString::from(path)),
                    ),
            )
    }
}

impl Render for Waku {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let empty = self
            .selected_session()
            .map(|session| session.messages.is_empty())
            .unwrap_or(true);
        let permission = self.render_permission(cx);
        let toast = self.toast.clone();
        div()
            .key_context("Waku")
            .on_action(cx.listener(Self::new_session_action))
            .on_action(cx.listener(Self::toggle_sidebar_action))
            .on_action(cx.listener(Self::focus_composer_action))
            .on_action(cx.listener(Self::cancel_turn_action))
            .size_full()
            .flex()
            .bg(theme.canvas)
            .text_color(theme.text)
            .font_family(".SystemUIFont")
            .when(self.sidebar_visible, |root| {
                root.child(self.render_sidebar(cx))
            })
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .bg(theme.surface)
                    .when(self.sidebar_visible, |element| {
                        element.border_l_1().border_color(theme.border)
                    })
                    .child(self.render_header(cx))
                    .child(if empty {
                        self.render_empty_state(cx).into_any_element()
                    } else {
                        self.render_transcript(cx)
                    })
                    .children(permission)
                    .when_some(toast, |element, toast| {
                        element.child(
                            div()
                                .px(px(20.0))
                                .pb(px(8.0))
                                .flex()
                                .justify_center()
                                .child(
                                    div()
                                        .px(px(12.0))
                                        .py(px(6.0))
                                        .rounded_full()
                                        .border_1()
                                        .border_color(theme.border_strong)
                                        .bg(theme.raised)
                                        .shadow_sm()
                                        .text_size(px(11.0))
                                        .text_color(theme.danger)
                                        .child(SharedString::from(toast)),
                                ),
                        )
                    })
                    .when(self.selected_project().is_some(), |element| {
                        element
                            .child(self.render_composer(window, cx))
                            .child(self.render_workspace_footer())
                    }),
            )
    }
}

// ── Shared pieces ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TranscriptRowKind {
    Message(usize),
    TurnBlock(usize),
}

/// Interleave live turn blocks at the exact message boundary where their
/// provider events arrived. `anchors[n] == 2` means block `n` renders after
/// messages 0 and 1, before message 2.
fn transcript_row_kinds(message_count: usize, anchors: &[usize]) -> Vec<TranscriptRowKind> {
    let mut blocks_after = vec![Vec::new(); message_count + 1];
    for (block_index, anchor) in anchors.iter().copied().enumerate() {
        blocks_after[anchor.min(message_count)].push(block_index);
    }
    let mut rows = Vec::with_capacity(message_count + anchors.len());
    rows.extend(
        blocks_after[0]
            .iter()
            .copied()
            .map(TranscriptRowKind::TurnBlock),
    );
    for message_index in 0..message_count {
        rows.push(TranscriptRowKind::Message(message_index));
        rows.extend(
            blocks_after[message_index + 1]
                .iter()
                .copied()
                .map(TranscriptRowKind::TurnBlock),
        );
    }
    rows
}

fn message_starts_followup_turn(messages: &[Message], message_index: usize) -> bool {
    messages
        .get(message_index)
        .is_some_and(|message| message.role == MessageRole::User)
        && messages[..message_index]
            .iter()
            .any(|message| message.role == MessageRole::User)
}

fn stream_delta_kind(event: &DriverEvent) -> Option<StreamDeltaKind> {
    match event {
        DriverEvent::TextDelta(_) => Some(StreamDeltaKind::Text),
        DriverEvent::ReasoningDelta(_) => Some(StreamDeltaKind::Reasoning),
        _ => None,
    }
}

fn stream_delta_text(event: &DriverEvent, kind: StreamDeltaKind) -> Option<&str> {
    match (kind, event) {
        (StreamDeltaKind::Text, DriverEvent::TextDelta(text))
        | (StreamDeltaKind::Reasoning, DriverEvent::ReasoningDelta(text)) => Some(text),
        _ => None,
    }
}

fn stream_frame_budget(backlog: usize) -> usize {
    backlog
        .div_ceil(STREAM_CATCH_UP_FRAMES)
        .clamp(
            STREAM_MIN_GRAPHEMES_PER_FRAME,
            STREAM_MAX_GRAPHEMES_PER_FRAME,
        )
        .min(backlog)
}

/// Pop one display-sized chunk while retaining the provider's event order.
///
/// Adjacent deltas of the same kind are coalesced. Large deltas are split on
/// grapheme and line boundaries, so a provider that emits its whole answer in
/// one event still gets the same progressive presentation as token streams.
fn pop_stream_chunk(
    events: &mut VecDeque<DriverEvent>,
    kind: StreamDeltaKind,
) -> Option<DriverEvent> {
    let backlog = events
        .iter()
        .map_while(|event| stream_delta_text(event, kind))
        .map(|text| text.graphemes(true).count())
        .sum();
    if backlog == 0 {
        return events.pop_front();
    }

    let mut remaining_budget = stream_frame_budget(backlog);
    let mut chunk = String::new();
    while remaining_budget > 0 {
        let Some(text) = events.front_mut().and_then(|event| match (kind, event) {
            (StreamDeltaKind::Text, DriverEvent::TextDelta(text))
            | (StreamDeltaKind::Reasoning, DriverEvent::ReasoningDelta(text)) => Some(text),
            _ => None,
        }) else {
            break;
        };

        let (prefix, graphemes) = take_stream_prefix(text, remaining_budget);
        let reached_line_boundary = prefix.ends_with('\n');
        chunk.push_str(&prefix);
        remaining_budget = remaining_budget.saturating_sub(graphemes);
        if text.is_empty() {
            events.pop_front();
        }
        if reached_line_boundary {
            break;
        }
    }

    match kind {
        StreamDeltaKind::Text => Some(DriverEvent::TextDelta(chunk)),
        StreamDeltaKind::Reasoning => Some(DriverEvent::ReasoningDelta(chunk)),
    }
}

fn take_stream_prefix(text: &mut String, budget: usize) -> (String, usize) {
    if text.is_empty() || budget == 0 {
        return (String::new(), 0);
    }

    let mut count = 0;
    let mut end = text.len();
    for (start, grapheme) in text.grapheme_indices(true) {
        count += 1;
        end = start + grapheme.len();
        if grapheme == "\n" || count == budget {
            break;
        }
    }

    let remainder = text.split_off(end);
    (std::mem::replace(text, remainder), count)
}

fn pulse_dot(id: impl Into<SharedString>, size: f32, color: Hsla) -> AnyElement {
    div()
        .w(px(size))
        .h(px(size))
        .flex_none()
        .rounded_full()
        .bg(color)
        .with_animation(
            id.into(),
            Animation::new(Duration::from_millis(1600))
                .repeat()
                .with_easing(pulsating_between(0.3, 1.0)),
            |element, delta| element.opacity(delta),
        )
        .into_any_element()
}

fn render_message(
    theme: &Theme,
    message: &Message,
    composer: Entity<ComposerInput>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let content = message.content.clone();
    let message_id = message.id;
    let role = message.role;
    let code = fenced_code(&content);
    let menu_content = content.clone();
    let element = match role {
        MessageRole::User => div().w_full().flex().justify_end().child(
            div()
                .max_w(px(540.0))
                .rounded(px(12.0))
                .bg(theme.raised)
                .px(px(12.0))
                .py(px(8.0))
                .text_size(px(14.0))
                .line_height(px(20.0))
                .text_color(theme.text)
                .whitespace_normal()
                .child(selectable_plain_text(
                    SharedString::from(format!("message-{message_id}-user")),
                    &content,
                    window,
                    cx,
                )),
        ),
        MessageRole::Assistant => {
            let mut column = div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .py(px(4.0))
                .text_size(px(13.5))
                .line_height(px(21.0))
                .text_color(theme.text)
                .child(
                    TextView::markdown(
                        SharedString::from(format!("message-{message_id}-assistant")),
                        content,
                        window,
                        cx,
                    )
                    .update_delay(STREAM_MARKDOWN_DELAY)
                    .style(assistant_markdown_style(theme))
                    .selectable(true)
                    .w_full()
                    .cursor_text(),
                );
            if message.streaming {
                column = column.child(pulse_dot(
                    format!("stream-{}", message.id),
                    6.0,
                    theme.accent,
                ));
            }
            column
        }
        MessageRole::System => div().w_full().flex().justify_center().child(
            div()
                .px(px(10.0))
                .py(px(4.0))
                .rounded_full()
                .bg(theme.overlay)
                .text_size(px(11.0))
                .line_height(px(16.0))
                .text_color(theme.text_tertiary)
                .child(selectable_plain_text(
                    SharedString::from(format!("message-{message_id}-system")),
                    &content,
                    window,
                    cx,
                )),
        ),
    };

    element
        .id(message_id)
        .context_menu_with_id(
            SharedString::from(format!("message-context-menu-{message_id}")),
            move |menu, window, cx| {
                let copy_content = menu_content.clone();
                let mut menu =
                    preserve_composer_focus_for_context_menu(&composer, menu, window, cx)
                        .min_w(px(170.0))
                        .item(
                            PopupMenuItem::new("Copy Message").on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    copy_content.clone(),
                                ));
                            }),
                        );

                if role == MessageRole::User {
                    let composer = composer.clone();
                    let edit_content = menu_content.clone();
                    menu = menu.item(PopupMenuItem::new("Edit in Composer").on_click(
                        move |_, window, cx| {
                            composer.update(cx, |composer, cx| {
                                composer.set_content(edit_content.clone(), cx);
                            });
                            window.focus(&composer.read(cx).focus());
                        },
                    ));
                }

                if let Some(code) = code.clone() {
                    menu = menu.item(PopupMenuItem::new("Copy Code").on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                    }));
                }

                menu
            },
        )
        .into_any_element()
}

fn assistant_markdown_style(theme: &Theme) -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(0.75))
        .heading_font_size(|level, base| match level {
            1 => base * 1.5,
            2 => base * 1.3,
            3 => base * 1.15,
            4 => base * 1.05,
            _ => base,
        })
        .code_block(
            StyleRefinement::default()
                .bg(theme.inset)
                .border_1()
                .border_color(theme.border_strong)
                .rounded(px(8.0))
                .p(px(12.0))
                .text_size(px(12.0)),
        )
}

fn selectable_plain_text(
    id: impl Into<gpui::ElementId>,
    content: &str,
    window: &mut Window,
    cx: &mut App,
) -> TextView {
    let html = if content.is_empty() {
        "<p></p>".to_owned()
    } else {
        content
            .split('\n')
            .map(|line| format!("<p>{}</p>", escape_html(line)))
            .collect::<String>()
    };
    TextView::html(id, html, window, cx)
        .style(TextViewStyle::default().paragraph_gap(rems(0.0)))
        .selectable(true)
        .w_full()
        .cursor_text()
}

fn escape_html(content: &str) -> String {
    content
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn fenced_code(content: &str) -> Option<String> {
    let mut code_blocks = Vec::new();
    let mut segments = content.split("```");
    let _ = segments.next();
    while let Some(fenced) = segments.next() {
        let (language, code) = fenced
            .split_once('\n')
            .map(|(language, code)| (language.trim(), code))
            .unwrap_or(("", fenced));
        let code = if language.is_empty() && !fenced.contains('\n') {
            fenced
        } else {
            code
        };
        if !code.trim().is_empty() {
            code_blocks.push(code.trim_end().to_owned());
        }
        let _ = segments.next();
    }
    (!code_blocks.is_empty()).then(|| code_blocks.join("\n\n"))
}

fn activity_summary(activities: &[ActivityItem]) -> String {
    let mut counts: Vec<(crate::model::ActivityKind, usize)> = Vec::new();
    for activity in activities {
        if let Some(entry) = counts.iter_mut().find(|(kind, _)| *kind == activity.kind) {
            entry.1 += 1;
        } else {
            counts.push((activity.kind, 1));
        }
    }
    let parts = counts
        .into_iter()
        .map(|(kind, count)| {
            let (singular, plural) = activity_noun(kind);
            format!("{count} {}", if count == 1 { singular } else { plural })
        })
        .collect::<Vec<_>>();
    let running = activities.iter().any(|activity| !activity.complete);
    format!(
        "{} {}",
        if running { "Running" } else { "Ran" },
        parts.join(" · ")
    )
}

fn git_branch(path: &std::path::Path) -> Option<String> {
    let output = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(path)
        .output()
        .ok()?;
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!branch.is_empty()).then_some(branch)
}

#[cfg(test)]
mod tests {
    use super::{
        StreamDeltaKind, TranscriptRowKind::*, escape_html, fenced_code,
        message_starts_followup_turn, pop_stream_chunk, take_stream_prefix, transcript_row_kinds,
    };
    use crate::model::{ActivityKind, DriverEvent, Message, MessageRole};
    use std::collections::VecDeque;

    #[test]
    fn plain_message_html_is_escaped() {
        assert_eq!(
            escape_html("<tag a='b'>&\""),
            "&lt;tag a=&#39;b&#39;&gt;&amp;&quot;"
        );
    }

    #[test]
    fn only_later_user_messages_start_followup_turns() {
        let messages = vec![
            Message::new(MessageRole::User, "first"),
            Message::new(MessageRole::Assistant, "answer"),
            Message::new(MessageRole::User, "follow-up"),
            Message::new(MessageRole::Assistant, "answer"),
        ];
        assert!(!message_starts_followup_turn(&messages, 0));
        assert!(!message_starts_followup_turn(&messages, 1));
        assert!(message_starts_followup_turn(&messages, 2));
        assert!(!message_starts_followup_turn(&messages, 3));
    }

    #[test]
    fn fenced_code_collects_all_blocks_without_languages() {
        let markdown = "Before\n```rust\nfn main() {}\n```\nAfter\n```\ncargo test\n```";
        assert_eq!(
            fenced_code(markdown).as_deref(),
            Some("fn main() {}\n\ncargo test")
        );
        assert_eq!(fenced_code("No code here"), None);
    }

    #[test]
    fn stream_prefix_stops_at_lines_without_splitting_graphemes() {
        let mut text = "hello 👋🏽\nnext line".to_owned();
        let (first, count) = take_stream_prefix(&mut text, 100);
        assert_eq!(first, "hello 👋🏽\n");
        assert_eq!(count, 8);
        assert_eq!(text, "next line");

        let mut emoji = "👨‍👩‍👧‍👦x".to_owned();
        let (first, count) = take_stream_prefix(&mut emoji, 1);
        assert_eq!(first, "👨‍👩‍👧‍👦");
        assert_eq!(count, 1);
        assert_eq!(emoji, "x");
    }

    #[test]
    fn stream_chunks_coalesce_deltas_and_preserve_event_order() {
        let mut events = VecDeque::from([
            DriverEvent::TextDelta("first ".into()),
            DriverEvent::TextDelta("line\nsecond line".into()),
            DriverEvent::Activity {
                id: None,
                kind: ActivityKind::Tool,
                title: "Tool".into(),
                detail: None,
                complete: true,
            },
            DriverEvent::TextDelta("after tool".into()),
        ]);

        assert!(matches!(
            pop_stream_chunk(&mut events, StreamDeltaKind::Text),
            Some(DriverEvent::TextDelta(text)) if text == "first line\n"
        ));
        assert!(matches!(
            events.front(),
            Some(DriverEvent::TextDelta(text)) if text == "second line"
        ));

        assert!(matches!(
            pop_stream_chunk(&mut events, StreamDeltaKind::Text),
            Some(DriverEvent::TextDelta(text)) if text == "second line"
        ));
        assert!(matches!(events.front(), Some(DriverEvent::Activity { .. })));
    }

    #[test]
    fn turn_blocks_keep_their_message_boundaries() {
        // user, assistant text, tool row, assistant text, reasoning row,
        // assistant text
        let rows = transcript_row_kinds(4, &[2, 3]);
        assert_eq!(
            rows,
            vec![
                Message(0),
                Message(1),
                TurnBlock(0),
                Message(2),
                TurnBlock(1),
                Message(3)
            ]
        );
    }

    #[test]
    fn blocks_follow_the_latest_message_without_a_reply() {
        let rows = transcript_row_kinds(2, &[2]);
        assert_eq!(rows, vec![Message(0), Message(1), TurnBlock(0)]);
    }

    #[test]
    fn plain_transcript_maps_one_to_one() {
        let rows = transcript_row_kinds(4, &[]);
        assert_eq!(rows, vec![Message(0), Message(1), Message(2), Message(3)]);
    }

    #[test]
    fn multiple_blocks_at_one_boundary_preserve_event_order() {
        let rows = transcript_row_kinds(2, &[1, 1]);
        assert_eq!(
            rows,
            vec![Message(0), TurnBlock(0), TurnBlock(1), Message(1)]
        );
    }
}
