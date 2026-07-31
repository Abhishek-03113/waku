use super::*;

impl Waku {
    fn window_drag_region(&self, region: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        region
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    window.titlebar_double_click();
                }
            })
            .on_mouse_down_out(cx.listener(|this, _, _, _| {
                this.header_drag_armed = false;
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.header_drag_armed = true;
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.header_drag_armed = false;
                }),
            )
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.header_drag_armed {
                    this.header_drag_armed = false;
                    crate::platform::start_window_move(window);
                }
            }))
    }
    // ── Sidebar ────────────────────────────────────────────────────────────

    fn render_sidebar_toggle(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = Theme::current(cx);
        div()
            .id("toggle-sidebar")
            .w(px(26.0))
            .h(px(26.0))
            .flex_none()
            .rounded(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .hover(|element| element.bg(theme.overlay))
            .active(|element| element.bg(theme.overlay_strong))
            .child(icon("icons/panel-left.svg", 14.0, theme.text_tertiary))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.sidebar_visible = !this.sidebar_visible;
                cx.notify();
            }))
    }

    fn render_sidebar_titlebar(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("sidebar-titlebar")
            .h(px(48.0))
            .flex_none()
            .flex()
            .items_center()
            .child(
                self.window_drag_region(
                    div()
                        .id("sidebar-traffic-light-drag-region")
                        .w(px(TRAFFIC_LIGHT_CLEARANCE))
                        .h_full()
                        .flex_none(),
                    cx,
                ),
            )
            .child(self.render_sidebar_toggle(cx))
            .child(self.window_drag_region(
                div().id("sidebar-titlebar-drag-region").h_full().flex_1(),
                cx,
            ))
    }

    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let selected_project = self.state.selected_project;
        let selected_session = self.state.selected_session;

        let mut projects = div().flex().flex_col();
        for project in &self.state.projects {
            let project_id = project.id;
            let selected = selected_project == Some(project.id);
            projects = projects.child(
                div()
                    .id(SharedString::from(format!("project-{}", project.id)))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .h(px(28.0))
                    .px(px(8.0))
                    .text_size(px(12.5))
                    .line_height(px(16.0))
                    .rounded(px(7.0))
                    .cursor_default()
                    .when(selected, |element| {
                        element.bg(theme.sidebar_item_background)
                    })
                    .hover(|element| element.bg(theme.sidebar_item_background))
                    .active(|element| element.bg(theme.sidebar_item_background))
                    .child(icon(
                        "icons/folder.svg",
                        13.0,
                        if selected {
                            theme.text_secondary
                        } else {
                            theme.text_tertiary
                        },
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5))
                            .text_color(if selected {
                                theme.text
                            } else {
                                theme.text_secondary
                            })
                            .child(SharedString::from(project.name.clone())),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_project(project_id, cx);
                    })),
            );
        }

        let mut sessions = div().flex().flex_col().gap(px(1.0));
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
                let active = !matches!(session.status, SessionStatus::Idle);
                let waku = cx.entity().downgrade();
                let composer = self.composer.clone();
                sessions = sessions.child(
                    div()
                        .id(SharedString::from(format!("session-{}", session.id)))
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .px(px(8.0))
                        .py(px(6.0))
                        .rounded(px(7.0))
                        .cursor_default()
                        .when(selected, |element| {
                            element.bg(theme.sidebar_item_background)
                        })
                        .hover(|element| element.bg(theme.sidebar_item_background))
                        .active(|element| element.bg(theme.sidebar_item_background))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .line_height(px(16.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(12.5))
                                        .text_color(if selected {
                                            theme.text
                                        } else {
                                            theme.text_secondary
                                        })
                                        .child(SharedString::from(session.title.clone())),
                                )
                                .when(active, |element| {
                                    element.child(pulse_dot(
                                        format!("session-pulse-{session_id}"),
                                        5.0,
                                        status_color(&theme, session.status),
                                    ))
                                })
                                .child(
                                    div()
                                        .flex_none()
                                        .text_size(px(10.0))
                                        .text_color(theme.text_ghost)
                                        .child(SharedString::from(relative_time(
                                            session.updated_at,
                                        ))),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(5.0))
                                .text_size(px(10.5))
                                .line_height(px(13.0))
                                .child(icon(
                                    provider_icon(session.provider),
                                    10.0,
                                    provider_color(&theme, session.provider).opacity(0.8),
                                ))
                                .child(
                                    div()
                                        .text_color(theme.text_tertiary)
                                        .child(session.provider.short_name()),
                                ),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_session(session_id, cx);
                        }))
                        .context_menu_with_id(
                            SharedString::from(format!("session-context-menu-{session_id}")),
                            move |menu, window, cx| {
                                let waku = waku.clone();
                                preserve_composer_focus_for_context_menu(
                                    &composer, menu, window, cx,
                                )
                                .min_w(px(140.0))
                                .item(
                                    PopupMenuItem::new("Remove").on_click(move |_, _, cx| {
                                        let _ = waku.update(cx, |waku, cx| {
                                            waku.remove_session(session_id, cx);
                                        });
                                    }),
                                )
                            },
                        ),
                );
            }
        }

        div()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme.sidebar)
            .child(self.render_sidebar_titlebar(cx))
            .child(
                div().px(px(10.0)).child(
                    div()
                        .id("new-session")
                        .h(px(30.0))
                        .px(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .text_size(px(12.5))
                        .line_height(px(16.0))
                        .rounded(px(7.0))
                        .cursor_default()
                        .hover(|element| element.bg(theme.sidebar_item_background))
                        .active(|element| element.bg(theme.sidebar_item_background))
                        .child(icon("icons/plus.svg", 13.0, theme.text_secondary))
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(12.5))
                                .text_color(theme.text)
                                .child("New session"),
                        )
                        .child(key_hint(&theme, "⌘N"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(project_id) = this.state.selected_project {
                                this.create_session_for(project_id, this.state.last_provider, cx);
                            }
                        })),
                ),
            )
            .child(
                div()
                    .id("sidebar-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .px(px(10.0))
                    .pt(px(14.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(section_label(&theme, "Projects"))
                            .child(
                                div()
                                    .id("add-project")
                                    .w(px(20.0))
                                    .h(px(20.0))
                                    .rounded(px(6.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_default()
                                    .hover(|element| element.bg(theme.overlay))
                                    .active(|element| element.bg(theme.overlay_strong))
                                    .child(icon("icons/plus.svg", 11.0, theme.text_ghost))
                                    .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
                            ),
                    )
                    .child(projects)
                    .child(div().h(px(16.0)))
                    .child(section_label(&theme, "Sessions"))
                    .child(sessions),
            )
    }

    // ── Header ─────────────────────────────────────────────────────────────

    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current(cx);
        let session = self.selected_session();
        let provider = session.map(|session| session.provider).unwrap_or_default();
        let status = session.map(|session| session.status).unwrap_or_default();
        div()
            .id("window-header")
            .h(px(46.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .pl(if self.sidebar_visible {
                px(10.0)
            } else {
                px(0.0)
            })
            .pr(px(14.0))
            .when(!self.sidebar_visible, |element| {
                element
                    .child(
                        self.window_drag_region(
                            div()
                                .id("header-traffic-light-drag-region")
                                .w(px(TRAFFIC_LIGHT_CLEARANCE - 8.0))
                                .h_full()
                                .flex_none(),
                            cx,
                        ),
                    )
                    .child(self.render_sidebar_toggle(cx))
            })
            .child(
                self.window_drag_region(
                    div()
                        .id("header-title-drag-region")
                        .h_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .truncate()
                        .text_size(px(13.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(SharedString::from(
                            session
                                .map(|session| session.title.clone())
                                .unwrap_or_else(|| "New task".into()),
                        )),
                    cx,
                ),
            )
            .child(
                self.window_drag_region(
                    div().id("header-center-drag-region").h_full().flex_1(),
                    cx,
                ),
            )
            .when(status != SessionStatus::Idle, |element| {
                element.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .text_size(px(11.0))
                        .line_height(px(14.0))
                        .child(match status {
                            SessionStatus::Connecting | SessionStatus::Working => {
                                pulse_dot("header-status-pulse", 5.0, status_color(&theme, status))
                            }
                            _ => div()
                                .w(px(5.0))
                                .h(px(5.0))
                                .rounded_full()
                                .bg(status_color(&theme, status))
                                .into_any_element(),
                        })
                        .child(
                            div()
                                .text_color(status_color(&theme, status))
                                .child(status_label(status)),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(11.5))
                    .line_height(px(14.0))
                    .child(icon(
                        provider_icon(provider),
                        11.0,
                        provider_color(&theme, provider).opacity(0.9),
                    ))
                    .child(
                        div()
                            .text_color(theme.text_secondary)
                            .child(provider.short_name()),
                    ),
            )
    }

    // ── Empty states ───────────────────────────────────────────────────────

    pub(super) fn render_empty_state(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        if self.selected_project().is_none() {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .px_8()
                .pb(px(46.0))
                .child(icon("icons/sparkle.svg", 24.0, theme.accent))
                .child(
                    div()
                        .mt(px(16.0))
                        .text_size(px(20.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child("Open a project to begin"),
                )
                .child(
                    div()
                        .mt(px(8.0))
                        .max_w(px(380.0))
                        .text_center()
                        .text_size(px(12.5))
                        .line_height(px(19.0))
                        .text_color(theme.text_tertiary)
                        .child(
                            "Waku runs coding agents in folders you choose. Your code, sessions, and history stay on this Mac.",
                        ),
                )
                .child(
                    div()
                        .id("onboarding-add-project")
                        .mt(px(20.0))
                        .h(px(32.0))
                        .px(px(14.0))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .cursor_default()
                        .bg(theme.inverse)
                        .text_color(theme.on_inverse)
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .hover(|element| element.opacity(0.9))
                        .active(|element| element.opacity(0.8))
                        .child("Open project folder…")
                        .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
                );
        }
        let project_name = self
            .selected_project()
            .map(|project| project.name.as_str())
            .unwrap_or("your project");
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px_8()
            .pb(px(52.0))
            .child(icon("icons/sparkle.svg", 20.0, theme.accent))
            .child(
                div()
                    .mt(px(14.0))
                    .text_size(px(20.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(SharedString::from(format!(
                        "What should we build in {project_name}?"
                    ))),
            )
    }
}
