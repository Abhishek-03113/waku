use super::*;

fn retain_runtime_after_cancel(provider: ProviderKind) -> bool {
    // Codex's app-server owns the Computer Use process tree, and Amp offers no
    // interrupt on its stream — stopping it means ending the process. Both
    // resume their native thread on the next prompt.
    !matches!(provider, ProviderKind::Codex | ProviderKind::Amp)
}

impl Waku {
    pub(super) fn select_project(&mut self, project_id: Uuid, cx: &mut Context<Self>) {
        self.state.selected_project = Some(project_id);
        self.create_session_for(project_id, self.state.last_provider, cx);
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

    /// Loads a session's transcript if startup only fetched its list columns.
    ///
    /// One row plus that session's messages, so it stays well inside a frame;
    /// the alternative is paying for all of history at launch.
    pub(super) fn ensure_session_loaded(&mut self, session_id: Uuid) {
        let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .filter(|session| !session.detail_loaded)
        else {
            return;
        };
        if let Err(error) = self.store.hydrate(session) {
            self.toast = Some(format!("Could not open that session: {error}"));
        }
    }

    fn activate_session(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        self.ensure_session_loaded(session_id);
        let session_changed = self.state.selected_session != Some(session_id);
        if session_changed {
            self.store_selected_right_panel_state();
        }
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
        if session_changed {
            self.restore_right_panel_state(session_id, cx);
        } else {
            self.ensure_right_panel_terminals(cx);
        }
        self.reset_visible_state();
        self.refresh_branch(cx);
        self.refresh_composer_sources(cx);
        self.reset_transcript_rows(self.transcript_row_count());
        self.save();
        cx.notify();
    }

    /// Drops cached answers about the workspace on disk.
    ///
    /// These queries cache to keep `git` and directory walks out of frames, but
    /// nothing tells us when the working tree changes underneath. Rather than
    /// expire on a timer, they are dropped at the moments the answer plausibly
    /// moved — coming back to the window, or a turn finishing.
    pub(super) fn invalidate_workspace_queries(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.selected_project().map(|project| project.path.clone()) else {
            return;
        };
        self.branches.invalidate(&path);
        self.refresh_branch(cx);
        self.refresh_workspace_surfaces(cx);
        self.invalidate_composer_sources(cx);
    }

    /// Resolves the checked-out branch for the selected project.
    ///
    /// `git` costs upwards of 10ms, more than a frame at 120Hz, so it is a
    /// query: a cache hit is free, a miss draws the previous value and fills in
    /// when the lookup lands.
    pub(super) fn refresh_branch(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.selected_project().map(|project| project.path.clone()) else {
            self.branch = None;
            return;
        };
        match self.branches.read(&path) {
            Query::Ready(branch) => self.branch = (*branch).clone(),
            // Leave the previous value on screen rather than flashing empty.
            Query::Pending => {}
            Query::Missing(token) => {
                cx.spawn(async move |waku, cx| {
                    let branch = cx
                        .background_executor()
                        .spawn({
                            let path = path.clone();
                            async move { git_branch(&path) }
                        })
                        .await;
                    waku.update(cx, |waku, cx| {
                        if waku.branches.fulfill(token, branch.clone())
                            && waku.selected_project().is_some_and(|p| p.path == path)
                        {
                            waku.branch = branch;
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    pub(super) fn create_session_for(
        &mut self,
        project_id: Uuid,
        provider: ProviderKind,
        cx: &mut Context<Self>,
    ) {
        if let Some(draft_id) = self
            .state
            .sessions
            .iter()
            .find(|session| session.project_id == project_id && !session.has_started())
            .map(|session| session.id)
        {
            self.select_session(draft_id, cx);
            return;
        }
        let session = self.state.new_session(project_id, provider);
        let id = session.id;
        self.state.push_session(session);
        self.select_session(id, cx);
    }

    pub(super) fn remove_session(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        // The turn count drives checkpoint cleanup, so the transcript has to be
        // loaded before it can be trusted.
        self.ensure_session_loaded(session_id);
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
        self.remove_right_panel_session_state(session_id);
        self.state.sessions.remove(index);
        self.session_navigation.remove(session_id);
        if let Some(project_path) = project_path {
            let _ = checkpoint::delete_session_refs(&project_path, session_id, last_turn_count);
        }
        self.invalidate_checkpoint_refs();

        if was_selected {
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
        } else {
            self.save();
            cx.notify();
        }

        // Only now is the row gone, so the sweep can see which blobs are
        // genuinely unreferenced. It reads the database and walks the blob
        // directory, so it stays off the UI thread.
        let sweep = self.store.blob_sweep();
        cx.background_executor().spawn(async move { sweep() }).detach();
    }

    pub(super) fn new_session_action(
        &mut self,
        _: &NewSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_page = None;
        if let Some(project_id) = self.state.selected_project {
            self.create_session_for(project_id, self.state.last_provider, cx);
        }
        let focus_handle = self.composer_focus(cx);
        window.focus(&focus_handle, cx);
    }

    pub(super) fn open_settings_action(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_page = Some(SettingsPage::Appearance);
        window.focus(&self.settings_focus, cx);
        cx.notify();
    }

    pub(super) fn toggle_sidebar_action(
        &mut self,
        _: &ToggleSidebar,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_sidebar_visible(!self.sidebar_visible, cx);
    }

    pub(super) fn toggle_right_panel_action(
        &mut self,
        _: &ToggleRightPanel,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_right_panel_visible(!self.right_panel_visible, cx);
    }

    pub(super) fn toggle_fps_counter_action(
        &mut self,
        _: &ToggleFpsCounter,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fps_counter_visible = !self.fps_counter_visible;
        cx.notify();
    }

    pub(super) fn set_sidebar_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.sidebar_visible == visible {
            return;
        }
        self.sidebar_visible = visible;
        self.persist_panel_layout();
        cx.notify();
    }

    pub(super) fn set_right_panel_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if visible {
            self.request_active_terminal_focus();
        } else {
            self.right_panel_pending_terminal_focus = None;
        }
        if self.right_panel_visible == visible {
            return;
        }
        self.right_panel_visible = visible;
        self.persist_panel_layout();
        cx.notify();
    }

    pub(super) fn persist_panel_layout(&mut self) {
        self.state.sidebar_visible = self.sidebar_visible;
        self.state.right_panel_visible = self.right_panel_visible;
        self.state.sidebar_width = self.sidebar_width;
        self.state.right_panel_width = self.right_panel_width;
        self.save();
    }

    pub(super) fn effective_panel_widths(&self, window: &Window) -> (f32, f32) {
        fitted_panel_widths(
            f32::from(window.viewport_size().width),
            self.sidebar_visible,
            self.right_panel_visible,
            self.sidebar_width,
            self.right_panel_width,
        )
    }

    pub(super) fn begin_panel_resize(
        &mut self,
        target: PanelResizeTarget,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (sidebar_width, right_panel_width) = self.effective_panel_widths(window);
        let start_width = match target {
            PanelResizeTarget::Sidebar => {
                self.sidebar_width = sidebar_width;
                crate::platform::set_sidebar_material_width(window, sidebar_width);
                sidebar_width
            }
            PanelResizeTarget::RightPanel => {
                self.right_panel_width = right_panel_width;
                right_panel_width
            }
            PanelResizeTarget::FileTree => {
                let width =
                    fitted_file_tree_width(right_panel_width, self.right_panel_file_tree_width);
                self.right_panel_file_tree_width = width;
                width
            }
        };
        self.panel_resize_drag = Some(PanelResizeDrag {
            target,
            start_mouse_x: f32::from(event.position.x),
            start_width,
        });
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn resize_panel_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.panel_resize_drag else {
            return;
        };
        let viewport_width = f32::from(window.viewport_size().width);
        let (sidebar_width, right_panel_width) = self.effective_panel_widths(window);
        let delta = f32::from(event.position.x) - drag.start_mouse_x;
        match drag.target {
            PanelResizeTarget::Sidebar => {
                let maximum = SIDEBAR_MAX_WIDTH
                    .min(viewport_width - MAIN_PANEL_MIN_WIDTH - right_panel_width)
                    .max(SIDEBAR_MIN_WIDTH);
                let width = (drag.start_width + delta).clamp(SIDEBAR_MIN_WIDTH, maximum);
                if (self.sidebar_width - width).abs() < 0.5 {
                    return;
                }
                self.sidebar_width = width;
                crate::platform::set_sidebar_material_width(window, width);
            }
            PanelResizeTarget::RightPanel => {
                let maximum = RIGHT_PANEL_MAX_WIDTH
                    .min(viewport_width - MAIN_PANEL_MIN_WIDTH - sidebar_width)
                    .max(RIGHT_PANEL_MIN_WIDTH);
                let width = (drag.start_width - delta).clamp(RIGHT_PANEL_MIN_WIDTH, maximum);
                if (self.right_panel_width - width).abs() < 0.5 {
                    return;
                }
                self.right_panel_width = width;
            }
            PanelResizeTarget::FileTree => {
                let maximum = FILE_TREE_MAX_WIDTH
                    .min(right_panel_width - FILE_EDITOR_MIN_WIDTH)
                    .max(FILE_TREE_MIN_WIDTH);
                let width = (drag.start_width - delta).clamp(FILE_TREE_MIN_WIDTH, maximum);
                if (self.right_panel_file_tree_width - width).abs() < 0.5 {
                    return;
                }
                self.right_panel_file_tree_width = width;
            }
        }
        cx.notify();
    }

    pub(super) fn finish_panel_resize(
        &mut self,
        event: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button == MouseButton::Left
            && let Some(drag) = self.panel_resize_drag.take()
        {
            if drag.target != PanelResizeTarget::FileTree {
                self.persist_panel_layout();
            }
            cx.notify();
        }
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
        self.settings_page = None;
        let focus_handle = self.composer_focus(cx);
        window.focus(&focus_handle, cx);
        cx.notify();
    }

    pub(super) fn cancel_turn_action(
        &mut self,
        _: &CancelTurn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_page.take().is_some() {
            let focus_handle = self.composer_focus(cx);
            window.focus(&focus_handle, cx);
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
        // Selection belongs to the session being left.
        self.transcript_selection.selection.borrow_mut().clear();
        self.transcript_selection.registry.borrow_mut().clear();
        // Parsed messages are keyed by message id, which is unique across
        // sessions, so they stay cached — switching back to a recent session
        // then costs no re-parse. Bounded so a long-running window cannot grow
        // without limit.
        let mut message_markdown = self.message_markdown.borrow_mut();
        let cached_bytes: usize = message_markdown
            .values()
            .map(md::render::MarkdownView::source_len)
            .sum();
        if cached_bytes > MAX_CACHED_MESSAGE_SOURCE_BYTES {
            message_markdown.clear();
        }
        drop(message_markdown);
        // Block parses are keyed by position within the session, so they would
        // be read as another session's blocks.
        self.block_markdown.borrow_mut().clear();
        self.menus.borrow_mut().clear();
        self.message_edit = None;
        self.toast = None;
        self.navigation_rail_active_scale_enabled.set(false);
        self.navigation_rail_reset_generation
            .set(self.navigation_rail_reset_generation.get().wrapping_add(1));
        self.transcript_anchor.set(None);
        self.transcript_anchor_end_space.set(Pixels::ZERO);
        self.transcript_anchor_following.set(false);
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
            let provider_changed = session.provider != provider;
            session.provider = provider;
            session.model = Some(model.clone());
            session.reasoning_effort = None;
            session.service_tier = None;
            self.state.last_provider = provider;
            self.state.last_model = Some(model);
            self.state.last_reasoning_effort = None;
            self.state.last_service_tier = None;
            self.model_picker_tab = ModelPickerTab::Provider(provider);
            // A different provider is a different binary and protocol; only a
            // model change within one provider can be applied in session.
            if provider_changed {
                self.reset_session_runtime(session_id);
                // A different provider is also a different command registry.
                self.refresh_composer_sources(cx);
            } else {
                self.apply_session_options(session_id);
            }
            self.save();
            cx.notify();
        }
    }

    /// Discovery is not requested here: launch already requested it for every
    /// installed provider, so tabs only ever switch between loaded lists.
    pub(super) fn select_model_picker_tab(&mut self, tab: ModelPickerTab, cx: &mut Context<Self>) {
        if self.model_picker_tab != tab {
            self.model_picker_tab = tab;
            // A different tab renumbers the rows under the keyboard cursor.
            self.model_picker_highlight = None;
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
            self.apply_session_options(session_id);
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
            self.apply_session_options(session_id);
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
            self.apply_session_options(session_id);
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
            self.apply_session_options(session_id);
            self.save();
            cx.notify();
        }
    }

    pub(super) fn cancel_turn(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.state.selected_session else {
            return;
        };
        let retain_runtime = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| retain_runtime_after_cancel(session.provider));
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
            runtime.pending_computer_approval = None;
            runtime.computer_use_previews.clear();
        }
        if has_active_turn {
            let needs_fallback = !self.turn_has_assistant_message(session_id);
            if let Some(session) = self.state.session_mut(session_id)
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
            self.start_pending_checkpoint_captures(cx);
        }
        // A provider runtime owns its Waku JavaScript REPL and Computer Use descendants.
        // Stopping the turn must close that whole process tree so capture,
        // status, and accessibility sessions do not outlive the turn. The
        // next prompt resumes the same provider thread with a fresh runtime.
        if retain_runtime
            && keep_runtime
            && let Some(runtime) = runtime
        {
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

    pub(super) fn respond_computer_permission(
        &mut self,
        decision: &'static str,
        cx: &mut Context<Self>,
    ) {
        let Some(session_id) = self.state.selected_session else {
            return;
        };
        let Some(mut runtime) = self.runtimes.remove(&session_id) else {
            return;
        };
        let Some(pending) = runtime.pending_computer_approval.take() else {
            self.runtimes.insert(session_id, runtime);
            return;
        };

        if decision == "deny" {
            runtime.driver.reject_computer_tool(
                pending.request,
                "The user denied control of this app.".into(),
            );
        } else {
            let key = pending.target.grant_key();
            runtime.computer_session_grants.insert(key);
            if decision == "always" && pending.target.persistable() {
                let grant = crate::computer_use::ComputerAppGrant {
                    bundle_id: pending.target.bundle_id.clone(),
                    app_name: pending.target.app_name.clone(),
                };
                if !self
                    .state
                    .computer_use_allowed_apps
                    .iter()
                    .any(|existing| existing.key() == grant.key())
                {
                    self.state.computer_use_allowed_apps.push(grant);
                    self.save();
                }
            }
            runtime.driver.run_computer_tool(pending.request);
        }
        if let Some(session) = self.state.session_mut(session_id)
        {
            session.status = SessionStatus::Working;
        }
        self.runtimes.insert(session_id, runtime);
        cx.notify();
    }

    pub(super) fn bring_computer_use_to_front(&mut self, window_id: u32, cx: &mut Context<Self>) {
        if let Some(runtime) = self
            .state
            .selected_session
            .and_then(|session_id| self.runtimes.get_mut(&session_id))
            && let Some(index) = runtime.computer_use_previews.iter().position(|preview| {
                preview
                    .target
                    .as_ref()
                    .is_some_and(|target| target.window_id == window_id)
            })
        {
            let preview = runtime.computer_use_previews.remove(index);
            runtime.computer_use_previews.push(preview);
        }
        cx.notify();
    }

    pub(super) fn dismiss_computer_use(&mut self, window_id: u32, cx: &mut Context<Self>) {
        if let Some(runtime) = self
            .state
            .selected_session
            .and_then(|session_id| self.runtimes.get_mut(&session_id))
        {
            runtime.computer_use_previews.retain(|preview| {
                preview
                    .target
                    .as_ref()
                    .is_none_or(|target| target.window_id != window_id)
            });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopping_releases_the_runtimes_that_cannot_be_interrupted_in_place() {
        // Codex owns a Computer Use process tree; Amp has no stream interrupt.
        assert!(!retain_runtime_after_cancel(ProviderKind::Codex));
        assert!(!retain_runtime_after_cancel(ProviderKind::Amp));
        for provider in ProviderKind::ALL {
            if !matches!(provider, ProviderKind::Codex | ProviderKind::Amp) {
                assert!(retain_runtime_after_cancel(provider));
            }
        }
    }
}
