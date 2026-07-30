use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crossbeam_channel::{Receiver, unbounded};
use gpui::{
    AnyElement, App, Context, Div, Entity, FocusHandle, Hsla, IntoElement, PathPromptOptions,
    Render, SharedString, Timer, Window, div, hsla, prelude::*, px, rgb,
};
use uuid::Uuid;

use crate::driver::{self, DriverHandle};
use crate::input::{ComposerEvent, ComposerInput};
use crate::model::{
    ActivityItem, ActivityKind, AgentSession, DriverEvent, Message, MessageRole, PendingPermission,
    Project, ProviderKind, ProviderProbe, RuntimeMode, SessionStatus, compact_path, unix_time,
};
use crate::persistence::{PersistedState, StateStore};
use crate::theme::Theme;
use crate::{CancelTurn, FocusComposer, NewSession, ToggleSidebar};

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
    pending_permission: Option<PendingPermission>,
    sidebar_visible: bool,
    branch: Option<String>,
    toast: Option<String>,
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
                pending_permission: None,
                sidebar_visible: true,
                branch,
                toast: None,
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
        self.pending_permission = None;
        self.toast = None;
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
        }
        changed
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
            DriverEvent::ReasoningDelta(delta) => self.reasoning.push_str(&delta),
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

    fn cycle_runtime_mode(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.selected_session_mut() {
            session.runtime_mode = match session.runtime_mode {
                RuntimeMode::Ask => RuntimeMode::Auto,
                RuntimeMode::Auto => RuntimeMode::Plan,
                RuntimeMode::Plan => RuntimeMode::Ask,
            };
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

    fn render_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::dark();
        let selected_project = self.state.selected_project;
        let selected_session = self.state.selected_session;
        let mut projects = div().flex().flex_col().gap_1();
        for project in &self.state.projects {
            let project_id = project.id;
            let selected = selected_project == Some(project.id);
            projects = projects.child(
                div()
                    .id(SharedString::from(format!("project-{}", project.id)))
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(34.0))
                    .px_2()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |element| element.bg(theme.panel_hover))
                    .hover(|element| element.bg(theme.panel_hover))
                    .child(
                        div()
                            .w(px(22.0))
                            .h(px(22.0))
                            .rounded_md()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(if selected {
                                theme.accent_soft
                            } else {
                                theme.elevated
                            })
                            .text_size(px(11.0))
                            .text_color(if selected {
                                theme.accent
                            } else {
                                theme.text_muted
                            })
                            .child(project.name.chars().next().unwrap_or('P').to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_size(px(13.0))
                            .font_weight(if selected {
                                gpui::FontWeight::MEDIUM
                            } else {
                                gpui::FontWeight::NORMAL
                            })
                            .text_color(if selected {
                                theme.text
                            } else {
                                theme.text_muted
                            })
                            .child(project.name.clone()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_project(project_id, cx);
                    })),
            );
        }

        let mut sessions = div().flex().flex_col().gap_1();
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
                sessions = sessions.child(
                    div()
                        .id(SharedString::from(format!("session-{}", session.id)))
                        .flex()
                        .items_center()
                        .gap_2()
                        .min_h(px(40.0))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .when(selected, |element| element.bg(theme.panel_hover))
                        .hover(|element| element.bg(theme.panel_hover))
                        .child(status_dot(session.status, session.provider))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .overflow_hidden()
                                .child(
                                    div()
                                        .text_size(px(12.5))
                                        .text_color(if selected {
                                            theme.text
                                        } else {
                                            theme.text_muted
                                        })
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(session.title.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.0))
                                        .text_color(theme.text_faint)
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
            .w(px(276.0))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .h(px(54.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .pl(px(78.0))
                    .pr_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_size(px(13.0))
                            .child(
                                div()
                                    .w(px(20.0))
                                    .h(px(20.0))
                                    .rounded_md()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(theme.accent)
                                    .text_color(theme.canvas)
                                    .text_size(px(12.0))
                                    .child("W"),
                            )
                            .child("Waku"),
                    ),
            )
            .child(
                div()
                    .id("sidebar-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .px_2()
                    .py_3()
                    .child(section_label("Projects"))
                    .child(projects)
                    .child(
                        div()
                            .id("add-project")
                            .mt_1()
                            .h(px(30.0))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_md()
                            .cursor_pointer()
                            .text_size(px(12.0))
                            .text_color(theme.text_faint)
                            .hover(|element| {
                                element.bg(theme.panel_hover).text_color(theme.text_muted)
                            })
                            .child("+")
                            .child("Add project")
                            .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
                    )
                    .child(
                        div()
                            .mt_5()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(section_label("Sessions"))
                            .child(
                                div()
                                    .id("sidebar-new-session")
                                    .w(px(24.0))
                                    .h(px(24.0))
                                    .rounded_md()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .text_size(px(16.0))
                                    .text_color(theme.text_faint)
                                    .hover(|element| {
                                        element.bg(theme.panel_hover).text_color(theme.text)
                                    })
                                    .child("+")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(project_id) = this.state.selected_project {
                                            this.create_session_for(
                                                project_id,
                                                this.state.last_provider,
                                                cx,
                                            );
                                        }
                                    })),
                            ),
                    )
                    .child(sessions),
            )
            .child(
                div()
                    .h(px(50.0))
                    .flex_none()
                    .px_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child("Local only")
                    .child("⌘⇧S"),
            )
    }

    fn render_header(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::dark();
        let session = self.selected_session();
        let project = self.selected_project();
        let provider = session.map(|session| session.provider).unwrap_or_default();
        let status = session.map(|session| session.status).unwrap_or_default();
        div()
            .h(px(54.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .px_4()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .id("toggle-sidebar")
                            .w(px(28.0))
                            .h(px(28.0))
                            .rounded_md()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_color(theme.text_faint)
                            .hover(|element| element.bg(theme.panel_hover).text_color(theme.text))
                            .child("☷")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sidebar_visible = !this.sidebar_visible;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(
                                        session
                                            .map(|session| session.title.clone())
                                            .unwrap_or_else(|| "New task".into()),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_size(px(10.0))
                                    .text_color(theme.text_faint)
                                    .child(
                                        project
                                            .map(|project| compact_path(&project.path))
                                            .unwrap_or_else(|| "No project".into()),
                                    )
                                    .when_some(self.branch.clone(), |element, branch| {
                                        element.child("·").child(format!("⑂ {branch}"))
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .h(px(28.0))
                            .rounded_md()
                            .bg(theme.panel)
                            .border_1()
                            .border_color(theme.border)
                            .child(provider_badge(provider, false))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(theme.text_muted)
                                    .child(provider.short_name()),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_size(px(10.0))
                            .text_color(status_color(status))
                            .child("●")
                            .child(status_label(status)),
                    ),
            )
    }

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
                .pb(px(54.0))
                .child(
                    div()
                        .w(px(46.0))
                        .h(px(46.0))
                        .rounded_xl()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(theme.accent_soft)
                        .text_color(theme.accent)
                        .text_size(px(20.0))
                        .child("⌘"),
                )
                .child(
                    div()
                        .mt_4()
                        .text_size(px(22.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child("Choose a project to get started"),
                )
                .child(
                    div()
                        .mt_2()
                        .max_w(px(430.0))
                        .text_center()
                        .text_size(px(13.0))
                        .line_height(px(20.0))
                        .text_color(theme.text_muted)
                        .child(
                            "Waku runs agents inside a folder you choose. Your code and session history stay local.",
                        ),
                )
                .child(
                    div()
                        .id("onboarding-add-project")
                        .mt_5()
                        .h(px(36.0))
                        .px_4()
                        .rounded_lg()
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .bg(theme.accent)
                        .text_color(theme.canvas)
                        .text_size(px(12.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .hover(|element| element.opacity(0.88))
                        .child("+")
                        .child("Open project folder")
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
        let mut provider_cards = div().flex().gap_2().mt_5();
        for provider in ProviderKind::ALL {
            let selected = selected_provider == provider;
            let probe = self.probes.iter().find(|probe| probe.provider == provider);
            let installed = probe.map(|probe| probe.installed).unwrap_or(false);
            provider_cards = provider_cards.child(
                div()
                    .id(SharedString::from(format!(
                        "provider-card-{}",
                        provider.id()
                    )))
                    .w(px(132.0))
                    .h(px(104.0))
                    .flex()
                    .flex_col()
                    .justify_between()
                    .p_3()
                    .rounded_xl()
                    .border_1()
                    .border_color(if selected { theme.accent } else { theme.border })
                    .bg(if selected {
                        theme.accent_soft
                    } else {
                        theme.panel
                    })
                    .cursor_pointer()
                    .hover(|element| {
                        element
                            .bg(theme.panel_hover)
                            .border_color(theme.border_strong)
                    })
                    .child(provider_badge(provider, true))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(provider.short_name()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_size(px(9.5))
                                    .text_color(if installed {
                                        theme.success
                                    } else {
                                        theme.danger
                                    })
                                    .child("●")
                                    .child(if installed {
                                        format!("Ready · {}", provider.integration_label())
                                    } else {
                                        "Not found".into()
                                    }),
                            ),
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
            .pb(px(80.0))
            .child(
                div()
                    .w(px(42.0))
                    .h(px(42.0))
                    .rounded_xl()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.accent_soft)
                    .text_color(theme.accent)
                    .text_size(px(19.0))
                    .child("✦"),
            )
            .child(
                div()
                    .mt_4()
                    .text_size(px(22.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(format!("What should we build in {project_name}?")),
            )
            .child(
                div()
                    .mt_2()
                    .text_size(px(13.0))
                    .text_color(theme.text_muted)
                    .child("Choose an agent, then describe the outcome you want."),
            )
            .child(provider_cards)
    }

    fn render_transcript(&self) -> AnyElement {
        let theme = Theme::dark();
        let mut content = div()
            .w_full()
            .max_w(px(820.0))
            .mx_auto()
            .flex()
            .flex_col()
            .gap_5()
            .px_6()
            .py_7();
        if let Some(session) = self.selected_session() {
            for message in &session.messages {
                content = content.child(render_message(message));
            }
        }
        if !self.reasoning.trim().is_empty() {
            content = content.child(
                div()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.panel)
                    .p_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(11.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text_muted)
                            .child("⌁")
                            .child("Reasoning"),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_size(px(12.0))
                            .line_height(px(19.0))
                            .text_color(theme.text_faint)
                            .whitespace_normal()
                            .child(self.reasoning.clone()),
                    ),
            );
        }
        if !self.activities.is_empty() {
            let mut list = div().flex().flex_col().gap_1();
            for activity in &self.activities {
                list = list.child(render_activity(activity));
            }
            content = content.child(
                div()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.panel)
                    .p_2()
                    .child(list),
            );
        }
        div()
            .id("transcript")
            .flex_1()
            .overflow_y_scroll()
            .child(content)
            .into_any_element()
    }

    fn render_permission(&self, cx: &mut Context<Self>) -> Option<Div> {
        let permission = self.pending_permission.as_ref()?;
        let theme = Theme::dark();
        let request_id = permission.request_id.clone();
        let mut buttons = div().flex().items_center().gap_2().mt_3();
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
                    .px_3()
                    .rounded_md()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_size(px(11.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .bg(if allow { theme.accent } else { theme.elevated })
                    .text_color(if allow {
                        theme.canvas
                    } else {
                        theme.text_muted
                    })
                    .hover(move |element| {
                        if allow {
                            element.opacity(0.88)
                        } else {
                            element.bg(theme.panel_hover).text_color(theme.text)
                        }
                    })
                    .child(option.label.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.respond_permission(request_id.clone(), option_id.clone(), cx);
                    })),
            );
        }
        Some(
            div()
                .mx_4()
                .mb_2()
                .p_3()
                .rounded_xl()
                .border_1()
                .border_color(theme.warning.opacity(0.42))
                .bg(hsla(38.0 / 360.0, 0.35, 0.12, 1.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(12.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.warning)
                        .child("◇")
                        .child(permission.title.clone()),
                )
                .child(
                    div()
                        .id("permission-detail")
                        .mt_2()
                        .max_h(px(80.0))
                        .overflow_y_scroll()
                        .font_family("SF Mono")
                        .text_size(px(10.5))
                        .line_height(px(16.0))
                        .text_color(theme.text_muted)
                        .whitespace_normal()
                        .child(permission.detail.clone()),
                )
                .child(buttons),
        )
    }

    fn render_composer(&self, cx: &mut Context<Self>) -> Div {
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
        div().flex_none().px_4().pb_4().child(
            div()
                .w_full()
                .max_w(px(860.0))
                .mx_auto()
                .rounded_xl()
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.panel)
                .shadow_lg()
                .p_3()
                .child(self.composer.clone())
                .child(
                    div()
                        .mt_3()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .h(px(25.0))
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .rounded_md()
                                        .bg(theme.elevated)
                                        .child(provider_badge(provider, false))
                                        .child(
                                            div()
                                                .text_size(px(10.5))
                                                .text_color(theme.text_muted)
                                                .child(provider.short_name()),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("runtime-mode")
                                        .h(px(25.0))
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .text_size(px(10.5))
                                        .text_color(theme.text_muted)
                                        .hover(|element| {
                                            element.bg(theme.elevated).text_color(theme.text)
                                        })
                                        .child("◌")
                                        .child(mode.label())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.cycle_runtime_mode(cx);
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_size(px(9.5))
                                        .text_color(theme.text_faint)
                                        .child(if working { "Esc to stop" } else { "↵ Send" }),
                                )
                                .child(
                                    div()
                                        .id("send-or-stop")
                                        .w(px(28.0))
                                        .h(px(28.0))
                                        .rounded_lg()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .bg(if working {
                                            theme.elevated
                                        } else {
                                            theme.accent
                                        })
                                        .text_color(if working { theme.text } else { theme.canvas })
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(if working { "■" } else { "↑" })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if working {
                                                this.cancel_turn(cx);
                                            } else {
                                                let prompt = this
                                                    .composer
                                                    .read(cx)
                                                    .content()
                                                    .trim()
                                                    .to_owned();
                                                if !prompt.is_empty() {
                                                    this.composer
                                                        .update(cx, |input, cx| input.clear(cx));
                                                    this.submit_prompt(prompt, cx);
                                                }
                                            }
                                        })),
                                ),
                        ),
                ),
        )
    }
}

impl Render for Waku {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let empty = self
            .selected_session()
            .map(|session| session.messages.is_empty())
            .unwrap_or(true);
        let permission = self.render_permission(cx);
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
                    .child(self.render_header(cx))
                    .child(if empty {
                        self.render_empty_state(cx).into_any_element()
                    } else {
                        self.render_transcript()
                    })
                    .children(permission)
                    .when_some(self.toast.clone(), |element, toast| {
                        element.child(
                            div()
                                .mx_4()
                                .mb_2()
                                .px_3()
                                .py_2()
                                .rounded_lg()
                                .border_1()
                                .border_color(theme.danger.opacity(0.34))
                                .bg(hsla(3.0 / 360.0, 0.30, 0.13, 1.0))
                                .text_size(px(11.0))
                                .text_color(theme.danger)
                                .child(toast),
                        )
                    })
                    .when(self.selected_project().is_some(), |element| {
                        element.child(self.render_composer(cx))
                    }),
            )
    }
}

fn section_label(label: &'static str) -> Div {
    let theme = Theme::dark();
    div()
        .h(px(26.0))
        .flex()
        .items_center()
        .px_2()
        .text_size(px(9.5))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.text_faint)
        .child(label.to_ascii_uppercase())
}

fn provider_color(provider: ProviderKind) -> Hsla {
    match provider {
        ProviderKind::Claude => rgb(0xe99b68).into(),
        ProviderKind::Codex => rgb(0xc8f169).into(),
        ProviderKind::OpenCode => rgb(0xa78bfa).into(),
        ProviderKind::Grok => rgb(0xe5e7eb).into(),
    }
}

fn provider_badge(provider: ProviderKind, large: bool) -> Div {
    let color = provider_color(provider);
    let size = if large { 28.0 } else { 16.0 };
    div()
        .w(px(size))
        .h(px(size))
        .flex_none()
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .bg(color.opacity(0.13))
        .text_color(color)
        .text_size(px(if large { 13.0 } else { 9.0 }))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(provider.glyph())
}

fn status_color(status: SessionStatus) -> Hsla {
    let theme = Theme::dark();
    match status {
        SessionStatus::Idle => theme.text_faint,
        SessionStatus::Connecting | SessionStatus::Working => theme.accent,
        SessionStatus::Waiting => theme.warning,
        SessionStatus::Failed => theme.danger,
    }
}

fn status_dot(status: SessionStatus, provider: ProviderKind) -> Div {
    div()
        .w(px(18.0))
        .h(px(18.0))
        .flex_none()
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .bg(provider_color(provider).opacity(0.10))
        .text_color(if status == SessionStatus::Idle {
            provider_color(provider)
        } else {
            status_color(status)
        })
        .text_size(px(8.0))
        .child(provider.glyph())
}

fn status_label(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "Ready",
        SessionStatus::Connecting => "Connecting",
        SessionStatus::Working => "Working",
        SessionStatus::Waiting => "Waiting",
        SessionStatus::Failed => "Needs attention",
    }
}

fn render_message(message: &Message) -> Div {
    let theme = Theme::dark();
    match message.role {
        MessageRole::User => div().w_full().flex().justify_end().child(
            div()
                .max_w(px(600.0))
                .rounded_xl()
                .rounded_br_sm()
                .bg(theme.elevated)
                .px_4()
                .py_3()
                .text_size(px(13.0))
                .line_height(px(20.0))
                .text_color(theme.text)
                .whitespace_normal()
                .child(message.content.clone()),
        ),
        MessageRole::Assistant => div()
            .w_full()
            .flex()
            .gap_3()
            .items_start()
            .child(
                div()
                    .mt_1()
                    .w(px(24.0))
                    .h(px(24.0))
                    .flex_none()
                    .rounded_lg()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.accent_soft)
                    .text_color(theme.accent)
                    .text_size(px(10.0))
                    .child("✦"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(13.0))
                    .line_height(px(21.0))
                    .text_color(theme.text)
                    .child(render_rich_text(&message.content))
                    .when(message.streaming, |element| {
                        element.child(
                            div()
                                .mt_2()
                                .w(px(18.0))
                                .h(px(3.0))
                                .rounded_full()
                                .bg(theme.accent),
                        )
                    }),
            ),
        MessageRole::System => div()
            .rounded_lg()
            .bg(theme.panel)
            .border_1()
            .border_color(theme.border)
            .px_3()
            .py_2()
            .text_size(px(11.0))
            .text_color(theme.text_muted)
            .child(message.content.clone()),
    }
}

fn render_rich_text(content: &str) -> Div {
    let theme = Theme::dark();
    let mut root = div().flex().flex_col().gap_2();
    let mut in_code = false;
    for (index, segment) in content.split("```").enumerate() {
        if segment.is_empty() {
            in_code = !in_code;
            continue;
        }
        if in_code {
            let code = segment
                .split_once('\n')
                .map(|(_, code)| code)
                .unwrap_or(segment)
                .trim_end();
            root = root.child(
                div()
                    .id(SharedString::from(format!("code-{index}")))
                    .w_full()
                    .rounded_lg()
                    .bg(theme.code)
                    .border_1()
                    .border_color(theme.border)
                    .p_3()
                    .font_family("SF Mono")
                    .text_size(px(11.0))
                    .line_height(px(18.0))
                    .text_color(rgb(0xd7d8d5))
                    .whitespace_normal()
                    .child(code.to_owned()),
            );
        } else {
            root = root.child(div().whitespace_normal().child(segment.trim().to_owned()));
        }
        in_code = !in_code;
    }
    root
}

fn render_activity(activity: &ActivityItem) -> impl IntoElement {
    let theme = Theme::dark();
    let (glyph, color) = match activity.kind {
        ActivityKind::Reasoning => ("⌁", theme.text_muted),
        ActivityKind::Command => ("›_", theme.warning),
        ActivityKind::FileChange => ("±", theme.success),
        ActivityKind::Search => ("⌕", rgb(0x7dd3fc).into()),
        ActivityKind::Plan => ("☷", rgb(0xc4b5fd).into()),
        ActivityKind::Tool => ("◇", theme.text_muted),
    };
    div()
        .id(SharedString::from(format!("activity-{}", activity.id)))
        .min_h(px(34.0))
        .px_2()
        .py_1()
        .rounded_md()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(20.0))
                .text_size(px(10.0))
                .text_color(color)
                .child(glyph),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_muted)
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(activity.title.clone()),
                )
                .when_some(activity.detail.clone(), |element, detail| {
                    element.child(
                        div()
                            .text_size(px(9.5))
                            .text_color(theme.text_faint)
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(detail),
                    )
                }),
        )
        .child(
            div()
                .text_size(px(9.0))
                .text_color(if activity.complete {
                    theme.success
                } else {
                    theme.text_faint
                })
                .child(if activity.complete { "Done" } else { "Running" }),
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
