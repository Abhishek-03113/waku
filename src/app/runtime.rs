use super::*;

impl Waku {
    pub fn composer_focus(&self, cx: &App) -> FocusHandle {
        self.composer.read(cx).focus()
    }

    pub(super) fn selected_project(&self) -> Option<&Project> {
        let id = self.state.selected_project?;
        self.state.projects.iter().find(|project| project.id == id)
    }

    pub(super) fn selected_session(&self) -> Option<&AgentSession> {
        let id = self.state.selected_session?;
        self.state.sessions.iter().find(|session| session.id == id)
    }

    pub(super) fn selected_session_mut(&mut self) -> Option<&mut AgentSession> {
        let id = self.state.selected_session?;
        self.state
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
    }

    pub(super) fn selected_runtime(&self) -> Option<&SessionRuntime> {
        self.runtimes.get(&self.state.selected_session?)
    }

    pub(super) fn provider_probe(&self, provider: ProviderKind) -> Option<&ProviderProbe> {
        self.probes.iter().find(|probe| probe.provider == provider)
    }

    pub(super) fn model_for_session<'a>(&'a self, session: &'a AgentSession) -> Option<&'a str> {
        session.model.as_deref().or_else(|| {
            self.provider_probe(session.provider)
                .and_then(ProviderProbe::preferred_model)
                .map(|model| model.id.as_str())
        })
    }

    pub(super) fn model_display_name(&self, provider: ProviderKind, model: Option<&str>) -> String {
        let Some(model) = model else {
            return provider.short_name().to_owned();
        };
        self.provider_probe(provider)
            .and_then(|probe| probe.models.iter().find(|candidate| candidate.id == model))
            .map(|candidate| candidate.name.clone())
            .unwrap_or_else(|| model.to_owned())
    }

    pub(super) fn model_metadata_for_session(
        &self,
        session: &AgentSession,
    ) -> Option<&ProviderModel> {
        let model = self.model_for_session(session)?;
        self.provider_probe(session.provider)?
            .models
            .iter()
            .find(|candidate| candidate.id == model)
    }

    pub(super) fn selected_transcript_blocks(&self) -> &[TranscriptBlock] {
        self.selected_session()
            .map(|session| session.transcript_blocks.as_slice())
            .unwrap_or(&[])
    }

    pub(super) fn save(&mut self) {
        self.last_stream_save = Instant::now();
        if let Err(error) = self.store.save(&self.state) {
            self.toast = Some(format!("Could not save local state: {error}"));
        } else {
            self.stream_state_dirty = false;
        }
    }

    pub(super) fn capture_latest_turn_checkpoint(&mut self) {
        if let Some(session_id) = self.state.selected_session {
            self.capture_latest_turn_checkpoint_for(session_id);
        }
    }

    pub(super) fn capture_latest_turn_checkpoint_for(&mut self, session_id: Uuid) {
        let Some((project_id, turn_count)) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| {
                session
                    .turns
                    .last()
                    .filter(|turn| turn.status != TurnStatus::Running)
                    .map(|turn| (session.project_id, turn.turn_count))
            })
        else {
            return;
        };
        let Some(project_path) = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| project.path.clone())
        else {
            return;
        };

        let checkpoint = match checkpoint::capture_turn(&project_path, session_id, turn_count) {
            Ok(checkpoint) => checkpoint,
            Err(error) => {
                self.toast = Some(format!("Could not capture the turn checkpoint: {error}"));
                Checkpoint {
                    turn_count,
                    git_ref: checkpoint::checkpoint_ref(session_id, turn_count),
                    status: CheckpointStatus::Error,
                    files: Vec::new(),
                    created_at: unix_time(),
                }
            }
        };
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            && let Some(turn) = session
                .turns
                .iter_mut()
                .find(|turn| turn.turn_count == turn_count)
        {
            turn.checkpoint = Some(checkpoint);
        }
    }

    pub(super) fn request_checkpoint_revert(
        &mut self,
        session_id: Uuid,
        turn_count: usize,
        cx: &mut Context<Self>,
    ) {
        if self.pending_revert == Some((session_id, turn_count)) {
            self.pending_revert = None;
            self.revert_to_checkpoint(session_id, turn_count, cx);
            return;
        }

        let discarded_turns = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(|session| session.turns.len().saturating_sub(turn_count))
            .unwrap_or_default();
        self.pending_revert = Some((session_id, turn_count));
        self.toast = Some(if discarded_turns == 0 {
            "Click “Confirm revert” to restore the workspace to this checkpoint.".into()
        } else {
            format!(
                "Click “Confirm revert” to restore the workspace and discard {discarded_turns} later turn(s)."
            )
        });
        cx.notify();
    }

    pub(super) fn revert_to_checkpoint(
        &mut self,
        session_id: Uuid,
        turn_count: usize,
        cx: &mut Context<Self>,
    ) {
        let Some((
            project_id,
            provider,
            status,
            provider_cursor,
            previous_turn_count,
            rollback_turns,
            checkpoint,
        )) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| {
                session
                    .turns
                    .iter()
                    .find(|turn| turn.turn_count == turn_count)
                    .and_then(|turn| turn.checkpoint.clone())
                    .map(|checkpoint| {
                        (
                            session.project_id,
                            session.provider,
                            session.status,
                            session.provider_cursor.clone(),
                            session.turns.len(),
                            session.provider_turns_after(turn_count),
                            checkpoint,
                        )
                    })
            })
        else {
            self.toast = Some("That checkpoint is no longer available.".into());
            cx.notify();
            return;
        };
        if self.state.selected_session != Some(session_id) {
            self.toast = Some("Select the task before reverting its checkpoint.".into());
            cx.notify();
            return;
        }
        if status != SessionStatus::Idle {
            self.toast = Some("Stop the current turn before reverting a checkpoint.".into());
            cx.notify();
            return;
        }
        if checkpoint.status != CheckpointStatus::Ready {
            self.toast = Some("This turn does not have a restorable Git checkpoint.".into());
            cx.notify();
            return;
        }
        if !provider.supports_conversation_rollback() || provider_cursor.is_none() {
            self.toast = Some(format!(
                "{} cannot safely roll back its native conversation yet.",
                provider.display_name()
            ));
            cx.notify();
            return;
        }
        let Some(project_path) = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| project.path.clone())
        else {
            self.toast = Some("The task's project could not be found.".into());
            cx.notify();
            return;
        };
        if !checkpoint::has_ref(&project_path, &checkpoint.git_ref) {
            self.toast = Some("The checkpoint's hidden Git ref is missing.".into());
            cx.notify();
            return;
        }

        let safety_ref = format!("refs/waku/revert-backup-{session_id}-{}", Uuid::new_v4());
        if let Err(error) = checkpoint::capture_ref(&project_path, &safety_ref) {
            self.toast = Some(format!(
                "Could not create a revert safety snapshot: {error}"
            ));
            cx.notify();
            return;
        }
        if let Err(error) = checkpoint::restore_ref(&project_path, &checkpoint.git_ref) {
            self.toast = Some(match checkpoint::restore_ref(&project_path, &safety_ref) {
                Ok(()) => {
                    let _ = checkpoint::delete_ref(&project_path, &safety_ref);
                    format!("Could not restore the checkpoint: {error}")
                }
                Err(restore_error) => format!(
                    "Checkpoint restore failed ({error}); safety restore also failed ({restore_error}). Recovery ref retained at {safety_ref}."
                ),
            });
            cx.notify();
            return;
        }

        if rollback_turns > 0 {
            let rollback_result = self
                .ensure_driver()
                .and_then(|driver| driver.rollback(rollback_turns));
            if let Err(error) = rollback_result {
                let restore_result = checkpoint::restore_ref(&project_path, &safety_ref);
                self.toast = Some(match restore_result {
                    Ok(()) => {
                        let _ = checkpoint::delete_ref(&project_path, &safety_ref);
                        format!(
                            "The provider rejected the rollback, so the workspace was restored: {error}"
                        )
                    }
                    Err(restore_error) => format!(
                        "Provider rollback failed ({error}) and the safety snapshot could not be restored ({restore_error}). Recovery ref retained at {safety_ref}."
                    ),
                });
                cx.notify();
                return;
            }
        }

        let _ = checkpoint::delete_ref(&project_path, &safety_ref);
        let cleanup_result = checkpoint::delete_turn_refs_after(
            &project_path,
            session_id,
            turn_count,
            previous_turn_count,
        );
        if let Some(session) = self
            .state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.truncate_after_turn(turn_count);
            session.status = SessionStatus::Idle;
        }
        if let Some(runtime) = self.runtimes.get_mut(&session_id) {
            runtime.pending_events.clear();
            runtime.stream_remeasure_pending = false;
            runtime.stream_phase = None;
            runtime.pending_permission = None;
        }
        self.reasoning_expanded.clear();
        self.activities_expanded.clear();
        self.expanded_activity_items.clear();
        self.expanded_turns.clear();
        self.reset_transcript_rows(self.transcript_row_count());
        self.toast = Some(match cleanup_result {
            Ok(()) => format!("Restored checkpoint after turn {turn_count}."),
            Err(error) => {
                format!("Restored checkpoint after turn {turn_count}; stale refs remain: {error}")
            }
        });
        self.save();
        cx.notify();
    }

    pub(super) fn ensure_driver(&mut self) -> anyhow::Result<DriverHandle> {
        let session = self
            .selected_session()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("No session selected"))?;
        if let Some(runtime) = self.runtimes.get(&session.id) {
            return Ok(runtime.driver.clone());
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
        let model = session.model.clone().or_else(|| {
            self.probes
                .iter()
                .find(|probe| probe.provider == session.provider)
                .and_then(ProviderProbe::preferred_model)
                .map(|model| model.id.clone())
        });
        let model_metadata = self.model_metadata_for_session(&session);
        let reasoning_effort = session.reasoning_effort.clone().filter(|effort| {
            model_metadata.is_some_and(|model| {
                model
                    .reasoning_efforts
                    .iter()
                    .any(|option| option.id == *effort)
            })
        });
        let service_tier = session.service_tier.clone().filter(|tier| {
            tier == "default"
                || model_metadata.is_some_and(|model| {
                    model.service_tiers.iter().any(|option| option.id == *tier)
                })
        });
        let (event_tx, event_rx) = unbounded();
        let handle = driver::start(
            session.provider,
            DriverStartOptions {
                binary,
                cwd: project.path.clone(),
                mode: session.runtime_mode,
                interaction_mode: session.interaction_mode,
                model,
                reasoning_effort,
                service_tier,
                provider_cursor: session.provider_cursor.clone(),
            },
            event_tx,
        )?;
        self.runtimes.insert(
            session.id,
            SessionRuntime {
                driver: handle.clone(),
                events: event_rx,
                pending_events: VecDeque::new(),
                stream_phase: None,
                stream_remeasure_pending: false,
                pending_permission: None,
            },
        );
        Ok(handle)
    }

    pub(super) fn submit_prompt(&mut self, prompt: String, cx: &mut Context<Self>) {
        let Some((session_id, project_id, status, next_turn_count)) =
            self.selected_session().map(|session| {
                (
                    session.id,
                    session.project_id,
                    session.status,
                    session.turns.len() + 1,
                )
            })
        else {
            return;
        };
        if matches!(
            status,
            SessionStatus::Working | SessionStatus::Connecting | SessionStatus::Waiting
        ) {
            self.toast = Some("The agent is already working. Stop it before sending again.".into());
            cx.notify();
            return;
        }
        let project_path = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| project.path.clone());
        let checkpoint_warning = project_path.as_deref().and_then(|path| {
            let baseline_count = next_turn_count - 1;
            let git_ref = checkpoint::checkpoint_ref(session_id, baseline_count);
            (!checkpoint::has_ref(path, &git_ref))
                .then(|| checkpoint::capture_turn(path, session_id, baseline_count).err())
                .flatten()
                .map(|error| format!("Could not capture the pre-turn checkpoint: {error}"))
        });
        let transcript_anchor = if let Some(session) = self.selected_session_mut() {
            session.set_title_from_prompt(&prompt);
            let turn_id = session.begin_turn(&prompt);
            session.status = SessionStatus::Connecting;
            session.updated_at = unix_time();
            Some(TranscriptAnchor {
                session_id,
                turn_id,
            })
        } else {
            None
        };
        if let Some(runtime) = self.runtimes.get_mut(&session_id) {
            runtime.pending_events.clear();
            runtime.stream_remeasure_pending = false;
            runtime.stream_phase = None;
            runtime.pending_permission = None;
        }
        self.reasoning_expanded.clear();
        self.activities_expanded.clear();
        self.expanded_activity_items.clear();
        self.expanded_turns.clear();
        self.pending_revert = None;
        self.toast = checkpoint_warning;
        self.transcript_anchor.set(transcript_anchor);
        self.transcript_anchor_end_space.set(Pixels::ZERO);
        self.transcript_anchor_following.set(true);
        self.reset_transcript_rows(self.transcript_row_count());
        self.scroll_transcript_to_anchor();
        let mut failed_to_start = false;
        match self.ensure_driver() {
            Ok(driver) => driver.prompt(prompt),
            Err(error) => {
                failed_to_start = true;
                let message = format!("Could not start the agent: {error}");
                if let Some(session) = self.selected_session_mut() {
                    session.status = SessionStatus::Failed;
                    session.push_message(MessageRole::Assistant, message);
                    session.finish_active_turn(TurnStatus::Failed);
                }
            }
        }
        if failed_to_start {
            self.capture_latest_turn_checkpoint();
        }
        self.save();
        cx.notify();
    }

    pub(super) fn collect_runtime_events(runtime: &mut SessionRuntime) {
        while let Ok(event) = runtime.events.try_recv() {
            runtime.pending_events.push_back(event);
        }
    }

    pub(super) fn drain_provider_probe_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok(probe) = self.provider_probe_events.try_recv() {
            if let Some(existing) = self
                .probes
                .iter_mut()
                .find(|existing| existing.provider == probe.provider)
            {
                *existing = probe;
            } else {
                self.probes.push(probe);
            }
            changed = true;
        }
        changed
    }

    pub(super) fn drain_driver_events(&mut self) -> bool {
        let session_ids = self.runtimes.keys().copied().collect::<Vec<_>>();
        let mut changed = false;
        let mut force_save = false;
        let mut selected_changed = false;
        for session_id in session_ids {
            let Some(mut runtime) = self.runtimes.remove(&session_id) else {
                continue;
            };
            let follow_up_remeasure = std::mem::take(&mut runtime.stream_remeasure_pending);
            Self::collect_runtime_events(&mut runtime);
            let mut runtime_changed = false;
            let mut markdown_changed = false;
            let mut revealed_stream_chunk = false;
            let mut keep_runtime = true;
            while let Some(event) = runtime.pending_events.front() {
                let kind = stream_delta_kind(event);
                if kind.is_some() && revealed_stream_chunk {
                    break;
                }

                let event = if let Some(kind) = kind {
                    revealed_stream_chunk = true;
                    pop_stream_chunk(&mut runtime.pending_events, kind)
                } else {
                    runtime.pending_events.pop_front()
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
                runtime_changed = true;
                keep_runtime &= self.handle_driver_event(session_id, &mut runtime, event);
                if !keep_runtime {
                    break;
                }
            }
            runtime.stream_remeasure_pending = markdown_changed;
            if keep_runtime {
                self.runtimes.insert(session_id, runtime);
            }
            changed |= runtime_changed;
            if self.state.selected_session == Some(session_id)
                && (runtime_changed || follow_up_remeasure)
            {
                selected_changed = true;
            }
        }

        if changed {
            self.stream_state_dirty = true;
        }
        if selected_changed {
            self.remeasure_transcript_tail();
        }
        if self.stream_state_dirty
            && (force_save || self.last_stream_save.elapsed() >= STREAM_SAVE_INTERVAL)
        {
            self.save();
        }
        changed || selected_changed
    }
}
