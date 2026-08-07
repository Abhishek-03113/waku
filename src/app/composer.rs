use super::*;

impl Waku {
    // ── Permission ─────────────────────────────────────────────────────────

    pub(super) fn render_permission(&self, cx: &mut Context<Self>) -> Option<Div> {
        if let Some(permission) = self.selected_runtime()?.pending_computer_approval.as_ref() {
            return Some(self.render_computer_permission(permission, cx));
        }
        let permission = self.selected_runtime()?.pending_permission.as_ref()?;
        let theme = Theme::current(cx);
        let request_id = permission.request_id.clone();
        let mut buttons = div().flex().items_center().gap(px(8.0)).mt(px(10.0));
        for option in &permission.options {
            let request_id = request_id.clone();
            let option_id = option.id.clone();
            let allow = option.allow;
            buttons = buttons.child(
                div()
                    .id(SharedString::from(format!(
                        "permission-{}-{}",
                        permission.request_id, option.id
                    )))
                    .h(px(28.0))
                    .px(px(13.0))
                    .rounded(px(7.0))
                    .flex()
                    .items_center()
                    .cursor_default()
                    .text_size(px(11.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(allow, |element| {
                        element
                            .bg(theme.inverse)
                            .text_color(theme.on_inverse)
                            .hover(|element| element.opacity(0.9))
                    })
                    .when(!allow, |element| {
                        element
                            .border_1()
                            .border_color(theme.border_strong)
                            .text_color(theme.text_secondary)
                            .hover(|element| element.bg(theme.overlay).text_color(theme.text))
                    })
                    .active(|element| element.opacity(0.8))
                    .child(SharedString::from(option.label.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.respond_permission(request_id.clone(), option_id.clone(), cx);
                    })),
            );
        }
        Some(
            div().px(px(20.0)).pb(px(8.0)).child(
                div()
                    .w_full()
                    .max_w(px(CONTENT_MAX_WIDTH))
                    .mx_auto()
                    .p(px(12.0))
                    .rounded(px(12.0))
                    .border_1()
                    .border_color(theme.border_strong)
                    .bg(theme.raised)
                    .shadow_md()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(icon("icons/alert.svg", 13.0, theme.warning))
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(SharedString::from(permission.title.clone())),
                            ),
                    )
                    .child(
                        div()
                            .id("permission-detail")
                            .mt(px(8.0))
                            .max_h(px(92.0))
                            .overflow_y_scroll()
                            .p(px(8.0))
                            .rounded(px(7.0))
                            .bg(theme.inset)
                            .font_family(crate::md::render::MONO_FAMILY)
                            .text_size(px(10.5))
                            .line_height(px(16.0))
                            .text_color(theme.text_secondary)
                            .whitespace_normal()
                            .child(SharedString::from(permission.detail.clone())),
                    )
                    .child(buttons),
            ),
        )
    }

    fn render_computer_permission(
        &self,
        permission: &PendingComputerApproval,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let target = &permission.target;
        let mut buttons = div().mt(px(12.0)).flex().items_center().gap(px(8.0));
        let mut options = vec![("task", "Allow for task", true), ("deny", "Deny", false)];
        if target.persistable() {
            options.insert(1, ("always", "Always allow app", false));
        }
        for (decision, label, primary) in options {
            buttons = buttons.child(
                div()
                    .id(SharedString::from(format!(
                        "computer-permission-{}-{decision}",
                        permission.request.call_id
                    )))
                    .h(px(29.0))
                    .px(px(13.0))
                    .rounded(px(7.0))
                    .flex()
                    .items_center()
                    .cursor_default()
                    .text_size(px(11.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(primary, |element| {
                        element
                            .bg(theme.inverse)
                            .text_color(theme.on_inverse)
                            .hover(|element| element.opacity(0.9))
                    })
                    .when(!primary, |element| {
                        element
                            .border_1()
                            .border_color(theme.border_strong)
                            .text_color(theme.text_secondary)
                            .hover(|element| element.bg(theme.overlay).text_color(theme.text))
                    })
                    .active(|element| element.opacity(0.8))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.respond_computer_permission(decision, cx);
                    })),
            );
        }

        div().px(px(20.0)).pb(px(8.0)).child(
            div()
                .w_full()
                .max_w(px(CONTENT_MAX_WIDTH))
                .mx_auto()
                .p(px(13.0))
                .rounded(px(12.0))
                .border_1()
                .border_color(theme.warning.opacity(0.5))
                .bg(theme.raised)
                .shadow_md()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(9.0))
                        .child(icon("icons/globe.svg", 14.0, theme.warning))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(format!("Allow Waku to control {}?", target.app_name)),
                        ),
                )
                .child(
                    div()
                        .mt(px(7.0))
                        .text_size(px(10.0))
                        .line_height(px(14.0))
                        .text_color(theme.text_secondary)
                        .child("A screenshot of this window will be shared with the active Codex model."),
                )
                .child(
                    div()
                        .mt(px(8.0))
                        .p(px(9.0))
                        .rounded(px(8.0))
                        .bg(theme.inset)
                        .child(
                            div()
                                .text_size(px(11.5))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .truncate()
                                .child(SharedString::from(target.window_title.clone())),
                        )
                        .child(
                            div()
                                .mt(px(4.0))
                                .text_size(px(10.5))
                                .text_color(theme.text_secondary)
                                .child(SharedString::from(permission.request.summary())),
                        )
                        .when(permission.sensitive, |element| {
                            element.child(
                                div()
                                    .mt(px(5.0))
                                    .text_size(px(10.5))
                                    .text_color(theme.warning)
                                    .child("This action can type text or press keys."),
                            )
                        }),
                )
                .child(
                    div()
                        .mt(px(7.0))
                        .text_size(px(10.0))
                        .text_color(theme.text_tertiary)
                        .child(if target.persistable() {
                            format!("Bundle ID: {}", target.bundle_id)
                        } else {
                            "This app has no bundle identifier, so it cannot be always allowed."
                                .into()
                        }),
                )
                .child(buttons),
        )
    }

    pub(super) fn render_computer_use_overlay(&self, cx: &mut Context<Self>) -> Option<Div> {
        let previews = self
            .selected_runtime()?
            .computer_use_previews
            .iter()
            .filter(|state| state.visible && state.phase != ComputerUsePhase::AwaitingApproval)
            .collect::<Vec<_>>();
        if previews.is_empty() {
            return None;
        }
        let theme = Theme::current(cx);
        let stack_x_offset = 14.0;
        let stack_y_offset = 24.0;
        let deepest_x_offset = (previews.len().saturating_sub(1) as f32) * stack_x_offset;
        let deepest_y_offset = (previews.len().saturating_sub(1) as f32) * stack_y_offset;
        let top_index = previews.len() - 1;
        let cards = previews
            .into_iter()
            .enumerate()
            .filter_map(|(index, state)| {
                let target = state.target.as_ref()?;
                let window_id = target.window_id;
                let app_name = target.app_name.clone();
                let app_initial = app_name.chars().next().unwrap_or('W').to_string();
                let title = target.window_title.clone();
                let screenshot = state.screenshot.clone();
                let active = state.phase == ComputerUsePhase::Running;
                let failed = state.phase == ComputerUsePhase::Failed;
                let is_top = index == top_index;
                let depth = (top_index - index) as f32;
                let x_offset = depth * stack_x_offset;
                let y_offset = depth * stack_y_offset;
                let status_color = if failed {
                    theme.danger
                } else if active {
                    theme.warning
                } else {
                    theme.accent
                };
                let status = if failed {
                    "Stopped"
                } else if active {
                    "Controlling"
                } else {
                    "Captured"
                };

                Some(
                    div()
                        .id(SharedString::from(format!(
                            "computer-use-preview-{window_id}"
                        )))
                        .absolute()
                        .right(px(x_offset))
                        .bottom(px(y_offset))
                        .w(px(304.0))
                        .h(px(220.0))
                        .p(px(6.0))
                        .rounded(px(16.0))
                        .overflow_hidden()
                        .border_1()
                        .border_color(if is_top {
                            theme.border_strong
                        } else {
                            theme.border
                        })
                        .bg(theme.raised)
                        .shadow_lg()
                        .cursor_default()
                        .when(!is_top, |element| element.opacity(0.96))
                        .child(
                            div()
                                .h(px(38.0))
                                .px(px(5.0))
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .w(px(27.0))
                                        .h(px(27.0))
                                        .rounded(px(7.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .bg(theme.overlay_strong)
                                        .text_size(px(11.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.text_secondary)
                                        .child(SharedString::from(app_initial)),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .text_size(px(11.5))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(theme.text)
                                                .truncate()
                                                .child(SharedString::from(app_name)),
                                        )
                                        .child(
                                            div()
                                                .mt(px(1.0))
                                                .text_size(px(9.5))
                                                .text_color(theme.text_tertiary)
                                                .truncate()
                                                .child(SharedString::from(title)),
                                        ),
                                )
                                .when(is_top, |element| {
                                    element.child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "computer-use-preview-action-{window_id}"
                                            )))
                                            .h(px(27.0))
                                            .px(px(10.0))
                                            .rounded(px(7.0))
                                            .border_1()
                                            .border_color(theme.border_strong)
                                            .flex()
                                            .items_center()
                                            .cursor_default()
                                            .text_size(px(10.0))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(if active {
                                                theme.danger
                                            } else {
                                                theme.text_secondary
                                            })
                                            .hover(|element| element.bg(theme.overlay))
                                            .active(|element| element.opacity(0.8))
                                            .child(if active { "Take Control" } else { "Close" })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                if active {
                                                    this.cancel_turn(cx);
                                                } else {
                                                    this.dismiss_computer_use(window_id, cx);
                                                }
                                            })),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .relative()
                                .h(px(170.0))
                                .w_full()
                                .rounded(px(11.0))
                                .overflow_hidden()
                                .bg(rgb(0x101010))
                                .when_some(screenshot, |element, screenshot| {
                                    element.child(
                                        img(screenshot)
                                            .w_full()
                                            .h_full()
                                            .object_fit(ObjectFit::Contain),
                                    )
                                })
                                .when(state.screenshot.is_none(), |element| {
                                    element.child(
                                        div()
                                            .absolute()
                                            .inset_0()
                                            .flex()
                                            .flex_col()
                                            .items_center()
                                            .justify_center()
                                            .gap(px(9.0))
                                            .child(
                                                div()
                                                    .w(px(34.0))
                                                    .h(px(23.0))
                                                    .rounded(px(5.0))
                                                    .border_1()
                                                    .border_color(theme.text_tertiary)
                                                    .child(
                                                        div()
                                                            .mt(px(4.0))
                                                            .ml(px(25.0))
                                                            .w(px(3.0))
                                                            .h(px(3.0))
                                                            .rounded_full()
                                                            .bg(status_color),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(10.0))
                                                    .text_color(theme.text_tertiary)
                                                    .child("Preparing preview…"),
                                            ),
                                    )
                                })
                                .child(
                                    div()
                                        .absolute()
                                        .top(px(8.0))
                                        .left(px(8.0))
                                        .h(px(24.0))
                                        .px(px(8.0))
                                        .rounded_full()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.0))
                                        .bg(theme.canvas.opacity(0.86))
                                        .border_1()
                                        .border_color(theme.border)
                                        .child(
                                            div()
                                                .w(px(6.0))
                                                .h(px(6.0))
                                                .rounded_full()
                                                .bg(status_color),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(9.5))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(theme.text)
                                                .child(status),
                                        ),
                                ),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.bring_computer_use_to_front(window_id, cx);
                        })),
                )
            })
            .collect::<Vec<_>>();

        Some(
            div()
                .absolute()
                .right(px(16.0))
                .bottom(px(82.0))
                .w(px(304.0 + deepest_x_offset))
                .h(px(220.0 + deepest_y_offset))
                .children(cards),
        )
    }

    // ── Composer ───────────────────────────────────────────────────────────

    pub(super) fn render_provider_model_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let session = self.selected_session();
        let provider = session.map(|session| session.provider).unwrap_or_default();
        let selected_model = session.and_then(|session| self.model_for_session(session));
        let selected_model_name = self.model_display_name(provider, selected_model);
        let locked_provider = session
            .filter(|session| !session.messages.is_empty())
            .map(|session| session.provider);
        let picker_enabled = session.is_some_and(|session| session.can_choose_model(provider));

        if !picker_enabled {
            return div()
                .h(px(24.0))
                .px(px(7.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(icon(
                    provider_icon(provider),
                    10.5,
                    provider_color(&theme, provider).opacity(0.9),
                ))
                .child(
                    div()
                        .max_w(px(210.0))
                        .truncate()
                        .text_color(theme.text_secondary)
                        .child(SharedString::from(selected_model_name)),
                )
                .into_any_element();
        }

        let search_query = self.model_search.read(cx).content().to_owned();
        let normalized_query = search_query.trim().to_ascii_lowercase();
        let searching = !normalized_query.is_empty();
        let selected_tab = self.model_picker_tab;
        let selected_model = selected_model.map(str::to_owned);
        let probes = self.probes.clone();
        let disabled_providers = self.state.disabled_providers.clone();
        let pending_discoveries = self.provider_model_discoveries_pending.clone();
        let favorites = self.state.favorite_models.clone();
        let weak = cx.entity().downgrade();
        let search = self.model_search.clone();
        let search_focus = search.read(cx).focus_handle(cx);

        let handle = {
            let reset_weak = weak.clone();
            let reset_search = search.clone();
            let picker_focus = search_focus.clone();
            self.menu_handle_with(MODEL_PICKER_MENU_ID, cx, move |open, window, cx| {
                let _ = reset_weak.update(cx, |this, cx| {
                    if open {
                        let provider = this
                            .selected_session()
                            .map(|session| session.provider)
                            .unwrap_or_default();
                        // A draft can sit on a provider that was since switched
                        // off; open onto the first usable provider instead of a
                        // tab whose rows the filter would leave empty.
                        let locked = this
                            .selected_session()
                            .is_some_and(|session| !session.messages.is_empty());
                        let provider = if !locked
                            && this.state.disabled_providers.contains(&provider)
                        {
                            ProviderKind::ALL
                                .into_iter()
                                .find(|kind| this.provider_enabled(*kind))
                                .unwrap_or(provider)
                        } else {
                            provider
                        };
                        this.model_picker_tab = ModelPickerTab::Provider(provider);
                        this.model_picker_highlight = None;
                        reset_search.update(cx, |search, cx| search.clear(cx));
                        this.reveal_selected_picker_model();
                    } else {
                        let focus_handle = this.composer.read(cx).focus();
                        window.focus(&focus_handle, cx);
                    }
                    cx.notify();
                });
                if open {
                    // The panel is deferred, so its input joins the dispatch
                    // tree only after the deferred draw — same two-frame wait
                    // the menus need before they can take focus. The reveal is
                    // re-issued here too: a parked scroll request resolves
                    // against the viewport bounds of the *previous* paint, so
                    // on the container's first-ever paint it reads a zeroed
                    // viewport, lands wrong, and is consumed. By this frame
                    // the panel has painted real bounds to resolve against.
                    let picker_focus = picker_focus.clone();
                    let reveal_weak = reset_weak.clone();
                    window.on_next_frame(move |window, _| {
                        window.on_next_frame(move |window, cx| {
                            window.focus(&picker_focus, cx);
                            let _ = reveal_weak.update(cx, |this, _| {
                                this.reveal_selected_picker_model();
                            });
                        });
                    });
                }
            })
        };

        // Only while the panel is open: this clones every installed provider's
        // model list, and the closed picker is on the composer's every frame.
        // Built out here rather than in the body so the key handler and the
        // rendered rows index one ordering and cannot disagree about what
        // `enter` selects.
        let available_models = Rc::new(if handle.is_open() {
            visible_picker_models(
                &probes,
                &favorites,
                &disabled_providers,
                locked_provider,
                selected_tab,
                &normalized_query,
            )
        } else {
            Vec::new()
        });
        let highlight = self
            .model_picker_highlight
            .filter(|index| *index < available_models.len());
        let scroll = self.model_picker_scroll.clone();

        popover(
            MenuChip::new("composer-provider-model")
                .icon(
                    provider_icon(provider),
                    provider_color(&theme, provider).opacity(0.9),
                )
                .label(selected_model_name)
                .selected(handle.is_open()),
            &handle,
            MenuAlign::AboveLeft,
            move |popover, _window, _cx| {
                let popover = popover.clone();
                let available_models = available_models.clone();

                let mut sidebar = div()
                    .w(px(50.0))
                    .h_full()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(4.0))
                    .p(px(5.0))
                    .rounded_tl(px(12.0))
                    .rounded_bl(px(12.0))
                    .bg(theme.canvas)
                    .border_r_1()
                    .border_color(theme.border);

                let favorites_selected = selected_tab == ModelPickerTab::Favorites && !searching;
                let favorite_weak = weak.clone();
                sidebar = sidebar
                    .child(
                        div()
                            .id("model-tab-favorites")
                            .w(px(38.0))
                            .h(px(38.0))
                            .rounded(px(7.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_default()
                            .when(favorites_selected, |element| {
                                element.bg(theme.overlay_strong)
                            })
                            .hover(|element| element.bg(theme.overlay))
                            .child(icon(
                                "icons/star.svg",
                                17.0,
                                if favorites_selected {
                                    theme.text
                                } else {
                                    theme.text_tertiary
                                },
                            ))
                            .on_click(move |_, _, cx| {
                                let _ = favorite_weak.update(cx, |this, cx| {
                                    this.select_model_picker_tab(ModelPickerTab::Favorites, cx);
                                });
                            }),
                    )
                    .child(div().w(px(34.0)).h(px(1.0)).my(px(3.0)).bg(theme.border));

                for kind in ProviderKind::ALL {
                    let installed = probes
                        .iter()
                        .find(|probe| probe.provider == kind)
                        .map(|probe| probe.installed)
                        .unwrap_or(false);
                    // A locked session keeps its own provider usable even if it
                    // was switched off afterwards; disabling is for new work.
                    let switched_off = disabled_providers.contains(&kind)
                        && locked_provider != Some(kind);
                    let allowed = (locked_provider.is_none() || locked_provider == Some(kind))
                        && !switched_off;
                    let selected = selected_tab == ModelPickerTab::Provider(kind) && !searching;
                    let tab_weak = weak.clone();
                    sidebar = sidebar.child(
                        div()
                            .id(SharedString::from(format!("model-tab-{}", kind.id())))
                            .w(px(38.0))
                            .h(px(38.0))
                            .rounded(px(7.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_default()
                            .when(selected, |element| element.bg(theme.overlay_strong))
                            .when(!installed || !allowed, |element| element.opacity(0.35))
                            .when(installed && allowed, |element| {
                                element.hover(|element| element.bg(theme.overlay)).on_click(
                                    move |_, _, cx| {
                                        let _ = tab_weak.update(cx, |this, cx| {
                                            this.select_model_picker_tab(
                                                ModelPickerTab::Provider(kind),
                                                cx,
                                            );
                                        });
                                    },
                                )
                            })
                            .child(icon(
                                provider_icon(kind),
                                18.0,
                                provider_color(&theme, kind).opacity(if selected {
                                    1.0
                                } else {
                                    0.82
                                }),
                            )),
                    );
                }

                let search_input = div()
                    .h(px(52.0))
                    .px(px(12.0))
                    .pt(px(10.0))
                    .pb(px(8.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .w_full()
                            .h(px(34.0))
                            .px(px(10.0))
                            .rounded(px(9.0))
                            .bg(theme.raised)
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(icon("icons/search.svg", 15.0, theme.text_secondary))
                            .child(div().flex_1().min_w_0().child(search.clone())),
                    );

                let mut rows = div()
                    .id("model-picker-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&scroll)
                    .p(px(9.0));
                if available_models.is_empty() {
                    let label = if searching {
                        "No models found"
                    } else if selected_tab == ModelPickerTab::Favorites {
                        "Star a model to keep it here"
                    } else if matches!(
                        selected_tab,
                        ModelPickerTab::Provider(provider)
                            if pending_discoveries.contains(&provider)
                    ) {
                        "Loading models…"
                    } else {
                        "No models reported by this provider"
                    };
                    rows = rows.child(
                        div()
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(11.5))
                            .text_color(theme.text_ghost)
                            .child(label),
                    );
                }

                for (row_index, (kind, model)) in available_models.iter().enumerate() {
                    let kind = *kind;
                    let is_selected =
                        kind == provider && selected_model.as_deref() == Some(model.id.as_str());
                    let is_highlighted = highlight == Some(row_index);
                    let is_favorite = favorites
                        .iter()
                        .any(|favorite| favorite.provider == kind && favorite.model == model.id);
                    let model_id = model.id.clone();
                    let select_weak = weak.clone();
                    let select_popover = popover.clone();
                    let favorite_model_id = model.id.clone();
                    let favorite_weak = weak.clone();
                    let subtitle = model.sub_provider.as_deref().map_or_else(
                        || kind.short_name().to_owned(),
                        |sub_provider| format!("{sub_provider} · {}", kind.short_name()),
                    );
                    rows = rows.child(
                        div()
                            .id(SharedString::from(format!(
                                "model-row-{}-{}",
                                kind.id(),
                                model.id
                            )))
                            .h(px(58.0))
                            .px(px(12.0))
                            .rounded(px(9.0))
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .cursor_default()
                            // Reserved on every row so highlighting one cannot
                            // resize it and shift the list by a pixel.
                            .border_1()
                            .border_color(gpui::transparent_black())
                            .when(is_selected, |element| element.bg(theme.overlay_strong))
                            // The keyboard cursor reads as a ring rather than a
                            // fill, so it stays legible on the current model's
                            // already-filled row.
                            .when(is_highlighted, |element| {
                                element.bg(theme.overlay).border_color(theme.accent)
                            })
                            .hover(|element| element.bg(theme.overlay))
                            .active(|element| element.opacity(0.85))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(13.0))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.text)
                                            .child(SharedString::from(model.name.clone())),
                                    )
                                    .child(
                                        div()
                                            .mt(px(4.0))
                                            .flex()
                                            .items_center()
                                            .gap(px(6.0))
                                            .child(icon(
                                                provider_icon(kind),
                                                10.5,
                                                provider_color(&theme, kind).opacity(0.85),
                                            ))
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_size(px(11.0))
                                                    .text_color(theme.text_tertiary)
                                                    .child(SharedString::from(subtitle)),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!(
                                        "favorite-model-{}-{}",
                                        kind.id(),
                                        model.id
                                    )))
                                    .w(px(28.0))
                                    .h(px(28.0))
                                    .rounded(px(6.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .hover(|element| element.bg(theme.overlay_strong))
                                    .child(icon(
                                        if is_favorite {
                                            "icons/star-filled.svg"
                                        } else {
                                            "icons/star.svg"
                                        },
                                        14.0,
                                        if is_favorite {
                                            theme.favorite
                                        } else {
                                            theme.text_ghost
                                        },
                                    ))
                                    .on_click(move |_, _, cx| {
                                        cx.stop_propagation();
                                        let _ = favorite_weak.update(cx, |this, cx| {
                                            this.toggle_favorite_model(
                                                kind,
                                                favorite_model_id.clone(),
                                                cx,
                                            );
                                        });
                                    }),
                            )
                            .on_click(move |_, window, cx| {
                                let _ = select_weak.update(cx, |this, cx| {
                                    this.choose_model(kind, model_id.clone(), cx);
                                });
                                select_popover.close(window, cx);
                            }),
                    );
                }

                let next_models = available_models.clone();
                let previous_models = available_models.clone();
                let confirm_models = available_models.clone();
                let next_weak = weak.clone();
                let previous_weak = weak.clone();
                let confirm_weak = weak.clone();
                let confirm_popover = popover.clone();
                div()
                    .w(px(460.0))
                    .h(px(390.0))
                    .rounded(px(13.0))
                    .overflow_hidden()
                    .border_1()
                    .border_color(theme.border_strong)
                    .bg(theme.raised)
                    .shadow_lg()
                    .flex()
                    // The filter field keeps focus and the selected row is only
                    // drawn, never focused — the same split Zed's picker uses.
                    // These arrive as actions bound to `WakuMenu > ComposerInput`,
                    // which is the only way to claim a key out from under a
                    // focused text field.
                    .on_action(move |_: &SelectNextEntry, _, cx| {
                        let _ = next_weak.update(cx, |this, cx| {
                            this.move_model_picker_highlight("down", &next_models, cx);
                        });
                    })
                    .on_action(move |_: &SelectPreviousEntry, _, cx| {
                        let _ = previous_weak.update(cx, |this, cx| {
                            this.move_model_picker_highlight("up", &previous_models, cx);
                        });
                    })
                    .on_action(move |_: &ConfirmEntry, window, cx| {
                        let _ = confirm_weak.update(cx, |this, cx| {
                            this.choose_highlighted_model(&confirm_models, cx);
                        });
                        confirm_popover.close(window, cx);
                        window.refresh();
                    })
                    .child(sidebar)
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .rounded_tr(px(12.0))
                            .rounded_br(px(12.0))
                            .bg(theme.surface)
                            .child(search_input)
                            .child(rows),
                    )
                    .into_any_element()
            },
        )
    }

    /// Move the picker's drawn selection. Nothing is focused: the filter field
    /// keeps focus so typing continues to narrow the list.
    fn move_model_picker_highlight(
        &mut self,
        key: &str,
        models: &[(ProviderKind, ProviderModel)],
        cx: &mut Context<Self>,
    ) {
        let current = self
            .model_picker_highlight
            .filter(|index| *index < models.len());
        let Some(next) = next_picker_highlight(current, models.len(), key) else {
            return;
        };
        self.model_picker_highlight = Some(next);
        self.model_picker_scroll.scroll_to_item(next);
        cx.notify();
    }

    /// Bring the current model's row into view whenever the picker shows the
    /// unfiltered list — on open, on a cleared query, and on tab switches.
    ///
    /// The request parks in the scroll handle until the row list next paints,
    /// so it may be issued from the open toggle before the deferred panel
    /// exists, and a tab whose models are still loading reveals the row once
    /// they arrive. Without a row to reveal it falls back to the top, so a
    /// scroll offset from an earlier open never leaks into a fresh list.
    pub(super) fn reveal_selected_picker_model(&self) {
        let session = self.selected_session();
        let provider = session.map(|session| session.provider).unwrap_or_default();
        let selected_model = session.and_then(|session| self.model_for_session(session));
        let locked_provider = session
            .filter(|session| !session.messages.is_empty())
            .map(|session| session.provider);
        let index = visible_picker_models(
            &self.probes,
            &self.state.favorite_models,
            &self.state.disabled_providers,
            locked_provider,
            self.model_picker_tab,
            "",
        )
        .iter()
        .position(|(kind, model)| *kind == provider && selected_model == Some(model.id.as_str()))
        .unwrap_or(0);
        self.model_picker_scroll.scroll_to_item(index);
    }

    /// Take the row the selection is on, defaulting to the first so `enter`
    /// works the moment the panel opens.
    fn choose_highlighted_model(
        &mut self,
        models: &[(ProviderKind, ProviderModel)],
        cx: &mut Context<Self>,
    ) {
        let Some((kind, model)) = models.get(self.model_picker_highlight.unwrap_or(0)) else {
            return;
        };
        let (kind, model_id) = (*kind, model.id.clone());
        self.choose_model(kind, model_id, cx);
    }

    pub(super) fn render_model_traits_control(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = Theme::current(cx);
        let session = self.selected_session()?;
        let model = self.model_metadata_for_session(session)?;
        if model.reasoning_efforts.is_empty() && model.service_tiers.is_empty() {
            return None;
        }

        let selected_effort = session
            .reasoning_effort
            .as_deref()
            .filter(|selected| {
                model
                    .reasoning_efforts
                    .iter()
                    .any(|option| option.id == *selected)
            })
            .or(model.default_reasoning_effort.as_deref())
            .or_else(|| {
                model
                    .reasoning_efforts
                    .first()
                    .map(|option| option.id.as_str())
            })
            .map(str::to_owned);
        let effort_label = selected_effort.as_deref().and_then(|selected| {
            model
                .reasoning_efforts
                .iter()
                .find(|option| option.id == selected)
                .map(|option| option.label.clone())
        });

        let selected_tier = session
            .service_tier
            .as_deref()
            .filter(|selected| {
                *selected == "default"
                    || model
                        .service_tiers
                        .iter()
                        .any(|option| option.id == *selected)
            })
            .or(model.default_service_tier.as_deref())
            .unwrap_or("default")
            .to_owned();
        let tier_label = if selected_tier == "default" {
            "Standard".to_owned()
        } else {
            model
                .service_tiers
                .iter()
                .find(|option| option.id == selected_tier)
                .map(|option| option.label.clone())
                .unwrap_or_else(|| selected_tier.clone())
        };
        let fast = selected_tier == "fast" || tier_label.eq_ignore_ascii_case("fast");
        let trigger_label = effort_label.unwrap_or_else(|| tier_label.clone());
        let reasoning_efforts = model.reasoning_efforts.clone();
        let default_effort = model.default_reasoning_effort.clone();
        let service_tiers = model.service_tiers.clone();
        let default_tier = model
            .default_service_tier
            .clone()
            .unwrap_or_else(|| "default".to_owned());
        let weak = cx.entity().downgrade();
        let handle = self.menu_handle("model-traits", cx);
        Some(dropdown_menu(
            MenuChip::new("model-traits")
                .when(fast, |trigger| {
                    trigger.icon("icons/zap.svg", theme.text_secondary)
                })
                .label(trigger_label)
                .selected(handle.is_open()),
            "model-traits-menu",
            &handle,
            MenuAlign::AboveLeft,
            move |_| {
                let mut items = Vec::new();
                if !reasoning_efforts.is_empty() {
                    items.push(MenuItem::Header("Reasoning".into()));
                    for option in reasoning_efforts.clone() {
                        let weak = weak.clone();
                        let effort = option.id;
                        let is_default = default_effort.as_deref() == Some(effort.as_str());
                        items.push(
                            traits_choice(theme, option.label, is_default)
                                .selected(selected_effort.as_deref() == Some(effort.as_str()))
                                .on_click(move |_, cx| {
                                    let _ = weak.update(cx, |this, cx| {
                                        this.set_reasoning_effort(effort.clone(), cx);
                                    });
                                }),
                        );
                    }
                }
                if !service_tiers.is_empty() {
                    if !reasoning_efforts.is_empty() {
                        items.push(MenuItem::Separator);
                    }
                    items.push(MenuItem::Header("Service Tier".into()));
                    let weak_standard = weak.clone();
                    items.push(
                        traits_choice(theme, "Standard".to_owned(), default_tier == "default")
                            .selected(selected_tier == "default")
                            .on_click(move |_, cx| {
                                let _ = weak_standard.update(cx, |this, cx| {
                                    this.set_service_tier("default".to_owned(), cx);
                                });
                            }),
                    );
                    for option in service_tiers.clone() {
                        let weak = weak.clone();
                        let tier = option.id;
                        let is_default = default_tier == tier;
                        items.push(
                            traits_choice(theme, option.label, is_default)
                                .selected(selected_tier == tier)
                                .on_click(move |_, cx| {
                                    let _ = weak.update(cx, |this, cx| {
                                        this.set_service_tier(tier.clone(), cx);
                                    });
                                }),
                        );
                    }
                }
                items
            },
        ))
    }

    pub(super) fn render_access_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let selected_mode = self
            .selected_session()
            .map(|session| session.runtime_mode)
            .filter(|mode| *mode != RuntimeMode::Plan)
            .unwrap_or_default();
        let weak = cx.entity().downgrade();
        let handle = self.menu_handle("runtime-mode", cx);
        dropdown_menu(
            MenuChip::new("runtime-mode")
                .icon(selected_mode.icon(), theme.text_tertiary)
                .label(selected_mode.label())
                .selected(handle.is_open()),
            "runtime-mode-menu",
            &handle,
            MenuAlign::AboveLeft,
            move |_| {
                RuntimeMode::ACCESS_OPTIONS
                    .into_iter()
                    .map(|option| {
                        let weak = weak.clone();
                        let selected = option == selected_mode;
                        MenuItem::custom(move |_, _| {
                            div()
                                .w(px(288.0))
                                .py(px(4.0))
                                .flex()
                                .items_center()
                                .gap(px(10.0))
                                .child(icon(option.icon(), 14.0, theme.text_tertiary))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .w_full()
                                                .truncate()
                                                .text_size(px(12.0))
                                                .font_weight(if selected {
                                                    FontWeight::SEMIBOLD
                                                } else {
                                                    FontWeight::MEDIUM
                                                })
                                                .text_color(theme.text)
                                                .child(option.label()),
                                        )
                                        .child(
                                            div()
                                                .w_full()
                                                .mt(px(2.0))
                                                .text_size(px(10.5))
                                                .line_height(px(14.0))
                                                .whitespace_normal()
                                                .text_color(theme.text_tertiary)
                                                .child(option.description()),
                                        ),
                                )
                                .when(selected, |element| {
                                    element.child(icon(
                                        "icons/check.svg",
                                        11.0,
                                        theme.text_tertiary,
                                    ))
                                })
                                .into_any_element()
                        })
                        .on_click(move |_, cx| {
                            let _ = weak.update(cx, |this, cx| this.set_runtime_mode(option, cx));
                        })
                    })
                    .collect()
            },
        )
    }

    pub(super) fn render_interaction_mode_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let mode = self
            .selected_session()
            .map(|session| session.interaction_mode)
            .unwrap_or_default();
        let next_mode = if mode == InteractionMode::Plan {
            InteractionMode::Build
        } else {
            InteractionMode::Plan
        };
        let weak = cx.entity().downgrade();
        div()
            .id("interaction-mode")
            .h(px(24.0))
            .px(px(7.0))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .cursor_default()
            .text_size(px(11.5))
            .line_height(px(14.0))
            .text_color(if mode == InteractionMode::Plan {
                theme.accent
            } else {
                theme.text_secondary
            })
            .hover(|element| element.bg(theme.overlay))
            .child(icon(
                if mode == InteractionMode::Plan {
                    "icons/list.svg"
                } else {
                    "icons/wrench.svg"
                },
                10.5,
                if mode == InteractionMode::Plan {
                    theme.accent
                } else {
                    theme.text_tertiary
                },
            ))
            .child(mode.label())
            .on_click(move |_, _, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.set_interaction_mode(next_mode, cx);
                });
            })
            .into_any_element()
    }

    /// Stage files dropped onto the composer as attachment chips. The mention
    /// each chip will submit takes the autocomplete's form: relative to the
    /// project root when the file is inside it, absolute otherwise,
    /// directories with a trailing slash.
    pub(super) fn stage_dropped_files(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.selected_project().map(|project| project.path.clone());
        let mut staged = false;
        for path in paths.paths() {
            if self
                .composer_attachments
                .iter()
                .any(|attachment| attachment.path == *path)
            {
                continue;
            }
            let is_dir = path.is_dir();
            let mention = dropped_file_mention(root.as_deref(), path, is_dir);
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| mention.clone());
            let is_image = !is_dir
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        matches!(
                            extension.to_ascii_lowercase().as_str(),
                            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff"
                                | "ico"
                        )
                    });
            self.composer_attachments.push(ComposerAttachment {
                path: path.clone(),
                mention,
                name: SharedString::from(name),
                is_dir,
                is_image,
            });
            staged = true;
        }
        if !staged {
            return;
        }
        let focus = self.composer.read(cx).focus();
        window.focus(&focus, cx);
        cx.notify();
    }

    /// The text a submission sends, with staged chips drained into it —
    /// submitting consumes them. `None` means there is nothing to send.
    pub(super) fn submission_with_attachments(&mut self, prompt: &str) -> Option<String> {
        let mentions = self
            .composer_attachments
            .drain(..)
            .map(|attachment| attachment.mention)
            .collect::<Vec<_>>();
        merged_submission(prompt, &mentions)
    }

    /// The staged-attachment chips above the input: a thumbnail tile per
    /// image, a file-type icon and basename for everything else, each with a
    /// floating remove button — T3 Code's attachment row in graphite.
    fn render_composer_attachments(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let mut row = div()
            .px(px(4.0))
            .pt(px(2.0))
            .pb(px(8.0))
            .flex()
            .flex_wrap()
            .gap(px(8.0));
        for (index, attachment) in self.composer_attachments.iter().enumerate() {
            let icon_path = if attachment.is_dir {
                "icons/folder.svg"
            } else {
                super::right_panel::file_icon_for_path(&attachment.mention)
            };
            let mut tile = div()
                .id(SharedString::from(format!("composer-attachment-{index}")))
                .relative()
                .w(px(64.0))
                .h(px(64.0))
                .rounded(px(8.0))
                .overflow_hidden()
                .border_1()
                .border_color(theme.border)
                .bg(theme.inset)
                .tooltip(Tooltip::text(format!("@{}", attachment.mention)));
            if attachment.is_image {
                tile = tile.child(
                    img(attachment.path.clone())
                        .size_full()
                        .object_fit(ObjectFit::Cover),
                );
            } else {
                tile = tile.child(
                    div()
                        .size_full()
                        .px(px(5.0))
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(5.0))
                        .child(icon(icon_path, 16.0, theme.text_tertiary))
                        .child(
                            div().w_full().flex().justify_center().child(
                                div()
                                    .max_w_full()
                                    .truncate()
                                    .text_size(px(8.5))
                                    .text_color(theme.text_tertiary)
                                    .child(attachment.name.clone()),
                            ),
                        ),
                );
            }
            row = row.child(
                tile.child(
                    div()
                        .id(SharedString::from(format!(
                            "composer-attachment-remove-{index}"
                        )))
                        .absolute()
                        .top(px(3.0))
                        .right(px(3.0))
                        .w(px(16.0))
                        .h(px(16.0))
                        .rounded(px(5.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_default()
                        .bg(theme.canvas.opacity(0.8))
                        .hover(|element| element.bg(theme.canvas.opacity(0.95)))
                        .active(|element| element.opacity(0.8))
                        .child(icon("icons/x.svg", 9.0, theme.text_secondary))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if index < this.composer_attachments.len() {
                                this.composer_attachments.remove(index);
                                cx.notify();
                            }
                        })),
                ),
            );
        }
        row
    }

    pub(super) fn render_composer(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let session = self.selected_session();
        let working = session
            .map(|session| {
                matches!(
                    session.status,
                    SessionStatus::Working | SessionStatus::Connecting | SessionStatus::Waiting
                )
            })
            .unwrap_or(false);
        let has_draft = !self.composer.read(cx).content().trim().is_empty()
            || !self.composer_attachments.is_empty();
        let autocomplete = self.render_composer_autocomplete(window, cx);
        let autocomplete_open = autocomplete.is_some();
        // Files dragged in from the OS light the card up as a drop target and
        // stage as attachment chips. The wash arrives pre-blended because a
        // drag-over refinement replaces the card's fill rather than
        // compositing over it.
        let drop_wash = theme.composer.blend(theme.overlay_strong);
        let drop_ring = theme.accent.opacity(0.7);
        div().flex_none().px(px(20.0)).child(
            div()
                .w_full()
                .max_w(px(CONTENT_MAX_WIDTH))
                .mx_auto()
                .rounded(px(13.0))
                .border_1()
                .border_color(theme.border)
                .bg(theme.composer)
                .p(px(10.0))
                .drag_over::<ExternalPaths>(move |style, _, _, _| {
                    style.bg(drop_wash).border_color(drop_ring)
                })
                .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                    this.stage_dropped_files(paths, window, cx);
                }))
                // Anchor for the bounds probe the autocomplete popup aligns to.
                .relative()
                .child(super::autocomplete::composer_card_bounds_probe(
                    self.composer_autocomplete.card_bounds_cell(),
                ))
                // Only while the popup is open: the key context routes the
                // arrows, `enter`, `tab` and `escape` here as actions, out
                // from under the focused field. When it closes, the context
                // disappears with it and `enter` submits again.
                .when(autocomplete_open, |card| {
                    card.key_context("ComposerAutocomplete")
                        .on_action(cx.listener(|this, _: &SelectNextEntry, window, cx| {
                            this.move_autocomplete_highlight("down", window, cx);
                        }))
                        .on_action(cx.listener(|this, _: &SelectPreviousEntry, window, cx| {
                            this.move_autocomplete_highlight("up", window, cx);
                        }))
                        .on_action(cx.listener(|this, _: &ConfirmEntry, window, cx| {
                            this.accept_autocomplete(None, window, cx);
                        }))
                        .on_action(cx.listener(|this, _: &DismissMenu, _, cx| {
                            this.dismiss_autocomplete(cx);
                        }))
                })
                .children(autocomplete)
                .when(!self.composer_attachments.is_empty(), |card| {
                    card.child(self.render_composer_attachments(cx))
                })
                .child(div().px(px(4.0)).pt(px(2.0)).child(self.composer.clone()))
                .child(
                    div()
                        .mt(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .text_size(px(11.5))
                        .line_height(px(14.0))
                        .child(self.render_provider_model_control(cx))
                        .children(self.render_model_traits_control(cx))
                        .child(self.render_access_control(cx))
                        .child(self.render_interaction_mode_control(cx))
                        .child(div().flex_1())
                        .child(if working {
                            div()
                                .id("send-or-stop")
                                .w(px(26.0))
                                .h(px(26.0))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_default()
                                .bg(theme.overlay_strong)
                                .hover(|element| element.bg(theme.danger_soft))
                                .active(|element| element.opacity(0.8))
                                .child(icon("icons/stop.svg", 18.0, theme.text))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_turn(cx);
                                }))
                        } else {
                            div()
                                .id("send-or-stop")
                                .w(px(26.0))
                                .h(px(26.0))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(if has_draft {
                                    theme.inverse
                                } else {
                                    theme.overlay_strong
                                })
                                .when(has_draft, |element| {
                                    element
                                        .cursor_default()
                                        .hover(|element| element.opacity(0.9))
                                        .active(|element| element.opacity(0.8))
                                })
                                .child(icon(
                                    "icons/arrow-up.svg",
                                    16.0,
                                    if has_draft {
                                        theme.on_inverse
                                    } else {
                                        theme.text_ghost
                                    },
                                ))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let prompt = this.composer.read(cx).content().to_owned();
                                    if let Some(prompt) =
                                        this.submission_with_attachments(&prompt)
                                    {
                                        this.composer.update(cx, |input, cx| input.clear(cx));
                                        this.submit_prompt(prompt, cx);
                                    }
                                }))
                        }),
                ),
        )
    }

    pub(super) fn render_workspace_footer(&self, cx: &App) -> Div {
        let theme = Theme::current(cx);
        let path = self
            .selected_project()
            .map(|project| compact_path(&project.path))
            .unwrap_or_default();
        div()
            .flex_none()
            .px(px(20.0))
            .pb(px(8.0))
            .pt(px(4.0))
            .child(
                div()
                    .w_full()
                    .max_w(px(CONTENT_MAX_WIDTH))
                    .mx_auto()
                    .h(px(20.0))
                    // Left edge lines up with the composer card's inner icon
                    // column (10px card padding + 7px chip padding).
                    .pl(px(17.0))
                    .pr(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .text_size(px(11.0))
                    .line_height(px(14.0))
                    .when_some(self.branch.clone(), |element, branch| {
                        element
                            .child(icon("icons/git-branch.svg", 10.5, theme.text_tertiary))
                            .child(
                                div()
                                    .text_color(theme.text_secondary)
                                    .child(SharedString::from(branch)),
                            )
                            .child(
                                div()
                                    .w(px(2.5))
                                    .h(px(2.5))
                                    .flex_none()
                                    .rounded_full()
                                    .bg(theme.text_ghost),
                            )
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(10.5))
                            .text_color(theme.text_ghost)
                            .child(SharedString::from(path)),
                    ),
            )
    }
}

/// The mention a dropped file submits: relative to the project root when the
/// file is inside it, absolute otherwise, directories with a trailing slash —
/// the same form the `@` autocomplete inserts. Dropping the root itself keeps
/// the absolute path rather than producing an empty mention.
pub(super) fn dropped_file_mention(
    root: Option<&std::path::Path>,
    path: &std::path::Path,
    is_dir: bool,
) -> String {
    let mention = root
        .and_then(|root| path.strip_prefix(root).ok())
        .filter(|relative| !relative.as_os_str().is_empty())
        .unwrap_or(path)
        .display()
        .to_string();
    if is_dir && !mention.ends_with('/') {
        format!("{mention}/")
    } else {
        mention
    }
}

/// The prompt a submission sends: the typed text plus one `@` mention per
/// staged attachment, appended at the end the way T3 Code appends dropped
/// files. `None` means there is nothing to send.
pub(super) fn merged_submission(prompt: &str, mentions: &[String]) -> Option<String> {
    let mentions = mentions
        .iter()
        .map(|mention| format!("@{mention}"))
        .collect::<Vec<_>>()
        .join(" ");
    let prompt = prompt.trim();
    match (prompt.is_empty(), mentions.is_empty()) {
        (true, true) => None,
        (false, true) => Some(prompt.to_owned()),
        (true, false) => Some(mentions),
        (false, false) => Some(format!("{prompt} {mentions}")),
    }
}

/// Where the picker's keyboard cursor lands, wrapping at both ends.
///
/// `None` for `current` means the cursor has not moved yet, so `down` opens on
/// the first row and `up` on the last. `None` in the result means the key does
/// not navigate.
pub(super) fn next_picker_highlight(
    current: Option<usize>,
    len: usize,
    key: &str,
) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match key {
        "down" => Some(current.map_or(0, |index| (index + 1) % len)),
        "up" => Some(current.map_or(len - 1, |index| (index + len - 1) % len)),
        _ => None,
    }
}

/// The models the picker lists, in display order.
///
/// Shared by the panel body and by `enter`'s handler so a keyboard cursor index
/// always means the same row in both.
pub(super) fn visible_picker_models(
    probes: &[ProviderProbe],
    favorites: &[FavoriteModel],
    disabled_providers: &[ProviderKind],
    locked_provider: Option<ProviderKind>,
    selected_tab: ModelPickerTab,
    normalized_query: &str,
) -> Vec<(ProviderKind, ProviderModel)> {
    let searching = !normalized_query.is_empty();
    let mut models = probes
        .iter()
        .filter(|probe| probe.installed)
        .flat_map(|probe| {
            probe
                .models
                .iter()
                .cloned()
                .map(move |model| (probe.provider, model))
        })
        .filter(|(kind, _)| locked_provider.is_none() || locked_provider == Some(*kind))
        // Switched-off providers keep serving the session already locked to
        // them, but offer nothing to new work — including favorites.
        .filter(|(kind, _)| !disabled_providers.contains(kind) || locked_provider == Some(*kind))
        .filter(|(kind, model)| {
            if searching {
                let searchable = format!(
                    "{} {} {} {}",
                    model.name,
                    model.id,
                    kind.short_name(),
                    model.sub_provider.as_deref().unwrap_or("")
                )
                .to_ascii_lowercase();
                return normalized_query
                    .split_whitespace()
                    .all(|token| searchable.contains(token));
            }
            match selected_tab {
                ModelPickerTab::Favorites => favorites
                    .iter()
                    .any(|favorite| favorite.provider == *kind && favorite.model == model.id),
                ModelPickerTab::Provider(provider) => provider == *kind,
            }
        })
        .collect::<Vec<_>>();
    if !searching && selected_tab == ModelPickerTab::Favorites {
        models.sort_by_key(|(kind, model)| {
            favorites
                .iter()
                .position(|favorite| favorite.provider == *kind && favorite.model == model.id)
                .unwrap_or(usize::MAX)
        });
    }
    models
}
