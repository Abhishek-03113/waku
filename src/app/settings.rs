use super::*;

const SETTINGS_CONTENT_MAX_WIDTH: f32 = 760.0;

impl Waku {
    pub(super) fn render_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);

        div()
            .key_context("Waku")
            .track_focus(&self.settings_focus)
            .on_action(|_: &CloseWindow, window, _| crate::platform::hide_window(window))
            .on_action(cx.listener(Self::new_session_action))
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::toggle_sidebar_action))
            .on_action(cx.listener(Self::toggle_right_panel_action))
            .on_action(cx.listener(Self::toggle_fps_counter_action))
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
            .content()
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
            (
                SettingsPage::Providers,
                "Providers",
                "icons/bot.svg",
                "providers agents models cli version install detect claude codex cursor opencode amp grok pi",
            ),
            (
                SettingsPage::ComputerUse,
                "Computer Use",
                "icons/cursor-spark.svg",
                "computer use screen recording accessibility apps control codex",
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
            .w(px(DEFAULT_SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme.sidebar)
            .child(self.render_settings_sidebar_titlebar(cx))
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
                            let focus_handle = this.composer_focus(cx);
                            window.focus(&focus_handle, cx);
                            cx.notify();
                        })),
                ),
            )
            .child(
                div().px(px(12.0)).pt(px(8.0)).child(
                    TextField::new("settings-search-field", self.settings_search.clone())
                        .icon("icons/search.svg", 13.0),
                ),
            )
            .child(div().h(px(18.0)))
            .child(div().px(px(12.0)).child(navigation))
    }

    fn render_settings_sidebar_titlebar(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("settings-sidebar-titlebar")
            .h(px(48.0))
            .flex_none()
            .flex()
            .items_center()
            .child(
                self.window_drag_region(
                    div()
                        .id("settings-sidebar-traffic-light-drag-region")
                        .w(px(TRAFFIC_LIGHT_CLEARANCE))
                        .h_full()
                        .flex_none(),
                    cx,
                ),
            )
            .child(
                self.render_settings_drag_region("settings-sidebar-titlebar-drag-region", cx)
                    .flex_1(),
            )
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
                            .max_w(px(SETTINGS_CONTENT_MAX_WIDTH))
                            .mx_auto()
                            .child(
                                div()
                                    .pt(px(2.0))
                                    .text_size(px(18.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(match page {
                                        SettingsPage::General => "General",
                                        SettingsPage::Providers => "Providers",
                                        SettingsPage::ComputerUse => "Computer Use",
                                        SettingsPage::Appearance => "Appearance",
                                    }),
                            )
                            .child(match page {
                                SettingsPage::General => self.render_general_settings(cx),
                                SettingsPage::Providers => self.render_providers_settings(cx),
                                SettingsPage::ComputerUse => self.render_computer_use_settings(cx),
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
        let handle = self.menu_handle("theme-selector", cx);
        let selector = dropdown_menu(
            MenuChip::new("theme-selector")
                .label(selected.label())
                .outlined()
                .selected(handle.is_open())
                .w(px(116.0))
                .justify_between(),
            "theme-selector-menu",
            &handle,
            MenuAlign::BelowRight,
            move |_| {
                ThemePreference::ALL
                    .into_iter()
                    .map(|preference| {
                        let weak = weak.clone();
                        MenuItem::new(preference.label(), move |window, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.set_theme_preference(preference, window, cx);
                            });
                        })
                        .selected(preference == selected)
                    })
                    .collect()
            },
        );

        div()
            .mt(px(15.0))
            .w_full()
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

    fn render_providers_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let checking = self.provider_detection_remaining > 0;
        let checked_label = self
            .provider_detection_checked_at
            .filter(|_| !checking)
            .map(|checked_at| detection_checked_label(checked_at.elapsed()));

        let refresh = div()
            .id("refresh-providers")
            .tab_index(0)
            .focus_visible(|style| style.border_color(theme.accent))
            .h(px(28.0))
            .px(px(11.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .items_center()
            .gap(px(6.0))
            .cursor_default()
            .text_size(px(10.5))
            .text_color(theme.text_secondary)
            .opacity(if checking { 0.6 } else { 1.0 })
            .hover(|element| element.bg(theme.overlay))
            .child(icon("icons/rotate-cw.svg", 11.0, theme.text_tertiary))
            .child(if checking { "Checking…" } else { "Refresh" })
            .on_click(cx.listener(|this, _, _, cx| {
                this.refresh_provider_detection(None);
                cx.notify();
            }));

        let mut rows = div().mt(px(4.0)).flex().flex_col();
        let provider_count = ProviderKind::ALL.len();
        for (index, kind) in ProviderKind::ALL.into_iter().enumerate() {
            let probe = self.provider_probe(kind);
            let installed = probe.is_some_and(|probe| probe.installed);
            let compacted_path = probe
                .filter(|probe| probe.installed)
                .and_then(|probe| probe.path.as_deref())
                .map(compact_path);
            let model_count = probe.map(|probe| probe.models.len()).unwrap_or(0);
            let version = self
                .provider_versions
                .get(&kind)
                .and_then(|version| version.clone());
            let disabled = self.state.disabled_providers.contains(&kind);

            let dot_color = if !installed {
                theme.text_ghost
            } else if disabled {
                theme.warning
            } else {
                theme.success
            };

            let detail: AnyElement = if installed {
                let mut parts = Vec::new();
                if let Some(path) = compacted_path {
                    parts.push(path);
                }
                if disabled {
                    parts.push("Disabled for new sessions".to_owned());
                } else if model_count > 0 {
                    parts.push(format!(
                        "{model_count} {}",
                        if model_count == 1 { "model" } else { "models" }
                    ));
                }
                div()
                    .truncate()
                    .child(SharedString::from(parts.join("  ·  ")))
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .items_baseline()
                    .child("Not detected on PATH as ")
                    .child(
                        div()
                            .font_family(crate::md::render::MONO_FAMILY)
                            .text_color(theme.text_secondary)
                            .child(kind.command()),
                    )
                    .child(".")
                    .into_any_element()
            };

            let toggle_on = !disabled;
            let toggle = div()
                .id(SharedString::from(format!(
                    "provider-enabled-{}",
                    kind.id()
                )))
                .tab_index(0)
                .focus_visible(|style| style.border_color(theme.accent))
                .w(px(36.0))
                .h(px(20.0))
                .p(px(2.0))
                .flex_none()
                .rounded_full()
                .cursor_default()
                .bg(if toggle_on {
                    theme.inverse
                } else {
                    theme.inset
                })
                .border_1()
                .border_color(if toggle_on {
                    theme.inverse
                } else {
                    theme.border_strong
                })
                .flex()
                .items_center()
                .when(toggle_on, |element| element.justify_end())
                .child(
                    div()
                        .w(px(14.0))
                        .h(px(14.0))
                        .rounded_full()
                        .bg(if toggle_on {
                            theme.on_inverse
                        } else {
                            theme.text_tertiary
                        }),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_provider_enabled(kind, disabled, cx);
                }));

            let expanded = self.expanded_provider_settings == Some(kind);
            let expand_button = icon_button(
                SharedString::from(format!("provider-expand-{}", kind.id())),
                if expanded {
                    "icons/chevron-down.svg"
                } else {
                    "icons/chevron-right.svg"
                },
                theme,
            )
            .tab_index(0)
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.toggle_provider_expanded(kind, window, cx);
            }));

            let header = div()
                .flex()
                .items_center()
                .gap(px(12.0))
                .child(
                    div()
                        .relative()
                        .w(px(30.0))
                        .h(px(30.0))
                        .flex_none()
                        .rounded(px(7.0))
                        .bg(theme.overlay)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(
                            provider_icon(kind),
                            16.0,
                            provider_color(&theme, kind).opacity(if installed { 1.0 } else { 0.5 }),
                        ))
                        .child(
                            div()
                                .absolute()
                                .bottom(px(-2.0))
                                .right(px(-2.0))
                                .w(px(10.0))
                                .h(px(10.0))
                                .rounded_full()
                                .border_2()
                                .border_color(theme.raised)
                                .bg(dot_color),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .flex()
                                .items_baseline()
                                .gap(px(7.0))
                                .child(
                                    div()
                                        .text_size(px(12.5))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(if installed {
                                            theme.text
                                        } else {
                                            theme.text_secondary
                                        })
                                        .child(kind.display_name()),
                                )
                                .when_some(version, |element, version| {
                                    element.child(
                                        div()
                                            .font_family(crate::md::render::MONO_FAMILY)
                                            .text_size(px(10.0))
                                            .text_color(theme.text_tertiary)
                                            .child(SharedString::from(format!("v{version}"))),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .mt(px(3.0))
                                .text_size(px(10.5))
                                .text_color(theme.text_tertiary)
                                .child(detail),
                        ),
                )
                .child(expand_button)
                .when(installed, |element| element.child(toggle));

            rows = rows.child(
                div()
                    .py(px(11.0))
                    .flex()
                    .flex_col()
                    .when(index + 1 != provider_count, |element| {
                        element.border_b_1().border_color(theme.border)
                    })
                    .child(header)
                    .when(expanded, |element| {
                        element.child(self.render_provider_expanded_settings(kind, theme, cx))
                    }),
            );
        }

        div()
            .mt(px(15.0))
            .w_full()
            .px(px(20.0))
            .py(px(14.0))
            .rounded(px(13.0))
            .bg(theme.raised)
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(20.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child("Coding agents"),
                            )
                            .child(
                                div()
                                    .mt(px(5.0))
                                    .text_size(px(12.0))
                                    .line_height(px(18.0))
                                    .text_color(theme.text_secondary)
                                    .child(
                                        "Waku drives agent CLIs installed on this Mac. \
                                         Install or sign in with each agent's own CLI, \
                                         then refresh.",
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .flex_col()
                            .items_end()
                            .gap(px(6.0))
                            .child(refresh)
                            .when_some(checked_label, |element, label| {
                                element.child(
                                    div()
                                        .text_size(px(9.5))
                                        .text_color(theme.text_ghost)
                                        .child(SharedString::from(label)),
                                )
                            }),
                    ),
            )
            .child(rows)
            .into_any_element()
    }

    /// The expanded row's settings body: the binary override for this
    /// provider, with the detection result as its caption.
    fn render_provider_expanded_settings(
        &self,
        kind: ProviderKind,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let override_value = self.state.provider_binary_overrides.get(&kind).cloned();
        let full_path = self
            .provider_probe(kind)
            .filter(|probe| probe.installed)
            .and_then(|probe| probe.path.as_ref())
            .map(|path| path.display().to_string());

        let caption = match (&override_value, full_path) {
            (Some(_), Some(path)) => format!("Using {path} instead of PATH detection."),
            (Some(_), None) => {
                "Nothing runnable at this path. Clear it to detect from PATH.".to_owned()
            }
            (None, Some(path)) => format!("Detected at {path}."),
            (None, None) => format!("Not detected. Waku searches PATH for {}.", kind.command()),
        };

        let reset = div()
            .id(SharedString::from(format!(
                "provider-path-reset-{}",
                kind.id()
            )))
            .tab_index(0)
            .focus_visible(|style| style.border_color(theme.accent))
            .h(px(29.0))
            .px(px(10.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .flex_none()
            .items_center()
            .cursor_default()
            .text_size(px(10.5))
            .text_color(theme.text_secondary)
            .hover(|element| element.bg(theme.overlay))
            .child("Reset")
            .on_click(cx.listener(|this, _, _, cx| {
                this.provider_path_input
                    .update(cx, |input, cx| input.clear(cx));
                this.apply_provider_path_override(cx);
            }));

        div()
            .mt(px(10.0))
            .pl(px(42.0))
            .flex()
            .flex_col()
            .gap(px(5.0))
            .child(
                div()
                    .text_size(px(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child("Binary path"),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .line_height(px(15.0))
                    .text_color(theme.text_tertiary)
                    .child(SharedString::from(format!(
                        "The executable Waku launches for {}. Press enter to apply; \
                         leave empty to detect from PATH.",
                        kind.short_name()
                    ))),
            )
            .child(
                div()
                    .mt(px(3.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        TextField::new(
                            SharedString::from(format!("provider-path-field-{}", kind.id())),
                            self.provider_path_input.clone(),
                        )
                        .flex_1()
                        .max_w(px(430.0)),
                    )
                    .when(override_value.is_some(), |element| element.child(reset)),
            )
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(theme.text_ghost)
                    .child(SharedString::from(caption)),
            )
    }

    fn toggle_provider_expanded(
        &mut self,
        provider: ProviderKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Commit any pending edit for the previously expanded provider before
        // the input is handed to another row.
        self.apply_provider_path_override(cx);
        if self.expanded_provider_settings == Some(provider) {
            self.expanded_provider_settings = None;
        } else {
            self.expanded_provider_settings = Some(provider);
            let override_value = self
                .state
                .provider_binary_overrides
                .get(&provider)
                .cloned()
                .unwrap_or_default();
            self.provider_path_input
                .update(cx, |input, cx| input.set_content(override_value, cx));
            let focus = self.provider_path_input.read(cx).focus();
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    /// Commit the binary override edit for the expanded provider: empty means
    /// detect from PATH. Re-detects that provider, which in turn refreshes its
    /// version and model catalog.
    pub(super) fn apply_provider_path_override(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = self.expanded_provider_settings else {
            return;
        };
        let text = self
            .provider_path_input
            .read(cx)
            .content()
            .trim()
            .to_owned();
        let current = self
            .state
            .provider_binary_overrides
            .get(&provider)
            .cloned()
            .unwrap_or_default();
        if text == current {
            return;
        }
        if text.is_empty() {
            self.state.provider_binary_overrides.remove(&provider);
        } else {
            self.state.provider_binary_overrides.insert(provider, text);
        }
        self.save();
        self.refresh_provider_detection(Some(provider));
        cx.notify();
    }

    /// Providers switched off here stop offering models to new sessions;
    /// sessions already locked to them keep working.
    fn set_provider_enabled(
        &mut self,
        provider: ProviderKind,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        if enabled {
            self.state
                .disabled_providers
                .retain(|kind| *kind != provider);
        } else if !self.state.disabled_providers.contains(&provider) {
            self.state.disabled_providers.push(provider);
        }
        if !enabled
            && let Some(fallback) = ProviderKind::ALL
                .into_iter()
                .find(|kind| self.provider_enabled(*kind))
        {
            // New work must land somewhere usable: move the new-session
            // default and any unstarted drafts off the switched-off provider.
            // The remembered model belongs to the old provider, so it resets
            // with it.
            if self.state.last_provider == provider {
                self.state.last_provider = fallback;
                self.state.last_model = None;
                self.state.last_reasoning_effort = None;
                self.state.last_service_tier = None;
            }
            let draft_ids = self
                .state
                .sessions
                .iter()
                .filter(|session| session.provider == provider && !session.has_started())
                .map(|session| session.id)
                .collect::<Vec<_>>();
            for id in draft_ids {
                if let Some(session) = self.state.session_mut(id) {
                    session.provider = fallback;
                    session.model = None;
                    session.reasoning_effort = None;
                    session.service_tier = None;
                }
            }
        }
        self.save();
        cx.notify();
    }

    fn render_computer_use_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let enabled = self.state.computer_use_enabled;
        let permissions = self.computer_permissions.clone();
        let pending = self.computer_permission_request_pending;
        let helper_name = crate::computer_use::helper_display_name();
        let mut allowed_apps = div().flex().flex_col().gap(px(1.0));
        if self.state.computer_use_allowed_apps.is_empty() {
            allowed_apps = allowed_apps.child(
                div()
                    .py(px(12.0))
                    .text_size(px(11.5))
                    .text_color(theme.text_tertiary)
                    .child("No apps are always allowed. Task-scoped grants stay in memory only."),
            );
        } else {
            for (index, grant) in self.state.computer_use_allowed_apps.iter().enumerate() {
                let key = grant.key();
                let is_last = index + 1 == self.state.computer_use_allowed_apps.len();
                let app_icon = self.computer_use_app_icon(&grant.bundle_id, cx);
                allowed_apps = allowed_apps.child(
                    div()
                        .py(px(9.0))
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .when(!is_last, |element| {
                            element.border_b_1().border_color(theme.border)
                        })
                        .child(
                            div()
                                .w(px(32.0))
                                .h(px(32.0))
                                .flex_none()
                                .rounded(px(7.0))
                                .when_some(app_icon, |element, app_icon| {
                                    element.child(img(app_icon).size_full().rounded(px(7.0)))
                                }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.text)
                                        .child(SharedString::from(grant.app_name.clone())),
                                )
                                .child(
                                    div()
                                        .mt(px(2.0))
                                        .text_size(px(9.5))
                                        .text_color(theme.text_tertiary)
                                        .truncate()
                                        .child(SharedString::from(grant.bundle_id.clone())),
                                ),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("revoke-computer-app-{key}")))
                                .h(px(25.0))
                                .px(px(9.0))
                                .rounded(px(6.0))
                                .border_1()
                                .border_color(theme.border_strong)
                                .flex()
                                .items_center()
                                .cursor_default()
                                .text_size(px(10.5))
                                .text_color(theme.text_secondary)
                                .hover(|element| element.bg(theme.overlay).text_color(theme.danger))
                                .child("Revoke")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.revoke_computer_app(&key, cx);
                                })),
                        ),
                );
            }
        }

        div()
            .mt(px(15.0))
            .w_full()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .px(px(20.0))
                    .py(px(14.0))
                    .rounded(px(13.0))
                    .bg(theme.raised)
                    .flex()
                    .items_center()
                    .gap(px(20.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child("Let Waku use apps"),
                            )
                            .child(
                                div()
                                    .mt(px(5.0))
                                    .text_size(px(12.0))
                                    .line_height(px(18.0))
                                    .text_color(theme.text_secondary)
                                    .child(
                                        "Computer Use is available with Codex, OpenCode, Grok, and Pi.",
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("computer-use-enabled")
                            .w(px(36.0))
                            .h(px(20.0))
                            .p(px(2.0))
                            .rounded_full()
                            .cursor_default()
                            .bg(if enabled { theme.inverse } else { theme.inset })
                            .border_1()
                            .border_color(if enabled { theme.inverse } else { theme.border_strong })
                            .flex()
                            .items_center()
                            .when(enabled, |element| element.justify_end())
                            .child(
                                div()
                                    .w(px(14.0))
                                    .h(px(14.0))
                                    .rounded_full()
                                    .bg(if enabled { theme.on_inverse } else { theme.text_tertiary }),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_computer_use_enabled(!enabled, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .px(px(20.0))
                    .py(px(14.0))
                    .rounded(px(13.0))
                    .bg(theme.raised)
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child("macOS access"),
                    )
                    .child(
                        div()
                            .mt(px(4.0))
                            .text_size(px(11.5))
                            .text_color(theme.text_secondary)
                            .child(SharedString::from(format!(
                                "Grant access to {helper_name}, Waku's isolated control helper."
                            ))),
                    )
                    .child(permission_status_row(
                        "Screen Recording",
                        "Captures only the approved app window.",
                        permissions.screen_recording,
                        "screen-recording-settings",
                        theme,
                        cx,
                    ))
                    .child(permission_status_row(
                        "Accessibility",
                        "Posts pointer and keyboard events after approval.",
                        permissions.accessibility,
                        "accessibility-settings",
                        theme,
                        cx,
                    ))
                    .child(
                        div()
                            .mt(px(11.0))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .id("recheck-computer-permissions")
                                    .h(px(28.0))
                                    .px(px(11.0))
                                    .rounded(px(7.0))
                                    .border_1()
                                    .border_color(theme.border_strong)
                                    .text_color(theme.text_secondary)
                                    .flex()
                                    .items_center()
                                    .cursor_default()
                                    .text_size(px(10.5))
                                    .opacity(if pending { 0.6 } else { 1.0 })
                                    .child(if pending { "Checking…" } else { "Recheck" })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.request_computer_permissions(false, cx);
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .px(px(20.0))
                    .py(px(14.0))
                    .rounded(px(13.0))
                    .bg(theme.raised)
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child("Always allowed apps"),
                    )
                    .child(
                        div()
                            .mt(px(4.0))
                            .text_size(px(11.5))
                            .text_color(theme.text_secondary)
                            .child("Waku pins these grants to the app's bundle ID and signing team."),
                    )
                    .child(allowed_apps),
            )
            .into_any_element()
    }

    fn set_computer_use_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.state.computer_use_enabled = enabled;
        self.save();
        if enabled {
            self.request_computer_permissions(true, cx);
        }
        cx.notify();
    }

    pub(super) fn request_computer_permissions(&mut self, prompt: bool, cx: &mut Context<Self>) {
        if self.computer_permission_request_pending {
            return;
        }
        self.computer_permission_request_pending = true;
        let tx = self.computer_permission_tx.clone();
        std::thread::Builder::new()
            .name("waku-computer-permission-request".into())
            .spawn(move || {
                let result = crate::computer_use::probe_permissions(prompt)
                    .map_err(|error| error.to_string());
                let _ = tx.send(result);
            })
            .ok();
        cx.notify();
    }

    fn revoke_computer_app(&mut self, key: &str, cx: &mut Context<Self>) {
        self.state
            .computer_use_allowed_apps
            .retain(|grant| grant.key() != key);
        self.save();
        cx.notify();
    }

    fn computer_use_app_icon(
        &self,
        bundle_id: &str,
        cx: &mut Context<Self>,
    ) -> Option<std::sync::Arc<gpui::Image>> {
        if let Some(icon) = self.computer_use_app_icons.borrow().get(bundle_id) {
            return icon.clone();
        }

        let bundle_id = bundle_id.to_owned();
        if self
            .computer_use_app_icon_loads
            .borrow_mut()
            .insert(bundle_id.clone())
        {
            cx.spawn(async move |this, cx| {
                let load_bundle_id = bundle_id.clone();
                let icon =
                    cx.background_executor()
                        .spawn(async move {
                            crate::platform::load_app_icon_for_bundle_id(&load_bundle_id)
                        })
                        .await;
                let _ = this.update(cx, |this, cx| {
                    this.computer_use_app_icon_loads
                        .borrow_mut()
                        .remove(&bundle_id);
                    this.computer_use_app_icons
                        .borrow_mut()
                        .insert(bundle_id, icon);
                    cx.notify();
                });
            })
            .detach();
        }
        None
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

/// "Checked …" caption for the Providers page. Recomputed whenever the page
/// redraws; precision beyond the minute is noise here.
fn detection_checked_label(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 90 {
        "Checked just now".to_owned()
    } else if seconds < 3600 {
        format!("Checked {}m ago", seconds / 60)
    } else {
        format!("Checked {}h ago", seconds / 3600)
    }
}

fn permission_status_row(
    name: &'static str,
    description: &'static str,
    granted: bool,
    id: &'static str,
    theme: Theme,
    cx: &mut Context<Waku>,
) -> Div {
    let status = if granted {
        div()
            .id(id)
            .h(px(25.0))
            .px(px(4.0))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .cursor_default()
            .text_size(px(10.0))
            .text_color(theme.success)
            .child(icon("icons/check.svg", 12.0, theme.success))
            .child("Access Granted")
    } else {
        div()
            .id(id)
            .h(px(25.0))
            .px(px(9.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .items_center()
            .cursor_default()
            .text_size(px(10.0))
            .text_color(theme.text_secondary)
            .hover(|element| element.bg(theme.overlay).text_color(theme.text))
            .child("Grant Access")
            .on_click(cx.listener(move |this, _, _, cx| {
                this.request_computer_permissions(true, cx);
            }))
    };

    div()
        .mt(px(10.0))
        .pt(px(10.0))
        .border_t_1()
        .border_color(theme.border)
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(11.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(name),
                )
                .child(
                    div()
                        .mt(px(2.0))
                        .text_size(px(10.0))
                        .text_color(theme.text_tertiary)
                        .child(description),
                ),
        )
        .child(status)
}
