use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{IconName, Sizable};

use super::*;

const TAB_SCROLL_FADE_WIDTH: f32 = 24.0;

#[derive(Clone, Debug, Eq, PartialEq)]
struct WorkingTreeEntry {
    relative_path: String,
    absolute_path: PathBuf,
    name: String,
    is_dir: bool,
    expanded: bool,
    depth: usize,
}

fn visible_working_tree_entries(
    root: &Path,
    expanded_paths: &HashSet<PathBuf>,
) -> Vec<WorkingTreeEntry> {
    fn visit(
        directory: &Path,
        relative_directory: &Path,
        depth: usize,
        expanded_paths: &HashSet<PathBuf>,
        entries: &mut Vec<WorkingTreeEntry>,
    ) {
        let Ok(read_dir) = std::fs::read_dir(directory) else {
            return;
        };
        let mut children = read_dir
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name == ".git" {
                    return None;
                }
                let is_dir = entry.file_type().ok()?.is_dir();
                Some((entry.path(), name, is_dir))
            })
            .collect::<Vec<_>>();
        children.sort_by_key(|(_, name, is_dir)| (!*is_dir, name.to_lowercase()));

        for (absolute_path, name, is_dir) in children {
            let relative_path = relative_directory.join(&name);
            let expanded = is_dir && expanded_paths.contains(&absolute_path);
            entries.push(WorkingTreeEntry {
                relative_path: relative_path.to_string_lossy().into_owned(),
                absolute_path: absolute_path.clone(),
                name,
                is_dir,
                expanded,
                depth,
            });
            if expanded {
                visit(
                    &absolute_path,
                    &relative_path,
                    depth + 1,
                    expanded_paths,
                    entries,
                );
            }
        }
    }

    let mut entries = Vec::new();
    visit(root, Path::new(""), 0, expanded_paths, &mut entries);
    entries
}

impl RightPanelSurface {
    fn new_browser() -> Self {
        Self::Browser(Uuid::new_v4())
    }

    fn new_terminal() -> Self {
        Self::Terminal(Uuid::new_v4())
    }

    fn terminal_id(&self) -> Option<Uuid> {
        match self {
            Self::Terminal(id) => Some(*id),
            _ => None,
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Browser(_) => "Browser",
            Self::Terminal(_) => "Terminal",
            Self::Files => "Files",
            Self::Diff => "Diff",
            Self::File(path) => path.rsplit('/').next().unwrap_or(path),
        }
    }

    fn icon_path(&self) -> &'static str {
        match self {
            Self::Browser(_) => "icons/globe.svg",
            Self::Terminal(_) => "icons/terminal.svg",
            Self::Files => "icons/folder.svg",
            Self::Diff => "icons/file-diff.svg",
            Self::File(_) => "icons/list.svg",
        }
    }
}

fn reusable_surface_index(
    surfaces: &[RightPanelSurface],
    requested: &RightPanelSurface,
) -> Option<usize> {
    match requested {
        RightPanelSurface::Browser(_) | RightPanelSurface::Terminal(_) => None,
        RightPanelSurface::Files | RightPanelSurface::Diff | RightPanelSurface::File(_) => {
            surfaces.iter().position(|surface| surface == requested)
        }
    }
}

#[derive(Clone, Copy)]
enum TabScrollFadeSide {
    Left,
    Right,
}

fn tab_scroll_fade_visibility(offset_x: Pixels, max_offset: Pixels) -> (bool, bool) {
    let scrolled = -offset_x;
    let threshold = px(0.5);
    (scrolled > threshold, max_offset - scrolled > threshold)
}

fn fade_safe_tab_offset(
    current_offset: Pixels,
    max_offset: Pixels,
    item_left: Pixels,
    item_right: Pixels,
    viewport_left: Pixels,
    viewport_right: Pixels,
) -> Pixels {
    let inset = px(TAB_SCROLL_FADE_WIDTH);
    let mut offset = current_offset;
    let visible_left = item_left + offset;
    let visible_right = item_right + offset;
    if visible_left < viewport_left + inset {
        offset += viewport_left + inset - visible_left;
    } else if visible_right > viewport_right - inset {
        offset -= visible_right - (viewport_right - inset);
    }
    offset.clamp(-max_offset, px(0.0))
}

fn tab_scroll_reveal_guard(
    scroll_handle: ScrollHandle,
    tab_index: usize,
    waku: WeakEntity<Waku>,
) -> impl IntoElement {
    canvas(
        move |_, window, _| {
            if let Some(item) = scroll_handle.bounds_for_item(tab_index) {
                let viewport = scroll_handle.bounds();
                let offset = scroll_handle.offset();
                let safe_offset = fade_safe_tab_offset(
                    offset.x,
                    scroll_handle.max_offset().x,
                    item.left(),
                    item.right(),
                    viewport.left(),
                    viewport.right(),
                );
                if safe_offset != offset.x {
                    scroll_handle.set_offset(point(safe_offset, offset.y));
                }
            }

            window.on_next_frame(move |_, cx| {
                let _ = waku.update(cx, |this, cx| {
                    if this.right_panel_pending_tab_reveal == Some(tab_index) {
                        this.right_panel_pending_tab_reveal = None;
                        cx.notify();
                    }
                });
            });
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
}

fn tab_scroll_fade(
    scroll_handle: ScrollHandle,
    side: TabScrollFadeSide,
    surface: Hsla,
) -> impl IntoElement {
    canvas(
        move |bounds, _, _| {
            let (show_left, show_right) =
                tab_scroll_fade_visibility(scroll_handle.offset().x, scroll_handle.max_offset().x);
            let visible = match side {
                TabScrollFadeSide::Left => show_left,
                TabScrollFadeSide::Right => show_right,
            };
            visible.then(|| {
                let transparent = surface.opacity(0.0);
                let background = match side {
                    TabScrollFadeSide::Left => linear_gradient(
                        90.0,
                        linear_color_stop(surface, 0.0),
                        linear_color_stop(transparent, 1.0),
                    ),
                    TabScrollFadeSide::Right => linear_gradient(
                        90.0,
                        linear_color_stop(transparent, 0.0),
                        linear_color_stop(surface, 1.0),
                    ),
                };
                fill(bounds, background)
            })
        },
        |_, fade, window, _| {
            if let Some(fade) = fade {
                window.paint_quad(fade);
            }
        },
    )
    .absolute()
    .top_0()
    .bottom_0()
    .when(matches!(side, TabScrollFadeSide::Left), |element| {
        element.left_0()
    })
    .when(matches!(side, TabScrollFadeSide::Right), |element| {
        element.right_0()
    })
    .w(px(TAB_SCROLL_FADE_WIDTH))
}

#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_tree_only_descends_into_expanded_directories() {
        let root = std::env::temp_dir().join(format!("waku-working-tree-{}", Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("README.md"), "# Waku\n").unwrap();

        let collapsed = visible_working_tree_entries(&root, &HashSet::new());
        assert_eq!(
            collapsed
                .iter()
                .map(|entry| entry.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["src", "README.md"]
        );

        let expanded = HashSet::from([root.join("src")]);
        let visible = visible_working_tree_entries(&root, &expanded);
        assert_eq!(
            visible
                .iter()
                .map(|entry| (entry.relative_path.as_str(), entry.depth))
                .collect::<Vec<_>>(),
            vec![
                ("src", 0),
                ("src/nested", 1),
                ("src/main.rs", 1),
                ("README.md", 0)
            ]
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_reuses_single_instance_surface_tabs() {
        let browser = RightPanelSurface::new_browser();
        let terminal = RightPanelSurface::new_terminal();
        let surfaces = vec![
            browser,
            terminal,
            RightPanelSurface::Files,
            RightPanelSurface::Diff,
        ];

        assert_eq!(
            reusable_surface_index(&surfaces, &RightPanelSurface::new_browser()),
            None
        );
        assert_eq!(
            reusable_surface_index(&surfaces, &RightPanelSurface::new_terminal()),
            None
        );
        assert_eq!(
            reusable_surface_index(&surfaces, &RightPanelSurface::Files),
            Some(2)
        );
        assert_eq!(
            reusable_surface_index(&surfaces, &RightPanelSurface::Diff),
            Some(3)
        );
    }

    #[test]
    fn tab_scroll_fades_only_show_toward_hidden_content() {
        assert_eq!(
            tab_scroll_fade_visibility(px(0.0), px(120.0)),
            (false, true)
        );
        assert_eq!(
            tab_scroll_fade_visibility(px(-40.0), px(120.0)),
            (true, true)
        );
        assert_eq!(
            tab_scroll_fade_visibility(px(-120.0), px(120.0)),
            (true, false)
        );
        assert_eq!(tab_scroll_fade_visibility(px(0.0), px(0.0)), (false, false));
    }

    #[test]
    fn selected_tab_offset_clears_fade_overlays() {
        assert_eq!(
            fade_safe_tab_offset(
                px(-100.0),
                px(300.0),
                px(90.0),
                px(190.0),
                px(0.0),
                px(300.0),
            ),
            px(-66.0)
        );
        assert_eq!(
            fade_safe_tab_offset(
                px(-100.0),
                px(324.0),
                px(300.0),
                px(400.0),
                px(0.0),
                px(300.0),
            ),
            px(-124.0)
        );
        assert_eq!(
            fade_safe_tab_offset(px(0.0), px(0.0), px(0.0), px(100.0), px(0.0), px(300.0),),
            px(0.0)
        );
    }
}

impl Waku {
    fn reveal_right_panel_tab(&mut self, index: usize) {
        self.right_panel_pending_tab_reveal = Some(index);
        self.right_panel_tabs_scroll_handle.scroll_to_item(index);
    }

    fn active_right_panel_surface(&self) -> Option<&RightPanelSurface> {
        self.right_panel_active_surface
            .and_then(|index| self.right_panel_surfaces.get(index))
    }

    fn open_right_panel_surface(&mut self, surface: RightPanelSurface, cx: &mut Context<Self>) {
        if surface == RightPanelSurface::Diff {
            self.refresh_right_panel_diff();
        }
        if let Some(terminal_id) = surface.terminal_id() {
            self.ensure_right_panel_terminal(terminal_id, cx);
        }
        let index =
            reusable_surface_index(&self.right_panel_surfaces, &surface).unwrap_or_else(|| {
                self.right_panel_surfaces.push(surface);
                self.right_panel_surfaces.len() - 1
            });
        self.right_panel_active_surface = Some(index);
        self.reveal_right_panel_tab(index);
        self.right_panel_visible = true;
        cx.notify();
    }

    fn open_right_panel_file(&mut self, relative_path: String, cx: &mut Context<Self>) {
        if let Some(active) = self.right_panel_active_surface
            && matches!(
                self.right_panel_surfaces.get(active),
                Some(RightPanelSurface::Files)
            )
        {
            self.right_panel_surfaces[active] = RightPanelSurface::File(relative_path);
            self.right_panel_visible = true;
            cx.notify();
            return;
        }

        self.open_right_panel_surface(RightPanelSurface::File(relative_path), cx);
    }

    fn close_right_panel_surface(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.right_panel_surfaces.len() {
            return;
        }
        if let Some(terminal_id) = self.right_panel_surfaces[index].terminal_id() {
            self.right_panel_terminals.remove(&terminal_id);
        }
        self.right_panel_surfaces.remove(index);
        self.right_panel_active_surface = if self.right_panel_surfaces.is_empty() {
            None
        } else {
            Some(match self.right_panel_active_surface {
                Some(active) if active > index => active - 1,
                Some(active) if active == index => index.saturating_sub(1),
                Some(active) => active.min(self.right_panel_surfaces.len() - 1),
                None => 0,
            })
        };
        if let Some(active) = self.right_panel_active_surface {
            self.reveal_right_panel_tab(active);
        } else {
            self.right_panel_pending_tab_reveal = None;
        }
        cx.notify();
    }

    pub(super) fn render_right_panel_toggle(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = Theme::current(cx);
        div()
            .id("toggle-right-panel")
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
            .child(icon("icons/panel-right.svg", 14.0, theme.text_tertiary))
            .tooltip(|window, cx| Tooltip::new("Toggle right panel").build(window, cx))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.right_panel_visible = !this.right_panel_visible;
                cx.notify();
            }))
    }

    pub(super) fn render_right_panel(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = Theme::current(cx);
        let body = match self.active_right_panel_surface().cloned() {
            None => self.render_right_panel_chooser(cx).into_any_element(),
            Some(RightPanelSurface::Files) => self.render_right_panel_files(cx).into_any_element(),
            Some(RightPanelSurface::Diff) => self.render_right_panel_diff(cx).into_any_element(),
            Some(RightPanelSurface::Terminal(terminal_id)) => self
                .right_panel_terminals
                .get(&terminal_id)
                .cloned()
                .map(IntoElement::into_any_element)
                .unwrap_or_else(|| {
                    self.render_right_panel_empty_message(
                        "Terminal unavailable",
                        "Open the Terminal surface again to start a shell.",
                        cx,
                    )
                    .into_any_element()
                }),
            Some(RightPanelSurface::File(path)) => {
                self.render_right_panel_file(path, cx).into_any_element()
            }
            Some(surface) => self
                .render_right_panel_placeholder(surface, cx)
                .into_any_element(),
        };

        div()
            .id("right-panel")
            .w(gpui::relative(RIGHT_PANEL_WIDTH_FRACTION))
            .min_w(px(RIGHT_PANEL_MIN_WIDTH))
            .max_w(px(RIGHT_PANEL_MAX_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .min_w_0()
            .border_l_1()
            .border_color(theme.border_strong)
            .bg(theme.surface)
            .child(self.render_right_panel_header(cx))
            .child(body)
    }

    fn ensure_right_panel_terminal(&mut self, terminal_id: Uuid, cx: &mut Context<Self>) {
        let Some(working_directory) = self.selected_project().map(|project| project.path.clone())
        else {
            self.right_panel_terminals.remove(&terminal_id);
            return;
        };
        let matches_project = self
            .right_panel_terminals
            .get(&terminal_id)
            .is_some_and(|terminal| terminal.read(cx).working_directory() == working_directory);
        if !matches_project {
            self.right_panel_terminals.insert(
                terminal_id,
                cx.new(|cx| TerminalView::new(working_directory.clone(), cx)),
            );
        }
    }

    pub(super) fn ensure_right_panel_terminals(&mut self, cx: &mut Context<Self>) {
        let terminal_ids = self
            .right_panel_surfaces
            .iter()
            .filter_map(RightPanelSurface::terminal_id)
            .collect::<Vec<_>>();
        self.right_panel_terminals
            .retain(|terminal_id, _| terminal_ids.contains(terminal_id));
        for terminal_id in terminal_ids {
            self.ensure_right_panel_terminal(terminal_id, cx);
        }
    }

    fn render_right_panel_header(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = Theme::current(cx);
        let active_surface = self.right_panel_active_surface;
        let mut tabs = div()
            .id("right-panel-tabs")
            .h_full()
            .min_w_0()
            .flex_1()
            .flex()
            .items_center()
            .gap(px(4.0))
            .overflow_x_scroll()
            .track_scroll(&self.right_panel_tabs_scroll_handle);
        for (index, surface) in self.right_panel_surfaces.iter().cloned().enumerate() {
            let active = active_surface == Some(index);
            let label = SharedString::from(surface.label().to_owned());
            let activate_weak = cx.entity().downgrade();
            let close_weak = cx.entity().downgrade();
            tabs = tabs.child(
                div()
                    .id(SharedString::from(format!("right-panel-tab-{index}")))
                    .h(px(28.0))
                    .min_w(px(100.0))
                    .max_w(px(176.0))
                    .px(px(8.0))
                    .rounded(px(6.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_default()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .when(active, |element| element.bg(theme.overlay_strong))
                    .when(!active, |element| {
                        element.hover(|element| element.bg(theme.overlay))
                    })
                    .child(icon(surface.icon_path(), 13.0, theme.text_secondary))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(px(12.0))
                            .text_color(if active {
                                theme.text
                            } else {
                                theme.text_secondary
                            })
                            .child(label),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("close-right-panel-tab-{index}")))
                            .w(px(16.0))
                            .h(px(16.0))
                            .rounded(px(4.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|element| element.bg(theme.overlay_strong))
                            .child(icon("icons/x.svg", 10.0, theme.text_tertiary))
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                let _ = close_weak.update(cx, |this, cx| {
                                    this.close_right_panel_surface(index, cx);
                                });
                            }),
                    )
                    .on_click(move |_, _, cx| {
                        let _ = activate_weak.update(cx, |this, cx| {
                            this.right_panel_active_surface = Some(index);
                            this.reveal_right_panel_tab(index);
                            cx.notify();
                        });
                    }),
            );
        }
        tabs = tabs.child(div().w(px(TAB_SCROLL_FADE_WIDTH)).h(px(1.0)).flex_none());

        let mut header = div()
            .id("right-panel-header")
            .h(px(48.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0))
            .pl(px(10.0))
            .pr(px(14.0))
            .child(
                div()
                    .relative()
                    .h_full()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .child(tabs)
                    .when_some(self.right_panel_pending_tab_reveal, |element, tab_index| {
                        element.child(tab_scroll_reveal_guard(
                            self.right_panel_tabs_scroll_handle.clone(),
                            tab_index,
                            cx.entity().downgrade(),
                        ))
                    })
                    .child(tab_scroll_fade(
                        self.right_panel_tabs_scroll_handle.clone(),
                        TabScrollFadeSide::Left,
                        theme.surface,
                    ))
                    .child(tab_scroll_fade(
                        self.right_panel_tabs_scroll_handle.clone(),
                        TabScrollFadeSide::Right,
                        theme.surface,
                    )),
            );

        if !self.right_panel_surfaces.is_empty() {
            let weak = cx.entity().downgrade();
            let existing_surfaces = self.right_panel_surfaces.clone();
            let options = [
                RightPanelSurface::new_browser(),
                RightPanelSurface::new_terminal(),
                RightPanelSurface::Files,
                RightPanelSurface::Diff,
            ];
            header = header.child(
                div()
                    .flex_none()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .child(
                        Button::new("add-right-panel-surface")
                            .xsmall()
                            .ghost()
                            .icon(IconName::Plus)
                            .dropdown_menu(move |mut menu, _, _| {
                                menu = menu.min_w(px(168.0)).max_w(px(168.0));
                                for surface in options.clone() {
                                    let item_weak = weak.clone();
                                    let item_surface = surface.clone();
                                    let item_theme = theme;
                                    let icon_path = surface.icon_path();
                                    let label = surface.label().to_owned();
                                    let checked =
                                        reusable_surface_index(&existing_surfaces, &surface)
                                            .is_some();
                                    menu =
                                        menu.item(
                                            PopupMenuItem::element(move |_, _| {
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap(px(8.0))
                                                    .child(icon(
                                                        icon_path,
                                                        13.0,
                                                        item_theme.text_tertiary,
                                                    ))
                                                    .child(label.clone())
                                            })
                                            .checked(checked)
                                            .on_click(move |_, _, cx| {
                                                let _ = item_weak.update(cx, |this, cx| {
                                                    this.open_right_panel_surface(
                                                        item_surface.clone(),
                                                        cx,
                                                    );
                                                });
                                            }),
                                        );
                                }
                                menu
                            })
                            .anchor(Anchor::TopLeft),
                    ),
            );
        }

        self.window_drag_region(header.child(self.render_right_panel_toggle(cx)), cx)
    }

    fn render_right_panel_chooser(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = Theme::current(cx);
        div()
            .id("right-panel-chooser")
            .flex_1()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .px(px(20.0))
            .pb(px(32.0))
            .child(
                div()
                    .w_full()
                    .max_w(px(420.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child("Open a surface"),
                    )
                    .child(
                        div()
                            .mt(px(5.0))
                            .text_size(px(11.0))
                            .text_color(theme.text_tertiary)
                            .child("Choose what to show in the right panel."),
                    )
                    .child(
                        div()
                            .mt(px(18.0))
                            .w_full()
                            .flex()
                            .gap(px(8.0))
                            .child(self.render_right_panel_card(
                                RightPanelSurface::new_browser(),
                                "Open a local app or URL.",
                                cx,
                            ))
                            .child(self.render_right_panel_card(
                                RightPanelSurface::new_terminal(),
                                "Start a shell in this workspace.",
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .mt(px(8.0))
                            .w_full()
                            .flex()
                            .gap(px(8.0))
                            .child(self.render_right_panel_card(
                                RightPanelSurface::Files,
                                "Browse and read workspace files.",
                                cx,
                            ))
                            .child(self.render_right_panel_card(
                                RightPanelSurface::Diff,
                                "Review current workspace changes.",
                                cx,
                            )),
                    ),
            )
    }

    fn render_right_panel_card(
        &self,
        surface: RightPanelSurface,
        description: &'static str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        let icon_path = surface.icon_path();
        let label = surface.label().to_owned();
        div()
            .id(SharedString::from(format!(
                "right-panel-card-{}",
                label.to_lowercase()
            )))
            .h(px(112.0))
            .flex_1()
            .min_w_0()
            .p(px(14.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.composer)
            .flex()
            .flex_col()
            .items_start()
            .cursor_default()
            .hover(|element| element.bg(theme.raised).border_color(theme.text_ghost))
            .active(|element| element.bg(theme.overlay_strong))
            .child(icon(icon_path, 18.0, theme.text_secondary))
            .child(
                div()
                    .mt(px(12.0))
                    .text_size(px(12.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(label),
            )
            .child(
                div()
                    .mt(px(4.0))
                    .text_size(px(10.5))
                    .line_height(px(15.0))
                    .text_color(theme.text_tertiary)
                    .whitespace_normal()
                    .child(description),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_right_panel_surface(surface.clone(), cx);
            }))
    }

    fn render_right_panel_placeholder(
        &self,
        surface: RightPanelSurface,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let description = match surface {
            RightPanelSurface::Browser(_) => {
                "Browser previews will appear here once the native preview backend is connected."
            }
            RightPanelSurface::Terminal(_) => {
                "A workspace terminal will appear here once the native terminal backend is connected."
            }
            _ => "This surface is not available yet.",
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px(px(48.0))
            .pb(px(32.0))
            .child(icon(surface.icon_path(), 24.0, theme.text_tertiary))
            .child(
                div()
                    .mt(px(14.0))
                    .text_size(px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(surface.label().to_owned()),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .max_w(px(310.0))
                    .text_center()
                    .text_size(px(11.0))
                    .line_height(px(17.0))
                    .text_color(theme.text_tertiary)
                    .whitespace_normal()
                    .child(description),
            )
    }

    fn render_right_panel_files(&self, cx: &mut Context<Self>) -> Div {
        self.render_right_panel_working_tree(None, cx)
    }

    fn render_right_panel_working_tree(
        &self,
        selected_path: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let Some(project) = self.selected_project() else {
            return self.render_right_panel_empty_message(
                "No project open",
                "Open a project to browse its files.",
                cx,
            );
        };
        let project_path = project.path.clone();
        let project_name = project.name.clone();
        let entries = visible_working_tree_entries(&project_path, &self.right_panel_expanded_paths);

        let mut list = div().flex().flex_col().py(px(6.0));
        for entry in entries {
            let relative_path = entry.relative_path.clone();
            let absolute_path = entry.absolute_path.clone();
            let is_dir = entry.is_dir;
            let selected = selected_path == Some(relative_path.as_str());
            let row = div()
                .id(SharedString::from(format!(
                    "right-panel-file-{relative_path}"
                )))
                .h(px(30.0))
                .mx(px(8.0))
                .pl(px(8.0 + entry.depth as f32 * 16.0))
                .pr(px(8.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .cursor_default()
                .when(selected, |element| element.bg(theme.overlay_strong))
                .hover(|element| element.bg(theme.overlay))
                .child(if is_dir {
                    icon(
                        if entry.expanded {
                            "icons/chevron-down.svg"
                        } else {
                            "icons/chevron-right.svg"
                        },
                        10.0,
                        theme.text_ghost,
                    )
                    .into_any_element()
                } else {
                    div().w(px(10.0)).h(px(10.0)).flex_none().into_any_element()
                })
                .child(icon(
                    if is_dir {
                        "icons/folder.svg"
                    } else {
                        "icons/list.svg"
                    },
                    13.0,
                    theme.text_tertiary,
                ))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_size(px(11.5))
                        .text_color(theme.text_secondary)
                        .child(entry.name),
                );
            list = if is_dir {
                list.child(row.on_click(cx.listener(move |this, _, _, cx| {
                    if !this.right_panel_expanded_paths.remove(&absolute_path) {
                        this.right_panel_expanded_paths
                            .insert(absolute_path.clone());
                    }
                    cx.notify();
                })))
            } else {
                list.child(row.on_click(cx.listener(move |this, _, _, cx| {
                    this.open_right_panel_file(relative_path.clone(), cx);
                })))
            };
        }

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(42.0))
                    .flex_none()
                    .px(px(16.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(icon("icons/folder.svg", 13.0, theme.text_tertiary))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(px(11.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text_secondary)
                            .child(project_name),
                    ),
            )
            .child(div().flex_1().min_h_0().child(list).overflow_y_scrollbar())
    }

    fn render_right_panel_file(&self, relative_path: String, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let content = self
            .selected_project()
            .map(|project| project.path.join(&relative_path))
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|content| {
                let mut chars = content.chars();
                let preview = chars.by_ref().take(30_000).collect::<String>();
                if chars.next().is_some() {
                    format!("{preview}\n\n… File preview truncated")
                } else {
                    preview
                }
            })
            .unwrap_or_else(|| "This file is binary, too large, or no longer available.".into());

        let editor = div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(42.0))
                    .flex_none()
                    .px(px(16.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(icon("icons/list.svg", 13.0, theme.text_tertiary))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(px(11.0))
                            .text_color(theme.text_secondary)
                            .child(relative_path.clone()),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .p(px(16.0))
                    .font_family("SF Mono")
                    .text_size(px(10.5))
                    .line_height(px(16.0))
                    .text_color(theme.text_secondary)
                    .whitespace_normal()
                    .child(content)
                    .overflow_y_scrollbar(),
            );

        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .child(editor)
            .child(
                div()
                    .w(px(184.0))
                    .min_w(px(164.0))
                    .h_full()
                    .flex_none()
                    .border_l_1()
                    .border_color(theme.border_strong)
                    .child(self.render_right_panel_working_tree(Some(&relative_path), cx)),
            )
    }

    fn render_right_panel_diff(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        if self.right_panel_diff_files.is_empty() {
            return self.render_right_panel_empty_message(
                "No changes",
                "The current workspace has no file changes to review.",
                cx,
            );
        }

        let additions = self
            .right_panel_diff_files
            .iter()
            .map(|file| file.additions)
            .sum::<u64>();
        let deletions = self
            .right_panel_diff_files
            .iter()
            .map(|file| file.deletions)
            .sum::<u64>();
        let count = self.right_panel_diff_files.len();
        let mut rows = div().flex().flex_col().py(px(6.0));
        for file in self.right_panel_diff_files.clone() {
            let path = file.path.clone();
            rows = rows.child(
                div()
                    .id(SharedString::from(format!("right-panel-diff-{path}")))
                    .h(px(32.0))
                    .mx(px(8.0))
                    .px(px(8.0))
                    .rounded(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .cursor_default()
                    .hover(|element| element.bg(theme.overlay))
                    .child(icon("icons/list.svg", 13.0, theme.text_tertiary))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(px(11.0))
                            .text_color(theme.text_secondary)
                            .child(file.path),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(10.5))
                            .text_color(theme.warning)
                            .child(format!("+{}", file.additions)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(10.5))
                            .text_color(theme.danger)
                            .child(format!("-{}", file.deletions)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_right_panel_surface(RightPanelSurface::File(path.clone()), cx);
                    })),
            );
        }

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(42.0))
                    .flex_none()
                    .px(px(16.0))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_size(px(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text_secondary)
                    .child(format!(
                        "{count} changed {}",
                        if count == 1 { "file" } else { "files" }
                    ))
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme.warning)
                            .child(format!("+{additions}")),
                    )
                    .child(
                        div()
                            .ml(px(6.0))
                            .text_size(px(10.5))
                            .text_color(theme.danger)
                            .child(format!("-{deletions}")),
                    ),
            )
            .child(div().flex_1().min_h_0().child(rows).overflow_y_scrollbar())
    }

    fn render_right_panel_empty_message(
        &self,
        title: &'static str,
        description: &'static str,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .pb(px(32.0))
            .child(
                div()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(title),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .max_w(px(300.0))
                    .text_center()
                    .text_size(px(11.0))
                    .line_height(px(17.0))
                    .text_color(theme.text_tertiary)
                    .child(description),
            )
    }

    fn refresh_right_panel_diff(&mut self) {
        let Some(project) = self.selected_project() else {
            self.right_panel_diff_files.clear();
            return;
        };
        let project_path = project.path.clone();
        let mut files = BTreeMap::<String, RightPanelDiffFile>::new();

        if let Ok(output) = Command::new("git")
            .args(["diff", "--numstat", "HEAD", "--", "."])
            .current_dir(&project_path)
            .output()
            && output.status.success()
        {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let mut columns = line.splitn(3, '\t');
                let additions = columns.next().unwrap_or("0").parse().unwrap_or(0);
                let deletions = columns.next().unwrap_or("0").parse().unwrap_or(0);
                let Some(path) = columns.next() else {
                    continue;
                };
                files.insert(
                    path.to_owned(),
                    RightPanelDiffFile {
                        path: path.to_owned(),
                        additions,
                        deletions,
                    },
                );
            }
        }

        if let Ok(output) = Command::new("git")
            .args(["ls-files", "--others", "--exclude-standard", "--", "."])
            .current_dir(&project_path)
            .output()
            && output.status.success()
        {
            for path in String::from_utf8_lossy(&output.stdout).lines() {
                if path.is_empty() || files.contains_key(path) {
                    continue;
                }
                let additions = std::fs::read_to_string(project_path.join(path))
                    .map(|content| content.lines().count() as u64)
                    .unwrap_or(0);
                files.insert(
                    path.to_owned(),
                    RightPanelDiffFile {
                        path: path.to_owned(),
                        additions,
                        deletions: 0,
                    },
                );
            }
        }

        self.right_panel_diff_files = files.into_values().collect();
    }
}
