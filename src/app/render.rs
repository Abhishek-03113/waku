use super::*;

impl Render for Waku {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.settings_page.is_some() {
            return self.render_settings(cx).into_any_element();
        }

        let theme = Theme::current(cx);
        let empty = self
            .selected_session()
            .map(|session| session.messages.is_empty())
            .unwrap_or(true);
        let permission = self.render_permission(cx);
        let toast = self.toast.clone();
        div()
            .key_context("Waku")
            .on_action(cx.listener(Self::new_session_action))
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::toggle_sidebar_action))
            .on_action(cx.listener(Self::focus_composer_action))
            .on_action(cx.listener(Self::cancel_turn_action))
            .size_full()
            .flex()
            .text_color(theme.text)
            .font_family(".SystemUIFont")
            .when(self.sidebar_visible, |root| {
                root.child(self.render_sidebar(cx))
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
                        self.render_transcript(window, cx)
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
                            .child(self.render_composer(window, cx))
                            .child(self.render_workspace_footer(cx))
                    }),
            )
            .into_any_element()
    }
}
