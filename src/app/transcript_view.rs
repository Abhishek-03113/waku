use super::*;

impl Waku {
    // ── Transcript ─────────────────────────────────────────────────────────

    pub(super) fn render_transcript(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        self.sync_transcript_rows();
        if self.sync_transcript_layout_width(window) {
            while self.transcript_resize_rx.try_recv().is_ok() {}
        } else {
            self.drain_transcript_resize_events();
        }
        let transcript_rows = self.active_transcript_rows().clone();
        let anchor_end_space = self.update_transcript_anchor_end_space(window);
        if self.transcript_anchor_following.get()
            && anchor_end_space <= Pixels::ZERO
            && self
                .selected_transcript_anchor_row()
                .is_some_and(|anchor_row| anchor_row + 1 < transcript_rows.item_count())
        {
            transcript_rows.scroll_to(ListOffset {
                item_ix: transcript_rows.item_count(),
                offset_in_item: Pixels::ZERO,
            });
            self.transcript_is_scrolled.set(false);
        }
        let entity = cx.entity().downgrade();
        let transcript_viewport = TextViewScrollViewport::from_list(&transcript_rows);
        let initial_measurement_pending = !self.transcript_provisional_rows.borrow().is_empty()
            || !self.transcript_exact_measurement_rows.borrow().is_empty();
        let scrollbar_handle = StableListScrollbarHandle::new(
            &transcript_rows,
            &self.transcript_estimated_height,
            &self.transcript_anchor_end_space,
            &self.transcript_anchor_following,
            &self.transcript_drag_estimated_height,
            &self.transcript_is_scrolled,
            initial_measurement_pending,
        );
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .relative()
            .child(
                list(transcript_rows, move |index, _window, cx| {
                    entity
                        .upgrade()
                        .map(|entity| {
                            entity.update(cx, |this, cx| {
                                this.transcript_row(index, transcript_viewport, cx)
                            })
                        })
                        .unwrap_or_else(|| div().into_any_element())
                })
                .size_full()
                .pb(anchor_end_space),
            )
            .vertical_scrollbar(&scrollbar_handle)
            .into_any_element()
    }

    /// The provider's latest ordered block is still reasoning.
    pub(super) fn reasoning_live(&self) -> bool {
        self.selected_runtime()
            .is_some_and(|runtime| runtime.stream_phase == Some(StreamPhase::Reasoning))
            && self
                .selected_session()
                .is_some_and(|session| session.status == SessionStatus::Working)
    }

    pub(super) fn toggle_reasoning(
        &mut self,
        block_index: usize,
        current: bool,
        cx: &mut Context<Self>,
    ) {
        self.pin_transcript_for_disclosure();
        self.reasoning_expanded.insert(block_index, !current);
        self.remeasure_transcript_block(block_index);
        cx.notify();
    }

    pub(super) fn toggle_activities(
        &mut self,
        block_index: usize,
        current: bool,
        cx: &mut Context<Self>,
    ) {
        self.pin_transcript_for_disclosure();
        self.activities_expanded.insert(block_index, !current);
        self.remeasure_transcript_block(block_index);
        cx.notify();
    }

    pub(super) fn toggle_activity_item(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.pin_transcript_for_disclosure();
        if !self.expanded_activity_items.remove(&id) {
            self.expanded_activity_items.insert(id);
        }
        if let Some(block_index) = self.selected_transcript_blocks().iter().position(|block| {
            matches!(
                &block.content,
                TranscriptBlockContent::Activities(activities)
                    if activities.iter().any(|activity| activity.id == id)
            )
        }) {
            self.remeasure_transcript_block(block_index);
        }
        cx.notify();
    }

    pub(super) fn toggle_turn_fold(
        &mut self,
        turn_id: Uuid,
        expanded: bool,
        cx: &mut Context<Self>,
    ) {
        self.pin_transcript_for_disclosure();
        let scroll_top = self.active_transcript_rows().logical_scroll_top();
        let previous_kinds = self.transcript_row_kinds.borrow().clone();
        let anchor_kind = previous_kinds.get(scroll_top.item_ix).copied();
        if expanded {
            self.expanded_turns.remove(&turn_id);
        } else {
            self.expanded_turns.insert(turn_id);
        }
        self.transcript_anchor_following.set(false);
        self.splice_transcript_rows_after_visibility_change(&previous_kinds);

        let next_kinds = self.transcript_row_kinds.borrow();
        let anchored_target =
            anchor_kind.and_then(|kind| next_kinds.iter().position(|candidate| *candidate == kind));
        let target = anchored_target.or_else(|| {
            next_kinds
                .iter()
                .position(|kind| *kind == TranscriptRowKind::TurnFold(turn_id))
        });
        drop(next_kinds);
        if let Some(item_ix) = target {
            self.active_transcript_rows().scroll_to(ListOffset {
                item_ix,
                offset_in_item: if anchored_target.is_some() {
                    scroll_top.offset_in_item
                } else {
                    Pixels::ZERO
                },
            });
            self.transcript_is_scrolled.set(true);
        }
        cx.notify();
    }

    /// A single transcript row, self-centered to the content column so the
    /// list can measure it at its true wrap width. Current-turn reasoning and
    /// activity blocks are anchored at the exact boundary between assistant
    /// text segments where their provider events arrived.
    pub(super) fn user_message_action_for_message(
        &self,
        message_index: usize,
    ) -> Option<UserMessageAction> {
        let session = self.selected_session()?;
        let message = session.messages.get(message_index)?;
        if message.role != MessageRole::User
            || !matches!(session.status, SessionStatus::Idle | SessionStatus::Failed)
        {
            return None;
        }
        let turn_id = message.turn_id?;
        let turn = session.turns.iter().find(|turn| turn.id == turn_id)?;
        if !session.provider.supports_conversation_rollback() {
            return None;
        }
        let retained_turn_count = turn.turn_count.saturating_sub(1);
        let project_path = self
            .state
            .projects
            .iter()
            .find(|project| project.id == session.project_id)
            .map(|project| project.path.as_path())?;
        if !checkpoint::has_ref(
            project_path,
            &checkpoint::checkpoint_ref(session.id, retained_turn_count),
        ) {
            return None;
        }
        let rollback_turns = session.provider_turns_after(retained_turn_count);
        if rollback_turns > 0 && session.provider_cursor.is_none() {
            return None;
        }
        Some(UserMessageAction {
            session_id: session.id,
            turn_count: turn.turn_count,
        })
    }

    pub(super) fn transcript_row(
        &mut self,
        index: usize,
        transcript_viewport: TextViewScrollViewport,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.transcript_provisional_rows.borrow_mut().remove(&index) {
            // Keep the scrollbar suppressed for the pass that replaces this
            // estimated-height row with its exact content. The following
            // render can then trust ListState's measured scroll range.
            self.transcript_exact_measurement_rows
                .borrow_mut()
                .insert(index);
            cx.notify();
            let estimated_height = self
                .transcript_row_estimates
                .borrow()
                .get(index)
                .copied()
                .unwrap_or(px(44.0));
            return div()
                .w_full()
                .h(estimated_height)
                .flex_none()
                .into_any_element();
        }
        if self
            .transcript_exact_measurement_rows
            .borrow_mut()
            .remove(&index)
        {
            // This render replaces the provisional element. Schedule one more
            // pass so the anchor reservation reads the exact post-layout row
            // bounds instead of leaving the estimate in place indefinitely.
            cx.notify();
        }

        let theme = Theme::current(cx);
        let composer = self.composer.clone();
        let waku = cx.entity().downgrade();
        let row_count = self.transcript_row_count();
        let kind = self
            .transcript_row_kinds
            .borrow()
            .get(index)
            .copied()
            .unwrap_or(TranscriptRowKind::Message(index));
        let starts_followup_turn = match kind {
            TranscriptRowKind::Message(message_index) => {
                self.selected_session().is_some_and(|session| {
                    message_starts_followup_turn(&session.messages, message_index)
                })
            }
            TranscriptRowKind::TurnBlock(_) | TranscriptRowKind::TurnFold(_) => false,
        };
        let inner = match kind {
            TranscriptRowKind::Message(message_index) => self
                .selected_session()
                .and_then(|session| session.messages.get(message_index))
                .cloned()
                .map(|message| {
                    let copied = self.copied_message_feedback.contains_key(&message.id);
                    let assistant_footer_copy_content = self
                        .selected_session()
                        .and_then(|session| assistant_response_footer(session, message_index));
                    let user_message_action = self.user_message_action_for_message(message_index);
                    let message_edit_input = user_message_action.and_then(|action| {
                        self.message_edit
                            .as_ref()
                            .filter(|edit| {
                                edit.session_id == action.session_id
                                    && edit.turn_count == action.turn_count
                            })
                            .map(|edit| edit.input.clone())
                    });
                    let text_state = self
                        .message_text_states
                        .entry(message.id)
                        .or_insert_with(|| cx.new(TextViewState::new))
                        .clone();
                    render_message(
                        &theme,
                        &message,
                        assistant_footer_copy_content,
                        copied,
                        user_message_action,
                        message_edit_input,
                        self.state.selected_session.unwrap_or_default(),
                        self.transcript_resize_tx.clone(),
                        self.transcript_layout_width.get(),
                        self.active_transcript_rows().clone(),
                        transcript_viewport,
                        text_state,
                        waku,
                        composer,
                        cx,
                    )
                })
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
            TranscriptRowKind::TurnFold(turn_id) => self.render_turn_fold_row(turn_id, &theme, cx),
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

    /// Settled reasoning, tool activity, and interim assistant commentary are
    /// folded into a compact divider while the terminal response stays visible.
    pub(super) fn render_turn_fold_row(
        &self,
        turn_id: Uuid,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let expanded = self.expanded_turns.contains(&turn_id);
        let label = self
            .selected_session()
            .map(|session| turn_fold_label(session, turn_id))
            .unwrap_or_else(|| "Worked".into());
        div()
            .w_full()
            .h(px(24.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(div().h(px(1.0)).flex_1().bg(theme.border))
            .child(
                div()
                    .id(SharedString::from(format!("turn-fold-{turn_id}")))
                    .h(px(24.0))
                    .px(px(2.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .cursor_default()
                    .text_size(px(11.5))
                    .line_height(px(16.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text_tertiary)
                    .child(SharedString::from(label))
                    .child(icon(
                        if expanded {
                            "icons/chevron-down.svg"
                        } else {
                            "icons/chevron-right.svg"
                        },
                        10.0,
                        theme.text_tertiary,
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_turn_fold(turn_id, expanded, cx);
                    })),
            )
            .child(div().h(px(1.0)).flex_1().bg(theme.border))
            .into_any_element()
    }

    /// The turn's reasoning as a disclosure: open while the provider is
    /// thinking, collapsing to "Thought for Ns" once the answer starts, and
    /// clickable either way.
    pub(super) fn render_reasoning_row(
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
    pub(super) fn render_activities_row(
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
}
