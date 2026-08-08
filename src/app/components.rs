use super::*;

use chrono::{Datelike, Days};

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

/// Three dots chasing a brightness wave, the transcript's "still working"
/// signal. Each dot runs the same repeating cycle with a phase offset, so the
/// bright spot travels left to right. Under reduce-motion GPUI holds the
/// cycle's first frame — the lead dot bright, the tail dim — which reads as a
/// static ellipsis.
pub(super) fn working_wave_dots(color: Hsla) -> AnyElement {
    const DOT_PHASE_STEP: f32 = 0.18;
    div()
        .flex()
        .items_center()
        .gap(px(3.5))
        .children((0..3).map(|index| {
            let phase_offset = index as f32 * DOT_PHASE_STEP;
            div()
                .size(px(4.5))
                .flex_none()
                .rounded_full()
                .bg(color)
                .with_animation(
                    SharedString::from(format!("working-wave-dot-{index}")),
                    Animation::new(Duration::from_millis(1400)).repeat(),
                    move |element, delta| {
                        let phase = (delta + 1.0 - phase_offset) % 1.0;
                        let wave = ((phase * std::f32::consts::TAU).sin() + 1.0) / 2.0;
                        element.opacity(0.25 + 0.75 * wave)
                    },
                )
        }))
        .into_any_element()
}

pub(super) fn format_message_time(created_at: u64) -> String {
    format_message_time_at(created_at, Local::now())
}

fn format_message_time_at(created_at: u64, now: DateTime<Local>) -> String {
    let Ok(seconds) = i64::try_from(created_at) else {
        return String::new();
    };
    DateTime::<Utc>::from_timestamp(seconds, 0)
        .map(|timestamp| {
            let timestamp = timestamp.with_timezone(&Local);
            let message_date = timestamp.date_naive();
            let today = now.date_naive();
            let time = timestamp
                .format("%I:%M %p")
                .to_string()
                .trim_start_matches('0')
                .to_owned();

            if message_date >= today {
                return time;
            }

            if today.pred_opt() == Some(message_date) {
                return format!("Yesterday {time}");
            }

            let week_start = today
                .checked_sub_days(Days::new(today.weekday().num_days_from_monday().into()))
                .unwrap_or(today);
            if message_date >= week_start {
                return format!("{} {time}", timestamp.format("%A"));
            }

            let day = timestamp.day();
            let ordinal_suffix = match day % 100 {
                11..=13 => "th",
                _ => match day % 10 {
                    1 => "st",
                    2 => "nd",
                    3 => "rd",
                    _ => "th",
                },
            };
            let date = if message_date.year() == today.year() {
                format!("{} {day}{ordinal_suffix}", timestamp.format("%b"))
            } else {
                format!(
                    "{} {day}{ordinal_suffix} {}",
                    timestamp.format("%b"),
                    timestamp.year()
                )
            };
            format!("{date}, {time}")
        })
        .unwrap_or_default()
}

impl Waku {
    fn show_message_copied(&mut self, message_id: Uuid, cx: &mut Context<Self>) {
        self.copied_message_generation = self.copied_message_generation.wrapping_add(1);
        let generation = self.copied_message_generation;
        self.copied_message_feedback.insert(message_id, generation);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = this.update(cx, |this, cx| {
                if this.copied_message_feedback.get(&message_id) == Some(&generation) {
                    this.copied_message_feedback.remove(&message_id);
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

#[allow(clippy::too_many_arguments)]
fn render_message_footer(
    theme: &Theme,
    message: &Message,
    footer_time: u64,
    copy_content: String,
    copied: bool,
    group_name: SharedString,
    align_right: bool,
    assistant_message_action: Option<AssistantMessageAction>,
    user_message_action: Option<UserMessageAction>,
    waku: gpui::WeakEntity<Waku>,
) -> AnyElement {
    let theme = *theme;
    let message_id = message.id;
    let copy_waku = waku.clone();
    let footer_color = if theme.is_dark {
        gpui::hsla(126.93 / 360.0, 0.000_000_1, 0.543_95, 1.0)
    } else {
        theme.text_ghost
    };
    let timestamp = div()
        .h(px(27.0))
        .px(px(4.0))
        .flex()
        .items_center()
        .text_size(px(11.5))
        .line_height(px(14.0))
        .text_color(footer_color)
        .child(format_message_time(footer_time));
    let copy_button = div()
        .id(SharedString::from(format!("copy-message-{message_id}")))
        .w(px(27.0))
        .h(px(27.0))
        .rounded(px(8.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_default()
        .hover(|element| element.bg(theme.overlay_strong))
        .child(icon(
            if copied {
                "icons/check.svg"
            } else {
                "icons/copy.svg"
            },
            14.0,
            footer_color,
        ))
        .tooltip(Tooltip::text(if copied {
            "Copied"
        } else {
            "Copy message"
        }))
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy_content.clone()));
            let _ = copy_waku.update(cx, |this, cx| {
                this.show_message_copied(message_id, cx);
            });
        });
    let mut footer = div()
        .w_full()
        .h(px(27.0))
        .flex()
        .items_center()
        .gap(px(1.0))
        .invisible()
        .group_hover(group_name, |element| element.visible())
        .when(!align_right, |element| element.ml(-px(7.0)))
        .when(align_right, |element| element.justify_end());

    if align_right {
        footer = footer.child(timestamp).child(copy_button);
    } else {
        footer = footer.child(copy_button);
        if let Some(action) = assistant_message_action {
            let fork_waku = waku.clone();
            footer = footer.child(
                div()
                    .id(SharedString::from(format!("fork-response-{message_id}")))
                    .w(px(27.0))
                    .h(px(27.0))
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_default()
                    .hover(|element| element.bg(theme.overlay_strong))
                    .child(icon("icons/fork.svg", 14.0, footer_color))
                    .tooltip(Tooltip::text("Fork task"))
                    .on_click(move |_, _, cx| {
                        let _ = fork_waku.update(cx, |this, cx| {
                            this.fork_session_from_response(
                                action.session_id,
                                action.turn_count,
                                cx,
                            );
                        });
                    }),
            );
        }
        footer = footer.child(timestamp);
    }

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
                .child(icon("icons/rewind.svg", 14.0, footer_color))
                .tooltip(Tooltip::text("Revert to here"))
                .on_click(move |_, window, cx| {
                    let _ = edit_waku.update(cx, |this, cx| {
                        this.begin_message_edit(action.session_id, action.turn_count, window, cx);
                    });
                }),
        );
    }

    footer.into_any_element()
}

/// Everything one transcript message row needs to render itself. Bundled
/// because these travel together from `transcript_row` and nowhere else.
pub(super) struct MessageRender<'a> {
    pub(super) theme: &'a Theme,
    pub(super) message: &'a Message,
    pub(super) assistant_footer_copy_content: Option<String>,
    pub(super) assistant_footer_time: Option<u64>,
    pub(super) copied: bool,
    pub(super) assistant_message_action: Option<AssistantMessageAction>,
    pub(super) user_message_action: Option<UserMessageAction>,
    pub(super) message_edit_input: Option<Entity<ComposerInput>>,
    /// The parsed response body. `None` for user and system messages, which are
    /// shown verbatim rather than as markdown.
    pub(super) markdown: Option<&'a MarkdownView>,
    pub(super) ctx: &'a MarkdownCtx<'a>,
    pub(super) menu: ContextMenuHandle,
    pub(super) waku: gpui::WeakEntity<Waku>,
    pub(super) composer: Entity<ComposerInput>,
}

pub(super) fn render_message(params: MessageRender, cx: &mut App) -> AnyElement {
    let MessageRender {
        theme,
        message,
        assistant_footer_copy_content,
        assistant_footer_time,
        copied,
        assistant_message_action,
        user_message_action,
        message_edit_input,
        markdown,
        ctx,
        menu,
        waku,
        composer,
    } = params;

    let content = message.content.clone();
    // "Copy Message" must match what the row presents. The terminal part of a
    // settled response stands in for the whole visible answer, so its menu
    // shares the footer's copy content — parts hidden behind "Worked for X"
    // stay out — rather than copying the final part alone.
    let menu_copy_content = assistant_footer_copy_content
        .clone()
        .unwrap_or_else(|| content.clone());
    let message_id = message.id;
    let role = message.role;
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
                        .child(md::render::plain_text(
                            content.clone(),
                            md::render::SANS_FAMILY,
                            FontWeight::NORMAL,
                            theme.text,
                            ctx,
                        )),
                );
                column = column.child(render_message_footer(
                    theme,
                    message,
                    message.created_at,
                    message.content.clone(),
                    copied,
                    group_name,
                    true,
                    None,
                    user_message_action,
                    waku.clone(),
                ));
            }
            column
        }
        MessageRole::Assistant => {
            let group_name = SharedString::from(format!("assistant-message-{message_id}"));
            let body = markdown
                .and_then(|markdown| md::render::markdown(markdown, ctx))
                // A response whose text has not reached the parser yet (or is
                // pure whitespace) still needs a row, so fall back to verbatim.
                .unwrap_or_else(|| {
                    md::render::plain_text(
                        content.clone(),
                        md::render::SANS_FAMILY,
                        FontWeight::NORMAL,
                        theme.text,
                        ctx,
                    )
                });
            let mut column = div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .py(px(4.0))
                .gap(px(3.0))
                .group(group_name.clone())
                .child(body);
            if message.streaming {
                column = column.child(pulse_dot(
                    format!("stream-{}", message.id),
                    6.0,
                    theme.accent,
                ));
            }
            if let Some(copy_content) = assistant_footer_copy_content {
                column = column.child(render_message_footer(
                    theme,
                    message,
                    assistant_footer_time.unwrap_or(message.created_at),
                    copy_content,
                    copied,
                    group_name,
                    false,
                    assistant_message_action,
                    None,
                    waku.clone(),
                ));
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
                .child(md::render::plain_text(
                    content.clone(),
                    md::render::SANS_FAMILY,
                    FontWeight::NORMAL,
                    theme.text_tertiary,
                    ctx,
                )),
        ),
    };

    let selection = ctx.selection().clone();
    context_menu(
        element.id(message_id),
        SharedString::from(format!("message-menu-{message_id}")),
        &menu,
        move |cx| {
            message_menu_items(
                &menu_copy_content,
                role,
                user_message_action,
                assistant_message_action,
                &selection,
                &composer,
                &waku,
                cx,
            )
        },
    )
}

/// The message row's context menu. Rebuilt on each open, so availability checks
/// here always reflect the current session state.
#[allow(clippy::too_many_arguments)]
fn message_menu_items(
    content: &str,
    role: MessageRole,
    user_message_action: Option<UserMessageAction>,
    assistant_message_action: Option<AssistantMessageAction>,
    selection: &TranscriptSelection,
    composer: &Entity<ComposerInput>,
    waku: &gpui::WeakEntity<Waku>,
    _cx: &mut App,
) -> Vec<MenuItem> {
    let mut items = Vec::new();

    if let Some(selected) = selection.selection.borrow().selected_text() {
        items.push(MenuItem::new("Copy Selection", move |_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(selected.clone()));
        }));
    }

    let copy_content = content.to_owned();
    items.push(MenuItem::new("Copy Message", move |_, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(copy_content.clone()));
    }));

    if role == MessageRole::User && user_message_action.is_none() {
        let composer = composer.clone();
        let edit_content = content.to_owned();
        items.push(MenuItem::new("Copy to Composer", move |window, cx| {
            composer.update(cx, |composer, cx| {
                composer.set_content(edit_content.clone(), cx);
            });
            let focus_handle = composer.read(cx).focus();
            window.focus(&focus_handle, cx);
        }));
    }

    if let Some(code) = fenced_code(content) {
        items.push(MenuItem::new("Copy Code", move |_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
        }));
    }

    if let Some(action) = user_message_action {
        let waku = waku.clone();
        items.push(MenuItem::Separator);
        items.push(
            MenuItem::new("Revert to Here", move |window, cx| {
                let _ = waku.update(cx, |this, cx| {
                    this.begin_message_edit(action.session_id, action.turn_count, window, cx);
                });
            })
            .icon("icons/rewind.svg"),
        );
    }

    if let Some(action) = assistant_message_action {
        let waku = waku.clone();
        items.push(MenuItem::Separator);
        items.push(
            MenuItem::new("Fork Task", move |_, cx| {
                let _ = waku.update(cx, |this, cx| {
                    this.fork_session_from_response(action.session_id, action.turn_count, cx);
                });
            })
            .icon("icons/fork.svg"),
        );
    }

    items
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

pub(super) fn activity_display_title(activity: &ActivityItem) -> String {
    if activity.kind == crate::model::ActivityKind::Tool
        && let Some(arguments) = activity.arguments.as_deref()
        && let Ok(arguments) = serde_json::from_str::<serde_json::Value>(arguments)
        && let Some(title) = arguments
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty())
    {
        return title.to_owned();
    }
    activity.title.clone()
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum ActivityDisclosureSectionKind {
    Arguments,
    Output,
    Detail,
}

impl ActivityDisclosureSectionKind {
    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Arguments => "arguments",
            Self::Output => "output",
            Self::Detail => "detail",
        }
    }

    pub(super) fn label(self) -> Option<&'static str> {
        match self {
            Self::Arguments => Some("Arguments"),
            Self::Output => Some("Output"),
            Self::Detail => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ActivityDisclosureSection {
    pub(super) kind: ActivityDisclosureSectionKind,
    pub(super) content: String,
}

pub(super) fn activity_disclosure_sections(
    activity: &ActivityItem,
) -> Vec<ActivityDisclosureSection> {
    let mut sections = Vec::new();
    if let Some(arguments) = activity
        .arguments
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        sections.push(ActivityDisclosureSection {
            kind: ActivityDisclosureSectionKind::Arguments,
            content: arguments.to_owned(),
        });
    }
    if let Some(output) = activity
        .output
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        sections.push(ActivityDisclosureSection {
            kind: ActivityDisclosureSectionKind::Output,
            content: output.to_owned(),
        });
    } else if !activity.image_urls.is_empty() {
        sections.push(ActivityDisclosureSection {
            kind: ActivityDisclosureSectionKind::Output,
            content: String::new(),
        });
    }
    if sections.is_empty()
        && let Some(detail) = activity
            .detail
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    {
        sections.push(ActivityDisclosureSection {
            kind: ActivityDisclosureSectionKind::Detail,
            content: detail.to_owned(),
        });
    }
    sections
}

pub(super) fn activity_preview(activity: &ActivityItem) -> String {
    let detail = activity.detail.as_deref().unwrap_or_default().trim();
    if detail.eq_ignore_ascii_case("failed")
        && let Some(output) = activity.output.as_deref()
        && let Some(first_line) = output.lines().find(|line| !line.trim().is_empty())
    {
        return first_line.trim().to_owned();
    }
    if (detail.is_empty() || detail.eq_ignore_ascii_case("failed"))
        && !activity.image_urls.is_empty()
    {
        return "Image output".to_owned();
    }
    detail.to_owned()
}

#[cfg(test)]
mod message_time_tests {
    use super::*;
    use chrono::TimeZone;

    /// Test-only rendering of disclosure sections into plain text; production
    /// renders them interactively via [`activity_disclosure_sections`].
    fn activity_disclosure_text(activity: &ActivityItem) -> Option<String> {
        let sections = activity_disclosure_sections(activity);
        (!sections.is_empty()).then(|| {
            sections
                .into_iter()
                .map(
                    |section| match (section.kind.label(), section.content.is_empty()) {
                        (Some(label), false) => format!("{label}\n{}", section.content),
                        (Some(label), true) => label.to_owned(),
                        (None, _) => section.content,
                    },
                )
                .collect::<Vec<_>>()
                .join("\n\n")
        })
    }

    fn local_datetime(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .expect("test date should be valid in the local timezone")
    }

    fn unix_seconds(timestamp: DateTime<Local>) -> u64 {
        timestamp
            .timestamp()
            .try_into()
            .expect("test date should have a positive Unix timestamp")
    }

    #[test]
    fn message_time_includes_calendar_context_for_older_messages() {
        let now = local_datetime(2026, 8, 9, 16, 0); // Sunday

        assert_eq!(
            format_message_time_at(unix_seconds(local_datetime(2026, 8, 9, 9, 5)), now),
            "9:05 AM"
        );
        assert_eq!(
            format_message_time_at(unix_seconds(local_datetime(2026, 8, 8, 17, 0)), now),
            "Yesterday 5:00 PM"
        );
        assert_eq!(
            format_message_time_at(unix_seconds(local_datetime(2026, 8, 7, 13, 12)), now),
            "Friday 1:12 PM"
        );
        assert_eq!(
            format_message_time_at(unix_seconds(local_datetime(2026, 5, 12, 23, 0)), now),
            "May 12th, 11:00 PM"
        );
        assert_eq!(
            format_message_time_at(unix_seconds(local_datetime(2024, 8, 4, 11, 0)), now),
            "Aug 4th 2024, 11:00 AM"
        );
    }

    #[test]
    fn message_time_uses_correct_ordinal_suffixes() {
        let now = local_datetime(2026, 8, 9, 16, 0);

        for (day, suffix) in [
            (1, "st"),
            (2, "nd"),
            (3, "rd"),
            (11, "th"),
            (12, "th"),
            (13, "th"),
            (21, "st"),
        ] {
            let formatted =
                format_message_time_at(unix_seconds(local_datetime(2026, 5, day, 9, 0)), now);
            assert!(formatted.starts_with(&format!("May {day}{suffix},")));
        }
    }

    #[test]
    fn activity_disclosure_keeps_arguments_and_output() {
        let activity = ActivityItem::new(
            Some("tool-1".into()),
            crate::model::ActivityKind::Tool,
            "Use Helium",
            Some("failed".into()),
            true,
        )
        .with_arguments(Some("{\n  \"actions\": []\n}".into()))
        .with_output(Some("Computer Use helper closed its session".into()))
        .with_failed(true);

        assert_eq!(
            activity_disclosure_sections(&activity),
            vec![
                ActivityDisclosureSection {
                    kind: ActivityDisclosureSectionKind::Arguments,
                    content: "{\n  \"actions\": []\n}".into(),
                },
                ActivityDisclosureSection {
                    kind: ActivityDisclosureSectionKind::Output,
                    content: "Computer Use helper closed its session".into(),
                },
            ]
        );
        assert_eq!(
            activity_disclosure_text(&activity).as_deref(),
            Some(
                "Arguments\n{\n  \"actions\": []\n}\n\nOutput\nComputer Use helper closed its session"
            )
        );
        assert_eq!(
            activity_preview(&activity),
            "Computer Use helper closed its session"
        );

        let image_only = ActivityItem::new(
            Some("tool-2".into()),
            crate::model::ActivityKind::Tool,
            "Screenshot",
            None,
            true,
        )
        .with_image_urls(vec!["data:image/png;base64,aGVsbG8=".into()]);
        assert_eq!(
            activity_disclosure_text(&image_only).as_deref(),
            Some("Output")
        );
        assert_eq!(activity_preview(&image_only), "Image output");
    }

    #[test]
    fn activity_display_title_prefers_the_human_facing_tool_argument() {
        let titled = ActivityItem::new(
            Some("tool-1".into()),
            crate::model::ActivityKind::Tool,
            "Js",
            None,
            true,
        )
        .with_arguments(Some(
            r#"{"title":"Inspect Helium browser","code":"sky.get_app_state()"}"#.into(),
        ));
        let untitled = ActivityItem::new(
            Some("tool-2".into()),
            crate::model::ActivityKind::Tool,
            "Js",
            None,
            true,
        )
        .with_arguments(Some(r#"{"code":"sky.list_apps()"}"#.into()));

        assert_eq!(activity_display_title(&titled), "Inspect Helium browser");
        assert_eq!(activity_display_title(&untitled), "Js");
    }
}
