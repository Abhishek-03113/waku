//! Context menus and dropdown menus.
//!
//! Both share one card and one dismissal model; they differ only in where they
//! anchor — a context menu at the pointer, a dropdown under its trigger.
//!
//! A menu is built lazily: the item list is only constructed once the menu
//! actually opens, and while closed the wrapper contributes one `Rc<Cell>` read
//! and no children. The open menu renders through `deferred(anchored(..))` so it
//! escapes its row's clipping and paints above every sibling.
//!
//! Dismissal follows Zed's own context menus:
//!
//! - **Click outside** uses `on_mouse_down_out`, which tests the card's own
//!   hitbox during the capture phase. An occluding full-window backdrop would
//!   also work but has to guess the window size and swallows hover elsewhere.
//!   A left click on the trigger is exempt — capture runs before the trigger's
//!   bubble-phase toggle, so closing here would make the toggle see a closed
//!   menu and reopen it.
//! - **Escape** is an action bound in the menu's own key context, so it beats
//!   the transcript's `escape` binding instead of also cancelling the turn.
//! - **Focus** is taken two frames after opening. Deferred elements are not
//!   linked into the dispatch tree until after the deferred draw runs, so
//!   focusing any earlier silently does nothing — and then no key reaches the
//!   menu at all.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{
    Anchor, AnyElement, App, Bounds, ElementId, FocusHandle, FontWeight, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, Pixels, Point,
    RenderOnce, SharedString, Styled, Window, actions, anchored, canvas, deferred, div,
    prelude::FluentBuilder, px,
};

actions!(
    waku_menu,
    [DismissMenu, SelectNextEntry, SelectPreviousEntry, ConfirmEntry]
);

/// Key context the open menu declares, and the scope its bindings live in.
const MENU_CONTEXT: &str = "WakuMenu";

/// Vertical gap between a trigger and its anchored card.
const TRIGGER_GAP: f32 = 4.0;

/// A text field inside an open panel, such as a picker's filter box.
///
/// The field holds real focus the whole time — the list's selection is drawn,
/// never focused, which is how Zed's picker works. So the list's keys have to
/// be claimed from under the focused field, and only a binding can do that:
/// `enter` and the arrows reach the field as *actions*, and an action consumes
/// the keystroke before any `on_key_down` listener above it ever runs.
const PANEL_FIELD_CONTEXT: &str = "WakuMenu > ComposerInput";

/// Bind the menu's own keys. Called once at startup.
///
/// Must run after [`crate::input::init`]: these share a context depth with the
/// field's own bindings, and the tie goes to whichever was registered last.
/// That is what lets `enter` here beat the field's submit.
pub fn init(cx: &mut App) {
    use gpui::KeyBinding;
    cx.bind_keys([
        KeyBinding::new("escape", DismissMenu, Some(MENU_CONTEXT)),
        KeyBinding::new("down", SelectNextEntry, Some(PANEL_FIELD_CONTEXT)),
        KeyBinding::new("up", SelectPreviousEntry, Some(PANEL_FIELD_CONTEXT)),
        KeyBinding::new("enter", ConfirmEntry, Some(PANEL_FIELD_CONTEXT)),
    ]);
}

use crate::theme::Theme;
use crate::ui::icon;

/// One row of a menu.
pub enum MenuItem {
    Entry {
        label: SharedString,
        icon: Option<&'static str>,
        /// Draws a trailing check, for menus that present a current choice.
        selected: bool,
        /// Shown greyed and inert. Preferred over omitting the row when the
        /// action is temporarily unavailable, so the menu keeps a stable shape.
        disabled: bool,
        #[allow(clippy::type_complexity)]
        on_click: Rc<dyn Fn(&mut Window, &mut App)>,
    },
    /// A caller-drawn row, for choices that need more than a label — a badge, a
    /// secondary line, an inline swatch. Clickable when `on_click` is set.
    Custom {
        #[allow(clippy::type_complexity)]
        render: Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>,
        #[allow(clippy::type_complexity)]
        on_click: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    },
    /// A non-interactive caption grouping the rows beneath it.
    Header(SharedString),
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
            selected: false,
            disabled: false,
            on_click: Rc::new(on_click),
        }
    }

    /// A caller-drawn row. `render` runs on every frame the menu is open.
    pub fn custom(render: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) -> Self {
        Self::Custom {
            render: Rc::new(render),
            on_click: None,
        }
    }

    pub fn on_click(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        if let Self::Custom { on_click, .. } = &mut self {
            *on_click = Some(Rc::new(handler));
        }
        self
    }

    pub fn selected(mut self, value: bool) -> Self {
        if let Self::Entry { selected, .. } = &mut self {
            *selected = value;
        }
        self
    }

    pub fn disabled(mut self, value: bool) -> Self {
        if let Self::Entry { disabled, .. } = &mut self {
            *disabled = value;
        }
        self
    }

    pub fn icon(mut self, path: &'static str) -> Self {
        if let Self::Entry { icon, .. } = &mut self {
            *icon = Some(path);
        }
        self
    }

    fn is_focusable(&self) -> bool {
        match self {
            Self::Entry { disabled, .. } => !disabled,
            Self::Custom { on_click, .. } => on_click.is_some(),
            Self::Header(_) | Self::Separator => false,
        }
    }

    fn click_handler(self) -> Option<Rc<dyn Fn(&mut Window, &mut App)>> {
        match self {
            Self::Entry {
                disabled: false,
                on_click,
                ..
            } => Some(on_click),
            Self::Entry { disabled: true, .. } => None,
            Self::Custom { on_click, .. } => on_click,
            Self::Header(_) | Self::Separator => None,
        }
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
    /// The trigger's bounds as of the last frame, so a dropdown can align under
    /// it. Recorded by a zero-cost canvas inside the trigger.
    trigger_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Notified with the new open state whenever the menu toggles, in order.
    /// The composer's caret preservation is one of these; a site can add its
    /// own on top.
    #[allow(clippy::type_complexity)]
    on_toggle: Rc<Vec<Rc<dyn Fn(bool, &mut Window, &mut App)>>>,
}

impl ContextMenuHandle {
    pub fn new(cx: &mut App) -> Self {
        Self {
            state: Rc::new(RefCell::new(MenuState::default())),
            focus: cx.focus_handle(),
            trigger_bounds: Rc::new(Cell::new(None)),
            on_toggle: Rc::new(Vec::new()),
        }
    }

    /// Observe open/close transitions. Called only on an actual change, in the
    /// order the observers were added.
    pub fn on_toggle(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        let mut handlers = (*self.on_toggle).clone();
        handlers.push(Rc::new(handler));
        self.on_toggle = Rc::new(handlers);
        self
    }

    fn notify_toggle(&self, open: bool, window: &mut Window, cx: &mut App) {
        for handler in self.on_toggle.iter() {
            handler(open, window, cx);
        }
    }

    pub fn is_open(&self) -> bool {
        self.state.borrow().open.is_some()
    }

    pub fn close(&self, window: &mut Window, cx: &mut App) {
        let was_open = {
            let mut state = self.state.borrow_mut();
            let was_open = state.open.is_some();
            state.open = None;
            state.highlighted = None;
            was_open
        };
        if was_open {
            self.notify_toggle(false, window, cx);
        }
    }

    /// Dismiss for a mouse down outside the card, except a left click on the
    /// trigger: the trigger's own bubble-phase handler is the toggle, and it
    /// runs after this capture-phase listener — closing here first would make
    /// it see a closed menu and reopen it. Context menus attach no trigger
    /// probe, so their bounds stay `None` and every outside click dismisses.
    fn dismiss_on_down_out(&self, event: &MouseDownEvent, window: &mut Window, cx: &mut App) {
        let on_trigger = event.button == MouseButton::Left
            && self
                .trigger_bounds
                .get()
                .is_some_and(|bounds| bounds.contains(&event.position));
        if on_trigger {
            return;
        }
        self.close(window, cx);
        window.refresh();
    }

    fn open_at(&self, position: Point<Pixels>, window: &mut Window, cx: &mut App) {
        let was_open = {
            let mut state = self.state.borrow_mut();
            let was_open = state.open.is_some();
            state.open = Some(position);
            state.highlighted = None;
            was_open
        };
        if !was_open {
            self.notify_toggle(true, window, cx);
        }
    }
}

/// Whether the opened surface takes focus itself.
///
/// A [`MenuCard`] tracks the handle's focus and needs it to see arrow keys. A
/// [`PopoverCard`] does not track it, so focusing the handle would detach focus
/// from the window's dispatch tree — and blur whatever the panel's content
/// focused for itself, such as a search field.
#[derive(Clone, Copy, Eq, PartialEq)]
enum SurfaceFocus {
    Card,
    Content,
}

/// Open at `position`, handing focus to the card when it owns focus.
///
/// The card is deferred, so its focus handle joins the dispatch tree only after
/// the deferred draw. Focusing before then is a silent no-op that leaves the
/// menu unable to see a keystroke — hence the two-frame wait, matching Zed.
fn open_menu(
    handle: &ContextMenuHandle,
    position: Point<Pixels>,
    focus_target: SurfaceFocus,
    window: &mut Window,
    cx: &mut App,
) {
    // Runs the toggle observers, which is where a content-focusing surface
    // schedules its own focus. Ours is scheduled after, so it would win — only
    // request it when the card is what should end up focused.
    handle.open_at(position, window, cx);
    if focus_target == SurfaceFocus::Card {
        let focus = handle.focus.clone();
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| window.focus(&focus, cx));
        });
    }
    window.refresh();
}

/// A zero-cost canvas that records its parent's bounds into the handle.
///
/// `inset_0` rather than `size_full`: an absolutely positioned child sizes
/// against its containing block, so `size_full` inside a padded trigger reports
/// the *content* box and the menu ends up indented by the trigger's padding.
fn trigger_bounds_probe(handle: &ContextMenuHandle) -> impl IntoElement {
    let bounds = handle.trigger_bounds.clone();
    canvas(
        move |probe: Bounds<Pixels>, _, _| bounds.set(Some(probe)),
        |_, _, _, _| (),
    )
    .absolute()
    .inset_0()
}

/// Where a dropdown's card sits relative to its trigger.
///
/// Side matters as much as alignment here: the composer's controls live at the
/// bottom of the window, so their menus have to grow upward or they open off
/// screen and get snapped back over the trigger.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MenuAlign {
    /// Below the trigger, left edges aligned.
    #[default]
    BelowLeft,
    /// Below the trigger, right edges aligned.
    BelowRight,
    /// Above the trigger, left edges aligned.
    AboveLeft,
    /// Above the trigger, right edges aligned. No site needs this corner yet;
    /// the four are kept together because they are one coordinate system, not
    /// four independent options.
    #[allow(dead_code)]
    AboveRight,
}

impl MenuAlign {
    fn above(self) -> bool {
        matches!(self, Self::AboveLeft | Self::AboveRight)
    }

    fn right_aligned(self) -> bool {
        matches!(self, Self::BelowRight | Self::AboveRight)
    }

    /// The card corner pinned to the anchor point.
    fn corner(self) -> Anchor {
        match self {
            Self::BelowLeft => Anchor::TopLeft,
            Self::BelowRight => Anchor::TopRight,
            Self::AboveLeft => Anchor::BottomLeft,
            Self::AboveRight => Anchor::BottomRight,
        }
    }

    /// The point on the trigger the card's corner attaches to.
    fn anchor_point(self, bounds: gpui::Bounds<Pixels>, gap: Pixels) -> Point<Pixels> {
        let x = if self.right_aligned() {
            bounds.right()
        } else {
            bounds.left()
        };
        let y = if self.above() {
            bounds.top() - gap
        } else {
            bounds.bottom() + gap
        };
        Point::new(x, y)
    }
}

/// A dropdown menu anchored under its trigger, toggled by a left click.
///
/// Unlike a context menu it aligns to the trigger rather than the pointer, so
/// the handle carries the trigger's last-known bounds. Those are a frame old,
/// which is invisible in practice: a trigger does not move between the click
/// and the menu appearing.
pub fn dropdown_menu<E>(
    trigger: E,
    id: impl Into<ElementId>,
    handle: &ContextMenuHandle,
    align: MenuAlign,
    items: impl Fn(&mut App) -> Vec<MenuItem> + 'static,
) -> AnyElement
where
    E: ParentElement + Styled + InteractiveElement + IntoElement + 'static,
{
    let id: ElementId = id.into();
    let items = Rc::new(items);
    anchored_surface(trigger, handle, align, SurfaceFocus::Card, move |handle| {
        MenuCard {
            id: id.clone(),
            handle: handle.clone(),
            items: items.clone(),
        }
        .into_any_element()
    })
}

/// A dropdown-anchored panel holding arbitrary content.
///
/// Same trigger, anchoring and dismissal as [`dropdown_menu`], but the card
/// draws no chrome — the content owns its own surface — and it does not take
/// focus, so a search field inside can. Escape still works: the card declares
/// the menu key context, and key dispatch walks up to it from the focused
/// descendant.
pub fn popover<E>(
    trigger: E,
    handle: &ContextMenuHandle,
    align: MenuAlign,
    content: impl Fn(&ContextMenuHandle, &mut Window, &mut App) -> AnyElement + 'static,
) -> AnyElement
where
    E: ParentElement + Styled + InteractiveElement + IntoElement + 'static,
{
    let content = Rc::new(content);
    anchored_surface(trigger, handle, align, SurfaceFocus::Content, move |handle| {
        PopoverCard {
            handle: handle.clone(),
            content: content.clone(),
        }
        .into_any_element()
    })
}

/// Toggle a [`popover`] as if its trigger were clicked, for keyboard shortcuts.
///
/// Anchors to the trigger's last recorded bounds, so it no-ops until the
/// trigger has drawn at least once. The handle's toggle observers may update
/// the owning entity, so a caller holding that entity's lease must defer this.
pub fn toggle_popover(
    handle: &ContextMenuHandle,
    align: MenuAlign,
    window: &mut Window,
    cx: &mut App,
) {
    if handle.is_open() {
        handle.close(window, cx);
        window.refresh();
        return;
    }
    let Some(anchor) = handle
        .trigger_bounds
        .get()
        .map(|bounds| align.anchor_point(bounds, px(TRIGGER_GAP)))
    else {
        return;
    };
    open_menu(handle, anchor, SurfaceFocus::Content, window, cx);
}

/// The shared half of both dropdown surfaces: a trigger that records its bounds
/// and toggles the handle, plus the open card deferred and anchored to it.
fn anchored_surface<E>(
    trigger: E,
    handle: &ContextMenuHandle,
    align: MenuAlign,
    focus_target: SurfaceFocus,
    card: impl Fn(&ContextMenuHandle) -> AnyElement + 'static,
) -> AnyElement
where
    E: ParentElement + Styled + InteractiveElement + IntoElement + 'static,
{
    let open_at = handle.state.borrow().open;
    let toggle_handle = handle.clone();

    let trigger = trigger
        .relative()
        .child(trigger_bounds_probe(handle))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            if toggle_handle.is_open() {
                toggle_handle.close(window, cx);
                window.refresh();
                cx.stop_propagation();
                return;
            }
            let anchor = toggle_handle
                .trigger_bounds
                .get()
                .map(|bounds| align.anchor_point(bounds, px(TRIGGER_GAP)))
                .unwrap_or_else(|| window.mouse_position());
            open_menu(&toggle_handle, anchor, focus_target, window, cx);
            cx.stop_propagation();
        });

    let Some(position) = open_at else {
        return trigger.into_any_element();
    };

    trigger
        .child(
            deferred(
                anchored()
                    .position(position)
                    .anchor(align.corner())
                    .snap_to_window_with_margin(px(8.0))
                    .child(card(handle)),
            )
            .with_priority(1),
        )
        .into_any_element()
}

/// A chrome-less card: dismissal and the menu key context, nothing else.
#[derive(IntoElement)]
struct PopoverCard {
    handle: ContextMenuHandle,
    #[allow(clippy::type_complexity)]
    content: Rc<dyn Fn(&ContextMenuHandle, &mut Window, &mut App) -> AnyElement>,
}

impl RenderOnce for PopoverCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let body = (self.content)(&self.handle, window, cx);
        div()
            .occlude()
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
                move |event, window, cx| handle.dismiss_on_down_out(event, window, cx)
            })
            .child(body)
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
            open_menu(
                &handle_for_down,
                event.position,
                SurfaceFocus::Card,
                window,
                cx,
            );
            cx.stop_propagation();
            window.prevent_default();
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
                move |event, window, cx| handle.dismiss_on_down_out(event, window, cx)
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
                MenuItem::Header(label) => div()
                    .px(px(10.0))
                    .pt(px(6.0))
                    .pb(px(2.0))
                    .text_size(px(10.0))
                    .line_height(px(14.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text_tertiary)
                    .child(label)
                    .into_any_element(),
                MenuItem::Entry {
                    label,
                    icon: item_icon,
                    selected,
                    disabled,
                    on_click,
                } => {
                    let color = match (disabled, selected) {
                        (true, _) => theme.text_ghost,
                        (false, true) => theme.text,
                        (false, false) => theme.text_secondary,
                    };
                    row(
                        index,
                        highlighted == Some(index),
                        &theme,
                        self.handle.clone(),
                        (!disabled).then_some(on_click),
                    )
                    .text_color(color)
                    .when(selected, |element| element.font_weight(FontWeight::MEDIUM))
                    .when_some(item_icon, |element, path| {
                        element.child(icon(path, 12.0, color))
                    })
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .when(selected, |element| {
                        element.child(icon("icons/check.svg", 11.0, theme.text_tertiary))
                    })
                    .into_any_element()
                }
                MenuItem::Custom { render, on_click } => {
                    let body = render(window, cx);
                    match on_click {
                        Some(on_click) => row(
                            index,
                            highlighted == Some(index),
                            &theme,
                            self.handle.clone(),
                            Some(on_click),
                        )
                        .child(body)
                        .into_any_element(),
                        // Non-interactive rows still need the row's insets so
                        // they line up with the entries around them.
                        None => div().mx(px(4.0)).px(px(8.0)).child(body).into_any_element(),
                    }
                }
            });
        }
        card
    }
}

/// The shared row: consistent insets, plus hover, keyboard highlight and
/// close-then-act when it has a handler. A `None` handler renders the same
/// geometry inert, which is how a disabled entry keeps the menu's shape.
fn row(
    index: usize,
    highlighted: bool,
    theme: &Theme,
    handle: ContextMenuHandle,
    on_click: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
) -> gpui::Stateful<gpui::Div> {
    let hover = theme.overlay;
    let highlight = theme.overlay_strong;
    div()
        .id(index)
        .mx(px(4.0))
        .px(px(8.0))
        .min_h(px(26.0))
        .rounded(px(6.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .text_size(px(11.5))
        .line_height(px(15.0))
        .when(highlighted, |element| element.bg(highlight))
        .when_some(on_click, |element, on_click| {
            element
                .cursor_default()
                .hover(move |element| element.bg(hover))
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    handle.close(window, cx);
                    on_click(window, cx);
                    window.refresh();
                })
        })
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
            .and_then(MenuItem::click_handler);
        if let Some(on_click) = activated {
            handle.close(window, cx);
            on_click(window, cx);
            window.refresh();
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Context, Modifiers, Render, TestAppContext, point};

    use super::*;

    /// Which anchored surface the harness mounts; both share
    /// `anchored_surface` but dismiss through different cards.
    #[derive(Clone, Copy)]
    enum Surface {
        Popover,
        Dropdown,
    }

    struct Harness {
        handle: ContextMenuHandle,
        surface: Surface,
    }

    impl Render for Harness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let trigger = div().w(px(120.0)).h(px(32.0));
            div().size_full().child(match self.surface {
                Surface::Popover => popover(
                    trigger,
                    &self.handle,
                    MenuAlign::BelowLeft,
                    |_, _, _| div().w(px(200.0)).h(px(100.0)).into_any_element(),
                ),
                Surface::Dropdown => dropdown_menu(
                    trigger,
                    "dropdown",
                    &self.handle,
                    MenuAlign::BelowLeft,
                    |_| vec![MenuItem::new("Entry", |_, _| {})],
                ),
            })
        }
    }

    /// The trigger sits at the window origin, 120×32; the card hangs below it,
    /// so a point inside the trigger is outside the card and vice versa.
    fn assert_trigger_toggles(surface: Surface, cx: &mut TestAppContext) {
        let handle = cx.update(ContextMenuHandle::new);
        let harness = Harness {
            handle: handle.clone(),
            surface,
        };
        let (_view, cx) = cx.add_window_view(|_, _| harness);
        let on_trigger = point(px(10.0), px(10.0));
        let outside = point(px(500.0), px(400.0));

        cx.simulate_mouse_down(on_trigger, MouseButton::Left, Modifiers::none());
        assert!(handle.is_open(), "first trigger click should open");

        // The card's capture-phase `on_mouse_down_out` sees this click first;
        // without the trigger exemption it closes the menu and the trigger's
        // own handler reopens it.
        cx.simulate_mouse_down(on_trigger, MouseButton::Left, Modifiers::none());
        assert!(!handle.is_open(), "second trigger click should close");

        cx.simulate_mouse_down(on_trigger, MouseButton::Left, Modifiers::none());
        assert!(handle.is_open(), "trigger click after close should reopen");

        cx.simulate_mouse_down(outside, MouseButton::Left, Modifiers::none());
        assert!(!handle.is_open(), "click outside should dismiss");
    }

    #[gpui::test]
    fn popover_trigger_toggles(cx: &mut TestAppContext) {
        assert_trigger_toggles(Surface::Popover, cx);
    }

    #[gpui::test]
    fn dropdown_trigger_toggles(cx: &mut TestAppContext) {
        assert_trigger_toggles(Surface::Dropdown, cx);
    }

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
