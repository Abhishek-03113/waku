use super::*;

fn start_driver(mut request: DriverStartRequest, cwd: PathBuf) -> anyhow::Result<PreparedDriver> {
    request.options.cwd = cwd;
    let (event_tx, events) = unbounded();
    let handle = driver::start(request.provider, request.options, event_tx)?;
    Ok(PreparedDriver { handle, events })
}

/// Perform every blocking operation between accepting a submission and
/// starting its provider. This function is called only from the background
/// executor; the UI thread owns applying the returned workspace afterward.
fn prepare_submission(
    project: Project,
    workspace: SessionWorkspace,
    driver_start: Option<anyhow::Result<DriverStartRequest>>,
    session_id: Uuid,
    prompt: &str,
    baseline_count: usize,
    baseline_in_flight: bool,
) -> anyhow::Result<PreparedSubmission> {
    let workspace = match workspace {
        SessionWorkspace::NewWorktree { base_branch } => {
            if project.is_projectless() {
                anyhow::bail!("a projectless task cannot create a Git worktree");
            }
            let created = crate::worktree::create(
                &project.path,
                project.id,
                session_id,
                prompt,
                base_branch.as_deref(),
            )?;
            SessionWorkspace::Worktree {
                path: created.path,
                branch: created.branch,
            }
        }
        workspace => workspace,
    };
    let project_path = workspace.path().unwrap_or(&project.path);

    // The pre-turn checkpoint is what a later rewind restores to. A capture
    // already running for the same turn writes this exact ref, so starting a
    // second `git add -A` would only race equivalent work over the workspace.
    let checkpoint_warning = (!baseline_in_flight)
        .then(|| {
            let git_ref = checkpoint::checkpoint_ref(session_id, baseline_count);
            (!checkpoint::has_ref(project_path, &git_ref))
                .then(|| checkpoint::capture_turn(project_path, session_id, baseline_count).err())
                .flatten()
        })
        .flatten()
        .map(|error| format!("Could not capture the pre-turn checkpoint: {error}"));

    // Process startup can synchronously resolve executables, bind sockets,
    // and spawn children. It belongs behind the same animated preparation
    // boundary as Git work, otherwise the last spinner frame visibly freezes
    // just before Stop appears.
    let driver = driver_start.map(|request| {
        request.and_then(|request| start_driver(request, project_path.to_path_buf()))
    });

    Ok(PreparedSubmission {
        workspace,
        checkpoint_warning,
        driver,
    })
}

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

    /// The directory every filesystem and provider operation for `session`
    /// must use. A not-yet-materialized worktree draft deliberately reads the
    /// local checkout until its first submission creates the isolated copy.
    pub(super) fn workspace_path_for_session<'a>(
        &'a self,
        session: &'a AgentSession,
    ) -> Option<&'a std::path::Path> {
        let project = self
            .state
            .projects
            .iter()
            .find(|project| project.id == session.project_id)?;
        Some(session.workspace.path().unwrap_or(&project.path))
    }

    pub(super) fn selected_workspace_path(&self) -> Option<&std::path::Path> {
        let session = self.selected_session()?;
        self.workspace_path_for_session(session)
    }

    /// Marks the session for the next save; see `PersistedState::session_mut`.
    pub(super) fn selected_session_mut(&mut self) -> Option<&mut AgentSession> {
        let id = self.state.selected_session?;
        self.state.session_mut(id)
    }

    pub(super) fn selected_runtime(&self) -> Option<&SessionRuntime> {
        self.runtimes.get(&self.state.selected_session?)
    }

    pub(super) fn provider_probe(&self, provider: ProviderKind) -> Option<&ProviderProbe> {
        self.probes.iter().find(|probe| probe.provider == provider)
    }

    /// Kick off discovery for every installed provider, so the model picker
    /// never opens onto a lazy load. Runs once at launch; the catalog cached
    /// by the last discovery (or the hardcoded fallback before any run has
    /// cached one) stands in until this launch's discovery lands.
    pub(super) fn request_all_model_discoveries(&mut self) {
        for provider in ProviderKind::ALL {
            self.request_provider_model_discovery(provider);
        }
    }

    pub(super) fn request_provider_model_discovery(&mut self, provider: ProviderKind) {
        if !provider.supports_model_discovery()
            || self.provider_model_discoveries.contains(&provider)
        {
            return;
        }
        let Some(probe) = self
            .provider_probe(provider)
            .filter(|probe| probe.installed)
            .cloned()
        else {
            return;
        };
        self.provider_model_discoveries.insert(provider);
        self.provider_model_discoveries_pending.insert(provider);
        let provider_probe_tx = self.provider_probe_tx.clone();
        if std::thread::Builder::new()
            .name(format!("waku-{}-model-discovery", provider.id()))
            .spawn(move || {
                // Stale-while-revalidate: the catalog cached by the last
                // successful discovery renders right away, and the CLI's
                // answer replaces it (and the cache) whenever it lands.
                if let Some(models) = crate::model_catalog::cached_models(provider) {
                    let mut cached = probe.clone();
                    cached.models = models;
                    let _ = provider_probe_tx.send(cached);
                }
                let _ = provider_probe_tx.send(probe.discover_models());
            })
            .is_err()
        {
            self.provider_model_discoveries.remove(&provider);
            self.provider_model_discoveries_pending.remove(&provider);
        }
    }

    /// Ask every installed CLI for its version, one short-lived subprocess per
    /// provider on its own thread. Answers land in `provider_versions` through
    /// the drain loop; render reads only that map.
    pub(super) fn request_provider_version_probes(&mut self) {
        let targets = self
            .probes
            .iter()
            .filter(|probe| probe.installed)
            .filter_map(|probe| probe.path.clone().map(|path| (probe.provider, path)))
            .collect::<Vec<_>>();
        for (provider, path) in targets {
            if !self.provider_version_probes_pending.insert(provider) {
                continue;
            }
            let provider_version_tx = self.provider_version_tx.clone();
            if std::thread::Builder::new()
                .name(format!("waku-{}-version-probe", provider.id()))
                .spawn(move || {
                    let version = probe_provider_version(&path);
                    let _ = provider_version_tx.send((provider, version));
                })
                .is_err()
            {
                self.provider_version_probes_pending.remove(&provider);
            }
        }
    }

    pub(super) fn drain_provider_version_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok((provider, version)) = self.provider_version_events.try_recv() {
            self.provider_version_probes_pending.remove(&provider);
            self.provider_versions.insert(provider, version);
            changed = true;
        }
        changed
    }

    /// Re-detect provider CLIs off-thread — every provider for the Providers
    /// page's refresh, or one whose binary override just changed. Also re-runs
    /// model discovery and version probes for whatever the detection finds
    /// installed.
    pub(super) fn refresh_provider_detection(&mut self, scope: Option<ProviderKind>) {
        if self.provider_detection_remaining > 0 {
            return;
        }
        let providers = match scope {
            Some(provider) => vec![provider],
            None => ProviderKind::ALL.to_vec(),
        };
        self.provider_detection_remaining = providers.len();
        let overrides = self.state.provider_binary_overrides.clone();
        let provider_detection_tx = self.provider_detection_tx.clone();
        let detect_providers = providers.clone();
        if std::thread::Builder::new()
            .name("waku-provider-detection".into())
            .spawn(move || {
                for provider in detect_providers {
                    let path = match overrides.get(&provider) {
                        Some(binary) => crate::command_env::resolve_binary_override(binary),
                        None => crate::command_env::find_executable(provider.command()),
                    };
                    let _ = provider_detection_tx.send((provider, path.is_some(), path));
                }
            })
            .is_err()
        {
            self.provider_detection_remaining = 0;
            return;
        }
        // A refresh means "re-check everything about these providers":
        // clearing the per-launch guard lets each one's catalog discovery run
        // again as its detection lands below.
        for provider in providers {
            self.provider_model_discoveries.remove(&provider);
        }
    }

    pub(super) fn drain_provider_detection_events(&mut self) -> bool {
        let mut changed = false;
        let mut installed_providers = Vec::new();
        while let Ok((provider, installed, path)) = self.provider_detection_events.try_recv() {
            self.provider_detection_remaining = self.provider_detection_remaining.saturating_sub(1);
            if self.provider_detection_remaining == 0 {
                self.provider_detection_checked_at = Some(Instant::now());
            }
            // Merge detection fields only: a probe's model catalog belongs to
            // model discovery, and overwriting it here would race a discovery
            // still in flight.
            if let Some(existing) = self
                .probes
                .iter_mut()
                .find(|existing| existing.provider == provider)
            {
                existing.installed = installed;
                existing.path = path;
            } else {
                self.probes.push(ProviderProbe {
                    provider,
                    installed,
                    path,
                    models: crate::model_catalog::fallback_models(provider),
                });
            }
            if installed {
                installed_providers.push(provider);
            } else {
                self.provider_versions.remove(&provider);
            }
            changed = true;
        }
        for provider in installed_providers {
            self.request_provider_model_discovery(provider);
        }
        if changed {
            self.request_provider_version_probes();
        }
        changed
    }

    /// Whether the provider can back a new session: installed and not switched
    /// off in the Providers settings.
    pub(super) fn provider_enabled(&self, provider: ProviderKind) -> bool {
        !self.state.disabled_providers.contains(&provider)
            && self
                .provider_probe(provider)
                .is_some_and(|probe| probe.installed)
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
        if let Err(error) = self.store.save(&mut self.state) {
            self.show_toast(format!("Could not save local state: {error}"));
        } else {
            self.stream_state_dirty = false;
        }
    }

    /// Queues the newest finished turn's checkpoint for capture.
    ///
    /// Bookkeeping only. The capture itself is upwards of ten `git`
    /// invocations, one of them a `git add -A` over the whole worktree, and the
    /// hottest caller is the driver-event drain that shares the UI thread with
    /// rendering — so the work belongs to
    /// [`Self::start_pending_checkpoint_captures`], which every caller that
    /// holds a `Context` runs straight after queueing.
    pub(super) fn capture_latest_turn_checkpoint_for(&mut self, session_id: Uuid) {
        let Some((session, turn_count)) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| {
                session
                    .turns
                    .last()
                    .filter(|turn| turn.status != TurnStatus::Running)
                    .map(|turn| (session, turn.turn_count))
            })
        else {
            return;
        };
        let Some(project_path) = self
            .workspace_path_for_session(session)
            .map(std::path::Path::to_path_buf)
        else {
            return;
        };
        self.pending_checkpoint_captures
            .push(PendingCheckpointCapture {
                session_id,
                turn_count,
                project_path,
            });
    }

    /// Runs queued turn checkpoints on the background executor.
    ///
    /// A capture lands a frame or many later, and the turn it belongs to may be
    /// gone by then, so the result is matched back by turn count rather than
    /// position. Nothing on screen waits for it: the transcript's rewind
    /// affordance appears when `invalidate_checkpoint_refs` prompts the next
    /// prefetch to notice the new ref.
    pub(super) fn start_pending_checkpoint_captures(&mut self, cx: &mut Context<Self>) {
        for request in std::mem::take(&mut self.pending_checkpoint_captures) {
            let PendingCheckpointCapture {
                session_id,
                turn_count,
                project_path,
            } = request;
            if !self
                .checkpoint_captures_in_flight
                .insert((session_id, turn_count))
            {
                continue;
            }
            cx.spawn(async move |waku, cx| {
                let captured =
                    cx.background_executor()
                        .spawn({
                            let project_path = project_path.clone();
                            async move {
                                checkpoint::capture_turn(&project_path, session_id, turn_count)
                            }
                        })
                        .await;
                waku.update(cx, |waku, cx| {
                    waku.checkpoint_captures_in_flight
                        .remove(&(session_id, turn_count));
                    let checkpoint = match captured {
                        Ok(checkpoint) => checkpoint,
                        Err(error) => {
                            waku.show_toast(format!(
                                "Could not capture the turn checkpoint: {error}"
                            ));
                            Checkpoint {
                                turn_count,
                                git_ref: checkpoint::checkpoint_ref(session_id, turn_count),
                                status: CheckpointStatus::Error,
                                files: Vec::new(),
                                created_at: unix_time(),
                            }
                        }
                    };
                    waku.invalidate_checkpoint_refs();
                    if let Some(session) = waku.state.session_mut(session_id)
                        && let Some(turn) = session
                            .turns
                            .iter_mut()
                            .find(|turn| turn.turn_count == turn_count)
                    {
                        turn.checkpoint = Some(checkpoint);
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    pub(super) fn fork_session_from_response(
        &mut self,
        session_id: Uuid,
        turn_count: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .cloned()
        else {
            self.show_toast("That response is no longer available.");
            cx.notify();
            return;
        };
        if self.state.selected_session != Some(session_id)
            || !matches!(source.status, SessionStatus::Idle | SessionStatus::Failed)
            || !source.provider.supports_conversation_fork()
            || source
                .turns
                .get(turn_count.saturating_sub(1))
                .is_none_or(|turn| turn.turn_count != turn_count || !turn.provider_turn_started)
        {
            self.show_toast("That response cannot be forked right now.");
            cx.notify();
            return;
        }
        let Some(source_workspace_path) = self
            .workspace_path_for_session(&source)
            .map(std::path::Path::to_path_buf)
        else {
            self.show_toast("That task's project could not be found.");
            cx.notify();
            return;
        };

        let provider_turn_count = source
            .turns
            .iter()
            .take(turn_count)
            .filter(|turn| turn.provider_turn_started)
            .count();
        let turns_to_remove = source.provider_turns_after(turn_count);
        let native_fork = (|| -> anyhow::Result<(
            ProviderResumeCursor,
            Option<std::collections::HashMap<String, String>>,
        )> {
            match source.provider {
                ProviderKind::Claude => {
                    let ProviderResumeCursor::Claude {
                        session_id: native_session_id,
                        ..
                    } = source
                        .provider_cursor
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Claude's native session is unavailable"))?
                    else {
                        anyhow::bail!("Claude's native session is unavailable");
                    };
                    let resume_at = source.turns[turn_count - 1]
                        .provider_resume_at
                        .clone()
                        .map(Ok)
                        .unwrap_or_else(|| {
                            crate::claude_session::message_id_for_turn(
                                native_session_id,
                                provider_turn_count,
                            )
                        })?;
                    let fork = crate::claude_session::fork_session_at(
                        native_session_id,
                        &resume_at,
                        &format!("{} (fork)", source.title),
                    )?;
                    let fork_resume_at = fork
                        .message_ids
                        .get(&resume_at)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("Claude omitted the fork checkpoint"))?;
                    Ok((
                        ProviderResumeCursor::Claude {
                            session_id: fork.session_id,
                            resume_at: Some(fork_resume_at),
                        },
                        Some(fork.message_ids),
                    ))
                }
                ProviderKind::Codex => {
                    if !matches!(
                        source.provider_cursor.as_ref(),
                        Some(ProviderResumeCursor::Codex { .. })
                    ) {
                        anyhow::bail!("Codex's native thread is unavailable");
                    }
                    Ok((self.ensure_driver()?.fork(turns_to_remove)?, None))
                }
                ProviderKind::Cursor => Ok((
                    crate::cursor_session::fork_session_at_turn(&source, turn_count)?,
                    None,
                )),
                ProviderKind::Amp => {
                    let Some(ProviderResumeCursor::Amp {
                        thread_id: native_thread_id,
                        fork_context,
                    }) = source.provider_cursor.as_ref()
                    else {
                        anyhow::bail!("Amp's native thread is unavailable");
                    };
                    let binary = self
                        .probes
                        .iter()
                        .find(|probe| probe.provider == ProviderKind::Amp)
                        .and_then(|probe| probe.path.as_deref())
                        .ok_or_else(|| anyhow::anyhow!("Amp is not installed"))?;
                    Ok((
                        crate::amp_session::fork_session_at_turn(
                            binary,
                            &source_workspace_path,
                            native_thread_id,
                            fork_context.as_deref(),
                            provider_turn_count,
                        )?,
                        None,
                    ))
                }
                ProviderKind::OpenCode => {
                    let Some(ProviderResumeCursor::OpenCode {
                        session_id: native_session_id,
                    }) = source.provider_cursor.as_ref()
                    else {
                        anyhow::bail!("OpenCode's native session is unavailable");
                    };
                    let binary = self
                        .probes
                        .iter()
                        .find(|probe| probe.provider == ProviderKind::OpenCode)
                        .and_then(|probe| probe.path.as_deref())
                        .ok_or_else(|| anyhow::anyhow!("OpenCode is not installed"))?;
                    Ok((
                        crate::opencode_session::fork_session_at_turn(
                            binary,
                            &source_workspace_path,
                            native_session_id,
                            provider_turn_count,
                        )?,
                        None,
                    ))
                }
                ProviderKind::Grok => {
                    let Some(ProviderResumeCursor::Grok {
                        session_id: native_session_id,
                    }) = source.provider_cursor.as_ref()
                    else {
                        anyhow::bail!("Grok's native session is unavailable");
                    };
                    let binary = self
                        .probes
                        .iter()
                        .find(|probe| probe.provider == ProviderKind::Grok)
                        .and_then(|probe| probe.path.as_deref())
                        .ok_or_else(|| anyhow::anyhow!("Grok Build is not installed"))?;
                    Ok((
                        crate::grok_session::fork_session_at_turn(
                            binary,
                            &source_workspace_path,
                            native_session_id,
                            provider_turn_count,
                        )?,
                        None,
                    ))
                }
                ProviderKind::Pi => {
                    if !matches!(
                        source.provider_cursor.as_ref(),
                        Some(ProviderResumeCursor::Pi {
                            session_file: Some(_),
                            ..
                        })
                    ) {
                        anyhow::bail!("Pi's native session file is unavailable");
                    }
                    Ok((self.ensure_driver()?.fork(turns_to_remove)?, None))
                }
            }
        })();

        let (provider_cursor, claude_message_ids) = match native_fork {
            Ok(fork) => fork,
            Err(error) => {
                if source.provider == ProviderKind::Pi {
                    // A failed restore after Pi creates a fork can leave the RPC
                    // process on that fork. Recreate it from the source cursor.
                    self.runtimes.remove(&session_id);
                }
                self.show_toast(format!("Could not fork the task: {error}"));
                cx.notify();
                return;
            }
        };
        let Some(mut forked) = source.fork_through_turn(turn_count, provider_cursor) else {
            self.show_toast("That response could not be copied into a new task.");
            cx.notify();
            return;
        };
        if let Some(message_ids) = claude_message_ids {
            for turn in &mut forked.turns {
                if let Some(message_id) = turn.provider_resume_at.as_mut()
                    && let Some(remapped) = message_ids.get(message_id)
                {
                    *message_id = remapped.clone();
                }
            }
        }

        let fork_id = forked.id;
        for turn in &mut forked.turns {
            if let Some(checkpoint) = turn.checkpoint.as_mut() {
                checkpoint.git_ref = checkpoint::checkpoint_ref(fork_id, checkpoint.turn_count);
            }
        }
        let checkpoint_warning =
            checkpoint::copy_session_refs(&source_workspace_path, source.id, fork_id, turn_count)
                .err();
        self.invalidate_checkpoint_refs();

        self.state.push_session(forked);
        self.select_session(fork_id, cx);
        self.show_toast(match checkpoint_warning {
            Some(error) => {
                format!("Forked task; some Git checkpoints could not be copied: {error}")
            }
            None => "Forked task from this response.".into(),
        });
        self.save();
        cx.notify();
    }

    pub(super) fn begin_message_edit(
        &mut self,
        session_id: Uuid,
        turn_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((message_index, initial_message)) = self
            .state
            .sessions
            .iter()
            .find(|session| {
                session.id == session_id
                    && session.provider.supports_conversation_rollback()
                    && matches!(session.status, SessionStatus::Idle | SessionStatus::Failed)
            })
            .and_then(|session| {
                let turn = session
                    .turns
                    .iter()
                    .find(|turn| turn.turn_count == turn_count)?;
                session
                    .messages
                    .iter()
                    .enumerate()
                    .find_map(|(index, message)| {
                        (message.turn_id == Some(turn.id) && message.role == MessageRole::User)
                            .then(|| (index, message.content.clone()))
                    })
            })
        else {
            self.show_toast("That message is not editable right now.");
            cx.notify();
            return;
        };

        let input = cx.new(|cx| ComposerInput::new(window, cx));
        input.update(cx, |input, cx| input.set_content(initial_message, cx));
        cx.subscribe(
            &input,
            |this: &mut Self, _, event: &ComposerEvent, cx| match event {
                ComposerEvent::Submit(_) => this.submit_message_edit(cx),
                // An edited past message resubmits from that point; there is
                // no running turn for it to steer.
                ComposerEvent::SubmitSteer(_) => this.submit_message_edit(cx),
                ComposerEvent::Edited => cx.notify(),
                ComposerEvent::Focus => {}
                ComposerEvent::BackspaceOnEmpty => {}
            },
        )
        .detach();
        self.message_edit = Some(MessageEdit {
            session_id,
            turn_count,
            input: input.clone(),
        });
        self.hide_toast();
        self.remeasure_transcript_message(message_index);
        let focus_handle = input.read(cx).focus();
        window.focus(&focus_handle, cx);
        cx.notify();
    }

    pub(super) fn cancel_message_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.message_edit.take() else {
            return;
        };
        let message_index = self.selected_session().and_then(|session| {
            let turn_id = session
                .turns
                .iter()
                .find(|turn| turn.turn_count == edit.turn_count)?
                .id;
            session.messages.iter().position(|message| {
                message.turn_id == Some(turn_id) && message.role == MessageRole::User
            })
        });
        if let Some(message_index) = message_index {
            self.remeasure_transcript_message(message_index);
        }
        let focus_handle = self.composer_focus(cx);
        window.focus(&focus_handle, cx);
        cx.notify();
    }

    pub(super) fn submit_message_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.message_edit.clone() else {
            return;
        };
        let prompt = edit.input.read(cx).content().trim().to_owned();
        if prompt.is_empty() {
            self.show_toast("The edited message cannot be empty.");
            cx.notify();
            return;
        }
        if !self.rewind_before_turn(edit.session_id, edit.turn_count, cx) {
            return;
        }
        self.message_edit = None;
        self.submit_prompt(prompt, cx);
    }

    fn rewind_before_turn(
        &mut self,
        session_id: Uuid,
        turn_count: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        let retained_turn_count = turn_count.saturating_sub(1);
        let Some((
            provider,
            status,
            provider_cursor,
            previous_turn_count,
            rollback_turns,
            provider_turn_count,
            provider_resume_at,
            session_title,
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
                    .map(|_| {
                        (
                            session.provider,
                            session.status,
                            session.provider_cursor.clone(),
                            session.turns.len(),
                            session.provider_turns_after(retained_turn_count),
                            session
                                .turns
                                .iter()
                                .take(retained_turn_count)
                                .filter(|turn| turn.provider_turn_started)
                                .count(),
                            retained_turn_count
                                .checked_sub(1)
                                .and_then(|index| session.turns.get(index))
                                .and_then(|turn| turn.provider_resume_at.clone()),
                            session.title.clone(),
                        )
                    })
            })
        else {
            self.show_toast("That message is no longer available.");
            cx.notify();
            return false;
        };
        if self.state.selected_session != Some(session_id) {
            self.show_toast("Select the task before rewinding its conversation.");
            cx.notify();
            return false;
        }
        if !matches!(status, SessionStatus::Idle | SessionStatus::Failed) {
            self.show_toast("Stop the current turn before rewinding the conversation.");
            cx.notify();
            return false;
        }
        if !provider.supports_conversation_rollback()
            || (rollback_turns > 0 && provider_cursor.is_none())
        {
            self.show_toast(format!(
                "{} cannot safely roll back its native conversation yet.",
                provider.display_name()
            ));
            cx.notify();
            return false;
        }
        let Some(project_path) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| self.workspace_path_for_session(session))
            .map(std::path::Path::to_path_buf)
        else {
            self.show_toast("The task's project could not be found.");
            cx.notify();
            return false;
        };
        let checkpoint_ref = checkpoint::checkpoint_ref(session_id, retained_turn_count);
        if !checkpoint::has_ref(&project_path, &checkpoint_ref) {
            self.show_toast("The message's pre-turn Git checkpoint is missing.");
            cx.notify();
            return false;
        }

        let claude_reset =
            provider == ProviderKind::Claude && rollback_turns > 0 && retained_turn_count == 0;
        let cursor_reset =
            provider == ProviderKind::Cursor && rollback_turns > 0 && retained_turn_count == 0;
        let grok_reset =
            provider == ProviderKind::Grok && rollback_turns > 0 && retained_turn_count == 0;
        let claude_rollback =
            if provider == ProviderKind::Claude && rollback_turns > 0 && retained_turn_count > 0 {
                let Some(ProviderResumeCursor::Claude {
                    session_id: native_session_id,
                    ..
                }) = provider_cursor.as_ref()
                else {
                    self.show_toast("Claude's native session cursor is unavailable.");
                    cx.notify();
                    return false;
                };
                let resume_at = match provider_resume_at {
                    Some(message_id) => message_id,
                    None => match crate::claude_session::message_id_for_turn(
                        native_session_id,
                        provider_turn_count,
                    ) {
                        Ok(message_id) => message_id,
                        Err(error) => {
                            self.show_toast(format!(
                                "Claude's native checkpoint for that turn is unavailable: {error}"
                            ));
                            cx.notify();
                            return false;
                        }
                    },
                };
                Some((native_session_id.clone(), resume_at))
            } else {
                None
            };

        let safety_ref = format!("refs/waku/revert-backup-{session_id}-{}", Uuid::new_v4());
        if let Err(error) = checkpoint::capture_ref(&project_path, &safety_ref) {
            self.show_toast(format!(
                "Could not create a rewind safety snapshot: {error}"
            ));
            cx.notify();
            return false;
        }
        if let Err(error) = checkpoint::restore_ref(&project_path, &checkpoint_ref) {
            self.show_toast(match checkpoint::restore_ref(&project_path, &safety_ref) {
                Ok(()) => {
                    let _ = checkpoint::delete_ref(&project_path, &safety_ref);
                    format!("Could not restore the checkpoint: {error}")
                }
                Err(restore_error) => format!(
                    "Checkpoint restore failed ({error}); safety restore also failed ({restore_error}). Recovery ref retained at {safety_ref}."
                ),
            });
            cx.notify();
            return false;
        }

        let mut claude_fork = None;
        let mut provider_rewind_cursor = None;
        if rollback_turns > 0 && !claude_reset && !cursor_reset && !grok_reset {
            let rollback_result = if let Some((native_session_id, resume_at)) = &claude_rollback {
                crate::claude_session::fork_session_at(
                    native_session_id,
                    resume_at,
                    &format!("{session_title} (rewind)"),
                )
                .map(|fork| {
                    claude_fork = Some((fork, resume_at.to_owned()));
                })
            } else if provider == ProviderKind::OpenCode {
                let Some(ProviderResumeCursor::OpenCode {
                    session_id: native_session_id,
                }) = provider_cursor.as_ref()
                else {
                    self.show_toast("OpenCode's native session cursor is unavailable.");
                    cx.notify();
                    return false;
                };
                let Some(binary) = self
                    .probes
                    .iter()
                    .find(|probe| probe.provider == ProviderKind::OpenCode)
                    .and_then(|probe| probe.path.as_deref())
                else {
                    self.show_toast("OpenCode is not installed or could not be found.");
                    cx.notify();
                    return false;
                };
                crate::opencode_session::fork_session_at_turn(
                    binary,
                    &project_path,
                    native_session_id,
                    provider_turn_count,
                )
                .map(|cursor| provider_rewind_cursor = Some(cursor))
            } else if provider == ProviderKind::Amp {
                let Some(ProviderResumeCursor::Amp {
                    thread_id: native_thread_id,
                    fork_context,
                }) = provider_cursor.as_ref()
                else {
                    self.show_toast("Amp's native thread cursor is unavailable.");
                    cx.notify();
                    return false;
                };
                let Some(binary) = self
                    .probes
                    .iter()
                    .find(|probe| probe.provider == ProviderKind::Amp)
                    .and_then(|probe| probe.path.as_deref())
                else {
                    self.show_toast("Amp is not installed or could not be found.");
                    cx.notify();
                    return false;
                };
                crate::amp_session::fork_session_at_turn(
                    binary,
                    &project_path,
                    native_thread_id,
                    fork_context.as_deref(),
                    provider_turn_count,
                )
                .map(|cursor| provider_rewind_cursor = Some(cursor))
            } else if provider == ProviderKind::Cursor {
                let Some(source) = self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
                else {
                    self.show_toast("Cursor's Waku task is unavailable.");
                    cx.notify();
                    return false;
                };
                crate::cursor_session::fork_session_at_turn(source, retained_turn_count)
                    .map(|cursor| provider_rewind_cursor = Some(cursor))
            } else if provider == ProviderKind::Grok {
                let Some(ProviderResumeCursor::Grok {
                    session_id: native_session_id,
                }) = provider_cursor.as_ref()
                else {
                    self.show_toast("Grok's native session cursor is unavailable.");
                    cx.notify();
                    return false;
                };
                let Some(binary) = self
                    .probes
                    .iter()
                    .find(|probe| probe.provider == ProviderKind::Grok)
                    .and_then(|probe| probe.path.as_deref())
                else {
                    self.show_toast("Grok Build is not installed or could not be found.");
                    cx.notify();
                    return false;
                };
                crate::grok_session::fork_session_at_turn(
                    binary,
                    &project_path,
                    native_session_id,
                    provider_turn_count,
                )
                .map(|cursor| provider_rewind_cursor = Some(cursor))
            } else {
                self.ensure_driver()
                    .and_then(|driver| driver.rollback(rollback_turns))
                    .map(|cursor| provider_rewind_cursor = cursor)
            };
            if let Err(error) = rollback_result {
                let restore_result = checkpoint::restore_ref(&project_path, &safety_ref);
                self.show_toast(match restore_result {
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
                return false;
            }
        }

        let _ = checkpoint::delete_ref(&project_path, &safety_ref);
        let cleanup_result = checkpoint::delete_turn_refs_after(
            &project_path,
            session_id,
            retained_turn_count,
            previous_turn_count,
        );
        self.invalidate_checkpoint_refs();
        self.sync_transcript_rows();
        let previous_kinds = self.transcript_row_kinds.borrow().clone();
        if let Some(session) = self.state.session_mut(session_id) {
            if let Some((fork, source_resume_at)) = &claude_fork {
                for turn in session.turns.iter_mut().take(retained_turn_count) {
                    if let Some(remapped) = turn
                        .provider_resume_at
                        .as_ref()
                        .and_then(|message_id| fork.message_ids.get(message_id))
                        .cloned()
                    {
                        turn.provider_resume_at = Some(remapped);
                    }
                }
                let remapped_resume_at = fork
                    .message_ids
                    .get(source_resume_at)
                    .cloned()
                    .expect("the Claude fork includes its target message");
                session.provider_cursor = Some(ProviderResumeCursor::Claude {
                    session_id: fork.session_id.clone(),
                    resume_at: Some(remapped_resume_at),
                });
            } else if claude_reset || cursor_reset || grok_reset {
                session.provider_cursor = None;
            } else if let Some(cursor) = provider_rewind_cursor.clone() {
                session.provider_cursor = Some(cursor);
            }
            session.truncate_after_turn(retained_turn_count);
            session.status = SessionStatus::Idle;
        }
        if claude_fork.is_some()
            || claude_reset
            || cursor_reset
            || grok_reset
            || (matches!(
                provider,
                ProviderKind::Amp
                    | ProviderKind::Cursor
                    | ProviderKind::OpenCode
                    | ProviderKind::Grok
            ) && provider_rewind_cursor.is_some())
        {
            // Headless drivers retain their original native session ID. Recreate
            // them lazily so the next prompt resumes the fork instead.
            self.runtimes.remove(&session_id);
        } else if let Some(runtime) = self.runtimes.get_mut(&session_id) {
            runtime.pending_events.clear();
            runtime.stream_remeasure_pending = false;
            runtime.stream_phase = None;
            runtime.pending_permission = None;
            runtime.pending_computer_approval = None;
        }
        self.reasoning_expanded.clear();
        self.activities_expanded.clear();
        self.expanded_activity_items.clear();
        self.expanded_turns.clear();
        self.splice_transcript_rows_after_visibility_change(&previous_kinds);
        self.show_toast(match cleanup_result {
            Ok(()) => format!("Rewound to before turn {turn_count}."),
            Err(error) => {
                format!("Rewound to before turn {turn_count}; stale refs remain: {error}")
            }
        });
        self.save();
        cx.notify();
        true
    }

    /// Resolves the turn options a driver should run with, dropping a reasoning
    /// effort or service tier the resolved model does not offer. Driver start
    /// and in-session option changes both go through this so they cannot
    /// disagree about what the session is currently set to.
    pub(super) fn session_options(&self, session: &AgentSession) -> SessionOptions {
        let model = session.model.clone().or_else(|| {
            self.provider_probe(session.provider)
                .and_then(ProviderProbe::preferred_model)
                .map(|model| model.id.clone())
        });
        let model_metadata = self.model_metadata_for_session(session);
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
        SessionOptions {
            mode: session.runtime_mode,
            interaction_mode: session.interaction_mode,
            model,
            reasoning_effort,
            service_tier,
        }
    }

    /// Releases provider processes for sessions nobody has touched in a while.
    ///
    /// Codex and Pi keep a process resident between turns, so an abandoned task
    /// otherwise holds an agent — and, with Computer Use on, a whole process
    /// tree — for as long as the app runs. Recreating a runtime is exactly the
    /// work the next prompt already does after Stop, and the resume cursor is
    /// persisted, so the conversation survives.
    pub(super) fn reap_idle_sessions(&mut self) {
        if self.last_idle_session_sweep.elapsed() < IDLE_SESSION_SWEEP_INTERVAL {
            return;
        }
        self.last_idle_session_sweep = Instant::now();
        let idle = self
            .runtimes
            .iter()
            .filter(|(session_id, runtime)| {
                let session = self
                    .state
                    .sessions
                    .iter()
                    .find(|session| session.id == **session_id);
                session_is_reapable(session, runtime.last_active_at.elapsed())
            })
            .map(|(session_id, _)| *session_id)
            .collect::<Vec<_>>();
        for session_id in idle {
            // Dropping the runtime is the release: it closes the transport, and
            // the driver's own `Drop` takes the process tree with it. No cancel,
            // because there is no turn to cancel.
            self.runtimes.remove(&session_id);
        }
    }

    /// Applies a changed model, effort, tier, or mode to a session. Transports
    /// that carry these per turn absorb the change and keep running; the rest
    /// are torn down so the next prompt starts with the new options.
    pub(super) fn apply_session_options(&mut self, session_id: Uuid) {
        let Some(options) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(|session| self.session_options(session))
        else {
            return;
        };
        let applied = self
            .runtimes
            .get(&session_id)
            .is_none_or(|runtime| runtime.driver.apply_options(options));
        if !applied {
            self.reset_session_runtime(session_id);
        }
    }

    pub(super) fn ensure_driver(&mut self) -> anyhow::Result<DriverHandle> {
        let session_id = self
            .selected_session()
            .map(|session| session.id)
            .ok_or_else(|| anyhow::anyhow!("No session selected"))?;
        self.ensure_driver_for_session(session_id)
    }

    pub(super) fn ensure_driver_for_session(
        &mut self,
        session_id: Uuid,
    ) -> anyhow::Result<DriverHandle> {
        let session = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Session not found"))?;
        if let Some(runtime) = self.runtimes.get(&session.id) {
            return Ok(runtime.driver.clone());
        }
        let workspace_path = self
            .workspace_path_for_session(&session)
            .map(std::path::Path::to_path_buf)
            .ok_or_else(|| anyhow::anyhow!("Project not found"))?;
        let prepared = start_driver(
            self.driver_start_request_for_session(&session, workspace_path.clone())?,
            workspace_path,
        )?;
        Ok(self.install_prepared_driver(session.id, prepared))
    }

    fn driver_start_request_for_session(
        &self,
        session: &AgentSession,
        cwd: PathBuf,
    ) -> anyhow::Result<DriverStartRequest> {
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
        let SessionOptions {
            mode,
            interaction_mode,
            model,
            reasoning_effort,
            service_tier,
        } = self.session_options(&session);
        Ok(DriverStartRequest {
            provider: session.provider,
            options: DriverStartOptions {
                binary,
                cwd,
                mode,
                interaction_mode,
                model,
                reasoning_effort,
                service_tier,
                computer_use_enabled: self.state.computer_use_enabled,
                provider_cursor: session.provider_cursor.clone(),
            },
        })
    }

    fn install_prepared_driver(
        &mut self,
        session_id: Uuid,
        prepared: PreparedDriver,
    ) -> DriverHandle {
        let handle = prepared.handle.clone();
        self.runtimes.insert(
            session_id,
            SessionRuntime {
                driver: prepared.handle,
                events: prepared.events,
                pending_events: VecDeque::new(),
                stream_phase: None,
                stream_remeasure_pending: false,
                pending_permission: None,
                pending_computer_approval: None,
                computer_use_previews: Vec::new(),
                computer_session_grants: HashSet::new(),
                last_driver_error: None,
                last_active_at: Instant::now(),
            },
        );
        handle
    }

    pub(super) fn submit_prompt(&mut self, prompt: String, cx: &mut Context<Self>) {
        let Some(session) = self.selected_session() else {
            return;
        };
        if session.is_busy() {
            // While the agent is working, Enter queues a follow-up instead of
            // refusing the message. The queue drains once the turn settles.
            self.enqueue_follow_up(session.id, prompt, cx);
            return;
        }
        self.submit_prompt_for_session(session.id, prompt, cx);
    }

    /// Deliver a steering message into the running turn. Providers without a
    /// live-turn transport (or a session that is not actively working) fall
    /// back to queueing a follow-up.
    pub(super) fn steer_prompt(&mut self, prompt: String, cx: &mut Context<Self>) {
        let Some(session) = self.selected_session().cloned() else {
            return;
        };
        if !session.is_busy() {
            self.submit_prompt(prompt, cx);
            return;
        }
        // A turn that has not reached the provider yet cannot be steered; the
        // driver reports the outcome asynchronously via SteerAccepted or
        // SteerRejected once it is handed off.
        let steerable = session.status != SessionStatus::Connecting
            && self
                .runtimes
                .get(&session.id)
                .is_some_and(|runtime| runtime.driver.supports_steer());
        if !steerable {
            self.enqueue_follow_up(session.id, prompt, cx);
            return;
        }
        if let Some(runtime) = self.runtimes.get_mut(&session.id) {
            runtime.driver.steer(prompt);
        } else {
            self.enqueue_follow_up(session.id, prompt, cx);
        }
        cx.notify();
    }

    pub(super) fn enqueue_follow_up(
        &mut self,
        session_id: Uuid,
        prompt: String,
        cx: &mut Context<Self>,
    ) {
        let prompt = prompt.trim().to_owned();
        if prompt.is_empty() {
            return;
        }
        if let Some(session) = self.state.session_mut(session_id) {
            session.queued_messages.push(QueuedMessage::new(prompt));
            session.updated_at = unix_time();
        }
        self.save();
        cx.notify();
    }

    pub(super) fn remove_queued_message(
        &mut self,
        session_id: Uuid,
        message_id: Uuid,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.state.session_mut(session_id) {
            session
                .queued_messages
                .retain(|message| message.id != message_id);
        }
        self.save();
        cx.notify();
    }

    /// Pop a queued message back into the composer so the user can edit and
    /// resubmit it.
    pub(super) fn edit_queued_message(
        &mut self,
        session_id: Uuid,
        message_id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(content) = self.state.session_mut(session_id).and_then(|session| {
            let index = session
                .queued_messages
                .iter()
                .position(|message| message.id == message_id)?;
            Some(session.queued_messages.remove(index).content)
        }) else {
            return;
        };
        self.composer
            .update(cx, |input, cx| input.set_content(content, cx));
        let focus_handle = self.composer_focus(cx);
        window.focus(&focus_handle, cx);
        self.save();
        cx.notify();
    }

    /// Start the next queued follow-up as a fresh turn. Only called once a
    /// settled turn has been fully closed, so the session is Idle.
    fn drain_queued_message(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        let Some(session) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        else {
            return;
        };
        if session.is_busy() || session.queued_messages.is_empty() {
            return;
        }
        let Some(prompt) = self
            .state
            .session_mut(session_id)
            .map(|session| session.queued_messages.remove(0).content)
        else {
            return;
        };
        self.submit_prompt_for_session(session_id, prompt, cx);
    }

    pub(super) fn submit_prompt_for_session(
        &mut self,
        session_id: Uuid,
        prompt: String,
        cx: &mut Context<Self>,
    ) {
        let selected = self.state.selected_session == Some(session_id);
        let Some(session) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        else {
            return;
        };
        if session.status.is_busy() {
            self.enqueue_follow_up(session_id, prompt, cx);
            return;
        }
        let next_turn_count = session.turns.len() + 1;
        let project_id = session.project_id;
        let workspace = session.workspace.clone();
        let driver_start = (!self.runtimes.contains_key(&session_id)).then(|| {
            let provisional_cwd = self
                .workspace_path_for_session(session)
                .map(std::path::Path::to_path_buf)
                .unwrap_or_default();
            self.driver_start_request_for_session(session, provisional_cwd)
        });
        let Some(project) = self
            .state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .cloned()
        else {
            if selected {
                self.composer
                    .update(cx, |input, cx| input.set_content(prompt, cx));
                self.show_toast("Could not prepare the task: project not found");
            }
            cx.notify();
            return;
        };
        let baseline_count = next_turn_count - 1;
        let baseline_in_flight = self
            .checkpoint_captures_in_flight
            .contains(&(session_id, baseline_count));

        // Busy is visible before any Git work begins. The separate transient
        // set keeps this non-cancellable phase visually distinct from a
        // connecting provider, whose runtime already has a working Stop path.
        if let Some(session) = self.state.session_mut(session_id) {
            session.status = SessionStatus::Connecting;
            session.updated_at = unix_time();
        }
        self.submission_preparations.insert(session_id);
        cx.notify();

        let recovery_prompt = prompt.clone();
        cx.spawn(async move |waku, cx| {
            let prepared = cx
                .background_executor()
                .spawn(async move {
                    prepare_submission(
                        project,
                        workspace,
                        driver_start,
                        session_id,
                        &prompt,
                        baseline_count,
                        baseline_in_flight,
                    )
                })
                .await;
            let _ = waku.update(cx, move |waku, cx| {
                waku.finish_submission_preparation(session_id, recovery_prompt, prepared, cx);
            });
        })
        .detach();
    }

    fn finish_submission_preparation(
        &mut self,
        session_id: Uuid,
        prompt: String,
        prepared: anyhow::Result<PreparedSubmission>,
        cx: &mut Context<Self>,
    ) {
        if !self.submission_preparations.contains(&session_id) {
            return;
        }
        let selected = self.state.selected_session == Some(session_id);
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.submission_preparations.remove(&session_id);
                if let Some(session) = self.state.session_mut(session_id)
                    && session.status == SessionStatus::Connecting
                    && session.active_turn_id().is_none()
                {
                    session.status = SessionStatus::Idle;
                }
                if selected {
                    self.composer
                        .update(cx, |input, cx| input.set_content(prompt, cx));
                    self.show_toast(format!("Could not create the worktree: {error}"));
                }
                cx.notify();
                return;
            }
        };
        let PreparedSubmission {
            workspace,
            checkpoint_warning,
            driver: prepared_driver,
        } = prepared;
        let can_start = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| {
                session.status == SessionStatus::Connecting && session.active_turn_id().is_none()
            });
        if !can_start {
            self.submission_preparations.remove(&session_id);
            cx.notify();
            return;
        }

        let workspace_changed = self.state.session_mut(session_id).is_some_and(|session| {
            let changed = session.workspace != workspace;
            session.workspace = workspace;
            changed
        });
        if selected && workspace_changed {
            self.invalidate_workspace_queries(cx);
            self.reload_clean_right_panel_file_editors(cx);
            self.ensure_right_panel_terminals(cx);
        }
        let driver = match prepared_driver {
            None => self
                .runtimes
                .get(&session_id)
                .map(|runtime| runtime.driver.clone())
                .ok_or_else(|| anyhow::anyhow!("the prepared agent runtime is unavailable")),
            Some(Ok(prepared)) => Ok(self.install_prepared_driver(session_id, prepared)),
            Some(Err(error)) => Err(error),
        };
        self.invalidate_checkpoint_refs();
        if selected {
            self.sync_transcript_rows();
        }
        let previous_kinds = if selected {
            self.transcript_row_kinds.borrow().clone()
        } else {
            Vec::new()
        };
        let transcript_anchor = if let Some(session) = self.state.session_mut(session_id) {
            session.set_title_from_prompt(&prompt);
            let turn_id = session.begin_turn(&prompt);
            session.updated_at = unix_time();
            selected.then_some(TranscriptAnchor {
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
            runtime.pending_computer_approval = None;
            runtime.last_active_at = Instant::now();
        }
        if selected {
            self.reasoning_expanded.clear();
            self.activities_expanded.clear();
            self.expanded_activity_items.clear();
            self.expanded_turns.clear();
            self.message_edit = None;
            self.replace_toast(checkpoint_warning);
            self.transcript_anchor.set(transcript_anchor);
            self.transcript_anchor_end_space.set(Pixels::ZERO);
            self.transcript_anchor_following.set(true);
            self.splice_transcript_rows_after_visibility_change(&previous_kinds);
            self.scroll_transcript_to_anchor();
        }
        // Template commands expand here, at the seam between the transcript
        // and the transport: the user message keeps the typed `/name …` —
        // the same echo the CLIs show — while the provider receives the
        // rendered prompt. Claude's commands pass through untouched; its CLI
        // owns their expansion.
        let driver_prompt =
            crate::composer_complete::expanded_submission(&prompt, &self.slash_command_index)
                .unwrap_or(prompt);
        let mut failed_to_start = false;
        match driver {
            Ok(driver) => driver.prompt(driver_prompt),
            Err(error) => {
                failed_to_start = true;
                let message = format!("Could not start the agent: {error}");
                if let Some(session) = self.state.session_mut(session_id) {
                    session.status = SessionStatus::Failed;
                    session.push_message(MessageRole::Assistant, message);
                    session.finish_active_turn(TurnStatus::Failed);
                }
            }
        }
        // From this point onward `cancel_turn` has either a live driver to
        // cancel or a settled startup failure. The next frame must therefore
        // show Stop (or Send after failure), never the preparation spinner.
        self.submission_preparations.remove(&session_id);
        if failed_to_start {
            self.capture_latest_turn_checkpoint_for(session_id);
            self.start_pending_checkpoint_captures(cx);
        }
        cx.notify();
        // Persist on the next frame boundary. Saving is intentionally after
        // the spinner-to-Stop paint: SQLite or blob externalization must not
        // hold the final preparation frame motionless.
        cx.spawn(async move |waku, cx| {
            cx.background_executor().timer(STREAM_FRAME_INTERVAL).await;
            let _ = waku.update(cx, |waku, _| waku.save());
        })
        .detach();
    }

    pub(super) fn collect_runtime_events(runtime: &mut SessionRuntime) {
        while let Ok(event) = runtime.events.try_recv() {
            runtime.pending_events.push_back(event);
        }
    }

    pub(super) fn drain_provider_probe_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok(probe) = self.provider_probe_events.try_recv() {
            self.provider_model_discoveries_pending
                .remove(&probe.provider);
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

    pub(super) fn drain_computer_permission_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok(result) = self.computer_permission_events.try_recv() {
            self.computer_permission_request_pending = false;
            match result {
                Ok(permissions) => self.computer_permissions = permissions,
                Err(error) => self.show_toast(error),
            }
            changed = true;
        }
        changed
    }

    pub(super) fn drain_driver_events(&mut self, cx: &mut Context<Self>) -> bool {
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
                        | DriverEvent::SteerAccepted { .. }
                        | DriverEvent::SteerRejected { .. }
                        | DriverEvent::TurnFinished { .. }
                        | DriverEvent::Error(_)
                        | DriverEvent::ProcessExited
                );
                markdown_changed |= matches!(event, DriverEvent::TextDelta(_));
                runtime_changed = true;
                keep_runtime &= self.handle_driver_event(session_id, &mut runtime, event, true, cx);
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

        if !self.pending_queue_drains.is_empty() {
            let drains = std::mem::take(&mut self.pending_queue_drains);
            for session_id in drains {
                self.drain_queued_message(session_id, cx);
            }
            changed = true;
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

/// Run `<cli> --version` and pull a version number out of whatever it prints.
/// Blocking; runs on a version-probe thread, never on the UI thread.
fn probe_provider_version(binary: &std::path::Path) -> Option<String> {
    let output = crate::command_env::command(binary)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    parse_cli_version(&combined)
}

/// The first token that reads as a version number — digits and dots, an
/// optional leading `v`, optional pre-release tail — from the first non-empty
/// line. CLIs decorate this differently ("codex-cli 0.45.0", "2.1.24 (Claude
/// Code)", "v1.3.0-beta"); the number is the part worth showing.
fn parse_cli_version(output: &str) -> Option<String> {
    let line = output.lines().find(|line| !line.trim().is_empty())?;
    line.split_whitespace()
        .map(|token| {
            token
                .trim_start_matches('v')
                .trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'))
        })
        .find(|token| {
            let mut parts = token.split('.');
            let leading_number = parts
                .next()
                .is_some_and(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
            leading_number
                && parts
                    .next()
                    .is_some_and(|part| part.chars().next().is_some_and(|c| c.is_ascii_digit()))
        })
        .map(str::to_owned)
}

#[cfg(test)]
mod version_tests {
    use super::parse_cli_version;

    #[test]
    fn parses_common_cli_version_banners() {
        assert_eq!(
            parse_cli_version("codex-cli 0.45.0\n"),
            Some("0.45.0".to_owned())
        );
        assert_eq!(
            parse_cli_version("2.1.24 (Claude Code)\n"),
            Some("2.1.24".to_owned())
        );
        assert_eq!(
            parse_cli_version("v1.3.0-beta.2"),
            Some("1.3.0-beta.2".to_owned())
        );
        assert_eq!(
            parse_cli_version("\nAmp CLI version 0.9.12\n"),
            Some("0.9.12".to_owned())
        );
        assert_eq!(parse_cli_version("not a version"), None);
        assert_eq!(parse_cli_version(""), None);
    }

    #[test]
    fn version_requires_a_dotted_number_not_a_bare_digit() {
        // "2024" alone or a hash must not read as a version.
        assert_eq!(parse_cli_version("build 2024 f3a9c1"), None);
        assert_eq!(
            parse_cli_version("cursor-agent 2025.09.12-4f8d8e2"),
            Some("2025.09.12-4f8d8e2".to_owned())
        );
    }
}
