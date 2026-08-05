//! Right-click context menus.
//!
//! A menu is built lazily: the item list is only constructed once a right-click
//! actually opens it, and while closed the wrapper contributes one `Rc<Cell>`
//! read and no children. The open menu renders through `deferred(anchored(..))`
//! so it escapes the transcript row's clipping and paints above every sibling.
//!
//! Dismissal follows Zed's own context menus:
//!
//! - **Click outside** uses `on_mouse_down_out`, which tests the card's own
//!   hitbox during the capture phase. An occluding full-window backdrop would
//!   also work but has to guess the window size and swallows hover elsewhere.
//! - **Escape** is an action bound in the menu's own key context, so it beats
//!   the transcript's `escape` binding instead of also cancelling the turn.
//! - **Focus** is taken two frames after opening. Deferred elements are not
//!   linked into the dispatch tree until after the deferred draw runs, so
//!   focusing any earlier silently does nothing — and then no key reaches the
//!   menu at all.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, ElementId, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, ParentElement, Pixels, Point, RenderOnce, SharedString, Styled,
    Window, actions, anchored, deferred, div, prelude::FluentBuilder, px,
};

actions!(waku_menu, [DismissMenu]);

/// Key context the open menu declares, and the scope its bindings live in.
const MENU_CONTEXT: &str = "WakuMenu";

/// Bind the menu's own keys. Called once at startup.
pub fn init(cx: &mut App) {
    cx.bind_keys([gpui::KeyBinding::new(
        "escape",
        DismissMenu,
        Some(MENU_CONTEXT),
    )]);
}

use crate::theme::Theme;
use crate::ui::icon;

/// One row of a context menu.
pub enum MenuItem {
    Entry {
        label: SharedString,
        icon: Option<&'static str>,
        #[allow(clippy::type_complexity)]
        on_click: Rc<dyn Fn(&mut Window, &mut App)>,
    },
    Separator,
}

impl MenuItem {
    pub fn new(
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self::Entry {
            label: label.into(),
            icon: None,
            on_click: Rc::new(on_click),
        }
    }

    pub fn icon(mut self, path: &'static str) -> Self {
        if let Self::Entry { icon, .. } = &mut self {
            *icon = Some(path);
        }
        self
    }

    fn is_focusable(&self) -> bool {
        matches!(self, Self::Entry { .. })
    }
}

/// Where an open menu is anchored, in window coordinates.
#[derive(Debug, Default)]
struct MenuState {
    open: Option<Point<Pixels>>,
    /// Keyboard cursor over focusable entries.
    highlighted: Option<usize>,
}

/// Cross-frame state for one context menu. The owner keeps one per menu site.
#[derive(Clone)]
pub struct ContextMenuHandle {
    state: Rc<RefCell<MenuState>>,
    focus: FocusHandle,
    /// Notified with the new open state whenever the menu toggles. The transcript
    /// uses this to keep the composer's caret visible while a menu holds focus.
    #[allow(clippy::type_complexity)]
    on_toggle: Option<Rc<dyn Fn(bool, &mut Window, &mut App)>>,
}

impl ContextMenuHandle {
    pub fn new(cx: &mut App) -> Self {
        Self {
            state: Rc::new(RefCell::new(MenuState::default())),
            focus: cx.focus_handle(),
            on_toggle: None,
        }
    }

    /// Observe open/close transitions. Called only on an actual change.
    pub fn on_toggle(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }

    pub fn close(&self, window: &mut Window, cx: &mut App) {
        let was_open = {
            let mut state = self.state.borrow_mut();
            let was_open = state.open.is_some();
            state.open = None;
            state.highlighted = None;
            was_open
        };
        if was_open && let Some(handler) = &self.on_toggle {
            handler(false, window, cx);
        }
    }

    fn open_at(&self, position: Point<Pixels>, window: &mut Window, cx: &mut App) {
        let was_open = {
            let mut state = self.state.borrow_mut();
            let was_open = state.open.is_some();
            state.open = Some(position);
            state.highlighted = None;
            was_open
        };
        if !was_open && let Some(handler) = &self.on_toggle {
            handler(true, window, cx);
        }
    }
}

/// Attach a context menu to `element`.
///
/// `items` is called only when the menu opens, so building the item list — which
/// may capture message content or run availability checks — never costs
/// anything on an ordinary frame.
pub fn context_menu<E>(
    element: E,
    id: impl Into<ElementId>,
    handle: &ContextMenuHandle,
    items: impl Fn(&mut App) -> Vec<MenuItem> + 'static,
) -> AnyElement
where
    E: ParentElement + Styled + InteractiveElement + IntoElement + 'static,
{
    let id: ElementId = id.into();
    let open_at = handle.state.borrow().open;
    let handle_for_down = handle.clone();

    let element = element.relative().on_mouse_down(
        MouseButton::Right,
        move |event: &MouseDownEvent, window, cx| {
            handle_for_down.open_at(event.position, window, cx);
            cx.stop_propagation();
            window.prevent_default();

            // The card is deferred, so its focus handle joins the dispatch tree
            // only after the deferred draw. Focusing before then is a silent
            // no-op that leaves the menu unable to see a keystroke.
            let focus = handle_for_down.focus.clone();
            window.on_next_frame(move |window, _| {
                window.on_next_frame(move |window, cx| window.focus(&focus, cx));
            });
            window.refresh();
        },
    );

    let Some(position) = open_at else {
        return element.into_any_element();
    };

    element
        .child(
            deferred(
                anchored()
                    .position(position)
                    .snap_to_window_with_margin(px(8.0))
                    .child(MenuCard {
                        id,
                        handle: handle.clone(),
                        items: Rc::new(items),
                    }),
            )
            .with_priority(1),
        )
        .into_any_element()
}

#[derive(IntoElement)]
struct MenuCard {
    id: ElementId,
    handle: ContextMenuHandle,
    #[allow(clippy::type_complexity)]
    items: Rc<dyn Fn(&mut App) -> Vec<MenuItem>>,
}

impl RenderOnce for MenuCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::current(cx);
        let _ = window;
        let items = (self.items)(cx);
        let focusable = focusable_indexes(&items);
        let highlighted = self.handle.state.borrow().highlighted;

        let mut card = div()
            .id(self.id)
            .occlude()
            .track_focus(&self.handle.focus)
            .key_context(MENU_CONTEXT)
            .on_action({
                let handle = self.handle.clone();
                move |_: &DismissMenu, window, cx| {
                    handle.close(window, cx);
                    window.refresh();
                }
            })
            .on_mouse_down_out({
                let handle = self.handle.clone();
                move |_, window, cx| {
                    handle.close(window, cx);
                    window.refresh();
                }
            })
            .min_w(px(176.0))
            .max_w(px(320.0))
            .py(px(4.0))
            .rounded(px(9.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.raised)
            .shadow_lg()
            .flex()
            .flex_col()
            .on_key_down({
                let handle = self.handle.clone();
                let focusable = focusable.clone();
                let items = self.items.clone();
                move |event: &KeyDownEvent, window, cx| {
                    on_menu_key(&handle, &focusable, &items, event, window, cx);
                }
            });

        for (index, item) in items.into_iter().enumerate() {
            card = card.child(match item {
                MenuItem::Separator => div()
                    .my(px(4.0))
                    .mx(px(6.0))
                    .h(px(1.0))
                    .bg(theme.border)
                    .into_any_element(),
                MenuItem::Entry {
                    label,
                    icon: item_icon,
                    on_click,
                } => {
                    let color = theme.text_secondary;
                    let handle = self.handle.clone();
                    div()
                        .id(index)
                        .mx(px(4.0))
                        .px(px(8.0))
                        .h(px(26.0))
                        .rounded(px(6.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .text_size(px(11.5))
                        .line_height(px(15.0))
                        .text_color(color)
                        .when(highlighted == Some(index), |element| {
                            element.bg(theme.overlay_strong)
                        })
                        .cursor_default()
                        .hover(|element| element.bg(theme.overlay))
                        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                            handle.close(window, cx);
                            on_click(window, cx);
                            window.refresh();
                        })
                        .when_some(item_icon, |element, path| {
                            element.child(icon(path, 12.0, color))
                        })
                        .child(div().flex_1().min_w_0().truncate().child(label))
                        .into_any_element()
                }
            });
        }
        card
    }
}

fn focusable_indexes(items: &[MenuItem]) -> Rc<Vec<usize>> {
    Rc::new(
        items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.is_focusable())
            .map(|(index, _)| index)
            .collect(),
    )
}

/// The next highlighted item index for a navigation key, wrapping at both ends.
/// `current` and the result are indexes into the *item list*, not into
/// `focusable`. `None` means the key does not navigate.
fn next_highlight(focusable: &[usize], current: Option<usize>, key: &str) -> Option<usize> {
    if focusable.is_empty() {
        return None;
    }
    let position =
        current.and_then(|item| focusable.iter().position(|candidate| *candidate == item));
    let next = match key {
        "down" => position.map_or(0, |index| (index + 1) % focusable.len()),
        "up" => position.map_or(focusable.len() - 1, |index| {
            (index + focusable.len() - 1) % focusable.len()
        }),
        "home" => 0,
        "end" => focusable.len() - 1,
        _ => return None,
    };
    Some(focusable[next])
}

fn on_menu_key(
    handle: &ContextMenuHandle,
    focusable: &[usize],
    items: &Rc<dyn Fn(&mut App) -> Vec<MenuItem>>,
    event: &KeyDownEvent,
    window: &mut Window,
    cx: &mut App,
) {
    let key = event.keystroke.key.as_str();
    if key == "escape" {
        handle.close(window, cx);
        window.refresh();
        return;
    }
    if focusable.is_empty() {
        return;
    }

    let current = handle.state.borrow().highlighted;
    if let Some(next) = next_highlight(focusable, current, key) {
        handle.state.borrow_mut().highlighted = Some(next);
        window.refresh();
        return;
    }

    if matches!(key, "enter" | "space")
        && let Some(highlighted) = handle.state.borrow().highlighted
    {
        // Rebuild to reach the entry's closure: the item list is intentionally
        // not retained between frames.
        let activated = items(cx)
            .into_iter()
            .nth(highlighted)
            .and_then(|item| match item {
                MenuItem::Entry { on_click, .. } => Some(on_click),
                MenuItem::Separator => None,
            });
        if let Some(on_click) = activated {
            handle.close(window, cx);
            on_click(window, cx);
            window.refresh();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<MenuItem> {
        vec![
            MenuItem::new("Copy", |_, _| {}),
            MenuItem::Separator,
            MenuItem::Separator,
            MenuItem::new("Revert", |_, _| {}),
        ]
    }

    #[test]
    fn separators_are_not_focusable() {
        assert_eq!(*focusable_indexes(&items()), vec![0, 3]);
    }

    #[test]
    fn keyboard_navigation_wraps_at_both_ends() {
        let focusable = focusable_indexes(&items());
        // Two focusable entries at indexes 0 and 3: down from the last wraps to
        // the first, and up from the first wraps to the last.
        assert_eq!(next_highlight(&focusable, None, "down"), Some(0));
        assert_eq!(next_highlight(&focusable, Some(0), "down"), Some(3));
        assert_eq!(next_highlight(&focusable, Some(3), "down"), Some(0));
        assert_eq!(next_highlight(&focusable, None, "up"), Some(3));
        assert_eq!(next_highlight(&focusable, Some(0), "up"), Some(3));
        assert_eq!(next_highlight(&focusable, Some(0), "home"), Some(0));
        assert_eq!(next_highlight(&focusable, Some(0), "end"), Some(3));
        assert_eq!(next_highlight(&focusable, Some(0), "tab"), None);
        assert_eq!(next_highlight(&[], None, "down"), None);
    }
}
