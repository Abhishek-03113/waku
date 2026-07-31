use std::ops::Range;
use std::time::Duration;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, DismissEvent, Element, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    GlobalElementId, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, SharedString, StyledText, Subscription,
    Task, TextLayout, TextRun, Timer, UTF16Selection, UnderlineStyle, Window, actions, div, fill,
    hsla, point, prelude::*, px, size,
};
use gpui_component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme::Theme;

actions!(
    composer,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Cut,
        Copy,
        Enter,
    ]
);

const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(500);
const CURSOR_BLINK_PAUSE: Duration = Duration::from_millis(300);

struct BlinkCursor {
    visible: bool,
    paused: bool,
    epoch: usize,
    _task: Task<()>,
}

impl BlinkCursor {
    fn new() -> Self {
        Self {
            visible: false,
            paused: false,
            epoch: 0,
            _task: Task::ready(()),
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.blink(self.epoch, cx);
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        self.epoch = 0;
        cx.notify();
    }

    fn visible(&self) -> bool {
        self.paused || self.visible
    }

    fn pause(&mut self, cx: &mut Context<Self>) {
        self.paused = true;
        self.visible = true;
        cx.notify();

        let epoch = self.next_epoch();
        self._task = cx.spawn(async move |this, cx| {
            Timer::after(CURSOR_BLINK_PAUSE).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| {
                    this.paused = false;
                    this.blink(epoch, cx);
                })
                .ok();
            }
        });
    }

    fn next_epoch(&mut self) -> usize {
        self.epoch += 1;
        self.epoch
    }

    fn blink(&mut self, epoch: usize, cx: &mut Context<Self>) {
        if self.paused || epoch != self.epoch {
            self.visible = true;
            return;
        }

        self.visible = !self.visible;
        cx.notify();

        let epoch = self.next_epoch();
        self._task = cx.spawn(async move |this, cx| {
            Timer::after(CURSOR_BLINK_INTERVAL).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| this.blink(epoch, cx)).ok();
            }
        });
    }
}

#[derive(Clone)]
pub enum ComposerEvent {
    Submit(String),
}

pub struct ComposerInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<TextLayout>,
    is_selecting: bool,
    external_context_menu_focus_holds: usize,
    blink_cursor: Entity<BlinkCursor>,
    _subscriptions: Vec<Subscription>,
}

impl ComposerInput {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let blink_cursor = cx.new(|_| BlinkCursor::new());
        let _subscriptions = vec![
            cx.observe(&blink_cursor, |_, _, cx| cx.notify()),
            cx.observe_window_activation(window, |input, window, cx| {
                if window.is_window_active()
                    && (input.focus_handle.is_focused(window)
                        || input.context_menu_preserves_visual_focus())
                {
                    input.blink_cursor.update(cx, |cursor, cx| cursor.start(cx));
                } else if !window.is_window_active() {
                    input.blink_cursor.update(cx, |cursor, cx| cursor.stop(cx));
                }
            }),
            cx.on_focus(&focus_handle, window, Self::on_focus),
            cx.on_blur(&focus_handle, window, Self::on_blur),
        ];
        Self {
            focus_handle,
            content: "".into(),
            placeholder: "Do anything…".into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            is_selecting: false,
            external_context_menu_focus_holds: 0,
            blink_cursor,
            _subscriptions,
        }
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    pub fn is_visually_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window) || self.context_menu_preserves_visual_focus()
    }

    pub fn preserve_visual_focus_for_context_menu(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.is_visually_focused(window) {
            return false;
        }
        self.external_context_menu_focus_holds += 1;
        cx.notify();
        true
    }

    pub fn release_visual_focus_for_context_menu(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.external_context_menu_focus_holds =
            self.external_context_menu_focus_holds.saturating_sub(1);
        if !self.is_visually_focused(window) {
            self.blink_cursor.update(cx, |cursor, cx| cursor.stop(cx));
        }
        cx.notify();
    }

    fn context_menu_preserves_visual_focus(&self) -> bool {
        self.external_context_menu_focus_holds > 0
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.content = "".into();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.pause_blink_cursor(cx);
        cx.notify();
    }

    pub fn set_content(&mut self, content: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.content = content.into();
        let offset = self.content.len();
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.marked_range = None;
        self.pause_blink_cursor(cx);
        cx.notify();
    }

    fn on_focus(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.blink_cursor.update(cx, |cursor, cx| cursor.start(cx));
    }

    fn on_blur(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.context_menu_preserves_visual_focus() {
            cx.notify();
            return;
        }
        self.blink_cursor.update(cx, |cursor, cx| cursor.stop(cx));
    }

    fn pause_blink_cursor(&mut self, cx: &mut Context<Self>) {
        self.blink_cursor.update(cx, |cursor, cx| cursor.pause(cx));
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn enter(&mut self, _: &Enter, _: &mut Window, cx: &mut Context<Self>) {
        let value = self.content.trim().to_owned();
        if !value.is_empty() {
            cx.emit(ComposerEvent::Submit(value));
            self.clear(cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text.replace(['\n', '\r'], " "), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn on_context_mouse_down(
        &mut self,
        _: &MouseDownEvent,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.pause_blink_cursor(cx);
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let Some(layout) = self.last_layout.as_ref() else {
            return 0;
        };
        layout
            .index_for_position(position)
            .unwrap_or_else(|index| index)
            .min(self.content.len())
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        self.pause_blink_cursor(cx);
        cx.notify();
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for character in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += character.len_utf16();
            utf8_offset += character.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for character in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += character.len_utf8();
            utf16_offset += character.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(index, _)| (index < offset).then_some(index))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(index, _)| (index > offset).then_some(index))
            .unwrap_or(self.content.len())
    }
}

impl EventEmitter<ComposerEvent> for ComposerInput {}

impl EntityInputHandler for ComposerInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content =
            (self.content[..range.start].to_owned() + new_text + &self.content[range.end..]).into();
        let offset = range.start + new_text.len();
        self.selected_range = offset..offset;
        self.marked_range = None;
        self.pause_blink_cursor(cx);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content =
            (self.content[..range.start].to_owned() + new_text + &self.content[range.end..]).into();
        self.marked_range =
            (!new_text.is_empty()).then_some(range.start..range.start + new_text.len());
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .map(|new_range| new_range.start + range.start..new_range.end + range.start)
            .unwrap_or_else(|| {
                let offset = range.start + new_text.len();
                offset..offset
            });
        self.pause_blink_cursor(cx);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let start = layout.position_for_index(range.start)?;
        let end = layout.position_for_index(range.end)?;
        let line_height = layout.line_height();
        if start.y == end.y {
            Some(Bounds::from_corners(
                start,
                point(end.x, end.y + line_height),
            ))
        } else {
            Some(Bounds::from_corners(
                point(bounds.left(), start.y),
                point(bounds.right(), end.y + line_height),
            ))
        }
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let layout = self.last_layout.as_ref()?;
        let utf8_index = layout
            .index_for_position(point)
            .unwrap_or_else(|index| index)
            .min(self.content.len());
        Some(self.offset_to_utf16(utf8_index))
    }
}

pub fn preserve_composer_focus_for_context_menu(
    composer: &Entity<ComposerInput>,
    mut menu: PopupMenu,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    if let Some(action_context) = window.focused(cx) {
        menu = menu.action_context(action_context);
    }

    let preserves_composer_focus = composer.update(cx, |composer, cx| {
        composer.preserve_visual_focus_for_context_menu(window, cx)
    });
    if preserves_composer_focus {
        let composer = composer.clone();
        let menu_entity = cx.entity();
        window
            .subscribe(&menu_entity, cx, move |_, _: &DismissEvent, window, cx| {
                composer.update(cx, |composer, cx| {
                    composer.release_visual_focus_for_context_menu(window, cx);
                });
            })
            .detach();
    }

    menu
}

fn cursor_should_be_visible(
    window_active: bool,
    input_focused: bool,
    context_menu_preserves_focus: bool,
    blink_visible: bool,
) -> bool {
    window_active && (context_menu_preserves_focus || (input_focused && blink_visible))
}

struct InputElement {
    input: Entity<ComposerInput>,
}

struct InputLayoutState {
    text: StyledText,
    text_layout_state: (),
}

struct PrepaintState {
    cursor: Option<PaintQuad>,
}

fn input_text_runs(
    display_len: usize,
    base_run: TextRun,
    selected_range: Option<&Range<usize>>,
    marked_range: Option<&Range<usize>>,
) -> Vec<TextRun> {
    let mut boundaries = vec![0, display_len];
    for range in [selected_range, marked_range].into_iter().flatten() {
        boundaries.push(range.start.min(display_len));
        boundaries.push(range.end.min(display_len));
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    boundaries
        .windows(2)
        .filter_map(|boundary| {
            let start = boundary[0];
            let end = boundary[1];
            (start < end).then(|| TextRun {
                len: end - start,
                background_color: selected_range
                    .filter(|range| range.start < end && range.end > start)
                    .map(|_| hsla(220.0 / 360.0, 0.10, 0.90, 0.18)),
                underline: marked_range
                    .filter(|range| range.start < end && range.end > start)
                    .map(|_| UnderlineStyle {
                        color: Some(base_run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                ..base_run.clone()
            })
        })
        .collect()
}

impl IntoElement for InputElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for InputElement {
    type RequestLayoutState = InputLayoutState;
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let style = window.text_style();
        let theme = Theme::dark();
        let (display_text, text_color, selected_range, marked_range) = if content.is_empty() {
            (input.placeholder.clone(), theme.text_ghost, None, None)
        } else {
            (
                content,
                style.color,
                Some(&input.selected_range),
                input.marked_range.as_ref(),
            )
        };
        let base_run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = input_text_runs(display_text.len(), base_run, selected_range, marked_range);
        let mut text = StyledText::new(display_text).with_runs(runs);
        let (layout_id, text_layout_state) = text.request_layout(id, inspector_id, window, cx);
        (
            layout_id,
            InputLayoutState {
                text,
                text_layout_state,
            },
        )
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout_state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        layout_state.text.prepaint(
            None,
            None,
            bounds,
            &mut layout_state.text_layout_state,
            window,
            cx,
        );
        let input = self.input.read(cx);
        let cursor = input.cursor_offset();
        let cursor_visible = cursor_should_be_visible(
            window.is_window_active(),
            input.focus_handle.is_focused(window),
            input.context_menu_preserves_visual_focus(),
            input.blink_cursor.read(cx).visible(),
        );
        let theme = Theme::dark();
        let layout = layout_state.text.layout();
        let cursor = (input.selected_range.is_empty() && cursor_visible)
            .then(|| layout.position_for_index(cursor))
            .flatten()
            .map(|cursor_position| {
                fill(
                    Bounds::new(cursor_position, size(px(1.5), layout.line_height())),
                    theme.accent,
                )
            });
        PrepaintState { cursor }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout_state: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let input = self.input.read(cx);
        let focus_handle = input.focus_handle.clone();
        let visually_focused = input.is_visually_focused(window);
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        layout_state.text.paint(
            None,
            None,
            bounds,
            &mut layout_state.text_layout_state,
            &mut (),
            window,
            cx,
        );
        if visually_focused && let Some(cursor) = prepaint.cursor.take() {
            window.paint_quad(cursor);
        }
        let text_layout = layout_state.text.layout().clone();
        self.input.update(cx, |input, _| {
            input.last_layout = Some(text_layout);
        });
    }
}

impl Render for ComposerInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let input = cx.entity();
        let context_menu_input = input.clone();
        div()
            .key_context("ComposerInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::enter))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_context_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .w_full()
            .min_h(px(24.0))
            .line_height(px(22.0))
            .text_size(px(13.5))
            .text_color(theme.text)
            .child(InputElement { input })
            .context_menu_with_id("composer-context-menu", move |menu, window, cx| {
                let (has_selection, has_content, all_selected) = {
                    let input = context_menu_input.read(cx);
                    let has_selection = !input.selected_range.is_empty();
                    let has_content = !input.content.is_empty();
                    let all_selected = has_content
                        && input.selected_range.start == 0
                        && input.selected_range.end == input.content.len();
                    (has_selection, has_content, all_selected)
                };
                let can_paste = cx
                    .read_from_clipboard()
                    .and_then(|item| item.text())
                    .is_some();

                preserve_composer_focus_for_context_menu(&context_menu_input, menu, window, cx)
                    .min_w(px(150.0))
                    .item(
                        PopupMenuItem::new("Cut")
                            .action(Box::new(Cut))
                            .disabled(!has_selection),
                    )
                    .item(
                        PopupMenuItem::new("Copy")
                            .action(Box::new(Copy))
                            .disabled(!has_selection),
                    )
                    .item(
                        PopupMenuItem::new("Paste")
                            .action(Box::new(Paste))
                            .disabled(!can_paste),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new("Select All")
                            .action(Box::new(SelectAll))
                            .disabled(!has_content || all_selected),
                    )
            })
    }
}

impl Focusable for ComposerInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TextRun, font, hsla};

    use super::{cursor_should_be_visible, input_text_runs};

    #[test]
    fn context_menu_keeps_cursor_visible_while_it_owns_focus() {
        assert!(cursor_should_be_visible(true, false, true, false));
        assert!(!cursor_should_be_visible(true, false, false, true));
        assert!(!cursor_should_be_visible(false, false, true, true));
    }

    #[test]
    fn selection_and_ime_styles_survive_wrapped_text_run_splitting() {
        let selection = 2..8;
        let marked = 4..6;
        let runs = input_text_runs(
            10,
            TextRun {
                len: 10,
                font: font(".SystemUIFont"),
                color: hsla(0.0, 0.0, 1.0, 1.0),
                background_color: None,
                underline: None,
                strikethrough: None,
            },
            Some(&selection),
            Some(&marked),
        );

        assert_eq!(
            runs.iter().map(|run| run.len).collect::<Vec<_>>(),
            [2, 2, 2, 2, 2]
        );
        assert_eq!(
            runs.iter()
                .map(|run| run.background_color.is_some())
                .collect::<Vec<_>>(),
            [false, true, true, true, false]
        );
        assert_eq!(
            runs.iter()
                .map(|run| run.underline.is_some())
                .collect::<Vec<_>>(),
            [false, false, true, false, false]
        );
    }
}
