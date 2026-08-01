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

pub(super) fn format_message_time(created_at: u64) -> String {
    let Ok(seconds) = i64::try_from(created_at) else {
        return String::new();
    };
    DateTime::<Utc>::from_timestamp(seconds, 0)
        .map(|timestamp| {
            timestamp
                .with_timezone(&Local)
                .format("%I:%M %p")
                .to_string()
                .trim_start_matches('0')
                .to_owned()
        })
        .unwrap_or_default()
}

fn render_message_footer(
    theme: &Theme,
    message: &Message,
    group_name: SharedString,
    align_right: bool,
    user_message_action: Option<UserMessageAction>,
    waku: gpui::WeakEntity<Waku>,
) -> AnyElement {
    let theme = *theme;
    let message_id = message.id;
    let copy_content = message.content.clone();
    let mut footer = div()
        .w_full()
        .h(px(27.0))
        .flex()
        .items_center()
        .gap(px(1.0))
        .invisible()
        .group_hover(group_name, |element| element.visible())
        .when(align_right, |element| element.justify_end())
        .child(
            div()
                .h(px(27.0))
                .px(px(4.0))
                .flex()
                .items_center()
                .text_size(px(11.5))
                .line_height(px(14.0))
                .text_color(theme.text_ghost)
                .child(format_message_time(message.created_at)),
        )
        .child(
            div()
                .id(SharedString::from(format!("copy-message-{message_id}")))
                .w(px(27.0))
                .h(px(27.0))
                .rounded(px(8.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_default()
                .hover(|element| element.bg(theme.overlay_strong))
                .child(icon("icons/copy.svg", 14.0, theme.text_secondary))
                .tooltip(|window, cx| Tooltip::new("Copy message").build(window, cx))
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy_content.clone()));
                }),
        );

    if let Some(action) = user_message_action {
        let edit_waku = waku;
        footer = footer.child(
            div()
                .id(SharedString::from(format!(
                    "user-message-action-{message_id}"
                )))
                .w(px(27.0))
                .h(px(27.0))
                .rounded(px(8.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_default()
                .hover(|element| element.bg(theme.overlay_strong))
                .child(icon("icons/rewind.svg", 14.0, theme.text_secondary))
                .tooltip(|window, cx| Tooltip::new("Revert to here").build(window, cx))
                .on_click(move |_, window, cx| {
                    let _ = edit_waku.update(cx, |this, cx| {
                        this.begin_message_edit(action.session_id, action.turn_count, window, cx);
                    });
                }),
        );
    }

    footer.into_any_element()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_message(
    theme: &Theme,
    message: &Message,
    user_message_action: Option<UserMessageAction>,
    message_edit_input: Option<Entity<ComposerInput>>,
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
        MessageRole::User => {
            let group_name = SharedString::from(format!("user-message-{message_id}"));
            let mut column = div()
                .w_full()
                .flex()
                .flex_col()
                .items_end()
                .gap(px(3.0))
                .group(group_name.clone());
            if let Some(edit_input) = message_edit_input {
                let can_submit = !edit_input.read(cx).content().trim().is_empty();
                let cancel_waku = waku.clone();
                let submit_waku = waku.clone();
                column = column.child(
                    div()
                        .w_full()
                        .max_w(px(540.0))
                        .rounded(px(12.0))
                        .bg(theme.raised)
                        .px(px(12.0))
                        .pt(px(9.0))
                        .pb(px(8.0))
                        .child(edit_input)
                        .child(
                            div()
                                .mt(px(7.0))
                                .flex()
                                .justify_end()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "cancel-message-edit-{message_id}"
                                        )))
                                        .h(px(26.0))
                                        .px(px(10.0))
                                        .rounded(px(7.0))
                                        .border_1()
                                        .border_color(theme.border)
                                        .bg(theme.overlay)
                                        .flex()
                                        .items_center()
                                        .text_size(px(11.5))
                                        .text_color(theme.text_secondary)
                                        .cursor_default()
                                        .hover(|element| element.bg(theme.overlay_strong))
                                        .child("Cancel")
                                        .on_click(move |_, window, cx| {
                                            let _ = cancel_waku.update(cx, |this, cx| {
                                                this.cancel_message_edit(window, cx);
                                            });
                                        }),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "submit-message-edit-{message_id}"
                                        )))
                                        .h(px(26.0))
                                        .px(px(11.0))
                                        .rounded(px(7.0))
                                        .bg(if can_submit {
                                            theme.inverse
                                        } else {
                                            theme.overlay_strong
                                        })
                                        .flex()
                                        .items_center()
                                        .text_size(px(11.5))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(if can_submit {
                                            theme.on_inverse
                                        } else {
                                            theme.text_ghost
                                        })
                                        .when(can_submit, |element| {
                                            element
                                                .cursor_default()
                                                .hover(|element| element.opacity(0.9))
                                        })
                                        .child("Send")
                                        .on_click(move |_, _, cx| {
                                            if can_submit {
                                                let _ = submit_waku.update(cx, |this, cx| {
                                                    this.submit_message_edit(cx);
                                                });
                                            }
                                        }),
                                ),
                        ),
                );
            } else {
                column = column.child(
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
                );
                column = column.child(render_message_footer(
                    theme,
                    message,
                    group_name,
                    true,
                    user_message_action,
                    waku.clone(),
                ));
            }
            column
        }
        MessageRole::Assistant => {
            let group_name = SharedString::from(format!("assistant-message-{message_id}"));
            let resize_tx = transcript_resize_tx.clone();
            let resize_waku = waku.clone();
            let mut column = div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .py(px(4.0))
                .gap(px(3.0))
                .group(group_name.clone())
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
            column = column.child(render_message_footer(
                theme,
                message,
                group_name,
                false,
                None,
                waku.clone(),
            ));
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

                if role == MessageRole::User && user_message_action.is_none() {
                    let composer = composer.clone();
                    let edit_content = menu_content.clone();
                    menu = menu.item(PopupMenuItem::new("Copy to Composer").on_click(
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

                if let Some(action) = user_message_action {
                    let action_waku = waku.clone();
                    menu = menu.item(PopupMenuItem::new("Revert to Here").on_click(
                        move |_, window, cx| {
                            let _ = action_waku.update(cx, |this, cx| {
                                this.begin_message_edit(
                                    action.session_id,
                                    action.turn_count,
                                    window,
                                    cx,
                                );
                            });
                        },
                    ));
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
