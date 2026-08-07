use super::*;

impl Waku {
    pub(super) fn render_panel_resize_handle(
        &self,
        id: &'static str,
        target: PanelResizeTarget,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        let active = self
            .panel_resize_drag
            .is_some_and(|drag| drag.target == target);
        // The right panel's left edge abuts the browser webview, a native view
        // that composites above every base-scene pixel at or beyond the edge.
        // Its bar and hover strip therefore sit entirely left of the edge,
        // where GPUI still owns rendering and input; the other edges keep the
        // conventional straddle.
        let (strip_left, strip_width) = match target {
            PanelResizeTarget::RightPanel => (-7.0, 8.0),
            PanelResizeTarget::Sidebar | PanelResizeTarget::FileTree => (-5.0, 10.0),
        };
        div()
            .id(id)
            .absolute()
            .top_0()
            .left(px(strip_left))
            .w(px(strip_width))
            .h_full()
            .group("panel-resize-handle")
            .cursor_col_resize()
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(5.0))
                    .w(px(2.0))
                    .h_full()
                    .bg(if active {
                        theme.resize_handle
                    } else {
                        gpui::transparent_black()
                    })
                    .group_hover("panel-resize-handle", |element| {
                        element.bg(theme.resize_handle)
                    }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event, window, cx| {
                    this.begin_panel_resize(target, event, window, cx);
                }),
            )
    }
}

impl Waku {
    /// Measure live frame rate by counting renders over a sliding one-second
    /// window and keep requesting animation frames so the counter stays current.
    fn tick_fps(&mut self, window: &Window) {
        let now = Instant::now();
        self.fps_frame_count = self.fps_frame_count.saturating_add(1);
        if now.duration_since(self.fps_last_frame) >= Duration::from_secs(1) {
            self.fps_value = self.fps_frame_count as u32;
            self.fps_frame_count = 0;
            self.fps_last_frame = now;
        }
        window.request_animation_frame();
    }
}

impl Render for Waku {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Before anything can early-return (the settings page below), settle
        // whether each native browser webview belongs on screen this frame —
        // it floats above everything GPUI paints.
        self.sync_browser_webviews(cx);
        if self.fps_counter_visible {
            self.tick_fps(window);
        }
        if self.settings_page.is_some() {
            return self.render_settings(cx).into_any_element();
        }

        let theme = Theme::current(cx);
        let empty = self
            .selected_session()
            .map(|session| session.messages.is_empty())
            .unwrap_or(true);
        let permission = self.render_permission(cx);
        let computer_use = self.render_computer_use_overlay(cx);
        let toast = self.toast.clone();
        let (sidebar_width, right_panel_width) = self.effective_panel_widths(window);
        let chat_viewport_width = f32::from(window.viewport_size().width)
            - if self.sidebar_visible {
                sidebar_width
            } else {
                0.0
            }
            - if self.right_panel_visible {
                right_panel_width
            } else {
                0.0
            };
        div()
            .key_context("Waku")
            .on_action(cx.listener(Self::close_window_or_right_panel_tab_action))
            .on_action(cx.listener(Self::new_session_action))
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::toggle_sidebar_action))
            .on_action(cx.listener(Self::toggle_right_panel_action))
            .on_action(cx.listener(Self::toggle_fps_counter_action))
            .on_action(cx.listener(Self::navigate_back_action))
            .on_action(cx.listener(Self::navigate_forward_action))
            .on_action(cx.listener(Self::focus_composer_action))
            .on_action(cx.listener(Self::toggle_model_picker_action))
            .on_action(cx.listener(Self::toggle_usage_panel_action))
            .on_action(cx.listener(Self::save_right_panel_file_action))
            .on_action(cx.listener(Self::cancel_turn_action))
            .on_action(cx.listener(Self::copy_selection_action))
            .on_action(cx.listener(Self::open_find_action))
            .on_action(cx.listener(Self::open_find_replace_action))
            .on_action(cx.listener(Self::close_find_action))
            .on_action(cx.listener(Self::find_next_action))
            .on_action(cx.listener(Self::find_previous_action))
            .on_action(cx.listener(Self::toggle_find_case_action))
            .on_action(cx.listener(Self::toggle_find_whole_word_action))
            .on_action(cx.listener(Self::toggle_find_regex_action))
            .on_action(cx.listener(Self::replace_all_matches_action))
            .capture_any_mouse_down(cx.listener(Self::navigation_mouse_down))
            .on_mouse_move(cx.listener(Self::resize_panel_mouse_move))
            .capture_any_mouse_up(cx.listener(Self::finish_panel_resize))
            .size_full()
            .flex()
            .text_color(theme.text)
            .font_family(".SystemUIFont")
            .when(self.sidebar_visible, |root| {
                root.child(self.render_sidebar(sidebar_width, cx))
            })
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .bg(theme.surface)
                    .when(self.sidebar_visible, |element| {
                        element.border_l_1().border_color(theme.sidebar_border)
                    })
                    .child(self.render_header(cx))
                    .child(if empty {
                        self.render_empty_state(cx).into_any_element()
                    } else {
                        self.render_transcript(window, chat_viewport_width, cx)
                    })
                    .children(permission)
                    .when_some(toast, |element, toast| {
                        element.child(
                            div()
                                .px(px(20.0))
                                .pb(px(8.0))
                                .flex()
                                .justify_center()
                                .child(
                                    div()
                                        .w_full()
                                        .max_w(px(CONTENT_MAX_WIDTH))
                                        .min_w_0()
                                        .px(px(12.0))
                                        .py(px(6.0))
                                        .rounded_full()
                                        .border_1()
                                        .border_color(theme.border_strong)
                                        .bg(theme.raised)
                                        .shadow_sm()
                                        .text_size(px(11.0))
                                        .text_color(theme.danger)
                                        .whitespace_normal()
                                        .child(SharedString::from(toast)),
                                ),
                        )
                    })
                    .when(self.selected_project().is_some(), |element| {
                        element
                            .children(self.render_queued_messages(cx))
                            .child(self.render_composer(window, cx))
                            .child(self.render_workspace_footer(cx))
                    })
                    .relative()
                    .children(computer_use)
                    .when(self.sidebar_visible, |element| {
                        element.child(self.render_panel_resize_handle(
                            "sidebar-resize-handle",
                            PanelResizeTarget::Sidebar,
                            cx,
                        ))
                    }),
            )
            .when(self.right_panel_visible, |root| {
                root.child(self.render_right_panel(right_panel_width, window, cx))
            })
            .into_any_element()
    }
}
