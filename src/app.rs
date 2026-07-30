use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use gpui::{
    Animation, AnimationExt, AnyElement, App, BoxShadow, Context, Corner, Div, Entity, FocusHandle,
    FontWeight, HighlightStyle, Hsla, IntoElement, ListAlignment, ListState, PathPromptOptions,
    Render, SharedString, StyledText, TextStyle, Timer, Window, div, hsla, list, point, prelude::*,
    pulsating_between, px,
};
use uuid::Uuid;

use crate::driver::{self, DriverHandle};
use crate::input::{ComposerEvent, ComposerInput};
use crate::model::{
    ActivityItem, AgentSession, DriverEvent, Message, MessageRole, PendingPermission, Project,
    ProviderKind, ProviderProbe, RuntimeMode, SessionStatus, compact_path, unix_time,
};
use gpui_component::Icon as ComponentIcon;
use gpui_component::menu::{DropdownMenu, PopupMenuItem};

use crate::persistence::{PersistedState, StateStore};
use crate::theme::Theme;
use crate::ui::{
    InlineSpan, MenuChip, activity_icon, activity_noun, icon, key_hint, parse_inline_markdown,
    provider_color, provider_icon, relative_time, section_label, status_color, status_label,
};
use crate::{CancelTurn, FocusComposer, NewSession, ToggleSidebar};

const TRAFFIC_LIGHT_CLEARANCE: f32 = 86.0;
const CONTENT_MAX_WIDTH: f32 = 720.0;
const SIDEBAR_WIDTH: f32 = 252.0;

pub struct Waku {
    state: PersistedState,
    store: StateStore,
    composer: Entity<ComposerInput>,
    probes: Vec<ProviderProbe>,
    driver: Option<DriverHandle>,
    driver_session: Option<Uuid>,
    driver_events: Option<Receiver<DriverEvent>>,
    activities: Vec<ActivityItem>,
    reasoning: String,
    reasoning_started: Option<Instant>,
    reasoning_finished: Option<Instant>,
    /// User override for the thinking disclosure; `None` follows the turn
    /// (open while thinking, closed once the answer starts).
    reasoning_expanded: Option<bool>,
    /// User override for the activity disclosure; `None` follows the turn.
    activities_expanded: Option<bool>,
    /// Individual tool rows the user has opened to read their full detail.
    expanded_activity_items: HashSet<Uuid>,
    pending_permission: Option<PendingPermission>,
    sidebar_visible: bool,
    branch: Option<String>,
    toast: Option<String>,
    transcript_rows: ListState,
}

impl Waku {
    pub fn new(_window: &mut Window, cx: &mut App) -> Entity<Self> {
        let composer = cx.new(ComposerInput::new);
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let store = StateStore::new(StateStore::default_path());
        let mut state = store.load_or_fresh(cwd);
        for session in &mut state.sessions {
            if session.status != SessionStatus::Idle {
                session.status = SessionStatus::Idle;
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
                    Timer::after(Duration::from_millis(32)).await;
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
                activities: Vec::new(),
                reasoning: String::new(),
                reasoning_started: None,
                reasoning_finished: None,
                reasoning_expanded: None,
                activities_expanded: None,
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

    fn save(&mut self) {
        if let Err(error) = self.store.save(&self.state) {
            self.toast = Some(format!("Could not save local state: {error}"));
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
        let mut assistant = Message::new(MessageRole::Assistant, "");
        assistant.streaming = true;
        session.messages.push(assistant);
        session.status = SessionStatus::Connecting;
        session.updated_at = unix_time();
        self.activities.clear();
        self.reasoning.clear();
        self.reasoning_started = None;
        self.reasoning_finished = None;
        self.reasoning_expanded = None;
        self.activities_expanded = None;
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
                    if let Some(last) = session.messages.last_mut() {
                        last.content = message;
                        last.streaming = false;
                    }
                }
            }
        }
        self.save();
        cx.notify();
    }

    fn drain_driver_events(&mut self) -> bool {
        let Some(receiver) = self.driver_events.clone() else {
            return false;
        };
        let mut changed = false;
        while let Ok(event) = receiver.try_recv() {
            changed = true;
            self.handle_driver_event(event);
        }
        if changed {
            self.save();
            self.remeasure_transcript_tail();
        }
        changed
    }

    /// One list row per message, plus live reasoning and activity clusters.
    fn transcript_row_count(&self) -> usize {
        let messages = self
            .selected_session()
            .map(|session| session.messages.len())
            .unwrap_or(0);
        messages
            + usize::from(!self.reasoning.trim().is_empty())
            + usize::from(!self.activities.is_empty())
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

    /// Streaming mutates the trailing rows in place, so re-measure them.
    fn remeasure_transcript_tail(&self) {
        self.sync_transcript_rows();
        let count = self.transcript_rows.item_count();
        let from = count.saturating_sub(3);
        if from < count {
            self.transcript_rows.splice(from..count, count - from);
        }
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
                if let Some(session) = self.selected_session_mut() {
                    if let Some(message) =
                        session.messages.iter_mut().rev().find(|message| {
                            message.role == MessageRole::Assistant && message.streaming
                        })
                    {
                        message.content.push_str(&delta);
                    } else {
                        let mut message = Message::new(MessageRole::Assistant, delta);
                        message.streaming = true;
                        session.messages.push(message);
                    }
                    session.updated_at = unix_time();
                }
            }
            DriverEvent::ReasoningDelta(delta) => {
                if self.reasoning_started.is_none() {
                    self.reasoning_started = Some(Instant::now());
                }
                self.reasoning_finished = Some(Instant::now());
                self.reasoning.push_str(&delta);
            }
            DriverEvent::Activity {
                kind,
                title,
                detail,
                complete,
            } => {
                if let Some(activity) = self
                    .activities
                    .iter_mut()
                    .rev()
                    .find(|activity| activity.title == title && !activity.complete)
                {
                    activity.complete = complete;
                    if detail.is_some() {
                        activity.detail = detail;
                    }
                } else {
                    self.activities
                        .push(ActivityItem::new(kind, title, detail, complete));
                }
            }
            DriverEvent::Permission {
                request_id,
                title,
                detail,
                options,
            } => {
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
            DriverEvent::TurnFinished { success, summary } => {
                if let Some(session) = self.selected_session_mut() {
                    session.status = if success {
                        SessionStatus::Idle
                    } else {
                        SessionStatus::Failed
                    };
                    if let Some(message) =
                        session.messages.iter_mut().rev().find(|message| {
                            message.role == MessageRole::Assistant && message.streaming
                        })
                    {
                        if message.content.is_empty() {
                            message.content = summary.unwrap_or_else(|| {
                                if success {
                                    "Turn completed.".into()
                                } else {
                                    "The agent stopped before returning a response.".into()
                                }
                            });
                        }
                        message.streaming = false;
                    }
                }
                self.pending_permission = None;
            }
            DriverEvent::Error(error) => {
                self.toast = Some(error.clone());
                if let Some(session) = self.selected_session_mut() {
                    if session.status != SessionStatus::Working {
                        session.status = SessionStatus::Failed;
                    }
                    if let Some(message) =
                        session.messages.iter_mut().rev().find(|message| {
                            message.role == MessageRole::Assistant && message.streaming
                        })
                        && message.content.is_empty()
                    {
                        message.content = error;
                        message.streaming = false;
                    }
                }
            }
            DriverEvent::ProcessExited => {
                if let Some(session) = self.selected_session_mut()
                    && matches!(
                        session.status,
                        SessionStatus::Connecting | SessionStatus::Working | SessionStatus::Waiting
                    )
                {
                    session.status = SessionStatus::Failed;
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
        self.activities.clear();
        self.reasoning.clear();
        self.reasoning_started = None;
        self.reasoning_finished = None;
        self.reasoning_expanded = None;
        self.activities_expanded = None;
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
        if let Some(session) = self.selected_session_mut() {
            session.status = SessionStatus::Idle;
            if let Some(message) = session
                .messages
                .iter_mut()
                .rev()
                .find(|message| message.role == MessageRole::Assistant && message.streaming)
            {
                if message.content.is_empty() {
                    message.content = "Stopped.".into();
                }
                message.streaming = false;
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
                    .cursor_pointer()
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
                sessions = sessions.child(
                    div()
                        .id(SharedString::from(format!("session-{}", session.id)))
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .px(px(8.0))
                        .py(px(6.0))
                        .rounded(px(7.0))
                        .cursor_pointer()
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
                        })),
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
                        .cursor_pointer()
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
                                    .cursor_pointer()
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
                    .cursor_pointer()
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
                        .cursor_pointer()
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
                    .cursor_pointer()
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
                list(self.transcript_rows.clone(), move |index, _window, cx| {
                    entity
                        .upgrade()
                        .map(|entity| entity.update(cx, |this, cx| this.transcript_row(index, cx)))
                        .unwrap_or_else(|| div().into_any_element())
                })
                .size_full(),
            )
            .into_any_element()
    }

    /// Seconds the provider spent streaming reasoning this turn.
    fn reasoning_seconds(&self) -> u64 {
        match (self.reasoning_started, self.reasoning_finished) {
            (Some(started), Some(finished)) => finished.duration_since(started).as_secs().max(1),
            _ => 1,
        }
    }

    /// The provider is still thinking: the turn's answer has not started.
    fn reasoning_live(&self) -> bool {
        self.selected_session()
            .and_then(|session| session.messages.last())
            .map(|message| {
                message.role == MessageRole::Assistant
                    && message.streaming
                    && message.content.is_empty()
            })
            .unwrap_or(false)
    }

    fn activities_running(&self) -> bool {
        self.activities.iter().any(|activity| !activity.complete)
    }

    fn toggle_reasoning(&mut self, cx: &mut Context<Self>) {
        let current = self.reasoning_expanded.unwrap_or(self.reasoning_live());
        self.reasoning_expanded = Some(!current);
        self.remeasure_transcript_tail();
        cx.notify();
    }

    fn toggle_activities(&mut self, cx: &mut Context<Self>) {
        let current = self
            .activities_expanded
            .unwrap_or(self.activities_running());
        self.activities_expanded = Some(!current);
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
    /// list can measure it at its true wrap width. The live reasoning and
    /// activity clusters belong to the in-flight (or just-finished) turn, so
    /// they render in chronological position: right before the assistant
    /// message they produced, never pinned to the end of the transcript.
    fn transcript_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::dark();
        let row_count = self.transcript_row_count();
        let (message_count, last_is_assistant) = self
            .selected_session()
            .map(|session| {
                (
                    session.messages.len(),
                    session
                        .messages
                        .last()
                        .map(|last| last.role == MessageRole::Assistant)
                        .unwrap_or(false),
                )
            })
            .unwrap_or((0, false));
        let kind = transcript_row_kind(
            message_count,
            last_is_assistant,
            !self.reasoning.trim().is_empty(),
            !self.activities.is_empty(),
            index,
        );
        let inner = match kind {
            TranscriptRowKind::Message(message_index) => self
                .selected_session()
                .and_then(|session| session.messages.get(message_index))
                .map(|message| render_message(&theme, message))
                .unwrap_or_else(|| div().into_any_element()),
            TranscriptRowKind::Reasoning => self.render_reasoning_row(&theme, cx),
            TranscriptRowKind::Activities => self.render_activities_row(&theme, cx),
        };
        div()
            .w_full()
            .flex()
            .justify_center()
            .px(px(20.0))
            .py(px(8.0))
            .when(index == 0, |element| element.pt(px(22.0)))
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
    fn render_reasoning_row(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let live = self.reasoning_live();
        let expanded = self.reasoning_expanded.unwrap_or(live);
        let label = if live {
            "Thinking".to_owned()
        } else {
            format!("Thought for {}s", self.reasoning_seconds())
        };
        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .id("thinking-toggle")
                    .h(px(22.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(11.0))
                    .line_height(px(14.0))
                    .cursor_pointer()
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
                                "thinking-pulse",
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
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_reasoning(cx))),
            )
            .when(expanded, |element| {
                element.child(
                    div()
                        .pl(px(15.0))
                        .text_size(px(12.0))
                        .line_height(px(18.0))
                        .text_color(theme.text_tertiary)
                        .whitespace_normal()
                        .child(SharedString::from(self.reasoning.clone())),
                )
            })
            .into_any_element()
    }

    /// The turn's tool activity as a disclosure: the summary line toggles the
    /// row list, and each row with detail expands to its full content.
    fn render_activities_row(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let running = self.activities_running();
        let expanded = self.activities_expanded.unwrap_or(running);
        let cluster = div().flex().flex_col().gap(px(2.0)).child(
            div()
                .id("activity-toggle")
                .h(px(22.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(px(11.0))
                .line_height(px(14.0))
                .cursor_pointer()
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
                    element.child(pulse_dot("activity-running", 5.0, theme.accent))
                })
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text_tertiary)
                        .child(SharedString::from(activity_summary(&self.activities))),
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_activities(cx))),
        );
        if !expanded {
            return cluster.into_any_element();
        }
        let mut items = div().flex().flex_col().pl(px(15.0));
        for activity in &self.activities {
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
                            .cursor_pointer()
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
                    .cursor_pointer()
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

    fn render_composer(&self, window: &Window, cx: &mut Context<Self>) -> Div {
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
        let focused = self.composer.read(cx).focus().is_focused(window);
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
                .border_color(if focused {
                    theme.border_strong
                } else {
                    theme.border
                })
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
                            MenuChip::new("composer-provider")
                                .icon(
                                    provider_icon(provider),
                                    provider_color(provider).opacity(0.9),
                                )
                                .label(provider.short_name())
                                .dropdown_menu(move |mut menu, _window, _cx| {
                                    menu = menu.min_w(px(190.0));
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
                            MenuChip::new("runtime-mode")
                                .label(mode.label())
                                .dropdown_menu(move |mut menu, _window, _cx| {
                                    menu = menu.min_w(px(140.0));
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
                                .cursor_pointer()
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
                                        .cursor_pointer()
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
    Reasoning,
    Activities,
}

/// Maps a flat list index onto the transcript's chronological order: earlier
/// messages, then the live reasoning and activity clusters, then the
/// assistant message they belong to. When the turn has no trailing assistant
/// message the clusters simply follow the last message.
fn transcript_row_kind(
    message_count: usize,
    last_is_assistant: bool,
    has_reasoning: bool,
    has_activities: bool,
    index: usize,
) -> TranscriptRowKind {
    let anchor = if last_is_assistant {
        message_count.saturating_sub(1)
    } else {
        message_count
    };
    let cluster_rows = usize::from(has_reasoning) + usize::from(has_activities);
    if index < anchor {
        return TranscriptRowKind::Message(index);
    }
    if has_reasoning && index == anchor {
        return TranscriptRowKind::Reasoning;
    }
    if has_activities && index == anchor + usize::from(has_reasoning) {
        return TranscriptRowKind::Activities;
    }
    TranscriptRowKind::Message(index - cluster_rows)
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

fn render_message(theme: &Theme, message: &Message) -> AnyElement {
    match message.role {
        MessageRole::User => div()
            .w_full()
            .flex()
            .justify_end()
            .child(
                div()
                    .max_w(px(540.0))
                    .rounded(px(12.0))
                    .bg(theme.raised)
                    .px(px(12.0))
                    .py(px(8.0))
                    .text_size(px(13.0))
                    .line_height(px(20.0))
                    .text_color(theme.text)
                    .whitespace_normal()
                    .child(SharedString::from(message.content.clone())),
            )
            .into_any_element(),
        MessageRole::Assistant => {
            let mut column = div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(render_rich_text(theme, &message.content));
            if message.streaming {
                column = column.child(pulse_dot(
                    format!("stream-{}", message.id),
                    6.0,
                    theme.accent,
                ));
            }
            column.into_any_element()
        }
        MessageRole::System => div()
            .w_full()
            .flex()
            .justify_center()
            .child(
                div()
                    .px(px(10.0))
                    .py(px(4.0))
                    .rounded_full()
                    .bg(theme.overlay)
                    .text_size(px(11.0))
                    .text_color(theme.text_tertiary)
                    .child(SharedString::from(message.content.clone())),
            )
            .into_any_element(),
    }
}

/// Renders assistant markdown-ish content: fenced code blocks, paragraphs,
/// bullet and numbered lists, and inline `code` / **bold** spans.
fn render_rich_text(theme: &Theme, content: &str) -> Div {
    let mut root = div().flex().flex_col().gap(px(8.0));
    let mut in_code = false;
    for segment in content.split("```") {
        if !segment.is_empty() {
            if in_code {
                let (language, code) = segment
                    .split_once('\n')
                    .map(|(language, code)| (language.trim(), code))
                    .unwrap_or(("", segment));
                let code = code.trim_end();
                if !code.is_empty() {
                    root = root.child(render_code_block(theme, language, code));
                }
            } else {
                for block in segment.split("\n\n") {
                    let block = block.trim();
                    if !block.is_empty() {
                        root = root.child(render_text_block(theme, block));
                    }
                }
            }
        }
        in_code = !in_code;
    }
    root
}

fn render_code_block(theme: &Theme, language: &str, code: &str) -> Div {
    let mut block = div()
        .w_full()
        .rounded(px(8.0))
        .bg(theme.inset)
        .border_1()
        .border_color(theme.border)
        .my(px(2.0));
    if !language.is_empty() {
        block = block.child(
            div()
                .px(px(10.0))
                .pt(px(7.0))
                .text_size(px(9.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text_ghost)
                .child(SharedString::from(language.to_ascii_uppercase())),
        );
    }
    block.child(
        div()
            .px(px(10.0))
            .py(px(8.0))
            .font_family("SF Mono")
            .text_size(px(11.5))
            .line_height(px(17.5))
            .text_color(theme.code_text)
            .whitespace_normal()
            .child(SharedString::from(code.to_owned())),
    )
}

/// A paragraph, heading, or list chunk between blank lines.
fn render_text_block(theme: &Theme, block: &str) -> Div {
    let lines = block.lines().map(str::trim_end).collect::<Vec<_>>();
    let is_list_line = |line: &str| {
        let trimmed = line.trim_start();
        trimmed.starts_with("- ")
            || trimmed.starts_with("* ")
            || trimmed.starts_with("• ")
            || numbered_prefix(trimmed).is_some()
    };
    if lines.iter().any(|line| is_list_line(line)) {
        let mut list = div().flex().flex_col().gap(px(5.0));
        for line in lines {
            let trimmed = line.trim_start();
            if let Some((marker, rest)) = list_item(trimmed) {
                list = list.child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(9.0))
                        .child(
                            div()
                                .flex_none()
                                .min_w(px(11.0))
                                .text_size(px(13.5))
                                .line_height(px(21.0))
                                .text_color(theme.text_tertiary)
                                .child(SharedString::from(marker)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(inline_text(theme, rest, 13.5, 21.0, theme.text)),
                        ),
                );
            } else if !trimmed.is_empty() {
                list = list.child(inline_text(theme, trimmed, 13.5, 21.0, theme.text));
            }
        }
        return list;
    }
    if let Some(rest) = block
        .strip_prefix("### ")
        .or_else(|| block.strip_prefix("## "))
    {
        return div()
            .mt(px(4.0))
            .text_size(px(14.0))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.text)
            .child(SharedString::from(rest.to_owned()));
    }
    if let Some(rest) = block.strip_prefix("# ") {
        return div()
            .mt(px(4.0))
            .text_size(px(15.5))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.text)
            .child(SharedString::from(rest.to_owned()));
    }
    div().child(inline_text(theme, block, 13.5, 21.0, theme.text))
}

fn list_item(line: &str) -> Option<(String, &str)> {
    for bullet in ["- ", "* ", "• "] {
        if let Some(rest) = line.strip_prefix(bullet) {
            return Some(("•".to_owned(), rest));
        }
    }
    numbered_prefix(line)
}

fn numbered_prefix(line: &str) -> Option<(String, &str)> {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    let rest = line[digits..].strip_prefix(". ")?;
    Some((format!("{}.", &line[..digits]), rest))
}

/// Body text with inline `code` and **bold** spans styled via highlights.
fn inline_text(theme: &Theme, text: &str, size: f32, line_height: f32, color: Hsla) -> AnyElement {
    let (cleaned, spans) = parse_inline_markdown(text);
    if spans.is_empty() {
        return div()
            .text_size(px(size))
            .line_height(px(line_height))
            .text_color(color)
            .whitespace_normal()
            .child(SharedString::from(cleaned))
            .into_any_element();
    }
    let mut style = TextStyle::default();
    style.color = color;
    style.font_family = ".SystemUIFont".into();
    style.font_size = px(size).into();
    style.line_height = px(line_height).into();
    let highlights = spans
        .into_iter()
        .map(|(range, kind)| {
            (
                range,
                match kind {
                    InlineSpan::Code => HighlightStyle {
                        color: Some(theme.chip_text),
                        background_color: Some(theme.chip_bg),
                        ..Default::default()
                    },
                    InlineSpan::Bold => HighlightStyle {
                        font_weight: Some(FontWeight::SEMIBOLD),
                        ..Default::default()
                    },
                },
            )
        })
        .collect::<Vec<_>>();
    div()
        .whitespace_normal()
        .child(StyledText::new(cleaned).with_default_highlights(&style, highlights))
        .into_any_element()
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
    use super::{TranscriptRowKind::*, transcript_row_kind};

    #[test]
    fn clusters_render_before_the_turns_assistant_message() {
        // user, assistant, user, assistant(streaming) + reasoning + activities
        let rows = (0..6)
            .map(|index| transcript_row_kind(4, true, true, true, index))
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            vec![
                Message(0),
                Message(1),
                Message(2),
                Reasoning,
                Activities,
                Message(3)
            ]
        );
    }

    #[test]
    fn clusters_follow_messages_when_no_assistant_reply_yet() {
        let rows = (0..3)
            .map(|index| transcript_row_kind(2, false, true, false, index))
            .collect::<Vec<_>>();
        assert_eq!(rows, vec![Message(0), Message(1), Reasoning]);
    }

    #[test]
    fn plain_transcript_maps_one_to_one() {
        let rows = (0..4)
            .map(|index| transcript_row_kind(4, true, false, false, index))
            .collect::<Vec<_>>();
        assert_eq!(rows, vec![Message(0), Message(1), Message(2), Message(3)]);
    }

    #[test]
    fn single_cluster_still_anchors_before_the_reply() {
        let rows = (0..3)
            .map(|index| transcript_row_kind(2, true, false, true, index))
            .collect::<Vec<_>>();
        assert_eq!(rows, vec![Message(0), Activities, Message(1)]);
    }
}
