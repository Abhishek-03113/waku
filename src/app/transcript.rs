use super::*;

impl Waku {
    /// One list row per message plus each ordered non-message turn block.
    pub(super) fn transcript_row_count(&self) -> usize {
        self.selected_transcript_row_kinds().len()
    }

    pub(super) fn selected_transcript_row_kinds(&self) -> Vec<TranscriptRowKind> {
        self.selected_session().map_or_else(Vec::new, |session| {
            folded_transcript_row_kinds(session, &self.expanded_turns)
        })
    }

    pub(super) fn active_transcript_rows(&self) -> &ListState {
        if self.transcript_anchor.get().is_some() {
            &self.anchored_transcript_rows
        } else {
            &self.transcript_rows
        }
    }

    /// Turn a tail-pinned list into an explicit scroll position before a
    /// disclosure changes the document height. Otherwise GPUI keeps the
    /// bottom edge fixed and makes the disclosure header jump upward while
    /// its newly visible content is inserted.
    pub(super) fn pin_transcript_for_disclosure(&self) {
        self.sync_transcript_rows();
        let transcript_rows = self.active_transcript_rows();
        let count = transcript_rows.item_count();
        let scroll_top = transcript_rows.logical_scroll_top();

        if scroll_top.item_ix >= count && count > 0 {
            let viewport_height = transcript_rows.viewport_bounds().size.height;
            let actual_max = transcript_rows.max_offset_for_scrollbar().y;
            if actual_max > px(0.5) {
                // GPUI represents the exact bottom as an implicit tail anchor.
                // Resolve the corresponding item just above the bottom, then
                // restore the final half pixel with scroll_to so the same
                // position remains explicit while rows below it grow.
                transcript_rows
                    .set_offset_from_scrollbar(point(Pixels::ZERO, -(actual_max - px(0.5))));
                let mut explicit_bottom = transcript_rows.logical_scroll_top();
                explicit_bottom.offset_in_item += px(0.5);
                transcript_rows.scroll_to(explicit_bottom);
            } else if viewport_height > Pixels::ZERO {
                // A short bottom-aligned transcript has leading empty space.
                // A negative item offset preserves that space so expanding a
                // row still grows downward from its current screen position.
                // `scroll_px_offset_for_scrollbar` is zero for a short list in
                // Zed's GPUI, so derive the actual content height from its
                // rendered row bounds instead of treating the list as empty.
                //
                // Only when those bounds actually exist. Rows that have not
                // been measured yet report `None`, and treating that as a
                // zero-height document asks for a leading space of the whole
                // viewport — which pushes every row off screen and leaves the
                // transcript blank until the reader scrolls it back.
                let measured_content_height = transcript_rows
                    .bounds_for_item(0)
                    .zip(transcript_rows.bounds_for_item(count - 1))
                    .map(|(first, last)| (last.bottom() - first.top()).max(Pixels::ZERO));
                if let Some(leading_space) =
                    disclosure_leading_space(viewport_height, measured_content_height)
                {
                    transcript_rows.scroll_to(ListOffset {
                        item_ix: 0,
                        offset_in_item: -leading_space,
                    });
                }
            }
        }

        self.transcript_anchor_following.set(false);
        self.transcript_is_scrolled.set(true);
    }

    /// Bulk-reset the transcript. Used for session/document replacement.
    pub(super) fn reset_transcript_rows(&self, count: usize) {
        self.transcript_is_scrolled.set(false);
        self.transcript_rows.reset(count);
        self.anchored_transcript_rows.reset(count);
    }

    /// Apply a local disclosure change without replacing unchanged transcript
    /// rows.
    pub(super) fn splice_transcript_rows_after_visibility_change(
        &self,
        previous_kinds: &[TranscriptRowKind],
    ) {
        let next_kinds = self.selected_transcript_row_kinds();
        let splice = transcript_row_splice(previous_kinds, &next_kinds);
        *self.transcript_row_kinds.borrow_mut() = next_kinds;
        self.splice_transcript_rows(splice);
    }

    pub(super) fn selected_transcript_anchor_row(&self) -> Option<usize> {
        let anchor = self.transcript_anchor.get()?;
        let session = self.selected_session()?;
        if session.id != anchor.session_id {
            return None;
        }
        let message_index = session.messages.iter().position(|message| {
            message.role == MessageRole::User && message.turn_id == Some(anchor.turn_id)
        })?;
        self.transcript_row_kinds
            .borrow()
            .iter()
            .position(|kind| *kind == TranscriptRowKind::Message(message_index))
    }

    pub(super) fn scroll_transcript_to_anchor(&self) {
        let Some(item_ix) = self.selected_transcript_anchor_row() else {
            return;
        };
        self.active_transcript_rows().scroll_to(ListOffset {
            item_ix,
            offset_in_item: Pixels::ZERO,
        });
        self.transcript_is_scrolled.set(true);
    }

    pub(super) fn update_transcript_anchor_end_space(&self, window: &Window) -> Pixels {
        let Some(anchor_row) = self.selected_transcript_anchor_row() else {
            self.transcript_anchor_end_space.set(Pixels::ZERO);
            self.transcript_anchor_following.set(false);
            return Pixels::ZERO;
        };

        let viewport_height = {
            let measured = self.active_transcript_rows().viewport_bounds().size.height;
            if measured > Pixels::ZERO {
                measured
            } else {
                // The first sent message replaces the empty state, so the list
                // has no prior bounds yet. The full window is a conservative
                // first-frame fallback that still guarantees a top anchor.
                window.viewport_size().height
            }
        };
        let transcript_rows = self.active_transcript_rows();
        let last_row = transcript_rows.item_count().checked_sub(1);
        let anchored_tail_height = last_row
            .and_then(|last_row| {
                let anchor = transcript_rows.bounds_for_item(anchor_row)?;
                let last = transcript_rows.bounds_for_item(last_row)?;
                Some((last.bottom() - anchor.top()).max(Pixels::ZERO))
            })
            .unwrap_or_default();
        let end_space = transcript_anchor_end_space(viewport_height, anchored_tail_height);
        self.transcript_anchor_end_space.set(end_space);
        if maintain_transcript_anchor(
            transcript_rows,
            anchor_row,
            self.transcript_anchor_following.get(),
            end_space,
        ) {
            self.transcript_is_scrolled.set(true);
        }
        end_space
    }

    /// Re-render rows whose content changed in place so GPUI re-measures them.
    pub(super) fn splice_transcript_rows(&self, splice: Option<(Range<usize>, usize)>) {
        let Some((range, new_count)) = splice else {
            return;
        };
        self.transcript_rows.splice(range.clone(), new_count);
        if self.transcript_anchor.get().is_some() {
            self.anchored_transcript_rows.splice(range, new_count);
        }
    }

    pub(super) fn sync_transcript_layout_width(&self, window: &Window) -> bool {
        let (sidebar_width, right_panel_width) = self.effective_panel_widths(window);
        let sidebar_width = px(sidebar_width);
        let right_panel_width = px(right_panel_width);
        let content_width =
            (window.viewport_size().width - sidebar_width - right_panel_width - px(40.0))
                .clamp(px(1.0), px(CONTENT_MAX_WIDTH));
        let previous = self.transcript_layout_width.replace(content_width);
        if previous > Pixels::ZERO && (previous - content_width).abs() < px(1.0) {
            return false;
        }

        // Reflow every row at the new wrap width.
        let count = self.active_transcript_rows().item_count();
        self.splice_transcript_rows(Some((0..count, count)));
        true
    }

    /// Keep the list's row count *and its row kinds* in sync with the
    /// transcript.
    ///
    /// The kinds cache is what tells `transcript_row` whether row `n` is a
    /// message, a reasoning block, a tool-activity cluster or a turn fold.
    /// Leaving it stale makes every row fall back to `Message(n)`, which
    /// silently drops all reasoning and activity from the transcript.
    ///
    /// Appends keep the reader's place (or the pinned tail); shrinking resets
    /// the view.
    pub(super) fn sync_transcript_rows(&self) {
        let next_kinds = self.selected_transcript_row_kinds();
        let count = next_kinds.len();
        *self.transcript_row_kinds.borrow_mut() = next_kinds;

        let transcript_rows = self.active_transcript_rows();
        let current = transcript_rows.item_count();
        if count > current {
            transcript_rows.splice(current..current, count - current);
        } else if count < current {
            self.reset_transcript_rows(count);
        }
    }

    pub(super) fn remeasure_transcript_tail(&self) {
        self.sync_transcript_rows();
        let count = self.active_transcript_rows().item_count();
        let from = count.saturating_sub(STREAM_REMEASURE_TAIL_ROWS);
        if from < count {
            self.splice_transcript_rows(Some((from..count, count - from)));
        }
    }

    pub(super) fn remeasure_transcript_block(&self, block_index: usize) {
        self.sync_transcript_rows();
        let splice = self
            .transcript_row_kinds
            .borrow()
            .iter()
            .position(|kind| *kind == TranscriptRowKind::TurnBlock(block_index))
            .map(|row_index| (row_index..row_index + 1, 1));
        self.splice_transcript_rows(splice);
    }

    pub(super) fn remeasure_transcript_message(&self, message_index: usize) {
        self.sync_transcript_rows();
        let splice = self
            .transcript_row_kinds
            .borrow()
            .iter()
            .position(|kind| *kind == TranscriptRowKind::Message(message_index))
            .map(|row_index| (row_index..row_index + 1, 1));
        self.splice_transcript_rows(splice);
    }
}

// ── Shared pieces ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum TranscriptRowKind {
    Message(usize),
    TurnBlock(usize),
    TurnFold(Uuid),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TranscriptNavigationTurn {
    pub message_id: Uuid,
    pub message_index: usize,
    pub row_index: usize,
    pub prompt: String,
    pub response: String,
}

pub(super) fn transcript_navigation_turns(
    session: &AgentSession,
    row_kinds: &[TranscriptRowKind],
) -> Vec<TranscriptNavigationTurn> {
    let user_message_indexes = session
        .messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| (message.role == MessageRole::User).then_some(index))
        .collect::<Vec<_>>();

    user_message_indexes
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(turn_index, message_index)| {
            let message = session.messages.get(message_index)?;
            let row_index = row_kinds
                .iter()
                .position(|kind| *kind == TranscriptRowKind::Message(message_index))?;
            let next_user_index = user_message_indexes
                .get(turn_index + 1)
                .copied()
                .unwrap_or(session.messages.len());
            let turn_running = message.turn_id.is_some_and(|turn_id| {
                session
                    .turns
                    .iter()
                    .any(|turn| turn.id == turn_id && turn.status == TurnStatus::Running)
            });
            let response = (!turn_running)
                .then(|| {
                    session.messages[message_index + 1..next_user_index]
                        .iter()
                        .rev()
                        .find(|candidate| {
                            candidate.role == MessageRole::Assistant
                                && !candidate.content.trim().is_empty()
                        })
                        .map(|candidate| navigation_preview_snippet(&candidate.content, 240))
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            Some(TranscriptNavigationTurn {
                message_id: message.id,
                message_index,
                row_index,
                prompt: navigation_preview_snippet(&message.content, 100),
                response,
            })
        })
        .collect()
}

pub(super) fn navigation_preview_snippet(content: &str, max_graphemes: usize) -> String {
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut graphemes = normalized.graphemes(true);
    let snippet = graphemes.by_ref().take(max_graphemes).collect::<String>();
    if graphemes.next().is_some() {
        format!("{snippet}…")
    } else {
        snippet
    }
}

pub(super) fn active_navigation_turn_index(
    turn_rows: &[usize],
    scroll_top_row: usize,
    at_transcript_end: bool,
) -> Option<usize> {
    if turn_rows.is_empty() {
        return None;
    }
    if at_transcript_end {
        return Some(turn_rows.len() - 1);
    }
    Some(
        turn_rows
            .partition_point(|row| *row <= scroll_top_row)
            .saturating_sub(1),
    )
}

pub(super) fn navigation_rail_scale(
    turn_index: usize,
    active_turn_index: Option<usize>,
    emphasized_turn_index: Option<usize>,
) -> f32 {
    let active_scale: f32 = if active_turn_index == Some(turn_index) {
        0.50
    } else {
        0.25
    };
    let emphasis_scale =
        emphasized_turn_index.map_or(0.25, |emphasized| match turn_index.abs_diff(emphasized) {
            0 => 1.0,
            1 => 0.68,
            2 => 0.44,
            _ => 0.25,
        });
    active_scale.max(emphasis_scale)
}

pub(super) fn navigation_rail_height(turn_count: usize, viewport_height: f32) -> f32 {
    (turn_count as f32 * NAVIGATION_RAIL_TURN_HEIGHT)
        .min(viewport_height * NAVIGATION_RAIL_VIEWPORT_HEIGHT_RATIO)
}

pub(super) fn should_show_navigation_rail(
    transcript_scrollable: bool,
    turn_count: usize,
    chat_viewport_width: f32,
) -> bool {
    let content_left = ((chat_viewport_width - CONTENT_MAX_WIDTH) / 2.0).max(20.0);
    let rail_right = NAVIGATION_RAIL_LEFT + NAVIGATION_RAIL_WIDTH;
    transcript_scrollable
        && turn_count >= 2
        && content_left >= rail_right + NAVIGATION_RAIL_CONTENT_GAP
}

/// A provider can split one assistant response into several ordered text
/// messages around reasoning and tool activity. The response footer belongs
/// only to the terminal text part, once the turn has settled.
pub(super) fn assistant_response_footer_index(
    session: &AgentSession,
    message_index: usize,
) -> Option<usize> {
    let message = session.messages.get(message_index)?;
    if message.role != MessageRole::Assistant || message.streaming {
        return None;
    }
    let Some(turn_id) = message.turn_id else {
        return Some(message_index);
    };
    if session
        .turns
        .iter()
        .find(|turn| turn.id == turn_id)
        .is_some_and(|turn| turn.status == TurnStatus::Running)
    {
        return None;
    }
    session.messages.iter().rposition(|candidate| {
        candidate.role == MessageRole::Assistant && candidate.turn_id == Some(turn_id)
    })
}

pub(super) fn assistant_response_footer(
    session: &AgentSession,
    message_index: usize,
) -> Option<String> {
    if assistant_response_footer_index(session, message_index) != Some(message_index) {
        return None;
    }
    let message = &session.messages[message_index];
    let Some(turn_id) = message.turn_id else {
        return Some(message.content.clone());
    };
    Some(
        session
            .messages
            .iter()
            .filter(|candidate| {
                candidate.role == MessageRole::Assistant
                    && candidate.turn_id == Some(turn_id)
                    && !candidate.content.trim().is_empty()
            })
            .map(|candidate| candidate.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

pub(super) fn assistant_response_footer_time(
    session: &AgentSession,
    message_index: usize,
) -> Option<u64> {
    if assistant_response_footer_index(session, message_index) != Some(message_index) {
        return None;
    }
    let message = &session.messages[message_index];
    let completed_at = message.turn_id.and_then(|turn_id| {
        session
            .turns
            .iter()
            .find(|turn| turn.id == turn_id)
            .and_then(|turn| turn.completed_at)
    });
    Some(completed_at.unwrap_or(message.created_at))
}

pub(super) fn transcript_row_splice(
    previous: &[TranscriptRowKind],
    next: &[TranscriptRowKind],
) -> Option<(Range<usize>, usize)> {
    let prefix = previous
        .iter()
        .zip(next)
        .take_while(|(left, right)| left == right)
        .count();
    let suffix = previous[prefix..]
        .iter()
        .rev()
        .zip(next[prefix..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let old_end = previous.len() - suffix;
    let new_count = next.len() - prefix - suffix;

    (prefix != old_end || new_count != 0).then_some((prefix..old_end, new_count))
}

/// Leading space that keeps a short bottom-aligned transcript pinned to the
/// bottom of its viewport, or `None` when the content has not been measured
/// yet and no scroll should be forced.
pub(super) fn disclosure_leading_space(
    viewport_height: Pixels,
    measured_content_height: Option<Pixels>,
) -> Option<Pixels> {
    let measured = measured_content_height?;
    Some((viewport_height - measured).max(Pixels::ZERO))
}

pub(super) fn transcript_anchor_end_space(
    viewport_height: Pixels,
    anchored_tail_height: Pixels,
) -> Pixels {
    (viewport_height - anchored_tail_height).max(Pixels::ZERO)
}

pub(super) fn maintain_transcript_anchor(
    transcript_rows: &ListState,
    anchor_row: usize,
    anchor_following: bool,
    end_space: Pixels,
) -> bool {
    if !anchor_following || end_space <= Pixels::ZERO {
        return false;
    }

    // A bottom-aligned GPUI list represents its pinned tail as no logical
    // scroll offset. While a response row is being remeasured, the retained
    // end spacer and the newly expanded content briefly overflow together;
    // without an explicit item offset that overflow is taken from the top of
    // the user row. Reassert the turn anchor in the same layout pass.
    transcript_rows.scroll_to(ListOffset {
        item_ix: anchor_row,
        offset_in_item: Pixels::ZERO,
    });
    true
}

pub(super) const ACTIVITY_IMAGE_WIDTH: f32 = 300.0;
pub(super) const ACTIVITY_IMAGE_HEIGHT: f32 = 200.0;

/// Interleave live turn blocks at the exact message boundary where their
/// provider events arrived. `anchors[n] == 2` means block `n` renders after
/// messages 0 and 1, before message 2.
pub(super) fn transcript_row_kinds(
    message_count: usize,
    anchors: &[usize],
) -> Vec<TranscriptRowKind> {
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

/// A settled turn presents its terminal assistant message plus the one-line
/// summaries of what it did — "Thought for 12s", "Ran 3 tool calls" — each
/// expandable in place.
///
/// Only the turn's *interim assistant commentary* folds away, behind one work
/// summary row. Reasoning and tool activity stay visible: they are already
/// collapsed to a single line each, and hiding them behind a second fold left
/// a settled turn saying nothing about what the agent actually did. This
/// mirrors T3 Code, which keeps work entries inline in the timeline and
/// collapses only overflow.
pub(super) fn folded_transcript_row_kinds(
    session: &AgentSession,
    expanded_turns: &HashSet<Uuid>,
) -> Vec<TranscriptRowKind> {
    let anchors = session
        .transcript_blocks
        .iter()
        .map(|block| block.after_message)
        .collect::<Vec<_>>();
    let raw_rows = transcript_row_kinds(session.messages.len(), &anchors);
    let mut hidden_rows = HashSet::new();
    let mut fold_anchors = HashMap::new();

    for turn in &session.turns {
        if turn.status == TurnStatus::Running {
            continue;
        }
        let terminal_message = session.messages.iter().rposition(|message| {
            message.role == MessageRole::Assistant && message.turn_id == Some(turn.id)
        });
        let hidden = raw_rows
            .iter()
            .copied()
            .filter(|row| match *row {
                TranscriptRowKind::Message(message_index) => {
                    Some(message_index) != terminal_message
                        && session.messages.get(message_index).is_some_and(|message| {
                            message.role == MessageRole::Assistant
                                && message.turn_id == Some(turn.id)
                        })
                }
                // Reasoning and activity summarise themselves; they stay.
                TranscriptRowKind::TurnBlock(_) | TranscriptRowKind::TurnFold(_) => false,
            })
            .collect::<Vec<_>>();
        let Some(anchor) = hidden.first().copied() else {
            continue;
        };
        fold_anchors.insert(anchor, turn.id);
        hidden_rows.extend(hidden);
    }

    let mut rows = Vec::with_capacity(raw_rows.len() + fold_anchors.len());
    for row in raw_rows {
        if let Some(turn_id) = fold_anchors.get(&row).copied() {
            rows.push(TranscriptRowKind::TurnFold(turn_id));
        }
        let expanded =
            row_turn_id(session, row).is_some_and(|turn_id| expanded_turns.contains(&turn_id));
        if expanded || !hidden_rows.contains(&row) {
            rows.push(row);
        }
    }
    rows
}

fn row_turn_id(session: &AgentSession, row: TranscriptRowKind) -> Option<Uuid> {
    match row {
        TranscriptRowKind::Message(index) => session.messages.get(index)?.turn_id,
        TranscriptRowKind::TurnBlock(index) => session.transcript_blocks.get(index)?.turn_id,
        TranscriptRowKind::TurnFold(turn_id) => Some(turn_id),
    }
}

pub(super) fn turn_fold_label(session: &AgentSession, turn_id: Uuid) -> String {
    let Some(turn) = session.turns.iter().find(|turn| turn.id == turn_id) else {
        return "Worked".into();
    };
    let seconds = turn
        .completed_at
        .unwrap_or_else(unix_time)
        .saturating_sub(turn.started_at)
        .max(1);
    let duration = format_worked_duration(seconds);
    if turn.status == TurnStatus::Interrupted {
        format!("You stopped after {duration}")
    } else {
        format!("Worked for {duration}")
    }
}

pub(super) fn format_worked_duration(seconds: u64) -> String {
    fn unit(value: u64, singular: &str) -> String {
        format!("{value} {singular}{}", if value == 1 { "" } else { "s" })
    }

    match seconds {
        0..=59 => unit(seconds, "second"),
        60..=3599 => {
            let minutes = seconds / 60;
            let seconds = seconds % 60;
            if seconds == 0 {
                unit(minutes, "minute")
            } else {
                format!("{} {}", unit(minutes, "minute"), unit(seconds, "second"))
            }
        }
        _ => {
            let hours = seconds / 3600;
            let minutes = (seconds % 3600) / 60;
            if minutes == 0 {
                unit(hours, "hour")
            } else {
                format!("{} {}", unit(hours, "hour"), unit(minutes, "minute"))
            }
        }
    }
}

pub(super) fn message_starts_followup_turn(messages: &[Message], message_index: usize) -> bool {
    messages
        .get(message_index)
        .is_some_and(|message| message.role == MessageRole::User)
        && messages[..message_index]
            .iter()
            .any(|message| message.role == MessageRole::User)
}
