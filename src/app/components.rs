use super::*;

pub(super) fn pulse_dot(id: impl Into<SharedString>, size: f32, color: Hsla) -> AnyElement {
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

#[allow(clippy::too_many_arguments)]
pub(super) fn render_message(
    theme: &Theme,
    message: &Message,
    checkpoint_action: Option<CheckpointAction>,
    session_id: Uuid,
    transcript_resize_tx: crossbeam_channel::Sender<TranscriptMarkdownResize>,
    transcript_layout_width: Pixels,
    transcript_rows: ListState,
    transcript_viewport: TextViewScrollViewport,
    text_state: Entity<TextViewState>,
    waku: gpui::WeakEntity<Waku>,
    composer: Entity<ComposerInput>,
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
                .child(
                    selectable_plain_text(
                        SharedString::from(format!("message-{message_id}-user")),
                        &content,
                        text_state,
                        cx,
                    )
                    .selection_scroll_handle(&transcript_rows)
                    .block_viewport(transcript_viewport),
                ),
        ),
        MessageRole::Assistant => {
            let resize_tx = transcript_resize_tx.clone();
            let resize_waku = waku.clone();
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
                    TextView::markdown_with_state(
                        SharedString::from(format!("message-{message_id}-assistant")),
                        content,
                        text_state,
                        cx,
                    )
                    .update_delay(STREAM_MARKDOWN_DELAY)
                    .style(assistant_markdown_style(theme))
                    .selectable(true)
                    .selection_scroll_handle(&transcript_rows)
                    .block_viewport(transcript_viewport)
                    .block_layout_width(transcript_layout_width)
                    .on_block_resize(move |resize: TextViewBlockResize, cx| {
                        let _ = resize_tx.send(TranscriptMarkdownResize {
                            session_id,
                            message_id,
                            delta: resize.delta,
                            anchor_delta: if resize.above_viewport {
                                resize.delta
                            } else {
                                Pixels::ZERO
                            },
                        });
                        if let Some(waku) = resize_waku.upgrade() {
                            cx.notify(waku.entity_id());
                        }
                    })
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
            if let Some(action) = checkpoint_action {
                let checkpoint_label = if action.file_count == 0 {
                    format!("Checkpoint {} · no file changes", action.turn_count)
                } else {
                    format!(
                        "Checkpoint {} · {} file(s)",
                        action.turn_count, action.file_count
                    )
                };
                let weak = waku.clone();
                column = column.child(
                    div()
                        .mt(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .text_size(px(10.5))
                        .line_height(px(16.0))
                        .text_color(theme.text_tertiary)
                        .child(icon("icons/check.svg", 10.0, theme.text_tertiary))
                        .child(SharedString::from(checkpoint_label))
                        .when(action.can_revert, |element| {
                            element.child(
                                div()
                                    .id(SharedString::from(format!(
                                        "revert-checkpoint-{}-{}",
                                        action.session_id, action.turn_count
                                    )))
                                    .ml(px(2.0))
                                    .px(px(7.0))
                                    .py(px(2.0))
                                    .rounded(px(5.0))
                                    .cursor_default()
                                    .text_color(if action.confirmed {
                                        theme.danger
                                    } else {
                                        theme.text_secondary
                                    })
                                    .bg(if action.confirmed {
                                        theme.danger_soft
                                    } else {
                                        theme.overlay
                                    })
                                    .hover(|element| element.bg(theme.overlay_strong))
                                    .child(if action.confirmed {
                                        "Confirm revert"
                                    } else {
                                        "Revert"
                                    })
                                    .on_click(move |_, _, cx| {
                                        let _ = weak.update(cx, |this, cx| {
                                            this.request_checkpoint_revert(
                                                action.session_id,
                                                action.turn_count,
                                                cx,
                                            );
                                        });
                                    }),
                            )
                        }),
                );
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
                .child(
                    selectable_plain_text(
                        SharedString::from(format!("message-{message_id}-system")),
                        &content,
                        text_state,
                        cx,
                    )
                    .selection_scroll_handle(&transcript_rows)
                    .block_viewport(transcript_viewport),
                ),
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

                if let Some(action) = checkpoint_action.filter(|action| action.can_revert) {
                    let weak = waku.clone();
                    menu = menu.item(
                        PopupMenuItem::new(if action.confirmed {
                            "Confirm Revert to Checkpoint"
                        } else {
                            "Revert to Checkpoint"
                        })
                        .on_click(move |_, _, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.request_checkpoint_revert(
                                    action.session_id,
                                    action.turn_count,
                                    cx,
                                );
                            });
                        }),
                    );
                }

                menu
            },
        )
        .into_any_element()
}

pub(super) fn assistant_markdown_style(theme: &Theme) -> TextViewStyle {
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

pub(super) fn selectable_plain_text(
    id: impl Into<gpui::ElementId>,
    content: &str,
    state: Entity<TextViewState>,
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
    TextView::html_with_state(id, html, state, cx)
        .style(TextViewStyle::default().paragraph_gap(rems(0.0)))
        .selectable(true)
        .w_full()
        .cursor_text()
}

pub(super) fn escape_html(content: &str) -> String {
    content
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub(super) fn fenced_code(content: &str) -> Option<String> {
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

pub(super) fn activity_summary(activities: &[ActivityItem]) -> String {
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

pub(super) fn git_branch(path: &std::path::Path) -> Option<String> {
    let output = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(path)
        .output()
        .ok()?;
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!branch.is_empty()).then_some(branch)
}
