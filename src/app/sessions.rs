use super::*;

impl Waku {
    pub(super) fn select_project(&mut self, project_id: Uuid, cx: &mut Context<Self>) {
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

    pub(super) fn select_session(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        if !self
            .state
            .sessions
            .iter()
            .any(|session| session.id == session_id)
        {
            return;
        }
        self.session_navigation
            .visit(self.state.selected_session, session_id);
        self.activate_session(session_id, cx);
    }

    fn activate_session(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        self.state.selected_session = Some(session_id);
        if let Some((project_id, provider, model, reasoning_effort, service_tier)) =
            self.selected_session().map(|session| {
                (
                    session.project_id,
                    session.provider,
                    session.model.clone(),
                    session.reasoning_effort.clone(),
                    session.service_tier.clone(),
                )
            })
        {
            self.state.selected_project = Some(project_id);
            self.state.last_provider = provider;
            self.state.last_model = model;
            self.state.last_reasoning_effort = reasoning_effort;
            self.state.last_service_tier = service_tier;
        }
        self.ensure_right_panel_terminals(cx);
        let message_ids = self
            .selected_session()
            .map(|session| {
                session
                    .messages
                    .iter()
                    .map(|message| message.id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for message_id in message_ids {
            if let Some(text_state) = self.message_text_states.get(&message_id) {
                text_state.update(cx, |state, _| state.reset_block_viewport_layout());
            }
        }
        self.reset_visible_state();
        self.branch = self
            .selected_project()
            .and_then(|project| git_branch(&project.path));
        self.reset_transcript_rows_with_placeholders(self.transcript_row_count());
        self.save();
        cx.notify();
    }

    pub(super) fn create_session_for(
        &mut self,
        project_id: Uuid,
        provider: ProviderKind,
        cx: &mut Context<Self>,
    ) {
        let session = self.state.new_session(project_id, provider);
        let id = session.id;
        self.state.sessions.push(session);
        self.select_session(id, cx);
    }

    pub(super) fn remove_session(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        let Some(index) = self
            .state
            .sessions
            .iter()
            .position(|session| session.id == session_id)
        else {
            return;
        };
        let project_id = self.state.sessions[index].project_id;
        let last_turn_count = self.state.sessions[index].turns.len();
        let project_path = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| project.path.clone());
        let was_selected = self.state.selected_session == Some(session_id);
        self.reset_session_runtime(session_id);
        self.state.sessions.remove(index);
        self.session_navigation.remove(session_id);
        if let Some(project_path) = project_path {
            let _ = checkpoint::delete_session_refs(&project_path, session_id, last_turn_count);
        }

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

    pub(super) fn new_session_action(
        &mut self,
        _: &NewSession,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_page = None;
        if let Some(project_id) = self.state.selected_project {
            self.create_session_for(project_id, self.state.last_provider, cx);
        }
    }

    pub(super) fn open_settings_action(
        &mut self,
        _: &OpenSettings,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_page = Some(SettingsPage::Appearance);
        cx.notify();
    }

    pub(super) fn toggle_sidebar_action(
        &mut self,
        _: &ToggleSidebar,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_visible = !self.sidebar_visible;
        cx.notify();
    }

    pub(super) fn toggle_right_panel_action(
        &mut self,
        _: &ToggleRightPanel,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.right_panel_visible = !self.right_panel_visible;
        cx.notify();
    }

    pub(super) fn navigate_back_action(
        &mut self,
        _: &NavigateBack,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_page.take().is_some() {
            let focus_handle = self.composer_focus(cx);
            window.focus(&focus_handle, cx);
            cx.notify();
            return;
        }

        let Some(current) = self.state.selected_session else {
            return;
        };
        if let Some(target) = self.session_navigation.go_back(current) {
            self.settings_page = None;
            self.activate_session(target, cx);
        }
    }

    pub(super) fn navigate_forward_action(
        &mut self,
        _: &NavigateForward,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_page.is_some() {
            return;
        }

        let Some(current) = self.state.selected_session else {
            return;
        };
        if let Some(target) = self.session_navigation.go_forward(current) {
            self.settings_page = None;
            self.activate_session(target, cx);
        }
    }

    pub(super) fn navigation_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.button {
            MouseButton::Navigate(NavigationDirection::Back) => {
                cx.stop_propagation();
                self.navigate_back_action(&NavigateBack, window, cx);
            }
            MouseButton::Navigate(NavigationDirection::Forward) => {
                cx.stop_propagation();
                self.navigate_forward_action(&NavigateForward, window, cx);
            }
            _ => {}
        }
    }

    pub(super) fn focus_composer_action(
        &mut self,
        _: &FocusComposer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus_handle = self.composer_focus(cx);
        window.focus(&focus_handle, cx);
    }

    pub(super) fn cancel_turn_action(
        &mut self,
        _: &CancelTurn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_page.take().is_some() {
            cx.notify();
            return;
        }
        if self.message_edit.is_some() {
            self.cancel_message_edit(window, cx);
            return;
        }
        self.cancel_turn(cx);
    }

    pub(super) fn reset_visible_state(&mut self) {
        self.reasoning_expanded.clear();
        self.activities_expanded.clear();
        self.expanded_activity_items.clear();
        self.expanded_turns.clear();
        self.message_edit = None;
        self.toast = None;
        self.transcript_anchor.set(None);
        self.transcript_anchor_end_space.set(Pixels::ZERO);
        self.transcript_anchor_following.set(false);
        self.transcript_exact_measurement_rows.borrow_mut().clear();
    }

    pub(super) fn reset_session_runtime(&mut self, session_id: Uuid) {
        if let Some(runtime) = self.runtimes.remove(&session_id) {
            runtime.driver.cancel();
        }
    }

    pub(super) fn choose_model(
        &mut self,
        provider: ProviderKind,
        model: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.selected_session_mut()
            && session.can_choose_model(provider)
            && (session.provider != provider || session.model.as_deref() != Some(model.as_str()))
        {
            let session_id = session.id;
            session.provider = provider;
            session.model = Some(model.clone());
            session.reasoning_effort = None;
            session.service_tier = None;
            self.state.last_provider = provider;
            self.state.last_model = Some(model);
            self.state.last_reasoning_effort = None;
            self.state.last_service_tier = None;
            self.model_picker_tab = ModelPickerTab::Provider(provider);
            self.reset_session_runtime(session_id);
            self.save();
            cx.notify();
        }
    }

    pub(super) fn select_model_picker_tab(&mut self, tab: ModelPickerTab, cx: &mut Context<Self>) {
        match tab {
            ModelPickerTab::Provider(provider) => {
                self.request_provider_model_discovery(provider);
            }
            ModelPickerTab::Favorites => {
                let providers = self
                    .state
                    .favorite_models
                    .iter()
                    .map(|favorite| favorite.provider)
                    .collect::<HashSet<_>>();
                for provider in providers {
                    self.request_provider_model_discovery(provider);
                }
            }
        }
        if self.model_picker_tab != tab {
            self.model_picker_tab = tab;
            cx.notify();
        }
    }

    pub(super) fn toggle_favorite_model(
        &mut self,
        provider: ProviderKind,
        model: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self
            .state
            .favorite_models
            .iter()
            .position(|favorite| favorite.provider == provider && favorite.model == model)
        {
            self.state.favorite_models.remove(index);
        } else {
            self.state
                .favorite_models
                .push(FavoriteModel { provider, model });
        }
        self.save();
        cx.notify();
    }

    pub(super) fn set_runtime_mode(&mut self, mode: RuntimeMode, cx: &mut Context<Self>) {
        if mode == RuntimeMode::Plan {
            return;
        }
        if let Some(session) = self.selected_session_mut()
            && session.runtime_mode != mode
        {
            let session_id = session.id;
            session.runtime_mode = mode;
            self.reset_session_runtime(session_id);
            self.save();
            cx.notify();
        }
    }

    pub(super) fn set_interaction_mode(&mut self, mode: InteractionMode, cx: &mut Context<Self>) {
        if let Some(session) = self.selected_session_mut()
            && session.interaction_mode != mode
        {
            let session_id = session.id;
            session.interaction_mode = mode;
            self.reset_session_runtime(session_id);
            self.save();
            cx.notify();
        }
    }

    pub(super) fn set_reasoning_effort(&mut self, effort: String, cx: &mut Context<Self>) {
        if let Some(session) = self.selected_session_mut()
            && session.reasoning_effort.as_deref() != Some(effort.as_str())
        {
            let session_id = session.id;
            session.reasoning_effort = Some(effort.clone());
            self.state.last_reasoning_effort = Some(effort);
            self.reset_session_runtime(session_id);
            self.save();
            cx.notify();
        }
    }

    pub(super) fn set_service_tier(&mut self, tier: String, cx: &mut Context<Self>) {
        if let Some(session) = self.selected_session_mut()
            && session.service_tier.as_deref() != Some(tier.as_str())
        {
            let session_id = session.id;
            session.service_tier = Some(tier.clone());
            self.state.last_service_tier = Some(tier);
            self.reset_session_runtime(session_id);
            self.save();
            cx.notify();
        }
    }

    pub(super) fn cancel_turn(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.state.selected_session else {
            return;
        };
        let mut runtime = self.runtimes.remove(&session_id);
        if let Some(runtime) = runtime.as_ref() {
            runtime.driver.cancel();
        }
        // Do not leave already-received text in the smoothing queue: once the
        // message is marked complete, a later delta would otherwise create a
        // second assistant bubble. Show the received portion immediately.
        let mut keep_runtime = true;
        if let Some(runtime) = runtime.as_mut() {
            Self::collect_runtime_events(runtime);
            while let Some(event) = runtime.pending_events.pop_front() {
                keep_runtime &= self.handle_driver_event(session_id, runtime, event);
                if !keep_runtime {
                    break;
                }
            }
        }
        let has_active_turn = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(AgentSession::active_turn_id)
            .is_some();
        self.finish_streaming_assistant(session_id);
        self.complete_turn_blocks(session_id);
        if let Some(runtime) = runtime.as_mut() {
            runtime.stream_phase = None;
            runtime.pending_permission = None;
        }
        if has_active_turn {
            let needs_fallback = !self.turn_has_assistant_message(session_id);
            if let Some(session) = self
                .state
                .sessions
                .iter_mut()
                .find(|session| session.id == session_id)
            {
                session.status = SessionStatus::Idle;
                if needs_fallback {
                    session.push_message(MessageRole::Assistant, "Stopped.");
                }
                session.finish_active_turn(TurnStatus::Interrupted);
            }
        }
        if has_active_turn {
            self.capture_latest_turn_checkpoint_for(session_id);
        }
        if keep_runtime && let Some(runtime) = runtime {
            self.runtimes.insert(session_id, runtime);
        }
        self.remeasure_transcript_tail();
        self.save();
        cx.notify();
    }

    pub(super) fn respond_permission(
        &mut self,
        request_id: String,
        option_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some(session_id) = self.state.selected_session else {
            return;
        };
        if let Some(runtime) = self.runtimes.get_mut(&session_id) {
            runtime.driver.respond(request_id, option_id);
            runtime.pending_permission = None;
        }
        if let Some(session) = self.selected_session_mut() {
            session.status = SessionStatus::Working;
        }
        cx.notify();
    }

    pub(super) fn add_project(&mut self, cx: &mut Context<Self>) {
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
}
