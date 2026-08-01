use super::*;

impl Waku {
    pub(super) fn render_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);

        div()
            .key_context("Waku")
            .on_action(|_: &CloseWindow, window, _| crate::platform::hide_window(window))
            .on_action(cx.listener(Self::new_session_action))
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::toggle_sidebar_action))
            .on_action(cx.listener(Self::navigate_back_action))
            .on_action(cx.listener(Self::navigate_forward_action))
            .on_action(cx.listener(Self::focus_composer_action))
            .on_action(cx.listener(Self::cancel_turn_action))
            .capture_any_mouse_down(cx.listener(Self::navigation_mouse_down))
            .size_full()
            .flex()
            .bg(theme.canvas)
            .text_color(theme.text)
            .font_family(".SystemUIFont")
            .child(self.render_settings_sidebar(cx))
            .child(self.render_settings_content(cx))
            .into_any_element()
    }

    fn render_settings_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let current_page = self.settings_page.unwrap_or(SettingsPage::Appearance);
        let query = self
            .settings_search
            .read(cx)
            .value()
            .trim()
            .to_ascii_lowercase();
        let mut navigation = div().flex().flex_col().gap(px(3.0));

        for (page, label, icon_path, _keywords) in [
            (
                SettingsPage::General,
                "General",
                "icons/settings.svg",
                "general local projects conversations privacy",
            ),
            (
                SettingsPage::Appearance,
                "Appearance",
                "icons/appearance.svg",
                "appearance theme system light dark",
            ),
        ]
        .into_iter()
        .filter(|(_, _, _, keywords)| query.is_empty() || keywords.contains(query.as_str()))
        {
            let selected = current_page == page;
            navigation = navigation.child(
                div()
                    .id(SharedString::from(format!(
                        "settings-tab-{}",
                        label.to_ascii_lowercase()
                    )))
                    .h(px(36.0))
                    .px(px(11.0))
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .cursor_default()
                    .text_size(px(13.0))
                    .text_color(if selected {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .when(selected, |element| {
                        element.bg(theme.sidebar_item_background)
                    })
                    .hover(|element| element.bg(theme.sidebar_item_background))
                    .active(|element| element.bg(theme.sidebar_item_background))
                    .child(icon(
                        icon_path,
                        15.0,
                        if selected {
                            theme.text_secondary
                        } else {
                            theme.text_tertiary
                        },
                    ))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings_page = Some(page);
                        cx.notify();
                    })),
            );
        }

        div()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme.sidebar)
            .child(self.render_settings_drag_region("settings-sidebar-titlebar", cx))
            .child(
                div().px(px(12.0)).child(
                    div()
                        .id("settings-back")
                        .h(px(34.0))
                        .px(px(9.0))
                        .rounded(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(9.0))
                        .cursor_default()
                        .text_size(px(13.0))
                        .text_color(theme.text_secondary)
                        .hover(|element| element.bg(theme.overlay))
                        .active(|element| element.bg(theme.overlay_strong))
                        .child(icon("icons/arrow-left.svg", 15.0, theme.text_tertiary))
                        .child("Back")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.settings_page = None;
                            window.focus(&this.composer_focus(cx));
                            cx.notify();
                        })),
                ),
            )
            .child(
                div().px(px(12.0)).pt(px(8.0)).child(
                    Input::new(&self.settings_search)
                        .appearance(false)
                        .bordered(false)
                        .focus_bordered(false)
                        .h(px(29.0))
                        .bg(theme.overlay)
                        .border_1()
                        .border_color(theme.border)
                        .rounded(px(7.0))
                        .prefix(icon("icons/search.svg", 13.0, theme.text_tertiary)),
                ),
            )
            .child(div().h(px(18.0)))
            .child(div().px(px(12.0)).child(navigation))
    }

    fn render_settings_content(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let page = self.settings_page.unwrap_or(SettingsPage::Appearance);

        div()
            .flex_1()
            .h_full()
            .min_w_0()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme.sidebar_border)
            .bg(theme.surface)
            .child(self.render_settings_drag_region("settings-content-titlebar", cx))
            .child(
                div()
                    .id("settings-content-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .px(px(32.0))
                    .pb(px(48.0))
                    .child(
                        div()
                            .w_full()
                            .max_w(px(1080.0))
                            .child(
                                div()
                                    .pt(px(2.0))
                                    .text_size(px(18.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(match page {
                                        SettingsPage::General => "General",
                                        SettingsPage::Appearance => "Appearance",
                                    }),
                            )
                            .child(match page {
                                SettingsPage::General => self.render_general_settings(cx),
                                SettingsPage::Appearance => self.render_appearance_settings(cx),
                            }),
                    ),
            )
    }

    fn render_general_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        div()
            .mt(px(15.0))
            .w_full()
            .max_w(px(1080.0))
            .px(px(20.0))
            .py(px(14.0))
            .rounded(px(13.0))
            .bg(theme.raised)
            .child(
                div()
                    .text_size(px(13.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child("Local by default"),
            )
            .child(
                div()
                    .mt(px(5.0))
                    .text_size(px(12.5))
                    .line_height(px(18.0))
                    .text_color(theme.text_secondary)
                    .child("Projects, conversations, and settings are stored on this Mac."),
            )
            .into_any_element()
    }

    fn render_appearance_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let selected = self.state.theme;
        let weak = cx.entity().downgrade();
        let selector = MenuChip::new("theme-selector")
            .label(selected.label())
            .outlined()
            .w(px(116.0))
            .justify_between()
            .dropdown_menu(move |mut menu, _window, _cx| {
                menu = menu.min_w(px(148.0)).max_w(px(148.0));
                for preference in ThemePreference::ALL {
                    let weak = weak.clone();
                    menu = menu.item(
                        PopupMenuItem::new(preference.label())
                            .selected(preference == selected)
                            .on_click(move |_, window, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.set_theme_preference(preference, window, cx);
                                });
                            }),
                    );
                }
                menu
            })
            .anchor(Corner::TopRight);

        div()
            .mt(px(15.0))
            .w_full()
            .max_w(px(1080.0))
            .min_h(px(60.0))
            .px(px(20.0))
            .py(px(12.0))
            .rounded(px(13.0))
            .bg(theme.raised)
            .flex()
            .items_center()
            .gap(px(24.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child("Theme"),
                    )
                    .child(
                        div()
                            .mt(px(5.0))
                            .text_size(px(12.5))
                            .line_height(px(18.0))
                            .text_color(theme.text_secondary)
                            .child("Choose between system, light, or dark themes."),
                    ),
            )
            .child(selector)
            .into_any_element()
    }

    fn render_settings_drag_region(
        &self,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .h(px(48.0))
            .flex_none()
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

    fn set_theme_preference(
        &mut self,
        preference: ThemePreference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.theme == preference {
            return;
        }
        self.state.theme = preference;
        crate::theme::apply_theme_preference(preference, window, cx);
        self.save();
        cx.notify();
    }
}
